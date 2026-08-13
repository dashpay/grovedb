//! Branched (multi-prefix) indexed-axis proofs: one query over N
//! sibling prefix branches, attested by a **single envelope** with a
//! single reconstructed root hash.
//!
//! ## Why this shape exists
//!
//! A compound index lays one indexed tree per prefix value:
//! `…/<prefix name>/<value>/…/<terminal>`. A query that pins several
//! prefix values at once (`prefix IN [a, b, c]`) reads N sibling
//! indexed trees whose paths differ at exactly one segment — the
//! *branching level*. Proving each branch separately duplicates every
//! layer above the branching level N times and forces the caller to
//! cross-check N root hashes. This envelope shares what is shared:
//!
//! - the layers **above** the branching level appear once
//!   (`shared_layer_proofs` + `shared_ancestor_attestations`);
//! - the branching level is **one multi-key Merk proof** binding every
//!   branch's value tree in the shared parent Merk simultaneously
//!   (`branching_layer_proof`);
//! - each branch carries only its own tail: the layers below its
//!   branch key, its primary/other-axes attestations, and its
//!   secondary proof.
//!
//! ## Chain of trust
//!
//! Verification runs strictly upward, per branch first: the branch's
//! secondary proof yields its secondary root hash; the deepest tail
//! layer binds that hash (and the primary root) into the indexed-tree
//! element's recorded `value_hash`; the tail walk reconstructs the
//! branch's value-tree root. That root, composed with the branch
//! element's bytes **from the multi-key proof**, must equal the
//! `value_hash` the multi-key proof recorded for the branch's key — so
//! a branch tail cannot be swapped, duplicated, or re-ordered without
//! the composition failing. The multi-key proof's own root then seeds
//! one shared ancestor walk up to the GroveDB root hash.
//!
//! Everything here reuses the audited single-path building blocks:
//! per-branch envelopes are produced by the existing builders and
//! split at the branching level, and verification reuses
//! [`verify_deepest_layer`] and [`walk_ancestor_chain`] verbatim on
//! the tail and shared windows.

#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::proofs::query::{
    verify_count_offset_on_range_proof, QueryItem as MerkQueryItemForRange,
};
use grovedb_merk::tree::axes_digest;
use grovedb_merk::{
    proofs::{query::QueryProofVerify, Query as MerkQuery},
    tree::{combine_hash, combine_hash_three, value_hash, CryptoHash},
};
#[cfg(feature = "minimal")]
use grovedb_path::SubtreePath;
use grovedb_query::QueryItem as MerkQueryItem;
#[cfg(feature = "minimal")]
use grovedb_storage::StorageBatch;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use crate::util::TxRef;
#[cfg(feature = "minimal")]
use crate::TransactionArg;
use crate::{Error, GroveDb};

use super::verify::{
    decode_axis_entries_from_count_offset_items, decode_axis_entries_from_result_set,
    reject_trailing_envelope_bytes, verify_deepest_layer, walk_ancestor_chain,
};
use super::{
    AncestorAttestation, BranchedProofBranch, IndexedAxisBranchedPaginatedProof,
    IndexedAxisBranchedPaginatedResult, IndexedAxisBranchedQueryResult,
    IndexedAxisBranchedRangeProof,
};

/// Validate the branch-key list shared by the prove and verify entry
/// points: at least two keys (one branch is the single-path envelope's
/// job), pairwise distinct (a duplicated key is one subtree walked
/// twice — a caller error, and it would make branch alignment
/// ambiguous).
fn validate_branch_keys(branch_keys: &[Vec<u8>], err_label: &'static str) -> Result<(), Error> {
    if branch_keys.len() < 2 {
        return Err(Error::InvalidInput(
            "branched indexed-axis proofs require at least two branch keys; use the \
             single-path envelope for one",
        ));
    }
    let mut sorted: Vec<&Vec<u8>> = branch_keys.iter().collect();
    sorted.sort();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::CorruptedData(format!(
            "{err_label}: duplicate branch keys — each branch key names one value tree \
             and may appear once"
        )));
    }
    Ok(())
}

/// Compose an intermediate layer's recorded `value_hash` from its
/// element bytes, its child Merk's root, and its attestation — the
/// same three-way rule [`walk_ancestor_chain`] applies per layer,
/// exposed for the branching-level check where the element bytes come
/// from the multi-key proof instead of a single-key layer proof.
fn compose_intermediate(
    element_bytes: &[u8],
    child_root: &CryptoHash,
    attestation: &AncestorAttestation,
) -> CryptoHash {
    let val_h = value_hash(element_bytes).value().to_owned();
    match attestation {
        AncestorAttestation::NotIndexed => combine_hash(&val_h, child_root).value().to_owned(),
        AncestorAttestation::SingleSecondary(secondary_root) => {
            combine_hash_three(&val_h, child_root, secondary_root)
                .value()
                .to_owned()
        }
        AncestorAttestation::MultiAxis(axes) => {
            let digest = axes_digest(axes).value().to_owned();
            combine_hash_three(&val_h, child_root, &digest)
                .value()
                .to_owned()
        }
    }
}

/// Execute the branching-level multi-key proof: one Merk proof
/// covering every branch key in the shared prefix's Merk. Returns the
/// layer's root hash plus, aligned with `branch_keys`, each key's
/// element bytes and the `value_hash` the parent Merk records for it.
fn execute_multi_key_proof(
    proof_bytes: &[u8],
    branch_keys: &[Vec<u8>],
    err_label: &'static str,
) -> Result<(CryptoHash, Vec<(Vec<u8>, CryptoHash)>), Error> {
    let mut query = MerkQuery::new();
    for key in branch_keys {
        query.insert_item(MerkQueryItem::Key(key.clone()));
    }
    let (root_hash, result) = query
        .execute_proof(proof_bytes, None, true, 0)
        .unwrap()
        .map_err(|e| {
            Error::CorruptedData(format!(
                "{err_label}: branching-level multi-key proof failed to verify: {e}"
            ))
        })?;
    let mut per_key = Vec::with_capacity(branch_keys.len());
    for key in branch_keys {
        let proved = result
            .result_set
            .iter()
            .find(|p| &p.key == key)
            .ok_or_else(|| {
                Error::CorruptedData(format!(
                    "{err_label}: branching-level proof does not contain branch key {}",
                    hex::encode(key)
                ))
            })?;
        let value = proved.value.clone().ok_or_else(|| {
            Error::CorruptedData(format!(
                "{err_label}: branching-level proof carries no value for branch key {}",
                hex::encode(key)
            ))
        })?;
        per_key.push((value, proved.proof));
    }
    Ok((root_hash, per_key))
}

/// The shared upward walk of a branched envelope, after every branch's
/// tail has been verified: check each branch's composition against the
/// multi-key proof's recorded hashes, then chain the shared layers to
/// the GroveDB root.
///
/// `branch_tail_roots[i]` is branch `i`'s reconstructed value-tree
/// root (the output of that branch's tail walk), and
/// `branch_first_attestations[i]` describes branch `i`'s value-tree
/// element.
#[allow(clippy::too_many_arguments)]
fn verify_branching_and_shared_layers(
    shared_layer_proofs: &[Vec<u8>],
    shared_ancestor_attestations: &[AncestorAttestation],
    branching_layer_proof: &[u8],
    branch_keys: &[Vec<u8>],
    path_prefix: &[&[u8]],
    branch_tail_roots: &[CryptoHash],
    branch_first_attestations: &[&AncestorAttestation],
    err_label: &'static str,
) -> Result<CryptoHash, Error> {
    if shared_layer_proofs.len() != path_prefix.len() {
        return Err(Error::CorruptedData(format!(
            "{err_label}: envelope has {} shared layers but the path prefix has {} segments",
            shared_layer_proofs.len(),
            path_prefix.len()
        )));
    }
    if shared_ancestor_attestations.len() != shared_layer_proofs.len() {
        return Err(Error::CorruptedData(format!(
            "{err_label}: envelope has {} shared attestations but {} shared layers",
            shared_ancestor_attestations.len(),
            shared_layer_proofs.len()
        )));
    }

    let (branching_root, per_key) =
        execute_multi_key_proof(branching_layer_proof, branch_keys, err_label)?;
    for (branch, ((element_bytes, recorded_hash), tail_root)) in
        per_key.iter().zip(branch_tail_roots.iter()).enumerate()
    {
        let combined =
            compose_intermediate(element_bytes, tail_root, branch_first_attestations[branch]);
        if combined != *recorded_hash {
            return Err(Error::CorruptedData(format!(
                "{err_label}: branch {branch} (key {}) chain mismatch at the branching \
                 level: the branch tail reconstructs a different subtree than the \
                 multi-key proof commits — a swapped, re-ordered, or foreign branch tail",
                hex::encode(&branch_keys[branch])
            )));
        }
    }

    // The shared walk consumes every shared layer proof: frame the
    // branching-level proof as the (already-consumed) deepest layer so
    // `walk_ancestor_chain`'s indexing lines up, and seed it with the
    // branching Merk's root.
    let mut layers: Vec<Vec<u8>> = Vec::with_capacity(shared_layer_proofs.len() + 1);
    layers.extend_from_slice(shared_layer_proofs);
    layers.push(Vec::new()); // placeholder for the consumed deepest slot
    let mut walk_path: Vec<&[u8]> = path_prefix.to_vec();
    walk_path.push(b""); // aligned placeholder; never read by the walk
    walk_ancestor_chain(
        &layers,
        shared_ancestor_attestations,
        &walk_path,
        branching_root,
        err_label,
    )
}

#[cfg(feature = "minimal")]
impl GroveDb {
    /// Prove one arbitrary secondary query (the same `secondary_query`
    /// and `limit` per branch) over N sibling prefix branches, as one
    /// envelope. The indexed tree of branch `i` lives at
    /// `path_prefix ++ [branch_keys[i]] ++ path_suffix`.
    ///
    /// Verified by [`GroveDb::verify_indexed_axis_query_branched`].
    #[allow(clippy::too_many_arguments)]
    pub fn prove_indexed_axis_query_branched(
        &self,
        path_prefix: &[&[u8]],
        branch_keys: &[Vec<u8>],
        path_suffix: &[&[u8]],
        axis: IndexAxis,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost::default();
        let err_label = "indexed-axis branched range proof";
        cost_return_on_error_no_add!(cost, validate_branch_keys(branch_keys, err_label));
        if path_suffix.is_empty() {
            return Err(Error::InvalidInput(
                "branched indexed-axis proofs require a non-empty path suffix: the indexed \
                 tree lives below the branch key",
            ))
            .wrap_with_cost(cost);
        }
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();
        let branching_depth = path_prefix.len();

        let mut shared_layer_proofs: Option<Vec<Vec<u8>>> = None;
        let mut shared_ancestor_attestations: Option<Vec<AncestorAttestation>> = None;
        let mut branches = Vec::with_capacity(branch_keys.len());
        let mut requested_limit = limit;
        let mut descending = false;
        for key in branch_keys {
            let full_path: Vec<&[u8]> = path_prefix
                .iter()
                .copied()
                .chain(std::iter::once(key.as_slice()))
                .chain(path_suffix.iter().copied())
                .collect();
            let sub_envelope = cost_return_on_error!(
                &mut cost,
                self.build_indexed_axis_range_proof(
                    SubtreePath::from(full_path.as_slice()),
                    axis,
                    secondary_query.clone(),
                    limit,
                    tx_ref,
                    &batch,
                    grove_version,
                )
            );
            requested_limit = sub_envelope.requested_limit;
            descending = sub_envelope.descending;
            let mut layer_proofs = sub_envelope.layer_proofs;
            let mut attestations = sub_envelope.ancestor_attestations;
            let tail_layer_proofs = layer_proofs.split_off(branching_depth + 1);
            let branch_attestations = attestations.split_off(branching_depth);
            if shared_layer_proofs.is_none() {
                layer_proofs.truncate(branching_depth);
                shared_layer_proofs = Some(layer_proofs);
                shared_ancestor_attestations = Some(attestations);
            }
            branches.push(BranchedProofBranch {
                ancestor_attestations: branch_attestations,
                tail_layer_proofs,
                primary_root_hash: sub_envelope.primary_root_hash,
                other_axes_root_hashes: sub_envelope.other_axes_root_hashes,
                target_is_pcpsit: sub_envelope.target_is_pcpsit,
                secondary_proof: sub_envelope.secondary_proof,
            });
        }

        let branching_layer_proof = cost_return_on_error!(
            &mut cost,
            self.build_branching_layer_proof(
                path_prefix,
                branch_keys,
                tx_ref,
                &batch,
                grove_version,
                err_label,
            )
        );

        let envelope = IndexedAxisBranchedRangeProof {
            axis_tag: axis.tag(),
            shared_layer_proofs: shared_layer_proofs.unwrap_or_default(),
            shared_ancestor_attestations: shared_ancestor_attestations.unwrap_or_default(),
            branching_layer_proof,
            branches,
            requested_limit,
            descending,
        };
        let bytes = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(&envelope, bincode::config::standard()).map_err(|e| {
                Error::CorruptedData(format!("encoding indexed-axis branched range proof: {e}"))
            })
        );
        Ok(bytes).wrap_with_cost(cost)
    }

    /// Prove one offset-paginated top-k walk (the same `(k, offset,
    /// descending)` per branch) over N sibling prefix branches, as one
    /// envelope.
    ///
    /// Verified by
    /// [`GroveDb::verify_indexed_axis_top_k_paginated_branched`].
    #[allow(clippy::too_many_arguments)]
    pub fn prove_indexed_axis_top_k_paginated_branched(
        &self,
        path_prefix: &[&[u8]],
        branch_keys: &[Vec<u8>],
        path_suffix: &[&[u8]],
        axis: IndexAxis,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost::default();
        let err_label = "indexed-axis branched paginated proof";
        cost_return_on_error_no_add!(cost, validate_branch_keys(branch_keys, err_label));
        if path_suffix.is_empty() {
            return Err(Error::InvalidInput(
                "branched indexed-axis proofs require a non-empty path suffix: the indexed \
                 tree lives below the branch key",
            ))
            .wrap_with_cost(cost);
        }
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();
        let branching_depth = path_prefix.len();

        let mut shared_layer_proofs: Option<Vec<Vec<u8>>> = None;
        let mut shared_ancestor_attestations: Option<Vec<AncestorAttestation>> = None;
        let mut branches = Vec::with_capacity(branch_keys.len());
        for key in branch_keys {
            let full_path: Vec<&[u8]> = path_prefix
                .iter()
                .copied()
                .chain(std::iter::once(key.as_slice()))
                .chain(path_suffix.iter().copied())
                .collect();
            let sub_envelope = cost_return_on_error!(
                &mut cost,
                self.build_indexed_axis_paginated_proof(
                    SubtreePath::from(full_path.as_slice()),
                    axis,
                    k,
                    offset,
                    descending,
                    tx_ref,
                    &batch,
                    grove_version,
                )
            );
            let mut layer_proofs = sub_envelope.layer_proofs;
            let mut attestations = sub_envelope.ancestor_attestations;
            let tail_layer_proofs = layer_proofs.split_off(branching_depth + 1);
            let branch_attestations = attestations.split_off(branching_depth);
            if shared_layer_proofs.is_none() {
                layer_proofs.truncate(branching_depth);
                shared_layer_proofs = Some(layer_proofs);
                shared_ancestor_attestations = Some(attestations);
            }
            branches.push(BranchedProofBranch {
                ancestor_attestations: branch_attestations,
                tail_layer_proofs,
                primary_root_hash: sub_envelope.primary_root_hash,
                other_axes_root_hashes: sub_envelope.other_axes_root_hashes,
                target_is_pcpsit: sub_envelope.target_is_pcpsit,
                secondary_proof: sub_envelope.secondary_proof,
            });
        }

        let branching_layer_proof = cost_return_on_error!(
            &mut cost,
            self.build_branching_layer_proof(
                path_prefix,
                branch_keys,
                tx_ref,
                &batch,
                grove_version,
                err_label,
            )
        );

        let envelope = IndexedAxisBranchedPaginatedProof {
            axis_tag: axis.tag(),
            shared_layer_proofs: shared_layer_proofs.unwrap_or_default(),
            shared_ancestor_attestations: shared_ancestor_attestations.unwrap_or_default(),
            branching_layer_proof,
            branches,
            requested_k: k,
            requested_offset: offset,
            descending,
        };
        let bytes = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(&envelope, bincode::config::standard()).map_err(|e| {
                Error::CorruptedData(format!(
                    "encoding indexed-axis branched paginated proof: {e}"
                ))
            })
        );
        Ok(bytes).wrap_with_cost(cost)
    }

    /// One multi-key Merk proof at the branching level: proves every
    /// branch key in the Merk at `path_prefix`.
    fn build_branching_layer_proof(
        &self,
        path_prefix: &[&[u8]],
        branch_keys: &[Vec<u8>],
        transaction: &crate::Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
        err_label: &'static str,
    ) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost::default();
        let parent_path: SubtreePath<&[u8]> = path_prefix.into();
        let parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                parent_path,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let mut query = MerkQuery::new();
        for key in branch_keys {
            query.insert_item(MerkQueryItem::Key(key.clone()));
        }
        let result = cost_return_on_error!(
            &mut cost,
            parent_merk
                .prove(query, None, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "{err_label}: prove branching-level multi-key layer: {e}"
                )))
        );
        Ok(result.proof).wrap_with_cost(cost)
    }
}

impl GroveDb {
    /// Verify an [`IndexedAxisBranchedRangeProof`]: the same
    /// `secondary_query` and `limit` executed over every branch,
    /// reconstructing one GroveDB root hash.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_indexed_axis_query_branched(
        proof_bytes: &[u8],
        path_prefix: &[&[u8]],
        branch_keys: &[Vec<u8>],
        path_suffix: &[&[u8]],
        expected_axis: IndexAxis,
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
    ) -> Result<IndexedAxisBranchedQueryResult, Error> {
        let err_label = "indexed-axis branched range proof";
        validate_branch_keys(branch_keys, err_label)?;
        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (envelope, consumed): (IndexedAxisBranchedRangeProof, _) =
            bincode::decode_from_slice(proof_bytes, config).map_err(|e| {
                Error::CorruptedData(format!("decoding indexed-axis branched range proof: {e}"))
            })?;
        reject_trailing_envelope_bytes(consumed, proof_bytes.len(), "branched range")?;
        if envelope.axis_tag != expected_axis.tag() {
            return Err(Error::CorruptedData(format!(
                "{err_label}: axis mismatch: expected {:?} (tag={}), envelope carries tag={}",
                expected_axis,
                expected_axis.tag(),
                envelope.axis_tag
            )));
        }
        if envelope.requested_limit != expected_limit {
            return Err(Error::CorruptedData(format!(
                "{err_label}: limit mismatch: expected {:?}, envelope carries {:?}",
                expected_limit, envelope.requested_limit
            )));
        }
        let expected_descending = !secondary_query.left_to_right;
        if envelope.descending != expected_descending {
            return Err(Error::CorruptedData(format!(
                "{err_label}: direction mismatch: expected descending={expected_descending}, \
                 envelope carries descending={}",
                envelope.descending
            )));
        }
        if envelope.branches.len() != branch_keys.len() {
            return Err(Error::CorruptedData(format!(
                "{err_label}: envelope carries {} branches; the request resolves to {}",
                envelope.branches.len(),
                branch_keys.len()
            )));
        }

        let left_to_right = secondary_query.left_to_right;
        let mut branch_entries = Vec::with_capacity(envelope.branches.len());
        let mut branch_tail_roots = Vec::with_capacity(envelope.branches.len());
        let mut branch_first_attestations = Vec::with_capacity(envelope.branches.len());
        for (branch_index, branch) in envelope.branches.iter().enumerate() {
            let (secondary_root_hash, sec_result) = secondary_query
                .clone()
                .execute_proof(
                    &branch.secondary_proof,
                    envelope.requested_limit,
                    left_to_right,
                    0,
                )
                .unwrap()
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "{err_label}: branch {branch_index} secondary proof failed to \
                         verify: {e}"
                    ))
                })?;
            let entries =
                decode_axis_entries_from_result_set(expected_axis, &sec_result.result_set)?;
            let tail_root = verify_branch_tail(
                branch,
                path_suffix,
                &secondary_root_hash,
                expected_axis,
                err_label,
            )?;
            branch_entries.push(entries);
            branch_tail_roots.push(tail_root);
            branch_first_attestations.push(&branch.ancestor_attestations[0]);
        }

        let root_hash = verify_branching_and_shared_layers(
            &envelope.shared_layer_proofs,
            &envelope.shared_ancestor_attestations,
            &envelope.branching_layer_proof,
            branch_keys,
            path_prefix,
            &branch_tail_roots,
            &branch_first_attestations,
            err_label,
        )?;
        Ok(IndexedAxisBranchedQueryResult {
            root_hash,
            branches: branch_entries,
        })
    }

    /// Verify an [`IndexedAxisBranchedPaginatedProof`]: the same
    /// `(k, offset, descending)` walk executed over every branch,
    /// reconstructing one GroveDB root hash. Per-branch `skipped`
    /// carries the same attested-skip semantics as the single-path
    /// paginated verifier.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_indexed_axis_top_k_paginated_branched(
        proof_bytes: &[u8],
        path_prefix: &[&[u8]],
        branch_keys: &[Vec<u8>],
        path_suffix: &[&[u8]],
        expected_axis: IndexAxis,
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
    ) -> Result<IndexedAxisBranchedPaginatedResult, Error> {
        let err_label = "indexed-axis branched paginated proof";
        validate_branch_keys(branch_keys, err_label)?;
        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (envelope, consumed): (IndexedAxisBranchedPaginatedProof, _) =
            bincode::decode_from_slice(proof_bytes, config).map_err(|e| {
                Error::CorruptedData(format!(
                    "decoding indexed-axis branched paginated proof: {e}"
                ))
            })?;
        reject_trailing_envelope_bytes(consumed, proof_bytes.len(), "branched paginated")?;
        if envelope.axis_tag != expected_axis.tag() {
            return Err(Error::CorruptedData(format!(
                "{err_label}: axis mismatch: expected {:?} (tag={}), envelope carries tag={}",
                expected_axis,
                expected_axis.tag(),
                envelope.axis_tag
            )));
        }
        if envelope.requested_k != expected_k {
            return Err(Error::CorruptedData(format!(
                "{err_label}: k mismatch: expected {expected_k}, envelope carries {}",
                envelope.requested_k
            )));
        }
        if envelope.requested_offset != expected_offset {
            return Err(Error::CorruptedData(format!(
                "{err_label}: offset mismatch: expected {expected_offset}, envelope carries {}",
                envelope.requested_offset
            )));
        }
        if envelope.descending != expected_descending {
            return Err(Error::CorruptedData(format!(
                "{err_label}: direction mismatch: expected descending={expected_descending}, \
                 envelope carries descending={}",
                envelope.descending
            )));
        }
        if envelope.branches.len() != branch_keys.len() {
            return Err(Error::CorruptedData(format!(
                "{err_label}: envelope carries {} branches; the request resolves to {}",
                envelope.branches.len(),
                branch_keys.len()
            )));
        }

        let inner_range = MerkQueryItemForRange::RangeFull(std::ops::RangeFull);
        let mut branch_pages = Vec::with_capacity(envelope.branches.len());
        let mut branch_tail_roots = Vec::with_capacity(envelope.branches.len());
        let mut branch_first_attestations = Vec::with_capacity(envelope.branches.len());
        for (branch_index, branch) in envelope.branches.iter().enumerate() {
            let count_offset_result = verify_count_offset_on_range_proof(
                &branch.secondary_proof,
                &inner_range,
                envelope.requested_offset,
                Some(envelope.requested_k as u64),
                !envelope.descending,
            )
            .unwrap()
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "{err_label}: branch {branch_index} secondary count-offset proof failed \
                     to verify: {e}"
                ))
            })?;
            let entries = decode_axis_entries_from_count_offset_items(
                expected_axis,
                &count_offset_result.returned_items,
            )?;
            let tail_root = verify_branch_tail(
                branch,
                path_suffix,
                &count_offset_result.root_hash,
                expected_axis,
                err_label,
            )?;
            branch_pages.push((count_offset_result.skipped, entries));
            branch_tail_roots.push(tail_root);
            branch_first_attestations.push(&branch.ancestor_attestations[0]);
        }

        let root_hash = verify_branching_and_shared_layers(
            &envelope.shared_layer_proofs,
            &envelope.shared_ancestor_attestations,
            &envelope.branching_layer_proof,
            branch_keys,
            path_prefix,
            &branch_tail_roots,
            &branch_first_attestations,
            err_label,
        )?;
        Ok(IndexedAxisBranchedPaginatedResult {
            root_hash,
            branches: branch_pages,
        })
    }
}

/// Verify one branch's tail — the deepest-layer binding of its
/// secondary root plus the walk up to (but not including) the
/// branching level — returning the branch's value-tree root hash for
/// the branching-level composition check.
fn verify_branch_tail(
    branch: &BranchedProofBranch,
    path_suffix: &[&[u8]],
    secondary_root_hash: &CryptoHash,
    axis: IndexAxis,
    err_label: &'static str,
) -> Result<CryptoHash, Error> {
    if branch.tail_layer_proofs.len() != path_suffix.len() {
        return Err(Error::CorruptedData(format!(
            "{err_label}: branch has {} tail layers but the path suffix has {} segments",
            branch.tail_layer_proofs.len(),
            path_suffix.len()
        )));
    }
    if branch.ancestor_attestations.len() != path_suffix.len() {
        return Err(Error::CorruptedData(format!(
            "{err_label}: branch has {} attestations but the path suffix has {} segments \
             (one per layer from the branch's value tree down to the indexed tree's \
             parent)",
            branch.ancestor_attestations.len(),
            path_suffix.len()
        )));
    }
    let initial_root = verify_deepest_layer(
        &branch.tail_layer_proofs,
        path_suffix,
        &branch.primary_root_hash,
        secondary_root_hash,
        axis,
        &branch.other_axes_root_hashes,
        branch.target_is_pcpsit,
        err_label,
    )?;
    walk_ancestor_chain(
        &branch.tail_layer_proofs,
        &branch.ancestor_attestations[1..],
        path_suffix,
        initial_root,
        err_label,
    )
}
