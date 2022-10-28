pub struct MerkOptions {
    pub base_root_storage_is_free: bool
}

impl Default for MerkOptions {
    fn default() -> Self {
        Self {
            base_root_storage_is_free: true
        }
    }
}