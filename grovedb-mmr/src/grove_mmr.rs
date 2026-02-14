//! GroveMmr: convenience wrapper around ckb MMR with Blake3 hashing.

use ckb_merkle_mountain_range::{
    helper::leaf_index_to_pos, util::MemStore, MMRStoreReadOps, MerkleProof, MMR,
};

use crate::{util::mmr_size_to_leaf_count, MergeBlake3, MmrError, MmrNode};

/// Convenience wrapper around ckb MMR with Blake3 hashing.
///
/// Uses ckb's `MemStore<MmrNode>` (BTreeMap-based with interior mutability)
/// as the backing store. The store is accessed by reference through the ckb
/// MMR.
pub struct GroveMmr {
    store: MemStore<MmrNode>,
    mmr_size: u64,
}

impl GroveMmr {
    /// Create a new empty MMR.
    pub fn new() -> Self {
        Self {
            store: MemStore::default(),
            mmr_size: 0,
        }
    }

    /// Append a value, returning its leaf index (0-based).
    pub fn push(&mut self, value: Vec<u8>) -> Result<u64, MmrError> {
        let leaf = MmrNode::leaf(value);
        let leaf_count = mmr_size_to_leaf_count(self.mmr_size);
        let mut mmr = MMR::<MmrNode, MergeBlake3, _>::new(self.mmr_size, &self.store);
        mmr.push(leaf)
            .map_err(|e| MmrError::OperationFailed(format!("push failed: {}", e)))?;
        mmr.commit()
            .map_err(|e| MmrError::OperationFailed(format!("commit failed: {}", e)))?;
        self.mmr_size = mmr.mmr_size();
        Ok(leaf_count)
    }

    /// Get the root hash (bags all peaks).
    pub fn root_hash(&self) -> Result<[u8; 32], MmrError> {
        let mmr = MMR::<MmrNode, MergeBlake3, _>::new(self.mmr_size, &self.store);
        let root = mmr
            .get_root()
            .map_err(|e| MmrError::OperationFailed(format!("get_root failed: {}", e)))?;
        Ok(root.hash)
    }

    /// Get a leaf's value by its 0-based leaf index.
    pub fn get_leaf(&self, leaf_index: u64) -> Result<Option<Vec<u8>>, MmrError> {
        let pos = leaf_index_to_pos(leaf_index);
        let mmr = MMR::<MmrNode, MergeBlake3, _>::new(self.mmr_size, &self.store);
        let node = mmr
            .store()
            .get_elem(pos)
            .map_err(|e| MmrError::OperationFailed(format!("get_elem failed: {}", e)))?;
        Ok(node.and_then(|n| n.value))
    }

    /// Number of leaves appended so far.
    pub fn leaf_count(&self) -> u64 {
        mmr_size_to_leaf_count(self.mmr_size)
    }

    /// The internal MMR size (total node count including internal nodes).
    pub fn mmr_size(&self) -> u64 {
        self.mmr_size
    }

    /// Generate a Merkle proof for the given positions.
    pub fn gen_proof(
        &self,
        positions: Vec<u64>,
    ) -> Result<MerkleProof<MmrNode, MergeBlake3>, MmrError> {
        let mmr = MMR::<MmrNode, MergeBlake3, _>::new(self.mmr_size, &self.store);
        mmr.gen_proof(positions)
            .map_err(|e| MmrError::OperationFailed(format!("gen_proof failed: {}", e)))
    }

    /// Get the underlying MemStore reference.
    pub fn store(&self) -> &MemStore<MmrNode> {
        &self.store
    }
}

impl Default for GroveMmr {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use ckb_merkle_mountain_range::{helper::leaf_index_to_pos, MMRStoreReadOps, MMR};

    use super::*;

    #[test]
    fn test_empty_mmr() {
        let mmr = GroveMmr::new();
        assert_eq!(mmr.mmr_size(), 0);
        assert_eq!(mmr.leaf_count(), 0);
        assert!(mmr.get_leaf(0).expect("get leaf from empty mmr").is_none());
    }

    #[test]
    fn test_push_single() {
        let mut mmr = GroveMmr::new();
        let idx = mmr.push(b"hello".to_vec()).expect("push hello");
        assert_eq!(idx, 0);
        assert_eq!(mmr.leaf_count(), 1);
        assert_eq!(
            mmr.get_leaf(0)
                .expect("get leaf 0")
                .expect("leaf should exist"),
            b"hello".to_vec()
        );
    }

    #[test]
    fn test_push_multiple() {
        let mut mmr = GroveMmr::new();
        for i in 0..10u64 {
            let idx = mmr
                .push(format!("item_{}", i).into_bytes())
                .expect("push item");
            assert_eq!(idx, i);
        }
        assert_eq!(mmr.leaf_count(), 10);
        for i in 0..10u64 {
            assert_eq!(
                mmr.get_leaf(i)
                    .expect("get leaf")
                    .expect("leaf should exist"),
                format!("item_{}", i).into_bytes()
            );
        }
    }

    #[test]
    fn test_deterministic_roots() {
        let mut mmr1 = GroveMmr::new();
        let mut mmr2 = GroveMmr::new();
        for i in 0..10u64 {
            mmr1.push(format!("val_{}", i).into_bytes())
                .expect("push to mmr1");
            mmr2.push(format!("val_{}", i).into_bytes())
                .expect("push to mmr2");
        }
        assert_eq!(
            mmr1.root_hash().expect("root hash 1"),
            mmr2.root_hash().expect("root hash 2")
        );
    }

    #[test]
    fn test_different_values_different_roots() {
        let mut mmr1 = GroveMmr::new();
        let mut mmr2 = GroveMmr::new();
        mmr1.push(b"aaa".to_vec()).expect("push aaa");
        mmr2.push(b"bbb".to_vec()).expect("push bbb");
        assert_ne!(
            mmr1.root_hash().expect("root hash 1"),
            mmr2.root_hash().expect("root hash 2")
        );
    }

    #[test]
    fn test_root_changes_on_push() {
        let mut mmr = GroveMmr::new();
        mmr.push(b"first".to_vec()).expect("push first");
        let root1 = mmr.root_hash().expect("root hash after first");
        mmr.push(b"second".to_vec()).expect("push second");
        let root2 = mmr.root_hash().expect("root hash after second");
        assert_ne!(root1, root2);
    }

    #[test]
    fn test_proof_generation_and_verification() {
        let mut mmr = GroveMmr::new();
        for i in 0..5u64 {
            mmr.push(format!("leaf_{}", i).into_bytes())
                .expect("push leaf");
        }
        let root = mmr.root_hash().expect("root hash");
        let root_node = MmrNode::internal(root);

        // Prove leaf at index 2
        let pos = leaf_index_to_pos(2);
        let proof = mmr.gen_proof(vec![pos]).expect("generate proof");

        // Get the leaf node to verify against
        let inner_mmr = MMR::<MmrNode, MergeBlake3, _>::new(mmr.mmr_size(), mmr.store());
        let leaf_node = inner_mmr
            .store()
            .get_elem(pos)
            .expect("get element at proof pos")
            .expect("leaf should exist");

        let result = proof.verify(root_node, vec![(pos, leaf_node)]);
        assert!(result.is_ok());
    }
}
