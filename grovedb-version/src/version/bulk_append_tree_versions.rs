//! Version gates for the bulk-append tree.
//!
//! As in [`super::mmr_versions`], only cost accounting is versioned: the
//! chunks, roots and stored bytes are identical under every version. These
//! gates matter more than the MMR ones, though, because the value they change
//! reaches a live fee — `CommitmentTree` adds the reported hash count straight
//! into its own `hash_node_calls`, and the shielded pool has been running on
//! it since mainnet activation.

use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct BulkAppendTreeVersions {
    pub cost: BulkAppendTreeCostVersions,
}

#[derive(Clone, Debug, Default)]
pub struct BulkAppendTreeCostVersions {
    /// The `hash_count` a compacting append reports, which
    /// `append_no_state_root` forwards and `CommitmentTree` bills.
    ///
    /// Version 0 reports `hash_count_for_push` — the chunk-blob leaf hash plus
    /// one per peak the MMR push collapses. That omits the peak bagging the
    /// compaction's own `get_root` performs, so a compaction landing on a
    /// multi-peak MMR under-reports by `peaks - 1`.
    ///
    /// Version 1 adds that bagging term. Shipped chunk bytes and roots are
    /// unaffected; what moves is the fee a compacting append is charged.
    pub compaction_hash_count: FeatureVersion,
}
