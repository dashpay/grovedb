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
            None,
            ops,
            update_element_flags_function,
            split_remove_bytes_function,
            merk_tree_cache,
            grove_version,
        )
    }

    /// Create batch structure from a list of ops. Returns CostResult.
    ///
    /// `previous_ops_by_qualified_paths` seeds the reference-resolution map
    /// with an earlier segment's user ops, so a continuation's reference
    /// ops resolve targets that segment wrote exactly as one combined batch
    /// would (in-batch, from the op's own element) instead of reading a
    /// possibly stale committed value. Ops in `ops` override seeded entries
    /// at the same qualified path. The caller must exclude conditional
    /// insertions that the earlier segment skipped, so their proposed
    /// elements cannot shadow the values that actually remain stored.
    pub(super) fn continue_from_ops(
        previous_ops: Option<OpsByLevelPath>,
        previous_ops_by_qualified_paths: Option<BTreeMap<Vec<Vec<u8>>, GroveOp>>,
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
        let mut ops_by_qualified_paths: BTreeMap<Vec<Vec<u8>>, GroveOp> =
            previous_ops_by_qualified_paths.unwrap_or_default();

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

            // A conditional insert colliding with an ancestor update targets a
            // tree that already exists (possibly created by the initial batch).
            // Resolve it before reference registration or cache insertion: a
            // skipped insert must not install the callback's element or reopen
            // its subtree as a fresh Merk.
            if merge_add_on_collisions
                && matches!(&grove_op, GroveOp::InsertIfNotExists { .. })
                && let Some(pending) = ops_by_level_paths
                    .get(op_path.len())
                    .and_then(|level| level.get(&op_path))
                    .and_then(|path_ops| path_ops.get(&key))
                && pending.is_pending_ancestor_update()
            {
                cost_return_on_error_no_add!(cost, merge_add_on_op_over_pending(pending, grove_op));
                continue;
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
                | GroveOp::Patch { element, .. }
                | GroveOp::ReplaceBackwardReferenceFamilyMember { element, .. } => {
                    if let Some(tree_type) = element.tree_type() {
                        cost_return_on_error!(
                            &mut cost,
                            merk_tree_cache.insert(&op_path, &key, tree_type)
                        );
                    }
                    if element.is_indexed_tree() {
                        merk_tree_cache.remember_indexed_element(&op_path, &key, element);
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
/// - an unconditional insert-family op must preserve the tree type and
///   inherits the pending hash / root key / aggregate (and per-axis state);
/// - a conditional insert targets an already-existing tree, so it errors
///   or retains the pending op unchanged, according to `error_if_exists`;
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
pub(super) fn merge_add_on_op_over_pending(
    pending: &GroveOp,
    add_on: GroveOp,
) -> Result<GroveOp, Error> {
    if pending.is_pending_ancestor_update()
        && let GroveOp::InsertIfNotExists {
            error_if_exists, ..
        } = &add_on
    {
        return if *error_if_exists {
            Err(Error::InvalidBatchOperation(
                "attempting to insert subtree that already exists",
            ))
        } else {
            Ok(pending.clone())
        };
    }

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
        | GroveOp::Patch { element, .. } => {
            // The pending state was computed using the child's original node
            // layout and hash scheme. It cannot authenticate a different tree
            // type, including an indexed/non-indexed counterpart with the same
            // aggregate payload, or a non-Merk tree.
            let pending_tree_type = if axes.is_some() {
                aggregate_data.indexed_parent_tree_type()
            } else {
                Some(aggregate_data.parent_tree_type())
            };
            if pending_tree_type.is_none() || element.tree_type() != pending_tree_type {
                return Err(Error::InvalidBatchOperation(
                    "add-on element must preserve the pending tree type",
                ));
            }
            crate::batch::insert_op_with_propagated_root(
                element,
                hash,
                root_key,
                aggregate_data,
                axes,
            )
        }
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

#[cfg(all(test, feature = "minimal"))]
mod tests {
    use grovedb_merk::tree::AggregateData;

    use super::merge_add_on_op_over_pending;
    use crate::{
        batch::{insert_op_with_propagated_root, GroveOp, NonMerkTreeMeta, QualifiedGroveDbOp},
        reference_path::ReferencePathType,
        Element, Error,
    };

    const HASH: [u8; 32] = [7; 32];
    const AXIS_HASH: [u8; 32] = [9; 32];

    fn root_key() -> Option<Vec<u8>> {
        Some(b"root".to_vec())
    }

    fn flags() -> Option<Vec<u8>> {
        Some(vec![1, 2, 3])
    }

    fn axes() -> Vec<(u8, [u8; 32], Option<Vec<u8>>)> {
        vec![(0, AXIS_HASH, Some(b"axis_root".to_vec()))]
    }

    fn pending_merk() -> GroveOp {
        GroveOp::ReplaceTreeRootKey {
            hash: HASH,
            root_key: root_key(),
            aggregate_data: AggregateData::Sum(5),
        }
    }

    fn insert(element: Element) -> GroveOp {
        GroveOp::InsertOrReplace { element }
    }

    fn assert_refused(result: Result<GroveOp, Error>) {
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "expected InvalidBatchOperation, got {result:?}"
        );
    }

    #[test]
    fn indexed_pending_composes_with_add_on_indexed_insert() {
        for pending in [
            GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                primary_hash: HASH,
                primary_root_key: root_key(),
                primary_aggregate_data: AggregateData::ProvableCount(3),
                axes: axes(),
            },
            GroveOp::InsertAggregateIndexedTreeRootKeys {
                element: Element::empty_provable_count_indexed_tree(),
                primary_hash: HASH,
                primary_root_key: root_key(),
                primary_aggregate_data: AggregateData::ProvableCount(3),
                axes: axes(),
            },
        ] {
            let add_on = insert(Element::empty_provable_count_indexed_tree_with_flags(
                flags(),
            ));
            let merged = merge_add_on_op_over_pending(&pending, add_on).expect("merged");
            assert_eq!(
                merged,
                GroveOp::InsertAggregateIndexedTreeRootKeys {
                    element: Element::empty_provable_count_indexed_tree_with_flags(flags()),
                    primary_hash: HASH,
                    primary_root_key: root_key(),
                    primary_aggregate_data: AggregateData::ProvableCount(3),
                    axes: axes(),
                }
            );
        }
    }

    #[test]
    fn pending_non_merk_root_update_refuses_any_add_on() {
        let meta = NonMerkTreeMeta::MmrTree { mmr_size: 4 };
        for pending in [
            GroveOp::ReplaceNonMerkTreeRoot {
                hash: HASH,
                meta: meta.clone(),
            },
            GroveOp::InsertNonMerkTree {
                hash: HASH,
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
                meta,
                non_counted: false,
            },
        ] {
            assert_refused(merge_add_on_op_over_pending(
                &pending,
                insert(Element::new_mmr_tree(4, flags())),
            ));
            assert_refused(merge_add_on_op_over_pending(&pending, GroveOp::Delete));
        }
    }

    #[test]
    fn pending_merk_root_refuses_refresh_and_unpreprocessed_add_ons() {
        let refresh = QualifiedGroveDbOp::refresh_reference_op(
            vec![],
            b"k".to_vec(),
            ReferencePathType::SiblingReference(b"other".to_vec()),
            None,
            None,
            false,
            true,
        )
        .op;
        assert_refused(merge_add_on_op_over_pending(&pending_merk(), refresh));
        assert_refused(merge_add_on_op_over_pending(
            &pending_merk(),
            GroveOp::MmrTreeAppend {
                value: b"leaf".to_vec(),
            },
        ));
    }

    #[test]
    fn pending_merk_root_and_delete_follow_the_emptiness_rule() {
        assert_refused(merge_add_on_op_over_pending(
            &pending_merk(),
            GroveOp::Delete,
        ));
        let emptied = GroveOp::InsertTreeWithRootHash {
            hash: HASH,
            root_key: None,
            aggregate_data: AggregateData::NoAggregateData,
            flags: None,
            non_counted: false,
            not_summed: false,
            not_counted_or_summed: false,
        };
        assert_eq!(
            merge_add_on_op_over_pending(&emptied, GroveOp::Delete).expect("empty child"),
            GroveOp::Delete
        );
    }

    #[test]
    fn unconditional_insert_family_add_on_composes_with_pending_root() {
        let element = Element::empty_sum_tree_with_flags(flags());
        for add_on in [
            GroveOp::InsertWithKnownToNotAlreadyExist {
                element: element.clone(),
            },
            GroveOp::Replace {
                element: element.clone(),
            },
            GroveOp::Patch {
                element: element.clone(),
                change_in_bytes: 0,
            },
        ] {
            assert_eq!(
                merge_add_on_op_over_pending(&pending_merk(), add_on).expect("composed"),
                GroveOp::InsertTreeWithRootHash {
                    hash: HASH,
                    root_key: root_key(),
                    flags: flags(),
                    aggregate_data: AggregateData::Sum(5),
                    non_counted: false,
                    not_summed: false,
                    not_counted_or_summed: false,
                }
            );
        }
    }

    #[test]
    fn add_on_cannot_change_aggregate_or_indexed_tree_kind() {
        let indexed = GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash: HASH,
            primary_root_key: root_key(),
            primary_aggregate_data: AggregateData::ProvableCount(3),
            axes: axes(),
        };
        for element in [
            Element::empty_tree(),
            Element::empty_count_tree(),
            Element::empty_mmr_tree(),
            Element::empty_provable_sum_tree(),
        ] {
            assert_refused(merge_add_on_op_over_pending(
                &pending_merk(),
                insert(element),
            ));
        }
        // The regular and indexed count trees have the SAME aggregate, but
        // their root commitments and owned storage namespaces differ.
        assert_refused(merge_add_on_op_over_pending(
            &indexed,
            insert(Element::empty_provable_count_tree()),
        ));
        assert_refused(merge_add_on_op_over_pending(
            &indexed,
            insert(Element::empty_provable_sum_indexed_tree()),
        ));
        let regular = GroveOp::ReplaceTreeRootKey {
            hash: HASH,
            root_key: root_key(),
            aggregate_data: AggregateData::ProvableCount(3),
        };
        assert_refused(merge_add_on_op_over_pending(
            &regular,
            insert(Element::empty_provable_count_indexed_tree()),
        ));
    }

    #[test]
    fn conditional_insert_preserves_every_pending_ancestor_variant() {
        for pending in [
            pending_merk(),
            GroveOp::InsertTreeWithRootHash {
                hash: HASH,
                root_key: None,
                flags: flags(),
                aggregate_data: AggregateData::Sum(0),
                non_counted: false,
                not_summed: false,
                not_counted_or_summed: false,
            },
            GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                primary_hash: HASH,
                primary_root_key: root_key(),
                primary_aggregate_data: AggregateData::ProvableCount(3),
                axes: axes(),
            },
            GroveOp::InsertAggregateIndexedTreeRootKeys {
                element: Element::empty_provable_count_indexed_tree_with_flags(flags()),
                primary_hash: HASH,
                primary_root_key: root_key(),
                primary_aggregate_data: AggregateData::ProvableCount(3),
                axes: axes(),
            },
            GroveOp::ReplaceNonMerkTreeRoot {
                hash: HASH,
                meta: NonMerkTreeMeta::MmrTree { mmr_size: 4 },
            },
            GroveOp::InsertNonMerkTree {
                hash: HASH,
                root_key: None,
                flags: flags(),
                aggregate_data: AggregateData::NoAggregateData,
                meta: NonMerkTreeMeta::MmrTree { mmr_size: 0 },
                non_counted: false,
            },
        ] {
            for error_if_exists in [true, false] {
                let result = merge_add_on_op_over_pending(
                    &pending,
                    GroveOp::InsertIfNotExists {
                        element: Element::new_item(b"ignored".to_vec()),
                        error_if_exists,
                    },
                );
                if error_if_exists {
                    assert_refused(result);
                } else {
                    assert_eq!(result.unwrap(), pending);
                }
            }
        }
    }

    /// The constructors refuse to wrap an indexed primary, so the helper's
    /// own guard is reached only through a hand-built wrapper.
    #[test]
    fn wrapped_indexed_primary_is_refused() {
        let wrapped = Element::NonCounted(Box::new(Element::empty_provable_count_indexed_tree()));
        assert_refused(insert_op_with_propagated_root(
            &wrapped,
            HASH,
            root_key(),
            AggregateData::ProvableCount(1),
            Some(axes()),
        ));
    }

    #[test]
    fn pending_user_op_is_last_op_wins() {
        let pending = insert(Element::new_item(b"first".to_vec()));
        let add_on = insert(Element::new_item(b"second".to_vec()));
        assert_eq!(
            merge_add_on_op_over_pending(&pending, add_on.clone()).expect("user op"),
            add_on
        );
    }

    #[test]
    fn propagated_root_insert_covers_every_tree_shape() {
        let merk = |element: Element| {
            insert_op_with_propagated_root(&element, HASH, root_key(), AggregateData::Sum(5), None)
        };

        // A plain tree drops the aggregate; a wrapped sum-family tree keeps
        // it and records its wrapper.
        assert_eq!(
            merk(Element::empty_tree_with_flags(flags())).expect("tree"),
            GroveOp::InsertTreeWithRootHash {
                hash: HASH,
                root_key: root_key(),
                flags: flags(),
                aggregate_data: AggregateData::NoAggregateData,
                non_counted: false,
                not_summed: false,
                not_counted_or_summed: false,
            }
        );
        let not_summed = Element::new_not_summed(Element::empty_sum_tree()).expect("wrap");
        assert!(matches!(
            merk(not_summed).expect("not summed sum tree"),
            GroveOp::InsertTreeWithRootHash {
                aggregate_data: AggregateData::Sum(5),
                not_summed: true,
                non_counted: false,
                not_counted_or_summed: false,
                ..
            }
        ));
        let not_counted_or_summed =
            Element::new_not_counted_or_summed(Element::empty_count_sum_tree()).expect("wrap");
        assert!(matches!(
            merk(not_counted_or_summed).expect("wrapped count-sum tree"),
            GroveOp::InsertTreeWithRootHash {
                not_counted_or_summed: true,
                ..
            }
        ));
        for element in [
            Element::empty_big_sum_tree(),
            Element::empty_count_tree(),
            Element::empty_provable_count_tree(),
            Element::empty_provable_sum_tree(),
            Element::empty_provable_count_sum_tree(),
            Element::empty_provable_count_provable_sum_tree(),
        ] {
            assert!(matches!(
                merk(element).expect("merk tree"),
                GroveOp::InsertTreeWithRootHash {
                    aggregate_data: AggregateData::Sum(5),
                    ..
                }
            ));
        }

        // Non-Merk trees carry their own metadata, plus the NonCounted
        // wrapper when present.
        let non_merk = |element: Element, meta: NonMerkTreeMeta, non_counted: bool| {
            assert_eq!(
                merk(element).expect("non-merk tree"),
                GroveOp::InsertNonMerkTree {
                    hash: HASH,
                    root_key: root_key(),
                    flags: None,
                    aggregate_data: AggregateData::Sum(5),
                    meta,
                    non_counted,
                }
            );
        };
        non_merk(
            Element::empty_commitment_tree(4).expect("ct"),
            NonMerkTreeMeta::CommitmentTree {
                total_count: 0,
                chunk_power: 4,
            },
            false,
        );
        non_merk(
            Element::empty_private_document_store(64, 4).expect("pds"),
            NonMerkTreeMeta::PrivateDocumentStore {
                total_count: 0,
                entry_size: 64,
                chunk_power: 4,
            },
            false,
        );
        non_merk(
            Element::new_non_counted(Element::new_mmr_tree(9, None)).expect("wrap"),
            NonMerkTreeMeta::MmrTree { mmr_size: 9 },
            true,
        );
        non_merk(
            Element::empty_bulk_append_tree(4).expect("bulk"),
            NonMerkTreeMeta::BulkAppendTree {
                total_count: 0,
                chunk_power: 4,
            },
            false,
        );
        non_merk(
            Element::new_dense_tree(3, 5, None),
            NonMerkTreeMeta::DenseTree {
                count: 3,
                height: 5,
            },
            false,
        );

        // An indexed primary without per-axis state (only the estimator
        // cache lacks it) files empty axes; a non-tree is refused.
        assert!(matches!(
            merk(Element::empty_provable_count_indexed_tree()).expect("indexed"),
            GroveOp::InsertAggregateIndexedTreeRootKeys { axes, .. } if axes.is_empty()
        ));
        assert_refused(merk(Element::new_item(b"not a tree".to_vec())));
    }
}
