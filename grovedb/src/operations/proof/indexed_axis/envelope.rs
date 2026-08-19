//! Wire-format envelope types and verified-result types.
//!
//! This is the consensus-frozen surface of the indexed-axis proof system:
//! the bincode schema of these types IS the proof wire format. Field order,
//! variant order and integer widths must never change once a version
//! carrying them activates; new shapes get new envelope types.

use bincode::{Decode, Encode};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::tree::CryptoHash;

use crate::IndexedAxisEntry;

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

/// How a node's serialized element bytes compose into the value hash its
/// parent Merk committed.
///
/// This is what makes a resolved indexed row shape-complete: the element
/// bytes alone determine the commitment only for item-like values, and
/// every other shape folds in something the bytes do not carry.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub enum IndexedTargetCommitment {
    /// Item-like value, committed as `H(value)`.
    Simple,
    /// Merk-backed or non-Merk tree, committed as
    /// `combine_hash(H(value), child_root_hash)`.
    Layered([u8; 32]),
    /// PCIT / PSIT, committed as
    /// `combine_hash_three(H(value), primary_root, secondary_root)`.
    IndexedSingle {
        /// Root hash of the indexed tree's primary Merk.
        primary_root_hash: [u8; 32],
        /// Root hash of its only secondary Merk.
        secondary_root_hash: [u8; 32],
    },
    /// PCPSIT, committed as
    /// `combine_hash_three(H(value), primary_root, axes_digest(axes))`.
    IndexedMulti {
        /// Root hash of the indexed tree's primary Merk.
        primary_root_hash: [u8; 32],
        /// Canonical `(axis_tag, secondary_root_hash)` list, tag-sorted.
        axes: Vec<(u8, [u8; 32])>,
    },
    /// Reference node, committed as
    /// `combine_hash(H(value), next_node_commitment)` — the next hop's
    /// commitment is reconstructed from the following chain entry rather
    /// than carried, which is what keeps a chain self-authenticating.
    Reference,
}

/// One node of a resolved target chain: its serialized element bytes and
/// the shape rule that turns them into a commitment.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct IndexedTargetNode {
    /// The node's serialized element bytes.
    pub value: Vec<u8>,
    /// How those bytes compose into the value hash its parent committed.
    pub commitment: IndexedTargetCommitment,
}

/// The resolved target of one secondary row: the immediate primary node,
/// followed by any ordinary reference hops through to the terminal.
///
/// **No per-node path proofs.** A chain authenticates itself from the
/// row's own committed value hash: each entry's commitment is
/// reconstructed from its bytes plus the NEXT entry's commitment, and the
/// head's commitment is what the row binds. Since the row's hash is bound
/// into the secondary root — and that into the indexed element, and that
/// to the grove root — substituting any value in the chain breaks the
/// root.
///
/// That is the same trust model shipped GroveDB reference proofs already
/// use (`KVRefValueHash*` binds a reference's committed target hash to the
/// returned value without separately proving the target's path
/// inclusion), so a chain is neither weaker nor stronger than reading the
/// same reference through an ordinary proof. It is what lets a top-k
/// result carry `k` values for a per-row cost of the value plus a hash,
/// instead of `k` inclusion proofs.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct IndexedTargetChain {
    /// Immediate primary node first, terminal last. Never empty.
    pub nodes: Vec<IndexedTargetNode>,
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
    /// One resolved-target chain per returned secondary row, in the
    /// secondary proof's result order.
    pub target_chains: Vec<IndexedTargetChain>,
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
/// (every axis is a dual-aggregate `ProvableCountProvableSumTree`), so
/// the secondary proof is always produced by
/// `Merk::prove_count_offset_on_range`: the skipped prefix is attested
/// by counted subtree commitments (`HashWithCountAndSum`), giving
/// `O(log n + k)` proof size regardless of `offset`.
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
    /// Same as [`IndexedAxisRangeProof::target_chains`].
    pub target_chains: Vec<IndexedTargetChain>,
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
    /// The walker follows the FOLD, not the axis:
    /// `AggregateFold::Population` → `prove_aggregate_count_on_range`
    /// output; `AggregateFold::Total` → `prove_aggregate_sum_on_range`
    /// output. The byte range the proof covers follows the axis.
    pub secondary_proof: Vec<u8>,
    /// Echoed inclusive lower bound on the secondary's sort-value
    /// (i.e. `count_value` for count axis, `sum_value` for sum axis).
    /// Stored as i128 to capture the union of u64/i64.
    pub lo: i128,
    /// Echoed inclusive upper bound.
    pub hi: i128,
    /// Echoed [`AggregateFold::tag`](grovedb_query::AggregateFold::tag)
    /// of the fold the proof answers. The verifier authenticates this
    /// against the caller's `expected_fold` — a population proof must
    /// not satisfy a question about a total, or vice versa.
    pub fold_tag: u8,
}

/// Sort-value variants per axis. Returned in
/// [`IndexedAxisQueryResult::entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxisEntries {
    /// Count-axis entries, ordered by count.
    Count(Vec<IndexedAxisEntry<u64>>),
    /// Sum-axis entries, ordered by sum.
    Sum(Vec<IndexedAxisEntry<i64>>),
    /// Avg-axis entries, ordered by fixed-point average.
    Avg(Vec<IndexedAxisEntry<i128>>),
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
    /// An empty entry list of the right variant for `axis`.
    pub fn empty_for_axis(axis: grovedb_element::indexed::IndexAxis) -> Self {
        match axis {
            grovedb_element::indexed::IndexAxis::Count => AxisEntries::Count(Vec::new()),
            grovedb_element::indexed::IndexAxis::Sum => AxisEntries::Sum(Vec::new()),
            grovedb_element::indexed::IndexAxis::Avg => AxisEntries::Avg(Vec::new()),
        }
    }

    /// The first entry's original (primary) key, if any — the yielded
    /// item of a `k = 1` page, used by rank verification.
    pub fn first_original_key(&self) -> Option<&[u8]> {
        match self {
            AxisEntries::Count(entries) => entries.first().map(|e| e.primary_key.as_slice()),
            AxisEntries::Sum(entries) => entries.first().map(|e| e.primary_key.as_slice()),
            AxisEntries::Avg(entries) => entries.first().map(|e| e.primary_key.as_slice()),
        }
    }

    /// Whether there are no entries.
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
