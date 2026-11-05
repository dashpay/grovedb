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
//!   layered tree the apply writes rather than as an item. V4 keeps the
//!   6.1.1 estimates, which price a store's creation below its apply. Gated
//!   because the estimate is the admission bound.
//!
//! - `operations.proof.count_offset_reference_empty_tree_rows: 1`: the
//!   global count-offset verifier exempts rows resolved through a reference
//!   from its empty-tree value-hash check, which a legacy reference to an
//!   empty tree can never pass; Merk already binds the target bytes. V1..V4
//!   refuse such a page, as 6.1.1 did. Gated because it turns a rejected
//!   proof into an accepted one.
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
        GroveDBElementMethodVersions, GroveDBOperationsAverageCaseVersions,
        GroveDBOperationsPrivateDocumentStoreVersions, GroveDBOperationsProofVersions,
        GroveDBOperationsVersions, GroveDBOperationsWorstCaseVersions,
        GroveDBPathQueryMethodVersions, GroveDBVersions,
    },
    v4::GROVE_V4,
    GroveVersion,
};

/// Local offsets compose count-bearing pages with ordinary exact-key branches.
/// All unrelated method versions retain their V4 behavior.
pub const GROVE_V5: GroveVersion = GroveVersion {
    protocol_version: 5,
    grovedb_versions: GroveDBVersions {
        element: GroveDBElementMethodVersions {
            path_query_push: 2, // changed: local count pages and trusted parent echo
            ..GROVE_V4.grovedb_versions.element
        },
        operations: GroveDBOperationsVersions {
            average_case: GroveDBOperationsAverageCaseVersions {
                average_case_merk_insert_element: 1, // changed: prices a store's creation
                ..GROVE_V4.grovedb_versions.operations.average_case
            },
            worst_case: GroveDBOperationsWorstCaseVersions {
                worst_case_merk_insert_element: 1, // changed: prices a store's creation
                ..GROVE_V4.grovedb_versions.operations.worst_case
            },
            private_document_store: GroveDBOperationsPrivateDocumentStoreVersions {
                get_range: 1,   // changed: a store's position-range read
                range_proof: 1, // changed: proving a store's entries
                ..GROVE_V4.grovedb_versions.operations.private_document_store
            },
            proof: GroveDBOperationsProofVersions {
                count_offset_reference_empty_tree_rows: 1, // changed: exempt resolved references
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
    ..GROVE_V4
};
