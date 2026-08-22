//! Tests

#[cfg(feature = "minimal")]
mod tests {

    use grovedb_costs::storage_cost::removal::{
        Identifier, StorageRemovalPerEpochByIdentifier, StorageRemovedBytes,
        StorageRemovedBytes::{BasicStorageRemoval, SectionedStorageRemoval},
        UNKNOWN_EPOCH,
    };
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;
    use intmap::IntMap;

    use crate::{
        batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element,
    };

    /// Inserts one flagged item at the root and deletes it through
    /// `delete_with_sectional_storage_function`, reporting the removed key
    /// bytes as a `BasicStorageRemoval` and the removed value bytes as a
    /// `SectionedStorageRemoval` under the default identifier's
    /// `UNKNOWN_EPOCH`. That is exactly the shape Drive's sectional removal
    /// callback produces, and it is what drives the `Basic += Sectioned`
    /// arm in `StorageRemovedBytes` when Merk folds the key and value
    /// removals together.
    ///
    /// Returns `(added_bytes, removed_bytes, removed_key_bytes,
    /// removed_value_bytes)` — the insertion's added bytes, the deletion's
    /// combined `StorageRemovedBytes`, and the two raw figures the callback
    /// observed (so the test can state what the legacy arithmetic loses).
    fn insert_then_delete_with_basic_key_and_default_sectioned_value(
        grove_version: &GroveVersion,
    ) -> (u32, StorageRemovedBytes, u32, u32) {
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"apple".to_vec())),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let mut observed_removed = (0u32, 0u32);
        let deletion_cost = db
            .delete_with_sectional_storage_function(
                EMPTY_PATH,
                b"key1",
                None,
                None,
                &mut |_element_flags, removed_key_bytes, removed_value_bytes| {
                    observed_removed = (removed_key_bytes, removed_value_bytes);
                    let mut removed_bytes = StorageRemovalPerEpochByIdentifier::default();
                    let mut removed_bytes_for_identity = IntMap::new();
                    removed_bytes_for_identity.insert(UNKNOWN_EPOCH, removed_value_bytes);
                    removed_bytes.insert(Identifier::default(), removed_bytes_for_identity);
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        SectionedStorageRemoval(removed_bytes),
                    ))
                },
                grove_version,
            )
            .cost_as_result()
            .expect("expected to delete successfully");

        (
            insertion_cost.storage_cost.added_bytes,
            deletion_cost.storage_cost.removed_bytes,
            observed_removed.0,
            observed_removed.1,
        )
    }

    /// Bytes added by inserting `key1 -> Item("cat", flags "apple")` at the
    /// root, and therefore the bytes a full-refund deletion must report.
    const BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES: u32 = 155;

    #[test]
    fn latest_delete_preserves_basic_plus_default_sectioned_removal_cost() {
        // GROVE_V4+ (issue #683): folding the basic key removal into the
        // sectioned value removal keeps the default section, so the deletion
        // refunds every byte the insertion added.
        let (added_bytes, removed_bytes, removed_key_bytes, removed_value_bytes) =
            insert_then_delete_with_basic_key_and_default_sectioned_value(GroveVersion::latest());

        assert_eq!(added_bytes, BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES);
        assert_eq!(
            removed_key_bytes + removed_value_bytes,
            BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES,
            "the sectional callback must see every added byte split across key and value"
        );

        // Both the basic key bytes and the sectioned value bytes land in the
        // default identifier's UNKNOWN_EPOCH entry.
        let mut expected_epochs = IntMap::new();
        expected_epochs.insert(UNKNOWN_EPOCH, BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES);
        let mut expected_sections = StorageRemovalPerEpochByIdentifier::default();
        expected_sections.insert(Identifier::default(), expected_epochs);
        assert_eq!(removed_bytes, SectionedStorageRemoval(expected_sections));
        assert_eq!(
            removed_bytes.total_removed_bytes(),
            BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES
        );
    }

    #[test]
    fn v3_delete_keeps_legacy_basic_plus_default_sectioned_removal_cost() {
        // GROVE_V3 is live on mainnet with the legacy removal arithmetic that
        // drops the mutated default section when a basic removal is combined
        // with a sectioned one (issue #683). Replay of v3 blocks depends on
        // reproducing that undercount EXACTLY, so this pins the legacy figure
        // rather than merely asserting "less than added": v3 must report an
        // empty sectioned removal — zero bytes — for a deletion whose callback
        // observed all 155 added bytes. The v4 fix is exercised by
        // `latest_delete_preserves_basic_plus_default_sectioned_removal_cost`.
        let (added_bytes, removed_bytes, removed_key_bytes, removed_value_bytes) =
            insert_then_delete_with_basic_key_and_default_sectioned_value(
                &grovedb_version::version::v3::GROVE_V3,
            );

        assert_eq!(added_bytes, BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES);
        assert_eq!(
            removed_key_bytes + removed_value_bytes,
            BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES,
            "the sectional callback sees the same bytes under every version"
        );

        // Legacy `Basic += Sectioned`: the default section is removed from the
        // map, mutated, and never reinserted — the whole removal is lost.
        assert_eq!(
            removed_bytes,
            SectionedStorageRemoval(StorageRemovalPerEpochByIdentifier::default()),
            "v3 must keep the legacy default-section-dropping removal arithmetic"
        );
        assert_eq!(
            removed_bytes.total_removed_bytes(),
            0,
            "v3 legacy total must stay pinned at the shipped value (0 of {} added bytes)",
            BASIC_PLUS_DEFAULT_SECTIONED_ADDED_BYTES
        );
    }

    #[test]
    fn test_batch_one_deletion_tree_costs_match_non_batch_on_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 113 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 37
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for empty tree value
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 37 + 39 = 113

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        tx.rollback().expect("expected to rollback");
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"key1".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];
        let batch_cost = db
            .apply_batch(ops, None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_item_costs_match_non_batch_on_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 113 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 71
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for required space for bytes
        //   3 bytes for value
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 71 + 39 = 147

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        tx.rollback().expect("expected to rollback");
        let ops = vec![QualifiedGroveDbOp::delete_op(vec![], b"key1".to_vec())];
        let batch_cost = db
            .apply_batch(ops, None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_tree_costs_match_non_batch_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 113 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 37
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for empty tree value
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 37 + 39 = 113

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        let db = make_empty_grovedb();

        let _insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"key1".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];
        let batch_cost = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_item_costs_match_non_batch_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 113 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 71
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for required space for bytes
        //   3 bytes for value
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 71 + 39 = 147

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        let db = make_empty_grovedb();

        let _insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let ops = vec![QualifiedGroveDbOp::delete_op(vec![], b"key1".to_vec())];
        let batch_cost = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_tree_with_flags_costs_match_non_batch_on_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree_with_flags(Some(b"dog".to_vec())),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 116 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 42
        //   1 for the flag option (but no flags)
        //   1 for the flags size
        //   3 bytes for flags
        //   1 for the enum type
        //   1 for empty tree value
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 42 + 40 = 119

        assert_eq!(insertion_cost.storage_cost.added_bytes, 119);
        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        tx.rollback().expect("expected to rollback");
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"key1".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];
        let batch_cost = db
            .apply_batch(ops, None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_tree_with_identity_cost_flags_costs_match_non_batch_on_transaction()
    {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree_with_flags(Some(vec![0, 0])),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete_with_sectional_storage_function(
                EMPTY_PATH,
                b"key1",
                None,
                Some(&tx),
                &mut |_element_flags, removed_key_bytes, removed_value_bytes| {
                    let mut removed_bytes = StorageRemovalPerEpochByIdentifier::default();
                    // we are removing 1 byte from epoch 0 for an identity
                    let mut removed_bytes_for_identity = IntMap::new();
                    removed_bytes_for_identity.insert(0, removed_key_bytes);
                    removed_bytes.insert(Identifier::default(), removed_bytes_for_identity);
                    let key_sectioned = SectionedStorageRemoval(removed_bytes);

                    let mut removed_bytes = StorageRemovalPerEpochByIdentifier::default();
                    // we are removing 1 byte from epoch 0 for an identity
                    let mut removed_bytes_for_identity = IntMap::new();
                    removed_bytes_for_identity.insert(0, removed_value_bytes);
                    removed_bytes.insert(Identifier::default(), removed_bytes_for_identity);
                    let value_sectioned = SectionedStorageRemoval(removed_bytes);
                    Ok((key_sectioned, value_sectioned))
                },
                grove_version,
            )
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 116 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 41
        //   1 for the flag option (but no flags)
        //   1 for the flags size
        //   2 bytes for flags
        //   1 for the enum type
        //   1 for empty tree value
        //   1 for basic merk
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 41 + 40 = 118

        assert_eq!(insertion_cost.storage_cost.added_bytes, 118);
        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );
        assert!(matches!(
            non_batch_cost.storage_cost.removed_bytes,
            SectionedStorageRemoval(_)
        ));

        tx.rollback().expect("expected to rollback");
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"key1".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];
        let batch_cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |_, _, _| Ok(false),
                |_element_flags, removed_key_bytes, removed_value_bytes| {
                    let mut removed_bytes = StorageRemovalPerEpochByIdentifier::default();
                    // we are removing 1 byte from epoch 0 for an identity
                    let mut removed_bytes_for_identity = IntMap::new();
                    removed_bytes_for_identity.insert(0, removed_key_bytes);
                    removed_bytes.insert(Identifier::default(), removed_bytes_for_identity);
                    let key_sectioned = SectionedStorageRemoval(removed_bytes);

                    let mut removed_bytes = StorageRemovalPerEpochByIdentifier::default();
                    // we are removing 1 byte from epoch 0 for an identity
                    let mut removed_bytes_for_identity = IntMap::new();
                    removed_bytes_for_identity.insert(0, removed_value_bytes);
                    removed_bytes.insert(Identifier::default(), removed_bytes_for_identity);
                    let value_sectioned = SectionedStorageRemoval(removed_bytes);
                    Ok((key_sectioned, value_sectioned))
                },
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
        assert!(matches!(
            batch_cost.storage_cost.removed_bytes,
            SectionedStorageRemoval(_)
        ));
    }

    #[test]
    fn test_batch_one_deletion_item_with_flags_costs_match_non_batch_on_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"apple".to_vec())),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 113 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 71
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for required space for bytes
        //   3 bytes for value
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 71 + 39 = 147

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        tx.rollback().expect("expected to rollback");
        let ops = vec![QualifiedGroveDbOp::delete_op(vec![], b"key1".to_vec())];
        let batch_cost = db
            .apply_batch(ops, None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_tree_with_flags_costs_match_non_batch_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree_with_flags(Some(b"dog".to_vec())),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 113 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 42
        //   1 for the flag option
        //   1 for flags size
        //   3 for flag bytes
        //   1 for the enum type
        //   1 for empty tree value
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 42 + 40 = 119

        assert_eq!(insertion_cost.storage_cost.added_bytes, 119);

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        let db = make_empty_grovedb();

        let _insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree_with_flags(Some(b"dog".to_vec())),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"key1".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];
        let batch_cost = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_item_with_flags_costs_match_non_batch_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"apple".to_vec())),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");

        // Explanation for 113 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 71
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for required space for bytes
        //   3 bytes for value
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 71 + 39 = 147

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        let db = make_empty_grovedb();

        let _insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"apple".to_vec())),
                None,
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let ops = vec![QualifiedGroveDbOp::delete_op(vec![], b"key1".to_vec())];
        let batch_cost = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }
}
