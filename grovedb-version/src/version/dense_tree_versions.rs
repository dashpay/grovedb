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
    /// Version 1 (GROVE_V4+) writes ONE path record per insert, under the
    /// inserting position's key (`b'h' || position`): the position's value
    /// hash and the node hash of every position on its ancestor path
    /// (`generation || present mask || value_hash || one 32-byte entry per
    /// level`, a fixed size for the tree's height). An insert derives its
    /// ancestors' hashes from earlier inserts' records — the record of the
    /// last insert into a subtree holds that subtree's current hash, located
    /// arithmetically from `count` — reading at most two records per level
    /// and hashing once per level plus twice for the leaf; the root is the
    /// last insert's record. Every insert is CHARGED a fixed figure for the
    /// tree's height (the blake3 calls and record reads averaged over a full
    /// buffer, rounded up — `v1_insert_model_cost`), not the work of its
    /// particular position, plus its two puts (slot, record) of
    /// position-independent size: the cost of appending to a tree of a given
    /// height is the same whatever the position. Records tagged with an older
    /// generation (an earlier epoch over the same slot keys) or absent
    /// altogether (a buffer filled under version 0) are not trusted: the
    /// subtree is recomputed from its values, exactly as version 0 would —
    /// read-only, billed the same model, and over once the epoch that
    /// switched versions ends.
    ///
    /// Stored values, positions, proofs and root hashes are identical under
    /// both versions; what differs is the records written alongside the
    /// values, and the reads, hashes and storage the insert is charged.
    pub root_maintenance: FeatureVersion,
}
