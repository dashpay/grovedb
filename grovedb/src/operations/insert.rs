use std::collections::HashMap;
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::Merk;
use storage::rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext};
use visualize::visualize_stdout;


use crate::{reference_path::path_from_reference_path_type, util::{
    merk_no_tx, merk_optional_tx, merk_using_tx, storage_context_optional_tx,
    storage_context_with_parent_no_tx, storage_context_with_parent_optional_tx,
    storage_context_with_parent_using_tx,
}, Element, Error, GroveDb, TransactionArg, Transaction};

impl GroveDb {
    pub fn insert<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
        where
            P: IntoIterator<Item = &'p [u8]>,
            <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        if let Some(transaction) = transaction {
            self.insert_on_transaction(path, key, element, transaction)
        } else {
            self.insert_without_transaction(path, key, element)
        }
    }

    fn insert_on_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: &'p Transaction,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        let mut merk_cache : HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbTransactionContext>> = HashMap::default();

        match element {
            Element::Tree(..) => {
                let merk = cost_return_on_error!(
                    &mut cost,
                    self.add_subtree_on_transaction(path_iter.clone(), key, element, transaction)
                );
                merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), merk);
            }
            Element::Reference(ref reference_path, ..) => {
                let reference_path = cost_return_on_error!(
                    &mut cost,
                    path_from_reference_path_type(
                        reference_path.clone(),
                        path_iter.clone(),
                        Some(key)
                    )
                    .wrap_with_cost(OperationCost::default())
                );

                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ))
                    .wrap_with_cost(cost);
                }

                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_invalid_path(path_iter.clone(), Some(transaction))
                );

                let (referenced_key, referenced_path) = reference_path.split_last().unwrap();
                let mut referenced_path_iter = referenced_path.iter().map(|x| x.as_slice());
                let referenced_element_value_hash_opt = merk_using_tx!(
                    &mut cost,
                    self.db,
                    referenced_path_iter.clone(),
                    transaction,
                    subtree,
                    {
                        let element = Element::get_value_hash(&subtree, referenced_key)
                            .unwrap_add_cost(&mut cost)
                            .unwrap();
                        merk_cache.insert(referenced_path.to_vec(), subtree);
                        element
                    }
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

                merk_using_tx!(
                    &mut cost,
                    self.db,
                    path_iter.clone(),
                    transaction,
                    subtree,
                    {
                        cost_return_on_error!(
                            &mut cost,
                            element.insert_reference(
                                &mut subtree,
                                key,
                                referenced_element_value_hash
                            )
                        );
                        merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), subtree);
                    }
                );
            }
            Element::Item(..) => {
                // If path is empty that means there is an attempt to insert
                // something into a root tree and this branch is for anything
                // but trees
                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leaves",
                    ))
                    .wrap_with_cost(cost);
                }
                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_invalid_path(path_iter.clone(), Some(transaction))
                );
                merk_using_tx!(
                    &mut cost,
                    self.db,
                    path_iter.clone(),
                    transaction,
                    subtree,
                    {
                        cost_return_on_error!(&mut cost, element.insert(&mut subtree, key));
                        merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), subtree);
                    }
                );
            }
        }
        cost_return_on_error!(&mut cost, self.propagate_changes_with_transaction(merk_cache, path_iter, transaction));

        Ok(()).wrap_with_cost(cost)
    }

    fn insert_without_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element
    ) -> CostResult<(), Error>
        where
            P: IntoIterator<Item = &'p [u8]>,
            <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        let mut merk_cache : HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbStorageContext>>  = HashMap::default();

        match element {
            Element::Tree(..) => {
                let merk = cost_return_on_error!(
                    &mut cost,
                    self.add_subtree_without_transaction(path_iter.clone(), key, element)
                );
                merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), merk);
            }
            Element::Reference(ref reference_path, ..) => {
                let reference_path = cost_return_on_error!(
                    &mut cost,
                    path_from_reference_path_type(
                        reference_path.clone(),
                        path_iter.clone(),
                        Some(key)
                    )
                    .wrap_with_cost(OperationCost::default())
                );

                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ))
                        .wrap_with_cost(cost);
                }

                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_invalid_path(path_iter.clone(), None)
                );

                let (referenced_key, referenced_path) = reference_path.split_last().unwrap();
                let mut referenced_path_iter = referenced_path.iter().map(|x| x.as_slice());
                let referenced_element_value_hash_opt = merk_no_tx!(
                    &mut cost,
                    self.db,
                    referenced_path_iter,
                    subtree,
                    {
                        let element = Element::get_value_hash(&subtree, referenced_key)
                            .unwrap_add_cost(&mut cost)
                            .unwrap();
                        merk_cache.insert(referenced_path.to_vec(), subtree);
                        element
                    }
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

                merk_no_tx!(
                    &mut cost,
                    self.db,
                    path_iter.clone(),
                    subtree,
                    {
                        cost_return_on_error!(
                            &mut cost,
                            element.insert_reference(
                                &mut subtree,
                                key,
                                referenced_element_value_hash
                            )
                        );
                        merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), subtree);
                    }
                );
            }
            Element::Item(..) => {
                // If path is empty that means there is an attempt to insert
                // something into a root tree and this branch is for anything
                // but trees
                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leaves",
                    ))
                        .wrap_with_cost(cost);
                }
                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_invalid_path(path_iter.clone(), None)
                );
                merk_no_tx!(
                    &mut cost,
                    self.db,
                    path_iter.clone(),
                    subtree,
                    {
                        cost_return_on_error!(&mut cost, element.insert(&mut subtree, key));
                        merk_cache.insert(path_iter.clone().map(|k| k.to_vec()).collect(), subtree);
                    }
                );
            }
        }
        cost_return_on_error!(&mut cost, self.propagate_changes_without_transaction(merk_cache, path_iter));

        Ok(()).wrap_with_cost(cost)
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_subtree_on_transaction<'p, P>(
        &'p self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: &'p Transaction,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let element_flag = cost_return_on_error_no_add!(
            &cost,
            match element {
                Element::Tree(_, flag) => Ok(flag),
                _ => Err(Error::CorruptedData("element should be a tree".to_owned())),
            }
        );
        let path_iter = path.into_iter();

        let mut parent_subtree: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path_iter, transaction)
        );
        let element = Element::empty_tree_with_flags(element_flag);
        cost_return_on_error!(&mut cost, element.insert(&mut parent_subtree, key));

        Ok(parent_subtree).wrap_with_cost(cost)
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_subtree_without_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element
    ) -> CostResult<Merk<PrefixedRocksDbStorageContext>, Error>
        where
            P: IntoIterator<Item = &'p [u8]>,
            <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let element_flag = cost_return_on_error_no_add!(
            &cost,
            match element {
                Element::Tree(_, flag) => Ok(flag),
                _ => Err(Error::CorruptedData("element should be a tree".to_owned())),
            }
        );
        let path_iter = path.into_iter();

        let mut parent_subtree: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path_iter)
        );
        let element = Element::empty_tree_with_flags(element_flag);
        cost_return_on_error!(&mut cost, element.insert(&mut parent_subtree, key));

        Ok(parent_subtree).wrap_with_cost(cost)
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
            self.insert(path_iter, key, element, transaction)
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

    use crate::{tests::make_empty_grovedb, Element};

    #[test]
    fn test_one_insert_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(vec![], b"key1", Element::empty_tree(), Some(&tx))
            .cost;
        // Explanation for 214 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 99
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   32 for empty tree
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

        // Total 37 + 99 + 39 + 39

        // Hash node calls
        // 2 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 4, // 1 to get tree, 1 to insert, 1 for root, 1 for insert into root
                storage_cost: StorageCost {
                    added_bytes: 214,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 4, // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_insert_cost_under_tree() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), Some(&tx))
            .cost;

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item(b"test".to_vec()),
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

        // Total 37 + 72 + 39 + 39

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
                seek_count: 11, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 187,
                    replaced_bytes: 99,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 285,
                hash_node_calls: 8, // todo: verify this
            }
        );
    }

    #[test]
    fn test_one_update_bigger_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"tree", Element::empty_tree(), Some(&tx))
            .cost;

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item(b"test".to_vec()),
            Some(&tx),
        )
        .cost;

        let cost = db
            .insert(
                vec![b"tree".as_slice()],
                b"key1",
                Element::new_item(b"test1".to_vec()),
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
