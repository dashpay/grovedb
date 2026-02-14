//! MMR storage adapter bridging `BulkStore` to ckb MMR traits.

use std::{cell::RefCell, collections::HashMap};

use grovedb_mmr::{MMRStoreReadOps, MMRStoreWriteOps, MmrNode};

use super::keys::mmr_node_key;
use crate::BulkStore;

/// Adapter bridging `BulkStore` to ckb MMR storage traits.
///
/// Uses a write-through cache to handle read-after-write when the
/// underlying store defers writes (e.g., batch-based transactional storage).
pub(crate) struct MmrAdapter<'a, S: BulkStore> {
    pub store: &'a S,
    pub cache: &'a RefCell<HashMap<u64, MmrNode>>,
}

impl<S: BulkStore> MMRStoreReadOps<MmrNode> for &MmrAdapter<'_, S> {
    fn get_elem(&self, pos: u64) -> grovedb_mmr::CkbResult<Option<MmrNode>> {
        // Check cache first
        if let Some(node) = self.cache.borrow().get(&pos) {
            return Ok(Some(node.clone()));
        }
        // Fall through to storage
        let key = mmr_node_key(pos);
        match self.store.get(&key) {
            Ok(Some(bytes)) => {
                let node = MmrNode::deserialize(&bytes).map_err(|e| {
                    grovedb_mmr::CkbError::StoreError(format!(
                        "deserialize MMR node at {}: {}",
                        pos, e
                    ))
                })?;
                Ok(Some(node))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(grovedb_mmr::CkbError::StoreError(e)),
        }
    }
}

impl<S: BulkStore> MMRStoreWriteOps<MmrNode> for &MmrAdapter<'_, S> {
    fn append(&mut self, pos: u64, elems: Vec<MmrNode>) -> grovedb_mmr::CkbResult<()> {
        for (i, node) in elems.into_iter().enumerate() {
            let p = pos + i as u64;
            let key = mmr_node_key(p);
            let serialized = node.serialize();
            self.store
                .put(&key, &serialized)
                .map_err(grovedb_mmr::CkbError::StoreError)?;
            self.cache.borrow_mut().insert(p, node);
        }
        Ok(())
    }
}
