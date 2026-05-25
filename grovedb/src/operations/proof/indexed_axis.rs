//! Unified, per-axis proof envelopes for `ProvableCountIndexedTree`
//! (PCIT), `ProvableSumIndexedTree` (PSIT) and
//! `ProvableCountProvableSumIndexedTree` (PCPSIT).
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

use bincode::{Decode, Encode};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::{
    decode_count_sort_key, decode_sum_sort_key, encode_count_sort_key, encode_sum_sort_key,
    IndexAxis,
};
use grovedb_merk::{
    element::get::ElementFetchFromStorageExtensions,
    proofs::{
        encode_into,
        query::{
            verify_aggregate_count_on_range_proof, verify_aggregate_sum_on_range_proof,
            verify_count_offset_on_range_proof, QueryItem as MerkQueryItemForRange,
            QueryProofVerify,
        },
        Query as MerkQuery,
    },
    tree::{axes_digest, combine_hash, combine_hash_three, value_hash, CryptoHash},
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_query::QueryItem as MerkQueryItem;
use grovedb_storage::StorageBatch;
use grovedb_version::version::GroveVersion;

use crate::{util::TxRef, Element, Error, GroveDb, Transaction, TransactionArg};

// ===================================================================
// Wire-format envelope types
// ===================================================================

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
/// For count and avg axes (`ProvableCountTree` / dual-axis
/// `ProvableCountProvableSumTree` secondaries) the secondary proof is
/// produced by `Merk::prove_count_offset_on_range`, giving
/// `O(log n + k)` proof size regardless of `offset`. For the sum axis
/// (`ProvableSumTree` secondary) there is no count-bound offset
/// primitive, so the prover instead emits a regular range proof with
/// `limit = offset + k` and the verifier discards the first `offset`
/// items independently. The `axis_tag` field disambiguates the two
/// shapes.
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
    /// Encoded paginated proof bytes for the per-axis secondary.
    ///
    /// For count/avg axes this is the
    /// `prove_count_offset_on_range`-produced `Vec<Op>` stream. For
    /// the sum axis this is a regular `Merk::prove`-produced range
    /// proof bound by `limit = offset + k`.
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

// ===================================================================
// Verified-result types
// ===================================================================

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
    /// Number of secondary entries the proof committed as skipped.
    /// For count/avg axes this is independently re-derived by the
    /// verifier from `HashWithCount` commitments in the proof bytes
    /// (i.e. *cryptographically* committed). For the sum axis this
    /// is the verifier-side count of items returned by the regular
    /// range proof up to the `offset` cutoff — also independently
    /// derived from the proof bytes, but constrained only by the
    /// merk's regular range-walk discipline (NOT a count
    /// commitment). The caller must cross-check
    /// `skipped == expected_offset` if exact-page semantics matter.
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

// ===================================================================
// Builder / verifier helpers (private)
// ===================================================================

/// Build the per-ancestor attestation list for a path of length N: the
/// list has length N-1 (one entry per intermediate layer). For each
/// intermediate layer, open the parent merk and inspect the element
/// at the depth's key to determine the chain composition.
fn build_ancestor_attestations<'db>(
    grovedb: &'db GroveDb,
    path_keys: &[Vec<u8>],
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    grove_version: &GroveVersion,
    err_label: &'static str,
) -> CostResult<Vec<AncestorAttestation>, Error> {
    let mut cost = OperationCost::default();
    let last_idx = path_keys.len() - 1;
    let mut atts: Vec<AncestorAttestation> = Vec::with_capacity(last_idx);
    for depth in 0..last_idx {
        // The parent merk at path_keys[..depth] holds the element with key
        // path_keys[depth]; we examine the element to decide the chain.
        let parent_slices: Vec<&[u8]> = path_keys[..depth].iter().map(|p| p.as_slice()).collect();
        let parent_path: SubtreePath<&[u8]> = parent_slices.as_slice().into();
        let parent_merk = cost_return_on_error!(
            &mut cost,
            grovedb.open_transactional_merk_at_path(
                parent_path,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let intermediate = cost_return_on_error!(
            &mut cost,
            Element::get(
                &parent_merk,
                path_keys[depth].as_slice(),
                true,
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "{err_label}: fetch intermediate-layer element at depth {depth}: {e}"
                ))
            })
        );
        let att = match intermediate.underlying() {
            Element::ProvableCountIndexedTree(_, secondary_root_key, ..)
            | Element::ProvableSumIndexedTree(_, secondary_root_key, ..) => {
                let axis = match intermediate.underlying() {
                    Element::ProvableCountIndexedTree(..) => IndexAxis::Count,
                    Element::ProvableSumIndexedTree(..) => IndexAxis::Sum,
                    _ => unreachable!(),
                };
                let ancestor_path_owned: SubtreePathBuilder<Vec<u8>> =
                    SubtreePathBuilder::owned_from_iter(path_keys[..=depth].iter().cloned());
                let ancestor_path = SubtreePath::from(&ancestor_path_owned);
                let ancestor_secondary = cost_return_on_error!(
                    &mut cost,
                    grovedb.open_indexed_secondary_at_path(
                        ancestor_path,
                        axis,
                        secondary_root_key.clone(),
                        transaction,
                        Some(batch),
                        grove_version,
                    )
                );
                let (sec_hash, _, _) = cost_return_on_error!(
                    &mut cost,
                    ancestor_secondary
                        .root_hash_key_and_aggregate_data()
                        .map_err(|e| Error::CorruptedData(format!(
                            "{err_label}: ancestor secondary root hash at depth {depth}: {e}"
                        )))
                );
                AncestorAttestation::SingleSecondary(sec_hash)
            }
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => {
                let ancestor_path_owned: SubtreePathBuilder<Vec<u8>> =
                    SubtreePathBuilder::owned_from_iter(path_keys[..=depth].iter().cloned());
                let ancestor_path = SubtreePath::from(&ancestor_path_owned);
                let mut axis_hashes: Vec<(u8, [u8; 32])> = Vec::with_capacity(axes.len());
                for (tag, sec_root_key) in axes {
                    let axis = cost_return_on_error_no_add!(
                        cost,
                        IndexAxis::try_from_tag(*tag).map_err(|e| Error::CorruptedData(format!(
                            "{err_label}: invalid axis tag in PCPSIT ancestor at depth {depth}: {e}"
                        )))
                    );
                    let ancestor_path_clone = ancestor_path.clone();
                    let ancestor_secondary = cost_return_on_error!(
                        &mut cost,
                        grovedb.open_indexed_secondary_at_path(
                            ancestor_path_clone,
                            axis,
                            sec_root_key.clone(),
                            transaction,
                            Some(batch),
                            grove_version,
                        )
                    );
                    let (sec_hash, _, _) = cost_return_on_error!(
                        &mut cost,
                        ancestor_secondary
                            .root_hash_key_and_aggregate_data()
                            .map_err(|e| Error::CorruptedData(format!(
                                "{err_label}: PCPSIT ancestor secondary root hash at depth \
                                 {depth} axis {:?}: {e}",
                                axis
                            )))
                    );
                    axis_hashes.push((*tag, sec_hash));
                }
                AncestorAttestation::MultiAxis(axis_hashes)
            }
            _ => AncestorAttestation::NotIndexed,
        };
        atts.push(att);
    }
    Ok(atts).wrap_with_cost(cost)
}

/// Build single-key Merk proofs per layer, top-down. `layer_proofs[i]`
/// proves the existence of `path_keys[i]` in the Merk at
/// `path_keys[..i]`.
fn build_layer_proofs<'db>(
    grovedb: &'db GroveDb,
    path_keys: &[Vec<u8>],
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    grove_version: &GroveVersion,
    err_label: &'static str,
) -> CostResult<Vec<Vec<u8>>, Error> {
    let mut cost = OperationCost::default();
    let mut layer_proofs: Vec<Vec<u8>> = Vec::with_capacity(path_keys.len());
    for depth in 0..path_keys.len() {
        let parent_slices: Vec<&[u8]> = path_keys[..depth].iter().map(|p| p.as_slice()).collect();
        let parent_path: SubtreePath<&[u8]> = parent_slices.as_slice().into();
        let parent_merk = cost_return_on_error!(
            &mut cost,
            grovedb.open_transactional_merk_at_path(
                parent_path,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let key = path_keys[depth].clone();
        let mut q = MerkQuery::new();
        q.insert_item(MerkQueryItem::Key(key));
        let result = cost_return_on_error!(
            &mut cost,
            parent_merk
                .prove(q, None, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "{err_label}: prove single-key at layer depth {depth}: {e}"
                )))
        );
        layer_proofs.push(result.proof);
    }
    Ok(layer_proofs).wrap_with_cost(cost)
}

/// Path-keys-driven variant of `read_queried_axis_info`. For PCPSIT, also
/// opens each non-queried axis's secondary to capture its root hash.
///
/// Returns `(secondary_root_key, other_axes_root_hashes, target_is_pcpsit)`.
fn read_queried_axis_info_with_path_keys<'db>(
    grovedb: &'db GroveDb,
    path_keys: &[Vec<u8>],
    axis: IndexAxis,
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    grove_version: &GroveVersion,
    err_label: &'static str,
) -> CostResult<(Option<Vec<u8>>, Vec<(u8, [u8; 32])>, bool), Error> {
    let mut cost = OperationCost::default();
    if path_keys.is_empty() {
        return Err(Error::InvalidPath(format!(
            "{err_label}: cannot query an indexed tree at the root path"
        )))
        .wrap_with_cost(cost);
    }
    let last_idx = path_keys.len() - 1;
    let parent_slices: Vec<&[u8]> = path_keys[..last_idx].iter().map(|p| p.as_slice()).collect();
    let parent_path: SubtreePath<&[u8]> = parent_slices.as_slice().into();
    let parent_merk = cost_return_on_error!(
        &mut cost,
        grovedb.open_transactional_merk_at_path(
            parent_path,
            transaction,
            Some(batch),
            grove_version,
        )
    );
    let element = cost_return_on_error!(
        &mut cost,
        Element::get(
            &parent_merk,
            path_keys[last_idx].as_slice(),
            true,
            grove_version,
        )
        .map_err(|e| {
            Error::CorruptedData(format!(
                "{err_label}: fetch indexed-tree element from parent merk: {e}"
            ))
        })
    );
    match (axis, element.underlying()) {
        (IndexAxis::Count, Element::ProvableCountIndexedTree(_, secondary, ..)) => {
            Ok((secondary.clone(), Vec::new(), false)).wrap_with_cost(cost)
        }
        (IndexAxis::Sum, Element::ProvableSumIndexedTree(_, secondary, ..)) => {
            Ok((secondary.clone(), Vec::new(), false)).wrap_with_cost(cost)
        }
        (_, Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _)) => {
            let want_tag = axis.tag();
            let mut queried_secondary: Option<Option<Vec<u8>>> = None;
            let mut other: Vec<(u8, [u8; 32])> = Vec::new();
            // Path to the PCPSIT element (the queried path's full chain).
            let pcpsit_path_owned: SubtreePathBuilder<Vec<u8>> =
                SubtreePathBuilder::owned_from_iter(path_keys.iter().cloned());
            for (tag, sec_root_key) in axes {
                let parsed_axis = cost_return_on_error_no_add!(
                    cost,
                    IndexAxis::try_from_tag(*tag).map_err(|e| Error::CorruptedData(format!(
                        "{err_label}: invalid axis tag in queried PCPSIT element: {e}"
                    )))
                );
                if *tag == want_tag {
                    queried_secondary = Some(sec_root_key.clone());
                    continue;
                }
                let secondary_path = SubtreePath::from(&pcpsit_path_owned);
                let other_secondary = cost_return_on_error!(
                    &mut cost,
                    grovedb.open_indexed_secondary_at_path(
                        secondary_path,
                        parsed_axis,
                        sec_root_key.clone(),
                        transaction,
                        Some(batch),
                        grove_version,
                    )
                );
                let (sec_hash, _, _) = cost_return_on_error!(
                    &mut cost,
                    other_secondary
                        .root_hash_key_and_aggregate_data()
                        .map_err(|e| Error::CorruptedData(format!(
                            "{err_label}: PCPSIT non-queried axis {:?} secondary root hash: {e}",
                            parsed_axis
                        )))
                );
                other.push((*tag, sec_hash));
            }
            match queried_secondary {
                Some(key) => Ok((key, other, true)).wrap_with_cost(cost),
                None => Err(Error::InvalidPath(format!(
                    "{:?} axis not indexed at this path",
                    axis
                )))
                .wrap_with_cost(cost),
            }
        }
        _ => Err(Error::InvalidPath(format!(
            "{:?} axis not indexed at this path",
            axis
        )))
        .wrap_with_cost(cost),
    }
}

/// Walk the verifier-side ancestor chain (depths `last_idx - 1` down to
/// `0`) and return the final reconstructed root hash. Returns the
/// outer GroveDB root hash on success.
fn walk_ancestor_chain(
    layer_proofs: &[Vec<u8>],
    ancestor_attestations: &[AncestorAttestation],
    path: &[&[u8]],
    initial_root: CryptoHash,
    err_label: &'static str,
) -> Result<CryptoHash, Error> {
    let last_idx = layer_proofs.len() - 1;
    if ancestor_attestations.len() != last_idx {
        return Err(Error::CorruptedData(format!(
            "{err_label}: ancestor_attestations has length {} but expected {}",
            ancestor_attestations.len(),
            last_idx
        )));
    }
    let mut current_layer_root = initial_root;
    for depth in (0..last_idx).rev() {
        let key = path[depth];
        let (value_bytes, layer_root, recorded_value_hash) =
            execute_single_key_proof(&layer_proofs[depth], key, err_label)?;
        let val_h = value_hash(&value_bytes).value().to_owned();
        let combined = match &ancestor_attestations[depth] {
            AncestorAttestation::NotIndexed => {
                combine_hash(&val_h, &current_layer_root).value().to_owned()
            }
            AncestorAttestation::SingleSecondary(ancestor_secondary_root) => {
                combine_hash_three(&val_h, &current_layer_root, ancestor_secondary_root)
                    .value()
                    .to_owned()
            }
            AncestorAttestation::MultiAxis(axes) => {
                // Compute axes_digest from the carried axes list. The
                // verifier does NOT trust the prover's axes list
                // structurally — it's bound to `recorded_value_hash`
                // via the chain check; any malformation here fails the
                // chain check below.
                let axes_digest_value = axes_digest(axes).value().to_owned();
                combine_hash_three(&val_h, &current_layer_root, &axes_digest_value)
                    .value()
                    .to_owned()
            }
        };
        if combined != recorded_value_hash {
            let chain_kind = match &ancestor_attestations[depth] {
                AncestorAttestation::NotIndexed => "combine_hash(H(value), child_root)",
                AncestorAttestation::SingleSecondary(_) => {
                    "combine_hash_three(H(value), child_root, secondary_root)"
                }
                AncestorAttestation::MultiAxis(_) => {
                    "combine_hash_three(H(value), child_root, axes_digest)"
                }
            };
            return Err(Error::CorruptedData(format!(
                "{err_label}: intermediate layer at depth {depth} chain mismatch: parent recorded \
                 value_hash {} but {} is {}",
                hex::encode(recorded_value_hash),
                chain_kind,
                hex::encode(combined)
            )));
        }
        current_layer_root = layer_root;
    }
    Ok(current_layer_root)
}

/// Reconstruct the deepest-layer composition: the queried indexed-tree
/// element's recorded `value_hash` should equal `combine_hash_three`
/// of `(value_hash(cidx_bytes), primary_root, queried_axis_digest)`,
/// where `queried_axis_digest` is the secondary's root hash for
/// PCIT/PSIT, or `axes_digest(...)` (rebuilt from
/// `other_axes_root_hashes` plus the queried axis's actual root hash)
/// for PCPSIT.
///
/// Returns `(initial_layer_root, cidx_value_bytes)`. The
/// `initial_layer_root` is then passed to `walk_ancestor_chain` as the
/// starting `current_layer_root` for the ancestor walk.
fn verify_deepest_layer(
    layer_proofs: &[Vec<u8>],
    path: &[&[u8]],
    primary_root_hash: &[u8; 32],
    secondary_root_hash: &[u8; 32],
    axis: IndexAxis,
    other_axes_root_hashes: &[(u8, [u8; 32])],
    target_is_pcpsit: bool,
    err_label: &'static str,
) -> Result<CryptoHash, Error> {
    let last_idx = layer_proofs.len() - 1;
    let cidx_key = path[last_idx];
    let (cidx_value_bytes, layer_root, cidx_value_hash_recorded) =
        execute_single_key_proof(&layer_proofs[last_idx], cidx_key, err_label)?;
    let actual_value_hash = value_hash(&cidx_value_bytes).value().to_owned();

    // Bind the proved element's family to the requested axis.
    //
    // Without this, the H1-A chain check below passes for a *relabeled*
    // proof: PCIT and PSIT both record
    // `combine_hash_three(H(value), primary_root, secondary_root)` — the
    // identical 3-input shape — so a PCIT count proof verified with
    // `axis = Sum, target_is_pcpsit = false` reconstructs the same hash
    // and "verifies", after which range/top-k/query decoding interprets
    // the count secondary keys (`count_be ‖ key`) as sum keys
    // (`sum_sortable_be ‖ key`) and returns garbage sum values under the
    // authentic root hash.
    //
    // We read just the element discriminant (no `grove_version` needed)
    // and normalize through any `NonCounted` wrapper via `base()`. For
    // PCPSIT the per-axis `axes_digest` reconstruction already
    // cryptographically binds each axis tag to its secondary root hash
    // (a relabeled axis or a non-configured axis produces a different
    // digest and fails the chain check below), so the family check is
    // all that is additionally required.
    {
        let proved_family = grovedb_element::ElementType::from_serialized_value(&cidx_value_bytes)
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "{err_label}: cannot determine element type of proved value: {e}"
                ))
            })?
            .base();
        let expected_family = if target_is_pcpsit {
            grovedb_element::ElementType::ProvableCountProvableSumIndexedTree
        } else {
            match axis {
                IndexAxis::Count => grovedb_element::ElementType::ProvableCountIndexedTree,
                IndexAxis::Sum => grovedb_element::ElementType::ProvableSumIndexedTree,
                IndexAxis::Avg => {
                    return Err(Error::CorruptedData(format!(
                        "{err_label}: Avg axis is only valid on a \
                         ProvableCountProvableSumIndexedTree (target_is_pcpsit=true); the \
                         envelope claims a single-axis (PCIT/PSIT) target"
                    )));
                }
            }
        };
        if proved_family != expected_family {
            return Err(Error::CorruptedData(format!(
                "{err_label}: proved element family {proved_family:?} does not match the \
                 requested axis {axis:?} (expected {expected_family:?}); possible \
                 axis-relabel forgery"
            )));
        }
    }

    let queried_axis_digest = if target_is_pcpsit {
        // PCPSIT: rebuild the canonical axes list by combining
        // `other_axes_root_hashes` with the queried axis's actual root
        // hash, then compute axes_digest. This applies even when the
        // PCPSIT's TLV holds only one axis — the element's on-disk
        // hash binds `axes_digest(...)`, not a raw secondary root hash.
        let mut combined: Vec<(u8, [u8; 32])> =
            Vec::with_capacity(other_axes_root_hashes.len() + 1);
        combined.extend_from_slice(other_axes_root_hashes);
        combined.push((axis.tag(), *secondary_root_hash));
        combined.sort_by_key(|(t, _)| *t);
        // Reject duplicate tags (would be a corrupt envelope).
        let mut prev_tag: Option<u8> = None;
        for (t, _) in &combined {
            if let Some(p) = prev_tag
                && *t <= p
            {
                return Err(Error::CorruptedData(format!(
                    "{err_label}: duplicate or unsorted axis tag in PCPSIT envelope"
                )));
            }
            prev_tag = Some(*t);
        }
        axes_digest(&combined).value().to_owned()
    } else {
        // PCIT or PSIT: the queried axis IS the only secondary; the
        // ancestor element's third-input slot is the secondary's root.
        if !other_axes_root_hashes.is_empty() {
            return Err(Error::CorruptedData(format!(
                "{err_label}: non-PCPSIT envelope must not carry other_axes_root_hashes"
            )));
        }
        *secondary_root_hash
    };

    let combined = combine_hash_three(&actual_value_hash, primary_root_hash, &queried_axis_digest)
        .value()
        .to_owned();
    if combined != cidx_value_hash_recorded {
        return Err(Error::CorruptedData(format!(
            "{err_label}: deepest-layer chain mismatch — parent recorded value_hash {} but \
             combine_hash_three(H(value), primary_root, axis_digest) is {}",
            hex::encode(cidx_value_hash_recorded),
            hex::encode(combined)
        )));
    }
    Ok(layer_root)
}

/// Verify a single-key Merk proof: returns
/// `(value_bytes, layer_root_hash, parent_recorded_value_hash)`.
fn execute_single_key_proof(
    proof_bytes: &[u8],
    target_key: &[u8],
    layer_label: &'static str,
) -> Result<(Vec<u8>, CryptoHash, CryptoHash), Error> {
    let mut query = MerkQuery::new();
    query.insert_item(MerkQueryItem::Key(target_key.to_vec()));
    let (root_hash, result) = query
        .execute_proof(proof_bytes, None, true, 0)
        .unwrap()
        .map_err(|e| {
            Error::CorruptedData(format!(
                "{layer_label} single-key proof for {} failed to verify: {e}",
                hex::encode(target_key)
            ))
        })?;
    let proved = result
        .result_set
        .iter()
        .find(|p| p.key == target_key)
        .ok_or_else(|| {
            Error::CorruptedData(format!(
                "{layer_label} proof did not contain expected key {}",
                hex::encode(target_key)
            ))
        })?;
    let value = proved.value.clone().ok_or_else(|| {
        Error::CorruptedData(format!(
            "{layer_label} proof for key {} returned no value bytes",
            hex::encode(target_key)
        ))
    })?;
    Ok((value, root_hash, proved.proof))
}

// ===================================================================
// Public prove/verify API
// ===================================================================

impl GroveDb {
    /// Generate a proof for the top-`k` entries of an indexed-tree on a
    /// specific [`IndexAxis`].
    ///
    /// The path's last segment must point to a variant that supports
    /// the requested axis:
    /// - [`IndexAxis::Count`] supports
    ///   [`Element::ProvableCountIndexedTree`] (PCIT) or
    ///   [`Element::ProvableCountProvableSumIndexedTree`] (PCPSIT) iff
    ///   the count axis is in the PCPSIT's TLV.
    /// - [`IndexAxis::Sum`] supports
    ///   [`Element::ProvableSumIndexedTree`] (PSIT) or PCPSIT iff the
    ///   sum axis is in the TLV.
    /// - [`IndexAxis::Avg`] supports PCPSIT only, and only if the avg
    ///   axis is in the TLV.
    ///
    /// Any other variant — or a PCPSIT whose TLV does not carry the
    /// requested axis — is rejected with [`Error::InvalidPath`].
    pub fn prove_indexed_axis_top_k<'b, B, P>(
        &self,
        path: P,
        axis: IndexAxis,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut full_range = MerkQuery::new();
        full_range.insert_all();
        full_range.left_to_right = !descending;
        self.prove_indexed_axis_query(path, axis, full_range, Some(k), transaction, grove_version)
    }

    /// Generate a proof for an arbitrary query against the per-axis
    /// secondary of an indexed-tree at `path`. The query is over the
    /// secondary's keyspace, which is `(sort_key_be ‖ original_key)`
    /// per axis (8 + N bytes for count/sum, 16 + N bytes for avg).
    pub fn prove_indexed_axis_query<'b, B, P>(
        &self,
        path: P,
        axis: IndexAxis,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let envelope = cost_return_on_error!(
            &mut cost,
            self.build_indexed_axis_range_proof(
                path,
                axis,
                secondary_query,
                limit,
                tx_ref,
                &batch,
                grove_version,
            )
        );

        let bytes = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(&envelope, bincode::config::standard()).map_err(|e| {
                Error::CorruptedData(format!("encoding indexed-axis range proof: {e}"))
            })
        );

        Ok(bytes).wrap_with_cost(cost)
    }

    /// Generate an offset-paginated proof for the top-`k` entries of an
    /// indexed-tree on a specific axis, starting after `offset` entries
    /// in the directional walk.
    ///
    /// For count and avg axes the secondary proof uses
    /// `Merk::prove_count_offset_on_range` (O(log n + k)); for the sum
    /// axis it uses a regular range proof with `limit = offset + k`
    /// (O(offset + k)) — no count-bound offset primitive exists for
    /// `ProvableSumTree` hosts.
    pub fn prove_indexed_axis_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        axis: IndexAxis,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let envelope = cost_return_on_error!(
            &mut cost,
            self.build_indexed_axis_paginated_proof(
                path,
                axis,
                k,
                offset,
                descending,
                tx_ref,
                &batch,
                grove_version,
            )
        );

        let bytes = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(&envelope, bincode::config::standard()).map_err(|e| {
                Error::CorruptedData(format!("encoding indexed-axis paginated proof: {e}"))
            })
        );

        Ok(bytes).wrap_with_cost(cost)
    }

    /// Generate an aggregate proof over a value-range against an
    /// indexed-tree's per-axis secondary.
    ///
    /// Only [`IndexAxis::Count`] and [`IndexAxis::Sum`] are supported.
    /// [`IndexAxis::Avg`] returns [`Error::NotSupported`] — averaging
    /// averages over a range is not a closed-form aggregate (callers
    /// should compute it client-side from
    /// `indexed_count_range_aggregate` + `indexed_sum_range_aggregate`
    /// against the same path).
    ///
    /// `lo > hi` is a degenerate range; the proof commits `0`.
    pub fn prove_indexed_axis_range_aggregate<'b, B, P>(
        &self,
        path: P,
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();
        match axis {
            IndexAxis::Count | IndexAxis::Sum => {}
            IndexAxis::Avg => {
                return Err(Error::NotSupported(
                    "indexed-axis aggregate proofs are not defined for the Avg axis".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        }
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let envelope = cost_return_on_error!(
            &mut cost,
            self.build_indexed_axis_aggregate_proof(
                path,
                axis,
                lo,
                hi,
                tx_ref,
                &batch,
                grove_version,
            )
        );

        let bytes = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(&envelope, bincode::config::standard()).map_err(|e| {
                Error::CorruptedData(format!("encoding indexed-axis aggregate proof: {e}"))
            })
        );

        Ok(bytes).wrap_with_cost(cost)
    }

    fn build_indexed_axis_range_proof<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedAxisRangeProof, Error> {
        let mut cost = OperationCost::default();

        let path_keys: Vec<Vec<u8>> = path.to_vec();
        if path_keys.is_empty() {
            return Err(Error::InvalidPath(
                "cannot prove indexed-axis query at root path".to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // 1. Per-axis validation + secondary root key + non-queried axis hashes.
        let (secondary_root_key, other_axes_root_hashes, target_is_pcpsit) = cost_return_on_error!(
            &mut cost,
            read_queried_axis_info_with_path_keys(
                self,
                &path_keys,
                axis,
                transaction,
                batch,
                grove_version,
                "indexed-axis range proof",
            )
        );

        // 2. Layer proofs + ancestor attestations.
        let layer_proofs = cost_return_on_error!(
            &mut cost,
            build_layer_proofs(
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
                "indexed-axis range proof",
            )
        );
        let ancestor_attestations = cost_return_on_error!(
            &mut cost,
            build_ancestor_attestations(
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
                "indexed-axis range proof",
            )
        );

        // 3. Open the queried primary and capture its root hash.
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !primary_merk.tree_type.is_indexed_primary() {
            return Err(Error::InvalidPath(
                "prove_indexed_axis_* requires the path's last segment to be an indexed-tree \
                 element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let (primary_root_hash, _, _) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "indexed-axis range proof: primary root hash: {e}"
                    ))
                })
        );

        // 4. Open the per-axis secondary and produce the range proof.
        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_indexed_secondary_at_path(
                path,
                axis,
                secondary_root_key,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let descending = !secondary_query.left_to_right;
        let requested_limit = limit;
        let sec_result = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .prove(secondary_query, limit, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "indexed-axis range proof: secondary range proof: {e}"
                )))
        );

        Ok(IndexedAxisRangeProof {
            axis_tag: axis.tag(),
            layer_proofs,
            primary_root_hash,
            ancestor_attestations,
            other_axes_root_hashes,
            target_is_pcpsit,
            secondary_proof: sec_result.proof,
            requested_limit,
            descending,
        })
        .wrap_with_cost(cost)
    }

    fn build_indexed_axis_paginated_proof<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedAxisPaginatedProof, Error> {
        let mut cost = OperationCost::default();

        let path_keys: Vec<Vec<u8>> = path.to_vec();
        if path_keys.is_empty() {
            return Err(Error::InvalidPath(
                "cannot prove indexed-axis paginated query at root path".to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // 1. Per-axis validation + secondary root key + non-queried axes.
        let (secondary_root_key, other_axes_root_hashes, target_is_pcpsit) = cost_return_on_error!(
            &mut cost,
            read_queried_axis_info_with_path_keys(
                self,
                &path_keys,
                axis,
                transaction,
                batch,
                grove_version,
                "indexed-axis paginated proof",
            )
        );

        // 2. Layer proofs + ancestor attestations.
        let layer_proofs = cost_return_on_error!(
            &mut cost,
            build_layer_proofs(
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
                "indexed-axis paginated proof",
            )
        );
        let ancestor_attestations = cost_return_on_error!(
            &mut cost,
            build_ancestor_attestations(
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
                "indexed-axis paginated proof",
            )
        );

        // 3. Open the primary and capture its root hash.
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !primary_merk.tree_type.is_indexed_primary() {
            return Err(Error::InvalidPath(
                "prove_indexed_axis_top_k_paginated requires the path's last segment to be an \
                 indexed-tree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let (primary_root_hash, _, _) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "indexed-axis paginated proof: primary root hash: {e}"
                    ))
                })
        );

        // 4. Open the per-axis secondary; emit the appropriate paginated
        //    proof per axis. Count/Avg axes have a HashWithCount-based
        //    primitive; Sum axis does not, so we fall back to a regular
        //    range proof with `limit = offset + k`.
        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_indexed_secondary_at_path(
                path,
                axis,
                secondary_root_key,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let serialized = match axis {
            IndexAxis::Count | IndexAxis::Avg => {
                let inner_range = MerkQueryItemForRange::RangeFull(std::ops::RangeFull);
                let prove_result = cost_return_on_error!(
                    &mut cost,
                    secondary_merk
                        .prove_count_offset_on_range(
                            &inner_range,
                            offset,
                            Some(k as u64),
                            !descending,
                            grove_version,
                        )
                        .map_err(|e| Error::CorruptedData(format!(
                            "indexed-axis paginated proof: secondary count-offset proof: {e}"
                        )))
                );
                let mut serialized = Vec::with_capacity(128);
                encode_into(prove_result.ops.iter(), &mut serialized);
                serialized
            }
            IndexAxis::Sum => {
                // Sum axis: ProvableSumTree has no count-offset
                // primitive. Emit a plain `prove` with limit = offset+k;
                // the verifier discards the leading `offset` items.
                let mut full_range = MerkQuery::new();
                full_range.insert_all();
                full_range.left_to_right = !descending;
                let combined_limit = (offset as u128)
                    .saturating_add(k as u128)
                    .min(u16::MAX as u128) as u16;
                let sec_result = cost_return_on_error!(
                    &mut cost,
                    secondary_merk
                        .prove(full_range, Some(combined_limit), grove_version)
                        .map_err(|e| Error::CorruptedData(format!(
                            "indexed-axis paginated proof: secondary regular proof (sum): {e}"
                        )))
                );
                sec_result.proof
            }
        };

        Ok(IndexedAxisPaginatedProof {
            axis_tag: axis.tag(),
            layer_proofs,
            primary_root_hash,
            ancestor_attestations,
            other_axes_root_hashes,
            target_is_pcpsit,
            secondary_proof: serialized,
            requested_k: k,
            requested_offset: offset,
            descending,
        })
        .wrap_with_cost(cost)
    }

    fn build_indexed_axis_aggregate_proof<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedAxisAggregateProof, Error> {
        let mut cost = OperationCost::default();

        let path_keys: Vec<Vec<u8>> = path.to_vec();
        if path_keys.is_empty() {
            return Err(Error::InvalidPath(
                "cannot prove indexed-axis aggregate at root path".to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // 1. Per-axis validation + secondary root key + non-queried axes.
        let (secondary_root_key, other_axes_root_hashes, target_is_pcpsit) = cost_return_on_error!(
            &mut cost,
            read_queried_axis_info_with_path_keys(
                self,
                &path_keys,
                axis,
                transaction,
                batch,
                grove_version,
                "indexed-axis aggregate proof",
            )
        );

        // 2. Layer proofs + ancestor attestations.
        let layer_proofs = cost_return_on_error!(
            &mut cost,
            build_layer_proofs(
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
                "indexed-axis aggregate proof",
            )
        );
        let ancestor_attestations = cost_return_on_error!(
            &mut cost,
            build_ancestor_attestations(
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
                "indexed-axis aggregate proof",
            )
        );

        // 3. Open the primary and capture its root hash.
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !primary_merk.tree_type.is_indexed_primary() {
            return Err(Error::InvalidPath(
                "prove_indexed_axis_range_aggregate requires the path's last segment to be an \
                 indexed-tree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let (primary_root_hash, _, _) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "indexed-axis aggregate proof: primary root hash: {e}"
                    ))
                })
        );

        // 4. Open the per-axis secondary; build the inner range against
        //    the secondary's keyspace per axis and emit the appropriate
        //    aggregate proof.
        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_indexed_secondary_at_path(
                path,
                axis,
                secondary_root_key,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let serialized = match axis {
            IndexAxis::Count => {
                // count_value ∈ [0, u64::MAX]. A range whose whole span is
                // outside that domain (hi < 0 OR lo > u64::MAX) must commit
                // an EMPTY (count = 0) proof — clamping the bounds into the
                // domain would otherwise collapse the range onto a boundary
                // key (e.g. lo = u64::MAX + 5, hi = u64::MAX + 10 → query
                // `count == u64::MAX`) and erroneously count entries
                // sitting exactly on the boundary.
                if aggregate_range_out_of_domain(IndexAxis::Count, lo, hi) {
                    cost_return_on_error_no_add!(
                        cost,
                        build_empty_count_aggregate_proof(
                            &secondary_merk,
                            grove_version,
                            &mut cost,
                        )
                    )
                } else {
                    let lo_u = if lo < 0 {
                        0u64
                    } else {
                        lo.min(u64::MAX as i128) as u64
                    };
                    let hi_u = hi.min(u64::MAX as i128) as u64;
                    cost_return_on_error_no_add!(
                        cost,
                        build_count_aggregate_secondary_proof(
                            &secondary_merk,
                            lo_u,
                            hi_u,
                            grove_version,
                            &mut cost,
                        )
                    )
                }
            }
            IndexAxis::Sum => {
                // sum_value ∈ [i64::MIN, i64::MAX]. As with count, a range
                // entirely above or below that domain must commit an EMPTY
                // (sum = 0) proof rather than clamping onto i64::MAX /
                // i64::MIN (which would count/sum boundary entries).
                if aggregate_range_out_of_domain(IndexAxis::Sum, lo, hi) {
                    cost_return_on_error_no_add!(
                        cost,
                        build_empty_sum_aggregate_proof(&secondary_merk, grove_version, &mut cost)
                    )
                } else {
                    let lo_i = lo.max(i64::MIN as i128).min(i64::MAX as i128) as i64;
                    let hi_i = hi.max(i64::MIN as i128).min(i64::MAX as i128) as i64;
                    cost_return_on_error_no_add!(
                        cost,
                        build_sum_aggregate_secondary_proof(
                            &secondary_merk,
                            lo_i,
                            hi_i,
                            grove_version,
                            &mut cost,
                        )
                    )
                }
            }
            IndexAxis::Avg => unreachable!("avg axis rejected by public entry point"),
        };

        Ok(IndexedAxisAggregateProof {
            axis_tag: axis.tag(),
            layer_proofs,
            primary_root_hash,
            ancestor_attestations,
            other_axes_root_hashes,
            target_is_pcpsit,
            secondary_proof: serialized,
            lo,
            hi,
        })
        .wrap_with_cost(cost)
    }

    // -------------------- public verify entry points --------------------

    /// Verify an `IndexedAxisRangeProof`-shaped top-k proof (full range,
    /// limit = `expected_k`, direction = `expected_descending`).
    pub fn verify_indexed_axis_top_k(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_axis: IndexAxis,
        expected_k: u16,
        expected_descending: bool,
    ) -> Result<IndexedAxisQueryResult, Error> {
        let envelope = decode_range_envelope(proof_bytes)?;
        if envelope.axis_tag != expected_axis.tag() {
            return Err(Error::CorruptedData(format!(
                "indexed-axis top_k proof axis mismatch: expected {:?} (tag={}), envelope carries \
                 tag={}",
                expected_axis,
                expected_axis.tag(),
                envelope.axis_tag
            )));
        }
        if envelope.descending != expected_descending {
            return Err(Error::CorruptedData(format!(
                "indexed-axis top_k proof direction mismatch: expected descending={}, envelope \
                 carries descending={}",
                expected_descending, envelope.descending
            )));
        }
        if envelope.requested_limit != Some(expected_k) {
            return Err(Error::CorruptedData(format!(
                "indexed-axis top_k proof limit mismatch: expected Some({}), envelope carries \
                 {:?}",
                expected_k, envelope.requested_limit
            )));
        }
        let mut full_range = MerkQuery::new();
        full_range.insert_all();
        full_range.left_to_right = !envelope.descending;
        verify_indexed_axis_range_inner(envelope, full_range, expected_axis, path)
    }

    /// Verify an `IndexedAxisRangeProof`-shaped arbitrary-query proof.
    ///
    /// `secondary_query` MUST match the query supplied at proof time.
    /// `expected_limit` MUST match the limit supplied at proof time.
    pub fn verify_indexed_axis_query(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_axis: IndexAxis,
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
    ) -> Result<IndexedAxisQueryResult, Error> {
        let envelope = decode_range_envelope(proof_bytes)?;
        if envelope.axis_tag != expected_axis.tag() {
            return Err(Error::CorruptedData(format!(
                "indexed-axis query proof axis mismatch: expected {:?} (tag={}), envelope carries \
                 tag={}",
                expected_axis,
                expected_axis.tag(),
                envelope.axis_tag
            )));
        }
        if envelope.requested_limit != expected_limit {
            return Err(Error::CorruptedData(format!(
                "indexed-axis query proof limit mismatch: expected {:?}, envelope carries {:?}",
                expected_limit, envelope.requested_limit
            )));
        }
        let expected_descending = !secondary_query.left_to_right;
        if envelope.descending != expected_descending {
            return Err(Error::CorruptedData(format!(
                "indexed-axis query proof direction mismatch: secondary_query implies \
                 descending={}, envelope carries descending={}",
                expected_descending, envelope.descending
            )));
        }
        verify_indexed_axis_range_inner(envelope, secondary_query, expected_axis, path)
    }

    /// Verify an `IndexedAxisPaginatedProof`-shaped paginated proof.
    pub fn verify_indexed_axis_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_axis: IndexAxis,
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (envelope, _): (IndexedAxisPaginatedProof, _) =
            bincode::decode_from_slice(proof_bytes, config).map_err(|e| {
                Error::CorruptedData(format!("decoding indexed-axis paginated proof: {e}"))
            })?;
        if envelope.axis_tag != expected_axis.tag() {
            return Err(Error::CorruptedData(format!(
                "indexed-axis paginated proof axis mismatch: expected {:?} (tag={}), envelope \
                 carries tag={}",
                expected_axis,
                expected_axis.tag(),
                envelope.axis_tag
            )));
        }
        if envelope.descending != expected_descending {
            return Err(Error::CorruptedData(format!(
                "indexed-axis paginated proof direction mismatch: expected descending={}, \
                 envelope carries descending={}",
                expected_descending, envelope.descending
            )));
        }
        if envelope.requested_k != expected_k {
            return Err(Error::CorruptedData(format!(
                "indexed-axis paginated proof k mismatch: expected {}, envelope carries {}",
                expected_k, envelope.requested_k
            )));
        }
        if envelope.requested_offset != expected_offset {
            return Err(Error::CorruptedData(format!(
                "indexed-axis paginated proof offset mismatch: expected {}, envelope carries {}",
                expected_offset, envelope.requested_offset
            )));
        }
        verify_indexed_axis_paginated_inner(envelope, expected_axis, path)
    }

    /// Verify an `IndexedAxisAggregateProof`-shaped aggregate proof.
    pub fn verify_indexed_axis_range_aggregate(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_axis: IndexAxis,
        expected_lo: i128,
        expected_hi: i128,
    ) -> Result<IndexedAxisAggregateResult, Error> {
        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (envelope, _): (IndexedAxisAggregateProof, _) =
            bincode::decode_from_slice(proof_bytes, config).map_err(|e| {
                Error::CorruptedData(format!("decoding indexed-axis aggregate proof: {e}"))
            })?;
        if envelope.axis_tag != expected_axis.tag() {
            return Err(Error::CorruptedData(format!(
                "indexed-axis aggregate proof axis mismatch: expected {:?} (tag={}), envelope \
                 carries tag={}",
                expected_axis,
                expected_axis.tag(),
                envelope.axis_tag
            )));
        }
        if !matches!(expected_axis, IndexAxis::Count | IndexAxis::Sum) {
            return Err(Error::NotSupported(
                "indexed-axis aggregate proofs are not defined for the Avg axis".to_string(),
            ));
        }
        if envelope.lo != expected_lo {
            return Err(Error::CorruptedData(format!(
                "indexed-axis aggregate proof lo mismatch: expected {}, envelope carries {}",
                expected_lo, envelope.lo
            )));
        }
        if envelope.hi != expected_hi {
            return Err(Error::CorruptedData(format!(
                "indexed-axis aggregate proof hi mismatch: expected {}, envelope carries {}",
                expected_hi, envelope.hi
            )));
        }
        verify_indexed_axis_aggregate_inner(envelope, expected_axis, path)
    }
}

// ===================================================================
// Decode helpers + per-shape verifier cores
// ===================================================================

fn decode_range_envelope(proof_bytes: &[u8]) -> Result<IndexedAxisRangeProof, Error> {
    let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
    let (envelope, _): (IndexedAxisRangeProof, _) = bincode::decode_from_slice(proof_bytes, config)
        .map_err(|e| Error::CorruptedData(format!("decoding indexed-axis range proof: {e}")))?;
    Ok(envelope)
}

fn verify_indexed_axis_range_inner(
    envelope: IndexedAxisRangeProof,
    secondary_query: MerkQuery,
    axis: IndexAxis,
    path: &[&[u8]],
) -> Result<IndexedAxisQueryResult, Error> {
    if envelope.layer_proofs.len() != path.len() {
        return Err(Error::CorruptedData(format!(
            "indexed-axis range proof has {} layers but path has {} segments",
            envelope.layer_proofs.len(),
            path.len()
        )));
    }
    if envelope.layer_proofs.is_empty() {
        return Err(Error::CorruptedData(
            "indexed-axis range proof has zero layers; expected at least one".to_string(),
        ));
    }

    let limit_for_verify = envelope.requested_limit;
    let left_to_right = secondary_query.left_to_right;
    let (secondary_root_hash, sec_result) = secondary_query
        .execute_proof(
            &envelope.secondary_proof,
            limit_for_verify,
            left_to_right,
            0,
        )
        .unwrap()
        .map_err(|e| {
            Error::CorruptedData(format!(
                "indexed-axis range proof: secondary proof failed to verify: {e}"
            ))
        })?;

    let entries = decode_axis_entries_from_result_set(axis, &sec_result.result_set)?;

    let initial_root = verify_deepest_layer(
        &envelope.layer_proofs,
        path,
        &envelope.primary_root_hash,
        &secondary_root_hash,
        axis,
        &envelope.other_axes_root_hashes,
        envelope.target_is_pcpsit,
        "indexed-axis range proof",
    )?;

    let root_hash = walk_ancestor_chain(
        &envelope.layer_proofs,
        &envelope.ancestor_attestations,
        path,
        initial_root,
        "indexed-axis range proof",
    )?;

    Ok(IndexedAxisQueryResult { root_hash, entries })
}

fn verify_indexed_axis_paginated_inner(
    envelope: IndexedAxisPaginatedProof,
    axis: IndexAxis,
    path: &[&[u8]],
) -> Result<IndexedAxisPaginatedResult, Error> {
    if envelope.layer_proofs.len() != path.len() {
        return Err(Error::CorruptedData(format!(
            "indexed-axis paginated proof has {} layers but path has {} segments",
            envelope.layer_proofs.len(),
            path.len()
        )));
    }
    if envelope.layer_proofs.is_empty() {
        return Err(Error::CorruptedData(
            "indexed-axis paginated proof has zero layers; expected at least one".to_string(),
        ));
    }

    let (secondary_root_hash, entries, skipped) = match axis {
        IndexAxis::Count | IndexAxis::Avg => {
            let inner_range = MerkQueryItemForRange::RangeFull(std::ops::RangeFull);
            let count_offset_result = verify_count_offset_on_range_proof(
                &envelope.secondary_proof,
                &inner_range,
                envelope.requested_offset,
                Some(envelope.requested_k as u64),
                !envelope.descending,
            )
            .unwrap()
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "indexed-axis paginated proof: secondary count-offset proof failed to \
                     verify: {e}"
                ))
            })?;
            let entries = decode_axis_entries_from_count_offset_items(
                axis,
                &count_offset_result.returned_items,
            )?;
            (
                count_offset_result.root_hash,
                entries,
                count_offset_result.skipped,
            )
        }
        IndexAxis::Sum => {
            // Sum-axis paginated: regular range proof with limit = offset+k.
            // Verifier reconstructs same range proof, then post-skips the
            // first `requested_offset` items in directional order.
            let mut full_range = MerkQuery::new();
            full_range.insert_all();
            full_range.left_to_right = !envelope.descending;
            let combined_limit = (envelope.requested_offset as u128)
                .saturating_add(envelope.requested_k as u128)
                .min(u16::MAX as u128) as u16;
            let (secondary_root_hash, sec_result) = full_range
                .execute_proof(
                    &envelope.secondary_proof,
                    Some(combined_limit),
                    !envelope.descending,
                    0,
                )
                .unwrap()
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "indexed-axis paginated proof: secondary regular proof (sum) failed to \
                         verify: {e}"
                    ))
                })?;
            let mut all_entries =
                decode_axis_entries_from_result_set(IndexAxis::Sum, &sec_result.result_set)?;
            let total_returned = all_entries.len() as u64;
            let skip = envelope.requested_offset.min(total_returned);
            // Trim the first `skip` items off the front of the result set.
            match &mut all_entries {
                AxisEntries::Sum(v) => {
                    v.drain(0..skip as usize);
                    if v.len() > envelope.requested_k as usize {
                        v.truncate(envelope.requested_k as usize);
                    }
                }
                _ => unreachable!("axis is Sum here"),
            }
            (secondary_root_hash, all_entries, skip)
        }
    };

    let initial_root = verify_deepest_layer(
        &envelope.layer_proofs,
        path,
        &envelope.primary_root_hash,
        &secondary_root_hash,
        axis,
        &envelope.other_axes_root_hashes,
        envelope.target_is_pcpsit,
        "indexed-axis paginated proof",
    )?;

    let root_hash = walk_ancestor_chain(
        &envelope.layer_proofs,
        &envelope.ancestor_attestations,
        path,
        initial_root,
        "indexed-axis paginated proof",
    )?;

    Ok(IndexedAxisPaginatedResult {
        root_hash,
        entries,
        skipped,
    })
}

fn verify_indexed_axis_aggregate_inner(
    envelope: IndexedAxisAggregateProof,
    axis: IndexAxis,
    path: &[&[u8]],
) -> Result<IndexedAxisAggregateResult, Error> {
    if envelope.layer_proofs.len() != path.len() {
        return Err(Error::CorruptedData(format!(
            "indexed-axis aggregate proof has {} layers but path has {} segments",
            envelope.layer_proofs.len(),
            path.len()
        )));
    }
    if envelope.layer_proofs.is_empty() {
        return Err(Error::CorruptedData(
            "indexed-axis aggregate proof has zero layers; expected at least one".to_string(),
        ));
    }

    let (secondary_root_hash, aggregate_value) = match axis {
        IndexAxis::Count => {
            let inner_range = count_aggregate_inner_range(envelope.lo, envelope.hi);
            let (root, count) =
                verify_aggregate_count_on_range_proof(&envelope.secondary_proof, &inner_range)
                    .unwrap()
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "indexed-axis aggregate proof: secondary aggregate-count proof \
                             failed to verify: {e}"
                        ))
                    })?;
            (root, count as i128)
        }
        IndexAxis::Sum => {
            let inner_range = sum_aggregate_inner_range(envelope.lo, envelope.hi);
            let (root, sum) =
                verify_aggregate_sum_on_range_proof(&envelope.secondary_proof, &inner_range)
                    .unwrap()
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "indexed-axis aggregate proof: secondary aggregate-sum proof failed \
                             to verify: {e}"
                        ))
                    })?;
            (root, sum as i128)
        }
        IndexAxis::Avg => {
            return Err(Error::NotSupported(
                "indexed-axis aggregate proofs are not defined for the Avg axis".to_string(),
            ));
        }
    };

    let initial_root = verify_deepest_layer(
        &envelope.layer_proofs,
        path,
        &envelope.primary_root_hash,
        &secondary_root_hash,
        axis,
        &envelope.other_axes_root_hashes,
        envelope.target_is_pcpsit,
        "indexed-axis aggregate proof",
    )?;

    let root_hash = walk_ancestor_chain(
        &envelope.layer_proofs,
        &envelope.ancestor_attestations,
        path,
        initial_root,
        "indexed-axis aggregate proof",
    )?;

    Ok(IndexedAxisAggregateResult {
        root_hash,
        axis,
        aggregate: aggregate_value,
    })
}

fn decode_axis_entries_from_result_set(
    axis: IndexAxis,
    result_set: &[grovedb_merk::proofs::query::ProvedKeyOptionalValue],
) -> Result<AxisEntries, Error> {
    match axis {
        IndexAxis::Count => {
            let mut entries: Vec<(u64, Vec<u8>)> = Vec::with_capacity(result_set.len());
            for proved in result_set {
                if proved.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (count) secondary key shorter than 8 bytes: {:?}",
                        proved.key
                    )));
                }
                let mut count_bytes = [0u8; 8];
                count_bytes.copy_from_slice(&proved.key[..8]);
                entries.push((
                    decode_count_sort_key(&count_bytes),
                    proved.key[8..].to_vec(),
                ));
            }
            Ok(AxisEntries::Count(entries))
        }
        IndexAxis::Sum => {
            let mut entries: Vec<(i64, Vec<u8>)> = Vec::with_capacity(result_set.len());
            for proved in result_set {
                if proved.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (sum) secondary key shorter than 8 bytes: {:?}",
                        proved.key
                    )));
                }
                let mut sum_bytes = [0u8; 8];
                sum_bytes.copy_from_slice(&proved.key[..8]);
                entries.push((decode_sum_sort_key(&sum_bytes), proved.key[8..].to_vec()));
            }
            Ok(AxisEntries::Sum(entries))
        }
        IndexAxis::Avg => {
            let mut entries: Vec<(i128, Vec<u8>)> = Vec::with_capacity(result_set.len());
            for proved in result_set {
                if proved.key.len() < 16 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (avg) secondary key shorter than 16 bytes: {:?}",
                        proved.key
                    )));
                }
                let mut avg_bytes = [0u8; 16];
                avg_bytes.copy_from_slice(&proved.key[..16]);
                entries.push((
                    grovedb_element::indexed::decode_avg_sort_key(&avg_bytes),
                    proved.key[16..].to_vec(),
                ));
            }
            Ok(AxisEntries::Avg(entries))
        }
    }
}

fn decode_axis_entries_from_count_offset_items(
    axis: IndexAxis,
    items: &[grovedb_merk::proofs::query::CountOffsetReturnedItem],
) -> Result<AxisEntries, Error> {
    match axis {
        IndexAxis::Count => {
            let mut entries: Vec<(u64, Vec<u8>)> = Vec::with_capacity(items.len());
            for it in items {
                if it.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (count) paginated secondary key shorter than 8 bytes: {:?}",
                        it.key
                    )));
                }
                let mut count_bytes = [0u8; 8];
                count_bytes.copy_from_slice(&it.key[..8]);
                entries.push((decode_count_sort_key(&count_bytes), it.key[8..].to_vec()));
            }
            Ok(AxisEntries::Count(entries))
        }
        IndexAxis::Avg => {
            let mut entries: Vec<(i128, Vec<u8>)> = Vec::with_capacity(items.len());
            for it in items {
                if it.key.len() < 16 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (avg) paginated secondary key shorter than 16 bytes: {:?}",
                        it.key
                    )));
                }
                let mut avg_bytes = [0u8; 16];
                avg_bytes.copy_from_slice(&it.key[..16]);
                entries.push((
                    grovedb_element::indexed::decode_avg_sort_key(&avg_bytes),
                    it.key[16..].to_vec(),
                ));
            }
            Ok(AxisEntries::Avg(entries))
        }
        IndexAxis::Sum => {
            unreachable!("count-offset proof does not apply to the sum axis")
        }
    }
}

// ===================================================================
// Aggregate-secondary-proof builders (per axis)
// ===================================================================

fn build_count_aggregate_secondary_proof<'db, S>(
    secondary_merk: &grovedb_merk::Merk<S>,
    lo_count: u64,
    hi_count: u64,
    grove_version: &GroveVersion,
    cost: &mut OperationCost,
) -> Result<Vec<u8>, Error>
where
    S: grovedb_storage::StorageContext<'db>,
{
    if lo_count > hi_count {
        // Degenerate; build a guaranteed-empty range so the merk still
        // emits a proof that hashes to the actual secondary root.
        let lo_bytes = hi_count.saturating_add(1).to_be_bytes().to_vec();
        let inner_range = MerkQueryItemForRange::Range(lo_bytes.clone()..lo_bytes);
        let (ops, _) = secondary_merk
            .prove_aggregate_count_on_range(&inner_range, grove_version)
            .unwrap_add_cost(cost)
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "indexed-axis count-aggregate degenerate-range proof: {e}"
                ))
            })?;
        let mut serialized = Vec::with_capacity(128);
        encode_into(ops.iter(), &mut serialized);
        return Ok(serialized);
    }
    let lo_bytes = encode_count_sort_key(lo_count).to_vec();
    let inner_range = if hi_count == u64::MAX {
        MerkQueryItemForRange::RangeFrom(lo_bytes..)
    } else {
        let upper_bytes = encode_count_sort_key(hi_count + 1).to_vec();
        MerkQueryItemForRange::Range(lo_bytes..upper_bytes)
    };
    let (ops, _) = secondary_merk
        .prove_aggregate_count_on_range(&inner_range, grove_version)
        .unwrap_add_cost(cost)
        .map_err(|e| {
            Error::CorruptedData(format!("indexed-axis count-aggregate range proof: {e}"))
        })?;
    let mut serialized = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut serialized);
    Ok(serialized)
}

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

fn build_empty_count_aggregate_proof<'db, S>(
    secondary_merk: &grovedb_merk::Merk<S>,
    grove_version: &GroveVersion,
    cost: &mut OperationCost,
) -> Result<Vec<u8>, Error>
where
    S: grovedb_storage::StorageContext<'db>,
{
    // Empty range = "count = 0", emitted as a guaranteed-empty range
    // so the secondary root is still committed.
    let bytes = u64::MAX.to_be_bytes().to_vec();
    let inner_range = MerkQueryItemForRange::Range(bytes.clone()..bytes);
    let (ops, _) = secondary_merk
        .prove_aggregate_count_on_range(&inner_range, grove_version)
        .unwrap_add_cost(cost)
        .map_err(|e| {
            Error::CorruptedData(format!(
                "indexed-axis count-aggregate empty-range proof: {e}"
            ))
        })?;
    let mut serialized = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut serialized);
    Ok(serialized)
}

fn build_sum_aggregate_secondary_proof<'db, S>(
    secondary_merk: &grovedb_merk::Merk<S>,
    lo_sum: i64,
    hi_sum: i64,
    grove_version: &GroveVersion,
    cost: &mut OperationCost,
) -> Result<Vec<u8>, Error>
where
    S: grovedb_storage::StorageContext<'db>,
{
    if lo_sum > hi_sum {
        // Degenerate: emit an empty-range proof against the secondary.
        let bytes = encode_sum_sort_key(hi_sum.saturating_add(1)).to_vec();
        let inner_range = MerkQueryItemForRange::Range(bytes.clone()..bytes);
        let (ops, _) = secondary_merk
            .prove_aggregate_sum_on_range(&inner_range, grove_version)
            .unwrap_add_cost(cost)
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "indexed-axis sum-aggregate degenerate-range proof: {e}"
                ))
            })?;
        let mut serialized = Vec::with_capacity(128);
        encode_into(ops.iter(), &mut serialized);
        return Ok(serialized);
    }
    let lo_bytes = encode_sum_sort_key(lo_sum).to_vec();
    let inner_range = if hi_sum == i64::MAX {
        MerkQueryItemForRange::RangeFrom(lo_bytes..)
    } else {
        let upper_bytes = encode_sum_sort_key(hi_sum + 1).to_vec();
        MerkQueryItemForRange::Range(lo_bytes..upper_bytes)
    };
    let (ops, _) = secondary_merk
        .prove_aggregate_sum_on_range(&inner_range, grove_version)
        .unwrap_add_cost(cost)
        .map_err(|e| {
            Error::CorruptedData(format!("indexed-axis sum-aggregate range proof: {e}"))
        })?;
    let mut serialized = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut serialized);
    Ok(serialized)
}

/// Canonical empty (sum = 0) aggregate proof for the sum axis. Emits a
/// guaranteed-empty range `[encode(i64::MAX) .. encode(i64::MAX))` so
/// the secondary root is still committed. Mirrors
/// [`build_empty_count_aggregate_proof`] for the sum axis; the verifier
/// reconstructs the identical range in [`sum_aggregate_inner_range`]'s
/// out-of-domain branch.
fn build_empty_sum_aggregate_proof<'db, S>(
    secondary_merk: &grovedb_merk::Merk<S>,
    grove_version: &GroveVersion,
    cost: &mut OperationCost,
) -> Result<Vec<u8>, Error>
where
    S: grovedb_storage::StorageContext<'db>,
{
    let bytes = encode_sum_sort_key(i64::MAX).to_vec();
    let inner_range = MerkQueryItemForRange::Range(bytes.clone()..bytes);
    let (ops, _) = secondary_merk
        .prove_aggregate_sum_on_range(&inner_range, grove_version)
        .unwrap_add_cost(cost)
        .map_err(|e| {
            Error::CorruptedData(format!("indexed-axis sum-aggregate empty-range proof: {e}"))
        })?;
    let mut serialized = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut serialized);
    Ok(serialized)
}

// ===================================================================
// Verifier-side inner-range reconstruction (matches the prover's
// secondary keyspace mapping).
// ===================================================================

fn count_aggregate_inner_range(lo: i128, hi: i128) -> MerkQueryItemForRange {
    // Out-of-domain (hi < 0 OR lo > u64::MAX): emit the canonical
    // empty-range shape (`u64::MAX..u64::MAX`) the prover commits via
    // `build_empty_count_aggregate_proof`. This must match exactly or
    // the aggregate proof fails to verify.
    if aggregate_range_out_of_domain(IndexAxis::Count, lo, hi) {
        let bytes = u64::MAX.to_be_bytes().to_vec();
        return MerkQueryItemForRange::Range(bytes.clone()..bytes);
    }
    let lo_u = if lo < 0 {
        0u64
    } else {
        lo.min(u64::MAX as i128) as u64
    };
    let hi_u = hi.min(u64::MAX as i128) as u64;
    if lo_u > hi_u {
        let bytes = hi_u.saturating_add(1).to_be_bytes().to_vec();
        return MerkQueryItemForRange::Range(bytes.clone()..bytes);
    }
    let lo_bytes = encode_count_sort_key(lo_u).to_vec();
    if hi_u == u64::MAX {
        MerkQueryItemForRange::RangeFrom(lo_bytes..)
    } else {
        let upper_bytes = encode_count_sort_key(hi_u + 1).to_vec();
        MerkQueryItemForRange::Range(lo_bytes..upper_bytes)
    }
}

fn sum_aggregate_inner_range(lo: i128, hi: i128) -> MerkQueryItemForRange {
    // Out-of-domain (hi < i64::MIN OR lo > i64::MAX): emit the canonical
    // empty-range shape the prover commits via
    // `build_empty_sum_aggregate_proof` (`encode(i64::MAX)..encode(i64::MAX)`).
    if aggregate_range_out_of_domain(IndexAxis::Sum, lo, hi) {
        let bytes = encode_sum_sort_key(i64::MAX).to_vec();
        return MerkQueryItemForRange::Range(bytes.clone()..bytes);
    }
    let lo_i = lo.max(i64::MIN as i128).min(i64::MAX as i128) as i64;
    let hi_i = hi.max(i64::MIN as i128).min(i64::MAX as i128) as i64;
    if lo_i > hi_i {
        let bytes = encode_sum_sort_key(hi_i.saturating_add(1)).to_vec();
        return MerkQueryItemForRange::Range(bytes.clone()..bytes);
    }
    let lo_bytes = encode_sum_sort_key(lo_i).to_vec();
    if hi_i == i64::MAX {
        MerkQueryItemForRange::RangeFrom(lo_bytes..)
    } else {
        let upper_bytes = encode_sum_sort_key(hi_i + 1).to_vec();
        MerkQueryItemForRange::Range(lo_bytes..upper_bytes)
    }
}

// ===================================================================
// Per-axis convenience wrappers
// ===================================================================

impl GroveDb {
    // ---------- count axis ----------

    /// Prove the top-`k` entries of the count axis. Thin wrapper over
    /// [`Self::prove_indexed_axis_top_k`] with `axis = Count`.
    pub fn prove_indexed_count_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k(
            path,
            IndexAxis::Count,
            k,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an offset-paginated top-`k` window on the count axis.
    pub fn prove_indexed_count_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k_paginated(
            path,
            IndexAxis::Count,
            k,
            offset,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an arbitrary query against the count-axis secondary.
    pub fn prove_indexed_count_query<'b, B, P>(
        &self,
        path: P,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_query(
            path,
            IndexAxis::Count,
            secondary_query,
            limit,
            transaction,
            grove_version,
        )
    }

    /// Prove the aggregate count of entries whose `count_value` is in
    /// `[lo_count, hi_count]`.
    pub fn prove_indexed_count_range_aggregate<'b, B, P>(
        &self,
        path: P,
        lo_count: u64,
        hi_count: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_range_aggregate(
            path,
            IndexAxis::Count,
            lo_count as i128,
            hi_count as i128,
            transaction,
            grove_version,
        )
    }

    /// Verify a count-axis top-k proof.
    pub fn verify_indexed_count_top_k(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_descending: bool,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_top_k(
            proof_bytes,
            path,
            IndexAxis::Count,
            expected_k,
            expected_descending,
        )
    }

    /// Verify a count-axis paginated proof.
    pub fn verify_indexed_count_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            IndexAxis::Count,
            expected_k,
            expected_offset,
            expected_descending,
        )
    }

    /// Verify a count-axis arbitrary-query proof.
    pub fn verify_indexed_count_query(
        proof_bytes: &[u8],
        path: &[&[u8]],
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_query(
            proof_bytes,
            path,
            IndexAxis::Count,
            secondary_query,
            expected_limit,
        )
    }

    /// Verify a count-axis aggregate proof.
    pub fn verify_indexed_count_range_aggregate(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_lo_count: u64,
        expected_hi_count: u64,
    ) -> Result<IndexedAxisAggregateResult, Error> {
        Self::verify_indexed_axis_range_aggregate(
            proof_bytes,
            path,
            IndexAxis::Count,
            expected_lo_count as i128,
            expected_hi_count as i128,
        )
    }

    // ---------- sum axis ----------

    /// Prove the top-`k` entries of the sum axis.
    pub fn prove_indexed_sum_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k(
            path,
            IndexAxis::Sum,
            k,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an offset-paginated top-`k` window on the sum axis.
    /// Note: the secondary is a `ProvableSumTree`, which has no
    /// count-bound offset primitive, so the proof size is
    /// O(offset + k). Use sparingly with large offsets.
    pub fn prove_indexed_sum_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k_paginated(
            path,
            IndexAxis::Sum,
            k,
            offset,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an arbitrary query against the sum-axis secondary.
    pub fn prove_indexed_sum_query<'b, B, P>(
        &self,
        path: P,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_query(
            path,
            IndexAxis::Sum,
            secondary_query,
            limit,
            transaction,
            grove_version,
        )
    }

    /// Prove the aggregate sum of entries whose `sum_value` is in
    /// `[lo_sum, hi_sum]`.
    pub fn prove_indexed_sum_range_aggregate<'b, B, P>(
        &self,
        path: P,
        lo_sum: i64,
        hi_sum: i64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_range_aggregate(
            path,
            IndexAxis::Sum,
            lo_sum as i128,
            hi_sum as i128,
            transaction,
            grove_version,
        )
    }

    /// Verify a sum-axis top-k proof.
    pub fn verify_indexed_sum_top_k(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_descending: bool,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_top_k(
            proof_bytes,
            path,
            IndexAxis::Sum,
            expected_k,
            expected_descending,
        )
    }

    /// Verify a sum-axis paginated proof.
    pub fn verify_indexed_sum_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            IndexAxis::Sum,
            expected_k,
            expected_offset,
            expected_descending,
        )
    }

    /// Verify a sum-axis arbitrary-query proof.
    pub fn verify_indexed_sum_query(
        proof_bytes: &[u8],
        path: &[&[u8]],
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_query(
            proof_bytes,
            path,
            IndexAxis::Sum,
            secondary_query,
            expected_limit,
        )
    }

    /// Verify a sum-axis aggregate proof.
    pub fn verify_indexed_sum_range_aggregate(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_lo_sum: i64,
        expected_hi_sum: i64,
    ) -> Result<IndexedAxisAggregateResult, Error> {
        Self::verify_indexed_axis_range_aggregate(
            proof_bytes,
            path,
            IndexAxis::Sum,
            expected_lo_sum as i128,
            expected_hi_sum as i128,
        )
    }

    // ---------- avg axis ----------

    /// Prove the top-`k` entries of the avg axis. PCPSIT-only. No
    /// aggregate variant exists — averaging an average over a range is
    /// not closed-form.
    pub fn prove_indexed_avg_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k(
            path,
            IndexAxis::Avg,
            k,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an offset-paginated top-`k` window on the avg axis.
    pub fn prove_indexed_avg_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k_paginated(
            path,
            IndexAxis::Avg,
            k,
            offset,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an arbitrary query against the avg-axis secondary.
    pub fn prove_indexed_avg_query<'b, B, P>(
        &self,
        path: P,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_query(
            path,
            IndexAxis::Avg,
            secondary_query,
            limit,
            transaction,
            grove_version,
        )
    }

    /// Verify an avg-axis top-k proof.
    pub fn verify_indexed_avg_top_k(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_descending: bool,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_top_k(
            proof_bytes,
            path,
            IndexAxis::Avg,
            expected_k,
            expected_descending,
        )
    }

    /// Verify an avg-axis paginated proof.
    pub fn verify_indexed_avg_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            IndexAxis::Avg,
            expected_k,
            expected_offset,
            expected_descending,
        )
    }

    /// Verify an avg-axis arbitrary-query proof.
    pub fn verify_indexed_avg_query(
        proof_bytes: &[u8],
        path: &[&[u8]],
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_query(
            proof_bytes,
            path,
            IndexAxis::Avg,
            secondary_query,
            expected_limit,
        )
    }
}
