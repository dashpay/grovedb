//! Removing an entry from an indexed tree removes its row from every
//! secondary, whatever shape the primary has.
//!
//! The mirror used to read a removed key back from the primary after the
//! level's apply. That read fetches the key from storage when it meets a link
//! the apply did not load, and the removed entry is still in storage while the
//! level's writes are pending, so the mirror saw no change and kept the row.
//! Whether it happened depended on the shape of the primary around the
//! removed node; `smallest_case_keys` builds the smallest such shape.
//!
//! From GROVE_V5 the mirror reads an entry by walking the primary's tree
//! from its root (`apply_batch.indexed_mirror_entry_read` 1). The walk
//! follows the tree as it stands, so a removed key is absent, also to a later
//! segment of the same partial batch, whose pre-state capture would otherwise
//! read the removed entry the same way. GROVE_V4 keeps the read, as GroveDB
//! 6.1.1 shipped it.

use grovedb_costs::{
    storage_cost::{
        removal::StorageRemovedBytes::{BasicStorageRemoval, NoStorageRemoval},
        StorageCost,
    },
    OperationCost,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    operations::delete::DeleteUpTreeOptions,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, Error, GroveDb,
};

const INDEXED: &[u8] = b"indexed";

/// The three indexed-tree variants, each with a child it accepts and its
/// dedicated insert and delete.
#[derive(Clone, Copy, Debug)]
enum Variant {
    Count,
    Sum,
    CountSum,
}

impl Variant {
    fn empty_tree(self) -> Element {
        match self {
            Variant::Count => Element::empty_provable_count_indexed_tree(),
            Variant::Sum => Element::empty_provable_sum_indexed_tree(),
            Variant::CountSum => Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (0u8, None),
                (1u8, None),
                (2u8, None),
            ])
            .expect("canonical axes"),
        }
    }

    fn child(self, sum: i64) -> Element {
        match self {
            Variant::Count => Element::new_item(b"v".to_vec()),
            Variant::Sum => Element::new_sum_item(sum),
            Variant::CountSum => Element::new_item_with_sum_item(b"v".to_vec(), sum),
        }
    }

    /// A child that differs from every `child(sum)` the fixtures insert, in
    /// its value and, where the variant has one, its sum.
    fn changed_child(self) -> Element {
        match self {
            Variant::Count => Element::new_item(b"changed".to_vec()),
            Variant::Sum => Element::new_sum_item(100),
            Variant::CountSum => Element::new_item_with_sum_item(b"changed".to_vec(), 100),
        }
    }

    fn insert(self, db: &GroveDb, key: &[u8], sum: i64, grove_version: &GroveVersion) {
        let path = [TEST_LEAF, INDEXED];
        let child = self.child(sum);
        match self {
            Variant::Count => {
                db.insert_into_count_indexed_tree(path.as_ref(), key, child, None, grove_version)
            }
            Variant::Sum => db.insert_into_provable_sum_indexed_tree(
                path.as_ref(),
                key,
                child,
                None,
                grove_version,
            ),
            Variant::CountSum => db.insert_into_provable_count_provable_sum_indexed_tree(
                path.as_ref(),
                key,
                child,
                None,
                grove_version,
            ),
        }
        .unwrap()
        .expect("insert an entry");
    }

    fn delete(self, db: &GroveDb, key: &[u8], grove_version: &GroveVersion) -> Result<bool, Error> {
        let path = [TEST_LEAF, INDEXED];
        match self {
            Variant::Count => {
                db.delete_from_count_indexed_tree(path.as_ref(), key, None, grove_version)
            }
            Variant::Sum => {
                db.delete_from_provable_sum_indexed_tree(path.as_ref(), key, None, grove_version)
            }
            Variant::CountSum => db.delete_from_provable_count_provable_sum_indexed_tree(
                path.as_ref(),
                key,
                None,
                grove_version,
            ),
        }
        .unwrap()
    }
}

/// An indexed tree of `variant` holding `keys`, inserted in that order, each
/// with a different sum so no two rows share a sort key by accident.
fn indexed_tree_with(
    variant: Variant,
    keys: &[Vec<u8>],
    grove_version: &GroveVersion,
) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    db.insert(
        [TEST_LEAF].as_ref(),
        INDEXED,
        variant.empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create the indexed tree");
    for (position, key) in keys.iter().enumerate() {
        variant.insert(&db, key, position as i64 + 1, grove_version);
    }
    db
}

/// The keys whose insertion order gives the smallest failing shape: `k2` ends
/// as the root with `k0` (holding `k1` on its right) and `k3` (holding `k4`)
/// under it.
fn smallest_case_keys() -> Vec<Vec<u8>> {
    ["k0", "k2", "k3", "k4", "k1"]
        .iter()
        .map(|key| key.as_bytes().to_vec())
        .collect()
}

fn verify_issue_count(db: &GroveDb, grove_version: &GroveVersion) -> usize {
    db.verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb runs")
        .len()
}

fn assert_clean(db: &GroveDb, grove_version: &GroveVersion, context: &str) {
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb runs");
    assert!(issues.is_empty(), "{context}: {issues:?}");
}

/// Reconciling secondaries that are already right changes nothing, so a root
/// it leaves alone is one whose secondaries hold exactly the primary's
/// entries.
fn assert_reconcile_changes_nothing(db: &GroveDb, grove_version: &GroveVersion) {
    let before = db.root_hash(None, grove_version).unwrap().expect("root");
    db.reconcile_indexed_tree_secondaries([TEST_LEAF, INDEXED].as_ref(), None, grove_version)
        .unwrap()
        .expect("reconcile");
    let after = db.root_hash(None, grove_version).unwrap().expect("root");
    assert_eq!(before, after, "the secondaries were already right");
}

#[test]
fn removing_an_entry_removes_its_rows_in_every_variant() {
    let grove_version = GroveVersion::latest();
    for variant in [Variant::Count, Variant::Sum, Variant::CountSum] {
        let db = indexed_tree_with(variant, &smallest_case_keys(), grove_version);
        let removed = variant
            .delete(&db, b"k2", grove_version)
            .expect("delete k2");
        assert!(removed, "{variant:?}: k2 was there");
        assert_clean(&db, grove_version, &format!("{variant:?}"));
        assert_reconcile_changes_nothing(&db, grove_version);
    }
}

#[test]
fn removing_any_entry_of_a_larger_tree_removes_its_row() {
    // Sequentially filled trees: on GROVE_V4 removing key 3 of 8, 3, 7 or 11
    // of 16 and seven of the 31 keeps the row.
    let grove_version = GroveVersion::latest();
    for size in [8u8, 16, 31] {
        let keys: Vec<Vec<u8>> = (0..size).map(|index| vec![b'k', index]).collect();
        for removed_key in &keys {
            let db = indexed_tree_with(Variant::Count, &keys, grove_version);
            Variant::Count
                .delete(&db, removed_key, grove_version)
                .expect("delete");
            assert_clean(
                &db,
                grove_version,
                &format!("{size} entries, removed {removed_key:?}"),
            );
        }
    }
}

#[test]
fn removing_an_entry_through_a_batch_removes_its_row() {
    let grove_version = GroveVersion::latest();
    let db = indexed_tree_with(Variant::Count, &smallest_case_keys(), grove_version);
    db.apply_batch(
        vec![QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec(), INDEXED.to_vec()],
            b"k2".to_vec(),
        )],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("delete k2 in a batch");
    assert_clean(&db, grove_version, "batch delete");
    assert_reconcile_changes_nothing(&db, grove_version);
}

/// An indexed tree of count-tree groups, each holding a `0` bucket with one
/// member, as a caller that groups entries by value builds it.
fn indexed_tree_of_groups(grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    db.insert(
        [TEST_LEAF].as_ref(),
        INDEXED,
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create the indexed tree");
    let indexed_path = vec![TEST_LEAF.to_vec(), INDEXED.to_vec()];
    for group in ["g0", "g2", "g3", "g4", "g1"] {
        let group = group.as_bytes().to_vec();
        let mut group_path = indexed_path.clone();
        group_path.push(group.clone());
        let mut bucket_path = group_path.clone();
        bucket_path.push(vec![0]);
        let transaction = db.start_transaction();
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    indexed_path.clone(),
                    group,
                    Element::empty_count_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    group_path,
                    vec![0],
                    Element::empty_count_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    bucket_path,
                    b"member".to_vec(),
                    Element::new_item(b"v".to_vec()),
                ),
            ],
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("insert a group");
        db.commit_transaction(transaction)
            .unwrap()
            .expect("commit the group");
    }
    db
}

/// Removes group `g2`'s only member and, with it, the emptied bucket and
/// group, as one delete-up-tree batch in a transaction.
fn remove_the_last_member_of_g2(db: &GroveDb, grove_version: &GroveVersion) {
    let transaction = db.start_transaction();
    let ops = db
        .delete_operations_for_delete_up_tree_while_empty(
            SubtreePath::from([TEST_LEAF, INDEXED, b"g2", &[0u8]].as_ref()),
            b"member",
            &DeleteUpTreeOptions {
                stop_path_height: Some(1),
                ..Default::default()
            },
            None,
            vec![],
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("delete-up-tree operations");
    assert_eq!(ops.len(), 3, "the member, its bucket and the group");
    db.apply_batch(ops, None, Some(&transaction), grove_version)
        .unwrap()
        .expect("apply the removal");
    db.commit_transaction(transaction)
        .unwrap()
        .expect("commit the removal");
    assert!(
        matches!(
            db.get([TEST_LEAF, INDEXED].as_ref(), b"g2", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ),
        "the group is gone from the primary"
    );
}

#[test]
fn pruning_a_group_whose_last_member_goes_removes_its_row() {
    let grove_version = GroveVersion::latest();
    let db = indexed_tree_of_groups(grove_version);
    remove_the_last_member_of_g2(&db, grove_version);
    assert_clean(&db, grove_version, "delete-up-tree");
    assert_reconcile_changes_nothing(&db, grove_version);
}

#[test]
fn grove_v4_keeps_the_row_and_reconcile_removes_it() {
    // GROVE_V4 is frozen as GroveDB 6.1.1 shipped it: the delete succeeds and
    // the secondary still holds the entry. Reconciling is the repair.
    let grove_version = &GROVE_V4;

    let db = indexed_tree_with(Variant::Count, &smallest_case_keys(), grove_version);
    assert_clean(&db, grove_version, "before the delete");
    let removed = Variant::Count
        .delete(&db, b"k2", grove_version)
        .expect("delete k2");
    assert!(removed);
    assert_eq!(
        verify_issue_count(&db, grove_version),
        1,
        "the row of k2 is an orphan in the secondary"
    );
    db.reconcile_indexed_tree_secondaries([TEST_LEAF, INDEXED].as_ref(), None, grove_version)
        .unwrap()
        .expect("reconcile");
    assert_clean(&db, grove_version, "after reconcile");

    let groups = indexed_tree_of_groups(grove_version);
    remove_the_last_member_of_g2(&groups, grove_version);
    assert_eq!(
        verify_issue_count(&groups, grove_version),
        1,
        "the row of g2 is an orphan in the secondary"
    );
}

#[test]
fn among_removals_only_one_that_kept_its_row_costs_differently_from_grove_v5() {
    // A removal whose row GROVE_V4 does remove costs the same on both: its
    // path is in memory before either read. The one whose row it keeps is
    // charged, from GROVE_V5, for the row's delete and for the nodes the walk
    // fetches to find the key absent.
    let removal_cost = |key: &[u8], grove_version: &GroveVersion| {
        let db = indexed_tree_with(Variant::Count, &smallest_case_keys(), &GROVE_V4);
        let result = db.delete_from_count_indexed_tree(
            [TEST_LEAF, INDEXED].as_ref(),
            key,
            None,
            grove_version,
        );
        assert!(
            matches!(result.value, Ok(true)),
            "removing {key:?}: {:?}",
            result.value
        );
        result.cost
    };
    for key in smallest_case_keys() {
        let v4 = removal_cost(&key, &GROVE_V4);
        let v5 = removal_cost(&key, GroveVersion::latest());
        if key == b"k2" {
            assert_eq!(
                v4,
                OperationCost {
                    seek_count: 24,
                    storage_cost: StorageCost {
                        added_bytes: 0,
                        replaced_bytes: 546,
                        removed_bytes: BasicStorageRemoval(159),
                    },
                    storage_loaded_bytes: 1772,
                    hash_node_calls: 27,
                    sinsemilla_hash_calls: 0,
                },
                "GROVE_V4 keeps the row and charges the read"
            );
            assert_eq!(
                v5,
                OperationCost {
                    seek_count: 30,
                    storage_cost: StorageCost {
                        added_bytes: 0,
                        replaced_bytes: 1005,
                        removed_bytes: BasicStorageRemoval(355),
                    },
                    storage_loaded_bytes: 1874,
                    hash_node_calls: 31,
                    sinsemilla_hash_calls: 0,
                },
                "GROVE_V5 removes the row and charges its delete"
            );
        } else {
            assert_eq!(v4, v5, "removing {key:?}");
        }
    }
}

#[test]
fn a_write_inside_an_entry_loads_less_from_grove_v5() {
    // The write reaches the primary as a change to the group's element. No
    // earlier step of the batch loaded the path to it, so GROVE_V4 reads the
    // key from storage on its own and the apply then loads the path, where
    // the GROVE_V5 walk loads the path once and the apply reuses it. What is
    // stored is the same.
    let cost_of_a_second_member = |grove_version: &GroveVersion| {
        let db = indexed_tree_of_groups(&GROVE_V4);
        let result = db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    TEST_LEAF.to_vec(),
                    INDEXED.to_vec(),
                    b"g4".to_vec(),
                    vec![0],
                ],
                b"second".to_vec(),
                Element::new_item(b"v".to_vec()),
            )],
            None,
            None,
            grove_version,
        );
        result.value.expect("add a member to g4");
        assert_clean(&db, grove_version, "a second member in g4");
        result.cost
    };
    let v4 = cost_of_a_second_member(&GROVE_V4);
    let v5 = cost_of_a_second_member(GroveVersion::latest());
    assert_eq!(
        v4,
        OperationCost {
            seek_count: 33,
            storage_cost: StorageCost {
                added_bytes: 167,
                replaced_bytes: 1207,
                removed_bytes: NoStorageRemoval,
            },
            storage_loaded_bytes: 1853,
            hash_node_calls: 50,
            sinsemilla_hash_calls: 0,
        },
        "GROVE_V4"
    );
    assert_eq!(
        v5,
        OperationCost {
            seek_count: 30,
            storage_cost: StorageCost {
                added_bytes: 167,
                replaced_bytes: 1207,
                removed_bytes: NoStorageRemoval,
            },
            storage_loaded_bytes: 1631,
            hash_node_calls: 50,
            sinsemilla_hash_calls: 0,
        },
        "GROVE_V5"
    );
    assert_eq!(v4.storage_cost, v5.storage_cost);
}

/// Removes `key` in a partial batch's initial segment and writes `written`
/// at the same key from the add-on callback, so the second segment runs
/// while the removal is still pending.
fn remove_then_write_in_one_partial_batch(
    db: &GroveDb,
    key: &[u8],
    written: Element,
    grove_version: &GroveVersion,
) {
    let path = vec![TEST_LEAF.to_vec(), INDEXED.to_vec()];
    db.apply_partial_batch(
        vec![QualifiedGroveDbOp::delete_op(path.clone(), key.to_vec())],
        None,
        |_, _| {
            Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                path.clone(),
                key.to_vec(),
                written.clone(),
            )])
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("remove then write in one partial batch");
}

/// The same two steps as two batches, the removal committed before the write.
fn remove_then_write_in_two_batches(
    db: &GroveDb,
    key: &[u8],
    written: Element,
    grove_version: &GroveVersion,
) {
    let path = vec![TEST_LEAF.to_vec(), INDEXED.to_vec()];
    db.apply_batch(
        vec![QualifiedGroveDbOp::delete_op(path.clone(), key.to_vec())],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("remove");
    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            path,
            key.to_vec(),
            written,
        )],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("write");
}

#[test]
fn a_partial_batch_that_removes_an_entry_and_writes_it_again_keeps_its_row() {
    // The second segment captures its pre-state on the primary the first
    // segment removed the key from. Reading the key there can return the
    // removed entry: an identical write then looks like no change and the
    // row the first segment removed never comes back.
    let grove_version = GroveVersion::latest();
    let keys = smallest_case_keys();
    for variant in [Variant::Count, Variant::Sum, Variant::CountSum] {
        for (position, key) in keys.iter().enumerate() {
            let identical = variant.child(position as i64 + 1);
            for written in [identical, variant.changed_child()] {
                let context = format!("{variant:?}, {key:?}, writing {written:?}");

                let partial = indexed_tree_with(variant, &keys, grove_version);
                remove_then_write_in_one_partial_batch(
                    &partial,
                    key,
                    written.clone(),
                    grove_version,
                );
                assert_clean(&partial, grove_version, &context);

                let sequential = indexed_tree_with(variant, &keys, grove_version);
                remove_then_write_in_two_batches(&sequential, key, written.clone(), grove_version);
                assert_clean(&sequential, grove_version, &context);

                assert_eq!(
                    partial
                        .get([TEST_LEAF, INDEXED].as_ref(), key, None, grove_version)
                        .unwrap()
                        .expect("the entry is back"),
                    written,
                    "{context}"
                );
                assert_eq!(
                    partial
                        .root_hash(None, grove_version)
                        .unwrap()
                        .expect("root"),
                    sequential
                        .root_hash(None, grove_version)
                        .unwrap()
                        .expect("root"),
                    "one partial batch commits what two batches do: {context}"
                );
                assert_reconcile_changes_nothing(&partial, grove_version);
            }
        }
    }
}

#[test]
fn a_partial_batch_that_removes_an_entry_twice_removes_its_row_once() {
    let grove_version = GroveVersion::latest();
    let db = indexed_tree_with(Variant::Count, &smallest_case_keys(), grove_version);
    let path = vec![TEST_LEAF.to_vec(), INDEXED.to_vec()];
    db.apply_partial_batch(
        vec![QualifiedGroveDbOp::delete_op(path.clone(), b"k2".to_vec())],
        None,
        |_, _| {
            Ok(vec![QualifiedGroveDbOp::delete_op(
                path.clone(),
                b"k2".to_vec(),
            )])
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("remove k2 in both segments");
    assert!(matches!(
        db.get([TEST_LEAF, INDEXED].as_ref(), b"k2", None, grove_version)
            .unwrap(),
        Err(Error::PathKeyNotFound(_))
    ));
    assert_clean(&db, grove_version, "removed in both segments");
    assert_reconcile_changes_nothing(&db, grove_version);
}

#[test]
fn a_partial_batch_that_removes_an_entry_then_writes_it_only_if_absent_keeps_the_rows_matching() {
    // Whatever the conditional write decides, the secondaries hold a row for
    // the entry exactly when the primary holds the entry.
    let grove_version = GroveVersion::latest();
    let keys = smallest_case_keys();
    for variant in [Variant::Count, Variant::Sum, Variant::CountSum] {
        for key in &keys {
            let db = indexed_tree_with(variant, &keys, grove_version);
            let path = vec![TEST_LEAF.to_vec(), INDEXED.to_vec()];
            db.apply_partial_batch(
                vec![QualifiedGroveDbOp::delete_op(path.clone(), key.clone())],
                None,
                |_, _| {
                    Ok(vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                        path.clone(),
                        key.clone(),
                        variant.changed_child(),
                    )])
                },
                None,
                grove_version,
            )
            .unwrap()
            .expect("remove, then write only if absent");
            let context = format!("{variant:?}, {key:?}");
            assert_clean(&db, grove_version, &context);
            assert_reconcile_changes_nothing(&db, grove_version);
        }
    }
}

#[test]
fn an_unknown_entry_read_version_is_refused() {
    let mut grove_version = GroveVersion::latest().clone();
    grove_version
        .grovedb_versions
        .apply_batch
        .indexed_mirror_entry_read = 2;
    let db = indexed_tree_with(
        Variant::Count,
        &smallest_case_keys(),
        GroveVersion::latest(),
    );
    let result = Variant::Count.delete(&db, b"k2", &grove_version);
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "got {result:?}"
    );
}
