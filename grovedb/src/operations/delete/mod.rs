//! Delete operations and costs
//!
//! # Dangling References
//!
//! GroveDB does **not** track backward (incoming) references. When an element
//! is deleted, any existing [`Reference`](crate::Element::Reference) elements
//! that point to it become *dangling*. Attempting to follow a dangling
//! reference will return
//! [`Error::CorruptedReferencePathKeyNotFound`](crate::Error::CorruptedReferencePathKeyNotFound)
//! rather than incorrect data, so the failure mode is safe.
//!
//! Callers are responsible for ensuring that all references to an element are
//! removed before (or atomically with) the deletion of that element.

#[cfg(feature = "estimated_costs")]
mod average_case;
/// Versioned dispatch for `delete_internal_on_transaction` (the shared
/// delete path). Consensus-critical — see the module docs.
#[cfg(feature = "minimal")]
mod delete_internal_on_transaction;
#[cfg(feature = "minimal")]
mod delete_up_tree;
/// Flat-subtree drop (issue #848): O(1) removal of a populated subtree
/// declared to contain no child subtrees, with storage reclaimed outside
/// consensus via range tombstones driven from durable redo records.
#[cfg(feature = "minimal")]
pub mod flat_drop;
#[cfg(feature = "estimated_costs")]
mod worst_case;

#[cfg(feature = "minimal")]
use std::collections::{BTreeSet, HashMap};

#[cfg(feature = "minimal")]
pub use delete_up_tree::DeleteUpTreeOptions;
#[cfg(feature = "minimal")]
pub use flat_drop::PendingPrefixDropsReport;
#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::element::{
    decode::ElementDecodeExtensions, tree_type::ElementTreeTypeExtensions,
};
#[cfg(feature = "minimal")]
use grovedb_merk::{proofs::Query, KVIterator, MaybeTree};
#[cfg(feature = "minimal")]
use grovedb_merk::{Error as MerkError, Merk, MerkOptions};
use grovedb_path::SubtreePath;
#[cfg(feature = "minimal")]
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::util::{compat, TxRef};
#[cfg(feature = "minimal")]
use crate::{
    batch::{GroveOp, QualifiedGroveDbOp, SubelementsDeletionBehavior},
    Element, ElementFlags, Error, GroveDb, Transaction, TransactionArg,
};

#[cfg(feature = "minimal")]
#[derive(Clone)]
/// Clear options
pub struct ClearOptions {
    /// Check for Subtrees
    pub check_for_subtrees: bool,
    /// Allow deleting non-empty trees if we check for subtrees
    pub allow_deleting_subtrees: bool,
    /// If we check for subtrees, and we don't allow deleting and there are
    /// some, should we error?
    pub trying_to_clear_with_subtrees_returns_error: bool,
}

#[cfg(feature = "minimal")]
impl Default for ClearOptions {
    fn default() -> Self {
        ClearOptions {
            check_for_subtrees: true,
            allow_deleting_subtrees: false,
            trying_to_clear_with_subtrees_returns_error: true,
        }
    }
}

#[cfg(feature = "minimal")]
#[derive(Clone)]
/// Delete options
pub struct DeleteOptions {
    /// Allow deleting non-empty trees
    pub allow_deleting_non_empty_trees: bool,
    /// Deleting non empty trees returns error
    pub deleting_non_empty_trees_returns_error: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
    /// Validate tree at path exists
    pub validate_tree_at_path_exists: bool,
}

#[cfg(feature = "minimal")]
impl Default for DeleteOptions {
    fn default() -> Self {
        DeleteOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
        }
    }
}

#[cfg(feature = "minimal")]
impl DeleteOptions {
    fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}

#[cfg(feature = "minimal")]
impl GroveDb {
    /// Delete an element at a specified subtree path and key.
    ///
    /// # Dangling references
    ///
    /// This operation does **not** check for incoming references. If other
    /// elements hold [`Reference`](crate::Element::Reference) paths that point
    /// to the deleted element, those references become dangling. Following a
    /// dangling reference will return
    /// [`Error::CorruptedReferencePathKeyNotFound`](crate::Error::CorruptedReferencePathKeyNotFound),
    /// not incorrect data. Callers must manage reference lifecycle and remove
    /// or update any references to this element before deleting it.
    pub fn delete<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        options: Option<DeleteOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "delete",
            grove_version.grovedb_versions.operations.delete.delete
        );

        let tx = TxRef::new(&self.db, transaction);

        let options = options.unwrap_or_default();
        let batch = StorageBatch::new();

        let mut cost = Default::default();

        cost_return_on_error!(
            &mut cost,
            self.delete_internal_on_transaction(
                path.into(),
                key,
                &options,
                tx.as_ref(),
                &mut |_, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                &batch,
                grove_version,
            )
            .map_ok(|_| ())
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    /// Delete all elements in a specified subtree.
    /// Returns if we successfully cleared the subtree.
    ///
    /// # Dangling references
    ///
    /// This operation does **not** check for incoming references. Any
    /// [`Reference`](crate::Element::Reference) elements elsewhere in the
    /// database that point to elements within the cleared subtree will become
    /// dangling. See the [module-level documentation](self) for details.
    pub fn clear_subtree<'b, B, P>(
        &self,
        path: P,
        options: Option<ClearOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> Result<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.clear_subtree_with_costs(path, options, transaction, grove_version)
            .unwrap()
    }

    /// Delete all elements in a specified subtree and get back costs
    /// Warning: The costs for this operation are not yet correct, hence we
    /// should keep this private for now
    /// Returns if we successfully cleared the subtree
    fn clear_subtree_with_costs<'b, B, P>(
        &self,
        path: P,
        options: Option<ClearOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "clear_subtree",
            grove_version
                .grovedb_versions
                .operations
                .delete
                .clear_subtree
        );

        let tx = TxRef::new(&self.db, transaction);

        let subtree_path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let batch = StorageBatch::new();

        let options = options.unwrap_or_default();

        let mut merk_to_clear = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                subtree_path.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version,
            )
        );

        // Clearing an indexed primary would empty the primary Merk while
        // leaving every per-axis secondary Merk fully populated, so the
        // element's secondary root key / axes digest would still commit to
        // rows that no longer exist. Reject rather than corrupt; callers
        // should delete the indexed tree itself (which sweeps all axes) or
        // remove entries through the dedicated `delete_from_*` APIs.
        cost_return_on_error_no_add!(
            cost,
            crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
                merk_to_clear.tree_type,
                "clear_subtree",
            )
        );

        // Non-Merk data trees store data in the data namespace as non-Element
        // entries.  We cannot iterate them with Element::iterator, so just
        // clear the storage directly.
        if merk_to_clear.tree_type.uses_non_merk_data_storage() {
            let mut storage = self
                .db
                .get_transactional_storage_context(subtree_path.clone(), Some(&batch), tx.as_ref())
                .unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                storage.clear().map_err(|e| {
                    Error::CorruptedData(format!(
                        "unable to clear non-merk tree data from storage: {e}",
                    ))
                })
            );

            cost_return_on_error!(
                &mut cost,
                self.db
                    .commit_multi_context_batch(batch, Some(tx.as_ref()))
                    .map_err(Into::into)
            );

            return tx.commit_local().map(|_| true).wrap_with_cost(cost);
        }

        if options.check_for_subtrees {
            let mut all_query = Query::new();
            all_query.insert_all();

            let mut element_iterator =
                KVIterator::new(merk_to_clear.storage.raw_iter(), &all_query).unwrap();

            // delete all nested subtrees
            while let Some((key, element_value)) =
                element_iterator.next_kv().unwrap_add_cost(&mut cost)
            {
                let element = match Element::raw_decode(&element_value, grove_version) {
                    Ok(e) => e,
                    Err(e) => {
                        return Err(Error::CorruptedData(format!(
                            "unable to decode element while clearing subtree: {e}"
                        )))
                        .wrap_with_cost(cost);
                    }
                };
                if element.is_any_tree() {
                    if options.allow_deleting_subtrees {
                        cost_return_on_error!(
                            &mut cost,
                            self.delete(
                                subtree_path.clone(),
                                key.as_slice(),
                                Some(DeleteOptions {
                                    allow_deleting_non_empty_trees: true,
                                    deleting_non_empty_trees_returns_error: false,
                                    ..Default::default()
                                }),
                                Some(tx.as_ref()),
                                grove_version,
                            )
                        );
                    } else if options.trying_to_clear_with_subtrees_returns_error {
                        return Err(Error::ClearingTreeWithSubtreesNotAllowed(
                            "options do not allow to clear this merk tree as it contains subtrees",
                        ))
                        .wrap_with_cost(cost);
                    } else {
                        return Ok(false).wrap_with_cost(cost);
                    }
                }
            }
        }

        // delete non subtree values
        cost_return_on_error!(&mut cost, merk_to_clear.clear().map_err(Error::MerkError));

        // propagate changes
        let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();
        merk_cache.insert(subtree_path.clone(), merk_to_clear);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(
                merk_cache,
                subtree_path.clone(),
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().map(|_| true).wrap_with_cost(cost)
    }

    /// Delete element with sectional storage function.
    ///
    /// # Dangling references
    ///
    /// This operation does **not** check for incoming references. Any
    /// [`Reference`](crate::Element::Reference) elements that point to the
    /// deleted element will become dangling. See the
    /// [module-level documentation](self) for details.
    pub fn delete_with_sectional_storage_function<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: Option<DeleteOptions>,
        transaction: TransactionArg,
        split_removal_bytes_function: &mut impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "delete_with_sectional_storage_function",
            grove_version
                .grovedb_versions
                .operations
                .delete
                .delete_with_sectional_storage_function
        );
        let _storage_removal_version_guard =
            grovedb_costs::storage_cost::removal::use_basic_sectioned_removal_addition_version(
                grove_version
                    .grovedb_versions
                    .storage_costs
                    .add_basic_storage_removal_to_sectioned_storage_removal,
            );

        let tx = TxRef::new(&self.db, transaction);

        let options = options.unwrap_or_default();
        let batch = StorageBatch::new();

        let mut cost = Default::default();

        cost_return_on_error!(
            &mut cost,
            self.delete_internal_on_transaction(
                path,
                key,
                &options,
                tx.as_ref(),
                &mut |value, removed_key_bytes, removed_value_bytes| {
                    let mut element = Element::deserialize(value.as_slice(), grove_version)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_flags = element.get_flags_mut();
                    match maybe_flags {
                        None => Ok((
                            BasicStorageRemoval(removed_key_bytes),
                            BasicStorageRemoval(removed_value_bytes),
                        )),
                        Some(flags) => split_removal_bytes_function(
                            flags,
                            removed_key_bytes,
                            removed_value_bytes,
                        )
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string())),
                    }
                },
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    /// Delete if an empty tree.
    ///
    /// # Dangling references
    ///
    /// This operation does **not** check for incoming references. Any
    /// [`Reference`](crate::Element::Reference) elements that point to the
    /// deleted tree will become dangling. See the
    /// [module-level documentation](self) for details.
    pub fn delete_if_empty_tree<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "delete_if_empty_tree",
            grove_version
                .grovedb_versions
                .operations
                .delete
                .delete_if_empty_tree
        );

        let mut cost = Default::default();

        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);

        let result = cost_return_on_error!(
            &mut cost,
            self.delete_if_empty_tree_with_sectional_storage_function(
                path.into(),
                key,
                tx.as_ref(),
                &mut |_, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().map(|_| result).wrap_with_cost(cost)
    }

    /// Delete if an empty tree with section storage function
    fn delete_if_empty_tree_with_sectional_storage_function<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        transaction: &Transaction,
        split_removal_bytes_function: &mut impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        check_grovedb_v0_with_cost!(
            "delete_if_empty_tree_with_sectional_storage_function",
            grove_version
                .grovedb_versions
                .operations
                .delete
                .delete_if_empty_tree_with_sectional_storage_function
        );
        let _storage_removal_version_guard =
            grovedb_costs::storage_cost::removal::use_basic_sectioned_removal_addition_version(
                grove_version
                    .grovedb_versions
                    .storage_costs
                    .add_basic_storage_removal_to_sectioned_storage_removal,
            );

        let options = DeleteOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        };

        self.delete_internal_on_transaction(
            path,
            key,
            &options,
            transaction,
            &mut |value, removed_key_bytes, removed_value_bytes| {
                let mut element = Element::deserialize(value.as_slice(), grove_version)
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                let maybe_flags = element.get_flags_mut();
                match maybe_flags {
                    None => Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    )),
                    Some(flags) => {
                        split_removal_bytes_function(flags, removed_key_bytes, removed_value_bytes)
                            .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                    }
                }
            },
            batch,
            grove_version,
        )
    }

    /// Delete operation for delete internal.
    ///
    /// # Dangling references
    ///
    /// This operation does **not** check for incoming references. Any
    /// [`Reference`](crate::Element::Reference) elements that point to the
    /// deleted element will become dangling. See the
    /// [module-level documentation](self) for details.
    pub fn delete_operation_for_delete_internal<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteOptions,
        is_known_to_be_subtree: Option<MaybeTree>,
        current_batch_operations: &[QualifiedGroveDbOp],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<QualifiedGroveDbOp>, Error> {
        check_grovedb_v0_with_cost!(
            "delete_operation_for_delete_internal",
            grove_version
                .grovedb_versions
                .operations
                .delete
                .delete_operation_for_delete_internal
        );

        let tx = TxRef::new(&self.db, transaction);

        let mut cost = OperationCost::default();

        if path.is_root() {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leaves currently cannot be deleted".to_owned(),
            ))
            .wrap_with_cost(cost)
        } else {
            if options.validate_tree_at_path_exists {
                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_path_not_found(
                        path.clone(),
                        tx.as_ref(),
                        grove_version
                    )
                );
            }
            // Fetch the element if not already known, so we can determine
            // tree type and (for non-Merk trees) entry count.
            let element = match is_known_to_be_subtree {
                None => Some(cost_return_on_error!(
                    &mut cost,
                    self.get_raw(path.clone(), key.as_ref(), Some(tx.as_ref()), grove_version)
                )),
                Some(_) => None,
            };
            let tree_type = match (&element, is_known_to_be_subtree) {
                (Some(el), _) => el.maybe_tree_type(),
                (None, Some(x)) => x,
                _ => unreachable!(),
            };

            if let MaybeTree::Tree(tree_type) = tree_type {
                let subtree_merk_path = path.derive_owned_with_child(key);
                let subtree_merk_path_vec = subtree_merk_path.to_vec();

                // Non-Merk data trees (CommitmentTree, MmrTree,
                // BulkAppendTree, DenseTree) never contain child subtrees
                // in the Merk sense, so is_empty_tree_except would
                // incorrectly see non-Merk keys.  We check their
                // element-level entry count instead.
                let mut is_empty = if tree_type.uses_non_merk_data_storage() {
                    // If we already fetched the element, use it; otherwise
                    // fetch it now to check the entry count.
                    let count = if let Some(ref el) = element {
                        el.non_merk_entry_count().unwrap_or(0)
                    } else {
                        let el = cost_return_on_error!(
                            &mut cost,
                            self.get_raw(
                                path.clone(),
                                key.as_ref(),
                                Some(tx.as_ref()),
                                grove_version,
                            )
                        );
                        el.non_merk_entry_count().unwrap_or(0)
                    };
                    count == 0
                } else {
                    let batch_deleted_keys = current_batch_operations
                        .iter()
                        .filter_map(|op| match op.op {
                            GroveOp::Delete | GroveOp::DeleteTree(..) => {
                                if op.path.eq_path_vec(&subtree_merk_path_vec) {
                                    Some(op.key.as_ref()?.as_slice())
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        })
                        .collect::<BTreeSet<&[u8]>>();
                    let subtree = cost_return_on_error!(
                        &mut cost,
                        compat::merk_optional_tx_path_not_empty(
                            &self.db,
                            SubtreePath::from(&subtree_merk_path),
                            tx.as_ref(),
                            None,
                            grove_version,
                        )
                    );

                    subtree
                        .is_empty_tree_except(batch_deleted_keys)
                        .unwrap_add_cost(&mut cost)
                };

                // If there is any current batch operation that is inserting something in this
                // tree then it is not empty either
                is_empty &= !current_batch_operations.iter().any(|op| match op.op {
                    GroveOp::Delete | GroveOp::DeleteTree(..) => false,
                    _ => op.path.eq_path_vec(&subtree_merk_path_vec),
                });

                let result = if !options.allow_deleting_non_empty_trees && !is_empty {
                    if options.deleting_non_empty_trees_returns_error {
                        Err(Error::DeletingNonEmptyTree(
                            "trying to do a delete operation for a non empty tree, but options \
                             not allowing this",
                        ))
                    } else {
                        Ok(None)
                    }
                } else if is_empty {
                    // Emptiness was already verified above — use
                    // DontCheckWithNoCleanup to avoid a redundant re-check
                    // and skip cleanup (the tree is empty, nothing to clean).
                    Ok(Some(QualifiedGroveDbOp::delete_tree_op(
                        path.to_vec(),
                        key.to_vec(),
                        tree_type,
                        SubelementsDeletionBehavior::DontCheckWithNoCleanup,
                    )))
                } else {
                    Err(Error::NotSupported(
                        "deletion operation for non empty tree not currently supported".to_string(),
                    ))
                };
                result.wrap_with_cost(cost)
            } else {
                Ok(Some(QualifiedGroveDbOp::delete_op(
                    path.to_vec(),
                    key.to_vec(),
                )))
                .wrap_with_cost(cost)
            }
        }
    }
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests {
    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::BasicStorageRemoval, StorageCost},
        OperationCost,
    };
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};
    use pretty_assertions::assert_eq;

    use crate::{
        operations::delete::{delete_up_tree::DeleteUpTreeOptions, ClearOptions, DeleteOptions},
        reference_path::ReferencePathType,
        tests::{
            common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF,
        },
        Element, Error,
    };

    /// Issue #686 regression: deleting a non-empty child tree must keep
    /// delete propagation on the PARENT's tree type. Under the legacy path
    /// the parent Merk was reopened labeled with the child's tree type.
    #[test]
    fn test_non_empty_tree_delete_under_count_tree_parent_updates_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful count tree insert");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child tree insert");
        db.insert(
            [TEST_LEAF, b"parent", b"child"].as_ref(),
            b"leaf",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child item insert");

        let before = db
            .get([TEST_LEAF].as_ref(), b"parent", None, grove_version)
            .unwrap()
            .expect("expected parent count tree");
        assert!(matches!(before, Element::CountTree(_, 1, _)));

        db.delete(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete non-empty child tree");

        let after = db
            .get([TEST_LEAF].as_ref(), b"parent", None, grove_version)
            .unwrap()
            .expect("expected parent count tree");
        assert!(matches!(after, Element::CountTree(_, 0, _)));
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify grovedb");
        assert!(issues.is_empty(), "verification issues: {:?}", issues);
    }

    /// Issue #686 regression: CountSumTree parent with a SumTree child —
    /// both count and sum must settle after deleting the populated child.
    #[test]
    fn test_non_empty_tree_delete_under_count_sum_tree_parent_updates_count_and_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful count sum tree insert");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child sum tree insert");
        db.insert(
            [TEST_LEAF, b"parent", b"child"].as_ref(),
            b"leaf",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child sum item insert");

        let before = db
            .get([TEST_LEAF].as_ref(), b"parent", None, grove_version)
            .unwrap()
            .expect("expected parent count sum tree");
        assert!(matches!(before, Element::CountSumTree(_, 1, 7, _)));

        db.delete(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete non-empty child tree");

        let after = db
            .get([TEST_LEAF].as_ref(), b"parent", None, grove_version)
            .unwrap()
            .expect("expected parent count sum tree");
        assert!(matches!(after, Element::CountSumTree(_, 0, 0, _)));
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify grovedb");
        assert!(issues.is_empty(), "verification issues: {:?}", issues);
    }

    /// Issue #686, the case that actually diverges on current code: a
    /// Provable* parent's link hash embeds its aggregate
    /// (`hash_for_link`), so reopening the parent labeled with the child's
    /// plain-tree type (legacy path) commits a wrong link hash into the
    /// grandparent. Under GROVE_V4 the already-open parent Merk is reused
    /// and the binding stays consistent.
    #[test]
    fn test_non_empty_tree_delete_under_provable_count_tree_parent_v4_keeps_binding() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful provable count tree insert");
        // A sibling keeps the parent Merk non-empty after the delete so the
        // link hash is actually recomputed during propagation.
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"sibling",
            Element::new_item(b"s".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful sibling insert");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child tree insert");
        db.insert(
            [TEST_LEAF, b"parent", b"child"].as_ref(),
            b"leaf",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child item insert");

        db.delete(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete non-empty child tree");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify grovedb");
        assert!(issues.is_empty(), "verification issues: {:?}", issues);
    }

    /// GROVE_V3 must keep the legacy path byte-for-byte: the same scenario
    /// as `..._v4_keeps_binding` leaves a wrong link hash in the
    /// grandparent, which `verify_grovedb` reports. Any such delete that
    /// happened on a live v3 chain committed that hash into a consensus
    /// root, so replay must reproduce it.
    #[test]
    fn test_non_empty_tree_delete_under_provable_count_tree_parent_v3_keeps_legacy() {
        let grove_version = &GROVE_V3;
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful provable count tree insert");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"sibling",
            Element::new_item(b"s".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful sibling insert");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child tree insert");
        db.insert(
            [TEST_LEAF, b"parent", b"child"].as_ref(),
            b"leaf",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child item insert");

        db.delete(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("legacy delete non-empty child tree");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify grovedb");
        assert!(
            !issues.is_empty(),
            "legacy v3 path is expected to leave a mismatched parent link hash; if this now \
             verifies clean, the v3 behavior changed — that breaks replay compatibility"
        );
    }

    /// Issue #686, panic variant: deleting a non-empty Provable* CHILD under
    /// a plain parent made the legacy path reopen the parent labeled
    /// ProvableCountTree; `hash_for_link` then panics on the parent's basic
    /// nodes. Fixed under GROVE_V4 by reusing the correctly-labeled parent.
    #[test]
    fn test_non_empty_provable_child_delete_under_normal_parent_v4_no_panic() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"child",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful provable count child insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sibling",
            Element::new_item(b"s".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful sibling insert");
        db.insert(
            [TEST_LEAF, b"child"].as_ref(),
            b"leaf",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child item insert");

        db.delete(
            [TEST_LEAF].as_ref(),
            b"child",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete non-empty provable child tree");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify grovedb");
        assert!(issues.is_empty(), "verification issues: {:?}", issues);
    }

    /// Pins the legacy panic on GROVE_V3 (see
    /// `..._v4_no_panic`). If this stops panicking the v3 code path
    /// changed, which would break replay compatibility.
    #[test]
    #[should_panic(expected = "ProvableCountTree::hash_for_link")]
    fn test_non_empty_provable_child_delete_under_normal_parent_v3_panics() {
        let grove_version = &GROVE_V3;
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"child",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful provable count child insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sibling",
            Element::new_item(b"s".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful sibling insert");
        db.insert(
            [TEST_LEAF, b"child"].as_ref(),
            b"leaf",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child item insert");

        let _ = db.delete(
            [TEST_LEAF].as_ref(),
            b"child",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        );
    }

    /// GROVE_V3 keeps the legacy (version 0) delete path for plain aggregate
    /// parents: the delete still lands and — on current code — the count
    /// settles correctly (aggregates are read from node feature types, so
    /// the mislabeled reopen is benign for non-Provable parents).
    #[test]
    fn test_legacy_non_empty_tree_delete_keeps_version_0_path() {
        let grove_version = &GROVE_V3;
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful count tree insert");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child tree insert");
        db.insert(
            [TEST_LEAF, b"parent", b"child"].as_ref(),
            b"leaf",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful child item insert");

        db.delete(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("legacy delete non-empty child tree");

        assert!(matches!(
            db.get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"child",
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        let after = db
            .get([TEST_LEAF].as_ref(), b"parent", None, grove_version)
            .unwrap()
            .expect("expected parent count tree");
        assert!(matches!(after, Element::CountTree(_, 0, _)));
    }

    #[test]
    fn test_empty_subtree_deletion_without_transaction() {
        let grove_version = GroveVersion::latest();
        let _element = Element::new_item(b"ayy".to_vec());
        let db = make_test_grovedb(grove_version);
        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key4",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 3 insert");

        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        db.delete([TEST_LEAF].as_ref(), b"key1", None, None, grove_version)
            .unwrap()
            .expect("unable to delete subtree");
        assert!(matches!(
            db.get(
                [TEST_LEAF, b"key1", b"key2"].as_ref(),
                b"key3",
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        // assert_eq!(db.subtrees.len().unwrap(), 3); // TEST_LEAF, ANOTHER_TEST_LEAF
        // TEST_LEAF.key4 stay
        assert!(db
            .get(EMPTY_PATH, TEST_LEAF, None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get(EMPTY_PATH, ANOTHER_TEST_LEAF, None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get([TEST_LEAF].as_ref(), b"key4", None, grove_version)
            .unwrap()
            .is_ok());
        assert_ne!(
            root_hash,
            db.root_hash(None, grove_version).unwrap().unwrap()
        );
    }

    #[test]
    fn test_empty_subtree_deletion_with_transaction() {
        let grove_version = GroveVersion::latest();
        let _element = Element::new_item(b"ayy".to_vec());

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key4",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 3 insert");

        db.delete(
            [TEST_LEAF].as_ref(),
            b"key1",
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("unable to delete subtree");
        assert!(matches!(
            db.get(
                [TEST_LEAF, b"key1", b"key2"].as_ref(),
                b"key3",
                Some(&transaction),
                grove_version
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        transaction.commit().expect("cannot commit transaction");
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), b"key1", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert!(db
            .get([TEST_LEAF].as_ref(), b"key4", None, grove_version)
            .unwrap()
            .is_ok());
    }

    #[test]
    fn test_subtree_deletion_if_empty_with_transaction() {
        let grove_version = GroveVersion::latest();
        let element = Element::new_item(b"value".to_vec());
        let db = make_test_grovedb(grove_version);

        let transaction = db.start_transaction();

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"level1-A",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert A on level 1");
        db.insert(
            [TEST_LEAF, b"level1-A"].as_ref(),
            b"level2-A",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert A on level 2");
        db.insert(
            [TEST_LEAF, b"level1-A"].as_ref(),
            b"level2-B",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert B on level 2");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"level1-A", b"level2-A"].as_ref(),
            b"level3-A",
            element,
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"level1-B",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert B on level 1");

        db.commit_transaction(transaction)
            .unwrap()
            .expect("cannot commit changes");

        // Currently we have:
        // Level 1:            A
        //                    / \
        // Level 2:          A   B
        //                   |
        // Level 3:          A: value

        let transaction = db.start_transaction();

        let deleted = db
            .delete_if_empty_tree(
                [TEST_LEAF].as_ref(),
                b"level1-A",
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("unable to delete subtree");
        assert!(!deleted);

        let deleted = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"level1-A", b"level2-A"].as_ref(),
                b"level3-A",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(0),
                    ..Default::default()
                },
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("unable to delete subtree");
        assert_eq!(deleted, 2);

        assert!(matches!(
            db.get(
                [TEST_LEAF, b"level1-A", b"level2-A"].as_ref(),
                b"level3-A",
                Some(&transaction),
                grove_version
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));

        assert!(matches!(
            db.get(
                [TEST_LEAF, b"level1-A"].as_ref(),
                b"level2-A",
                Some(&transaction),
                grove_version
            )
            .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));

        assert!(matches!(
            db.get(
                [TEST_LEAF].as_ref(),
                b"level1-A",
                Some(&transaction),
                grove_version
            )
            .unwrap(),
            Ok(Element::Tree(..)),
        ));
    }

    #[test]
    fn test_subtree_deletion_if_empty_without_transaction() {
        let grove_version = GroveVersion::latest();
        let element = Element::new_item(b"value".to_vec());
        let db = make_test_grovedb(grove_version);

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"level1-A",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert A on level 1");
        db.insert(
            [TEST_LEAF, b"level1-A"].as_ref(),
            b"level2-A",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert A on level 2");
        db.insert(
            [TEST_LEAF, b"level1-A"].as_ref(),
            b"level2-B",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert B on level 2");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"level1-A", b"level2-A"].as_ref(),
            b"level3-A",
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"level1-B",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert B on level 1");

        // Currently we have:
        // Level 1:            A
        //                    / \
        // Level 2:          A   B
        //                   |
        // Level 3:          A: value

        let deleted = db
            .delete_if_empty_tree([TEST_LEAF].as_ref(), b"level1-A", None, grove_version)
            .unwrap()
            .expect("unable to delete subtree");
        assert!(!deleted);

        let deleted = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"level1-A", b"level2-A"].as_ref(),
                b"level3-A",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(0),
                    ..Default::default()
                },
                None,
                grove_version,
            )
            .unwrap()
            .expect("unable to delete subtree");
        assert_eq!(deleted, 2);

        assert!(matches!(
            db.get(
                [TEST_LEAF, b"level1-A", b"level2-A"].as_ref(),
                b"level3-A",
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));

        assert!(matches!(
            db.get(
                [TEST_LEAF, b"level1-A"].as_ref(),
                b"level2-A",
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));

        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), b"level1-A", None, grove_version)
                .unwrap(),
            Ok(Element::Tree(..)),
        ));
    }

    #[test]
    fn test_recurring_deletion_through_subtrees_with_transaction() {
        let grove_version = GroveVersion::latest();
        let element = Element::new_item(b"ayy".to_vec());

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 2 insert");

        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1", b"key2"].as_ref(),
            b"key3",
            element,
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key4",
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 3 insert");

        db.delete(
            [TEST_LEAF].as_ref(),
            b"key1",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("unable to delete subtree");
        assert!(matches!(
            db.get(
                [TEST_LEAF, b"key1", b"key2"].as_ref(),
                b"key3",
                Some(&transaction),
                grove_version
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        transaction.commit().expect("cannot commit transaction");
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), b"key1", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        db.get([TEST_LEAF].as_ref(), b"key4", None, grove_version)
            .unwrap()
            .expect("expected to get key4");
    }

    #[test]
    fn test_recurring_deletion_through_subtrees_without_transaction() {
        let grove_version = GroveVersion::latest();
        let element = Element::new_item(b"ayy".to_vec());

        let db = make_test_grovedb(grove_version);

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 2 insert");

        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1", b"key2"].as_ref(),
            b"key3",
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key4",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 3 insert");

        db.delete(
            [TEST_LEAF].as_ref(),
            b"key1",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("unable to delete subtree");
        assert!(matches!(
            db.get(
                [TEST_LEAF, b"key1", b"key2"].as_ref(),
                b"key3",
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), b"key1", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert!(db
            .get([TEST_LEAF].as_ref(), b"key4", None, grove_version)
            .unwrap()
            .is_ok());
    }

    #[test]
    fn test_item_deletion() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful insert");
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        assert!(db
            .delete([TEST_LEAF].as_ref(), b"key", None, None, grove_version)
            .unwrap()
            .is_ok());
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), b"key", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert_ne!(
            root_hash,
            db.root_hash(None, grove_version).unwrap().unwrap()
        );
    }

    #[test]
    fn test_delete_one_item_cost() {
        let grove_version = GroveVersion::latest();
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
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 0,
                    removed_bytes: BasicStorageRemoval(149)
                },
                storage_loaded_bytes: 154, // todo: verify this
                hash_node_calls: 0,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_delete_one_sum_item_cost() {
        let grove_version = GroveVersion::latest();
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
                seek_count: 8, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 91,
                    removed_bytes: BasicStorageRemoval(170)
                },
                storage_loaded_bytes: 418, // todo: verify this
                hash_node_calls: 5,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_delete_one_item_in_sum_tree_cost() {
        let grove_version = GroveVersion::latest();
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
                seek_count: 8, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 91,
                    removed_bytes: BasicStorageRemoval(167)
                },
                storage_loaded_bytes: 418, // todo: verify this
                hash_node_calls: 5,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_subtree_clear() {
        let grove_version = GroveVersion::latest();
        let element = Element::new_item(b"ayy".to_vec());

        let db = make_test_grovedb(grove_version);

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 2 insert");

        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1", b"key2"].as_ref(),
            b"key3",
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key4",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree 3 insert");

        let key1_tree = db
            .get([TEST_LEAF].as_ref(), b"key1", None, grove_version)
            .unwrap()
            .unwrap();
        assert!(!matches!(key1_tree, Element::Tree(None, _)));

        let transaction = db.start_transaction();

        let key1_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        assert_ne!(key1_merk.root_hash().unwrap(), [0; 32]);

        let root_hash_before_clear = db.root_hash(None, grove_version).unwrap().unwrap();
        db.clear_subtree([TEST_LEAF, b"key1"].as_ref(), None, None, grove_version)
            .expect_err("unable to delete subtree");

        let success = db
            .clear_subtree(
                [TEST_LEAF, b"key1"].as_ref(),
                Some(ClearOptions {
                    check_for_subtrees: true,
                    allow_deleting_subtrees: false,
                    trying_to_clear_with_subtrees_returns_error: false,
                }),
                None,
                grove_version,
            )
            .expect("expected no error");
        assert!(!success);

        let success = db
            .clear_subtree(
                [TEST_LEAF, b"key1"].as_ref(),
                Some(ClearOptions {
                    check_for_subtrees: true,
                    allow_deleting_subtrees: true,
                    trying_to_clear_with_subtrees_returns_error: false,
                }),
                None,
                grove_version,
            )
            .expect("unable to delete subtree");

        assert!(success);

        assert!(matches!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert!(matches!(
            db.get(
                [TEST_LEAF, b"key1", b"key2"].as_ref(),
                b"key3",
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        let key1_tree = db
            .get([TEST_LEAF].as_ref(), b"key1", None, grove_version)
            .unwrap()
            .unwrap();
        assert!(matches!(key1_tree, Element::Tree(None, _)));

        let transaction = db.start_transaction();

        let key1_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        assert_eq!(key1_merk.root_hash().unwrap(), [0; 32]);

        let root_hash_after_clear = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_ne!(root_hash_before_clear, root_hash_after_clear);
    }

    /// Documents known behavior: deleting a referenced element leaves a
    /// dangling reference.  Following the dangling reference must return
    /// `CorruptedReferencePathKeyNotFound` (safe failure), never wrong data.
    #[test]
    fn test_delete_referenced_element_leaves_dangling_reference() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Step 1: Insert an item that will be referenced.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target_item",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful target item insert");

        // Step 2: Insert a reference pointing to the item.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_to_target",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"target_item".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful reference insert");

        // Sanity check: following the reference resolves to the target item.
        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_to_target", None, grove_version)
            .unwrap()
            .expect("expected successful get through reference");
        assert_eq!(result, Element::new_item(b"hello".to_vec()));

        // Step 3: Delete the target item without removing the reference first.
        // GroveDB does not track backward references, so this succeeds.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"target_item",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful delete of referenced item");

        // Step 4: The reference still exists in the database.
        let raw_ref = db
            .get_raw(
                [TEST_LEAF].as_ref().into(),
                b"ref_to_target",
                None,
                grove_version,
            )
            .unwrap()
            .expect("reference element should still exist");
        assert!(
            matches!(raw_ref, Element::Reference(..)),
            "expected a Reference element, got {:?}",
            raw_ref
        );

        // Step 5: Following the now-dangling reference must return
        // CorruptedReferencePathKeyNotFound, NOT wrong data.
        let err = db
            .get([TEST_LEAF].as_ref(), b"ref_to_target", None, grove_version)
            .unwrap()
            .expect_err("expected error when following dangling reference");
        assert!(
            matches!(err, Error::CorruptedReferencePathKeyNotFound(_)),
            "expected CorruptedReferencePathKeyNotFound, got {:?}",
            err
        );
    }
}
