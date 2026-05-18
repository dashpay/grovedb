//! `ProvableSumIndexedTree` (PSIT) tests.
//!
//! Phase 2 coverage for the single-axis (sum) indexed tree:
//! - Empty creation + `verify_grovedb` passes.
//! - Single insert + read-back.
//! - Multiple inserts + `verify_grovedb` passes.
//! - Delete + `verify_grovedb` still passes.
//! - Child-type rejection: PSIT must reject non-sum-bearing items
//!   (`Item`).

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    fn insert_empty_psit_at_test_leaf(
        db: &crate::GroveDb,
        key: &[u8],
        grove_version: &GroveVersion,
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected PSIT insertion to succeed");
    }

    #[test]
    fn psit_empty_creation_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb should succeed");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues for an empty PSIT: {:?}",
            issues
        );
    }

    #[test]
    fn psit_single_insert_then_read_back() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);

        // Insert a SumItem under the PSIT primary via the dedicated
        // PSIT insertion API. SumItem(42) contributes sum = 42 to the
        // primary and gets a (sum_sort_key ‖ key) entry in the sum
        // secondary.
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row1",
            Element::new_sum_item(42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("PSIT insertion of SumItem should succeed");

        // Read back the item from the primary via the regular get API.
        let read = db
            .get([TEST_LEAF, b"psit"].as_ref(), b"row1", None, grove_version)
            .unwrap()
            .expect("get should return the inserted item");
        assert_eq!(read, Element::new_sum_item(42));

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb should succeed");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues after a single PSIT insert: {:?}",
            issues
        );
    }

    #[test]
    fn psit_multiple_inserts_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);

        for (key, sum) in [
            (b"a".as_ref(), 10),
            (b"b".as_ref(), -5),
            (b"c".as_ref(), 100),
        ] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("PSIT insertion should succeed");
        }

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb should succeed");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues after multiple PSIT inserts: {:?}",
            issues
        );
    }

    #[test]
    fn psit_delete_then_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);

        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row1",
            Element::new_sum_item(42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert should succeed");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row2",
            Element::new_sum_item(-7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert should succeed");

        let removed = db
            .delete_from_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"row1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete should succeed");
        assert!(removed, "delete must report success for an existing key");

        // Deleting a missing key returns Ok(false).
        let removed_again = db
            .delete_from_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"row1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete of missing key returns false");
        assert!(!removed_again);

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb should succeed");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues after delete: {:?}",
            issues
        );
    }

    #[test]
    fn psit_rejects_non_sum_bearing_item() {
        // PSIT only accepts sum-bearing children. A plain Item must
        // be rejected by the dedicated insert API with InvalidInputError.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);

        let result = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"row1",
                Element::new_item(b"not-sum-bearing".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PSIT must reject a plain Item child, got: {:?}",
            result
        );
    }
}
