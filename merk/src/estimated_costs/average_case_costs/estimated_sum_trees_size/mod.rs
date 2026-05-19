//! Versioned dispatch for [`EstimatedSumTrees::estimated_size`].
//!
//! Each `v*.rs` adds one `estimated_size_v*` method via a separate
//! `impl EstimatedSumTrees` block. The dispatcher below selects the
//! implementation by
//! `GroveVersion::merk_versions.average_case_costs.sum_tree_estimated_size`.

mod v0;
mod v1;
mod v2;

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use super::EstimatedSumTrees;
use crate::error::Error;

impl EstimatedSumTrees {
    /// Returns the average per-node cost contribution of the sum/count
    /// aggregate state for a tree with this distribution of leaf tree
    /// types. Version-dispatched:
    /// - v0: shipped formula from grove v1 (uses only sum/non-sum weights)
    /// - v1: weighted by per-tree-type `inner_node_type().cost()` across
    ///   the four legacy aggregate weights (grove v2)
    /// - v2: extends v1 with the four `provable_*` weights (grove v3)
    pub(in crate::estimated_costs) fn estimated_size(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        match grove_version
            .merk_versions
            .average_case_costs
            .sum_tree_estimated_size
        {
            0 => self.estimated_size_v0(),
            1 => self.estimated_size_v1(),
            2 => self.estimated_size_v2(),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "EstimatedSumTrees::estimated_size".to_string(),
                    known_versions: vec![0, 1, 2],
                    received: version,
                },
            )),
        }
    }
}
