//! `add_worst_case_merk_finalize` v0, the first: grove version 6.
//!
//! A finalize changes the `marked` elements and every node above one of
//! them. In a Merk of `L` levels that is at most
//! `marked + nodes_above_marked(L, marked)` nodes. Each is charged a fetch
//! (a seek and the biggest record), a put (a seek and the biggest value as
//! replaced bytes) and the two hash calls of a node. Each marked element is
//! also read by its key before it is rewritten: a seek and the biggest
//! record.
//!
//! The levels of a Merk that is given by its number of elements are the
//! most an AVL tree of that many nodes can have, not the fewest: a way to a
//! marked element is as long as the tree is deep.

use grovedb_costs::OperationCost;

use crate::{
    error::Error,
    estimated_costs::{
        checked_levels, most_levels, nodes_above_marked,
        worst_case_costs::{
            WorstCaseLayerInformation, MERK_BIGGEST_KEY_SIZE, MERK_BIGGEST_VALUE_SIZE,
        },
    },
};

pub(super) fn add_worst_case_merk_finalize_v0(
    cost: &mut OperationCost,
    input: &WorstCaseLayerInformation,
    marked: u32,
) -> Result<(), Error> {
    if marked == 0 {
        return Ok(());
    }
    let levels = match input {
        WorstCaseLayerInformation::MaxElementsNumber(elements) => most_levels(*elements),
        WorstCaseLayerInformation::NumberOfLevels(levels) => checked_levels(*levels)?,
    };
    let overflow = || Error::Overflow("overflow for the nodes a finalize rewrites");
    let nodes = marked
        .checked_add(nodes_above_marked(levels, marked))
        .ok_or_else(overflow)?;
    let reads = nodes.checked_add(marked).ok_or_else(overflow)?;
    let biggest_record = u64::from(MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE);

    // Every field in full before any is set: nothing is added where the
    // estimate is refused.
    let seeks = nodes
        .checked_add(reads)
        .and_then(|seeks| cost.seek_count.checked_add(seeks))
        .ok_or_else(overflow)?;
    let loaded = u64::from(reads)
        .checked_mul(biggest_record)
        .and_then(|bytes| cost.storage_loaded_bytes.checked_add(bytes))
        .ok_or_else(overflow)?;
    let replaced = nodes
        .checked_mul(MERK_BIGGEST_VALUE_SIZE)
        .and_then(|bytes| cost.storage_cost.replaced_bytes.checked_add(bytes))
        .ok_or_else(overflow)?;
    let hashes = nodes
        .checked_mul(2)
        .and_then(|hashes| cost.hash_node_calls.checked_add(hashes))
        .ok_or_else(overflow)?;
    cost.seek_count = seeks;
    cost.storage_loaded_bytes = loaded;
    cost.storage_cost.replaced_bytes = replaced;
    cost.hash_node_calls = hashes;
    Ok(())
}
