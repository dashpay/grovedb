use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct MerkVersions {
    pub batch: MerkBatchVersions,
    pub average_case_costs: MerkAverageCaseCostsVersions,
    pub proof: MerkProofVersions,
    pub tree: MerkTreeVersions,
    pub worst_case_costs: MerkWorstCaseCostsVersions,
    pub cost_model: MerkCostModelVersions,
}

/// The versions of the cost rules that are decided above the storage layer
/// and outside any one node of a Merk (`docs/lignindb/cost-model.md`,
/// section 16).
#[derive(Clone, Debug, Default)]
pub struct MerkCostModelVersions {
    /// Which operation carries a changed Merk into the element that hosts
    /// it, and is charged for it.
    ///
    /// - `0` (GROVE_V1..=V5): the write. Every write carries each Merk it
    ///   changed to the root of the grove before it returns, and its cost
    ///   holds every level on the way.
    /// - `1` (GROVE_V6): finalize. A write stops at its stop points and
    ///   carries nothing above them: it opens a Merk, to change it or to
    ///   read from it, by the Merk's own path and root, without reading the
    ///   host, and leaves a mark on the host for nothing. A finalize
    ///   carries every marked Merk once, and its charge is the cost of the
    ///   finalize call.
    ///
    /// The storage layer's rules are the same on both: this slot moves no
    /// charge of a put, a fetch or a hash, only the operation that makes it.
    pub propagation: FeatureVersion,
}

/// Merk's worst-case estimates.
#[derive(Clone, Debug, Default)]
pub struct MerkWorstCaseCostsVersions {
    /// `add_worst_case_merk_finalize`: the nodes of one Merk that a finalize
    /// rewrites on the ways to its marked elements. The function refuses a
    /// grove version on which no finalize exists
    /// (`MerkCostModelVersions::propagation` 0).
    pub add_worst_case_merk_finalize: FeatureVersion,
    /// `add_worst_case_merk_propagate_versioned`: the nodes a write rewrites
    /// on the way to a key of one Merk.
    ///
    /// - `0` (GROVE_V1..=V5): `add_worst_case_merk_propagate`, which takes
    ///   no grove version: the fewest levels a Merk of the given size can
    ///   have, and one seek for each node.
    /// - `1` (GROVE_V6): the most levels an AVL tree of that size can have,
    ///   and the fetch and the put of each node: the estimate of one level
    ///   by itself, which is all the estimate of a write that stops at a
    ///   stop point has.
    pub add_worst_case_merk_propagate: FeatureVersion,
}

/// Merk tree-node method versions.
#[derive(Clone, Debug, Default)]
pub struct MerkTreeVersions {
    /// `TreeNode::put_value` / `TreeNode::put_value_and_reference_value_hash`
    /// — the ordinary `Op::Put` / `Op::PutCombinedReference` replacement of
    /// an existing node's value (Items and References).
    ///
    /// Version 0 keeps the `value_defined_cost` metadata stamped on the node
    /// when it was loaded from storage. When the predecessor was a
    /// specialized element (a tree or a sum item) that metadata is the
    /// predecessor's fixed cost size, so the replacement's storage charge is
    /// derived from the predecessor's fixed size and the physical-size check
    /// is skipped, even though the full replacement bytes are written
    /// (issue #908). Consensus-locked for grove v1..v3.
    ///
    /// Version 1 clears the metadata on every ordinary replacement so the
    /// charge is computed from the replacement's own serialized bytes and
    /// verified against the bytes actually written. Old-value removal
    /// accounting is unchanged: it is always computed from the stored
    /// predecessor bytes.
    pub put_value: FeatureVersion,
    /// `TreeNode::just_in_time_tree_node_value_update`: the client-driven
    /// update a replacement of a stored value runs (flags carry-over,
    /// flags-update callback, removal sectioning).
    ///
    /// Version 0 measures the replacement with the new value carrying the
    /// OLD value's flags, and keeps that measurement when the flags-update
    /// callback answers that it left the flags unchanged. The new value is
    /// then stored with its own flags, so when those differ in length from
    /// the old ones the recorded cost does not match the bytes written and
    /// the commit fails with a storage cost mismatch. Consensus-locked for
    /// grove v1..v3.
    ///
    /// Version 1 re-measures the replacement from the bytes it stores when
    /// the callback answers unchanged and their size differs from the last
    /// measurement taken, and restores the put's value-defined cost
    /// whenever it restores the put's value. Every update whose cost
    /// version 0 records for the bytes it stores is charged the same.
    pub just_in_time_value_update: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct MerkBatchVersions {
    /// Version 0: commit_batch discards accumulated batch costs (legacy bug)
    /// Version 1: commit_batch returns accumulated batch costs
    pub commit: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct MerkAverageCaseCostsVersions {
    /// `add_average_case_merk_propagate`. Versions 0 to 2 are the estimates
    /// of GROVE_V1, V2 and V3..=V5 (`v3.rs` and `v4.rs` say what changed).
    /// Version 3 (GROVE_V6) is version 2 with a fetch for each node it has
    /// a put for, but the root, and with the levels of a count in whole
    /// numbers.
    pub add_average_case_merk_propagate: FeatureVersion,
    /// `EstimatedSumTrees::estimated_size`. Versions 0 to 2 are the
    /// estimates of GROVE_V1, V2 and V3..=V5. Version 3 (GROVE_V6) sizes a
    /// tree's aggregates as the cost of its element does: 9 bytes for a
    /// count or a sum, where version 2 has 8.
    pub sum_tree_estimated_size: FeatureVersion,
    /// `EstimatedLayerSizes::value_with_feature_and_flags_size`. Version
    /// 0 keeps the pre-existing **unweighted** Mix average
    /// `Σ size_i / Σ weight_i` (used by grove v1/v2, consensus-locked).
    /// Version 1 switches to a proper weighted average
    /// `Σ (size_i · weight_i) / Σ weight_i`. Output for non-Mix
    /// variants is identical across versions. Version 2 (GROVE_V6) is
    /// version 1 with sizes that an estimate cannot count refused: the
    /// aggregates of a tree are a byte longer there, and version 1 adds in
    /// 32 bits unchecked.
    pub value_with_feature_and_flags_size: FeatureVersion,
    /// `EstimatedLayerSizes::subtree_with_feature_and_flags_size`. Version
    /// 0 is the function of GROVE_V1..=V5, which adds the aggregates of a
    /// tree to its flags in 32 bits unchecked. Version 1 (GROVE_V6) is
    /// version 0 with sizes that an estimate cannot count refused, for the
    /// byte the aggregates are longer by there.
    pub subtree_with_feature_and_flags_size: FeatureVersion,
    /// `add_average_case_merk_finalize`: what a finalize does inside one
    /// Merk to rewrite its marked elements, but for their replacement. A
    /// node's bytes are sized as `add_average_case_merk_propagate` sizes a
    /// node. The function refuses a grove version on which no finalize
    /// exists (`MerkCostModelVersions::propagation` 0).
    pub add_average_case_merk_finalize: FeatureVersion,
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
    /// Also gates `Merk::prove_local_count_offset_on_range`, which skips
    /// committed count units using the same proof format and verifier.
    pub prove_count_offset_on_range: FeatureVersion,
}
