use costs::{CostResult, CostsExt, OperationCost};
use integer_encoding::VarInt;
use merk::{
    estimated_costs::{
        add_cost_case_merk_insert, add_cost_case_merk_insert_layered, add_cost_case_merk_replace,
        add_cost_case_merk_replace_layered, add_cost_case_merk_replace_same_size,
        worst_case_costs::{
            add_worst_case_get_merk_node, add_worst_case_merk_delete,
            add_worst_case_merk_delete_layered, add_worst_case_merk_propagate,
            add_worst_case_merk_replace_layered, WorstCaseLayerInformation,
            MERK_BIGGEST_VALUE_SIZE,
        },
    },
    tree::Tree,
    HASH_LENGTH,
};
use storage::{worst_case_costs::WorstKeyLength, Storage};

use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    element::{SUM_ITEM_COST_SIZE, SUM_TREE_COST_SIZE, TREE_COST_SIZE},
    Element, ElementFlags, Error, GroveDb,
};

pub const WORST_CASE_FLAGS_LEN: u32 = 16386; // 2 bytes to represent this number for varint

impl GroveDb {
    /// Add worst case for getting a merk tree
    pub fn add_worst_case_get_merk_at_path<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        is_sum_tree: bool,
    ) {
        cost.seek_count += 2;
        match path.last() {
            None => {}
            Some(key) => {
                cost.storage_loaded_bytes += Tree::worst_case_encoded_tree_size(
                    key.max_length() as u32,
                    HASH_LENGTH as u32,
                    is_sum_tree,
                );
            }
        }
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    /// Add worst case for insertion into merk
    pub(crate) fn worst_case_merk_replace_tree(
        key: &KeyInfo,
        is_sum_tree: bool,
        is_in_parent_sum_tree: bool,
        worst_case_layer_information: &WorstCaseLayerInformation,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let tree_cost = if is_sum_tree {
            SUM_TREE_COST_SIZE
        } else {
            TREE_COST_SIZE
        };
        let layer_extra_size = tree_cost + WORST_CASE_FLAGS_LEN;
        add_worst_case_merk_replace_layered(
            &mut cost,
            key_len,
            layer_extra_size,
            is_in_parent_sum_tree,
        );
        if propagate {
            add_worst_case_merk_propagate(&mut cost, worst_case_layer_information)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add worst case for insertion into merk
    pub fn worst_case_merk_insert_tree(
        key: &KeyInfo,
        flags: &Option<ElementFlags>,
        is_sum_tree: bool,
        is_in_parent_sum_tree: bool,
        propagate_if_input: Option<&WorstCaseLayerInformation>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_len = flags.as_ref().map_or(0, |flags| {
            let flags_len = flags.len() as u32;
            flags_len + flags_len.required_space() as u32
        });
        let tree_cost = if is_sum_tree {
            SUM_TREE_COST_SIZE
        } else {
            TREE_COST_SIZE
        };
        let value_len = tree_cost + flags_len;
        add_cost_case_merk_insert_layered(&mut cost, key_len, value_len, is_in_parent_sum_tree);
        if let Some(input) = propagate_if_input {
            add_worst_case_merk_propagate(&mut cost, input).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add worst case for insertion into merk
    pub fn worst_case_merk_delete_tree(
        key: &KeyInfo,
        is_sum_tree: bool,
        worst_case_layer_information: &WorstCaseLayerInformation,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let tree_cost = if is_sum_tree {
            SUM_TREE_COST_SIZE
        } else {
            TREE_COST_SIZE
        };
        let layer_extra_size = tree_cost + WORST_CASE_FLAGS_LEN;
        add_worst_case_merk_delete_layered(&mut cost, key_len, layer_extra_size);
        if propagate {
            add_worst_case_merk_propagate(&mut cost, worst_case_layer_information)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add worst case for insertion into merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn worst_case_merk_insert_element(
        key: &KeyInfo,
        value: &Element,
        in_parent_tree_using_sums: bool,
        propagate_for_level: Option<&WorstCaseLayerInformation>,
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
                add_cost_case_merk_insert_layered(
                    &mut cost,
                    key_len,
                    value_len,
                    in_parent_tree_using_sums,
                )
            }
            _ => add_cost_case_merk_insert(
                &mut cost,
                key_len,
                value.serialized_size() as u32,
                in_parent_tree_using_sums,
            ),
        };
        if let Some(level) = propagate_for_level {
            add_worst_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add worst case for replacement in merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn worst_case_merk_replace_element(
        key: &KeyInfo,
        value: &Element,
        in_parent_tree_using_sums: bool,
        propagate_for_level: Option<&WorstCaseLayerInformation>,
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
                    in_parent_tree_using_sums,
                )
            }
            Element::SumItem(_, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = SUM_ITEM_COST_SIZE + flags_len;
                add_cost_case_merk_replace_same_size(
                    &mut cost,
                    key_len,
                    value_len,
                    in_parent_tree_using_sums,
                )
            }
            _ => add_cost_case_merk_replace(
                &mut cost,
                key_len,
                value.serialized_size() as u32,
                in_parent_tree_using_sums,
            ),
        };
        if let Some(level) = propagate_for_level {
            add_worst_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    pub fn worst_case_merk_delete_element(
        key: &KeyInfo,
        worst_case_layer_information: &WorstCaseLayerInformation,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        add_worst_case_merk_delete(&mut cost, key_len, MERK_BIGGEST_VALUE_SIZE);
        if propagate {
            add_worst_case_merk_propagate(&mut cost, worst_case_layer_information)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    pub fn add_worst_case_has_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) {
        let value_size = Tree::worst_case_encoded_tree_size(
            key.max_length() as u32,
            max_element_size,
            in_parent_tree_using_sums,
        );
        cost.seek_count += 1;
        cost.storage_loaded_bytes += value_size;
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    pub fn add_worst_case_get_raw_tree_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
    ) {
        cost.seek_count += 1;
        let tree_cost_size = if is_sum_tree {
            SUM_TREE_COST_SIZE
        } else {
            TREE_COST_SIZE
        };
        add_worst_case_get_merk_node(
            cost,
            key.max_length() as u32,
            tree_cost_size,
            in_parent_tree_using_sums,
        );
    }

    pub fn add_worst_case_get_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) {
        cost.seek_count += 1;
        add_worst_case_get_merk_node(
            cost,
            key.max_length() as u32,
            max_element_size,
            in_parent_tree_using_sums,
        );
    }

    pub fn add_worst_case_get_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
        max_references_sizes: Vec<u32>,
    ) {
        // todo: verify
        let value_size: u32 = Tree::worst_case_encoded_tree_size(
            key.max_length() as u32,
            max_element_size,
            in_parent_tree_using_sums,
        );
        cost.seek_count += 1 + max_references_sizes.len() as u16;
        cost.storage_loaded_bytes += value_size + max_references_sizes.iter().sum::<u32>();
        *cost += S::get_storage_context_cost(path.as_vec());
    }
}

#[cfg(test)]
mod test {
    use std::{iter::empty, option::Option::None};

    use costs::OperationCost;
    use merk::{
        estimated_costs::worst_case_costs::add_worst_case_get_merk_node,
        test_utils::make_batch_seq, Merk,
    };
    use storage::{rocksdb_storage::RocksDbStorage, worst_case_costs::WorstKeyLength, Storage};
    use tempfile::TempDir;

    use crate::{
        batch::{key_info::KeyInfo::KnownKey, KeyInfoPath},
        tests::TEST_LEAF,
        Element, GroveDb,
    };

    #[test]
    fn test_get_merk_node_worst_case() {
        // Open a merk and insert 10 elements.
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
            .unwrap()
            .expect("cannot open merk");
        let batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
            .unwrap()
            .unwrap();

        // drop merk, so nothing is stored in memory
        drop(merk);

        // Reopen merk: this time, only root node is loaded to memory
        let merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
            .unwrap()
            .expect("cannot open merk");

        // To simulate worst case, we need to pick a node that:
        // 1. Is not in memory
        // 2. Left link exists
        // 3. Right link exists
        // Based on merk's avl rotation algorithm node is key 8 satisfies this
        let node_result = merk.get(&8_u64.to_be_bytes(), true);

        // By tweaking the max element size, we can adapt the worst case function to
        // this scenario. make_batch_seq creates values that are 60 bytes in size
        // (this will be the max_element_size)
        let mut cost = OperationCost::default();
        let key = KnownKey(8_u64.to_be_bytes().to_vec());
        add_worst_case_get_merk_node(&mut cost, key.max_length() as u32, 60, false);
        assert_eq!(cost, node_result.cost);
    }

    #[test]
    fn test_has_raw_worst_case() {
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // insert empty tree to start
        db.insert([], TEST_LEAF, Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful root tree leaf insert");

        // In this tree, we insert 3 items with keys [1, 2, 3]
        // after tree rotation, 2 will be at the top hence would have both left and
        // right links this will serve as our worst case candidate.
        let elem = Element::new_item(b"value".to_vec());
        db.insert([TEST_LEAF], &[1], elem.clone(), None, None)
            .unwrap()
            .expect("expected insert");
        db.insert([TEST_LEAF], &[2], elem.clone(), None, None)
            .unwrap()
            .expect("expected insert");
        db.insert([TEST_LEAF], &[3], elem.clone(), None, None)
            .unwrap()
            .expect("expected insert");

        let path = KeyInfoPath::from_vec(vec![KnownKey(TEST_LEAF.to_vec())]);
        let key = KnownKey(vec![1]);
        let mut worst_case_has_raw_cost = OperationCost::default();
        GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
            &mut worst_case_has_raw_cost,
            &path,
            &key,
            elem.serialized_size() as u32,
            false,
        );

        let actual_cost = db.has_raw([TEST_LEAF], &[2], None);

        assert_eq!(worst_case_has_raw_cost, actual_cost.cost);
    }
}
