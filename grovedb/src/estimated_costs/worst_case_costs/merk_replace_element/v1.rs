//! `worst_case_merk_replace_element` — **v1** (`GROVE_V4`+).
//!
//! Sizes a `SumItem` or `ItemWithSumItem` (wrapped or not) by the specialized
//! cost the apply writes it with: `SUM_ITEM_COST_SIZE` and its flags, plus
//! the item bytes of an `ItemWithSumItem` and the byte of a wrapper. Differs
//! from [`super::v0`] only there: v0 leaves out a wrapped `SumItem`'s wrapper
//! byte and sizes an `ItemWithSumItem` by its serialized size, which encodes
//! the sum value as a varint and so comes in up to 8 bytes below.
//!
//! The arms keep their v0 shapes: a `SumItem` has one fixed size, so it is
//! still replaced at the same size, while an `ItemWithSumItem` carries an
//! item of any length, so it is still charged as a replace that may grow.

use grovedb_costs::{cost_return_on_error_into_no_add, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    element::costs::ElementCostExtensions,
    estimated_costs::{
        add_cost_case_merk_replace, add_cost_case_merk_replace_layered,
        add_cost_case_merk_replace_same_size,
        worst_case_costs::{add_worst_case_merk_propagate, WorstCaseLayerInformation},
    },
    tree_type::{TreeType, SUM_TREE_COST_SIZE, TREE_COST_SIZE},
};
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{batch::key_info::KeyInfo, Element, Error, GroveDb};

impl GroveDb {
    /// `worst_case_merk_replace_element` v1 — see the module documentation.
    pub(super) fn worst_case_merk_replace_element_v1(
        key: &KeyInfo,
        value: &Element,
        in_parent_tree_type: TreeType,
        propagate_for_level: Option<&WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        // Look through wrapper variants for cost dispatch.
        let wrapper_overhead = if value.is_wrapped() { 1u32 } else { 0 };
        // The specialized cost of a sum-bearing item already holds its flags
        // and a wrapper's byte.
        let specialized_cost = || {
            value.specialized_value_defined_cost(grove_version).ok_or(
                Error::CorruptedCodeExecution("sum items should always have a value defined cost"),
            )
        };
        match value.underlying() {
            Element::Tree(_, flags) | Element::SumTree(_, _, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let tree_cost_size = if value.is_sum_tree() {
                    SUM_TREE_COST_SIZE
                } else {
                    TREE_COST_SIZE
                };
                let value_len = tree_cost_size + flags_len + wrapper_overhead;
                add_cost_case_merk_replace_layered(
                    &mut cost,
                    key_len,
                    value_len,
                    in_parent_tree_type,
                )
            }
            Element::SumItem(..) => {
                let value_len = cost_return_on_error_into_no_add!(cost, specialized_cost());
                add_cost_case_merk_replace_same_size(
                    &mut cost,
                    key_len,
                    value_len,
                    in_parent_tree_type,
                )
            }
            Element::ItemWithSumItem(..) => {
                let value_len = cost_return_on_error_into_no_add!(cost, specialized_cost());
                add_cost_case_merk_replace(&mut cost, key_len, value_len, in_parent_tree_type)
            }
            _ => add_cost_case_merk_replace(
                &mut cost,
                key_len,
                cost_return_on_error_into_no_add!(cost, value.serialized_size(grove_version))
                    as u32,
                in_parent_tree_type,
            ),
        };
        if let Some(level) = propagate_for_level {
            add_worst_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }
}
