//! Automatic deletion with cached old-value observation on GROVE_V4.

use crate::operations::indexed_tree::reject_generic_write_into_indexed_primary;
use crate::BackwardReferencesPolicy;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, storage_cost::removal::StorageRemovedBytes,
    CostResult, CostsExt,
};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
use grovedb_merk::{
    element::{costs::ElementCostExtensions, delete::ElementDeleteFromStorageExtensions},
    Error as MerkError,
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use super::DeleteOptions;
use crate::{
    bidirectional_references,
    merk_cache::MerkCache,
    util::visitor::{GroveVisitor, Visit, WalkResult},
    Element, Error, GroveDb, Transaction,
};

impl GroveDb {
    /// Automatic V4 delete dispatcher.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn delete_internal_on_transaction_v2<B: AsRef<[u8]>>(
        &self,
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
            MerkError,
        >,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        if options.backward_references_policy.maintains() {
            self.delete_with_backward_references_v2(
                path,
                key,
                options,
                transaction,
                sectioned_removal,
                batch,
                grove_version,
            )
        } else {
            self.delete_internal_on_transaction_v1(
                path,
                key,
                options,
                transaction,
                sectioned_removal,
                batch,
                grove_version,
            )
        }
    }

    /// The `MerkCache`-based delete flow with backward-references cascade.
    #[allow(clippy::too_many_arguments)]
    fn delete_with_backward_references_v2<B: AsRef<[u8]>>(
        &self,
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
            MerkError,
        >,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        let mut cost = Default::default();

        let merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        let subtree_to_delete_from_type = merk.tree_type;
        cost_return_on_error_no_add!(
            cost,
            reject_generic_write_into_indexed_primary(merk.tree_type, "delete")
        );
        let mut observed = None;
        cost_return_on_error!(
            &mut cost,
            merk.observe_old_value(
                key,
                Some(&Element::value_defined_cost_for_serialized_value),
                &mut |_, bytes| {
                    observed = Some(Element::deserialize(bytes, grove_version));
                },
                grove_version,
            )
            .map_err(Error::MerkError)
        );
        let element =
            cost_return_on_error_no_add!(cost, observed.transpose().map_err(Error::from)
            .and_then(|value| value.ok_or_else(|| Error::PathKeyNotFound(hex::encode(key)))));
        let descendants_need_maintenance = if element.is_any_tree()
            && !element.uses_non_merk_data_storage()
            && element
                .root_key_and_tree_type()
                .is_some_and(|(root, _)| root.is_some())
        {
            !cost_return_on_error!(
                &mut cost,
                self.backward_reference_participants(
                    path.derive_owned_with_child(key).to_vec().as_slice(),
                    transaction,
                    grove_version,
                )
            )
            .is_empty()
        } else {
            false
        };
        if !element.supports_backward_references() && !descendants_need_maintenance {
            return self
                .delete_prepared_ordinary_v1(
                    element,
                    merk,
                    path,
                    key,
                    options,
                    transaction,
                    sectioned_removal,
                    batch,
                    grove_version,
                )
                .add_cost(cost);
        }
        let cache = MerkCache::with_prepared_merk(
            self,
            transaction,
            grove_version,
            path.derive_owned(),
            merk,
            batch,
        );
        let mut subtree_to_delete_from =
            cost_return_on_error!(&mut cost, cache.get_merk(path.derive_owned()));

        if element.is_any_tree() {
            // A subtree deletion was requested.

            // The visitor-based sweep below iterates Merk elements; the
            // specialized data trees don't store their contents as Merk
            // elements, and clearing an indexed primary would strand its
            // secondary Merks. None of their contents can be targeted by
            // bidirectional references — delete them without the flag.
            if element.underlying().uses_non_merk_data_storage()
                || element.underlying().is_indexed_tree()
            {
                return Err(Error::NotSupported(
                    "specialized data trees and indexed trees cannot be deleted with \
                     automatic reference maintenance; remove specialized descendants first"
                        .to_owned(),
                ))
                .wrap_with_cost(cost);
            }

            let merk_to_delete_path = path.derive_owned_with_child(key);
            let mut merk_to_delete =
                cost_return_on_error!(&mut cost, cache.get_merk(merk_to_delete_path.clone()));
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
                // Perform recursive deletion of everything below the element
                // we're deleting. During traversal bidirectional references
                // are also cleaned up with all required procedures, altering
                // the cache state. The rest of the deletion is done outside
                // of the cache and is accumulated into a different batch
                // that is merged in afterwards.
                let visitor = GroveVisitor::new(
                    &self.db,
                    transaction,
                    DeletionVisitor::new(
                        &cache,
                        options.backward_references_policy,
                        true,
                        sectioned_removal,
                    ),
                    true,
                    grove_version,
                );

                let WalkResult {
                    batch: deletion_batch,
                    ..
                } = cost_return_on_error!(
                    &mut cost,
                    visitor.walk_from(merk_to_delete_path.clone())
                );

                Some(deletion_batch)
            } else {
                None
            };

            // The tree element deletion itself:
            cost_return_on_error!(
                &mut cost,
                subtree_to_delete_from.for_merk(|m| {
                    Element::delete_with_sectioned_removal_bytes(
                        m,
                        key,
                        Some(options.as_merk_options()),
                        true,
                        subtree_to_delete_from_type,
                        sectioned_removal,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                })
            );
            // And marking the subtree as deleted in the cache:
            cache.mark_deleted(merk_to_delete_path);

            // Processing the given batch:
            // 1. add deferred operations from the cache, such as reference and
            //    regular propagations, ensuring that the "root" of this
            //    deletion operation is removed beforehand,
            // 2. append the batch of recursive deletions. Since the previous
            //    operations (from the cache) have already removed all
            //    connections to this data, no special handling is needed —
            //    just cleanup.
            cost_return_on_error!(&mut cost, cache.finish_prepared());
            deletion_batch
                .into_iter()
                .for_each(|b| batch.merge_overwriting(b));
            Ok(true).wrap_with_cost(cost)
        } else {
            // A non-tree element deletion was requested. The removed element
            // must be loaded for possible references propagation:
            let old = cost_return_on_error!(
                &mut cost,
                subtree_to_delete_from.for_merk(|m| {
                    let mut inner_cost = Default::default();

                    cost_return_on_error!(
                        &mut inner_cost,
                        Element::delete_with_sectioned_removal_bytes(
                            m,
                            key,
                            Some(options.as_merk_options()),
                            false,
                            subtree_to_delete_from_type,
                            sectioned_removal,
                            grove_version,
                        )
                        .map_err(Error::MerkError)
                    );

                    Ok(Some(element)).wrap_with_cost(inner_cost)
                })
            );

            cost_return_on_error!(
                &mut cost,
                bidirectional_references::process_update_element_with_backward_references(
                    &cache,
                    subtree_to_delete_from,
                    path.derive_owned(),
                    key,
                    grovedb_merk::element::insert::Delta { new: None, old },
                    sectioned_removal,
                )
            );

            // Fill the provided batch with what we ended up with after
            // deletion using the cache:
            cost_return_on_error!(&mut cost, cache.finish_prepared());
            Ok(true).wrap_with_cost(cost)
        }
    }
}

/// We perform recursive deletions by traversing GroveDB.
/// For performance reasons the visitor uses raw iterators and doesn't build
/// Merks, and at first glance it doesn't play well with the caching we have
/// to use for bidirectional references. However, since we're in control of
/// when and how we do modifications inside of the deletion implementation,
/// we're good as long as we do nothing outside of the cache, then finalize
/// it, and only then merge with the final deletion batches.
struct DeletionVisitor<'c, 'db, 'b, 's, B: AsRef<[u8]>> {
    backward_references_policy: BackwardReferencesPolicy,
    allow_deleting_subtrees: bool,
    cache: &'c MerkCache<'db, 'b, B>,
    /// The caller's removal-accounting policy, applied to every referrer a
    /// cascade deletes on the way.
    sectioned_removal: bidirectional_references::SectionedRemovalFn<'s>,
}

impl<'c, 'db, 'b, 's, B: AsRef<[u8]>> DeletionVisitor<'c, 'db, 'b, 's, B> {
    fn new(
        cache: &'c MerkCache<'db, 'b, B>,
        backward_references_policy: BackwardReferencesPolicy,
        allow_deleting_subtrees: bool,
        sectioned_removal: bidirectional_references::SectionedRemovalFn<'s>,
    ) -> Self {
        Self {
            backward_references_policy,
            allow_deleting_subtrees,
            cache,
            sectioned_removal,
        }
    }
}

impl<'b, B: AsRef<[u8]>> Visit<'b, B> for DeletionVisitor<'_, '_, 'b, '_, B> {
    fn visit_merk(&mut self, _path: SubtreePathBuilder<'b, B>) -> CostResult<bool, Error> {
        Ok(false).wrap_with_cost(Default::default())
    }

    fn visit_element(
        &mut self,
        path: SubtreePathBuilder<'b, B>,
        key: &[u8],
        storage: &PrefixedRocksDbTransactionContext,
        element: Element,
    ) -> CostResult<bool, Error> {
        // The process involves two main tasks during traversal: cleaning up
        // elements and optionally propagating backward references, possibly
        // outside the deletion area. To achieve this efficiently within a
        // single traversal, we use both a cache and an internal batch for
        // traversal. These can then be merged in the correct order
        // afterwards.
        let mut cost = Default::default();

        // Step 1: Delete visited element; the deletion is deferred and stays
        // inside of the batch that will be returned after traversal:
        if element.is_any_tree() && !self.allow_deleting_subtrees {
            // If we're not allowing subtrees deletion, then quick way out
            // with a report.
            return Ok(true).wrap_with_cost(cost);
        } else {
            // The same fail-closed rule the directly selected element gets:
            // a specialized data tree's contents are not Merk elements (the
            // recursive sweep cannot even decode them), and clearing an
            // indexed primary here would strand its secondary namespaces.
            // Refuse the whole flagged deletion; the caller deletes those
            // subtrees without the flag first.
            if element.underlying().uses_non_merk_data_storage()
                || element.underlying().is_indexed_tree()
            {
                return Err(Error::NotSupported(
                    "a descendant specialized data tree or indexed tree blocks deletion with \
                     automatic reference maintenance; remove it separately first"
                        .to_owned(),
                ))
                .wrap_with_cost(cost);
            }
            cost_return_on_error!(&mut cost, storage.delete(key, None).map_err(Into::into));
        }

        // Step 2: perform backward references' deletion on top of cached
        // data:
        if self.backward_references_policy.maintains()
            && matches!(
                element,
                Element::ItemWithBackwardsReferences(..)
                    | Element::SumItemWithBackwardsReferences(..)
                    | Element::ItemWithSumItemWithBackwardsReferences(..)
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
                    grovedb_merk::element::insert::Delta {
                        new: None,
                        old: Some(element)
                    },
                    &mut *self.sectioned_removal,
                )
            );
        }

        Ok(false).wrap_with_cost(cost)
    }
}
