//! `subtree_with_feature_and_flags_size` v1: grove version 6.
//!
//! v0, behind `EstimatedLayerSizes::check_sizes`. v0 adds the aggregates of
//! a tree to its flags in 32 bits, unchecked, and grove version 6 sizes the
//! aggregates a byte longer (`EstimatedSumTrees::estimated_size` v3): a
//! layer that v0 could just count on grove version 5 would overflow here.

use grovedb_version::version::GroveVersion;

use crate::{error::Error, estimated_costs::average_case_costs::EstimatedLayerSizes};

impl EstimatedLayerSizes {
    pub(super) fn subtree_with_feature_and_flags_size_v1(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        self.check_sizes()?;
        self.subtree_with_feature_and_flags_size_v0(grove_version)
    }
}
