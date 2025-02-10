//! Delete operations and costs

mod v0;
mod v1;

#[cfg(feature = "estimated_costs")]
mod average_case;
#[cfg(feature = "minimal")]
mod delete_up_tree;
#[cfg(feature = "estimated_costs")]
mod worst_case;

#[cfg(feature = "minimal")]
use std::collections::BTreeSet;

#[cfg(feature = "minimal")]
pub use delete_up_tree::DeleteUpTreeOptions;
#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error,
    storage_cost::removal::StorageRemovedBytes::{self, BasicStorageRemoval},
    CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "minimal")]
use grovedb_merk::MaybeTree;
#[cfg(feature = "minimal")]
use grovedb_merk::{Error as MerkError, MerkOptions};
use grovedb_path::SubtreePath;
#[cfg(feature = "minimal")]
use grovedb_storage::{Storage, StorageBatch};
use grovedb_version::{check_grovedb_v0_with_cost, dispatch_version, version::GroveVersion};

use crate::util::{compat, TxRef};
#[cfg(feature = "minimal")]
use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
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
    /// Trigger cascade deletions of backward references' chains
    pub propagate_backward_references: bool,
}

#[cfg(feature = "minimal")]
impl Default for ClearOptions {
    fn default() -> Self {
        ClearOptions {
            check_for_subtrees: true,
            allow_deleting_subtrees: false,
            trying_to_clear_with_subtrees_returns_error: true,
            propagate_backward_references: false,
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
    /// Trigger cascade deletions of backward references' chains
    pub propagate_backward_references: bool,
}

#[cfg(feature = "minimal")]
impl Default for DeleteOptions {
    fn default() -> Self {
        DeleteOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
            propagate_backward_references: false,
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
        // The deletion process on high level is the same for both versions:
        dispatch_version!(
            "delete",
            grove_version.grovedb_versions.operations.delete.delete,
            0 | 1 => {}
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

    /// Delete all elements in a specified subtree
    /// Returns if we successfully cleared the subtree
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
        let tx = TxRef::new(&self.db, transaction);
        if self
            .clear_subtree_with_costs(path, options, tx.as_ref(), grove_version)
            .unwrap()?
        {
            tx.commit_local()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn clear_subtree_with_costs<'b, B, P>(
        &self,
        path: P,
        options: Option<ClearOptions>,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        dispatch_version!(
            "clear_subtree",
            grove_version
                .grovedb_versions
                .operations
                .delete
                .clear_subtree,
            0 => {
                v0::clear_subtree_with_costs(
                    self,
                    path,
                    options,
                    transaction,
                    grove_version,
                )
            }
            1 => {
                v1::clear_subtree_with_costs(
                    self,
                    path.into().derive_owned(),
                    options,
                    transaction,
                    grove_version,
                )
            }
        )
    }

    /// Delete element with sectional storage function
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

    /// Delete if an empty tree
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

    /// Delete operation for delete internal
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
            let tree_type = match is_known_to_be_subtree {
                None => {
                    let element = cost_return_on_error!(
                        &mut cost,
                        self.get_raw(path.clone(), key.as_ref(), Some(tx.as_ref()), grove_version)
                    );
                    element.maybe_tree_type()
                }
                Some(x) => x,
            };

            if let MaybeTree::Tree(tree_type) = tree_type {
                let subtree_merk_path = path.derive_owned_with_child(key);
                let subtree_merk_path_vec = subtree_merk_path.to_vec();
                let batch_deleted_keys = current_batch_operations
                    .iter()
                    .filter_map(|op| match op.op {
                        GroveOp::Delete | GroveOp::DeleteTree(_) => {
                            // todo: to_path clones (best to figure out how to compare without
                            // cloning)
                            if op.path.to_path() == subtree_merk_path_vec {
                                Some(op.key.as_slice())
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

                let mut is_empty = subtree
                    .is_empty_tree_except(batch_deleted_keys)
                    .unwrap_add_cost(&mut cost);

                // If there is any current batch operation that is inserting something in this
                // tree then it is not empty either
                is_empty &= !current_batch_operations.iter().any(|op| match op.op {
                    GroveOp::Delete | GroveOp::DeleteTree(_) => false,
                    // todo: fix for to_path (it clones)
                    _ => op.path.to_path() == subtree_merk_path_vec,
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
                    Ok(Some(QualifiedGroveDbOp::delete_tree_op(
                        path.to_vec(),
                        key.to_vec(),
                        tree_type,
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

    fn delete_internal_on_transaction<B: AsRef<[u8]>>(
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
        dispatch_version!(
            "delete_internal_on_transaction",
            grove_version
                .grovedb_versions
                .operations
                .delete
                .delete_internal_on_transaction,
            0 => {
                v0::delete_internal_on_transaction(
                    self,
                    path,
                    key,
                    options,
                    transaction,
                    sectioned_removal,
                    batch,
                    grove_version
                )
            }
            1 => {
                v1::delete_internal_on_transaction(
                    self,
                    path,
                    key,
                    options,
                    transaction,
                    sectioned_removal,
                    batch,
                    grove_version
                )
            }
        )
    }
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests {
    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::BasicStorageRemoval, StorageCost},
        OperationCost,
    };
    use grovedb_version::version::GroveVersion;
    use pretty_assertions::assert_eq;

    use crate::{
        operations::delete::{delete_up_tree::DeleteUpTreeOptions, ClearOptions, DeleteOptions},
        tests::{
            common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF,
        },
        Element, Error,
    };

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
                    propagate_backward_references: false,
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
                    propagate_backward_references: false,
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
}
