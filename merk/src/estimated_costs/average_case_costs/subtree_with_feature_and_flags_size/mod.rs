//! Versioned dispatch for
//! [`EstimatedLayerSizes::subtree_with_feature_and_flags_size`], selected
//! by `GroveVersion::merk_versions.average_case_costs.subtree_with_feature_and_flags_size`.

mod v0;
mod v1;

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use super::EstimatedLayerSizes;
use crate::error::Error;

impl EstimatedLayerSizes {
    /// Returns the size of a subtree's feature and flags
    /// This only takes into account subtrees in the estimated layer info
    /// Only should be used when it is known to be a subtree
    pub fn subtree_with_feature_and_flags_size(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        match grove_version
            .merk_versions
            .average_case_costs
            .subtree_with_feature_and_flags_size
        {
            0 => self.subtree_with_feature_and_flags_size_v0(grove_version),
            1 => self.subtree_with_feature_and_flags_size_v1(grove_version),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "EstimatedLayerSizes::subtree_with_feature_and_flags_size".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }
}
