use grovedb_costs::CostResult;

use crate::DenseMerkleError;

/// Abstract storage for dense tree node values.
///
/// Uses `&self` (interior mutability) to match the GroveDB `StorageContext`
/// pattern. Returns `CostResult` to track storage I/O costs.
pub trait DenseTreeStore {
    /// Retrieve the value stored at `position`, or `None` if empty.
    fn get_value(&self, position: u16) -> CostResult<Option<Vec<u8>>, DenseMerkleError>;
    /// Store `value` at the given `position`, overwriting any previous value.
    fn put_value(&self, position: u16, value: &[u8]) -> CostResult<(), DenseMerkleError>;
}
