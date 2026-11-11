//! A batch that inserts a sum or count tree and leaves the level below it
//! empty: it carries operations at the tree's own path, and none of them
//! leaves a key there. An empty Merk reports no aggregate, and the batch
//! rebuilds the element it stores from the aggregate the level reports.
//!
//! From GROVE_V5 the tree the caller sent is stored, exactly as the same
//! insert without those operations stores it. GROVE_V4 stores a plain `Tree`,
//! as GroveDB 6.1.1 did (`apply_batch.tree_insert_above_emptied_level` 0).

use std::collections::HashMap;

use grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
use grovedb_merk::estimated_costs::worst_case_costs::WorstCaseLayerInformation;
use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

use crate::{
    batch::{
        estimated_costs::EstimatedCostsType::WorstCaseCostsType, KeyInfoPath, QualifiedGroveDbOp,
    },
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element, Error, GroveDb,
};

const PARENT_KEY: &[u8] = b"parent";
const TREE_KEY: &[u8] = b"tree";
const ITEM_KEY: &[u8] = b"item";
const MISSING_KEY: &[u8] = b"missing";

/// Every Merk aggregate tree, empty.
fn aggregate_trees() -> Vec<(&'static str, Element)> {
    vec![
        ("sum tree", Element::empty_sum_tree()),
        ("big sum tree", Element::empty_big_sum_tree()),
        ("count tree", Element::empty_count_tree()),
        ("count sum tree", Element::empty_count_sum_tree()),
        ("provable count tree", Element::empty_provable_count_tree()),
        (
            "provable count sum tree",
            Element::empty_provable_count_sum_tree(),
        ),
        ("provable sum tree", Element::empty_provable_sum_tree()),
        (
            "provable count provable sum tree",
            Element::empty_provable_count_provable_sum_tree(),
        ),
    ]
}

/// The wrapped forms a count sum tree parent accepts.
fn wrapped_aggregate_trees() -> Vec<(&'static str, Element)> {
    vec![
        (
            "non counted count tree",
            Element::new_non_counted(Element::empty_count_tree()).expect("wrap"),
        ),
        (
            "not summed sum tree",
            Element::new_not_summed(Element::empty_sum_tree()).expect("wrap"),
        ),
        (
            "not counted or summed count sum tree",
            Element::new_not_counted_or_summed(Element::empty_count_sum_tree()).expect("wrap"),
        ),
    ]
}

/// A database holding `parent` at [`PARENT_KEY`].
fn db_with_parent(parent: Element, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_empty_grovedb();
    db.insert(EMPTY_PATH, PARENT_KEY, parent, None, None, grove_version)
        .unwrap()
        .expect("insert the parent");
    db
}

fn insert_tree(tree: &Element) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(
        vec![PARENT_KEY.to_vec()],
        TREE_KEY.to_vec(),
        tree.clone(),
    )
}

/// A delete at the inserted tree's own path.
fn delete_under_tree(key: &[u8]) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::delete_op(vec![PARENT_KEY.to_vec(), TREE_KEY.to_vec()], key.to_vec())
}

fn stored(db: &TempGroveDb, key: &[u8], grove_version: &GroveVersion) -> Element {
    let parent_path: &[&[u8]] = if key == PARENT_KEY {
        &[]
    } else {
        &[PARENT_KEY]
    };
    db.get_raw(parent_path.into(), key, None, grove_version)
        .unwrap()
        .expect("the element is stored")
}

/// Inserts `tree` with a delete of a key that is not stored under it, and
/// checks the batch stores what the insert alone stores.
fn assert_emptied_insert_matches_plain_insert(name: &str, tree: &Element) {
    let grove_version = GroveVersion::latest();

    let emptied = db_with_parent(Element::empty_count_sum_tree(), grove_version);
    emptied
        .apply_batch(
            vec![insert_tree(tree), delete_under_tree(MISSING_KEY)],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap_or_else(|e| panic!("{name}: the batch applies, got {e:?}"));

    let plain = db_with_parent(Element::empty_count_sum_tree(), grove_version);
    plain
        .apply_batch(vec![insert_tree(tree)], None, None, grove_version)
        .unwrap()
        .unwrap_or_else(|e| panic!("{name}: the insert alone applies, got {e:?}"));

    assert_eq!(
        &stored(&emptied, TREE_KEY, grove_version),
        tree,
        "{name}: the tree the caller sent is stored"
    );
    assert_eq!(
        emptied.root_hash(None, grove_version).unwrap().unwrap(),
        plain.root_hash(None, grove_version).unwrap().unwrap(),
        "{name}: same state as the insert without the delete"
    );
    assert!(
        emptied
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb")
            .is_empty(),
        "{name}"
    );
}

#[test]
fn inserted_aggregate_tree_left_empty_keeps_its_type() {
    for (name, tree) in aggregate_trees() {
        assert_emptied_insert_matches_plain_insert(name, &tree);
    }
}

#[test]
fn inserted_wrapped_aggregate_tree_left_empty_keeps_its_type_and_wrapper() {
    for (name, tree) in wrapped_aggregate_trees() {
        assert_emptied_insert_matches_plain_insert(name, &tree);
    }
}

#[test]
fn sum_written_later_under_such_a_tree_reaches_its_parent() {
    let grove_version = GroveVersion::latest();
    let db = db_with_parent(Element::empty_sum_tree(), grove_version);
    db.apply_batch(
        vec![
            insert_tree(&Element::empty_sum_tree()),
            delete_under_tree(MISSING_KEY),
        ],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("the batch applies");

    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![PARENT_KEY.to_vec(), TREE_KEY.to_vec()],
            ITEM_KEY.to_vec(),
            Element::new_sum_item(100),
        )],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("the sum item is inserted");

    assert_eq!(
        stored(&db, TREE_KEY, grove_version),
        Element::new_sum_tree_with_flags_and_sum_value(Some(ITEM_KEY.to_vec()), 100, None)
    );
    assert_eq!(
        stored(&db, PARENT_KEY, grove_version),
        Element::new_sum_tree_with_flags_and_sum_value(Some(TREE_KEY.to_vec()), 100, None)
    );
}

/// GROVE_V4 is what Dash Platform 5.0 runs: the plain `Tree` it stores is
/// part of that version's state and stays.
#[test]
fn grove_v4_stores_a_plain_tree() {
    for (name, tree) in aggregate_trees() {
        let db = db_with_parent(Element::empty_count_sum_tree(), &GROVE_V4);
        db.apply_batch(
            vec![insert_tree(&tree), delete_under_tree(MISSING_KEY)],
            None,
            None,
            &GROVE_V4,
        )
        .unwrap()
        .unwrap_or_else(|e| panic!("{name}: the batch applies, got {e:?}"));

        assert_eq!(
            stored(&db, TREE_KEY, &GROVE_V4),
            Element::empty_tree(),
            "{name}"
        );
    }
}

/// A `NonCounted` wrapper stays around the plain tree GROVE_V4 stores. Only a
/// sum tree may be `NotSummed` or `NotCountedOrSummed`, so GROVE_V4 fails a
/// batch whose tree carries either.
#[test]
fn grove_v4_keeps_its_outcome_for_wrapped_trees() {
    for (name, tree) in wrapped_aggregate_trees() {
        let db = db_with_parent(Element::empty_count_sum_tree(), &GROVE_V4);
        let result = db
            .apply_batch(
                vec![insert_tree(&tree), delete_under_tree(MISSING_KEY)],
                None,
                None,
                &GROVE_V4,
            )
            .unwrap();

        if tree.is_non_counted() {
            result.unwrap_or_else(|e| panic!("{name}: the batch applies, got {e:?}"));
            assert_eq!(
                stored(&db, TREE_KEY, &GROVE_V4),
                Element::new_non_counted(Element::empty_tree()).expect("wrap"),
                "{name}"
            );
        } else {
            assert!(
                matches!(result, Err(Error::CorruptedCodeExecution(_))),
                "{name}: got {result:?}"
            );
        }
    }
}

#[test]
fn unknown_slot_version_is_refused() {
    let mut version = GroveVersion::latest().clone();
    version
        .grovedb_versions
        .apply_batch
        .tree_insert_above_emptied_level = 2;
    let db = db_with_parent(Element::empty_sum_tree(), &version);
    let result = db
        .apply_batch(
            vec![
                insert_tree(&Element::empty_sum_tree()),
                delete_under_tree(MISSING_KEY),
            ],
            None,
            None,
            &version,
        )
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "an unknown slot value must be refused, got {result:?}"
    );
}

/// The worst-case estimate runs the same propagation, and its levels report
/// no aggregate at all: from GROVE_V5 it sizes a sum tree inserted with a
/// write under it as that sum tree, 9 bytes over the plain tree GROVE_V4
/// sizes.
#[test]
fn worst_case_estimate_sizes_the_inserted_tree_as_its_own_type() {
    let added_bytes = |grove_version: &GroveVersion| {
        let ops = vec![
            insert_tree(&Element::empty_sum_tree()),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![PARENT_KEY.to_vec(), TREE_KEY.to_vec()],
                ITEM_KEY.to_vec(),
                Element::new_sum_item(100),
            ),
        ];
        let paths = [
            (vec![], 10),
            (vec![PARENT_KEY.to_vec()], 10),
            (vec![PARENT_KEY.to_vec(), TREE_KEY.to_vec()], 0),
        ]
        .into_iter()
        .map(|(path, max_elements)| {
            (
                KeyInfoPath::from_known_owned_path(path),
                WorstCaseLayerInformation::MaxElementsNumber(max_elements),
            )
        })
        .collect::<HashMap<_, _>>();
        GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("the worst-case estimate")
        .storage_cost
        .added_bytes
    };

    assert_eq!(
        added_bytes(GroveVersion::latest()),
        added_bytes(&GROVE_V4) + 9
    );
}
