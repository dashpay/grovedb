//! Grove protocol version 5: branch-local committed-count-unit pagination.
//!
//! Snapshot of the method versions changed from V4:
//! 1. `path_query_methods.per_instance_query_offsets: 1` serves `Query::offset`
//!    as a committed-count-unit skip within one provable count-bearing host.
//! 2. `path_query_methods.merge: 2` composes local pages with exclusive exact-key
//!    branches while preserving their per-instance pagination.
//! 3. `element.path_query_push: 2` serves local count pages and includes requested
//! Grove protocol version 5: branch-local committed-count-unit pagination.
//!
//! Changed capability slots from GROVE_V4:
//! - `element.path_query_push = 2` enables V5 trusted parent-tree echoes.
//! - `path_query_methods.merge = 2` enables exclusive branch-local pagination merge rules.
//! - `path_query_methods.per_instance_query_offsets = 1` enables local offsets on supported provable count trees.
//! Offset-bearing queries remain rejected by earlier Grove versions.

        },
        ..GROVE_V4.grovedb_versions
    },
    ..GROVE_V4
};
