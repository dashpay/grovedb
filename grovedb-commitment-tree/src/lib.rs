//! Orchard-style commitment tree integration for GroveDB.
//!
//! This crate provides a lightweight frontier-based Sinsemilla Merkle tree
//! for tracking note commitment anchors. It wraps the `incrementalmerkletree`
//! `Frontier` type with `orchard::tree::MerkleHashOrchard` hashing.
//!
//! # Architecture
//!
//! - Uses `Frontier<MerkleHashOrchard, 32>` for O(1) append and root
//!   computation
//! - Stores only the rightmost path (~1KB constant size) rather than the full
//!   tree
//! - Items (cmx || encrypted_note) are stored as GroveDB CountTree items
//! - The frontier is serialized to aux storage alongside the CountTree
//! - Historical anchors are managed by Platform in a separate tree (not here)

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
pub use client::ClientMemoryCommitmentTree;
#[cfg(feature = "sqlite")]
mod sqlite_client;
#[cfg(feature = "sqlite")]
mod sqlite_store;
#[cfg(feature = "storage")]
mod storage_adapter;
pub use grovedb_costs::{self};
#[cfg(feature = "server")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};
#[cfg(feature = "server")]
use incrementalmerkletree::frontier::Frontier;
pub use incrementalmerkletree::{Hashable, Level, Position, Retention};
// Builder for constructing shielded transactions
pub use orchard::builder::{Builder, BundleType};
/// Re-export of `orchard::bundle::BatchValidator` for verifying Orchard
/// bundles.
///
/// # Sighash Requirement
///
/// [`BatchValidator::add_bundle`] requires a `sighash: [u8; 32]` parameter —
/// the transaction hash that the Orchard bundle commits to. This hash covers
/// the transaction data excluding the Orchard bundle itself and is used to
/// verify both spend authorization signatures and the binding signature.
///
/// Platform **must** compute the sighash according to the Dash-adapted
/// equivalent of ZIP-244's transaction digest algorithm and pass it when adding
/// each bundle. Without the correct sighash, signature verification will fail
/// even if the ZK proofs are valid.
///
/// # Usage
///
/// ```ignore
/// use grovedb_commitment_tree::{BatchValidator, VerifyingKey};
/// use rand::rngs::OsRng;
///
/// let mut validator = BatchValidator::new();
/// // sighash must be the transaction digest for this bundle
/// validator.add_bundle(&bundle, sighash);
/// // Validate all accumulated bundles (ZK proofs + signatures)
/// let valid = validator.validate(&verifying_key, OsRng);
/// ```
pub use orchard::bundle::BatchValidator;
// Bundle/Action types
pub use orchard::bundle::{Authorized, Flags};
// Proof creation/verification (requires orchard "circuit" feature)
pub use orchard::circuit::{ProvingKey, VerifyingKey};
// Key management
pub use orchard::keys::{
    FullViewingKey, IncomingViewingKey, OutgoingViewingKey, PreparedIncomingViewingKey, Scope,
    SpendAuthorizingKey, SpendValidatingKey, SpendingKey,
};
// Compact note size constant (52 bytes, same for all memo sizes)
pub use orchard::memo::COMPACT_NOTE_SIZE;
// Memo size types for Dash 36-byte memos
pub use orchard::memo::{DashMemo, MemoSize};
// Note types (orchard::Address aliased to avoid conflict with incrementalmerkletree::Address)
pub use orchard::note::RandomSeed;
// Bundle reconstruction types (needed for deserializing bundles from bytes)
pub use orchard::note::TransmittedNoteCiphertext;
// Orchard tree types
pub use orchard::note::{ExtractedNoteCommitment, Nullifier};
// Note encryption / trial decryption
pub use orchard::note_encryption::{CompactAction, OrchardDomain};
// Byte wrapper for constructing note ciphertexts
pub use orchard::zcash_note_encryption::note_bytes::NoteBytesData;
// Trial decryption functions and traits
pub use orchard::zcash_note_encryption::{
    try_compact_note_decryption, try_note_decryption, Domain, EphemeralKeyBytes, ShieldedOutput,
};
pub use orchard::{
    note::Rho,
    primitives::redpallas,
    tree::{Anchor, MerkleHashOrchard, MerklePath},
    value::{NoteValue, ValueCommitment},
    Action, Address as PaymentAddress, Bundle, Note, Proof, NOTE_COMMITMENT_TREE_DEPTH,
};
#[cfg(feature = "sqlite")]
pub use rusqlite;
#[cfg(feature = "sqlite")]
pub use sqlite_client::ClientPersistentCommitmentTree;
#[cfg(feature = "sqlite")]
pub use sqlite_store::{SqliteShardStore, SqliteShardStoreError};
#[cfg(feature = "storage")]
pub use storage_adapter::{CommitmentTree, COMMITMENT_TREE_DATA_KEY};
use thiserror::Error;

/// Depth of the Sinsemilla Merkle tree as a u8 constant for the Frontier type
/// parameter.
#[cfg(feature = "server")]
const FRONTIER_DEPTH: u8 = NOTE_COMMITMENT_TREE_DEPTH as u8;

/// Errors that can occur during commitment tree operations.
#[derive(Debug, Error)]
pub enum CommitmentTreeError {
    #[error("tree is full (max {max} leaves)", max = 1u64 << NOTE_COMMITMENT_TREE_DEPTH)]
    TreeFull,
    #[error("invalid frontier data: {0}")]
    InvalidData(String),
    #[error("invalid Pallas field element")]
    InvalidFieldElement,
}

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

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

    /// Create a deterministic test leaf from an index.
    fn test_leaf(index: u64) -> [u8; 32] {
        let empty = MerkleHashOrchard::empty_leaf();
        let varied =
            MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty);
        MerkleHashOrchard::combine(Level::from(0), &empty, &varied).to_bytes()
    }

    #[test]
    fn test_empty_frontier() {
        let f = CommitmentFrontier::new();
        assert_eq!(f.position(), None);
        assert_eq!(f.tree_size(), 0);

        let empty_anchor = Anchor::empty_tree();
        assert_eq!(f.anchor(), empty_anchor);
    }

    #[test]
    fn test_append_changes_root() {
        let mut f = CommitmentFrontier::new();
        let empty_root = f.root_hash();

        let result = f.append(test_leaf(0));
        let new_root = result.value.expect("append should succeed");
        assert_ne!(empty_root, new_root);
        assert_eq!(f.root_hash(), new_root);
    }

    #[test]
    fn test_append_tracks_position() {
        let mut f = CommitmentFrontier::new();
        assert_eq!(f.position(), None);
        assert_eq!(f.tree_size(), 0);

        f.append(test_leaf(0)).value.expect("append 0");
        assert_eq!(f.position(), Some(0));
        assert_eq!(f.tree_size(), 1);

        f.append(test_leaf(1)).value.expect("append 1");
        assert_eq!(f.position(), Some(1));
        assert_eq!(f.tree_size(), 2);

        for i in 2..100u64 {
            f.append(test_leaf(i)).value.expect("append loop");
        }
        assert_eq!(f.position(), Some(99));
        assert_eq!(f.tree_size(), 100);
    }

    #[test]
    fn test_deterministic_roots() {
        let mut f1 = CommitmentFrontier::new();
        let mut f2 = CommitmentFrontier::new();

        for i in 0..10u64 {
            f1.append(test_leaf(i)).value.expect("append f1");
            f2.append(test_leaf(i)).value.expect("append f2");
        }

        assert_eq!(f1.root_hash(), f2.root_hash());
    }

    #[test]
    fn test_different_leaves_different_roots() {
        let mut f1 = CommitmentFrontier::new();
        let mut f2 = CommitmentFrontier::new();

        f1.append(test_leaf(0)).value.expect("append f1");
        f2.append(test_leaf(1)).value.expect("append f2");

        assert_ne!(f1.root_hash(), f2.root_hash());
    }

    #[test]
    fn test_serialize_empty() {
        let f = CommitmentFrontier::new();
        let data = f.serialize();
        let f2 = CommitmentFrontier::deserialize(&data).unwrap();

        assert_eq!(f.root_hash(), f2.root_hash());
        assert_eq!(f.position(), f2.position());
    }

    #[test]
    fn test_serialize_roundtrip() {
        let mut f = CommitmentFrontier::new();
        for i in 0..100u64 {
            f.append(test_leaf(i)).value.expect("append");
        }

        let data = f.serialize();
        let f2 = CommitmentFrontier::deserialize(&data).unwrap();

        assert_eq!(f.root_hash(), f2.root_hash());
        assert_eq!(f.position(), f2.position());
        assert_eq!(f.tree_size(), f2.tree_size());
    }

    #[test]
    fn test_serialize_roundtrip_with_many_leaves() {
        let mut f = CommitmentFrontier::new();
        for i in 0..1000u64 {
            f.append(test_leaf(i)).value.expect("append");
        }

        let data = f.serialize();
        // Frontier should be small regardless of leaf count
        // 1 (flag) + 8 (position) + 32 (leaf) + 1 (ommer_count) + N*32 (ommers)
        // Max ommers for depth 32 = 32, so max ~1.1KB
        assert!(
            data.len() < 1200,
            "frontier serialized to {} bytes",
            data.len()
        );

        let f2 = CommitmentFrontier::deserialize(&data).unwrap();
        assert_eq!(f.root_hash(), f2.root_hash());
        assert_eq!(f.tree_size(), f2.tree_size());
    }

    #[test]
    fn test_invalid_field_element() {
        // All 0xFF bytes is not a valid Pallas field element
        let result = CommitmentFrontier::new().append([0xff; 32]);
        assert!(result.value.is_err());
    }

    #[test]
    fn test_deserialize_invalid_data() {
        assert!(CommitmentFrontier::deserialize(&[]).is_err());
        assert!(CommitmentFrontier::deserialize(&[0x02]).is_err());
        assert!(CommitmentFrontier::deserialize(&[0x01]).is_err());
    }

    #[test]
    fn test_root_hash_is_32_bytes() {
        let f = CommitmentFrontier::new();
        assert_eq!(f.root_hash().len(), 32);
    }

    #[test]
    fn test_empty_tree_root_matches_orchard() {
        let f = CommitmentFrontier::new();
        let root = f.root_hash();
        let expected =
            MerkleHashOrchard::empty_root(Level::from(NOTE_COMMITMENT_TREE_DEPTH as u8)).to_bytes();
        assert_eq!(root, expected);
    }

    #[test]
    fn test_empty_sinsemilla_root_constant() {
        // Verify the precomputed constant matches the runtime value
        let computed = empty_sinsemilla_root();
        assert_eq!(
            computed, EMPTY_SINSEMILLA_ROOT,
            "EMPTY_SINSEMILLA_ROOT constant is stale. Update it to: {:?}",
            computed
        );
    }

    #[test]
    fn test_default_impl() {
        let f = CommitmentFrontier::default();
        assert_eq!(f.position(), None);
        assert_eq!(f.tree_size(), 0);
        assert_eq!(f.root_hash(), CommitmentFrontier::new().root_hash());
    }

    #[test]
    fn test_deserialize_truncated_ommers() {
        // Build a valid serialized frontier with 1 leaf so we know the ommer
        // count byte, then truncate the ommer data.
        let mut f = CommitmentFrontier::new();
        // Append enough leaves to generate ommers. After 3 appends (positions
        // 0,1,2), position=2 has trailing_ones=0 so ommer_count may be 1.
        // After 4 appends position=3 has trailing_ones=2, generating ommers.
        for i in 0..4u64 {
            f.append(test_leaf(i)).value.expect("append");
        }
        let data = f.serialize();
        // data layout: 1 (flag) + 8 (position) + 32 (leaf) + 1 (ommer_count) + N*32
        let ommer_count = data[42] as usize;
        assert!(
            ommer_count > 0,
            "need at least one ommer to test truncation"
        );
        // Truncate: keep header + ommer_count byte but chop the ommer data
        let truncated = &data[..43];
        let err = CommitmentFrontier::deserialize(truncated);
        assert!(err.is_err(), "should fail on truncated ommers");
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("truncated ommers"),
            "expected 'truncated ommers' error, got: {msg}"
        );
    }

    #[test]
    fn test_deserialize_invalid_leaf_field_element() {
        // Construct bytes with valid header but an invalid Pallas field element
        // as the leaf (all 0xFF is not a valid point).
        let mut data = vec![0x01]; // has_frontier = true
        data.extend_from_slice(&0u64.to_be_bytes()); // position = 0
        data.extend_from_slice(&[0xFF; 32]); // invalid leaf
        data.push(0); // ommer_count = 0

        let err = CommitmentFrontier::deserialize(&data);
        assert!(err.is_err(), "should fail on invalid leaf field element");
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("invalid Pallas field element"),
            "expected InvalidFieldElement error, got: {msg}"
        );
    }

    #[test]
    fn test_deserialize_invalid_ommer_field_element() {
        // Build a valid frontier, then replace one ommer with 0xFF bytes.
        let mut f = CommitmentFrontier::new();
        for i in 0..4u64 {
            f.append(test_leaf(i)).value.expect("append");
        }
        let mut data = f.serialize();
        let ommer_count = data[42] as usize;
        assert!(ommer_count > 0, "need at least one ommer");
        // First ommer starts at byte 43, replace it with all 0xFF
        for b in &mut data[43..43 + 32] {
            *b = 0xFF;
        }
        let err = CommitmentFrontier::deserialize(&data);
        assert!(err.is_err(), "should fail on invalid ommer field element");
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("invalid Pallas field element"),
            "expected InvalidFieldElement error, got: {msg}"
        );
    }

    #[test]
    fn test_deserialize_from_parts_failure() {
        // Construct technically valid field elements but with an inconsistent
        // position/ommer combination that `Frontier::from_parts` rejects.
        // Position 0 should have 0 ommers; providing 1 ommer triggers the
        // from_parts validation error.
        let valid_leaf = test_leaf(0);
        let valid_ommer = test_leaf(1);

        let mut data = vec![0x01]; // has_frontier
        data.extend_from_slice(&0u64.to_be_bytes()); // position = 0
        data.extend_from_slice(&valid_leaf); // leaf
        data.push(1); // ommer_count = 1 (wrong for position 0)
        data.extend_from_slice(&valid_ommer); // ommer

        let err = CommitmentFrontier::deserialize(&data);
        assert!(err.is_err(), "should fail on inconsistent from_parts");
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("frontier reconstruction"),
            "expected 'frontier reconstruction' error, got: {msg}"
        );
    }

    #[test]
    fn test_append_cost_sinsemilla_hash_calls() {
        let mut f = CommitmentFrontier::new();

        // First append (position 0): 32 hashes + 0 trailing_ones(empty) = 32
        let r0 = f.append(test_leaf(0));
        r0.value.expect("append 0");
        assert_eq!(r0.cost.sinsemilla_hash_calls, 32);

        // Second append (position 0 in frontier before append): trailing_ones(0)
        // = 0 0 in binary is ...0, trailing_ones = 0, so 32 + 0 = 32
        let r1 = f.append(test_leaf(1));
        r1.value.expect("append 1");
        assert_eq!(r1.cost.sinsemilla_hash_calls, 32);

        // Third append (position 1): trailing_ones(1) = 1, so 32 + 1 = 33
        let r2 = f.append(test_leaf(2));
        r2.value.expect("append 2");
        assert_eq!(r2.cost.sinsemilla_hash_calls, 33);

        // Fourth append (position 2): trailing_ones(2=0b10) = 0, so 32
        let r3 = f.append(test_leaf(3));
        r3.value.expect("append 3");
        assert_eq!(r3.cost.sinsemilla_hash_calls, 32);

        // Fifth append (position 3): trailing_ones(3=0b11) = 2, so 34
        let r4 = f.append(test_leaf(4));
        r4.value.expect("append 4");
        assert_eq!(r4.cost.sinsemilla_hash_calls, 34);
    }

    #[test]
    fn test_deserialize_invalid_frontier_flag() {
        // Test with a frontier flag value that is neither 0x00 nor 0x01
        let err = CommitmentFrontier::deserialize(&[0x42]);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("invalid frontier flag: 0x42"),
            "expected 'invalid frontier flag' error, got: {msg}"
        );
    }
}
