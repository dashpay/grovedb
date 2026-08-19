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
use grovedb_element::{Element, ElementType, ProofNodeType};
use grovedb_version::version::GroveVersion;

use super::{
    binds_sum_into_hash, provable_count_from_aggregate, provable_sum_from_dual_axis_aggregate,
};
use crate::{
    proofs::{
        query::{
            aggregate_common::{classify_subtree, SubtreeClassification, NULL_HASH},
            QueryItem,
        },
        Node, Op,
    },
    tree::{kv::ValueDefinedCostType, AggregateData, Fetch, RefWalker},
    CryptoHash, Error, TreeType,
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
    tree_type: TreeType,
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
        // Emit one collapsed-subtree op. The four (or five for dual-axis)
        // committed fields recompute the parent's hashing function;
        // tampering with the count (or sum) fails the parent's hash check.
        //
        // For PCPS hosts: emit `HashWithCountAndSum(kv, l, r, count, sum)`
        // so the verifier can reconstruct `node_hash_with_count_and_sum`.
        // For single-axis hosts (`ProvableCountTree` /
        // `ProvableCountSumTree`): emit the count-only `HashWithCount`.
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
        let node = if binds_sum_into_hash(tree_type) {
            let subtree_sum = match provable_sum_from_dual_axis_aggregate(aggregate) {
                Ok(s) => s,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };
            Node::HashWithCountAndSum(
                kv_hash,
                left_child_hash,
                right_child_hash,
                subtree_count,
                subtree_sum,
            )
        } else {
            Node::HashWithCount(kv_hash, left_child_hash, right_child_hash, subtree_count)
        };
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
    // For PCPS hosts we also need the per-node sum so we can emit
    // `KVDigestCountSum` (carrying both axes) at boundary positions.
    // Single-axis hosts ignore this.
    let node_sum: i64 = if binds_sum_into_hash(tree_type) {
        match aggregate {
            AggregateData::ProvableCountAndProvableSum(_, s) => s,
            other => {
                return Err(Error::InvalidProofError(format!(
                    "expected ProvableCountAndProvableSum aggregate on a \
                     ProvableCountProvableSumTree node, got {:?}",
                    other
                )))
                .wrap_with_cost(cost);
            }
        }
    } else {
        0
    };

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

    // Reject value shapes the count-offset proof flow does not yet
    // support, so the prover surfaces an explicit `NotSupported`
    // instead of producing a proof that silently diverges from regular
    // GroveDB query semantics. Three cases, each pinned to a finding
    // in the PR review:
    //
    //   • **NonCounted-wrapped in-range entry** (`own_struct == 0`):
    //     regular GroveDB returns the NonCounted item's value; the
    //     current count-offset flow has no way to emit it (the proof's
    //     `KVDigestCount` carries only the key/hash, not the value).
    //     Silently dropping it would be a correctness divergence — we
    //     reject upfront instead.
    //
    //   • **Non-empty tree** in-range entry: V1 strict-mode requires a
    //     `KVValueHashFeatureTypeWithChildHash` proof node for these,
    //     which the count-offset prover doesn't emit. The verifier
    //     would reject the resulting proof anyway; rejecting at prove
    //     time saves the work of producing an honest-but-unverifiable
    //     proof.
    //
    // **References are supported.** They emit as
    // `KVValueHashFeatureType` (see `emit_returned_node`), which the
    // callers' reference post-pass rewrites into the
    // `KVRefValueHash{Count,CountSum}` family carrying the resolved
    // target — the same rewrite the regular count-tree flow performs.
    // The verifier accepts and authenticates those variants, so a
    // verified result carries the dereferenced target rather than a raw
    // `Element::Reference`. A caller that does NOT run the post-pass
    // still gets a sound proof; it just surfaces the reference bytes,
    // which is why GroveDB's indexed-axis path applies the post-pass
    // unconditionally.
    //
    // Lifting the remaining rejection is straightforward future work:
    // emit the appropriate node variant and update the verifier
    // symmetrically.
    if is_in_range {
        if own_struct == 0 {
            return Err(Error::InvalidProofError(
                "count-offset paginated proofs do not yet support NonCounted-wrapped \
                 in-range entries (regular GroveDB query semantics return their values, \
                 but this proof flow has no way to emit those without changing the wire \
                 format)"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let value_bytes = walker.tree().value_as_slice();
        match Element::deserialize(value_bytes, grove_version) {
            Ok(elem) => {
                let inner = elem.into_underlying();
                if inner.is_non_empty_tree() {
                    return Err(Error::InvalidProofError(
                        "count-offset paginated proofs do not yet support non-empty tree \
                         return values — the prover doesn't emit \
                         KVValueHashFeatureTypeWithChildHash for these, which V1 \
                         strict-mode requires"
                            .to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
            }
            Err(_) => {
                // Raw / non-Element value bytes — accept (this is the
                // path raw merk users hit; they get tamper-resistant
                // KVCount emission and that's it).
            }
        }
    }

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
                tree_type,
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
    // Helper closure: emit the right boundary-node flavor (key +
    // value_hash + count [+ sum]) for this host. For PCPS we emit
    // `KVDigestCountSum` carrying both axes; for single-axis hosts the
    // count-only `KVDigestCount`.
    let make_boundary_node = || {
        if binds_sum_into_hash(tree_type) {
            Node::KVDigestCountSum(node_key.clone(), node_value_hash, node_count, node_sum)
        } else {
            Node::KVDigestCount(node_key.clone(), node_value_hash, node_count)
        }
    };

    let self_node = if !is_in_range || own_struct == 0 {
        // Path node or NonCounted in-range. No state mutation; the
        // structural-count check handles own=0 enforcement.
        make_boundary_node()
    } else if state.offset_remaining > 0 {
        state.offset_remaining -= 1;
        make_boundary_node()
    } else if state.limit_remaining == Some(0) {
        make_boundary_node()
    } else {
        // Returned item. Pick the value-node flavor based on element
        // type so the proof shape matches what the regular count-tree
        // proof flow emits (this is what the GroveDB layer expects).
        if let Some(ref mut l) = state.limit_remaining {
            *l -= 1;
        }
        state.returned = state.returned.saturating_add(1);
        emit_returned_node(walker, node_count, tree_type)
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
                tree_type,
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
fn emit_returned_node<S>(walker: &RefWalker<'_, S>, count: u64, tree_type: TreeType) -> Node
where
    S: Fetch + Sized + Clone,
{
    let value_bytes = walker.tree().value_as_slice();
    let key = walker.tree().key().to_vec();

    // For each host we want the count-bearing (or count+sum-bearing)
    // variant so the verifier's hash recomputation includes the
    // aggregates. The element type tells us whether the value is
    // hashed directly (Item-flavored → `KVCount` / `KVCountSum`) or
    // via the combined value+inner_root hash (Tree/Reference → carry
    // the feature_type so the verifier can route the right hash
    // function). PCPS hosts use the dual-axis variants so the
    // verifier reconstructs `node_hash_with_count_and_sum`.
    let parent_tree_type = tree_type.to_element_type();
    let kind = ElementType::from_serialized_value(value_bytes)
        .map(|et| et.proof_node_type(parent_tree_type))
        .unwrap_or(if binds_sum_into_hash(tree_type) {
            ProofNodeType::KvCountSum
        } else {
            ProofNodeType::KvCount
        });

    match kind {
        ProofNodeType::Kv => walker.to_kv_node(),
        ProofNodeType::KvCount => Node::KVCount(key, value_bytes.to_vec(), count),
        ProofNodeType::KvCountSum => {
            // PCPS host, Item-flavored entry: emit the dual-axis
            // `KVCountSum` so the verifier can reconstruct
            // `node_hash_with_count_and_sum`. Use the existing
            // `to_kv_count_sum_node()` helper for consistency with the
            // regular count-tree proof flow (it reads the aggregate
            // count + sum out of `aggregate_data()` directly).
            walker.to_kv_count_sum_node()
        }
        ProofNodeType::KvSum => {
            // Reaching this branch would mean a SumItem (not a
            // CountAndSumItem) is sitting under a count tree, which the
            // batch layer should never produce. Fall back to the
            // host-appropriate count-bearing variant so the proof
            // shape stays valid.
            if binds_sum_into_hash(tree_type) {
                walker.to_kv_count_sum_node()
            } else {
                Node::KVCount(key, value_bytes.to_vec(), count)
            }
        }
        ProofNodeType::KvValueHash => walker.to_kv_value_hash_node(),
        // For tree/reference children of a count tree, delegate to the
        // regular flow's helper so the feature_type carries the
        // aggregate count (and, for PCPS, sum). The same helper is
        // what `create_proof_internal` uses, so the resulting node is
        // byte-identical to what a regular count-tree proof emits
        // for the same entry.
        ProofNodeType::KvValueHashFeatureType
        | ProofNodeType::KvRefValueHash
        | ProofNodeType::KvRefValueHashCount
        | ProofNodeType::KvRefValueHashSum
        | ProofNodeType::KvRefValueHashCountSum => walker.to_kv_value_hash_feature_type_node(),
    }
}
