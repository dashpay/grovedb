use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostsExt, OperationCost,
};
use merk::{Merk, ROOT_KEY_KEY};
use storage::{Storage, StorageContext};

use crate::{
    util::{merk_optional_tx, meta_storage_context_optional_tx, storage_context_optional_tx},
    Element, Error, GroveDb, TransactionArg, ROOT_LEAFS_SERIALIZED_KEY,
};

impl GroveDb {
    pub fn insert<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> CostContext<Result<(), Error>>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        match element {
            Element::Tree(..) => {
                if path_iter.len() == 0 {
                    cost_return_on_error!(&mut cost, self.add_root_leaf(key, transaction));
                } else {
                    cost_return_on_error!(
                        &mut cost,
                        self.add_non_root_subtree(path_iter.clone(), key, element, transaction)
                    );
                    cost_return_on_error!(
                        &mut cost,
                        self.propagate_changes(path_iter, transaction)
                    );
                }
            }
            Element::Reference(ref reference_path, _) => {
                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ))
                    .wrap_with_cost(cost);
                }

                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_invalid_path(path_iter.clone(), transaction)
                );
                let referenced_element = cost_return_on_error!(
                    &mut cost,
                    self.follow_reference(reference_path.to_owned(), transaction)
                );

                merk_optional_tx!(
                    &mut cost,
                    self.db,
                    path_iter.clone(),
                    transaction,
                    mut subtree,
                    {
                        let serialized =
                            cost_return_on_error_no_add!(&cost, referenced_element.serialize());
                        cost_return_on_error!(
                            &mut cost,
                            element.insert_reference(&mut subtree, key, serialized)
                        );
                    }
                );
                cost_return_on_error!(&mut cost, self.propagate_changes(path_iter, transaction));
            }
            _ => {
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
                    self.check_subtree_exists_invalid_path(path_iter.clone(), transaction)
                );
                merk_optional_tx!(
                    &mut cost,
                    self.db,
                    path_iter.clone(),
                    transaction,
                    mut subtree,
                    {
                        cost_return_on_error!(&mut cost, element.insert(&mut subtree, key));
                    }
                );
                cost_return_on_error!(&mut cost, self.propagate_changes(path_iter, transaction));
            }
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Add subtree to the root tree
    fn add_root_leaf(
        &self,
        key: &[u8],
        transaction: TransactionArg,
    ) -> CostContext<Result<(), Error>> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, transaction, meta_storage, {
            let mut root_leaf_keys =
                cost_return_on_error!(&mut cost, Self::get_root_leaf_keys_internal(&meta_storage));
            if root_leaf_keys.get(&key.to_vec()).is_none() {
                root_leaf_keys.insert(key.to_vec(), root_leaf_keys.len());
            }
            let value = cost_return_on_error_no_add!(
                &cost,
                bincode::serialize(&root_leaf_keys).map_err(|_| {
                    Error::CorruptedData(String::from("unable to serialize root leaves data"))
                })
            );

            cost_return_on_error_no_add!(
                &cost,
                meta_storage
                    .put_meta(ROOT_LEAFS_SERIALIZED_KEY, &value)
                    .map_err(|e| e.into())
            );
            cost.storage_written_bytes += ROOT_LEAFS_SERIALIZED_KEY.len() + value.len();
        });

        // Persist root leaf as a regular subtree
        storage_context_optional_tx!(self.db, [key], transaction, storage, {
            cost_return_on_error_no_add!(
                &cost,
                storage.put_root(ROOT_KEY_KEY, key).map_err(|e| e.into())
            );
            cost.storage_written_bytes += ROOT_KEY_KEY.len() + key.len()
        });

        Ok(()).wrap_with_cost(cost)
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_non_root_subtree<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> CostContext<Result<(), Error>>
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
        cost_return_on_error!(
            &mut cost,
            self.check_subtree_exists_invalid_path(path_iter.clone(), transaction)
        );

        if let Some(tx) = transaction {
            let parent_storage = self
                .db
                .get_transactional_storage_context(path_iter.clone(), tx);
            let mut parent_subtree = cost_return_on_error!(
                &mut cost,
                Merk::open(parent_storage)
                    .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))
            );
            let child_storage = self.db.get_transactional_storage_context(
                path_iter.clone().chain(std::iter::once(key)),
                tx,
            );
            let child_subtree = cost_return_on_error!(
                &mut cost,
                Merk::open(child_storage)
                    .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))
            );
            let element = Element::new_tree_with_flags(
                child_subtree.root_hash().unwrap_add_cost(&mut cost),
                element_flag,
            );
            cost_return_on_error!(&mut cost, element.insert(&mut parent_subtree, key));
        } else {
            let parent_storage = self.db.get_storage_context(path_iter.clone());
            let mut parent_subtree = cost_return_on_error!(
                &mut cost,
                Merk::open(parent_storage)
                    .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))
            );
            let child_storage = self
                .db
                .get_storage_context(path_iter.clone().chain(std::iter::once(key)));
            let child_subtree = cost_return_on_error!(
                &mut cost,
                Merk::open(child_storage)
                    .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))
            );
            let element = Element::new_tree_with_flags(
                child_subtree.root_hash().unwrap_add_cost(&mut cost),
                element_flag,
            );
            cost_return_on_error!(&mut cost, element.insert(&mut parent_subtree, key));
        }
        Ok(()).wrap_with_cost(cost)
    }

    pub fn insert_if_not_exists<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> CostContext<Result<bool, Error>>
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
