//! The verifier: envelope decoding, the ancestor-chain walk, and the
//! per-shape verification cores.
//!
//! Everything here operates on untrusted bytes. The chain of trust runs
//! strictly downward: every byte the verifier relies on must be derived
//! from a hash it has already verified, layer by layer, until the
//! reconstructed GroveDB root hash is returned for the caller to compare.

use grovedb_element::indexed::{
    decode_count_sort_key, decode_sum_sort_key, encode_count_sort_key, encode_sum_sort_key,
    IndexAxis,
};
use grovedb_merk::{
    proofs::{
        query::{
            verify_aggregate_count_on_range_proof, verify_aggregate_sum_on_range_proof,
            verify_count_offset_on_range_proof, QueryItem as MerkQueryItemForRange,
            QueryProofVerify,
        },
        Query as MerkQuery,
    },
    tree::{axes_digest, combine_hash, combine_hash_three, value_hash, CryptoHash},
};
use grovedb_query::QueryItem as MerkQueryItem;

use crate::{Error, GroveDb};

use super::{
    aggregate_range_out_of_domain, AncestorAttestation, AxisEntries, IndexedAxisAggregateProof,
    IndexedAxisAggregateResult, IndexedAxisPaginatedProof, IndexedAxisPaginatedResult,
    IndexedAxisQueryResult, IndexedAxisRangeProof,
};

/// Walk the verifier-side ancestor chain (depths `last_idx - 1` down to
/// `0`) and return the final reconstructed root hash. Returns the
/// outer GroveDB root hash on success.
pub(super) fn walk_ancestor_chain(
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
pub(super) fn verify_deepest_layer(
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
pub(super) fn execute_single_key_proof(
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

impl GroveDb {
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
        let (envelope, consumed): (IndexedAxisPaginatedProof, _) =
            bincode::decode_from_slice(proof_bytes, config).map_err(|e| {
                Error::CorruptedData(format!("decoding indexed-axis paginated proof: {e}"))
            })?;
        reject_trailing_envelope_bytes(consumed, proof_bytes.len(), "paginated")?;
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

    /// Verify a rank-of-key proof produced by
    /// `prove_indexed_axis_rank_of_key`: the claim "exactly
    /// `expected_rank` entries come strictly before `item_key` in the
    /// directional walk of this axis".
    ///
    /// The proof is an offset-paginated envelope with
    /// `offset = expected_rank, k = 1`. On top of the paginated
    /// verification this additionally requires:
    /// - the attested skipped count equals `expected_rank` exactly (a
    ///   truncated skip would mean the walk has fewer than
    ///   `expected_rank` entries, so no entry can sit at that rank),
    /// - exactly one entry was yielded, and its original key is
    ///   `item_key` (binding the rank to the claimed key rather than
    ///   whatever happens to sit at that position).
    ///
    /// Returns the paginated result whose single entry carries the
    /// item's axis value; `root_hash` must be compared against the
    /// trusted GroveDB root as usual.
    pub fn verify_indexed_axis_rank_of_key(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_axis: IndexAxis,
        item_key: &[u8],
        expected_rank: u64,
        expected_descending: bool,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        let result = Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            expected_axis,
            1,
            expected_rank,
            expected_descending,
        )?;
        if result.skipped != expected_rank {
            return Err(Error::CorruptedData(format!(
                "indexed-axis rank proof: the walk attests only {} entries before the window, \
                 but rank {} was claimed — the walk is shorter than the claimed rank",
                result.skipped, expected_rank
            )));
        }
        let yielded_key: &[u8] = match &result.entries {
            AxisEntries::Count(v) if v.len() == 1 => &v[0].1,
            AxisEntries::Sum(v) if v.len() == 1 => &v[0].1,
            AxisEntries::Avg(v) if v.len() == 1 => &v[0].1,
            other => {
                return Err(Error::CorruptedData(format!(
                    "indexed-axis rank proof: expected exactly one yielded entry at the rank \
                     window, got {}",
                    other.len()
                )));
            }
        };
        if yielded_key != item_key {
            return Err(Error::CorruptedData(format!(
                "indexed-axis rank proof: the entry at rank {} is {}, not the claimed key {}",
                expected_rank,
                hex::encode(yielded_key),
                hex::encode(item_key)
            )));
        }
        Ok(result)
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
        let (envelope, consumed): (IndexedAxisAggregateProof, _) =
            bincode::decode_from_slice(proof_bytes, config).map_err(|e| {
                Error::CorruptedData(format!("decoding indexed-axis aggregate proof: {e}"))
            })?;
        reject_trailing_envelope_bytes(consumed, proof_bytes.len(), "aggregate")?;
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

fn decode_range_envelope(proof_bytes: &[u8]) -> Result<IndexedAxisRangeProof, Error> {
    let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
    let (envelope, consumed): (IndexedAxisRangeProof, _) =
        bincode::decode_from_slice(proof_bytes, config)
            .map_err(|e| Error::CorruptedData(format!("decoding indexed-axis range proof: {e}")))?;
    reject_trailing_envelope_bytes(consumed, proof_bytes.len(), "range")?;
    Ok(envelope)
}

/// Reject an envelope whose decode did not consume the whole buffer.
/// Trailing bytes never change the verified content, but tolerating
/// them makes the proof byte-malleable — two distinct byte strings
/// would verify as the same proof, which breaks any caller that
/// dedups, caches, or consensus-compares proofs by their bytes.
pub(super) fn reject_trailing_envelope_bytes(
    consumed: usize,
    total: usize,
    shape: &'static str,
) -> Result<(), Error> {
    if consumed != total {
        return Err(Error::CorruptedData(format!(
            "indexed-axis {shape} proof has {} trailing byte(s) after the envelope",
            total - consumed
        )));
    }
    Ok(())
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

    // Every axis's secondary binds a count aggregate into its node
    // hashes (count axis: ProvableCountTree; sum and avg axes:
    // ProvableCountProvableSumTree), so all three verify through the
    // count-offset primitive: the skipped prefix is independently
    // re-derived from the counted subtree commitments, making `skipped`
    // cryptographically attested for every axis.
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
            "indexed-axis paginated proof: secondary count-offset proof failed to verify: {e}"
        ))
    })?;
    let entries =
        decode_axis_entries_from_count_offset_items(axis, &count_offset_result.returned_items)?;
    let (secondary_root_hash, skipped) =
        (count_offset_result.root_hash, count_offset_result.skipped);

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

pub(super) fn decode_axis_entries_from_result_set(
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

pub(super) fn decode_axis_entries_from_count_offset_items(
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
            let mut entries: Vec<(i64, Vec<u8>)> = Vec::with_capacity(items.len());
            for it in items {
                if it.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (sum) paginated secondary key shorter than 8 bytes: {:?}",
                        it.key
                    )));
                }
                let mut sum_bytes = [0u8; 8];
                sum_bytes.copy_from_slice(&it.key[..8]);
                entries.push((decode_sum_sort_key(&sum_bytes), it.key[8..].to_vec()));
            }
            Ok(AxisEntries::Sum(entries))
        }
    }
}

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
