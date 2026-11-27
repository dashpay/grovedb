//! Versioned dispatch for [`add_average_case_merk_finalize`]: the estimate
//! of what a finalize is charged inside one Merk, above the elements it
//! rewrites (`docs/lignindb/cost-model.md`, ES3).
//!
//! A finalize exists from the grove version whose writes stop at stop
//! points (`merk_versions.cost_model.propagation` 1). On an earlier version
//! a write carries to the root of the grove itself, there is nothing to
//! estimate, and the function refuses.

mod v0;

use grovedb_costs::OperationCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};
use v0::add_average_case_merk_finalize_v0;

use super::EstimatedLayerInformation;
use crate::{
    cost_model::{propagation_cost_model, Carrier},
    error::Error,
};

/// Adds the average-case cost of what a finalize does inside one Merk to
/// rewrite `marked` of its elements, but for the replacement of each
/// element (`add_average_case_merk_replace_layered`) and the open of the
/// Merk: the nodes above the marked elements, and the fetches and reads on
/// the way to them.
pub fn add_average_case_merk_finalize(
    cost: &mut OperationCost,
    input: &EstimatedLayerInformation,
    marked: u32,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    if propagation_cost_model(grove_version)?.carrier() != Carrier::Finalize {
        return Err(Error::NotSupported(
            "add_average_case_merk_finalize: a grove version whose writes carry to the root of \
             the grove has no finalize"
                .to_string(),
        ));
    }
    match grove_version
        .merk_versions
        .average_case_costs
        .add_average_case_merk_finalize
    {
        0 => add_average_case_merk_finalize_v0(cost, input, marked, grove_version),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "add_average_case_merk_finalize".to_string(),
                known_versions: vec![0],
                received: version,
            },
        )),
    }
}
