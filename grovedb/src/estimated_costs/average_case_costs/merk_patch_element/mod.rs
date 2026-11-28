//! `average_case_merk_patch_element` — versioned dispatch.
//!
//! Adds the average-case cost of patching an item in a Merk, for one level
//! only: the batch estimator propagates the levels above.
//!
//! * **[v0]** — `GROVE_V1`..`GROVE_V5`.
//! * **[v1]** — `GROVE_V6` (`average_case_merk_patch_element: 1`). The patch
//!   sized from the bytes of the item, which have its flags, where v0 adds
//!   the flags to them once more; a growth of more bytes than the item has
//!   refused; and the hash calls of the put counted from the bytes of the
//!   element.
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

use crate::{
    batch::key_info::KeyInfo, estimated_costs::in_full_or_refused, Element, Error, GroveDb,
};

impl GroveDb {
    /// Add average case for patching an element in merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn average_case_merk_patch_element(
        key: &KeyInfo,
        value: &Element,
        change_in_bytes: i32,
        in_tree_type: TreeType,
        propagate_for_level: Option<&EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_merk_patch_element
        {
            0 => Self::average_case_merk_patch_element_v0(
                key,
                value,
                change_in_bytes,
                in_tree_type,
                propagate_for_level,
                grove_version,
            ),
            1 => in_full_or_refused(Self::average_case_merk_patch_element_v1(
                key,
                value,
                change_in_bytes,
                in_tree_type,
                propagate_for_level,
                grove_version,
            )),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "average_case_merk_patch_element".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
