use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::Merk;
use storage::Storage;

use crate::{
    reference_path::path_from_reference_path_type,
    util::{merk_optional_tx, storage_context_optional_tx},
    Element, Error, GroveDb, TransactionArg,
};

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
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        match element {
            Element::Tree(..) => {
                cost_return_on_error!(
                    &mut cost,
                    self.add_subtree(path_iter.clone(), key, element, transaction)
                );
                cost_return_on_error!(&mut cost, self.propagate_changes(path_iter, transaction));
            }
            Element::Reference(ref reference_path, ..) => {
                let reference_path =
                    path_from_reference_path_type(reference_path.clone(), path_iter.clone());
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

                let (referenced_key, referenced_path) = reference_path.split_last().unwrap();
                let referenced_path_iter = referenced_path.iter().map(|x| x.as_slice());
                let referenced_element_value_hash_opt = merk_optional_tx!(
                    &mut cost,
                    self.db,
                    referenced_path_iter,
                    transaction,
                    subtree,
                    {
                        Element::get_value_hash(&subtree, referenced_key)
                            .unwrap_add_cost(&mut cost)
                            .unwrap()
                    }
                );
                let referenced_element_value_hash = cost_return_on_error!(
                    &mut cost,
                    referenced_element_value_hash_opt
                        .ok_or(Error::MissingReference("cannot find referenced value"))
                        .wrap_with_cost(OperationCost::default())
                );

                merk_optional_tx!(
                    &mut cost,
                    self.db,
                    path_iter.clone(),
                    transaction,
                    mut subtree,
                    {
                        cost_return_on_error!(
                            &mut cost,
                            element.insert_reference(
                                &mut subtree,
                                key,
                                referenced_element_value_hash
                            )
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

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_subtree<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
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
                .get_transactional_storage_context(path_iter.clone(), tx)
                .unwrap_add_cost(&mut cost);
            let mut parent_subtree = cost_return_on_error!(
                &mut cost,
                Merk::open(parent_storage)
                    .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))
            );
            let child_storage = self
                .db
                .get_transactional_storage_context(path_iter.chain(std::iter::once(key)), tx)
                .unwrap_add_cost(&mut cost);
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
            let parent_storage = self
                .db
                .get_storage_context(path_iter.clone())
                .unwrap_add_cost(&mut cost);
            let mut parent_subtree = cost_return_on_error!(
                &mut cost,
                Merk::open(parent_storage)
                    .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))
            );
            let child_storage = self
                .db
                .get_storage_context(path_iter.chain(std::iter::once(key)))
                .unwrap_add_cost(&mut cost);
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
