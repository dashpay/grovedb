//! Indexed-tree (PCIT / PSIT / PCPSIT) helpers for the batch apply pipeline.
//!
//! These functions encapsulate the indexed-primary propagation steps that
//! `execute_ops_on_path` runs around the merk apply boundary, plus the
//! pre-apply consistency checks the `apply_batch*` entry points run before
//! any merk is touched:
//!
//! - [`capture_indexed_pre_state`] validates this level's ops against the
//!   indexed-primary rules and reads each mutated key's *old* `(count, sum)`
//!   pair, before the merk is mutated, so the post-apply mirror can compute
//!   old → new transitions.
//! - [`apply_indexed_secondary_mirror_post_apply`] runs once per configured
//!   axis after the primary merk's `apply_with_specialized_costs` returns.
//!   It re-reads each captured key's new aggregates, assembles one atomic
//!   Merk batch of row moves for that axis's secondary, applies it, and
//!   returns the secondary's post-mirror `(root_hash, root_key)` for the
//!   bubble-up to fold into the parent's H1-A composition — directly for
//!   the single-axis variants, through `axes_digest` for PCPSIT.
//! - [`reject_freshly_inserted_cidx_with_descendants`] is the preflight
//!   guard that rejects batches which both create an indexed primary AND
//!   write under it in the same batch — there is no
//!   `InsertAggregateIndexedTreeWithRootKeys` op, so the H1-A propagation
//!   cannot read the just-created element from a parent merk that has not
//!   been flushed yet.
//! - [`inspect_cidx_overwrite`] runs inside the per-op loop when
//!   tree-override protection is OFF and the op could overwrite an existing
//!   element (V4+ only — the stored-element read it starts with is gated by
//!   `overwrite_indexed_cleanup_inspection` so released versions keep their
//!   cost shape). It classifies overwrites of every indexed variant:
//!   indexed → non-indexed and indexed → empty indexed are allowed and
//!   schedule the old tree's storage for cleanup; indexed → non-empty
//!   indexed is rejected as ambiguous.
//!
//! Kept out of `mod.rs` to keep that file focused on the generic batch
//! pipeline; the two-Merk propagation pattern is self-contained here and
//! shared by all three indexed variants.

use std::collections::BTreeMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, delete::ElementDeleteFromStorageExtensions,
        insert::ElementInsertToStorageExtensions, tree_type::ElementTreeTypeExtensions,
    },
    BatchEntry, CryptoHash, Merk,
};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use super::{GroveOp, KeyInfo, QualifiedGroveDbOp};
use crate::{
    operations::indexed_tree::{make_axis_secondary_key, MAX_CIDX_ITEM_KEY_LEN},
    Element, Error,
};

/// A primary entry's `(count, sum)` aggregate pair, `None` when the entry
/// does not exist on that side of the transition.
type AggregatePair = Option<(u64, i64)>;

/// One captured key's aggregate transition: `(item_key, old, new)`.
type AggregateTransition = (Vec<u8>, AggregatePair, AggregatePair);

/// Enforce the item-key ceiling for children of an indexed primary.
///
/// The loosest bound that applies to EVERY axis (count and sum both prepend
/// an 8-byte sort key; Merk requires keys < 256 bytes). Generic batch
/// validation only enforces the 255-byte cap. The tighter avg bound (16-byte
/// prefix, 239 bytes) is enforced per axis in
/// [`apply_indexed_secondary_mirror_post_apply`], where the axis is known —
/// deriving it here from the tree type alone would assume every PCPSIT
/// indexes avg and wrongly reject 240..=247-byte keys on one that does not,
/// which the dedicated insert path accepts.
fn enforce_indexed_item_key_ceiling(
    ops_at_path_by_key: &BTreeMap<KeyInfo, GroveOp>,
) -> Result<(), Error> {
    for key_info in ops_at_path_by_key.keys() {
        if key_info.as_slice().len() > MAX_CIDX_ITEM_KEY_LEN {
            return Err(Error::InvalidInput(
                "item key for an indexed-tree primary must be at most 247 bytes in batch ops \
                 (the secondary key is sort_key ‖ item_key and Merk requires keys < 256 \
                 bytes); a tree indexing the avg axis is bounded further at 239",
            ));
        }
    }
    Ok(())
}

/// Validate every caller-supplied element this level's ops would place under
/// an indexed primary, with the same rules the dedicated insert paths
/// enforce (`reject_non_empty_dedicated_indexed_child_claim` and friends):
///
/// - the element must be insertable into a tree of the primary's type,
/// - it must satisfy the primary variant's child-shape rule (sum-bearing
///   for PSIT, count-and-sum-bearing for PCPSIT),
/// - and it may not claim a NON-ZERO aggregate while being ROOTLESS. With no
///   contents to derive the value from it is a bare assertion, and under an
///   indexed primary it becomes the authenticated secondary sort key and the
///   parent's aggregate contribution.
///
/// The batch door has to be shut as well as the dedicated one: an
/// `insert_or_replace_op` carrying `ProvableCountTree(None, 9, None)` under
/// a PCIT was accepted and written, and `verify_grovedb` then reported the
/// child as an aggregate mismatch (recorded 9 against an empty inner Merk).
/// The legitimate way to reach count 9 is to insert the child empty and
/// populate it in the same batch, letting propagation derive it.
fn validate_indexed_child_ops(
    ops_at_path_by_key: &BTreeMap<KeyInfo, GroveOp>,
    primary_tree_type: grovedb_merk::TreeType,
) -> Result<(), Error> {
    for op in ops_at_path_by_key.values() {
        // EXHAUSTIVE on purpose — no `_` arm. An earlier revision used a
        // catch-all and silently skipped `GroveOp::Patch`, which carries an
        // element like the insert/replace ops do: a patch of
        // `ProvableCountTree(None, 9, None)` was written unchecked, moved the
        // authenticated root, and returned the forged 9 through top-k. Listing
        // every variant makes a new op a compile error here rather than a
        // silent hole, the same technique `GroveOp::can_mutate_child_count`
        // uses for exactly this bug class.
        let element = match op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::Replace { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => element,
            // Ops that carry no caller-supplied element, or whose element is
            // internally derived rather than caller-claimed.
            GroveOp::Delete
            | GroveOp::DeleteTree(..)
            | GroveOp::ReplaceTreeRootKey { .. }
            | GroveOp::InsertTreeWithRootHash { .. }
            | GroveOp::ReplaceNonMerkTreeRoot { .. }
            | GroveOp::InsertNonMerkTree { .. }
            | GroveOp::ReplaceAggregateIndexedTreeRootKeys { .. }
            | GroveOp::RefreshReference { .. }
            | GroveOp::CommitmentTreeInsert { .. }
            | GroveOp::MmrTreeAppend { .. }
            | GroveOp::BulkAppend { .. }
            | GroveOp::DenseTreeInsert { .. } => continue,
        };
        // Child-type acceptance, delegated to merk's own rule rather than a
        // second copy of it: `get_feature_type` is what decides whether an
        // element can live in a tree of this type, and it is what the
        // dedicated insert path already enforces. Without this the batch door
        // accepted children the dedicated door refused — a `SumItem` in a
        // count-only PCIT primary was written, `verify_grovedb` reported it
        // clean, and the caller's sum was silently dropped because a
        // count-only primary aggregates nothing else.
        element
            .validate_insertable_into(primary_tree_type)
            .map_err(Error::MerkError)?;
        crate::operations::indexed_tree::validate_indexed_child_for_variant(
            element,
            primary_tree_type,
        )?;
        let rootless_with_aggregate = match element.underlying() {
            Element::SumTree(None, sum, _) | Element::ProvableSumTree(None, sum, _) => *sum != 0,
            Element::BigSumTree(None, big_sum, _) => *big_sum != 0,
            Element::CountTree(None, count, _) | Element::ProvableCountTree(None, count, _) => {
                *count != 0
            }
            Element::CountSumTree(None, count, sum, _)
            | Element::ProvableCountSumTree(None, count, sum, _)
            | Element::ProvableCountProvableSumTree(None, count, sum, _) => {
                *count != 0 || *sum != 0
            }
            _ => false,
        };
        if rootless_with_aggregate {
            return Err(Error::InvalidBatchOperation(
                "a child of an indexed-tree primary may not claim a non-zero \
                 aggregate while having no root key: with no contents to derive \
                 it from, the value is a bare assertion that would become the \
                 authenticated secondary sort key. Insert the child empty and \
                 populate it (in the same batch is fine) so the aggregate is derived",
            ));
        }
    }
    Ok(())
}

/// Validate this level's ops against the indexed-primary rules, then
/// capture — *before* batch ops are applied to the primary merk — the
/// pre-apply `(count, sum)` pair for each key the ops will mutate. The
/// post-apply mirror uses the captured state to compute each entry's
/// old → new transition.
///
/// Both aggregates are captured regardless of which axes are configured:
/// the avg axis derives its sort key from the pair, and a PCPSIT can index
/// count, sum and avg simultaneously.
///
/// Only ops whose `can_mutate_child_count()` is true are captured —
/// non-count-mutating ops (e.g., `CommitmentTreeInsert`) are skipped.
pub(crate) fn capture_indexed_pre_state<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    ops_at_path_by_key: &BTreeMap<KeyInfo, GroveOp>,
    grove_version: &GroveVersion,
) -> CostResult<BTreeMap<Vec<u8>, AggregatePair>, Error> {
    let mut cost = OperationCost::default();

    cost_return_on_error_no_add!(cost, enforce_indexed_item_key_ceiling(ops_at_path_by_key));
    cost_return_on_error_no_add!(
        cost,
        validate_indexed_child_ops(ops_at_path_by_key, primary_merk.tree_type)
    );

    let mut pre: BTreeMap<Vec<u8>, AggregatePair> = BTreeMap::new();
    for (key_info, op) in ops_at_path_by_key.iter() {
        let key_bytes = key_info.get_key_clone();
        // Single source of truth: `GroveOp::can_mutate_child_count`
        // uses an exhaustive match so adding a new variant forces
        // explicit classification at the type-system level. This is
        // the structural guard against the nested-cidx bug class
        // (commit a8bb34fb).
        if op.can_mutate_child_count() && !pre.contains_key(&key_bytes) {
            let old_aggregates = cost_return_on_error!(
                &mut cost,
                read_entry_aggregates(primary_merk, &key_bytes, "pre", grove_version)
            );
            pre.insert(key_bytes, old_aggregates);
        }
    }
    Ok(pre).wrap_with_cost(cost)
}

/// Read one primary entry's current `(count, sum)` pair, `None` if the key
/// does not exist. `phase` labels error messages ("pre" for the capture
/// before the primary apply, "post" for the delta read after it).
fn read_entry_aggregates<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    key: &[u8],
    phase: &str,
    grove_version: &GroveVersion,
) -> CostResult<AggregatePair, Error> {
    let mut cost = OperationCost::default();
    let maybe_bytes = cost_return_on_error!(
        &mut cost,
        primary_merk
            .get(
                key,
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(format!(
                "indexed {phase}-state read for key {}: {e}",
                hex::encode(key)
            )))
    );
    let aggregates = if let Some(bytes) = maybe_bytes {
        let elem = cost_return_on_error_no_add!(
            cost,
            Element::deserialize(bytes.as_slice(), grove_version).map_err(|e| {
                Error::CorruptedData(format!("indexed {phase}-state deserialize: {e}"))
            })
        );
        Some(elem.count_sum_value_or_default())
    } else {
        None
    };
    Ok(aggregates).wrap_with_cost(cost)
}

/// The per-axis payload stored alongside the sort key. The key encodes the
/// ordering value; the payload carries what the secondary's own aggregate
/// must sum to, which is why the sum and avg axes cannot store a bare item.
fn axis_row_payload(axis: IndexAxis, sum: i64) -> Element {
    match axis {
        IndexAxis::Count => Element::new_item(Vec::new()),
        IndexAxis::Sum => Element::new_sum_item(sum),
        IndexAxis::Avg => Element::new_item_with_sum_item(Vec::new(), sum),
    }
}

/// Enforce the precise per-axis item-key bound: avg prepends a 16-byte sort
/// key, count and sum 8, so the same item key can be legal on one axis and
/// not another. Runs before any secondary write; erroring here aborts the
/// whole batch with its storage batch discarded, so nothing is committed.
fn enforce_axis_item_key_bound<'a>(
    keys: impl Iterator<Item = &'a Vec<u8>>,
    axis: IndexAxis,
) -> Result<(), Error> {
    let max_item_key_len = crate::operations::indexed_tree::max_item_key_len_for_axis(axis);
    for key in keys {
        if key.len() > max_item_key_len {
            return Err(Error::InvalidInput(
                "item key for an indexed-tree primary is too long for a configured axis's \
                 sort key (count/sum allow 247 bytes, avg 239); the secondary key is \
                 sort_key ‖ item_key and Merk requires keys < 256 bytes",
            ));
        }
    }
    Ok(())
}

/// Re-read each captured key's aggregates after the primary apply, pairing
/// them with the captured pre-state into one transition per key.
fn read_post_apply_transitions<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    pre: &BTreeMap<Vec<u8>, AggregatePair>,
    grove_version: &GroveVersion,
) -> CostResult<Vec<AggregateTransition>, Error> {
    let mut cost = OperationCost::default();
    let mut transitions: Vec<AggregateTransition> = Vec::with_capacity(pre.len());
    for (key, old_aggregates) in pre {
        let new_aggregates = cost_return_on_error!(
            &mut cost,
            read_entry_aggregates(primary_merk, key, "post", grove_version)
        );
        transitions.push((key.clone(), *old_aggregates, new_aggregates));
    }
    Ok(transitions).wrap_with_cost(cost)
}

/// Assemble one axis's secondary row moves into a sorted Merk batch.
///
/// Each transition is compared as this axis's `(key, payload)` rather than
/// the raw aggregates: on the avg axis two different `(count, sum)` pairs
/// can share a sort key while carrying different payloads, and on the count
/// axis a sum change moves nothing at all.
fn build_axis_mirror_batch(
    transitions: &[AggregateTransition],
    axis: IndexAxis,
    grove_version: &GroveVersion,
) -> CostResult<Vec<BatchEntry<Vec<u8>>>, Error> {
    let mut cost = OperationCost::default();
    let secondary_tree_type = crate::operations::indexed_tree::axis_secondary_tree_type(axis);
    let mut secondary_batch: Vec<BatchEntry<Vec<u8>>> = Vec::with_capacity(transitions.len() * 2);
    for (key, old_aggregates, new_aggregates) in transitions {
        let old_entry = old_aggregates.map(|(c, s)| {
            (
                make_axis_secondary_key(axis, c, s, key),
                axis_row_payload(axis, s),
            )
        });
        let new_entry = new_aggregates.map(|(c, s)| {
            (
                make_axis_secondary_key(axis, c, s, key),
                axis_row_payload(axis, s),
            )
        });
        if old_entry == new_entry {
            continue;
        }
        // Delete the old row ONLY if the new one lands on a different key.
        // On the avg axis a change can alter the payload while leaving the
        // sort key fixed — (count, sum) going (1, 5) -> (2, 10) keeps
        // avg = 5 — and emitting a delete plus a put for one key in a single
        // Merk batch is rejected outright ("Keys in batch must be unique"),
        // failing the whole GroveDB batch. Where the key is unchanged the put
        // alone overwrites the payload, which is what the row needs.
        let new_secondary_key_ref = new_entry.as_ref().map(|(key, _)| key);
        if let Some((old_secondary_key, _)) = &old_entry
            && Some(old_secondary_key) != new_secondary_key_ref
        {
            cost_return_on_error!(
                &mut cost,
                Element::delete_into_batch_operations(
                    old_secondary_key.clone(),
                    false,
                    secondary_tree_type,
                    &mut secondary_batch,
                    grove_version,
                )
                .map_err(Error::MerkError)
            );
        }
        if let Some((new_secondary_key, entry)) = new_entry {
            let feature_type = cost_return_on_error_no_add!(
                cost,
                entry
                    .get_feature_type(secondary_tree_type)
                    .map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                entry
                    .insert_into_batch_operations(
                        new_secondary_key,
                        &mut secondary_batch,
                        feature_type,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
            );
        }
    }
    secondary_batch.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(secondary_batch).wrap_with_cost(cost)
}

/// Apply one axis's secondary-mirror update for every key captured by
/// [`capture_indexed_pre_state`], after the primary merk's batch ops have been
/// applied. Returns that axis secondary's post-mirror `(root_hash, root_key)`
/// so the caller can fold it into the parent's H1-A composition — directly for
/// the single-axis variants, or through `axes_digest` for PCPSIT.
///
/// Call once per configured axis. Each axis derives its own sort key from the
/// same `(count, sum)` pair, so an entry can move in the count index while
/// staying put in the sum index, and the avg index can move when neither of
/// the other two does.
///
/// **Determinism and atomic-transition note:** the input `pre` is a
/// `BTreeMap`, so iteration is key-sorted. All deletes and inserts for this
/// axis are assembled into one sorted Merk batch. Applying them as separate
/// Merk mutations can retain a stale row after multiple same-level changes;
/// one atomic batch gives the secondary a single pre/post transition.
pub(crate) fn apply_indexed_secondary_mirror_post_apply<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    pre: &BTreeMap<Vec<u8>, AggregatePair>,
    axis: IndexAxis,
    secondary_merk: &mut Merk<S>,
    grove_version: &GroveVersion,
) -> CostResult<(CryptoHash, Option<Vec<u8>>), Error> {
    let mut cost = OperationCost::default();

    let transitions = cost_return_on_error!(
        &mut cost,
        read_post_apply_transitions(primary_merk, pre, grove_version)
    );
    cost_return_on_error_no_add!(cost, enforce_axis_item_key_bound(pre.keys(), axis));
    let secondary_batch = cost_return_on_error!(
        &mut cost,
        build_axis_mirror_batch(&transitions, axis, grove_version)
    );

    if !secondary_batch.is_empty() {
        let secondary_tree_type = crate::operations::indexed_tree::axis_secondary_tree_type(axis);
        cost_return_on_error!(
            &mut cost,
            secondary_merk
                .apply_with_specialized_costs::<_, Vec<u8>>(
                    &secondary_batch,
                    &[],
                    None,
                    &|key, value| {
                        Element::specialized_costs_for_key_value(
                            key,
                            value,
                            secondary_tree_type.inner_node_type(),
                            grove_version,
                        )
                        .map_err(|e| grovedb_merk::Error::ClientCorruptionError(e.to_string()))
                    },
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(Error::MerkError)
        );
    }
    let (sec_hash, sec_root_key, _) = cost_return_on_error!(
        &mut cost,
        secondary_merk
            .root_hash_key_and_aggregate_data()
            .map_err(|e| Error::CorruptedData(format!(
                "indexed secondary root hash capture after mirror: {e}"
            )))
    );
    Ok((sec_hash, sec_root_key)).wrap_with_cost(cost)
}

/// The qualified path (`op.path + op.key`) of every indexed-tree primary
/// this batch CREATES, via any insert-style op carrying an indexed element.
///
/// All three indexed variants, not just PCIT: the limitation is the same for
/// each — the bubble-up has to read the indexed element from the parent merk
/// to learn its secondary root keys (and, for PCPSIT, its axes), and a
/// freshly-inserted element has not been flushed there yet. Before this
/// covered PSIT/PCPSIT the batch failed later and less clearly, with a
/// PathKeyNotFound from the secondary opener.
fn freshly_created_indexed_paths(ops: &[QualifiedGroveDbOp]) -> Vec<Vec<Vec<u8>>> {
    let mut fresh_cidx_paths: Vec<Vec<Vec<u8>>> = Vec::new();
    for op in ops {
        let elem = match &op.op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::Replace { element }
            | GroveOp::Patch { element, .. } => element,
            _ => continue,
        };
        if matches!(
            elem.underlying(),
            Element::ProvableCountIndexedTree(..)
                | Element::ProvableSumIndexedTree(..)
                | Element::ProvableCountProvableSumIndexedTree(..)
        ) && let Some(key) = &op.key
        {
            let mut cidx_path = op.path.to_path();
            cidx_path.push(key.get_key_clone());
            fresh_cidx_paths.push(cidx_path);
        }
    }
    fresh_cidx_paths
}

/// Preflight check: reject any batch that both **creates** a
/// `CountIndexedTree` / `ProvableCountIndexedTree` element AND
/// contains other ops targeting paths inside the freshly-created
/// cidx in the same batch.
///
/// Why: cidx propagation needs both primary and secondary root state
/// to bubble up via the H1-A `combine_hash_three` composition. There
/// is no `InsertAggregateIndexedTreeWithRootKeys` counterpart to
/// `ReplaceAggregateIndexedTreeRootKeys`, and the secondary merk
/// cannot be opened during propagation because the parent's cidx
/// element bytes aren't on disk yet. Without this preflight, callers
/// hit a confusing `MerkError(PathKeyNotFound)` mid-batch as the
/// secondary-merk closure tries to read the cidx element from a
/// parent merk that doesn't yet contain it.
///
/// Workaround: split into two batches. First batch creates the
/// empty cidx; second batch populates it (or call
/// `db.insert_into_count_indexed_tree` directly for individual
/// items).
pub(crate) fn reject_freshly_inserted_cidx_with_descendants(
    ops: &[QualifiedGroveDbOp],
) -> Result<(), Error> {
    let fresh_cidx_paths = freshly_created_indexed_paths(ops);
    if fresh_cidx_paths.is_empty() {
        return Ok(());
    }
    // Reject any op whose effective target path is strictly under one
    // of the fresh cidx paths. The effective path is
    // `op.path + op.key` (keyless ops use just `op.path`). The
    // cidx-creation op itself doesn't trigger (its target equals the
    // cidx path exactly).
    for op in ops {
        let mut op_target = op.path.to_path();
        if let Some(key) = &op.key {
            op_target.push(key.get_key_clone());
        }
        for cidx_path in &fresh_cidx_paths {
            if op_target.len() > cidx_path.len() && op_target[..cidx_path.len()] == cidx_path[..] {
                return Err(Error::NotSupported(
                    "populating a freshly-inserted indexed tree (ProvableCountIndexedTree \
                     / ProvableSumIndexedTree / ProvableCountProvableSumIndexedTree) in the \
                     same batch as its creation is not \
                     supported (no Insert variant for aggregate-indexed two-Merk \
                     propagation exists, and the secondary merk cannot be opened from \
                     stale parent state during bubble-up). Split into two batches: \
                     insert the empty cidx first, then populate it via \
                     `db.insert_into_count_indexed_tree` or a follow-up batch."
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// What "empty" means for each indexed variant offered as a replacement:
/// `Some(true)` for an empty indexed element, `Some(false)` for a non-empty
/// one, `None` when the element is not indexed at all.
///
/// Emptiness cannot be read off any single field. The primary root key, the
/// aggregate(s) AND every secondary root key must all be unset — and for
/// PCPSIT the axes TLV itself stays canonically non-empty even for an empty
/// tree, so only its per-axis root keys count, not its length.
fn replacement_indexed_emptiness(new_element: &Element) -> Option<bool> {
    match new_element.underlying() {
        Element::ProvableSumIndexedTree(p, s, sum, _) => {
            Some(p.is_none() && s.is_none() && *sum == 0)
        }
        Element::ProvableCountIndexedTree(p, s, c, _) => {
            Some(p.is_none() && s.is_none() && *c == 0)
        }
        Element::ProvableCountProvableSumIndexedTree(p, c, sum, axes, _) => Some(
            p.is_none()
                && *c == 0
                && *sum == 0
                && axes.iter().all(|(_, root_key)| root_key.is_none()),
        ),
        _ => None,
    }
}

/// Classify an `op_could_overwrite` insert at `path / key_info` against
/// the existing primary-merk entry, when tree-override protection is
/// **OFF** for this batch. Allows indexed-tree safe-subset overwrites and
/// rejects the ambiguous ones:
///
/// |  existing              |  new                       |  outcome                      |
/// |------------------------|----------------------------|-------------------------------|
/// |  none                  |  *                         |  `Ok(None)`                   |
/// |  non-indexed           |  *                         |  `Ok(None)`                   |
/// |  indexed              |  non-indexed               |  `Ok(Some(indexed_path))`     |
/// |  indexed              |  empty indexed             |  `Ok(Some(indexed_path))`     |
/// |  indexed              |  non-empty indexed         |  `Err(NotSupported)`          |
///
/// When `Ok(Some(cidx_path))` is returned, the caller should push
/// `cidx_path` onto its `cidx_overwrite_cleanup_paths` list so the
/// post-apply pass can clear the old cidx's storage namespaces
/// (subtree prefixes + secondary namespace at
/// `Blake3(primary_prefix ‖ 0x01)`). Non-empty cidx replacement stays
/// rejected because the new element's primary_root_key /
/// secondary_root_key would point at on-disk data while our post-apply
/// cleanup of the OLD cidx's prefixes also clears that data — the
/// storage-pointer semantics are ambiguous (reuse old? fresh?) and
/// the safe answer is to force the caller through delete-then-recreate.
///
/// Additionally, if a safe-subset overwrite is detected but the batch
/// contains *any* write whose qualified path lies strictly under the
/// cidx primary's path, the function returns
/// `Err(InvalidBatchOperation)` — the post-apply cleanup would
/// silently lose those writes. The generic consistency check
/// (`verify_consistency_of_operations`) only blocks writes under
/// `Delete` / `DeleteTree` paths; it does not know about safe-subset
/// cidx-overwrite cleanup, so the descendant-check lives here.
pub(crate) fn inspect_cidx_overwrite<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    path: &[Vec<u8>],
    key_info: &KeyInfo,
    new_element: &Element,
    ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, GroveOp>,
    grove_version: &GroveVersion,
) -> CostResult<Option<Vec<Vec<u8>>>, Error> {
    let mut cost = OperationCost::default();

    let maybe_existing = cost_return_on_error!(
        &mut cost,
        primary_merk
            .get(
                key_info.get_key_clone().as_slice(),
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(format!(
                "unable to check for existing element: {e}"
            )))
    );

    let Some(existing_bytes) = maybe_existing else {
        return Ok(None).wrap_with_cost(cost);
    };

    let existing_element = cost_return_on_error_no_add!(
        cost,
        Element::deserialize(existing_bytes.as_slice(), grove_version).map_err(|_| {
            Error::CorruptedData("unable to deserialize existing element".to_string())
        })
    );

    if !existing_element.is_indexed_tree() {
        return Ok(None).wrap_with_cost(cost);
    }

    if matches!(replacement_indexed_emptiness(new_element), Some(false)) {
        return Err(Error::NotSupported(
            "overwriting an existing indexed tree with a NON-EMPTY indexed tree via the \
             batch path is not supported (storage-pointer semantics \
             are ambiguous: the new element's root_keys would refer to data while the \
             post-apply cleanup also clears it). DeleteTree the old indexed tree and re-create \
             the new state in a follow-up batch"
                .to_string(),
        ))
        .wrap_with_cost(cost);
    }

    // Safe subset: indexed → non-indexed OR indexed → empty indexed.
    // Schedule the OLD indexed tree's storage namespaces for cleanup. Its path is
    // `path + key_info`.
    let mut cidx_path = path.to_vec();
    cidx_path.push(key_info.get_key_clone());

    // CONSISTENCY CHECK: writes UNDER the cidx's path in the same batch
    // would be silently lost when the post-apply cleanup clears the prefix.
    //
    // This loop is currently UNREACHABLE, and deliberately kept anyway.
    // The reason it cannot fire is self-cancelling: a descendant write makes
    // the deeper level bubble a `ReplaceTreeRootKey` into this key's slot
    // before the shallower level runs, so by the time this function reads the
    // existing element it no longer sees an indexed tree and returns above,
    // never reaching here. In other words the very condition being checked
    // for is what prevents the check from running. Verified by instrumenting
    // both this loop and the function entry: for a PCIT overwritten with a
    // plain tree alongside a write underneath it, the function is entered and
    // returns early, and the loop is never reached.
    //
    // It stays because the unreachability is a property of the ORDER the
    // levels are processed in, not of this function — reorder the bubble-up,
    // or call the classifier from anywhere that reads the pre-batch element,
    // and the hazard is live again. Do not treat it as protection that exists
    // today.
    let cidx_path_len = cidx_path.len();
    for q_path in ops_by_qualified_paths.keys() {
        if q_path.len() > cidx_path_len && q_path[..cidx_path_len] == cidx_path[..] {
            return Err(Error::InvalidBatchOperation(
                "batch contains a write under a cidx primary path that is being \
                 safe-subset-overwritten in the same batch; the post-apply cleanup \
                 would silently clear the descendant write. Split into two batches: \
                 delete + recreate first, then populate.",
            ))
            .wrap_with_cost(cost);
        }
    }

    Ok(Some(cidx_path)).wrap_with_cost(cost)
}
