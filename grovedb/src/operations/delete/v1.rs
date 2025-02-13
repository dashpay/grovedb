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
            // Perform recursive deletion of everything below the element we're deleting.
            // During traversal bidirectional references are also cleaned up with all
            // required procedures if the flag is set, altering the cache state. However,
            // the rest of the deletion is done outside of the cache and is accumulated
            // into a different batch that is returned by the end of this block
            let visitor = util::GroveVisitor::new(
                &db.db,
                transaction,
                DeletionVisitor::new(&cache, options.propagate_backward_references, true),
                true,
                grove_version,
            );

            let WalkResult {
                batch: deletion_batch,
                ..
            } = cost_return_on_error!(&mut cost, visitor.walk_from(merk_to_delete_path.clone()));

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
        // And marking the subtree as deleted in the cache:
        cache.mark_deleted(merk_to_delete_path);

        // Processing the given batch:
        // 1. add deferred operations from the cache, such as reference and regular
        //    propagations, ensuring that the "root" of this deletion operation is
        //    removed beforehand,
        // 2. append the batch of recursive deletions. Since the previous operations
        //    (from the cache) have already removed all connections to this data, no
        //    special handling is needed -- just cleanup.

        batch.merge_overwriting(*cost_return_on_error!(&mut cost, cache.into_batch()));
        deletion_batch
            .into_iter()
            .for_each(|b| batch.merge_overwriting(b));
        Ok(true).wrap_with_cost(cost)
    } else {
        // An non-tree element deletion was requested:
        if options.propagate_backward_references {
            // With backward references propagation flag set, a removed element must be
            // loaded for possible references propagation:
            let old = cost_return_on_error!(
                &mut cost,
                subtree_to_delete_from.for_merk(|m| {
                    let mut inner_cost = Default::default();

                    let old = cost_return_on_error!(
                        &mut inner_cost,
                        Element::get_optional(m, key, true, grove_version)
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
                    Delta { new: None, old },
                )
            );
        } else {
            // The user decided not to pay for handling bidirectional references. So, we're
            // just removing it with no extra steps.
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
        }

        // Fill the provied batch with what we ended up with after deletion using cache:
        batch.merge_overwriting(*cost_return_on_error!(&mut cost, cache.into_batch()));
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
                    element.insert_subtree(m, parent_key, Default::default(), None, grove_version)
                })
            })
        );
    }

    let batch = cost_return_on_error!(&mut cost, cache.into_batch());
    batch.merge_overwriting(deletion_batch);

    cost_return_on_error!(
        &mut cost,
        db.db
            .commit_multi_context_batch(*batch, Some(transaction))
            .map_err(Into::into)
    );

    Ok(true).wrap_with_cost(cost)
}

#[cfg(test)]
mod tests {
    use grovedb_costs::{storage_cost::StorageCost, OperationCost};
    use grovedb_version::version::v2::GROVE_V2;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::tests::{common::EMPTY_PATH, make_empty_grovedb};

    #[test]
    fn test_delete_one_sum_item_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to insert");

        let insertion_cost = db
            .insert(
                [b"sum_tree".as_slice()].as_ref(),
                b"key1",
                Element::new_sum_item(15000),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        let cost = db
            .delete(
                [b"sum_tree".as_slice()].as_ref(),
                b"key1",
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to delete");

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            cost.storage_cost.removed_bytes.total_removed_bytes()
        );

        assert_eq!(
            db.root_hash(Some(&tx), grove_version).unwrap().unwrap(),
            [
                140, 110, 203, 30, 191, 33, 89, 2, 187, 18, 14, 63, 161, 217, 202, 46, 122, 109,
                83, 75, 231, 212, 120, 176, 57, 153, 88, 81, 179, 93, 225, 11
            ]
        );

        // Explanation for 171 storage removed bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the flag option (but no flags)
        //   1 for the enum type sum item
        //   9 for the sum item
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for the feature type
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Summed Merk 9

        // Total 37 + 85 + 48 = 170

        // Hash node calls
        // everything is empty, so no need for hashes?
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 91,
                    removed_bytes: StorageRemovedBytes::BasicStorageRemoval(170)
                },
                storage_loaded_bytes: 252, // todo: verify this
                hash_node_calls: 4,
            }
        );
    }

    #[test]
    fn test_delete_one_item_in_sum_tree_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to insert");

        let insertion_cost = db
            .insert(
                [b"sum_tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"hello".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        let cost = db
            .delete(
                [b"sum_tree".as_slice()].as_ref(),
                b"key1",
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to delete");

        assert_eq!(
            db.root_hash(Some(&tx), grove_version).unwrap().unwrap(),
            [
                140, 110, 203, 30, 191, 33, 89, 2, 187, 18, 14, 63, 161, 217, 202, 46, 122, 109,
                83, 75, 231, 212, 120, 176, 57, 153, 88, 81, 179, 93, 225, 11
            ]
        );

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            cost.storage_cost.removed_bytes.total_removed_bytes()
        );
        // Explanation for 171 storage removed bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 82
        //   1 for the flag option (but no flags)
        //   1 for the enum type sum item
        //   5 for the item
        //   1 for the item len
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for the feature type
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Summed Merk 9

        // Total 37 + 82 + 48 = 167

        // Hash node calls
        // everything is empty, so no need for hashes?
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 91,
                    removed_bytes: StorageRemovedBytes::BasicStorageRemoval(167)
                },
                storage_loaded_bytes: 251, // todo: verify this
                hash_node_calls: 4,
            }
        );
    }

    #[test]
    fn test_delete_one_item_cost() {
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        let cost = db
            .delete(EMPTY_PATH, b"key1", None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to delete");

        assert_eq!(
            db.root_hash(Some(&tx), grove_version).unwrap().unwrap(),
            [0; 32]
        );

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            cost.storage_cost.removed_bytes.total_removed_bytes()
        );
        // Explanation for 147 storage removed bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for Basic Merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 72 + 40 = 149

        // Hash node calls
        // everything is empty, so no need for hashes?
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 4, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 0,
                    removed_bytes: StorageRemovedBytes::BasicStorageRemoval(149)
                },
                storage_loaded_bytes: 77, // todo: verify this
                hash_node_calls: 0,
            }
        );
    }
}
