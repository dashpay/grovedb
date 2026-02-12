//! Orchard-style commitment tree integration for GroveDB.
//!
//! This crate wraps the Zcash `orchard` crate's Sinsemilla Merkle tree and
//! adapts it for use as a GroveDB subtree type. The tree is a fixed-depth 32,
//! append-only binary Merkle tree using Sinsemilla hashing over the Pallas
//! curve.
//!
//! # Architecture
//!
//! - Uses `shardtree::ShardTree` for efficient tree management with pruning
//! - Uses `orchard::tree::MerkleHashOrchard` for Sinsemilla-based node hashing
//! - Stores tree data via the `ShardStore` trait (must be implemented for the
//!   backing storage)
//! - Produces 32-byte root hashes compatible with GroveDB's `Hash` type

pub mod kv_store;
pub mod serialization;
mod storage;

// Incremental Merkle tree primitives
use std::fmt::Debug;

pub use incrementalmerkletree::{Address, Hashable, Level, Position, Retention};
pub use kv_store::{KvShardStore, MemKvStore};
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
    FullViewingKey, IncomingViewingKey, OutgoingViewingKey, Scope, SpendAuthorizingKey,
    SpendValidatingKey, SpendingKey,
};
// Note types (orchard::Address aliased to avoid conflict with incrementalmerkletree::Address)
pub use orchard::note::Rho;
// Bundle reconstruction types (needed for deserializing bundles from bytes)
pub use orchard::note::TransmittedNoteCiphertext;
// Orchard tree types
pub use orchard::note::{ExtractedNoteCommitment, Nullifier};
pub use orchard::{
    primitives::redpallas,
    tree::{Anchor, MerkleHashOrchard, MerklePath},
    value::{NoteValue, ValueCommitment},
    Action, Address as PaymentAddress, Bundle, Note, Proof, NOTE_COMMITMENT_TREE_DEPTH,
};
pub use shardtree::store::ShardStore;
use shardtree::ShardTree;
pub use storage::{new_memory_store, MemoryCommitmentStore};
use thiserror::Error;

/// Height of each shard in the commitment tree.
///
/// With depth 32 and shard height 16, the tree is split into 2^16 = 65536
/// shards at the top level, each containing a subtree of depth 16.
pub const SHARD_HEIGHT: u8 = 16;

/// A GroveDB-integrated Orchard commitment tree.
///
/// Wraps `ShardTree` parameterized for Orchard's depth-32 Sinsemilla Merkle
/// tree. The type parameter `S` is the backing store (must implement
/// `ShardStore`).
pub struct CommitmentTree<S: ShardStore<H = MerkleHashOrchard>> {
    inner: ShardTree<S, { NOTE_COMMITMENT_TREE_DEPTH as u8 }, SHARD_HEIGHT>,
}

#[derive(Debug, Error)]
pub enum CommitmentTreeError<E: std::error::Error> {
    #[error("shard tree error: {0}")]
    ShardTree(#[from] shardtree::error::ShardTreeError<E>),
    #[error("tree is full (max {max} leaves)", max = 1u64 << NOTE_COMMITMENT_TREE_DEPTH)]
    TreeFull,
    #[error("checkpoint at depth {0} not found")]
    CheckpointNotFound(usize),
}

impl<S> CommitmentTree<S>
where
    S: ShardStore<H = MerkleHashOrchard>,
    S::CheckpointId: Debug + Ord + Clone,
{
    /// Create a new commitment tree backed by the given store.
    ///
    /// `max_checkpoints` controls how many historical roots are retained
    /// for witness generation.
    pub fn new(store: S, max_checkpoints: usize) -> Self {
        Self {
            inner: ShardTree::new(store, max_checkpoints),
        }
    }

    /// Append a note commitment to the tree.
    ///
    /// The commitment is converted to a `MerkleHashOrchard` leaf and appended
    /// at the next available position.
    pub fn append(
        &mut self,
        cmx: ExtractedNoteCommitment,
        retention: Retention<S::CheckpointId>,
    ) -> Result<(), CommitmentTreeError<S::Error>> {
        let leaf = MerkleHashOrchard::from_cmx(&cmx);
        self.inner.append(leaf, retention)?;
        Ok(())
    }

    /// Append a raw 32-byte commitment to the tree.
    ///
    /// This is useful when the caller has already computed the tree leaf hash.
    pub fn append_raw(
        &mut self,
        leaf: MerkleHashOrchard,
        retention: Retention<S::CheckpointId>,
    ) -> Result<(), CommitmentTreeError<S::Error>> {
        self.inner.append(leaf, retention)?;
        Ok(())
    }

    /// Get the current root hash of the tree.
    ///
    /// Returns the root at the current tree state (no checkpoint depth).
    /// The returned `MerkleHashOrchard` can be converted to `[u8; 32]` for
    /// use as a GroveDB hash.
    pub fn root(&self) -> Result<MerkleHashOrchard, CommitmentTreeError<S::Error>> {
        // None means "current state" (no checkpoint depth)
        self.root_at_checkpoint_depth(None)
    }

    /// Get the root hash as a 32-byte array suitable for GroveDB.
    pub fn root_hash(&self) -> Result<[u8; 32], CommitmentTreeError<S::Error>> {
        Ok(self.root()?.to_bytes())
    }

    /// Get the root at a specific checkpoint depth.
    ///
    /// `None` means the current state, `Some(0)` is the most recent
    /// checkpoint, `Some(1)` is one checkpoint back, etc.
    pub fn root_at_checkpoint_depth(
        &self,
        depth: Option<usize>,
    ) -> Result<MerkleHashOrchard, CommitmentTreeError<S::Error>> {
        let maybe_root = self.inner.root_at_checkpoint_depth(depth)?;
        match (maybe_root, depth) {
            (Some(root), _) => Ok(root),
            // Current state (no checkpoint) — empty tree is valid.
            (None, None) => {
                use incrementalmerkletree::Hashable;
                Ok(MerkleHashOrchard::empty_root(Level::from(
                    NOTE_COMMITMENT_TREE_DEPTH as u8,
                )))
            }
            // Specific checkpoint requested but doesn't exist.
            (None, Some(d)) => Err(CommitmentTreeError::CheckpointNotFound(d)),
        }
    }

    /// Generate a Merkle inclusion proof (witness) for the leaf at `position`.
    ///
    /// The proof is generated against the most recent checkpoint state.
    /// Returns `None` if no witness can be generated for the given position
    /// (e.g., the position has been pruned).
    pub fn witness(
        &self,
        position: Position,
    ) -> Result<
        Option<incrementalmerkletree::MerklePath<MerkleHashOrchard, 32>>,
        CommitmentTreeError<S::Error>,
    > {
        Ok(self.inner.witness_at_checkpoint_depth(position, 0)?)
    }

    /// Generate an Orchard-specific Merkle path for use in ZK proofs.
    ///
    /// Converts the generic `incrementalmerkletree::MerklePath` into
    /// `orchard::tree::MerklePath` suitable for the Orchard circuit.
    pub fn orchard_witness(
        &self,
        position: Position,
    ) -> Result<Option<MerklePath>, CommitmentTreeError<S::Error>> {
        Ok(self.witness(position)?.map(|p| p.into()))
    }

    /// Create a checkpoint at the current tree state.
    ///
    /// Checkpoints allow computing roots and witnesses at historical states.
    pub fn checkpoint(&mut self, id: S::CheckpointId) -> Result<(), CommitmentTreeError<S::Error>> {
        self.inner.checkpoint(id)?;
        Ok(())
    }

    /// Get the anchor (root) for a specific checkpoint.
    pub fn anchor(&self) -> Result<Anchor, CommitmentTreeError<S::Error>> {
        Ok(Anchor::from(self.root()?))
    }

    /// Get the position of the most recently appended leaf.
    ///
    /// Returns `None` if the tree is empty.
    pub fn max_leaf_position(&self) -> Result<Option<Position>, CommitmentTreeError<S::Error>> {
        Ok(self.inner.max_leaf_position(None)?)
    }

    /// Access the underlying `ShardTree` for advanced operations.
    pub fn inner(&self) -> &ShardTree<S, { NOTE_COMMITMENT_TREE_DEPTH as u8 }, SHARD_HEIGHT> {
        &self.inner
    }

    /// Access the underlying `ShardTree` mutably.
    pub fn inner_mut(
        &mut self,
    ) -> &mut ShardTree<S, { NOTE_COMMITMENT_TREE_DEPTH as u8 }, SHARD_HEIGHT> {
        &mut self.inner
    }

    /// Consume the tree and return the underlying store.
    pub fn into_store(self) -> S {
        self.inner.into_store()
    }
}

/// Convert raw 32 bytes to a `MerkleHashOrchard`, returning `None` if the
/// bytes do not represent a valid Pallas field element.
pub fn merkle_hash_from_bytes(bytes: &[u8; 32]) -> Option<MerkleHashOrchard> {
    Option::from(MerkleHashOrchard::from_bytes(bytes))
}

/// Verify that a Merkle path is valid for a given commitment and anchor.
///
/// This is a standalone verification function that does not require access
/// to the full tree.
pub fn verify_inclusion(
    cmx: ExtractedNoteCommitment,
    path: &MerklePath,
    expected_anchor: &Anchor,
) -> bool {
    path.root(cmx) == *expected_anchor
}

#[cfg(test)]
mod tests {
    use incrementalmerkletree::Hashable;

    use super::*;

    /// Create a deterministic test leaf from an index.
    fn test_leaf(index: u64) -> MerkleHashOrchard {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&index.to_le_bytes());
        // Use Sinsemilla combine to produce a valid tree node from raw bytes.
        // We combine at level 0 to get a valid MerkleHashOrchard value.
        let empty = MerkleHashOrchard::empty_leaf();
        MerkleHashOrchard::combine(Level::from(0), &empty, &{
            // Create a slightly different leaf for each index by combining
            // the empty leaf with itself at different levels.
            MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty)
        })
    }

    #[test]
    fn test_empty_tree_root() {
        let store = new_memory_store();
        let tree = CommitmentTree::new(store, 100);
        let root = tree.root().expect("should get root of empty tree");
        let empty_anchor = Anchor::empty_tree();
        assert_eq!(Anchor::from(root), empty_anchor);
    }

    #[test]
    fn test_append_changes_root() {
        let store = new_memory_store();
        let mut tree = CommitmentTree::new(store, 100);

        let empty_root = tree.root_hash().unwrap();

        let leaf = test_leaf(0);
        tree.append_raw(leaf, Retention::Marked).unwrap();

        let new_root = tree.root_hash().unwrap();
        assert_ne!(empty_root, new_root, "root should change after append");
    }

    #[test]
    fn test_append_multiple_and_witness() {
        let store = new_memory_store();
        let mut tree = CommitmentTree::new(store, 100);

        let mut leaves = Vec::new();

        // Append 10 leaves
        for i in 0..10u64 {
            let leaf = test_leaf(i);
            leaves.push(leaf);
            tree.append_raw(leaf, Retention::Marked).unwrap();
        }

        tree.checkpoint(0u32).unwrap();

        // Verify we can get a witness for each position
        for i in 0..10u64 {
            let witness = tree
                .witness(Position::from(i))
                .expect("witness generation should not error")
                .expect("witness should exist for marked position");

            // Verify the witness: compute root from leaf and path
            let computed_root = witness.root(leaves[i as usize]);
            let expected_root = tree.root().unwrap();
            assert_eq!(
                computed_root, expected_root,
                "witness for position {} should produce correct root",
                i
            );
        }
    }

    #[test]
    fn test_verify_inclusion_via_orchard_path() {
        let store = new_memory_store();
        let mut tree = CommitmentTree::new(store, 100);

        let leaf = test_leaf(42);
        // We need an ExtractedNoteCommitment for verify_inclusion.
        // Since MerkleHashOrchard wraps the same bytes, we can convert.
        let leaf_bytes = leaf.to_bytes();
        let cmx = ExtractedNoteCommitment::from_bytes(&leaf_bytes).unwrap();

        tree.append_raw(leaf, Retention::Marked).unwrap();
        tree.checkpoint(0u32).unwrap();

        let path = tree.orchard_witness(Position::from(0u64)).unwrap().unwrap();
        let anchor = tree.anchor().unwrap();

        assert!(
            verify_inclusion(cmx, &path, &anchor),
            "valid inclusion proof should verify"
        );

        // Wrong commitment should fail
        let wrong_leaf_bytes = test_leaf(99).to_bytes();
        let wrong_cmx = ExtractedNoteCommitment::from_bytes(&wrong_leaf_bytes).unwrap();
        assert!(
            !verify_inclusion(wrong_cmx, &path, &anchor),
            "wrong commitment should not verify"
        );
    }

    #[test]
    fn test_root_hash_is_32_bytes() {
        let store = new_memory_store();
        let tree = CommitmentTree::new(store, 100);
        let hash = tree.root_hash().unwrap();
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_deterministic_roots() {
        let mut tree1 = CommitmentTree::new(new_memory_store(), 100);
        let mut tree2 = CommitmentTree::new(new_memory_store(), 100);

        for i in 0..5u64 {
            let leaf = test_leaf(i);
            tree1.append_raw(leaf, Retention::Marked).unwrap();
            tree2.append_raw(leaf, Retention::Marked).unwrap();
        }

        assert_eq!(
            tree1.root_hash().unwrap(),
            tree2.root_hash().unwrap(),
            "identical appends should produce identical roots"
        );
    }

    #[test]
    fn test_different_leaves_different_roots() {
        let mut tree1 = CommitmentTree::new(new_memory_store(), 100);
        let mut tree2 = CommitmentTree::new(new_memory_store(), 100);

        tree1.append_raw(test_leaf(0), Retention::Marked).unwrap();
        tree2.append_raw(test_leaf(1), Retention::Marked).unwrap();

        assert_ne!(
            tree1.root_hash().unwrap(),
            tree2.root_hash().unwrap(),
            "different leaves should produce different roots"
        );
    }

    #[test]
    fn test_checkpoint_preserves_historical_root() {
        let mut tree = CommitmentTree::new(new_memory_store(), 100);

        tree.append_raw(test_leaf(0), Retention::Marked).unwrap();
        tree.checkpoint(0u32).unwrap();
        let root_after_one = tree.root_hash().unwrap();

        tree.append_raw(test_leaf(1), Retention::Marked).unwrap();
        tree.checkpoint(1u32).unwrap();
        let root_after_two = tree.root_hash().unwrap();

        assert_ne!(root_after_one, root_after_two);

        // Historical root at depth 1 should match root_after_one
        let historical = tree.root_at_checkpoint_depth(Some(1)).unwrap().to_bytes();
        assert_eq!(historical, root_after_one);
    }
}
