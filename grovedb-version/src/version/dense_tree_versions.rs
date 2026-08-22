//! Version gates for the dense fixed-sized Merkle tree crate.
//!
//! The dense tree is the buffer of the whole append-only family —
//! `BulkAppendTree`, and through it `CommitmentTree` (the shielded pool, live
//! on mainnet) and `PrivateDocumentStore` — as well as a tree type of its own
//! (`Element::DenseAppendOnlyFixedSizeTree`). Unlike the sibling crates' gates,
//! the one here changes more than a reported figure: it selects *how the root
//! is maintained*, which decides what an insert reads, hashes and writes. The
//! root VALUE is identical under every version — the hashing scheme
//! (`blake3(H(value) || H(left) || H(right))`, positions filled in BFS order)
//! is untouched — so no committed root hash moves; what moves is the work an
//! insert performs and therefore the cost it is charged.

use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct DenseTreeVersions {
    /// How the tree derives its root after an insert and on a root read.
    ///
    /// Version 0 (GROVE_V1..V3) keeps no intermediate hashes: every insert
    /// re-derives the root from scratch by walking every filled position out
    /// of storage — one read and two blake3 calls per position — so the k-th
    /// insert of an epoch costs O(k), and a full `2^h - 1` buffer O(2^h) on
    /// its last insert. Reading the root costs the same walk.
    ///
    /// Version 1 (GROVE_V4+) persists a per-position hash record
    /// (`generation || value_hash || node_hash`, keyed `b'h' || position`)
    /// and updates only the inserted position and its ancestors: an insert
    /// reads at most two records per level (the parent's own record, for its
    /// value hash, and the off-path sibling's), writes one record per level,
    /// and hashes once per level plus twice for the leaf — O(h) for a tree of
    /// height `h` instead of O(count). The root is the record at position 0
    /// (one read). Records tagged with an older generation (an earlier epoch
    /// over the same slot keys) or absent altogether (a buffer filled under
    /// version 0) are not trusted: the sibling subtree is recomputed from its
    /// values, exactly as version 0 would, and its record is written so the
    /// next insert is O(h) again — a one-time catch-up that costs no more than
    /// the version-0 walk.
    ///
    /// Stored values, positions, proofs and root hashes are identical under
    /// both versions; what differs is the records written alongside the
    /// values, and the reads, hashes and storage the insert is charged.
    pub root_maintenance: FeatureVersion,
}
