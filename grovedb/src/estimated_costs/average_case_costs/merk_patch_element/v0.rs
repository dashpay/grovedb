//! `average_case_merk_patch_element` — **v0** (`GROVE_V1`..`GROVE_V5`).
//!
//! The estimate as it has been from the first grove version.

use grovedb_costs::{cost_return_on_error_into_no_add, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::{
        add_cost_case_merk_patch,
        average_case_costs::{add_average_case_merk_propagate, EstimatedLayerInformation},
    },
    tree_type::TreeType,
};
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{batch::key_info::KeyInfo, Element, Error, GroveDb};

impl GroveDb {
    /// `average_case_merk_patch_element` v0 — see the module documentation.
    pub(super) fn average_case_merk_patch_element_v0(
        key: &KeyInfo,
        value: &Element,
        change_in_bytes: i32,
        in_tree_type: TreeType,
        propagate_for_level: Option<&EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        match value {
            Element::Item(_, flags) => {
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                // Items need to be always the same serialized size for this to work
                let item_cost_size =
                    cost_return_on_error_into_no_add!(cost, value.serialized_size(grove_version))
                        as u32;
                let value_len = item_cost_size + flags_len;
                add_cost_case_merk_patch(
                    &mut cost,
                    key_len,
                    value_len,
                    change_in_bytes,
                    in_tree_type,
                )
            }
            _ => {
                return Err(Error::InvalidParameter("patching can only be on Items"))
                    .wrap_with_cost(cost);
            }
        };
        if let Some(level) = propagate_for_level {
            add_average_case_merk_propagate(&mut cost, level, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }
}
