//! Versioned dispatch for [`add_worst_case_merk_propagate_versioned`]: the
//! worst-case cost of the nodes a write rewrites on the way to a key of
//! one Merk.
//!
//! [`add_worst_case_merk_propagate`](super::add_worst_case_merk_propagate)
//! takes no grove version and is what grove versions 1 to 5 estimate with.
//! This is the same estimate by grove version:
//!
//! * **v0**: `GROVE_V1`..`GROVE_V5`. That function, as it is.
//! * **[v1]**: `GROVE_V6`. The most levels a Merk of the given size can
//!   have, five nodes a level and one, and the fetch and the put of each
//!   node.
//!
//! [v1]: self::v1

mod v1;

use grovedb_costs::OperationCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};
use v1::add_worst_case_merk_propagate_v1;

use super::{add_worst_case_merk_propagate, WorstCaseLayerInformation};
use crate::error::Error;

/// Add worst case cost of a merk propagation, as `grove_version` estimates
/// it.
pub fn add_worst_case_merk_propagate_versioned(
    cost: &mut OperationCost,
    input: &WorstCaseLayerInformation,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    match grove_version
        .merk_versions
        .worst_case_costs
        .add_worst_case_merk_propagate
    {
        0 => add_worst_case_merk_propagate(cost, input),
        1 => add_worst_case_merk_propagate_v1(cost, input),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "add_worst_case_merk_propagate".to_string(),
                known_versions: vec![0, 1],
                received: version,
            },
        )),
    }
}
