//! Validation and pre-apply capture for an indexed primary's batch level.
//!
//! Runs just before the primary merk's batch apply. Validation shuts the
//! batch door with the same rules the dedicated insert paths enforce; the
//! capture records each mutated key's old `(count, sum)` pair so the
//! post-apply [`mirror`](super::mirror) can compute old → new transitions.

use std::collections::BTreeMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{element::insert::ElementInsertToStorageExtensions, Merk};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use super::{read_entry_aggregates, AggregatePair};
use crate::{
    batch::{GroveOp, KeyInfo},
    operations::indexed_tree::MAX_CIDX_ITEM_KEY_LEN,
    Element, Error,
};

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
            | GroveOp::InsertAggregateIndexedTreeRootKeys { .. }
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
