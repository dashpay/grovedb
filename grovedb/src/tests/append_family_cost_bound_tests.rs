//! `estimated >= actual` property tests for the append-only family under the
//! GROVE_V4 dense-buffer root maintenance (per-position hash records).
//!
//! The commitment tree's own sweeps live in
//! `commitment_tree_cost_bound_tests`; this file adds what the record
//! maintenance introduces:
//!
//! - the **catch-up insert**: a commitment tree whose buffer was filled under
//!   GROVE_V3 (no records) receives its first GROVE_V4 append, which derives
//!   every record it needs from the values — the one insert that still pays
//!   a full-buffer walk, and the reason the V4 estimate keeps that bound;
//! - **`PrivateDocumentStoreInsert`**, whose estimate is tightened to the
//!   record model (the store activates in V4, so no store ever pays the
//!   catch-up), swept across positions and an epoch boundary;
//! - **`BulkAppend`** and **`DenseTreeInsert`** across positions, so the
//!   record terms added to their arms are exercised against real applies.
//!
//! Every dimension of both estimators must dominate the actual apply cost at
//! every position — the invariant Dash Platform's admission control relies
//! on (issue #812).

use std::collections::HashMap;

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
use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

use crate::{
    batch::{
        estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
        KeyInfoPath, QualifiedGroveDbOp,
    },
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element, GroveDb,
};

// ── Estimation helpers ──────────────────────────────────────────────────

/// Average-case estimate against a small root layer, with the target tree's
/// own layer declared as `tree_type` (the declare-your-layers contract the
/// append-only estimators follow).
fn average_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
    key: &[u8],
    tree_type: TreeType,
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
    paths.insert(
        KeyInfoPath::from_known_owned_path(vec![key.to_vec()]),
        EstimatedLayerInformation {
            tree_type,
            estimated_layer_count: EstimatedLevel(16, false),
            estimated_layer_sizes: AllItems(8, value_size, None),
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
    .expect("average case estimate")
}

/// Worst-case estimate against a small root layer.
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
    .expect("worst case estimate")
}

/// Assert both estimators dominate `actual` in every cost dimension.
fn assert_estimates_dominate(
    what: &str,
    position: u64,
    average: &OperationCost,
    worst: &OperationCost,
    actual: &OperationCost,
) {
    assert!(
        average.worse_or_eq_than(actual),
        "{what}: average-case estimate must dominate actual at position {position};\nestimated \
         {average:?}\nactual {actual:?}",
    );
    assert!(
        worst.worse_or_eq_than(actual),
        "{what}: worst-case estimate must dominate actual at position {position};\nestimated \
         {worst:?}\nactual {actual:?}",
    );
}

/// Positions to sweep for a `chunk_power = 4` tree: every position of the
/// first epoch and a half, then deeper `2^k - 1` / `2^k` pairs, which cross
/// several compaction boundaries (15, 31, 63, ...).
fn sweep_targets() -> Vec<u64> {
    let mut targets: Vec<u64> = (0..36).collect();
    targets.extend([62, 63, 64, 65, 126, 127, 128, 254, 255, 256]);
    targets
}

/// Seed `db` with `ops_for(i)` for `i` in `next..target` in one batch, then
/// apply `ops_for(target)` on its own and return that apply's actual cost
/// together with the estimates made for it.
fn step(
    db: &TempGroveDb,
    next: &mut u64,
    target: u64,
    ops_for: &dyn Fn(u64) -> QualifiedGroveDbOp,
    estimate: &dyn Fn(QualifiedGroveDbOp) -> (OperationCost, OperationCost),
    grove_version: &GroveVersion,
) -> (OperationCost, OperationCost, OperationCost) {
    if *next < target {
        let seed_ops: Vec<_> = (*next..target).map(ops_for).collect();
        *next = target;
        db.apply_batch(seed_ops, None, None, grove_version)
            .unwrap()
            .expect("seeding appends should succeed");
    }
    let op = ops_for(*next);
    let (average, worst) = estimate(op.clone());
    let CostContext {
        value,
        cost: actual,
    } = db.apply_batch(vec![op], None, None, grove_version);
    value.expect("append should succeed");
    *next += 1;
    (average, worst, actual)
}

// ── Commitment tree: the GROVE_V3 → V4 catch-up insert ─────────────────

mod commitment_tree {
    use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};

    use super::*;

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

    fn ct_op(index: u64) -> QualifiedGroveDbOp {
        let index = index as u32;
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

    fn ct_db(chunk_power: u8, grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"pool",
            Element::empty_commitment_tree(chunk_power).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");
        db
    }

    /// Seed `legacy_fill` notes under GROVE_V3 (no hash records), then
    /// append under GROVE_V4 from there to past the next compaction. The
    /// first V4 append catches the buffer's records up from the values —
    /// the walk the V4 estimate keeps its hash bound for — and every
    /// append, catch-up included, must be dominated by the V4 estimates.
    fn catch_up_sweep(chunk_power: u8, legacy_fill: u64, v4_appends: u64) {
        let v4 = GroveVersion::latest();
        let db = ct_db(chunk_power, &GROVE_V3);
        let seed_ops: Vec<_> = (0..legacy_fill).map(ct_op).collect();
        db.apply_batch(seed_ops, None, None, &GROVE_V3)
            .unwrap()
            .expect("seeding under GROVE_V3 should succeed");
        let root_before = db.root_hash(None, v4).unwrap().unwrap();

        // The same sequence appended under V4 from the start, as the
        // reference root: the records change no root.
        let reference = ct_db(chunk_power, v4);
        let seed_ops: Vec<_> = (0..legacy_fill).map(ct_op).collect();
        reference
            .apply_batch(seed_ops, None, None, v4)
            .unwrap()
            .expect("seeding under GROVE_V4 should succeed");
        assert_eq!(
            reference.root_hash(None, v4).unwrap().unwrap(),
            root_before,
            "a buffer filled under V3 and one filled under V4 have the same root"
        );

        let mut next = legacy_fill;
        for index in legacy_fill..legacy_fill + v4_appends {
            let (average, worst, actual) = step(
                &db,
                &mut next,
                index,
                &ct_op,
                &|op| {
                    (
                        average_case_estimate(
                            vec![op.clone()],
                            b"pool",
                            TreeType::CommitmentTree(chunk_power),
                            312,
                            v4,
                        ),
                        worst_case_estimate(vec![op], v4),
                    )
                },
                v4,
            );
            assert_estimates_dominate(
                &format!("CommitmentTreeInsert after a V3 fill of {legacy_fill} (chunk_power {chunk_power})"),
                index,
                &average,
                &worst,
                &actual,
            );
            reference
                .apply_batch(vec![ct_op(index)], None, None, v4)
                .unwrap()
                .expect("reference append");
            assert_eq!(
                db.root_hash(None, v4).unwrap().unwrap(),
                reference.root_hash(None, v4).unwrap().unwrap(),
                "roots agree at position {index}"
            );
        }
    }

    /// chunk_power 4: a V3 buffer of 10 notes, then V4 appends through the
    /// compaction at 15 and into the next epoch (whose buffer is V4-born).
    #[test]
    fn test_commitment_tree_v4_estimates_cover_the_catch_up_insert_chunk_power_4() {
        catch_up_sweep(4, 10, 12);
    }

    /// chunk_power 11 — the shielded pool's value: a V3 buffer two short of
    /// full, so the first V4 append walks ≈ 2k positions to catch up, the
    /// next compacts the epoch, and the one after starts a V4-born buffer.
    #[test]
    fn test_commitment_tree_v4_estimates_cover_the_catch_up_insert_chunk_power_11() {
        catch_up_sweep(11, (1 << 11) - 2, 3);
    }

    /// The catch-up is physical work, not a charge: the first V4 append
    /// after a V3 fill (which walks the legacy buffer) and the next one are
    /// billed exactly the same — the buffer's fixed model plus the
    /// position-independent rest.
    #[test]
    fn test_commitment_tree_catch_up_is_not_billed() {
        const CHUNK_POWER: u8 = 8;
        const LEGACY_FILL: u64 = 200;
        let v4 = GroveVersion::latest();
        let db = ct_db(CHUNK_POWER, &GROVE_V3);
        let seed_ops: Vec<_> = (0..LEGACY_FILL).map(ct_op).collect();
        db.apply_batch(seed_ops, None, None, &GROVE_V3)
            .unwrap()
            .expect("seed under V3");

        let first = db.apply_batch(vec![ct_op(LEGACY_FILL)], None, None, v4);
        first.value.expect("catch-up append");
        let second = db.apply_batch(vec![ct_op(LEGACY_FILL + 1)], None, None, v4);
        second.value.expect("append after catch-up");

        // What may differ is the frontier (its ommer cascade and serialized
        // size depend on the global position): the blake3 count and the
        // seeks — where a walk of the legacy buffer would show — are the
        // model on both.
        assert_eq!(
            (first.cost.hash_node_calls, first.cost.seek_count),
            (second.cost.hash_node_calls, second.cost.seek_count),
            "catch-up must not change the charge: first {:?}, second {:?}",
            first.cost,
            second.cost
        );
    }
}

// ── PrivateDocumentStore ────────────────────────────────────────────────

mod private_document_store {
    use super::*;

    const ENTRY_SIZE: u32 = 48;

    fn pds_op(index: u64) -> QualifiedGroveDbOp {
        let mut entry = vec![0u8; ENTRY_SIZE as usize];
        entry[..8].copy_from_slice(&index.to_be_bytes());
        entry[8] = 0x5A;
        QualifiedGroveDbOp::private_document_store_insert_op(vec![b"docs".to_vec()], entry)
    }

    fn pds_db(chunk_power: u8, grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"docs",
            Element::empty_private_document_store(ENTRY_SIZE, chunk_power).expect("valid config"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert private document store");
        db
    }

    fn estimate(op: QualifiedGroveDbOp, chunk_power: u8) -> (OperationCost, OperationCost) {
        let v4 = GroveVersion::latest();
        (
            average_case_estimate(
                vec![op.clone()],
                b"docs",
                TreeType::PrivateDocumentStore(chunk_power),
                ENTRY_SIZE,
                v4,
            ),
            worst_case_estimate(vec![op], v4),
        )
    }

    /// Every position of the first epoch and a half at chunk_power 4, plus
    /// deeper compaction boundaries.
    #[test]
    fn test_private_document_store_insert_estimated_covers_actual_positions_chunk_power_4() {
        const CHUNK_POWER: u8 = 4;
        let v4 = GroveVersion::latest();
        let db = pds_db(CHUNK_POWER, v4);
        let mut next = 0u64;
        for target in sweep_targets() {
            let (average, worst, actual) = step(
                &db,
                &mut next,
                target,
                &pds_op,
                &|op| estimate(op, CHUNK_POWER),
                v4,
            );
            assert_estimates_dominate(
                "PrivateDocumentStoreInsert (chunk_power 4)",
                target,
                &average,
                &worst,
                &actual,
            );
        }
    }

    /// The epoch boundary at chunk_power 11: the last buffered insert (the
    /// deepest ancestor path of the epoch), the compacting insert (the
    /// whole epoch read back), and the first insert of the next epoch.
    #[test]
    fn test_private_document_store_insert_estimated_covers_actual_epoch_boundary_chunk_power_11() {
        const CHUNK_POWER: u8 = 11;
        const EPOCH: u64 = 1 << CHUNK_POWER;
        let v4 = GroveVersion::latest();
        let db = pds_db(CHUNK_POWER, v4);
        let mut next = 0u64;
        for target in [EPOCH - 2, EPOCH - 1, EPOCH] {
            let (average, worst, actual) = step(
                &db,
                &mut next,
                target,
                &pds_op,
                &|op| estimate(op, CHUNK_POWER),
                v4,
            );
            assert_estimates_dominate(
                "PrivateDocumentStoreInsert (chunk_power 11)",
                target,
                &average,
                &worst,
                &actual,
            );
        }
    }

    /// The record model is a real tightening: at chunk_power 11 the declared
    /// average-case estimate charges the ancestor path, the MMR cascade and
    /// the roots in hashes — not the `2 * 2047` a full-buffer walk costs.
    #[test]
    fn test_private_document_store_average_case_hashes_scale_with_height_not_fill() {
        const CHUNK_POWER: u8 = 11;
        let (average, _) = estimate(pds_op(0), CHUNK_POWER);
        // Leaf (2) + 10 ancestors + chunk leaf (1) + MMR merges (65) + bulk
        // root + composite + config (3) + the parent-Merk replace — under
        // 128, against 4,094 for the walk.
        assert!(
            average.hash_node_calls < 128,
            "the declared PDS estimate should not charge a buffer walk: {average:?}"
        );
    }
}

// ── BulkAppend ──────────────────────────────────────────────────────────

mod bulk_append {
    use super::*;

    const VALUE_SIZE: u32 = 40;

    fn bulk_op(index: u64) -> QualifiedGroveDbOp {
        let mut value = vec![0u8; VALUE_SIZE as usize];
        value[..8].copy_from_slice(&index.to_be_bytes());
        QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], value)
    }

    /// Every position of the first epoch and a half at chunk_power 4, plus
    /// deeper compaction boundaries, with the tree's layer declared so the
    /// average-case arm models the epoch.
    #[test]
    fn test_bulk_append_estimated_covers_actual_positions_chunk_power_4() {
        const CHUNK_POWER: u8 = 4;
        let v4 = GroveVersion::latest();
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"bulk",
            Element::empty_bulk_append_tree(CHUNK_POWER).expect("valid chunk_power"),
            None,
            None,
            v4,
        )
        .unwrap()
        .expect("insert bulk append tree");
        let mut next = 0u64;
        for target in sweep_targets() {
            let (average, worst, actual) = step(
                &db,
                &mut next,
                target,
                &bulk_op,
                &|op| {
                    (
                        average_case_estimate(
                            vec![op.clone()],
                            b"bulk",
                            TreeType::BulkAppendTree(CHUNK_POWER),
                            VALUE_SIZE,
                            v4,
                        ),
                        worst_case_estimate(vec![op], v4),
                    )
                },
                v4,
            );
            assert_estimates_dominate(
                "BulkAppend (chunk_power 4)",
                target,
                &average,
                &worst,
                &actual,
            );
        }
    }
}

// ── DenseAppendOnlyFixedSizeTree ────────────────────────────────────────

mod dense_tree {
    use super::*;

    const VALUE_SIZE: u32 = 24;

    fn dense_op(index: u64) -> QualifiedGroveDbOp {
        let mut value = vec![0u8; VALUE_SIZE as usize];
        value[..8].copy_from_slice(&index.to_be_bytes());
        QualifiedGroveDbOp::dense_tree_insert_op(vec![b"dense".to_vec()], value)
    }

    /// A standalone dense tree filled under GROVE_V3 (no records): its first
    /// V4 insert walks and hashes the whole existing buffer to derive the
    /// records — far more than the ancestor path — and both estimates
    /// (average at the declared height, worst at the ceiling) must dominate
    /// it, as well as the O(height) inserts that follow.
    #[test]
    fn test_dense_tree_v4_estimates_cover_the_catch_up_insert() {
        const HEIGHT: u8 = 10;
        const LEGACY_FILL: u64 = 600;
        let v4 = GroveVersion::latest();
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(HEIGHT),
            None,
            None,
            &GROVE_V3,
        )
        .unwrap()
        .expect("insert dense tree");
        let seed_ops: Vec<_> = (0..LEGACY_FILL).map(dense_op).collect();
        db.apply_batch(seed_ops, None, None, &GROVE_V3)
            .unwrap()
            .expect("seed under V3");

        let mut first = None;
        for position in LEGACY_FILL..LEGACY_FILL + 4 {
            let op = dense_op(position);
            let average = average_case_estimate(
                vec![op.clone()],
                b"dense",
                TreeType::DenseAppendOnlyFixedSizeTree(HEIGHT),
                VALUE_SIZE,
                v4,
            );
            let worst = worst_case_estimate(vec![op.clone()], v4);
            let CostContext {
                value,
                cost: actual,
            } = db.apply_batch(vec![op], None, None, v4);
            value.expect("insert should succeed");
            assert_estimates_dominate(
                "DenseTreeInsert after a V3 fill (height 10)",
                position,
                &average,
                &worst,
                &actual,
            );
            first.get_or_insert(actual.clone());
        }
        // The catch-up is not billed: the first insert costs what a later
        // one does, and the average-case estimate at the declared height is
        // far below the ceiling's.
        let first = first.expect("first insert ran");
        let later = db.apply_batch(vec![dense_op(LEGACY_FILL + 4)], None, None, v4);
        later.value.expect("insert");
        assert_eq!(first, later.cost, "catch-up must not change the charge");
        let average = average_case_estimate(
            vec![dense_op(LEGACY_FILL + 5)],
            b"dense",
            TreeType::DenseAppendOnlyFixedSizeTree(HEIGHT),
            VALUE_SIZE,
            v4,
        );
        let worst = worst_case_estimate(vec![dense_op(LEGACY_FILL + 5)], v4);
        assert!(
            average.hash_node_calls < worst.hash_node_calls,
            "the declared height is tighter than the physical ceiling: {average:?} vs {worst:?}"
        );
    }

    /// Every position of a height-5 tree (31 inserts): the estimates'
    /// record terms at the physical ceiling dominate the record maintenance
    /// at every depth.
    #[test]
    fn test_dense_tree_insert_estimated_covers_actual_every_position() {
        const HEIGHT: u8 = 5;
        let v4 = GroveVersion::latest();
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(HEIGHT),
            None,
            None,
            v4,
        )
        .unwrap()
        .expect("insert dense tree");
        for position in 0..((1u64 << HEIGHT) - 1) {
            let op = dense_op(position);
            let average = average_case_estimate(
                vec![op.clone()],
                b"dense",
                TreeType::DenseAppendOnlyFixedSizeTree(HEIGHT),
                VALUE_SIZE,
                v4,
            );
            let worst = worst_case_estimate(vec![op.clone()], v4);
            let CostContext {
                value,
                cost: actual,
            } = db.apply_batch(vec![op], None, None, v4);
            value.expect("insert should succeed");
            assert_estimates_dominate(
                "DenseTreeInsert (height 5)",
                position,
                &average,
                &worst,
                &actual,
            );
        }
    }
}
