//! `add_worst_case_merk_finalize` v0, the first: grove version 6.
//!
//! A finalize changes the `marked` elements and every node above one of
//! them. In a Merk of `L` levels that is at most
//! `marked + nodes_above_marked(L, marked)` nodes. Each is charged a fetch
//! (a seek and the longest record), a put (a seek and the most bytes a node
//! replaces) and the two hash calls of a node. Each marked element is also
//! read by its key before it is rewritten: a seek and the longest record.
//!
//! The bytes are those of a node whose value is as long as the caller says
//! a value of this Merk can be (`biggest_value`), under the longest key,
//! with the feature and the links that cost and encode the most: no rule of
//! GroveDB bounds a value, so a bound of bytes needs its length from the
//! caller. The worst case of a level takes 65,535 bytes instead
//! (`MERK_BIGGEST_VALUE_SIZE`), with none of what a node has around its
//! value, and is exceeded by a value of that many bytes and by any longer.
//! A value too long for a node to be sized by it in 32 bits is refused.
//!
//! The levels of a Merk that is given by its number of elements are the
//! most an AVL tree of that many nodes can have, not the fewest: a way to a
//! marked element is as long as the tree is deep.

use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;

use crate::{
    cost_model::{longest_node_record_len, storage_cost_model},
    error::Error,
    estimated_costs::{
        checked_levels, most_levels, nodes_above_marked,
        worst_case_costs::{
            WorstCaseLayerInformation, MERK_BIGGEST_KEY_SIZE, MERK_BIGGEST_NODE_TYPE,
        },
    },
    tree::kv::KV,
};

/// The longest value a node can be sized by: 65,536 bytes under what 32
/// bits hold, as `EstimatedLayerSizes::check_sizes` has it. The key, the
/// hashes, the feature and a link are added to it, well under that many.
const LONGEST_SIZED_VALUE: u32 = u32::MAX - 65_536;

/// The longest record a node whose value is at most `biggest_value` bytes
/// is read as, and the most bytes such a node replaces when it is put: its
/// value with its two hashes and its feature, the bytes of that length, and
/// the link to it, under the longest key and of the node type that costs
/// the most. A tree's node replaces fewer: it has one hash.
pub(super) fn biggest_node(
    biggest_value: u32,
    grove_version: &GroveVersion,
) -> Result<(u64, u32), Error> {
    if biggest_value > LONGEST_SIZED_VALUE {
        return Err(Error::Overflow(
            "a value longer than a worst case can size a node by",
        ));
    }
    let record =
        longest_node_record_len(storage_cost_model(grove_version), biggest_value as usize)?;
    let replaced = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
        MERK_BIGGEST_KEY_SIZE,
        biggest_value,
        MERK_BIGGEST_NODE_TYPE,
    );
    Ok((record as u64, replaced))
}

pub(super) fn add_worst_case_merk_finalize_v0(
    cost: &mut OperationCost,
    input: &WorstCaseLayerInformation,
    marked: u32,
    biggest_value: u32,
    grove_version: &GroveVersion,
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
    let (record, replaced_by_a_node) = biggest_node(biggest_value, grove_version)?;

    // Every field in full before any is set: nothing is added where the
    // estimate is refused.
    let seeks = nodes
        .checked_add(reads)
        .and_then(|seeks| cost.seek_count.checked_add(seeks))
        .ok_or_else(overflow)?;
    let loaded = u64::from(reads)
        .checked_mul(record)
        .and_then(|bytes| cost.storage_loaded_bytes.checked_add(bytes))
        .ok_or_else(overflow)?;
    let replaced = nodes
        .checked_mul(replaced_by_a_node)
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
