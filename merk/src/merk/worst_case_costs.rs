use costs::OperationCost;

use super::Merk;

impl Merk {
    /// Add worst case for getting a merk tree
    pub fn add_worst_case_get_merk<'p, P>(cost: &mut OperationCost, path: P)
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        cost.seek_count += 1;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.loaded_bytes += 0;
        cost.hash_byte_calls += RocksDbStorage::build_prefix_hash_count(path) as u32;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_merk_has_element(cost: &mut OperationCost, key: &[u8]) {
        cost.seek_count += 1;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.loaded_bytes += key.len() as u32;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for getting a merk tree root hash
    pub fn add_worst_case_merk_root_hash(cost: &mut OperationCost) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }
}
