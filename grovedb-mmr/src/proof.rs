//! MMR tree proof generation and verification.
//!
//! Generates proofs that specific leaf values exist in an MMR.
//! The proof ties into the GroveDB hierarchy: the parent Merk proves the
//! MMR element bytes (containing the mmr_root), and this proof shows
//! that queried leaves are consistent with that root.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

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
        if leaf_indices.is_empty() {
            return Err(MmrError::InvalidInput(
                "leaf_indices must not be empty".into(),
            ));
        }

        let leaf_count = mmr_size_to_leaf_count(mmr_size);

        // Validate indices and reject duplicates
        let mut seen_indices = BTreeSet::new();
        for &idx in leaf_indices {
            if idx >= leaf_count {
                return Err(MmrError::InvalidInput(format!(
                    "MMR leaf index {} out of range (leaf_count={})",
                    idx, leaf_count
                )));
            }
            if !seen_indices.insert(idx) {
                return Err(MmrError::InvalidInput(format!(
                    "duplicate leaf index {}",
                    idx
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

        // Lazy-loading store: only fetches nodes needed by proof generation
        let store = LazyNodeStore::new(&get_node);

        // Generate the ckb MerkleProof
        let mmr = crate::MMR::<MmrNode, MergeBlake3, _>::new(mmr_size, &store);
        let proof_result = mmr.gen_proof(positions);

        // Check deferred storage errors first — if the store failed, the ckb
        // error (e.g. InconsistentStore) is a symptom, not the root cause.
        if let Some(err) = store.take_error() {
            return Err(err);
        }
        let proof = proof_result
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
        if self.leaves.is_empty() {
            return Err(MmrError::InvalidProof(
                "proof contains no leaves to verify".into(),
            ));
        }

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

        // Deduplicate by leaf_index: the ckb library deduplicates positions
        // internally, but self.leaves may contain duplicate indices that were
        // not independently verified. Only return unique leaf entries.
        let mut seen = BTreeSet::new();
        let verified_leaves: Vec<(u64, Vec<u8>)> = self
            .leaves
            .iter()
            .filter(|(idx, _)| seen.insert(*idx))
            .cloned()
            .collect();
        Ok(verified_leaves)
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
    ///
    /// The bincode size limit is capped at 100 MiB to prevent
    /// crafted length headers from causing huge allocations.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, MmrError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 100 * 1024 * 1024 }>();
        let (proof, _) = bincode::decode_from_slice(bytes, config)
            .map_err(|e| MmrError::InvalidData(format!("failed to decode MmrTreeProof: {}", e)))?;
        Ok(proof)
    }
}

/// Lazy-loading read-only store for MMR proof generation.
///
/// Instead of eagerly loading all O(N) nodes, this store fetches nodes
/// on demand via the provided closure and caches them. Proof generation
/// only needs O(k * log n) nodes for k proved leaves, so this is a
/// significant improvement for large MMRs.
///
/// Storage errors are captured internally and must be checked after
/// the MMR operation completes via `take_error()`.
struct LazyNodeStore<F> {
    get_node: F,
    cache: RefCell<BTreeMap<u64, Option<MmrNode>>>,
    error: RefCell<Option<MmrError>>,
}

impl<F> LazyNodeStore<F>
where
    F: Fn(u64) -> Result<Option<MmrNode>, MmrError>,
{
    fn new(get_node: F) -> Self {
        Self {
            get_node,
            cache: RefCell::new(BTreeMap::new()),
            error: RefCell::new(None),
        }
    }

    /// Take any deferred storage error that occurred during reads.
    fn take_error(&self) -> Option<MmrError> {
        self.error.borrow_mut().take()
    }
}

impl<F> MMRStoreReadOps<MmrNode> for &LazyNodeStore<F>
where
    F: Fn(u64) -> Result<Option<MmrNode>, MmrError>,
{
    fn get_elem(&self, pos: u64) -> crate::CkbResult<Option<MmrNode>> {
        // If a previous read already failed, short-circuit
        if self.error.borrow().is_some() {
            return Ok(None);
        }

        let cache = self.cache.borrow();
        if let Some(cached) = cache.get(&pos) {
            return Ok(cached.clone());
        }
        drop(cache);

        match (self.get_node)(pos) {
            Ok(result) => {
                self.cache.borrow_mut().insert(pos, result.clone());
                Ok(result)
            }
            Err(e) => {
                *self.error.borrow_mut() = Some(e);
                Ok(None)
            }
        }
    }
}

impl<F> MMRStoreWriteOps<MmrNode> for &LazyNodeStore<F>
where
    F: Fn(u64) -> Result<Option<MmrNode>, MmrError>,
{
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

    #[test]
    fn test_generate_rejects_duplicate_leaf_indices() {
        let mut mmr = crate::GroveMmr::new();
        for i in 0..5u64 {
            mmr.push(format!("leaf_{}", i).into_bytes())
                .expect("push leaf");
        }
        let mmr_size = mmr.mmr_size();

        let result = MmrTreeProof::generate(mmr_size, &[1, 3, 1], get_node_from_mmr(mmr.store()));
        assert!(result.is_err(), "should reject duplicate leaf indices");
        let err_msg = format!("{}", result.expect_err("should be a duplicate index error"));
        assert!(
            err_msg.contains("duplicate"),
            "error should mention 'duplicate': {}",
            err_msg
        );
    }

    #[test]
    fn test_verify_deduplicates_tampered_leaves() {
        // Simulate a tampered proof with duplicate leaf entries.
        // Even though the ckb library deduplicates internally, verify()
        // must not return duplicate entries.
        let mut mmr = crate::GroveMmr::new();
        for i in 0..5u64 {
            mmr.push(format!("val_{}", i).into_bytes())
                .expect("push value");
        }
        let root = mmr.root_hash().expect("root hash");
        let mmr_size = mmr.mmr_size();

        // Generate a valid proof for leaf 2
        let valid_proof = MmrTreeProof::generate(mmr_size, &[2], get_node_from_mmr(mmr.store()))
            .expect("generate proof");

        // Construct a tampered proof with duplicated leaf
        let tampered = MmrTreeProof {
            mmr_size: valid_proof.mmr_size,
            leaves: vec![
                (2, b"val_2".to_vec()),
                (2, b"val_2".to_vec()), // duplicate
            ],
            proof_items: valid_proof.proof_items.clone(),
        };

        let result = tampered.verify(&root).expect("verify should succeed");
        assert_eq!(
            result.len(),
            1,
            "verify must deduplicate: got {} entries",
            result.len()
        );
        assert_eq!(result[0], (2, b"val_2".to_vec()));
    }

    #[test]
    fn test_verify_rejects_forged_value_in_duplicate() {
        // A more dangerous attack: duplicate leaf index with a different value.
        // The first entry is the real value that passes ckb verification;
        // the duplicate has a forged value. After deduplication, only the
        // first (verified) entry should remain.
        let mut mmr = crate::GroveMmr::new();
        for i in 0..5u64 {
            mmr.push(format!("val_{}", i).into_bytes())
                .expect("push value");
        }
        let root = mmr.root_hash().expect("root hash");
        let mmr_size = mmr.mmr_size();

        let valid_proof = MmrTreeProof::generate(mmr_size, &[2], get_node_from_mmr(mmr.store()))
            .expect("generate proof");

        // Duplicate with forged value — the second entry is unverified
        let tampered = MmrTreeProof {
            mmr_size: valid_proof.mmr_size,
            leaves: vec![
                (2, b"val_2".to_vec()),
                (2, b"FORGED".to_vec()), // forged value
            ],
            proof_items: valid_proof.proof_items.clone(),
        };

        let result = tampered.verify(&root).expect("verify should succeed");
        assert_eq!(result.len(), 1, "should deduplicate to 1 entry");
        assert_eq!(
            result[0].1,
            b"val_2".to_vec(),
            "should keep verified value, not forged"
        );
    }

    #[test]
    fn test_domain_separation_leaf_vs_merge() {
        // Verify that a 64-byte leaf value does NOT produce the same hash
        // as an internal node merging two 32-byte child hashes.
        // This is the core property that domain separation provides.
        use crate::{blake3_merge, leaf_hash};

        let left_hash = [0xAAu8; 32];
        let right_hash = [0xBBu8; 32];

        // Internal merge hash
        let merge_hash = blake3_merge(&left_hash, &right_hash);

        // Create a 64-byte value that equals left_hash || right_hash
        let mut fake_value = Vec::with_capacity(64);
        fake_value.extend_from_slice(&left_hash);
        fake_value.extend_from_slice(&right_hash);
        let leaf_hash_result = leaf_hash(&fake_value);

        assert_ne!(
            merge_hash, leaf_hash_result,
            "domain separation must prevent leaf/internal hash collision"
        );
    }

    #[test]
    fn test_lazy_store_caches_reads() {
        // Verify the lazy store caches on first read and reuses on second.
        use std::sync::atomic::{AtomicU64, Ordering};

        let mut mmr = crate::GroveMmr::new();
        for i in 0..5u64 {
            mmr.push(format!("val_{}", i).into_bytes())
                .expect("push value");
        }

        let call_count = AtomicU64::new(0);
        let mmr_store = mmr.store();
        let counted_get = |pos: u64| -> Result<Option<MmrNode>, MmrError> {
            call_count.fetch_add(1, Ordering::Relaxed);
            mmr_store
                .get_elem(pos)
                .map_err(|e| MmrError::OperationFailed(format!("{}", e)))
        };

        let store = LazyNodeStore::new(counted_get);

        // First access
        let node1: Option<MmrNode> = MMRStoreReadOps::get_elem(&&store, 0).expect("get_elem first");
        assert!(node1.is_some(), "node at position 0 should exist");
        assert_eq!(call_count.load(Ordering::Relaxed), 1);

        // Second access — should hit cache
        let node2: Option<MmrNode> =
            MMRStoreReadOps::get_elem(&&store, 0).expect("get_elem second");
        assert!(node2.is_some(), "cached node should exist");
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "second read should use cache, not call closure again"
        );
    }

    #[test]
    fn test_generate_rejects_empty_leaf_indices() {
        let mut mmr = crate::GroveMmr::new();
        mmr.push(b"data".to_vec()).expect("push data");
        let mmr_size = mmr.mmr_size();

        let result = MmrTreeProof::generate(mmr_size, &[], get_node_from_mmr(mmr.store()));
        assert!(result.is_err(), "should reject empty leaf indices");
        let err_msg = format!("{}", result.expect_err("should be an empty input error"));
        assert!(
            err_msg.contains("must not be empty"),
            "error should mention empty: {}",
            err_msg
        );
    }

    #[test]
    fn test_verify_rejects_empty_leaves() {
        // Construct a proof with no leaves — should fail verification
        let tampered = MmrTreeProof {
            mmr_size: 1,
            leaves: vec![],
            proof_items: vec![],
        };
        let root = [0u8; 32];
        let result = tampered.verify(&root);
        assert!(result.is_err(), "should reject proof with no leaves");
        let err_msg = format!("{}", result.expect_err("should be an empty proof error"));
        assert!(
            err_msg.contains("no leaves"),
            "error should mention no leaves: {}",
            err_msg
        );
    }
}
