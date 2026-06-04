//! Versioned dispatch for
//! [`EstimatedLayerSizes::value_with_feature_and_flags_size`].
//!
//! Each `v*.rs` adds one `value_with_feature_and_flags_size_v*` method via
//! a separate `impl EstimatedLayerSizes` block. The dispatcher below
//! selects the implementation by
//! `GroveVersion::merk_versions.average_case_costs.value_with_feature_and_flags_size`.

mod v0;
mod v1;

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use super::EstimatedLayerSizes;
use crate::error::Error;

impl EstimatedLayerSizes {
    /// Returns the size of a value's feature and flags. Version-dispatched
    /// to preserve consensus-locked grove v1/v2 outputs while letting
    /// grove v3+ use a corrected weighted-average formula on the Mix
    /// arm (v0 = `Σ size_i / Σ weight_i`, v1 = `Σ (size_i · weight_i) / Σ weight_i`).
    /// Non-Mix variants are version-independent.
    pub fn value_with_feature_and_flags_size(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        match grove_version
            .merk_versions
            .average_case_costs
            .value_with_feature_and_flags_size
        {
            0 => self.value_with_feature_and_flags_size_v0(grove_version),
            1 => self.value_with_feature_and_flags_size_v1(grove_version),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "EstimatedLayerSizes::value_with_feature_and_flags_size".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }
}
