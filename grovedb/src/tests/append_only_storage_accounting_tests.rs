//! Storage accounting of the append-only family (issue #822).
//!
//! Under `bulk_append_tree_versions.cost.storage_accounting = 1` (GROVE_V4)
//! each entry's permanent bytes are charged as **added** by the append that
//! creates it, and the churn — the compaction's chunk blob (which supersedes
//! the pre-paid buffer entries) and the frontier rewrite (one value
//! overwritten in place) — is reported as **replaced**. Under the shipped
//! report (V1..V3) every data put is key + value as added, so the
//! compacting append is billed the whole epoch's bytes as new storage.
//!
//! The assertions are structural (ratios and floors) rather than exact byte
//! counts, so they pin the accounting model without freezing framing
//! constants; the exact figures are covered by the estimate-dominates
//! property tests, which must keep holding under the new report.

use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};
use grovedb_costs::{storage_cost::StorageCost, CostContext};
use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element,
};

/// cmx (32) + rho (32) + cv_net (32) + 216-byte DashMemo ciphertext.
const ENTRY_SIZE: u32 = 312;
const CHUNK_POWER: u8 = 4;
const EPOCH: u32 = 1 << CHUNK_POWER as u32;

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

fn ct_op(index: u32) -> QualifiedGroveDbOp {
    let mut rho = [0u8; 32];
    rho[..4].copy_from_slice(&index.to_le_bytes());
    rho[4] = 0xAA;
    let mut cv_net = [0u8; 32];
    cv_net[..4].copy_from_slice(&index.to_le_bytes());
    cv_net[4] = 0xCC;
    QualifiedGroveDbOp::commitment_tree_insert_op_typed(
        vec![b"pool".to_vec()],
        test_cmx(index),
        rho,
        cv_net,
        &test_ciphertext(index),
    )
}

/// A fresh db with an empty commitment tree at `pool`, plus the per-append
/// storage cost of applying `count` sequential inserts under `grove_version`.
fn per_append_storage(grove_version: &GroveVersion, count: u32) -> (TempGroveDb, Vec<StorageCost>) {
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");
    let mut costs = Vec::with_capacity(count as usize);
    for index in 0..count {
        let CostContext { value, cost } =
            db.apply_batch(vec![ct_op(index)], None, None, grove_version);
        value.expect("append should succeed");
        costs.push(cost.storage_cost);
    }
    (db, costs)
}

/// v1: the compacting append reports the epoch's entries as replaced, not
/// as an epoch-sized spike of new storage; its added bytes stay in the same
/// band as an ordinary append.
#[test]
fn v1_compaction_is_replacement_not_added_storage() {
    let (_db, costs) = per_append_storage(&GROVE_V4, EPOCH + 2);
    let compacting = &costs[(EPOCH - 1) as usize];
    let ordinary = &costs[(EPOCH - 2) as usize];

    assert!(
        compacting.replaced_bytes >= EPOCH * ENTRY_SIZE,
        "the compacting append must report the whole epoch's entries as replaced: \
         replaced {} < {}",
        compacting.replaced_bytes,
        EPOCH * ENTRY_SIZE
    );
    assert!(
        compacting.added_bytes < compacting.replaced_bytes / 4,
        "the compacting append's added bytes must not scale with the epoch: added {} vs \
         replaced {}",
        compacting.added_bytes,
        compacting.replaced_bytes
    );
    // An ordinary append adds its entry (+ framing share); the compacting
    // append adds that plus the blob residual and a few MMR nodes — same
    // order of magnitude, never an epoch multiple.
    assert!(
        compacting.added_bytes < 4 * ordinary.added_bytes,
        "compacting added {} vs ordinary added {}",
        compacting.added_bytes,
        ordinary.added_bytes
    );
}

/// v1: every append — first epoch and later — is charged its entry as added
/// (the entry's permanent bytes, paid once by the append that creates it),
/// and from the second append on the frontier rewrite shows up as replaced.
#[test]
fn v1_every_append_pays_its_entry_once_and_frontier_rewrites_are_replaced() {
    let (_db, costs) = per_append_storage(&GROVE_V4, 2 * EPOCH + 2);
    for (index, cost) in costs.iter().enumerate() {
        assert!(
            cost.added_bytes >= ENTRY_SIZE,
            "append #{index} must add at least its entry: added {}",
            cost.added_bytes
        );
    }
    // Second-epoch appends overwrite a stale buffer slot at an existing key:
    // they still add the entry but not a new key, so they cannot exceed the
    // first epoch's corresponding append.
    for i in 0..(EPOCH - 1) as usize {
        let first = costs[i].added_bytes;
        let second = costs[i + EPOCH as usize].added_bytes;
        assert!(
            second <= first,
            "second-epoch append #{} added {} > first-epoch #{} added {}",
            i + EPOCH as usize,
            second,
            i,
            first
        );
    }
    // The frontier is one value rewritten in place: after the first save
    // every append replaces at least the minimal serialized frontier.
    const MIN_FRONTIER_LEN: u32 = 42;
    for (index, cost) in costs.iter().enumerate().skip(1) {
        assert!(
            cost.replaced_bytes >= MIN_FRONTIER_LEN,
            "append #{index} must report the frontier rewrite as replaced: replaced {}",
            cost.replaced_bytes
        );
    }
}

/// v0 (V1..V3, the shipped report): the compacting append is billed the
/// epoch's bytes as new storage — pinned so the legacy figure cannot drift
/// under live versions.
#[test]
fn v0_compaction_is_billed_as_new_storage() {
    let (_db, costs) = per_append_storage(&GROVE_V3, EPOCH + 1);
    let compacting = &costs[(EPOCH - 1) as usize];
    let ordinary = &costs[(EPOCH - 2) as usize];
    assert!(
        compacting.added_bytes >= EPOCH * ENTRY_SIZE,
        "v0 must bill the blob as added: added {} < {}",
        compacting.added_bytes,
        EPOCH * ENTRY_SIZE
    );
    assert!(
        compacting.added_bytes > 8 * ordinary.added_bytes,
        "v0 compaction spike expected: {} vs {}",
        compacting.added_bytes,
        ordinary.added_bytes
    );
}

/// The two reports differ only in how bytes are labelled: stored bytes,
/// chunks and roots are identical, so the root hash after the same appends
/// agrees across versions.
#[test]
fn accounting_versions_leave_state_and_roots_identical() {
    let (db_v3, _) = per_append_storage(&GROVE_V3, 2 * EPOCH + 3);
    let (db_v4, _) = per_append_storage(&GROVE_V4, 2 * EPOCH + 3);
    let root_v3 = db_v3.root_hash(None, &GROVE_V3).unwrap().expect("root v3");
    let root_v4 = db_v4.root_hash(None, &GROVE_V4).unwrap().expect("root v4");
    assert_eq!(
        root_v3, root_v4,
        "storage accounting must not change stored bytes or roots"
    );
}

/// Across a full epoch, v1's total added bytes are within a small factor of
/// the bytes that persist (entries + framing + a few MMR/frontier bytes),
/// whereas v0 charges roughly twice that by billing the blob copy as new.
#[test]
fn v1_epoch_total_added_tracks_permanent_bytes_while_v0_double_counts() {
    let (_, v4) = per_append_storage(&GROVE_V4, EPOCH);
    let (_, v3) = per_append_storage(&GROVE_V3, EPOCH);
    let total = |costs: &[StorageCost]| -> u64 { costs.iter().map(|c| c.added_bytes as u64).sum() };
    let permanent_floor = (EPOCH * ENTRY_SIZE) as u64;
    let v4_total = total(&v4);
    let v3_total = total(&v3);
    assert!(
        v4_total >= permanent_floor && v4_total < permanent_floor * 3 / 2,
        "v1 epoch total added {v4_total} should be within 1.5x of the permanent entry bytes \
         {permanent_floor}"
    );
    assert!(
        v3_total >= 2 * permanent_floor,
        "v0 epoch total added {v3_total} should double-count the entries ({permanent_floor})"
    );
}
