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
    /// How an append's data-storage writes are reported to the storage cost
    /// layer (issue #822).
    ///
    /// Version 0 issues every put with no cost information, so the commit
    /// path charges key + value as NEW storage: a dense-buffer slot rewritten
    /// in epoch 2 and later, and the chunk blob that supersedes the buffer it
    /// was built from, are both billed as permanent growth — the whole blob
    /// (≈ 630 KB at `chunk_power` 11) lands on the one compacting append.
    ///
    /// Version 1 charges each entry's permanent bytes once, at its own
    /// append: the entry's chunk-blob share (`value.len()`) is reported as
    /// added storage on every append; a buffer slot that already holds a
    /// committed value is reported as replaced (growth added, shrink not
    /// credited) and a slot written for the first time stays fully added; the
    /// compaction blob is reported as a replacement of the entry bytes it
    /// supersedes, with only its framing (and the MMR internal nodes) added.
    /// Stored bytes, chunks and roots are identical under both versions.
    pub append_storage_accounting: FeatureVersion,
}
