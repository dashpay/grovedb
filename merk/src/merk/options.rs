#[cfg(feature = "full")]
pub struct MerkOptions {
    pub base_root_storage_is_free: bool,
}

#[cfg(feature = "full")]
impl Default for MerkOptions {
    fn default() -> Self {
        Self {
            base_root_storage_is_free: true,
        }
    }
}
