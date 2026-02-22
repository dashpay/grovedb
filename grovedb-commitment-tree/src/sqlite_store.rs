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

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use incrementalmerkletree::{Address, Level, Position};
use orchard::tree::MerkleHashOrchard;
use rusqlite::{params, Connection, OptionalExtension};
use shardtree::{
    store::{Checkpoint, ShardStore, TreeState},
    LocatedPrunableTree, LocatedTree, Node, PrunableTree, RetentionFlags, Tree,
};

use crate::merkle_hash_from_bytes;

/// Shard height — must match the value used in
/// `ClientPersistentCommitmentTree`.
const SHARD_HEIGHT: u8 = 4;

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
    /// mutex for each individual SQL operation.
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
    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        match &self.holder {
            ConnectionHolder::Owned(conn) => f(conn),
            ConnectionHolder::Shared(arc) => {
                let guard = arc.lock().expect("connection mutex poisoned");
                f(&guard)
            }
        }
    }
}

/// Create the 4 commitment-tree tables if they don't already exist.
fn create_tables(conn: &Connection) -> Result<(), SqliteShardStoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS commitment_tree_shards (
            shard_index INTEGER PRIMARY KEY,
            shard_data  BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS commitment_tree_cap (
            id       INTEGER PRIMARY KEY CHECK (id = 0),
            cap_data BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS commitment_tree_checkpoints (
            checkpoint_id INTEGER PRIMARY KEY,
            position      INTEGER
        );
        CREATE TABLE IF NOT EXISTS commitment_tree_checkpoint_marks_removed (
            checkpoint_id INTEGER NOT NULL,
            position      INTEGER NOT NULL,
            PRIMARY KEY (checkpoint_id, position),
            FOREIGN KEY (checkpoint_id) REFERENCES commitment_tree_checkpoints(checkpoint_id)
        );",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SQL helpers — all take &Connection so they can be called from with_conn
// ---------------------------------------------------------------------------

fn sql_get_shard(
    conn: &Connection,
    shard_root: Address,
) -> Result<Option<LocatedPrunableTree<MerkleHashOrchard>>, SqliteShardStoreError> {
    let index = shard_root.index() as i64;
    let row: Option<Vec<u8>> = conn
        .query_row(
            "SELECT shard_data FROM commitment_tree_shards WHERE shard_index = ?1",
            params![index],
            |row| row.get(0),
        )
        .optional()?;

    match row {
        None => Ok(None),
        Some(data) => {
            let mut pos = 0;
            let tree = deserialize_tree(&data, &mut pos)?;
            let located = LocatedTree::from_parts(shard_root, tree).map_err(|addr| {
                SqliteShardStoreError::Serialization(format!(
                    "tree extends beyond shard root at {addr:?}"
                ))
            })?;
            Ok(Some(located))
        }
    }
}

fn sql_last_shard(
    conn: &Connection,
) -> Result<Option<LocatedPrunableTree<MerkleHashOrchard>>, SqliteShardStoreError> {
    let row: Option<(i64, Vec<u8>)> = conn
        .query_row(
            "SELECT shard_index, shard_data FROM commitment_tree_shards ORDER BY shard_index DESC \
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    match row {
        None => Ok(None),
        Some((index, data)) => {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), index as u64);
            let mut pos = 0;
            let tree = deserialize_tree(&data, &mut pos)?;
            let located = LocatedTree::from_parts(addr, tree).map_err(|addr| {
                SqliteShardStoreError::Serialization(format!(
                    "tree extends beyond shard root at {addr:?}"
                ))
            })?;
            Ok(Some(located))
        }
    }
}

fn sql_put_shard(
    conn: &Connection,
    subtree: &LocatedPrunableTree<MerkleHashOrchard>,
) -> Result<(), SqliteShardStoreError> {
    let index = subtree.root_addr().index() as i64;
    let data = serialize_tree(subtree.root());
    conn.execute(
        "INSERT OR REPLACE INTO commitment_tree_shards (shard_index, shard_data) VALUES (?1, ?2)",
        params![index, data],
    )?;
    Ok(())
}

fn sql_get_shard_roots(conn: &Connection) -> Result<Vec<Address>, SqliteShardStoreError> {
    let mut stmt =
        conn.prepare("SELECT shard_index FROM commitment_tree_shards ORDER BY shard_index")?;
    let rows = stmt.query_map([], |row| {
        let index: i64 = row.get(0)?;
        Ok(Address::from_parts(Level::from(SHARD_HEIGHT), index as u64))
    })?;
    let mut result = Vec::new();
    for addr in rows {
        result.push(addr?);
    }
    Ok(result)
}

fn sql_truncate_shards(conn: &Connection, shard_index: u64) -> Result<(), SqliteShardStoreError> {
    conn.execute(
        "DELETE FROM commitment_tree_shards WHERE shard_index >= ?1",
        params![shard_index as i64],
    )?;
    Ok(())
}

fn sql_get_cap(
    conn: &Connection,
) -> Result<PrunableTree<MerkleHashOrchard>, SqliteShardStoreError> {
    let row: Option<Vec<u8>> = conn
        .query_row(
            "SELECT cap_data FROM commitment_tree_cap WHERE id = 0",
            [],
            |row| row.get(0),
        )
        .optional()?;

    match row {
        None => Ok(Tree::empty()),
        Some(data) => {
            let mut pos = 0;
            deserialize_tree(&data, &mut pos)
        }
    }
}

fn sql_put_cap(
    conn: &Connection,
    cap: &PrunableTree<MerkleHashOrchard>,
) -> Result<(), SqliteShardStoreError> {
    let data = serialize_tree(cap);
    conn.execute(
        "INSERT OR REPLACE INTO commitment_tree_cap (id, cap_data) VALUES (0, ?1)",
        params![data],
    )?;
    Ok(())
}

fn sql_min_checkpoint_id(conn: &Connection) -> Result<Option<u32>, SqliteShardStoreError> {
    let row: Option<u32> = conn.query_row(
        "SELECT MIN(checkpoint_id) FROM commitment_tree_checkpoints",
        [],
        |row| row.get::<_, Option<u32>>(0),
    )?;
    Ok(row)
}

fn sql_max_checkpoint_id(conn: &Connection) -> Result<Option<u32>, SqliteShardStoreError> {
    let row: Option<u32> = conn.query_row(
        "SELECT MAX(checkpoint_id) FROM commitment_tree_checkpoints",
        [],
        |row| row.get::<_, Option<u32>>(0),
    )?;
    Ok(row)
}

fn sql_add_checkpoint(
    conn: &Connection,
    checkpoint_id: u32,
    checkpoint: &Checkpoint,
) -> Result<(), SqliteShardStoreError> {
    let position: Option<i64> = match checkpoint.tree_state() {
        TreeState::Empty => None,
        TreeState::AtPosition(pos) => Some(u64::from(pos) as i64),
    };
    conn.execute(
        "INSERT INTO commitment_tree_checkpoints (checkpoint_id, position) VALUES (?1, ?2)",
        params![checkpoint_id, position],
    )?;

    for mark_pos in checkpoint.marks_removed() {
        conn.execute(
            "INSERT INTO commitment_tree_checkpoint_marks_removed (checkpoint_id, position) \
             VALUES (?1, ?2)",
            params![checkpoint_id, u64::from(*mark_pos) as i64],
        )?;
    }
    Ok(())
}

fn sql_checkpoint_count(conn: &Connection) -> Result<usize, SqliteShardStoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM commitment_tree_checkpoints",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

fn sql_get_checkpoint_at_depth(
    conn: &Connection,
    checkpoint_depth: usize,
) -> Result<Option<(u32, Checkpoint)>, SqliteShardStoreError> {
    let row: Option<(u32, Option<i64>)> = conn
        .query_row(
            "SELECT checkpoint_id, position FROM commitment_tree_checkpoints ORDER BY \
             checkpoint_id DESC LIMIT 1 OFFSET ?1",
            params![checkpoint_depth as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    match row {
        None => Ok(None),
        Some((id, pos)) => {
            let checkpoint = sql_load_checkpoint(conn, id, pos)?;
            Ok(Some((id, checkpoint)))
        }
    }
}

fn sql_get_checkpoint(
    conn: &Connection,
    checkpoint_id: u32,
) -> Result<Option<Checkpoint>, SqliteShardStoreError> {
    let row: Option<Option<i64>> = conn
        .query_row(
            "SELECT position FROM commitment_tree_checkpoints WHERE checkpoint_id = ?1",
            params![checkpoint_id],
            |row| row.get(0),
        )
        .optional()?;

    match row {
        None => Ok(None),
        Some(pos) => {
            let checkpoint = sql_load_checkpoint(conn, checkpoint_id, pos)?;
            Ok(Some(checkpoint))
        }
    }
}

fn sql_list_checkpoints(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(u32, Checkpoint)>, SqliteShardStoreError> {
    let mut stmt = conn.prepare(
        "SELECT checkpoint_id, position FROM commitment_tree_checkpoints ORDER BY checkpoint_id \
         DESC LIMIT ?1",
    )?;
    let rows: Vec<(u32, Option<i64>)> = stmt
        .query_map(params![limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut result = Vec::with_capacity(rows.len());
    for (id, pos) in rows {
        let checkpoint = sql_load_checkpoint(conn, id, pos)?;
        result.push((id, checkpoint));
    }
    Ok(result)
}

fn sql_update_checkpoint_with<F>(
    conn: &Connection,
    checkpoint_id: u32,
    update: F,
) -> Result<bool, SqliteShardStoreError>
where
    F: Fn(&mut Checkpoint) -> Result<(), SqliteShardStoreError>,
{
    let existing = sql_get_checkpoint(conn, checkpoint_id)?;
    match existing {
        None => Ok(false),
        Some(mut cp) => {
            update(&mut cp)?;
            conn.execute(
                "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id = ?1",
                params![checkpoint_id],
            )?;
            let position: Option<i64> = match cp.tree_state() {
                TreeState::Empty => None,
                TreeState::AtPosition(pos) => Some(u64::from(pos) as i64),
            };
            conn.execute(
                "UPDATE commitment_tree_checkpoints SET position = ?1 WHERE checkpoint_id = ?2",
                params![position, checkpoint_id],
            )?;
            for mark_pos in cp.marks_removed() {
                conn.execute(
                    "INSERT INTO commitment_tree_checkpoint_marks_removed (checkpoint_id, \
                     position) VALUES (?1, ?2)",
                    params![checkpoint_id, u64::from(*mark_pos) as i64],
                )?;
            }
            Ok(true)
        }
    }
}

fn sql_remove_checkpoint(
    conn: &Connection,
    checkpoint_id: u32,
) -> Result<(), SqliteShardStoreError> {
    conn.execute(
        "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id = ?1",
        params![checkpoint_id],
    )?;
    conn.execute(
        "DELETE FROM commitment_tree_checkpoints WHERE checkpoint_id = ?1",
        params![checkpoint_id],
    )?;
    Ok(())
}

fn sql_truncate_checkpoints_retaining(
    conn: &Connection,
    checkpoint_id: u32,
) -> Result<(), SqliteShardStoreError> {
    conn.execute(
        "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id > ?1",
        params![checkpoint_id],
    )?;
    conn.execute(
        "DELETE FROM commitment_tree_checkpoints WHERE checkpoint_id > ?1",
        params![checkpoint_id],
    )?;
    conn.execute(
        "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id = ?1",
        params![checkpoint_id],
    )?;
    Ok(())
}

/// Load a full Checkpoint (including marks_removed).
fn sql_load_checkpoint(
    conn: &Connection,
    checkpoint_id: u32,
    position: Option<i64>,
) -> Result<Checkpoint, SqliteShardStoreError> {
    let tree_state = match position {
        None => TreeState::Empty,
        Some(p) => TreeState::AtPosition(Position::from(p as u64)),
    };

    let mut stmt = conn.prepare(
        "SELECT position FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id = ?1",
    )?;
    let marks: BTreeSet<Position> = stmt
        .query_map(params![checkpoint_id], |row| {
            let p: i64 = row.get(0)?;
            Ok(Position::from(p as u64))
        })?
        .collect::<Result<BTreeSet<_>, _>>()?;

    Ok(Checkpoint::from_parts(tree_state, marks))
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

// ---------------------------------------------------------------------------
// Tree serialization
// ---------------------------------------------------------------------------

/// Binary format tags for tree nodes.
const TAG_NIL: u8 = 0x00;
const TAG_LEAF: u8 = 0x01;
const TAG_PARENT: u8 = 0x02;

/// Serialize a `PrunableTree<MerkleHashOrchard>` to bytes.
///
/// Format:
/// - `Nil`:    `[0x00]`
/// - `Leaf`:   `[0x01][hash: 32][flags: 1]`
/// - `Parent`: `[0x02][has_ann: 1][ann?: 32][left][right]`
fn serialize_tree(tree: &PrunableTree<MerkleHashOrchard>) -> Vec<u8> {
    let mut buf = Vec::new();
    serialize_tree_inner(tree, &mut buf);
    buf
}

fn serialize_tree_inner(tree: &PrunableTree<MerkleHashOrchard>, buf: &mut Vec<u8>) {
    match &**tree {
        Node::Nil => {
            buf.push(TAG_NIL);
        }
        Node::Leaf {
            value: (hash, flags),
        } => {
            buf.push(TAG_LEAF);
            buf.extend_from_slice(&hash.to_bytes());
            buf.push(flags.bits());
        }
        Node::Parent { ann, left, right } => {
            buf.push(TAG_PARENT);
            match ann {
                Some(arc_hash) => {
                    buf.push(0x01);
                    buf.extend_from_slice(&arc_hash.to_bytes());
                }
                None => {
                    buf.push(0x00);
                }
            }
            serialize_tree_inner(left, buf);
            serialize_tree_inner(right, buf);
        }
    }
}

/// Deserialize a `PrunableTree<MerkleHashOrchard>` from bytes.
fn deserialize_tree(
    data: &[u8],
    pos: &mut usize,
) -> Result<PrunableTree<MerkleHashOrchard>, SqliteShardStoreError> {
    if *pos >= data.len() {
        return Err(SqliteShardStoreError::Serialization(
            "unexpected end of data".to_string(),
        ));
    }

    let tag = data[*pos];
    *pos += 1;

    match tag {
        TAG_NIL => Ok(Tree::empty()),
        TAG_LEAF => {
            if *pos + 33 > data.len() {
                return Err(SqliteShardStoreError::Serialization(
                    "truncated leaf data".to_string(),
                ));
            }
            let hash_bytes: [u8; 32] = data[*pos..*pos + 32]
                .try_into()
                .map_err(|_| SqliteShardStoreError::Serialization("bad hash".to_string()))?;
            *pos += 32;
            let flags_byte = data[*pos];
            *pos += 1;

            let hash = merkle_hash_from_bytes(&hash_bytes).ok_or_else(|| {
                SqliteShardStoreError::Serialization(
                    "invalid Pallas field element in leaf".to_string(),
                )
            })?;
            let flags = RetentionFlags::from_bits_truncate(flags_byte);
            Ok(Tree::leaf((hash, flags)))
        }
        TAG_PARENT => {
            if *pos >= data.len() {
                return Err(SqliteShardStoreError::Serialization(
                    "truncated parent annotation flag".to_string(),
                ));
            }
            let has_ann = data[*pos];
            *pos += 1;

            let ann: Option<Arc<MerkleHashOrchard>> = if has_ann == 0x01 {
                if *pos + 32 > data.len() {
                    return Err(SqliteShardStoreError::Serialization(
                        "truncated parent annotation".to_string(),
                    ));
                }
                let ann_bytes: [u8; 32] = data[*pos..*pos + 32]
                    .try_into()
                    .map_err(|_| SqliteShardStoreError::Serialization("bad ann".to_string()))?;
                *pos += 32;
                let hash = merkle_hash_from_bytes(&ann_bytes).ok_or_else(|| {
                    SqliteShardStoreError::Serialization(
                        "invalid Pallas field element in annotation".to_string(),
                    )
                })?;
                Some(Arc::new(hash))
            } else {
                None
            };

            let left = deserialize_tree(data, pos)?;
            let right = deserialize_tree(data, pos)?;
            Ok(Tree::parent(ann, left, right))
        }
        other => Err(SqliteShardStoreError::Serialization(format!(
            "unknown tree node tag: 0x{other:02x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use incrementalmerkletree::{Address, Hashable, Level, Position};
    use orchard::tree::MerkleHashOrchard;
    use rusqlite::Connection;
    use shardtree::{
        store::{Checkpoint, ShardStore, TreeState},
        LocatedTree, Node, PrunableTree, RetentionFlags, Tree,
    };

    use super::*;

    fn test_store() -> SqliteShardStore {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        SqliteShardStore::new(conn).expect("create store")
    }

    fn test_hash(i: u8) -> MerkleHashOrchard {
        let empty = MerkleHashOrchard::empty_leaf();
        MerkleHashOrchard::combine(Level::from(i % 31 + 1), &empty, &empty)
    }

    // -- Schema tests --

    #[test]
    fn test_schema_creation() {
        let store = test_store();
        let count: i64 = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE \
                     'commitment_tree_%'",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("query tables");
        assert_eq!(count, 4, "expected 4 commitment_tree_ tables");
    }

    #[test]
    fn test_schema_idempotent() {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        let _store = SqliteShardStore::new(conn).expect("first create");
    }

    #[test]
    fn test_shared_connection() {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        let arc = Arc::new(Mutex::new(conn));
        let mut store = SqliteShardStore::new_shared(arc.clone()).expect("create shared store");

        // Store works
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 0);
        let h = test_hash(1);
        let tree = Tree::leaf((h, RetentionFlags::MARKED));
        let located = LocatedTree::from_parts(addr, tree).expect("create located");
        store.put_shard(located).expect("put shard via shared");

        let retrieved = store.get_shard(addr).expect("get shard via shared");
        assert!(retrieved.is_some());

        // Original Arc is still usable (mutex not poisoned)
        let guard = arc.lock().expect("lock after store ops");
        let count: i64 = guard
            .query_row("SELECT COUNT(*) FROM commitment_tree_shards", [], |row| {
                row.get(0)
            })
            .expect("direct query");
        assert_eq!(count, 1);
    }

    // -- Serialization round-trip tests --

    #[test]
    fn test_serialize_nil() {
        let tree: PrunableTree<MerkleHashOrchard> = Tree::empty();
        let data = serialize_tree(&tree);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize nil");
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_serialize_leaf() {
        let hash = test_hash(1);
        let flags = RetentionFlags::MARKED;
        let tree = Tree::leaf((hash, flags));
        let data = serialize_tree(&tree);
        assert_eq!(data.len(), 34); // 1 tag + 32 hash + 1 flags
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize leaf");
        match &*decoded {
            Node::Leaf { value: (h, f) } => {
                assert_eq!(h.to_bytes(), hash.to_bytes());
                assert_eq!(*f, RetentionFlags::MARKED);
            }
            _ => panic!("expected leaf"),
        }
    }

    #[test]
    fn test_serialize_parent_with_annotation() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);
        let left = Tree::leaf((h1, RetentionFlags::MARKED));
        let right = Tree::leaf((h2, RetentionFlags::CHECKPOINT));
        let tree = Tree::parent(Some(Arc::new(h3)), left, right);
        let data = serialize_tree(&tree);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize parent");
        match &*decoded {
            Node::Parent { ann, .. } => {
                assert!(ann.is_some());
                assert_eq!(ann.as_ref().expect("ann").to_bytes(), h3.to_bytes());
            }
            _ => panic!("expected parent"),
        }
    }

    #[test]
    fn test_serialize_parent_without_annotation() {
        let h1 = test_hash(1);
        let left = Tree::leaf((h1, RetentionFlags::EPHEMERAL));
        let right = Tree::empty();
        let tree: PrunableTree<MerkleHashOrchard> = Tree::parent(None, left, right);
        let data = serialize_tree(&tree);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize parent no ann");
        match &*decoded {
            Node::Parent { ann, .. } => {
                assert!(ann.is_none());
            }
            _ => panic!("expected parent"),
        }
    }

    #[test]
    fn test_serialize_deep_tree() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);
        let leaf1 = Tree::leaf((h1, RetentionFlags::MARKED));
        let leaf2 = Tree::leaf((h2, RetentionFlags::EPHEMERAL));
        let inner: PrunableTree<MerkleHashOrchard> = Tree::parent(None, leaf1, leaf2);
        let leaf3 = Tree::leaf((h3, RetentionFlags::CHECKPOINT | RetentionFlags::MARKED));
        let root: PrunableTree<MerkleHashOrchard> = Tree::parent(Some(Arc::new(h1)), inner, leaf3);
        let data = serialize_tree(&root);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize deep tree");
        assert_eq!(pos, data.len(), "should consume all bytes");
        match &*decoded {
            Node::Parent { ann, left, right } => {
                assert!(ann.is_some());
                match &***left {
                    Node::Parent { ann: inner_ann, .. } => {
                        let _: &Option<Arc<MerkleHashOrchard>> = inner_ann;
                        assert!(inner_ann.is_none());
                    }
                    _ => panic!("expected inner parent"),
                }
                match &***right {
                    Node::Leaf { value: (_, f) } => {
                        assert!(f.is_checkpoint());
                        assert!(f.is_marked());
                    }
                    _ => panic!("expected leaf"),
                }
            }
            _ => panic!("expected root parent"),
        }
    }

    // -- Shard CRUD tests --

    #[test]
    fn test_shard_round_trip() {
        let mut store = test_store();
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 0);
        let h1 = test_hash(1);
        let tree = Tree::leaf((h1, RetentionFlags::MARKED));
        let located = LocatedTree::from_parts(addr, tree).expect("create located tree");
        store.put_shard(located).expect("put shard");

        let retrieved = store.get_shard(addr).expect("get shard");
        assert!(retrieved.is_some());
        let retrieved = retrieved.expect("shard should exist");
        assert_eq!(retrieved.root_addr(), addr);
    }

    #[test]
    fn test_shard_not_found() {
        let store = test_store();
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 42);
        let result = store.get_shard(addr).expect("get shard");
        assert!(result.is_none());
    }

    #[test]
    fn test_last_shard() {
        let mut store = test_store();
        assert!(store.last_shard().expect("last shard empty").is_none());

        for i in 0..3u64 {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), i);
            let h = test_hash(i as u8);
            let tree = Tree::leaf((h, RetentionFlags::EPHEMERAL));
            let located = LocatedTree::from_parts(addr, tree).expect("create located");
            store.put_shard(located).expect("put shard");
        }

        let last = store
            .last_shard()
            .expect("last shard")
            .expect("should exist");
        assert_eq!(last.root_addr().index(), 2);
    }

    #[test]
    fn test_get_shard_roots() {
        let mut store = test_store();
        assert!(store.get_shard_roots().expect("empty roots").is_empty());

        for i in [0, 2, 5u64] {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), i);
            let h = test_hash(i as u8);
            let tree = Tree::leaf((h, RetentionFlags::EPHEMERAL));
            let located = LocatedTree::from_parts(addr, tree).expect("create located");
            store.put_shard(located).expect("put shard");
        }

        let roots = store.get_shard_roots().expect("roots");
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0].index(), 0);
        assert_eq!(roots[1].index(), 2);
        assert_eq!(roots[2].index(), 5);
    }

    #[test]
    fn test_truncate_shards() {
        let mut store = test_store();
        for i in 0..5u64 {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), i);
            let h = test_hash(i as u8);
            let tree = Tree::leaf((h, RetentionFlags::EPHEMERAL));
            let located = LocatedTree::from_parts(addr, tree).expect("create located");
            store.put_shard(located).expect("put shard");
        }

        store.truncate_shards(3).expect("truncate shards");
        let roots = store.get_shard_roots().expect("roots");
        assert_eq!(roots.len(), 3);
        assert_eq!(roots.last().expect("last root").index(), 2);
    }

    // -- Cap tests --

    #[test]
    fn test_cap_empty_default() {
        let store = test_store();
        let cap = store.get_cap().expect("get cap");
        assert!(cap.is_empty());
    }

    #[test]
    fn test_cap_round_trip() {
        let mut store = test_store();
        let h = test_hash(10);
        let cap: PrunableTree<MerkleHashOrchard> = Tree::leaf((h, RetentionFlags::EPHEMERAL));
        store.put_cap(cap).expect("put cap");
        let retrieved = store.get_cap().expect("get cap");
        match &*retrieved {
            Node::Leaf { value: (hash, _) } => {
                assert_eq!(hash.to_bytes(), h.to_bytes());
            }
            _ => panic!("expected leaf cap"),
        }
    }

    // -- Checkpoint tests --

    #[test]
    fn test_checkpoint_empty() {
        let store = test_store();
        assert_eq!(store.checkpoint_count().expect("count"), 0);
        assert!(store.min_checkpoint_id().expect("min").is_none());
        assert!(store.max_checkpoint_id().expect("max").is_none());
    }

    #[test]
    fn test_checkpoint_add_and_get() {
        let mut store = test_store();
        let cp = Checkpoint::at_position(Position::from(42));
        store.add_checkpoint(1, cp).expect("add checkpoint");

        assert_eq!(store.checkpoint_count().expect("count"), 1);
        assert_eq!(store.min_checkpoint_id().expect("min"), Some(1));
        assert_eq!(store.max_checkpoint_id().expect("max"), Some(1));

        let retrieved = store.get_checkpoint(&1).expect("get").expect("exists");
        assert_eq!(
            retrieved.tree_state(),
            TreeState::AtPosition(Position::from(42))
        );
        assert!(retrieved.marks_removed().is_empty());
    }

    #[test]
    fn test_checkpoint_with_marks_removed() {
        let mut store = test_store();
        let mut marks = BTreeSet::new();
        marks.insert(Position::from(10));
        marks.insert(Position::from(20));
        marks.insert(Position::from(30));
        let cp = Checkpoint::from_parts(TreeState::AtPosition(Position::from(50)), marks);
        store.add_checkpoint(5, cp).expect("add checkpoint");

        let retrieved = store.get_checkpoint(&5).expect("get").expect("exists");
        assert_eq!(retrieved.marks_removed().len(), 3);
        assert!(retrieved.marks_removed().contains(&Position::from(10)));
        assert!(retrieved.marks_removed().contains(&Position::from(20)));
        assert!(retrieved.marks_removed().contains(&Position::from(30)));
    }

    #[test]
    fn test_checkpoint_at_depth() {
        let mut store = test_store();
        for i in 1..=5u32 {
            let cp = Checkpoint::at_position(Position::from(i as u64 * 10));
            store.add_checkpoint(i, cp).expect("add checkpoint");
        }

        let (id, cp) = store
            .get_checkpoint_at_depth(0)
            .expect("depth 0")
            .expect("exists");
        assert_eq!(id, 5);
        assert_eq!(cp.tree_state(), TreeState::AtPosition(Position::from(50)));

        let (id, _) = store
            .get_checkpoint_at_depth(2)
            .expect("depth 2")
            .expect("exists");
        assert_eq!(id, 3);

        assert!(store
            .get_checkpoint_at_depth(10)
            .expect("depth 10")
            .is_none());
    }

    #[test]
    fn test_checkpoint_empty_tree_state() {
        let mut store = test_store();
        let cp = Checkpoint::tree_empty();
        store.add_checkpoint(1, cp).expect("add empty checkpoint");

        let retrieved = store.get_checkpoint(&1).expect("get").expect("exists");
        assert_eq!(retrieved.tree_state(), TreeState::Empty);
    }

    #[test]
    fn test_remove_checkpoint() {
        let mut store = test_store();
        let mut marks = BTreeSet::new();
        marks.insert(Position::from(5));
        let cp = Checkpoint::from_parts(TreeState::AtPosition(Position::from(10)), marks);
        store.add_checkpoint(1, cp).expect("add");
        store
            .add_checkpoint(2, Checkpoint::tree_empty())
            .expect("add");

        store.remove_checkpoint(&1).expect("remove");
        assert_eq!(store.checkpoint_count().expect("count"), 1);
        assert!(store.get_checkpoint(&1).expect("get").is_none());
        assert!(store.get_checkpoint(&2).expect("get").is_some());
    }

    #[test]
    fn test_truncate_checkpoints_retaining() {
        let mut store = test_store();
        for i in 1..=5u32 {
            let mut marks = BTreeSet::new();
            marks.insert(Position::from(i as u64));
            let cp =
                Checkpoint::from_parts(TreeState::AtPosition(Position::from(i as u64 * 10)), marks);
            store.add_checkpoint(i, cp).expect("add");
        }

        store.truncate_checkpoints_retaining(&3).expect("truncate");
        assert_eq!(store.checkpoint_count().expect("count"), 3);
        assert_eq!(store.max_checkpoint_id().expect("max"), Some(3));

        let cp3 = store.get_checkpoint(&3).expect("get").expect("exists");
        assert!(cp3.marks_removed().is_empty());

        let cp2 = store.get_checkpoint(&2).expect("get").expect("exists");
        assert_eq!(cp2.marks_removed().len(), 1);
    }

    #[test]
    fn test_update_checkpoint_with() {
        let mut store = test_store();
        let cp = Checkpoint::at_position(Position::from(10));
        store.add_checkpoint(1, cp).expect("add");

        let updated = store
            .update_checkpoint_with(&1, |_cp| Ok(()))
            .expect("update");
        assert!(updated);

        let updated = store
            .update_checkpoint_with(&999, |_| Ok(()))
            .expect("update nonexistent");
        assert!(!updated);
    }

    #[test]
    fn test_for_each_checkpoint() {
        let mut store = test_store();
        for i in 1..=5u32 {
            store
                .add_checkpoint(i, Checkpoint::at_position(Position::from(i as u64)))
                .expect("add");
        }

        let mut ids = Vec::new();
        store
            .for_each_checkpoint(3, |id, _| {
                ids.push(*id);
                Ok(())
            })
            .expect("for_each");
        assert_eq!(ids, vec![5, 4, 3]);
    }
}
