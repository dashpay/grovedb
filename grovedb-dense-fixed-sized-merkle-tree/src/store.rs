use grovedb_costs::CostResult;

use crate::DenseMerkleError;

/// Abstract storage for dense tree node values.
///
/// Uses `&self` (interior mutability) to match the GroveDB `StorageContext`
/// pattern. Returns `CostResult` to track storage I/O costs.
pub trait DenseTreeStore {
    fn get_value(&self, position: u16) -> CostResult<Option<Vec<u8>>, DenseMerkleError>;
    fn put_value(&self, position: u16, value: &[u8]) -> CostResult<(), DenseMerkleError>;
}
