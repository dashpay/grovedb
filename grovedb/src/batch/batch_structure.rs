//! Batch structure

#[cfg(feature = "minimal")]
use std::{
    collections::{btree_map::Entry, BTreeMap},
    fmt,
};

#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{removal::StorageRemovedBytes, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
#[cfg(feature = "minimal")]
use grovedb_storage::worst_case_costs::WorstKeyLength;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use grovedb_visualize::{DebugByteVectors, DebugBytes};
#[cfg(feature = "minimal")]
use intmap::IntMap;

#[cfg(feature = "minimal")]
use crate::{
    batch::{key_info::KeyInfo, GroveOp, KeyInfoPath, QualifiedGroveDbOp, TreeCache},
    ElementFlags, Error,
};

/// Mapping from path to operations keyed by key info.
#[cfg(feature = "minimal")]
pub type OpsByPath = BTreeMap<KeyInfoPath, BTreeMap<KeyInfo, GroveOp>>;
/// Level, path, key, op
#[cfg(feature = "minimal")]
pub type OpsByLevelPath = IntMap<u32, OpsByPath>;

/// Build the synthetic key under which a keyless append-only op is filed.
///
/// The `MaxKeySize` variant sizes estimates with the real tree-key length,
/// while the 8-byte big-endian op-index prefix in `unique_id` keeps several
/// appends to the same tree from collapsing into a single `BTreeMap` entry
/// (each append must be charged). [`keyless_op_tree_key`] is the inverse.
#[cfg(feature = "minimal")]
pub(in crate::batch) fn keyless_op_synthetic_key(op_index: usize, tree_key: &KeyInfo) -> KeyInfo {
    let mut unique_id = (op_index as u64).to_be_bytes().to_vec();
    unique_id.extend_from_slice(tree_key.as_slice());
    KeyInfo::MaxKeySize {
        unique_id,
        max_size: tree_key.max_length(),
    }
}

/// Recover the real tree-key bytes from a [`keyless_op_synthetic_key`].
///
/// Only meaningful for keys of ops that arrive keyless (the append-only tree
/// ops) — a user-supplied `MaxKeySize` key on a keyed op has no such
/// structure, so callers must check the op type before trusting the result.
#[cfg(feature = "minimal")]
pub(in crate::batch) fn keyless_op_tree_key(key: &KeyInfo) -> Option<&[u8]> {
    match key {
        KeyInfo::MaxKeySize { unique_id, .. } => unique_id.get(8..),
        KeyInfo::KnownKey(_) => None,
    }
}

/// Batch structure
#[cfg(feature = "minimal")]
pub(super) struct BatchStructure<C, F, SR> {
    /// Operations by level path
    pub(super) ops_by_level_paths: OpsByLevelPath,
    /// This is for references
    pub(super) ops_by_qualified_paths: BTreeMap<Vec<Vec<u8>>, GroveOp>,
    /// Merk trees
    /// Very important: the type of run mode we are in is contained in this
    /// cache
    pub(super) merk_tree_cache: C,
    /// Flags modification function
    pub(super) flags_update: F,
    /// Split removal bytes
    pub(super) split_removal_bytes: SR,
    /// Last level
    pub(super) last_level: u32,
}

#[cfg(feature = "minimal")]
impl<F, SR, S: fmt::Debug> fmt::Debug for BatchStructure<S, F, SR> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmt_int_map = IntMap::default();
        for (level, path_map) in self.ops_by_level_paths.iter() {
            let mut fmt_path_map = BTreeMap::default();

            for (path, key_map) in path_map.iter() {
                let mut fmt_key_map = BTreeMap::default();

                for (key, op) in key_map.iter() {
                    fmt_key_map.insert(DebugBytes(key.get_key_clone()), op);
                }
                fmt_path_map.insert(DebugByteVectors(path.to_path()), fmt_key_map);
            }
            fmt_int_map.insert(level, fmt_path_map);
        }

        f.debug_struct("BatchStructure")
            .field("ops_by_level_paths", &fmt_int_map)
            .field("merk_tree_cache", &self.merk_tree_cache)
            .field("last_level", &self.last_level)
            .finish()
    }
}

#[cfg(feature = "minimal")]
impl<C, F, SR> BatchStructure<C, F, SR>
where
    C: TreeCache<F, SR>,
    F: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
    SR: FnMut(
        &mut ElementFlags,
        u32,
        u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
{
    /// Create batch structure from a list of ops. Returns CostResult.
    pub(super) fn from_ops(
        ops: Vec<QualifiedGroveDbOp>,
        update_element_flags_function: F,
        split_remove_bytes_function: SR,
        merk_tree_cache: C,
        grove_version: &GroveVersion,
    ) -> CostResult<BatchStructure<C, F, SR>, Error> {
        Self::continue_from_ops(
            None,
            ops,
            update_element_flags_function,
            split_remove_bytes_function,
            merk_tree_cache,
            grove_version,
        )
    }

    /// Create batch structure from a list of ops. Returns CostResult.
    pub(super) fn continue_from_ops(
        previous_ops: Option<OpsByLevelPath>,
        ops: Vec<QualifiedGroveDbOp>,
        update_element_flags_function: F,
        split_remove_bytes_function: SR,
        mut merk_tree_cache: C,
        grove_version: &GroveVersion,
    ) -> CostResult<BatchStructure<C, F, SR>, Error> {
        let keyless_ops_reach_cost_dispatch = grove_version
            .grovedb_versions
            .apply_batch
            .keyless_op_cost_dispatch
            >= 1;
        let mut cost = OperationCost::default();

        // Only a continuation (a partial batch resuming with the callback's
        // add-on ops) carries pending ops an incoming op can collide with.
        let merge_add_on_collisions = previous_ops.is_some()
            && grove_version
                .grovedb_versions
                .apply_batch
                .add_on_op_collision
                >= 1;

        let mut ops_by_level_paths: OpsByLevelPath = previous_ops.unwrap_or_default();
        let mut current_last_level: u32 =
            ops_by_level_paths.iter().map(|(k, _)| k).max().unwrap_or(0);

        // qualified paths meaning path + key
        let mut ops_by_qualified_paths: BTreeMap<Vec<Vec<u8>>, GroveOp> = BTreeMap::new();

        for (op_index, op) in ops.into_iter().enumerate() {
            let QualifiedGroveDbOp {
                path: op_path,
                key: op_key,
                op: grove_op,
            } = op;

            // Keyless ops (append-only tree ops: CommitmentTreeInsert,
            // MmrTreeAppend, BulkAppend, DenseTreeInsert) carry the tree key
            // as the last segment of `path`. In the apply path they are
            // rewritten into keyed ops by preprocessing before reaching here;
            // in the estimated-cost paths there is no preprocessing, so split
            // the tree key off the path and let the op flow to the cost
            // dispatch. Silently dropping them (as V1..V3 do below) makes
            // every append estimate as free — see issue #812. The old skip
            // is version-gated, not deleted: downstream the estimate is an
            // admission bound, and historical blocks admitted under the old
            // under-estimate must re-validate identically on replay.
            //
            // The synthetic key (see `keyless_op_synthetic_key`) sizes
            // estimates with the real tree-key length while keeping one map
            // entry per op, so each append is charged. If such an op ever
            // reaches real execution, `execute_ops_on_path` rejects it with
            // "should have been preprocessed" — a loud failure instead of a
            // silent drop.
            let (op_path, key, is_keyless_append) = match op_key {
                Some(k) => (op_path, k, false),
                None if !keyless_ops_reach_cost_dispatch => continue,
                None => {
                    let mut path = op_path;
                    let Some(tree_key) = path.0.pop() else {
                        return Err(Error::InvalidBatchOperation(
                            "keyless append-only op must have the tree key as its path's last \
                             segment",
                        ))
                        .wrap_with_cost(cost);
                    };
                    let key = keyless_op_synthetic_key(op_index, &tree_key);
                    (path, key, true)
                }
            };

            // Validate key length: Merk link encoding stores key length as a
            // single u8, so keys longer than 255 bytes would corrupt the
            // encoding.
            if let KeyInfo::KnownKey(ref key_bytes) = key
                && key_bytes.len() > u8::MAX as usize
            {
                return Err(Error::InvalidInput("key length must be at most 255 bytes"))
                    .wrap_with_cost(cost);
            }

            // Build qualified path (path + key) for reference lookups.
            // Keyless append ops are skipped: they are not elements a
            // reference can target, and their synthetic keys must not
            // shadow the tree element itself.
            if !is_keyless_append {
                let mut qualified_path = op_path.clone();
                qualified_path.push(key.clone());
                ops_by_qualified_paths.insert(qualified_path.to_path_consume(), grove_op.clone());
            }

            let op_cost = OperationCost::default();
            let op_result = match &grove_op {
                GroveOp::InsertWithKnownToNotAlreadyExist { element }
                | GroveOp::InsertIfNotExists { element, .. }
                | GroveOp::InsertOrReplace { element }
                | GroveOp::Replace { element }
                | GroveOp::Patch { element, .. } => {
                    if let Some(tree_type) = element.tree_type() {
                        cost_return_on_error!(
                            &mut cost,
                            merk_tree_cache.insert(&op_path, &key, tree_type)
                        );
                    }
                    Ok(())
                }
                GroveOp::RefreshReference { .. } | GroveOp::Delete | GroveOp::DeleteTree(..) => {
                    Ok(())
                }
                GroveOp::CommitmentTreeInsert { .. }
                | GroveOp::MmrTreeAppend { .. }
                | GroveOp::BulkAppend { .. }
                | GroveOp::DenseTreeInsert { .. }
                | GroveOp::PrivateDocumentStoreInsert { .. }
                | GroveOp::ReplaceNonMerkTreeRoot { .. } => {
                    // User-facing tree ops are preprocessed before batch
                    // execution into ReplaceNonMerkTreeRoot ops, which must
                    // also pass through here.
                    Ok(())
                }
                GroveOp::ReplaceTreeRootKey { .. }
                | GroveOp::InsertTreeWithRootHash { .. }
                | GroveOp::InsertNonMerkTree { .. }
                | GroveOp::ReplaceAggregateIndexedTreeRootKeys { .. }
                | GroveOp::InsertAggregateIndexedTreeRootKeys { .. } => {
                    Err(Error::InvalidBatchOperation(
                        "replace and insert tree hash are internal operations only",
                    ))
                }
            };
            if let Err(e) = op_result {
                return Err(e).wrap_with_cost(op_cost);
            }

            let level = op_path.len();
            let ops_on_level = ops_by_level_paths.entry(level).or_insert_with(|| {
                if current_last_level < level {
                    current_last_level = level;
                }
                BTreeMap::new()
            });
            match ops_on_level.entry(op_path).or_default().entry(key) {
                Entry::Vacant(vacant) => {
                    vacant.insert(grove_op);
                }
                Entry::Occupied(occupied) => {
                    // Pre-V4 the incoming op replaced whatever was filed —
                    // including a pending root propagation, which orphaned
                    // the subtree it carried (issue #708). See
                    // `merge_add_on_op_over_pending`.
                    let op = if merge_add_on_collisions {
                        cost_return_on_error_no_add!(
                            cost,
                            merge_add_on_op_over_pending(occupied.get(), grove_op)
                        )
                    } else {
                        grove_op
                    };
                    *occupied.into_mut() = op;
                }
            }
        }

        Ok(BatchStructure {
            ops_by_level_paths,
            ops_by_qualified_paths,
            merk_tree_cache,
            flags_update: update_element_flags_function,
            split_removal_bytes: split_remove_bytes_function,
            last_level: current_last_level,
        })
        .wrap_with_cost(cost)
    }
}

/// Resolve an add-on op (returned by a partial batch's callback) that lands
/// on a `(path, key)` the paused batch still has an op filed for.
///
/// The pending op is almost always the internal root propagation of a child
/// tree the batch already executed — `ReplaceTreeRootKey` /
/// `InsertTreeWithRootHash` for a Merk tree, the aggregate-indexed variants
/// for an indexed primary. Replacing it wholesale (the pre-`GROVE_V4`
/// behaviour, issue #708) commits the child's writes but rewrites the parent
/// element from the add-on's bytes: root key `None`, stale aggregate, stale
/// hash — the subtree is orphaned. So the add-on is merged the way upward
/// propagation merges an in-batch insert with its child's computed state:
///
/// - an insert-family op supplies the element bytes and inherits the
///   pending hash / root key / aggregate (and per-axis state);
/// - a delete is accepted only if the child ended the batch empty
///   (`root_key == None`), matching the in-batch rule;
/// - a reference refresh, or a collision with a pending non-Merk root
///   update (`ReplaceNonMerkTreeRoot` / `InsertNonMerkTree`, whose metadata
///   the add-on element cannot reproduce), is refused.
///
/// A pending *user* op (one the paused batch filed but has not executed
/// yet, or an earlier op of the same add-on list) is not root state and is
/// left to the same last-op-wins rule a single batch applies when its
/// consistency check is disabled; with the check enabled the entry point
/// refuses such cross-segment duplicates before the continuation is built
/// (`GroveOp::is_pending_ancestor_update`).
#[cfg(feature = "minimal")]
fn merge_add_on_op_over_pending(pending: &GroveOp, add_on: GroveOp) -> Result<GroveOp, Error> {
    let (hash, root_key, aggregate_data, axes) = match pending {
        GroveOp::ReplaceTreeRootKey {
            hash,
            root_key,
            aggregate_data,
        }
        | GroveOp::InsertTreeWithRootHash {
            hash,
            root_key,
            aggregate_data,
            ..
        } => (*hash, root_key.clone(), *aggregate_data, None),
        GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash,
            primary_root_key,
            primary_aggregate_data,
            axes,
        }
        | GroveOp::InsertAggregateIndexedTreeRootKeys {
            primary_hash,
            primary_root_key,
            primary_aggregate_data,
            axes,
            ..
        } => (
            *primary_hash,
            primary_root_key.clone(),
            *primary_aggregate_data,
            Some(axes.clone()),
        ),
        GroveOp::ReplaceNonMerkTreeRoot { .. } | GroveOp::InsertNonMerkTree { .. } => {
            return Err(Error::InvalidBatchOperation(
                "add-on operation collides with a pending non-Merk tree root update",
            ));
        }
        // A user op carries no root state to preserve: last op wins.
        _ => return Ok(add_on),
    };

    match &add_on {
        GroveOp::InsertOrReplace { element }
        | GroveOp::InsertWithKnownToNotAlreadyExist { element }
        | GroveOp::InsertIfNotExists { element, .. }
        | GroveOp::Replace { element }
        | GroveOp::Patch { element, .. } => crate::batch::insert_op_with_propagated_root(
            element,
            hash,
            root_key,
            aggregate_data,
            axes,
        ),
        GroveOp::Delete | GroveOp::DeleteTree(..) => {
            if root_key.is_some() {
                Err(Error::InvalidBatchOperation(
                    "modification of tree when it will be deleted",
                ))
            } else {
                Ok(add_on)
            }
        }
        GroveOp::RefreshReference { .. } => Err(Error::InvalidBatchOperation(
            "insertion of element under a refreshed reference",
        )),
        // Internal variants were already refused by the op loop above; the
        // append-only user ops must have been preprocessed before reaching
        // the batch structure.
        _ => Err(Error::InvalidBatchOperation(
            "add-on operation cannot be merged with a pending ancestor update",
        )),
    }
}
