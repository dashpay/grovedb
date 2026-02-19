//! Proof verification for the dense fixed-sized Merkle tree.
//!
//! Pure function — no storage required. Recomputes the root hash from the
//! proof data and compares it to the expected root.
//!
//! Internal nodes use `blake3(0x01 || H(value) || H(left) || H(right))`,
//! so ancestor nodes only need a 32-byte value hash, not the full value.

use std::collections::BTreeMap;

use crate::{
    hash::{INTERNAL_DOMAIN_TAG, LEAF_DOMAIN_TAG},
    proof::DenseTreeProof,
    DenseMerkleError,
};

/// Maximum number of elements per proof field (entries, node_value_hashes,
/// node_hashes) to prevent DoS via expensive ancestor set computation.
const MAX_PROOF_ELEMENTS: usize = 100_000;

impl DenseTreeProof {
    /// Verify the proof against an expected root hash.
    ///
    /// Returns the proved `(position, value)` pairs on success.
    pub fn verify(
        &self,
        expected_root: &[u8; 32],
    ) -> Result<Vec<(u64, Vec<u8>)>, DenseMerkleError> {
        // Validate height to prevent shift overflow
        if !(1..=63).contains(&self.height) {
            return Err(DenseMerkleError::InvalidProof(format!(
                "invalid height {} in proof (must be 1..=63)",
                self.height
            )));
        }

        let capacity = (1u64 << self.height) - 1;

        // Validate count against capacity
        if self.count > capacity {
            return Err(DenseMerkleError::InvalidProof(format!(
                "count {} exceeds capacity {} for height {}",
                self.count, capacity, self.height
            )));
        }

        // DoS prevention: limit the number of elements in each proof field
        if self.entries.len() > MAX_PROOF_ELEMENTS
            || self.node_value_hashes.len() > MAX_PROOF_ELEMENTS
            || self.node_hashes.len() > MAX_PROOF_ELEMENTS
        {
            return Err(DenseMerkleError::InvalidProof(format!(
                "proof contains too many elements (max {} per field)",
                MAX_PROOF_ELEMENTS
            )));
        }

        // Vuln 3: Reject duplicate positions in entries
        {
            let mut seen = std::collections::BTreeSet::new();
            for (pos, _) in &self.entries {
                if !seen.insert(*pos) {
                    return Err(DenseMerkleError::InvalidProof(format!(
                        "duplicate entry at position {}",
                        pos
                    )));
                }
            }
        }

        // Vuln 3: Reject duplicate positions in node_value_hashes
        {
            let mut seen = std::collections::BTreeSet::new();
            for (pos, _) in &self.node_value_hashes {
                if !seen.insert(*pos) {
                    return Err(DenseMerkleError::InvalidProof(format!(
                        "duplicate node_value_hash at position {}",
                        pos
                    )));
                }
            }
        }

        // Vuln 3: Reject duplicate positions in node_hashes
        {
            let mut seen = std::collections::BTreeSet::new();
            for (pos, _) in &self.node_hashes {
                if !seen.insert(*pos) {
                    return Err(DenseMerkleError::InvalidProof(format!(
                        "duplicate node_hash at position {}",
                        pos
                    )));
                }
            }
        }

        // Vuln 6: Validate that entries, node_value_hashes, and node_hashes have
        // pairwise-disjoint position sets
        let entry_positions: std::collections::BTreeSet<u64> =
            self.entries.iter().map(|(p, _)| *p).collect();
        let value_hash_positions: std::collections::BTreeSet<u64> =
            self.node_value_hashes.iter().map(|(p, _)| *p).collect();
        let hash_positions: std::collections::BTreeSet<u64> =
            self.node_hashes.iter().map(|(p, _)| *p).collect();

        if !entry_positions.is_disjoint(&value_hash_positions) {
            return Err(DenseMerkleError::InvalidProof(
                "overlapping positions between entries and node_value_hashes".into(),
            ));
        }
        if !entry_positions.is_disjoint(&hash_positions) {
            return Err(DenseMerkleError::InvalidProof(
                "overlapping positions between entries and node_hashes".into(),
            ));
        }
        if !value_hash_positions.is_disjoint(&hash_positions) {
            return Err(DenseMerkleError::InvalidProof(
                "overlapping positions between node_value_hashes and node_hashes".into(),
            ));
        }

        // Vuln 1: Validate that no node_hash is at an ancestor of any proved
        // entry. Build the expanded set (proved positions + all ancestors).
        let mut ancestor_set = entry_positions.clone();
        for &pos in &entry_positions {
            let mut p = pos;
            while p > 0 {
                p = (p - 1) / 2;
                ancestor_set.insert(p);
            }
        }
        for (pos, _) in &self.node_hashes {
            if ancestor_set.contains(pos) {
                return Err(DenseMerkleError::InvalidProof(format!(
                    "node_hash at position {} is on the auth path of a proved entry",
                    pos
                )));
            }
        }

        // Build lookup maps
        let entry_map: BTreeMap<u64, &Vec<u8>> =
            self.entries.iter().map(|(pos, val)| (*pos, val)).collect();
        let value_hash_map: BTreeMap<u64, &[u8; 32]> = self
            .node_value_hashes
            .iter()
            .map(|(pos, hash)| (*pos, hash))
            .collect();
        let hash_map: BTreeMap<u64, &[u8; 32]> = self
            .node_hashes
            .iter()
            .map(|(pos, hash)| (*pos, hash))
            .collect();

        // Recompute root hash from position 0
        let computed_root =
            self.recompute_hash(0, capacity, &entry_map, &value_hash_map, &hash_map)?;

        if &computed_root != expected_root {
            return Err(DenseMerkleError::InvalidProof(format!(
                "root hash mismatch: expected {}, got {}",
                hex_encode(expected_root),
                hex_encode(&computed_root)
            )));
        }

        // Vuln 2: Only return entries at valid positions (< count AND < capacity)
        Ok(self
            .entries
            .iter()
            .filter(|(pos, _)| *pos < self.count && *pos < capacity)
            .map(|(pos, val)| (*pos, val.clone()))
            .collect())
    }

    /// Recursively recompute the hash for a position.
    fn recompute_hash(
        &self,
        position: u64,
        capacity: u64,
        entry_map: &BTreeMap<u64, &Vec<u8>>,
        value_hash_map: &BTreeMap<u64, &[u8; 32]>,
        hash_map: &BTreeMap<u64, &[u8; 32]>,
    ) -> Result<[u8; 32], DenseMerkleError> {
        // Beyond capacity or count -> zero hash
        if position >= capacity || position >= self.count {
            return Ok([0u8; 32]);
        }

        // Precomputed subtree hash available -> use it directly
        if let Some(hash) = hash_map.get(&position) {
            return Ok(**hash);
        }

        // Check leaf condition BEFORE computing child indices to avoid
        // u64 overflow for positions near capacity in height-62/63 trees.
        let first_leaf = (capacity - 1) / 2;
        if position >= first_leaf {
            // Leaf node: must have full value (from entries)
            let value = entry_map.get(&position).ok_or_else(|| {
                DenseMerkleError::InvalidProof(format!(
                    "incomplete proof: no value for leaf position {}",
                    position
                ))
            })?;
            // hash = blake3(0x00 || value)
            let mut hasher = blake3::Hasher::new();
            hasher.update(&[LEAF_DOMAIN_TAG]);
            hasher.update(value);
            return Ok(*hasher.finalize().as_bytes());
        }

        // Internal node: need H(value) — either compute from full value
        // (entries) or use precomputed value hash (node_value_hashes)
        let value_hash: [u8; 32] = if let Some(value) = entry_map.get(&position) {
            *blake3::hash(value).as_bytes()
        } else if let Some(hash) = value_hash_map.get(&position) {
            **hash
        } else {
            return Err(DenseMerkleError::InvalidProof(format!(
                "incomplete proof: no value or value hash for internal position {}",
                position
            )));
        };

        // hash = blake3(0x01 || H(value) || H(left) || H(right))
        let left_child = 2 * position + 1;
        let right_child = 2 * position + 2;
        let left_hash =
            self.recompute_hash(left_child, capacity, entry_map, value_hash_map, hash_map)?;
        let right_hash =
            self.recompute_hash(right_child, capacity, entry_map, value_hash_map, hash_map)?;

        let mut hasher = blake3::Hasher::new();
        hasher.update(&[INTERNAL_DOMAIN_TAG]);
        hasher.update(&value_hash);
        hasher.update(&left_hash);
        hasher.update(&right_hash);

        Ok(*hasher.finalize().as_bytes())
    }
}

fn hex_encode(bytes: &[u8; 32]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}
