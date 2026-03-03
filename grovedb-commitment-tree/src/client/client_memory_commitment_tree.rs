use incrementalmerkletree::{Position, Retention};
use orchard::{
    tree::{MerkleHashOrchard, MerklePath},
    Anchor, NOTE_COMMITMENT_TREE_DEPTH,
};
use shardtree::{store::memory::MemoryShardStore, ShardTree};

use super::SHARD_HEIGHT;
use crate::commitment_frontier::{merkle_hash_from_bytes, CommitmentTreeError};

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
/// [`CommitmentFrontier`](crate::commitment_frontier::CommitmentFrontier)
/// instead.
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
