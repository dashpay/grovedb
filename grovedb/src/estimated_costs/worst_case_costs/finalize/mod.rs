//! `worst_case_finalize`: versioned dispatch.
//!
//! The worst-case estimate of a finalize
//! (`docs/lignindb/cost-model.md`, ES3). A finalize exists from the grove
//! version whose writes stop at stop points
//! (`merk_versions.cost_model.propagation` 1). On an earlier version the
//! function refuses.
//!
//! * **[v0]**: `GROVE_V6`, the first.
//!
//! [v0]: self::v0

mod v0;

use std::collections::HashMap;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    cost_model::{propagation_cost_model, Carrier},
    estimated_costs::worst_case_costs::WorstCaseLayerInformation,
};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{batch::KeyInfoPath, Error, GroveDb};

impl GroveDb {
    /// The worst-case cost of a finalize.
    ///
    /// `stop_points` are the subtrees whose hosts are marked when the
    /// finalize begins: the stop points of the writes since the last
    /// finalize, less those whose mark a later write took with its host
    /// (SP6). A stop point at the root Merk leaves no mark and adds
    /// nothing.
    ///
    /// `paths` describes every Merk that the finalize rewrites an element
    /// of, which is every Merk above a stop point up to the root Merk.
    ///
    /// `biggest_element` is the most bytes an element of a Merk the
    /// finalize rewrites a node of serializes to: the elements that host a
    /// marked Merk, and those above them in their Merks. The hash of a
    /// carried element is counted from it, and the bytes every read loads
    /// and every put replaces are those of a node of such an element. No
    /// rule of GroveDB bounds an element: the flags of a tree are as long
    /// as they are given, and so is an item. So the estimate is a bound for
    /// the groves whose elements are within what its caller gives, and one
    /// too long for a node to be sized by it is refused.
    ///
    /// Visible in the crate and no further until the grove version is
    /// listed: the change that builds finalize calls it, and makes it
    /// public for a caller that estimates one.
    #[allow(dead_code)]
    pub(crate) fn worst_case_finalize(
        stop_points: &[KeyInfoPath],
        paths: &HashMap<KeyInfoPath, WorstCaseLayerInformation>,
        biggest_element: u32,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match propagation_cost_model(grove_version).map(|model| model.carrier()) {
            Ok(Carrier::Finalize) => {}
            Ok(Carrier::Write) => {
                return Err(Error::NotSupported(
                    "worst_case_finalize: a grove version whose writes carry to the root of the \
                     grove has no finalize"
                        .to_string(),
                ))
                .wrap_with_cost(OperationCost::default())
            }
            Err(e) => return Err(Error::MerkError(e)).wrap_with_cost(OperationCost::default()),
        }
        match grove_version
            .grovedb_versions
            .operations
            .worst_case
            .worst_case_finalize
        {
            0 => Self::worst_case_finalize_v0(stop_points, paths, biggest_element, grove_version),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "worst_case_finalize".to_string(),
                    known_versions: vec![0],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
