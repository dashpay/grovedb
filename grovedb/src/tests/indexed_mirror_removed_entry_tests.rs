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
//! From GROVE_V5 the mirror takes a key the level's Merk batch deletes as
//! absent (`apply_batch.indexed_mirror_removed_entry_post_state` 1). GROVE_V4
//! keeps the read, as GroveDB 6.1.1 shipped it.

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
fn only_a_removal_that_kept_its_row_costs_differently_from_grove_v5() {
    // The gate moves no other cost: a removal whose row GROVE_V4 does remove
    // costs the same on both, and the one whose row it keeps is charged for
    // the row's delete from GROVE_V5.
    for key in smallest_case_keys() {
        let cost_at = |grove_version: &GroveVersion| {
            let db = indexed_tree_with(Variant::Count, &smallest_case_keys(), &GROVE_V4);
            db.delete_from_count_indexed_tree(
                [TEST_LEAF, INDEXED].as_ref(),
                &key,
                None,
                grove_version,
            )
            .cost
        };
        let v4 = cost_at(&GROVE_V4);
        let v5 = cost_at(GroveVersion::latest());
        if key == b"k2" {
            assert!(
                v5.storage_cost.removed_bytes.total_removed_bytes()
                    > v4.storage_cost.removed_bytes.total_removed_bytes(),
                "the row's bytes are removed: {v4:?} against {v5:?}"
            );
        } else {
            assert_eq!(v4, v5, "removing {key:?}");
        }
    }
}

#[test]
fn an_unknown_removed_entry_post_state_version_is_refused() {
    let mut grove_version = GroveVersion::latest().clone();
    grove_version
        .grovedb_versions
        .apply_batch
        .indexed_mirror_removed_entry_post_state = 2;
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
