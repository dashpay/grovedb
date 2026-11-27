//! `add_average_case_merk_propagate` v3: grove version 6.
//!
//! v2, and the fetch of each node it rewrites but the root. v2 has one
//! seek for each node it rewrites, which is the node's put: a node for
//! each level, and where there is more than one level a node beside the
//! way, which a rotation rewrites. The apply also fetches each of them that
//! is not loaded, and the open of the Merk loads its root alone. So there
//! is a fetch for each level below the first, and one for the node beside
//! the way. The loaded bytes of v2 are already those of every node.
//!
//! On grove versions 1 to 5 a batch's estimate has every level up to the
//! root of the grove. A write that stops at a stop point has its own level
//! alone, so that level is estimated in full
//! (`docs/lignindb/cost-model.md`, section 16.5).
//!
//! It counts the levels of a layer that is given by its count in whole
//! numbers (`EstimatedLayerCount::fewest_levels`), where v2 takes a
//! logarithm in floating point.
//!
//! It also keeps what it is given from overflowing the arithmetic. A
//! height no Merk can have is refused, and so are sizes of one element that
//! an estimate cannot count (`EstimatedLayerSizes::check_sizes`). For a
//! layer of one kind of element v2 multiplies the bytes of a node by the
//! nodes in 32 bits: here the bytes of one node are taken from v2 and
//! multiplied in the width of each field, and a product that does not fit
//! its field is refused. For a `Mix` layer v2 weighs the kinds, multiplies
//! by the nodes and divides by the weights, in 64 bits, and refuses a total
//! beyond 32 for the loaded bytes as for the replaced ones. The loaded bytes
//! of a cost are 64 bits wide, so here they are kept in them: the same
//! weights, the same order and the same rounding, and a layer of items is
//! counted the same whether it is given as items or as a `Mix` of them. (A
//! `Mix` takes the flags of a tree without the bytes of their length, as
//! v2 does, so a layer of trees with flags is a byte a node apart.) A kind
//! that a `Mix` gives no weight is not sized. Nothing is added to the cost
//! where anything is refused.
//!
//! v2 is what grove versions 3 to 5 select, so it cannot change under this.

use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;

use super::v2::add_average_case_merk_propagate_v2;
use crate::{
    error::Error,
    estimated_costs::{
        average_case_costs::{EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes},
        checked_levels, LAYER_COST_SIZE,
    },
    tree::kv::KV,
    tree_type::TreeType,
};

/// For each kind of element a `Mix` layer names: its weight, and the bytes
/// one node of that kind replaces and loads, as v2 sizes them for such a
/// layer. A tree's flags are taken without the bytes of their length there,
/// as v2 takes them.
fn weighed_nodes_of_a_mix(
    sizes: &EstimatedLayerSizes,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> Result<Vec<(u64, u64, u64)>, Error> {
    let EstimatedLayerSizes::Mix {
        subtrees_size,
        items_size,
        references_size,
        items_with_sum_item_size,
        references_with_sum_item_size,
    } = sizes
    else {
        return Ok(Vec::new());
    };
    let node_type = tree_type.inner_node_type();
    let mut kinds = Vec::new();
    // A kind of no weight adds nothing to the sum, and is not sized: v2 sizes
    // none where every weight is nothing.
    if let Some((key, sum_trees, flags, weight)) = subtrees_size.filter(|kind| kind.3 > 0) {
        let value_len = LAYER_COST_SIZE + flags.unwrap_or(0);
        let aggregates = sum_trees.estimated_size(grove_version)?;
        let key = u32::from(key);
        kinds.push((
            u64::from(weight),
            u64::from(
                KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key, value_len, node_type,
                ) + aggregates,
            ),
            u64::from(KV::layered_node_byte_cost_size_for_key_and_value_lengths(
                key,
                value_len + aggregates,
                node_type,
            )),
        ));
    }
    // An element with a sum is ten bytes longer than its value and flags.
    for (kind, with_a_sum) in [
        (items_size, 0),
        (references_size, 0),
        (items_with_sum_item_size, 10),
        (references_with_sum_item_size, 10),
    ] {
        if let Some((key, value, flags, weight)) = kind.filter(|kind| kind.3 > 0) {
            let value_len = value + flags.unwrap_or(0) + with_a_sum;
            let key = u32::from(key);
            kinds.push((
                u64::from(weight),
                u64::from(KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                    key, value_len, node_type,
                )),
                u64::from(KV::node_byte_cost_size_for_key_and_raw_value_lengths(
                    key, value_len, node_type,
                )),
            ));
        }
    }
    Ok(kinds)
}

pub(super) fn add_average_case_merk_propagate_v3(
    cost: &mut OperationCost,
    input: &EstimatedLayerInformation,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    let levels = checked_levels(input.estimated_layer_count.fewest_levels())?;
    if levels == 0 {
        return Ok(());
    }
    input.estimated_layer_sizes.check_sizes()?;
    let overflow = || Error::Overflow("overflow for the nodes a propagation rewrites");

    // The nodes v2 rewrites: one for each level, and one beside the way
    // where there is more than one level.
    let nodes = levels + u32::from(levels > 1);
    let mut level = OperationCost {
        seek_count: nodes,
        hash_node_calls: 2 * nodes,
        ..Default::default()
    };
    if matches!(input.estimated_layer_sizes, EstimatedLayerSizes::Mix { .. }) {
        // As v2: the bytes of a node of each kind by the kind's weight, by
        // the nodes, divided by the weights. The loaded bytes stay in the
        // 64 bits their field has.
        let kinds =
            weighed_nodes_of_a_mix(&input.estimated_layer_sizes, input.tree_type, grove_version)?;
        let weights: u64 = kinds.iter().map(|(weight, ..)| weight).sum();
        if weights != 0 {
            let weighed = |bytes: fn(&(u64, u64, u64)) -> u64| {
                kinds
                    .iter()
                    .try_fold(0u64, |sum, kind| {
                        kind.0.checked_mul(bytes(kind))?.checked_add(sum)
                    })
                    .and_then(|sum| sum.checked_mul(u64::from(nodes)))
                    .map(|scaled| scaled / weights)
                    .ok_or_else(overflow)
            };
            level.storage_cost.replaced_bytes =
                u32::try_from(weighed(|kind| kind.1)?).map_err(|_| overflow())?;
            level.storage_loaded_bytes = weighed(|kind| kind.2)?;
        }
    } else {
        let one_node_layer = EstimatedLayerInformation {
            tree_type: input.tree_type,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(1, false),
            estimated_layer_sizes: input.estimated_layer_sizes,
        };
        let mut one_node = OperationCost::default();
        add_average_case_merk_propagate_v2(&mut one_node, &one_node_layer, grove_version)?;
        level.storage_cost.replaced_bytes = nodes
            .checked_mul(one_node.storage_cost.replaced_bytes)
            .ok_or_else(overflow)?;
        level.storage_loaded_bytes = u64::from(nodes)
            .checked_mul(one_node.storage_loaded_bytes)
            .ok_or_else(overflow)?;
    }
    // The fetch of each node that is rewritten but the root: one for each
    // level below the first, and one for the node beside the way.
    let fetches = if levels > 1 { levels } else { 0 };

    let replaced = cost
        .storage_cost
        .replaced_bytes
        .checked_add(level.storage_cost.replaced_bytes)
        .ok_or_else(overflow)?;
    let loaded = cost
        .storage_loaded_bytes
        .checked_add(level.storage_loaded_bytes)
        .ok_or_else(overflow)?;
    let seeks = level
        .seek_count
        .checked_add(fetches)
        .and_then(|seeks| cost.seek_count.checked_add(seeks))
        .ok_or_else(overflow)?;
    let hashes = cost
        .hash_node_calls
        .checked_add(level.hash_node_calls)
        .ok_or_else(overflow)?;

    cost.storage_cost.replaced_bytes = replaced;
    cost.storage_loaded_bytes = loaded;
    cost.seek_count = seeks;
    cost.hash_node_calls = hashes;
    Ok(())
}
