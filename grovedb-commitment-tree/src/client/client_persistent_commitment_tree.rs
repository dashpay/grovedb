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
use shardtree::{store::ShardStore, ShardTree};

use super::{
    sqlite_store::{SqliteShardStore, SqliteShardStoreError},
    SHARD_HEIGHT,
};
use crate::commitment_frontier::{merkle_hash_from_bytes, CommitmentTreeError};

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
    ///
    /// Mutating tree operations ([`append`](Self::append),
    /// [`checkpoint`](Self::checkpoint)) wrap their statements in a SQLite
    /// savepoint. Savepoints nest, so calling them while the wallet holds an
    /// open transaction on this connection is supported — the wallet then
    /// owns the final commit or rollback. Because the mutex is released
    /// between individual statements, other threads must not write through
    /// the same connection while a mutating tree operation is in flight: if
    /// the operation fails, its rollback would revert those interleaved
    /// writes as well.
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
    ///
    /// Checkpoint ids must be strictly increasing: appending with
    /// `Retention::Checkpoint { id, .. }` where `id` is not greater than the
    /// current maximum checkpoint id fails with
    /// [`CommitmentTreeError::CheckpointOutOfOrder`] before anything is
    /// written.
    ///
    /// # Atomicity
    ///
    /// The whole operation (shard write, checkpoint row, checkpoint pruning)
    /// runs inside a SQLite savepoint. On `Err` the savepoint has been rolled
    /// back and the persisted tree state is unchanged, so the append can be
    /// safely retried.
    pub fn append(
        &mut self,
        cmx: [u8; 32],
        retention: Retention<u32>,
    ) -> Result<(), CommitmentTreeError> {
        let leaf = merkle_hash_from_bytes(&cmx).ok_or(CommitmentTreeError::InvalidFieldElement)?;
        // `ShardTree::batch_insert` (unlike `ShardTree::append`) does not
        // check checkpoint-id ordering before writing, and the SQLite store
        // would only reject the duplicate id after the shard row is already
        // persisted. Refuse here, before any mutation, with the same
        // strictly-increasing contract as `ShardTree::append`.
        if let Retention::Checkpoint { id, .. } = &retention {
            let max =
                self.inner.store().max_checkpoint_id().map_err(|e| {
                    CommitmentTreeError::InvalidData(format!("max_checkpoint_id: {e}"))
                })?;
            if max.as_ref() >= Some(id) {
                return Err(CommitmentTreeError::CheckpointOutOfOrder {
                    provided: *id,
                    max: max.expect("comparison above requires max to be Some"),
                });
            }
        }
        let start = self.next_position()?;
        self.atomically(|tree| {
            tree.inner
                .batch_insert(start, std::iter::once((leaf, retention)))
                .map_err(|e| CommitmentTreeError::InvalidData(format!("append failed: {e}")))?;
            Ok(())
        })
    }

    /// Create a checkpoint at the current tree state.
    ///
    /// Checkpoints allow `witness_at_checkpoint_depth` to produce witnesses
    /// relative to historical anchors.
    ///
    /// Returns `Ok(false)` without modifying anything if `checkpoint_id` is
    /// not greater than the current maximum checkpoint id.
    ///
    /// # Atomicity
    ///
    /// The whole operation (shard retention update, checkpoint row,
    /// checkpoint pruning) runs inside a SQLite savepoint. On `Err` the
    /// savepoint has been rolled back and the persisted tree state is
    /// unchanged.
    pub fn checkpoint(&mut self, checkpoint_id: u32) -> Result<bool, CommitmentTreeError> {
        self.atomically(|tree| {
            tree.inner
                .checkpoint(checkpoint_id)
                .map_err(|e| CommitmentTreeError::InvalidData(format!("checkpoint failed: {e}")))
        })
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

    /// Run a mutating multi-statement tree operation inside a SQLite
    /// savepoint so it commits or rolls back as a unit.
    ///
    /// `ShardTree` operations issue several store calls (shard writes,
    /// checkpoint inserts, pruning); without this, a storage error partway
    /// through would leave the earlier writes committed while the operation
    /// reports failure. Savepoints nest, so this also composes with a
    /// caller-owned wallet transaction on a shared connection — the wallet
    /// then owns the final commit.
    ///
    /// On a shared connection the mutex is only held per SQL statement, so
    /// other threads must not write through the same connection while a
    /// mutating tree operation is in flight: a rollback here would revert
    /// their interleaved writes as well.
    fn atomically<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, CommitmentTreeError>,
    ) -> Result<T, CommitmentTreeError> {
        self.inner
            .store()
            .with_conn(|conn| conn.execute_batch("SAVEPOINT commitment_tree_client_op"))
            .map_err(|e| CommitmentTreeError::InvalidData(format!("savepoint failed: {e}")))?;
        match f(self) {
            Ok(value) => {
                self.inner
                    .store()
                    .with_conn(|conn| {
                        conn.execute_batch("RELEASE SAVEPOINT commitment_tree_client_op")
                    })
                    .map_err(|e| {
                        CommitmentTreeError::InvalidData(format!("savepoint release failed: {e}"))
                    })?;
                Ok(value)
            }
            Err(e) => {
                // ROLLBACK TO undoes the writes but keeps the savepoint on
                // the stack; RELEASE pops it, restoring the caller's
                // transaction state.
                if let Err(rollback_err) = self.inner.store().with_conn(|conn| {
                    conn.execute_batch(
                        "ROLLBACK TO SAVEPOINT commitment_tree_client_op; RELEASE SAVEPOINT \
                         commitment_tree_client_op",
                    )
                }) {
                    return Err(CommitmentTreeError::InvalidData(format!(
                        "rollback failed after error ({e}): {rollback_err}"
                    )));
                }
                Err(e)
            }
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
