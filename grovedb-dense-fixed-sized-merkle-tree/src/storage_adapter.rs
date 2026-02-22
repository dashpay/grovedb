//! Storage adapter bridging GroveDB's `StorageContext` to `DenseTreeStore`.
//!
//! Provides `AuxDenseTreeStore`, which implements the `DenseTreeStore` trait
//! backed by a GroveDB storage context, with a write-through cache and
//! `OperationCost` accumulation.

use std::{cell::RefCell, collections::HashMap};

use grovedb_costs::OperationCost;
use grovedb_storage::StorageContext;

use crate::{DenseMerkleError, DenseTreeStore};

/// Encode a position as a big-endian 2-byte key for storage.
pub fn position_key(pos: u16) -> [u8; 2] {
    pos.to_be_bytes()
}

/// Storage adapter wrapping a GroveDB `StorageContext` for
/// DenseFixedSizedMerkleTree.
///
/// Uses auxiliary storage keyed by position (big-endian u16).
/// Write-through cache ensures nodes written during insert are immediately
/// readable, even when the underlying storage context defers writes.
pub struct AuxDenseTreeStore<'a, C> {
    ctx: &'a C,
    cost: RefCell<OperationCost>,
    cache: RefCell<HashMap<u16, Vec<u8>>>,
}

impl<'a, C> AuxDenseTreeStore<'a, C> {
    pub fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            cost: RefCell::new(OperationCost::default()),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Take accumulated costs out of this store.
    pub fn take_cost(&self) -> OperationCost {
        self.cost.take()
    }
}

impl<'db, C: StorageContext<'db>> DenseTreeStore for AuxDenseTreeStore<'_, C> {
    fn get_value(&self, position: u16) -> Result<Option<Vec<u8>>, DenseMerkleError> {
        // Check the write-through cache first
        if let Some(v) = self.cache.borrow().get(&position) {
            return Ok(Some(v.clone()));
        }
        let key = position_key(position);
        let result = self
            .ctx
            .get(&key)
            .unwrap_add_cost(&mut *self.cost.borrow_mut());
        result.map_err(|e| DenseMerkleError::StoreError(format!("get at pos {}: {}", position, e)))
    }

    fn put_value(&self, position: u16, value: &[u8]) -> Result<(), DenseMerkleError> {
        let key = position_key(position);
        let result = self
            .ctx
            .put(&key, value, None, None)
            .unwrap_add_cost(&mut *self.cost.borrow_mut());
        result
            .map_err(|e| DenseMerkleError::StoreError(format!("put at pos {}: {}", position, e)))?;
        self.cache.borrow_mut().insert(position, value.to_vec());
        Ok(())
    }
}
