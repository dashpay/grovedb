//! `average_case_merk_delete_tree` — versioned dispatch.
//!
//! Adds the average-case cost of removing a tree from a Merk, for one level
//! only: the batch estimator propagates the levels above.
//!
//! * **[v0]** — `GROVE_V1`..`GROVE_V5`. The flags of the layer and the bytes
//!   of their length are added in 32 bits, unchecked.
//! * **[v1]** — `GROVE_V6` (`average_case_merk_delete_tree: 1`). v0, behind
//!   the check of the sizes of the layer: sizes an estimate cannot count are
//!   refused, and a refusal has no cost.
//!
//! The estimate is the admission bound historical blocks are replayed
//! against, so the released versions keep theirs.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::average_case_costs::EstimatedLayerInformation, tree_type::TreeType,
};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{batch::key_info::KeyInfo, estimated_costs::in_full_or_refused, Error, GroveDb};

impl GroveDb {
    /// Add average case for the removal of a tree from a merk
    pub fn average_case_merk_delete_tree(
        key: &KeyInfo,
        tree_type: TreeType,
        estimated_layer_information: &EstimatedLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_merk_delete_tree
        {
            0 => Self::average_case_merk_delete_tree_v0(
                key,
                tree_type,
                estimated_layer_information,
                propagate,
                grove_version,
            ),
            1 => in_full_or_refused(Self::average_case_merk_delete_tree_v1(
                key,
                tree_type,
                estimated_layer_information,
                propagate,
                grove_version,
            )),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "average_case_merk_delete_tree".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
