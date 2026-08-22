//! Root-maintenance version 1: a hash record per position, updated along the
//! inserted position's ancestor path. Used from GROVE_V4.
//!
//! # What an insert at position `p` does
//!
//! 1. Writes the slot (as version 0 does).
//! 2. Hashes the new leaf: `value_hash = blake3(value)`,
//!    `node_hash = blake3(value_hash || 0 || 0)` — both children of `p` are
//!    beyond `count`, hence empty. Writes `p`'s record.
//! 3. Walks up: for each ancestor `a`, the child on the path has the hash
//!    just computed; the other child (`sibling`) is either beyond `count`
//!    (empty, `[0; 32]`, no read) or filled, in which case its record's
//!    `node_hash` is the hash of its subtree as of the last insert into it —
//!    every insert into a subtree rewrites the records of all its ancestors,
//!    so a filled sibling's record is always current. `a`'s own record
//!    supplies its `value_hash` (so its value is not read back).
//!    `node_hash(a) = blake3(value_hash(a) || H(left) || H(right))`, one
//!    blake3; `a`'s record is rewritten.
//! 4. The last ancestor is position 0; its hash is the root.
//!
//! Per insert at depth `d` (root depth 0): `2 + d` blake3 calls, at most
//! `2d` record reads (`d` parent records, at most `d` sibling records), and
//! `d + 1` record writes — O(height), independent of how full the tree is.
//!
//! # Records that cannot be trusted
//!
//! A record is used only when it carries the tree's current generation. A
//! record that is absent (the buffer was filled under version 0) or carries
//! an earlier generation (left by an earlier epoch over the same slot keys,
//! see [`DenseFixedSizedMerkleTree::reset`]) is treated as missing: the
//! subtree is recomputed from its values — the version-0 walk, restricted to
//! that subtree — and the record is written. For a parent only its value
//! hash is needed, so only its value is read and hashed. This catch-up costs
//! at most one version-0 walk per buffer, once, after which every position
//! below `count` has a current record.
//!
//! # Why a stale record cannot be read as current
//!
//! Positions fill sequentially and are written once per generation; every
//! insert under this version writes the record of the position it fills and
//! of every ancestor, and `reset` advances the generation. So a record with
//! the current generation at a position below `count` was written by an
//! insert of this epoch into that position's subtree — later than any value
//! it hashes over. Grove versions are monotonic for a tree, so a version-0
//! insert can never follow a version-1 one into the same epoch.
//!
//! # Cost sizing of record writes
//!
//! A record's key either already holds a value in committed storage (an
//! earlier epoch's record, or this epoch's record written in an earlier
//! session) — then the write is reported as an in-place replacement — or it
//! does not, and the write is new storage. The session learns which from the
//! storage read that resolving the record performs anyway
//! ([`CachedRecord::committed`]); the new leaf's record can pre-exist only
//! when its slot does (the owner says so through
//! [`SlotWriteAccounting::Overwrite`]), in which case it is read once.
//!
//! [`CachedRecord::committed`]: crate::tree::CachedRecord::committed

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;

use crate::{
    hash::node_hash,
    tree::{
        cost_return_on_error, DenseFixedSizedMerkleTree, HashRecord, SlotWriteAccounting,
        HASH_RECORD_LEN,
    },
    DenseMerkleError,
};

const ZERO_HASH: [u8; 32] = [0u8; 32];

/// Write `value` at the next position and bring the records on its ancestor
/// path up to date. Returns the new root and the position. The caller has
/// checked the tree is not full.
pub(super) fn insert_next<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    value: &[u8],
    accounting: SlotWriteAccounting,
) -> CostResult<([u8; 32], u16), DenseMerkleError> {
    let mut cost = OperationCost::default();
    let position = tree.count();
    cost_return_on_error!(cost, tree.put_value(position, value, accounting));
    tree.set_count(position + 1);

    match maintain_path(tree, position, value, accounting).unwrap_add_cost(&mut cost) {
        Ok(root) => Ok((root, position)).wrap_with_cost(cost),
        Err(e) => {
            // Roll back the in-memory view: the count, the cached value, and
            // every cached record — the path may have been half rewritten,
            // and storage (which the caller discards with its transaction)
            // is re-read next time rather than trusted from memory.
            tree.set_count(position);
            tree.uncache_value(position);
            tree.clear_record_cache();
            Err(e).wrap_with_cost(cost)
        }
    }
}

/// Hash the new leaf at `position` and rewrite the records of its ancestors.
/// Returns the root. `tree.count()` already includes `position`.
fn maintain_path<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    position: u16,
    value: &[u8],
    accounting: SlotWriteAccounting,
) -> CostResult<[u8; 32], DenseMerkleError> {
    let mut cost = OperationCost::default();
    let generation = tree.generation();
    let count = tree.count();

    // The new leaf. Both children are at or beyond `count`: empty.
    let value_hash = *blake3::hash(value).as_bytes();
    cost.hash_node_calls += 1;
    let leaf_hash = node_hash(&value_hash, &ZERO_HASH, &ZERO_HASH);
    cost.hash_node_calls += 1;
    // Its record key can pre-exist only if the slot itself does (an earlier
    // epoch wrote both); the owner's slot accounting says whether that is
    // possible, and if so the key is read to size the write.
    let leaf_committed = match accounting {
        SlotWriteAccounting::AsNew => false,
        SlotWriteAccounting::Overwrite { .. } => {
            cost_return_on_error!(cost, tree.read_record_from_storage(position)).is_some()
        }
    };
    cost_return_on_error!(
        cost,
        tree.put_record(
            position,
            HashRecord {
                generation,
                value_hash,
                node_hash: leaf_hash,
            },
            leaf_committed,
        )
    );

    let mut current = position;
    let mut current_hash = leaf_hash;
    while current > 0 {
        let parent = (current - 1) / 2;
        let current_is_left = current % 2 == 1;
        let sibling = if current_is_left {
            current + 1
        } else {
            current - 1
        };
        let sibling_hash = if sibling < count {
            cost_return_on_error!(cost, subtree_hash(tree, sibling))
        } else {
            ZERO_HASH
        };
        let (parent_value_hash, parent_committed) =
            cost_return_on_error!(cost, parent_value_hash(tree, parent));
        let (left, right) = if current_is_left {
            (current_hash, sibling_hash)
        } else {
            (sibling_hash, current_hash)
        };
        let parent_hash = node_hash(&parent_value_hash, &left, &right);
        cost.hash_node_calls += 1;
        cost_return_on_error!(
            cost,
            tree.put_record(
                parent,
                HashRecord {
                    generation,
                    value_hash: parent_value_hash,
                    node_hash: parent_hash,
                },
                parent_committed,
            )
        );
        current = parent;
        current_hash = parent_hash;
    }

    Ok(current_hash).wrap_with_cost(cost)
}

/// The subtree hash of filled position `s` (off the insert path): its current
/// record's `node_hash`, or — when no current record exists — the walk over
/// its values, whose result is recorded so the next insert finds it.
fn subtree_hash<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    s: u16,
) -> CostResult<[u8; 32], DenseMerkleError> {
    let mut cost = OperationCost::default();
    let (record, committed) = cost_return_on_error!(cost, tree.resolve_record(s));
    if let Some(record) = record {
        return Ok(record.node_hash).wrap_with_cost(cost);
    }
    let (value_hash, subtree_hash) = cost_return_on_error!(cost, tree.hash_node_with_value_hash(s));
    cost_return_on_error!(
        cost,
        tree.put_record(
            s,
            HashRecord {
                generation: tree.generation(),
                value_hash,
                node_hash: subtree_hash,
            },
            committed,
        )
    );
    Ok(subtree_hash).wrap_with_cost(cost)
}

/// The value hash of ancestor `a` (on the insert path), and whether its
/// record key exists in committed storage. From its current record when
/// there is one; otherwise its value is read and hashed (its record is about
/// to be rewritten by the caller, so none is written here).
fn parent_value_hash<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    a: u16,
) -> CostResult<([u8; 32], bool), DenseMerkleError> {
    let mut cost = OperationCost::default();
    let (record, committed) = cost_return_on_error!(cost, tree.resolve_record(a));
    if let Some(record) = record {
        return Ok((record.value_hash, committed)).wrap_with_cost(cost);
    }
    let value = match cost_return_on_error!(cost, tree.get_value(a)) {
        Some(v) => v,
        None => {
            return Err(DenseMerkleError::StoreError(format!(
                "expected value at position {} but found none",
                a
            )))
            .wrap_with_cost(cost);
        }
    };
    let value_hash = *blake3::hash(&value).as_bytes();
    cost.hash_node_calls += 1;
    Ok((value_hash, committed)).wrap_with_cost(cost)
}

/// The root: the record at position 0 when a current one exists (this
/// session's, or stored), otherwise the walk over the values. An empty tree
/// has root `[0; 32]` and costs nothing.
///
/// A walk here is not recorded (this is a read); the next insert records it.
pub(super) fn root_hash<'db, S: StorageContext<'db>>(
    tree: &DenseFixedSizedMerkleTree<S>,
) -> CostResult<[u8; 32], DenseMerkleError> {
    let mut cost = OperationCost::default();
    if tree.count() == 0 {
        return Ok(ZERO_HASH).wrap_with_cost(cost);
    }
    if let Some(cached) = tree.cached_record(0) {
        // Charged like the storage read it stands in for, so a root read
        // costs the same in the session that wrote the record and later.
        cost.seek_count += 1;
        cost.storage_loaded_bytes += HASH_RECORD_LEN as u64;
        return Ok(cached.record.node_hash).wrap_with_cost(cost);
    }
    if let Some((Some(record), true)) =
        cost_return_on_error!(cost, tree.read_record_from_storage(0))
    {
        return Ok(record.node_hash).wrap_with_cost(cost);
    }
    tree.compute_root_hash().add_cost(cost)
}
