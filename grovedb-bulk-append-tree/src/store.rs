//! Storage abstraction for the bulk append tree.

/// Abstraction over key-value storage for the bulk append tree.
///
/// `put` and `delete` take `&self` (not `&mut self`) to match GroveDB's
/// `StorageContext` pattern where writes go through a batch with interior
/// mutability.
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
