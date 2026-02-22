//! MMR proof types: generic `MerkleProof` and GroveDB-specific `MmrTreeProof`.
//!
//! `MerkleProof` is the MMR inclusion proof that works with `MmrNode` elements
//! and Blake3 merging. `MmrTreeProof` wraps it for GroveDB integration with
//! serialization and lazy storage loading.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    mem,
};

use bincode::{Decode, Encode};
use grovedb_costs::{CostResult, CostsExt, OperationCost};

use crate::{
    Error, MMRStoreReadOps, MMRStoreWriteOps, MmrNode, Result,
    helper::{
        get_peak_map, get_peaks, leaf_index_to_mmr_size, leaf_index_to_pos,
        leaf_index_to_pos as leaf_to_pos, mmr_size_to_leaf_count, parent_offset,
        pos_height_in_tree, sibling_offset,
    },
    leaf_hash,
    mmr::bag_peaks,
};

// =============================================================================
// MerkleProof
// =============================================================================

/// An MMR Merkle inclusion proof.
///
/// Contains the sibling/peak hashes needed to recompute the root from a set
/// of leaf positions. This is the proof object produced by
/// [`MMR::gen_proof`](crate::MMR::gen_proof); see [`MmrTreeProof`] for the
/// GroveDB-specific serializable wrapper.
#[derive(Debug)]
pub struct MerkleProof {
    mmr_size: u64,
    proof: Vec<MmrNode>,
}

impl MerkleProof {
    /// Construct a proof from pre-computed proof items.
    pub fn new(mmr_size: u64, proof: Vec<MmrNode>) -> Self {
        MerkleProof { mmr_size, proof }
    }

    /// The MMR size at the time this proof was generated.
    pub fn mmr_size(&self) -> u64 {
        self.mmr_size
    }

    /// The raw proof items (sibling/peak hashes).
    pub fn proof_items(&self) -> &[MmrNode] {
        &self.proof
    }

    /// Recompute the MMR root from the given leaves and this proof's items.
    pub fn calculate_root(&self, leaves: Vec<(u64, MmrNode)>) -> Result<MmrNode> {
        calculate_root(leaves, self.mmr_size, self.proof.iter())
    }

    /// From a proof of leaf `n`, calculate the root of `n + 1` leaves.
    ///
    /// Uses the MMR construction graph to extend the proof with one new
    /// leaf without regenerating from scratch.
    /// See <https://github.com/jjyr/merkle-mountain-range#construct>.
    pub fn calculate_root_with_new_leaf(
        &self,
        mut leaves: Vec<(u64, MmrNode)>,
        new_pos: u64,
        new_elem: MmrNode,
        new_mmr_size: u64,
    ) -> Result<MmrNode> {
        if new_pos >= new_mmr_size {
            return Err(Error::InvalidInput(format!(
                "new_pos {} must be less than new_mmr_size {}",
                new_pos, new_mmr_size
            )));
        }
        let pos_height = pos_height_in_tree(new_pos);
        let next_height = pos_height_in_tree(new_pos + 1);
        if next_height > pos_height {
            let mut peaks_hashes =
                calculate_peaks_hashes(leaves, self.mmr_size, self.proof.iter())?;
            let peaks_pos = get_peaks(new_mmr_size);
            let i = peaks_pos
                .iter()
                .position(|p| *p >= new_pos)
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "new_pos {} exceeds all peaks for new_mmr_size {}",
                        new_pos, new_mmr_size
                    ))
                })?;
            if i > peaks_hashes.len() {
                return Err(Error::InvalidInput(format!(
                    "peak index {} out of range for {} peak hashes",
                    i,
                    peaks_hashes.len()
                )));
            }
            peaks_hashes[i..].reverse();
            calculate_root(vec![(new_pos, new_elem)], new_mmr_size, peaks_hashes.iter())
        } else {
            leaves.push((new_pos, new_elem));
            calculate_root(leaves, new_mmr_size, self.proof.iter())
        }
    }

    /// Verify that the given leaves produce the expected `root`.
    pub fn verify(&self, root: MmrNode, leaves: Vec<(u64, MmrNode)>) -> Result<bool> {
        self.calculate_root(leaves)
            .map(|calculated_root| calculated_root == root)
    }

    /// Verify an old root and all incremental leaves.
    ///
    /// If this method returns `true`, it means the following assertion are
    /// true:
    /// - The old root could be generated in the history of the current MMR.
    /// - All incremental leaves are on the current MMR.
    /// - The MMR, which could generate the old root, appends all incremental
    ///   leaves, becomes the current MMR.
    pub fn verify_incremental(
        &self,
        root: MmrNode,
        prev_root: MmrNode,
        incremental: Vec<MmrNode>,
    ) -> Result<bool> {
        let current_leaves_count = get_peak_map(self.mmr_size);
        if current_leaves_count <= incremental.len() as u64 {
            return Err(Error::InvalidProof(
                "incremental leaves exceed current leaf count".into(),
            ));
        }
        // Test if previous root is correct.
        let prev_leaves_count = current_leaves_count - incremental.len() as u64;
        let prev_peaks_positions = {
            let prev_index = prev_leaves_count - 1;
            let prev_mmr_size = leaf_index_to_mmr_size(prev_index);
            let prev_peaks_positions = get_peaks(prev_mmr_size);
            if prev_peaks_positions.len() != self.proof.len() {
                return Err(Error::InvalidProof(
                    "proof item count does not match previous peak count".into(),
                ));
            }
            prev_peaks_positions
        };
        let current_peaks_positions = get_peaks(self.mmr_size);

        let mut reverse_index = prev_peaks_positions.len() - 1;
        for (i, (prev_pos, cur_pos)) in prev_peaks_positions
            .iter()
            .zip(current_peaks_positions.iter())
            .enumerate()
        {
            if prev_pos < cur_pos {
                reverse_index = i;
                break;
            }
        }
        let mut prev_peaks: Vec<_> = self.proof_items().to_vec();
        let mut reverse_peaks = prev_peaks.split_off(reverse_index);
        reverse_peaks.reverse();
        prev_peaks.extend(reverse_peaks);

        let calculated_prev_root =
            bag_peaks(prev_peaks)?.ok_or(Error::InvalidProof("no peaks to bag".into()))?;
        if calculated_prev_root != prev_root {
            return Ok(false);
        }

        // Test if incremental leaves are correct.
        let leaves = incremental
            .into_iter()
            .enumerate()
            .map(|(index, leaf)| {
                let pos = leaf_index_to_pos(prev_leaves_count + index as u64);
                (pos, leaf)
            })
            .collect();
        self.verify(root, leaves)
    }
}

fn calculate_peak_root<'a, I: Iterator<Item = &'a MmrNode>>(
    leaves: Vec<(u64, MmrNode)>,
    peak_pos: u64,
    proof_iter: &mut I,
) -> Result<MmrNode> {
    debug_assert!(!leaves.is_empty(), "can't be empty");

    let mut queue: VecDeque<_> = leaves
        .into_iter()
        .map(|(pos, item)| (pos, item, 0))
        .collect();

    // calculate tree root from each items
    while let Some((pos, item, height)) = queue.pop_front() {
        if pos == peak_pos {
            if queue.is_empty() {
                // return root once queue is consumed
                return Ok(item);
            } else {
                return Err(Error::InvalidProof(
                    "queue not empty after reaching peak position".into(),
                ));
            }
        }
        // calculate sibling
        let next_height = pos_height_in_tree(pos + 1);
        let (parent_pos, parent_item) = {
            let sibling_offset = sibling_offset(height);
            if next_height > height {
                // implies pos is right sibling
                let sib_pos = pos - sibling_offset;
                let parent_pos = pos + 1;
                let parent_item = if Some(&sib_pos) == queue.front().map(|(pos, ..)| pos) {
                    let sibling_item = queue.pop_front().map(|(_, item, _)| item).unwrap();
                    MmrNode::merge(&sibling_item, &item)
                } else {
                    let sibling_item = proof_iter.next().ok_or(Error::InvalidProof(
                        "not enough proof items for right sibling".into(),
                    ))?;
                    MmrNode::merge(sibling_item, &item)
                };
                (parent_pos, parent_item)
            } else {
                // pos is left sibling
                let sib_pos = pos + sibling_offset;
                let parent_pos = pos + parent_offset(height);
                let parent_item = if Some(&sib_pos) == queue.front().map(|(pos, ..)| pos) {
                    let sibling_item = queue.pop_front().map(|(_, item, _)| item).unwrap();
                    MmrNode::merge(&item, &sibling_item)
                } else {
                    let sibling_item = proof_iter.next().ok_or(Error::InvalidProof(
                        "not enough proof items for left sibling".into(),
                    ))?;
                    MmrNode::merge(&item, sibling_item)
                };
                (parent_pos, parent_item)
            }
        };

        if parent_pos <= peak_pos {
            queue.push_back((parent_pos, parent_item, height + 1))
        } else {
            return Err(Error::InvalidProof(
                "parent position exceeds peak position".into(),
            ));
        }
    }
    Err(Error::InvalidProof(
        "queue exhausted without reaching peak".into(),
    ))
}

fn calculate_peaks_hashes<'a, I: Iterator<Item = &'a MmrNode>>(
    mut leaves: Vec<(u64, MmrNode)>,
    mmr_size: u64,
    mut proof_iter: I,
) -> Result<Vec<MmrNode>> {
    if leaves.iter().any(|(pos, _)| pos_height_in_tree(*pos) > 0) {
        return Err(Error::NodeProofsNotSupported);
    }

    // special handle the only 1 leaf MMR
    if mmr_size == 1 && leaves.len() == 1 && leaves[0].0 == 0 {
        return Ok(leaves.into_iter().map(|(_pos, item)| item).collect());
    }
    // ensure leaves are sorted and unique
    leaves.sort_by_key(|(pos, _)| *pos);
    leaves.dedup_by(|a, b| a.0 == b.0);
    let peaks = get_peaks(mmr_size);

    let mut peaks_hashes: Vec<MmrNode> = Vec::with_capacity(peaks.len() + 1);
    for peak_pos in peaks {
        let mut leaves: Vec<_> = take_while_vec(&mut leaves, |(pos, _)| *pos <= peak_pos);
        let peak_root = if leaves.len() == 1 && leaves[0].0 == peak_pos {
            // leaf is the peak
            leaves.remove(0).1
        } else if leaves.is_empty() {
            // if empty, means the next proof is a peak root or rhs bagged root
            if let Some(peak_root) = proof_iter.next() {
                peak_root.clone()
            } else {
                // means that either all right peaks are bagged, or proof is corrupted
                // so we break loop and check no items left
                break;
            }
        } else {
            calculate_peak_root(leaves, peak_pos, &mut proof_iter)?
        };
        peaks_hashes.push(peak_root.clone());
    }

    // ensure nothing left in leaves
    if !leaves.is_empty() {
        return Err(Error::InvalidProof("unprocessed leaves remain".into()));
    }

    // check rhs peaks
    if let Some(rhs_peaks_hashes) = proof_iter.next() {
        peaks_hashes.push(rhs_peaks_hashes.clone());
    }
    // ensure nothing left in proof_iter
    if proof_iter.next().is_some() {
        return Err(Error::InvalidProof(
            "excess proof items after processing all peaks".into(),
        ));
    }
    Ok(peaks_hashes)
}

/// merkle proof
/// 1. sort items by position
/// 2. calculate root of each peak
/// 3. bagging peaks
fn calculate_root<'a, I: Iterator<Item = &'a MmrNode>>(
    leaves: Vec<(u64, MmrNode)>,
    mmr_size: u64,
    proof_iter: I,
) -> Result<MmrNode> {
    let peaks_hashes = calculate_peaks_hashes(leaves, mmr_size, proof_iter)?;
    bag_peaks(peaks_hashes)?.ok_or(Error::InvalidProof("no peaks to bag".into()))
}

/// Drain elements from the front of `v` while `p` returns true.
pub(crate) fn take_while_vec<T, P: Fn(&T) -> bool>(v: &mut Vec<T>, p: P) -> Vec<T> {
    for i in 0..v.len() {
        if !p(&v[i]) {
            return v.drain(..i).collect();
        }
    }
    mem::take(v)
}

// =============================================================================
// GroveDB-specific MmrTreeProof
// =============================================================================

/// Verified leaf entries: `(leaf_index, value_bytes)` pairs.
pub type VerifiedLeaves = Vec<(u64, Vec<u8>)>;

/// A proof that specific leaves exist in an MMR tree.
///
/// Contains the MMR size, the proved leaf values with their indices,
/// and the sibling/peak hashes needed for verification.
#[derive(Debug, Clone, Encode, Decode)]
pub struct MmrTreeProof {
    mmr_size: u64,
    leaves: Vec<(u64, Vec<u8>)>,
    proof_items: Vec<[u8; 32]>,
}

impl MmrTreeProof {
    /// Create a proof from its constituent parts.
    pub fn new(mmr_size: u64, leaves: Vec<(u64, Vec<u8>)>, proof_items: Vec<[u8; 32]>) -> Self {
        Self {
            mmr_size,
            leaves,
            proof_items,
        }
    }

    /// The MMR size at proof generation time.
    pub fn mmr_size(&self) -> u64 {
        self.mmr_size
    }

    /// The proved leaves as `(leaf_index, value_bytes)` pairs.
    pub fn leaves(&self) -> &[(u64, Vec<u8>)] {
        &self.leaves
    }

    /// The sibling/peak hashes from the MMR proof (32 bytes each).
    pub fn proof_items(&self) -> &[[u8; 32]] {
        &self.proof_items
    }

    /// Generate an MMR proof for the given leaf indices.
    ///
    /// Reads nodes from storage via the provided closure and generates
    /// a MerkleProof for the requested positions.
    ///
    /// # Arguments
    /// * `mmr_size` - The MMR size from the element
    /// * `leaf_indices` - 0-based leaf indices to prove
    /// * `get_node` - Closure to read an MmrNode by MMR position from storage
    pub fn generate<F>(mmr_size: u64, leaf_indices: &[u64], get_node: F) -> Result<Self>
    where
        F: Fn(u64) -> Result<Option<MmrNode>>,
    {
        if leaf_indices.is_empty() {
            return Err(Error::InvalidInput("leaf_indices must not be empty".into()));
        }

        let leaf_count = mmr_size_to_leaf_count(mmr_size);

        // Validate indices and reject duplicates
        let mut seen_indices = BTreeSet::new();
        for &idx in leaf_indices {
            if idx >= leaf_count {
                return Err(Error::InvalidInput(format!(
                    "MMR leaf index {} out of range (leaf_count={})",
                    idx, leaf_count
                )));
            }
            if !seen_indices.insert(idx) {
                return Err(Error::InvalidInput(format!("duplicate leaf index {}", idx)));
            }
        }

        // Convert leaf indices to MMR positions
        let positions: Vec<u64> = leaf_indices.iter().map(|&idx| leaf_to_pos(idx)).collect();

        // Collect leaf values
        let mut leaves = Vec::with_capacity(leaf_indices.len());
        for &idx in leaf_indices {
            let pos = leaf_to_pos(idx);
            let node = get_node(pos)?.ok_or_else(|| {
                Error::InvalidData(format!(
                    "MMR leaf node missing at position {} (leaf index {})",
                    pos, idx
                ))
            })?;
            let value = node.into_value().ok_or_else(|| {
                Error::InvalidData(format!(
                    "MMR node at position {} is internal, expected leaf",
                    pos
                ))
            })?;
            leaves.push((idx, value));
        }

        // Lazy-loading store: only fetches nodes needed by proof generation
        let store = LazyNodeStore::new(&get_node);

        // Generate the MerkleProof (drops zero costs from lazy store)
        let mmr = crate::MMR::new(mmr_size, &store);
        let proof_result = mmr.gen_proof(positions).unwrap();

        // Check deferred storage errors first — if the store failed, the
        // error (e.g. InconsistentStore) is a symptom, not the root cause.
        if let Some(err) = store.take_error() {
            return Err(err);
        }
        let proof = proof_result
            .map_err(|e| Error::OperationFailed(format!("MMR gen_proof failed: {}", e)))?;

        // Extract proof item hashes
        let proof_items: Vec<[u8; 32]> =
            proof.proof_items().iter().map(|node| node.hash()).collect();

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
    pub fn verify(&self, expected_mmr_root: &[u8; 32]) -> Result<VerifiedLeaves> {
        if self.leaves.is_empty() {
            return Err(Error::InvalidProof(
                "proof contains no leaves to verify".into(),
            ));
        }

        // Validate leaf indices to prevent arithmetic overflow in
        // leaf_index_to_pos / leaf_index_to_mmr_size.
        let leaf_count = mmr_size_to_leaf_count(self.mmr_size);
        for (idx, _) in &self.leaves {
            if *idx >= leaf_count {
                return Err(Error::InvalidProof(format!(
                    "leaf index {} out of range for mmr_size {} (leaf_count {})",
                    idx, self.mmr_size, leaf_count
                )));
            }
        }

        // Reconstruct proof items as MmrNodes (internal, hash-only)
        let proof_nodes: Vec<MmrNode> = self
            .proof_items
            .iter()
            .map(|hash| MmrNode::internal(*hash))
            .collect();

        // Reconstruct the MerkleProof
        let proof = MerkleProof::new(self.mmr_size, proof_nodes);

        // Build leaf entries for verification: (mmr_position, MmrNode)
        // Only the hash matters for verification (PartialEq + Merge use hash
        // only), so we compute leaf_hash without cloning the value bytes.
        let verification_leaves: Vec<(u64, MmrNode)> = self
            .leaves
            .iter()
            .map(|(idx, value)| {
                let pos = leaf_to_pos(*idx);
                let node = MmrNode::internal(leaf_hash(value));
                (pos, node)
            })
            .collect();

        // Verify against the expected root
        let root_node = MmrNode::internal(*expected_mmr_root);
        let valid = proof
            .verify(root_node, verification_leaves)
            .map_err(|e| Error::InvalidProof(format!("MMR proof verification failed: {}", e)))?;

        if !valid {
            return Err(Error::InvalidProof(
                "MMR proof root hash mismatch".to_string(),
            ));
        }

        // Deduplicate by leaf_index: the library deduplicates positions
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

    /// Verify the proof and return the computed MMR root hash along with the
    /// verified leaves.
    ///
    /// Unlike [`verify`](Self::verify), this does NOT check against an expected
    /// root — the caller is responsible for validating the root (typically via
    /// the Merk child hash mechanism).
    pub fn verify_and_get_root(&self) -> Result<([u8; 32], VerifiedLeaves)> {
        if self.leaves.is_empty() {
            return Err(Error::InvalidProof(
                "proof contains no leaves to verify".into(),
            ));
        }

        // Validate leaf indices to prevent arithmetic overflow in
        // leaf_index_to_pos / leaf_index_to_mmr_size.
        let leaf_count = mmr_size_to_leaf_count(self.mmr_size);
        for (idx, _) in &self.leaves {
            if *idx >= leaf_count {
                return Err(Error::InvalidProof(format!(
                    "leaf index {} out of range for mmr_size {} (leaf_count {})",
                    idx, self.mmr_size, leaf_count
                )));
            }
        }

        // Reconstruct proof items as MmrNodes (internal, hash-only)
        let proof_nodes: Vec<MmrNode> = self
            .proof_items
            .iter()
            .map(|hash| MmrNode::internal(*hash))
            .collect();

        let proof = MerkleProof::new(self.mmr_size, proof_nodes);

        // Build leaf entries: (mmr_position, MmrNode)
        let verification_leaves: Vec<(u64, MmrNode)> = self
            .leaves
            .iter()
            .map(|(idx, value)| {
                let pos = leaf_to_pos(*idx);
                let node = MmrNode::internal(leaf_hash(value));
                (pos, node)
            })
            .collect();

        // Calculate root from the proof (no expected root to compare against)
        let root = proof.calculate_root(verification_leaves).map_err(|e| {
            Error::InvalidProof(format!("MMR proof root calculation failed: {}", e))
        })?;

        // Deduplicate by leaf_index
        let mut seen = BTreeSet::new();
        let verified_leaves: Vec<(u64, Vec<u8>)> = self
            .leaves
            .iter()
            .filter(|(idx, _)| seen.insert(*idx))
            .cloned()
            .collect();

        Ok((root.hash(), verified_leaves))
    }

    /// Serialize this proof to bytes using bincode.
    pub fn encode_to_vec(&self) -> Result<Vec<u8>> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        bincode::encode_to_vec(self, config)
            .map_err(|e| Error::InvalidData(format!("failed to encode MmrTreeProof: {}", e)))
    }

    /// Deserialize a proof from bytes.
    ///
    /// The bincode size limit is capped at 100 MiB to prevent
    /// crafted length headers from causing huge allocations.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 100 * 1024 * 1024 }>();
        let (proof, _) = bincode::decode_from_slice(bytes, config)
            .map_err(|e| Error::InvalidData(format!("failed to decode MmrTreeProof: {}", e)))?;
        Ok(proof)
    }
}

// =============================================================================
// LazyNodeStore
// =============================================================================

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
    error: RefCell<Option<Error>>,
}

impl<F> LazyNodeStore<F>
where
    F: Fn(u64) -> Result<Option<MmrNode>>,
{
    fn new(get_node: F) -> Self {
        Self {
            get_node,
            cache: RefCell::new(BTreeMap::new()),
            error: RefCell::new(None),
        }
    }

    /// Take any deferred storage error that occurred during reads.
    fn take_error(&self) -> Option<Error> {
        self.error.borrow_mut().take()
    }
}

impl<F> MMRStoreReadOps for &LazyNodeStore<F>
where
    F: Fn(u64) -> Result<Option<MmrNode>>,
{
    fn element_at_position(&self, pos: u64) -> CostResult<Option<MmrNode>, crate::Error> {
        // If a previous read already failed, short-circuit
        if self.error.borrow().is_some() {
            return Ok(None).wrap_with_cost(OperationCost::default());
        }

        let cache = self.cache.borrow();
        if let Some(cached) = cache.get(&pos) {
            return Ok(cached.clone()).wrap_with_cost(OperationCost::default());
        }
        drop(cache);

        match (self.get_node)(pos) {
            Ok(result) => {
                self.cache.borrow_mut().insert(pos, result.clone());
                Ok(result).wrap_with_cost(OperationCost::default())
            }
            Err(e) => {
                *self.error.borrow_mut() = Some(e);
                Ok(None).wrap_with_cost(OperationCost::default())
            }
        }
    }
}

impl<F> MMRStoreWriteOps for &LazyNodeStore<F>
where
    F: Fn(u64) -> Result<Option<MmrNode>>,
{
    fn append(&mut self, _pos: u64, _elems: Vec<MmrNode>) -> CostResult<(), crate::Error> {
        // Read-only store — proof generation doesn't need writes
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MMR, MMRStoreReadOps, mem_store::MemStore};

    /// Push MmrNode leaves into a MemStore and return (store, mmr_size).
    fn build_mmr(values: &[&[u8]]) -> (MemStore, u64) {
        let store = MemStore::default();
        let mut mmr = MMR::new(0, &store);
        for v in values {
            mmr.push(MmrNode::leaf(v.to_vec()))
                .unwrap()
                .expect("push should succeed");
        }
        mmr.commit().unwrap().expect("commit should succeed");
        let size = mmr.mmr_size;
        (store, size)
    }

    /// Get root hash from a MemStore + mmr_size.
    fn root_hash(store: &MemStore, mmr_size: u64) -> [u8; 32] {
        let mmr = MMR::new(mmr_size, store);
        mmr.get_root()
            .unwrap()
            .expect("get_root should succeed")
            .hash()
    }

    /// Create a get_node closure from a MemStore.
    fn get_node_from_store(store: &MemStore) -> impl Fn(u64) -> Result<Option<MmrNode>> + '_ {
        move |pos| {
            store
                .element_at_position(pos)
                .unwrap()
                .map_err(|e| Error::OperationFailed(format!("element_at_position: {}", e)))
        }
    }

    #[test]
    fn test_mmr_proof_roundtrip_single_leaf() {
        let (store, mmr_size) = build_mmr(&[b"leaf_0", b"leaf_1", b"leaf_2", b"leaf_3", b"leaf_4"]);
        let root = root_hash(&store, mmr_size);

        let proof = MmrTreeProof::generate(mmr_size, &[2], get_node_from_store(&store))
            .expect("generate proof");

        assert_eq!(proof.leaves().len(), 1);
        assert_eq!(proof.leaves()[0].0, 2);
        assert_eq!(proof.leaves()[0].1, b"leaf_2".to_vec());

        let verified = proof.verify(&root).expect("verify proof");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].1, b"leaf_2".to_vec());
    }

    #[test]
    fn test_mmr_proof_roundtrip_multiple_leaves() {
        let values: Vec<Vec<u8>> = (0..10u64)
            .map(|i| format!("val_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
        let (store, mmr_size) = build_mmr(&refs);
        let root = root_hash(&store, mmr_size);

        let proof = MmrTreeProof::generate(mmr_size, &[1, 5, 8], get_node_from_store(&store))
            .expect("generate proof");

        assert_eq!(proof.leaves().len(), 3);

        let verified = proof.verify(&root).expect("verify proof");
        assert_eq!(verified[0], (1, b"val_1".to_vec()));
        assert_eq!(verified[1], (5, b"val_5".to_vec()));
        assert_eq!(verified[2], (8, b"val_8".to_vec()));
    }

    #[test]
    fn test_mmr_proof_wrong_root_fails() {
        let values: Vec<Vec<u8>> = (0..5u64)
            .map(|i| format!("data_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
        let (store, mmr_size) = build_mmr(&refs);

        let proof = MmrTreeProof::generate(mmr_size, &[0], get_node_from_store(&store))
            .expect("generate proof");

        let wrong_root = [0xFFu8; 32];
        assert!(proof.verify(&wrong_root).is_err());
    }

    #[test]
    fn test_mmr_proof_encode_decode() {
        let (store, mmr_size) = build_mmr(&[b"item_0", b"item_1", b"item_2"]);
        let root = root_hash(&store, mmr_size);

        let proof = MmrTreeProof::generate(mmr_size, &[0, 2], get_node_from_store(&store))
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
        let (store, mmr_size) = build_mmr(&[b"only"]);

        let result = MmrTreeProof::generate(mmr_size, &[5], get_node_from_store(&store));
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_rejects_duplicate_leaf_indices() {
        let values: Vec<Vec<u8>> = (0..5u64)
            .map(|i| format!("leaf_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
        let (store, mmr_size) = build_mmr(&refs);

        let result = MmrTreeProof::generate(mmr_size, &[1, 3, 1], get_node_from_store(&store));
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
        let values: Vec<Vec<u8>> = (0..5u64)
            .map(|i| format!("val_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
        let (store, mmr_size) = build_mmr(&refs);
        let root = root_hash(&store, mmr_size);

        let valid_proof = MmrTreeProof::generate(mmr_size, &[2], get_node_from_store(&store))
            .expect("generate proof");

        let tampered = MmrTreeProof::new(
            valid_proof.mmr_size(),
            vec![(2, b"val_2".to_vec()), (2, b"val_2".to_vec())],
            valid_proof.proof_items().to_vec(),
        );

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
        let values: Vec<Vec<u8>> = (0..5u64)
            .map(|i| format!("val_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
        let (store, mmr_size) = build_mmr(&refs);
        let root = root_hash(&store, mmr_size);

        let valid_proof = MmrTreeProof::generate(mmr_size, &[2], get_node_from_store(&store))
            .expect("generate proof");

        let tampered = MmrTreeProof::new(
            valid_proof.mmr_size(),
            vec![(2, b"val_2".to_vec()), (2, b"FORGED".to_vec())],
            valid_proof.proof_items().to_vec(),
        );

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
        use crate::{blake3_merge, leaf_hash};

        let left_hash = [0xAAu8; 32];
        let right_hash = [0xBBu8; 32];

        let merge_hash = blake3_merge(&left_hash, &right_hash);

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
        use std::sync::atomic::{AtomicU64, Ordering};

        let values: Vec<Vec<u8>> = (0..5u64)
            .map(|i| format!("val_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = values.iter().map(|v| v.as_slice()).collect();
        let (store, _mmr_size) = build_mmr(&refs);

        let call_count = AtomicU64::new(0);
        let counted_get = |pos: u64| -> Result<Option<MmrNode>> {
            call_count.fetch_add(1, Ordering::Relaxed);
            (&store)
                .element_at_position(pos)
                .unwrap()
                .map_err(|e| Error::OperationFailed(format!("{}", e)))
        };

        let lazy = LazyNodeStore::new(counted_get);

        let node1: Option<MmrNode> = MMRStoreReadOps::element_at_position(&&lazy, 0)
            .unwrap()
            .expect("element_at_position first");
        assert!(node1.is_some(), "node at position 0 should exist");
        assert_eq!(call_count.load(Ordering::Relaxed), 1);

        let node2: Option<MmrNode> = MMRStoreReadOps::element_at_position(&&lazy, 0)
            .unwrap()
            .expect("element_at_position second");
        assert!(node2.is_some(), "cached node should exist");
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "second read should use cache, not call closure again"
        );
    }

    #[test]
    fn test_generate_rejects_empty_leaf_indices() {
        let (store, mmr_size) = build_mmr(&[b"data"]);

        let result = MmrTreeProof::generate(mmr_size, &[], get_node_from_store(&store));
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
        let tampered = MmrTreeProof::new(1, vec![], vec![]);
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
