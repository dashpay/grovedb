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
use grovedb_query::{AggregateFold, QueryItem as MerkQueryItem};
use grovedb_version::{check_grovedb_v0, version::GroveVersion};

use crate::{operations::MAX_REFERENCE_HOPS, Element, Error, GroveDb, IndexedAxisEntry};

use super::{
    aggregate_range_out_of_domain, AncestorAttestation, AxisEntries, IndexedAxisAggregateProof,
    IndexedAxisAggregateResult, IndexedAxisPaginatedProof, IndexedAxisPaginatedResult,
    IndexedAxisQueryResult, IndexedAxisRangeProof, IndexedTargetCommitment, IndexedTargetWitness,
};

#[derive(Debug)]
pub(crate) struct ProvenAxisRow<T> {
    ordering_value: T,
    primary_key: Vec<u8>,
    row_bytes: Vec<u8>,
    row_value_hash: CryptoHash,
}

#[derive(Debug)]
pub(crate) enum ProvenAxisRows {
    Count(Vec<ProvenAxisRow<u64>>),
    Sum(Vec<ProvenAxisRow<i64>>),
    Avg(Vec<ProvenAxisRow<i128>>),
}

impl ProvenAxisRows {
    pub(crate) fn empty_for_axis(axis: IndexAxis) -> Self {
        match axis {
            IndexAxis::Count => Self::Count(Vec::new()),
            IndexAxis::Sum => Self::Sum(Vec::new()),
            IndexAxis::Avg => Self::Avg(Vec::new()),
        }
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

    let queried_axis_digest = recompute_axis_binding_digest(
        &cidx_value_bytes,
        axis,
        secondary_root_hash,
        other_axes_root_hashes,
        target_is_pcpsit,
        err_label,
    )?;

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

/// The attestation-binding core shared by the standalone envelopes and
/// the embedded V1 axis descent: check the proved element's family
/// against the requested axis, then recompute the third
/// `combine_hash_three` input — the queried secondary's (recomputed)
/// root hash for PCIT / PSIT, or the `axes_digest` over
/// `other_axes_root_hashes` + the queried axis's recomputed root for
/// PCPSIT.
///
/// The caller performs the `combine_hash_three(H(value), primary_root,
/// returned_digest)` comparison against whatever parent-committed hash
/// its envelope carries.
pub(crate) fn recompute_axis_binding_digest(
    element_value_bytes: &[u8],
    axis: IndexAxis,
    secondary_root_hash: &[u8; 32],
    other_axes_root_hashes: &[(u8, [u8; 32])],
    target_is_pcpsit: bool,
    err_label: &'static str,
) -> Result<CryptoHash, Error> {
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
        let proved_family =
            grovedb_element::ElementType::from_serialized_value(element_value_bytes)
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

    Ok(queried_axis_digest)
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

struct VerifiedTargetNode {
    value_bytes: Vec<u8>,
    element: Element,
    recorded_value_hash: Option<CryptoHash>,
}

fn verify_indexed_target_witness(
    witness: &IndexedTargetWitness,
    indexed_path: &[&[u8]],
    primary_key: &[u8],
    grove_root_hash: &CryptoHash,
    grove_version: &GroveVersion,
) -> Result<(Element, CryptoHash, Element), Error> {
    if witness.nodes.is_empty() {
        return Err(Error::CorruptedData(
            "indexed target witness contains no nodes".to_string(),
        ));
    }
    if witness.nodes.len() > MAX_REFERENCE_HOPS + 1 {
        return Err(Error::ReferenceLimit);
    }

    let mut current_path: Vec<Vec<u8>> = indexed_path
        .iter()
        .map(|segment| segment.to_vec())
        .collect();
    current_path.push(primary_key.to_vec());

    let mut seen_paths = std::collections::HashSet::new();
    let mut verified_nodes = Vec::with_capacity(witness.nodes.len());
    for (index, node) in witness.nodes.iter().enumerate() {
        if !seen_paths.insert(current_path.clone()) {
            return Err(Error::CyclicReference);
        }
        let recorded_value_hash = match (index, &node.authentication) {
            (0, None) => None,
            (0, Some(_)) => {
                return Err(Error::CorruptedData(
                    "indexed target witness redundantly authenticates its immediate primary node"
                        .to_string(),
                ));
            }
            (_, None) => {
                return Err(Error::CorruptedData(format!(
                    "indexed reference-chain target node {index} has no root authentication"
                )));
            }
            (_, Some(authentication)) => {
                if authentication.layer_proofs.len() != current_path.len() {
                    return Err(Error::CorruptedData(format!(
                        "indexed reference-chain target node {index} has {} layer proofs for a \
                         {}-segment path",
                        authentication.layer_proofs.len(),
                        current_path.len()
                    )));
                }
                let key = current_path.last().expect("initial path is non-empty");
                let deepest = authentication.layer_proofs.len() - 1;
                let (proved_value, node_parent_root, recorded_value_hash) =
                    execute_single_key_proof(
                        &authentication.layer_proofs[deepest],
                        key,
                        "indexed reference-chain target",
                    )?;
                if proved_value != node.value {
                    return Err(Error::CorruptedData(format!(
                        "indexed reference-chain target node {index} value differs from its root \
                         proof"
                    )));
                }
                let path_slices: Vec<&[u8]> = current_path.iter().map(Vec::as_slice).collect();
                let reconstructed_root = walk_ancestor_chain(
                    &authentication.layer_proofs,
                    &authentication.ancestor_attestations,
                    &path_slices,
                    node_parent_root,
                    "indexed reference-chain target",
                )?;
                if reconstructed_root != *grove_root_hash {
                    return Err(Error::CorruptedData(format!(
                        "indexed reference-chain target node {index} reconstructs GroveDB root \
                         {}, expected {}",
                        hex::encode(reconstructed_root),
                        hex::encode(grove_root_hash)
                    )));
                }
                Some(recorded_value_hash)
            }
        };
        let element = Element::deserialize(&node.value, grove_version).map_err(|e| {
            Error::CorruptedData(format!(
                "indexed target witness node {index} contains an invalid element: {e}"
            ))
        })?;

        match element.underlying() {
            Element::Reference(reference_path, ..)
            | Element::ReferenceWithSumItem(reference_path, ..) => {
                if index + 1 == witness.nodes.len() {
                    return Err(Error::CorruptedData(
                        "indexed target witness terminates at a reference".to_string(),
                    ));
                }
                let Some((key, parent_segments)) = current_path.split_last() else {
                    return Err(Error::CorruptedData(
                        "indexed target witness reference has an empty path".to_string(),
                    ));
                };
                let parent_builder: grovedb_path::SubtreePathBuilder<Vec<u8>> =
                    grovedb_path::SubtreePathBuilder::owned_from_iter(
                        parent_segments.iter().cloned(),
                    );
                current_path = reference_path
                    .clone()
                    .absolute_qualified_path(parent_builder, key)
                    .map_err(Error::ElementError)?
                    .to_vec();
            }
            _ if index + 1 != witness.nodes.len() => {
                return Err(Error::CorruptedData(format!(
                    "indexed target witness continues after terminal node {index}"
                )));
            }
            _ => {}
        }
        verified_nodes.push(VerifiedTargetNode {
            value_bytes: node.value.clone(),
            element,
            recorded_value_hash,
        });
    }

    let mut committed_hashes = vec![[0u8; 32]; verified_nodes.len()];
    for index in (0..verified_nodes.len()).rev() {
        let wire_node = &witness.nodes[index];
        let verified = &verified_nodes[index];
        let serialized_hash = value_hash(&verified.value_bytes).value().to_owned();
        let underlying = verified.element.underlying();
        let expected_hash = match &wire_node.commitment {
            IndexedTargetCommitment::Simple => {
                if underlying.is_reference() || underlying.is_any_tree() {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness node {index} claims a simple commitment for {}",
                        underlying.type_str()
                    )));
                }
                if index + 1 != verified_nodes.len() {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness continues after terminal node {index}"
                    )));
                }
                serialized_hash
            }
            IndexedTargetCommitment::Layered(child_root_hash) => {
                if !underlying.is_any_tree() || underlying.is_indexed_tree() {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness node {index} claims a layered commitment for {}",
                        underlying.type_str()
                    )));
                }
                if index + 1 != verified_nodes.len() {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness continues after terminal node {index}"
                    )));
                }
                combine_hash(&serialized_hash, child_root_hash)
                    .value()
                    .to_owned()
            }
            IndexedTargetCommitment::IndexedSingle {
                primary_root_hash,
                secondary_root_hash,
            } => {
                if !matches!(
                    underlying,
                    Element::ProvableCountIndexedTree(..) | Element::ProvableSumIndexedTree(..)
                ) {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness node {index} claims a single-axis indexed \
                         commitment for {}",
                        underlying.type_str()
                    )));
                }
                if index + 1 != verified_nodes.len() {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness continues after terminal node {index}"
                    )));
                }
                combine_hash_three(&serialized_hash, primary_root_hash, secondary_root_hash)
                    .value()
                    .to_owned()
            }
            IndexedTargetCommitment::IndexedMulti {
                primary_root_hash,
                axes,
            } => {
                let Element::ProvableCountProvableSumIndexedTree(_, _, _, configured_axes, _) =
                    underlying
                else {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness node {index} claims a multi-axis indexed \
                         commitment for {}",
                        underlying.type_str()
                    )));
                };
                if axes.len() != configured_axes.len()
                    || axes
                        .iter()
                        .zip(configured_axes)
                        .any(|((got_tag, _), (want_tag, _))| got_tag != want_tag)
                {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness node {index} axes do not match its PCPSIT element"
                    )));
                }
                if index + 1 != verified_nodes.len() {
                    return Err(Error::CorruptedData(format!(
                        "indexed target witness continues after terminal node {index}"
                    )));
                }
                let digest = axes_digest(axes).value().to_owned();
                combine_hash_three(&serialized_hash, primary_root_hash, &digest)
                    .value()
                    .to_owned()
            }
            IndexedTargetCommitment::Reference => {
                match underlying {
                    Element::Reference(..) | Element::ReferenceWithSumItem(..) => {}
                    _ => {
                        return Err(Error::CorruptedData(format!(
                            "indexed target witness node {index} claims a reference commitment \
                             for {}",
                            underlying.type_str()
                        )));
                    }
                }
                let Some(terminal_hash) = committed_hashes.last() else {
                    return Err(Error::CorruptedData(
                        "indexed target witness terminates at a reference".to_string(),
                    ));
                };
                combine_hash(&serialized_hash, terminal_hash)
                    .value()
                    .to_owned()
            }
        };
        if let Some(recorded_value_hash) = verified.recorded_value_hash
            && expected_hash != recorded_value_hash
        {
            return Err(Error::CorruptedData(format!(
                "indexed reference-chain target node {index} commitment mismatch: computed {}, \
                 root proof records {}",
                hex::encode(expected_hash),
                hex::encode(recorded_value_hash)
            )));
        }
        committed_hashes[index] = expected_hash;
    }

    let immediate = verified_nodes.first().expect("checked non-empty");
    let terminal = verified_nodes.last().expect("checked non-empty");
    if terminal.element.underlying().is_reference() {
        return Err(Error::CorruptedData(
            "indexed target witness did not resolve to a terminal value".to_string(),
        ));
    }
    Ok((
        immediate.element.clone(),
        committed_hashes[0],
        terminal.element.clone().into_underlying(),
    ))
}

fn resolve_axis_rows<T>(
    axis: IndexAxis,
    rows: Vec<ProvenAxisRow<T>>,
    witnesses: &[IndexedTargetWitness],
    indexed_path: &[&[u8]],
    grove_root_hash: &CryptoHash,
    grove_version: &GroveVersion,
) -> Result<Vec<IndexedAxisEntry<T>>, Error> {
    if rows.len() != witnesses.len() {
        return Err(Error::CorruptedData(format!(
            "indexed-axis proof returned {} secondary rows but carries {} target witnesses",
            rows.len(),
            witnesses.len()
        )));
    }
    let mut entries = Vec::with_capacity(rows.len());
    for (row, witness) in rows.into_iter().zip(witnesses) {
        let (immediate, immediate_value_hash, terminal) = verify_indexed_target_witness(
            witness,
            indexed_path,
            &row.primary_key,
            grove_root_hash,
            grove_version,
        )?;
        let (count, sum) = immediate.count_sum_value_or_default();
        let expected_row =
            grovedb_element::canonical_axis_reference(axis, &row.primary_key, count, sum)
                .map_err(Error::ElementError)?;
        let expected_row_bytes = expected_row
            .serialize(grove_version)
            .map_err(Error::ElementError)?;
        if row.row_bytes != expected_row_bytes {
            return Err(Error::CorruptedData(format!(
                "indexed-axis secondary row for primary key {} is not the canonical one-hop \
                 reference",
                hex::encode(&row.primary_key)
            )));
        }
        let row_hash = value_hash(&row.row_bytes).value().to_owned();
        let expected_secondary_value_hash = combine_hash(&row_hash, &immediate_value_hash)
            .value()
            .to_owned();
        if row.row_value_hash != expected_secondary_value_hash {
            return Err(Error::CorruptedData(format!(
                "indexed-axis secondary reference for primary key {} is stale or bound to the \
                 wrong immediate primary value hash",
                hex::encode(&row.primary_key)
            )));
        }
        entries.push(IndexedAxisEntry {
            ordering_value: row.ordering_value,
            primary_key: row.primary_key,
            value: terminal,
        });
    }
    Ok(entries)
}

pub(crate) fn resolve_indexed_axis_rows(
    rows: ProvenAxisRows,
    witnesses: &[IndexedTargetWitness],
    indexed_path: &[&[u8]],
    grove_root_hash: &CryptoHash,
    grove_version: &GroveVersion,
) -> Result<AxisEntries, Error> {
    match rows {
        ProvenAxisRows::Count(rows) => Ok(AxisEntries::Count(resolve_axis_rows(
            IndexAxis::Count,
            rows,
            witnesses,
            indexed_path,
            grove_root_hash,
            grove_version,
        )?)),
        ProvenAxisRows::Sum(rows) => Ok(AxisEntries::Sum(resolve_axis_rows(
            IndexAxis::Sum,
            rows,
            witnesses,
            indexed_path,
            grove_root_hash,
            grove_version,
        )?)),
        ProvenAxisRows::Avg(rows) => Ok(AxisEntries::Avg(resolve_axis_rows(
            IndexAxis::Avg,
            rows,
            witnesses,
            indexed_path,
            grove_root_hash,
            grove_version,
        )?)),
    }
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
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        check_grovedb_v0!(
            "verify_indexed_axis_top_k",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .verify_single_path
        );
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
        verify_indexed_axis_range_inner(envelope, full_range, expected_axis, path, grove_version)
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
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        check_grovedb_v0!(
            "verify_indexed_axis_query",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .verify_single_path
        );
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
        verify_indexed_axis_range_inner(
            envelope,
            secondary_query,
            expected_axis,
            path,
            grove_version,
        )
    }

    /// Verify an `IndexedAxisPaginatedProof`-shaped paginated proof.
    pub fn verify_indexed_axis_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_axis: IndexAxis,
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        check_grovedb_v0!(
            "verify_indexed_axis_top_k_paginated",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .verify_single_path
        );
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
        verify_indexed_axis_paginated_inner(envelope, expected_axis, path, grove_version)
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
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        let result = Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            expected_axis,
            1,
            expected_rank,
            expected_descending,
            grove_version,
        )?;
        if result.skipped != expected_rank {
            return Err(Error::CorruptedData(format!(
                "indexed-axis rank proof: the walk attests only {} entries before the window, \
                 but rank {} was claimed — the walk is shorter than the claimed rank",
                result.skipped, expected_rank
            )));
        }
        let yielded_key: &[u8] = match &result.entries {
            AxisEntries::Count(v) if v.len() == 1 => &v[0].primary_key,
            AxisEntries::Sum(v) if v.len() == 1 => &v[0].primary_key,
            AxisEntries::Avg(v) if v.len() == 1 => &v[0].primary_key,
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
    pub fn verify_indexed_axis_aggregate_over_value_range(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_axis: IndexAxis,
        expected_lo: i128,
        expected_hi: i128,
        expected_fold: AggregateFold,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisAggregateResult, Error> {
        check_grovedb_v0!(
            "verify_indexed_axis_aggregate_over_value_range",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .verify_single_path
        );
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
        if envelope.fold_tag != expected_fold.tag() {
            return Err(Error::CorruptedData(format!(
                "indexed-axis aggregate proof fold mismatch: expected {expected_fold} \
                 (tag={}), envelope carries tag={} — a population proof cannot answer a \
                 question about a total, or vice versa",
                expected_fold.tag(),
                envelope.fold_tag
            )));
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
        verify_indexed_axis_aggregate_inner(envelope, expected_axis, expected_fold, path)
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
fn reject_trailing_envelope_bytes(
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
    grove_version: &GroveVersion,
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

    let rows = decode_axis_entries_from_result_set(axis, &sec_result.result_set)?;

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
    let entries = resolve_indexed_axis_rows(
        rows,
        &envelope.target_witnesses,
        path,
        &root_hash,
        grove_version,
    )?;

    Ok(IndexedAxisQueryResult { root_hash, entries })
}

fn verify_indexed_axis_paginated_inner(
    envelope: IndexedAxisPaginatedProof,
    axis: IndexAxis,
    path: &[&[u8]],
    grove_version: &GroveVersion,
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
    // hashes (every axis is a dual-aggregate
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
    let rows =
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
    let entries = resolve_indexed_axis_rows(
        rows,
        &envelope.target_witnesses,
        path,
        &root_hash,
        grove_version,
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
    fold: AggregateFold,
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

    // The byte range follows the AXIS; the walker follows the FOLD —
    // the same split the prover's `build_aggregate_secondary_proof`
    // makes, sharing these exact range reconstructors so the two sides
    // cannot drift on clamping, degenerate, or out-of-domain shapes.
    let inner_range = match axis {
        IndexAxis::Count => count_aggregate_inner_range(envelope.lo, envelope.hi),
        IndexAxis::Sum => sum_aggregate_inner_range(envelope.lo, envelope.hi),
        IndexAxis::Avg => {
            return Err(Error::NotSupported(
                "indexed-axis aggregate proofs are not defined for the Avg axis".to_string(),
            ));
        }
    };
    let (secondary_root_hash, aggregate_value) = match fold {
        AggregateFold::Population => {
            let (root, count) =
                verify_aggregate_count_on_range_proof(&envelope.secondary_proof, &inner_range)
                    .unwrap()
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "indexed-axis aggregate proof: secondary population proof \
                             failed to verify: {e}"
                        ))
                    })?;
            (root, count as i128)
        }
        AggregateFold::Total => {
            let (root, sum) =
                verify_aggregate_sum_on_range_proof(&envelope.secondary_proof, &inner_range)
                    .unwrap()
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "indexed-axis aggregate proof: secondary total proof failed \
                             to verify: {e}"
                        ))
                    })?;
            (root, sum as i128)
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

pub(crate) fn decode_axis_entries_from_result_set(
    axis: IndexAxis,
    result_set: &[grovedb_merk::proofs::query::ProvedKeyOptionalValue],
) -> Result<ProvenAxisRows, Error> {
    match axis {
        IndexAxis::Count => {
            let mut entries = Vec::with_capacity(result_set.len());
            for proved in result_set {
                if proved.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (count) secondary key shorter than 8 bytes: {:?}",
                        proved.key
                    )));
                }
                let mut count_bytes = [0u8; 8];
                count_bytes.copy_from_slice(&proved.key[..8]);
                entries.push(ProvenAxisRow {
                    ordering_value: decode_count_sort_key(&count_bytes),
                    primary_key: proved.key[8..].to_vec(),
                    row_bytes: proved.value.clone().ok_or_else(|| {
                        Error::CorruptedData(
                            "indexed-axis secondary proof omitted a returned row value".to_string(),
                        )
                    })?,
                    row_value_hash: proved.proof,
                });
            }
            Ok(ProvenAxisRows::Count(entries))
        }
        IndexAxis::Sum => {
            let mut entries = Vec::with_capacity(result_set.len());
            for proved in result_set {
                if proved.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (sum) secondary key shorter than 8 bytes: {:?}",
                        proved.key
                    )));
                }
                let mut sum_bytes = [0u8; 8];
                sum_bytes.copy_from_slice(&proved.key[..8]);
                entries.push(ProvenAxisRow {
                    ordering_value: decode_sum_sort_key(&sum_bytes),
                    primary_key: proved.key[8..].to_vec(),
                    row_bytes: proved.value.clone().ok_or_else(|| {
                        Error::CorruptedData(
                            "indexed-axis secondary proof omitted a returned row value".to_string(),
                        )
                    })?,
                    row_value_hash: proved.proof,
                });
            }
            Ok(ProvenAxisRows::Sum(entries))
        }
        IndexAxis::Avg => {
            let mut entries = Vec::with_capacity(result_set.len());
            for proved in result_set {
                if proved.key.len() < 16 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (avg) secondary key shorter than 16 bytes: {:?}",
                        proved.key
                    )));
                }
                let mut avg_bytes = [0u8; 16];
                avg_bytes.copy_from_slice(&proved.key[..16]);
                entries.push(ProvenAxisRow {
                    ordering_value: grovedb_element::indexed::decode_avg_sort_key(&avg_bytes),
                    primary_key: proved.key[16..].to_vec(),
                    row_bytes: proved.value.clone().ok_or_else(|| {
                        Error::CorruptedData(
                            "indexed-axis secondary proof omitted a returned row value".to_string(),
                        )
                    })?,
                    row_value_hash: proved.proof,
                });
            }
            Ok(ProvenAxisRows::Avg(entries))
        }
    }
}

pub(crate) fn decode_axis_entries_from_count_offset_items(
    axis: IndexAxis,
    items: &[grovedb_merk::proofs::query::CountOffsetReturnedItem],
) -> Result<ProvenAxisRows, Error> {
    match axis {
        IndexAxis::Count => {
            let mut entries = Vec::with_capacity(items.len());
            for it in items {
                if it.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (count) paginated secondary key shorter than 8 bytes: {:?}",
                        it.key
                    )));
                }
                let mut count_bytes = [0u8; 8];
                count_bytes.copy_from_slice(&it.key[..8]);
                entries.push(ProvenAxisRow {
                    ordering_value: decode_count_sort_key(&count_bytes),
                    primary_key: it.key[8..].to_vec(),
                    row_bytes: it.value.clone(),
                    row_value_hash: it.value_hash,
                });
            }
            Ok(ProvenAxisRows::Count(entries))
        }
        IndexAxis::Avg => {
            let mut entries = Vec::with_capacity(items.len());
            for it in items {
                if it.key.len() < 16 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (avg) paginated secondary key shorter than 16 bytes: {:?}",
                        it.key
                    )));
                }
                let mut avg_bytes = [0u8; 16];
                avg_bytes.copy_from_slice(&it.key[..16]);
                entries.push(ProvenAxisRow {
                    ordering_value: grovedb_element::indexed::decode_avg_sort_key(&avg_bytes),
                    primary_key: it.key[16..].to_vec(),
                    row_bytes: it.value.clone(),
                    row_value_hash: it.value_hash,
                });
            }
            Ok(ProvenAxisRows::Avg(entries))
        }
        IndexAxis::Sum => {
            let mut entries = Vec::with_capacity(items.len());
            for it in items {
                if it.key.len() < 8 {
                    return Err(Error::CorruptedData(format!(
                        "indexed-axis (sum) paginated secondary key shorter than 8 bytes: {:?}",
                        it.key
                    )));
                }
                let mut sum_bytes = [0u8; 8];
                sum_bytes.copy_from_slice(&it.key[..8]);
                entries.push(ProvenAxisRow {
                    ordering_value: decode_sum_sort_key(&sum_bytes),
                    primary_key: it.key[8..].to_vec(),
                    row_bytes: it.value.clone(),
                    row_value_hash: it.value_hash,
                });
            }
            Ok(ProvenAxisRows::Sum(entries))
        }
    }
}

pub(crate) fn count_aggregate_inner_range(lo: i128, hi: i128) -> MerkQueryItemForRange {
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

pub(crate) fn sum_aggregate_inner_range(lo: i128, hi: i128) -> MerkQueryItemForRange {
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
