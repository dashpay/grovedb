//! `add_average_case_merk_finalize` v0, the first: grove version 6.
//!
//! A finalize rewrites `marked` elements of a Merk and every node above one
//! of them. The marked elements are taken to be at the last of the Merk's
//! `L` levels, where their ways are longest, so the nodes above them are at
//! most `nodes_above_marked(L, marked)`. What is added here:
//!
//! - for each node above a marked element, its put and its two hash calls,
//!   with the layer's average value cost as replaced bytes;
//! - the fetch of every changed node but the root, which the open of the
//!   Merk fetched, and the loaded bytes of every changed node;
//! - for each marked element below the root, the read by its key that comes
//!   before it is rewritten: a seek and its bytes.
//!
//! The put of a marked element, its replaced bytes and its hashes are the
//! replacement's and not added here. A node's bytes are sized as the grove
//! version's `add_average_case_merk_propagate` sizes one node of the layer.
//! A finalize inserts and removes nothing, so no node is added for a
//! rotation.

use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;

use crate::{
    error::Error,
    estimated_costs::{
        average_case_costs::{
            add_average_case_merk_propagate, EstimatedLayerCount, EstimatedLayerInformation,
        },
        checked_levels, nodes_above_marked,
    },
};

pub(super) fn add_average_case_merk_finalize_v0(
    cost: &mut OperationCost,
    input: &EstimatedLayerInformation,
    marked: u32,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    if marked == 0 {
        return Ok(());
    }
    // A Merk that hosts a marked element has a node.
    let levels = checked_levels(input.estimated_layer_count.fewest_levels())?.max(1);
    let above = nodes_above_marked(levels, marked);
    let read_by_key = if levels > 1 { marked } else { 0 };

    let one_node_layer = EstimatedLayerInformation {
        tree_type: input.tree_type,
        estimated_layer_count: EstimatedLayerCount::EstimatedLevel(1, false),
        estimated_layer_sizes: input.estimated_layer_sizes,
    };
    let mut one_node = OperationCost::default();
    add_average_case_merk_propagate(&mut one_node, &one_node_layer, grove_version)?;

    let overflow = || Error::Overflow("overflow for the nodes a finalize rewrites");
    let changed = above.checked_add(marked).ok_or_else(overflow)?;
    let fetches = changed - 1;
    let seeks = above
        .checked_add(fetches)
        .and_then(|seeks| seeks.checked_add(read_by_key))
        .ok_or_else(overflow)?;
    let loads = changed.checked_add(read_by_key).ok_or_else(overflow)?;

    // Every field in full before any is set: nothing is added where the
    // estimate is refused.
    let seeks = cost.seek_count.checked_add(seeks).ok_or_else(overflow)?;
    let hashes = above
        .checked_mul(2)
        .and_then(|hashes| cost.hash_node_calls.checked_add(hashes))
        .ok_or_else(overflow)?;
    let replaced = above
        .checked_mul(one_node.storage_cost.replaced_bytes)
        .and_then(|bytes| cost.storage_cost.replaced_bytes.checked_add(bytes))
        .ok_or_else(overflow)?;
    let loaded = u64::from(loads)
        .checked_mul(one_node.storage_loaded_bytes)
        .and_then(|bytes| cost.storage_loaded_bytes.checked_add(bytes))
        .ok_or_else(overflow)?;
    cost.seek_count = seeks;
    cost.hash_node_calls = hashes;
    cost.storage_cost.replaced_bytes = replaced;
    cost.storage_loaded_bytes = loaded;
    Ok(())
}
