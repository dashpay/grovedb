//! Versioned dispatch for [`add_worst_case_merk_finalize`]: the worst-case
//! estimate of what a finalize is charged inside one Merk
//! (`docs/lignindb/cost-model.md`, ES3).
//!
//! A finalize exists from the grove version whose writes stop at stop
//! points (`merk_versions.cost_model.propagation` 1). On an earlier version
//! the function refuses.

mod v0;

use grovedb_costs::OperationCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};
use v0::add_worst_case_merk_finalize_v0;

use super::WorstCaseLayerInformation;
use crate::{
    cost_model::{propagation_cost_model, Carrier},
    error::Error,
};

/// Adds the worst-case cost of what a finalize does inside one Merk to
/// rewrite `marked` of its elements: the read of each, and the fetch, the
/// put and the hash of each of them and of every node above them. The
/// hashes of the new values and the open of the Merk are not in it.
///
/// `biggest_value` is the most bytes the value of a node of this Merk is,
/// as it is stored and as its cost counts it: the bytes of a read and of a
/// put are those of a node of that value. No rule of GroveDB bounds a
/// value, so the caller gives its bound.
pub fn add_worst_case_merk_finalize(
    cost: &mut OperationCost,
    input: &WorstCaseLayerInformation,
    marked: u32,
    biggest_value: u32,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    if propagation_cost_model(grove_version)?.carrier() != Carrier::Finalize {
        return Err(Error::NotSupported(
            "add_worst_case_merk_finalize: a grove version whose writes carry to the root of \
             the grove has no finalize"
                .to_string(),
        ));
    }
    match grove_version
        .merk_versions
        .worst_case_costs
        .add_worst_case_merk_finalize
    {
        0 => add_worst_case_merk_finalize_v0(cost, input, marked, biggest_value, grove_version),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "add_worst_case_merk_finalize".to_string(),
                known_versions: vec![0],
                received: version,
            },
        )),
    }
}
