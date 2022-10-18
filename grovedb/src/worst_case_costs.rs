use costs::OperationCost;
use merk::{HASH_BLOCK_SIZE, HASH_LENGTH, HASH_LENGTH_U32};
use storage::{worst_case_costs::WorstKeyLength, Storage};

use super::GroveDb;
use crate::{
    batch::{KeyInfo, KeyInfoPath},
    Element,
};

impl GroveDb {
    fn worst_case_encoded_tree_size(key: &KeyInfo, max_element_size: u32) -> u32 {
        // two option values for the left and right link
        // the actual left and right link encoding size
        // the encoded kv node size
        2 + (2 * Self::worst_case_encoded_link_size(key))
            + Self::worst_case_encoded_kv_node_size(max_element_size)
    }

    // Worst case costs for operations within a single merk
    fn worst_case_encoded_link_size(key: &KeyInfo) -> u32 {
        // Links are optional values that represent the right or left node for a given
        // 1 byte to represent key_length
        // key_length to represent the actual key
        // 32 bytes for the hash of the node
        // 1 byte for the left child height
        // 1 byte for the right child height
        1 + key.len() as u32 + 32 + 1 + 1
    }

    fn worst_case_encoded_kv_node_size(max_element_size: u32) -> u32 {
        // KV holds the state of a node
        // 32 bytes to encode the hash of the node
        // 32 bytes to encode the value hash
        // max_element_size to encode the worst case value size
        32 + 32 + max_element_size
    }

    /// Add worst case for getting a merk node
    pub(crate) fn add_worst_case_get_merk_node(
        cost: &mut OperationCost,
        key: &KeyInfo,
        max_element_size: u32,
    ) {
        // Worst case scenario, the element is not already in memory.
        // One direct seek has to be performed to read the node from storage_cost.
        cost.seek_count += 1;

        // To write a node to disk, the left link, right link and kv nodes are encoded.
        // worst case, the node has both the left and right link present.
        cost.storage_loaded_bytes += Self::worst_case_encoded_tree_size(key, max_element_size);
    }

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

        // for each walk, we have to seek and load from storage_cost (equivalent to a
        // get)
        for _ in 0..max_number_of_walks {
            GroveDb::add_worst_case_get_merk_node(cost, key, max_element_size)
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
        GroveDb::add_worst_case_get_merk_node(cost, key, max_element_size);

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
        let value_size = Self::worst_case_encoded_tree_size(key, max_element_size);

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

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_get_merk<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
    ) {
        cost.seek_count += 2; // seek in meta for root key + loading that root key
        match path.last() {
            None => {}
            Some(key) => {
                cost.storage_loaded_bytes +=
                    Self::worst_case_encoded_tree_size(key, HASH_LENGTH as u32);
            }
        }
        *cost += S::get_storage_context_cost(path.as_vec());
    }

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_merk_has_element(
        cost: &mut OperationCost,
        key: &KeyInfo,
        max_element_size: u32,
    ) {
        cost.seek_count += 1;
        cost.storage_loaded_bytes += key.len() as u32 + max_element_size;
    }

    /// Add worst case for getting a merk tree root hash
    pub fn add_worst_case_merk_root_hash(cost: &mut OperationCost) {
        cost.hash_node_calls += Self::node_hash_update_count();
    }

    const fn node_hash_update_count() -> u16 {
        // It's a hash of node hash, left and right
        let bytes = HASH_LENGTH * 3;
        // todo: verify this
        let blocks = (bytes + 1) / HASH_BLOCK_SIZE;

        blocks as u16
    }

    /// Add worst case for insertion into merk
    pub(crate) fn add_worst_case_merk_insert(
        cost: &mut OperationCost,
        key: &KeyInfo,
        value: &Element,
        input: MerkWorstCaseInput,
    ) {
        let bytes_len = value.total_byte_size(key.len() as usize);

        cost.storage_cost.added_bytes += bytes_len as u32;
        // .. and hash computation for the inserted element itself
        // todo: verify this
        cost.hash_node_calls += ((bytes_len + 1) / HASH_BLOCK_SIZE) as u16;

        Self::add_worst_case_merk_propagate(cost, input);
    }

    pub(crate) fn add_worst_case_merk_propagate(
        cost: &mut OperationCost,
        input: MerkWorstCaseInput,
    ) {
        let mut nodes_updated = 0;
        // Propagation requires to recompute and write hashes up to the root
        let levels = match input {
            MerkWorstCaseInput::MaxElementsNumber(n) => ((n + 1) as f32).log2().ceil() as u32,
            MerkWorstCaseInput::NumberOfLevels(n) => n,
        };
        nodes_updated += levels;
        // In AVL tree two rotation may happen at most on insertion, some of them may
        // update one more node except one we already have on our path to the
        // root, thus two more updates.
        nodes_updated += 2;

        cost.storage_cost.replaced_bytes += nodes_updated * HASH_LENGTH_U32;
        // Same number of hash recomputations for propagation
        cost.hash_node_calls += (nodes_updated as u16) * Self::node_hash_update_count();
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
        let value_size = Self::worst_case_encoded_tree_size(key, max_element_size);
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
        Self::add_worst_case_get_merk::<S>(cost, path);
        Self::add_worst_case_get_merk_node(cost, key, max_element_size);
    }

    pub fn add_worst_case_get_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        max_references_sizes: Vec<u32>,
    ) {
        // todo: verify
        let value_size: u32 = Self::worst_case_encoded_tree_size(key, max_element_size);
        cost.seek_count += 1 + max_references_sizes.len() as u16;
        cost.storage_loaded_bytes += value_size + max_references_sizes.iter().sum::<u32>();
        *cost += S::get_storage_context_cost(path.as_vec());
    }
}

pub(crate) enum MerkWorstCaseInput {
    MaxElementsNumber(u32),
    NumberOfLevels(u32),
}

#[cfg(test)]
mod test {
    use std::{iter::empty, option::Option::None};

    use costs::OperationCost;
    use merk::{test_utils::make_batch_seq, Merk, Op};
    use storage::{rocksdb_storage::RocksDbStorage, Storage};
    use tempfile::TempDir;

    use crate::{
        batch::{
            KeyInfo::{KnownKey, MaxKeySize},
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
            .expect("cannot open rocksdb storage_cost");
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");
        let batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[])
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
        GroveDb::add_worst_case_get_merk_node(&mut cost, &key, 60);
        assert_eq!(cost, node_result.cost);
    }

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
        worst_case_cost.storage_loaded_bytes -=
            4 * GroveDb::worst_case_encoded_link_size(&key_info);

        // storage_written_bytes
        // we write [5, 6, 7, 8, 9] + root_key
        // all the walked nodes, root_key, the root node and the newly inserted node
        // root_key is accurate
        // 5 and 7 have 2 links so accurate also
        // 8 has just 1 link (we assume 2 so cost of 1)
        // 6 and 9 have 0 links (we assume 2 so cost of 2 each = 4)
        // Total overhead = 5
        worst_case_cost.storage_cost.added_bytes -=
            5 * GroveDb::worst_case_encoded_link_size(&key_info);

        // Now actual cost
        // Open a merk and insert setup elements
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage_cost");
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
            merk.apply::<_, Vec<_>>(&[(m, Op::Put(b"a".to_vec()))], &[])
                .unwrap()
                .unwrap();
        }

        // drop merk, so nothing is stored in memory
        drop(merk);
        // Reopen merk: this time, only root node is loaded to memory
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");

        let actual_cost = merk.apply::<_, Vec<_>>(&[(b"9".to_vec(), Op::Put(b"a".to_vec()))], &[]);

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
