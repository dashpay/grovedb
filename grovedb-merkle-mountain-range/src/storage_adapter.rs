//! Storage adapter bridging GroveDB's `StorageContext` to MMR traits.
//!
//! Provides `MmrStore`, which implements `MMRStoreReadOps` and
//! `MMRStoreWriteOps` backed by a GroveDB storage context.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;

use crate::{MMRStoreReadOps, MMRStoreWriteOps, MmrNode, helper::mmr_node_key};

/// Storage adapter wrapping a GroveDB `StorageContext` for MMR operations.
///
/// Reads and writes MMR nodes to data storage keyed by position.
/// Costs from storage operations are returned directly via `CostResult`.
///
/// Callers should call `get_root()` **before** `commit()` so that
/// recently-pushed nodes are still available in the `MMRBatch` overlay.
/// This eliminates the need for a write-through cache.
pub struct MmrStore<'a, C> {
    ctx: &'a C,
}

impl<'a, C> MmrStore<'a, C> {
    /// Create a new store backed by the given storage context.
    pub fn new(ctx: &'a C) -> Self {
        Self { ctx }
    }
}

impl<'db, C: StorageContext<'db>> MMRStoreReadOps for &MmrStore<'_, C> {
    fn element_at_position(&self, pos: u64) -> CostResult<Option<MmrNode>, crate::Error> {
        let key = mmr_node_key(pos);
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
            let key = mmr_node_key(node_pos);
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
            let result = self.ctx.put(&key, &serialized, None, None);
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
