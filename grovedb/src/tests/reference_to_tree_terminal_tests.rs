//! Regression tests: a reference whose chain terminates at a **tree** is
//! refused on the direct insert path (`GROVE_V4`+), as it always has been
//! by the batch reference resolver.
//!
//! A reference node commits to `H(terminal bytes)`. When the terminal is a
//! tree element that hash covers only the tree's own element bytes, not
//! the subtree behind it: mutating the subtree leaves the reference's
//! commitment (and `verify_grovedb`) untouched, `get` through the
//! reference surfaces the bare tree element, a subquery through it errors
//! ("the reference must result in an item"), and a V1 proof of the row
//! fails to verify because the verifier expects a lower layer for a
//! non-empty tree. `apply_batch` has always rejected the shape with
//! `InvalidBatchOperation("references can not point to trees being
//! updated")`; `GroveDb::insert` accepted it.
//!
//! `GROVE_V3` is live, so the refusal lives in `add_element_on_transaction`
//! v2 (selected by `GROVE_V4`+). v1 keeps accepting, which is pinned here.

#[cfg(test)]
mod tests {
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::QualifiedGroveDbOp,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, Error,
    };

    const REF_KEY: &[u8] = b"ref_to_tree";
    const ITEM_KEY: &[u8] = b"item";

    fn absolute_reference(path: &[&[u8]]) -> Element {
        Element::new_reference(ReferencePathType::AbsolutePathReference(
            path.iter().map(|segment| segment.to_vec()).collect(),
        ))
    }

    /// `ANOTHER_TEST_LEAF` holds one item, so it is a populated tree.
    fn db_with_populated_tree(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            ITEM_KEY,
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item under ANOTHER_TEST_LEAF");
        db
    }

    fn root(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash")
    }

    fn assert_direct_refusal(result: Result<(), Error>) {
        match result {
            Err(Error::InvalidInput(message)) => assert!(
                message.contains("references can not point to trees"),
                "unexpected refusal message: {message}"
            ),
            other => panic!("expected InvalidInput refusal, got {other:?}"),
        }
    }

    /// The refusal must leave no trace: no row, same root, clean walk.
    fn assert_nothing_written(
        db: &TempGroveDb,
        path: &[&[u8]],
        key: &[u8],
        root_before: [u8; 32],
        grove_version: &GroveVersion,
    ) {
        let stored = db
            .get_raw_optional(path.into(), key, None, grove_version)
            .unwrap()
            .expect("read back the refused key");
        assert_eq!(stored, None, "a refused reference must not be stored");
        assert_eq!(
            root(db, grove_version),
            root_before,
            "a refused insert must not move the root"
        );
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb runs");
        assert!(
            issues.is_empty(),
            "integrity walk must stay clean: {issues:?}"
        );
    }

    /// The exact reproduction: item under `ANOTHER_TEST_LEAF`, then a
    /// direct insert of a reference to `ANOTHER_TEST_LEAF` itself.
    #[test]
    fn direct_insert_refuses_reference_to_populated_tree() {
        let grove_version = GroveVersion::latest();
        let db = db_with_populated_tree(grove_version);
        let root_before = root(&db, grove_version);

        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                absolute_reference(&[ANOTHER_TEST_LEAF]),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert_direct_refusal(result);
        assert_nothing_written(&db, &[TEST_LEAF], REF_KEY, root_before, grove_version);
    }

    /// The batch resolver refuses references to trees regardless of
    /// whether the tree holds anything; so does the direct path, for
    /// every tree family the direct path can create.
    #[test]
    fn direct_insert_refuses_reference_to_empty_tree_of_any_type() {
        let grove_version = GroveVersion::latest();
        let trees: Vec<(&[u8], Element)> = vec![
            (b"tree", Element::empty_tree()),
            (b"sum", Element::empty_sum_tree()),
            (b"big_sum", Element::empty_big_sum_tree()),
            (b"count", Element::empty_count_tree()),
            (b"count_sum", Element::empty_count_sum_tree()),
            (b"provable_count", Element::empty_provable_count_tree()),
            (b"provable_sum", Element::empty_provable_sum_tree()),
            (b"mmr", Element::empty_mmr_tree()),
        ];
        for (tree_key, tree) in trees {
            let db = make_test_grovedb(grove_version);
            db.insert(
                [ANOTHER_TEST_LEAF].as_ref(),
                tree_key,
                tree.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap_or_else(|e| panic!("insert empty {tree:?}: {e}"));
            let root_before = root(&db, grove_version);

            let result = db
                .insert(
                    [TEST_LEAF].as_ref(),
                    REF_KEY,
                    absolute_reference(&[ANOTHER_TEST_LEAF, tree_key]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap();
            assert!(
                matches!(result, Err(Error::InvalidInput(_))),
                "reference to empty {tree:?} must be refused, got {result:?}"
            );
            assert_nothing_written(&db, &[TEST_LEAF], REF_KEY, root_before, grove_version);
        }

        // The root-level leaves created by `make_test_grovedb` are empty
        // plain trees too.
        let db = make_test_grovedb(grove_version);
        let root_before = root(&db, grove_version);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                absolute_reference(&[ANOTHER_TEST_LEAF]),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert_direct_refusal(result);
        assert_nothing_written(&db, &[TEST_LEAF], REF_KEY, root_before, grove_version);
    }

    /// `ReferenceWithSumItem` shares the resolution arm and is refused the
    /// same way.
    #[test]
    fn direct_insert_refuses_reference_with_sum_item_to_tree() {
        let grove_version = GroveVersion::latest();
        let db = db_with_populated_tree(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");
        let root_before = root(&db, grove_version);

        let result = db
            .insert(
                [TEST_LEAF, b"sum_tree"].as_ref(),
                REF_KEY,
                Element::new_reference_with_sum_item(
                    ReferencePathType::AbsolutePathReference(vec![ANOTHER_TEST_LEAF.to_vec()]),
                    5,
                ),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert_direct_refusal(result);
        assert_nothing_written(
            &db,
            &[TEST_LEAF, b"sum_tree"],
            REF_KEY,
            root_before,
            grove_version,
        );
    }

    /// A chain is judged by its terminal. The legacy hop (a reference to
    /// a tree) is written under `GROVE_V3`, which still accepts it; after
    /// the upgrade a new reference that resolves through that hop to the
    /// tree is refused.
    #[test]
    fn direct_insert_refuses_reference_chain_that_terminates_at_tree() {
        let db = db_with_populated_tree(&GROVE_V3);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"legacy_hop",
            absolute_reference(&[ANOTHER_TEST_LEAF]),
            None,
            None,
            &GROVE_V3,
        )
        .unwrap()
        .expect("GROVE_V3 still accepts a reference to a tree");

        let latest = GroveVersion::latest();
        let root_before = root(&db, latest);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                absolute_reference(&[TEST_LEAF, b"legacy_hop"]),
                None,
                None,
                latest,
            )
            .unwrap();
        assert_direct_refusal(result);
        assert_nothing_written(&db, &[TEST_LEAF], REF_KEY, root_before, latest);
    }

    /// A reference to an item stays accepted: the refusal is specific to
    /// tree terminals, including when the chain hops through another
    /// reference on its way to the item.
    #[test]
    fn direct_insert_still_accepts_reference_to_item_and_chain_to_item() {
        let grove_version = GroveVersion::latest();
        let db = db_with_populated_tree(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_to_item",
            absolute_reference(&[ANOTHER_TEST_LEAF, ITEM_KEY]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("reference to an item is accepted");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_to_ref",
            absolute_reference(&[TEST_LEAF, b"ref_to_item"]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("reference chain ending at an item is accepted");

        for key in [b"ref_to_item".as_slice(), b"ref_to_ref".as_slice()] {
            let got = db
                .get([TEST_LEAF].as_ref(), key, None, grove_version)
                .unwrap()
                .expect("get through reference");
            assert_eq!(got, Element::new_item(b"value".to_vec()));
        }
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb runs");
        assert!(issues.is_empty(), "{issues:?}");
    }

    /// Every public insert entry point routes through the same arm.
    #[test]
    fn conditional_insert_entry_points_refuse_reference_to_tree() {
        let grove_version = GroveVersion::latest();
        let db = db_with_populated_tree(grove_version);
        let root_before = root(&db, grove_version);
        let reference = absolute_reference(&[ANOTHER_TEST_LEAF]);

        let result = db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                reference.clone(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidInput(_))),
            "insert_if_not_exists must refuse, got {result:?}"
        );

        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                reference.clone(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidInput(_))),
            "insert_if_not_exists_return_existing_element must refuse, got {result:?}"
        );

        let result = db
            .insert_if_changed_value(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                reference,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidInput(_))),
            "insert_if_changed_value must refuse, got {result:?}"
        );

        assert_nothing_written(&db, &[TEST_LEAF], REF_KEY, root_before, grove_version);
    }

    /// The refusal also holds inside an explicit transaction: the batch is
    /// discarded and nothing reaches the transaction.
    #[test]
    fn direct_insert_refuses_reference_to_tree_inside_transaction() {
        let grove_version = GroveVersion::latest();
        let db = db_with_populated_tree(grove_version);
        let root_before = root(&db, grove_version);

        let tx = db.start_transaction();
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                absolute_reference(&[ANOTHER_TEST_LEAF]),
                None,
                Some(&tx),
                grove_version,
            )
            .unwrap();
        assert_direct_refusal(result);
        let stored = db
            .get_raw_optional(
                [TEST_LEAF].as_ref().into(),
                REF_KEY,
                Some(&tx),
                grove_version,
            )
            .unwrap()
            .expect("read back inside the transaction");
        assert_eq!(stored, None);
        db.commit_transaction(tx).unwrap().expect("commit");
        assert_nothing_written(&db, &[TEST_LEAF], REF_KEY, root_before, grove_version);
    }

    /// Direct and batch writes of the same reference now agree: both
    /// refuse, each with its path's typed error, and neither moves the
    /// root.
    #[test]
    fn direct_and_batch_paths_agree_on_reference_to_tree() {
        let grove_version = GroveVersion::latest();
        let reference = absolute_reference(&[ANOTHER_TEST_LEAF]);

        let direct = db_with_populated_tree(grove_version);
        let direct_root = root(&direct, grove_version);
        let result = direct
            .insert(
                [TEST_LEAF].as_ref(),
                REF_KEY,
                reference.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert_direct_refusal(result);

        let batched = db_with_populated_tree(grove_version);
        let batched_root = root(&batched, grove_version);
        let result = batched
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    REF_KEY.to_vec(),
                    reference,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidBatchOperation(message)) => assert!(
                message.contains("references can not point to trees"),
                "unexpected batch refusal message: {message}"
            ),
            other => panic!("expected InvalidBatchOperation refusal, got {other:?}"),
        }

        assert_eq!(direct_root, batched_root, "both databases start identical");
        assert_nothing_written(&direct, &[TEST_LEAF], REF_KEY, direct_root, grove_version);
        assert_nothing_written(&batched, &[TEST_LEAF], REF_KEY, batched_root, grove_version);
    }

    /// Consensus pin: `GROVE_V3` is live, so its direct insert must keep
    /// accepting a reference to a tree and committing the legacy hash of
    /// the bare tree element. Reading that legacy row under `GROVE_V4`
    /// must still work, and must not rewrite it.
    #[test]
    fn grove_v3_direct_insert_still_accepts_reference_to_tree() {
        let db = db_with_populated_tree(&GROVE_V3);
        db.insert(
            [TEST_LEAF].as_ref(),
            REF_KEY,
            absolute_reference(&[ANOTHER_TEST_LEAF]),
            None,
            None,
            &GROVE_V3,
        )
        .unwrap()
        .expect("GROVE_V3 direct insert keeps accepting a reference to a tree");
        let root_after_legacy_write = root(&db, &GROVE_V3);

        for reader_version in [&GROVE_V3, GroveVersion::latest()] {
            let got = db
                .get([TEST_LEAF].as_ref(), REF_KEY, None, reader_version)
                .unwrap()
                .expect("get through the legacy reference");
            assert!(
                got.is_any_tree(),
                "the legacy reference resolves to the tree element, got {got:?}"
            );
            let issues = db
                .verify_grovedb(None, true, true, reader_version)
                .expect("verify_grovedb runs");
            assert!(
                issues.is_empty(),
                "legacy row is internally consistent: {issues:?}"
            );
            assert_eq!(
                root(&db, reader_version),
                root_after_legacy_write,
                "reads must not rewrite commitments"
            );
        }
    }
}
