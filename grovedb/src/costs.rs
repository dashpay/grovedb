use costs::OperationCost;

use super::GroveDb;

impl GroveDb {
    /// Add worst case for opening a root meta storage
    pub fn add_worst_case_open_root_meta_storage(cost: &mut OperationCost) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for saving the root tree
    pub fn add_worst_case_save_root_leaves(cost: &mut OperationCost) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }

    /// Add worst case for loading the root tree
    pub fn add_worst_case_load_root_leaves(cost: &mut OperationCOst) {
        cost.seek_count += 0;
        cost.storage_written_bytes += 0;
        cost.storage_loaded_bytes += 0;
        cost.loaded_bytes += 0;
        cost.hash_byte_calls += 0;
        cost.hash_node_calls += 0;
    }
}
