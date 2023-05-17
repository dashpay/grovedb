// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Delete up tree

use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    CostResult, CostsExt, OperationCost,
};
use path::SubtreePath;

use crate::{
    batch::GroveDbOp, operations::delete::DeleteOptions, ElementFlags, Error, GroveDb,
    TransactionArg,
};

#[cfg(feature = "full")]
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
}

#[cfg(feature = "full")]
impl Default for DeleteUpTreeOptions {
    fn default() -> Self {
        DeleteUpTreeOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
            stop_path_height: None,
        }
    }
}

#[cfg(feature = "full")]
impl DeleteUpTreeOptions {
    fn to_delete_options(&self) -> DeleteOptions {
        DeleteOptions {
            allow_deleting_non_empty_trees: self.allow_deleting_non_empty_trees,
            deleting_non_empty_trees_returns_error: self.deleting_non_empty_trees_returns_error,
            base_root_storage_is_free: self.base_root_storage_is_free,
            validate_tree_at_path_exists: self.validate_tree_at_path_exists,
        }
    }
}

#[cfg(feature = "full")]
impl GroveDb {
    /// Delete up tree while empty will delete nodes while they are empty up a
    /// tree.
    pub fn delete_up_tree_while_empty<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        transaction: TransactionArg,
    ) -> CostResult<u16, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
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
    ) -> CostResult<u16, Error> {
        let mut cost = OperationCost::default();
        let mut batch_operations: Vec<GroveDbOp> = Vec::new();

        let maybe_ops = cost_return_on_error!(
            &mut cost,
            self.add_delete_operations_for_delete_up_tree_while_empty(
                path,
                key,
                options,
                None,
                &mut batch_operations,
                transaction,
            )
        );

        let ops = cost_return_on_error_no_add!(
            &cost,
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
        )
        .map_ok(|_| ops_len as u16)
    }

    /// Returns a vector of GroveDb ops
    pub fn delete_operations_for_delete_up_tree_while_empty<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        is_known_to_be_subtree_with_sum: Option<(bool, bool)>,
        mut current_batch_operations: Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> CostResult<Vec<GroveDbOp>, Error> {
        self.add_delete_operations_for_delete_up_tree_while_empty(
            path,
            key,
            options,
            is_known_to_be_subtree_with_sum,
            &mut current_batch_operations,
            transaction,
        )
        .map_ok(|ops| ops.unwrap_or_default())
    }

    /// Adds operations to "delete operations" for delete up tree while empty
    /// for each level. Returns a vector of GroveDb ops.
    pub fn add_delete_operations_for_delete_up_tree_while_empty<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteUpTreeOptions,
        is_known_to_be_subtree_with_sum: Option<(bool, bool)>,
        current_batch_operations: &mut Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> CostResult<Option<Vec<GroveDbOp>>, Error> {
        let mut cost = OperationCost::default();

        if let Some(stop_path_height) = options.stop_path_height {
            if stop_path_height == path.to_vec().len() as u16 {
                // TODO investigate how necessary it is to have path length
                return Ok(None).wrap_with_cost(cost);
            }
        }
        if options.validate_tree_at_path_exists {
            cost_return_on_error!(
                &mut cost,
                self.check_subtree_exists_path_not_found(path.clone(), transaction)
            );
        }
        if let Some(delete_operation_this_level) = cost_return_on_error!(
            &mut cost,
            self.delete_operation_for_delete_internal(
                path.clone(),
                key,
                &options.to_delete_options(),
                is_known_to_be_subtree_with_sum,
                current_batch_operations,
                transaction,
            )
        ) {
            let mut delete_operations = vec![delete_operation_this_level.clone()];
            if let Some((parent_path, parent_key)) = path.derive_parent() {
                current_batch_operations.push(delete_operation_this_level);
                let mut new_options = options.clone();
                // we should not give an error from now on
                new_options.allow_deleting_non_empty_trees = false;
                new_options.deleting_non_empty_trees_returns_error = false;
                if let Some(mut delete_operations_upper_level) = cost_return_on_error!(
                    &mut cost,
                    self.add_delete_operations_for_delete_up_tree_while_empty(
                        parent_path,
                        parent_key,
                        &new_options,
                        None, // todo: maybe we can know this?
                        current_batch_operations,
                        transaction,
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
