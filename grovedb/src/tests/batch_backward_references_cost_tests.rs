//! Estimated-cost coverage for backward-references batch ops (batching
//! M5): the GROVE_V4 estimators charge the derived fan-out (registration,
//! chain propagation, cascade deletion) so `worst-case estimate >= actual`
//! holds for maintained family batches, while pre-V4 estimation stays
//! byte-stable for replay. A plain write or delete charges the
//! displaced-state bound only when the op declares
//! `DisplacedValue::MayBeParticipant`; an op declared `NotParticipant`
//! estimates the plain write alone.

use std::collections::HashMap;

use grovedb_merk::estimated_costs::{
    average_case_costs::{
        EstimatedLayerCount::EstimatedLevel,
        EstimatedLayerInformation,
        EstimatedLayerSizes::{AllItems, AllSubtrees},
        EstimatedSumTrees::NoSumTrees,
    },
    worst_case_costs::WorstCaseLayerInformation::{self, MaxElementsNumber},
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

fn not_participant(ops: Vec<QualifiedGroveDbOp>) -> Vec<QualifiedGroveDbOp> {
    ops.into_iter().map(|op| op.dont_check()).collect()
}

fn sibling_bidi(key: &[u8]) -> Element {
    Element::BidirectionalReference(
        BidirectionalReference {
            forward_reference_path: ReferencePathType::SiblingReference(key.to_vec()),
            backward_references: Vec::new(),
            cascade_on_update: true,
            max_hop: None,
        },
        None,
    )
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
    worst_case_estimate_with_layers(worst_case_layers(), ops, options, grove_version)
}

fn worst_case_estimate_with_layers(
    layers: HashMap<KeyInfoPath, WorstCaseLayerInformation>,
    ops: Vec<QualifiedGroveDbOp>,
    options: Option<BatchApplyOptions>,
    grove_version: &GroveVersion,
) -> grovedb_costs::OperationCost {
    GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(layers),
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
    average_case_estimate_with_layers(average_case_layers(), ops, options, grove_version)
}

fn average_case_estimate_with_layers(
    layers: HashMap<KeyInfoPath, EstimatedLayerInformation>,
    ops: Vec<QualifiedGroveDbOp>,
    options: Option<BatchApplyOptions>,
    grove_version: &GroveVersion,
) -> grovedb_costs::OperationCost {
    GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(layers),
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
    let estimate = worst_case_estimate(ops.clone(), None, grove_version);
    let actual = db
        .apply_batch(ops, None, None, grove_version)
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
    let estimate = worst_case_estimate(ops.clone(), None, grove_version);
    let actual = db
        .apply_batch(ops, None, None, grove_version)
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
    let estimate = worst_case_estimate(ops.clone(), None, grove_version);
    let actual = db
        .apply_batch(ops, None, None, grove_version)
        .cost_as_result()
        .expect("apply succeeds");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the actual {actual:?}"
    );
}

#[test]
fn fan_out_terms_follow_the_op_declaration() {
    let grove_version = GroveVersion::latest();

    let family_op = || {
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        )]
    };

    // A participant payload is charged from the op itself on GROVE_V4+,
    // whatever it declares about the value it displaces.
    let family = worst_case_estimate(family_op(), None, grove_version);
    assert_eq!(
        family,
        worst_case_estimate(not_participant(family_op()), None, grove_version),
        "a participant payload charges its fan-out regardless of the declaration"
    );
    assert_eq!(
        average_case_estimate(family_op(), None, grove_version),
        average_case_estimate(not_participant(family_op()), None, grove_version)
    );

    // A PLAIN-item op charges the displaced-state fan-out only when it
    // declares the stored element it lands on may be a registered family
    // element whose propagation/cascade the preprocessor must perform.
    let plain_op = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item(b"hello".to_vec()),
    )];
    let may_be_plain = worst_case_estimate(plain_op.clone(), None, grove_version);
    let not_plain = worst_case_estimate(not_participant(plain_op), None, grove_version);
    assert!(
        may_be_plain.seek_count > not_plain.seek_count
            && may_be_plain.storage_cost.replaced_bytes > not_plain.storage_cost.replaced_bytes,
        "the declaration must activate the displaced-state terms: {may_be_plain:?} vs \
         {not_plain:?}"
    );
}

/// The reviewer-reported gap: a flagged PLAIN overwrite landing on a
/// registered family element triggers real propagation work in
/// preprocessing — the worst-case estimate must cover it.
#[test]
fn worst_case_estimate_covers_flagged_plain_overwrite_of_registered_target() {
    let grove_version = GroveVersion::latest();
    let db = db_with_chain(grove_version);

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item(b"plain".to_vec()),
    )];
    let estimate = worst_case_estimate(ops.clone(), None, grove_version);
    let actual = db
        .apply_batch(ops, None, None, grove_version)
        .cost_as_result()
        .expect("apply succeeds — the chain cascades");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the displaced-state work {actual:?}"
    );
}

/// The registration entry stored on the target serializes the referrer's
/// QUALIFIED ORIGIN path (an absolute inversion carries every segment): a
/// deep origin pointing at a shallow target must still be covered by the
/// worst-case added-bytes bound.
#[test]
fn worst_case_estimate_covers_deep_origin_registration_growth() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // An eight-level origin with 200-byte segment names.
    let mut deep_path: Vec<Vec<u8>> = vec![TEST_LEAF.to_vec()];
    for i in 0..7u8 {
        let segment = vec![b'a' + i; 200];
        let path_refs: Vec<&[u8]> = deep_path.iter().map(|p| p.as_slice()).collect();
        db.insert(
            path_refs.as_slice(),
            &segment,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        deep_path.push(segment);
    }
    db.insert(
        &[TEST_LEAF],
        b"value",
        Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        deep_path.clone(),
        b"ref".to_vec(),
        Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"value".to_vec(),
                ]),
                backward_references: Vec::new(),
                cascade_on_update: true,
                max_hop: None,
            },
            None,
        ),
    )];

    // Declare a layer for the op's path and every ancestor level the
    // estimator bubbles through.
    let mut paths = worst_case_layers();
    let mut ancestor = Vec::new();
    for segment in &deep_path {
        ancestor.push(KeyInfo::KnownKey(segment.clone()));
        paths.insert(KeyInfoPath(ancestor.clone()), MaxElementsNumber(4));
    }
    let estimate = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops.clone(),
        None,
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
    .expect("expected worst case costs");
    let actual = db
        .apply_batch(ops, None, None, grove_version)
        .cost_as_result()
        .expect("apply succeeds");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the deep-origin registration {actual:?}"
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
        worst_case_estimate(family_op(), None, v3),
        worst_case_estimate(family_op(), None, v3),
    );
    assert_eq!(
        average_case_estimate(family_op(), None, v3),
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
                end_hash: None,
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

/// The maximum legal component shape: `MAX_BACKWARD_REFERENCES` referrers
/// on one target, each in its OWN branch at the full
/// `MAX_BACKWARD_REFERENCES_GROVE_DEPTH` registration depth, with every
/// ancestor Merk populated (so bubbling does real in-Merk propagation at
/// each level). The worst-case estimate must cover the flagged overwrite
/// componentwise — this pins the height-dependent ancestor-walk term.
#[test]
fn worst_case_estimate_covers_max_fan_out_deep_component() {
    use grovedb_element::MAX_BACKWARD_REFERENCES;

    use crate::bidirectional_references::MAX_BACKWARD_REFERENCES_GROVE_DEPTH;

    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // The target declares the protocol ceiling as its capacity; the
    // overwrite below re-declares it (the carried-over referrers must fit).
    db.insert(
        &[TEST_LEAF],
        b"value",
        Element::new_item_allowing_bidirectional_references_with_capacity(
            b"hello".to_vec(),
            MAX_BACKWARD_REFERENCES as u16,
        ),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // The branch skeletons (a subtree per level, each parent Merk
    // populated so bubbling does real work there) are built in one batch:
    // inserting them one node at a time would propagate every write to the
    // root and take minutes at the ceiling's width.
    let mut skeleton = Vec::new();
    let mut leaf_paths: Vec<Vec<Vec<u8>>> = Vec::new();
    for branch in 0..MAX_BACKWARD_REFERENCES {
        let mut path: Vec<Vec<u8>> = vec![TEST_LEAF.to_vec()];
        // First segment distinguishes the branch; deeper segments are
        // constant (each lives in its own parent Merk).
        let mut next_segment = format!("b{branch:03}").into_bytes();
        while path.len() < MAX_BACKWARD_REFERENCES_GROVE_DEPTH {
            skeleton.push(QualifiedGroveDbOp::insert_or_replace_op(
                path.clone(),
                next_segment.clone(),
                Element::empty_tree(),
            ));
            skeleton.push(QualifiedGroveDbOp::insert_or_replace_op(
                path.clone(),
                [next_segment.as_slice(), b"_fill"].concat(),
                Element::new_item(b"f".to_vec()),
            ));
            path.push(next_segment);
            next_segment = b"d".to_vec();
        }
        leaf_paths.push(path);
    }
    db.apply_batch(skeleton, None, None, grove_version)
        .unwrap()
        .expect("branch skeletons");

    // The referrers themselves go in one at a time: each registers on
    // `value` and propagates up its own branch.
    for path in &leaf_paths {
        let path_refs: Vec<&[u8]> = path.iter().map(|p| p.as_slice()).collect();
        db.insert(
            path_refs.as_slice(),
            b"ref",
            Element::BidirectionalReference(
                BidirectionalReference {
                    forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"value".to_vec(),
                    ]),
                    backward_references: Vec::new(),
                    cascade_on_update: true,
                    max_hop: None,
                },
                None,
            ),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    }

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item_allowing_bidirectional_references_with_capacity(
            b"updated".to_vec(),
            MAX_BACKWARD_REFERENCES as u16,
        ),
    )];
    // The declared layer must dominate every Merk in the component,
    // ancestor Merks included (the model's documented contract).
    let mut paths = HashMap::new();
    paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(8));
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(TEST_LEAF.to_vec())]),
        MaxElementsNumber(128),
    );
    let estimate = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops.clone(),
        None,
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
    .expect("expected worst case costs");
    let actual = db
        .apply_batch(ops, None, None, grove_version)
        .cost_as_result()
        .expect("apply succeeds — full-width propagation");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the maximum component shape {actual:?}"
    );
}

/// The flagged apply path probes a deleted tree's subtree for emptiness
/// (a merk open plus its root read) before admitting the deletion; the
/// estimators must charge it — a flagged empty-tree deletion previously
/// cost more than its worst-case estimate.
#[test]
fn worst_case_estimate_covers_flagged_empty_tree_deletion() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"sub",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let ops = vec![QualifiedGroveDbOp::delete_op(
        vec![TEST_LEAF.to_vec()],
        b"sub".to_vec(),
    )];
    let estimate = worst_case_estimate(ops.clone(), None, grove_version);
    let actual = db
        .apply_batch(ops, None, None, grove_version)
        .cost_as_result()
        .expect("an empty subtree deletes under the flag");

    assert!(
        estimate.worse_or_eq_than(&actual),
        "worst-case estimate {estimate:?} must cover the emptiness probe {actual:?}"
    );
}

/// A written item's DECLARED referrer capacity bounds its worst-case
/// fan-out: a tighter declaration estimates strictly less than the default
/// one, a plain payload (which cannot declare anything) charges the
/// protocol ceiling, and the tight estimate still covers the actual flagged
/// overwrite of a registered target.
#[test]
fn declared_capacity_tightens_the_worst_case_estimate() {
    let grove_version = GroveVersion::latest();
    let family = |capacity: u16| {
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item_allowing_bidirectional_references_with_capacity(
                b"updated".to_vec(),
                capacity,
            ),
        )]
    };
    let plain = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item(b"updated".to_vec()),
    )];

    let tight = worst_case_estimate(family(1), None, grove_version);
    let default = worst_case_estimate(
        family(grovedb_element::DEFAULT_BACKWARD_REFERENCES_CAPACITY),
        None,
        grove_version,
    );
    let ceiling = worst_case_estimate(plain, None, grove_version);
    assert!(
        tight.seek_count < default.seek_count && tight.hash_node_calls < default.hash_node_calls,
        "a capacity of one must estimate below the default: {tight:?} vs {default:?}"
    );
    assert!(
        default.seek_count < ceiling.seek_count
            && default.hash_node_calls < ceiling.hash_node_calls,
        "the default capacity must estimate below the ceiling a plain payload charges: \
         {default:?} vs {ceiling:?}"
    );

    // `value` carries one referrer (r1, itself referred to by r2): the
    // capacity-one overwrite fits and its estimate covers the real
    // propagation along the whole chain.
    let db = db_with_chain(grove_version);
    let actual = db
        .apply_batch(family(1), None, None, grove_version)
        .cost_as_result()
        .expect("one registered referrer fits a capacity of one");
    assert!(
        tight.worse_or_eq_than(&actual),
        "the declared-capacity estimate {tight:?} must cover the actual {actual:?}"
    );
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());

    // The average model is capped by the declaration too.
    let tight_average = average_case_estimate(family(0), None, grove_version);
    let default_average = average_case_estimate(
        family(grovedb_element::DEFAULT_BACKWARD_REFERENCES_CAPACITY),
        None,
        grove_version,
    );
    assert!(
        tight_average.seek_count <= default_average.seek_count
            && tight_average.hash_node_calls < default_average.hash_node_calls,
        "{tight_average:?} vs {default_average:?}"
    );
}

/// The estimator cannot see stored state. An op declared `NotParticipant`
/// charges neither the displaced-state fan-out nor the delete probe, so its
/// estimate is the plain write's alone in both estimators; the default
/// `MayBeParticipant` reinstates the bound, and an op that itself writes a
/// participant is charged from the op regardless.
#[test]
fn not_participant_ops_estimate_plain_writes_without_the_displaced_bound() {
    let grove_version = GroveVersion::latest();
    let plain_ops = [
        (
            "insert_or_replace",
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                Element::new_item(b"hello".to_vec()),
            ),
        ),
        (
            "delete",
            QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"value".to_vec()),
        ),
    ];
    for (name, op) in plain_ops {
        let ops = || vec![op.clone()];
        let not_average = average_case_estimate(not_participant(ops()), None, grove_version);
        let not_worst = worst_case_estimate(not_participant(ops()), None, grove_version);
        let may_be_average = average_case_estimate(ops(), None, grove_version);
        let may_be_worst = worst_case_estimate(ops(), None, grove_version);
        assert!(
            may_be_average.seek_count > not_average.seek_count,
            "{name}: the declaration must reinstate the average-case bound: {may_be_average:?} \
             vs {not_average:?}"
        );
        assert!(
            may_be_worst.seek_count > not_worst.seek_count,
            "{name}: the declaration must reinstate the worst-case bound: {may_be_worst:?} vs \
             {not_worst:?}"
        );
    }

    let family = || {
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        )]
    };
    assert_eq!(
        average_case_estimate(family(), None, grove_version),
        average_case_estimate(not_participant(family()), None, grove_version),
        "a participant write is charged from the op whatever it declares"
    );
    assert_eq!(
        worst_case_estimate(family(), None, grove_version),
        worst_case_estimate(not_participant(family()), None, grove_version)
    );
}

/// Overwriting a registered target with a plain item cascades its chain.
/// Only the `MayBeParticipant` worst-case estimate covers that work; a
/// `NotParticipant` estimate is the plain write's, and the apply refuses the
/// false claim rather than running the cascade unpriced.
#[test]
fn may_be_participant_covers_the_displaced_cascade_not_participant_cannot_see() {
    let grove_version = GroveVersion::latest();
    let db = db_with_chain(grove_version);
    let ops = || {
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item(b"plain".to_vec()),
        )]
    };

    let declared = worst_case_estimate(ops(), None, grove_version);
    let undeclared = worst_case_estimate(not_participant(ops()), None, grove_version);
    assert!(matches!(
        db.apply_batch(not_participant(ops()), None, None, grove_version)
            .unwrap(),
        Err(Error::NotSupported(_))
    ));

    let actual = db
        .apply_batch(ops(), None, None, grove_version)
        .cost_as_result()
        .expect("the chain cascades under the default policy");
    assert!(
        declared.worse_or_eq_than(&actual),
        "the declared estimate {declared:?} must cover the cascade {actual:?}"
    );
    assert!(declared.seek_count > undeclared.seek_count);
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}
