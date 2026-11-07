//! Committed-count-unit skip traversal for branch-local pagination.
//! Collapsed skips consume authenticated count units without scanning their
//! contents. Descended rows retain the ordinary unit-row restrictions.

use std::collections::LinkedList;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::Element;
use grovedb_version::version::GroveVersion;

use super::{
    binds_sum_into_hash,
    emit::{emit_returned_node, non_unit_row_error, CollapseAction, EmitState},
    provable_count_from_aggregate, provable_sum_from_dual_axis_aggregate,
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

/// What a local page's traversal builds. Selection is the same either way, so a
/// trusted read returns the rows a proof of the same state proves, and each
/// consumer pays only for its own copy of them.
pub(super) enum LocalPageOutput<'a> {
    /// The proof ops a prover serves.
    Proof(&'a mut LinkedList<Op>),
    /// The `(key, value)` rows a trusted read returns, in walk order.
    Rows(&'a mut Vec<(Vec<u8>, Vec<u8>)>),
}

pub(super) fn emit_local_count_offset_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    state: &mut EmitState,
    output: &mut LocalPageOutput<'_>,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> CostResult<u64, Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();

    let class = classify_subtree(subtree_lo_excl, subtree_hi_excl, range);

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

    if let Some(action) = collapse_action(class, subtree_count, state) {
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
        if let LocalPageOutput::Proof(ops) = output {
            ops.push_back(if state.left_to_right {
                Op::Push(node)
            } else {
                Op::PushInverted(node)
            });
        }
        settle_collapsed(action, subtree_count, state);
        return Ok(subtree_count).wrap_with_cost(cost);
    }

    let node_key: Vec<u8> = walker.tree().key().to_vec();
    let node_value_hash: CryptoHash = *walker.tree().value_hash();
    let node_count: u64 = subtree_count;
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

    let own_struct: u64 = node_count
        .saturating_sub(left_link_count)
        .saturating_sub(right_link_count);

    let is_in_range = range.contains(&node_key);

    if own_struct > 1 {
        return Err(non_unit_row_error(&node_key, own_struct)).wrap_with_cost(cost);
    }

    if is_in_range {
        if own_struct == 0 {
            return Err(Error::InvalidProofError(format!(
                "count-offset paginated proofs require every in-range row to contribute \
                 exactly one count unit, but the row at key {} contributes zero (a \
                 NonCounted-wrapped entry or an empty count-bearing tree) — regular \
                 GroveDB pagination counts it as one row, and this proof flow has no way \
                 to emit it without changing the wire format",
                hex::encode(&node_key)
            )))
            .wrap_with_cost(cost);
        }
        let value_bytes = walker.tree().value_as_slice();
        if let Ok(elem) = Element::deserialize(value_bytes, grove_version) {
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
    }

    let (first_dir, second_dir) = if state.left_to_right {
        (true, false)
    } else {
        (false, true)
    };
    let child_bounds = |left: bool| {
        if left {
            (subtree_lo_excl, Some(node_key.as_slice()))
        } else {
            (Some(node_key.as_slice()), subtree_hi_excl)
        }
    };

    let (first_lo, first_hi) = child_bounds(first_dir);
    let first_emitted = cost_return_on_error!(
        &mut cost,
        emit_local_child(
            walker,
            first_dir,
            range,
            first_lo,
            first_hi,
            state,
            output,
            tree_type,
            grove_version,
        )
    );

    let selected = if !is_in_range || own_struct == 0 {
        false
    } else if state.offset_remaining > 0 {
        state.offset_remaining -= 1;
        false
    } else {
        state.limit_remaining != Some(0)
    };
    if selected {
        if let Some(ref mut l) = state.limit_remaining {
            *l -= 1;
        }
        state.returned = state.returned.saturating_add(1);
    }

    match output {
        LocalPageOutput::Proof(ops) => {
            let self_node = if selected {
                emit_returned_node(walker, node_count, tree_type)
            } else if binds_sum_into_hash(tree_type) {
                Node::KVDigestCountSum(node_key.clone(), node_value_hash, node_count, node_sum)
            } else {
                Node::KVDigestCount(node_key.clone(), node_value_hash, node_count)
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
        }
        LocalPageOutput::Rows(rows) => {
            if selected {
                rows.push((node_key.clone(), walker.tree().value_as_slice().to_vec()));
            }
        }
    }

    let (second_lo, second_hi) = child_bounds(second_dir);
    let second_emitted = cost_return_on_error!(
        &mut cost,
        emit_local_child(
            walker,
            second_dir,
            range,
            second_lo,
            second_hi,
            state,
            output,
            tree_type,
            grove_version,
        )
    );

    if second_emitted && let LocalPageOutput::Proof(ops) = output {
        ops.push_back(if state.left_to_right {
            Op::Child
        } else {
            Op::ChildInverted
        });
    }

    Ok(node_count).wrap_with_cost(cost)
}

/// What a collapsible subtree does without being walked, decided from its
/// committed count, or `None` when the walk must enter it.
fn collapse_action(
    class: SubtreeClassification,
    subtree_count: u64,
    state: &EmitState,
) -> Option<CollapseAction> {
    match class {
        SubtreeClassification::Disjoint => Some(CollapseAction::Disjoint),
        SubtreeClassification::Contained => {
            if state.offset_remaining == 0 && state.limit_remaining == Some(0) {
                Some(CollapseAction::PastLimit)
            } else if state.offset_remaining > 0 && subtree_count <= state.offset_remaining {
                Some(CollapseAction::SkippedByOffset)
            } else {
                None
            }
        }
        SubtreeClassification::Boundary => None,
    }
}

/// Applies a collapsed subtree's effect on the page: an offset skip consumes
/// its count units, and the other collapses leave the state alone.
fn settle_collapsed(action: CollapseAction, subtree_count: u64, state: &mut EmitState) {
    if matches!(action, CollapseAction::SkippedByOffset) {
        state.offset_remaining = state.offset_remaining.saturating_sub(subtree_count);
    }
}

/// Emits the child on `left`'s side, whose keys lie strictly between `lo` and
/// `hi`, and returns whether there is one. A trusted read needs only a
/// collapsed child's committed count, which the parent's link carries, so it
/// settles that child without fetching it. A proof loads it for the hashes it
/// commits to.
#[allow(clippy::too_many_arguments)]
fn emit_local_child<S>(
    walker: &mut RefWalker<'_, S>,
    left: bool,
    range: &QueryItem,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
    state: &mut EmitState,
    output: &mut LocalPageOutput<'_>,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> CostResult<bool, Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();
    let Some(link) = walker.tree().link(left) else {
        return Ok(false).wrap_with_cost(cost);
    };
    if matches!(output, LocalPageOutput::Rows(_)) {
        let child_count = cost_return_on_error_no_add!(
            cost,
            provable_count_from_aggregate(link.aggregate_data())
        );
        let class = classify_subtree(lo, hi, range);
        if let Some(action) = collapse_action(class, child_count, state) {
            settle_collapsed(action, child_count, state);
            return Ok(true).wrap_with_cost(cost);
        }
    }
    let walked = cost_return_on_error!(
        &mut cost,
        walker.walk(
            left,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
    );
    let Some(mut child_walker) = walked else {
        return Err(Error::CorruptedState(
            "tree.link(left) was Some but walk returned None",
        ))
        .wrap_with_cost(cost);
    };
    cost_return_on_error!(
        &mut cost,
        emit_local_count_offset_proof(
            &mut child_walker,
            range,
            lo,
            hi,
            state,
            output,
            tree_type,
            grove_version,
        )
    );
    Ok(true).wrap_with_cost(cost)
}
