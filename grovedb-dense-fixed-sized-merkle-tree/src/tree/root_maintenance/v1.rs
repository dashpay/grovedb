//! Root-maintenance version 1: one path record per insert, and a fixed
//! per-insert cost derived from the tree's height. Used from GROVE_V4.
//!
//! # What an insert at position `p` does
//!
//! 1. Writes the slot (as version 0 does).
//! 2. Hashes the new leaf: `value_hash = blake3(value)`,
//!    `node_hash = blake3(value_hash || 0 || 0)` — both children of `p` are
//!    beyond `count`, hence empty.
//! 3. Walks up: for each ancestor `a`, the child on the path has the hash
//!    just computed; the other child (`sibling`) is either beyond `count`
//!    (empty, `[0; 32]`, no read) or filled, in which case its current
//!    subtree hash sits in the path record of the LAST insert into its
//!    subtree (located arithmetically from `count`, see
//!    [`last_filled_in_subtree`]); `a`'s own value hash sits in `a`'s own
//!    record. `node_hash(a) = blake3(value_hash(a) || H(left) || H(right))`,
//!    one blake3 per level.
//! 4. Writes ONE record under `p`'s key: `p`'s value hash and the node hash
//!    of every position on its path (`entry[depth]`, depths `0..=depth(p)`),
//!    fixed size for the tree's height. Nothing else is written: an earlier
//!    record is never rewritten by a normal insert, because the record of
//!    the last insert into a subtree IS that subtree's current hash.
//! 5. The root is `entry[0]` of the last insert's record.
//!
//! # What an insert is charged
//!
//! A FIXED figure for the tree's height, not the work this particular insert
//! did — [`v1_insert_model_cost`]: the blake3 calls and record reads an
//! insert performs, averaged over every position of a full buffer (the
//! average depth is `((h - 2) · 2^h + 2) / (2^h - 1)`, ≈ `h - 2` for the
//! heights that matter), rounded up, plus the two puts (slot, record) the
//! insert really issues, each of a size that does not depend on the
//! position. So every append to a tree of a given height costs the same,
//! whatever its position — the property the fee layer wants — and an
//! estimator can charge exactly it. The real reads are still performed and
//! still deterministic; only the figure returned is the model.
//!
//! # Records that cannot be trusted
//!
//! A record is used only when it carries the tree's current generation. A
//! record that is absent (the buffer was filled under version 0) or carries
//! an earlier generation (left by an earlier epoch over the same slot keys,
//! see [`DenseFixedSizedMerkleTree::reset`]) is treated as missing: the
//! subtree is recomputed from its values — the version-0 walk, restricted to
//! that subtree — and for a parent only its value is read and hashed. This
//! catch-up is READ-ONLY: an insert writes exactly its own path record
//! whatever the buffer's history, so neither its storage writes nor its
//! billed cost (the model) depend on it. The walks repeat while a legacy
//! subtree is needed as a sibling and no version-1 insert has landed in it
//! (which records it); for the append-only family's rolling buffer that
//! ends with the epoch the switch happened in, and the work is no more than
//! version 0 did on every insert.
//!
//! # Why a stale record cannot be read as current
//!
//! Positions fill sequentially and are written once per generation; every
//! insert under this version writes the record of the position it fills,
//! and `reset` advances the generation. A record with the current generation
//! under position `q` was therefore written by the insert at `q` (or
//! synthesized from the values as of a later insert), and the hashes it
//! holds for its path were computed over every value of this epoch below
//! them at that time — the latest state of those subtrees if `q` is the last
//! insert into them, which is exactly when a reader consults it. Grove
//! versions are monotonic for a tree, so a version-0 insert can never follow
//! a version-1 one into the same epoch.
//!
//! # Cost sizing of record writes
//!
//! Under [`SlotWriteAccounting::Churn`] (the bulk-append tree's buffer) a
//! record write is an in-place replacement of its fixed size, whatever the
//! key held. Otherwise (a tree whose buffer is its long-term storage) a
//! first write is new storage and a rewrite a replacement, learned from the
//! read that resolving the record performs anyway ([`CachedRecord::committed`]).
//!
//! [`CachedRecord::committed`]: crate::tree::CachedRecord::committed
//! [`last_filled_in_subtree`]: crate::tree::last_filled_in_subtree

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;

use crate::{
    hash::node_hash,
    tree::{
        cost_return_on_error, depth_of, last_filled_in_subtree, path_record_len,
        DenseFixedSizedMerkleTree, PathRecord, SlotWriteAccounting,
    },
    DenseMerkleError,
};

const ZERO_HASH: [u8; 32] = [0u8; 32];

/// The fixed per-insert figures root-maintenance version 1 charges for a
/// tree of a given height. See [`v1_insert_model_cost`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V1InsertModel {
    /// blake3 calls: two for the leaf plus one per ancestor level, averaged
    /// over a full buffer and rounded up.
    pub hash_node_calls: u32,
    /// Record reads: one for each ancestor's value hash and one for each
    /// filled sibling's subtree hash, averaged over a full buffer and
    /// rounded up.
    pub record_reads: u32,
    /// Bytes of one path record for this height — what each record read
    /// loads and what the insert's record put writes.
    pub record_len: u32,
}

impl V1InsertModel {
    /// The model for a tree of `height` (1..=16).
    pub fn for_height(height: u8) -> Self {
        let h = height.clamp(1, 16) as u64;
        // Positions of a full buffer and the sum of their depths:
        // Σ_{p < 2^h - 1} depth(p) = Σ_{d < h} d · 2^d = (h - 2) · 2^h + 2.
        let positions = (1u64 << h) - 1;
        let total_depth = (h as i64 - 2) * (1i64 << h) + 2;
        let total_depth = total_depth.max(0) as u64;
        // Reads per insert at depth d: d ancestor value hashes, and a
        // sibling subtree hash at every level where the sibling is filled —
        // every ancestor level, and the leaf's own level only when the leaf
        // is a right child (its left sibling is filled). So 2d minus one for
        // every left child, of which a full buffer has 2^(h-1) - 1.
        let left_children = (1u64 << (h - 1)) - 1;
        let total_reads = 2 * total_depth - left_children;
        let ceil_div = |n: u64, d: u64| n.div_ceil(d) as u32;
        Self {
            hash_node_calls: 2 + ceil_div(total_depth, positions),
            record_reads: ceil_div(total_reads, positions),
            record_len: path_record_len(height) as u32,
        }
    }

    /// The model as an [`OperationCost`]: the hashes, the record reads (one
    /// seek and one record of loaded bytes each). The slot put and the record
    /// put are real storage writes and carry their own cost at commit.
    pub fn cost(&self) -> OperationCost {
        OperationCost {
            seek_count: self.record_reads,
            storage_loaded_bytes: self.record_reads as u64 * self.record_len as u64,
            hash_node_calls: self.hash_node_calls,
            ..Default::default()
        }
    }
}

/// The fixed cost root-maintenance version 1 charges for one insert into a
/// tree of `height`, whatever the position — see [`V1InsertModel`].
pub fn v1_insert_model_cost(height: u8) -> OperationCost {
    V1InsertModel::for_height(height).cost()
}

/// Write `value` at the next position, write its path record, and return
/// the new root and the position. The caller has checked the tree is not
/// full. The returned cost is the fixed model for the tree's height plus
/// whatever the storage context charged for the two puts.
pub(super) fn insert_next<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    value: &[u8],
    accounting: SlotWriteAccounting,
) -> CostResult<([u8; 32], u16), DenseMerkleError> {
    // The model is what this insert is charged; the work below is real but
    // its cost is not what is returned.
    let mut cost = v1_insert_model_cost(tree.height());
    let mut work = OperationCost::default();

    let position = tree.count();
    let put_slot = tree.put_value(position, value, accounting);
    cost += put_slot.cost;
    if let Err(e) = put_slot.value {
        return Err(e).wrap_with_cost(cost);
    }
    tree.set_count(position + 1);

    match maintain_path(tree, position, value, accounting).unwrap_add_cost(&mut work) {
        Ok((root, record_put_cost)) => {
            cost += record_put_cost;
            Ok((root, position)).wrap_with_cost(cost)
        }
        Err(e) => {
            // Roll back the in-memory view: the count, the cached value, and
            // every cached record — storage (which the caller discards with
            // its transaction) is re-read next time rather than trusted from
            // memory.
            tree.set_count(position);
            tree.uncache_value(position);
            tree.clear_record_cache();
            Err(e).wrap_with_cost(cost)
        }
    }
}

/// Hash the new leaf at `position`, derive the node hashes of its ancestors,
/// and write its path record. Returns the root and the cost the storage
/// context charged for the record put (the rest of the returned cost is the
/// real work, which the caller discards in favour of the model).
/// `tree.count()` already includes `position`.
fn maintain_path<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    position: u16,
    value: &[u8],
    accounting: SlotWriteAccounting,
) -> CostResult<([u8; 32], OperationCost), DenseMerkleError> {
    let mut cost = OperationCost::default();
    let generation = tree.generation();
    let height = tree.height();
    let count = tree.count();

    // The new leaf. Both children are at or beyond `count`: empty.
    let value_hash = *blake3::hash(value).as_bytes();
    cost.hash_node_calls += 1;
    let leaf_hash = node_hash(&value_hash, &ZERO_HASH, &ZERO_HASH);
    cost.hash_node_calls += 1;
    let mut record = PathRecord::new(generation, value_hash, height);
    record.set_entry(depth_of(position), leaf_hash);

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
        let parent_value_hash = cost_return_on_error!(cost, parent_value_hash(tree, parent));
        let (left, right) = if current_is_left {
            (current_hash, sibling_hash)
        } else {
            (sibling_hash, current_hash)
        };
        let parent_hash = node_hash(&parent_value_hash, &left, &right);
        cost.hash_node_calls += 1;
        record.set_entry(depth_of(parent), parent_hash);
        current = parent;
        current_hash = parent_hash;
    }

    // The leaf's record key can pre-exist only if the slot itself does (an
    // earlier epoch wrote both); under `Overwrite` the owner says so and the
    // key is read once to size the write. `Churn` sizes it as churn without
    // looking; `AsNew` slots never had a record.
    let committed = match accounting {
        SlotWriteAccounting::Overwrite { .. } => {
            cost_return_on_error!(cost, tree.read_record_from_storage(position)).is_some()
        }
        SlotWriteAccounting::AsNew | SlotWriteAccounting::Churn => false,
    };
    let put = tree.put_record(position, record, committed, accounting);
    let put_cost = put.cost;
    match put.value {
        Ok(()) => Ok((current_hash, put_cost)).wrap_with_cost(cost),
        Err(e) => Err(e).wrap_with_cost(cost),
    }
}

/// The subtree hash of filled position `s` (off the insert path): the entry
/// for `s`'s depth in the path record of the last insert into `s`'s subtree.
/// When that record is missing or lacks the entry (the subtree was filled
/// without records, under version 0), the subtree is walked from its values
/// — read-only: nothing is written outside the inserting position's own
/// record, so what an insert writes never depends on the buffer's history.
fn subtree_hash<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    s: u16,
) -> CostResult<[u8; 32], DenseMerkleError> {
    let mut cost = OperationCost::default();
    let depth = depth_of(s);
    let last = last_filled_in_subtree(s, tree.count(), tree.height()).unwrap_or(s);
    let (record, _) = cost_return_on_error!(cost, tree.resolve_record(last));
    if let Some(hash) = record.as_ref().and_then(|r| r.entry(depth)) {
        return Ok(hash).wrap_with_cost(cost);
    }
    tree.hash_node(s).add_cost(cost)
}

/// The value hash of ancestor `a` (on the insert path): from `a`'s own path
/// record; otherwise (`a` was inserted without a record, under version 0)
/// `a`'s value is read and hashed — read-only, see [`subtree_hash`].
fn parent_value_hash<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    a: u16,
) -> CostResult<[u8; 32], DenseMerkleError> {
    let mut cost = OperationCost::default();
    let (record, _) = cost_return_on_error!(cost, tree.resolve_record(a));
    if let Some(record) = record {
        return Ok(record.value_hash).wrap_with_cost(cost);
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
    cost.hash_node_calls += 1;
    Ok(*blake3::hash(&value).as_bytes()).wrap_with_cost(cost)
}

/// The root: `entry[0]` of the last insert's record when current and present
/// (this session's, or stored), otherwise the walk over the values. An
/// empty tree has root `[0; 32]` and costs nothing.
///
/// A walk here is not recorded (this is a read); the next insert records it.
pub(super) fn root_hash<'db, S: StorageContext<'db>>(
    tree: &DenseFixedSizedMerkleTree<S>,
) -> CostResult<[u8; 32], DenseMerkleError> {
    let mut cost = OperationCost::default();
    if tree.count() == 0 {
        return Ok(ZERO_HASH).wrap_with_cost(cost);
    }
    let last = tree.count() - 1;
    if let Some(cached) = tree.cached_record(last) {
        // Charged like the storage read it stands in for.
        cost.seek_count += 1;
        cost.storage_loaded_bytes += path_record_len(tree.height()) as u64;
        if let Some(root) = cached.record.entry(0) {
            return Ok(root).wrap_with_cost(cost);
        }
        return tree.compute_root_hash().add_cost(cost);
    }
    if let Some((Some(record), true)) =
        cost_return_on_error!(cost, tree.read_record_from_storage(last))
        && let Some(root) = record.entry(0)
    {
        return Ok(root).wrap_with_cost(cost);
    }
    tree.compute_root_hash().add_cost(cost)
}
