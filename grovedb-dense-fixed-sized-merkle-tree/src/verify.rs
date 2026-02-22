//! Proof verification for the dense fixed-sized Merkle tree.
//!
//! Pure function — no storage required. Recomputes the root hash from the
//! proof data and compares it to the expected root.
//!
//! All nodes use `blake3(H(value) || H(left) || H(right))`,
//! so ancestor nodes only need a 32-byte value hash, not the full value.

use std::collections::{BTreeMap, BTreeSet};

use grovedb_query::Query;

use crate::{hash::node_hash, proof::DenseTreeProof, DenseMerkleError};

/// Result of proof verification: computed root hash and proved (position,
/// value) pairs.
type VerifyResult = Result<([u8; 32], Vec<(u16, Vec<u8>)>), DenseMerkleError>;

impl DenseTreeProof {
    /// Verify the proof against an expected root hash.
    ///
    /// `height` and `count` are trusted values obtained from an authenticated
    /// source (e.g. the parent `Element` in Merk).
    ///
    /// Returns the proved `(position, value)` pairs on success.
    pub fn verify_against_expected_root(
        &self,
        expected_root: &[u8; 32],
        height: u8,
        count: u16,
    ) -> Result<Vec<(u16, Vec<u8>)>, DenseMerkleError> {
        let (computed_root, entries) = self.verify_inner(height, count)?;

        if &computed_root != expected_root {
            return Err(DenseMerkleError::InvalidProof(format!(
                "root hash mismatch: expected {}, got {}",
                hex_encode(expected_root),
                hex_encode(&computed_root)
            )));
        }

        Ok(entries)
    }

    /// Verify the proof and return the computed root hash along with proved
    /// entries, without comparing against an expected root.
    ///
    /// `height` and `count` are trusted values obtained from an authenticated
    /// source (e.g. the parent `Element` in Merk).
    ///
    /// This is used when the root hash flows through the Merk child hash
    /// mechanism rather than being stored in the Element.
    pub fn verify_and_get_root(&self, height: u8, count: u16) -> VerifyResult {
        self.verify_inner(height, count)
    }

    /// Verify the proof against a [`Query`], returning the computed root hash
    /// and proved entries.
    ///
    /// `height` and `count` are trusted values obtained from an authenticated
    /// source (e.g. the parent `Element` in Merk). Open-ended query ranges are
    /// clamped to `count`.
    ///
    /// In addition to the structural checks performed by
    /// [`verify_and_get_root`](Self::verify_and_get_root), this method ensures
    /// the proof is **complete** and **sound** with respect to the query:
    ///
    /// - **Complete**: every position requested by the query has a
    ///   corresponding entry in the proof.
    /// - **Sound**: the proof contains no entries for positions that were not
    ///   requested by the query.
    pub fn verify_for_query(&self, query: &Query, height: u8, count: u16) -> VerifyResult {
        let (root, entries) = self.verify_inner(height, count)?;

        let expected_positions: BTreeSet<u16> = match crate::proof::query_to_positions(query, count)
        {
            Ok(p) => p.into_iter().collect(),
            Err(e) => return Err(e),
        };

        let proved_positions: BTreeSet<u16> = entries.iter().map(|(pos, _)| *pos).collect();

        // Completeness: every queried position must be in the proof
        let missing: Vec<u16> = expected_positions
            .difference(&proved_positions)
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(DenseMerkleError::InvalidProof(format!(
                "incomplete proof: missing positions {:?}",
                missing
            )));
        }

        // Soundness: no extra positions beyond what was queried
        let extra: Vec<u16> = proved_positions
            .difference(&expected_positions)
            .copied()
            .collect();
        if !extra.is_empty() {
            return Err(DenseMerkleError::InvalidProof(format!(
                "unsound proof: unexpected positions {:?}",
                extra
            )));
        }

        Ok((root, entries))
    }

    /// Shared verification logic: validates the proof structure, recomputes
    /// the root hash, and returns `(computed_root, proved_entries)`.
    fn verify_inner(&self, height: u8, count: u16) -> VerifyResult {
        // Validate height to prevent shift overflow
        if !(1..=16).contains(&height) {
            return Err(DenseMerkleError::InvalidProof(format!(
                "invalid height {} (must be 1..=16)",
                height
            )));
        }

        let capacity = ((1u32 << height) - 1) as u16;

        // Validate count against capacity
        if count > capacity {
            return Err(DenseMerkleError::InvalidProof(format!(
                "count {} exceeds capacity {} for height {}",
                count, capacity, height
            )));
        }

        // Reject entries at out-of-range positions to prevent malleability
        for (pos, _) in &self.entries {
            if *pos >= count || *pos >= capacity {
                return Err(DenseMerkleError::InvalidProof(format!(
                    "entry at position {} is out of range (count={}, capacity={})",
                    pos, count, capacity
                )));
            }
        }

        // DoS prevention: no proof field can exceed the tree's capacity
        let cap = capacity as usize;
        if self.entries.len() > cap
            || self.node_value_hashes.len() > cap
            || self.node_hashes.len() > cap
        {
            return Err(DenseMerkleError::InvalidProof(format!(
                "proof field exceeds tree capacity {} (entries={}, value_hashes={}, hashes={})",
                capacity,
                self.entries.len(),
                self.node_value_hashes.len(),
                self.node_hashes.len()
            )));
        }

        // Reject duplicate positions in entries
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

        // Reject duplicate positions in node_value_hashes
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

        // Reject duplicate positions in node_hashes
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

        // Validate that entries, node_value_hashes, and node_hashes have
        // pairwise-disjoint position sets
        let entry_positions: std::collections::BTreeSet<u16> =
            self.entries.iter().map(|(p, _)| *p).collect();
        let value_hash_positions: std::collections::BTreeSet<u16> =
            self.node_value_hashes.iter().map(|(p, _)| *p).collect();
        let hash_positions: std::collections::BTreeSet<u16> =
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

        // Validate that no node_hash is at an ancestor of any proved
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
        let entry_map: BTreeMap<u16, &Vec<u8>> =
            self.entries.iter().map(|(pos, val)| (*pos, val)).collect();
        let value_hash_map: BTreeMap<u16, &[u8; 32]> = self
            .node_value_hashes
            .iter()
            .map(|(pos, hash)| (*pos, hash))
            .collect();
        let hash_map: BTreeMap<u16, &[u8; 32]> = self
            .node_hashes
            .iter()
            .map(|(pos, hash)| (*pos, hash))
            .collect();

        // Recompute root hash from position 0
        let computed_root =
            recompute_hash(0, capacity, count, &entry_map, &value_hash_map, &hash_map)?;

        // All entry positions validated in-range above; return them directly
        let entries: Vec<(u16, Vec<u8>)> = self.entries.to_vec();

        Ok((computed_root, entries))
    }
}

/// Recursively recompute the hash for a position.
///
/// All nodes use `blake3(H(value) || H(left) || H(right))`.
/// Leaf nodes simply have `[0; 32]` for both child hashes.
fn recompute_hash(
    position: u16,
    capacity: u16,
    count: u16,
    entry_map: &BTreeMap<u16, &Vec<u8>>,
    value_hash_map: &BTreeMap<u16, &[u8; 32]>,
    hash_map: &BTreeMap<u16, &[u8; 32]>,
) -> Result<[u8; 32], DenseMerkleError> {
    // Beyond capacity or count -> zero hash
    if position >= capacity || position >= count {
        return Ok([0u8; 32]);
    }

    // Precomputed subtree hash available -> use it directly
    if let Some(hash) = hash_map.get(&position) {
        return Ok(**hash);
    }

    // Every node needs H(value) — either compute from full value
    // (entries) or use precomputed value hash (node_value_hashes)
    let value_hash: [u8; 32] = if let Some(value) = entry_map.get(&position) {
        *blake3::hash(value).as_bytes()
    } else if let Some(hash) = value_hash_map.get(&position) {
        **hash
    } else {
        return Err(DenseMerkleError::InvalidProof(format!(
            "incomplete proof: no value or value hash for position {}",
            position
        )));
    };

    // Use u32 to avoid overflow for leaf positions near capacity.
    let left_child_u32 = 2 * position as u32 + 1;
    let right_child_u32 = 2 * position as u32 + 2;

    let left_hash = if left_child_u32 < capacity as u32 {
        recompute_hash(
            left_child_u32 as u16,
            capacity,
            count,
            entry_map,
            value_hash_map,
            hash_map,
        )?
    } else {
        [0u8; 32]
    };
    let right_hash = if right_child_u32 < capacity as u32 {
        recompute_hash(
            right_child_u32 as u16,
            capacity,
            count,
            entry_map,
            value_hash_map,
            hash_map,
        )?
    } else {
        [0u8; 32]
    };

    Ok(node_hash(&value_hash, &left_hash, &right_hash))
}

fn hex_encode(bytes: &[u8; 32]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}
