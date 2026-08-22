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
    /// Version 1 reports nothing for the compaction itself: its hashes (the
    /// chunk-leaf hash, the push merges, the root bagging) are amortized
    /// over the epoch as one blake3 on every append
    /// (`AMORTIZED_COMPACTION_HASHES`), so a compacting append is charged
    /// what any other append is. Shipped chunk bytes and roots are
    /// unaffected; what moves is where the fee lands.
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
    /// Version 1 charges every append the FIXED per-append model, whatever
    /// its position: the entry's long-term footprint as added storage — its
    /// chunk-blob share (`value.len()`) plus the epoch's share of the blob
    /// framing and MMR nodes (`amortized_compaction_added_bytes`) — and
    /// churn as replaced storage — its buffer slot and path record (epoch 1
    /// included, nothing read to size them; the buffer is a fixed-size
    /// per-tree scratch area rewritten every epoch) and its own bytes again
    /// as its part of the blob rewrite; plus the buffer's fixed
    /// root-maintenance model (`dense_tree_versions.root_maintenance`) and
    /// one amortized compaction blake3. The compacting append writes the
    /// blob and the MMR nodes prepaid (zero-byte cost information) and is
    /// charged the slot / record churn it does not write, so its figure is
    /// the same; it also persists the MMR root (key `r`, 32 bytes, prepaid)
    /// so a reopened tree reads it instead of bagging the peaks' blobs, and
    /// the state root's two root reads are charged as the model. A tree
    /// whose last compaction predates version 1 has no such key: its first
    /// version-1 append bags the peaks once and backfills the key (one
    /// prepaid put, not billed — like the dense buffer's read-only
    /// catch-up). Stored chunks and roots are identical under both
    /// versions; version 1 adds the persisted root key.
    pub append_storage_accounting: FeatureVersion,
}
