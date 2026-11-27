//! `average_case_merk_delete_tree` — **v1** (`GROVE_V6`).
//!
//! [`super::v0`], behind the check of the sizes of the layer
//! (`EstimatedLayerSizes::check_sizes`): v0 adds the flags of the layer and
//! the bytes of their length in 32 bits, unchecked, before any estimate it
//! calls has looked at them.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::average_case_costs::EstimatedLayerInformation, tree_type::TreeType,
};
use grovedb_version::version::GroveVersion;

use crate::{batch::key_info::KeyInfo, Error, GroveDb};

impl GroveDb {
    /// `average_case_merk_delete_tree` v1 — see the module documentation.
    pub(super) fn average_case_merk_delete_tree_v1(
        key: &KeyInfo,
        tree_type: TreeType,
        estimated_layer_information: &EstimatedLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        if let Err(refused) = estimated_layer_information
            .estimated_layer_sizes
            .check_sizes()
        {
            return Err(Error::MerkError(refused)).wrap_with_cost(OperationCost::default());
        }
        Self::average_case_merk_delete_tree_v0(
            key,
            tree_type,
            estimated_layer_information,
            propagate,
            grove_version,
        )
    }
}
