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
//! its field is refused. For a `Mix` layer v2 is called as it is: it adds
//! up in 64 bits and refuses a total beyond 32. Nothing is added to the
//! cost where anything is refused.
//!
//! v2 is what grove versions 3 to 5 select, so it cannot change under this.

use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;

use super::v2::add_average_case_merk_propagate_v2;
use crate::{
    error::Error,
    estimated_costs::{
        average_case_costs::{EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes},
        checked_levels,
    },
};

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

    let mut level = OperationCost::default();
    if matches!(input.estimated_layer_sizes, EstimatedLayerSizes::Mix { .. }) {
        let leveled = EstimatedLayerInformation {
            tree_type: input.tree_type,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(levels, false),
            estimated_layer_sizes: input.estimated_layer_sizes,
        };
        add_average_case_merk_propagate_v2(&mut level, &leveled, grove_version)?;
    } else {
        let one_node_layer = EstimatedLayerInformation {
            tree_type: input.tree_type,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(1, false),
            estimated_layer_sizes: input.estimated_layer_sizes,
        };
        let mut one_node = OperationCost::default();
        add_average_case_merk_propagate_v2(&mut one_node, &one_node_layer, grove_version)?;
        // The nodes v2 rewrites: one for each level, and one beside the way
        // where there is more than one level.
        let nodes = levels + u32::from(levels > 1);
        level.seek_count = nodes;
        level.hash_node_calls = 2 * nodes;
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
