use crate::DenseMerkleError;

/// Abstract storage for dense tree node values.
///
/// Uses `&self` (interior mutability) to match the GroveDB `StorageContext`
/// pattern.
pub trait DenseTreeStore {
    fn get_value(&self, position: u16) -> Result<Option<Vec<u8>>, DenseMerkleError>;
    fn put_value(&self, position: u16, value: &[u8]) -> Result<(), DenseMerkleError>;
}
