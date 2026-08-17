//! `estimated >= actual` property tests for `CommitmentTreeInsert`.
//!
//! Downstream consumers (Dash Platform admission control) use the estimated
//! cost as the bound deciding whether a transaction is adequately funded,
//! then re-meter with the real cost during execution. If actual ever exceeds
//! estimated, an underfunded transaction is admitted and fails mid-execution
//! (issue #812 — two mainnet chain stalls). These tests pin the invariant the
//! consumers rely on: for every cost dimension, both the average-case and the
//! worst-case estimate of a single `CommitmentTreeInsert` dominate the actual
//! apply cost, across tree positions.
//!
//! Position coverage is deliberately adversarial, not random:
//! - positions `2^k - 1` maximize the Sinsemilla ommer cascade
//!   (`trailing_ones(position)`),
//! - positions crossing a `2^chunk_power` boundary trigger epoch compaction
//!   (the whole chunk blob is written by that single append).
//!
//! Both are deterministic and cheaply reachable by an adversary choosing when
//! to append, so they must be covered by the estimate, not treated as tail
//! cases.

use std::collections::HashMap;

use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};
use grovedb_costs::{storage_cost::removal::StorageRemovedBytes::NoStorageRemoval, OperationCost};
use grovedb_merk::{
    estimated_costs::{
        average_case_costs::{
            EstimatedLayerCount::EstimatedLevel, EstimatedLayerInformation,
            EstimatedLayerSizes::AllSubtrees, EstimatedSumTrees::NoSumTrees,
        },
        worst_case_costs::WorstCaseLayerInformation::MaxElementsNumber,
    },
    tree_type::TreeType,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{
        estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
        KeyInfoPath, QualifiedGroveDbOp,
    },
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, GroveDb,
};

/// Deterministic valid Pallas field element from an index.
fn test_cmx(index: u32) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&index.to_le_bytes());
    // Clear the top bit so the bytes stay below the Pallas modulus.
    bytes[31] &= 0x7f;
    bytes
}

/// Deterministic 216-byte DashMemo ciphertext from an index.
fn test_ciphertext(index: u32) -> TransmittedNoteCiphertext<DashMemo> {
    let mut epk_bytes = [0u8; 32];
    epk_bytes[..4].copy_from_slice(&index.to_le_bytes());
    let mut enc_data = [0u8; 104];
    enc_data[..4].copy_from_slice(&index.to_le_bytes());
    let mut out_ciphertext = [0u8; 80];
    out_ciphertext[..4].copy_from_slice(&index.to_le_bytes());
    TransmittedNoteCiphertext::from_parts(epk_bytes, NoteBytesData(enc_data), out_ciphertext)
}

/// A single CommitmentTreeInsert op for the tree at root key `pool`.
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

/// Average-case estimate for a batch of `ops` against a root layer holding a
/// handful of subtrees.
fn average_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
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
    GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("expected to compute average case costs for CommitmentTreeInsert")
}

/// Worst-case estimate for a batch of `ops` against a small root layer.
fn worst_case_estimate(
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
    .expect("expected to compute worst case costs for CommitmentTreeInsert")
}

/// Assert both estimators dominate `actual` in every cost dimension.
fn assert_estimates_dominate(
    position: u64,
    chunk_power: u8,
    average: &OperationCost,
    worst: &OperationCost,
    actual: &OperationCost,
) {
    assert!(
        average.worse_or_eq_than(actual),
        "average-case estimate must dominate actual at position {} (chunk_power {});\nestimated \
         {:?}\nactual {:?}",
        position,
        chunk_power,
        average,
        actual,
    );
    assert!(
        worst.worse_or_eq_than(actual),
        "worst-case estimate must dominate actual at position {} (chunk_power {});\nestimated \
         {:?}\nactual {:?}",
        position,
        chunk_power,
        worst,
        actual,
    );
}

/// Sweep every position in `0..36` plus deeper `2^k - 1` / `2^k` pairs with a
/// small epoch (chunk_power 4), so the sweep crosses several compaction
/// boundaries (15, 31, 63, ...) that coincide with maximal ommer cascades.
#[test]
fn test_commitment_tree_insert_estimated_covers_actual_positions_chunk_power_4() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    const CHUNK_POWER: u8 = 4;

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

    let mut targets: Vec<u64> = (0..36).collect();
    targets.extend([62, 63, 64, 65, 126, 127, 128, 254, 255, 256]);
    targets.sort_unstable();
    targets.dedup();

    let mut next_index: u32 = 0;
    for &target in &targets {
        // Seed the tree up to `target` (cheap bulk batch; correctness of the
        // seeded appends is covered by the commitment tree tests).
        if (next_index as u64) < target {
            let seed_ops: Vec<_> = ((next_index as u64)..target)
                .map(|i| ct_op(i as u32))
                .collect();
            next_index = target as u32;
            db.apply_batch(seed_ops, None, None, grove_version)
                .unwrap()
                .expect("seeding appends should succeed");
        }

        let op = ct_op(next_index);
        let average = average_case_estimate(vec![op.clone()], grove_version);
        let worst = worst_case_estimate(vec![op.clone()], grove_version);
        let actual = db.apply_batch(vec![op], None, None, grove_version).cost;
        next_index += 1;

        assert_estimates_dominate(target, CHUNK_POWER, &average, &worst, &actual);
    }
}

/// Cross the epoch boundary at the estimator's chunk-power cap
/// (`MAX_ESTIMATED_CHUNK_POWER` = 10): position 1022 maximizes the dense
/// buffer's per-append root recompute, and position 1023 triggers compaction
/// of a full 1024-entry epoch — the single most expensive append a
/// cap-conforming tree can produce.
#[test]
fn test_commitment_tree_insert_estimated_covers_actual_epoch_boundary_chunk_power_10() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    const CHUNK_POWER: u8 = 10;

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

    // Seed to position 1022 in one bulk batch.
    let seed_ops: Vec<_> = (0..1022).map(ct_op).collect();
    db.apply_batch(seed_ops, None, None, grove_version)
        .unwrap()
        .expect("seeding appends should succeed");

    for index in [1022u32, 1023, 1024] {
        let op = ct_op(index);
        let average = average_case_estimate(vec![op.clone()], grove_version);
        let worst = worst_case_estimate(vec![op.clone()], grove_version);
        let actual = db.apply_batch(vec![op], None, None, grove_version).cost;

        assert_estimates_dominate(index as u64, CHUNK_POWER, &average, &worst, &actual);
    }
}

/// A batch with several appends to the SAME tree must charge every append —
/// the ops share (path, key), and before the fix for issue #812 the batch
/// structure either dropped them entirely (keyless skip) or would have
/// collapsed them into a single map entry. The estimate for N ops must
/// therefore dominate N times the flat append cost, which it can only do if
/// each op is individually dispatched.
#[test]
fn test_commitment_tree_insert_estimate_charges_every_append_in_batch() {
    let grove_version = GroveVersion::latest();

    let one = average_case_estimate(vec![ct_op(0)], grove_version);
    let three = average_case_estimate(vec![ct_op(0), ct_op(1), ct_op(2)], grove_version);

    // Each additional op must contribute at least the flat append cost's
    // Sinsemilla component (the parent-node replacement may be shared).
    assert!(
        three.sinsemilla_hash_calls >= 3 * one.sinsemilla_hash_calls,
        "3-op estimate must charge Sinsemilla for every append; one={:?} three={:?}",
        one,
        three,
    );
    assert!(
        three.storage_cost.added_bytes >= 2 * one.storage_cost.added_bytes,
        "3-op estimate must charge storage for every append; one={:?} three={:?}",
        one,
        three,
    );

    let one_worst = worst_case_estimate(vec![ct_op(0)], grove_version);
    let three_worst = worst_case_estimate(vec![ct_op(0), ct_op(1), ct_op(2)], grove_version);
    assert!(
        three_worst.sinsemilla_hash_calls >= 3 * one_worst.sinsemilla_hash_calls,
        "3-op worst-case estimate must charge Sinsemilla for every append; one={:?} three={:?}",
        one_worst,
        three_worst,
    );
}

/// A batch spanning several appends must still be dominated by the estimate
/// when applied for real — the end-to-end shape of the mainnet failure (a
/// multi-action shielded transaction).
#[test]
fn test_commitment_tree_insert_estimated_covers_actual_multi_op_batch() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    const CHUNK_POWER: u8 = 4;

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

    // Seed so the batch below crosses the compaction boundary at 15.
    let seed_ops: Vec<_> = (0..14).map(ct_op).collect();
    db.apply_batch(seed_ops, None, None, grove_version)
        .unwrap()
        .expect("seeding appends should succeed");

    // A 4-op batch covering positions 14..=17 (compaction at 15).
    let ops: Vec<_> = (14..18).map(ct_op).collect();
    let average = average_case_estimate(ops.clone(), grove_version);
    let worst = worst_case_estimate(ops.clone(), grove_version);
    let actual = db.apply_batch(ops, None, None, grove_version).cost;

    assert_estimates_dominate(14, CHUNK_POWER, &average, &worst, &actual);
}

/// The keyless-op fix covers every append-only op type, not just
/// `CommitmentTreeInsert`: MMR, bulk-append, and dense-tree appends must also
/// reach their cost arms instead of estimating as free.
#[test]
fn test_other_keyless_append_ops_reach_estimation() {
    let grove_version = GroveVersion::latest();

    let ops_for = |op: QualifiedGroveDbOp| vec![op];

    for (name, op) in [
        (
            "MmrTreeAppend",
            QualifiedGroveDbOp::mmr_tree_append_op(vec![b"mmr".to_vec()], vec![1u8; 64]),
        ),
        (
            "BulkAppend",
            QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], vec![2u8; 64]),
        ),
        (
            "DenseTreeInsert",
            QualifiedGroveDbOp::dense_tree_insert_op(vec![b"dense".to_vec()], vec![3u8; 64]),
        ),
    ] {
        let average = average_case_estimate(ops_for(op.clone()), grove_version);
        assert!(
            average.seek_count > 0 && average.storage_cost.added_bytes > 0,
            "{name} average-case estimate must be non-zero, got {average:?}",
        );
        let worst = worst_case_estimate(ops_for(op), grove_version);
        assert!(
            worst.seek_count > 0 && worst.storage_cost.added_bytes > 0,
            "{name} worst-case estimate must be non-zero, got {worst:?}",
        );
    }
}
