//! MMR storage adapter bridging `BulkStore` to ckb MMR traits.

use std::{cell::RefCell, collections::HashMap};

use ckb_merkle_mountain_range::{MMRStoreReadOps, MMRStoreWriteOps};
use grovedb_mmr::MmrNode;

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
    fn get_elem(&self, pos: u64) -> ckb_merkle_mountain_range::Result<Option<MmrNode>> {
        // Check cache first
        if let Some(node) = self.cache.borrow().get(&pos) {
            return Ok(Some(node.clone()));
        }
        // Fall through to storage
        let key = mmr_node_key(pos);
        match self.store.get(&key) {
            Ok(Some(bytes)) => {
                if bytes.len() != 32 {
                    return Err(ckb_merkle_mountain_range::Error::StoreError(format!(
                        "MMR node at {} has invalid size {}",
                        pos,
                        bytes.len()
                    )));
                }
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&bytes);
                Ok(Some(MmrNode::internal(hash)))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(ckb_merkle_mountain_range::Error::StoreError(e)),
        }
    }
}

impl<S: BulkStore> MMRStoreWriteOps<MmrNode> for &MmrAdapter<'_, S> {
    fn append(&mut self, pos: u64, elems: Vec<MmrNode>) -> ckb_merkle_mountain_range::Result<()> {
        for (i, node) in elems.into_iter().enumerate() {
            let p = pos + i as u64;
            let key = mmr_node_key(p);
            self.store
                .put(&key, &node.hash)
                .map_err(ckb_merkle_mountain_range::Error::StoreError)?;
            self.cache.borrow_mut().insert(p, node);
        }
        Ok(())
    }
}
