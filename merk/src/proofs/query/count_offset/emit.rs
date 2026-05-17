//! Recursive proof-emission engine for offset-paginated count-tree
//! range queries.
//!
//! For each subtree we visit, the bound classification (Disjoint /
//! Contained / Boundary) plus the prover's current offset/limit
//! position determines what op to push and whether to descend:
//!
//! - **Disjoint** → emit a single `HashWithCount` for the collapsed
//!   subtree root. The subtree has no in-range keys so neither offset
//!   nor limit is touched, but the structural count still has to be
//!   hash-bound for the parent's `own_count` derivation.
//! - **Contained** with `subtree_count ≤ offset_remaining` → emit a
//!   single `HashWithCount` and subtract the subtree's count from
//!   offset_remaining. Whole-subtree skip pays O(log n) proof size for
//!   O(subtree_count) skipped items — the central optimization this
//!   module exists for.
//! - **Contained** with `offset_remaining == 0 && limit_remaining ==
//!   Some(0)` → past limit. Emit a single `HashWithCount` to bind the
//!   structural count without emitting any items.
//! - **Contained** otherwise / **Boundary** → descend per-element.
//!   Each node is then classified individually as path / skipped /
//!   returned / past-limit and emitted as `KVHashCount`,
//!   `KVDigestCount`, a value-bearing node, or `KVDigestCount`
//!   respectively.
//!
//! For the per-node emission step inside a descent, the prover does
//! **not** read the value bytes unless it is actually going to return
//! the item — every offset-skipped or limit-truncated entry emits as
//! `KVDigestCount(key, value_hash, count)`, which is the same shape used
//! for boundary-absence nodes in regular count-tree proofs. Returned
//! items emit one of `KVCount` / `KVValueHashFeatureType` /
//! `KVValueHash` depending on the underlying element type (mirroring
//! `create_proof_internal`).
//!
//! Direction handling: when `left_to_right = false` we walk the right
//! child first, then the current node, then the left child, and the
//! emitted ops use the inverted family (`PushInverted` / `ParentInverted`
//! / `ChildInverted`). The bound classification is direction-independent
//! (it depends only on set membership), but the offset/limit accounting
//! is positional, so direction has to drive which child the walker
//! visits first.

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_element::{ElementType, ProofNodeType};
use grovedb_version::version::GroveVersion;

use super::provable_count_from_aggregate;
use crate::{
    proofs::{
        query::{
            aggregate_common::{classify_subtree, SubtreeClassification, NULL_HASH},
            QueryItem,
        },
        Node, Op,
    },
    tree::{kv::ValueDefinedCostType, Fetch, RefWalker},
    CryptoHash, Error,
};

/// Mutable state threaded through the recursion. Wrapped in a struct so
/// the recursive signature stays readable.
pub(super) struct EmitState {
    /// Remaining offset to "burn". Counts in-range items the prover
    /// still needs to skip before it starts returning data.
    pub(super) offset_remaining: u64,
    /// Remaining limit. `None` means unlimited; the prover always emits
    /// every in-range item past offset.
    pub(super) limit_remaining: Option<u64>,
    /// Number of in-range items the prover has returned so far. Bumped
    /// each time we emit a value-bearing node; exposed back to the
    /// caller as a convenience (the verifier independently computes it
    /// from the reconstructed proof, so this is not a trust input).
    pub(super) returned: u64,
    /// Walk direction. `true` = ascending (left-to-right), `false` =
    /// descending (right-to-left).
    pub(super) left_to_right: bool,
}

/// Recursive proof emitter. Always called on a non-empty subtree.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both
/// `None` at the root call). The bounds get tightened on each
/// descent: walking left yields `(lo, Some(node_key))`, walking right
/// yields `(Some(node_key), hi)`. Direction-independent — these are
/// tree-structural bounds, not iteration bounds.
///
/// Returns the **structural** count of this subtree (i.e. its
/// aggregate count, which is what the parent's verifier needs to
/// derive `own_count = aggregate − left_struct − right_struct`).
pub(super) fn emit_count_offset_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    state: &mut EmitState,
    ops: &mut LinkedList<Op>,
    grove_version: &GroveVersion,
) -> CostResult<u64, Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();

    // Step 1: classify this subtree against the inner range.
    let class = classify_subtree(subtree_lo_excl, subtree_hi_excl, range);

    // Pull the structural count (and gate the tree's aggregate-data
    // type) up front — we use it both for the Disjoint/Contained
    // collapse paths and for own_count derivation later if we descend.
    let aggregate = match walker.tree().aggregate_data() {
        Ok(a) => a,
        Err(e) => {
            return Err(Error::InvalidProofError(format!("aggregate_data: {}", e)))
                .wrap_with_cost(cost);
        }
    };
    let subtree_count = match provable_count_from_aggregate(aggregate) {
        Ok(c) => c,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    // Step 2: see if the whole subtree can be collapsed into a single
    // self-verifying `HashWithCount` op.
    //
    //   Disjoint                                       → always collapse
    //   Contained + sub ≤ offset_remaining             → collapse, offset −= sub
    //   Contained + offset == 0 && limit_remaining == 0 → collapse
    //
    // Anything else falls through to per-element descent below.
    let collapse_action = match class {
        SubtreeClassification::Disjoint => Some(CollapseAction::Disjoint),
        SubtreeClassification::Contained => {
            if subtree_count <= state.offset_remaining {
                Some(CollapseAction::SkippedByOffset)
            } else if state.offset_remaining == 0 && state.limit_remaining == Some(0) {
                Some(CollapseAction::PastLimit)
            } else {
                None
            }
        }
        SubtreeClassification::Boundary => None,
    };

    if let Some(action) = collapse_action {
        // Emit one HashWithCount for the entire subtree. The four
        // committed fields recompute `node_hash_with_count`; tampering
        // with the count fails the parent's hash check.
        let kv_hash = *walker.tree().kv_hash();
        let left_child_hash = walker
            .tree()
            .link(true)
            .map(|l| *l.hash())
            .unwrap_or(NULL_HASH);
        let right_child_hash = walker
            .tree()
            .link(false)
            .map(|l| *l.hash())
            .unwrap_or(NULL_HASH);
        let node = Node::HashWithCount(kv_hash, left_child_hash, right_child_hash, subtree_count);
        ops.push_back(if state.left_to_right {
            Op::Push(node)
        } else {
            Op::PushInverted(node)
        });
        if matches!(action, CollapseAction::SkippedByOffset) {
            // saturating_sub is safe: the branch condition above ensures
            // subtree_count ≤ offset_remaining, so this is exact.
            state.offset_remaining = state.offset_remaining.saturating_sub(subtree_count);
        }
        return Ok(subtree_count).wrap_with_cost(cost);
    }
    // class == Boundary OR Contained-but-must-descend.

    // Step 3: snapshot what we need from the current node before
    // walking into children (walk(left/right) takes &mut self.tree).
    let node_key: Vec<u8> = walker.tree().key().to_vec();
    let node_value_hash: CryptoHash = *walker.tree().value_hash();
    let node_count: u64 = subtree_count;

    let left_link_count: u64 = walker
        .tree()
        .link(true)
        .map(|l| l.aggregate_data().as_count_u64())
        .unwrap_or(0);
    let right_link_count: u64 = walker
        .tree()
        .link(false)
        .map(|l| l.aggregate_data().as_count_u64())
        .unwrap_or(0);
    // left_link_present / right_link_present are read indirectly via
    // walker.tree().link(dir).is_some() below where they're needed.

    // own_struct is what *this* node contributes structurally — 0 for
    // a `NonCounted`-wrapped entry, 1 for a normal entry. checked_sub
    // would be more conservative, but saturating_sub mirrors what
    // `emit_count_proof` does and keeps the prover lenient: if the
    // in-memory tree ever returns inconsistent aggregates the verifier
    // will catch it via the hash chain.
    let own_struct: u64 = node_count
        .saturating_sub(left_link_count)
        .saturating_sub(right_link_count);

    let is_in_range = range.contains(&node_key);

    // The two children get traversed in direction order. For ascending
    // (left_to_right = true), first = left, second = right. For
    // descending, first = right, second = left.
    let (first_dir, second_dir) = if state.left_to_right {
        (true, false)
    } else {
        (false, true)
    };

    // Step 4: walk the FIRST child. Its bounds are the inherited
    // half-space on its side of the current key.
    let first_emitted = if walker.tree().link(first_dir).is_some() {
        let (child_lo, child_hi) = if first_dir {
            (subtree_lo_excl, Some(node_key.as_slice()))
        } else {
            (Some(node_key.as_slice()), subtree_hi_excl)
        };
        let walked = cost_return_on_error!(
            &mut cost,
            walker.walk(
                first_dir,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
        );
        // `walker.walk(dir)` returns `None` only when the link was
        // missing — but we just checked `link(first_dir).is_some()`
        // immediately above (and `walker.tree()` is not aliased
        // between the check and the call), so this branch is
        // structurally unreachable. Keeping it as `unreachable!()`
        // turns a silent corruption into a fail-loud panic if the
        // invariant is ever broken by a refactor.
        let mut child_walker =
            walked.unwrap_or_else(|| unreachable!("walk(first_dir) None despite link.is_some()"));
        cost_return_on_error!(
            &mut cost,
            emit_count_offset_proof(
                &mut child_walker,
                range,
                child_lo,
                child_hi,
                state,
                ops,
                grove_version,
            )
        );
        // We don't use the child's structural count at this level —
        // the verifier re-derives `own_count` from the proof tree. We
        // only need the return value to satisfy the "always returns
        // structural count" contract for callers using the top-level
        // recursion.
        true
    } else {
        false
    };

    // Step 5: emit this node.
    //
    // Per-node disposition (with own_struct ∈ {0, 1}):
    //   - Out-of-range key OR in-range `NonCounted` entry (own_struct
    //     = 0) OR in-range counted entry in offset window OR in-range
    //     counted entry past limit:
    //       emit `KVDigestCount(key, value_hash, node_count)`.
    //       Offset consumption applies only to the third case
    //       (in-range counted in offset window).
    //   - In-range, counted, offset_remaining == 0, limit_remaining > 0:
    //       emit the appropriate value-bearing node (KVCount /
    //       KVValueHashFeatureType / KVValueHash), decrement limit,
    //       increment returned.
    //
    // Why `KVDigestCount` (key-bearing) instead of `KVHashCount`
    // (hash-only) for path positions: the verifier needs the node's
    // key to tighten subtree bounds for its child recursions. The
    // structural-count check + `node_hash_with_count` recomputation
    // already cover hash-binding regardless of whether the key is
    // exposed, so emitting the key costs only proof size — not
    // soundness — and is what `AggregateCountOnRange` does for the
    // same reason.
    let self_node = if !is_in_range || own_struct == 0 {
        // Path node or NonCounted in-range. No state mutation; the
        // structural-count check handles own=0 enforcement.
        Node::KVDigestCount(node_key.clone(), node_value_hash, node_count)
    } else if state.offset_remaining > 0 {
        state.offset_remaining -= 1;
        Node::KVDigestCount(node_key.clone(), node_value_hash, node_count)
    } else if state.limit_remaining == Some(0) {
        Node::KVDigestCount(node_key.clone(), node_value_hash, node_count)
    } else {
        // Returned item. Pick the value-node flavor based on element
        // type so the proof shape matches what the regular count-tree
        // proof flow emits (this is what the GroveDB layer expects).
        if let Some(ref mut l) = state.limit_remaining {
            *l -= 1;
        }
        state.returned = state.returned.saturating_add(1);
        emit_returned_node(walker, node_count)
    };

    ops.push_back(if state.left_to_right {
        Op::Push(self_node)
    } else {
        Op::PushInverted(self_node)
    });
    if first_emitted {
        ops.push_back(if state.left_to_right {
            Op::Parent
        } else {
            Op::ParentInverted
        });
    }

    // Step 6: walk the SECOND child. Same bound-derivation pattern.
    let second_emitted = if walker.tree().link(second_dir).is_some() {
        let (child_lo, child_hi) = if second_dir {
            (subtree_lo_excl, Some(node_key.as_slice()))
        } else {
            (Some(node_key.as_slice()), subtree_hi_excl)
        };
        let walked = cost_return_on_error!(
            &mut cost,
            walker.walk(
                second_dir,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
        );
        let mut child_walker = match walked {
            Some(w) => w,
            None => {
                return Err(Error::CorruptedState(
                    "tree.link(second_dir) was Some but walk returned None",
                ))
                .wrap_with_cost(cost)
            }
        };
        cost_return_on_error!(
            &mut cost,
            emit_count_offset_proof(
                &mut child_walker,
                range,
                child_lo,
                child_hi,
                state,
                ops,
                grove_version,
            )
        );
        true
    } else {
        false
    };

    if second_emitted {
        ops.push_back(if state.left_to_right {
            Op::Child
        } else {
            Op::ChildInverted
        });
    }

    // Tactical note: silence unused-variable warnings on
    // left_link_count / right_link_count. The verifier re-derives
    // `own_count` from the reconstructed children's structural counts,
    // so the prover doesn't actually need these locally past the
    // own_struct computation. Keep them named for readability.
    let _ = (left_link_count, right_link_count);

    Ok(node_count).wrap_with_cost(cost)
}

/// Classify why we're collapsing a subtree into a single
/// `HashWithCount`. The only one that mutates state is
/// `SkippedByOffset` (which decrements `offset_remaining`); the other
/// two emit the op for the parent's hash-binding but otherwise leave
/// state alone.
#[derive(Clone, Copy)]
enum CollapseAction {
    /// Subtree's keys are entirely outside the inner range — no
    /// in-range items, but the structural count still has to be
    /// committed for the parent's `own_count` derivation.
    Disjoint,
    /// Subtree is entirely inside the inner range and fits within
    /// `offset_remaining`. We subtract its count from
    /// `offset_remaining` and emit one HashWithCount.
    SkippedByOffset,
    /// Subtree is entirely inside the inner range but the prover has
    /// already exhausted `limit_remaining`. We emit one HashWithCount
    /// and don't touch state.
    PastLimit,
}

/// Pick the value-bearing Node variant for a returned item. Mirrors
/// the `create_proof_internal` dispatch: the element type stored in the
/// value's first byte tells us whether to use the count-bearing flavor
/// (`KVCount` for Items, `KVValueHashFeatureType` for trees/references)
/// or the plain flavor. Falling back to `KVCount` for raw / unknown
/// types matches the "tamper-resistant by default" choice the regular
/// proof flow makes for count-tree subtrees.
///
/// The feature-type-carrying variants (`KVValueHashFeatureType` for
/// trees/references) delegate to the same `to_kv_value_hash_feature_type_node`
/// helper the regular proof flow uses, which rewrites the feature_type
/// to carry the *aggregate* count (not the on-disk own count). Skipping
/// that rewrite would produce a feature_type whose count is the own
/// count, which `aggregate_data().into()` then decodes as a wrong
/// AggregateData at verify time — the verifier's `own_count = aggregate
/// − left_struct − right_struct` derivation would underflow at every
/// internal node and the proof would reject.
fn emit_returned_node<S>(walker: &RefWalker<'_, S>, count: u64) -> Node
where
    S: Fetch + Sized + Clone,
{
    let value_bytes = walker.tree().value_as_slice();
    let key = walker.tree().key().to_vec();

    // For ProvableCountTree / ProvableCountSumTree we want the
    // count-bearing variant so the verifier's hash recomputation
    // includes the count. The element type tells us whether the value
    // is hashed directly (Item-flavored → `KVCount`) or via the
    // combined value+inner_root hash (Tree/Reference → carry the
    // feature_type so the verifier can route the right hash function).
    let parent_tree_type = Some(ElementType::ProvableCountTree);
    let kind = ElementType::from_serialized_value(value_bytes)
        .map(|et| et.proof_node_type(parent_tree_type))
        .unwrap_or(ProofNodeType::KvCount);

    match kind {
        ProofNodeType::Kv => walker.to_kv_node(),
        ProofNodeType::KvCount => Node::KVCount(key, value_bytes.to_vec(), count),
        ProofNodeType::KvSum => {
            // Reaching this branch would mean a SumItem (not a
            // CountAndSumItem) is sitting under a count tree, which the
            // batch layer should never produce. Fall back to KVCount so
            // the proof shape stays count-bound.
            Node::KVCount(key, value_bytes.to_vec(), count)
        }
        ProofNodeType::KvValueHash => walker.to_kv_value_hash_node(),
        // For tree/reference children of a count tree, delegate to the
        // regular flow's helper so the feature_type carries the
        // aggregate count (not the on-disk own count). The same helper
        // is what `create_proof_internal` uses, so the resulting node
        // is byte-identical to what a regular count-tree proof emits
        // for the same entry.
        ProofNodeType::KvValueHashFeatureType
        | ProofNodeType::KvRefValueHash
        | ProofNodeType::KvRefValueHashCount
        | ProofNodeType::KvRefValueHashSum => walker.to_kv_value_hash_feature_type_node(),
    }
}
