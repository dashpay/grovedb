//! Tests for the `is_empty_tree` operation.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element,
    };

    #[test]
    fn test_is_empty_tree_on_empty_subtree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF exists but has no items in it
        let result = db
            .is_empty_tree([TEST_LEAF].as_ref(), None, grove_version)
            .unwrap()
            .expect("should succeed checking empty tree");
        assert!(result, "freshly created subtree should be empty");
    }

    #[test]
    fn test_is_empty_tree_on_subtree_with_items() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item into TEST_LEAF
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let result = db
            .is_empty_tree([TEST_LEAF].as_ref(), None, grove_version)
            .unwrap()
            .expect("should succeed checking non-empty tree");
        assert!(!result, "subtree with items should not be empty");
    }

    #[test]
    fn test_is_empty_tree_on_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Root has test leaves inserted
        let result = db
            .is_empty_tree(EMPTY_PATH, None, grove_version)
            .unwrap()
            .expect("should succeed checking root tree");
        assert!(!result, "root tree with leaves should not be empty");
    }

    #[test]
    fn test_is_empty_tree_on_truly_empty_root() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let result = db
            .is_empty_tree(EMPTY_PATH, None, grove_version)
            .unwrap()
            .expect("should succeed checking empty root");
        assert!(result, "empty root should be empty");
    }

    #[test]
    fn test_is_empty_tree_on_nonexistent_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let result = db
            .is_empty_tree([b"nonexistent".as_ref()].as_ref(), None, grove_version)
            .unwrap();
        assert!(
            result.is_err(),
            "checking a nonexistent path should return an error"
        );
    }

    #[test]
    fn test_is_empty_tree_with_transaction_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let tx = db.start_transaction();

        // Within the transaction, TEST_LEAF is still empty
        let result = db
            .is_empty_tree([TEST_LEAF].as_ref(), Some(&tx), grove_version)
            .unwrap()
            .expect("should succeed with transaction");
        assert!(result, "subtree should be empty in transaction");
    }

    #[test]
    fn test_is_empty_tree_with_transaction_after_insert() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let tx = db.start_transaction();

        // Insert an item within the transaction
        db.insert(
            [TEST_LEAF].as_ref(),
            b"tx_key",
            Element::new_item(b"tx_value".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should insert in transaction");

        // With the transaction, should see the item
        let result = db
            .is_empty_tree([TEST_LEAF].as_ref(), Some(&tx), grove_version)
            .unwrap()
            .expect("should succeed with transaction");
        assert!(
            !result,
            "subtree with transactional insert should not be empty"
        );

        // Without the transaction, should still be empty (not committed)
        let result_no_tx = db
            .is_empty_tree([TEST_LEAF].as_ref(), None, grove_version)
            .unwrap()
            .expect("should succeed without transaction");
        assert!(
            result_no_tx,
            "subtree should still be empty outside transaction"
        );
    }

    #[test]
    fn test_is_empty_tree_with_nested_subtree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a sub-subtree under TEST_LEAF
        db.insert(
            [TEST_LEAF].as_ref(),
            b"inner_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert inner tree");

        // TEST_LEAF is no longer empty (has inner_tree)
        let result = db
            .is_empty_tree([TEST_LEAF].as_ref(), None, grove_version)
            .unwrap()
            .expect("should succeed");
        assert!(!result, "subtree with a nested tree should not be empty");

        // inner_tree is empty though
        let inner_result = db
            .is_empty_tree([TEST_LEAF, b"inner_tree"].as_ref(), None, grove_version)
            .unwrap()
            .expect("should succeed");
        assert!(inner_result, "inner tree should be empty");
    }

    #[test]
    fn test_is_empty_tree_after_delete() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert then delete an item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"temp_key",
            Element::new_item(b"temp_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        db.delete([TEST_LEAF].as_ref(), b"temp_key", None, None, grove_version)
            .unwrap()
            .expect("should delete");

        let result = db
            .is_empty_tree([TEST_LEAF].as_ref(), None, grove_version)
            .unwrap()
            .expect("should succeed after delete");
        assert!(result, "subtree should be empty after deleting all items");
    }
}
