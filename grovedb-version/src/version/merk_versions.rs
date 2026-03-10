use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct MerkVersions {
    pub commit: MerkCommitVersions,
    pub average_case_costs: MerkAverageCaseCostsVersions,
}

#[derive(Clone, Debug, Default)]
pub struct MerkCommitVersions {
    /// Version 0: commit_batch discards accumulated batch costs (legacy bug)
    /// Version 1: commit_batch returns accumulated batch costs
    pub commit: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct MerkAverageCaseCostsVersions {
    pub add_average_case_merk_propagate: FeatureVersion,
    pub sum_tree_estimated_size: FeatureVersion,
}
