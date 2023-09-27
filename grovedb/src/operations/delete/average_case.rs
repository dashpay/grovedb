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

//! Average case delete cost

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    estimated_costs::{
        average_case_costs::EstimatedLayerInformation,
        worst_case_costs::add_average_case_cost_for_is_empty_tree_except,
    },
    HASH_LENGTH_U32,
};
use grovedb_storage::{worst_case_costs::WorstKeyLength, Storage};
use intmap::IntMap;

use crate::{
    batch::{key_info::KeyInfo, GroveDbOp, KeyInfoPath},
    Error, GroveDb,
};

/// 0 represents key size, 1 represents element size
type EstimatedKeyAndElementSize = (u32, u32);

impl GroveDb {
    /// Average case delete operations for delete up tree while empty
    // todo finish this
    pub fn average_case_delete_operations_for_delete_up_tree_while_empty<'db, S: Storage<'db>>(
        path: &KeyInfoPath,
        key: &KeyInfo,
        stop_path_height: Option<u16>,
        validate: bool,
        estimated_layer_info: IntMap<EstimatedLayerInformation>,
    ) -> CostResult<Vec<GroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        let stop_path_height = stop_path_height.unwrap_or_default();

        if (path.len() as u16) < stop_path_height {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidParameter(
                "path length need to be greater or equal to stop path height",
            ))
            .wrap_with_cost(cost)
        } else {
            let mut used_path = path.0.as_slice();
            let mut ops = vec![];
            let path_len = path.len() as u16;
            for height in (stop_path_height..path_len).rev() {
                let (
                    path_at_level,
                    key_at_level,
                    check_if_tree,
                    except_keys_count,
                    key_len,
                    estimated_element_size,
                    is_sum_tree,
                ) = cost_return_on_error_no_add!(
                    &cost,
                    if height == path_len - 1 {
                        if let Some(layer_info) = estimated_layer_info.get(height as u64) {
                            let estimated_value_len = cost_return_on_error_no_add!(
                                &cost,
                                layer_info
                                    .estimated_layer_sizes
                                    .value_with_feature_and_flags_size()
                                    .map_err(Error::MerkError)
                            );
                            Ok((
                                used_path,
                                key,
                                true,
                                0,
                                key.max_length() as u32,
                                estimated_value_len,
                                layer_info.is_sum_tree,
                            ))
                        } else {
                            Err(Error::InvalidParameter(
                                "intermediate flag size missing for height at path length",
                            ))
                        }
                    } else {
                        let (last_key, smaller_path) = used_path.split_last().unwrap();
                        used_path = smaller_path;
                        if let Some(layer_info) = estimated_layer_info.get(height as u64) {
                            let estimated_value_len = cost_return_on_error_no_add!(
                                &cost,
                                layer_info
                                    .estimated_layer_sizes
                                    .subtree_with_feature_and_flags_size()
                                    .map_err(Error::MerkError)
                            );
                            Ok((
                                used_path,
                                last_key,
                                false,
                                1,
                                last_key.max_length() as u32,
                                estimated_value_len,
                                layer_info.is_sum_tree,
                            ))
                        } else {
                            Err(Error::InvalidParameter("intermediate layer info missing"))
                        }
                    }
                );
                let op = cost_return_on_error!(
                    &mut cost,
                    Self::average_case_delete_operation_for_delete_internal::<S>(
                        &KeyInfoPath::from_vec(path_at_level.to_vec()),
                        key_at_level,
                        is_sum_tree,
                        validate,
                        check_if_tree,
                        except_keys_count,
                        (key_len, estimated_element_size)
                    )
                );
                ops.push(op);
            }
            Ok(ops).wrap_with_cost(cost)
        }
    }

    /// Average case delete operation for delete internal
    pub fn average_case_delete_operation_for_delete_internal<'db, S: Storage<'db>>(
        path: &KeyInfoPath,
        key: &KeyInfo,
        parent_tree_is_sum_tree: bool,
        validate: bool,
        check_if_tree: bool,
        except_keys_count: u16,
        estimated_key_element_size: EstimatedKeyAndElementSize,
    ) -> CostResult<GroveDbOp, Error> {
        let mut cost = OperationCost::default();

        if validate {
            GroveDb::add_average_case_get_merk_at_path::<S>(
                &mut cost,
                path,
                false,
                parent_tree_is_sum_tree,
            );
        }
        if check_if_tree {
            GroveDb::add_average_case_get_raw_cost::<S>(
                &mut cost,
                path,
                key,
                estimated_key_element_size.1,
                parent_tree_is_sum_tree,
            );
        }
        // in the worst case this is a tree
        add_average_case_cost_for_is_empty_tree_except(
            &mut cost,
            except_keys_count,
            estimated_key_element_size.0 + HASH_LENGTH_U32,
        );

        Ok(GroveDbOp::delete_estimated_op(path.clone(), key.clone())).wrap_with_cost(cost)
    }
}
