//! `subtree_with_feature_and_flags_size` v0: grove versions 1 to 5. The
//! function as it has been from the first grove version.

use grovedb_version::version::GroveVersion;

use crate::{error::Error, estimated_costs::average_case_costs::EstimatedLayerSizes};

impl EstimatedLayerSizes {
    pub(super) fn subtree_with_feature_and_flags_size_v0(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        match self {
            EstimatedLayerSizes::AllSubtrees(_, estimated_sum_trees, flags_size) => {
                // 1 for enum type
                // 1 for empty
                // 1 for flags size
                Ok(estimated_sum_trees.estimated_size(grove_version)?
                    + flags_size.unwrap_or_default()
                    + 3)
            }
            EstimatedLayerSizes::Mix { subtrees_size, .. } => match subtrees_size {
                None => Err(Error::WrongEstimatedCostsElementTypeForLevel(
                    "this layer is a mix but doesn't have subtrees",
                )),
                Some((_, est, fs, _)) => {
                    Ok(est.estimated_size(grove_version)? + fs.unwrap_or_default() + 3)
                }
            },
            _ => Err(Error::WrongEstimatedCostsElementTypeForLevel(
                "this layer needs to have trees",
            )),
        }
    }
}
