use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct GroveDBVersions {
    pub apply_batch: GroveDBApplyBatchVersions,
    pub element: GroveDBElementMethodVersions,
    pub operations: GroveDBOperationsVersions,
    pub aggregate_sum_path_query_methods: GroveDBAggregateSumPathQueryMethodVersions,
    pub path_query_methods: GroveDBPathQueryMethodVersions,
    pub replication: GroveDBReplicationVersions,
    pub query_limits: GroveDBQueryLimits,
}

#[derive(Clone, Debug)]
pub struct GroveDBQueryLimits {
    pub max_aggregate_sum_query_elements_scanned: u16,
}

impl Default for GroveDBQueryLimits {
    fn default() -> Self {
        Self {
            max_aggregate_sum_query_elements_scanned: 1024,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBAggregateSumPathQueryMethodVersions {
    pub merge: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBPathQueryMethodVersions {
    pub terminal_keys: FeatureVersion,
    pub merge: FeatureVersion,
    pub query_items_at_path: FeatureVersion,
    pub should_add_parent_tree_at_path: FeatureVersion,
    /// Whether `PathQuery` read modes (axis-ordered and sum-budget
    /// reads, `Query::read_mode`) are served.
    ///
    /// - `0` (GROVE_V1..=V3): any `PathQuery` carrying a read mode is
    ///   rejected with `NotSupported` at every read / prove / verify
    ///   entry point. The vocabulary itself still encodes and decodes —
    ///   the gate is about *serving*, so a v4-built query constructed
    ///   ahead of activation fails closed instead of being misread as
    ///   plain key selection (an axis read has empty items; running it
    ///   as key selection would return an empty result masquerading as
    ///   real absence, and a proof would attest to the wrong read).
    /// - `1` (GROVE_V4+): `run_path_query` (and, as they land, the
    ///   unified prove/verify dispatch) serve read-mode queries.
    ///
    /// Prover and verifier read the same slot, so there is no version
    /// at which the two sides can disagree about whether a read-mode
    /// shape exists.
    pub unified_read_mode: FeatureVersion,
}

/// Method versions for the standalone indexed-axis query family — the
/// per-axis reads and the echo-based proof envelopes
/// (`prove/verify_indexed_axis_*` and their per-axis wrappers). These
/// entry points predate this struct and shipped unversioned; the slots
/// exist so the first future divergence bumps a number instead of
/// forking behavior silently. The embedded (V1-envelope) axis shapes
/// are gated separately via
/// `GroveDBOperationsProofVersions::axis_descent_in_v1_envelope`.
#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsIndexedAxisVersions {
    /// The trusted per-axis reads (`indexed_{count,sum,avg}_*`).
    pub read: FeatureVersion,
    /// The standalone single-path envelope provers
    /// (`prove_indexed_axis_{top_k,top_k_paginated,query,rank_of_key,aggregate_over_value_range}`).
    pub prove_single_path: FeatureVersion,
    /// The matching standalone verifiers.
    pub verify_single_path: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBApplyBatchVersions {
    pub apply_batch_structure: FeatureVersion,
    pub apply_body: FeatureVersion,
    pub continue_partial_apply_body: FeatureVersion,
    pub apply_operations_without_batching: FeatureVersion,
    pub apply_batch: FeatureVersion,
    pub apply_partial_batch: FeatureVersion,
    pub open_batch_transactional_merk_at_path: FeatureVersion,
    pub open_batch_merk_at_path: FeatureVersion,
    pub apply_batch_with_element_flags_update: FeatureVersion,
    pub apply_partial_batch_with_element_flags_update: FeatureVersion,
    pub estimated_case_operations_for_batch: FeatureVersion,
    /// Which tree type a batch `DeleteTree` op uses to select the storage
    /// namespaces it cleans up.
    ///
    /// - `0` (V1..V3): the caller-declared `TreeType` carried by the op is
    ///   taken at face value.
    /// - `1` (V4+): the ACTUAL stored type is used instead, and a
    ///   declared/stored mismatch involving an indexed tree is rejected.
    ///   Closes an indexed type-confusion — a declared type hiding a stored
    ///   indexed primary skips the per-axis secondary sweep and leaves
    ///   authenticated stale rows — and a `CommitmentTree` case where the
    ///   declared type sends the op down the wrong emptiness path, orphaning
    ///   its non-Merk data.
    ///
    /// The stored element comes from data the apply already loads (the
    /// emptiness pre-scan's own read, or the old value the merk delete
    /// surfaces through the old-value observer), so V4 charges exactly the
    /// V1..V3 cost per op. The slot still gates the check because it flips
    /// an accepted/rejected outcome — a mismatched delete that V1..V3
    /// accept is refused on V4+ when an indexed tree is involved.
    pub delete_tree_cleanup_type_source: FeatureVersion,
    /// Whether a batch overwrite (`InsertOrReplace` / `Replace` / `Patch`,
    /// with tree-override protection off) classifies the element it
    /// displaces to detect an indexed tree being overwritten.
    ///
    /// - `0` (V1..V3): no classification. Overwrites keep their released
    ///   accepted/rejected outcomes.
    /// - `1` (V4+): the displaced element is classified — the safe subset
    ///   (empty indexed or non-indexed replacement, references included)
    ///   schedules the per-axis secondary storage for cleanup, and an
    ///   ambiguous non-empty indexed replacement is refused. Without this,
    ///   overwriting an indexed primary would orphan its secondary
    ///   namespaces at their derived prefixes.
    ///
    /// The old element bytes come from the node the merk walk fetched
    /// anyway to rewrite the key, surfaced through the old-value observer —
    /// no dedicated stored-element read, so V4 charges exactly the V1..V3
    /// cost per overwrite-capable op. Like
    /// [`Self::delete_tree_cleanup_type_source`] the slot gates behaviour,
    /// not cost: a non-empty indexed replacement that would be accepted
    /// blind on V1..V3 is refused on V4+.
    pub overwrite_indexed_cleanup_inspection: FeatureVersion,
    /// Whether keyless append-only ops (`CommitmentTreeInsert`,
    /// `MmrTreeAppend`, `BulkAppend`, `DenseTreeInsert`) reach the cost
    /// dispatch in the estimated-cost batch structure.
    ///
    /// - `0` (V1..V3): keyless ops are silently skipped when building the
    ///   batch structure, so in the estimated-cost paths the append
    ///   contributes ZERO — the under-estimate behind issue #812's
    ///   admission-control bypass. Preserved for replay: historical blocks
    ///   were admitted under this estimate and must evaluate identically.
    /// - `1` (V4+): the tree key is split off the op's path and the op is
    ///   filed under a unique synthetic key, so every append reaches the
    ///   cost arms and is charged individually.
    ///
    /// The apply path is unaffected on every version: preprocessing
    /// rewrites keyless ops into keyed ops before the batch structure is
    /// built.
    pub keyless_op_cost_dispatch: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsVersions {
    pub get: GroveDBOperationsGetVersions,
    pub insert: GroveDBOperationsInsertVersions,
    pub delete: GroveDBOperationsDeleteVersions,
    pub delete_up_tree: GroveDBOperationsDeleteUpTreeVersions,
    pub query: GroveDBOperationsQueryVersions,
    pub proof: GroveDBOperationsProofVersions,
    pub indexed_axis: GroveDBOperationsIndexedAxisVersions,
    pub average_case: GroveDBOperationsAverageCaseVersions,
    pub worst_case: GroveDBOperationsWorstCaseVersions,
    pub private_document_store: GroveDBOperationsPrivateDocumentStoreVersions,
}

/// Version slots for the PrivateDocumentStore operation family.
///
/// Unlike most families, these slots act as **capability gates**, not
/// implementation selectors: slot value `0` means the operation (and the
/// element type itself, via `element_creation`) is unavailable and fails
/// closed with a version-mismatch error; `1` means the v1 implementation is
/// active. `GROVE_V1`..`GROVE_V3` hold every slot at `0` — the
/// `PrivateDocumentStore` element (discriminant 24) cannot be created or
/// operated on under released protocol versions. `GROVE_V4` flips them to
/// `1`.
///
/// Note this does NOT gate `Element::deserialize` — the element bincode
/// codec stays protocol-independent (append-only discriminants, see the
/// doc on `GroveDBElementMethodVersions::serialize`). The gate lives at
/// the write/read operation entry points and at element insertion.
#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsPrivateDocumentStoreVersions {
    /// Creating a `PrivateDocumentStore` element (direct or batch insert of
    /// the element itself).
    pub element_creation: FeatureVersion,
    /// Appending an entry (`private_document_store_insert`, the
    /// `PrivateDocumentStoreInsert` batch op).
    pub insert: FeatureVersion,
    /// Reading an entry by position (`private_document_store_get_value`).
    pub get_value: FeatureVersion,
    /// Reading the entry count (`private_document_store_count`).
    pub count: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsGetVersions {
    pub get: FeatureVersion,
    pub get_caching_optional: FeatureVersion,
    pub follow_reference: FeatureVersion,
    pub ref_path_follow_reference: FeatureVersion,
    pub follow_reference_once: FeatureVersion,
    pub get_raw: FeatureVersion,
    pub get_raw_caching_optional: FeatureVersion,
    pub get_raw_optional: FeatureVersion,
    pub get_raw_optional_caching_optional: FeatureVersion,
    pub has_raw: FeatureVersion,
    pub check_subtree_exists_invalid_path: FeatureVersion,
    pub average_case_for_has_raw: FeatureVersion,
    pub average_case_for_has_raw_tree: FeatureVersion,
    pub average_case_for_get_raw: FeatureVersion,
    pub average_case_for_get: FeatureVersion,
    pub average_case_for_get_tree: FeatureVersion,
    pub worst_case_for_has_raw: FeatureVersion,
    pub worst_case_for_get_raw: FeatureVersion,
    pub worst_case_for_get: FeatureVersion,
    pub is_empty_tree: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsProofVersions {
    pub prove_query: FeatureVersion,
    pub prove_query_many: FeatureVersion,
    pub prove_query_non_serialized: FeatureVersion,
    pub prove_trunk_chunk: FeatureVersion,
    pub prove_trunk_chunk_non_serialized: FeatureVersion,
    pub prove_branch_chunk: FeatureVersion,
    pub prove_branch_chunk_non_serialized: FeatureVersion,
    pub prove_bulk_position_range: FeatureVersion,
    pub verify_bulk_position_range_proof: FeatureVersion,
    pub verify_query_with_options: FeatureVersion,
    pub verify_query_raw: FeatureVersion,
    pub verify_layer_proof: FeatureVersion,
    pub verify_query: FeatureVersion,
    pub verify_subset_query: FeatureVersion,
    pub verify_query_with_absence_proof: FeatureVersion,
    pub verify_subset_query_with_absence_proof: FeatureVersion,
    pub verify_query_with_chained_path_queries: FeatureVersion,
    pub verify_query_get_parent_tree_info_with_options: FeatureVersion,
    /// Whether a V1 proof binds the element bytes of a **terminally-reported
    /// non-Merk tree** — `CommitmentTree`, `MmrTree`, `BulkAppendTree`,
    /// `DenseAppendOnlyFixedSizeTree`, `PrivateDocumentStore` — to the
    /// `value_hash` its parent Merk commits to. "Terminal" means the query
    /// targets the tree element itself and the prover emits no lower layer.
    /// (`PrivateDocumentStore` cannot exist before V4, so it is always
    /// bound.)
    ///
    /// - `0` (V1..V3): the prover emits a bare `KVValueHash` node and the
    ///   verifier does not require a child hash. That node hashes only
    ///   `(key, value_hash)`, so the serialized element bytes are unbound: a
    ///   prover can serve a forged entry count (an inflated or deflated
    ///   `CommitmentTree` `total_count`, a different MMR size) alongside the
    ///   genuine `value_hash` and still reconstruct the correct root hash.
    /// - `1` (V4+): the prover emits
    ///   `KVValueHashFeatureTypeWithChildHash` carrying the tree's own state
    ///   root, and the verifier requires it, so the merk-level
    ///   `combine_hash(H(value), child_hash) == value_hash` check closes the
    ///   loop. This is exactly the composition the parent commits, since these
    ///   types are written through `insert_subtree`.
    ///
    /// Gated rather than applied unconditionally on two counts. It flips an
    /// accepted/rejected outcome — an upgraded verifier rejects proofs a
    /// released one accepts — and computing the state root costs the prover
    /// extra storage reads and hash calls on a released path. The
    /// non-Merk tree types this covers are the only elements affected;
    /// non-empty **Merk** trees have required the child hash since V3 and
    /// stay bound at every version.
    pub terminal_non_merk_tree_child_hash: FeatureVersion,
    /// Whether the V1 proof envelope carries **axis-ordered descents**
    /// into indexed trees (`ProofBytes::IndexedTreeAxisDescent`),
    /// serving `PathQuery`s whose query node holds `ReadMode::Axis`.
    ///
    /// - `0` (V1..V3): the prover refuses axis-shaped queries with
    ///   `NotSupported` and the verifier rejects any proof/query pair
    ///   involving one. These versions also reject the version-2
    ///   `Query` wire encoding outright, so the slot's `0` value is the
    ///   in-process mirror of that fail-closed decode.
    /// - `1` (V4+): the prover emits the axis-descent layer — a proof
    ///   over the queried per-axis **secondary** in place of the primary
    ///   descent — and the verifier accepts it, recomputing the
    ///   secondary-root attestation from the carried secondary proof
    ///   (never trusting 32 raw bytes) before performing the same
    ///   `combine_hash_three` parent binding as the other indexed
    ///   shapes.
    ///
    /// Gated because it adds an acceptance rule to the live V1
    /// envelope: an upgraded verifier accepts proof shapes a released
    /// one rejects. Prover and verifier read the same slot, so there is
    /// no version at which they disagree about whether the shape
    /// exists. Indexed trees themselves cannot exist in pre-V4
    /// production data, so `0` never rejects anything real.
    pub axis_descent_in_v1_envelope: FeatureVersion,
    /// Whether the V1 proof envelope carries **sum-budget windows**
    /// (`ProofBytes::SumBudgetWindow`), serving `PathQuery`s whose root
    /// query node holds `ReadMode::SumBudget` — an ordinary Merk proof
    /// over exactly the window the budget walk scanned, replayed by the
    /// verifier with the engine's own fold arithmetic.
    ///
    /// - `0` (V1..V3): the prover refuses sum-budget queries and the
    ///   verifier rejects any proof/query pair involving one.
    /// - `1` (V4+): served, with the fold replay attesting the stop
    ///   condition (budget reached / match limit / hard scan cap /
    ///   range exhausted).
    ///
    /// Gated because it adds an acceptance rule to the live V1
    /// envelope, same as `axis_descent_in_v1_envelope`.
    pub sum_budget_in_v1_envelope: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsQueryVersions {
    pub query_encoded_many: FeatureVersion,
    pub query_many_raw: FeatureVersion,
    pub get_proved_path_query: FeatureVersion,
    pub query: FeatureVersion,
    pub query_item_value: FeatureVersion,
    pub query_item_value_or_sum: FeatureVersion,
    pub query_aggregate_sums: FeatureVersion,
    pub query_aggregate_count_on_range: FeatureVersion,
    pub query_aggregate_sum_on_range: FeatureVersion,
    pub query_aggregate_count_and_sum_on_range: FeatureVersion,
    pub query_sums: FeatureVersion,
    pub query_raw: FeatureVersion,
    pub query_keys_optional: FeatureVersion,
    pub query_raw_keys_optional: FeatureVersion,
    pub follow_element: FeatureVersion,
    /// The unified read dispatch (`GroveDb::run_path_query`). This is
    /// the method's own algorithm slot; whether read-mode *shapes* are
    /// served is the separate
    /// `GroveDBPathQueryMethodVersions::unified_read_mode` gate.
    pub run_path_query: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsAverageCaseVersions {
    pub add_average_case_get_merk_at_path: FeatureVersion,
    pub average_case_merk_replace_tree: FeatureVersion,
    pub average_case_merk_insert_tree: FeatureVersion,
    pub average_case_merk_delete_tree: FeatureVersion,
    pub average_case_merk_insert_element: FeatureVersion,
    pub average_case_merk_replace_element: FeatureVersion,
    pub average_case_merk_patch_element: FeatureVersion,
    pub average_case_merk_delete_element: FeatureVersion,
    pub add_average_case_has_raw_cost: FeatureVersion,
    pub add_average_case_has_raw_tree_cost: FeatureVersion,
    pub add_average_case_get_raw_cost: FeatureVersion,
    pub add_average_case_get_raw_tree_cost: FeatureVersion,
    pub add_average_case_get_cost: FeatureVersion,
    /// Cost model for the `CommitmentTreeInsert` estimation arm.
    ///
    /// - `0` (V1..V3): the legacy average-case constants (33 Sinsemilla
    ///   hashes, 554-byte frontier, 1 blake3, frontier charged as replaced
    ///   bytes). Preserved for replay of historical admission decisions.
    /// - `1` (V4+): the depth-derived upper-bound model shared with the
    ///   worst-case arm (`commitment_tree_insert_op_cost`), covering the
    ///   full ommer cascade, dense-buffer recompute, and epoch compaction
    ///   (issue #812).
    pub average_case_commitment_tree_insert: FeatureVersion,
    /// Cost model for backward-references family ops in batch estimation.
    ///
    /// - `0` (V1..V3): the family is estimated like plain elements with no
    ///   derived fan-out, and `ReplaceBackwardReferenceFamilyMember` is
    ///   refused. Matches those versions' apply path, which rejects the
    ///   family in batches, so historical admission decisions replay
    ///   byte-identically.
    /// - `1` (V4+): family-carrying ops and (under
    ///   `BatchApplyOptions::propagate_backward_references`) deletes charge
    ///   the derived registration / propagation / cascade fan-out, bounded
    ///   by the apply path's budgets (≤32 referrers per item, ≤10-hop
    ///   chains, 1 referrer per reference), and the derived op itself gets
    ///   a real model.
    pub average_case_backward_references_fan_out: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsWorstCaseVersions {
    pub add_worst_case_get_merk_at_path: FeatureVersion,
    pub worst_case_merk_replace_tree: FeatureVersion,
    pub worst_case_merk_insert_tree: FeatureVersion,
    pub worst_case_merk_delete_tree: FeatureVersion,
    pub worst_case_merk_insert_element: FeatureVersion,
    pub worst_case_merk_replace_element: FeatureVersion,
    pub worst_case_merk_patch_element: FeatureVersion,
    pub worst_case_merk_delete_element: FeatureVersion,
    pub add_worst_case_has_raw_cost: FeatureVersion,
    pub add_worst_case_get_raw_tree_cost: FeatureVersion,
    pub add_worst_case_get_raw_cost: FeatureVersion,
    pub add_worst_case_get_cost: FeatureVersion,
    /// Cost model for the `CommitmentTreeInsert` estimation arm.
    ///
    /// - `0` (V1..V3): the legacy flat model (64 Sinsemilla hashes and a
    ///   1066-byte frontier, but only 3 seeks, 1 blake3, no dense-buffer
    ///   recompute or epoch compaction, frontier charged as replaced
    ///   bytes). Preserved for replay of historical admission decisions.
    /// - `1` (V4+): the depth-derived upper-bound model shared with the
    ///   average-case arm (`commitment_tree_insert_op_cost`), covering the
    ///   full ommer cascade, dense-buffer recompute, and epoch compaction
    ///   (issue #812).
    pub worst_case_commitment_tree_insert: FeatureVersion,
    /// Cost model for backward-references family ops in batch estimation.
    /// Same contract as
    /// `GroveDBOperationsAverageCaseVersions::average_case_backward_references_fan_out`,
    /// with the worst-case bounds charged in full.
    pub worst_case_backward_references_fan_out: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsInsertVersions {
    pub insert: FeatureVersion,
    pub insert_on_transaction: FeatureVersion,
    pub add_element_on_transaction: FeatureVersion,
    pub insert_if_not_exists: FeatureVersion,
    pub insert_if_not_exists_return_existing_element: FeatureVersion,
    pub insert_if_changed_value: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsDeleteVersions {
    pub delete: FeatureVersion,
    pub clear_subtree: FeatureVersion,
    pub delete_with_sectional_storage_function: FeatureVersion,
    pub delete_if_empty_tree: FeatureVersion,
    pub delete_if_empty_tree_with_sectional_storage_function: FeatureVersion,
    pub delete_operation_for_delete_internal: FeatureVersion,
    pub delete_internal_on_transaction: FeatureVersion,
    pub average_case_delete_operation_for_delete: FeatureVersion,
    pub worst_case_delete_operation_for_delete: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsDeleteUpTreeVersions {
    pub delete_up_tree_while_empty: FeatureVersion,
    pub delete_up_tree_while_empty_with_sectional_storage: FeatureVersion,
    pub delete_operations_for_delete_up_tree_while_empty: FeatureVersion,
    pub add_delete_operations_for_delete_up_tree_while_empty: FeatureVersion,
    pub average_case_delete_operations_for_delete_up_tree_while_empty: FeatureVersion,
    pub worst_case_delete_operations_for_delete_up_tree_while_empty: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBOperationsApplyBatchVersions {
    pub apply_batch_structure: FeatureVersion,
    pub apply_body: FeatureVersion,
    pub continue_partial_apply_body: FeatureVersion,
    pub apply_operations_without_batching: FeatureVersion,
    pub apply_batch: FeatureVersion,
    pub apply_partial_batch: FeatureVersion,
    pub open_batch_transactional_merk_at_path: FeatureVersion,
    pub open_batch_merk_at_path: FeatureVersion,
    pub apply_batch_with_element_flags_update: FeatureVersion,
    pub apply_partial_batch_with_element_flags_update: FeatureVersion,
    pub estimated_case_operations_for_batch: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBElementMethodVersions {
    pub delete: FeatureVersion,
    pub delete_with_sectioned_removal_bytes: FeatureVersion,
    pub delete_into_batch_operations: FeatureVersion,
    pub element_at_key_already_exists: FeatureVersion,
    pub get: FeatureVersion,
    pub get_optional: FeatureVersion,
    pub get_from_storage: FeatureVersion,
    pub get_optional_from_storage: FeatureVersion,
    pub get_with_absolute_refs: FeatureVersion,
    pub get_value_hash: FeatureVersion,
    pub get_with_value_hash: FeatureVersion,
    pub get_specialized_cost: FeatureVersion,
    pub value_defined_cost: FeatureVersion,
    pub value_defined_cost_for_serialized_value: FeatureVersion,
    pub specialized_costs_for_key_value: FeatureVersion,
    pub required_item_space: FeatureVersion,
    pub required_item_with_sum_item_space: FeatureVersion,
    pub required_reference_with_sum_item_space: FeatureVersion,
    pub insert: FeatureVersion,
    pub insert_into_batch_operations: FeatureVersion,
    pub insert_if_not_exists: FeatureVersion,
    pub insert_if_not_exists_into_batch_operations: FeatureVersion,
    pub insert_if_changed_value: FeatureVersion,
    pub insert_subtree_if_changed: FeatureVersion,
    pub insert_if_changed_value_into_batch_operations: FeatureVersion,
    pub insert_reference: FeatureVersion,
    pub insert_reference_into_batch_operations: FeatureVersion,
    pub insert_reference_if_changed_value: FeatureVersion,
    pub insert_subtree: FeatureVersion,
    pub insert_subtree_into_batch_operations: FeatureVersion,
    pub get_query: FeatureVersion,
    pub get_aggregate_sum_query: FeatureVersion,
    pub get_query_values: FeatureVersion,
    pub get_query_apply_function: FeatureVersion,
    pub get_path_query: FeatureVersion,
    pub get_sized_query: FeatureVersion,
    pub get_aggregate_sum_query_apply_function: FeatureVersion,
    pub path_query_push: FeatureVersion,
    pub aggregate_sum_path_query_push: FeatureVersion,
    pub query_item: FeatureVersion,
    pub basic_push: FeatureVersion,
    pub basic_aggregate_sum_push: FeatureVersion,
    // AUDIT NOTE (issue #717 — intentional, do not re-flag): the element
    // serialize/serialized_size/deserialize codec versions are `0` for EVERY
    // protocol version (v1/v2/v3), and that is correct. Element bincode
    // encoding is protocol-INDEPENDENT: each variant's discriminant is fixed by
    // declaration order and new variants are only ever *appended* (see the
    // append-only discriminant contract in `grovedb-element/src/element_type.rs`
    // and the `ProvableSumTree`/`ProvableCountProvableSumTree` notes in
    // `element/mod.rs`). A newer variant therefore serializes and deserializes
    // identically regardless of the protocol version constant, so there is no
    // "newer variant serialized under an older format" hazard. These codec
    // versions are a cost-accounting selector, NOT a wire-format gate — bumping
    // them for new variants would break the append-only compatibility contract.
    // Whether a given protocol should *construct* a newer variant at all is a
    // concern for the higher-level insert/validation logic, not for this codec,
    // which deliberately stays version-agnostic.
    pub serialize: FeatureVersion,
    pub serialized_size: FeatureVersion,
    pub deserialize: FeatureVersion,
    pub aggregate_sum_query_item: FeatureVersion,
}

#[derive(Clone, Debug, Default)]
pub struct GroveDBReplicationVersions {
    pub get_subtrees_metadata: FeatureVersion,
    pub fetch_chunk: FeatureVersion,
    pub start_snapshot_syncing: FeatureVersion,
    pub apply_chunk: FeatureVersion,
}
