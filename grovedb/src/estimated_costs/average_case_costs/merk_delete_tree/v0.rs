//! `average_case_merk_delete_tree` — **v0** (`GROVE_V1`..`GROVE_V5`).
//!
//! The estimate as it has been from the first grove version. It adds the
//! flags of the layer and the bytes of their length in 32 bits, unchecked.

use grovedb_costs::{cost_return_on_error_no_add, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::average_case_costs::{
        add_average_case_merk_delete_layered, add_average_case_merk_propagate,
        EstimatedLayerInformation,
    },
    tree_type::{CostSize, TreeType},
};
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{batch::key_info::KeyInfo, Error, GroveDb};

impl GroveDb {
    /// `average_case_merk_delete_tree` v0 — see the module documentation.
    pub(super) fn average_case_merk_delete_tree_v0(
        key: &KeyInfo,
        tree_type: TreeType,
        estimated_layer_information: &EstimatedLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_size = cost_return_on_error_no_add!(
            cost,
            estimated_layer_information
                .estimated_layer_sizes
                .layered_flags_size()
                .map_err(Error::MerkError)
        )
        .map(|f| f + f.required_space() as u32)
        .unwrap_or_default();
        let tree_cost_size = tree_type.cost_size();
        let layer_extra_size = tree_cost_size + flags_size;
        add_average_case_merk_delete_layered(&mut cost, key_len, layer_extra_size);
        if propagate {
            add_average_case_merk_propagate(&mut cost, estimated_layer_information, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }
}
