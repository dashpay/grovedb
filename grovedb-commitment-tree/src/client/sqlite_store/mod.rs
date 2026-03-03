//! SQLite-backed ShardStore for persistent commitment tree storage.
//!
//! Implements the `shardtree::store::ShardStore` trait using a SQLite database,
//! allowing commitment tree state to persist across application restarts.
//!
//! The store creates 4 tables with a `commitment_tree_` prefix so it can
//! coexist safely in any existing SQLite database.
//!
//! # Connection modes
//!
//! - **Owned**: `SqliteShardStore::new(conn)` takes ownership of a
//!   `Connection`.
//! - **Shared**: `SqliteShardStore::new_shared(arc)` shares an
//!   `Arc<Mutex<Connection>>` with other components (e.g., PMT's `Database`).

mod sql_helpers;
pub(crate) mod tree_serialization;

use std::sync::{Arc, Mutex};

use incrementalmerkletree::Address;
use orchard::tree::MerkleHashOrchard;
use rusqlite::Connection;
use shardtree::{
    store::{Checkpoint, ShardStore},
    LocatedPrunableTree, PrunableTree,
};
use sql_helpers::*;

// Re-export SHARD_HEIGHT from parent so sql_helpers can use it.
pub(crate) use super::SHARD_HEIGHT;

/// How the store accesses the SQLite connection.
enum ConnectionHolder {
    /// The store owns the connection exclusively.
    Owned(Connection),
    /// The store shares the connection with other components.
    Shared(Arc<Mutex<Connection>>),
}

/// SQLite-backed implementation of `ShardStore` for Orchard commitment trees.
///
/// Stores shard data, cap, and checkpoints in 4 SQLite tables prefixed with
/// `commitment_tree_`. The tables are created automatically on construction.
///
/// # Connection modes
///
/// Use [`new`](Self::new) with an owned `Connection`, or
/// [`new_shared`](Self::new_shared) with an `Arc<Mutex<Connection>>` to share
/// one connection with the rest of your application.
pub struct SqliteShardStore {
    holder: ConnectionHolder,
}

/// Errors from the SQLite shard store.
#[derive(Debug)]
pub enum SqliteShardStoreError {
    /// An error from the underlying SQLite connection.
    Sqlite(rusqlite::Error),
    /// A serialization or deserialization error.
    Serialization(String),
}

impl std::fmt::Display for SqliteShardStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "sqlite error: {e}"),
            Self::Serialization(msg) => write!(f, "serialization error: {msg}"),
        }
    }
}

impl std::error::Error for SqliteShardStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Serialization(_) => None,
        }
    }
}

impl From<rusqlite::Error> for SqliteShardStoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl SqliteShardStore {
    /// Create a store that **owns** the given connection.
    ///
    /// Creates the required tables if they do not already exist.
    pub fn new(conn: Connection) -> Result<Self, SqliteShardStoreError> {
        create_tables(&conn)?;
        Ok(Self {
            holder: ConnectionHolder::Owned(conn),
        })
    }

    /// Create a store that **shares** a connection via
    /// `Arc<Mutex<Connection>>`.
    ///
    /// This lets you use the same SQLite connection that the rest of your
    /// application (e.g., a wallet database) already holds. The store locks the
    /// mutex for each `ShardStore` trait method call, ensuring that
    /// multi-statement operations (like checkpoint add with marks) execute
    /// atomically within a single lock acquisition.
    ///
    /// Creates the required tables if they do not already exist.
    pub fn new_shared(conn: Arc<Mutex<Connection>>) -> Result<Self, SqliteShardStoreError> {
        {
            let guard = conn.lock().expect("connection mutex poisoned");
            create_tables(&guard)?;
        }
        Ok(Self {
            holder: ConnectionHolder::Shared(conn),
        })
    }

    /// Execute a closure with a reference to the underlying connection.
    ///
    /// For the `Owned` variant this is a direct borrow. For `Shared` it
    /// acquires the mutex for the duration of the closure.
    ///
    /// # Panics
    ///
    /// Panics if the shared mutex is poisoned (another thread panicked while
    /// holding the lock). A poisoned mutex means the connection may be in an
    /// inconsistent state, so recovery is not safe.
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        match &self.holder {
            ConnectionHolder::Owned(conn) => f(conn),
            ConnectionHolder::Shared(arc) => {
                let guard = arc.lock().expect("connection mutex poisoned");
                f(&guard)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ShardStore trait implementation — delegates to sql_* via with_conn
// ---------------------------------------------------------------------------

impl ShardStore for SqliteShardStore {
    type CheckpointId = u32;
    type Error = SqliteShardStoreError;
    type H = MerkleHashOrchard;

    fn get_shard(
        &self,
        shard_root: Address,
    ) -> Result<Option<LocatedPrunableTree<Self::H>>, Self::Error> {
        self.with_conn(|conn| sql_get_shard(conn, shard_root))
    }

    fn last_shard(&self) -> Result<Option<LocatedPrunableTree<Self::H>>, Self::Error> {
        self.with_conn(sql_last_shard)
    }

    fn put_shard(&mut self, subtree: LocatedPrunableTree<Self::H>) -> Result<(), Self::Error> {
        self.with_conn(|conn| sql_put_shard(conn, &subtree))
    }

    fn get_shard_roots(&self) -> Result<Vec<Address>, Self::Error> {
        self.with_conn(sql_get_shard_roots)
    }

    fn truncate_shards(&mut self, shard_index: u64) -> Result<(), Self::Error> {
        self.with_conn(|conn| sql_truncate_shards(conn, shard_index))
    }

    fn get_cap(&self) -> Result<PrunableTree<Self::H>, Self::Error> {
        self.with_conn(sql_get_cap)
    }

    fn put_cap(&mut self, cap: PrunableTree<Self::H>) -> Result<(), Self::Error> {
        self.with_conn(|conn| sql_put_cap(conn, &cap))
    }

    fn min_checkpoint_id(&self) -> Result<Option<Self::CheckpointId>, Self::Error> {
        self.with_conn(sql_min_checkpoint_id)
    }

    fn max_checkpoint_id(&self) -> Result<Option<Self::CheckpointId>, Self::Error> {
        self.with_conn(sql_max_checkpoint_id)
    }

    fn add_checkpoint(
        &mut self,
        checkpoint_id: Self::CheckpointId,
        checkpoint: Checkpoint,
    ) -> Result<(), Self::Error> {
        self.with_conn(|conn| sql_add_checkpoint(conn, checkpoint_id, &checkpoint))
    }

    fn checkpoint_count(&self) -> Result<usize, Self::Error> {
        self.with_conn(sql_checkpoint_count)
    }

    fn get_checkpoint_at_depth(
        &self,
        checkpoint_depth: usize,
    ) -> Result<Option<(Self::CheckpointId, Checkpoint)>, Self::Error> {
        self.with_conn(|conn| sql_get_checkpoint_at_depth(conn, checkpoint_depth))
    }

    fn get_checkpoint(
        &self,
        checkpoint_id: &Self::CheckpointId,
    ) -> Result<Option<Checkpoint>, Self::Error> {
        self.with_conn(|conn| sql_get_checkpoint(conn, *checkpoint_id))
    }

    fn with_checkpoints<F>(&mut self, limit: usize, mut callback: F) -> Result<(), Self::Error>
    where
        F: FnMut(&Self::CheckpointId, &Checkpoint) -> Result<(), Self::Error>,
    {
        let entries = self.with_conn(|conn| sql_list_checkpoints(conn, limit))?;
        for (id, checkpoint) in &entries {
            callback(id, checkpoint)?;
        }
        Ok(())
    }

    fn for_each_checkpoint<F>(&self, limit: usize, mut callback: F) -> Result<(), Self::Error>
    where
        F: FnMut(&Self::CheckpointId, &Checkpoint) -> Result<(), Self::Error>,
    {
        let entries = self.with_conn(|conn| sql_list_checkpoints(conn, limit))?;
        for (id, checkpoint) in &entries {
            callback(id, checkpoint)?;
        }
        Ok(())
    }

    fn update_checkpoint_with<F>(
        &mut self,
        checkpoint_id: &Self::CheckpointId,
        update: F,
    ) -> Result<bool, Self::Error>
    where
        F: Fn(&mut Checkpoint) -> Result<(), Self::Error>,
    {
        self.with_conn(|conn| sql_update_checkpoint_with(conn, *checkpoint_id, update))
    }

    fn remove_checkpoint(&mut self, checkpoint_id: &Self::CheckpointId) -> Result<(), Self::Error> {
        self.with_conn(|conn| sql_remove_checkpoint(conn, *checkpoint_id))
    }

    fn truncate_checkpoints_retaining(
        &mut self,
        checkpoint_id: &Self::CheckpointId,
    ) -> Result<(), Self::Error> {
        self.with_conn(|conn| sql_truncate_checkpoints_retaining(conn, *checkpoint_id))
    }
}
