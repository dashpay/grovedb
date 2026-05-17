use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct MerkVersions {
    pub batch: MerkBatchVersions,
    pub average_case_costs: MerkAverageCaseCostsVersions,
    pub proof: MerkProofVersions,
}

#[derive(Clone, Debug, Default)]
pub struct MerkBatchVersions {
    /// Version 0: commit_batch discards accumulated batch costs (legacy bug)
    /// Version 1: commit_batch returns accumulated batch costs
    pub commit: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct MerkAverageCaseCostsVersions {
    pub add_average_case_merk_propagate: FeatureVersion,
    pub sum_tree_estimated_size: FeatureVersion,
}

/// Merk-level proof method versions.
#[derive(Clone, Debug, Default)]
pub struct MerkProofVersions {
    /// `Merk::prove_count_offset_on_range` — offset-paginated proof
    /// for a single range on a `ProvableCountTree` /
    /// `ProvableCountSumTree`. Version 0 is the initial implementation
    /// shipped in grove v3 alongside the V1 proof envelope; v1/v2 do
    /// not call this method (V0 proofs reject offsets unconditionally,
    /// so the count-offset path never enters their dispatch).
    ///
    /// Bump this if the prover's emitted op stream changes shape in a
    /// way that requires a coordinated verifier update.
    pub prove_count_offset_on_range: FeatureVersion,
}
