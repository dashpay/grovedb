//! `average_case_merk_insert_element` — **v0** (released behaviour,
//! `GROVE_V1`..`GROVE_V3`).
//!
//! Sizes every element that is not a tree by its serialized size, a
//! sum-bearing item included. Differs from [`super::v1`] only there: the
//! apply charges a sum-bearing item its specialized cost, up to 8 bytes more.
//! Preserved because historical admission decisions were made with it; see
//! the module docs in [`super`][`mod@super`].

use grovedb_costs::{cost_return_on_error_into_no_add, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    estimated_costs::{
        add_cost_case_merk_insert, add_cost_case_merk_insert_layered,
        average_case_costs::{add_average_case_merk_propagate, EstimatedLayerInformation},
    },
    tree_type::{CostSize, TreeType},
};
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{batch::key_info::KeyInfo, Element, Error, GroveDb};

impl GroveDb {
    /// `average_case_merk_insert_element` v0 — see the module documentation.
    pub(super) fn average_case_merk_insert_element_v0(
        key: &KeyInfo,
        value: &Element,
        in_tree_type: TreeType,
        propagate_for_level: Option<&EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        if let Some((flags, tree_type)) = value.tree_flags_and_type() {
            let flags_len = flags.as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
            let tree_cost_size = tree_type.cost_size();
            let value_len = tree_cost_size + flags_len;
            add_cost_case_merk_insert_layered(&mut cost, key_len, value_len, in_tree_type)
        } else {
            add_cost_case_merk_insert(
                &mut cost,
                key_len,
                cost_return_on_error_into_no_add!(cost, value.serialized_size(grove_version))
                    as u32,
                in_tree_type,
            )
        }
        if let Some(level) = propagate_for_level {
            add_average_case_merk_propagate(&mut cost, level, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }
}
