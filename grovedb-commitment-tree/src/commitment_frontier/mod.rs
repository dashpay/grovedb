use grovedb_costs::{CostResult, CostsExt, OperationCost};
use incrementalmerkletree::{frontier::Frontier, Hashable, Level, Position};
use orchard::{tree::MerkleHashOrchard, Anchor, NOTE_COMMITMENT_TREE_DEPTH};

pub use crate::error::CommitmentTreeError;

#[cfg(all(test, feature = "server"))]
mod tests;

/// Depth of the Sinsemilla Merkle tree as a u8 constant for the Frontier type
/// parameter.
#[cfg(feature = "server")]
const FRONTIER_DEPTH: u8 = NOTE_COMMITMENT_TREE_DEPTH as u8;

/// A lightweight frontier-based Sinsemilla commitment tree.
///
/// Stores only the rightmost path of the depth-32 Merkle tree (~1KB),
/// supporting O(1) append and root hash computation.
///
/// The full note data (cmx || encrypted_note) is stored separately as
/// items in a GroveDB CountTree. This struct only tracks the Sinsemilla
/// hash state. Historical anchors for spend authorization are managed
/// by Platform in a separate provable tree.
///
/// Requires the `server` feature.
#[cfg(feature = "server")]
#[derive(Debug, Clone)]
pub struct CommitmentFrontier {
    frontier: Frontier<MerkleHashOrchard, FRONTIER_DEPTH>,
}

#[cfg(feature = "server")]
impl CommitmentFrontier {
    /// Create a new empty commitment frontier.
    pub fn new() -> Self {
        Self {
            frontier: Frontier::empty(),
        }
    }

    /// Append a commitment (cmx) to the frontier.
    ///
    /// Returns the new Sinsemilla root hash after the append. The returned
    /// [`OperationCost`] tracks `sinsemilla_hash_calls`: 32 hashes for the
    /// leaf-to-root path plus `trailing_ones(position)` ommer hashes.
    pub fn append(&mut self, cmx: [u8; 32]) -> CostResult<[u8; 32], CommitmentTreeError> {
        let mut cost = OperationCost::default();
        let leaf = match merkle_hash_from_bytes(&cmx) {
            Some(l) => l,
            None => {
                return Err(CommitmentTreeError::InvalidFieldElement).wrap_with_cost(cost);
            }
        };

        // Count Sinsemilla hashes: 32 levels for the leaf path + trailing_ones
        // for ommer merges
        let ommer_hashes = self
            .frontier
            .value()
            .map(|f| u64::from(f.position()).trailing_ones())
            .unwrap_or(0);
        cost.sinsemilla_hash_calls += 32 + ommer_hashes;

        if !self.frontier.append(leaf) {
            return Err(CommitmentTreeError::TreeFull).wrap_with_cost(cost);
        }
        Ok(self.root_hash()).wrap_with_cost(cost)
    }

    /// Get the current Sinsemilla root hash as 32 bytes.
    ///
    /// Returns the empty tree root if no leaves have been appended.
    pub fn root_hash(&self) -> [u8; 32] {
        self.frontier.root().to_bytes()
    }

    /// Get the current root as an Orchard `Anchor`.
    pub fn anchor(&self) -> Anchor {
        Anchor::from(self.frontier.root())
    }

    /// Get the position of the most recently appended leaf.
    ///
    /// Returns `None` if the frontier is empty. The position is 0-indexed,
    /// so it equals `count - 1`.
    pub fn position(&self) -> Option<u64> {
        self.frontier.value().map(|f| u64::from(f.position()))
    }

    /// Get the number of leaves that have been appended.
    pub fn tree_size(&self) -> u64 {
        self.frontier.tree_size()
    }

    /// Serialize the frontier to bytes.
    ///
    /// Format:
    /// ```text
    /// has_frontier: u8 (0x00 = empty, 0x01 = non-empty)
    /// If non-empty:
    ///   position: u64 BE (8 bytes)
    ///   leaf: [u8; 32]
    ///   ommer_count: u8
    ///   ommers: [ommer_count × 32 bytes]
    /// ```
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        match self.frontier.value() {
            None => {
                buf.push(0x00);
            }
            Some(f) => {
                buf.push(0x01);
                buf.extend_from_slice(&u64::from(f.position()).to_be_bytes());
                buf.extend_from_slice(&f.leaf().to_bytes());
                let ommers = f.ommers();
                buf.push(ommers.len() as u8);
                for ommer in ommers {
                    buf.extend_from_slice(&ommer.to_bytes());
                }
            }
        }

        buf
    }

    /// Deserialize a frontier from bytes.
    pub fn deserialize(data: &[u8]) -> Result<Self, CommitmentTreeError> {
        if data.is_empty() {
            return Err(CommitmentTreeError::InvalidData("empty input".to_string()));
        }

        let mut pos = 0;

        let has_frontier = data[pos];
        pos += 1;

        let frontier = if has_frontier == 0x00 {
            Frontier::empty()
        } else if has_frontier == 0x01 {
            if data.len() < pos + 8 + 32 + 1 {
                return Err(CommitmentTreeError::InvalidData(
                    "truncated frontier header".to_string(),
                ));
            }

            let position_u64 = u64::from_be_bytes(
                data[pos..pos + 8]
                    .try_into()
                    .map_err(|_| CommitmentTreeError::InvalidData("bad position".to_string()))?,
            );
            pos += 8;

            let leaf_bytes: [u8; 32] = data[pos..pos + 32]
                .try_into()
                .map_err(|_| CommitmentTreeError::InvalidData("bad leaf".to_string()))?;
            let leaf = merkle_hash_from_bytes(&leaf_bytes)
                .ok_or(CommitmentTreeError::InvalidFieldElement)?;
            pos += 32;

            let ommer_count = data[pos] as usize;
            pos += 1;

            if data.len() < pos + ommer_count * 32 {
                return Err(CommitmentTreeError::InvalidData(
                    "truncated ommers".to_string(),
                ));
            }

            let mut ommers = Vec::with_capacity(ommer_count);
            for _ in 0..ommer_count {
                let ommer_bytes: [u8; 32] = data[pos..pos + 32]
                    .try_into()
                    .map_err(|_| CommitmentTreeError::InvalidData("bad ommer".to_string()))?;
                let ommer = merkle_hash_from_bytes(&ommer_bytes)
                    .ok_or(CommitmentTreeError::InvalidFieldElement)?;
                ommers.push(ommer);
                pos += 32;
            }

            // Allow trailing bytes for forward compatibility (old serialization
            // included historical anchors after the frontier data).
            let _ = pos;

            Frontier::from_parts(Position::from(position_u64), leaf, ommers).map_err(|e| {
                CommitmentTreeError::InvalidData(format!("frontier reconstruction: {:?}", e))
            })?
        } else {
            return Err(CommitmentTreeError::InvalidData(format!(
                "invalid frontier flag: 0x{:02x}",
                has_frontier
            )));
        };

        Ok(Self { frontier })
    }
}

#[cfg(feature = "server")]
impl Default for CommitmentFrontier {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert raw 32 bytes to a `MerkleHashOrchard`, returning `None` if the
/// bytes do not represent a valid Pallas field element.
pub fn merkle_hash_from_bytes(bytes: &[u8; 32]) -> Option<MerkleHashOrchard> {
    Option::from(MerkleHashOrchard::from_bytes(bytes))
}

/// Return the Sinsemilla root hash of an empty depth-32 commitment tree.
///
/// This is the root when zero leaves have been appended. It equals
/// `MerkleHashOrchard::empty_root(Level::from(32))`.
///
/// The value is computed once and cached. It is also available as the
/// constant [`EMPTY_SINSEMILLA_ROOT`].
pub fn empty_sinsemilla_root() -> [u8; 32] {
    MerkleHashOrchard::empty_root(Level::from(NOTE_COMMITMENT_TREE_DEPTH as u8)).to_bytes()
}

/// Precomputed Sinsemilla root of an empty depth-32 commitment tree.
///
/// Generated by `MerkleHashOrchard::empty_root(Level::from(32)).to_bytes()`.
/// Verified at compile time via `grovedb-commitment-tree` unit tests.
pub const EMPTY_SINSEMILLA_ROOT: [u8; 32] = [
    0xae, 0x29, 0x35, 0xf1, 0xdf, 0xd8, 0xa2, 0x4a, 0xed, 0x7c, 0x70, 0xdf, 0x7d, 0xe3, 0xa6, 0x68,
    0xeb, 0x7a, 0x49, 0xb1, 0x31, 0x98, 0x80, 0xdd, 0xe2, 0xbb, 0xd9, 0x03, 0x1a, 0xe5, 0xd8, 0x2f,
];
