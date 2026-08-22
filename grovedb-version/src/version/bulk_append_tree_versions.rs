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
    /// How the append-only family's data-storage writes are reported to the
    /// fee layer (the dense-buffer entry write, the compaction chunk blob,
    /// and — for `CommitmentTree` — the frontier rewrite).
    ///
    /// Version 0 (shipped, V1..V3) issues every data put with no cost info,
    /// so the commit path charges key + value as `added_bytes`: the chunk
    /// blob is billed as ~`epoch_size × entry` of NEW storage on the one
    /// append that compacts (although it supersedes the buffer entries it
    /// was built from), buffer writes from the second epoch on are billed
    /// as new (although they overwrite last epoch's stale value at the same
    /// position key), and the frontier is billed in full on every append
    /// (although it is one value rewritten in place). Metered storage ends
    /// up ~2× the bytes that persist, concentrated on one arbitrary tx per
    /// epoch.
    ///
    /// Version 1 (V4+) charges each entry's permanent bytes once, at the
    /// append that creates it (entry + its amortized share of the chunk
    /// blob's framing, as `added_bytes`), and reports the churn as
    /// `replaced_bytes`: the chunk blob as replacement of the buffer bytes
    /// it supersedes, the frontier rewrite as replacement of its previous
    /// serialization (growth only is added). Stored bytes, chunks, roots and
    /// proofs are identical under both versions; only the cost report
    /// moves.
    pub storage_accounting: FeatureVersion,
}
