use super::GroveDb;

impl GroveDb {
    /// Add worst case for opening a root meta storage
    pub fn add_worst_case_open_root_meta_storage(&mut self) {
        self.seek_count += 0;
        self.storage_written_bytes += 0;
        self.storage_loaded_bytes += 0;
        self.loaded_bytes += 0;
        self.hash_byte_calls += 0;
        self.hash_node_calls += 0;
    }

    /// Add worst case for saving the root tree
    pub fn add_worst_case_save_root_leaves(&mut self) {
        self.seek_count += 0;
        self.storage_written_bytes += 0;
        self.storage_loaded_bytes += 0;
        self.loaded_bytes += 0;
        self.hash_byte_calls += 0;
        self.hash_node_calls += 0;
    }

    /// Add worst case for loading the root tree
    pub fn add_worst_case_load_root_leaves(&mut self) {
        self.seek_count += 0;
        self.storage_written_bytes += 0;
        self.storage_loaded_bytes += 0;
        self.loaded_bytes += 0;
        self.hash_byte_calls += 0;
        self.hash_node_calls += 0;
    }
}
