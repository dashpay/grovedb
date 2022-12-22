use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use intmap::IntMap;
use merk::{
    estimated_costs::worst_case_costs::add_worst_case_cost_for_is_empty_tree_except, tree::kv::KV,
};
use storage::{worst_case_costs::WorstKeyLength, Storage};

use crate::{
    batch::{key_info::KeyInfo, GroveDbOp, KeyInfoPath},
    element::SUM_TREE_COST_SIZE,
    Error, GroveDb,
};

#[cfg(feature = "full")]
impl GroveDb {
    pub fn worst_case_delete_operations_for_delete_up_tree_while_empty<'db, S: Storage<'db>>(
        path: &KeyInfoPath,
        key: &KeyInfo,
        stop_path_height: Option<u16>,
        validate: bool,
        intermediate_tree_info: IntMap<(bool, u32)>,
        max_element_size: u32,
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
            for height in (stop_path_height..(path_len as u16)).rev() {
                let (
                    path_at_level,
                    key_at_level,
                    check_if_tree,
                    except_keys_count,
                    max_element_size,
                    is_sum_tree,
                ) = cost_return_on_error_no_add!(
                    &cost,
                    if height == path_len {
                        if let Some((is_in_sum_tree, _)) = intermediate_tree_info.get(height as u64)
                        {
                            Ok((used_path, key, true, 0, max_element_size, *is_in_sum_tree))
                        } else {
                            Err(Error::InvalidParameter(
                                "intermediate flag size missing for height at path length",
                            ))
                        }
                    } else {
                        let (last_key, smaller_path) = used_path.split_last().unwrap();
                        used_path = smaller_path;
                        if let Some((is_in_sum_tree, flags_size_at_level)) =
                            intermediate_tree_info.get(height as u64)
                        {
                            // the worst case is that we are only in sum trees
                            let value_len = SUM_TREE_COST_SIZE + flags_size_at_level;
                            let max_tree_size =
                                KV::layered_node_byte_cost_size_for_key_and_value_lengths(
                                    last_key.max_length() as u32,
                                    value_len,
                                    *is_in_sum_tree,
                                );
                            Ok((
                                used_path,
                                last_key,
                                false,
                                1,
                                max_tree_size,
                                *is_in_sum_tree,
                            ))
                        } else {
                            Err(Error::InvalidParameter("intermediate flag size missing"))
                        }
                    }
                );
                let op = cost_return_on_error!(
                    &mut cost,
                    Self::worst_case_delete_operation_for_delete_internal::<S>(
                        &KeyInfoPath::from_vec(path_at_level.to_vec()),
                        key_at_level,
                        is_sum_tree,
                        validate,
                        check_if_tree,
                        except_keys_count,
                        max_element_size
                    )
                );
                ops.push(op);
            }
            Ok(ops).wrap_with_cost(cost)
        }
    }

    pub fn worst_case_delete_operation_for_delete_internal<'db, S: Storage<'db>>(
        path: &KeyInfoPath,
        key: &KeyInfo,
        parent_tree_is_sum_tree: bool,
        validate: bool,
        check_if_tree: bool,
        except_keys_count: u16,
        max_element_size: u32,
    ) -> CostResult<GroveDbOp, Error> {
        let mut cost = OperationCost::default();

        if validate {
            GroveDb::add_worst_case_get_merk_at_path::<S>(&mut cost, path, parent_tree_is_sum_tree);
        }
        if check_if_tree {
            GroveDb::add_worst_case_get_raw_cost::<S>(
                &mut cost,
                path,
                key,
                max_element_size,
                parent_tree_is_sum_tree,
            );
        }
        // in the worst case this is a tree
        add_worst_case_cost_for_is_empty_tree_except(&mut cost, except_keys_count);

        Ok(GroveDbOp::delete_estimated_op(path.clone(), key.clone())).wrap_with_cost(cost)
    }
}
