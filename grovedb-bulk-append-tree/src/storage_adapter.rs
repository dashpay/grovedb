//! Storage adapter bridging GroveDB's `StorageContext` to `BulkStore`.
//!
//! Provides `DataBulkStore`, which implements the `BulkStore` trait backed by a
//! GroveDB storage context, accumulating `OperationCost` for each storage
//! operation.

use std::cell::RefCell;

use grovedb_costs::OperationCost;
use grovedb_storage::StorageContext;

use crate::BulkStore;

/// Storage adapter wrapping a GroveDB `StorageContext` for BulkAppendTree.
///
/// Reads and writes data to the data namespace of a GroveDB storage context.
/// Uses `RefCell` for cost accumulation since `BulkStore` methods take `&self`.
pub struct DataBulkStore<'a, C> {
    ctx: &'a C,
    cost: RefCell<OperationCost>,
}

impl<'a, C> DataBulkStore<'a, C> {
    pub fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            cost: RefCell::new(OperationCost::default()),
        }
    }

    /// Take accumulated costs out of this store.
    pub fn take_cost(&self) -> OperationCost {
        self.cost.take()
    }
}

impl<'db, C: StorageContext<'db>> BulkStore for DataBulkStore<'_, C> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let result = self
            .ctx
            .get(key)
            .unwrap_add_cost(&mut *self.cost.borrow_mut());
        result.map_err(|e| format!("storage get failed: {}", e))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String> {
        let result = self
            .ctx
            .put(key, value, None, None)
            .unwrap_add_cost(&mut *self.cost.borrow_mut());
        result.map_err(|e| format!("storage put failed: {}", e))
    }

    fn delete(&self, key: &[u8]) -> Result<(), String> {
        let result = self
            .ctx
            .delete(key, None)
            .unwrap_add_cost(&mut *self.cost.borrow_mut());
        result.map_err(|e| format!("storage delete failed: {}", e))
    }
}
