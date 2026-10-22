//! Grove protocol version 5: branch-local committed-count-unit pagination.

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
            path_query_push: 2,
            ..GROVE_V4.grovedb_versions.element
        },
        path_query_methods: GroveDBPathQueryMethodVersions {
            merge: 2,
            per_instance_query_offsets: 1,
            ..GROVE_V4.grovedb_versions.path_query_methods
        },
        ..GROVE_V4.grovedb_versions
    },
    ..GROVE_V4
};
