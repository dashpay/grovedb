use super::Merk;

impl Merk {
    /// Add worst case for getting a merk tree
    pub fn add_worst_case_get_merk<'p, P>(&mut self, path: P)
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        self.seek_count += 1;
        self.storage_written_bytes += 0;
        self.storage_loaded_bytes += 0;
        self.loaded_bytes += 0;
        self.hash_byte_calls += RocksDbStorage::build_prefix_hash_count(path) as u32;
        self.hash_node_calls += 0;
    }

    /// Add worst case for getting a merk tree
    pub fn add_worst_case_merk_has_element(&mut self, key: &[u8]) {
        self.seek_count += 1;
        self.storage_written_bytes += 0;
        self.storage_loaded_bytes += 0;
        self.loaded_bytes += key.len() as u32;
        self.hash_byte_calls += 0;
        self.hash_node_calls += 0;
    }

    /// Add worst case for getting a merk tree root hash
    pub fn add_worst_case_merk_root_hash(&mut self) {
        self.seek_count += 0;
        self.storage_written_bytes += 0;
        self.storage_loaded_bytes += 0;
        self.loaded_bytes += 0;
        self.hash_byte_calls += 0;
        self.hash_node_calls += 0;
    }

}
