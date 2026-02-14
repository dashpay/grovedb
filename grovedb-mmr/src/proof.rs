//! MMR tree proof generation and verification.
//!
//! Generates proofs that specific leaf values exist in an MMR.
//! The proof ties into the GroveDB hierarchy: the parent Merk proves the
//! MMR element bytes (containing the mmr_root), and this proof shows
//! that queried leaves are consistent with that root.

use std::collections::BTreeMap;

use bincode::{Decode, Encode};

use crate::{
    leaf_to_pos, mmr_size_to_leaf_count, MMRStoreReadOps, MMRStoreWriteOps, MergeBlake3,
    MerkleProof, MmrError, MmrNode,
};

/// A proof that specific leaves exist in an MMR tree.
///
/// Contains the MMR size, the proved leaf values with their indices,
/// and the sibling/peak hashes needed for verification.
#[derive(Debug, Clone, Encode, Decode)]
pub struct MmrTreeProof {
    /// The MMR size at proof generation time.
    pub mmr_size: u64,
    /// (leaf_index, value_bytes) for each proved leaf.
    pub leaves: Vec<(u64, Vec<u8>)>,
    /// Sibling/peak hashes from the MMR proof (32 bytes each).
    pub proof_items: Vec<[u8; 32]>,
}

impl MmrTreeProof {
    /// Generate an MMR proof for the given leaf indices.
    ///
    /// Reads nodes from storage via the provided closure and generates
    /// a ckb MerkleProof for the requested positions.
    ///
    /// # Arguments
    /// * `mmr_size` - The MMR size from the element
    /// * `leaf_indices` - 0-based leaf indices to prove
    /// * `get_node` - Closure to read an MmrNode by MMR position from storage
    pub fn generate<F>(mmr_size: u64, leaf_indices: &[u64], get_node: F) -> Result<Self, MmrError>
    where
        F: Fn(u64) -> Result<Option<MmrNode>, MmrError>,
    {
        let leaf_count = mmr_size_to_leaf_count(mmr_size);

        // Validate indices
        for &idx in leaf_indices {
            if idx >= leaf_count {
                return Err(MmrError::InvalidInput(format!(
                    "MMR leaf index {} out of range (leaf_count={})",
                    idx, leaf_count
                )));
            }
        }

        // Convert leaf indices to MMR positions
        let positions: Vec<u64> = leaf_indices.iter().map(|&idx| leaf_to_pos(idx)).collect();

        // Collect leaf values
        let mut leaves = Vec::with_capacity(leaf_indices.len());
        for &idx in leaf_indices {
            let pos = leaf_to_pos(idx);
            let node = get_node(pos)?.ok_or_else(|| {
                MmrError::InvalidData(format!(
                    "MMR leaf node missing at position {} (leaf index {})",
                    pos, idx
                ))
            })?;
            let value = node.value.ok_or_else(|| {
                MmrError::InvalidData(format!(
                    "MMR node at position {} is internal, expected leaf",
                    pos
                ))
            })?;
            leaves.push((idx, value));
        }

        // Build an in-memory store from storage for proof generation
        let store = MemNodeStore::new(&get_node, mmr_size)?;

        // Generate the ckb MerkleProof
        let mmr = crate::MMR::<MmrNode, MergeBlake3, _>::new(mmr_size, &store);
        let proof = mmr
            .gen_proof(positions)
            .map_err(|e| MmrError::OperationFailed(format!("MMR gen_proof failed: {}", e)))?;

        // Extract proof item hashes
        let proof_items: Vec<[u8; 32]> = proof.proof_items().iter().map(|node| node.hash).collect();

        Ok(MmrTreeProof {
            mmr_size,
            leaves,
            proof_items,
        })
    }

    /// Verify this proof against an expected MMR root hash.
    ///
    /// This is a pure function — no database access needed.
    ///
    /// # Arguments
    /// * `expected_mmr_root` - The MMR root hash from the parent element
    ///
    /// # Returns
    /// The verified leaf values as `(leaf_index, value_bytes)` pairs.
    pub fn verify(&self, expected_mmr_root: &[u8; 32]) -> Result<Vec<(u64, Vec<u8>)>, MmrError> {
        // Reconstruct proof items as MmrNodes (internal, hash-only)
        let proof_nodes: Vec<MmrNode> = self
            .proof_items
            .iter()
            .map(|hash| MmrNode::internal(*hash))
            .collect();

        // Reconstruct the ckb MerkleProof
        let proof = MerkleProof::<MmrNode, MergeBlake3>::new(self.mmr_size, proof_nodes);

        // Build leaf entries for verification: (mmr_position, MmrNode)
        let verification_leaves: Vec<(u64, MmrNode)> = self
            .leaves
            .iter()
            .map(|(idx, value)| {
                let pos = leaf_to_pos(*idx);
                let node = MmrNode::leaf(value.clone());
                (pos, node)
            })
            .collect();

        // Verify against the expected root
        let root_node = MmrNode::internal(*expected_mmr_root);
        let valid = proof
            .verify(root_node, verification_leaves)
            .map_err(|e| MmrError::InvalidProof(format!("MMR proof verification failed: {}", e)))?;

        if !valid {
            return Err(MmrError::InvalidProof(
                "MMR proof root hash mismatch".to_string(),
            ));
        }

        Ok(self.leaves.clone())
    }

    /// Serialize this proof to bytes using bincode.
    pub fn encode_to_vec(&self) -> Result<Vec<u8>, MmrError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        bincode::encode_to_vec(self, config)
            .map_err(|e| MmrError::InvalidData(format!("failed to encode MmrTreeProof: {}", e)))
    }

    /// Deserialize a proof from bytes.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, MmrError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (proof, _) = bincode::decode_from_slice(bytes, config)
            .map_err(|e| MmrError::InvalidData(format!("failed to decode MmrTreeProof: {}", e)))?;
        Ok(proof)
    }
}

/// In-memory store that loads all required MMR nodes for proof generation.
///
/// The ckb MMR proof generator needs random access to nodes. This store
/// pre-loads them from storage via the provided closure.
struct MemNodeStore {
    nodes: BTreeMap<u64, MmrNode>,
}

impl MemNodeStore {
    fn new<F>(get_node: &F, mmr_size: u64) -> Result<Self, MmrError>
    where
        F: Fn(u64) -> Result<Option<MmrNode>, MmrError>,
    {
        let mut nodes = BTreeMap::new();
        // Load all nodes up to mmr_size
        for pos in 0..mmr_size {
            if let Some(node) = get_node(pos)? {
                nodes.insert(pos, node);
            }
        }
        Ok(Self { nodes })
    }
}

impl MMRStoreReadOps<MmrNode> for &MemNodeStore {
    fn get_elem(&self, pos: u64) -> crate::CkbResult<Option<MmrNode>> {
        Ok(self.nodes.get(&pos).cloned())
    }
}

impl MMRStoreWriteOps<MmrNode> for &MemNodeStore {
    fn append(&mut self, _pos: u64, _elems: Vec<MmrNode>) -> crate::CkbResult<()> {
        // Read-only store — proof generation doesn't need writes
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MMRStoreReadOps;

    /// Helper to create get_node closure from a GroveMmr's store.
    fn get_node_from_mmr(
        store: &crate::CkbMemStore<MmrNode>,
    ) -> impl Fn(u64) -> Result<Option<MmrNode>, MmrError> + '_ {
        move |pos| {
            store
                .get_elem(pos)
                .map_err(|e| MmrError::OperationFailed(format!("get_elem: {}", e)))
        }
    }

    #[test]
    fn test_mmr_proof_roundtrip_single_leaf() {
        let mut mmr = crate::GroveMmr::new();
        for i in 0..5u64 {
            mmr.push(format!("leaf_{}", i).into_bytes())
                .expect("push leaf");
        }
        let root = mmr.root_hash().expect("root hash");
        let mmr_size = mmr.mmr_size();

        let proof = MmrTreeProof::generate(mmr_size, &[2], get_node_from_mmr(mmr.store()))
            .expect("generate proof");

        assert_eq!(proof.leaves.len(), 1);
        assert_eq!(proof.leaves[0].0, 2);
        assert_eq!(proof.leaves[0].1, b"leaf_2".to_vec());

        let verified = proof.verify(&root).expect("verify proof");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].1, b"leaf_2".to_vec());
    }

    #[test]
    fn test_mmr_proof_roundtrip_multiple_leaves() {
        let mut mmr = crate::GroveMmr::new();
        for i in 0..10u64 {
            mmr.push(format!("val_{}", i).into_bytes())
                .expect("push value");
        }
        let root = mmr.root_hash().expect("root hash");
        let mmr_size = mmr.mmr_size();

        let proof = MmrTreeProof::generate(mmr_size, &[1, 5, 8], get_node_from_mmr(mmr.store()))
            .expect("generate proof");

        assert_eq!(proof.leaves.len(), 3);

        let verified = proof.verify(&root).expect("verify proof");
        assert_eq!(verified[0], (1, b"val_1".to_vec()));
        assert_eq!(verified[1], (5, b"val_5".to_vec()));
        assert_eq!(verified[2], (8, b"val_8".to_vec()));
    }

    #[test]
    fn test_mmr_proof_wrong_root_fails() {
        let mut mmr = crate::GroveMmr::new();
        for i in 0..5u64 {
            mmr.push(format!("data_{}", i).into_bytes())
                .expect("push data");
        }
        let mmr_size = mmr.mmr_size();

        let proof = MmrTreeProof::generate(mmr_size, &[0], get_node_from_mmr(mmr.store()))
            .expect("generate proof");

        let wrong_root = [0xFFu8; 32];
        assert!(proof.verify(&wrong_root).is_err());
    }

    #[test]
    fn test_mmr_proof_encode_decode() {
        let mut mmr = crate::GroveMmr::new();
        for i in 0..3u64 {
            mmr.push(format!("item_{}", i).into_bytes())
                .expect("push item");
        }
        let root = mmr.root_hash().expect("root hash");
        let mmr_size = mmr.mmr_size();

        let proof = MmrTreeProof::generate(mmr_size, &[0, 2], get_node_from_mmr(mmr.store()))
            .expect("generate proof");

        let bytes = proof.encode_to_vec().expect("encode proof");
        let decoded = MmrTreeProof::decode_from_slice(&bytes).expect("decode proof");
        let verified = decoded.verify(&root).expect("verify decoded proof");
        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0].1, b"item_0".to_vec());
        assert_eq!(verified[1].1, b"item_2".to_vec());
    }

    #[test]
    fn test_mmr_proof_out_of_range_leaf_index() {
        let mut mmr = crate::GroveMmr::new();
        mmr.push(b"only".to_vec()).expect("push");
        let mmr_size = mmr.mmr_size();

        let result = MmrTreeProof::generate(mmr_size, &[5], get_node_from_mmr(mmr.store()));
        assert!(result.is_err());
    }
}
