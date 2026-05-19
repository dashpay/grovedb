//! Versioned dispatch for [`add_average_case_merk_propagate`] (and the
//! cost-wrapping convenience helper [`average_case_merk_propagate`]).
//!
//! Each `v*.rs` defines one `add_average_case_merk_propagate_v*` free
//! function. The dispatcher below selects the implementation by
//! `GroveVersion::merk_versions.average_case_costs.add_average_case_merk_propagate`.

#[cfg(feature = "minimal")]
mod v0;
#[cfg(feature = "minimal")]
mod v1;
#[cfg(feature = "minimal")]
mod v2;

#[cfg(feature = "minimal")]
use v0::add_average_case_merk_propagate_v0;
#[cfg(feature = "minimal")]
use v1::add_average_case_merk_propagate_v1;
#[cfg(feature = "minimal")]
use v2::add_average_case_merk_propagate_v2;

#[cfg(feature = "minimal")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};
#[cfg(feature = "minimal")]
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

#[cfg(feature = "minimal")]
use super::EstimatedLayerInformation;
#[cfg(feature = "minimal")]
use crate::error::Error;

#[cfg(feature = "minimal")]
/// Average case cost of propagating a merk
pub fn average_case_merk_propagate(
    input: &EstimatedLayerInformation,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    add_average_case_merk_propagate(&mut cost, input, grove_version).wrap_with_cost(cost)
}

#[cfg(feature = "minimal")]
/// Add average case cost for propagating a merk
pub fn add_average_case_merk_propagate(
    cost: &mut OperationCost,
    input: &EstimatedLayerInformation,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    match grove_version
        .merk_versions
        .average_case_costs
        .add_average_case_merk_propagate
    {
        0 => add_average_case_merk_propagate_v0(cost, input, grove_version),
        1 => add_average_case_merk_propagate_v1(cost, input, grove_version),
        2 => add_average_case_merk_propagate_v2(cost, input, grove_version),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "add_average_case_merk_propagate".to_string(),
                known_versions: vec![0, 1, 2],
                received: version,
            },
        )),
    }
}
