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
use grovedb_costs::{
    storage_cost::removal::StorageRemovedBytes::NoStorageRemoval, CostContext, OperationCost,
};
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

/// Average-case estimation result for a batch of `ops` against a root layer
/// holding a handful of subtrees. When `declared_chunk_power` is set, the
/// commitment tree's own layer is declared with
/// `TreeType::CommitmentTree(chunk_power)` — the shape Dash Platform
/// registers, and the declaration the estimator REQUIRES for
/// CommitmentTreeInsert ops.
fn try_average_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
    declared_chunk_power: Option<u8>,
    grove_version: &GroveVersion,
) -> Result<OperationCost, crate::Error> {
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
            KeyInfoPath::from_known_owned_path(vec![b"pool".to_vec()]),
            EstimatedLayerInformation {
                tree_type: TreeType::CommitmentTree(chunk_power),
                estimated_layer_count: EstimatedLevel(16, false),
                estimated_layer_sizes: AllItems(8, 312, None),
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
}

/// Average-case estimate with the commitment tree's layer declared.
fn average_case_estimate_with_layers(
    ops: Vec<QualifiedGroveDbOp>,
    declared_chunk_power: Option<u8>,
    grove_version: &GroveVersion,
) -> OperationCost {
    try_average_case_estimate(ops, declared_chunk_power, grove_version)
        .expect("expected to compute average case costs for CommitmentTreeInsert")
}

/// Average-case estimate without the commitment tree's own layer declared —
/// valid for non-commitment-tree ops and for grove versions that skip
/// keyless ops.
fn average_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> OperationCost {
    average_case_estimate_with_layers(ops, None, grove_version)
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
        let average =
            average_case_estimate_with_layers(vec![op.clone()], Some(CHUNK_POWER), grove_version);
        let worst = worst_case_estimate(vec![op.clone()], grove_version);
        let CostContext {
            value,
            cost: actual,
        } = db.apply_batch(vec![op], None, None, grove_version);
        value.expect("append should succeed");
        next_index += 1;

        assert_estimates_dominate(target, CHUNK_POWER, &average, &worst, &actual);
    }
}

/// Cross the epoch boundary at chunk_power 11 — the value Dash Platform's
/// shielded notes pool uses: position 2046 maximizes the dense buffer's
/// per-append root recompute, and position 2047 triggers compaction of a
/// full 2048-entry epoch — the single most expensive append such a tree
/// can produce.
#[test]
fn test_commitment_tree_insert_estimated_covers_actual_epoch_boundary_chunk_power_11() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    const CHUNK_POWER: u8 = 11;
    const EPOCH: u32 = 1 << CHUNK_POWER as u32;

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

    // Seed to two positions before the compaction boundary in one bulk batch.
    let seed_ops: Vec<_> = (0..EPOCH - 2).map(ct_op).collect();
    db.apply_batch(seed_ops, None, None, grove_version)
        .unwrap()
        .expect("seeding appends should succeed");

    for index in [EPOCH - 2, EPOCH - 1, EPOCH] {
        let op = ct_op(index);
        let average =
            average_case_estimate_with_layers(vec![op.clone()], Some(CHUNK_POWER), grove_version);
        let worst = worst_case_estimate(vec![op.clone()], grove_version);
        let CostContext {
            value,
            cost: actual,
        } = db.apply_batch(vec![op], None, None, grove_version);
        value.expect("append should succeed");

        assert_estimates_dominate(index as u64, CHUNK_POWER, &average, &worst, &actual);
    }
}

/// Declaring the tree's own layer (as Dash Platform does) makes the
/// average-case estimate use the tree's actual epoch scale: at a small
/// chunk power the declared estimate is far tighter than the cap-based
/// fallback, while still dominating the actual cost at the compaction
/// position — the most expensive append such a tree can produce.
#[test]
fn test_commitment_tree_insert_declared_chunk_power_tightens_estimate() {
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

    // Seed to one position before the compaction boundary at 15.
    let seed_ops: Vec<_> = (0..15).map(ct_op).collect();
    db.apply_batch(seed_ops, None, None, grove_version)
        .unwrap()
        .expect("seeding appends should succeed");

    let op = ct_op(15);
    let declared =
        average_case_estimate_with_layers(vec![op.clone()], Some(CHUNK_POWER), grove_version);
    let worst = worst_case_estimate(vec![op.clone()], grove_version);
    let CostContext {
        value,
        cost: actual,
    } = db.apply_batch(vec![op], None, None, grove_version);
    value.expect("compaction append should succeed");

    // Far tighter than the worst-case physical-ceiling assumption
    // (2^4 vs 2^16 epoch). The epoch scale lives in the replaced-bytes
    // term (the compaction's blob supersedes the epoch's entries); the
    // added-bytes term is per-append and does not scale with the epoch.
    assert!(
        declared.storage_cost.replaced_bytes < worst.storage_cost.replaced_bytes / 100,
        "declared estimate should be far tighter than the physical-ceiling worst case; declared \
         {declared:?}\nworst {worst:?}",
    );
    // ...while still an upper bound of the compaction append.
    assert!(
        declared.worse_or_eq_than(&actual),
        "declared-chunk-power estimate must dominate actual at the compaction \
         position;\nestimated {declared:?}\nactual {actual:?}",
    );
}

/// The average-case estimator REQUIRES the commitment tree's own layer to be
/// declared: an undeclared CommitmentTreeInsert estimation fails loudly
/// instead of silently guessing an epoch scale that could under-bound (too
/// small) or grotesquely over-reserve (the physical ceiling).
#[test]
fn test_commitment_tree_insert_estimation_requires_declared_layer() {
    let grove_version = GroveVersion::latest();
    let result = try_average_case_estimate(vec![ct_op(0)], None, grove_version);
    assert!(
        result.is_err(),
        "undeclared CommitmentTreeInsert estimation must error, got {result:?}",
    );
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

    let one = average_case_estimate_with_layers(vec![ct_op(0)], Some(11), grove_version);
    let three = average_case_estimate_with_layers(
        vec![ct_op(0), ct_op(1), ct_op(2)],
        Some(11),
        grove_version,
    );

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
    let average = average_case_estimate_with_layers(ops.clone(), Some(CHUNK_POWER), grove_version);
    let worst = worst_case_estimate(ops.clone(), grove_version);
    let CostContext {
        value,
        cost: actual,
    } = db.apply_batch(ops, None, None, grove_version);
    value.expect("multi-op batch should succeed");

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

/// Replay guarantee: on grove versions at or below V3 the estimated-cost
/// batch structure must keep SKIPPING keyless append ops — the append
/// contributes zero, exactly the (under-counting) estimate historical
/// blocks were admitted under. The new cost dispatch is V4-gated
/// (`apply_batch.keyless_op_cost_dispatch`); flipping it for old versions
/// would change historical admission bounds and brick chain sync replay.
#[test]
fn test_keyless_append_ops_still_estimate_as_free_before_v4() {
    use grovedb_version::version::v3::GROVE_V3;

    for op in [
        ct_op(0),
        QualifiedGroveDbOp::mmr_tree_append_op(vec![b"mmr".to_vec()], vec![1u8; 64]),
        QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], vec![2u8; 64]),
        QualifiedGroveDbOp::dense_tree_insert_op(vec![b"dense".to_vec()], vec![3u8; 64]),
    ] {
        let average = average_case_estimate(vec![op.clone()], &GROVE_V3);
        assert_eq!(
            average,
            OperationCost::default(),
            "V3 average-case estimate for a keyless append op must stay zero (op {op:?})",
        );
        let worst = worst_case_estimate(vec![op.clone()], &GROVE_V3);
        assert_eq!(
            worst,
            OperationCost::default(),
            "V3 worst-case estimate for a keyless append op must stay zero (op {op:?})",
        );
    }
}

/// A commitment tree with large caller-supplied flags: the preprocessing
/// read loads the flags too, so the estimate's element-load bound must
/// cover them. The average-case estimator derives the bound from the
/// parent layer's declared flags size (the same metadata the parent-node
/// replace uses); the worst-case estimator assumes the largest Merk
/// value. Before this bound existed, flags above a fixed 512-byte
/// allowance broke `estimated >= actual` on `storage_loaded_bytes`.
#[test]
fn test_commitment_tree_insert_estimated_covers_actual_with_large_flags() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    const CHUNK_POWER: u8 = 4;
    const FLAGS_LEN: usize = 2000;

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree_with_flags(CHUNK_POWER, Some(vec![7u8; FLAGS_LEN]))
            .expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree with flags");

    let op = ct_op(0);

    // Average case with the flags size declared in the parent layer and the
    // commitment tree's own layer declared with its chunk power.
    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLevel(1, false),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, Some(FLAGS_LEN as u32)),
        },
    );
    paths.insert(
        KeyInfoPath::from_known_owned_path(vec![b"pool".to_vec()]),
        EstimatedLayerInformation {
            tree_type: TreeType::CommitmentTree(CHUNK_POWER),
            estimated_layer_count: EstimatedLevel(16, false),
            estimated_layer_sizes: AllItems(8, 312, None),
        },
    );
    let average = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        vec![op.clone()],
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("expected average case costs with declared flags size");

    let worst = worst_case_estimate(vec![op.clone()], grove_version);
    let CostContext {
        value,
        cost: actual,
    } = db.apply_batch(vec![op], None, None, grove_version);
    value.expect("append to flagged tree should succeed");

    assert_estimates_dominate(0, CHUNK_POWER, &average, &worst, &actual);
}
