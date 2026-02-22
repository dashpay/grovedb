//! Storage adapter bridging GroveDB's `StorageContext` to `DenseTreeStore`.
//!
//! Provides `DenseTreeStorageContext`, which implements the `DenseTreeStore`
//! trait backed by a GroveDB storage context with a write-through cache.

use std::{cell::RefCell, collections::HashMap};

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;

use crate::{DenseMerkleError, DenseTreeStore};

/// Encode a position as a big-endian 2-byte key for storage.
pub fn position_key(pos: u16) -> [u8; 2] {
    pos.to_be_bytes()
}

/// Storage adapter wrapping a GroveDB `StorageContext` for
/// DenseFixedSizedMerkleTree.
///
/// Uses data storage keyed by position (big-endian u16).
/// Write-through cache ensures nodes written during insert are immediately
/// readable, even when the underlying storage context defers writes.
pub struct DenseTreeStorageContext<'a, C> {
    ctx: &'a C,
    cache: RefCell<HashMap<u16, Vec<u8>>>,
}

impl<'a, C> DenseTreeStorageContext<'a, C> {
    /// Create a new storage context adapter.
    pub fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            cache: RefCell::new(HashMap::new()),
        }
    }
}

impl<'db, C: StorageContext<'db>> DenseTreeStore for DenseTreeStorageContext<'_, C> {
    fn get_value(&self, position: u16) -> CostResult<Option<Vec<u8>>, DenseMerkleError> {
        // Check the write-through cache first
        if let Some(v) = self.cache.borrow().get(&position) {
            let loaded = v.len() as u64;
            return Ok(Some(v.clone())).wrap_with_cost(OperationCost {
                seek_count: 1,
                storage_loaded_bytes: loaded,
                ..Default::default()
            });
        }
        let mut cost = OperationCost::default();
        let key = position_key(position);
        let result = self.ctx.get(&key).unwrap_add_cost(&mut cost);
        match result {
            Ok(opt) => Ok(opt).wrap_with_cost(cost),
            Err(e) => Err(DenseMerkleError::StoreError(format!(
                "get at pos {}: {}",
                position, e
            )))
            .wrap_with_cost(cost),
        }
    }

    fn put_value(&self, position: u16, value: &[u8]) -> CostResult<(), DenseMerkleError> {
        let mut cost = OperationCost::default();
        let key = position_key(position);
        let result = self
            .ctx
            .put(&key, value, None, None)
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(()) => {
                self.cache.borrow_mut().insert(position, value.to_vec());
                Ok(()).wrap_with_cost(cost)
            }
            Err(e) => Err(DenseMerkleError::StoreError(format!(
                "put at pos {}: {}",
                position, e
            )))
            .wrap_with_cost(cost),
        }
    }
}
