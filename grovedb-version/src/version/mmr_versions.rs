//! Version gates for the Merkle Mountain Range crate.
//!
//! Only cost accounting is versioned here. Every gate below leaves the
//! returned hashes, roots and proofs bit-identical — what changes is how many
//! `hash_node_calls` the operation reports. That still has to be gated,
//! because costs become fees: a node replaying a historical block must charge
//! what the block was admitted under, so a corrected charge cannot simply
//! replace the old one.

use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct MmrVersions {
    pub cost: MmrCostVersions,
}

/// Hash-charge versions for the three MMR operations that perform blake3
/// merges internally.
///
/// In every case version 0 is the shipped behaviour, which billed the storage
/// reads an operation performed but not the merges those reads fed, and
/// version 1 charges one hash per merge actually computed.
#[derive(Clone, Debug, Default)]
pub struct MmrCostVersions {
    /// `MMR::push`. A push collapses one peak per set trailing bit of the
    /// leaf count, calling `MmrNode::merge` — a blake3 — each time. Version 0
    /// billed the sibling reads those merges consume but not the merges.
    /// Version 1 charges one hash per collapse.
    pub push: FeatureVersion,
    /// `MMR::get_root`. Bagging folds the peaks right-to-left with one
    /// `MmrNode::merge` per additional peak. Version 0 billed the peak reads
    /// only; version 1 charges `peaks - 1` merges.
    pub get_root: FeatureVersion,
    /// `MMR::gen_proof`. Proof generation folds the right-hand peaks through
    /// the same `bag_peaks` helper `get_root` uses. Version 0 charged none of
    /// those merges; version 1 charges `bagging_track - 1`.
    pub gen_proof: FeatureVersion,
}
