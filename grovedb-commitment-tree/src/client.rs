//! Client-side commitment tree with full witness generation.
//!
//! This module provides [`ClientMemoryCommitmentTree`], a wrapper around
//! `shardtree::ShardTree` with an in-memory store, pinned to Orchard types.
//! It is intended for wallets and test harnesses that need to generate
//! Merkle path witnesses for spending notes.
//!
//! Enable the `client` feature to use this module:
//! ```toml
//! grovedb-commitment-tree = { version = "4", features = ["client"] }
//! ```

use incrementalmerkletree::{Position, Retention};
use orchard::{
    tree::{Anchor, MerkleHashOrchard, MerklePath},
    NOTE_COMMITMENT_TREE_DEPTH,
};
use shardtree::{store::memory::MemoryShardStore, ShardTree};

use crate::{merkle_hash_from_bytes, CommitmentTreeError};

/// Shard height for the ShardTree. Each shard covers 16 levels.
const SHARD_HEIGHT: u8 = 4;

/// Client-side Orchard commitment tree with full Merkle witness support.
///
/// Wraps `ShardTree<MemoryShardStore<MerkleHashOrchard, u32>, 32, 4>` with
/// a convenient Orchard-typed API. All state is in-memory and lost on drop.
///
/// Use this for:
/// - Wallet note tracking and spend witness generation
/// - Test harnesses that construct valid Orchard spend bundles
///
/// Do **not** use this for server-side anchor tracking — use
/// [`CommitmentFrontier`](crate::CommitmentFrontier) instead.
pub struct ClientMemoryCommitmentTree {
    inner: ShardTree<
        MemoryShardStore<MerkleHashOrchard, u32>,
        { NOTE_COMMITMENT_TREE_DEPTH as u8 },
        SHARD_HEIGHT,
    >,
}

impl ClientMemoryCommitmentTree {
    /// Create a new empty client commitment tree.
    ///
    /// `max_checkpoints` controls how many checkpoints are retained before
    /// the oldest are pruned.
    pub fn new(max_checkpoints: usize) -> Self {
        Self {
            inner: ShardTree::new(MemoryShardStore::empty(), max_checkpoints),
        }
    }

    /// Append a note commitment to the tree.
    ///
    /// `cmx` is the 32-byte extracted note commitment. `retention` controls
    /// whether the leaf is marked for witness generation, checkpointed, or
    /// ephemeral.
    pub fn append(
        &mut self,
        cmx: [u8; 32],
        retention: Retention<u32>,
    ) -> Result<(), CommitmentTreeError> {
        let leaf = merkle_hash_from_bytes(&cmx).ok_or(CommitmentTreeError::InvalidFieldElement)?;
        self.inner
            .batch_insert(self.next_position()?, std::iter::once((leaf, retention)))
            .map_err(|e| CommitmentTreeError::InvalidData(format!("append failed: {e}")))?;
        Ok(())
    }

    /// Create a checkpoint at the current tree state.
    ///
    /// Checkpoints allow `witness_at_checkpoint_depth` to produce witnesses
    /// relative to historical anchors.
    pub fn checkpoint(&mut self, checkpoint_id: u32) -> Result<bool, CommitmentTreeError> {
        self.inner
            .checkpoint(checkpoint_id)
            .map_err(|e| CommitmentTreeError::InvalidData(format!("checkpoint failed: {e}")))
    }

    /// Get the position of the most recently appended leaf.
    ///
    /// Returns `None` if the tree is empty.
    pub fn max_leaf_position(&self) -> Result<Option<Position>, CommitmentTreeError> {
        self.inner
            .max_leaf_position(None)
            .map_err(|e| CommitmentTreeError::InvalidData(format!("max_leaf_position failed: {e}")))
    }

    /// Generate a Merkle witness (authentication path) for spending a note
    /// at the given position.
    ///
    /// `checkpoint_depth` is 0 for the current tree state, 1 for the
    /// previous checkpoint, etc. The leaf at `position` must have been
    /// inserted with `Retention::Marked` or `Retention::Checkpoint { marking:
    /// Marking::Marked, .. }`.
    pub fn witness(
        &self,
        position: Position,
        checkpoint_depth: usize,
    ) -> Result<Option<MerklePath>, CommitmentTreeError> {
        self.inner
            .witness_at_checkpoint_depth(position, checkpoint_depth)
            .map(|opt| opt.map(MerklePath::from))
            .map_err(|e| CommitmentTreeError::InvalidData(format!("witness failed: {e}")))
    }

    /// Get the current root as an Orchard `Anchor`.
    ///
    /// Returns the empty tree anchor if no leaves have been appended.
    pub fn anchor(&self) -> Result<Anchor, CommitmentTreeError> {
        match self
            .inner
            .root_at_checkpoint_depth(None)
            .map_err(|e| CommitmentTreeError::InvalidData(format!("root failed: {e}")))?
        {
            Some(root) => Ok(Anchor::from(root)),
            None => Ok(Anchor::empty_tree()),
        }
    }

    /// Get the next insertion position (0 for empty tree).
    fn next_position(&self) -> Result<Position, CommitmentTreeError> {
        let pos = self
            .inner
            .max_leaf_position(None)
            .map_err(|e| CommitmentTreeError::InvalidData(format!("max_leaf_position: {e}")))?;
        Ok(match pos {
            Some(p) => p + 1,
            None => Position::from(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use incrementalmerkletree::{Hashable, Level, Retention};

    use super::*;

    fn test_leaf(index: u64) -> [u8; 32] {
        let empty = MerkleHashOrchard::empty_leaf();
        let varied =
            MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty);
        MerkleHashOrchard::combine(Level::from(0), &empty, &varied).to_bytes()
    }

    #[test]
    fn test_empty_tree() {
        let tree = ClientMemoryCommitmentTree::new(10);
        assert_eq!(tree.max_leaf_position().unwrap(), None);
        assert_eq!(tree.anchor().unwrap(), Anchor::empty_tree());
    }

    #[test]
    fn test_append_and_position() {
        let mut tree = ClientMemoryCommitmentTree::new(10);

        tree.append(test_leaf(0), Retention::Marked).unwrap();
        assert_eq!(tree.max_leaf_position().unwrap(), Some(Position::from(0)));

        tree.append(test_leaf(1), Retention::Ephemeral).unwrap();
        assert_eq!(tree.max_leaf_position().unwrap(), Some(Position::from(1)));
    }

    #[test]
    fn test_anchor_changes() {
        let mut tree = ClientMemoryCommitmentTree::new(10);
        let empty_anchor = tree.anchor().unwrap();

        tree.append(test_leaf(0), Retention::Marked).unwrap();
        let anchor1 = tree.anchor().unwrap();
        assert_ne!(empty_anchor, anchor1);

        tree.append(test_leaf(1), Retention::Marked).unwrap();
        let anchor2 = tree.anchor().unwrap();
        assert_ne!(anchor1, anchor2);
    }

    #[test]
    fn test_witness_generation() {
        let mut tree = ClientMemoryCommitmentTree::new(10);

        // Append a marked leaf so we can witness it
        tree.append(test_leaf(0), Retention::Marked).unwrap();
        tree.append(test_leaf(1), Retention::Ephemeral).unwrap();
        tree.checkpoint(1).unwrap();

        // Witness for position 0 at current state
        let path = tree.witness(Position::from(0), 0).unwrap();
        assert!(path.is_some(), "should produce witness for marked leaf");
    }

    #[test]
    #[cfg(feature = "server")]
    fn test_frontier_and_client_same_root() {
        use crate::CommitmentFrontier;

        let mut frontier = CommitmentFrontier::new();
        let mut client = ClientMemoryCommitmentTree::new(10);

        for i in 0..20u64 {
            frontier.append(test_leaf(i)).unwrap();
            client.append(test_leaf(i), Retention::Ephemeral).unwrap();
        }

        assert_eq!(frontier.anchor(), client.anchor().unwrap());
    }
}
