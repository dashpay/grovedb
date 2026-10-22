//! Committed-count-unit skip traversal for branch-local pagination.
//! Collapsed skips consume authenticated count units without scanning their
//! contents. Descended rows retain the ordinary unit-row restrictions.

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
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

pub(super) fn emit_local_count_offset_proof<S>(
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

    let collapse_action = match class {
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
    };

    if let Some(action) = collapse_action {
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
            state.offset_remaining = state.offset_remaining.saturating_sub(subtree_count);
        }
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
        let mut child_walker = match walked {
            Some(w) => w,
            None => {
                return Err(Error::CorruptedState(
                    "tree.link(first_dir) was Some but walk returned None",
                ))
                .wrap_with_cost(cost)
            }
        };
        cost_return_on_error!(
            &mut cost,
            emit_local_count_offset_proof(
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

    let make_boundary_node = || {
        if binds_sum_into_hash(tree_type) {
            Node::KVDigestCountSum(node_key.clone(), node_value_hash, node_count, node_sum)
        } else {
            Node::KVDigestCount(node_key.clone(), node_value_hash, node_count)
        }
    };

    let self_node = if !is_in_range || own_struct == 0 {
        make_boundary_node()
    } else if state.offset_remaining > 0 {
        state.offset_remaining -= 1;
        make_boundary_node()
    } else if state.limit_remaining == Some(0) {
        make_boundary_node()
    } else {
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
            emit_local_count_offset_proof(
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

    let _ = (left_link_count, right_link_count);

    Ok(node_count).wrap_with_cost(cost)
}
