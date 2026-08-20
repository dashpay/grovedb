//! Generate proof operations

use std::collections::BTreeMap;

use grovedb_bulk_append_tree::BulkAppendTreeProof;
use grovedb_commitment_tree::COMMITMENT_TREE_DATA_KEY;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_into,
    cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_dense_fixed_sized_merkle_tree::DenseTreeProof;
use grovedb_merk::{
    proofs::{encode_into, query::QueryItem, Node, Op},
    tree::{combine_hash, value_hash},
    Merk, ProofWithoutEncodingResult, TreeFeatureType,
};
use grovedb_merkle_mountain_range::MmrTreeProof;
use grovedb_storage::{Storage, StorageContext};
use grovedb_version::{
    check_grovedb_v0_or_v1_with_cost, check_grovedb_v0_with_cost, version::GroveVersion,
};

#[cfg(feature = "proof_debug")]
use crate::query_result_type::QueryResultType;
use crate::{
    operations::proof::{
        util::hex_to_ascii, GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof,
        MerkOnlyLayerProof, ProofBytes, ProveOptions,
    },
    query::PathTrunkChunkQuery,
    reference_path::path_from_reference_path_type,
    Element, Error, GroveDb, PathQuery, Transaction,
};

impl GroveDb {
    /// Prove one or more path queries.
    /// If we have more than one path query, we merge into a single path query
    /// before proving.
    pub fn prove_query_many(
        &self,
        query: Vec<&PathQuery>,
        prove_options: Option<ProveOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        check_grovedb_v0_with_cost!(
            "prove_query_many",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .prove_query_many
        );
        if query.is_empty() {
            return Err(Error::InvalidInput(
                "prove_query_many called with empty query vector",
            ))
            .wrap_with_cost(OperationCost::default());
        }
        if query.len() > 1 {
            let query = cost_return_on_error_default!(PathQuery::merge(query, grove_version));
            self.prove_query(&query, prove_options, grove_version)
        } else {
            self.prove_query(query[0], prove_options, grove_version)
        }
    }

    /// Generate a minimalistic proof for a given path query.
    /// Doesn't allow for subset verification.
    /// Proofs generated with this can only be verified by the path query used
    /// to generate them.
    ///
    /// Version dispatch happens in `prove_query_non_serialized`.
    pub fn prove_query(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        check_grovedb_v0_with_cost!(
            "prove_query",
            grove_version.grovedb_versions.operations.proof.prove_query
        );
        let mut cost = OperationCost::default();
        let proof = cost_return_on_error!(
            &mut cost,
            self.prove_query_non_serialized(path_query, prove_options, grove_version)
        );
        #[cfg(feature = "proof_debug")]
        {
            println!("constructed proof is {}", proof);
        }
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let encoded_proof = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(proof, config)
                .map_err(|e| Error::CorruptedData(format!("unable to encode proof {}", e)))
        );
        Ok(encoded_proof).wrap_with_cost(cost)
    }

    /// Generates a proof and does not serialize the result.
    ///
    /// Dispatches to v0 or v1 based on the version.
    pub fn prove_query_non_serialized(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<GroveDBProof, Error> {
        // Read-mode dispatch. Axis shapes are served by the V1 envelope
        // — validated here (classify runs the full shape grammar) and
        // gated below once `prove_version` is known. Sum-budget shapes
        // have no proof form yet. Anything malformed fails closed
        // rather than being misread as key selection.
        let (is_axis_shape, is_sum_budget_shape) =
            if path_query.query.query.has_read_mode_anywhere() {
                match path_query.classify() {
                    Ok(crate::PathQueryShape::AxisRead { .. })
                    | Ok(crate::PathQueryShape::BranchedAxisRead { .. }) => (true, false),
                    Ok(crate::PathQueryShape::SumBudget { .. }) => (false, true),
                    Ok(_) => {
                        return Err(Error::CorruptedCodeExecution(
                            "a read-mode-bearing query classified as a non-read-mode shape",
                        ))
                        .wrap_with_cost(OperationCost::default());
                    }
                    Err(e) => return Err(e).wrap_with_cost(OperationCost::default()),
                }
            } else {
                (false, false)
            };
        // Aggregate-count gate: validate at entry so malformed ACOR
        // queries (invalid inner range, ACOR-hidden-in-subquery, etc.) are
        // rejected up front instead of being skipped when the recursive
        // prover never reaches the ACOR-bearing leaf — for example because
        // the path doesn't exist. Without this gate, `prove_query` would
        // happily return a regular path/absence proof for an invalid
        // aggregate-count request.
        let is_acor_query = path_query
            .query
            .query
            .has_aggregate_count_on_range_anywhere();
        if is_acor_query && let Err(e) = path_query.validate_aggregate_count_on_range() {
            return Err(e).wrap_with_cost(OperationCost::default());
        }
        // Mirror of the count gate for sum. Same defense-in-depth: catch
        // malformed `AggregateSumOnRange` shapes up front so the prover
        // never silently returns a regular proof for a path that doesn't
        // exist.
        let is_asor_query = path_query.query.query.has_aggregate_sum_on_range_anywhere();
        if is_asor_query && let Err(e) = path_query.validate_aggregate_sum_on_range() {
            return Err(e).wrap_with_cost(OperationCost::default());
        }
        // Combined-aggregate gate (mirror of the ACOR / ASOR gates).
        // Catch malformed `AggregateCountAndSumOnRange` shapes up front
        // so the prover never silently returns a regular proof for an
        // invalid combined-aggregate request.
        let is_acasor_query = path_query
            .query
            .query
            .has_aggregate_count_and_sum_on_range_anywhere();
        if is_acasor_query && let Err(e) = path_query.validate_aggregate_count_and_sum_on_range() {
            return Err(e).wrap_with_cost(OperationCost::default());
        }

        let prove_version = grove_version
            .grovedb_versions
            .operations
            .proof
            .prove_query_non_serialized;

        // AggregateCountOnRange requires V1 proof envelopes. The legacy
        // V0 (`MerkOnlyLayerProof`) envelope predates ACOR and is only
        // produced by grove versions that pre-date Dash Platform v12;
        // refusing the combination here keeps callers from accidentally
        // emitting a V0 ACOR proof that the verifier would (correctly)
        // reject.
        // Axis shapes are V1-envelope-only, and a V4 capability: refuse
        // the V0 envelope (same contract as the aggregate gates below)
        // and pre-V4 versions up front, mirroring the verifier's
        // envelope gate so both sides agree at every version.
        if is_axis_shape {
            if prove_version == 0 {
                return Err(Error::NotSupported(
                    "axis-ordered path queries require V1 proof envelopes; upgrade the grove \
                     version producing the proof"
                        .to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            }
            if grove_version
                .grovedb_versions
                .operations
                .proof
                .axis_descent_in_v1_envelope
                != 1
            {
                return Err(Error::NotSupported(
                    "axis-ordered descents in the V1 proof envelope are not emitted at this \
                     grove version"
                        .to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            }
        }

        // Sum-budget shapes mirror the axis gates: V1 envelope only, and
        // a GROVE_V4 capability on both sides.
        if is_sum_budget_shape {
            if prove_version == 0 {
                return Err(Error::NotSupported(
                    "sum-budget path queries require V1 proof envelopes; upgrade the grove \
                     version producing the proof"
                        .to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            }
            if grove_version
                .grovedb_versions
                .operations
                .proof
                .sum_budget_in_v1_envelope
                != 1
            {
                return Err(Error::NotSupported(
                    "sum-budget windows in the V1 proof envelope are not emitted at this \
                     grove version"
                        .to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            }
        }

        if is_acor_query && prove_version == 0 {
            return Err(Error::NotSupported(
                "AggregateCountOnRange proofs require V1 proof envelopes; upgrade the grove \
                 version producing the proof"
                    .to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }

        // Mirror of the count V0 gate for sum. `AggregateSumOnRange`
        // postdates V0 envelopes for the same reason as count, so a V0
        // aggregate-sum proof can never be honestly produced; refuse
        // the combination here so callers see a clear `NotSupported`
        // instead of a downstream verifier rejection.
        if is_asor_query && prove_version == 0 {
            return Err(Error::NotSupported(
                "AggregateSumOnRange proofs require V1 proof envelopes; upgrade the grove \
                 version producing the proof"
                    .to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }

        // Combined-aggregate proofs are a grove v3+ feature; V0 envelopes
        // predate them and cannot legitimately carry one. Same contract
        // as the ACOR / ASOR V0 gates above. V0 proofs are **LOCKED** —
        // combined aggregates live on V1 only.
        if is_acasor_query && prove_version == 0 {
            return Err(Error::NotSupported(
                "AggregateCountAndSumOnRange proofs require V1 proof envelopes; upgrade the \
                 grove version producing the proof"
                    .to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }

        match prove_version {
            0 => self.prove_query_non_serialized_v0(path_query, prove_options, grove_version),
            1 => self.prove_query_non_serialized_v1(path_query, prove_options, grove_version),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "prove_query_non_serialized".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }

    /// Helper for the top-level count-offset gate in
    /// `prove_query_non_serialized_v{0,1}`. Opens the merk at
    /// `path_query.path` and confirms its `tree_type` is one of the
    /// two count-bearing flavors. Run only when the caller has set a
    /// non-zero offset *and* the syntactic gate
    /// (`validate_count_offset_paginated`) already passed.
    ///
    /// Why this lives at the top entry rather than only at the
    /// leaf-level short-circuit: for an empty NormalTree at the
    /// target path, the descent inside `prove_subqueries_v{0,1}`
    /// hits the empty-tree arm and *doesn't* recurse into the leaf
    /// merk, so the leaf-level tree-type check never fires. Doing it
    /// here gives callers a clear up-front error in that case.
    ///
    /// Error contract: any failure to resolve `path_query.path` to an
    /// eligible merk surfaces as `Error::InvalidQuery`. We don't
    /// forward the raw `open_transactional_merk_at_path` error because
    /// it can leak storage-layer specifics (missing-path,
    /// path-not-a-tree, corrupted-link, etc.) — from the caller's
    /// point of view all of those have the same actionable meaning
    /// here: "you can't run a count-offset query against this path",
    /// and the single `InvalidQuery` covers all of them uniformly.
    /// Storage-layer or hardware-IO errors still flow through but get
    /// classified the same way; that's acceptable because the
    /// alternative — surfacing them as `MerkError` / `CorruptedData`
    /// from a purely syntactic gate — gives callers an unstable
    /// error contract that depends on whether the merk happens to
    /// exist.
    /// Build the [`SumBudgetWindowProof`] payload for the sum-budget
    /// read at `target_path` (which is `path_query.path` — classify
    /// admits the sum-budget node at the query root only).
    ///
    /// Runs the budget walk with the **provable** fold semantics (skip
    /// non-sum elements, ignore references — the two behaviors a window
    /// proof can replay deterministically; reference targets live
    /// outside the window and cannot be) to learn the scanned window
    /// size and stop condition, then emits an ordinary Merk proof over
    /// exactly that window: limited to the window size when a stop
    /// condition fired, unlimited when the walk exhausted the ranges
    /// (so the proof itself attests exhaustion).
    fn build_sum_budget_window_payload(
        &self,
        target_path: &[&[u8]],
        path_query: &PathQuery,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<crate::operations::proof::SumBudgetWindowProof, Error> {
        use grovedb_merk::proofs::query::{AggregateSumQuery, ReadMode};

        use crate::element::aggregate_sum_query::{
            AggregateSumQueryOptions, ElementAggregateSumQueryExtensions,
        };

        let mut cost = OperationCost::default();

        let node = &path_query.query.query;
        let Some(ReadMode::SumBudget(budget)) = node.read_mode.as_deref() else {
            return Err(Error::CorruptedCodeExecution(
                "sum-budget window build without a root sum-budget read",
            ))
            .wrap_with_cost(cost);
        };

        // 1. Run the budget walk with the provable fold semantics.
        let aggregate_sum_path_query = crate::AggregateSumPathQuery {
            path: target_path.iter().map(|segment| segment.to_vec()).collect(),
            aggregate_sum_query: AggregateSumQuery {
                items: node.items.clone(),
                left_to_right: node.left_to_right,
                sum_limit: budget.sum_limit,
                limit_of_items_to_check: budget.match_limit,
            },
        };
        let provable_options = AggregateSumQueryOptions {
            allow_cache: true,
            error_if_intermediate_path_tree_not_present: true,
            error_if_non_sum_item_found: false,
            ignore_references: true,
        };
        let walk = cost_return_on_error!(
            &mut cost,
            Element::get_aggregate_sum_query(
                &self.db,
                &aggregate_sum_path_query,
                provable_options,
                Some(transaction),
                grove_version,
            )
        );

        // 2. Determine the stop condition the verifier will replay.
        let mut remaining: i64 = cost_return_on_error_no_add!(
            cost,
            i64::try_from(budget.sum_limit)
                .map_err(|_| Error::InvalidQuery("sum-budget limit must fit in i64"))
        );
        for (_, value) in &walk.results {
            remaining = remaining.saturating_sub(*value);
        }
        let budget_reached = remaining <= 0;
        let match_limit_reached = budget
            .match_limit
            .is_some_and(|limit| walk.results.len() >= limit as usize);
        let exhausted = !budget_reached && !match_limit_reached && !walk.hard_limit_reached;

        // 3. Emit the Merk window proof with the query's own items.
        let target_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                target_path.into(),
                transaction,
                None,
                grove_version
            )
        );
        let mut window_query = grovedb_merk::proofs::Query::new_with_direction(node.left_to_right);
        window_query.items = node.items.clone();
        let window_limit = if exhausted {
            None
        } else {
            Some(walk.elements_scanned)
        };
        let proof_result = cost_return_on_error!(
            &mut cost,
            target_merk
                .prove(window_query, window_limit, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "sum-budget window: merk proof over the scanned window: {e}"
                )))
        );

        Ok(crate::operations::proof::SumBudgetWindowProof {
            exhausted,
            window_len: walk.elements_scanned,
            merk_proof: proof_result.proof,
        })
        .wrap_with_cost(cost)
    }

    fn check_count_offset_target_tree_type(
        &self,
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        use grovedb_merk::TreeType as MerkTreeType;
        let mut cost = OperationCost::default();
        let tx = self.start_transaction();
        let path_slices: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();
        let open_result = self
            .open_transactional_merk_at_path(
                path_slices.as_slice().into(),
                &tx,
                None,
                grove_version,
            )
            .unwrap_add_cost(&mut cost);
        let target = match open_result {
            Ok(t) => t,
            Err(_e) => {
                return Err(Error::InvalidQuery(
                    "count-offset paginated queries are only valid against \
                     ProvableCountTree / ProvableCountSumTree / ProvableCountProvableSumTree \
                     merks; the target path could not be resolved to an eligible merk",
                ))
                .wrap_with_cost(cost);
            }
        };
        if !matches!(
            target.tree_type,
            MerkTreeType::ProvableCountTree
                | MerkTreeType::ProvableCountSumTree
                | MerkTreeType::ProvableCountProvableSumTree
        ) {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries are only valid against \
                 ProvableCountTree / ProvableCountSumTree / ProvableCountProvableSumTree merks",
            ))
            .wrap_with_cost(cost);
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// V0: Generates a Merk-only proof without serialization.
    ///
    /// ╔══════════════════════════════════════════════════════════════════╗
    /// ║                  ⚠⚠⚠  DO NOT MODIFY V0 PROOFS  ⚠⚠⚠               ║
    /// ╠══════════════════════════════════════════════════════════════════╣
    /// ║ V0 is a **shipped wire format**. Live grove versions v1 and v2   ║
    /// ║ produce and verify V0 proofs in production (see                  ║
    /// ║ `grovedb-version` — `prove_query_non_serialized: 0` for both).   ║
    /// ║ ANY change to the bytes V0 produces — adding new accepted        ║
    /// ║ query shapes, accepting offsets that were previously rejected,   ║
    /// ║ emitting new node variants, anything — silently changes what     ║
    /// ║ deployed validators accept and is a consensus-breaking change.   ║
    /// ║                                                                   ║
    /// ║ New proof features go on V1 (`prove_query_non_serialized_v1`     ║
    /// ║ in this file, `verify_layer_proof_v1` in verify.rs) and a fresh  ║
    /// ║ `GroveVersion` that selects them. The V0 entry points must keep  ║
    /// ║ behaving exactly as they did when v1/v2 shipped, including       ║
    /// ║ rejecting every input v1/v2 rejected.                            ║
    /// ║                                                                   ║
    /// ║ If you find yourself wanting to "just adjust" something here:    ║
    /// ║ STOP. Add the feature to V1 and bump the grove version instead.  ║
    /// ╚══════════════════════════════════════════════════════════════════╝
    pub(crate) fn prove_query_non_serialized_v0(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<GroveDBProof, Error> {
        let mut cost = OperationCost::default();

        let prove_options = prove_options.unwrap_or_default();

        if path_query.query.offset.is_some() && path_query.query.offset != Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not have offsets",
            ))
            .wrap_with_cost(cost);
        }

        if path_query.query.limit == Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not be for limit 0",
            ))
            .wrap_with_cost(cost);
        }

        #[cfg(feature = "proof_debug")]
        {
            // We want to query raw because we want the references to not be
            // resolved at this point. This is purely for debugging — if
            // query_raw fails (e.g. because the path contains non-Merk tree
            // types like DenseTree, MmrTree, etc.), we just print the error
            // and continue with proof generation.
            let values_result = self.query_raw(
                path_query,
                false,
                prove_options.decrease_limit_on_empty_sub_query_result,
                false,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            );
            match values_result.value() {
                Ok(values) => {
                    println!("values are {}", values.0);

                    let precomputed_result_map = self
                        .query_raw(
                            path_query,
                            false,
                            prove_options.decrease_limit_on_empty_sub_query_result,
                            false,
                            QueryResultType::QueryPathKeyElementTrioResultType,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("query_raw should succeed if it succeeded above")
                        .0
                        .to_btree_map_level_results();
                    println!("precomputed results are {}", precomputed_result_map);
                }
                Err(e) => {
                    println!(
                        "proof_debug: query_raw failed (non-Merk tree in path?): {}",
                        e
                    );
                }
            }
        }

        let mut limit = path_query.query.limit;

        let root_layer = cost_return_on_error!(
            &mut cost,
            self.prove_subqueries(
                vec![],
                path_query,
                &mut limit,
                &prove_options,
                0,
                grove_version
            )
        );

        Ok(GroveDBProof::V0(GroveDBProofV0 {
            root_layer,
            prove_options,
        }))
        .wrap_with_cost(cost)
    }

    /// Perform a pre-order traversal of the tree based on the provided
    /// subqueries.
    ///
    /// ╔══════════════════════════════════════════════════════════════════╗
    /// ║                  ⚠⚠⚠  DO NOT MODIFY V0 PROOFS  ⚠⚠⚠               ║
    /// ╠══════════════════════════════════════════════════════════════════╣
    /// ║ This function produces V0 proof bytes that are consumed by      ║
    /// ║ grove versions v1 and v2 in production. Any change to the       ║
    /// ║ accepted query shapes, the emitted op stream, or the wrapper    ║
    /// ║ envelope is a consensus-breaking change. Add new features on    ║
    /// ║ V1 (`prove_subqueries_v1`) behind a fresh grove version         ║
    /// ║ instead. See `prove_query_non_serialized_v0` for the full       ║
    /// ║ rationale.                                                       ║
    /// ╚══════════════════════════════════════════════════════════════════╝
    pub(crate) fn prove_subqueries(
        &self,
        path: Vec<&[u8]>,
        path_query: &PathQuery,
        overall_limit: &mut Option<u16>,
        prove_options: &ProveOptions,
        current_depth: usize,
        grove_version: &GroveVersion,
    ) -> CostResult<MerkOnlyLayerProof, Error> {
        let mut cost = OperationCost::default();

        if current_depth > super::MAX_PROOF_DEPTH {
            return Err(Error::InvalidInput(
                "proof generation exceeded maximum depth limit",
            ))
            .wrap_with_cost(cost);
        }

        let tx = self.start_transaction();

        let query = cost_return_on_error_no_add!(
            cost,
            path_query
                .query_items_at_path(path.as_slice(), grove_version)
                .and_then(|query_items| {
                    query_items.ok_or(Error::CorruptedPath(format!(
                        "prove subqueries: path {} should be part of path_query {}",
                        path.iter()
                            .map(|a| hex_to_ascii(a))
                            .collect::<Vec<_>>()
                            .join("/"),
                        path_query
                    )))
                })
        );

        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path.as_slice().into(), &tx, None, grove_version)
        );

        // V0 proofs are LOCKED to the wire format shipped with grove
        // v1/v2; `ProvableCountProvableSumTree` (PCPS) was added
        // later and its dual-axis node hash (`node_hash_with_count_and_sum`)
        // needs proof Node variants that the V0 post-processor doesn't
        // know how to emit/route (`KVCountSum`,
        // `KVRefValueHashCountSum`, `HashWithCountAndSum`, etc.).
        // Refuse the combination at dispatch time rather than silently
        // produce a proof that drops the dual-axis aggregates from
        // the hash chain. PCPS users must produce proofs via V1
        // (grove v3+).
        if matches!(
            subtree.tree_type,
            grovedb_merk::TreeType::ProvableCountProvableSumTree
        ) {
            return Err(Error::NotSupported(
                "ProvableCountProvableSumTree hosts require V1 proof envelopes; \
                 upgrade the grove version producing the proof to v3 or later"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        let limit = if path.len() < path_query.path.len() {
            // There is no need for a limit because we are only asking for a single item
            None
        } else {
            *overall_limit
        };

        // Aggregate-count short-circuit: if any item at this level is an
        // `AggregateCountOnRange`, the surrounding `PathQuery` must validate
        // as a well-formed aggregate-count query. We do **not** route on a
        // partial match (e.g. a query with extra items, subqueries, or an
        // illegal inner) — those would silently produce a count proof for
        // the wrong shape. Instead we run the same validation the verifier
        // runs and let it surface the precise error.
        if query
            .items
            .iter()
            .any(QueryItem::is_aggregate_count_on_range)
        {
            let inner_range = cost_return_on_error_no_add!(
                cost,
                path_query.validate_aggregate_count_on_range().cloned()
            );
            let (count_ops, _count) = cost_return_on_error!(
                &mut cost,
                subtree
                    .prove_aggregate_count_on_range(&inner_range, grove_version)
                    .map_err(Error::MerkError)
            );
            let mut serialized = Vec::with_capacity(128);
            encode_into(count_ops.iter(), &mut serialized);
            return Ok(MerkOnlyLayerProof {
                merk_proof: serialized,
                lower_layers: BTreeMap::new(),
            })
            .wrap_with_cost(cost);
        }

        // Aggregate-sum short-circuit (mirror of count). Same contract: any
        // `AggregateSumOnRange` at this level requires the whole `PathQuery`
        // to be well-formed; the validate call surfaces the precise error
        // otherwise.
        if query.items.iter().any(QueryItem::is_aggregate_sum_on_range) {
            let inner_range = cost_return_on_error_no_add!(
                cost,
                path_query.validate_aggregate_sum_on_range().cloned()
            );
            let (sum_ops, _sum) = cost_return_on_error!(
                &mut cost,
                subtree
                    .prove_aggregate_sum_on_range(&inner_range, grove_version)
                    .map_err(|e| Error::CorruptedData(format!(
                        "prove_aggregate_sum_on_range failed: {}",
                        e
                    )))
            );
            let mut serialized = Vec::with_capacity(128);
            encode_into(sum_ops.iter(), &mut serialized);
            return Ok(MerkOnlyLayerProof {
                merk_proof: serialized,
                lower_layers: BTreeMap::new(),
            })
            .wrap_with_cost(cost);
        }

        // NOTE: count-offset paginated proofs are intentionally NOT
        // supported on V0. The V0 envelope is a shipped wire format
        // (grove versions v1 and v2 produce it in production); adding
        // new accepted query shapes here would be a consensus-breaking
        // change for already-deployed validators. The
        // `prove_query_non_serialized_v0` entry-point rejects
        // non-zero offsets unconditionally, so this short-circuit
        // never needed to fire — leaving it out keeps the V0 proof
        // surface identical to what shipped.

        let mut merk_proof = cost_return_on_error!(
            &mut cost,
            self.generate_merk_proof(
                &subtree,
                &query.items,
                query.left_to_right,
                limit,
                grove_version
            )
        );

        #[cfg(feature = "proof_debug")]
        {
            println!(
                "generated merk proof at level path level [{}], limit is {:?}, {}",
                path.iter()
                    .map(|a| hex_to_ascii(a))
                    .collect::<Vec<_>>()
                    .join("/"),
                overall_limit,
                if query.left_to_right {
                    "left to right"
                } else {
                    "right to left"
                }
            );
        }

        let mut lower_layers = BTreeMap::new();

        let mut has_a_result_at_level = false;
        let mut done_with_results = false;

        for op in merk_proof.proof.iter_mut() {
            done_with_results |= overall_limit == &Some(0);
            // Check if node should preserve its special type before destructuring
            // We need this flag to avoid converting it to Node::KV later
            // - KVValueHashFeatureType: used by ProvableCountTree / ProvableSumTree for
            //   trees/references
            // - KVCount: used by ProvableCountTree for Items (tamper-resistant with count)
            // - KVSum: used by ProvableSumTree for Items (tamper-resistant with sum)
            //
            // NOTE: V0 proofs are LOCKED — no PCPS handling here.
            // `ProvableCountProvableSumTree` was added after the V0
            // envelope shipped; users that need to prove PCPS-host
            // queries must use V1 (grove v3+). V0 dispatch rejects
            // PCPS-rooted leaf subtrees at the entry point of
            // `prove_query_non_serialized_v0` via the
            // `reject_pcps_leaf_under_v0` guard.
            let should_preserve_node_type = matches!(
                op,
                Op::Push(Node::KVValueHashFeatureType(..))
                    | Op::PushInverted(Node::KVValueHashFeatureType(..))
                    | Op::Push(Node::KVCount(..))
                    | Op::PushInverted(Node::KVCount(..))
                    | Op::Push(Node::KVSum(..))
                    | Op::PushInverted(Node::KVSum(..))
            );
            // Extract count if present for ProvableCountTree references
            let count_for_ref = match op {
                Op::Push(Node::KVValueHashFeatureType(_, _, _, ft))
                | Op::PushInverted(Node::KVValueHashFeatureType(_, _, _, ft)) => match ft {
                    TreeFeatureType::ProvableCountedMerkNode(count) => Some(*count),
                    _ => None,
                },
                _ => None,
            };
            // Extract sum if present for ProvableSumTree references. Mirrors
            // count_for_ref — the merk layer emits `KVValueHashFeatureType`
            // with a `ProvableSummedMerkNode(sum)` feature for references;
            // the GroveDB layer rewrites that to `KVRefValueHashSum` with
            // the dereferenced value.
            let sum_for_ref = match op {
                Op::Push(Node::KVValueHashFeatureType(_, _, _, ft))
                | Op::PushInverted(Node::KVValueHashFeatureType(_, _, _, ft)) => match ft {
                    TreeFeatureType::ProvableSummedMerkNode(sum) => Some(*sum),
                    _ => None,
                },
                _ => None,
            };
            match op {
                Op::Push(node) | Op::PushInverted(node) => match node {
                    Node::KV(key, value)
                    | Node::KVValueHash(key, value, ..)
                    | Node::KVCount(key, value, _)
                    | Node::KVSum(key, value, _)
                    | Node::KVValueHashFeatureType(key, value, ..)
                        if !done_with_results =>
                    {
                        // Look through NonCounted: dispatch on inner type.
                        // The serialized `value` (which is what's hashed in
                        // the proof) keeps its wrapper byte either way.
                        let elem =
                            Element::deserialize(value, grove_version).map(|e| e.into_underlying());
                        match elem {
                            // `ReferenceWithSumItem` shares the proof shape
                            // with `Reference`: combined value hash, GroveDB
                            // post-processes to KVRefValueHash{,Count} with
                            // the dereferenced value. The carried sum is
                            // hashed inside the (unchanged) serialized `value`
                            // bytes; the proof verifier sees it as part of
                            // the reference's KV-value-hash and the
                            // parent's feature_type (via merk's normal flow).
                            Ok(Element::Reference(reference_path, ..))
                            | Ok(Element::ReferenceWithSumItem(reference_path, ..)) => {
                                let absolute_path = cost_return_on_error_into!(
                                    &mut cost,
                                    path_from_reference_path_type(
                                        reference_path,
                                        &path.to_vec(),
                                        Some(key.as_slice())
                                    )
                                    .wrap_with_cost(OperationCost::default())
                                );

                                let referenced_elem = cost_return_on_error_into!(
                                    &mut cost,
                                    self.follow_reference(
                                        absolute_path.as_slice().into(),
                                        true,
                                        None,
                                        grove_version
                                    )
                                );

                                let serialized_referenced_elem =
                                    referenced_elem.serialize(grove_version);
                                if serialized_referenced_elem.is_err() {
                                    return Err(Error::CorruptedData(String::from(
                                        "unable to serialize element",
                                    )))
                                    .wrap_with_cost(cost);
                                }

                                // Dispatch priority:
                                //   ProvableSumTree references -> KVRefValueHashSum
                                //   ProvableCountTree references -> KVRefValueHashCount
                                //   regular references          -> KVRefValueHash
                                //
                                // NOTE: V0 proofs are LOCKED — no
                                // `KVRefValueHashCountSum` arm here.
                                // `ProvableCountProvableSumTree`
                                // references must be proved via V1
                                // (grove v3+); V0 dispatch rejects
                                // PCPS-rooted leaf subtrees at the
                                // entry point of
                                // `prove_query_non_serialized_v0`.
                                //
                                // The two ref-aggregate flags are
                                // mutually exclusive (a ref child sees
                                // one parent tree type), but Sum takes
                                // priority if both are erroneously
                                // set — Sum-in-hash is the stricter
                                // invariant.
                                *node = if let Some(sum) = sum_for_ref {
                                    Node::KVRefValueHashSum(
                                        key.to_owned(),
                                        serialized_referenced_elem.expect("confirmed ok above"),
                                        value_hash(value).unwrap_add_cost(&mut cost),
                                        sum,
                                    )
                                } else if let Some(count) = count_for_ref {
                                    Node::KVRefValueHashCount(
                                        key.to_owned(),
                                        serialized_referenced_elem.expect("confirmed ok above"),
                                        value_hash(value).unwrap_add_cost(&mut cost),
                                        count,
                                    )
                                } else {
                                    Node::KVRefValueHash(
                                        key.to_owned(),
                                        serialized_referenced_elem.expect("confirmed ok above"),
                                        value_hash(value).unwrap_add_cost(&mut cost),
                                    )
                                };
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            Ok(Element::Item(..))
                            | Ok(Element::SumItem(..))
                            | Ok(Element::ItemWithSumItem(..))
                                if !done_with_results =>
                            {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!("found {}", hex_to_ascii(key));
                                }
                                // Only convert to Node::KV if not already a special node type
                                // - KVValueHashFeatureType: preserves feature_type for trees/refs
                                // - KVCount: preserves count for Items in ProvableCountTree
                                // - KVSum: preserves sum for Items in ProvableSumTree
                                if !should_preserve_node_type {
                                    *node = Node::KV(key.to_owned(), value.to_owned());
                                }
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            // V0 is a frozen wire format. Adding cidx
                            // descent to it would change the proof bytes,
                            // so V0 will not learn cidx subqueries. Use
                            // V1 (or the dedicated `prove_indexed_count_*`
                            // entry points) for cidx queries.
                            Ok(Element::ProvableCountIndexedTree(..))
                            | Ok(Element::ProvableSumIndexedTree(..))
                            | Ok(Element::ProvableCountProvableSumIndexedTree(..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                return Err(Error::NotSupported(
                                    "V0 proofs do not support subqueries into \
                                     CountIndexedTree / ProvableCountIndexedTree; \
                                     use prove_query_v1 or prove_indexed_count_top_k"
                                        .to_string(),
                                ))
                                .wrap_with_cost(cost);
                            }
                            Ok(Element::Tree(Some(_), _))
                            | Ok(Element::SumTree(Some(_), ..))
                            | Ok(Element::BigSumTree(Some(_), ..))
                            | Ok(Element::CountTree(Some(_), ..))
                            | Ok(Element::CountSumTree(Some(_), ..))
                            | Ok(Element::ProvableCountTree(Some(_), ..))
                            | Ok(Element::ProvableCountSumTree(Some(_), ..))
                            | Ok(Element::ProvableSumTree(Some(_), ..))
                            | Ok(Element::ProvableCountProvableSumTree(Some(_), ..))
                            | Ok(Element::CommitmentTree(..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!(
                                        "found tree {}, query is {}",
                                        hex_to_ascii(key),
                                        query
                                    );
                                }
                                // We only want to check in sub nodes for the proof if the tree has
                                // elements
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries(
                                        lower_path,
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                if previous_limit != *overall_limit {
                                    // a lower layer updated the limit, don't subtract 1 at this
                                    // level
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // MmrTree and BulkAppendTree don't have Merk
                            // subtrees, so V0 proofs cannot descend into
                            // them. Return an error directing the caller to
                            // use prove_query_v1 instead.
                            Ok(Element::MmrTree(..))
                            | Ok(Element::BulkAppendTree(..))
                            | Ok(Element::DenseAppendOnlyFixedSizeTree(..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                return Err(Error::NotSupported(
                                    "V0 proofs do not support subqueries into MmrTree, \
                                     BulkAppendTree, or DenseAppendOnlyFixedSizeTree elements; \
                                     use prove_query_v1 instead"
                                        .to_string(),
                                ))
                                .wrap_with_cost(cost);
                            }

                            // V0 proofs do not inject child hashes for
                            // non-empty trees without subqueries.  The node
                            // stays as-is (KVValueHashFeatureType etc.) and
                            // counts as a result.
                            Ok(Element::Tree(_, _))
                            | Ok(Element::SumTree(..))
                            | Ok(Element::BigSumTree(..))
                            | Ok(Element::CountTree(..))
                            | Ok(Element::ProvableCountTree(..))
                            | Ok(Element::CountSumTree(..))
                            | Ok(Element::ProvableCountSumTree(..))
                            | Ok(Element::ProvableSumTree(..))
                            | Ok(Element::ProvableCountProvableSumTree(..))
                            | Ok(Element::CommitmentTree(..))
                            | Ok(Element::MmrTree(..))
                            | Ok(Element::BulkAppendTree(..))
                            | Ok(Element::DenseAppendOnlyFixedSizeTree(..))
                            | Ok(Element::ProvableSumIndexedTree(..))
                            | Ok(Element::ProvableCountIndexedTree(..))
                            | Ok(Element::ProvableCountProvableSumIndexedTree(..))
                                if !done_with_results =>
                            {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!(
                                        "found tree {}, no subquery query is {:?}",
                                        hex_to_ascii(key),
                                        query
                                    );
                                }
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            // todo: transform the unused trees into a Hash or KVHash to make proof
                            // smaller Ok(Element::Tree(..)) if
                            // done_with_results => {     *node =
                            // Node::Hash()     // we are done with the
                            // results, we can modify the proof to alter
                            // }

                            // Explicit: when done_with_results is true, the above guards fail
                            // and we skip. Listed explicitly so adding a new Element variant
                            // produces a compile error here instead of silently dropping it.
                            Ok(Element::Item(..))
                            | Ok(Element::SumItem(..))
                            | Ok(Element::ItemWithSumItem(..))
                            | Ok(Element::Tree(..))
                            | Ok(Element::SumTree(..))
                            | Ok(Element::BigSumTree(..))
                            | Ok(Element::CountTree(..))
                            | Ok(Element::CountSumTree(..))
                            | Ok(Element::ProvableCountTree(..))
                            | Ok(Element::ProvableCountSumTree(..))
                            | Ok(Element::ProvableSumTree(..))
                            | Ok(Element::ProvableCountProvableSumTree(..))
                            | Ok(Element::CommitmentTree(..))
                            | Ok(Element::MmrTree(..))
                            | Ok(Element::BulkAppendTree(..))
                            | Ok(Element::DenseAppendOnlyFixedSizeTree(..))
                            | Ok(Element::ProvableSumIndexedTree(..))
                            | Ok(Element::ProvableCountIndexedTree(..))
                            | Ok(Element::ProvableCountProvableSumIndexedTree(..)) => continue,
                            // NonCounted is unwrapped above via into_underlying().
                            Ok(Element::NonCounted(_))
                            | Ok(Element::NotSummed(_))
                            | Ok(Element::NotCountedOrSummed(_)) => {
                                unreachable!("unwrapped above")
                            }
                            Err(e) => {
                                return Err(Error::CorruptedData(format!(
                                    "failed to deserialize element during proof generation: {e}"
                                )))
                                .wrap_with_cost(cost);
                            }
                        }
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }

        if !has_a_result_at_level
            && !done_with_results
            && prove_options.decrease_limit_on_empty_sub_query_result
            && let Some(limit) = overall_limit.as_mut()
        {
            #[cfg(feature = "proof_debug")]
            {
                println!(
                    "no results at level {}",
                    path.iter()
                        .map(|a| hex_to_ascii(a))
                        .collect::<Vec<_>>()
                        .join("/")
                );
            }
            *limit -= 1;
        }

        let mut serialized_merk_proof = Vec::with_capacity(1024);
        encode_into(merk_proof.proof.iter(), &mut serialized_merk_proof);

        Ok(MerkOnlyLayerProof {
            merk_proof: serialized_merk_proof,
            lower_layers,
        })
        .wrap_with_cost(cost)
    }

    /// Generates query proof given a subtree and appends the result to a proof
    /// list
    fn generate_merk_proof<'a, S>(
        &self,
        subtree: &'a Merk<S>,
        query_items: &[QueryItem],
        left_to_right: bool,
        limit: Option<u16>,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofWithoutEncodingResult, Error>
    where
        S: StorageContext<'a> + 'a,
    {
        subtree
            .prove_unchecked_query_items(query_items, limit, left_to_right, grove_version)
            .map_ok(|(proof, limit)| ProofWithoutEncodingResult::new(proof, limit))
            .map_err(|e| {
                Error::InternalError(format!(
                    "failed to generate proof for query_items [{}] error is : {}",
                    query_items
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    e
                ))
            })
    }

    /// Generate a trunk chunk proof for a tree at the given path.
    ///
    /// This retrieves the top N levels of a count-based tree, returning a proof
    /// that can be verified to obtain a `TrunkQueryResult`.
    ///
    /// # Arguments
    /// * `query` - The path trunk chunk query containing the path and max_depth
    /// * `grove_version` - The grove version for compatibility
    ///
    /// # Returns
    /// A serialized `TrunkChunkProof` that can be verified
    pub fn prove_trunk_chunk(
        &self,
        query: &PathTrunkChunkQuery,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        check_grovedb_v0_or_v1_with_cost!(
            "prove_trunk_chunk",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .prove_trunk_chunk
        );
        let mut cost = OperationCost::default();

        let proof = cost_return_on_error!(
            &mut cost,
            self.prove_trunk_chunk_non_serialized(query, grove_version)
        );

        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let encoded_proof = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(proof, config)
                .map_err(|e| Error::CorruptedData(format!("unable to encode proof {}", e)))
        );

        Ok(encoded_proof).wrap_with_cost(cost)
    }

    /// Generate a trunk chunk proof without serializing.
    ///
    /// Returns a `GroveDBProof` with the standard `LayerProof` hierarchy.
    /// The path is navigated layer by layer, and at the target tree the
    /// merk_proof contains the trunk chunk proof (not a query proof).
    pub fn prove_trunk_chunk_non_serialized(
        &self,
        query: &PathTrunkChunkQuery,
        grove_version: &GroveVersion,
    ) -> CostResult<GroveDBProof, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .prove_trunk_chunk_non_serialized
        {
            0 => self.prove_trunk_chunk_non_serialized_v0(query, grove_version),
            1 => self.prove_trunk_chunk_non_serialized_v1(query, grove_version),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "prove_trunk_chunk_non_serialized".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }

    /// V0: Generate a trunk chunk proof using MerkOnlyLayerProof.
    fn prove_trunk_chunk_non_serialized_v0(
        &self,
        query: &PathTrunkChunkQuery,
        grove_version: &GroveVersion,
    ) -> CostResult<GroveDBProof, Error> {
        let mut cost = OperationCost::default();

        let tx = self.start_transaction();

        // Build the proof from the target tree back to the root
        // We collect proofs for each layer, then nest them
        let path_slices: Vec<&[u8]> = query.path.iter().map(|p| p.as_slice()).collect();

        // First, generate the trunk proof for the target tree
        let target_tree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path_slices.as_slice().into(),
                &tx,
                None,
                grove_version
            )
        );

        // Perform the trunk query
        let trunk_result = cost_return_on_error!(
            &mut cost,
            target_tree
                .trunk_query(query.max_depth, query.min_depth, grove_version)
                .map_err(Error::MerkError)
        );

        // Encode the trunk proof ops
        let mut trunk_proof_encoded = Vec::new();
        encode_into(trunk_result.proof.iter(), &mut trunk_proof_encoded);

        // Start with the innermost LayerProof (the trunk proof at target tree)
        let mut current_layer = MerkOnlyLayerProof {
            merk_proof: trunk_proof_encoded,
            lower_layers: BTreeMap::new(),
        };

        // Build nested LayerProofs from inside out (target -> root)
        for i in (0..query.path.len()).rev() {
            let current_path: Vec<&[u8]> = path_slices[..i].to_vec();
            let key = query.path[i].clone();

            // Open the merk at the current path
            let subtree = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    current_path.as_slice().into(),
                    &tx,
                    None,
                    grove_version
                )
            );

            // Generate a proof for the path segment key
            let query_item = QueryItem::Key(key.clone());
            let merk_proof = cost_return_on_error!(
                &mut cost,
                self.generate_merk_proof(&subtree, &[query_item], true, None, grove_version)
            );

            // Encode the merk proof
            let mut encoded_proof = Vec::new();
            encode_into(merk_proof.proof.iter(), &mut encoded_proof);

            // Create the new layer with the current layer as a lower layer
            let mut lower_layers = BTreeMap::new();
            lower_layers.insert(key, current_layer);

            current_layer = MerkOnlyLayerProof {
                merk_proof: encoded_proof,
                lower_layers,
            };
        }

        Ok(GroveDBProof::V0(GroveDBProofV0 {
            root_layer: current_layer,
            prove_options: ProveOptions::default(),
        }))
        .wrap_with_cost(cost)
    }

    /// V1: Generate a trunk chunk proof using LayerProof with ProofBytes::Merk.
    ///
    /// Nearly identical to V0 but uses the V1 proof types (LayerProof,
    /// ProofBytes::Merk, GroveDBProofV1) so the verifier can apply the
    /// stricter combine_hash check that prevents the count==0 forgery.
    fn prove_trunk_chunk_non_serialized_v1(
        &self,
        query: &PathTrunkChunkQuery,
        grove_version: &GroveVersion,
    ) -> CostResult<GroveDBProof, Error> {
        let mut cost = OperationCost::default();

        let tx = self.start_transaction();

        let path_slices: Vec<&[u8]> = query.path.iter().map(|p| p.as_slice()).collect();

        // Generate the trunk proof for the target tree
        let target_tree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path_slices.as_slice().into(),
                &tx,
                None,
                grove_version
            )
        );

        let trunk_result = cost_return_on_error!(
            &mut cost,
            target_tree
                .trunk_query(query.max_depth, query.min_depth, grove_version)
                .map_err(Error::MerkError)
        );

        let mut trunk_proof_encoded = Vec::new();
        encode_into(trunk_result.proof.iter(), &mut trunk_proof_encoded);

        // Start with the innermost LayerProof using ProofBytes::Merk
        let mut current_layer = LayerProof {
            merk_proof: ProofBytes::Merk(trunk_proof_encoded),
            lower_layers: BTreeMap::new(),
        };

        // Build nested LayerProofs from inside out (target -> root)
        for i in (0..query.path.len()).rev() {
            let current_path: Vec<&[u8]> = path_slices[..i].to_vec();
            let key = query.path[i].clone();

            let subtree = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    current_path.as_slice().into(),
                    &tx,
                    None,
                    grove_version
                )
            );

            let query_item = QueryItem::Key(key.clone());
            let merk_proof = cost_return_on_error!(
                &mut cost,
                self.generate_merk_proof(&subtree, &[query_item], true, None, grove_version)
            );

            let mut encoded_proof = Vec::new();
            encode_into(merk_proof.proof.iter(), &mut encoded_proof);

            let mut lower_layers = BTreeMap::new();
            lower_layers.insert(key, current_layer);

            current_layer = LayerProof {
                merk_proof: ProofBytes::Merk(encoded_proof),
                lower_layers,
            };
        }

        Ok(GroveDBProof::V1(GroveDBProofV1 {
            root_layer: current_layer,
        }))
        .wrap_with_cost(cost)
    }

    /// Generate a serialized branch chunk proof.
    ///
    /// Navigates to the specified key in the tree at the given path,
    /// then returns a proof of the subtree rooted at that key.
    /// The proof can be verified against the `Node::Hash` from a trunk query's
    /// terminal node.
    pub fn prove_branch_chunk(
        &self,
        query: &crate::query::PathBranchChunkQuery,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        check_grovedb_v0_with_cost!(
            "prove_branch_chunk",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .prove_branch_chunk
        );
        let mut cost = OperationCost::default();

        let branch_result = cost_return_on_error!(
            &mut cost,
            self.prove_branch_chunk_non_serialized(query, grove_version)
        );

        // Encode just the proof ops - the verifier will execute them
        let mut encoded_proof = Vec::new();
        encode_into(branch_result.proof.iter(), &mut encoded_proof);

        Ok(encoded_proof).wrap_with_cost(cost)
    }

    /// Generate a branch chunk proof without serializing.
    ///
    /// Returns a `BranchQueryResult` containing the proof ops and branch root
    /// hash. The `branch_root_hash` should match a `Node::Hash` from the
    /// trunk query's terminal nodes.
    pub fn prove_branch_chunk_non_serialized(
        &self,
        query: &crate::query::PathBranchChunkQuery,
        grove_version: &GroveVersion,
    ) -> CostResult<grovedb_merk::BranchQueryResult, Error> {
        check_grovedb_v0_with_cost!(
            "prove_branch_chunk_non_serialized",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .prove_branch_chunk_non_serialized
        );
        let mut cost = OperationCost::default();

        let tx = self.start_transaction();

        let path_slices: Vec<&[u8]> = query.path.iter().map(|p| p.as_slice()).collect();

        // Open the target tree and perform the branch query
        let target_tree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path_slices.as_slice().into(),
                &tx,
                None,
                grove_version
            )
        );

        // Perform the branch query - returns BranchQueryResult directly
        let branch_result = cost_return_on_error!(
            &mut cost,
            target_tree
                .branch_query(&query.key, query.depth, grove_version)
                .map_err(Error::MerkError)
        );

        Ok(branch_result).wrap_with_cost(cost)
    }

    // ── V1 Proof Generation (MmrTree / BulkAppendTree support) ──────────

    /// V1: Generates a proof that supports MmrTree / BulkAppendTree elements.
    pub(crate) fn prove_query_non_serialized_v1(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<GroveDBProof, Error> {
        let mut cost = OperationCost::default();
        let prove_options = prove_options.unwrap_or_default();

        if path_query.query.offset.is_some() && path_query.query.offset != Some(0) {
            // A non-zero offset is honored *only* if the surrounding
            // query is an offset-paginated range query against a
            // ProvableCountTree / ProvableCountSumTree (see
            // `SizedQuery::validate_count_offset_paginated`).
            //
            // We do two checks here at the top entry:
            //   1. Syntactic gate via `validate_count_offset_paginated`
            //      (single range item, no subqueries, offset > 0).
            //   2. Open the target leaf merk and confirm its
            //      `tree_type` is one of the two allowed flavors.
            //
            // Step 2 has to be done at the top because the leaf-level
            // short-circuit in `prove_subqueries_v1` only fires after
            // the descent reaches the leaf — and for an empty
            // NormalTree at the target path the descent's empty-tree
            // arm decrements the limit and returns instead of
            // recursing, so the leaf check would silently accept.
            // Doing the merk-open here gives a clear up-front error
            // for that case.
            if let Err(e) = path_query.validate_count_offset_paginated() {
                return Err(e).wrap_with_cost(cost);
            }
            cost_return_on_error!(
                &mut cost,
                self.check_count_offset_target_tree_type(path_query, grove_version)
            );
        }
        if path_query.query.limit == Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not be for limit 0",
            ))
            .wrap_with_cost(cost);
        }

        let mut limit = path_query.query.limit;

        let root_layer = cost_return_on_error!(
            &mut cost,
            self.prove_subqueries_v1(
                vec![],
                path_query,
                &mut limit,
                &prove_options,
                0,
                grove_version
            )
        );

        // A single-path axis read has exactly one answer — the axis
        // descent at the queried path. The generic walk cannot produce
        // one when the target is missing or is not an indexed tree; it
        // returns `Ok` with an ordinary (or empty) layer instead, and
        // the verifier then rejects the result as "must verify exactly
        // one axis layer, got 0". Fail generation here instead, so the
        // prover never hands out a proof that cannot answer the query
        // it was asked.
        //
        // Branched axis reads are deliberately excluded: an absent
        // branch key legitimately produces no descent, and its absence
        // is what the branching-level Merk proof authenticates.
        if matches!(
            path_query.classify(),
            Ok(crate::PathQueryShape::AxisRead { .. })
        ) {
            fn count_axis_descents(layer: &LayerProof) -> usize {
                usize::from(matches!(
                    layer.merk_proof,
                    ProofBytes::IndexedTreeAxisDescent(_)
                )) + layer
                    .lower_layers
                    .values()
                    .map(count_axis_descents)
                    .sum::<usize>()
            }
            let descents = count_axis_descents(&root_layer);
            if descents != 1 {
                return Err(Error::InvalidPath(format!(
                    "a single-path axis read must produce exactly one axis descent at the \
                     queried path, but the walk produced {descents} — the path does not \
                     name an indexed tree carrying that axis"
                )))
                .wrap_with_cost(cost);
            }
        }

        Ok(GroveDBProof::V1(GroveDBProofV1 { root_layer })).wrap_with_cost(cost)
    }

    /// Compute the 32-byte secondary attestation that an indexed-tree element
    /// commits to as the third input of its parent's `combine_hash_three`.
    ///
    /// PCIT / PSIT attest with their single secondary Merk's root hash; PCPSIT
    /// attests with the `axes_digest` over every configured axis's secondary
    /// root hash (`NULL_HASH` for an empty axis), exactly as the insert commit
    /// path composes it.
    ///
    /// `indexed_path` is the path OF the indexed primary (i.e. the parent path
    /// with the element's key already pushed).
    fn indexed_secondary_attestation(
        &self,
        element: &Element,
        indexed_path: &[&[u8]],
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<grovedb_merk::CryptoHash, Error> {
        let mut cost = OperationCost::default();
        let subtree_path: grovedb_path::SubtreePath<&[u8]> = indexed_path.into();

        let single_axis = match element {
            Element::ProvableCountIndexedTree(_, secondary, ..) => Some((
                grovedb_element::indexed::IndexAxis::Count,
                secondary.clone(),
            )),
            Element::ProvableSumIndexedTree(_, secondary, ..) => {
                Some((grovedb_element::indexed::IndexAxis::Sum, secondary.clone()))
            }
            _ => None,
        };

        if let Some((axis, secondary_root_key)) = single_axis {
            let secondary_merk = cost_return_on_error!(
                &mut cost,
                self.open_indexed_secondary_at_path(
                    subtree_path,
                    axis,
                    secondary_root_key,
                    transaction,
                    None,
                    grove_version,
                )
            );
            let (secondary_root, _, _) = cost_return_on_error!(
                &mut cost,
                secondary_merk
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            return Ok(secondary_root).wrap_with_cost(cost);
        }

        let Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) = element else {
            return Err(Error::CorruptedCodeExecution(
                "indexed_secondary_attestation called with a non-indexed element",
            ))
            .wrap_with_cost(cost);
        };

        let mut axis_hashes: Vec<(u8, grovedb_merk::CryptoHash)> = Vec::with_capacity(axes.len());
        for (tag, secondary_root_key) in axes.iter() {
            let axis = cost_return_on_error_no_add!(
                cost,
                grovedb_element::indexed::IndexAxis::try_from_tag(*tag).map_err(|e| {
                    Error::CorruptedData(format!("invalid axis tag in PCPSIT during proof: {e}"))
                })
            );
            let secondary_merk = cost_return_on_error!(
                &mut cost,
                self.open_indexed_secondary_at_path(
                    indexed_path.into(),
                    axis,
                    secondary_root_key.clone(),
                    transaction,
                    None,
                    grove_version,
                )
            );
            let (secondary_root, _, _) = cost_return_on_error!(
                &mut cost,
                secondary_merk
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            axis_hashes.push((*tag, secondary_root));
        }

        Ok(grovedb_merk::tree::axes_digest(&axis_hashes).unwrap_add_cost(&mut cost))
            .wrap_with_cost(cost)
    }

    /// V1 version of prove_subqueries that returns `LayerProof` and handles
    /// MmrTree/BulkAppendTree elements with type-specific proofs.
    pub(crate) fn prove_subqueries_v1(
        &self,
        path: Vec<&[u8]>,
        path_query: &PathQuery,
        overall_limit: &mut Option<u16>,
        prove_options: &ProveOptions,
        current_depth: usize,
        grove_version: &GroveVersion,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        if current_depth > super::MAX_PROOF_DEPTH {
            return Err(Error::InvalidInput(
                "proof generation exceeded maximum depth limit",
            ))
            .wrap_with_cost(cost);
        }

        let tx = self.start_transaction();

        let query = cost_return_on_error_no_add!(
            cost,
            path_query
                .query_items_at_path(path.as_slice(), grove_version)
                .and_then(|query_items| {
                    query_items.ok_or(Error::CorruptedPath(format!(
                        "prove subqueries v1: path {} should be part of path_query {}",
                        path.iter()
                            .map(|a| hex_to_ascii(a))
                            .collect::<Vec<_>>()
                            .join("/"),
                        path_query
                    )))
                })
        );

        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path.as_slice().into(), &tx, None, grove_version)
        );

        let limit = if path.len() < path_query.path.len() {
            None
        } else {
            *overall_limit
        };

        // Aggregate-count short-circuit (v1 path). Same validation contract
        // as v0: any AggregateCountOnRange at this level requires the
        // surrounding PathQuery to validate as a well-formed aggregate-count
        // query. The count-proof bytes are wrapped in `ProofBytes::Merk`
        // since they share the merk Op stream encoding.
        if query
            .items
            .iter()
            .any(QueryItem::is_aggregate_count_on_range)
        {
            let inner_range = cost_return_on_error_no_add!(
                cost,
                path_query.validate_aggregate_count_on_range().cloned()
            );
            let (count_ops, _count) = cost_return_on_error!(
                &mut cost,
                subtree
                    .prove_aggregate_count_on_range(&inner_range, grove_version)
                    .map_err(Error::MerkError)
            );
            let mut serialized = Vec::with_capacity(128);
            encode_into(count_ops.iter(), &mut serialized);
            return Ok(LayerProof {
                merk_proof: ProofBytes::Merk(serialized),
                lower_layers: BTreeMap::new(),
            })
            .wrap_with_cost(cost);
        }

        // Aggregate-sum short-circuit (v1 path). Mirror of the count v1
        // branch.
        if query.items.iter().any(QueryItem::is_aggregate_sum_on_range) {
            let inner_range = cost_return_on_error_no_add!(
                cost,
                path_query.validate_aggregate_sum_on_range().cloned()
            );
            let (sum_ops, _sum) = cost_return_on_error!(
                &mut cost,
                subtree
                    .prove_aggregate_sum_on_range(&inner_range, grove_version)
                    .map_err(|e| Error::CorruptedData(format!(
                        "prove_aggregate_sum_on_range failed: {}",
                        e
                    )))
            );
            let mut serialized = Vec::with_capacity(128);
            encode_into(sum_ops.iter(), &mut serialized);
            return Ok(LayerProof {
                merk_proof: ProofBytes::Merk(serialized),
                lower_layers: BTreeMap::new(),
            })
            .wrap_with_cost(cost);
        }

        // Combined-aggregate short-circuit (v1 path). PCPS-only —
        // emits the dual-axis op stream that carries both count and
        // sum from a single proof. Mirror of the ACOR / ASOR v1
        // branches above.
        if query
            .items
            .iter()
            .any(QueryItem::is_aggregate_count_and_sum_on_range)
        {
            let inner_range = cost_return_on_error_no_add!(
                cost,
                path_query
                    .validate_aggregate_count_and_sum_on_range()
                    .cloned()
            );
            let (ops, _count, _sum) = cost_return_on_error!(
                &mut cost,
                subtree
                    .prove_aggregate_count_and_sum_on_range(&inner_range, grove_version)
                    .map_err(|e| Error::CorruptedData(format!(
                        "prove_aggregate_count_and_sum_on_range failed: {}",
                        e
                    )))
            );
            let mut serialized = Vec::with_capacity(128);
            encode_into(ops.iter(), &mut serialized);
            return Ok(LayerProof {
                merk_proof: ProofBytes::Merk(serialized),
                lower_layers: BTreeMap::new(),
            })
            .wrap_with_cost(cost);
        }

        // Count-offset paginated short-circuit (v1 path). Mirror of the
        // aggregate-count/sum branches. Only fires at the leaf level
        // (path is the full path_query.path) and only when the caller
        // requested a non-zero offset on a syntactically-eligible query.
        // The tree-type check happens here — the syntactic gate at the
        // top entry already ran, so a mismatched tree type is a
        // hard-error case (the caller asked for count-offset pagination
        // against something that isn't a count tree).
        if path.len() == path_query.path.len() && path_query.has_non_zero_offset() {
            use grovedb_merk::TreeType as MerkTreeType;
            let inner_range = cost_return_on_error_no_add!(
                cost,
                path_query.validate_count_offset_paginated().cloned()
            );
            if !matches!(
                subtree.tree_type,
                MerkTreeType::ProvableCountTree
                    | MerkTreeType::ProvableCountSumTree
                    | MerkTreeType::ProvableCountProvableSumTree
            ) {
                return Err(Error::InvalidQuery(
                    "count-offset paginated queries are only valid against \
                     ProvableCountTree / ProvableCountSumTree / ProvableCountProvableSumTree \
                     merks",
                ))
                .wrap_with_cost(cost);
            }
            let offset = path_query.query.offset.map(|o| o as u64).unwrap_or(0);
            // Carry the SizedQuery::limit into the merk-level proof so
            // the prover stops emitting value nodes once the requested
            // page is full. After the merk prover returns, decrement
            // the outer overall_limit accordingly so the upstream
            // multi-layer accounting (if any) reflects the consumed
            // slots.
            let limit_u64 = path_query.query.limit.map(|l| l as u64);
            let mut prove_result = cost_return_on_error!(
                &mut cost,
                subtree
                    .prove_count_offset_on_range(
                        &inner_range,
                        offset,
                        limit_u64,
                        query.left_to_right,
                        grove_version,
                    )
                    // Wrap with operational context so a downstream
                    // proof failure (corrupted merk, invariant
                    // violation in the prover, etc.) is identifiable
                    // as a count-offset-specific failure rather than
                    // an opaque `MerkError`. Mirrors the
                    // `prove_aggregate_sum_on_range` wrapping a few
                    // hundred lines up.
                    .map_err(|e| Error::CorruptedData(format!(
                        "prove_count_offset_on_range failed: {}",
                        e
                    )))
            );
            // Dereference reference rows before encoding.
            //
            // This short-circuit returns without reaching the main
            // ref-rewriting loop below, which is why the count-offset flow
            // used to reject reference entries outright. Running the same
            // rewrite here closes that gap rather than bypassing it.
            //
            // These are ORDINARY user references, so they follow ordinary
            // terminal-reference semantics — unlike an indexed secondary
            // row, which binds its immediate primary node and is resolved
            // by `indexed_axis::reference_resolution`. The two rules are
            // deliberately separate code paths.
            for op in prove_result.ops.iter_mut() {
                let node = match op {
                    Op::Push(node) | Op::PushInverted(node) => node,
                    _ => continue,
                };
                let Node::KVValueHashFeatureType(key, value, _, feature_type) = node else {
                    continue;
                };
                let elem = match Element::deserialize(value, grove_version) {
                    Ok(e) => e.into_underlying(),
                    Err(_) => continue,
                };
                let (Element::Reference(reference_path, ..)
                | Element::ReferenceWithSumItem(reference_path, ..)) = elem
                else {
                    continue;
                };
                let absolute_path = match path_from_reference_path_type(
                    reference_path,
                    &path.to_vec(),
                    Some(key.as_slice()),
                ) {
                    Ok(p) => p,
                    Err(e) => return Err(Error::from(e)).wrap_with_cost(cost),
                };
                let referenced_elem = cost_return_on_error!(
                    &mut cost,
                    self.follow_reference(
                        absolute_path.as_slice().into(),
                        true,
                        None,
                        grove_version
                    )
                );
                let serialized_referenced_elem = match referenced_elem.serialize(grove_version) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return Err(Error::CorruptedData(String::from(
                            "unable to serialize element",
                        )))
                        .wrap_with_cost(cost);
                    }
                };
                let reference_element_hash = value_hash(value).unwrap_add_cost(&mut cost);
                *node = match feature_type {
                    TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                        Node::KVRefValueHashCountSum(
                            key.to_owned(),
                            serialized_referenced_elem,
                            reference_element_hash,
                            *count,
                            *sum,
                        )
                    }
                    // `ProvableCountSumTree` is an eligible count-offset
                    // host but commits only the COUNT into its node hash
                    // (`binds_sum_into_hash` is true for PCPS alone), so
                    // its reference rows take the count-only node — the
                    // same variant `emit_returned_node` picks for its
                    // directly-valued rows. Without this arm a reference in
                    // such a tree hard-errored.
                    TreeFeatureType::ProvableCountedMerkNode(count)
                    | TreeFeatureType::ProvableCountedSummedMerkNode(count, _) => {
                        Node::KVRefValueHashCount(
                            key.to_owned(),
                            serialized_referenced_elem,
                            reference_element_hash,
                            *count,
                        )
                    }
                    other => {
                        return Err(Error::CorruptedData(format!(
                            "count-offset proof: reference row {} carries non-count feature type \
                             {other:?}",
                            hex::encode(key)
                        )))
                        .wrap_with_cost(cost);
                    }
                };
            }
            let mut serialized = Vec::with_capacity(128);
            encode_into(prove_result.ops.iter(), &mut serialized);
            // Apply consumed limit slots to the outer accounting.
            if let Some(outer_limit) = overall_limit.as_mut() {
                let returned_u16: u16 = prove_result.returned.min(u16::MAX as u64) as u16;
                *outer_limit = outer_limit.saturating_sub(returned_u16);
            }
            return Ok(LayerProof {
                merk_proof: ProofBytes::Merk(serialized),
                lower_layers: BTreeMap::new(),
            })
            .wrap_with_cost(cost);
        }

        // Whether the surrounding query is an aggregate-count carrier:
        // empty trees that match a `subquery_path` step still need a
        // lower-layer descent so the aggregate-count short-circuit can
        // emit an empty count proof (verifier reads it as count = 0).
        // For non-aggregate-count queries, empty trees keep their
        // existing "terminal result" semantics.
        let is_aggregate_count_query = path_query
            .query
            .query
            .has_aggregate_count_on_range_anywhere();
        // Same reasoning for aggregate-sum on the new dual-axis
        // ProvableCountProvableSumTree (and the single-axis
        // ProvableSumTree): an empty merk at the sum-bearing host
        // still has to emit a lower-layer ASOR proof (verifier reads
        // it as sum = 0).
        let is_aggregate_sum_query = path_query.query.query.has_aggregate_sum_on_range_anywhere();
        // Combined-aggregate (PCPS-only) carrier detection mirrors the
        // ACOR / ASOR flags above. Empty PCPS hosts under an
        // AggregateCountAndSumOnRange carrier need a lower-layer
        // descent so the combined short-circuit can emit an empty
        // proof (verifier reads it as count = 0, sum = 0).
        let is_aggregate_count_and_sum_query = path_query
            .query
            .query
            .has_aggregate_count_and_sum_on_range_anywhere();

        let mut merk_proof = cost_return_on_error!(
            &mut cost,
            self.generate_merk_proof(
                &subtree,
                &query.items,
                query.left_to_right,
                limit,
                grove_version
            )
        );

        let mut lower_layers = BTreeMap::new();
        let mut has_a_result_at_level = false;
        let mut done_with_results = false;

        for op in merk_proof.proof.iter_mut() {
            done_with_results |= overall_limit == &Some(0);
            // Mirror generate.rs's first ref-rewriting loop — preserve
            // ProvableSumTree special nodes too, plus the dual-axis
            // KVCountSum used by ProvableCountProvableSumTree (PCPS)
            // for Items. Without the KVCountSum arm here a PCPS Item
            // would be rewritten back to Node::KV by the Item handler
            // below, destroying the dual-axis hash binding.
            let should_preserve_node_type = matches!(
                op,
                Op::Push(Node::KVValueHashFeatureType(..))
                    | Op::PushInverted(Node::KVValueHashFeatureType(..))
                    | Op::Push(Node::KVCount(..))
                    | Op::PushInverted(Node::KVCount(..))
                    | Op::Push(Node::KVSum(..))
                    | Op::PushInverted(Node::KVSum(..))
                    | Op::Push(Node::KVCountSum(..))
                    | Op::PushInverted(Node::KVCountSum(..))
            );
            let count_for_ref = match op {
                Op::Push(Node::KVValueHashFeatureType(_, _, _, ft))
                | Op::PushInverted(Node::KVValueHashFeatureType(_, _, _, ft)) => match ft {
                    // `ProvableCountSumTree` hashes via `node_hash_with_count`
                    // (only PCPS binds the sum in), so its references need the
                    // COUNT just as a `ProvableCountTree`'s do. Without this
                    // arm they downgraded to the aggregateless
                    // `KVRefValueHash` and the host's node hash could not be
                    // reconstructed — the proof verified nowhere.
                    TreeFeatureType::ProvableCountedMerkNode(count)
                    | TreeFeatureType::ProvableCountedSummedMerkNode(count, _) => Some(*count),
                    _ => None,
                },
                _ => None,
            };
            let sum_for_ref = match op {
                Op::Push(Node::KVValueHashFeatureType(_, _, _, ft))
                | Op::PushInverted(Node::KVValueHashFeatureType(_, _, _, ft)) => match ft {
                    TreeFeatureType::ProvableSummedMerkNode(sum) => Some(*sum),
                    _ => None,
                },
                _ => None,
            };
            // Mirror of the v1 loop above: extract BOTH count and sum for
            // dual-axis (PCPS) references so we can emit
            // `KVRefValueHashCountSum` instead of downgrading to a
            // single-axis or aggregateless ref node.
            let count_sum_for_ref = match op {
                Op::Push(Node::KVValueHashFeatureType(_, _, _, ft))
                | Op::PushInverted(Node::KVValueHashFeatureType(_, _, _, ft)) => match ft {
                    TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                        Some((*count, *sum))
                    }
                    _ => None,
                },
                _ => None,
            };

            match op {
                Op::Push(node) | Op::PushInverted(node) => match node {
                    Node::KV(key, value)
                    | Node::KVValueHash(key, value, ..)
                    | Node::KVCount(key, value, _)
                    | Node::KVSum(key, value, _)
                    | Node::KVCountSum(key, value, ..)
                    | Node::KVValueHashFeatureType(key, value, ..)
                        if !done_with_results =>
                    {
                        // Look through NonCounted: dispatch on inner type.
                        // The serialized `value` (which is what's hashed in
                        // the proof) keeps its wrapper byte either way.
                        let elem =
                            Element::deserialize(value, grove_version).map(|e| e.into_underlying());
                        match elem {
                            // `ReferenceWithSumItem` shares this proof path
                            // with `Reference` — both produce a
                            // KVRefValueHash{,Count} node with the
                            // dereferenced target's serialized bytes.
                            Ok(Element::Reference(reference_path, ..))
                            | Ok(Element::ReferenceWithSumItem(reference_path, ..)) => {
                                let absolute_path = cost_return_on_error_into!(
                                    &mut cost,
                                    path_from_reference_path_type(
                                        reference_path,
                                        &path.to_vec(),
                                        Some(key.as_slice())
                                    )
                                    .wrap_with_cost(OperationCost::default())
                                );

                                let referenced_elem = cost_return_on_error_into!(
                                    &mut cost,
                                    self.follow_reference(
                                        absolute_path.as_slice().into(),
                                        true,
                                        None,
                                        grove_version
                                    )
                                );

                                let serialized_referenced_elem =
                                    referenced_elem.serialize(grove_version);
                                if serialized_referenced_elem.is_err() {
                                    return Err(Error::CorruptedData(String::from(
                                        "unable to serialize element",
                                    )))
                                    .wrap_with_cost(cost);
                                }

                                // Dispatch in priority order — dual-axis
                                // PCPS first (strictest invariant), then
                                // single-axis Sum, then single-axis Count,
                                // then plain ref. See the v1 loop for the
                                // longer-form comment.
                                *node = if let Some((count, sum)) = count_sum_for_ref {
                                    Node::KVRefValueHashCountSum(
                                        key.to_owned(),
                                        serialized_referenced_elem.expect("confirmed ok above"),
                                        value_hash(value).unwrap_add_cost(&mut cost),
                                        count,
                                        sum,
                                    )
                                } else if let Some(sum) = sum_for_ref {
                                    Node::KVRefValueHashSum(
                                        key.to_owned(),
                                        serialized_referenced_elem.expect("confirmed ok above"),
                                        value_hash(value).unwrap_add_cost(&mut cost),
                                        sum,
                                    )
                                } else if let Some(count) = count_for_ref {
                                    Node::KVRefValueHashCount(
                                        key.to_owned(),
                                        serialized_referenced_elem.expect("confirmed ok above"),
                                        value_hash(value).unwrap_add_cost(&mut cost),
                                        count,
                                    )
                                } else {
                                    Node::KVRefValueHash(
                                        key.to_owned(),
                                        serialized_referenced_elem.expect("confirmed ok above"),
                                        value_hash(value).unwrap_add_cost(&mut cost),
                                    )
                                };
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            Ok(Element::Item(..))
                            | Ok(Element::SumItem(..))
                            | Ok(Element::ItemWithSumItem(..))
                                if !done_with_results =>
                            {
                                if !should_preserve_node_type {
                                    *node = Node::KV(key.to_owned(), value.to_owned());
                                }
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }

                            // Sum-budget read of a merk-backed tree: the
                            // query node governing this element carries
                            // ReadMode::SumBudget, so the layer carries a
                            // sum-budget window — an ordinary Merk proof
                            // over exactly the window the budget walk
                            // scanned — instead of a key-selection
                            // descent. Matched before every other tree arm
                            // so the shape can never be silently served as
                            // a plain descent.
                            Ok(ref elem)
                                if !done_with_results && {
                                    let mut lower_path = path.clone();
                                    lower_path.push(key.as_slice());
                                    path_query.sum_budget_read_at_path(&lower_path).is_some()
                                } =>
                            {
                                use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

                                if matches!(
                                    elem,
                                    Element::MmrTree(..)
                                        | Element::BulkAppendTree(..)
                                        | Element::DenseAppendOnlyFixedSizeTree(..)
                                        | Element::CommitmentTree(..)
                                ) || elem.tree_type().is_none()
                                {
                                    return Err(Error::NotSupported(
                                        "sum-budget reads target merk-backed trees; the query \
                                         path names a different element kind"
                                            .to_string(),
                                    ))
                                    .wrap_with_cost(cost);
                                }
                                if grove_version
                                    .grovedb_versions
                                    .operations
                                    .proof
                                    .sum_budget_in_v1_envelope
                                    != 1
                                {
                                    return Err(Error::NotSupported(
                                        "sum-budget windows in the V1 proof envelope are not \
                                         emitted at this grove version"
                                            .to_string(),
                                    ))
                                    .wrap_with_cost(cost);
                                }

                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());
                                let payload = cost_return_on_error!(
                                    &mut cost,
                                    self.build_sum_budget_window_payload(
                                        &lower_path,
                                        path_query,
                                        &tx,
                                        grove_version,
                                    )
                                );
                                let payload_bytes =
                                    cost_return_on_error_no_add!(cost, payload.encode_canonical());
                                lower_layers.insert(
                                    key.clone(),
                                    LayerProof {
                                        merk_proof:
                                            crate::operations::proof::ProofBytes::SumBudgetWindow(
                                                payload_bytes,
                                            ),
                                        lower_layers: Default::default(),
                                    },
                                );
                                has_a_result_at_level |= true;
                            }

                            // MmrTree with subquery → generate MMR proof
                            // root_key is always None for MmrTree (no child Merk data)
                            Ok(Element::MmrTree(mmr_size, _))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.generate_mmr_layer_proof(
                                        &lower_path,
                                        path_query,
                                        mmr_size,
                                        overall_limit,
                                        &tx,
                                        grove_version,
                                    )
                                );

                                has_a_result_at_level |= true;
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // BulkAppendTree with subquery → generate BulkAppend proof
                            // root_key is always None for BulkAppendTree (no child Merk data)
                            Ok(Element::BulkAppendTree(total_count, chunk_power, _))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.generate_bulk_append_layer_proof(
                                        &lower_path,
                                        path_query,
                                        [0u8; 32], // unused parameter
                                        total_count,
                                        chunk_power,
                                        overall_limit,
                                        &tx,
                                        grove_version,
                                    )
                                );

                                has_a_result_at_level |= true;
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // DenseAppendOnlyFixedSizeTree with subquery → generate
                            // dense tree proof
                            Ok(Element::DenseAppendOnlyFixedSizeTree(
                                dense_count,
                                dense_height,
                                _,
                            )) if !done_with_results
                                && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.generate_dense_tree_layer_proof(
                                        &lower_path,
                                        path_query,
                                        dense_count,
                                        dense_height,
                                        overall_limit,
                                        &tx,
                                        grove_version,
                                    )
                                );

                                has_a_result_at_level |= true;
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // CommitmentTree with subquery → generate proof
                            // that includes sinsemilla_root for anchor binding
                            Ok(Element::CommitmentTree(total_count, chunk_power, _))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.generate_commitment_tree_layer_proof(
                                        &lower_path,
                                        path_query,
                                        total_count,
                                        chunk_power,
                                        overall_limit,
                                        &tx,
                                        grove_version,
                                    )
                                );

                                has_a_result_at_level |= true;
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // Axis-ordered read of an indexed tree: the
                            // query node governing this element carries
                            // ReadMode::Axis, so instead of descending the
                            // primary the layer carries an axis-descent
                            // payload — a proof over the queried per-axis
                            // secondary. Matched before the primary-descent
                            // arms below so an axis read can never be
                            // silently served as a primary descent; matches
                            // empty primaries too (the payload commits
                            // NULL_HASH roots naturally).
                            Ok(Element::ProvableCountIndexedTree(..))
                            | Ok(Element::ProvableSumIndexedTree(..))
                            | Ok(Element::ProvableCountProvableSumIndexedTree(..))
                                if !done_with_results && {
                                    let mut lower_path = path.clone();
                                    lower_path.push(key.as_slice());
                                    path_query.axis_read_at_path(&lower_path).is_some()
                                } =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());
                                let Some(axis_query) = path_query.axis_read_at_path(&lower_path)
                                else {
                                    return Err(Error::CorruptedCodeExecution(
                                        "axis read vanished between the match guard and the arm",
                                    ))
                                    .wrap_with_cost(cost);
                                };

                                // The axis descent is a GROVE_V4 envelope
                                // capability; older versions refuse to emit
                                // it, mirroring the verifier-side gate.
                                if grove_version
                                    .grovedb_versions
                                    .operations
                                    .proof
                                    .axis_descent_in_v1_envelope
                                    != 1
                                {
                                    return Err(Error::NotSupported(
                                        "axis-ordered descents in the V1 proof envelope are \
                                         not emitted at this grove version"
                                            .to_string(),
                                    ))
                                    .wrap_with_cost(cost);
                                }

                                let payload_batch = grovedb_storage::StorageBatch::new();
                                let lower_subtree_path: grovedb_path::SubtreePath<&[u8]> =
                                    lower_path.as_slice().into();
                                let payload = cost_return_on_error!(
                                    &mut cost,
                                    self.build_axis_descent_payload(
                                        lower_subtree_path,
                                        axis_query,
                                        &tx,
                                        &payload_batch,
                                        grove_version,
                                    )
                                );
                                let payload_bytes =
                                    cost_return_on_error_no_add!(cost, payload.encode_canonical());
                                lower_layers.insert(
                                    key.clone(),
                                    LayerProof {
                                        merk_proof:
                                            crate::operations::proof::ProofBytes::IndexedTreeAxisDescent(
                                                payload_bytes,
                                            ),
                                        lower_layers: Default::default(),
                                    },
                                );
                                has_a_result_at_level |= true;
                            }

                            // Subquery into CountIndexedTree: descend into
                            // the primary like a regular tree, then wrap
                            // the resulting Merk proof bytes with a 32-byte
                            // attestation of the cidx's secondary root
                            // hash. The verifier consumes
                            // ProofBytes::CountIndexedTree(secondary ‖
                            // primary_proof) and chains via
                            // combine_hash_three at this layer. Callers who
                            // want secondary-ordered output should use
                            // prove_indexed_count_top_k.
                            // Cidx descent only for NON-EMPTY primary
                            // (Some(_)): mirrors the regular-tree
                            // pattern above. An empty cidx primary
                            // (None) is handled by the empty-tree arm
                            // below, which decrements the limit
                            // without recursing — emitting a wrapped
                            // ProofBytes::CountIndexedTree for an
                            // empty primary would carry a degenerate
                            // (zero-secondary-hash + empty merk proof)
                            // payload that the verifier handles via
                            // the matching empty-cidx terminal arm.
                            Ok(ref cidx_elem @ Element::ProvableCountIndexedTree(Some(_), ..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                // Aggregate carrier queries (AggregateCountOnRange /
                                // AggregateSumOnRange) are verified by
                                // `operations::proof::aggregate_common`, whose
                                // `expect_merk_bytes` accepts only
                                // `ProofBytes::Merk` and whose chain check uses the
                                // two-input `combine_hash`. Neither can consume the
                                // indexed envelope this arm is about to emit, so the
                                // proof would verify-fail at the far end. Reject here
                                // instead of shipping an unverifiable proof.
                                if is_aggregate_count_query
                                    || is_aggregate_sum_query
                                    || is_aggregate_count_and_sum_query
                                {
                                    return Err(Error::NotSupported(
                                        "aggregate-on-range carrier queries cannot descend \
                                         through an indexed tree (PCIT / PSIT / PCPSIT); use \
                                         the dedicated indexed-axis aggregate proofs \
                                         (prove_indexed_count_aggregate_over_value_range / \
                                         prove_indexed_sum_aggregate_over_value_range) instead"
                                            .to_string(),
                                    ))
                                    .wrap_with_cost(cost);
                                }

                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let mut layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries_v1(
                                        lower_path.clone(),
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                // Capture the cidx's current secondary
                                // root hash for the verifier's
                                // combine_hash_three attestation.
                                let secondary_root_key = match cidx_elem {
                                    Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
                                    _ => unreachable!(),
                                };
                                let lower_path_owned: Vec<Vec<u8>> =
                                    lower_path.iter().map(|p| p.to_vec()).collect();
                                let lower_path_refs: Vec<&[u8]> =
                                    lower_path_owned.iter().map(|v| v.as_slice()).collect();
                                let cidx_subtree_path: grovedb_path::SubtreePath<&[u8]> =
                                    lower_path_refs.as_slice().into();
                                let secondary_merk = cost_return_on_error!(
                                    &mut cost,
                                    self.open_indexed_secondary_at_path(
                                        cidx_subtree_path,
                                        grovedb_element::indexed::IndexAxis::Count,
                                        secondary_root_key,
                                        &tx,
                                        None,
                                        grove_version,
                                    )
                                );
                                let (sec_root, _, _) = cost_return_on_error!(
                                    &mut cost,
                                    secondary_merk
                                        .root_hash_key_and_aggregate_data()
                                        .map_err(Error::MerkError)
                                );

                                // Re-wrap merk_proof bytes with the
                                // 32-byte secondary attestation prefix.
                                let primary_bytes = match layer_proof.merk_proof {
                                    crate::operations::proof::ProofBytes::Merk(b) => b,
                                    _ => {
                                        return Err(Error::CorruptedCodeExecution(
                                            "expected Merk proof bytes from prove_subqueries_v1 \
                                             for cidx primary",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                };
                                let mut wrapped = Vec::with_capacity(32 + primary_bytes.len());
                                wrapped.extend_from_slice(&sec_root);
                                wrapped.extend_from_slice(&primary_bytes);
                                layer_proof.merk_proof =
                                    crate::operations::proof::ProofBytes::CountIndexedTree(wrapped);

                                if previous_limit != *overall_limit {
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // Subquery into ProvableSumIndexedTree: identical
                            // shape to the PCIT descent above. Descend into the
                            // primary Merk, then re-wrap the primary proof bytes
                            // with the 32-byte Sum-axis secondary root hash. The
                            // verifier consumes ProofBytes::CountIndexedTree
                            // (secondary ‖ primary_proof) and chains via
                            // combine_hash_three(H(value), primary_root,
                            // secondary_root). Only NON-EMPTY primaries
                            // (Some(_)) descend; an empty PSIT primary is handled
                            // by the empty-tree arm below (decrement limit, no
                            // recursion, matching the empty-cidx terminal path in
                            // the verifier).
                            Ok(ref sidx_elem @ Element::ProvableSumIndexedTree(Some(_), ..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                // Aggregate carrier queries (AggregateCountOnRange /
                                // AggregateSumOnRange) are verified by
                                // `operations::proof::aggregate_common`, whose
                                // `expect_merk_bytes` accepts only
                                // `ProofBytes::Merk` and whose chain check uses the
                                // two-input `combine_hash`. Neither can consume the
                                // indexed envelope this arm is about to emit, so the
                                // proof would verify-fail at the far end. Reject here
                                // instead of shipping an unverifiable proof.
                                if is_aggregate_count_query
                                    || is_aggregate_sum_query
                                    || is_aggregate_count_and_sum_query
                                {
                                    return Err(Error::NotSupported(
                                        "aggregate-on-range carrier queries cannot descend \
                                         through an indexed tree (PCIT / PSIT / PCPSIT); use \
                                         the dedicated indexed-axis aggregate proofs \
                                         (prove_indexed_count_aggregate_over_value_range / \
                                         prove_indexed_sum_aggregate_over_value_range) instead"
                                            .to_string(),
                                    ))
                                    .wrap_with_cost(cost);
                                }

                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let mut layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries_v1(
                                        lower_path.clone(),
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                // Capture the PSIT's current Sum-axis secondary
                                // root hash for the verifier's combine_hash_three
                                // attestation.
                                let secondary_root_key = match sidx_elem {
                                    Element::ProvableSumIndexedTree(_, s, ..) => s.clone(),
                                    _ => unreachable!(),
                                };
                                let lower_path_owned: Vec<Vec<u8>> =
                                    lower_path.iter().map(|p| p.to_vec()).collect();
                                let lower_path_refs: Vec<&[u8]> =
                                    lower_path_owned.iter().map(|v| v.as_slice()).collect();
                                let sidx_subtree_path: grovedb_path::SubtreePath<&[u8]> =
                                    lower_path_refs.as_slice().into();
                                let secondary_merk = cost_return_on_error!(
                                    &mut cost,
                                    self.open_indexed_secondary_at_path(
                                        sidx_subtree_path,
                                        grovedb_element::indexed::IndexAxis::Sum,
                                        secondary_root_key,
                                        &tx,
                                        None,
                                        grove_version,
                                    )
                                );
                                let (sec_root, _, _) = cost_return_on_error!(
                                    &mut cost,
                                    secondary_merk
                                        .root_hash_key_and_aggregate_data()
                                        .map_err(Error::MerkError)
                                );

                                let primary_bytes = match layer_proof.merk_proof {
                                    crate::operations::proof::ProofBytes::Merk(b) => b,
                                    _ => {
                                        return Err(Error::CorruptedCodeExecution(
                                            "expected Merk proof bytes from prove_subqueries_v1 \
                                             for psit primary",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                };
                                let mut wrapped = Vec::with_capacity(32 + primary_bytes.len());
                                wrapped.extend_from_slice(&sec_root);
                                wrapped.extend_from_slice(&primary_bytes);
                                layer_proof.merk_proof =
                                    crate::operations::proof::ProofBytes::CountIndexedTree(wrapped);

                                if previous_limit != *overall_limit {
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // Subquery into ProvableCountProvableSumIndexedTree:
                            // same envelope as PCIT/PSIT, but the 32-byte
                            // attestation prefix is the axes_digest — the
                            // canonical digest over the element's axes list, each
                            // axis carrying its secondary Merk's current root hash
                            // (NULL_HASH for an empty axis). The verifier chains
                            // via combine_hash_three(H(value), primary_root,
                            // axes_digest), mirroring the insert commit path.
                            // Only NON-EMPTY primaries (Some(_)) descend.
                            Ok(
                                ref pcpsit_elem @ Element::ProvableCountProvableSumIndexedTree(
                                    Some(_),
                                    ..,
                                ),
                            ) if !done_with_results
                                && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                // See the PCIT arm: the aggregate-carrier
                                // verifier cannot consume an indexed envelope.
                                if is_aggregate_count_query
                                    || is_aggregate_sum_query
                                    || is_aggregate_count_and_sum_query
                                {
                                    return Err(Error::NotSupported(
                                        "aggregate-on-range carrier queries cannot descend \
                                         through an indexed tree (PCIT / PSIT / PCPSIT); use \
                                         the dedicated indexed-axis aggregate proofs \
                                         (prove_indexed_count_aggregate_over_value_range / \
                                         prove_indexed_sum_aggregate_over_value_range) instead"
                                            .to_string(),
                                    ))
                                    .wrap_with_cost(cost);
                                }

                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let mut layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries_v1(
                                        lower_path.clone(),
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                // Recompute the axes_digest over each axis's live
                                // secondary root hash (NULL_HASH when empty),
                                // exactly as the insert commit path does.
                                let axes = match pcpsit_elem {
                                    Element::ProvableCountProvableSumIndexedTree(_, _, _, a, _) => {
                                        a.clone()
                                    }
                                    _ => unreachable!(),
                                };
                                let lower_path_owned: Vec<Vec<u8>> =
                                    lower_path.iter().map(|p| p.to_vec()).collect();
                                let lower_path_refs: Vec<&[u8]> =
                                    lower_path_owned.iter().map(|v| v.as_slice()).collect();
                                let mut axis_hashes: Vec<(u8, grovedb_merk::CryptoHash)> =
                                    Vec::with_capacity(axes.len());
                                for (tag, sec_root_key) in axes.iter() {
                                    let axis = cost_return_on_error_no_add!(
                                        cost,
                                        grovedb_element::indexed::IndexAxis::try_from_tag(*tag)
                                            .map_err(|e| Error::CorruptedData(format!(
                                                "invalid axis tag in PCPSIT during proof: {e}"
                                            )))
                                    );
                                    let pcpsit_subtree_path: grovedb_path::SubtreePath<&[u8]> =
                                        lower_path_refs.as_slice().into();
                                    let secondary_merk = cost_return_on_error!(
                                        &mut cost,
                                        self.open_indexed_secondary_at_path(
                                            pcpsit_subtree_path,
                                            axis,
                                            sec_root_key.clone(),
                                            &tx,
                                            None,
                                            grove_version,
                                        )
                                    );
                                    let (s_hash, _, _) = cost_return_on_error!(
                                        &mut cost,
                                        secondary_merk
                                            .root_hash_key_and_aggregate_data()
                                            .map_err(Error::MerkError)
                                    );
                                    axis_hashes.push((*tag, s_hash));
                                }
                                let digest = grovedb_merk::tree::axes_digest(&axis_hashes)
                                    .unwrap_add_cost(&mut cost);

                                let primary_bytes = match layer_proof.merk_proof {
                                    crate::operations::proof::ProofBytes::Merk(b) => b,
                                    _ => {
                                        return Err(Error::CorruptedCodeExecution(
                                            "expected Merk proof bytes from prove_subqueries_v1 \
                                             for pcpsit primary",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                };
                                let mut wrapped = Vec::with_capacity(32 + primary_bytes.len());
                                wrapped.extend_from_slice(&digest);
                                wrapped.extend_from_slice(&primary_bytes);
                                layer_proof.merk_proof =
                                    crate::operations::proof::ProofBytes::CountIndexedTree(wrapped);

                                if previous_limit != *overall_limit {
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // Other tree types with subqueries → recurse into Merk
                            Ok(Element::Tree(Some(_), _))
                            | Ok(Element::SumTree(Some(_), ..))
                            | Ok(Element::BigSumTree(Some(_), ..))
                            | Ok(Element::CountTree(Some(_), ..))
                            | Ok(Element::CountSumTree(Some(_), ..))
                            | Ok(Element::ProvableCountTree(Some(_), ..))
                            | Ok(Element::ProvableCountSumTree(Some(_), ..))
                            | Ok(Element::ProvableSumTree(Some(_), ..))
                            | Ok(Element::ProvableCountProvableSumTree(Some(_), ..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries_v1(
                                        lower_path,
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                if previous_limit != *overall_limit {
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            // Non-Merk tree that is itself the result, with
                            // nothing queried below it. These types have no
                            // child Merk, so there is no lower layer to bind
                            // them — a bare `KVValueHash` node hashes only
                            // (key, value_hash) and would leave the element
                            // bytes (and with them the entry count a caller
                            // reads) free for a prover to forge under a
                            // genuine root hash. Their parent commits
                            // `combine_hash(H(value), state_root)`, exactly
                            // the two-input form
                            // `KVValueHashFeatureTypeWithChildHash` is
                            // verified with, so carry the state root in the
                            // node and let the merk verifier close the loop.
                            //
                            // Version-gated on
                            // `proof.terminal_non_merk_tree_child_hash`, so the
                            // binding itself lives in
                            // `bind_terminal_non_merk_tree`: deriving the state
                            // root costs storage reads and hash calls that
                            // V1..V3 did not pay, and cost feeds fees. Under
                            // those versions the node is left as the prover has
                            // always emitted it and only the limit moves.
                            Ok(ref non_merk_elem @ Element::MmrTree(..))
                            | Ok(ref non_merk_elem @ Element::BulkAppendTree(..))
                            | Ok(
                                ref non_merk_elem @ Element::DenseAppendOnlyFixedSizeTree(..),
                            )
                            | Ok(ref non_merk_elem @ Element::CommitmentTree(..))
                                if !done_with_results =>
                            {
                                cost_return_on_error!(
                                    &mut cost,
                                    self.bind_terminal_non_merk_tree(
                                        node,
                                        non_merk_elem,
                                        &path,
                                        &tx,
                                        grove_version,
                                    )
                                );

                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }

                            Ok(Element::Tree(Some(_), _))
                            | Ok(Element::SumTree(Some(_), ..))
                            | Ok(Element::BigSumTree(Some(_), ..))
                            | Ok(Element::CountTree(Some(_), ..))
                            | Ok(Element::ProvableCountTree(Some(_), ..))
                            | Ok(Element::CountSumTree(Some(_), ..))
                            | Ok(Element::ProvableCountSumTree(Some(_), ..))
                            | Ok(Element::ProvableSumTree(Some(_), ..))
                            | Ok(Element::ProvableCountProvableSumTree(Some(_), ..))
                                if !done_with_results =>
                            {
                                // Non-empty tree without subquery: inject child
                                // root hash for combine_hash verification
                                let mut child_path = path.clone();
                                child_path.push(key.as_slice());
                                let child_merk = cost_return_on_error!(
                                    &mut cost,
                                    self.open_transactional_merk_at_path(
                                        child_path.as_slice().into(),
                                        &tx,
                                        None,
                                        grove_version
                                    )
                                );
                                let child_root_hash =
                                    child_merk.root_hash().unwrap_add_cost(&mut cost);

                                let key_owned = key.to_owned();
                                let value_owned = value.to_owned();
                                let (vh, ft) = match node {
                                    Node::KVValueHashFeatureType(_, _, vh, ft) => (*vh, *ft),
                                    Node::KVValueHash(_, _, vh) => {
                                        (*vh, TreeFeatureType::BasicMerkNode)
                                    }
                                    _ => {
                                        let element_vh =
                                            value_hash(&value_owned).unwrap_add_cost(&mut cost);
                                        let vh = combine_hash(&element_vh, &child_root_hash)
                                            .unwrap_add_cost(&mut cost);
                                        (vh, TreeFeatureType::BasicMerkNode)
                                    }
                                };
                                *node = Node::KVValueHashFeatureTypeWithChildHash(
                                    key_owned,
                                    value_owned,
                                    vh,
                                    ft,
                                    child_root_hash,
                                );

                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            // Non-empty indexed tree that is itself a result,
                            // with nothing queried below it. Regular trees are
                            // handled just above by upgrading the node to
                            // `KVValueHashFeatureTypeWithChildHash`, but that
                            // node type is verified with the two-input
                            // `combine_hash(H(value), child_hash)`, which can
                            // never reproduce an indexed tree's three-input
                            // `combine_hash_three(H(value), primary_root,
                            // attestation)` binding. Emit a terminal
                            // attestation lower layer carrying both hashes
                            // instead; the verifier performs the three-input
                            // check itself. Without this the prover emitted an
                            // unbound node and every honest proof containing a
                            // populated indexed tree was rejected.
                            Ok(ref indexed_elem @ Element::ProvableCountIndexedTree(Some(_), ..))
                            | Ok(ref indexed_elem @ Element::ProvableSumIndexedTree(Some(_), ..))
                            | Ok(
                                ref indexed_elem @ Element::ProvableCountProvableSumIndexedTree(
                                    Some(_),
                                    ..,
                                ),
                            ) if !done_with_results => {
                                let mut indexed_path = path.clone();
                                indexed_path.push(key.as_slice());

                                let primary_merk = cost_return_on_error!(
                                    &mut cost,
                                    self.open_transactional_merk_at_path(
                                        indexed_path.as_slice().into(),
                                        &tx,
                                        None,
                                        grove_version
                                    )
                                );
                                let primary_root_hash =
                                    primary_merk.root_hash().unwrap_add_cost(&mut cost);

                                let attestation = cost_return_on_error!(
                                    &mut cost,
                                    self.indexed_secondary_attestation(
                                        indexed_elem,
                                        indexed_path.as_slice(),
                                        &tx,
                                        grove_version,
                                    )
                                );

                                let mut terminal = Vec::with_capacity(64);
                                terminal.extend_from_slice(&attestation);
                                terminal.extend_from_slice(&primary_root_hash);
                                lower_layers.insert(
                                    key.clone(),
                                    LayerProof {
                                        merk_proof:
                                            crate::operations::proof::ProofBytes::IndexedTreeTerminal(
                                                terminal,
                                            ),
                                        lower_layers: BTreeMap::new(),
                                    },
                                );

                                // The verifier pushes this element as a result
                                // and decrements, so account for it here too.
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }

                            // Empty count trees under an aggregate-count
                            // carrier still need a lower-layer descent —
                            // the recursion hits the ACOR short-circuit on
                            // the empty merk and emits an empty count proof
                            // (verifier reads it as count = 0).
                            Ok(Element::ProvableCountTree(None, ..))
                            | Ok(Element::ProvableCountSumTree(None, ..))
                            | Ok(Element::ProvableCountProvableSumTree(None, ..))
                                if !done_with_results
                                    && is_aggregate_count_query
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries_v1(
                                        lower_path,
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                if previous_limit != *overall_limit {
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }
                            // Same descent path for sum-bearing empty
                            // hosts (ProvableSumTree and
                            // ProvableCountProvableSumTree) when the
                            // outer query has an
                            // `AggregateSumOnRange` carrier — recurses
                            // into prove_subqueries_v1, hits the ASOR
                            // short-circuit on the empty merk, and
                            // emits an empty sum proof (verifier reads
                            // it as sum = 0).
                            Ok(Element::ProvableSumTree(None, ..))
                            | Ok(Element::ProvableCountProvableSumTree(None, ..))
                                if !done_with_results
                                    && is_aggregate_sum_query
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries_v1(
                                        lower_path,
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                if previous_limit != *overall_limit {
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }
                            // Combined-aggregate carrier descent for an
                            // empty PCPS host: recurse so the
                            // combined-aggregate short-circuit at the
                            // leaf emits an empty proof (count = 0,
                            // sum = 0).
                            Ok(Element::ProvableCountProvableSumTree(None, ..))
                                if !done_with_results
                                    && is_aggregate_count_and_sum_query
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries_v1(
                                        lower_path,
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        current_depth + 1,
                                        grove_version,
                                    )
                                );

                                if previous_limit != *overall_limit {
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }
                            // Empty trees without subquery. CommitmentTree is
                            // NOT here — like the other non-Merk trees it is
                            // bound by the child-hash arm above, which applies
                            // whether or not it holds any notes.
                            Ok(Element::Tree(None, _))
                            | Ok(Element::SumTree(None, ..))
                            | Ok(Element::BigSumTree(None, ..))
                            | Ok(Element::CountTree(None, ..))
                            | Ok(Element::ProvableCountTree(None, ..))
                            | Ok(Element::CountSumTree(None, ..))
                            | Ok(Element::ProvableCountSumTree(None, ..))
                            | Ok(Element::ProvableSumTree(None, ..))
                            | Ok(Element::ProvableCountProvableSumTree(None, ..))
                            | Ok(Element::ProvableSumIndexedTree(None, ..))
                            | Ok(Element::ProvableCountIndexedTree(None, ..))
                            // PCPSIT empty form: primary root key is None AND the axes TLV is
                            // empty of secondary keys. We still treat any None-primary PCPSIT
                            // as "empty" for the purposes of the limit decrement — even if
                            // some axes have populated secondaries, we don't recurse here.
                            | Ok(Element::ProvableCountProvableSumIndexedTree(None, ..))
                            | Ok(Element::CommitmentTree(..))
                                if !done_with_results =>
                            {
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }

                            // Explicit: when done_with_results is true, the above guards fail
                            // and we skip. Listed explicitly so adding a new Element variant
                            // produces a compile error here instead of silently dropping it.
                            Ok(Element::Item(..))
                            | Ok(Element::SumItem(..))
                            | Ok(Element::ItemWithSumItem(..))
                            | Ok(Element::Tree(..))
                            | Ok(Element::SumTree(..))
                            | Ok(Element::BigSumTree(..))
                            | Ok(Element::CountTree(..))
                            | Ok(Element::CountSumTree(..))
                            | Ok(Element::ProvableCountTree(..))
                            | Ok(Element::ProvableCountSumTree(..))
                            | Ok(Element::ProvableSumTree(..))
                            | Ok(Element::ProvableCountProvableSumTree(..))
                            | Ok(Element::CommitmentTree(..))
                            | Ok(Element::MmrTree(..))
                            | Ok(Element::BulkAppendTree(..))
                            | Ok(Element::DenseAppendOnlyFixedSizeTree(..))
                            | Ok(Element::ProvableSumIndexedTree(..))
                            | Ok(Element::ProvableCountIndexedTree(..))
                            | Ok(Element::ProvableCountProvableSumIndexedTree(..)) => continue,
                            // NonCounted is unwrapped above via into_underlying().
                            Ok(Element::NonCounted(_))
                            | Ok(Element::NotSummed(_))
                            | Ok(Element::NotCountedOrSummed(_)) => {
                                unreachable!("unwrapped above")
                            }
                            Err(e) => {
                                return Err(Error::CorruptedData(format!(
                                    "failed to deserialize element during proof generation: {e}"
                                )))
                                .wrap_with_cost(cost);
                            }
                        }
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }

        if !has_a_result_at_level
            && !done_with_results
            && prove_options.decrease_limit_on_empty_sub_query_result
            && let Some(limit) = overall_limit.as_mut()
        {
            *limit -= 1;
        }

        let mut serialized_merk_proof = Vec::with_capacity(1024);
        encode_into(merk_proof.proof.iter(), &mut serialized_merk_proof);

        Ok(LayerProof {
            merk_proof: ProofBytes::Merk(serialized_merk_proof),
            lower_layers,
        })
        .wrap_with_cost(cost)
    }

    /// Generate an MMR tree layer proof for a subquery.
    fn generate_mmr_layer_proof(
        &self,
        subtree_path: &[&[u8]],
        path_query: &PathQuery,
        mmr_size: u64,
        overall_limit: &mut Option<u16>,
        tx: &crate::Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        // Get the subquery items for this path to determine which leaf indices to prove
        let sub_query = cost_return_on_error_no_add!(
            cost,
            path_query
                .query_items_at_path(subtree_path, grove_version)
                .and_then(|q| {
                    q.ok_or(Error::CorruptedPath(
                        "MMR subtree path not in path_query".into(),
                    ))
                })
        );

        // Convert query items to leaf indices (keys are BE u64 bytes)
        let leaf_indices = cost_return_on_error_no_add!(
            cost,
            Self::query_items_to_leaf_indices(&sub_query.items, mmr_size)
        );

        // An empty MMR (mmr_size == 0) has no nodes to prove. Return an empty
        // proof directly instead of calling MmrTreeProof::generate which
        // rejects empty leaf_indices.
        if mmr_size == 0 {
            let empty_proof = MmrTreeProof::new(mmr_size, vec![], vec![]);
            let proof_bytes = cost_return_on_error_no_add!(
                cost,
                empty_proof
                    .encode_to_vec()
                    .map_err(|e| Error::CorruptedData(format!(
                        "failed to encode MmrTreeProof: {}",
                        e
                    )))
            );
            return Ok(LayerProof {
                merk_proof: ProofBytes::MMR(proof_bytes),
                lower_layers: BTreeMap::new(),
            })
            .wrap_with_cost(cost);
        }

        // Open aux storage at the subtree path
        let path_vec: Vec<Vec<u8>> = subtree_path.iter().map(|s| s.to_vec()).collect();
        let path_refs: Vec<&[u8]> = path_vec.iter().map(|v| v.as_slice()).collect();
        let storage_path = grovedb_path::SubtreePath::from(path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_transactional_storage_context(storage_path, None, tx)
            .unwrap_add_cost(&mut cost);

        // Generate the MMR proof using MmrStore for correct key format
        let store = grovedb_merkle_mountain_range::MmrStore::new(&storage_ctx);
        let mmr_proof = cost_return_on_error_no_add!(
            cost,
            MmrTreeProof::generate(mmr_size, &leaf_indices, |pos| {
                use grovedb_merkle_mountain_range::MMRStoreReadOps;
                let store_ref: &grovedb_merkle_mountain_range::MmrStore<_> = &store;
                let result = store_ref.element_at_position(pos);
                result.value.map_err(|e| {
                    grovedb_merkle_mountain_range::Error::OperationFailed(format!(
                        "storage error: {}",
                        e
                    ))
                })
            })
            .map_err(|e| Error::CorruptedData(format!("{}", e)))
        );

        // Update limit
        if let Some(limit) = overall_limit.as_mut() {
            let count = mmr_proof.leaves().len().min(u16::MAX as usize) as u16;
            *limit = limit.saturating_sub(count);
        }

        let proof_bytes = cost_return_on_error_no_add!(
            cost,
            mmr_proof
                .encode_to_vec()
                .map_err(|e| Error::CorruptedData(format!("{}", e)))
        );

        Ok(LayerProof {
            merk_proof: ProofBytes::MMR(proof_bytes),
            lower_layers: BTreeMap::new(),
        })
        .wrap_with_cost(cost)
    }

    /// Generate a BulkAppendTree layer proof for a subquery.
    fn generate_bulk_append_layer_proof(
        &self,
        subtree_path: &[&[u8]],
        path_query: &PathQuery,
        _state_root: [u8; 32],
        total_count: u64,
        chunk_power: u8,
        overall_limit: &mut Option<u16>,
        tx: &crate::Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        // Get the subquery items for this path
        let sub_query = cost_return_on_error_no_add!(
            cost,
            path_query
                .query_items_at_path(subtree_path, grove_version)
                .and_then(|q| {
                    q.ok_or(Error::CorruptedPath(
                        "BulkAppendTree subtree path not in path_query".into(),
                    ))
                })
        );

        // Convert query items to a position range
        let (start, end) = cost_return_on_error_no_add!(
            cost,
            Self::query_items_to_range(&sub_query.items, total_count)
        );

        // Open aux storage
        let path_vec: Vec<Vec<u8>> = subtree_path.iter().map(|s| s.to_vec()).collect();
        let path_refs: Vec<&[u8]> = path_vec.iter().map(|v| v.as_slice()).collect();
        let storage_path = grovedb_path::SubtreePath::from(path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_transactional_storage_context(storage_path, None, tx)
            .unwrap_add_cost(&mut cost);

        // Create BulkAppendTree from state with embedded storage
        let tree = cost_return_on_error_no_add!(
            cost,
            grovedb_bulk_append_tree::BulkAppendTree::from_state(
                total_count,
                chunk_power,
                storage_ctx,
            )
            .map_err(|e| Error::CorruptedData(format!("failed to create BulkAppendTree: {}", e)))
        );

        // Build a Query from the subquery items for the proof generator
        let bulk_query = grovedb_query::Query {
            items: sub_query.items.to_vec(),
            left_to_right: sub_query.left_to_right,
            ..grovedb_query::Query::default()
        };

        // Generate the BulkAppendTree proof
        let bulk_proof = cost_return_on_error_no_add!(
            cost,
            BulkAppendTreeProof::generate(&bulk_query, &tree)
                .map_err(|e| Error::CorruptedData(format!("{}", e)))
        );

        // Update limit: count individual values in the queried range
        if let Some(limit) = overall_limit.as_mut() {
            let count = (end.min(total_count) - start).min(u16::MAX as u64) as u16;
            *limit = limit.saturating_sub(count);
        }

        let proof_bytes = cost_return_on_error_no_add!(
            cost,
            bulk_proof
                .encode_to_vec()
                .map_err(|e| Error::CorruptedData(format!("{}", e)))
        );

        Ok(LayerProof {
            merk_proof: ProofBytes::BulkAppendTree(proof_bytes),
            lower_layers: BTreeMap::new(),
        })
        .wrap_with_cost(cost)
    }

    /// Generate a CommitmentTree layer proof that includes the Sinsemilla root.
    ///
    /// The proof bytes are: `sinsemilla_root (32 bytes) || bulk_append_proof`.
    /// This binds the Orchard anchor to the GroveDB root hash, allowing the
    /// verifier to reconstruct the combined state root.
    fn generate_commitment_tree_layer_proof(
        &self,
        subtree_path: &[&[u8]],
        path_query: &PathQuery,
        total_count: u64,
        chunk_power: u8,
        overall_limit: &mut Option<u16>,
        tx: &crate::Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        // 1. Read the Sinsemilla frontier from storage to get the current root
        let path_vec: Vec<Vec<u8>> = subtree_path.iter().map(|s| s.to_vec()).collect();
        let path_refs: Vec<&[u8]> = path_vec.iter().map(|v| v.as_slice()).collect();
        let storage_path = grovedb_path::SubtreePath::from(path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_transactional_storage_context(storage_path, None, tx)
            .unwrap_add_cost(&mut cost);

        let sinsemilla_root = match storage_ctx.get(COMMITMENT_TREE_DATA_KEY).value {
            Ok(Some(frontier_bytes)) => {
                match grovedb_commitment_tree::CommitmentFrontier::deserialize(
                    frontier_bytes.as_ref(),
                ) {
                    Ok(frontier) => frontier.root_hash(),
                    Err(_) => grovedb_commitment_tree::EMPTY_SINSEMILLA_ROOT,
                }
            }
            _ => grovedb_commitment_tree::EMPTY_SINSEMILLA_ROOT,
        };
        drop(storage_ctx);

        // 2. Generate the BulkAppendTree proof (reuse existing method)
        let bulk_layer_proof = cost_return_on_error!(
            &mut cost,
            self.generate_bulk_append_layer_proof(
                subtree_path,
                path_query,
                [0u8; 32],
                total_count,
                chunk_power,
                overall_limit,
                tx,
                grove_version,
            )
        );

        // 3. Extract bulk proof bytes and prepend sinsemilla_root
        let bulk_bytes = match bulk_layer_proof.merk_proof {
            ProofBytes::BulkAppendTree(bytes) => bytes,
            _ => {
                return Err(Error::InternalError(
                    "expected BulkAppendTree proof bytes from generate_bulk_append_layer_proof"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        let mut combined_bytes = Vec::with_capacity(32 + bulk_bytes.len());
        combined_bytes.extend_from_slice(&sinsemilla_root);
        combined_bytes.extend_from_slice(&bulk_bytes);

        Ok(LayerProof {
            merk_proof: ProofBytes::CommitmentTree(combined_bytes),
            lower_layers: BTreeMap::new(),
        })
        .wrap_with_cost(cost)
    }

    /// Generate a DenseAppendOnlyFixedSizeTree layer proof for a subquery.
    fn generate_dense_tree_layer_proof(
        &self,
        subtree_path: &[&[u8]],
        path_query: &PathQuery,
        dense_count: u16,
        dense_height: u8,
        overall_limit: &mut Option<u16>,
        tx: &crate::Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        // Get the subquery items for this path to determine which positions to prove
        let sub_query = cost_return_on_error_no_add!(
            cost,
            path_query
                .query_items_at_path(subtree_path, grove_version)
                .and_then(|q| {
                    q.ok_or(Error::CorruptedPath(
                        "DenseTree subtree path not in path_query".into(),
                    ))
                })
        );

        // Convert query items to positions (same as MMR but capped by dense_count)
        let positions = cost_return_on_error_no_add!(
            cost,
            Self::query_items_to_positions(&sub_query.items, dense_count)
        );

        // Open storage at the subtree path
        let path_vec: Vec<Vec<u8>> = subtree_path.iter().map(|s| s.to_vec()).collect();
        let path_refs: Vec<&[u8]> = path_vec.iter().map(|v| v.as_slice()).collect();
        let storage_path = grovedb_path::SubtreePath::from(path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_transactional_storage_context(storage_path, None, tx)
            .unwrap_add_cost(&mut cost);

        // Create dense tree with embedded storage
        let tree = cost_return_on_error_no_add!(
            cost,
            grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree::from_state(
                dense_height,
                dense_count,
                storage_ctx,
            )
            .map_err(|e| Error::CorruptedData(format!("{}", e)))
        );

        // Generate the proof
        let dense_proof = cost_return_on_error!(
            &mut cost,
            DenseTreeProof::generate(&tree, &positions)
                .map_err(|e| Error::CorruptedData(format!("{}", e)))
        );

        // Update limit
        if let Some(limit) = overall_limit.as_mut() {
            let count = dense_proof.entries.len().min(u16::MAX as usize) as u16;
            *limit = limit.saturating_sub(count);
        }

        let proof_bytes = cost_return_on_error_no_add!(
            cost,
            dense_proof
                .encode_to_vec()
                .map_err(|e| Error::CorruptedData(format!("{}", e)))
        );

        Ok(LayerProof {
            merk_proof: ProofBytes::DenseTree(proof_bytes),
            lower_layers: BTreeMap::new(),
        })
        .wrap_with_cost(cost)
    }

    /// Convert query items to position indices for dense tree proofs.
    ///
    /// Query keys are interpreted as BE u16 bytes representing positions.
    fn query_items_to_positions(items: &[QueryItem], count: u16) -> Result<Vec<u16>, Error> {
        if count == 0 {
            return Ok(Vec::new());
        }

        const MAX_INDICES: usize = 65_535;
        let max_idx = count - 1;
        let mut indices = Vec::new();

        for item in items {
            match item {
                QueryItem::Key(key) => {
                    let idx = Self::decode_be_u16(key)?;
                    if idx < count {
                        indices.push(idx);
                    }
                }
                QueryItem::RangeInclusive(range) => {
                    let start = Self::decode_be_u16(range.start())?;
                    let end = Self::decode_be_u16(range.end())?;
                    for idx in start..=end.min(max_idx) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::Range(range) => {
                    let start = Self::decode_be_u16(&range.start)?;
                    let end = Self::decode_be_u16(&range.end)?;
                    for idx in start..end.min(count) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::RangeFrom(range) => {
                    let start = Self::decode_be_u16(&range.start)?;
                    for idx in start..count {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::RangeTo(range) => {
                    let end = Self::decode_be_u16(&range.end)?;
                    for idx in 0..end.min(count) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::RangeToInclusive(range) => {
                    let end = Self::decode_be_u16(&range.end)?;
                    for idx in 0..=end.min(max_idx) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::RangeFull(..) => {
                    for idx in 0..count {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::RangeAfter(range) => {
                    let start = Self::decode_be_u16(&range.start)?;
                    for idx in start.saturating_add(1)..count {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::RangeAfterTo(range) => {
                    let start = Self::decode_be_u16(&range.start)?;
                    let end = Self::decode_be_u16(&range.end)?;
                    for idx in start.saturating_add(1)..end.min(count) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::RangeAfterToInclusive(range) => {
                    let start = Self::decode_be_u16(range.start())?;
                    let end = Self::decode_be_u16(range.end())?;
                    for idx in start.saturating_add(1)..=end.min(max_idx) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput(
                                "query range too large for dense tree proof",
                            ));
                        }
                    }
                }
                QueryItem::AggregateCountOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountOnRange is only supported on provable count trees, \
                         not on dense fixed-size merkle trees",
                    ));
                }
                QueryItem::AggregateSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateSumOnRange is only supported on provable sum trees, \
                         not on dense fixed-size merkle trees",
                    ));
                }
                QueryItem::AggregateCountAndSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountAndSumOnRange is only supported on \
                         ProvableCountProvableSumTree, not on dense fixed-size merkle trees",
                    ));
                }
            }
        }

        indices.sort_unstable();
        indices.dedup();
        Ok(indices)
    }

    /// Convert query items to leaf indices for MMR proofs.
    ///
    /// Query keys are interpreted as BE u64 bytes representing leaf indices.
    fn query_items_to_leaf_indices(items: &[QueryItem], mmr_size: u64) -> Result<Vec<u64>, Error> {
        let leaf_count = grovedb_merkle_mountain_range::mmr_size_to_leaf_count(mmr_size);

        // Nothing to prove when MMR is empty
        if leaf_count == 0 {
            return Ok(Vec::new());
        }

        // Cap total expansion to avoid allocating billions of indices
        // for unbounded ranges. 10 million is generous for any real query.
        const MAX_INDICES: usize = 10_000_000;

        let max_idx = leaf_count - 1; // safe: leaf_count > 0
        let mut indices = Vec::new();

        for item in items {
            match item {
                QueryItem::Key(key) => {
                    let idx = Self::decode_be_u64(key)?;
                    if idx < leaf_count {
                        indices.push(idx);
                    }
                }
                QueryItem::RangeInclusive(range) => {
                    let start = Self::decode_be_u64(range.start())?;
                    let end = Self::decode_be_u64(range.end())?;
                    for idx in start..=end.min(max_idx) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::Range(range) => {
                    let start = Self::decode_be_u64(&range.start)?;
                    let end = Self::decode_be_u64(&range.end)?;
                    for idx in start..end.min(leaf_count) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::RangeFrom(range) => {
                    let start = Self::decode_be_u64(&range.start)?;
                    for idx in start..leaf_count {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::RangeTo(range) => {
                    let end = Self::decode_be_u64(&range.end)?;
                    for idx in 0..end.min(leaf_count) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::RangeToInclusive(range) => {
                    let end = Self::decode_be_u64(&range.end)?;
                    for idx in 0..=end.min(max_idx) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::RangeFull(..) => {
                    for idx in 0..leaf_count {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::RangeAfter(range) => {
                    let start = Self::decode_be_u64(&range.start)?;
                    for idx in start.saturating_add(1)..leaf_count {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::RangeAfterTo(range) => {
                    let start = Self::decode_be_u64(&range.start)?;
                    let end = Self::decode_be_u64(&range.end)?;
                    for idx in start.saturating_add(1)..end.min(leaf_count) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::RangeAfterToInclusive(range) => {
                    let start = Self::decode_be_u64(range.start())?;
                    let end = Self::decode_be_u64(range.end())?;
                    for idx in start.saturating_add(1)..=end.min(max_idx) {
                        indices.push(idx);
                        if indices.len() > MAX_INDICES {
                            return Err(Error::InvalidInput("query range too large for MMR proof"));
                        }
                    }
                }
                QueryItem::AggregateCountOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountOnRange is only supported on provable count trees, \
                         not on MMR trees",
                    ));
                }
                QueryItem::AggregateSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateSumOnRange is only supported on provable sum trees, \
                         not on MMR trees",
                    ));
                }
                QueryItem::AggregateCountAndSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountAndSumOnRange is only supported on \
                         ProvableCountProvableSumTree, not on MMR trees",
                    ));
                }
            }
        }

        indices.sort_unstable();
        indices.dedup();
        Ok(indices)
    }

    /// Convert query items to a position range [start, end) for BulkAppendTree.
    fn query_items_to_range(items: &[QueryItem], total_count: u64) -> Result<(u64, u64), Error> {
        let mut min_start = total_count;
        let mut max_end = 0u64;

        for item in items {
            match item {
                QueryItem::Key(key) => {
                    let pos = Self::decode_be_u64(key)?;
                    min_start = min_start.min(pos);
                    max_end = max_end.max(pos.saturating_add(1));
                }
                QueryItem::RangeInclusive(range) => {
                    let s = Self::decode_be_u64(range.start())?;
                    let e = Self::decode_be_u64(range.end())?;
                    min_start = min_start.min(s);
                    max_end = max_end.max(e.saturating_add(1));
                }
                QueryItem::Range(range) => {
                    let s = Self::decode_be_u64(&range.start)?;
                    let e = Self::decode_be_u64(&range.end)?;
                    min_start = min_start.min(s);
                    max_end = max_end.max(e);
                }
                QueryItem::RangeFrom(range) => {
                    let s = Self::decode_be_u64(&range.start)?;
                    min_start = min_start.min(s);
                    max_end = total_count;
                }
                QueryItem::RangeTo(range) => {
                    min_start = 0;
                    let e = Self::decode_be_u64(&range.end)?;
                    max_end = max_end.max(e);
                }
                QueryItem::RangeToInclusive(range) => {
                    min_start = 0;
                    let e = Self::decode_be_u64(&range.end)?;
                    max_end = max_end.max(e.saturating_add(1));
                }
                QueryItem::RangeFull(..) => {
                    min_start = 0;
                    max_end = total_count;
                }
                QueryItem::RangeAfter(range) => {
                    let s = Self::decode_be_u64(&range.start)?;
                    min_start = min_start.min(s.saturating_add(1));
                    max_end = total_count;
                }
                QueryItem::RangeAfterTo(range) => {
                    let s = Self::decode_be_u64(&range.start)?;
                    let e = Self::decode_be_u64(&range.end)?;
                    min_start = min_start.min(s.saturating_add(1));
                    max_end = max_end.max(e);
                }
                QueryItem::RangeAfterToInclusive(range) => {
                    let s = Self::decode_be_u64(range.start())?;
                    let e = Self::decode_be_u64(range.end())?;
                    min_start = min_start.min(s.saturating_add(1));
                    max_end = max_end.max(e.saturating_add(1));
                }
                QueryItem::AggregateCountOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountOnRange is only supported on provable count trees, \
                         not on BulkAppendTree",
                    ));
                }
                QueryItem::AggregateSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateSumOnRange is only supported on provable sum trees, \
                         not on BulkAppendTree",
                    ));
                }
                QueryItem::AggregateCountAndSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountAndSumOnRange is only supported on \
                         ProvableCountProvableSumTree, not on BulkAppendTree",
                    ));
                }
            }
        }

        // Clamp to total_count
        max_end = max_end.min(total_count);
        Ok((min_start, max_end))
    }

    /// Decode a big-endian u64 from key bytes.
    fn decode_be_u64(key: &[u8]) -> Result<u64, Error> {
        if key.len() != 8 {
            return Err(Error::InvalidInput(
                "position key must be exactly 8 bytes (BE u64)",
            ));
        }
        let arr: [u8; 8] = key
            .try_into()
            .map_err(|_| Error::InvalidInput("invalid u64 key bytes"))?;
        Ok(u64::from_be_bytes(arr))
    }

    /// Decode a big-endian u16 from key bytes.
    fn decode_be_u16(key: &[u8]) -> Result<u16, Error> {
        if key.len() != 2 {
            return Err(Error::InvalidInput(
                "position key must be exactly 2 bytes (BE u16)",
            ));
        }
        let arr: [u8; 2] = key
            .try_into()
            .map_err(|_| Error::InvalidInput("invalid u16 key bytes"))?;
        Ok(u16::from_be_bytes(arr))
    }
}

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;

    use crate::{Error, GroveDb};

    /// Helper: encode a u16 as big-endian bytes.
    fn be_u16(v: u16) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    /// Helper: encode a u64 as big-endian bytes.
    fn be_u64(v: u64) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    // -----------------------------------------------------------------------
    // query_items_to_positions (u16, dense tree)
    // -----------------------------------------------------------------------

    #[test]
    fn range_after_u16_max_returns_empty() {
        // RangeAfter(u16::MAX) should produce empty: no index after 65535
        let items = vec![QueryItem::RangeAfter(be_u16(u16::MAX)..)];
        let result = GroveDb::query_items_to_positions(&items, 100).unwrap();
        assert!(result.is_empty(), "expected empty, got {:?}", result);
    }

    #[test]
    fn range_after_to_u16_max_returns_empty() {
        // RangeAfterTo(u16::MAX..100): saturated start >= end, so empty
        let items = vec![QueryItem::RangeAfterTo(be_u16(u16::MAX)..be_u16(100))];
        let result = GroveDb::query_items_to_positions(&items, 200).unwrap();
        assert!(result.is_empty(), "expected empty, got {:?}", result);
    }

    #[test]
    fn range_after_to_inclusive_u16_max_returns_empty() {
        // RangeAfterToInclusive(u16::MAX..=u16::MAX) with count=100:
        // saturated start (u16::MAX) > end.min(99), so empty
        let items = vec![QueryItem::RangeAfterToInclusive(
            be_u16(u16::MAX)..=be_u16(u16::MAX),
        )];
        let result = GroveDb::query_items_to_positions(&items, 100).unwrap();
        assert!(result.is_empty(), "expected empty, got {:?}", result);
    }

    #[test]
    fn range_after_normal_u16_works() {
        // RangeAfter(5) with count=10 should yield [6, 7, 8, 9]
        let items = vec![QueryItem::RangeAfter(be_u16(5)..)];
        let result = GroveDb::query_items_to_positions(&items, 10).unwrap();
        assert_eq!(result, vec![6, 7, 8, 9]);
    }

    // -----------------------------------------------------------------------
    // query_items_to_leaf_indices (u64, MMR)
    // -----------------------------------------------------------------------

    #[test]
    fn range_after_u64_max_returns_empty() {
        // RangeAfter(u64::MAX) should produce empty indices
        let items = vec![QueryItem::RangeAfter(be_u64(u64::MAX)..)];
        // mmr_size=7 -> leaf_count=4
        let result = GroveDb::query_items_to_leaf_indices(&items, 7).unwrap();
        assert!(result.is_empty(), "expected empty, got {:?}", result);
    }

    #[test]
    fn range_after_to_u64_max_returns_empty() {
        let items = vec![QueryItem::RangeAfterTo(be_u64(u64::MAX)..be_u64(100))];
        let result = GroveDb::query_items_to_leaf_indices(&items, 7).unwrap();
        assert!(result.is_empty(), "expected empty, got {:?}", result);
    }

    #[test]
    fn range_after_to_inclusive_u64_max_returns_empty() {
        let items = vec![QueryItem::RangeAfterToInclusive(
            be_u64(u64::MAX)..=be_u64(u64::MAX),
        )];
        // leaf_count=4, max_idx=3; saturated u64::MAX..=3 is empty
        let result = GroveDb::query_items_to_leaf_indices(&items, 7).unwrap();
        assert!(result.is_empty(), "expected empty, got {:?}", result);
    }

    // -----------------------------------------------------------------------
    // query_items_to_range (u64, BulkAppendTree)
    // -----------------------------------------------------------------------

    #[test]
    fn range_after_u64_max_range_no_overflow() {
        let items = vec![QueryItem::RangeAfter(be_u64(u64::MAX)..)];
        let (start, end) = GroveDb::query_items_to_range(&items, 100).unwrap();
        assert!(
            start >= end,
            "expected empty range, got ({}, {})",
            start,
            end
        );
    }

    #[test]
    fn key_u64_max_range_no_overflow() {
        let items = vec![QueryItem::Key(be_u64(u64::MAX))];
        let (start, end) = GroveDb::query_items_to_range(&items, 100).unwrap();
        assert!(
            start >= end,
            "expected empty range, got ({}, {})",
            start,
            end
        );
    }

    #[test]
    fn range_inclusive_u64_max_end_no_overflow() {
        let items = vec![QueryItem::RangeInclusive(be_u64(0)..=be_u64(u64::MAX))];
        let (start, end) = GroveDb::query_items_to_range(&items, 100).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 100); // clamped to total_count
    }

    #[test]
    fn range_to_inclusive_u64_max_no_overflow() {
        let items = vec![QueryItem::RangeToInclusive(..=be_u64(u64::MAX))];
        let (start, end) = GroveDb::query_items_to_range(&items, 50).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 50); // clamped
    }

    #[test]
    fn range_after_to_inclusive_u64_max_no_overflow() {
        let items = vec![QueryItem::RangeAfterToInclusive(
            be_u64(u64::MAX)..=be_u64(u64::MAX),
        )];
        let (start, end) = GroveDb::query_items_to_range(&items, 100).unwrap();
        assert!(
            start >= end,
            "expected empty range, got ({}, {})",
            start,
            end
        );
    }

    // -----------------------------------------------------------------------
    // AggregateCountOnRange rejection on non-provable-count tree types.
    //
    // `AggregateCountOnRange` is only meaningful against `ProvableCountTree`
    // and `ProvableCountSumTree` (their nodes commit a count via
    // `node_hash_with_count`). Dense, MMR, and BulkAppendTree have no such
    // commitment, so the index-resolution helpers must reject the variant
    // outright rather than silently fall through.
    // -----------------------------------------------------------------------

    #[test]
    fn dense_tree_rejects_aggregate_count_on_range() {
        let inner = QueryItem::RangeInclusive(be_u16(0)..=be_u16(5));
        let items = vec![QueryItem::AggregateCountOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_positions(&items, 100)
            .expect_err("dense tree must reject AggregateCountOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("dense fixed-size") || msg.contains("provable count"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn mmr_tree_rejects_aggregate_count_on_range() {
        let inner = QueryItem::RangeInclusive(be_u64(0)..=be_u64(5));
        let items = vec![QueryItem::AggregateCountOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_leaf_indices(&items, 7)
            .expect_err("MMR must reject AggregateCountOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("MMR") || msg.contains("provable count"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn bulk_append_tree_rejects_aggregate_count_on_range() {
        let inner = QueryItem::RangeInclusive(be_u64(0)..=be_u64(5));
        let items = vec![QueryItem::AggregateCountOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_range(&items, 100)
            .expect_err("BulkAppendTree must reject AggregateCountOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("BulkAppendTree") || msg.contains("provable count"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // AggregateSumOnRange rejection on non-provable-sum tree types.
    //
    // Same rationale as the count side: `AggregateSumOnRange` is only valid
    // against `ProvableSumTree` (binds sum into the node hash via
    // `node_hash_with_sum`). Dense / MMR / BulkAppend trees must reject.
    // -----------------------------------------------------------------------

    #[test]
    fn dense_tree_rejects_aggregate_sum_on_range() {
        let inner = QueryItem::RangeInclusive(be_u16(0)..=be_u16(5));
        let items = vec![QueryItem::AggregateSumOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_positions(&items, 100)
            .expect_err("dense tree must reject AggregateSumOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("dense fixed-size") || msg.contains("provable sum"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn mmr_tree_rejects_aggregate_sum_on_range() {
        let inner = QueryItem::RangeInclusive(be_u64(0)..=be_u64(5));
        let items = vec![QueryItem::AggregateSumOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_leaf_indices(&items, 7)
            .expect_err("MMR must reject AggregateSumOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("MMR") || msg.contains("provable sum"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn bulk_append_tree_rejects_aggregate_sum_on_range() {
        let inner = QueryItem::RangeInclusive(be_u64(0)..=be_u64(5));
        let items = vec![QueryItem::AggregateSumOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_range(&items, 100)
            .expect_err("BulkAppendTree must reject AggregateSumOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("BulkAppendTree") || msg.contains("provable sum"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // AggregateCountAndSumOnRange rejection on non-PCPS tree types.
    //
    // `AggregateCountAndSumOnRange` is only meaningful against
    // `ProvableCountProvableSumTree` — its nodes commit BOTH a count AND
    // a sum via `node_hash_with_count_and_sum`. Dense fixed-size merkle
    // trees, MMR trees, and BulkAppendTree have no such dual-axis
    // commitment, so the index-resolution helpers must reject the
    // variant outright rather than silently fall through.
    //
    // Mirrors `dense_tree_rejects_aggregate_count_on_range` /
    // `dense_tree_rejects_aggregate_sum_on_range` (and the MMR /
    // BulkAppendTree siblings). Each test pins exactly one of the three
    // helper functions' `AggregateCountAndSumOnRange` arms.
    // -----------------------------------------------------------------------

    #[test]
    fn dense_tree_rejects_aggregate_count_and_sum_on_range() {
        // Pins the `QueryItem::AggregateCountAndSumOnRange(_)` arm in
        // `query_items_to_positions` (the dense fixed-size merkle tree
        // index resolver). Same rationale as the ACOR / ASOR siblings:
        // dense trees have no per-node aggregate commitment, so the
        // combined-aggregate variant must be rejected up front.
        let inner = QueryItem::RangeInclusive(be_u16(0)..=be_u16(5));
        let items = vec![QueryItem::AggregateCountAndSumOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_positions(&items, 100)
            .expect_err("dense tree must reject AggregateCountAndSumOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("dense fixed-size") || msg.contains("ProvableCountProvableSumTree"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn mmr_tree_rejects_aggregate_count_and_sum_on_range() {
        // Pins the `QueryItem::AggregateCountAndSumOnRange(_)` arm in
        // `query_items_to_leaf_indices` (the MMR tree leaf-index
        // resolver). MMR leaves carry only an opaque hash; there is no
        // per-leaf count or sum bound in the tree shape, so any
        // aggregate variant must be rejected at index resolution time.
        let inner = QueryItem::RangeInclusive(be_u64(0)..=be_u64(5));
        let items = vec![QueryItem::AggregateCountAndSumOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_leaf_indices(&items, 7)
            .expect_err("MMR must reject AggregateCountAndSumOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("MMR") || msg.contains("ProvableCountProvableSumTree"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn bulk_append_tree_rejects_aggregate_count_and_sum_on_range() {
        // Pins the `QueryItem::AggregateCountAndSumOnRange(_)` arm in
        // `query_items_to_range` (the BulkAppendTree position-range
        // resolver). BulkAppendTree elements are append-only
        // positional items with no per-position aggregate commitment;
        // the combined-aggregate variant has no meaningful semantics
        // against them.
        let inner = QueryItem::RangeInclusive(be_u64(0)..=be_u64(5));
        let items = vec![QueryItem::AggregateCountAndSumOnRange(Box::new(inner))];
        let err = GroveDb::query_items_to_range(&items, 100)
            .expect_err("BulkAppendTree must reject AggregateCountAndSumOnRange");
        match err {
            Error::InvalidInput(msg) => assert!(
                msg.contains("BulkAppendTree") || msg.contains("ProvableCountProvableSumTree"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }
}
