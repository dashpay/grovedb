//! Grove protocol version 5.
//!
//! Introduced identical to [`GROVE_V4`](super::v4::GROVE_V4) apart from its
//! `protocol_version`, and no longer is. Currently flipped:
//!
//! - `element.path_query_push = 2` enables V5 trusted parent-tree echoes.
//! - `path_query_methods.merge = 2` enables exclusive branch-local pagination merge rules.
//! - `path_query_methods.per_instance_query_offsets = 1` enables local offsets on supported provable count trees.
//!
//!   Offset-bearing queries remain rejected by earlier Grove versions.
//!
//! - `operations.private_document_store.get_range: 1` and
//!   `operations.private_document_store.range_proof: 1`: a store's entries
//!   can be read by position range, and proved. A V1 proof descends into a
//!   store as into its bulk append tree, and the verifier binds the bulk root
//!   through the store's committed config. V4 refuses both, as 6.1.1 did:
//!   the range read fails closed, and a V1 proof neither carries nor accepts
//!   a lower layer under a store. Gated because it adds an acceptance rule
//!   to the live V1 envelope.
//!
//! - `operations.average_case.average_case_merk_insert_element: 1` and
//!   `operations.worst_case.worst_case_merk_insert_element: 1`: the insert
//!   estimators price creating a `PrivateDocumentStore` as the apply does,
//!   adding the existence check a write-once store makes and the two hashes
//!   deriving its empty root, and in the worst case pricing the store as the
//!   layered tree the apply writes rather than as an item. The average-case
//!   estimator also sizes a tree wrapped in `NonCounted`, `NotSummed` or
//!   `NotCountedOrSummed` with the byte of its wrapper, which the apply
//!   charges; the worst-case one always has. V4 keeps the 6.1.1 estimates,
//!   which price a store's creation below its apply and a wrapped tree's
//!   insert 1 byte below its apply. Gated because the estimate is the
//!   admission bound.
//!
//! - `operations.proof.count_offset_reference_empty_tree_rows: 1`: the
//!   global count-offset verifier exempts rows resolved through a reference
//!   from its empty-tree value-hash check, which a legacy reference to an
//!   empty tree can never pass; Merk already binds the target bytes. V1..V4
//!   refuse such a page, as 6.1.1 did. Gated because it turns a rejected
//!   proof into an accepted one.
//!
//! - `operations.proof.instance_cap_layer_truncation: 1`: a V1 proof layer
//!   that has subquery branches under a per-instance cap (`Query::limit`)
//!   stops after the key whose rows spent the instance budget, so the proof
//!   is proportional to the page. The verifier reads such a layer as a gap-free
//!   prefix of its query and accepts an early stop only once it has itself
//!   seen the instance budget (or the global one) spent by the rows below the
//!   keys the layer reveals. V4, the first version to serve per-instance
//!   caps, proves the layer under the global budget only, as 6.1.1 did: with
//!   no global limit it carries every key of the range, and its verifier
//!   refuses a truncated one (issue #962). V1..V3 refuse a per-instance cap
//!   before any proof code runs. The rows a query returns are the same in V4
//!   and V5. Gated because it
//!   changes proof bytes and turns a rejected proof into an accepted one.
//! - `operations.proof.unreported_empty_tree_row_charge: 1`: a V1 verifier
//!   that leaves empty trees out of its results (the default of
//!   `verify_query` and `verify_subset_query`) charges an empty plain `Tree`
//!   that is itself a result row against the global limit and the
//!   per-instance cap, as the prover and the trusted read do, and stops where
//!   the budget is spent. V1..V4 bind the row and charge nothing, as 6.1.1
//!   did, so the verifier's budget runs ahead of the prover's: it refuses the
//!   honest proof of a page whose budget such a row spent ("missing lower
//!   layer for non-empty tree"), or reports an item past the page. Gated
//!   because it turns a rejected proof into an accepted one and changes the
//!   rows returned for a proof accepted before. Proof bytes are the same.
//!
//! - `operations.proof.subquery_descent_on_key: 1`: the V1 prover and
//!   verifier treat a key as descended into only when the query has
//!   something below it, as the trusted read does. V1..V4 descend at every
//!   key a conditional branch matches, as 6.1.1 did, even a branch with
//!   neither a subquery nor a subquery path
//!   (`add_conditional_subquery(item, None, None)`, which exempts keys from
//!   the default subquery). There an empty tree is proved as an empty child:
//!   never reported, and charged against the global limit only, so a page
//!   under a per-instance cap runs past the trusted read's; and a populated
//!   tree cannot be proved at all. From V5 the tree at such a key is a result
//!   row. Gated because it changes the proof emitted for such a query, turns
//!   a refused proof request into a served one, and changes the rows a
//!   verifier returns.
//!
//! - `apply_batch.keyless_append_repeats: 1`: the batch consistency check
//!   counts keyed operations only for repeats, so a batch may carry the same
//!   keyless append (`CommitmentTreeInsert`, `MmrTreeAppend`, `BulkAppend`,
//!   `DenseTreeInsert`, `PrivateDocumentStoreInsert`) more than once and
//!   stores what the same appends applied one by one store. V4 refuses such a
//!   batch, as 6.1.1 did. Gated because it flips an accepted/rejected outcome
//!   (issue #1031).
//!
//! - `apply_batch.same_key_tree_type_conflict: 1`: a batch applied with its
//!   consistency check disabled is refused when it inserts trees of
//!   different tree types at one key. V4 applies it, as 6.1.1 did, with the
//!   child Merk keeping the first insert's type whatever the last insert
//!   asked for. Gated because it flips an accepted/rejected outcome.
//! - `apply_batch.indexed_mirror_entry_read: 1`: the
//!   indexed-tree mirror reads an entry of the primary by walking its
//!   tree from the root, before and after the level's apply. The walk
//!   follows the tree as it stands, so a removed key is absent, in a
//!   partial batch's later segment as well. On V4 the read can return the
//!   entry just removed (the delete is still pending in the storage batch),
//!   and the entry's row then stays in every secondary. Gated because it
//!   changes a committed secondary root and tracked costs.
//!
//! - `apply_batch.tree_insert_above_emptied_level: 1`: a batch that inserts
//!   a sum or count tree and leaves the level below it empty (it only deletes
//!   there, say a key that is not stored) stores the tree the caller sent,
//!   with a zero aggregate, exactly as the same insert without the deletes
//!   does. V1..V4 store a plain `Tree` there, as 6.1.1 did, so sums and
//!   counts written under it later never reach its parent, and refuse the
//!   batch when the tree is wrapped in `NotSummed` or `NotCountedOrSummed`.
//!   Gated because it changes the stored element, so the root hash and the
//!   cost, and flips that refusal. A conditional insert (`InsertIfNotExists`)
//!   keeps the V4 outcome.
//!
//! - `merk_versions.tree.just_in_time_value_update: 2`: Merk accepts that
//!   answer and records the removal plus the insertion. Versions 0 and 1
//!   refuse it, so grove v1..v4 cannot account an update as a replacement at
//!   any layer. Gated because the accounting and the stored flags are
//!   consensus-relevant once a caller uses it.
//!
//! - `element.delete: 1`, `element.delete_with_sectioned_removal_bytes: 1`
//!   and `element.delete_into_batch_operations: 1`: deleting an element of a
//!   `NormalTree` without declaring it a tree refunds what the stored
//!   element was charged, as it does in every other tree type. V1..V4 refund
//!   its raw value bytes there: a tree removed by a batch `Delete` gives
//!   back more than it was charged (it never paid for its value hash), and a
//!   sum item gives back less (it was charged a fixed size). Items,
//!   references and declared tree deletes are refunded the same in every
//!   version. Gated because it changes a tracked cost.
//!
//! - `operations.insert.provable_count_provable_big_sum_tree_creation: 1`:
//!   an `Element::ProvableCountProvableBigSumTree` can be created, by a
//!   direct or a batch insert. The tree commits its element count and a
//!   128-bit sum into every node hash: the wide counterpart of
//!   `ProvableCountProvableSumTree`, for totals that can pass the `i64`
//!   range. V1..V4 refuse the creation, as 6.1.1 does not know the element.
//!   Gated because it flips an accepted/rejected outcome.
//!
//! - `operations.insert.big_sum_tree_twins_creation: 1`: an
//!   `Element::CountBigSumTree` (a count and a 128-bit sum kept in the
//!   element) and an `Element::ProvableBigSumTree` (a 128-bit sum in every
//!   node hash) can be created, by a direct or a batch insert. V1..V4 refuse
//!   the creation, as 6.1.1 does not know the elements. Gated because it
//!   flips an accepted/rejected outcome.
//!
//! It exists so that changes made after GroveDB 6.1.1 have somewhere to
//! land. GROVE_V4 as 6.1.1 shipped it is what Dash Platform 5.0 (protocol
//! version 14) runs, so a change that alters an accepted/rejected outcome, a
//! committed root hash, a tracked cost or a cost estimate cannot be made to
//! V4: nodes carrying it would diverge from nodes that do not. Such a change
//! bumps the relevant slot here and branches on it, leaving V1..V4
//! untouched.
//!
//! When adding a gate, list it here and say what changes in the slot
//! comment, the way v4.rs does. `GroveVersion::latest()` returns the last
//! entry of `GROVE_VERSIONS`, so it resolves to this version: anything
//! defaulting to "latest" (tests, benchmarks, tools) exercises every gate
//! listed here, and a test about V4 behaviour names `GROVE_V4`.

use super::{
    grovedb_versions::{
        GroveDBApplyBatchVersions, GroveDBElementMethodVersions,
        GroveDBOperationsAverageCaseVersions, GroveDBOperationsInsertVersions,
        GroveDBOperationsPrivateDocumentStoreVersions, GroveDBOperationsProofVersions,
        GroveDBOperationsVersions, GroveDBOperationsWorstCaseVersions,
        GroveDBPathQueryMethodVersions, GroveDBVersions,
    },
    merk_versions::{MerkTreeVersions, MerkVersions},
    v4::GROVE_V4,
    GroveVersion,
};

/// Local offsets compose count-bearing pages with ordinary exact-key branches.
/// All unrelated method versions retain their V4 behavior.
pub const GROVE_V5: GroveVersion = GroveVersion {
    protocol_version: 5,
    grovedb_versions: GroveDBVersions {
        apply_batch: GroveDBApplyBatchVersions {
            keyless_append_repeats: 1, // changed: a batch may repeat a keyless append
            same_key_tree_type_conflict: 1, // changed: two tree types at one key are refused
            tree_insert_above_emptied_level: 1, // changed: the inserted tree keeps its type
            indexed_mirror_entry_read: 1, // changed: entries are read by walking the tree
            ..GROVE_V4.grovedb_versions.apply_batch
        },
        element: GroveDBElementMethodVersions {
            // changed: a delete in a `NormalTree` refunds from the stored element
            delete: 1,
            delete_with_sectioned_removal_bytes: 1,
            delete_into_batch_operations: 1,
            path_query_push: 2, // changed: local count pages and trusted parent echo
            ..GROVE_V4.grovedb_versions.element
        },
        operations: GroveDBOperationsVersions {
            average_case: GroveDBOperationsAverageCaseVersions {
                // changed: prices a store's creation and a tree's wrapper byte
                average_case_merk_insert_element: 1,
                ..GROVE_V4.grovedb_versions.operations.average_case
            },
            worst_case: GroveDBOperationsWorstCaseVersions {
                worst_case_merk_insert_element: 1, // changed: prices a store's creation
                ..GROVE_V4.grovedb_versions.operations.worst_case
            },
            insert: GroveDBOperationsInsertVersions {
                // changed: the wide count and sum tree can be created
                provable_count_provable_big_sum_tree_creation: 1,
                // changed: so can the other trees with a 128-bit sum
                big_sum_tree_twins_creation: 1,
                ..GROVE_V4.grovedb_versions.operations.insert
            },
            private_document_store: GroveDBOperationsPrivateDocumentStoreVersions {
                get_range: 1,   // changed: a store's position-range read
                range_proof: 1, // changed: proving a store's entries
                ..GROVE_V4.grovedb_versions.operations.private_document_store
            },
            proof: GroveDBOperationsProofVersions {
                count_offset_reference_empty_tree_rows: 1, // changed: exempt resolved references
                // changed: a capped layer with subqueries stops where its budget is spent
                instance_cap_layer_truncation: 1,
                unreported_empty_tree_row_charge: 1, // changed: the verifier charges the row
                subquery_descent_on_key: 1, // changed: a branch without a subquery is a row
                ..GROVE_V4.grovedb_versions.operations.proof
            },
            ..GROVE_V4.grovedb_versions.operations
        },
        path_query_methods: GroveDBPathQueryMethodVersions {
            merge: 2, // changed: preserve branch-local pagination during composition
            per_instance_query_offsets: 1, // changed: serve local committed-count-unit skips
            ..GROVE_V4.grovedb_versions.path_query_methods
        },
        ..GROVE_V4.grovedb_versions
    },
    merk_versions: MerkVersions {
        tree: MerkTreeVersions {
            just_in_time_value_update: 2, // changed: an update may be accounted as a replacement
            ..GROVE_V4.merk_versions.tree
        },
        ..GROVE_V4.merk_versions
    },
    ..GROVE_V4
};
