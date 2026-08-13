//! Wire-format envelope types and verified-result types.
//!
//! This is the consensus-frozen surface of the indexed-axis proof system:
//! the bincode schema of these types IS the proof wire format. Field order,
//! variant order and integer widths must never change once a version
//! carrying them activates; new shapes get new envelope types.

use bincode::{Decode, Encode};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::tree::CryptoHash;

/// Per-ancestor attestation for chaining the cidx/psit/pcpsit layer
/// composition during verification.
///
/// For each intermediate layer (i.e. every path segment shallower than
/// the queried indexed-tree element), the verifier needs to know what
/// hash composition the ancestor used so it can reconstruct the
/// `value_hash` recorded for that ancestor in its own parent merk.
///
/// Variants:
/// - [`Self::NotIndexed`] — the ancestor is a regular tree (or other
///   non-indexed element); the verifier chains via
///   `combine_hash(value_hash, child_root)`.
/// - [`Self::SingleSecondary`] — the ancestor is a `PCIT` or `PSIT`;
///   chain via `combine_hash_three(value_hash, child_root,
///   secondary_root)`.
/// - [`Self::MultiAxis`] — the ancestor is a `PCPSIT`; chain via
///   `combine_hash_three(value_hash, child_root, axes_digest(axes))`.
///   The carried list is the *canonical* axes list of the ancestor
///   (sorted by tag ascending, 1..=3 entries) with each tag mapped to
///   the secondary's root hash at proof time.
#[derive(Encode, Decode, Debug, Clone)]
pub enum AncestorAttestation {
    /// Regular tree ancestor.
    NotIndexed,
    /// Single-secondary indexed ancestor (PCIT or PSIT).
    SingleSecondary([u8; 32]),
    /// Multi-axis indexed ancestor (PCPSIT). The list is canonical: the
    /// same `(axis_tag, secondary_root_hash)` order the ancestor uses
    /// to compute its on-disk `axes_digest`.
    MultiAxis(Vec<(u8, [u8; 32])>),
}

/// Wire-format envelope for a range / top-k / arbitrary-query proof
/// over an indexed-tree's per-axis secondary index.
#[derive(Encode, Decode, Debug)]
pub struct IndexedAxisRangeProof {
    /// Echoed [`IndexAxis::tag`] of the queried axis. The verifier
    /// authenticates this against the caller's `expected_axis`.
    pub axis_tag: u8,
    /// Single-key Merk proof per path segment, top-down. The deepest
    /// entry proves the indexed-tree element's existence in its parent
    /// merk; shallower entries chain via the per-ancestor attestations.
    pub layer_proofs: Vec<Vec<u8>>,
    /// 32-byte attestation of the indexed-tree primary's root hash.
    /// Needed by the deepest-layer H1-A reconstruction.
    pub primary_root_hash: [u8; 32],
    /// Per-intermediate-layer ancestor attestation. Length =
    /// `layer_proofs.len() - 1`. See [`AncestorAttestation`].
    pub ancestor_attestations: Vec<AncestorAttestation>,
    /// **For PCPSIT only**: the canonical axes list of the queried
    /// indexed-tree element, EXCLUDING the queried axis. The verifier
    /// needs this to rebuild the deepest-layer `axes_digest` (the
    /// queried axis's root hash is re-derived from `secondary_proof`;
    /// the other axes' root hashes are carried here).
    ///
    /// For PCIT/PSIT this is empty (`Vec::new()`); the deepest layer
    /// composes via `combine_hash_three(value_hash, primary_root,
    /// secondary_root)` directly.
    ///
    /// Encoded canonically per the PCPSIT TLV rules (sorted by tag,
    /// no duplicates, 0..=2 entries — the queried axis is removed).
    pub other_axes_root_hashes: Vec<(u8, [u8; 32])>,
    /// Discriminator: `true` iff the queried target is a PCPSIT (the
    /// deepest-layer composition uses `axes_digest(...)` even when only
    /// the queried axis is in the TLV). For PCIT and PSIT this is
    /// `false` and the composition uses the single-secondary root hash
    /// directly.
    pub target_is_pcpsit: bool,
    /// Encoded Merk range proof for the per-axis secondary.
    pub secondary_proof: Vec<u8>,
    /// Echoed query limit (preserves `None`-vs-`Some(0)` semantics).
    pub requested_limit: Option<u16>,
    /// Echoed iteration direction. `false` = ascending, `true` =
    /// descending.
    pub descending: bool,
}

/// Wire-format envelope for an offset-paginated top-k proof over an
/// indexed-tree's per-axis secondary.
///
/// Every axis's secondary binds a count aggregate into its node hashes
/// (count axis: `ProvableCountTree`; sum and avg axes: dual-axis
/// `ProvableCountProvableSumTree`), so the secondary proof is always
/// produced by `Merk::prove_count_offset_on_range`: the skipped prefix
/// is attested by counted subtree commitments (`HashWithCount` /
/// `HashWithCountAndSum`), giving `O(log n + k)` proof size regardless
/// of `offset`.
#[derive(Encode, Decode, Debug)]
pub struct IndexedAxisPaginatedProof {
    /// Echoed [`IndexAxis::tag`] of the queried axis. The verifier
    /// authenticates this against the caller's `expected_axis`.
    pub axis_tag: u8,
    /// Same shape as [`IndexedAxisRangeProof::layer_proofs`].
    pub layer_proofs: Vec<Vec<u8>>,
    /// Same as [`IndexedAxisRangeProof::primary_root_hash`].
    pub primary_root_hash: [u8; 32],
    /// Same as [`IndexedAxisRangeProof::ancestor_attestations`].
    pub ancestor_attestations: Vec<AncestorAttestation>,
    /// Same as [`IndexedAxisRangeProof::other_axes_root_hashes`].
    pub other_axes_root_hashes: Vec<(u8, [u8; 32])>,
    /// Same as [`IndexedAxisRangeProof::target_is_pcpsit`].
    pub target_is_pcpsit: bool,
    /// Encoded paginated proof bytes for the per-axis secondary: the
    /// `prove_count_offset_on_range`-produced `Vec<Op>` stream (every
    /// axis's secondary carries a provable count).
    pub secondary_proof: Vec<u8>,
    /// Echoed pagination parameters.
    pub requested_k: u16,
    /// Echoed offset.
    pub requested_offset: u64,
    /// Echoed iteration direction.
    pub descending: bool,
}

/// Wire-format envelope for an aggregate proof (count axis: count,
/// sum axis: signed sum) over a value-range against the per-axis
/// secondary. The avg axis has no aggregate variant — averaging
/// averages is not closed-form.
#[derive(Encode, Decode, Debug)]
pub struct IndexedAxisAggregateProof {
    /// Echoed [`IndexAxis::tag`] of the queried axis. The verifier
    /// authenticates this against the caller's `expected_axis`. Must
    /// be [`IndexAxis::Count`] or [`IndexAxis::Sum`].
    pub axis_tag: u8,
    /// Same shape as [`IndexedAxisRangeProof::layer_proofs`].
    pub layer_proofs: Vec<Vec<u8>>,
    /// Same as [`IndexedAxisRangeProof::primary_root_hash`].
    pub primary_root_hash: [u8; 32],
    /// Same as [`IndexedAxisRangeProof::ancestor_attestations`].
    pub ancestor_attestations: Vec<AncestorAttestation>,
    /// Same as [`IndexedAxisRangeProof::other_axes_root_hashes`].
    pub other_axes_root_hashes: Vec<(u8, [u8; 32])>,
    /// Same as [`IndexedAxisRangeProof::target_is_pcpsit`].
    pub target_is_pcpsit: bool,
    /// Encoded aggregate proof bytes for the per-axis secondary.
    /// For count axis: `prove_aggregate_count_on_range` output.
    /// For sum axis: `prove_aggregate_sum_on_range` output.
    pub secondary_proof: Vec<u8>,
    /// Echoed inclusive lower bound on the secondary's sort-value
    /// (i.e. `count_value` for count axis, `sum_value` for sum axis).
    /// Stored as i128 to capture the union of u64/i64.
    pub lo: i128,
    /// Echoed inclusive upper bound.
    pub hi: i128,
}

/// Sort-value variants per axis. Returned in
/// [`IndexedAxisQueryResult::entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxisEntries {
    /// Count-axis entries: `(count_value, original_key)`.
    Count(Vec<(u64, Vec<u8>)>),
    /// Sum-axis entries: `(sum_value, original_key)`.
    Sum(Vec<(i64, Vec<u8>)>),
    /// Avg-axis entries: `(avg_fixed_point_i128, original_key)`.
    Avg(Vec<(i128, Vec<u8>)>),
}

impl AxisEntries {
    /// Number of entries.
    pub fn len(&self) -> usize {
        match self {
            AxisEntries::Count(v) => v.len(),
            AxisEntries::Sum(v) => v.len(),
            AxisEntries::Avg(v) => v.len(),
        }
    }

    /// Whether the result list is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Verified result of a range / top-k / arbitrary-query proof.
#[derive(Debug)]
pub struct IndexedAxisQueryResult {
    /// GroveDB root hash this proof reconstructs.
    pub root_hash: CryptoHash,
    /// Per-axis decoded entries in the order they were proven.
    pub entries: AxisEntries,
}

/// Verified result of an offset-paginated proof.
#[derive(Debug)]
pub struct IndexedAxisPaginatedResult {
    /// GroveDB root hash this proof reconstructs.
    pub root_hash: CryptoHash,
    /// Per-axis decoded entries (after the `skipped` offset region).
    pub entries: AxisEntries,
    /// Number of secondary entries the proof committed as skipped,
    /// independently re-derived by the verifier from the counted
    /// subtree commitments (`HashWithCount` / `HashWithCountAndSum`)
    /// in the proof bytes — i.e. *cryptographically* attested for
    /// every axis.
    ///
    /// `skipped == requested_offset` unless the walk was exhausted
    /// first, in which case `skipped < requested_offset` and
    /// `entries` is empty — that shape is itself a proof that the
    /// secondary's total population is exactly `skipped` (the counted
    /// commitments cover the whole walk). Callers wanting strict
    /// "page exists" semantics should cross-check
    /// `skipped == expected_offset`.
    pub skipped: u64,
}

/// Verified result of an aggregate proof.
#[derive(Debug)]
pub struct IndexedAxisAggregateResult {
    /// GroveDB root hash this proof reconstructs.
    pub root_hash: CryptoHash,
    /// Echoed axis (Count or Sum).
    pub axis: IndexAxis,
    /// Cryptographically-committed aggregate value over `[lo, hi]`.
    /// For count axis this is a non-negative count cast to i128; for
    /// sum axis this is the signed sum.
    pub aggregate: i128,
}

/// One branch of a branched (multi-prefix) indexed-axis proof: the
/// layers *below* the branching level, plus the branch's own target
/// attestations and secondary proof. The layers above the branching
/// level — and the branching level itself — are shared across branches
/// and live once on the enclosing envelope.
#[derive(Encode, Decode, Debug, Clone)]
pub struct BranchedProofBranch {
    /// Per-intermediate attestations for the branch's own layers:
    /// entry 0 describes the branch's value-tree element at the
    /// branching level (proved by the shared multi-key layer proof);
    /// subsequent entries describe deeper intermediates. Length equals
    /// `tail_layer_proofs.len()`.
    pub ancestor_attestations: Vec<AncestorAttestation>,
    /// Single-key layer proofs for the segments below the branch key,
    /// top-down; the deepest proves the indexed-tree element. Length
    /// equals the path-suffix length.
    pub tail_layer_proofs: Vec<Vec<u8>>,
    /// Same as [`IndexedAxisRangeProof::primary_root_hash`], per branch.
    pub primary_root_hash: [u8; 32],
    /// Same as [`IndexedAxisRangeProof::other_axes_root_hashes`].
    pub other_axes_root_hashes: Vec<(u8, [u8; 32])>,
    /// Same as [`IndexedAxisRangeProof::target_is_pcpsit`].
    pub target_is_pcpsit: bool,
    /// The branch's secondary proof: a Merk range proof for the range
    /// shape, a `prove_count_offset_on_range` stream for the paginated
    /// shape.
    pub secondary_proof: Vec<u8>,
}

/// Wire-format envelope for a **branched** range / arbitrary-query
/// proof: one query over N sibling prefix branches, attested by a
/// single envelope. The layers above the branching level appear once;
/// the branching level is one multi-key Merk proof binding every
/// branch's value tree simultaneously; each branch carries only its
/// own tail. Exactly one GroveDB root hash is reconstructed.
#[derive(Encode, Decode, Debug)]
pub struct IndexedAxisBranchedRangeProof {
    /// Echoed [`IndexAxis::tag`] of the queried axis.
    pub axis_tag: u8,
    /// Single-key layer proofs for the shared path prefix, top-down.
    /// Length equals the path-prefix length.
    pub shared_layer_proofs: Vec<Vec<u8>>,
    /// Attestations for the shared intermediate layers. Length equals
    /// `shared_layer_proofs.len()`.
    pub shared_ancestor_attestations: Vec<AncestorAttestation>,
    /// One multi-key Merk proof at the branching level, proving every
    /// echoed branch key in the shared prefix's Merk.
    pub branching_layer_proof: Vec<u8>,
    /// Per-branch tails, aligned with the verifier's branch-key list.
    pub branches: Vec<BranchedProofBranch>,
    /// Echoed query limit, shared by every branch.
    pub requested_limit: Option<u16>,
    /// Echoed iteration direction, shared by every branch.
    pub descending: bool,
}

/// Wire-format envelope for a **branched** offset-paginated top-k
/// proof. Same layer structure as
/// [`IndexedAxisBranchedRangeProof`]; each branch's secondary proof is
/// a `prove_count_offset_on_range` stream.
#[derive(Encode, Decode, Debug)]
pub struct IndexedAxisBranchedPaginatedProof {
    /// Echoed [`IndexAxis::tag`] of the queried axis.
    pub axis_tag: u8,
    /// Same as [`IndexedAxisBranchedRangeProof::shared_layer_proofs`].
    pub shared_layer_proofs: Vec<Vec<u8>>,
    /// Same as
    /// [`IndexedAxisBranchedRangeProof::shared_ancestor_attestations`].
    pub shared_ancestor_attestations: Vec<AncestorAttestation>,
    /// Same as
    /// [`IndexedAxisBranchedRangeProof::branching_layer_proof`].
    pub branching_layer_proof: Vec<u8>,
    /// Per-branch tails, aligned with the verifier's branch-key list.
    pub branches: Vec<BranchedProofBranch>,
    /// Echoed page size, shared by every branch.
    pub requested_k: u16,
    /// Echoed offset, shared by every branch.
    pub requested_offset: u64,
    /// Echoed iteration direction, shared by every branch.
    pub descending: bool,
}

/// Verified result of a branched range / arbitrary query: one root
/// hash, per-branch entries aligned with the caller's branch keys.
#[derive(Debug)]
pub struct IndexedAxisBranchedQueryResult {
    /// GroveDB root hash the whole envelope reconstructs.
    pub root_hash: CryptoHash,
    /// Per-branch decoded entries, aligned with the caller's branch
    /// keys.
    pub branches: Vec<AxisEntries>,
}

/// Verified result of a branched paginated top-k query.
#[derive(Debug)]
pub struct IndexedAxisBranchedPaginatedResult {
    /// GroveDB root hash the whole envelope reconstructs.
    pub root_hash: CryptoHash,
    /// Per-branch `(skipped, entries)`, aligned with the caller's
    /// branch keys; `skipped` carries the same attested-skip semantics
    /// as [`IndexedAxisPaginatedResult::skipped`], per branch.
    pub branches: Vec<(u64, AxisEntries)>,
}
