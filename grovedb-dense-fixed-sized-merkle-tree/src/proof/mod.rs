//! Inclusion proof generation for the dense fixed-sized Merkle tree.
//!
//! A `DenseTreeProof` proves that specific positions hold specific values,
//! authenticated against the tree's root hash.
//!
//! All nodes hash `blake3(H(value) || H(left) || H(right))`, so ancestor
//! nodes on the auth path only need their value *hash* (not the full value),
//! keeping proofs compact.

use std::collections::BTreeSet;

use bincode::{Decode, Encode};
use grovedb_costs::{CostResult, CostsExt, OperationCost};

use crate::{DenseMerkleError, DenseTreeStore};

mod tests;

/// Unwrap a `CostResult`, accumulate its cost into `$cost`, and return early
/// (with accumulated cost) on error.
macro_rules! cost_return_on_error {
    ($cost:ident, $expr:expr) => {
        match $expr.unwrap_add_cost(&mut $cost) {
            Ok(x) => x,
            Err(e) => return Err(e).wrap_with_cost($cost),
        }
    };
}

/// An inclusion proof for one or more positions in a dense fixed-sized Merkle
/// tree.
#[derive(Debug, Clone, Encode, Decode)]
pub struct DenseTreeProof {
    /// Height of the tree (capacity = 2^height - 1).
    pub height: u8,
    /// Number of filled positions.
    pub count: u16,
    /// The proved (position, value) pairs.
    pub entries: Vec<(u16, Vec<u8>)>,
    /// Hashes of ancestor node values on the auth path that are NOT proved
    /// entries. Only the 32-byte hash is needed because all nodes use
    /// `H(value)` in their hash computation.
    pub node_value_hashes: Vec<(u16, [u8; 32])>,
    /// Precomputed subtree hashes for sibling nodes not in the expanded set.
    pub node_hashes: Vec<(u16, [u8; 32])>,
}

impl DenseTreeProof {
    /// Generate a proof for the given positions.
    ///
    /// Positions must be < count. Duplicates are deduplicated.
    pub fn generate<S: DenseTreeStore>(
        height: u8,
        count: u16,
        positions: &[u16],
        store: &S,
    ) -> CostResult<Self, DenseMerkleError> {
        let mut cost = OperationCost::default();

        // Validate height before the shift to avoid panic on height >= 16
        if let Err(e) = crate::hash::validate_height(height) {
            return Err(e).wrap_with_cost(cost);
        }
        let capacity = ((1u32 << height) - 1) as u16;

        // Validate positions
        for &pos in positions {
            if pos >= count {
                return Err(DenseMerkleError::InvalidProof(format!(
                    "position {} is out of range (count={})",
                    pos, count
                )))
                .wrap_with_cost(cost);
            }
        }

        // Deduplicate
        let proved_set: BTreeSet<u16> = positions.iter().copied().collect();

        // Build expanded set: proved positions + all ancestors up to root
        let mut expanded: BTreeSet<u16> = proved_set.clone();
        for &pos in &proved_set {
            let mut p = pos;
            while p > 0 {
                p = (p - 1) / 2; // parent
                expanded.insert(p);
            }
        }

        // Collect entries, node_value_hashes, node_hashes
        let mut entries: Vec<(u16, Vec<u8>)> = Vec::new();
        let mut node_value_hashes: Vec<(u16, [u8; 32])> = Vec::new();
        let mut node_hashes: Vec<(u16, [u8; 32])> = Vec::new();

        // Use from_state to get a tree object for hash_position
        let tree = match crate::tree::DenseFixedSizedMerkleTree::from_state(height, count) {
            Ok(t) => t,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };

        for &pos in &expanded {
            // Get the value for this position
            let opt = cost_return_on_error!(cost, store.get_value(pos));
            let value = match opt {
                Some(v) => v,
                None => {
                    return Err(DenseMerkleError::StoreError(format!(
                        "expected value at position {} but found none",
                        pos
                    )))
                    .wrap_with_cost(cost)
                }
            };

            if proved_set.contains(&pos) {
                entries.push((pos, value));
            } else {
                // Ancestor node: only need the hash of the value
                let value_hash = *blake3::hash(&value).as_bytes();
                cost.hash_node_calls += 1;
                node_value_hashes.push((pos, value_hash));
            }

            // For each child of this position that is NOT in the expanded set
            // and within capacity, compute its hash and include it.
            let left_child_u32 = 2 * pos as u32 + 1;
            let right_child_u32 = 2 * pos as u32 + 2;

            if left_child_u32 < capacity as u32 {
                let left_child = left_child_u32 as u16;
                if !expanded.contains(&left_child) {
                    let hash = cost_return_on_error!(cost, tree.hash_position(left_child, store));
                    node_hashes.push((left_child, hash));
                }
            }
            if right_child_u32 < capacity as u32 {
                let right_child = right_child_u32 as u16;
                if !expanded.contains(&right_child) {
                    let hash = cost_return_on_error!(cost, tree.hash_position(right_child, store));
                    node_hashes.push((right_child, hash));
                }
            }
        }

        Ok(DenseTreeProof {
            height,
            count,
            entries,
            node_value_hashes,
            node_hashes,
        })
        .wrap_with_cost(cost)
    }

    /// Generate a proof for a contiguous range of positions `[start, end)`.
    ///
    /// Convenience wrapper around [`generate`](Self::generate).
    pub fn generate_range<S: DenseTreeStore>(
        height: u8,
        count: u16,
        start: u16,
        end: u16,
        store: &S,
    ) -> CostResult<Self, DenseMerkleError> {
        let positions: Vec<u16> = (start..end).collect();
        Self::generate(height, count, &positions, store)
    }

    /// Encode to bytes using bincode.
    pub fn encode_to_vec(&self) -> Result<Vec<u8>, DenseMerkleError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        bincode::encode_to_vec(self, config)
            .map_err(|e| DenseMerkleError::InvalidProof(format!("encode error: {}", e)))
    }

    /// Decode from bytes using bincode.
    ///
    /// Validates that the decoded height is in [1, 16] to prevent overflow.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, DenseMerkleError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 100 * 1024 * 1024 }>(); // 100MB limit
        let (proof, _): (Self, _) = bincode::decode_from_slice(bytes, config)
            .map_err(|e| DenseMerkleError::InvalidProof(format!("decode error: {}", e)))?;
        if !(1..=16).contains(&proof.height) {
            return Err(DenseMerkleError::InvalidProof(format!(
                "invalid height {} in proof (must be 1..=16)",
                proof.height
            )));
        }
        let capacity = ((1u32 << proof.height) - 1) as u16;
        if proof.count > capacity {
            return Err(DenseMerkleError::InvalidProof(format!(
                "count {} exceeds capacity {} for height {}",
                proof.count, capacity, proof.height
            )));
        }
        Ok(proof)
    }
}
