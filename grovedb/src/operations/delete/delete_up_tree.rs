//! Delete up tree

use crate::BackwardsReferences;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::MaybeTree;
use grovedb_path::SubtreePath;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp, operations::delete::DeleteOptions, util::TxRef, ElementFlags, Error,
    GroveDb, TransactionArg,
};

#[cfg(feature = "minimal")]
#[derive(Clone)]
/// Delete up tree options
pub struct DeleteUpTreeOptions {
    /// Allow deleting non empty trees
    pub allow_deleting_non_empty_trees: bool,
    /// Deleting non empty trees returns error
    pub deleting_non_empty_trees_returns_error: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
    /// Validate tree at path exists
    pub validate_tree_at_path_exists: bool,
    /// Stop path height
    pub stop_path_height: Option<u16>,
    /// Whether the values the chain displaces may take part in backward
    /// references. `DontCheck` builds the `DontCheckForBackwardsReferences` twins of the
    /// delete ops; a participant met under that declaration is refused.
    pub backwards_references: BackwardsReferences,
}

#[cfg(feature = "minimal")]
impl Default for DeleteUpTreeOptions {
    fn default() -> Self {
        DeleteUpTreeOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
            stop_path_height: None,
            backwards_references: BackwardsReferences::Check,
        }
    }
}

#[cfg(feature = "minimal")]
impl DeleteUpTreeOptions {
    fn to_delete_options(&self) -> DeleteOptions {
        DeleteOptions {
            allow_deleting_non_empty_trees: self.allow_deleting_non_empty_trees,
            deleting_non_empty_trees_returns_error: self.deleting_non_empty_trees_returns_error,
            base_root_storage_is_free: self.base_root_storage_is_free,
            validate_tree_at_path_exists: self.validate_tree_at_path_exists,
            backwards_references: self.backwards_references,
        }
    }
}

#[cfg(feature = "minimal")]
impl GroveDb {
    /// Delete up tree while empty will delete nodes while they are empty up a
    /// tree.
    ///
    /// # Dangling references
    ///
    /// This operation does **not** check for incoming references. Any
    /// [`Reference`](crate::Element::Reference) elements that point to
    /// deleted elements will become dangling. See the
    /// [module-level documentation](super) for details.
    pub fn delete_up_tree_while_empty<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<u16, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "delete",
            grove_version
                .grovedb_versions
                .operations
                .delete_up_tree
                .delete_up_tree_while_empty
        );
        self.delete_up_tree_while_empty_with_sectional_storage(
            path.into(),
            key,
            options,
            transaction,
            |_, removed_key_bytes, removed_value_bytes| {
                Ok((
                    BasicStorageRemoval(removed_key_bytes),
                    (BasicStorageRemoval(removed_value_bytes)),
                ))
            },
            grove_version,
        )
    }

    /// Delete up tree while empty will delete nodes while they are empty up a
    /// tree.
    pub fn delete_up_tree_while_empty_with_sectional_storage<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        transaction: TransactionArg,
        split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<u16, Error> {
        check_grovedb_v0_with_cost!(
            "delete",
            grove_version
                .grovedb_versions
                .operations
                .delete_up_tree
                .delete_up_tree_while_empty_with_sectional_storage
        );
        let mut cost = OperationCost::default();
        let mut batch_operations: Vec<QualifiedGroveDbOp> = Vec::new();

        let maybe_ops = cost_return_on_error!(
            &mut cost,
            self.add_delete_operations_for_delete_up_tree_while_empty(
                path,
                key,
                options,
                None,
                &mut batch_operations,
                transaction,
                grove_version,
            )
        );

        let ops = cost_return_on_error_no_add!(
            cost,
            if let Some(stop_path_height) = options.stop_path_height {
                maybe_ops.ok_or_else(|| {
                    Error::DeleteUpTreeStopHeightMoreThanInitialPathSize(format!(
                        "stop path height {stop_path_height} is too much",
                    ))
                })
            } else {
                maybe_ops.ok_or(Error::CorruptedCodeExecution(
                    "stop path height not set, but still not deleting element",
                ))
            }
        );
        let ops_len = ops.len();
        self.apply_batch_with_element_flags_update(
            ops,
            None,
            |_, _, _| Ok(false),
            split_removal_bytes_function,
            transaction,
            grove_version,
        )
        .map_ok(|_| u16::try_from(ops_len).unwrap_or(u16::MAX))
    }

    /// Returns a vector of GroveDb ops
    ///
    /// `current_batch_operations` are the operations already in the batch the
    /// deletes are built into. Every level reads them, with the deletes of the
    /// levels below it, as
    /// [`delete_operation_for_delete_internal`](Self::delete_operation_for_delete_internal)
    /// does. They are borrowed, so a caller building many deletes into one
    /// batch passes its pending operations as they are (`&ops`, `ops.iter()`
    /// or an adapter over its own operation type) instead of copying them for
    /// each call.
    pub fn delete_operations_for_delete_up_tree_while_empty<'a, B, I>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        is_known_to_be_subtree: Option<MaybeTree>,
        current_batch_operations: I,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error>
    where
        B: AsRef<[u8]>,
        I: IntoIterator<Item = &'a QualifiedGroveDbOp> + Clone,
    {
        check_grovedb_v0_with_cost!(
            "delete",
            grove_version
                .grovedb_versions
                .operations
                .delete_up_tree
                .delete_operations_for_delete_up_tree_while_empty
        );
        self.add_delete_operations_for_delete_up_tree_while_empty_after(
            path,
            key,
            options,
            is_known_to_be_subtree,
            current_batch_operations,
            &mut Vec::new(),
            transaction,
            grove_version,
        )
        .map_ok(|ops| ops.unwrap_or_default())
    }

    /// Adds operations to "delete operations" for delete up tree while empty
    /// for each level. Returns a vector of GroveDb ops.
    ///
    /// Each level's delete is appended to `current_batch_operations` before
    /// the level above it is built.
    pub fn add_delete_operations_for_delete_up_tree_while_empty<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        is_known_to_be_subtree: Option<MaybeTree>,
        current_batch_operations: &mut Vec<QualifiedGroveDbOp>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<QualifiedGroveDbOp>>, Error> {
        let mut level_deletes = Vec::new();
        let result = self.add_delete_operations_for_delete_up_tree_while_empty_after(
            path,
            key,
            options,
            is_known_to_be_subtree,
            current_batch_operations.iter(),
            &mut level_deletes,
            transaction,
            grove_version,
        );
        current_batch_operations.append(&mut level_deletes);
        result
    }

    /// The levels of
    /// [`add_delete_operations_for_delete_up_tree_while_empty`](Self::add_delete_operations_for_delete_up_tree_while_empty),
    /// reading the pending operations as `current_batch_operations` followed by
    /// `level_deletes`, where each level's delete is appended before the level
    /// above it is built.
    #[allow(clippy::too_many_arguments)]
    fn add_delete_operations_for_delete_up_tree_while_empty_after<'a, B, I>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        is_known_to_be_subtree: Option<MaybeTree>,
        current_batch_operations: I,
        level_deletes: &mut Vec<QualifiedGroveDbOp>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<QualifiedGroveDbOp>>, Error>
    where
        B: AsRef<[u8]>,
        I: IntoIterator<Item = &'a QualifiedGroveDbOp> + Clone,
    {
        check_grovedb_v0_with_cost!(
            "delete",
            grove_version
                .grovedb_versions
                .operations
                .delete_up_tree
                .add_delete_operations_for_delete_up_tree_while_empty
        );
        let mut cost = OperationCost::default();

        if let Some(stop_path_height) = options.stop_path_height
            && u16::try_from(path.to_vec().len()).unwrap_or(u16::MAX) == stop_path_height
        {
            // TODO investigate how necessary it is to have path length
            return Ok(None).wrap_with_cost(cost);
        }

        let tx = TxRef::new(&self.db, transaction);

        if options.validate_tree_at_path_exists {
            cost_return_on_error!(
                &mut cost,
                self.check_subtree_exists_path_not_found(path.clone(), tx.as_ref(), grove_version)
            );
        }
        // The `map` shortens the borrow of the caller's operations to this
        // level's, so the deletes built for the levels below can follow them.
        #[allow(clippy::map_identity)]
        let pending_operations = current_batch_operations
            .clone()
            .into_iter()
            .map(|op| op)
            .chain(level_deletes.iter());
        if let Some(delete_operation_this_level) = cost_return_on_error!(
            &mut cost,
            self.delete_operation_for_delete_internal(
                path.clone(),
                key,
                &options.to_delete_options(),
                is_known_to_be_subtree,
                pending_operations,
                Some(tx.as_ref()),
                grove_version,
            )
        ) {
            let mut delete_operations = vec![delete_operation_this_level.clone()];
            if let Some((parent_path, parent_key)) = path.derive_parent() {
                level_deletes.push(delete_operation_this_level);
                let mut new_options = options.clone();
                // we should not give an error from now on
                new_options.allow_deleting_non_empty_trees = false;
                new_options.deleting_non_empty_trees_returns_error = false;
                if let Some(mut delete_operations_upper_level) = cost_return_on_error!(
                    &mut cost,
                    self.add_delete_operations_for_delete_up_tree_while_empty_after(
                        parent_path,
                        parent_key,
                        &new_options,
                        None, // todo: maybe we can know this?
                        current_batch_operations,
                        level_deletes,
                        transaction,
                        grove_version,
                    )
                ) {
                    delete_operations.append(&mut delete_operations_upper_level);
                }
            }
            Ok(Some(delete_operations)).wrap_with_cost(cost)
        } else {
            Ok(None).wrap_with_cost(cost)
        }
    }
}
