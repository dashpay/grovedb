use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::tree::NULL_HASH;
use grovedb_path::SubtreePath;
use grovedb_storage::StorageBatch;
use grovedb_version::{dispatch_version, version::GroveVersion};

use super::InsertOptions;
use crate::{
    bidirectional_references::{
        process_bidirectional_reference_insertion, process_update_element_with_backward_references,
    },
    merk_cache::MerkCache,
    reference_path::follow_reference,
    Element, Error, GroveDb, Transaction,
};

pub(super) fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
    db: &GroveDb,
    path: SubtreePath<'b, B>,
    key: &[u8],
    element: Element,
    options: InsertOptions,
    transaction: &'db Transaction,
    batch: &StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    dispatch_version!(
        "insert_on_transaction",
        grove_version
            .grovedb_versions
            .operations
            .insert
            .insert_on_transaction,
        1 => {}
    );

    let mut cost = Default::default();

    let cache = MerkCache::new(db, transaction, grove_version);

    let mut subtree_to_insert_into =
        cost_return_on_error!(&mut cost, cache.get_merk(path.derive_owned()));

    if options.checks_for_override() {
        let maybe_element_bytes = cost_return_on_error!(
            &mut cost,
            subtree_to_insert_into.for_merk(|m| m
                .get(
                    key,
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(e.to_string())))
        );
        if let Some(element_bytes) = maybe_element_bytes {
            if options.validate_insertion_does_not_override {
                return Err(Error::OverrideNotAllowed(
                    "insertion not allowed to override",
                ))
                .wrap_with_cost(cost);
            }
            if options.validate_insertion_does_not_override_tree {
                let element = cost_return_on_error_no_add!(
                    cost,
                    Element::deserialize(element_bytes.as_slice(), grove_version).map_err(|_| {
                        Error::CorruptedData(String::from("unable to deserialize element"))
                    })
                );
                if element.is_any_tree() {
                    return Err(Error::OverrideNotAllowed(
                        "insertion not allowed to override tree",
                    ))
                    .wrap_with_cost(cost);
                }
            }
        }
    }

    match element {
        Element::Reference(ref reference_path, ..) => {
            let resolved_reference = cost_return_on_error!(
                &mut cost,
                follow_reference(&cache, path.derive_owned(), key, reference_path.clone())
            );
            let referenced_item: Element = resolved_reference.target_element;

            if matches!(
                referenced_item,
                Element::Tree(_, _) | Element::SumTree(_, _, _)
            ) {
                return Err(Error::NotSupported(
                    "References cannot point to subtrees".to_owned(),
                ))
                .wrap_with_cost(cost);
            }

            if options.propagate_backward_references {
                let delta = cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert_reference_if_changed_value(
                        m,
                        key,
                        resolved_reference.target_node_value_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    ))
                );

                cost_return_on_error!(
                    &mut cost,
                    process_update_element_with_backward_references(
                        &cache,
                        subtree_to_insert_into.clone(),
                        path.derive_owned(),
                        key,
                        delta
                    )
                );
            } else {
                cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert_reference(
                        m,
                        key,
                        resolved_reference.target_node_value_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    ))
                );
            }
        }
        Element::Tree(ref value, _)
        | Element::SumTree(ref value, ..)
        | Element::BigSumTree(ref value, ..)
        | Element::CountTree(ref value, ..) => {
            if value.is_some() {
                return Err(Error::InvalidCodeExecution(
                    "a tree should be empty at the moment of insertion when not using batches",
                ))
                .wrap_with_cost(cost);
            } else {
                if options.propagate_backward_references {
                    let delta = cost_return_on_error!(
                        &mut cost,
                        subtree_to_insert_into.for_merk(|m| element.insert_subtree_if_changed(
                            m,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options()),
                            grove_version
                        ))
                    );

                    cost_return_on_error!(
                        &mut cost,
                        process_update_element_with_backward_references(
                            &cache,
                            subtree_to_insert_into.clone(),
                            path.derive_owned(),
                            key,
                            delta
                        )
                    );
                } else {
                    cost_return_on_error!(
                        &mut cost,
                        subtree_to_insert_into.for_merk(|m| element.insert_subtree(
                            m,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options()),
                            grove_version
                        ))
                    );
                }
            }
        }

        Element::BidirectionalReference(reference) => {
            cost_return_on_error!(
                &mut cost,
                process_bidirectional_reference_insertion(
                    &cache,
                    path,
                    key,
                    reference,
                    Some(options)
                )
            );
        }
        _ => {
            if options.propagate_backward_references {
                let delta = cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert_if_changed_value(
                        m,
                        key,
                        Some(options.as_merk_options()),
                        grove_version
                    ))
                );
                cost_return_on_error!(
                    &mut cost,
                    process_update_element_with_backward_references(
                        &cache,
                        subtree_to_insert_into.clone(),
                        path.derive_owned(),
                        key,
                        delta
                    )
                );
            } else {
                cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert(
                        m,
                        key,
                        Some(options.as_merk_options()),
                        grove_version
                    ))
                );
            }
        }
    }

    let result_batch = cost_return_on_error!(&mut cost, cache.into_batch());

    batch.merge(*result_batch);

    Ok(()).wrap_with_cost(cost)
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests {
    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use grovedb_version::version::v2::GROVE_V2;
    use pretty_assertions::assert_eq;

    use crate::{
        operations::insert::InsertOptions,
        tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    #[test]
    fn test_non_root_insert_item_without_transaction() {
        let grove_version = &GROVE_V2;
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful insert");
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key", None, grove_version)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_subtree_then_insert_item_without_transaction() {
        let grove_version = &GROVE_V2;
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());

        // Insert a subtree first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_item_with_transaction() {
        let grove_version = &GROVE_V2;
        let item_key = b"key3";

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let element1 = Element::new_item(b"ayy".to_vec());

        db.insert(
            [TEST_LEAF].as_ref(),
            item_key,
            element1,
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item into GroveDB");

        // The key was inserted inside the transaction, so it shouldn't be
        // possible to get it back without committing or using transaction
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
        // Check that the element can be retrieved when transaction is passed
        let result_with_transaction = db
            .get(
                [TEST_LEAF].as_ref(),
                item_key,
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::new_item(b"ayy".to_vec()));

        // Test that commit works
        db.commit_transaction(transaction).unwrap().unwrap();

        // Check that the change was committed
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::new_item(b"ayy".to_vec()));
    }

    #[test]
    fn test_non_root_insert_subtree_with_transaction() {
        let grove_version = &GROVE_V2;
        let subtree_key = b"subtree_key";

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        db.insert(
            [TEST_LEAF].as_ref(),
            subtree_key,
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item into GroveDB");

        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let result_with_transaction = db
            .get(
                [TEST_LEAF].as_ref(),
                subtree_key,
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::empty_tree());

        db.commit_transaction(transaction).unwrap().unwrap();

        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::empty_tree());
    }

    #[test]
    fn test_insert_if_not_exists() {
        let grove_version = &GROVE_V2;
        let db = make_test_grovedb(grove_version);

        // Insert twice at the same path
        assert!(db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version
            )
            .unwrap()
            .expect("Provided valid path"));
        assert!(!db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version
            )
            .unwrap()
            .expect("Provided valid path"));

        // Should propagate errors from insertion
        let result = db
            .insert_if_not_exists(
                [TEST_LEAF, b"unknown"].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidParentLayerPath(_))));
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element() {
        let grove_version = &GROVE_V2;
        let db = make_test_grovedb(grove_version);

        let element_key = b"key1";
        let new_element = Element::new_item(b"new_value".to_vec());

        // Insert a new element and check if it returns None
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                new_element.clone(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("Expected insertion of new element");

        assert_eq!(result, None);

        // Try inserting the same element again and expect it to return the existing
        // element
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                Element::new_item(b"another_value".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("Expected to return existing element");

        assert_eq!(result, Some(new_element.clone()));

        // Check if the existing element is still the original one and not replaced
        let fetched_element = db
            .get([TEST_LEAF].as_ref(), element_key, None, grove_version)
            .unwrap()
            .expect("Expected to retrieve the existing element");

        assert_eq!(fetched_element, new_element);
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element_with_transaction() {
        let grove_version = &GROVE_V2;
        let db = make_test_grovedb(grove_version);

        let element_key = b"key2";
        let new_element = Element::new_item(b"transaction_value".to_vec());
        let transaction = db.start_transaction();

        // Insert a new element within a transaction and check if it returns None
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                new_element.clone(),
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected insertion of new element in transaction");

        assert_eq!(result, None);

        // Try inserting the same element again within the transaction
        // and expect it to return the existing element
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                Element::new_item(b"another_transaction_value".to_vec()),
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to return existing element in transaction");

        assert_eq!(result, Some(new_element.clone()));

        // Commit the transaction
        db.commit_transaction(transaction).unwrap().unwrap();

        // Check if the element is still the original one and not replaced
        let fetched_element = db
            .get([TEST_LEAF].as_ref(), element_key, None, grove_version)
            .unwrap()
            .expect("Expected to retrieve the existing element after transaction commit");

        assert_eq!(fetched_element, new_element);
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element_invalid_path() {
        let grove_version = &GROVE_V2;
        let db = make_test_grovedb(grove_version);

        // Try inserting to an invalid path and expect an error
        let result = db.insert_if_not_exists_return_existing_element(
            [b"invalid_path"].as_ref(),
            b"key",
            Element::new_item(b"value".to_vec()),
            None,
            grove_version,
        );

        assert!(matches!(
            result.unwrap(),
            Err(Error::InvalidParentLayerPath(_))
        ));
    }

    #[test]
    fn test_one_insert_item_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 for Basic merk
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Basic Merk 1
        // Child Heights 2

        // Total 37 + 72 + 40 = 149

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // TODO: verify, caching happens
                storage_cost: StorageCost {
                    added_bytes: 149,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
            }
        );
    }

    #[test]
    fn test_one_insert_sum_item_in_sum_tree_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key1",
                Element::new_sum_item(5),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        //   1 for the flag option (but no flags)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 84, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 156,
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_insert_sum_item_under_sum_item_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        db.insert(
            [b"s".as_slice()].as_ref(),
            b"key1",
            Element::new_sum_item(5),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key2",
                Element::new_sum_item(6),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170

        // replaced bytes
        // 133 for key1 (higher node/same merk level)
        // ?

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 217,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 232,
                hash_node_calls: 10,
            }
        );
    }

    #[test]
    fn test_one_insert_bigger_sum_item_under_sum_item_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        db.insert(
            [b"s".as_slice()].as_ref(),
            b"key1",
            Element::new_sum_item(126),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        // the cost of the varint goes up by 2 after 126 and another 2 at 32768
        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key2",
                Element::new_sum_item(32768),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 81)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 217, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 237,
                hash_node_calls: 10,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_with_flags() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"dog".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 76
        //   1 for the flag option
        //   3 for flags
        //   1 for flags length
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 76 + 40 = 153

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // TODO: verify, caching happens
                storage_cost: StorageCost {
                    added_bytes: 153,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
            }
        );
    }

    #[test]
    fn test_one_insert_empty_tree_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 38
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 38 + 40 = 115

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 115,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3, // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_insert_empty_sum_tree_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_sum_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 47
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        //   9 bytes for sum
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 47 + 40 = 124

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 124,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3, // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_insert_empty_tree_cost_with_flags() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree_with_flags(Some(b"cat".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 42
        //   1 for the flag option
        //   1 byte for flag size
        //   3 bytes for flags
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 42 + 40 = 119

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash
        // The node hash is not being called, as the root hash isn't cached
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 119,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_under_tree() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 73
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 72)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 73 + 40 = 150

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for a basic merk
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 150,
                    replaced_bytes: 78,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 152, // todo: verify this
                hash_node_calls: 8,        // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_under_count_tree() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_count_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 81
        //   1 for the enum type item
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for the flag option (but no flags)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Count node
        // 1 byte for the value_size (required space for 1)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Count Merk 9
        // Child Heights 2

        // Total 37 + 81 + 48 = 166

        // Explanation for replaced bytes

        // Replaced parent Value -> 86
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for the count merk
        //   9 for the count
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 166,
                    replaced_bytes: 87,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 162, // todo: verify this
                hash_node_calls: 8,        // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_apple_flags_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 79 + 40 = 156

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // The node hash is not being called, as the root hash isn't cached
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_flags_cost_under_tree() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for the basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 78)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 79 + 40 = 156

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for a basic merk
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size

        // Hash node calls
        // 1 for getting the merk
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 2 for the node hash

        // on the level above
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 78,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 152, // todo: verify this
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_flags_cost_under_tree_with_flags() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree_with_flags(Some(b"cat".to_vec())),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 78)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 79 + 40 = 156

        // Explanation for replaced bytes

        // Replaced parent Value -> 82
        //   1 for the flag option
        //   3 bytes for flags
        //   1 for flags size
        //   1 for the enum type
        //   1 for an empty option
        //   1 for basic merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for the child to parent hook
        // 2 byte for the value_size (required space)

        // Hash node calls
        // 1 for getting the merk
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 2 for the node hash

        // on the level above
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 82,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 160, // todo: verify this
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_update_item_same_cost_at_root() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 110 replaced bytes

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 71)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // 72 + 40 = 112

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 112,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 77,
                hash_node_calls: 2,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_tree() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 190,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 230, // todo verify this
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_bigger_sum_item() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(15),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(1000000),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 248,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 266, // todo verify this
                hash_node_calls: 9,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_bigger_sum_item_parent_sum_tree_already_big(
    ) {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [1; 32].as_slice(),
            Element::new_sum_item(1000000),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(15),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(1000000),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 9, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 409, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 487, // todo verify this
                hash_node_calls: 11,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_smaller_sum_item() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(1000000),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(15),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 248,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 276, // todo verify this
                hash_node_calls: 9,
            }
        );
    }

    #[test]
    fn test_one_update_bigger_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_item(b"test".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test1".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 1,
                    replaced_bytes: 191, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 231,
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_update_tree_bigger_cost_with_flags() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_tree(None),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_tree_with_flags(None, Some(b"cat".to_vec())),
                Some(InsertOptions {
                    validate_insertion_does_not_override: false,
                    validate_insertion_does_not_override_tree: false,
                    base_root_storage_is_free: true,
                    propagate_backward_references: false,
                }),
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 4 added bytes

        // 1 for size of "cat" flags
        // 3 for bytes

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for child to parent hook
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Replaced current tree -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for child to parent hook
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 4,
                    replaced_bytes: 156,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 227,
                hash_node_calls: 9, // todo: verify this
            }
        );
    }
}
