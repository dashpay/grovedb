use costs::OperationCost;
use storage::rocksdb_storage::RocksDbStorage;

use super::GroveDb;

impl GroveDb {
    /// Add worst case for opening a root meta storage
    pub fn add_worst_case_open_root_meta_storage(cost: &mut OperationCost) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for saving the root tree
    pub fn add_worst_case_save_root_leaves(cost: &mut OperationCost) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for loading the root tree
    pub fn add_worst_case_load_root_leaves(cost: &mut OperationCost) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_get_merk<'p, P>(cost: &mut OperationCost, path: P)
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        cost.seek_count += 1;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.hash_byte_calls += RocksDbStorage::build_prefix_hash_count(path) as u32;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_merk_has_element(cost: &mut OperationCost, key: &[u8]) {
        cost.seek_count += 1;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += key.len() as u32;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for getting a merk tree root hash
    pub fn add_worst_case_merk_root_hash(cost: &mut OperationCost) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }
}
