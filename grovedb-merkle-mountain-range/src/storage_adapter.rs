//! Storage adapter bridging GroveDB's `StorageContext` to MMR traits.
//!
//! Provides `MmrStore`, which implements `MMRStoreReadOps` and
//! `MMRStoreWriteOps` backed by a GroveDB storage context.

use std::{cell::RefCell, collections::BTreeMap};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostResult, CostsExt, OperationCost,
};
use grovedb_storage::StorageContext;

use crate::{
    helper::{mmr_node_key_sized, MmrKeySize},
    MMRStoreReadOps, MMRStoreWriteOps, MmrNode,
};

/// Storage adapter wrapping a GroveDB `StorageContext` for MMR operations.
///
/// Reads and writes MMR nodes to data storage keyed by position.
/// Costs from storage operations are returned directly via `CostResult`.
///
/// The `key_size` field controls the byte width of storage keys:
/// [`MmrKeySize::U64`] (default) uses 8-byte keys, [`MmrKeySize::U32`]
/// uses 4-byte keys for space savings when positions fit in a `u32`.
///
/// Callers should call `get_root()` **before** `commit()` so that
/// recently-pushed nodes are still available in the `MMRBatch` overlay.
/// This eliminates the need for a write-through cache.
pub struct MmrStore<'a, C> {
    ctx: &'a C,
    key_size: MmrKeySize,
    /// Storage cost info to attach to specific node positions when they are
    /// written (consumed on use). Lets a caller that knows what a leaf
    /// replaces — the bulk-append tree's chunk blob supersedes the buffer
    /// entries it was built from — report it as such instead of as new
    /// bytes, even though MMR nodes are staged in an overlay and written
    /// later. Positions without an entry get the storage layer's default
    /// report (new bytes).
    put_cost_infos: RefCell<BTreeMap<u64, KeyValueStorageCost>>,
}

impl<'a, C> MmrStore<'a, C> {
    /// Create a new store backed by the given storage context.
    ///
    /// Uses [`MmrKeySize::U64`] (8-byte keys) by default.
    pub fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            key_size: MmrKeySize::U64,
            put_cost_infos: RefCell::new(BTreeMap::new()),
        }
    }

    /// Create a new store with a specific key size.
    ///
    /// Use [`MmrKeySize::U32`] for compact 4-byte keys when positions
    /// are guaranteed to fit in a `u32`.
    pub fn with_key_size(ctx: &'a C, key_size: MmrKeySize) -> Self {
        Self {
            ctx,
            key_size,
            put_cost_infos: RefCell::new(BTreeMap::new()),
        }
    }

    /// Attach storage cost info to the writes of specific node positions;
    /// each entry is consumed by the write of its position.
    pub fn with_put_cost_infos(self, cost_infos: BTreeMap<u64, KeyValueStorageCost>) -> Self {
        *self.put_cost_infos.borrow_mut() = cost_infos;
        self
    }

    /// Take back any cost infos whose positions were not written (so a
    /// caller can re-stage them after a failed commit).
    pub fn take_put_cost_infos(&self) -> BTreeMap<u64, KeyValueStorageCost> {
        std::mem::take(&mut *self.put_cost_infos.borrow_mut())
    }
}

impl<'db, C: StorageContext<'db>> MMRStoreReadOps for &MmrStore<'_, C> {
    fn element_at_position(&self, pos: u64) -> CostResult<Option<MmrNode>, crate::Error> {
        let key = match mmr_node_key_sized(pos, self.key_size) {
            Ok(k) => k,
            Err(e) => return Err(e).wrap_with_cost(OperationCost::default()),
        };
        let result = self.ctx.get(key);
        let cost = result.cost;
        match result.value {
            Ok(Some(bytes)) => {
                let node = MmrNode::deserialize(&bytes).map_err(|e| {
                    crate::Error::StoreError(format!("deserialize node at pos {}: {}", pos, e))
                });
                match node {
                    Ok(n) => Ok(Some(n)).wrap_with_cost(cost),
                    Err(e) => Err(e).wrap_with_cost(cost),
                }
            }
            Ok(None) => Ok(None).wrap_with_cost(cost),
            Err(e) => Err(crate::Error::StoreError(format!(
                "get at pos {}: {}",
                pos, e
            )))
            .wrap_with_cost(cost),
        }
    }
}

impl<'db, C: StorageContext<'db>> MMRStoreWriteOps for &MmrStore<'_, C> {
    fn append(&mut self, pos: u64, elems: Vec<MmrNode>) -> CostResult<(), crate::Error> {
        let mut cost = OperationCost::default();
        for (i, elem) in elems.into_iter().enumerate() {
            let node_pos = pos + i as u64;
            let cost_info = self.put_cost_infos.borrow_mut().remove(&node_pos);
            let key = match mmr_node_key_sized(node_pos, self.key_size) {
                Ok(k) => k,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };
            let serialized = match elem.serialize() {
                Ok(s) => s,
                Err(e) => {
                    return Err(crate::Error::StoreError(format!(
                        "serialize at pos {}: {}",
                        node_pos, e
                    )))
                    .wrap_with_cost(cost);
                }
            };
            let result = self.ctx.put(key, &serialized, None, cost_info);
            cost += result.cost;
            if let Err(e) = result.value {
                return Err(crate::Error::StoreError(format!(
                    "put at pos {}: {}",
                    node_pos, e
                )))
                .wrap_with_cost(cost);
            }
        }
        Ok(()).wrap_with_cost(cost)
    }
}
