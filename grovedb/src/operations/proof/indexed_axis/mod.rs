//! Unified, per-axis proof envelopes for `ProvableCountIndexedTree`
//! (PCIT), `ProvableSumIndexedTree` (PSIT) and
//! `ProvableCountProvableSumIndexedTree` (PCPSIT).
//!
//! ## RETIRED from the public API — `PathQuery` is the only proof surface
//!
//! The standalone prove/verify entry points in this module
//! (`prove_indexed_axis_*`, `verify_indexed_axis_*`, and the per-axis
//! `axis_api` wrappers) are **`#[cfg(test)]` test oracles**: they exist so
//! the in-crate suites can cross-check the unified V1-envelope axis proofs
//! against an independent implementation of the same engines (see
//! `tests/envelope_byte_equality_tests.rs` for the byte-level relationship).
//! External callers must use [`crate::PathQuery`]'s axis constructors with
//! `GroveDb::prove_query` / `GroveDb::verify_path_query`. The standalone
//! envelope wire format has never been emitted by a released version, and
//! retiring it before GROVE_V4 activates means it never becomes
//! consensus-frozen.
//!
//! The engine code both surfaces share (descent payload builders, the
//! secondary-proof builders, target-chain resolution, `axis_lowering`)
//! remains live — the unified surface is a thin envelope over it.
//!
//! This is the Phase-4 generalization of the Phase-2 `count_indexed`
//! envelope: instead of three per-axis families of types each shaped
//! identically, the wire format here carries an explicit
//! [`grovedb_element::indexed::IndexAxis`] tag and a per-ancestor
//! attestation enum that supports all three variants on the descent
//! path:
//!
//! - non-indexed ancestors chain via `combine_hash(value_hash, child_root)`,
//! - PCIT / PSIT ancestors chain via
//!   `combine_hash_three(value_hash, child_root, secondary_root)`,
//! - PCPSIT ancestors chain via
//!   `combine_hash_three(value_hash, child_root, axes_digest(...))` where
//!   `axes_digest` runs over the *canonical* axes list of that ancestor.
//!
//! See `count_indexed.rs` for the original count-only envelope shape.
//! That module's `CountIndexedRangeProof` / `CountIndexedPaginatedProof`
//! / `CountIndexedAggregateCountProof` types and their prove/verify
//! entry points remain in place, byte-identical, for legacy callers.
//! The new types live next to them (not on top of them) so the legacy
//! wire format is not perturbed.
//!
//! ## Wire shape
//!
//! Each envelope here carries:
//! - `axis_tag` — `IndexAxis::tag()` for the queried axis.
//! - `layer_proofs` — single-key Merk proofs per path segment, top-down.
//! - `primary_root_hash` — root hash of the cidx/psit/pcpsit primary at
//!   proof time.
//! - `ancestor_attestations` — one [`AncestorAttestation`] per
//!   intermediate layer (length = `layer_proofs.len() - 1`), telling
//!   the verifier how to chain the ancestor's recorded `value_hash`.
//! - `secondary_proof` — the axis-specific merk proof against the
//!   queried secondary.
//! - axis-specific echoed query parameters.
//!
//! ## Result shape
//!
//! [`IndexedAxisQueryResult`] / [`IndexedAxisPaginatedResult`] /
//! [`IndexedAxisAggregateResult`] each carry the reconstructed
//! `root_hash` and an axis-tagged result list / aggregate value.
//!
//! ## Module layout
//!
//! One file per concern, in trust order:
//!
//! - [`envelope`] — the wire-format types and result types. This is the
//!   consensus-frozen surface; everything else can evolve.
//! - [`generate`] — the prover: layer proofs, ancestor attestations, and
//!   the per-shape envelope builders.
//! - [`verify`] — the verifier: envelope decoding, the ancestor-chain
//!   walk, and the per-shape verification cores.
//! - [`axis_api`] — thin per-axis (count / sum / avg) wrappers over the
//!   generic prove/verify entry points.
//!
//! [`aggregate_range_out_of_domain`] lives here in `mod.rs` because both
//! sides must agree on it: the prover uses it to decide an aggregate
//! range is provably empty, and the verifier uses it to accept the empty
//! shape only when the range really is out of the axis's domain.

#[cfg(test)]
mod axis_api;
pub(crate) mod canonical_row;
mod envelope;
#[cfg(feature = "minimal")]
mod generate;
pub(crate) mod target_chain;
pub(crate) mod verify;

pub use envelope::{
    AncestorAttestation, AxisEntries, IndexedAxisAggregateProof, IndexedAxisAggregateResult,
    IndexedAxisPaginatedProof, IndexedAxisPaginatedResult, IndexedAxisQueryResult,
    IndexedAxisRangeProof, IndexedTargetChain, IndexedTargetCommitment, IndexedTargetNode,
};
use grovedb_element::indexed::IndexAxis;

/// True when the ordered-or-not aggregate value range `[lo, hi]` (i128)
/// has no overlap with the axis's representable domain — count is
/// `[0, u64::MAX]`, sum is `[i64::MIN, i64::MAX]`. Such a request must
/// commit an EMPTY aggregate (0) rather than clamping the bounds into
/// the domain, which would collapse an out-of-domain range onto a
/// boundary key and erroneously include the entries sitting exactly on
/// `u64::MAX` / `i64::MAX` / `i64::MIN`.
///
/// In-domain degenerate ranges (`lo > hi` with both bounds inside the
/// domain) are intentionally NOT covered here — they flow through the
/// existing clamped degenerate-range path, which emits a matching
/// empty-range shape on both the prover and verifier sides.
fn aggregate_range_out_of_domain(axis: IndexAxis, lo: i128, hi: i128) -> bool {
    match axis {
        IndexAxis::Count => hi < 0 || lo > u64::MAX as i128,
        IndexAxis::Sum => hi < (i64::MIN as i128) || lo > (i64::MAX as i128),
        // Avg has no aggregate form; rejected at the public entry point.
        IndexAxis::Avg => true,
    }
}
