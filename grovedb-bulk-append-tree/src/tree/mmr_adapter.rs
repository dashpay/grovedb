//! MMR storage adapter bridging `BulkStore` to MMR traits.

use grovedb_merkle_mountain_range::{
    CostResult, CostsExt, MMRStoreReadOps, MMRStoreWriteOps, MmrNode, OperationCost,
};

use super::keys::mmr_node_key;
use crate::BulkStore;

/// Adapter bridging `BulkStore` to MMR storage traits.
///
/// Callers should call `get_root()` **before** `commit()` so that
/// recently-pushed nodes are still available in the `MMRBatch` overlay.
/// This eliminates the need for a write-through cache.
pub(crate) struct MmrAdapter<'a, S: BulkStore> {
    pub store: &'a S,
}

impl<S: BulkStore> MMRStoreReadOps for &MmrAdapter<'_, S> {
    fn element_at_position(
        &self,
        pos: u64,
    ) -> CostResult<Option<MmrNode>, grovedb_merkle_mountain_range::Error> {
        let key = mmr_node_key(pos);
        let result = match self.store.get(&key) {
            Ok(Some(bytes)) => {
                let node = MmrNode::deserialize(&bytes).map_err(|e| {
                    grovedb_merkle_mountain_range::Error::StoreError(format!(
                        "deserialize MMR node at {}: {}",
                        pos, e
                    ))
                });
                match node {
                    Ok(n) => Ok(Some(n)),
                    Err(e) => Err(e),
                }
            }
            Ok(None) => Ok(None),
            Err(e) => Err(grovedb_merkle_mountain_range::Error::StoreError(e)),
        };
        result.wrap_with_cost(OperationCost::default())
    }
}

impl<S: BulkStore> MMRStoreWriteOps for &MmrAdapter<'_, S> {
    fn append(
        &mut self,
        pos: u64,
        elems: Vec<MmrNode>,
    ) -> CostResult<(), grovedb_merkle_mountain_range::Error> {
        for (i, node) in elems.into_iter().enumerate() {
            let p = pos + i as u64;
            let key = mmr_node_key(p);
            let serialized = match node.serialize() {
                Ok(s) => s,
                Err(e) => {
                    return Err(grovedb_merkle_mountain_range::Error::StoreError(format!(
                        "serialize MMR node at {}: {}",
                        p, e
                    )))
                    .wrap_with_cost(OperationCost::default());
                }
            };
            if let Err(e) = self.store.put(&key, &serialized) {
                return Err(grovedb_merkle_mountain_range::Error::StoreError(e))
                    .wrap_with_cost(OperationCost::default());
            }
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}
