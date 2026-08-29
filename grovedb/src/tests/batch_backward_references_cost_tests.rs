//! Estimated-cost coverage for backward-references batch ops (batching
//! M5): under `BatchApplyOptions::propagate_backward_references`, the
//! GROVE_V4 estimators charge the derived fan-out (registration, chain
//! propagation, cascade deletion) so `worst-case estimate >= actual` holds
//! for flagged family batches, while pre-V4 estimation stays byte-stable
//! for replay.

use std::collections::HashMap;

use grovedb_merk::estimated_costs::{
    average_case_costs::{
        EstimatedLayerCount::EstimatedLevel,
        EstimatedLayerInformation,
        EstimatedLayerSizes::{AllItems, AllSubtrees},
        EstimatedSumTrees::NoSumTrees,
    },
    worst_case_costs::WorstCaseLayerInformation::MaxElementsNumber,
};
use grovedb_merk::tree_type::TreeType;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{
        estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
        key_info::KeyInfo,
        BatchApplyOptions, GroveOp, KeyInfoPath, QualifiedGroveDbOp,
    },
    bidirectional_references::BidirectionalReference,
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, Error, GroveDb,
};

fn batch_flag_on() -> Option<BatchApplyOptions> {
    Some(BatchApplyOptions {
        propagate_backward_references: true,
        ..Default::default()
    })
}

fn sibling_bidi(key: &[u8]) -> Element {
    Element::BidirectionalReference(BidirectionalReference {
        forward_reference_path: ReferencePathType::SiblingReference(key.to_vec()),
        backward_references: Vec::new(),
        cascade_on_update: true,
        max_hop: None,
        flags: None,
    })
}

/// TEST_LEAF holding the registered chain `r2 -> r1 -> value`.
fn db_with_chain(grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    for (key, element) in [
        (
            b"value".as_slice(),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        ),
        (b"r1", sibling_bidi(b"value")),
        (b"r2", sibling_bidi(b"r1")),
    ] {
        db.insert(&[TEST_LEAF], key, element, None, None, grove_version)
            .unwrap()
            .unwrap();
    }
    db
}

fn worst_case_layers(
) -> HashMap<KeyInfoPath, grovedb_merk::estimated_costs::worst_case_costs::WorstCaseLayerInformation>
{
    let mut paths = HashMap::new();
    paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(4));
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(TEST_LEAF.to_vec())]),
        MaxElementsNumber(16),
    );
    paths
}

fn average_case_layers() -> HashMap<KeyInfoPath, EstimatedLayerInformation> {
    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLevel(1, false),
            estimated_layer_sizes: AllSubtrees(32, NoSumTrees, None),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(TEST_LEAF.to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLevel(2, true),
            estimated_layer_sizes: AllItems(32, 128, None),
        },
    );
    paths
}

fn worst_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
    options: Option<BatchApplyOptions>,
    grove_version: &GroveVersion,
) -> grovedb_costs::OperationCost {
    GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(worst_case_layers()),
        ops,
        options,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| {
            Ok((
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
            ))
        },
        grove_version,
    )
    .cost_as_result()
    .expect("expected worst case costs")
}

fn average_case_estimate(
    ops: Vec<QualifiedGroveDbOp>,
    options: Option<BatchApplyOptions>,
    grove_version: &GroveVersion,
) -> grovedb_costs::OperationCost {
    GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(average_case_layers()),
        ops,
        options,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| {
            Ok((
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
            ))
        },
        grove_version,
    )
    .cost_as_result()
    .expect("expected average case costs")
}

#[test]
fn worst_case_estimate_covers_flagged_family_overwrite() {
    let grove_version = GroveVersion::latest();
    let db = db_with_chain(grove_version);

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item_allowing_bidirectional_references(b"updated".to_vec()),
    )];
    let estimate = worst_case_estimate(ops.clone(), batch_flag_on(), grove_version);
    let actual = db
        .apply_batch(ops, batch_flag_on(), None, grove_version)
        .cost_as_result()
        .expect("apply succeeds");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the actual {actual:?}"
    );
}

#[test]
fn worst_case_estimate_covers_flagged_delete_cascade() {
    let grove_version = GroveVersion::latest();
    let db = db_with_chain(grove_version);

    let ops = vec![QualifiedGroveDbOp::delete_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
    )];
    let estimate = worst_case_estimate(ops.clone(), batch_flag_on(), grove_version);
    let actual = db
        .apply_batch(ops, batch_flag_on(), None, grove_version)
        .cost_as_result()
        .expect("apply succeeds");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the actual cascade {actual:?}"
    );
}

#[test]
fn worst_case_estimate_covers_bidi_insert_with_in_batch_target() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"ref".to_vec(),
            sibling_bidi(b"value"),
        ),
    ];
    let estimate = worst_case_estimate(ops.clone(), batch_flag_on(), grove_version);
    let actual = db
        .apply_batch(ops, batch_flag_on(), None, grove_version)
        .cost_as_result()
        .expect("apply succeeds");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the actual {actual:?}"
    );
}

#[test]
fn fan_out_terms_activate_only_with_the_flag() {
    let grove_version = GroveVersion::latest();

    let family_op = || {
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        )]
    };

    // Flag on adds the fan-out on GROVE_V4+…
    let flagged = worst_case_estimate(family_op(), batch_flag_on(), grove_version);
    let unflagged = worst_case_estimate(family_op(), None, grove_version);
    assert!(
        flagged.seek_count > unflagged.seek_count
            && flagged.storage_cost.replaced_bytes > unflagged.storage_cost.replaced_bytes,
        "the flag must activate the fan-out terms: {flagged:?} vs {unflagged:?}"
    );
    let flagged_avg = average_case_estimate(family_op(), batch_flag_on(), grove_version);
    let unflagged_avg = average_case_estimate(family_op(), None, grove_version);
    assert!(flagged_avg.seek_count > unflagged_avg.seek_count);

    // …and a plain-item op charges no fan-out even under the flag.
    let plain_op = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item(b"hello".to_vec()),
    )];
    let flagged_plain = worst_case_estimate(plain_op.clone(), batch_flag_on(), grove_version);
    let unflagged_plain = worst_case_estimate(plain_op, None, grove_version);
    assert_eq!(
        flagged_plain, unflagged_plain,
        "plain element writes must not pick up fan-out terms"
    );
}

#[test]
fn pre_v4_estimation_is_byte_stable_for_replay() {
    // On GROVE_V3 the fan-out version is 0: flagged and unflagged
    // estimates of the same ops must stay identical, so historical
    // admission decisions replay byte-for-byte.
    let v3 = &grovedb_version::version::v3::GROVE_V3;

    let family_op = || {
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        )]
    };
    assert_eq!(
        worst_case_estimate(family_op(), batch_flag_on(), v3),
        worst_case_estimate(family_op(), None, v3),
    );
    assert_eq!(
        average_case_estimate(family_op(), batch_flag_on(), v3),
        average_case_estimate(family_op(), None, v3),
    );
}

#[test]
fn derived_op_estimation_is_version_gated() {
    let grove_version = GroveVersion::latest();
    let v3 = &grovedb_version::version::v3::GROVE_V3;

    // The internal derived op cannot be supplied through apply_batch, but
    // the estimation surface must model it (an expanded batch could be
    // estimated in-crate) — on GROVE_V4 only.
    let derived_op = || {
        vec![QualifiedGroveDbOp {
            path: KeyInfoPath(vec![KeyInfo::KnownKey(TEST_LEAF.to_vec())]),
            key: Some(KeyInfo::KnownKey(b"value".to_vec())),
            op: GroveOp::ReplaceBackwardReferenceFamilyMember {
                element: Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
                node_value_hash: [7; 32],
            },
        }]
    };

    let cost = worst_case_estimate(derived_op(), None, grove_version);
    assert!(cost.seek_count > 0);
    let cost = average_case_estimate(derived_op(), None, grove_version);
    assert!(cost.seek_count > 0);

    let refused = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(worst_case_layers()),
        derived_op(),
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| {
            Ok((
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
            ))
        },
        v3,
    )
    .cost_as_result();
    assert!(matches!(refused, Err(Error::NotSupported(_))));
}
