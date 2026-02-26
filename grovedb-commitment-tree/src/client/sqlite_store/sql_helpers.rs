//! SQL helper functions for the SQLite shard store.
//!
//! All functions take `&Connection` directly so they can be called from
//! `SqliteShardStore::with_conn`.

use std::collections::BTreeSet;

use incrementalmerkletree::{Address, Level, Position};
use orchard::tree::MerkleHashOrchard;
use rusqlite::{params, Connection, OptionalExtension};
use shardtree::{
    store::{Checkpoint, TreeState},
    LocatedPrunableTree, LocatedTree, PrunableTree, Tree,
};

use super::{
    tree_serialization::{deserialize_tree, serialize_tree},
    SqliteShardStoreError, SHARD_HEIGHT,
};

pub(crate) fn create_tables(conn: &Connection) -> Result<(), SqliteShardStoreError> {
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

pub(crate) fn sql_get_shard(
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

pub(crate) fn sql_last_shard(
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

pub(crate) fn sql_put_shard(
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

pub(crate) fn sql_get_shard_roots(
    conn: &Connection,
) -> Result<Vec<Address>, SqliteShardStoreError> {
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

pub(crate) fn sql_truncate_shards(
    conn: &Connection,
    shard_index: u64,
) -> Result<(), SqliteShardStoreError> {
    conn.execute(
        "DELETE FROM commitment_tree_shards WHERE shard_index >= ?1",
        params![shard_index as i64],
    )?;
    Ok(())
}

pub(crate) fn sql_get_cap(
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

pub(crate) fn sql_put_cap(
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

pub(crate) fn sql_min_checkpoint_id(
    conn: &Connection,
) -> Result<Option<u32>, SqliteShardStoreError> {
    let row: Option<u32> = conn.query_row(
        "SELECT MIN(checkpoint_id) FROM commitment_tree_checkpoints",
        [],
        |row| row.get::<_, Option<u32>>(0),
    )?;
    Ok(row)
}

pub(crate) fn sql_max_checkpoint_id(
    conn: &Connection,
) -> Result<Option<u32>, SqliteShardStoreError> {
    let row: Option<u32> = conn.query_row(
        "SELECT MAX(checkpoint_id) FROM commitment_tree_checkpoints",
        [],
        |row| row.get::<_, Option<u32>>(0),
    )?;
    Ok(row)
}

pub(crate) fn sql_add_checkpoint(
    conn: &Connection,
    checkpoint_id: u32,
    checkpoint: &Checkpoint,
) -> Result<(), SqliteShardStoreError> {
    let tx = conn.unchecked_transaction()?;
    let position: Option<i64> = match checkpoint.tree_state() {
        TreeState::Empty => None,
        TreeState::AtPosition(pos) => Some(u64::from(pos) as i64),
    };
    tx.execute(
        "INSERT INTO commitment_tree_checkpoints (checkpoint_id, position) VALUES (?1, ?2)",
        params![checkpoint_id, position],
    )?;

    for mark_pos in checkpoint.marks_removed() {
        tx.execute(
            "INSERT INTO commitment_tree_checkpoint_marks_removed (checkpoint_id, position) \
             VALUES (?1, ?2)",
            params![checkpoint_id, u64::from(*mark_pos) as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn sql_checkpoint_count(conn: &Connection) -> Result<usize, SqliteShardStoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM commitment_tree_checkpoints",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

pub(crate) fn sql_get_checkpoint_at_depth(
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

pub(crate) fn sql_get_checkpoint(
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

pub(crate) fn sql_list_checkpoints(
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

pub(crate) fn sql_update_checkpoint_with<F>(
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
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id = ?1",
                params![checkpoint_id],
            )?;
            let position: Option<i64> = match cp.tree_state() {
                TreeState::Empty => None,
                TreeState::AtPosition(pos) => Some(u64::from(pos) as i64),
            };
            tx.execute(
                "UPDATE commitment_tree_checkpoints SET position = ?1 WHERE checkpoint_id = ?2",
                params![position, checkpoint_id],
            )?;
            for mark_pos in cp.marks_removed() {
                tx.execute(
                    "INSERT INTO commitment_tree_checkpoint_marks_removed (checkpoint_id, \
                     position) VALUES (?1, ?2)",
                    params![checkpoint_id, u64::from(*mark_pos) as i64],
                )?;
            }
            tx.commit()?;
            Ok(true)
        }
    }
}

pub(crate) fn sql_remove_checkpoint(
    conn: &Connection,
    checkpoint_id: u32,
) -> Result<(), SqliteShardStoreError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id = ?1",
        params![checkpoint_id],
    )?;
    tx.execute(
        "DELETE FROM commitment_tree_checkpoints WHERE checkpoint_id = ?1",
        params![checkpoint_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn sql_truncate_checkpoints_retaining(
    conn: &Connection,
    checkpoint_id: u32,
) -> Result<(), SqliteShardStoreError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id > ?1",
        params![checkpoint_id],
    )?;
    tx.execute(
        "DELETE FROM commitment_tree_checkpoints WHERE checkpoint_id > ?1",
        params![checkpoint_id],
    )?;
    tx.execute(
        "DELETE FROM commitment_tree_checkpoint_marks_removed WHERE checkpoint_id = ?1",
        params![checkpoint_id],
    )?;
    tx.commit()?;
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
