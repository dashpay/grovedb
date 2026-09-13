//! Storage accounting of the append-only tree family (issue #822).
//!
//! From GROVE_V4 an append charges each entry's permanent bytes once and
//! reports write churn as replacement:
//!
//! - every append charges the entry's chunk-blob share (its own bytes) as
//!   `added_bytes`;
//! - a dense-buffer slot that already holds a committed value (epoch 2 on)
//!   is read (one billed seek plus the committed bytes) and its rewrite is
//!   `replaced_bytes`, growth added, shrink not credited;
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
//! counts and the Sinsemilla work are identical on both sides; the only
//! I/O difference is the committed-slot read). The legacy figures
//! themselves are then pinned by the model below.

use std::collections::HashMap;

use grovedb_commitment_tree::{
    CommitmentFrontier, DashMemo, NoteBytesData, TransmittedNoteCiphertext,
};
use grovedb_costs::{storage_cost::removal::StorageRemovedBytes::NoStorageRemoval, OperationCost};
use grovedb_merk::{
    estimated_costs::{
        average_case_costs::{
            EstimatedLayerCount::EstimatedLevel,
            EstimatedLayerInformation,
            EstimatedLayerSizes::{AllItems, AllSubtrees},
            EstimatedSumTrees::NoSumTrees,
        },
        worst_case_costs::WorstCaseLayerInformation::MaxElementsNumber,
    },
    tree_type::TreeType,
};
use grovedb_version::version::{
    v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4, GroveVersion,
};

use crate::{
    batch::{
        estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
        KeyInfoPath, QualifiedGroveDbOp,
    },
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element, GroveDb,
};

/// A stored note entry: cmx (32) || rho (32) || cv_net (32) || DashMemo
/// ciphertext payload (216).
const NOTE_ENTRY: u32 = 312;

/// A path record for a buffer of `chunk_power`: generation (8) + present
/// mask (2) + value hash (32) + one 32-byte entry per level.
fn record_len(chunk_power: u8) -> u32 {
    grovedb_dense_fixed_sized_merkle_tree::path_record_len(chunk_power) as u32
}

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
/// Every figure — storage, loaded bytes, seeks, hashes, Sinsemilla — must be
/// identical: the fixed model, whatever the position (the compacting
/// append's commit-time puts are prepaid and their seeks amortized into
/// every append, so not even the seek count moves).
fn assert_fixed(what: &str, base: &OperationCost, cost: &OperationCost) {
    assert_eq!(
        cost, base,
        "{what}: every figure must be the fixed model;\nbase {base:?}\ncost {cost:?}"
    );
}

/// The dense buffer's fixed root-maintenance model for `chunk_power`.
fn buffer_model(chunk_power: u8) -> OperationCost {
    grovedb_dense_fixed_sized_merkle_tree::v1_insert_model_cost(chunk_power)
}

/// The compaction overhead share an append is charged at `chunk_power`.
fn amortized(chunk_power: u8) -> u32 {
    grovedb_bulk_append_tree::amortized_compaction_added_bytes(1u64 << chunk_power)
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
/// two and a half epochs: EVERY append is charged the same — the note's
/// long-term footprint as added storage, the buffer slot / path record /
/// blob-rewrite part / frontier as replaced, the buffer's fixed model and
/// the frontier's fixed model in hashes and bytes — at every position,
/// including the compactions at 15, 31 and 47 (their blob and MMR node puts
/// are prepaid, their seeks amortized into every append).
#[test]
fn commitment_tree_append_cost_is_fixed_across_positions() {
    const CHUNK_POWER: u8 = 4;
    let db = ct_db(CHUNK_POWER, &GROVE_V4);
    let base = ct_insert(&db, 0, &GROVE_V4);
    for position in 1..48u64 {
        let cost = ct_insert(&db, position as u32, &GROVE_V4);
        assert_fixed(&format!("position {position}"), &base, &cost);
    }
    // What the fixed charge is made of.
    let model = buffer_model(CHUNK_POWER);
    assert_eq!(
        base.sinsemilla_hash_calls,
        grovedb_commitment_tree::MODEL_FRONTIER_APPEND_SINSEMILLA_HASHES
    );
    // Buffer model + amortized compaction + bulk state root + ct_state root
    // (+ the parent Merk's own hashing, which does not depend on the
    // position either).
    assert!(base.hash_node_calls >= model.hash_node_calls + 1 + 2);
    // Added: the share plus the amortized compaction overhead (plus the
    // parent Merk's own growth, none here); the parent element's replaced
    // bytes sit on top of the append's own churn.
    let own_added = NOTE_ENTRY + amortized(CHUNK_POWER);
    assert!(
        base.storage_cost.added_bytes >= own_added
            && base.storage_cost.added_bytes < own_added + 64,
        "{base:?}"
    );
    let own_replaced = paid(NOTE_ENTRY)
        + paid(record_len(CHUNK_POWER))
        + NOTE_ENTRY
        + paid(grovedb_commitment_tree::MODEL_FRONTIER_SERIALIZED_LEN);
    assert!(base.storage_cost.replaced_bytes >= own_replaced, "{base:?}");
    assert_verifies(&db, &GROVE_V4);
}

/// The same at chunk_power 11 — the shielded pool's scale — around the
/// epoch boundary: the last buffered append, the compacting one and the
/// first of the next epoch cost the same.
#[test]
fn commitment_tree_epoch_boundary_at_chunk_power_11_is_charged_the_fixed_model() {
    const CHUNK_POWER: u8 = 11;
    const EPOCH: u32 = 1 << CHUNK_POWER as u32;
    let db = ct_db(CHUNK_POWER, &GROVE_V4);
    let seed: Vec<_> = (0..EPOCH - 2).map(ct_op).collect();
    db.apply_batch(seed, None, None, &GROVE_V4)
        .unwrap()
        .expect("seed to two before the boundary");
    let before = ct_insert(&db, EPOCH - 2, &GROVE_V4);
    let compacting = ct_insert(&db, EPOCH - 1, &GROVE_V4);
    let after = ct_insert(&db, EPOCH, &GROVE_V4);
    assert_fixed("compacting append", &before, &compacting);
    assert_fixed("first of the next epoch", &before, &after);
    // No 640 KB anywhere: the blob is prepaid a share at a time.
    assert!(
        compacting.storage_cost.replaced_bytes < 4_000
            && compacting.storage_cost.added_bytes < 1_000,
        "{compacting:?}"
    );
}

/// The frontier is charged its fixed model at every position: the
/// ommer-heavy positions `2^k - 1` and the collapsing `2^k` cost the same
/// Sinsemilla hashes and the same frontier bytes as any other.
#[test]
fn frontier_is_charged_the_fixed_model_at_every_position() {
    const CHUNK_POWER: u8 = 11; // nothing compacts in 0..34
    let db = ct_db(CHUNK_POWER, &GROVE_V4);
    let base = ct_insert(&db, 0, &GROVE_V4);
    for position in 1..34u32 {
        let cost = ct_insert(&db, position, &GROVE_V4);
        assert_fixed(&format!("position {position}"), &base, &cost);
    }
    assert_eq!(base.sinsemilla_hash_calls, 33);
    // The actual frontier at position 33 is 42 + 32·2 = 106 bytes; the
    // charge is the model's 554 + varint, replaced.
    assert_eq!(frontier_len(33), 106);
    assert!(
        base.storage_loaded_bytes >= grovedb_commitment_tree::MODEL_FRONTIER_SERIALIZED_LEN as u64
    );
}

/// Over an epoch, a note's `added_bytes` are its long-term footprint and
/// nothing else: the same every append, and the sum over the epoch is the
/// epoch's entry bytes plus the amortized overhead — not the legacy's
/// "every note twice plus the whole blob".
#[test]
fn added_bytes_over_an_epoch_are_the_long_term_footprint() {
    const CHUNK_POWER: u8 = 4;
    const EPOCH: u32 = 1 << CHUNK_POWER as u32;
    let v4_db = ct_db(CHUNK_POWER, &GROVE_V4);
    let legacy_db = ct_db(CHUNK_POWER, &GROVE_V3);
    for position in 0..EPOCH {
        ct_insert(&v4_db, position, &GROVE_V4);
        ct_insert(&legacy_db, position, &GROVE_V3);
    }
    let mut v4_added: u64 = 0;
    let mut legacy_added: u64 = 0;
    let mut per_append = None;
    for position in EPOCH..2 * EPOCH {
        let v4 = ct_insert(&v4_db, position, &GROVE_V4)
            .storage_cost
            .added_bytes;
        assert_eq!(*per_append.get_or_insert(v4), v4, "position {position}");
        v4_added += v4 as u64;
        legacy_added += ct_insert(&legacy_db, position, &GROVE_V3)
            .storage_cost
            .added_bytes as u64;
    }
    let footprint = EPOCH as u64 * (NOTE_ENTRY + amortized(CHUNK_POWER)) as u64;
    assert!(
        v4_added >= footprint && v4_added < footprint + 64 * EPOCH as u64,
        "v4 {v4_added} vs footprint {footprint}"
    );
    // Legacy: 15 slot rewrites (key + note), the whole blob, 16 frontier
    // saves — several times the footprint.
    assert!(
        legacy_added > 2 * footprint,
        "legacy charges every note twice over its life: legacy {legacy_added}, v4 {v4_added}"
    );
    assert_eq!(
        root_hash(&v4_db, &GROVE_V4),
        root_hash(&legacy_db, &GROVE_V3)
    );
}

/// A commitment tree that compacted under GROVE_V3 has no persisted MMR
/// root. Its first V4 append backfills the key (one extra commit-time put,
/// prepaid) and is otherwise charged the fixed model; every V4 append after
/// it is charged exactly the model, reading the key and never the peaks.
#[test]
fn legacy_commitment_tree_is_charged_the_fixed_model_after_one_backfill_put() {
    const CHUNK_POWER: u8 = 4;
    const EPOCH: u32 = 1 << CHUNK_POWER as u32;
    let db = ct_db(CHUNK_POWER, &GROVE_V3);
    for position in 0..EPOCH {
        ct_insert(&db, position, &GROVE_V3);
    }
    let first = ct_insert(&db, EPOCH, &GROVE_V4);
    let base = ct_insert(&db, EPOCH + 1, &GROVE_V4);
    // The backfill put is prepaid (no seek), so even this append is the model.
    assert_fixed("first V4 append (backfill put)", &base, &first);
    for position in EPOCH + 2..2 * EPOCH + 2 {
        let cost = ct_insert(&db, position, &GROVE_V4);
        assert_fixed(&format!("position {position}"), &base, &cost);
    }
    // The same figure as a tree that ran under V4 from the start.
    let v4_db = ct_db(CHUNK_POWER, &GROVE_V4);
    for position in 0..EPOCH + 2 {
        ct_insert(&v4_db, position, &GROVE_V4);
    }
    assert_fixed(
        "V4-only tree",
        &base,
        &ct_insert(&v4_db, EPOCH + 2, &GROVE_V4),
    );
    assert_eq!(root_hash(&db, &GROVE_V4), {
        for position in EPOCH + 3..2 * EPOCH + 2 {
            ct_insert(&v4_db, position, &GROVE_V4);
        }
        root_hash(&v4_db, &GROVE_V4)
    });
    assert_verifies(&db, &GROVE_V4);
}

/// An epoch boundary inside ONE batch on a fresh tree: the batch's cost is
/// the sum of the per-append fixed charges (slot and record keys written
/// twice in the batch are charged once — a `StorageBatch` keeps one put per
/// key — as churn), with no blob spike.
#[test]
fn epoch_boundary_inside_one_batch_is_the_sum_of_fixed_charges() {
    const CHUNK_POWER: u8 = 2; // epoch 4, capacity 3
    let db = ct_db(CHUNK_POWER, &GROVE_V4);
    let ops = (0..8u32).map(ct_op).collect::<Vec<_>>();
    let ctx = db.apply_batch(ops, None, None, &GROVE_V4);
    ctx.value.expect("v4 batch");
    let model = buffer_model(CHUNK_POWER);
    // 8 × (share + amortized) of added storage, plus the parent Merk's.
    let own_added = 8 * (NOTE_ENTRY + amortized(CHUNK_POWER));
    assert!(
        ctx.cost.storage_cost.added_bytes >= own_added
            && ctx.cost.storage_cost.added_bytes < own_added + 128,
        "{:?}",
        ctx.cost
    );
    // 8 × the model's reads (compacting appends included).
    assert!(
        ctx.cost.seek_count >= 8 * model.seek_count,
        "{:?}",
        ctx.cost
    );
    assert!(
        ctx.cost.storage_loaded_bytes >= 8 * model.storage_loaded_bytes,
        "{:?}",
        ctx.cost
    );
    assert_eq!(ctx.cost.sinsemilla_hash_calls, 8 * 33);
    assert_verifies(&db, &GROVE_V4);
}

/// A plain `BulkAppendTree` with VARIABLE-size values: the charge differs
/// between positions only by the value's own bytes — its share (added), its
/// slot and its blob-rewrite part (replaced) — never by the position.
#[test]
fn bulk_append_tree_cost_differs_only_by_the_value_bytes() {
    const CHUNK_POWER: u8 = 2; // epoch 4, capacity 3
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        &GROVE_V4,
    )
    .unwrap()
    .expect("insert bulk append tree");
    let sizes: [u32; 12] = [8, 8, 8, 8, 8, 16, 4, 8, 8, 8, 8, 8];
    let mut base: Option<OperationCost> = None;
    for (position, &len) in sizes.iter().enumerate() {
        let value = vec![position as u8 + 1; len as usize];
        let ctx = db.bulk_append(EMPTY_PATH, b"bulk", value, None, &GROVE_V4);
        ctx.value.expect("v4 append");
        let cost = ctx.cost;
        let base = base.get_or_insert(cost.clone());
        // Normalize the value-size terms away: added = share + amortized;
        // replaced = paid(slot) + record + share.
        let mut normalized = cost.clone();
        normalized.storage_cost.added_bytes = normalized.storage_cost.added_bytes + 8 - len;
        normalized.storage_cost.replaced_bytes =
            normalized.storage_cost.replaced_bytes + paid(8) + 8 - paid(len) - len;
        assert_fixed(
            &format!("position {position} (len {len})"),
            base,
            &normalized,
        );
    }
    assert_verifies(&db, &GROVE_V4);
}

/// `PrivateDocumentStore` (fixed entry size): the same fixed charge on every
/// append, the compacting one included.
#[test]
fn private_document_store_append_cost_is_fixed() {
    const CHUNK_POWER: u8 = 2; // epoch 4
    const ENTRY: u32 = 24;
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"docs",
        Element::empty_private_document_store(ENTRY, CHUNK_POWER).expect("valid config"),
        None,
        None,
        &GROVE_V4,
    )
    .unwrap()
    .expect("insert private document store");
    let mut base: Option<OperationCost> = None;
    for position in 0..12u64 {
        let entry = vec![position as u8 + 1; ENTRY as usize];
        let ctx = db.private_document_store_insert(EMPTY_PATH, b"docs", entry, None, &GROVE_V4);
        ctx.value.expect("v4 append");
        let cost = ctx.cost;
        let base = base.get_or_insert(cost.clone());
        assert_fixed(&format!("position {position}"), base, &cost);
    }
    assert_verifies(&db, &GROVE_V4);
}

/// The standalone `MmrTree` and dense tree never rewrite a key (every
/// append lands on a fresh position), so their accounting is untouched:
/// identical costs with the gates on and off.
/// GROVE_V4 with the append-only family's accounting gates switched off.
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

// ── BulkAppend estimates vs actual at compaction ─────────────────────

fn bulk_op(value: Vec<u8>) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], value)
}

/// Average-case estimate with the bulk tree's own layer declared as
/// `TreeType::BulkAppendTree(chunk_power)` (or not, when `None`).
fn bulk_average_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
    declared_chunk_power: Option<u8>,
    value_size: u32,
    grove_version: &GroveVersion,
) -> OperationCost {
    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLevel(1, false),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        },
    );
    if let Some(chunk_power) = declared_chunk_power {
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![b"bulk".to_vec()]),
            EstimatedLayerInformation {
                tree_type: TreeType::BulkAppendTree(chunk_power),
                estimated_layer_count: EstimatedLevel(16, false),
                estimated_layer_sizes: AllItems(8, value_size, None),
            },
        );
    }
    GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case estimate for BulkAppend")
}

fn bulk_worst_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> OperationCost {
    let mut paths = HashMap::new();
    paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(2));
    GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case estimate for BulkAppend")
}

/// Seed `seed` values into a fresh chunk_power-4 bulk tree, then apply
/// `last` as its own batch and return that batch's actual cost.
fn bulk_compaction_actual(seed: Vec<Vec<u8>>, last: Vec<u8>) -> OperationCost {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(4).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");
    db.apply_batch(
        seed.into_iter().map(bulk_op).collect(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("seed");
    let ctx = db.apply_batch(vec![bulk_op(last)], None, None, grove_version);
    ctx.value.expect("compaction append");
    ctx.cost
}

/// Under the fixed model the compaction blob is prepaid a share at a time,
/// so a compacting append over LARGE buffered values costs a tiny overflow
/// value exactly what any append of that value costs — and both estimators
/// dominate it without any epoch-sized term.
#[test]
fn bulk_append_estimates_dominate_actual_compaction_with_variable_sizes() {
    let grove_version = GroveVersion::latest();
    const BIG: usize = 10 * 1024;

    // Fifteen 10 KiB values, then a 16-byte overflow value that compacts
    // them: the 16-byte op is charged its own bytes, not the 150 KiB blob.
    let small = vec![9u8; 16];
    let actual_mixed =
        bulk_compaction_actual((0..15u8).map(|i| vec![i; BIG]).collect(), small.clone());
    assert!(
        (actual_mixed.storage_cost.replaced_bytes as usize) < BIG,
        "the blob is prepaid; the compacting op is charged its own churn only: {actual_mixed:?}"
    );
    let worst = bulk_worst_case_estimate(vec![bulk_op(small.clone())], grove_version);
    assert!(
        worst.worse_or_eq_than(&actual_mixed),
        "worst case must dominate a compaction over larger buffered values;\nestimated \
         {worst:?}\nactual {actual_mixed:?}"
    );
    let average_declared_small =
        bulk_average_case_estimate(vec![bulk_op(small.clone())], Some(4), 16, grove_version);
    assert!(
        average_declared_small.worse_or_eq_than(&actual_mixed),
        "declared average case must dominate the compacting op too;\nestimated \
         {average_declared_small:?}\nactual {actual_mixed:?}"
    );

    // Sixteen same-size values: the declared average-case estimate is an
    // upper bound of the compaction, and the worst case still dominates.
    let big = vec![15u8; BIG];
    let actual_same =
        bulk_compaction_actual((0..15u8).map(|i| vec![i; BIG]).collect(), big.clone());
    let average_declared = bulk_average_case_estimate(
        vec![bulk_op(big.clone())],
        Some(4),
        BIG as u32,
        grove_version,
    );
    assert!(
        average_declared.worse_or_eq_than(&actual_same),
        "declared average case must dominate a same-size compaction;\nestimated \
         {average_declared:?}\nactual {actual_same:?}"
    );
    let worst = bulk_worst_case_estimate(vec![bulk_op(big)], grove_version);
    assert!(
        worst.worse_or_eq_than(&actual_same),
        "worst case must dominate;\nestimated {worst:?}\nactual {actual_same:?}"
    );
}
