//! `delete_internal_on_transaction` — **v2** (`GROVE_V4`+).
//!
//! A behaviour-preserving router. A call without
//! [`DeleteOptions::propagate_backward_references`] runs the exact v1 body
//! (`GROVE_V4`'s parent-reuse delete, issue #686) — identical root hashes
//! and costs. A call WITH the flag runs the `MerkCache`-based flow below,
//! which fetches the deleted element, cascades backward-reference chains
//! (each affected bidirectional reference must allow `cascade_on_update`),
//! and for subtree deletion sweeps the subtree with a raw-iterator visitor
//! while cleaning up backward references along the way. See
//! `adr/bidirectional_references.md`.
//!
//! ## Support under the backward-references flow
//!
//! The flag-on flow supports plain Merk subtrees. Specialized tree types
//! (commitment / MMR / bulk-append / dense / private document store) and
//! indexed-tree primaries are rejected with the flag set — delete them
//! without the flag (none of their contents can be targeted by
//! bidirectional references).

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, storage_cost::removal::StorageRemovedBytes,
    CostResult, CostsExt,
};
use grovedb_merk::{
    element::{delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions},
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
    /// `delete_internal_on_transaction` v2 — see the module documentation.
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
        if options.propagate_backward_references {
            self.delete_with_backward_references(
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
    fn delete_with_backward_references<B: AsRef<[u8]>>(
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

        let cache = MerkCache::<B>::new(self, transaction, grove_version);

        let mut subtree_to_delete_from =
            cost_return_on_error!(&mut cost, cache.get_merk(path.derive_owned()));

        let subtree_to_delete_from_type = cost_return_on_error!(
            &mut cost,
            subtree_to_delete_from.for_merk(|m| Ok(m.tree_type).wrap_with_cost(Default::default()))
        );

        // Guard on the CONTAINING Merk's type, before even looking the key
        // up: deleting a row out of an indexed-tree primary through this
        // generic flow would strand its mirrored secondary state. (The
        // separate check further down guards the case where the deleted
        // element is itself a specialized/indexed tree.)
        cost_return_on_error_no_add!(
            cost,
            crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
                subtree_to_delete_from_type,
                "delete with propagate_backward_references",
            )
        );

        let element = cost_return_on_error!(
            &mut cost,
            subtree_to_delete_from.for_merk(|m| {
                Element::get(m, key, true, grove_version).map_err(Error::MerkError)
            })
        );

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
                     propagate_backward_references set; delete them without the flag"
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
                    DeletionVisitor::new(&cache, options.propagate_backward_references, true),
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
            batch.merge_overwriting(*cost_return_on_error!(&mut cost, cache.into_batch()));
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

                    let old = cost_return_on_error!(
                        &mut inner_cost,
                        Element::get_optional(m, key, true, grove_version)
                            .map_err(Error::MerkError)
                    );

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

                    Ok(old).wrap_with_cost(inner_cost)
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
                )
            );

            // Fill the provided batch with what we ended up with after
            // deletion using the cache:
            batch.merge_overwriting(*cost_return_on_error!(&mut cost, cache.into_batch()));
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

impl<'b, B: AsRef<[u8]>> Visit<'b, B> for DeletionVisitor<'_, '_, 'b, B> {
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
                    "a descendant specialized data tree or indexed tree blocks deletion with                      propagate_backward_references set; delete it without the flag first"
                        .to_owned(),
                ))
                .wrap_with_cost(cost);
            }
            cost_return_on_error!(&mut cost, storage.delete(key, None).map_err(Into::into));
        }

        // Step 2: perform backward references' deletion on top of cached
        // data:
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
                    grovedb_merk::element::insert::Delta {
                        new: None,
                        old: Some(element)
                    }
                )
            );
        }

        Ok(false).wrap_with_cost(cost)
    }
}
