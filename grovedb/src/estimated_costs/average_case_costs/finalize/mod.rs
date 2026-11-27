//! `average_case_finalize`: versioned dispatch.
//!
//! The average-case estimate of a finalize
//! (`docs/lignindb/cost-model.md`, ES3). A finalize exists from the grove
//! version whose writes stop at stop points
//! (`merk_versions.cost_model.propagation` 1). On an earlier version a write
//! carries to the root of the grove itself and its own estimate holds every
//! level, so there is nothing to estimate and the function refuses.
//!
//! * **[v0]**: `GROVE_V6`, the first.
//!
//! [v0]: self::v0

mod v0;

use std::collections::{HashMap, HashSet};

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    cost_model::{propagation_cost_model, Carrier},
    estimated_costs::average_case_costs::EstimatedLayerInformation,
};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{batch::KeyInfoPath, Error, GroveDb};

impl GroveDb {
    /// The average-case cost of a finalize.
    ///
    /// `stop_points` are the subtrees whose hosts are marked when the
    /// finalize begins: the stop points of the writes since the last
    /// finalize, less those whose mark a later write took with its host
    /// (SP6). A stop point at the root Merk leaves no mark and adds
    /// nothing.
    ///
    /// `paths` describes, as it is when the finalize begins, every Merk
    /// that the finalize rewrites an element of, which is every Merk above
    /// a stop point up to the root Merk, and every stop point: its tree
    /// type sizes the element that hosts it and its keys the root key that
    /// element names. A stop point whose layer is estimated to be empty is
    /// taken to name no root.
    ///
    /// `wrapped` are the Merks, among the stop points and the Merks above
    /// them, whose element is wrapped (`NonCounted`, `NotSummed`,
    /// `NotCountedOrSummed`). A wrapper is a byte of the element, in its
    /// cost and in what is hashed, and a layer does not say that a tree it
    /// holds has one. A wrapped Merk that is left out is estimated a byte
    /// short. A caller that does not know gives every one, and is a byte
    /// over for those that are not.
    ///
    /// Visible in the crate and no further until the grove version is
    /// listed: the change that builds finalize calls it, and makes it
    /// public for a caller that estimates one.
    #[allow(dead_code)]
    pub(crate) fn average_case_finalize(
        stop_points: &[KeyInfoPath],
        wrapped: &HashSet<KeyInfoPath>,
        paths: &HashMap<KeyInfoPath, EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match propagation_cost_model(grove_version).map(|model| model.carrier()) {
            Ok(Carrier::Finalize) => {}
            Ok(Carrier::Write) => {
                return Err(Error::NotSupported(
                    "average_case_finalize: a grove version whose writes carry to the root of \
                     the grove has no finalize"
                        .to_string(),
                ))
                .wrap_with_cost(OperationCost::default())
            }
            Err(e) => return Err(Error::MerkError(e)).wrap_with_cost(OperationCost::default()),
        }
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_finalize
        {
            0 => Self::average_case_finalize_v0(stop_points, wrapped, paths, grove_version),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "average_case_finalize".to_string(),
                    known_versions: vec![0],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
