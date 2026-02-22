//! BulkAppendTree proof generation and verification.
//!
//! Generates proofs that specific values/chunks exist in a BulkAppendTree.
//! The proof ties into the GroveDB hierarchy: the parent Merk proves the
//! BulkAppendTree element bytes (containing the state_root), and this proof
//! shows that queried data is consistent with that root.
//!
//! For range queries, the proof returns:
//! - Full chunk blobs for any completed chunk overlapping the range
//! - Individual buffer entries for data still in the buffer
//! - All buffer entries needed to recompute the buffer_hash chain

use std::{cell::RefCell, collections::BTreeMap};

use bincode::{Decode, Encode};
use grovedb_merkle_mountain_range::{
    leaf_hash, leaf_to_pos, CostsExt, MMRStoreReadOps, MMRStoreWriteOps, MerkleProof, MmrNode,
    OperationCost,
};

use crate::{
    chain_buffer_hash, compute_state_root, deserialize_chunk_blob, error::BulkAppendError,
};

/// A proof that specific data exists in a BulkAppendTree.
///
/// Contains:
/// - Chunk blobs for completed chunks overlapping the query range
/// - An MMR proof binding those chunks to the chunk MMR root
/// - All buffer entries (needed to recompute buffer_hash)
/// - Metadata to recompute state_root = blake3("bulk_state" || mmr_root ||
///   buffer_hash)
#[derive(Debug, Clone, Encode, Decode)]
pub struct BulkAppendTreeProof {
    /// The chunk power (chunk_size = 1 << chunk_power).
    pub chunk_power: u8,
    /// Total count of values appended.
    pub total_count: u64,
    /// (chunk_index, chunk_blob_bytes) for chunks overlapping the range.
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,
    /// Chunk MMR size.
    pub chunk_mmr_size: u64,
    /// MMR proof sibling/peak hashes for the proved chunks.
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,
    /// (leaf_index_in_mmr, mmr_leaf_hash) for each proved chunk.
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,
    /// ALL buffer entries (needed to recompute buffer_hash from [0;32]).
    pub buffer_entries: Vec<Vec<u8>>,
    /// The buffer_hash from metadata (for verification).
    pub buffer_hash: [u8; 32],
    /// The chunk MMR root hash.
    pub chunk_mmr_root: [u8; 32],
}

impl BulkAppendTreeProof {
    /// Generate a BulkAppendTree proof for a position range.
    ///
    /// # Arguments
    /// * `total_count` - Total values appended
    /// * `chunk_power` - Chunk power from the element (chunk_size = 1 <<
    ///   chunk_power)
    /// * `mmr_size` - Internal MMR size from metadata
    /// * `buffer_hash` - Running buffer hash from metadata
    /// * `start` - Start position (inclusive)
    /// * `end` - End position (exclusive)
    /// * `get_aux` - Closure to read storage values by key
    pub fn generate<F>(
        total_count: u64,
        chunk_power: u8,
        mmr_size: u64,
        buffer_hash: [u8; 32],
        start: u64,
        end: u64,
        get_aux: F,
    ) -> Result<Self, BulkAppendError>
    where
        F: Fn(&[u8]) -> Result<Option<Vec<u8>>, BulkAppendError>,
    {
        let chunk_size = 1u32 << chunk_power;
        let chunk_size_u64 = chunk_size as u64;
        let completed_chunks = total_count / chunk_size_u64;
        let buffer_count = (total_count % chunk_size_u64) as u32;

        // Determine overlapping chunks
        let mut chunk_blobs = Vec::new();
        let mut chunk_indices = Vec::new();

        if completed_chunks > 0 && start < completed_chunks * chunk_size_u64 {
            let first_chunk = start / chunk_size_u64;
            let last_chunk = std::cmp::min(
                (end.saturating_sub(1)) / chunk_size_u64,
                completed_chunks - 1,
            );

            for chunk_idx in first_chunk..=last_chunk {
                // Read chunk blob from the MMR leaf node
                let mmr_pos = leaf_to_pos(chunk_idx);
                let key = grovedb_merkle_mountain_range::mmr_node_key(mmr_pos);
                let raw = get_aux(&key)?.ok_or_else(|| {
                    BulkAppendError::CorruptedData(format!(
                        "MMR leaf missing for chunk {}",
                        chunk_idx
                    ))
                })?;
                let node = MmrNode::deserialize(&raw).map_err(|e| {
                    BulkAppendError::CorruptedData(format!(
                        "failed to deserialize MMR leaf for chunk {}: {}",
                        chunk_idx, e
                    ))
                })?;
                let blob = node.into_value().ok_or_else(|| {
                    BulkAppendError::CorruptedData(format!(
                        "MMR leaf for chunk {} has no data",
                        chunk_idx
                    ))
                })?;
                chunk_blobs.push((chunk_idx, blob));
                chunk_indices.push(chunk_idx);
            }
        }

        // Generate MMR proof for the chunk indices
        let mut chunk_mmr_proof_items = Vec::new();
        let mut chunk_mmr_leaves = Vec::new();

        // Create a shared lazy-loading store for MMR node access.
        // Used for both proof generation and root computation.
        let lazy_mmr_store = LazyMmrNodeStore::new(&get_aux);

        if !chunk_indices.is_empty() {
            // Compute MMR leaf hash for each chunk blob.
            // The MMR stores chunks as standard leaf nodes whose hash is
            // blake3(0x00 || blob), so the proof must carry the same hash.
            for (chunk_idx, blob) in &chunk_blobs {
                let mmr_leaf_hash = leaf_hash(blob);
                chunk_mmr_leaves.push((*chunk_idx, mmr_leaf_hash));
            }

            // Build MMR positions and generate proof (lazy store, drops zero costs)
            let positions: Vec<u64> = chunk_indices.iter().map(|&idx| leaf_to_pos(idx)).collect();
            let mmr = grovedb_merkle_mountain_range::MMR::new(mmr_size, &lazy_mmr_store);
            let proof_result = mmr.gen_proof(positions).unwrap();

            // Check deferred storage errors first -- if the store failed,
            // the ckb error is a symptom, not the root cause.
            if let Some(err) = lazy_mmr_store.take_error() {
                return Err(err);
            }
            let proof = proof_result.map_err(|e| {
                BulkAppendError::MmrError(format!("chunk MMR gen_proof failed: {}", e))
            })?;

            chunk_mmr_proof_items = proof.proof_items().iter().map(|node| node.hash()).collect();
        }

        // Get the chunk MMR root (reuses cached nodes from proof generation, drops zero
        // costs)
        let chunk_mmr_root = if mmr_size > 0 {
            let mmr = grovedb_merkle_mountain_range::MMR::new(mmr_size, &lazy_mmr_store);
            let root_result = mmr.get_root().unwrap();

            if let Some(err) = lazy_mmr_store.take_error() {
                return Err(err);
            }
            let root = root_result.map_err(|e| {
                BulkAppendError::MmrError(format!("chunk MMR get_root failed: {}", e))
            })?;

            root.hash()
        } else {
            [0u8; 32]
        };

        // Read ALL buffer entries (bounded by chunk_size)
        let mut buffer_entries = Vec::with_capacity(buffer_count as usize);
        for i in 0..buffer_count {
            let key = crate::buffer_key(i);
            let value = get_aux(&key)?.ok_or_else(|| {
                BulkAppendError::CorruptedData(format!("buffer entry missing at index {}", i))
            })?;
            buffer_entries.push(value);
        }

        Ok(BulkAppendTreeProof {
            chunk_power,
            total_count,
            chunk_blobs,
            chunk_mmr_size: mmr_size,
            chunk_mmr_proof_items,
            chunk_mmr_leaves,
            buffer_entries,
            buffer_hash,
            chunk_mmr_root,
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
    /// The data in the queried range, as a mix of chunk blobs and buffer
    /// entries.
    pub fn verify(
        &self,
        expected_state_root: &[u8; 32],
    ) -> Result<BulkAppendTreeProofResult, BulkAppendError> {
        let (computed_state_root, result) = self.verify_inner()?;

        if &computed_state_root != expected_state_root {
            return Err(BulkAppendError::InvalidProof(format!(
                "state root mismatch: expected {}, computed {}",
                hex::encode(expected_state_root),
                hex::encode(computed_state_root)
            )));
        }

        Ok(result)
    }

    /// Verify this proof's internal consistency and return the computed
    /// state root.
    ///
    /// Unlike [`verify`], this does NOT check against an expected state root.
    /// The caller is responsible for authenticating the returned state root
    /// through some other mechanism (e.g., as a child Merk hash).
    ///
    /// # Returns
    /// `(computed_state_root, proof_result)` — the state root derived from the
    /// proof's internal data plus the verified result.
    pub fn verify_and_compute_root(
        &self,
    ) -> Result<([u8; 32], BulkAppendTreeProofResult), BulkAppendError> {
        self.verify_inner()
    }

    /// Internal verification logic shared by `verify` and
    /// `verify_and_compute_root`.
    ///
    /// Returns `(computed_state_root, proof_result)`.
    fn verify_inner(&self) -> Result<([u8; 32], BulkAppendTreeProofResult), BulkAppendError> {
        // 0. Cross-validate metadata consistency
        if self.chunk_power > 31 {
            return Err(BulkAppendError::InvalidProof(format!(
                "invalid chunk_power: {} (must be <= 31)",
                self.chunk_power
            )));
        }

        let chunk_size_u64 = (1u32 << self.chunk_power) as u64;
        let completed_chunks = self.total_count / chunk_size_u64;
        let buffer_count = (self.total_count % chunk_size_u64) as usize;

        // Verify buffer entry count matches metadata
        if self.buffer_entries.len() != buffer_count {
            return Err(BulkAppendError::InvalidProof(format!(
                "buffer entry count mismatch: proof has {}, expected {} (total_count={}, \
                 chunk_power={})",
                self.buffer_entries.len(),
                buffer_count,
                self.total_count,
                self.chunk_power
            )));
        }

        // Verify chunk_mmr_size is consistent with completed_chunks
        if completed_chunks > 0 {
            let expected_leaf_count =
                grovedb_merkle_mountain_range::mmr_size_to_leaf_count(self.chunk_mmr_size);
            if expected_leaf_count != completed_chunks {
                return Err(BulkAppendError::InvalidProof(format!(
                    "chunk MMR leaf count mismatch: MMR has {} leaves, expected {} completed \
                     chunks",
                    expected_leaf_count, completed_chunks
                )));
            }
        } else if self.chunk_mmr_size != 0 {
            return Err(BulkAppendError::InvalidProof(format!(
                "chunk_mmr_size should be 0 with no completed chunks, got {}",
                self.chunk_mmr_size
            )));
        }

        // 1. Verify chunk blobs: compute leaf_hash(blob) for each and check it
        //    matches the chunk_mmr_leaves. The MMR stores chunks as standard
        //    leaf nodes whose hash = blake3(0x00 || blob).
        for (chunk_idx, blob) in &self.chunk_blobs {
            let expected_hash = self
                .chunk_mmr_leaves
                .iter()
                .find(|(idx, _)| idx == chunk_idx)
                .map(|(_, hash)| hash)
                .ok_or_else(|| {
                    BulkAppendError::InvalidProof(format!(
                        "no MMR leaf entry for chunk {}",
                        chunk_idx
                    ))
                })?;

            let computed_hash = leaf_hash(blob);
            if &computed_hash != expected_hash {
                return Err(BulkAppendError::InvalidProof(format!(
                    "chunk blob hash mismatch for chunk {}: expected {}, got {}",
                    chunk_idx,
                    hex::encode(expected_hash),
                    hex::encode(computed_hash)
                )));
            }
        }

        // 2. Verify chunk MMR proof
        if !self.chunk_mmr_leaves.is_empty() {
            let proof_nodes: Vec<MmrNode> = self
                .chunk_mmr_proof_items
                .iter()
                .map(|hash| MmrNode::internal(*hash))
                .collect();

            let proof = MerkleProof::new(self.chunk_mmr_size, proof_nodes);

            let verification_leaves: Vec<(u64, MmrNode)> = self
                .chunk_mmr_leaves
                .iter()
                .map(|(idx, root)| {
                    let pos = leaf_to_pos(*idx);
                    let node = MmrNode::internal(*root);
                    (pos, node)
                })
                .collect();

            let root_node = MmrNode::internal(self.chunk_mmr_root);
            let valid = proof.verify(root_node, verification_leaves).map_err(|e| {
                BulkAppendError::InvalidProof(format!("chunk MMR proof verification failed: {}", e))
            })?;

            if !valid {
                return Err(BulkAppendError::InvalidProof(
                    "chunk MMR proof root hash mismatch".to_string(),
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

        // 4. Compute state_root = blake3("bulk_state" || mmr_root || buffer_hash)
        let computed_state_root = compute_state_root(&self.chunk_mmr_root, &self.buffer_hash);

        Ok((
            computed_state_root,
            BulkAppendTreeProofResult {
                chunk_blobs: self.chunk_blobs.clone(),
                buffer_entries: self.buffer_entries.clone(),
                total_count: self.total_count,
                chunk_power: self.chunk_power,
            },
        ))
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
    ///
    /// The bincode size limit is capped at 100 MB to prevent crafted length
    /// headers from causing huge allocations.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BulkAppendError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 100 * 1024 * 1024 }>();
        let (proof, _) = bincode::decode_from_slice(bytes, config).map_err(|e| {
            BulkAppendError::CorruptedData(format!("failed to decode BulkAppendTreeProof: {}", e))
        })?;
        Ok(proof)
    }
}

/// Result of a verified BulkAppendTree proof.
#[derive(Debug, Clone)]
pub struct BulkAppendTreeProofResult {
    /// Chunk blobs overlapping the queried range.
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,
    /// All buffer entries.
    pub buffer_entries: Vec<Vec<u8>>,
    /// Total count of values in the tree.
    pub total_count: u64,
    /// Chunk power (chunk_size = 1 << chunk_power).
    pub chunk_power: u8,
}

impl BulkAppendTreeProofResult {
    /// Extract values in the position range [start, end).
    ///
    /// Collects values from chunk blobs and buffer entries that fall within
    /// the specified range.
    pub fn values_in_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, BulkAppendError> {
        let chunk_size_u64 = (1u32 << self.chunk_power) as u64;
        let completed_chunks = self.total_count / chunk_size_u64;
        let mut result = Vec::new();

        // Extract from chunk blobs
        for (chunk_idx, blob) in &self.chunk_blobs {
            let values = deserialize_chunk_blob(blob).map_err(|e| {
                BulkAppendError::CorruptedData(format!(
                    "failed to deserialize chunk blob {}: {}",
                    chunk_idx, e
                ))
            })?;

            let chunk_start = chunk_idx * chunk_size_u64;
            for (i, value) in values.into_iter().enumerate() {
                let global_pos = chunk_start + i as u64;
                if global_pos >= start && global_pos < end {
                    result.push((global_pos, value));
                }
            }
        }

        // Extract from buffer entries
        let buffer_start = completed_chunks * chunk_size_u64;
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

/// Lazy-loading read-only store for MMR proof generation.
///
/// Fetches nodes on demand from aux storage, caching results. This avoids
/// loading all O(N) MMR nodes when only O(k * log n) are needed.
///
/// Storage errors are captured internally and must be checked after
/// the MMR operation via `take_error()`.
struct LazyMmrNodeStore<'a, F> {
    get_aux: &'a F,
    cache: RefCell<BTreeMap<u64, Option<MmrNode>>>,
    error: RefCell<Option<BulkAppendError>>,
}

impl<'a, F> LazyMmrNodeStore<'a, F>
where
    F: Fn(&[u8]) -> Result<Option<Vec<u8>>, BulkAppendError>,
{
    fn new(get_aux: &'a F) -> Self {
        Self {
            get_aux,
            cache: RefCell::new(BTreeMap::new()),
            error: RefCell::new(None),
        }
    }

    fn take_error(&self) -> Option<BulkAppendError> {
        self.error.borrow_mut().take()
    }
}

impl<F> MMRStoreReadOps for &LazyMmrNodeStore<'_, F>
where
    F: Fn(&[u8]) -> Result<Option<Vec<u8>>, BulkAppendError>,
{
    fn element_at_position(
        &self,
        pos: u64,
    ) -> grovedb_merkle_mountain_range::CostResult<
        Option<MmrNode>,
        grovedb_merkle_mountain_range::Error,
    > {
        if self.error.borrow().is_some() {
            return Ok(None).wrap_with_cost(OperationCost::default());
        }

        let cache = self.cache.borrow();
        if let Some(cached) = cache.get(&pos) {
            return Ok(cached.clone()).wrap_with_cost(OperationCost::default());
        }
        drop(cache);

        let key = grovedb_merkle_mountain_range::mmr_node_key(pos);
        match (self.get_aux)(&key) {
            Ok(Some(bytes)) => match MmrNode::deserialize(&bytes) {
                Ok(node) => {
                    self.cache.borrow_mut().insert(pos, Some(node.clone()));
                    Ok(Some(node)).wrap_with_cost(OperationCost::default())
                }
                Err(e) => {
                    *self.error.borrow_mut() = Some(BulkAppendError::CorruptedData(format!(
                        "failed to deserialize MMR node at pos {}: {}",
                        pos, e
                    )));
                    Ok(None).wrap_with_cost(OperationCost::default())
                }
            },
            Ok(None) => {
                self.cache.borrow_mut().insert(pos, None);
                Ok(None).wrap_with_cost(OperationCost::default())
            }
            Err(e) => {
                *self.error.borrow_mut() = Some(e);
                Ok(None).wrap_with_cost(OperationCost::default())
            }
        }
    }
}

impl<F> MMRStoreWriteOps for &LazyMmrNodeStore<'_, F>
where
    F: Fn(&[u8]) -> Result<Option<Vec<u8>>, BulkAppendError>,
{
    fn append(
        &mut self,
        _pos: u64,
        _elems: Vec<MmrNode>,
    ) -> grovedb_merkle_mountain_range::CostResult<(), grovedb_merkle_mountain_range::Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BulkAppendTree, BulkStore};

    /// Simple BTreeMap-based store for computing MMR root in tests.
    struct BTreeMapStore(BTreeMap<u64, MmrNode>);

    impl MMRStoreReadOps for &BTreeMapStore {
        fn element_at_position(
            &self,
            pos: u64,
        ) -> grovedb_merkle_mountain_range::CostResult<
            Option<MmrNode>,
            grovedb_merkle_mountain_range::Error,
        > {
            Ok(self.0.get(&pos).cloned()).wrap_with_cost(OperationCost::default())
        }
    }

    impl MMRStoreWriteOps for &BTreeMapStore {
        fn append(
            &mut self,
            _pos: u64,
            _elems: Vec<MmrNode>,
        ) -> grovedb_merkle_mountain_range::CostResult<(), grovedb_merkle_mountain_range::Error>
        {
            Ok(()).wrap_with_cost(OperationCost::default())
        }
    }

    /// Helper: build an in-memory BulkAppendTree with N values and return
    /// the state needed for proof generation.
    fn build_test_tree(
        chunk_power: u8,
        values: &[Vec<u8>],
    ) -> (
        u64,                                         // total_count
        u64,                                         // mmr_size
        [u8; 32],                                    // buffer_hash
        [u8; 32],                                    // state_root
        std::collections::HashMap<Vec<u8>, Vec<u8>>, // aux storage
    ) {
        let store = InMemBulkStore::new();
        let mut tree = BulkAppendTree::new(chunk_power).expect("create tree");

        for value in values {
            tree.append(&store, value).expect("append value");
        }

        let mmr_root = if tree.mmr_size() > 0 {
            // Compute MMR root from stored nodes.
            // MMR node keys are 8-byte big-endian positions (no prefix).
            let mut mmr_nodes = BTreeMap::new();
            for (k, v) in store.0.borrow().iter() {
                if k.len() == 8 {
                    let pos = u64::from_be_bytes(k[0..8].try_into().expect("pos bytes"));
                    let node = MmrNode::deserialize(v).expect("deserialize node");
                    mmr_nodes.insert(pos, node);
                }
            }
            let mmr_store = BTreeMapStore(mmr_nodes);
            let mmr = grovedb_merkle_mountain_range::MMR::new(tree.mmr_size(), &mmr_store);
            mmr.get_root().unwrap().expect("get root").hash()
        } else {
            [0u8; 32]
        };
        let state_root = compute_state_root(&mmr_root, &tree.buffer_hash());

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
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(2u8, &values);

        let proof =
            BulkAppendTreeProof::generate(total_count, 2u8, mmr_size, buffer_hash, 0, 3, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        assert!(proof.chunk_blobs.is_empty());
        assert_eq!(proof.buffer_entries.len(), 3);

        let result = proof.verify(&state_root).expect("verify proof");
        let vals = result.values_in_range(0, 3).expect("extract range");
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0], (0, b"val_0".to_vec()));
        assert_eq!(vals[1], (1, b"val_1".to_vec()));
        assert_eq!(vals[2], (2, b"val_2".to_vec()));
    }

    #[test]
    fn test_bulk_proof_chunk_and_buffer() {
        // 6 values with chunk_size=4 (chunk_power=2) -> 1 chunk (0..4) + 2 buffer
        // entries (4..6)
        let values: Vec<Vec<u8>> = (0..6u32)
            .map(|i| format!("data_{}", i).into_bytes())
            .collect();
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(2u8, &values);

        assert_eq!(total_count, 6);

        // Query range 0..6 (all data)
        let proof =
            BulkAppendTreeProof::generate(total_count, 2u8, mmr_size, buffer_hash, 0, 6, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        assert_eq!(proof.chunk_blobs.len(), 1);
        assert_eq!(proof.buffer_entries.len(), 2);

        let result = proof.verify(&state_root).expect("verify proof");
        let vals = result.values_in_range(0, 6).expect("extract range");
        assert_eq!(vals.len(), 6);
        for i in 0..6u32 {
            assert_eq!(vals[i as usize].1, format!("data_{}", i).into_bytes());
        }
    }

    #[test]
    fn test_bulk_proof_multiple_chunks() {
        // 12 values with chunk_size=4 (chunk_power=2) -> 3 chunks
        let values: Vec<Vec<u8>> = (0..12u32)
            .map(|i| format!("e_{}", i).into_bytes())
            .collect();
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(2u8, &values);

        // Query range 2..10 (overlaps chunk 0, 1, 2)
        let proof =
            BulkAppendTreeProof::generate(total_count, 2u8, mmr_size, buffer_hash, 2, 10, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        assert_eq!(proof.chunk_blobs.len(), 3);

        let result = proof.verify(&state_root).expect("verify proof");
        let vals = result.values_in_range(2, 10).expect("extract range");
        assert_eq!(vals.len(), 8);
        assert_eq!(vals[0], (2, b"e_2".to_vec()));
        assert_eq!(vals[7], (9, b"e_9".to_vec()));
    }

    #[test]
    fn test_bulk_proof_wrong_state_root_fails() {
        let values: Vec<Vec<u8>> = (0..4u32).map(|i| format!("x_{}", i).into_bytes()).collect();
        let (total_count, mmr_size, buffer_hash, _state_root, aux) = build_test_tree(2u8, &values);

        let proof =
            BulkAppendTreeProof::generate(total_count, 2u8, mmr_size, buffer_hash, 0, 4, |key| {
                Ok(aux.get(key).cloned())
            })
            .expect("generate proof");

        let wrong_root = [0xFFu8; 32];
        assert!(proof.verify(&wrong_root).is_err());
    }

    #[test]
    fn test_bulk_proof_encode_decode() {
        let values: Vec<Vec<u8>> = (0..5u32).map(|i| format!("r_{}", i).into_bytes()).collect();
        let (total_count, mmr_size, buffer_hash, state_root, aux) = build_test_tree(2u8, &values);

        let proof =
            BulkAppendTreeProof::generate(total_count, 2u8, mmr_size, buffer_hash, 0, 5, |key| {
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
