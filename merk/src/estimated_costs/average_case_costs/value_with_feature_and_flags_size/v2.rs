//! `value_with_feature_and_flags_size` v2: grove version 6.
//!
//! v1, behind `EstimatedLayerSizes::check_sizes`. v1 adds the aggregates of
//! a tree, the flags and the bytes of an element's kind in 32 bits,
//! unchecked. Grove version 6 sizes the aggregates of a tree a byte longer
//! (`EstimatedSumTrees::estimated_size` v3), so a layer that v1 could just
//! count on grove version 5 would overflow here. Sizes of one element that
//! an estimate cannot count are refused before anything is added to them.

use grovedb_version::version::GroveVersion;

use crate::{error::Error, estimated_costs::average_case_costs::EstimatedLayerSizes};

impl EstimatedLayerSizes {
    pub(super) fn value_with_feature_and_flags_size_v2(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        self.check_sizes()?;
        self.value_with_feature_and_flags_size_v1(grove_version)
    }
}
