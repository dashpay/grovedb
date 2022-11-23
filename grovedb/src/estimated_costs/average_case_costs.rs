use costs::{CostResult, CostsExt, OperationCost};
use integer_encoding::VarInt;
use merk::{
    tree::Tree,
    estimated_costs::average_case_costs::{
        add_average_case_get_merk_node, add_average_case_merk_insert,
        add_average_case_merk_insert_layered, add_average_case_merk_propagate,
        add_average_case_merk_replace_layered, MerkAverageCaseInput,
    },
    HASH_LENGTH,
};
use storage::{worst_case_costs::WorstKeyLength, Storage};

use crate::{Error, GroveDb};
use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    subtree::TREE_COST_SIZE,
    Element, ElementFlags,
};

impl GroveDb {
    /// Add worst case for getting a merk tree
    pub fn add_average_case_get_merk_at_path<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
    ) {
        cost.seek_count += 2; // seek in meta for root key + loading that root key
        match path.last() {
            None => {}
            Some(key) => {
                cost.storage_loaded_bytes +=
                    Tree::average_case_encoded_tree_size(key.len() as u32, HASH_LENGTH as u32);
            }
        }
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    /// Add worst case for insertion into merk
    pub(crate) fn average_case_merk_replace_tree(
        key: &KeyInfo,
        propagate_if_input: Option<MerkAverageCaseInput>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.len() as u32;
        add_average_case_merk_replace_layered(&mut cost, key_len, 3);
        if let Some(input) = propagate_if_input {
            add_average_case_merk_propagate(&mut cost, input).map_err(Error::MerkError)
        } else {
            Ok(())
        }.wrap_with_cost(cost)
    }

    /// Add worst case for insertion into merk
    pub(crate) fn average_case_merk_insert_tree(
        key: &KeyInfo,
        flags: &Option<ElementFlags>,
        propagate_if_input: Option<MerkAverageCaseInput>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.len() as u32;
        let flags_len = flags.as_ref().map_or(0, |flags| {
            let flags_len = flags.len() as u32;
            flags_len + flags_len.required_space() as u32
        });
        let value_len = TREE_COST_SIZE + flags_len;
        add_average_case_merk_insert_layered(&mut cost, key_len, value_len);
        if let Some(input) = propagate_if_input {
            add_average_case_merk_propagate(&mut cost, input).map_err(Error::MerkError)
        } else {
            Ok(())
        }.wrap_with_cost(cost)
    }

    /// Add worst case for insertion into merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub(crate) fn average_case_merk_insert_element(
        key: &KeyInfo,
        value: &Element,
        propagate_for_level: Option<MerkAverageCaseInput>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.len() as u32;
        match value {
            Element::Tree(_, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = TREE_COST_SIZE + flags_len;
                add_average_case_merk_insert_layered(&mut cost, key_len, value_len)
            }
            _ => add_average_case_merk_insert(&mut cost, key_len, value.serialized_size() as u32),
        };
        if let Some(level) = propagate_for_level {
            add_average_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }.wrap_with_cost(cost)
    }

    pub fn average_case_delete_cost(
        key: &KeyInfo,
        estimated_element_size: u32,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        Ok(()).wrap_with_cost(cost)
    }

    pub fn add_average_case_has_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
    ) {
        let value_size = Tree::average_case_encoded_tree_size(key.len() as u32, estimated_element_size);
        cost.seek_count += 1;
        cost.storage_loaded_bytes += value_size;
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    pub fn add_average_case_get_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
    ) {
        // todo: verify, we need to run a test to see if has raw has any better
        // performance than get raw
        Self::add_average_case_get_merk_at_path::<S>(cost, path);
        add_average_case_get_merk_node(cost, key.len() as u32, max_element_size);
    }

    pub fn add_average_case_get_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        max_references_sizes: Vec<u32>,
    ) {
        // todo: verify
        let value_size: u32 =
            Tree::average_case_encoded_tree_size(key.len() as u32, max_element_size);
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
        test_utils::make_batch_seq, estimated_costs::average_case_costs::add_average_case_get_merk_node, Merk,
    };
    use storage::{rocksdb_storage::RocksDbStorage, worst_case_costs::WorstKeyLength, Storage};
    use tempfile::TempDir;

    use crate::{
        batch::{
            key_info::KeyInfo::{KnownKey},
            KeyInfoPath,
        },
        tests::TEST_LEAF,
        Element, GroveDb,
    };

    #[test]
    fn test_get_merk_node_average_case() {
        // Open a merk and insert 10 elements.
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");
        let batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
            .unwrap()
            .unwrap();

        // drop merk, so nothing is stored in memory
        drop(merk);

        // Reopen merk: this time, only root node is loaded to memory
        let merk = Merk::open_base(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");

        // To simulate worst case, we need to pick a node that:
        // 1. Is not in memory
        // 2. Left link exists
        // 3. Right link exists
        // Based on merk's avl rotation algorithm node is key 8 satisfies this
        let node_result = merk.get(&8_u64.to_be_bytes());

        // By tweaking the max element size, we can adapt the worst case function to
        // this scenario. make_batch_seq creates values that are 60 bytes in size
        // (this will be the max_element_size)
        let mut cost = OperationCost::default();
        let key = KnownKey(8_u64.to_be_bytes().to_vec());
        add_average_case_get_merk_node(&mut cost, key.len() as u32, 60);
        assert_eq!(cost, node_result.cost);
    }

    #[test]
    fn test_has_raw_average_case() {
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
        let mut average_case_has_raw_cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
            &mut average_case_has_raw_cost,
            &path,
            &key,
            elem.serialized_size() as u32,
        );

        let actual_cost = db.has_raw([TEST_LEAF], &[2], None);

        assert_eq!(average_case_has_raw_cost, actual_cost.cost);
    }
}
