//! `add_worst_case_merk_propagate` v1: grove version 6.
//!
//! Differs from the estimate of grove versions 1 to 5 in three things:
//!
//! - **The levels of a Merk that is given by its number of elements are
//!   the most an AVL tree of that size can have.** The earlier estimate
//!   takes the fewest, `ceil(log2(n + 1))`. A height no Merk can have is
//!   refused.
//! - **Five nodes for each level, and one more.** At a level the write
//!   changes the node on its way, and where it rebalances there, up to four
//!   beside it: a double rotation detaches the child on the taller side,
//!   that child's child and both children of that one, and attaches each
//!   again (`Walker::maybe_balance`, `rotate`). The one more is the other
//!   child of a removed node, which is attached under the node that takes
//!   its place. The earlier estimate has one or two nodes beside the way in
//!   all. An insertion rebalances at one level. A removal can rebalance at
//!   every level on its way back up, and where the removed node has two
//!   children it walks the edge below it, whose levels are among these.
//! - **Each node is charged its fetch and its put**: two seeks. The earlier
//!   estimate has one, and the apply fetches every node on the way that is
//!   not loaded before it writes it.
//!
//! The bytes of a node are `MERK_BIGGEST_VALUE_SIZE` and
//! `MERK_BIGGEST_KEY_SIZE`, as before. Those are no limits of GroveDB: a
//! Merk that holds a longer value is charged more than this for it.
//!
//! On grove versions 1 to 5 a batch's estimate has every level up to the
//! root of the grove. A write that stops at a stop point has its own level
//! alone, so that level is estimated in full
//! (`docs/lignindb/cost-model.md`, section 16.5).

use grovedb_costs::OperationCost;

use crate::{
    error::Error,
    estimated_costs::{
        checked_levels, most_levels,
        worst_case_costs::{
            WorstCaseLayerInformation, MERK_BIGGEST_KEY_SIZE, MERK_BIGGEST_VALUE_SIZE,
        },
    },
};

pub(super) fn add_worst_case_merk_propagate_v1(
    cost: &mut OperationCost,
    input: &WorstCaseLayerInformation,
) -> Result<(), Error> {
    let levels = match input {
        WorstCaseLayerInformation::MaxElementsNumber(elements) => most_levels(*elements),
        WorstCaseLayerInformation::NumberOfLevels(levels) => checked_levels(*levels)?,
    };
    let overflow = || Error::Overflow("overflow for the nodes a propagation rewrites");
    let nodes = match levels {
        0 => 0,
        levels => levels
            .checked_mul(5)
            .and_then(|nodes| nodes.checked_add(1))
            .ok_or_else(overflow)?,
    };

    // Every field in full before any is set: nothing is added where the
    // estimate is refused.
    let replaced = nodes
        .checked_mul(MERK_BIGGEST_VALUE_SIZE)
        .and_then(|bytes| cost.storage_cost.replaced_bytes.checked_add(bytes))
        .ok_or_else(overflow)?;
    let loaded = u64::from(nodes)
        .checked_mul(u64::from(MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE))
        .and_then(|bytes| cost.storage_loaded_bytes.checked_add(bytes))
        .ok_or_else(overflow)?;
    let seeks = nodes
        .checked_mul(2)
        .and_then(|seeks| cost.seek_count.checked_add(seeks))
        .ok_or_else(overflow)?;
    let hashes = nodes
        .checked_mul(2)
        .and_then(|hashes| cost.hash_node_calls.checked_add(hashes))
        .ok_or_else(overflow)?;
    cost.storage_cost.replaced_bytes = replaced;
    cost.storage_loaded_bytes = loaded;
    cost.seek_count = seeks;
    cost.hash_node_calls = hashes;
    Ok(())
}
