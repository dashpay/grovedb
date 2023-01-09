// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Insert operations

#[cfg(feature = "full")]
use std::{collections::HashMap, option::Option::None};

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use merk::{tree::NULL_HASH, Merk, MerkOptions};
#[cfg(feature = "full")]
use storage::rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext};

#[cfg(feature = "full")]
use crate::{
    reference_path::path_from_reference_path_type, Element, Error, GroveDb, Transaction,
    TransactionArg,
};

#[cfg(feature = "full")]
#[derive(Clone)]
/// Insert options
pub struct InsertOptions {
    /// Validate insertion does not override
    pub validate_insertion_does_not_override: bool,
    /// Validate insertion does not override tree
    pub validate_insertion_does_not_override_tree: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
}

#[cfg(feature = "full")]
impl Default for InsertOptions {
    fn default() -> Self {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: true,
            base_root_storage_is_free: true,
        }
    }
}

#[cfg(feature = "full")]
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

#[cfg(feature = "full")]
impl GroveDb {
    /// Insert operation
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
                    if element.is_tree() {
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
                                .map(hex::encode)
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
                    if element.is_tree() {
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
                                .map(hex::encode)
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

    /// Insert if not exists
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

#[cfg(feature = "full")]
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
    fn test_one_insert_sum_item_in_sum_tree_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"s", Element::empty_sum_tree(), None, Some(&tx))
            .unwrap()
            .expect("expected to add upper tree");

        let cost = db
            .insert(
                vec![b"s".as_slice()],
                b"key1",
                Element::new_sum_item(5),
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

        // Value -> 78
        //   1 for the enum type item
        //   1 for the value (encoded var vec)
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

        // Total 37 + 78 + 48 = 163
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5,
                storage_cost: StorageCost {
                    added_bytes: 162,
                    replaced_bytes: 83, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 143,
                hash_node_calls: 8,
            }
        );
    }

    #[test]
    fn test_one_insert_sum_item_under_sum_item_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"s", Element::empty_sum_tree(), None, Some(&tx))
            .unwrap()
            .expect("expected to add upper tree");

        db.insert(
            vec![b"s".as_slice()],
            b"key1",
            Element::new_sum_item(5),
            None,
            Some(&tx),
        )
        .unwrap()
        .expect("should insert");

        let cost = db
            .insert(
                vec![b"s".as_slice()],
                b"key2",
                Element::new_sum_item(6),
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

        // Value -> 77
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   1 for the value (encoded var vec)
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

        // Total 37 + 77 + 48 = 162
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 162,
                    replaced_bytes: 208, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 224,
                hash_node_calls: 10,
            }
        );
    }

    #[test]
    fn test_one_insert_bigger_sum_item_under_sum_item_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"s", Element::empty_sum_tree(), None, Some(&tx))
            .unwrap()
            .expect("expected to add upper tree");

        db.insert(
            vec![b"s".as_slice()],
            b"key1",
            Element::new_sum_item(126),
            None,
            Some(&tx),
        )
        .unwrap()
        .expect("should insert");

        // the cost of the varint goes up by 2 after 126 and another 2 at 32768
        let cost = db
            .insert(
                vec![b"s".as_slice()],
                b"key2",
                Element::new_sum_item(32768),
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

        // Value -> 81
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   5 for the value (encoded var vec)
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

        // Total 37 + 81 + 48 = 166
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 166,
                    replaced_bytes: 210, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 231,
                hash_node_calls: 10,
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
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
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
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(vec![], b"key1", Element::empty_sum_tree(), None, Some(&tx))
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 46
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        //   8 bytes for sum
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

        // Total 37 + 46 + 40 = 123

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 123,
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
                    replaced_bytes: 190,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 227, // todo verify this
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
                    replaced_bytes: 191, // todo: verify this
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
                storage_loaded_bytes: 224,
                hash_node_calls: 9, // todo: verify this
            }
        );
    }
}
