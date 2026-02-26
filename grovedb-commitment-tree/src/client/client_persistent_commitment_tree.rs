//! Persistent client commitment tree backed by SQLite.
//!
//! This module provides [`ClientPersistentCommitmentTree`], a commitment tree
//! that persists its state in a SQLite database. The tree survives application
//! restarts and can be re-opened from the same database.
//!
//! # Bring-your-own-connection
//!
//! You can pass **any** `rusqlite::Connection` — for example, your wallet's
//! existing database. The store only creates its own tables (prefixed with
//! `commitment_tree_`) and will not interfere with other tables.
//!
//! ```ignore
//! use rusqlite::Connection;
//! use grovedb_commitment_tree::ClientPersistentCommitmentTree;
//!
//! // Use your existing wallet database
//! let conn = Connection::open("wallet.db")?;
//! let mut tree = ClientPersistentCommitmentTree::open(conn, 100)?;
//! tree.append(cmx_bytes, Retention::Marked)?;
//! // State is persisted — survives restarts.
//! ```

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use incrementalmerkletree::{Position, Retention};
use orchard::{
    tree::{Anchor, MerklePath},
    NOTE_COMMITMENT_TREE_DEPTH,
};
use rusqlite::Connection;
use shardtree::ShardTree;

use super::sqlite_store::{SqliteShardStore, SqliteShardStoreError};
use crate::commitment_frontier::{merkle_hash_from_bytes, CommitmentTreeError};

/// Shard height for the ShardTree. Each shard covers 16 levels.
const SHARD_HEIGHT: u8 = 4;

/// Persistent Orchard commitment tree backed by SQLite.
///
/// Same API as
/// [`ClientMemoryCommitmentTree`](crate::ClientMemoryCommitmentTree)
/// but all state is persisted to a SQLite database. Drop and re-open from the
/// same database to resume where you left off.
pub struct ClientPersistentCommitmentTree {
    inner: ShardTree<SqliteShardStore, { NOTE_COMMITMENT_TREE_DEPTH as u8 }, SHARD_HEIGHT>,
}

impl ClientPersistentCommitmentTree {
    /// Open a persistent commitment tree using an existing SQLite connection.
    ///
    /// The required tables are created automatically if they don't exist.
    /// Pass your wallet's existing database connection to share the same file.
    pub fn open(conn: Connection, max_checkpoints: usize) -> Result<Self, SqliteShardStoreError> {
        let store = SqliteShardStore::new(conn)?;
        Ok(Self {
            inner: ShardTree::new(store, max_checkpoints),
        })
    }

    /// Open a persistent commitment tree on a shared SQLite connection.
    ///
    /// Use this when your application already holds an `Arc<Mutex<Connection>>`
    /// (e.g., a wallet database). The commitment tree tables are created if
    /// missing, and the mutex is locked only for the duration of each SQL
    /// operation.
    pub fn open_on_shared_connection(
        conn: Arc<Mutex<Connection>>,
        max_checkpoints: usize,
    ) -> Result<Self, SqliteShardStoreError> {
        let store = SqliteShardStore::new_shared(conn)?;
        Ok(Self {
            inner: ShardTree::new(store, max_checkpoints),
        })
    }

    /// Open a persistent commitment tree at the given file path.
    ///
    /// Creates the SQLite database if it doesn't exist. This is a convenience
    /// method for applications that want a dedicated commitment tree database.
    pub fn open_path(
        path: impl AsRef<Path>,
        max_checkpoints: usize,
    ) -> Result<Self, SqliteShardStoreError> {
        let conn = Connection::open(path)?;
        Self::open(conn, max_checkpoints)
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
    /// previous checkpoint, etc.
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
