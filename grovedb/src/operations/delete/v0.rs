use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt,
};
use grovedb_merk::Merk;
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch};
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use super::DeleteOptions;
use crate::{Element, Error, GroveDb, Transaction};

pub(super) fn delete_internal_on_transaction<B: AsRef<[u8]>>(
    db: &GroveDb,
    path: SubtreePath<B>,
    key: &[u8],
    options: &DeleteOptions,
    transaction: &Transaction,
    sectioned_removal: &mut impl FnMut(
        &Vec<u8>,
        u32,
        u32,
    ) -> Result<
        (StorageRemovedBytes, StorageRemovedBytes),
        grovedb_merk::Error,
    >,
    batch: &StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<bool, Error> {
    check_grovedb_v0_with_cost!(
        "delete_internal_on_transaction",
        grove_version
            .grovedb_versions
            .operations
            .delete
            .delete_internal_on_transaction
    );

    let mut cost = Default::default();

    let element = cost_return_on_error!(
        &mut cost,
        db.get_raw(path.clone(), key.as_ref(), Some(transaction), grove_version)
    );
    let mut subtree_to_delete_from = cost_return_on_error!(
        &mut cost,
        db.open_transactional_merk_at_path(path.clone(), transaction, Some(batch), grove_version)
    );
    let uses_sum_tree = subtree_to_delete_from.tree_type;
    if let Some(tree_type) = element.tree_type() {
        let subtree_merk_path = path.derive_owned_with_child(key);
        let subtree_merk_path_ref = SubtreePath::from(&subtree_merk_path);

        let subtree_of_tree_we_are_deleting = cost_return_on_error!(
            &mut cost,
            db.open_transactional_merk_at_path(
                subtree_merk_path_ref.clone(),
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let is_empty = subtree_of_tree_we_are_deleting
            .is_empty_tree()
            .unwrap_add_cost(&mut cost);

        if !options.allow_deleting_non_empty_trees && !is_empty {
            return if options.deleting_non_empty_trees_returns_error {
                Err(Error::DeletingNonEmptyTree(
                    "trying to do a delete operation for a non empty tree, but options not \
                     allowing this",
                ))
                .wrap_with_cost(cost)
            } else {
                Ok(false).wrap_with_cost(cost)
            };
        } else if !is_empty {
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                db.find_subtrees(&subtree_merk_path_ref, Some(transaction), grove_version)
            );
            for subtree_path in subtrees_paths {
                let p: SubtreePath<_> = subtree_path.as_slice().into();
                let mut storage = db
                    .db
                    .get_transactional_storage_context(p, Some(batch), transaction)
                    .unwrap_add_cost(&mut cost);

                cost_return_on_error!(
                    &mut cost,
                    storage.clear().map_err(|e| {
                        Error::CorruptedData(format!("unable to cleanup tree from storage: {e}",))
                    })
                );
            }
            // todo: verify why we need to open the same? merk again
            let storage = db
                .db
                .get_transactional_storage_context(path.clone(), Some(batch), transaction)
                .unwrap_add_cost(&mut cost);

            let mut merk_to_delete_tree_from = cost_return_on_error!(
                &mut cost,
                Merk::open_layered_with_root_key(
                    storage,
                    subtree_to_delete_from.root_key(),
                    tree_type,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|_| {
                    Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                })
            );
            // We are deleting a tree, a tree uses 3 bytes
            cost_return_on_error!(
                &mut cost,
                Element::delete_with_sectioned_removal_bytes(
                    &mut merk_to_delete_tree_from,
                    key,
                    Some(options.as_merk_options()),
                    true,
                    uses_sum_tree,
                    sectioned_removal,
                    grove_version,
                )
            );
            let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
                HashMap::default();
            merk_cache.insert(path.clone(), merk_to_delete_tree_from);
            cost_return_on_error!(
                &mut cost,
                db.propagate_changes_with_transaction(
                    merk_cache,
                    path,
                    transaction,
                    batch,
                    grove_version,
                )
            );
        } else {
            // We are deleting a tree, a tree uses 3 bytes
            cost_return_on_error!(
                &mut cost,
                Element::delete_with_sectioned_removal_bytes(
                    &mut subtree_to_delete_from,
                    key,
                    Some(options.as_merk_options()),
                    true,
                    uses_sum_tree,
                    sectioned_removal,
                    grove_version,
                )
            );
            let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
                HashMap::default();
            merk_cache.insert(path.clone(), subtree_to_delete_from);
            cost_return_on_error!(
                &mut cost,
                db.propagate_changes_with_transaction(
                    merk_cache,
                    path,
                    transaction,
                    batch,
                    grove_version
                )
            );
        }
    } else {
        cost_return_on_error!(
            &mut cost,
            Element::delete_with_sectioned_removal_bytes(
                &mut subtree_to_delete_from,
                key,
                Some(options.as_merk_options()),
                false,
                uses_sum_tree,
                sectioned_removal,
                grove_version,
            )
        );
        let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();
        merk_cache.insert(path.clone(), subtree_to_delete_from);
        cost_return_on_error!(
            &mut cost,
            db.propagate_changes_with_transaction(
                merk_cache,
                path,
                transaction,
                batch,
                grove_version
            )
        );
    }

    Ok(true).wrap_with_cost(cost)
}
