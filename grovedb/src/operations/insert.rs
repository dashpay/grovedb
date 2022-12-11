use std::{collections::HashMap, option::Option::None};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::{tree::NULL_HASH, Merk, MerkOptions};
use storage::rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext};

use crate::{
    reference_path::path_from_reference_path_type, Element, Error, GroveDb, Transaction,
    TransactionArg,
};

#[derive(Clone)]
pub struct InsertOptions {
    pub validate_insertion_does_not_override: bool,
    pub validate_insertion_does_not_override_tree: bool,
    pub base_root_storage_is_free: bool,
}

impl Default for InsertOptions {
    fn default() -> Self {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: true,
            base_root_storage_is_free: true,
        }
    }
}

impl InsertOptions {
    fn checks_for_override(&self) -> bool {
        self.validate_insertion_does_not_override_tree || self.validate_insertion_does_not_override
    }

    fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}

impl GroveDb {
    pub fn insert<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        options: Option<InsertOptions>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        if let Some(transaction) = transaction {
            self.insert_on_transaction(path, key, element, options.unwrap_or_default(), transaction)
        } else {
            self.insert_without_transaction(path, key, element, options.unwrap_or_default())
        }
    }

    fn insert_on_transaction<'db, 'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        let mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();

        let merk = cost_return_on_error!(
            &mut cost,
            self.add_element_on_transaction(path_iter.clone(), key, element, options, transaction)
        );
        merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(merk_cache, path_iter, transaction)
        );

        Ok(()).wrap_with_cost(cost)
    }

    fn insert_without_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        options: InsertOptions,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        let mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbStorageContext>> =
            HashMap::default();

        let merk = cost_return_on_error!(
            &mut cost,
            self.add_element_without_transaction(path_iter.clone(), key, element, options)
        );
        merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), merk);

        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_without_transaction(merk_cache, path_iter)
        );

        Ok(()).wrap_with_cost(cost)
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_element_on_transaction<'db, 'p, P>(
        &'db self,
        path: P,
        key: &'p [u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();
        let path_iter = path.into_iter();
        let mut subtree_to_insert_into = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path_iter.clone(), transaction)
        );
        // if we don't allow a tree override then we should check

        if options.checks_for_override() {
            let maybe_element_bytes = cost_return_on_error!(
                &mut cost,
                subtree_to_insert_into
                    .get(key)
                    .map_err(|e| Error::CorruptedData(e.to_string()))
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
                        &cost,
                        Element::deserialize(element_bytes.as_slice()).map_err(|_| {
                            Error::CorruptedData(String::from("unable to deserialize element"))
                        })
                    );
                    if matches!(element, Element::Tree(..)) {
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
                let reference_path = cost_return_on_error!(
                    &mut cost,
                    path_from_reference_path_type(reference_path.clone(), path_iter, Some(key))
                        .wrap_with_cost(OperationCost::default())
                );

                let (referenced_key, referenced_path) = reference_path.split_last().unwrap();
                let referenced_path_iter = referenced_path.iter().map(|x| x.as_slice());
                let subtree_for_reference = cost_return_on_error!(
                    &mut cost,
                    self.open_transactional_merk_at_path(referenced_path_iter, transaction)
                );

                let referenced_element_value_hash_opt = cost_return_on_error!(
                    &mut cost,
                    Element::get_value_hash(&subtree_for_reference, referenced_key)
                );

                let referenced_element_value_hash = cost_return_on_error!(
                    &mut cost,
                    referenced_element_value_hash_opt
                        .ok_or({
                            let reference_string = reference_path
                                .iter()
                                .map(|a| hex::encode(a))
                                .collect::<Vec<String>>()
                                .join("/");
                            Error::MissingReference(format!(
                                "reference {}/{} can not be found",
                                reference_string,
                                hex::encode(key)
                            ))
                        })
                        .wrap_with_cost(OperationCost::default())
                );

                cost_return_on_error!(
                    &mut cost,
                    element.insert_reference(
                        &mut subtree_to_insert_into,
                        key,
                        referenced_element_value_hash,
                        Some(options.as_merk_options()),
                    )
                );
            }
            Element::Tree(ref value, _) | Element::SumTree(ref value, ..) => {
                if value.is_some() {
                    return Err(Error::InvalidCodeExecution(
                        "a tree should be empty at the moment of insertion when not using batches",
                    ))
                    .wrap_with_cost(cost);
                } else {
                    cost_return_on_error!(
                        &mut cost,
                        element.insert_subtree(
                            &mut subtree_to_insert_into,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options())
                        )
                    );
                }
            }
            _ => {
                cost_return_on_error!(
                    &mut cost,
                    element.insert(
                        &mut subtree_to_insert_into,
                        key,
                        Some(options.as_merk_options())
                    )
                );
            }
        }

        Ok(subtree_to_insert_into).wrap_with_cost(cost)
    }

    /// Add an empty tree or item to a parent tree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_element_without_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        options: InsertOptions,
    ) -> CostResult<Merk<PrefixedRocksDbStorageContext>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();
        let path_iter = path.into_iter();
        let mut subtree_to_insert_into: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path_iter.clone())
        );

        if options.checks_for_override() {
            let maybe_element_bytes = cost_return_on_error!(
                &mut cost,
                subtree_to_insert_into
                    .get(key)
                    .map_err(|e| Error::CorruptedData(e.to_string()))
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
                        &cost,
                        Element::deserialize(element_bytes.as_slice()).map_err(|_| {
                            Error::CorruptedData(String::from("unable to deserialize element"))
                        })
                    );
                    if matches!(element, Element::Tree(..)) {
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
                let reference_path = cost_return_on_error!(
                    &mut cost,
                    path_from_reference_path_type(reference_path.clone(), path_iter, Some(key))
                        .wrap_with_cost(OperationCost::default())
                );

                let (referenced_key, referenced_path) = reference_path.split_last().unwrap();
                let referenced_path_iter = referenced_path.iter().map(|x| x.as_slice());
                let subtree_for_reference = cost_return_on_error!(
                    &mut cost,
                    self.open_non_transactional_merk_at_path(referenced_path_iter)
                );

                let referenced_element_value_hash_opt = cost_return_on_error!(
                    &mut cost,
                    Element::get_value_hash(&subtree_for_reference, referenced_key)
                );

                let referenced_element_value_hash = cost_return_on_error!(
                    &mut cost,
                    referenced_element_value_hash_opt
                        .ok_or({
                            let reference_string = reference_path
                                .iter()
                                .map(|a| hex::encode(a))
                                .collect::<Vec<String>>()
                                .join("/");
                            Error::MissingReference(format!(
                                "reference {}/{} can not be found",
                                reference_string,
                                hex::encode(key)
                            ))
                        })
                        .wrap_with_cost(OperationCost::default())
                );

                cost_return_on_error!(
                    &mut cost,
                    element.insert_reference(
                        &mut subtree_to_insert_into,
                        key,
                        referenced_element_value_hash,
                        Some(options.as_merk_options())
                    )
                );
            }
            Element::Tree(ref value, _) => {
                if value.is_some() {
                    return Err(Error::InvalidCodeExecution(
                        "a tree should be empty at the moment of insertion when not using batches",
                    ))
                    .wrap_with_cost(cost);
                } else {
                    cost_return_on_error!(
                        &mut cost,
                        element.insert_subtree(
                            &mut subtree_to_insert_into,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options())
                        )
                    );
                }
            }
            _ => {
                cost_return_on_error!(
                    &mut cost,
                    element.insert(
                        &mut subtree_to_insert_into,
                        key,
                        Some(options.as_merk_options())
                    )
                );
            }
        }

        Ok(subtree_to_insert_into).wrap_with_cost(cost)
    }

    pub fn insert_if_not_exists<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();
        if cost_return_on_error!(&mut cost, self.has_raw(path_iter.clone(), key, transaction)) {
            Ok(false).wrap_with_cost(cost)
        } else {
            self.insert(path_iter, key, element, None, transaction)
                .map_ok(|_| true)
                .add_cost(cost)
        }
    }
}

#[cfg(test)]
mod tests {
    use costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use pretty_assertions::assert_eq;

    use crate::{
        operations::insert::InsertOptions,
        tests::{make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    #[test]
    fn test_non_root_insert_item_without_transaction() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        db.insert([TEST_LEAF], b"key", element.clone(), None, None)
            .unwrap()
            .expect("successful insert");
        assert_eq!(
            db.get([TEST_LEAF], b"key", None)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_subtree_then_insert_item_without_transaction() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        // Insert a subtree first
        db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert");
        // Insert an element into subtree
        db.insert([TEST_LEAF, b"key1"], b"key2", element.clone(), None, None)
            .unwrap()
            .expect("successful value insert");
        assert_eq!(
            db.get([TEST_LEAF, b"key1"], b"key2", None)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_item_with_transaction() {
        let item_key = b"key3";

        let db = make_test_grovedb();
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db.get([TEST_LEAF], item_key, None).unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let element1 = Element::new_item(b"ayy".to_vec());

        db.insert([TEST_LEAF], item_key, element1, None, Some(&transaction))
            .unwrap()
            .expect("cannot insert an item into GroveDB");

        // The key was inserted inside the transaction, so it shouldn't be
        // possible to get it back without committing or using transaction
        let result = db.get([TEST_LEAF], item_key, None).unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
        // Check that the element can be retrieved when transaction is passed
        let result_with_transaction = db
            .get([TEST_LEAF], item_key, Some(&transaction))
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::new_item(b"ayy".to_vec()));

        // Test that commit works
        db.commit_transaction(transaction).unwrap().unwrap();

        // Check that the change was committed
        let result = db
            .get([TEST_LEAF], item_key, None)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::new_item(b"ayy".to_vec()));
    }

    #[test]
    fn test_non_root_insert_subtree_with_transaction() {
        let subtree_key = b"subtree_key";

        let db = make_test_grovedb();
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db.get([TEST_LEAF], subtree_key, None).unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        db.insert(
            [TEST_LEAF],
            subtree_key,
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("cannot insert an item into GroveDB");

        let result = db.get([TEST_LEAF], subtree_key, None).unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let result_with_transaction = db
            .get([TEST_LEAF], subtree_key, Some(&transaction))
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::empty_tree());

        db.commit_transaction(transaction).unwrap().unwrap();

        let result = db
            .get([TEST_LEAF], subtree_key, None)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::empty_tree());
    }

    #[test]
    fn test_insert_if_not_exists() {
        let db = make_test_grovedb();

        // Insert twice at the same path
        assert!(db
            .insert_if_not_exists([TEST_LEAF], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("Provided valid path"));
        assert!(!db
            .insert_if_not_exists([TEST_LEAF], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("Provided valid path"));

        // Should propagate errors from insertion
        let result = db
            .insert_if_not_exists(
                [TEST_LEAF, b"unknown"],
                b"key1",
                Element::empty_tree(),
                None,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidParentLayerPath(_))));
    }

    #[test]
    fn test_one_insert_item_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                vec![],
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                Some(&tx),
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
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
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
    fn test_one_insert_item_cost_with_flags() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                vec![],
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"dog".to_vec())),
                None,
                Some(&tx),
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

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 76 + 39 = 152

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 152,
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
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(vec![], b"key1", Element::empty_tree(), None, Some(&tx))
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

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 38 + 39 = 114

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 114,
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
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                vec![],
                b"key1",
                Element::empty_tree_with_flags(Some(b"cat".to_vec())),
                None,
                Some(&tx),
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

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 42 + 39 = 118

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
                    added_bytes: 118,
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
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .unwrap();

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 74
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 73)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 74 + 39 = 149

        // Explanation for replaced bytes

        // Replaced parent Value -> 77
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 76)
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 149,
                    replaced_bytes: 77,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 144, // todo: verify this
                hash_node_calls: 8,        // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_apple_flags_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                vec![],
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
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

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 79 + 39 = 155

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // The node hash is not being called, as the root hash isn't cached
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 155,
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
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .unwrap();

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
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

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 79 + 39 = 155

        // Explanation for replaced bytes

        // Replaced parent Value -> 77
        //   1 for the flag option
        //   3 bytes for flags
        //   1 for flags size
        //   1 for the enum type
        //   1 for an empty option
        //   1 for the basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 75)

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
                    added_bytes: 155,
                    replaced_bytes: 77,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 144, // todo: verify this
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_flags_cost_under_tree_with_flags() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            vec![],
            b"tree",
            Element::empty_tree_with_flags(Some(b"cat".to_vec())),
            None,
            Some(&tx),
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
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

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 79 + 39 = 155

        // Explanation for replaced bytes

        // Replaced parent Value -> 81
        //   1 for the flag option
        //   3 bytes for flags
        //   1 for flags size
        //   1 for the enum type
        //   1 for an empty option
        //   1 for basic merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 39 for the child to parent hook
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
                    added_bytes: 155,
                    replaced_bytes: 81,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 152, // todo: verify this
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_update_item_same_cost_at_root() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            vec![],
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
        )
        .cost;

        let cost = db
            .insert(
                vec![],
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 110 replaced bytes

        // Value -> 71
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // 71 + 39 = 110

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 111,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 77,
                hash_node_calls: 2,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_tree() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), None, Some(&tx))
            .cost;

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
        )
        .cost;

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 188,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 227,//todo verify this
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_update_bigger_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), None, Some(&tx))
            .cost;

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item(b"test".to_vec()),
            None,
            Some(&tx),
        )
        .cost;

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item(b"test1".to_vec()),
                None,
                Some(&tx),
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 1,
                    replaced_bytes: 189, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 228,
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_update_tree_bigger_cost_with_flags() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), None, Some(&tx))
            .cost;

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_tree(None),
            None,
            Some(&tx),
        )
        .cost;

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_tree_with_flags(None, Some(b"cat".to_vec())),
                Some(InsertOptions {
                    validate_insertion_does_not_override: false,
                    validate_insertion_does_not_override_tree: false,
                    base_root_storage_is_free: true,
                }),
                Some(&tx),
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 4 added bytes

        // 1 for size of "cat" flags
        // 3 for bytes

        // Explanation for replaced bytes

        // Replaced parent Value -> 76
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Replaced current tree -> 76
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 4,
                    replaced_bytes: 154, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 224,
                hash_node_calls: 9, // todo: verify this
            }
        );
    }
}
