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

//! Average case costs
//! Implements average case cost functions in GroveDb

use grovedb_costs::{cost_return_on_error_no_add, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::{
        add_cost_case_merk_insert, add_cost_case_merk_insert_layered, add_cost_case_merk_patch,
        add_cost_case_merk_replace_layered, add_cost_case_merk_replace_same_size,
        average_case_costs::{
            add_average_case_get_merk_node, add_average_case_merk_delete,
            add_average_case_merk_delete_layered, add_average_case_merk_propagate,
            add_average_case_merk_replace_layered, EstimatedLayerInformation,
        },
    },
    tree::TreeNode,
    HASH_LENGTH,
};
use grovedb_storage::{worst_case_costs::WorstKeyLength, Storage};
use integer_encoding::VarInt;

use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    element::{SUM_ITEM_COST_SIZE, SUM_TREE_COST_SIZE, TREE_COST_SIZE},
    Element, ElementFlags, Error, GroveDb,
};

impl GroveDb {
    /// Add average case for getting a merk tree
    pub fn add_average_case_get_merk_at_path<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        merk_should_be_empty: bool,
        is_sum_tree: bool,
    ) {
        cost.seek_count += 1;
        // If the merk is not empty we load the tree
        if !merk_should_be_empty {
            cost.seek_count += 1;
        }
        match path.last() {
            None => {}
            Some(key) => {
                cost.storage_loaded_bytes += TreeNode::average_case_encoded_tree_size(
                    key.max_length() as u32,
                    HASH_LENGTH as u32,
                    is_sum_tree,
                );
            }
        }
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    /// Add average case for insertion into merk
    pub(crate) fn average_case_merk_replace_tree(
        key: &KeyInfo,
        estimated_layer_information: &EstimatedLayerInformation,
        _is_sum_tree: bool,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_size = cost_return_on_error_no_add!(
            &cost,
            estimated_layer_information
                .estimated_layer_sizes
                .layered_flags_size()
                .map_err(Error::MerkError)
        )
        .map(|f| f + f.required_space() as u32)
        .unwrap_or_default();
        let tree_cost_size = if estimated_layer_information.is_sum_tree {
            SUM_TREE_COST_SIZE
        } else {
            TREE_COST_SIZE
        };
        let layer_extra_size = tree_cost_size + flags_size;
        add_average_case_merk_replace_layered(
            &mut cost,
            key_len,
            layer_extra_size,
            estimated_layer_information.is_sum_tree,
        );
        if propagate {
            add_average_case_merk_propagate(&mut cost, estimated_layer_information)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for insertion into merk
    pub fn average_case_merk_insert_tree(
        key: &KeyInfo,
        flags: &Option<ElementFlags>,
        is_sum_tree: bool,
        in_tree_using_sums: bool,
        propagate_if_input: Option<&EstimatedLayerInformation>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_len = flags.as_ref().map_or(0, |flags| {
            let flags_len = flags.len() as u32;
            flags_len + flags_len.required_space() as u32
        });
        let tree_cost_size = if is_sum_tree {
            SUM_TREE_COST_SIZE
        } else {
            TREE_COST_SIZE
        };
        let value_len = tree_cost_size + flags_len;
        add_cost_case_merk_insert_layered(&mut cost, key_len, value_len, in_tree_using_sums);
        if let Some(input) = propagate_if_input {
            add_average_case_merk_propagate(&mut cost, input).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for insertion into merk
    pub fn average_case_merk_delete_tree(
        key: &KeyInfo,
        is_sum_tree: bool,
        estimated_layer_information: &EstimatedLayerInformation,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_size = cost_return_on_error_no_add!(
            &cost,
            estimated_layer_information
                .estimated_layer_sizes
                .layered_flags_size()
                .map_err(Error::MerkError)
        )
        .map(|f| f + f.required_space() as u32)
        .unwrap_or_default();
        let tree_cost_size = if is_sum_tree {
            SUM_TREE_COST_SIZE
        } else {
            TREE_COST_SIZE
        };
        let layer_extra_size = tree_cost_size + flags_size;
        add_average_case_merk_delete_layered(&mut cost, key_len, layer_extra_size);
        if propagate {
            add_average_case_merk_propagate(&mut cost, estimated_layer_information)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for insertion into merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn average_case_merk_insert_element(
        key: &KeyInfo,
        value: &Element,
        in_tree_using_sums: bool,
        propagate_for_level: Option<&EstimatedLayerInformation>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        match value {
            Element::Tree(_, flags) | Element::SumTree(_, _, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let tree_cost_size = if value.is_sum_tree() {
                    SUM_TREE_COST_SIZE
                } else {
                    TREE_COST_SIZE
                };
                let value_len = tree_cost_size + flags_len;
                add_cost_case_merk_insert_layered(&mut cost, key_len, value_len, in_tree_using_sums)
            }
            _ => add_cost_case_merk_insert(
                &mut cost,
                key_len,
                value.serialized_size() as u32,
                in_tree_using_sums,
            ),
        };
        if let Some(level) = propagate_for_level {
            add_average_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for replacement into merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn average_case_merk_replace_element(
        key: &KeyInfo,
        value: &Element,
        in_tree_using_sums: bool,
        propagate_for_level: Option<&EstimatedLayerInformation>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        match value {
            Element::Tree(_, flags) | Element::SumTree(_, _, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let tree_cost_size = if value.is_sum_tree() {
                    SUM_TREE_COST_SIZE
                } else {
                    TREE_COST_SIZE
                };
                let value_len = tree_cost_size + flags_len;
                add_cost_case_merk_replace_layered(
                    &mut cost,
                    key_len,
                    value_len,
                    in_tree_using_sums,
                )
            }
            Element::Item(_, flags) | Element::SumItem(_, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                // Items need to be always the same serialized size for this to work
                let sum_item_cost_size = if value.is_sum_item() {
                    SUM_ITEM_COST_SIZE
                } else {
                    value.serialized_size() as u32
                };
                let value_len = sum_item_cost_size + flags_len;
                add_cost_case_merk_replace_same_size(
                    &mut cost,
                    key_len,
                    value_len,
                    in_tree_using_sums,
                )
            }
            _ => add_cost_case_merk_replace_same_size(
                &mut cost,
                key_len,
                value.serialized_size() as u32,
                in_tree_using_sums,
            ),
        };
        if let Some(level) = propagate_for_level {
            add_average_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for patching an element in merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn average_case_merk_patch_element(
        key: &KeyInfo,
        value: &Element,
        change_in_bytes: i32,
        in_tree_using_sums: bool,
        propagate_for_level: Option<&EstimatedLayerInformation>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        match value {
            Element::Item(_, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                // Items need to be always the same serialized size for this to work
                let item_cost_size = value.serialized_size() as u32;
                let value_len = item_cost_size + flags_len;
                add_cost_case_merk_patch(
                    &mut cost,
                    key_len,
                    value_len,
                    change_in_bytes,
                    in_tree_using_sums,
                )
            }
            _ => {
                return Err(Error::InvalidParameter("patching can only be on Items"))
                    .wrap_with_cost(cost)
            }
        };
        if let Some(level) = propagate_for_level {
            add_average_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for deletion into Merk
    pub fn average_case_merk_delete_element(
        key: &KeyInfo,
        estimated_layer_information: &EstimatedLayerInformation,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let value_size = cost_return_on_error_no_add!(
            &cost,
            estimated_layer_information
                .estimated_layer_sizes
                .value_with_feature_and_flags_size()
                .map_err(Error::MerkError)
        );
        add_average_case_merk_delete(&mut cost, key_len, value_size);
        if propagate {
            add_average_case_merk_propagate(&mut cost, estimated_layer_information)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Adds the average case of checking to see if a raw value exists
    pub fn add_average_case_has_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) {
        let value_size = TreeNode::average_case_encoded_tree_size(
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
        cost.seek_count += 1;
        cost.storage_loaded_bytes += value_size;
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    /// Adds the average case of checking to see if a tree exists
    pub fn add_average_case_has_raw_tree_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
    ) {
        let estimated_element_size = if is_sum_tree {
            SUM_TREE_COST_SIZE + estimated_flags_size
        } else {
            TREE_COST_SIZE + estimated_flags_size
        };
        Self::add_average_case_has_raw_cost::<S>(
            cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
    }

    /// Add average case to get raw cost into merk
    pub fn add_average_case_get_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) {
        cost.seek_count += 1;
        add_average_case_get_merk_node(
            cost,
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
    }

    /// adds the average cost of getting a tree
    pub fn add_average_case_get_raw_tree_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
    ) {
        let estimated_element_size = if is_sum_tree {
            SUM_TREE_COST_SIZE + estimated_flags_size
        } else {
            TREE_COST_SIZE + estimated_flags_size
        };
        cost.seek_count += 1;
        add_average_case_get_merk_node(
            cost,
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
    }

    /// adds the average cost of getting an element knowing there can be
    /// intermediate references
    pub fn add_average_case_get_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        in_parent_tree_using_sums: bool,
        estimated_element_size: u32,
        estimated_references_sizes: Vec<u32>,
    ) {
        // todo: verify
        let value_size: u32 = TreeNode::average_case_encoded_tree_size(
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
        cost.seek_count += 1 + estimated_references_sizes.len() as u16;
        cost.storage_loaded_bytes += value_size + estimated_references_sizes.iter().sum::<u32>();
        *cost += S::get_storage_context_cost(path.as_vec());
    }
}

#[cfg(test)]
mod test {
    use std::option::Option::None;

    use grovedb_costs::OperationCost;
    use grovedb_merk::{
        estimated_costs::average_case_costs::add_average_case_get_merk_node,
        test_utils::make_batch_seq, tree::kv::ValueDefinedCostType, Merk,
    };
    use grovedb_storage::{
        rocksdb_storage::RocksDbStorage, worst_case_costs::WorstKeyLength, Storage, StorageBatch,
    };
    use tempfile::TempDir;

    use crate::{
        batch::{key_info::KeyInfo::KnownKey, KeyInfoPath},
        tests::{common::EMPTY_PATH, TEST_LEAF},
        Element, GroveDb,
    };

    #[test]
    fn test_get_merk_node_average_case() {
        // Open a merk and insert 10 elements.
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let batch = StorageBatch::new();

        let mut merk = Merk::open_base(
            storage
                .get_storage_context(EMPTY_PATH, Some(&batch))
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");
        let merk_batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(merk_batch.as_slice(), &[], None)
            .unwrap()
            .unwrap();

        // this consumes the batch so storage contexts and merks will be dropped
        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .unwrap();

        // Reopen merk: this time, only root node is loaded to memory
        let merk = Merk::open_base(
            storage.get_storage_context(EMPTY_PATH, None).unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");

        // To simulate average case, we need to pick a node that:
        // 1. Is not in memory
        // 2. Left link exists
        // 3. Right link exists
        // Based on merk's avl rotation algorithm node is key 8 satisfies this
        let node_result = merk.get(
            &8_u64.to_be_bytes(),
            true,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        );

        // By tweaking the max element size, we can adapt the average case function to
        // this scenario. make_batch_seq creates values that are 60 bytes in size
        // (this will be the max_element_size)
        let mut cost = OperationCost::default();
        let key = KnownKey(8_u64.to_be_bytes().to_vec());
        add_average_case_get_merk_node(&mut cost, key.max_length() as u32, 60, false);
        assert_eq!(cost, node_result.cost);
    }

    #[test]
    fn test_has_raw_average_case() {
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // insert empty tree to start
        db.insert(EMPTY_PATH, TEST_LEAF, Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful root tree leaf insert");

        // In this tree, we insert 3 items with keys [1, 2, 3]
        // after tree rotation, 2 will be at the top hence would have both left and
        // right links this will serve as our average case candidate.
        let elem = Element::new_item(b"value".to_vec());
        db.insert([TEST_LEAF].as_ref(), &[1], elem.clone(), None, None)
            .unwrap()
            .expect("expected insert");
        db.insert([TEST_LEAF].as_ref(), &[2], elem.clone(), None, None)
            .unwrap()
            .expect("expected insert");
        db.insert([TEST_LEAF].as_ref(), &[3], elem.clone(), None, None)
            .unwrap()
            .expect("expected insert");

        let path = KeyInfoPath::from_vec(vec![KnownKey(TEST_LEAF.to_vec())]);
        let key = KnownKey(vec![1]);
        let mut average_case_has_raw_cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
            &mut average_case_has_raw_cost,
            &path,
            &key,
            elem.serialized_size() as u32,
            false,
        );

        let actual_cost = db.has_raw([TEST_LEAF].as_ref(), &[2], None);

        assert_eq!(average_case_has_raw_cost, actual_cost.cost);
    }
}
