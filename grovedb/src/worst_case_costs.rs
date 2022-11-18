use costs::OperationCost;
use integer_encoding::VarInt;
use merk::{
    tree::Tree,
    worst_case_costs::{
        add_worst_case_get_merk_node, add_worst_case_merk_insert,
        add_worst_case_merk_insert_layered, add_worst_case_merk_propagate,
        add_worst_case_merk_replace_layered, MerkWorstCaseInput,
    },
    HASH_LENGTH,
};
use storage::{worst_case_costs::WorstKeyLength, Storage};

use super::GroveDb;
use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    subtree::TREE_COST_SIZE,
    Element, ElementFlags,
};

impl GroveDb {
    /// Add worst case for getting a merk tree
    pub fn add_worst_case_get_merk_at_path<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
    ) {
        cost.seek_count += 2; // seek in meta for root key + loading that root key
        match path.last() {
            None => {}
            Some(key) => {
                cost.storage_loaded_bytes +=
                    Tree::worst_case_encoded_tree_size(key.len() as u32, HASH_LENGTH as u32);
            }
        }
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    #[allow(dead_code)] // TODO
    pub(crate) fn add_merk_worst_case_insert_reference(
        cost: &mut OperationCost,
        max_element_size: u32,
        max_element_number: u32,
        key: &KeyInfo,
    ) {
        // same as insert node but one less hash node call as that is done on the
        // grovedb layer
        Self::add_worst_case_insert_merk_node(cost, max_element_size, max_element_number, key);
        cost.hash_node_calls -= 1;
    }

    pub(crate) fn add_worst_case_insert_merk_node(
        cost: &mut OperationCost,
        max_element_size: u32,
        max_element_number: u32,
        key: &KeyInfo,
    ) {
        // Insertion Process
        // - Walk from the root to insertion point (marking every node in path as
        //   modified)
        // - Insert new node (also mark as modified)
        // - Perform rotation
        //      - Worst case scenario, we rotate into an already occupied slot
        //      - In that case, we need to fetch that node from backing store to move it
        //        (also marked as modified)
        // - Lastly we commit
        //      - Write all modified nodes to the backing store.

        // For worst case conditions, the merk tree must have just been opened. This way
        // each walk will need us to get to the backing store.

        // Root is already loaded on merk open, so initial walk to insertion point only
        // loads nodes after root from backing store.
        // Here we calculate the number of walks we would have to perform.

        // We reduce by 1 because we are given the max element number, you can't add
        // after max so worst case has to be 1 element away from max
        let worst_case_element_number = max_element_number - 1;
        // With this we can calculate the maximum tree height root inclusive
        let max_tree_height = (1.44 * (worst_case_element_number as f32).log2()).floor() as u32;
        // max_number_of_walks is basically max_tree_height - 1 (because we exclude the
        // root)
        let max_number_of_walks = max_tree_height - 1;

        // for each walk, we have to seek and load from storage (equivalent to a
        // get)
        for _ in 0..max_number_of_walks {
            add_worst_case_get_merk_node(cost, key.len() as u32, max_element_size)
        }

        // after getting to the point of insertion, we need to build the node
        // this requires computing the value_hash and kv_hash
        cost.hash_node_calls += 2;

        // dbg!(&cost);
        // dbg!(&cost.storage_loaded_bytes - 4 *
        // Self::worst_case_encoded_link_size(max_key_size)); dbg!(Self::
        // worst_case_encoded_link_size(max_key_size)); dbg!(Self::
        // worst_case_encoded_kv_node_size(max_element_size));

        // After insertion, we need to balance the tree
        // ordinarily balancing a tree has no extra cost, as it just moves around
        // already modified nodes, but there are times when we try to rotate
        // into an already occupied slot. In such cases, we need to fetch what
        // is currently in that slot (from the backing store) remove it, rotate
        // and then reattach to rotated node. Merk marks every attached node as
        // modified.

        // TODO: This might be 2 look into this.
        // Add 1 get cost for the node in rotation spot
        let mut modified_node_count = max_number_of_walks + 1;
        add_worst_case_get_merk_node(cost, key.len() as u32, max_element_size);

        // During the commit phase
        // We update the backing store state
        // We write all the modified nodes + the root node + newly inserted node
        // i.e max_number_of_walks + 2 extra nodes
        modified_node_count += 2;

        // let max_number_of_modified_nodes = max_number_of_walks + 2 + 1;
        // dbg!(max_number_of_modified_nodes);

        // When writing a key value pair to the backing store
        // the key has to prefixed with a 32 byte hash
        // and the value is the encoded tree node
        let prefixed_key_size = 32 + key.len() as u32;
        // Note: encoded tree calculation assumes that each node has 2 children, this is
        // not always the case and as such is the source of worst case from
        // actual cost deviation. we can do better than assuming all nodes have
        // 2 links as there are some bounds on avl tree e.g. there must be a
        // leaf. for simplicity sake, keeping as this.
        let value_size = Tree::worst_case_encoded_tree_size(key.len() as u32, max_element_size);

        for _ in 0..modified_node_count {
            cost.seek_count += 1;
            cost.hash_node_calls += 1;
            cost.storage_cost.added_bytes += prefixed_key_size + value_size
        }

        // Reduce the hash node call count by 1 because the root node is not rehashed on
        // commit when you call merk.root_hash() the hash is computed in real
        // time.
        cost.hash_node_calls -= 1;

        // Write the root key
        cost.seek_count += 1;
        cost.storage_cost.added_bytes += prefixed_key_size + b"r".len() as u32;
    }

    /// Add worst case for insertion into merk
    pub(crate) fn add_worst_case_merk_replace_tree(
        cost: &mut OperationCost,
        key: &KeyInfo,
        propagate_if_input: Option<MerkWorstCaseInput>,
    ) {
        let key_len = key.len() as u32;
        add_worst_case_merk_replace_layered(cost, key_len, 3);
        if let Some(input) = propagate_if_input {
            add_worst_case_merk_propagate(cost, input);
        }
    }

    /// Add worst case for insertion into merk
    pub(crate) fn add_worst_case_merk_insert_tree(
        cost: &mut OperationCost,
        key: &KeyInfo,
        flags: &Option<ElementFlags>,
        propagate_if_input: Option<MerkWorstCaseInput>,
    ) {
        let key_len = key.len() as u32;
        let flags_len = flags.as_ref().map_or(0, |flags| {
            let flags_len = flags.len() as u32;
            flags_len + flags_len.required_space() as u32
        });
        let value_len = TREE_COST_SIZE + flags_len;
        add_worst_case_merk_insert_layered(cost, key_len, value_len);
        if let Some(input) = propagate_if_input {
            add_worst_case_merk_propagate(cost, input);
        }
    }

    /// Add worst case for insertion into merk
    pub(crate) fn add_worst_case_merk_insert_element(
        cost: &mut OperationCost,
        key: &KeyInfo,
        value: &Element,
        propagate_if_input: Option<MerkWorstCaseInput>,
    ) {
        let key_len = key.len() as u32;
        match value {
            Element::Tree(_, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = TREE_COST_SIZE + flags_len;
                add_worst_case_merk_insert_layered(cost, key_len, value_len)
            }
            _ => add_worst_case_merk_insert(cost, key_len, value.serialized_size() as u32),
        };
        if let Some(input) = propagate_if_input {
            add_worst_case_merk_propagate(cost, input);
        }
    }

    pub fn add_worst_case_delete_cost(
        _cost: &mut OperationCost,
        _max_element_size: u32,
        _max_key_size: u32,
    ) {
        // does nothing for now
    }

    pub fn add_worst_case_has_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
    ) {
        let value_size = Tree::worst_case_encoded_tree_size(key.len() as u32, max_element_size);
        cost.seek_count += 1;
        cost.storage_loaded_bytes += value_size;
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    pub fn add_worst_case_get_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
    ) {
        // todo: verify, we need to run a test to see if has raw has any better
        // performance than get raw
        Self::add_worst_case_get_merk_at_path::<S>(cost, path);
        add_worst_case_get_merk_node(cost, key.len() as u32, max_element_size);
    }

    pub fn add_worst_case_get_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        max_references_sizes: Vec<u32>,
    ) {
        // todo: verify
        let value_size: u32 =
            Tree::worst_case_encoded_tree_size(key.len() as u32, max_element_size);
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
        test_utils::make_batch_seq, worst_case_costs::add_worst_case_get_merk_node, Link, Merk, Op,
    };
    use storage::{rocksdb_storage::RocksDbStorage, worst_case_costs::WorstKeyLength, Storage};
    use tempfile::TempDir;

    use crate::{
        batch::{
            key_info::KeyInfo::{KnownKey, MaxKeySize},
            KeyInfoPath,
        },
        tests::TEST_LEAF,
        Element, GroveDb,
    };

    #[test]
    fn test_get_merk_node_worst_case() {
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
        add_worst_case_get_merk_node(&mut cost, key.len() as u32, 60);
        assert_eq!(cost, node_result.cost);
    }

    // this test needs to eventually be rewritten
    #[ignore]
    #[test]
    fn test_insert_merk_node_worst_case() {
        // Setup
        //      5
        //    /   \
        //   4    7
        //      /   \
        //     6    8

        // Test the worst case cost for inserting 9

        // Final tree
        //      7
        //    /   \
        //   5      8
        //  / \      \
        // 4   6      9

        // Testing approach:
        // The scenario defined above is not the worst possible case.
        // but because we know cost is deterministic and we know the cost of each
        // operation we should be use theory to apply precise reductions to the
        // worst case and if we are right then the reduced worst case should be
        // equal to the actual cost

        // Each key and each value are 1 byte each
        // max_number_of_elements is 6
        let key_info = MaxKeySize {
            unique_id: vec![0],
            max_size: 1,
        };
        const MAX_ELEMENT_SIZE: u32 = 1;
        const MAX_ELEMENT_NUMBER: u32 = 6;

        let mut worst_case_cost = OperationCost::default();
        GroveDb::add_worst_case_insert_merk_node(
            &mut worst_case_cost,
            MAX_ELEMENT_SIZE,
            MAX_ELEMENT_NUMBER,
            &key_info,
        );

        // Let's apply worst case reductions
        // seek_count and hash_node_calls should be accurate
        // deviation comes from storage_loaded_bytes and storage_written_bytes
        // this is because our get and write cost assumes all nodes have 2 children
        // which is not always the case.

        // storage_loaded_bytes
        // we load 7 and 8 during insertion
        // we also load 6 during rotation (as 5 has to to a left rotation on 7 but 6
        // occupies that slot). load = [7, 8, 6]
        // of these only 7 has two children, 8 and 6 have none
        // this means we are 4 links encoding higher
        worst_case_cost.storage_loaded_bytes -= 4 * Link::encoded_link_size(key_info.len() as u32);

        // storage_written_bytes
        // we write [5, 6, 7, 8, 9] + root_key
        // all the walked nodes, root_key, the root node and the newly inserted node
        // root_key is accurate
        // 5 and 7 have 2 links so accurate also
        // 8 has just 1 link (we assume 2 so cost of 1)
        // 6 and 9 have 0 links (we assume 2 so cost of 2 each = 4)
        // Total overhead = 5
        worst_case_cost.storage_cost.added_bytes -=
            5 * Link::encoded_link_size(key_info.len() as u32);

        // Now actual cost
        // Open a merk and insert setup elements
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");

        let a = vec![
            b"5".to_vec(),
            b"4".to_vec(),
            b"7".to_vec(),
            b"6".to_vec(),
            b"8".to_vec(),
        ];
        for m in a {
            merk.apply::<_, Vec<_>>(&[(m, Op::Put(b"a".to_vec()))], &[], None)
                .unwrap()
                .unwrap();
        }

        // drop merk, so nothing is stored in memory
        drop(merk);
        // Reopen merk: this time, only root node is loaded to memory
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");

        let actual_cost =
            merk.apply::<_, Vec<_>>(&[(b"9".to_vec(), Op::Put(b"a".to_vec()))], &[], None);

        assert_eq!(actual_cost.cost, worst_case_cost);
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
        );

        let actual_cost = db.has_raw([TEST_LEAF], &[2], None);

        assert_eq!(worst_case_has_raw_cost, actual_cost.cost);
    }
}
