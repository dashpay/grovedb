//! The prover: single-key layer proofs down the path, per-ancestor
//! attestations, and the per-shape envelope builders.
//!
//! Everything here runs against live storage under a transaction. The
//! verifier ([`super::verify`]) must be able to reconstruct the GroveDB
//! root hash from nothing but the envelope these builders emit.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::get::ElementFetchFromStorageExtensions,
    proofs::{encode_into, query::QueryItem as MerkQueryItemForRange, Query as MerkQuery},
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_query::{AggregateFold, QueryItem as MerkQueryItem};
use grovedb_storage::StorageBatch;
use grovedb_version::version::GroveVersion;

use crate::{util::TxRef, Element, Error, GroveDb, Transaction, TransactionArg};

use super::{
    verify::{count_aggregate_inner_range, sum_aggregate_inner_range},
    AncestorAttestation, IndexedAxisAggregateProof, IndexedAxisPaginatedProof,
    IndexedAxisRangeProof,
};
use crate::operations::proof::AxisDescentProof;

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
                    // Structurally unreachable: the outer arm already
                    // bound this value as PCIT or PSIT. Return a graceful
                    // error rather than panicking if that invariant is
                    // ever broken by a refactor.
                    _ => {
                        return Err(Error::CorruptedCodeExecution(
                            "build_ancestor_attestations: element matched PCIT/PSIT in the \
                             outer arm but neither in the inner axis match",
                        ))
                        .wrap_with_cost(cost);
                    }
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
        grovedb_version::check_grovedb_v0_with_cost!(
            "prove_indexed_axis_top_k",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .prove_single_path
        );
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
        grovedb_version::check_grovedb_v0_with_cost!(
            "prove_indexed_axis_query",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .prove_single_path
        );
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
    /// Every axis's secondary carries a hash-bound count aggregate
    /// (every axis is a dual-aggregate `ProvableCountProvableSumTree`),
    /// so the secondary proof always uses
    /// `Merk::prove_count_offset_on_range` — the skipped prefix is
    /// attested by counted subtree commitments (`HashWithCountAndSum`)
    /// instead of enumeration, giving
    /// O(log n + k) proof size regardless of `offset`.
    ///
    /// Ties (equal axis values) break by `original_key` in walk
    /// direction, in both the skipped prefix and the yielded window —
    /// the secondary is keyed `(axis_sort_key ‖ original_key)` so the
    /// walk order is total and deterministic.
    ///
    /// `offset` past the end of the walk is provable: the prover skips
    /// everything it can and yields nothing; the verifier reports the
    /// attested `skipped < offset`, which together with the root-bound
    /// count commitments is a proof that the total population is
    /// exactly `skipped`.
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
        grovedb_version::check_grovedb_v0_with_cost!(
            "prove_indexed_axis_top_k_paginated",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .prove_single_path
        );
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

    /// Compute the rank of `item_key` in the directional walk over the
    /// per-axis secondary of the indexed tree at `path` — the count of
    /// entries strictly before it, read in O(log n) off the secondary's
    /// count aggregates. Shared by the rank proof (which then attests
    /// the rank via a paginated envelope at `offset = rank, k = 1`) and
    /// the trusted read path. The returned rank has no cryptographic
    /// guarantee on its own.
    ///
    /// Errors with `InvalidPath` at the root or on a non-indexed
    /// target, and `PathKeyNotFound` when `item_key` is not in the
    /// indexed primary.
    pub(crate) fn compute_indexed_axis_rank_of_key<'b, B, P>(
        &self,
        path: P,
        axis: IndexAxis,
        item_key: &[u8],
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<u64, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        use crate::operations::indexed_tree::make_axis_secondary_key;

        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let path_keys: Vec<Vec<u8>> = path.to_vec();
        if path_keys.is_empty() {
            return Err(Error::InvalidPath(
                "cannot compute an indexed-axis rank at the root path".to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // 1. Read the item's element from the primary to derive its
        //    secondary sort key (the walk position is a pure function of
        //    the entry's (count, sum) aggregates plus its key).
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path.clone(), tx_ref, Some(&batch), grove_version)
        );
        if !primary_merk.tree_type.is_indexed_primary() {
            return Err(Error::InvalidPath(
                "indexed-axis rank requires the path's last segment to be an indexed-tree \
                 element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let item_element = cost_return_on_error!(
            &mut cost,
            Element::get(&primary_merk, item_key, true, grove_version).map_err(|e| {
                Error::PathKeyNotFound(format!(
                    "indexed-axis rank: item key {} not found in the indexed primary: {e}",
                    hex::encode(item_key)
                ))
            })
        );
        let (count, sum) = item_element.count_sum_value_or_default();
        let secondary_key = make_axis_secondary_key(axis, count, sum, item_key);

        // 2. Compute the rank: the count of entries strictly before the
        //    item in the directional walk, read O(log n) off the
        //    secondary's count aggregates.
        let (secondary_root_key, _, _) = cost_return_on_error!(
            &mut cost,
            read_queried_axis_info_with_path_keys(
                self,
                &path_keys,
                axis,
                tx_ref,
                &batch,
                grove_version,
                "indexed-axis rank",
            )
        );
        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_indexed_secondary_at_path(
                path.clone(),
                axis,
                secondary_root_key,
                tx_ref,
                Some(&batch),
                grove_version,
            )
        );
        let before_range = if descending {
            // Descending walk: everything with a strictly GREATER
            // secondary key comes first.
            MerkQueryItemForRange::RangeAfter(secondary_key.clone()..)
        } else {
            // Ascending walk: everything with a strictly SMALLER
            // secondary key comes first.
            MerkQueryItemForRange::RangeTo(..secondary_key.clone())
        };
        let rank = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .count_aggregate_on_range(&before_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "indexed-axis rank: counting entries before the item: {e}"
                )))
        );
        Ok(rank).wrap_with_cost(cost)
    }

    /// Prove that `item_key` sits at a specific rank in the directional
    /// walk of an indexed axis: rank `R` (0-based) means exactly `R`
    /// entries come strictly before it in the walk. Ties (equal axis
    /// values) are broken by `original_key` in walk direction — the
    /// same total order every other axis proof uses — so the rank is
    /// well-defined even inside a tie group.
    ///
    /// Returns `(proof_bytes, rank)`. The proof is an ordinary
    /// offset-paginated envelope with `offset = rank, k = 1`: the count
    /// commitments attest that exactly `rank` entries precede the
    /// single yielded entry, and the yielded entry's key binds the
    /// claim to `item_key`. Verify with
    /// [`Self::verify_indexed_axis_rank_of_key`], which additionally
    /// checks the yielded entry is `item_key` and the attested skip is
    /// exactly `rank`.
    ///
    /// Errors if `item_key` is not present in the indexed tree's
    /// primary, or if the axis is not indexed at this path.
    pub fn prove_indexed_axis_rank_of_key<'b, B, P>(
        &self,
        path: P,
        axis: IndexAxis,
        item_key: &[u8],
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(Vec<u8>, u64), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        grovedb_version::check_grovedb_v0_with_cost!(
            "prove_indexed_axis_rank_of_key",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .prove_single_path
        );
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();

        // Steps 1-2 (derive the item's secondary sort key, count the
        // entries strictly before it in the walk direction) are shared
        // with the trusted read path — see
        // `compute_indexed_axis_rank_of_key`.
        let rank = cost_return_on_error!(
            &mut cost,
            self.compute_indexed_axis_rank_of_key(
                path.clone(),
                axis,
                item_key,
                descending,
                transaction,
                grove_version,
            )
        );

        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        // 3. The rank proof IS the paginated proof at (offset = rank,
        //    k = 1): its counted commitments attest the skipped prefix
        //    and its single yielded entry binds the item.
        let envelope = cost_return_on_error!(
            &mut cost,
            self.build_indexed_axis_paginated_proof(
                path,
                axis,
                1,
                rank,
                descending,
                tx_ref,
                &batch,
                grove_version,
            )
        );
        let bytes = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(&envelope, bincode::config::standard()).map_err(|e| {
                Error::CorruptedData(format!("encoding indexed-axis rank proof: {e}"))
            })
        );

        Ok((bytes, rank)).wrap_with_cost(cost)
    }

    /// Generate an aggregate proof over a value-range against an
    /// indexed-tree's per-axis secondary.
    ///
    /// Only [`IndexAxis::Count`] and [`IndexAxis::Sum`] are supported.
    /// [`IndexAxis::Avg`] returns [`Error::NotSupported`] — averaging
    /// averages over a range is not a closed-form aggregate (callers
    /// should compute it client-side from
    /// `indexed_count_aggregate_over_value_range` + `indexed_sum_aggregate_over_value_range`
    /// against the same path).
    ///
    /// `lo > hi` is a degenerate range; the proof commits `0`.
    pub fn prove_indexed_axis_aggregate_over_value_range<'b, B, P>(
        &self,
        path: P,
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        fold: AggregateFold,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        grovedb_version::check_grovedb_v0_with_cost!(
            "prove_indexed_axis_aggregate_over_value_range",
            grove_version
                .grovedb_versions
                .operations
                .indexed_axis
                .prove_single_path
        );
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
                fold,
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
        let mut sec_result = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .prove_without_encoding(secondary_query, limit, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "indexed-axis range proof: secondary range proof: {e}"
                )))
        );
        // Resolve canonical rows into the primary values they point at,
        // before encoding, so the verified result carries the referenced
        // Element rather than the reference.
        cost_return_on_error!(
            &mut cost,
            super::reference_resolution::resolve_axis_reference_nodes(
                sec_result.proof.iter_mut(),
                axis,
                &primary_merk,
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
            )
        );
        let mut serialized_secondary = Vec::with_capacity(128);
        encode_into(sec_result.proof.iter(), &mut serialized_secondary);

        Ok(IndexedAxisRangeProof {
            axis_tag: axis.tag(),
            layer_proofs,
            primary_root_hash,
            ancestor_attestations,
            other_axes_root_hashes,
            target_is_pcpsit,
            secondary_proof: serialized_secondary,
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

        // 4. Open the per-axis secondary and emit the count-offset
        //    paginated proof. The gate is structural, not per-axis:
        //    `Merk::prove_count_offset_on_range` rejects any host whose
        //    tree type does not bind a count aggregate into node hashes,
        //    so an axis whose secondary somehow lacked counts would fail
        //    here rather than silently degrade to enumeration.
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
        if !secondary_merk.tree_type.is_count_bearing() {
            return Err(Error::NotSupported(format!(
                "indexed-axis paginated proof: the {axis:?} axis secondary ({:?}) does not carry \
                 a provable count aggregate, so offset pagination cannot be attested",
                secondary_merk.tree_type
            )))
            .wrap_with_cost(cost);
        }
        let inner_range = MerkQueryItemForRange::RangeFull(std::ops::RangeFull);
        let mut prove_result = cost_return_on_error!(
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
        // Resolve canonical rows into the primary values they point at,
        // BEFORE encoding — the count-offset path used to return here
        // without a reference post-pass, which is why it rejected
        // reference rows outright.
        cost_return_on_error!(
            &mut cost,
            super::reference_resolution::resolve_axis_reference_nodes(
                prove_result.ops.iter_mut(),
                axis,
                &primary_merk,
                self,
                &path_keys,
                transaction,
                batch,
                grove_version,
            )
        );
        let mut serialized = Vec::with_capacity(128);
        encode_into(prove_result.ops.iter(), &mut serialized);

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
        fold: AggregateFold,
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
                "prove_indexed_axis_aggregate_over_value_range requires the path's last segment to be an \
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
        let serialized = cost_return_on_error_no_add!(
            cost,
            build_aggregate_secondary_proof(
                &secondary_merk,
                axis,
                lo,
                hi,
                fold,
                grove_version,
                &mut cost,
            )
        );

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
            fold_tag: fold.tag(),
        })
        .wrap_with_cost(cost)
    }

    /// Build the [`AxisDescentProof`] payload for an axis-ordered read
    /// of the indexed tree at `path` — the embedded (V1-envelope) form
    /// of an axis proof. Unlike the standalone envelope builders above,
    /// no path-walk layers or ancestor attestations are collected here:
    /// in the V1 envelope those are ordinary layers of the general
    /// proof walk, and this payload covers only the indexed element's
    /// own axis read.
    ///
    /// The traversal is taken from the query and NOT echoed into the
    /// payload (the V1 envelope's query-as-input philosophy); the one
    /// exception is `RankOfKey`, whose computed rank must travel so the
    /// verifier can drive the count-offset verification walk — the
    /// count commitments then attest it.
    pub(crate) fn build_axis_descent_payload<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        axis_query: &grovedb_query::AxisQuery,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<AxisDescentProof, Error> {
        use grovedb_query::AxisTraversal;

        let mut cost = OperationCost::default();
        let axis = axis_query.axis;

        let path_keys: Vec<Vec<u8>> = path.to_vec();
        if path_keys.is_empty() {
            return Err(Error::InvalidPath(
                "cannot build an axis descent at the root path".to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // Per-axis validation + secondary root key + non-queried axes.
        let (secondary_root_key, other_axes_root_hashes, target_is_pcpsit) = cost_return_on_error!(
            &mut cost,
            read_queried_axis_info_with_path_keys(
                self,
                &path_keys,
                axis,
                transaction,
                batch,
                grove_version,
                "axis descent",
            )
        );

        // Primary root hash (an empty primary commits NULL_HASH
        // naturally).
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
                "axis descent requires the path's last segment to be an indexed-tree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let (primary_root_hash, _, _) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(|e| Error::CorruptedData(format!("axis descent: primary root hash: {e}")))
        );

        // Rank first (it opens its own merks), so the secondary borrow
        // below doesn't overlap.
        let rank = match &axis_query.traversal {
            AxisTraversal::RankOfKey { key } => Some(cost_return_on_error!(
                &mut cost,
                self.compute_indexed_axis_rank_of_key(
                    path.clone(),
                    axis,
                    key,
                    axis_query.descending,
                    Some(transaction),
                    grove_version,
                )
            )),
            _ => None,
        };

        // The secondary proof for the query's traversal.
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
        let secondary_proof = match &axis_query.traversal {
            AxisTraversal::RankedPage { k, offset } => {
                cost_return_on_error_no_add!(
                    cost,
                    build_paginated_secondary_proof(
                        &secondary_merk,
                        &primary_merk,
                        self,
                        &path_keys,
                        transaction,
                        batch,
                        *offset,
                        *k,
                        axis_query.descending,
                        axis,
                        grove_version,
                        &mut cost,
                    )
                )
            }
            AxisTraversal::RankOfKey { .. } => {
                // rank computed above; the rank proof IS the paginated
                // proof at (offset = rank, k = 1).
                let rank_offset = rank.expect("set above for RankOfKey");
                cost_return_on_error_no_add!(
                    cost,
                    build_paginated_secondary_proof(
                        &secondary_merk,
                        &primary_merk,
                        self,
                        &path_keys,
                        transaction,
                        batch,
                        rank_offset,
                        1,
                        axis_query.descending,
                        axis,
                        grove_version,
                        &mut cost,
                    )
                )
            }
            AxisTraversal::Bounded { limit, .. } => {
                // An empty secondary cannot carry a Merk range proof
                // (`Merk::prove` refuses empty trees), so — mirroring
                // the count-offset and aggregate-on-range empty
                // conventions — the payload carries EMPTY proof bytes,
                // which the verifier resolves to a NULL_HASH secondary
                // root. The parent binding then only passes if the
                // element genuinely commits an empty secondary.
                if cost_return_on_error!(&mut cost, secondary_merk.is_empty_tree().map(Ok)) {
                    Vec::new()
                } else {
                    let secondary_query = cost_return_on_error_no_add!(
                        cost,
                        crate::query::axis_lowering::axis_bounded_merk_query(axis_query)
                    );
                    let mut sec_result = cost_return_on_error!(
                        &mut cost,
                        secondary_merk
                            .prove_without_encoding(secondary_query, Some(*limit), grove_version)
                            .map_err(|e| Error::CorruptedData(format!(
                                "axis descent: secondary range proof: {e}"
                            )))
                    );
                    cost_return_on_error!(
                        &mut cost,
                        super::reference_resolution::resolve_axis_reference_nodes(
                            sec_result.proof.iter_mut(),
                            axis,
                            &primary_merk,
                            self,
                            &path_keys,
                            transaction,
                            batch,
                            grove_version,
                        )
                    );
                    let mut serialized = Vec::with_capacity(128);
                    encode_into(sec_result.proof.iter(), &mut serialized);
                    serialized
                }
            }
            AxisTraversal::AggregateOverValueRange { lo, hi, fold } => {
                // classify() rejects wholly-out-of-domain ranges and the
                // Avg axis before a descent is ever built; the builder
                // still fails closed on both.
                cost_return_on_error_no_add!(
                    cost,
                    build_aggregate_secondary_proof(
                        &secondary_merk,
                        axis,
                        *lo,
                        *hi,
                        *fold,
                        grove_version,
                        &mut cost,
                    )
                )
            }
        };

        Ok(AxisDescentProof {
            axis_tag: axis.tag(),
            target_is_pcpsit,
            other_axes_root_hashes,
            primary_root_hash,
            rank,
            secondary_proof,
        })
        .wrap_with_cost(cost)
    }
}

/// The count-offset paginated secondary proof shared by the `TopK` and
/// `RankOfKey` embedded traversals — the same shape step 4 of
/// `build_indexed_axis_paginated_proof` emits.
#[allow(clippy::too_many_arguments)]
fn build_paginated_secondary_proof<'db, S>(
    secondary_merk: &grovedb_merk::Merk<S>,
    primary_merk: &grovedb_merk::Merk<S>,
    grovedb: &'db GroveDb,
    primary_path: &[Vec<u8>],
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    offset: u64,
    k: u16,
    descending: bool,
    axis: IndexAxis,
    grove_version: &GroveVersion,
    cost: &mut OperationCost,
) -> Result<Vec<u8>, Error>
where
    S: grovedb_storage::StorageContext<'db>,
{
    if !secondary_merk.tree_type.is_count_bearing() {
        return Err(Error::NotSupported(format!(
            "axis descent: the {axis:?} axis secondary ({:?}) does not carry a provable count \
             aggregate, so offset pagination cannot be attested",
            secondary_merk.tree_type
        )));
    }
    let inner_range = MerkQueryItemForRange::RangeFull(std::ops::RangeFull);
    let mut prove_result = secondary_merk
        .prove_count_offset_on_range(
            &inner_range,
            offset,
            Some(k as u64),
            !descending,
            grove_version,
        )
        .unwrap_add_cost(cost)
        .map_err(|e| {
            Error::CorruptedData(format!("axis descent: secondary count-offset proof: {e}"))
        })?;
    // Resolve canonical rows into the primary values they point at,
    // before encoding.
    super::reference_resolution::resolve_axis_reference_nodes(
        prove_result.ops.iter_mut(),
        axis,
        primary_merk,
        grovedb,
        primary_path,
        transaction,
        batch,
        grove_version,
    )
    .unwrap_add_cost(cost)?;
    let mut serialized = Vec::with_capacity(128);
    encode_into(prove_result.ops.iter(), &mut serialized);
    Ok(serialized)
}

/// The secondary-side aggregate proof for
/// [`AxisTraversal::AggregateOverValueRange`]: the byte range follows
/// the AXIS (its sort-key encoding, via the same
/// [`count_aggregate_inner_range`] / [`sum_aggregate_inner_range`]
/// reconstructors the verifier uses, so the two sides cannot drift on
/// clamping, degenerate, or out-of-domain shapes), and the walker
/// follows the FOLD ([`AggregateFold::Population`] proves the count
/// aggregate, [`AggregateFold::Total`] the sum aggregate).
///
/// Every (Count/Sum, fold) cell is served: all secondaries are
/// dual-aggregate `ProvableCountProvableSumTree`s, the count axis
/// mirroring its `count_value` into the sum half (issue #806). `Avg`
/// is refused for both folds, mirroring `AxisQuery::validate`.
fn build_aggregate_secondary_proof<'db, S>(
    secondary_merk: &grovedb_merk::Merk<S>,
    axis: IndexAxis,
    lo: i128,
    hi: i128,
    fold: AggregateFold,
    grove_version: &GroveVersion,
    cost: &mut OperationCost,
) -> Result<Vec<u8>, Error>
where
    S: grovedb_storage::StorageContext<'db>,
{
    let inner_range = match axis {
        IndexAxis::Count => count_aggregate_inner_range(lo, hi),
        IndexAxis::Sum => sum_aggregate_inner_range(lo, hi),
        IndexAxis::Avg => {
            return Err(Error::NotSupported(
                "value-range aggregates are not defined for the Avg axis".to_string(),
            ));
        }
    };
    let ops = match fold {
        AggregateFold::Population => {
            secondary_merk
                .prove_aggregate_count_on_range(&inner_range, grove_version)
                .unwrap_add_cost(cost)
                .map_err(|e| {
                    Error::CorruptedData(format!("indexed-axis population-over-range proof: {e}"))
                })?
                .0
        }
        AggregateFold::Total => {
            secondary_merk
                .prove_aggregate_sum_on_range(&inner_range, grove_version)
                .unwrap_add_cost(cost)
                .map_err(|e| {
                    Error::CorruptedData(format!("indexed-axis total-over-range proof: {e}"))
                })?
                .0
        }
    };
    let mut serialized = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut serialized);
    Ok(serialized)
}
