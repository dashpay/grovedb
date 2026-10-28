//! Grove protocol version 5: branch-local committed-count-unit pagination.
//!
//! Snapshot of the method versions changed from V4:
//! 1. `path_query_methods.per_instance_query_offsets: 1` serves `Query::offset`
//!    as a committed-count-unit skip within one provable count-bearing host.
//! 2. `path_query_methods.merge: 2` composes local pages with exclusive exact-key
//!    branches while preserving their per-instance pagination.
//! 3. `element.path_query_push: 2` serves local count pages and includes requested
//!    parent-tree metadata in trusted raw reads.

use super::{
    grovedb_versions::{
        GroveDBElementMethodVersions, GroveDBPathQueryMethodVersions, GroveDBVersions,
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
        path_query_methods: GroveDBPathQueryMethodVersions {
            merge: 2, // changed: preserve branch-local pagination during composition
            per_instance_query_offsets: 1, // changed: serve local committed-count-unit skips
            ..GROVE_V4.grovedb_versions.path_query_methods
        },
        ..GROVE_V4.grovedb_versions
    },
    ..GROVE_V4
};
