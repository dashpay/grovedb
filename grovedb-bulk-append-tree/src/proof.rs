//! BulkAppendTree proof generation and verification.
//!
//! Generates proofs that specific values/epochs exist in a BulkAppendTree.
//! The proof ties into the GroveDB hierarchy: the parent Merk proves the
//! BulkAppendTree element bytes (containing the state_root), and this proof
//! shows that queried data is consistent with that root.
//!
//! For range queries, the proof returns:
//! - Full epoch blobs for any completed epoch overlapping the range
//! - Individual buffer entries for data still in the buffer
//! - All buffer entries needed to recompute the buffer_hash chain

use std::collections::BTreeMap;

use bincode::{Decode, Encode};
use grovedb_mmr::{
    leaf_to_pos, MMRStoreReadOps, MMRStoreWriteOps, MergeBlake3, MerkleProof, MmrNode,
};

use crate::{
    chain_buffer_hash, compute_state_root, deserialize_epoch_blob, error::BulkAppendError,
};

/// A proof that specific data exists in a BulkAppendTree.
///
/// Contains:
/// - Epoch blobs for completed epochs overlapping the query range
/// - An MMR proof binding those epochs to the epoch MMR root
/// - All buffer entries (needed to recompute buffer_hash)
/// - Metadata to recompute state_root = blake3("bulk_state" || mmr_root ||
///   buffer_hash)
#[derive(Debug, Clone, Encode, Decode)]
pub struct BulkAppendTreeProof {
    /// The epoch size (power of 2).
    pub epoch_size: u32,
    /// Total count of values appended.
    pub total_count: u64,
    /// (epoch_index, epoch_blob_bytes) for epochs overlapping the range.
    pub epoch_blobs: Vec<(u64, Vec<u8>)>,
    /// Epoch MMR size.
    pub epoch_mmr_size: u64,
    /// MMR proof sibling/peak hashes for the proved epochs.
    pub epoch_mmr_proof_items: Vec<[u8; 32]>,
    /// (leaf_index_in_mmr, dense_merkle_root) for each proved epoch.
    pub epoch_mmr_leaves: Vec<(u64, [u8; 32])>,
    /// ALL buffer entries (needed to recompute buffer_hash from [0;32]).
    pub buffer_entries: Vec<Vec<u8>>,
    /// The buffer_hash from metadata (for verification).
    pub buffer_hash: [u8; 32],
    /// The epoch MMR root hash.
    pub epoch_mmr_root: [u8; 32],
}

impl BulkAppendTreeProof {
    /// Generate a BulkAppendTree proof for a position range.
    ///
    /// # Arguments
    /// * `total_count` - Total values appended
    /// * `epoch_size` - Epoch size from the element
    /// * `mmr_size` - Internal MMR size from metadata
    /// * `buffer_hash` - Running buffer hash from metadata
    /// * `start` - Start position (inclusive)
    /// * `end` - End position (exclusive)
    /// * `get_aux` - Closure to read storage values by key
    pub fn generate<F>(
        total_count: u64,
        epoch_size: u32,
        mmr_size: u64,
        buffer_hash: [u8; 32],
        start: u64,
        end: u64,
        get_aux: F,
    ) -> Result<Self, BulkAppendError>
    where
        F: Fn(&[u8]) -> Result<Option<Vec<u8>>, BulkAppendError>,
    {
        let epoch_size_u64 = epoch_size as u64;
        let completed_epochs = total_count / epoch_size_u64;
        let buffer_count = (total_count % epoch_size_u64) as u32;

        // Determine overlapping epochs
        let mut epoch_blobs = Vec::new();
        let mut epoch_indices = Vec::new();

        if completed_epochs > 0 && start < completed_epochs * epoch_size_u64 {
            let first_epoch = start / epoch_size_u64;
            let last_epoch = std::cmp::min(
                (end.saturating_sub(1)) / epoch_size_u64,
                completed_epochs - 1,
            );

            for epoch_idx in first_epoch..=last_epoch {
                let key = crate::epoch_key(epoch_idx);
                let blob = get_aux(&key)?.ok_or_else(|| {
                    BulkAppendError::CorruptedData(format!(
                        "epoch blob missing for epoch {}",
                        epoch_idx
                    ))
                })?;
                epoch_blobs.push((epoch_idx, blob));
                epoch_indices.push(epoch_idx);
            }
        }

        // Generate MMR proof for the epoch indices
        let mut epoch_mmr_proof_items = Vec::new();
        let mut epoch_mmr_leaves = Vec::new();

        if !epoch_indices.is_empty() {
            // Compute dense Merkle root for each epoch blob
            for (epoch_idx, blob) in &epoch_blobs {
                let values = deserialize_epoch_blob(blob).map_err(|e| {
                    BulkAppendError::CorruptedData(format!(
                        "failed to deserialize epoch {} blob: {}",
                        epoch_idx, e
                    ))
                })?;
                let value_refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
                let (dense_root, _) = grovedb_mmr::compute_dense_merkle_root_from_values(
                    &value_refs,
                )
                .map_err(|e| {
                    BulkAppendError::CorruptedData(format!(
                        "failed to compute dense merkle root for epoch {}: {}",
                        epoch_idx, e
                    ))
                })?;
                epoch_mmr_leaves.push((*epoch_idx, dense_root));
            }

            // Build MMR positions and generate proof
            let positions: Vec<u64> = epoch_indices.iter().map(|&idx| leaf_to_pos(idx)).collect();

            // Load all MMR nodes for proof generation
            let mut mmr_nodes = BTreeMap::new();
            for pos in 0..mmr_size {
                let key = grovedb_mmr::mmr_node_key(pos);
                if let Some(bytes) = get_aux(&key)? {
                    let node = MmrNode::deserialize(&bytes).map_err(|e| {
                        BulkAppendError::CorruptedData(format!(
                            "failed to deserialize MMR node at pos {}: {}",
                            pos, e
                        ))
                    })?;
                    mmr_nodes.insert(pos, node);
                }
            }

            let store = BTreeMapStore(mmr_nodes);
            let mmr = grovedb_mmr::MMR::<MmrNode, MergeBlake3, _>::new(mmr_size, &store);
            let proof = mmr.gen_proof(positions).map_err(|e| {
                BulkAppendError::MmrError(format!("epoch MMR gen_proof failed: {}", e))
            })?;

            epoch_mmr_proof_items = proof.proof_items().iter().map(|node| node.hash).collect();
        }

        // Get the epoch MMR root
        let epoch_mmr_root = if mmr_size > 0 {
            let mut mmr_nodes = BTreeMap::new();
            for pos in 0..mmr_size {
                let key = grovedb_mmr::mmr_node_key(pos);
                if let Some(bytes) = get_aux(&key)? {
                    let node = MmrNode::deserialize(&bytes).map_err(|e| {
                        BulkAppendError::CorruptedData(format!(
                            "failed to deserialize MMR node at pos {}: {}",
                            pos, e
                        ))
                    })?;
                    mmr_nodes.insert(pos, node);
                }
            }
            let store = BTreeMapStore(mmr_nodes);
            let mmr = grovedb_mmr::MMR::<MmrNode, MergeBlake3, _>::new(mmr_size, &store);
            let root = mmr.get_root().map_err(|e| {
                BulkAppendError::MmrError(format!("epoch MMR get_root failed: {}", e))
            })?;
            root.hash
        } else {
            [0u8; 32]
        };

        // Read ALL buffer entries (bounded by epoch_size)
        let mut buffer_entries = Vec::with_capacity(buffer_count as usize);
        for i in 0..buffer_count {
            let key = crate::buffer_key(i);
            let value = get_aux(&key)?.ok_or_else(|| {
                BulkAppendError::CorruptedData(format!("buffer entry missing at index {}", i))
            })?;
            buffer_entries.push(value);
        }

        Ok(BulkAppendTreeProof {
            epoch_size,
            total_count,
            epoch_blobs,
            epoch_mmr_size: mmr_size,
            epoch_mmr_proof_items,
            epoch_mmr_leaves,
            buffer_entries,
            buffer_hash,
            epoch_mmr_root,
        })
    }

    /// Verify this proof against an expected state root.
    ///
    /// This is a pure function — no database access needed.
    ///
    /// # Arguments
    /// * `expected_state_root` - The state_root from the parent element
    ///
    /// # Returns
    /// The data in the queried range, as a mix of epoch blobs and buffer
    /// entries.
    pub fn verify(
        &self,
        expected_state_root: &[u8; 32],
    ) -> Result<BulkAppendTreeProofResult, BulkAppendError> {
        // 1. Verify epoch blobs: compute dense Merkle root for each and check it
        //    matches the epoch_mmr_leaves
        for (epoch_idx, blob) in &self.epoch_blobs {
            let values = deserialize_epoch_blob(blob).map_err(|e| {
                BulkAppendError::InvalidProof(format!(
                    "failed to deserialize epoch {} blob: {}",
                    epoch_idx, e
                ))
            })?;

            // Find the corresponding leaf entry
            let expected_root = self
                .epoch_mmr_leaves
                .iter()
                .find(|(idx, _)| idx == epoch_idx)
                .map(|(_, root)| root)
                .ok_or_else(|| {
                    BulkAppendError::InvalidProof(format!(
                        "no MMR leaf entry for epoch {}",
                        epoch_idx
                    ))
                })?;

            let value_refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
            let (computed_root, _) =
                grovedb_mmr::compute_dense_merkle_root_from_values(&value_refs).map_err(|e| {
                    BulkAppendError::InvalidProof(format!(
                        "failed to compute dense merkle root for epoch {}: {}",
                        epoch_idx, e
                    ))
                })?;

            if &computed_root != expected_root {
                return Err(BulkAppendError::InvalidProof(format!(
                    "dense merkle root mismatch for epoch {}: expected {}, got {}",
                    epoch_idx,
                    hex::encode(expected_root),
                    hex::encode(computed_root)
                )));
            }
        }

        // 2. Verify epoch MMR proof
        if !self.epoch_mmr_leaves.is_empty() {
            let proof_nodes: Vec<MmrNode> = self
                .epoch_mmr_proof_items
                .iter()
                .map(|hash| MmrNode::internal(*hash))
                .collect();

            let proof = MerkleProof::<MmrNode, MergeBlake3>::new(self.epoch_mmr_size, proof_nodes);

            let verification_leaves: Vec<(u64, MmrNode)> = self
                .epoch_mmr_leaves
                .iter()
                .map(|(idx, root)| {
                    let pos = leaf_to_pos(*idx);
                    let node = MmrNode::internal(*root);
                    (pos, node)
                })
                .collect();

            let root_node = MmrNode::internal(self.epoch_mmr_root);
            let valid = proof.verify(root_node, verification_leaves).map_err(|e| {
                BulkAppendError::InvalidProof(format!("epoch MMR proof verification failed: {}", e))
            })?;

            if !valid {
                return Err(BulkAppendError::InvalidProof(
                    "epoch MMR proof root hash mismatch".to_string(),
                ));
            }
        }

        // 3. Recompute buffer_hash from all buffer entries
        let mut computed_buffer_hash = [0u8; 32];
        for entry in &self.buffer_entries {
            computed_buffer_hash = chain_buffer_hash(&computed_buffer_hash, entry);
        }

        if computed_buffer_hash != self.buffer_hash {
            return Err(BulkAppendError::InvalidProof(format!(
                "buffer hash mismatch: expected {}, computed {}",
                hex::encode(self.buffer_hash),
                hex::encode(computed_buffer_hash)
            )));
        }

        // 4. Verify state_root = blake3("bulk_state" || mmr_root || buffer_hash)
        let computed_state_root = compute_state_root(&self.epoch_mmr_root, &self.buffer_hash);

        if &computed_state_root != expected_state_root {
            return Err(BulkAppendError::InvalidProof(format!(
                "state root mismatch: expected {}, computed {}",
                hex::encode(expected_state_root),
                hex::encode(computed_state_root)
            )));
        }

        Ok(BulkAppendTreeProofResult {
            epoch_blobs: self.epoch_blobs.clone(),
            buffer_entries: self.buffer_entries.clone(),
            total_count: self.total_count,
            epoch_size: self.epoch_size,
        })
    }

    /// Serialize this proof to bytes using bincode.
    pub fn encode_to_vec(&self) -> Result<Vec<u8>, BulkAppendError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        bincode::encode_to_vec(self, config).map_err(|e| {
            BulkAppendError::CorruptedData(format!("failed to encode BulkAppendTreeProof: {}", e))
        })
    }

    /// Deserialize a proof from bytes.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BulkAppendError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (proof, _) = bincode::decode_from_slice(bytes, config).map_err(|e| {
            BulkAppendError::CorruptedData(format!("failed to decode BulkAppendTreeProof: {}", e))
        })?;
        Ok(proof)
    }
}

/// Result of a verified BulkAppendTree proof.
#[derive(Debug, Clone)]
pub struct BulkAppendTreeProofResult {
    /// Epoch blobs overlapping the queried range.
    pub epoch_blobs: Vec<(u64, Vec<u8>)>,
    /// All buffer entries.
    pub buffer_entries: Vec<Vec<u8>>,
    /// Total count of values in the tree.
    pub total_count: u64,
    /// Epoch size.
    pub epoch_size: u32,
}

impl BulkAppendTreeProofResult {
    /// Extract values in the position range [start, end).
    ///
    /// Collects values from epoch blobs and buffer entries that fall within
    /// the specified range.
    pub fn values_in_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, BulkAppendError> {
        let epoch_size_u64 = self.epoch_size as u64;
        let completed_epochs = self.total_count / epoch_size_u64;
        let mut result = Vec::new();

        // Extract from epoch blobs
        for (epoch_idx, blob) in &self.epoch_blobs {
            let values = deserialize_epoch_blob(blob).map_err(|e| {
                BulkAppendError::CorruptedData(format!(
                    "failed to deserialize epoch blob {}: {}",
                    epoch_idx, e
                ))
            })?;

            let epoch_start = epoch_idx * epoch_size_u64;
            for (i, value) in values.into_iter().enumerate() {
                let global_pos = epoch_start + i as u64;
                if global_pos >= start && global_pos < end {
                    result.push((global_pos, value));
                }
            }
        }

        // Extract from buffer entries
        let buffer_start = completed_epochs * epoch_size_u64;
        for (i, entry) in self.buffer_entries.iter().enumerate() {
            let global_pos = buffer_start + i as u64;
            if global_pos >= start && global_pos < end {
                result.push((global_pos, entry.clone()));
            }
        }

        result.sort_by_key(|(pos, _)| *pos);
        Ok(result)
    }
}

/// BTreeMap-based read-only store for MMR proof generation.
struct BTreeMapStore(BTreeMap<u64, MmrNode>);

impl MMRStoreReadOps<MmrNode> for &BTreeMapStore {
    fn get_elem(&self, pos: u64) -> grovedb_mmr::CkbResult<Option<MmrNode>> {
        Ok(self.0.get(&pos).cloned())
    }
}

impl MMRStoreWriteOps<MmrNode> for &BTreeMapStore {
    fn append(&mut self, _pos: u64, _elems: Vec<MmrNode>) -> grovedb_mmr::CkbResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BulkAppendTree, BulkStore};

    /// Helper: build an in-memory BulkAppendTree with N values and return
    /// the state needed for proof generation.
    fn build_test_tree(
        epoch_size: u32,
        values: &[Vec<u8>],
    ) -> (
        u64,                                         // total_count
        u64,                                         // mmr_size
        [u8; 32],                                    // buffer_hash
        [u8; 32],                                    // state_root
        std::collections::HashMap<Vec<u8>, Vec<u8>>, // aux storage
    ) {
        let store = InMemBulkStore::new();
        let mut tree = BulkAppendTree::new(epoch_size).expect("create tree");

        for value in values {
            tree.append(&store, value).expect("append value");
        }

        let state_root = compute_state_root(
            &{
                if tree.mmr_size() > 0 {
                    // Compute MMR root from stored nodes
                    let mut mmr_nodes = BTreeMap::new();
                    for (k, v) in store.0.borrow().iter() {
                        if k.starts_with(b"m") {
                            let pos = u64::from_be_bytes(k[1..9].try_into().expect("pos bytes"));
                            let node = MmrNode::deserialize(v).expect("deserialize node");
                            mmr_nodes.insert(pos, node);
                        }
                    }
                    let mmr_store = BTreeMapStore(mmr_nodes);
                    let mmr = grovedb_mmr::MMR::<MmrNode, MergeBlake3, _>::new(
                        tree.mmr_size(),
                        &mmr_store,
                    );
                    mmr.get_root().expect("get root").hash
                } else {
                    [0u8; 32]
                }
            },
            &tree.buffer_hash(),
        );

        let aux: std::collections::HashMap<Vec<u8>, Vec<u8>> = store
            .0
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        (
            tree.total_count(),
            tree.mmr_size(),
            tree.buffer_hash(),
            state_root,
            aux,
        )
    }

    /// Simple in-memory BulkStore for testing.
    struct InMemBulkStore(std::cell::RefCell<std::collections::HashMap<Vec<u8>, Vec<u8>>>);

    impl InMemBulkStore {
        fn new() -> Self {
            Self(std::cell::RefCell::new(std::collections::HashMap::new()))
        }
    }

    impl BulkStore for InMemBulkStore {
        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
            Ok(self.0.borrow().get(key).cloned())
        }

        fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String> {
            self.0.borrow_mut().insert(key.to_vec(), value.to_vec());
            Ok(())
        }

        fn delete(&self, key: &[u8]) -> Result<(), String> {
            self.0.borrow_mut().remove(key);
            Ok(())
        }
    }

    #[test]
    fn test_bulk_proof_buffer_only() {
        let values: Vec<Vec<u8>> = (0..3u32)
            .map(|i| format!("val_{}", i).into_bytes())
            .collect();
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(4, &values);

        let proof =
            BulkAppendTreeProof::generate(total_count, 4, mmr_size, buffer_hash, 0, 3, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        assert!(proof.epoch_blobs.is_empty());
        assert_eq!(proof.buffer_entries.len(), 3);

        let result = proof.verify(&state_root).expect("verify proof");
        let vals = result.values_in_range(0, 3).expect("extract range");
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0], (0, b"val_0".to_vec()));
        assert_eq!(vals[1], (1, b"val_1".to_vec()));
        assert_eq!(vals[2], (2, b"val_2".to_vec()));
    }

    #[test]
    fn test_bulk_proof_epoch_and_buffer() {
        // 6 values with epoch_size=4 → 1 epoch (0..4) + 2 buffer entries (4..6)
        let values: Vec<Vec<u8>> = (0..6u32)
            .map(|i| format!("data_{}", i).into_bytes())
            .collect();
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(4, &values);

        assert_eq!(total_count, 6);

        // Query range 0..6 (all data)
        let proof =
            BulkAppendTreeProof::generate(total_count, 4, mmr_size, buffer_hash, 0, 6, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        assert_eq!(proof.epoch_blobs.len(), 1);
        assert_eq!(proof.buffer_entries.len(), 2);

        let result = proof.verify(&state_root).expect("verify proof");
        let vals = result.values_in_range(0, 6).expect("extract range");
        assert_eq!(vals.len(), 6);
        for i in 0..6u32 {
            assert_eq!(vals[i as usize].1, format!("data_{}", i).into_bytes());
        }
    }

    #[test]
    fn test_bulk_proof_multiple_epochs() {
        // 12 values with epoch_size=4 → 3 epochs
        let values: Vec<Vec<u8>> = (0..12u32)
            .map(|i| format!("e_{}", i).into_bytes())
            .collect();
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(4, &values);

        // Query range 2..10 (overlaps epoch 0, 1, 2)
        let proof =
            BulkAppendTreeProof::generate(total_count, 4, mmr_size, buffer_hash, 2, 10, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        assert_eq!(proof.epoch_blobs.len(), 3);

        let result = proof.verify(&state_root).expect("verify proof");
        let vals = result.values_in_range(2, 10).expect("extract range");
        assert_eq!(vals.len(), 8);
        assert_eq!(vals[0], (2, b"e_2".to_vec()));
        assert_eq!(vals[7], (9, b"e_9".to_vec()));
    }

    #[test]
    fn test_bulk_proof_wrong_state_root_fails() {
        let values: Vec<Vec<u8>> = (0..4u32).map(|i| format!("x_{}", i).into_bytes()).collect();
        let (total_count, mmr_size, buffer_hash, _state_root, aux) = build_test_tree(4, &values);

        let proof =
            BulkAppendTreeProof::generate(total_count, 4, mmr_size, buffer_hash, 0, 4, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        let wrong_root = [0xFFu8; 32];
        assert!(proof.verify(&wrong_root).is_err());
    }

    #[test]
    fn test_bulk_proof_encode_decode() {
        let values: Vec<Vec<u8>> = (0..5u32).map(|i| format!("r_{}", i).into_bytes()).collect();
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(4, &values);

        let proof =
            BulkAppendTreeProof::generate(total_count, 4, mmr_size, buffer_hash, 0, 5, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        let bytes = proof.encode_to_vec().expect("encode proof");
        let decoded = BulkAppendTreeProof::decode_from_slice(&bytes).expect("decode proof");
        let result = decoded.verify(&state_root).expect("verify decoded proof");
        let vals = result.values_in_range(0, 5).expect("extract range");
        assert_eq!(vals.len(), 5);
    }
}
