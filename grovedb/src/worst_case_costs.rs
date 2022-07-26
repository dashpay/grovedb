use costs::OperationCost;
use storage::Storage;

use super::GroveDb;
use crate::Element;

impl GroveDb {
    // Worst case costs for operations within a single merk
    fn worst_case_encoded_link_size(key: &[u8]) -> u32 {
        // Links are optional values that represent the right or left node for a given
        // tree 1 byte to represent the option state
        // 1 byte to represent key_length
        // key_length to represent the actual key
        // 32 bytes for the hash of the node
        // 1 byte for the left child height
        // 1 byte for the right child height
        1 + 1 + key.len() as u32 + 32 + 1 + 1
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
        key: &[u8],
        max_element_size: u32,
    ) {
        // Worst case scenario, the element is not already in memory.
        // One direct seek has to be performed to read the node from storage.
        cost.seek_count += 1;

        // To write a node to disk, the left link, right link and kv nodes are encoded.
        // worst case, the node has both the left and right link present.
        let loaded_storage_bytes = (2 * Self::worst_case_encoded_link_size(key))
            + Self::worst_case_encoded_kv_node_size(max_element_size);
        cost.storage_loaded_bytes += loaded_storage_bytes;
    }

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_get_merk<'db, 'p, P, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: P,
        max_element_size: u32,
    ) where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        cost.seek_count += 2; // 1 for seek in meta for root key, 1 for loading that root key
        cost.storage_loaded_bytes += max_element_size;
        *cost += S::get_storage_context_cost(path);
    }

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_merk_has_element(
        cost: &mut OperationCost,
        key: &[u8],
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
        let bytes = merk::HASH_LENGTH * 3;
        let blocks = (bytes - 64 + 1) / 64;

        blocks as u16
    }

    /// Add worst case for insertion into merk
    pub(crate) fn add_worst_case_merk_insert(
        cost: &mut OperationCost,
        key: &[u8],
        value: &Element,
        input: MerkWorstCaseInput,
    ) {
        // TODO is is safe to unwrap?
        let bytes_len = key.len() + value.serialize().expect("element is serializeable").len();

        cost.storage_written_bytes += bytes_len as u32;
        // .. and hash computation for the inserted element iteslf
        cost.hash_node_calls += ((bytes_len - 64 + 1) / 64) as u16;

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

        // TODO: use separate field for hash propagation rather than written bytes
        cost.storage_written_bytes += nodes_updated * 32;
        // Same number of hash recomputations for propagation
        cost.hash_node_calls += (nodes_updated as u16) * Self::node_hash_update_count();
    }
}

pub(crate) enum MerkWorstCaseInput {
    MaxElementsNumber(u32),
    NumberOfLevels(u32),
}

#[cfg(test)]
mod test {
    use std::iter::empty;

    use costs::{CostContext, OperationCost};
    use merk::{test_utils::make_batch_seq, Merk};
    use storage::{rocksdb_storage::RocksDbStorage, Storage};
    use tempfile::TempDir;

    use crate::GroveDb;

    #[test]
    fn test_get_worst_case() {
        // Try to replicate a worst case scenario
        // open a merk, insert elements in the merk
        // get one of the elements that must have both a right and left child node
        // and is currently not loaded in the tree.
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");
        let batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[])
            .unwrap()
            .unwrap();
        drop(merk);

        let mut merk = Merk::open(storage.get_storage_context(empty()).unwrap())
            .unwrap()
            .expect("cannot open merk");
        dbg!("getting node 8");
        let m = merk.get(&8_u64.to_be_bytes());
        let mut cost = OperationCost::default();
        // make_batch_seq creates values of 60 bytes
        GroveDb::add_worst_case_get_merk_node(&mut cost, &8_u64.to_be_bytes(), 60);
        assert_eq!(cost, m.cost);
    }
}
