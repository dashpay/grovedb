use grovedb_costs::{
    cost_return_on_error, storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt,
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use grovedb_version::{dispatch_version, version::GroveVersion};

use super::{ClearOptions, DeleteOptions};
use crate::{
    bidirectional_references,
    element::Delta,
    merk_cache::MerkCache,
    util::{self, WalkResult},
    Element, Error, GroveDb, Transaction,
};

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
    dispatch_version!(
        "delete_internal_on_transaction",
        grove_version
            .grovedb_versions
            .operations
            .delete
            .delete_internal_on_transaction,
        1 => {}
    );

    let mut cost = Default::default();

    let cache = MerkCache::<B>::new(db, transaction, grove_version);

    let mut subtree_to_delete_from =
        cost_return_on_error!(&mut cost, cache.get_merk(path.derive_owned()));

    let element = cost_return_on_error!(
        &mut cost,
        subtree_to_delete_from.for_merk(|m| Element::get(m, key, true, grove_version))
    );

    let subtree_to_delete_from_type = cost_return_on_error!(
        &mut cost,
        subtree_to_delete_from.for_merk(|m| m.tree_type.wrap_cost_ok())
    );

    if element.is_any_tree() {
        // A subtree deletion was requested:

        let mut merk_to_delete =
            cost_return_on_error!(&mut cost, cache.get_merk(path.derive_owned_with_child(key)));
        let is_empty = cost_return_on_error!(
            &mut cost,
            merk_to_delete.for_merk(|m| m.is_empty_tree().map(Ok))
        );

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
        }

        let deletion_batch = if !is_empty {
            // Perform recursive deletion of everything below the element we're deleting
            let visitor = util::GroveVisitor::new(
                &db.db,
                transaction,
                DeletionVisitor::new(&cache, options.propagate_backward_references, true),
                true,
                grove_version,
            );

            let subtree_merk_path = path.derive_owned_with_child(key);
            let WalkResult {
                batch: deletion_batch,
                ..
            } = cost_return_on_error!(&mut cost, visitor.walk_from(subtree_merk_path));

            Some(deletion_batch)
        } else {
            None
        };

        // A tree element deletion:
        cost_return_on_error!(
            &mut cost,
            subtree_to_delete_from.for_merk(|m| Element::delete_with_sectioned_removal_bytes(
                m,
                key,
                Some(options.as_merk_options()),
                true,
                subtree_to_delete_from_type,
                sectioned_removal,
                grove_version,
            ))
        );

        // Processing the given batch:
        // 1. add deferred operations from the cache, such as reference and regular
        //    propagations, ensuring that the "root" of this deletion operation is
        //    removed beforehand,
        // 2. append the batch of recursive deletions. Since the previous operations
        //    (from the cache) have already removed all connections to this data, no
        //    special handling is needed -- just cleanup.

        batch.merge(*cost_return_on_error!(&mut cost, cache.into_batch()));
        deletion_batch.into_iter().for_each(|b| batch.merge(b));
        Ok(true).wrap_with_cost(cost)
    } else {
        // An non-tree element deletion was requested:
        cost_return_on_error!(
            &mut cost,
            subtree_to_delete_from.for_merk(|m| Element::delete_with_sectioned_removal_bytes(
                m,
                key,
                Some(options.as_merk_options()),
                false,
                subtree_to_delete_from_type,
                sectioned_removal,
                grove_version,
            ))
        );

        // Fill the provied batch with what we ended up with after deletion using cache:
        batch.merge(*cost_return_on_error!(&mut cost, cache.into_batch()));
        Ok(true).wrap_with_cost(cost)
    }
}

/// We perform recursive deletions by traversing GroveDB.
/// For performance reasons the visitor uses raw iterators and doesn't build
/// Merks, and at the first glance it doesn't play well with caching we have to
/// use for bidirectional references. However, since we're in control of when
/// and how we do modifications inside of deletion implementation, we're good as
/// long as we do nothing outside of the cache, then finalize it, and only then
/// merging with final deletions batches.
struct DeletionVisitor<'c, 'db, 'b, B: AsRef<[u8]>> {
    propagate_backward_references: bool,
    allow_deleting_subtrees: bool,
    cache: &'c MerkCache<'db, 'b, B>,
}

impl<'c, 'db, 'b, B: AsRef<[u8]>> DeletionVisitor<'c, 'db, 'b, B> {
    fn new(
        cache: &'c MerkCache<'db, 'b, B>,
        propagate_backward_references: bool,
        allow_deleting_subtrees: bool,
    ) -> Self {
        Self {
            propagate_backward_references,
            allow_deleting_subtrees,
            cache,
        }
    }
}

impl<'c, 'db, 'b, B: AsRef<[u8]>> util::Visit<'b, B> for DeletionVisitor<'c, 'db, 'b, B> {
    fn visit_merk(&mut self, _path: SubtreePathBuilder<'b, B>) -> CostResult<bool, Error> {
        false.wrap_cost_ok()
    }

    fn visit_element(
        &mut self,
        path: SubtreePathBuilder<'b, B>,
        key: &[u8],
        storage: &PrefixedRocksDbTransactionContext,
        element: Element,
    ) -> CostResult<bool, Error> {
        // The process involves two main tasks during traversal: cleaning up elements
        // and optionally propagating backward references, possibly outside the
        // deletion area. To achieve this efficiently within a single traversal,
        // we use both a cache and an internal batch for traversal. These can
        // then be merged in the correct order afterwards.
        let mut cost = Default::default();

        // Step 1: Delete visited element, "deletion" is deferred and stays inside of
        // batch that will be returned after traversal:
        if element.is_any_tree() && !self.allow_deleting_subtrees {
            // If we're not allowing subtrees deletion, then quick way out with a report
            return Ok(true).wrap_with_cost(cost);
        } else {
            cost_return_on_error!(&mut cost, storage.delete(key, None).map_err(Into::into));
        }

        // Step 2: perform backward references' deletion on top of cached data:
        if self.propagate_backward_references
            && matches!(
                element,
                Element::ItemWithBackwardsReferences(..)
                    | Element::SumItemWithBackwardsReferences(..)
                    | Element::BidirectionalReference(..)
            )
        {
            let cached_subtree =
                cost_return_on_error!(&mut cost, self.cache.get_merk(path.clone()));
            cost_return_on_error!(
                &mut cost,
                bidirectional_references::process_update_element_with_backward_references(
                    self.cache,
                    cached_subtree,
                    path,
                    key,
                    Delta {
                        new: None,
                        old: Some(element)
                    }
                )
            );
        }

        Ok(false).wrap_with_cost(cost)
    }
}

/// Delete all elements in a specified subtree and get back costs
/// Warning: The costs for this operation are not yet correct, hence we
/// should keep this private for now
/// Returns true if we successfully cleared the subtree
pub(super) fn clear_subtree_with_costs<'b, B>(
    db: &GroveDb,
    path: SubtreePathBuilder<'b, B>,
    options: Option<ClearOptions>,
    transaction: &Transaction,
    grove_version: &GroveVersion,
) -> CostResult<bool, Error>
where
    B: AsRef<[u8]> + 'b,
{
    dispatch_version!(
        "clear_subtree",
        grove_version
            .grovedb_versions
            .operations
            .delete
            .clear_subtree,
        1 => {}
    );

    let mut cost = Default::default();

    let options = options.unwrap_or_default();

    let cache = MerkCache::<B>::new(db, transaction, grove_version);

    let deletion_batch = if !options.check_for_subtrees {
        // If we're not concerned about subtrees, then we can perform shallow
        // cleanup on top of an Element deletion, how regular `delete` would do:
        let visitor = util::GroveVisitor::new(
            &db.db,
            transaction,
            DeletionVisitor::new(
                &cache,
                options.propagate_backward_references,
                true, // allowing subtrees deletion because we have "don't care" flag
            ),
            false, // do not recurse
            grove_version,
        );

        let WalkResult {
            batch: deletion_batch,
            ..
        } = cost_return_on_error!(&mut cost, visitor.walk_from(path.clone()));
        deletion_batch
    } else {
        // This time we don't ignore subtrees existence
        let visitor = util::GroveVisitor::new(
            &db.db,
            transaction,
            DeletionVisitor::new(
                &cache,
                options.propagate_backward_references,
                options.allow_deleting_subtrees,
            ),
            true,
            grove_version,
        );

        let WalkResult {
            batch: deletion_batch,
            short_circuited,
            ..
        } = cost_return_on_error!(&mut cost, visitor.walk_from(path.clone()));

        if short_circuited {
            // Deletion visitor will short circuit if it hits a subtree, but we're not
            // allowing deletion of those, based on another flag we decide what to do:
            return if options.trying_to_clear_with_subtrees_returns_error {
                Err(Error::ClearingTreeWithSubtreesNotAllowed(
                    "options do not allow to clear this merk tree as it contains subtrees",
                ))
            } else {
                Ok(false)
            }
            .wrap_with_cost(cost);
        }

        deletion_batch
    };

    // Last step before assembling the batch is to mark the subtree as empty by
    // clearing the root key info on a parent element:
    if let Some((parent_path, parent_key)) = path.derive_parent_owned() {
        let mut parent = cost_return_on_error!(&mut cost, cache.get_merk(parent_path));
        cost_return_on_error!(
            &mut cost,
            parent.for_merk(|m| {
                Element::get(m, &parent_key, true, grove_version).flat_map_ok(|mut element| {
                    element.set_root_key(None);
                    element.insert(m, parent_key, None, grove_version)
                })
            })
        );
    }

    let batch = cost_return_on_error!(&mut cost, cache.into_batch());
    batch.merge(deletion_batch);

    cost_return_on_error!(
        &mut cost,
        db.db
            .commit_multi_context_batch(*batch, Some(transaction))
            .map_err(Into::into)
    );

    Ok(true).wrap_with_cost(cost)
}
