//! Storage accounting of the append-only tree family (issue #822).
//!
//! From GROVE_V4 an append charges each entry's permanent bytes once and
//! reports write churn as replacement:
//!
//! - every append charges the entry's chunk-blob share (its own bytes) as
//!   `added_bytes`;
//! - a dense-buffer slot that already holds a committed value (epoch 2 on)
//!   is `replaced_bytes`, growth added, shrink not credited;
//! - the compaction blob replaces the epoch's prepaid entry bytes, so only
//!   its framing and the MMR internal nodes are added;
//! - the commitment tree's frontier rewrite replaces the frontier loaded at
//!   open, growth added.
//!
//! V1..V3 issue every data put with no cost information — key + value as
//! new storage, every time. Stored bytes and roots are identical.
//!
//! These tests run the real operations against RocksDB twice — once under
//! GROVE_V4 and once under GROVE_V4 with the two accounting gates switched
//! off — so that the difference between the two costs is exactly the
//! accounting change and nothing else (the parent-Merk update, the hash
//! counts and the Sinsemilla work are identical on both sides). The legacy
//! figures themselves are then pinned by the model below.

use grovedb_commitment_tree::{
    CommitmentFrontier, DashMemo, NoteBytesData, TransmittedNoteCiphertext,
};
use grovedb_costs::OperationCost;
use grovedb_version::version::{
    v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4, GroveVersion,
};

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element,
};

/// A stored note entry: cmx (32) || rho (32) || cv_net (32) || DashMemo
/// ciphertext payload (216).
const NOTE_ENTRY: u32 = 312;

/// Paid key bytes of a dense-buffer slot: 32-byte prefix + 2-byte position
/// + 1 length byte.
const SLOT_KEY_PAID: u32 = 35;
/// Paid key bytes of the frontier: prefix + `__ct_data__` (11) + 1.
const FRONTIER_KEY_PAID: u32 = 44;

fn varint_len(mut n: u32) -> u32 {
    let mut len = 1;
    while n >= 0x80 {
        n >>= 7;
        len += 1;
    }
    len
}

/// The paid size of a stored value: its length plus the varint of it.
fn paid(len: u32) -> u32 {
    len + varint_len(len)
}

/// Serialized frontier size once the note at `position` is the latest leaf:
/// 1 (flag) + 8 (position) + 32 (leaf) + 1 (ommer count) + 32 per ommer,
/// and a frontier at leaf index `p` holds `popcount(p)` ommers.
fn frontier_len(position: u64) -> u32 {
    42 + 32 * position.count_ones()
}

/// GROVE_V4 with both accounting gates switched off: what V1..V3 report,
/// inside the V4 envelope so nothing else differs.
fn legacy_accounting() -> GroveVersion {
    let mut version = GROVE_V4.clone();
    version
        .bulk_append_tree_versions
        .cost
        .append_storage_accounting = 0;
    version
        .commitment_tree_versions
        .cost
        .frontier_save_storage_accounting = 0;
    version
}

/// Expected `(added, replaced)` difference, V4 minus legacy, for the append
/// that lands at `position` in a tree with `2^chunk_power` entries per epoch
/// and fixed `entry_len`-byte entries; `with_frontier` for a commitment
/// tree, which also rewrites its frontier.
fn expected_delta(
    position: u64,
    chunk_power: u8,
    entry_len: u32,
    with_frontier: bool,
) -> (i64, i64) {
    let epoch = 1u64 << chunk_power;
    let mut added: i64 = 0;
    let mut replaced: i64 = 0;

    // The entry's chunk-blob share, charged at every append.
    added += entry_len as i64;

    if position % epoch == epoch - 1 {
        // Compacting append: the overflow entry goes straight into the
        // blob (no slot write). Legacy charges the whole blob as added; V4
        // reports the epoch's entry bytes as replaced and only the framing
        // (identical on both sides) as added.
        let entry_bytes = (epoch * entry_len as u64) as i64;
        added -= entry_bytes;
        replaced += entry_bytes;
    } else if position >= epoch {
        // Slot rewrite: legacy charges key + value as new; V4 replaces the
        // value (same size: nothing added) and does not charge the key.
        added -= (SLOT_KEY_PAID + paid(entry_len)) as i64;
        replaced += paid(entry_len) as i64;
    }
    // else: a fresh slot is new storage on both sides.

    if with_frontier && position > 0 {
        // Legacy: key + whole frontier added on every save. V4: replaces the
        // frontier loaded at open, adds growth only.
        let new = paid(frontier_len(position));
        let old = paid(frontier_len(position - 1));
        added += new.saturating_sub(old) as i64 - (FRONTIER_KEY_PAID + new) as i64;
        replaced += new.min(old) as i64;
    }
    // else: the first save creates the key on both sides.

    (added, replaced)
}

fn delta(v4: &OperationCost, legacy: &OperationCost) -> (i64, i64) {
    (
        v4.storage_cost.added_bytes as i64 - legacy.storage_cost.added_bytes as i64,
        v4.storage_cost.replaced_bytes as i64 - legacy.storage_cost.replaced_bytes as i64,
    )
}

/// Everything except the storage figures must agree: the accounting gates
/// move bytes between `added` and `replaced`, nothing else.
fn assert_only_storage_differs(v4: &OperationCost, legacy: &OperationCost, what: &str) {
    assert_eq!(v4.seek_count, legacy.seek_count, "{what}: seek_count");
    assert_eq!(
        v4.storage_loaded_bytes, legacy.storage_loaded_bytes,
        "{what}: storage_loaded_bytes"
    );
    assert_eq!(
        v4.hash_node_calls, legacy.hash_node_calls,
        "{what}: hash_node_calls"
    );
    assert_eq!(
        v4.sinsemilla_hash_calls, legacy.sinsemilla_hash_calls,
        "{what}: sinsemilla_hash_calls"
    );
    assert_eq!(
        v4.storage_cost.removed_bytes, legacy.storage_cost.removed_bytes,
        "{what}: removed_bytes"
    );
}

// ── Commitment tree fixtures ──────────────────────────────────────────

fn test_cmx(index: u32) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&index.to_le_bytes());
    bytes[31] &= 0x7f;
    bytes
}

fn test_ciphertext(index: u32) -> TransmittedNoteCiphertext<DashMemo> {
    let mut epk_bytes = [0u8; 32];
    epk_bytes[..4].copy_from_slice(&index.to_le_bytes());
    let mut enc_data = [0u8; 104];
    enc_data[..4].copy_from_slice(&index.to_le_bytes());
    let mut out_ciphertext = [0u8; 80];
    out_ciphertext[..4].copy_from_slice(&index.to_le_bytes());
    TransmittedNoteCiphertext::from_parts(epk_bytes, NoteBytesData(enc_data), out_ciphertext)
}

fn rho(index: u32) -> [u8; 32] {
    let mut rho = [0u8; 32];
    rho[..4].copy_from_slice(&index.to_le_bytes());
    rho[4] = 0xAA;
    rho
}

fn cv_net(index: u32) -> [u8; 32] {
    let mut cv = [0u8; 32];
    cv[..4].copy_from_slice(&index.to_le_bytes());
    cv[4] = 0xCC;
    cv
}

fn ct_op(index: u32) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::commitment_tree_insert_op_typed(
        vec![b"pool".to_vec()],
        test_cmx(index),
        rho(index),
        cv_net(index),
        &test_ciphertext(index),
    )
}

fn ct_db(chunk_power: u8, version: &GroveVersion) -> TempGroveDb {
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(chunk_power).expect("valid chunk_power"),
        None,
        None,
        version,
    )
    .unwrap()
    .expect("insert commitment tree");
    db
}

fn ct_insert(db: &TempGroveDb, index: u32, version: &GroveVersion) -> OperationCost {
    let ctx = db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool",
        test_cmx(index),
        rho(index),
        cv_net(index),
        test_ciphertext(index),
        None,
        version,
    );
    ctx.value.expect("commitment tree insert");
    ctx.cost
}

fn root_hash(db: &TempGroveDb, version: &GroveVersion) -> [u8; 32] {
    db.root_hash(None, version).unwrap().expect("root hash")
}

fn assert_verifies(db: &TempGroveDb, version: &GroveVersion) {
    let issues = db
        .verify_grovedb(None, true, false, version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "integrity issues: {issues:?}");
}

// ── Tests ─────────────────────────────────────────────────────────────

/// The frontier-size model the expectations rely on matches the real
/// serialization at every position used below.
#[test]
fn frontier_size_model_matches_serialization() {
    let mut frontier = CommitmentFrontier::new();
    for position in 0..64u32 {
        frontier
            .append(test_cmx(position))
            .unwrap()
            .expect("append to frontier");
        assert_eq!(
            frontier.serialize().len() as u32,
            frontier_len(position as u64),
            "position {position}"
        );
    }
}

/// The gates are off in every released version and on in V4.
#[test]
fn accounting_gates_are_locked_before_v4() {
    for version in [&GROVE_V1, &GROVE_V2, &GROVE_V3] {
        assert_eq!(
            version
                .bulk_append_tree_versions
                .cost
                .append_storage_accounting,
            0
        );
        assert_eq!(
            version
                .commitment_tree_versions
                .cost
                .frontier_save_storage_accounting,
            0
        );
    }
    assert_eq!(
        GROVE_V4
            .bulk_append_tree_versions
            .cost
            .append_storage_accounting,
        1
    );
    assert_eq!(
        GROVE_V4
            .commitment_tree_versions
            .cost
            .frontier_save_storage_accounting,
        1
    );
}

/// Commitment tree at chunk_power 4, one direct append per position across
/// two and a half epochs: every append's `(added, replaced)` moves by
/// exactly the model — fresh slots in epoch 1, slot rewrites from epoch 2,
/// the blob-as-replacement at positions 15, 31 and 47, and the frontier
/// rewrite (growth at `2^k - 1`, shrink at `2^k`, first save at 0) — while
/// nothing else in the cost changes and both trees stay byte-identical.
#[test]
fn commitment_tree_append_storage_accounting_matches_model_across_epochs() {
    const CHUNK_POWER: u8 = 4;
    let legacy = legacy_accounting();
    let legacy_db = ct_db(CHUNK_POWER, &legacy);
    let v4_db = ct_db(CHUNK_POWER, &GROVE_V4);

    for position in 0..40u64 {
        let legacy_cost = ct_insert(&legacy_db, position as u32, &legacy);
        let v4_cost = ct_insert(&v4_db, position as u32, &GROVE_V4);

        assert_eq!(
            delta(&v4_cost, &legacy_cost),
            expected_delta(position, CHUNK_POWER, NOTE_ENTRY, true),
            "position {position}: v4 {v4_cost:?}\nlegacy {legacy_cost:?}"
        );
        assert_only_storage_differs(&v4_cost, &legacy_cost, &format!("position {position}"));
        assert_eq!(
            root_hash(&v4_db, &GROVE_V4),
            root_hash(&legacy_db, &legacy),
            "position {position}: the accounting must not touch stored bytes"
        );
    }
    assert_verifies(&v4_db, &GROVE_V4);
    assert_verifies(&legacy_db, &legacy);
}

/// The frontier rewrite in isolation, at chunk_power 11 where nothing
/// compacts: at `2^k - 1` the frontier gains an ommer (replaced at the old
/// size, 32 bytes added), at `2^k` it collapses to 74 bytes (replaced at the
/// new size, nothing credited), and in between it is replaced in full.
#[test]
fn frontier_rewrite_replaces_previous_size_and_adds_only_growth() {
    const CHUNK_POWER: u8 = 11;
    let legacy = legacy_accounting();
    let legacy_db = ct_db(CHUNK_POWER, &legacy);
    let v4_db = ct_db(CHUNK_POWER, &GROVE_V4);

    for position in 0..34u64 {
        let legacy_cost = ct_insert(&legacy_db, position as u32, &legacy);
        let v4_cost = ct_insert(&v4_db, position as u32, &GROVE_V4);
        let (d_added, d_replaced) = delta(&v4_cost, &legacy_cost);

        // Strip the (epoch-1, fresh-slot) share so only the frontier is left.
        let frontier_added = d_added - NOTE_ENTRY as i64;
        let frontier_replaced = d_replaced;
        if position == 0 {
            assert_eq!(
                (frontier_added, frontier_replaced),
                (0, 0),
                "first save: new on both"
            );
            continue;
        }
        let new = paid(frontier_len(position));
        let old = paid(frontier_len(position - 1));
        assert_eq!(
            frontier_replaced,
            new.min(old) as i64,
            "position {position}: replaced the smaller of previous/new paid size"
        );
        assert_eq!(
            frontier_added,
            new.saturating_sub(old) as i64 - (FRONTIER_KEY_PAID + new) as i64,
            "position {position}: legacy key + full value added; v4 growth only"
        );
        if position >= 4 && position.is_power_of_two() {
            // popcount drops from k >= 2 to 1 (positions 1 and 2 keep it at 1).
            assert!(new < old, "position {position} collapses the frontier");
            assert_eq!(new.saturating_sub(old), 0, "shrink is not credited");
        } else if (position + 1).is_power_of_two() {
            // popcount rises by one: 32 more bytes (plus one more length byte
            // when the varint widens, e.g. 106 -> 138 at position 7).
            assert_eq!(
                frontier_len(position) - frontier_len(position - 1),
                32,
                "position {position} gains one ommer"
            );
            assert!(new - old >= 32, "position {position}: growth is added");
        }
    }
}

/// The epoch boundary at Dash Platform's chunk_power 11: the compacting
/// append is no longer a ~630 KB `added_bytes` spike. V4 adds the note's
/// own 312 bytes plus a few hundred bytes of framing, and reports the
/// 2048 × 312 entry bytes the blob supersedes as replaced.
#[test]
fn epoch_boundary_at_chunk_power_11_is_not_an_added_bytes_spike() {
    const CHUNK_POWER: u8 = 11;
    const EPOCH: u32 = 1 << CHUNK_POWER as u32;
    let legacy = legacy_accounting();
    let legacy_db = ct_db(CHUNK_POWER, &legacy);
    let v4_db = ct_db(CHUNK_POWER, &GROVE_V4);

    // Seeding 2047 notes walks the dense buffer O(n^2); do both trees at
    // once.
    std::thread::scope(|scope| {
        for (db, version) in [(&legacy_db, &legacy), (&v4_db, &GROVE_V4)] {
            scope.spawn(move || {
                let seed: Vec<_> = (0..EPOCH - 1).map(ct_op).collect();
                db.apply_batch(seed, None, None, version)
                    .unwrap()
                    .expect("seed to one before the boundary");
            });
        }
    });

    let legacy_cost = ct_insert(&legacy_db, EPOCH - 1, &legacy);
    let v4_cost = ct_insert(&v4_db, EPOCH - 1, &GROVE_V4);

    let blob_entry_bytes = (EPOCH * NOTE_ENTRY) as u64; // 638_976
    assert!(
        legacy_cost.storage_cost.added_bytes as u64 > blob_entry_bytes,
        "legacy bills the whole blob as new storage: {legacy_cost:?}"
    );
    assert!(
        v4_cost.storage_cost.added_bytes < 2_000,
        "v4 adds the note's share plus framing only: {v4_cost:?}"
    );
    assert!(
        v4_cost.storage_cost.replaced_bytes as u64 >= blob_entry_bytes,
        "v4 reports the superseded entry bytes as replaced: {v4_cost:?}"
    );
    assert_eq!(
        delta(&v4_cost, &legacy_cost),
        expected_delta((EPOCH - 1) as u64, CHUNK_POWER, NOTE_ENTRY, true)
    );
    assert_only_storage_differs(&v4_cost, &legacy_cost, "boundary");
    assert_eq!(root_hash(&v4_db, &GROVE_V4), root_hash(&legacy_db, &legacy));
}

/// Over a whole epoch of steady state (epoch 2 at chunk_power 4) the V4
/// `added_bytes` add up to the bytes that persist — one blob share per
/// note plus the blob framing and the MMR node — while the legacy figures
/// add up to that PLUS a second copy of every note (the slot rewrites) and
/// the blob: ≈ 2× the physical growth.
#[test]
fn added_bytes_over_an_epoch_match_physical_growth() {
    const CHUNK_POWER: u8 = 4;
    const EPOCH: u64 = 1 << CHUNK_POWER;
    let legacy = legacy_accounting();
    let legacy_db = ct_db(CHUNK_POWER, &legacy);
    let v4_db = ct_db(CHUNK_POWER, &GROVE_V4);
    for position in 0..EPOCH {
        ct_insert(&legacy_db, position as u32, &legacy);
        ct_insert(&v4_db, position as u32, &GROVE_V4);
    }

    let mut v4_added: u64 = 0;
    let mut legacy_added: u64 = 0;
    let mut expected_added_delta: i64 = 0;
    for position in EPOCH..2 * EPOCH {
        legacy_added += ct_insert(&legacy_db, position as u32, &legacy)
            .storage_cost
            .added_bytes as u64;
        v4_added += ct_insert(&v4_db, position as u32, &GROVE_V4)
            .storage_cost
            .added_bytes as u64;
        expected_added_delta += expected_delta(position, CHUNK_POWER, NOTE_ENTRY, true).0;
    }
    assert_eq!(v4_added as i64 - legacy_added as i64, expected_added_delta);

    // Legacy over the epoch: 15 slot rewrites (key + note) + the whole blob
    // + 16 frontier saves (key + value) + the parent-Merk update; V4: 16
    // blob shares + blob framing + frontier growth + the parent-Merk update.
    // Strip the common parts the model does not see (parent Merk, MMR node,
    // frontier) by comparing the two sums: legacy carries an extra
    // 15 × (35 + 314) + (blob - framing = 16 × 312) - 16 × 312 (shares)
    // + frontier keys/values.
    let slot_rewrites = 15 * (SLOT_KEY_PAID + paid(NOTE_ENTRY)) as u64;
    assert!(
        legacy_added - v4_added >= slot_rewrites,
        "legacy charges every note twice over its life: legacy {legacy_added}, v4 {v4_added}"
    );
}

/// An epoch boundary inside ONE batch on a fresh tree: every slot is
/// written twice in the same session, but a `StorageBatch` keeps one put
/// per key and the slot is new in committed storage, so it is charged once
/// as new — never as a rewrite of a value that was never committed. Both
/// blobs replace their epochs' prepaid bytes; the shares net out exactly.
#[test]
fn epoch_boundary_inside_one_batch_charges_slots_once_as_new() {
    const CHUNK_POWER: u8 = 2; // epoch 4, capacity 3
    let legacy = legacy_accounting();
    let legacy_db = ct_db(CHUNK_POWER, &legacy);
    let v4_db = ct_db(CHUNK_POWER, &GROVE_V4);

    let ops = || (0..8u32).map(ct_op).collect::<Vec<_>>();
    let legacy_ctx = legacy_db.apply_batch(ops(), None, None, &legacy);
    legacy_ctx.value.expect("legacy batch");
    let v4_ctx = v4_db.apply_batch(ops(), None, None, &GROVE_V4);
    v4_ctx.value.expect("v4 batch");

    // Eight shares (8 × 312) exactly cover the two blobs' entry bytes
    // (2 × 4 × 312) that legacy bills as added and V4 as replaced; the
    // three slots are new on both sides (committed storage held nothing);
    // the single frontier save is the first (new on both sides).
    let two_blobs_entry_bytes = 2 * 4 * NOTE_ENTRY as i64;
    assert_eq!(
        delta(&v4_ctx.cost, &legacy_ctx.cost),
        (0, two_blobs_entry_bytes),
        "v4 {:?}\nlegacy {:?}",
        v4_ctx.cost,
        legacy_ctx.cost
    );
    assert_only_storage_differs(&v4_ctx.cost, &legacy_ctx.cost, "batch");
    assert_eq!(root_hash(&v4_db, &GROVE_V4), root_hash(&legacy_db, &legacy));
    assert_verifies(&v4_db, &GROVE_V4);
}

/// A plain `BulkAppendTree` with VARIABLE-size values: a slot rewrite is
/// sized against the value the slot held in committed storage (growth
/// added, shrink not credited), the share is the value's own length, and a
/// variable-format blob still replaces exactly the entry bytes.
#[test]
fn bulk_append_tree_accounting_with_variable_size_values() {
    const CHUNK_POWER: u8 = 2; // epoch 4, capacity 3
    let legacy = legacy_accounting();
    let make = |version: &GroveVersion| {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"bulk",
            Element::empty_bulk_append_tree(CHUNK_POWER).expect("valid chunk_power"),
            None,
            None,
            version,
        )
        .unwrap()
        .expect("insert bulk append tree");
        db
    };
    let legacy_db = make(&legacy);
    let v4_db = make(&GROVE_V4);

    // Epoch 1 fixed, epoch 2 variable, epoch 3 fixed again.
    let sizes: [u32; 12] = [8, 8, 8, 8, 8, 16, 4, 8, 8, 8, 8, 8];
    // Committed value length per slot.
    let mut slots: [Option<u32>; 3] = [None; 3];
    let mut epoch_bytes: u32 = 0;

    for (position, &len) in sizes.iter().enumerate() {
        let value = vec![position as u8 + 1; len as usize];
        let legacy_ctx = legacy_db.bulk_append(EMPTY_PATH, b"bulk", value.clone(), None, &legacy);
        legacy_ctx.value.expect("legacy append");
        let v4_ctx = v4_db.bulk_append(EMPTY_PATH, b"bulk", value, None, &GROVE_V4);
        v4_ctx.value.expect("v4 append");

        let mut added: i64 = len as i64; // the share
        let mut replaced: i64 = 0;
        epoch_bytes += len;
        if position % 4 == 3 {
            // Compaction: the blob replaces the epoch's entry bytes.
            added -= epoch_bytes as i64;
            replaced += epoch_bytes as i64;
            epoch_bytes = 0;
        } else {
            let slot = position % 4;
            if let Some(previous) = slots[slot] {
                added += paid(len).saturating_sub(paid(previous)) as i64
                    - (SLOT_KEY_PAID + paid(len)) as i64;
                replaced += paid(len).min(paid(previous)) as i64;
            }
            slots[slot] = Some(len);
        }
        assert_eq!(
            delta(&v4_ctx.cost, &legacy_ctx.cost),
            (added, replaced),
            "position {position} (len {len}): v4 {:?}\nlegacy {:?}",
            v4_ctx.cost,
            legacy_ctx.cost
        );
        assert_only_storage_differs(
            &v4_ctx.cost,
            &legacy_ctx.cost,
            &format!("position {position}"),
        );
    }
    assert_eq!(root_hash(&v4_db, &GROVE_V4), root_hash(&legacy_db, &legacy));
    assert_verifies(&v4_db, &GROVE_V4);
}

/// `PrivateDocumentStore` (fixed entry size) follows the same model. Its
/// append propagates the dense tree's costs, so V4 also bills the read that
/// sizes the slot rewrite: one seek per buffered append, loading the
/// committed entry from epoch 2 on.
#[test]
fn private_document_store_accounting_matches_model() {
    const CHUNK_POWER: u8 = 2; // epoch 4
    const ENTRY: u32 = 24;
    let legacy = legacy_accounting();
    let make = |version: &GroveVersion| {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"docs",
            Element::empty_private_document_store(ENTRY, CHUNK_POWER).expect("valid config"),
            None,
            None,
            version,
        )
        .unwrap()
        .expect("insert private document store");
        db
    };
    let legacy_db = make(&legacy);
    let v4_db = make(&GROVE_V4);

    for position in 0..12u64 {
        let entry = vec![position as u8 + 1; ENTRY as usize];
        let legacy_ctx = legacy_db.private_document_store_insert(
            EMPTY_PATH,
            b"docs",
            entry.clone(),
            None,
            &legacy,
        );
        legacy_ctx.value.expect("legacy append");
        let v4_ctx =
            v4_db.private_document_store_insert(EMPTY_PATH, b"docs", entry, None, &GROVE_V4);
        v4_ctx.value.expect("v4 append");

        assert_eq!(
            delta(&v4_ctx.cost, &legacy_ctx.cost),
            expected_delta(position, CHUNK_POWER, ENTRY, false),
            "position {position}: v4 {:?}\nlegacy {:?}",
            v4_ctx.cost,
            legacy_ctx.cost
        );
        let compacting = position % 4 == 3;
        let (extra_seeks, extra_loaded) = if compacting {
            (0, 0)
        } else if position >= 4 {
            (1, ENTRY as u64)
        } else {
            (1, 0)
        };
        assert_eq!(
            v4_ctx.cost.seek_count,
            legacy_ctx.cost.seek_count + extra_seeks,
            "position {position}: the slot read before a buffered write"
        );
        assert_eq!(
            v4_ctx.cost.storage_loaded_bytes,
            legacy_ctx.cost.storage_loaded_bytes + extra_loaded,
            "position {position}: the committed entry loaded to size the rewrite"
        );
        assert_eq!(v4_ctx.cost.hash_node_calls, legacy_ctx.cost.hash_node_calls);
    }
    assert_eq!(root_hash(&v4_db, &GROVE_V4), root_hash(&legacy_db, &legacy));
    assert_verifies(&v4_db, &GROVE_V4);
}

/// The standalone `MmrTree` and dense tree never rewrite a key (every
/// append lands on a fresh position), so their accounting is untouched:
/// identical costs with the gates on and off.
#[test]
fn standalone_mmr_and_dense_trees_are_unaffected() {
    let legacy = legacy_accounting();
    let make = |version: &GroveVersion| {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            version,
        )
        .unwrap()
        .expect("insert mmr tree");
        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(3),
            None,
            None,
            version,
        )
        .unwrap()
        .expect("insert dense tree");
        db
    };
    let legacy_db = make(&legacy);
    let v4_db = make(&GROVE_V4);
    for i in 0..6u8 {
        let value = vec![i + 1; 10 + i as usize];
        let a = legacy_db
            .mmr_tree_append(EMPTY_PATH, b"mmr", value.clone(), None, &legacy)
            .cost;
        let b = v4_db
            .mmr_tree_append(EMPTY_PATH, b"mmr", value.clone(), None, &GROVE_V4)
            .cost;
        assert_eq!(a, b, "mmr append {i}");
        let a = legacy_db
            .dense_tree_insert(EMPTY_PATH, b"dense", value.clone(), None, &legacy)
            .cost;
        let b = v4_db
            .dense_tree_insert(EMPTY_PATH, b"dense", value, None, &GROVE_V4)
            .cost;
        assert_eq!(a, b, "dense insert {i}");
    }
}
