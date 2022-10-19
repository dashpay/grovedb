use std::{collections::HashMap, option::Option::None};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::Merk;
use storage::rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext};

use crate::{
    reference_path::path_from_reference_path_type, Element, Error, GroveDb, Transaction,
    TransactionArg,
};

pub struct InsertOptions {
    pub validate_insertion_does_not_override: bool,
    pub validate_insertion_does_not_override_tree: bool,
}

impl Default for InsertOptions {
    fn default() -> Self {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: true,
        }
    }
}

impl InsertOptions {
    fn checks_for_override(&self) -> bool {
        self.validate_insertion_does_not_override_tree || self.validate_insertion_does_not_override
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
        let mut subtree_to_insert_into: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
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
                let subtree_for_reference: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
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
                        referenced_element_value_hash
                    )
                );
            }
            _ => {
                cost_return_on_error!(&mut cost, element.insert(&mut subtree_to_insert_into, key));
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
                let subtree_for_reference: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
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
                        referenced_element_value_hash
                    )
                );
            }
            _ => {
                cost_return_on_error!(&mut cost, element.insert(&mut subtree_to_insert_into, key));
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
    use std::option::Option::None;

    use costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use pretty_assertions::assert_eq;

    use crate::{tests::make_empty_grovedb, Element};

    #[test]
    fn test_one_insert_cost() {
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

        // Value -> 68
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 98)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 68 + 39 = 144

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 144,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2, // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_insert_cost_under_tree() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), None, Some(&tx))
            .cost;

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
            )
            .cost;

        // Explanation for 187 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 98)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Root -> 39
        // 1 byte for the root key length size
        // 1 byte for the root value length size
        // 32 for the root key prefix
        // 4 bytes for the key to put in root
        // 1 byte for the root "r"

        // Total 37 + 72 + 39 = 148

        // Explanation for replaced bytes

        // Replaced parent Value -> 99
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   32 for empty tree
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 98)
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 148,
                    replaced_bytes: 99,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 285,
                hash_node_calls: 4, // todo: verify this
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
                seek_count: 12, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 1,
                    replaced_bytes: 253,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 363,
                hash_node_calls: 6, // todo: verify this
            }
        );
    }
}
