//! `PathQuery::merge` — versioned implementations.
//!
//! Merging combines multiple path queries into one equivalent query
//! rooted at their common path prefix. Which inputs a merge accepts and
//! what the merged query means are consensus-relevant — merged queries
//! feed proofs, and the verifier re-runs the same merge at the same
//! grove version, so both sides must derive the identical query — which
//! is why the behavior is version-gated on
//! `path_query_methods.merge`:
//!
//! * **[v0]** — legacy behavior, frozen for `GROVE_V1`..`GROVE_V3` (all
//!   live in production). Input directions are silently dropped
//!   (sub-level inputs end up under a synthesized root whose direction
//!   is the default), and any input carrying a limit or offset is
//!   refused.
//! * **[v1]** — `GROVE_V4`+. Direction-aware: every input must agree
//!   on `left_to_right` (typed error on conflict) and the shared
//!   direction propagates to the merged root. It also merges *limited*
//!   path queries by lifting: an input's global `SizedQuery::limit`
//!   becomes its merged branch's per-instance cap (`Query::limit`) —
//!   exact, because the branch instance executes exactly once — and
//!   authored per-instance limits ride along on their branches.
//!   Budgets never blend, so limits merge only as exclusive grafts: a
//!   limited input landing at the merged root, or two limited branches
//!   colliding on a key, are refused with typed errors. Offsets are
//!   still refused.
//!
//!   (An intermediate carrying only the direction rules was once gated
//!   here for `GROVE_V4`; since no grove version ever shipped it, it
//!   was folded into v1 rather than kept as a dead dispatch arm.)
//!
//! The version-independent prelude (empty-input rejection, the
//! read-mode refusal, and the single-input shortcut) lives in
//! [`PathQuery::merge`] itself; the dispatcher below only sees inputs
//! that survived it.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_version::version::GroveVersion;

use crate::{Error, PathQuery};

/// Validates the `merge` slot without dispatching. [`PathQuery::merge`]
/// runs this before its version-independent prelude, so an unknown
/// version fails closed even for inputs the prelude would
/// short-circuit (a single input, or a read-mode refusal).
pub(crate) fn validate_version(grove_version: &GroveVersion) -> Result<(), Error> {
    match grove_version.grovedb_versions.path_query_methods.merge {
        0 | 1 => Ok(()),
        version => Err(Error::VersionError(
            grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                method: "merge".to_string(),
                known_versions: vec![0, 1],
                received: version,
            },
        )),
    }
}

/// Version dispatch for [`PathQuery::merge`] — see the module
/// documentation.
pub(crate) fn merge(
    path_queries: Vec<&PathQuery>,
    grove_version: &GroveVersion,
) -> Result<PathQuery, Error> {
    match grove_version.grovedb_versions.path_query_methods.merge {
        0 => v0::merge_v0(path_queries),
        1 => v1::merge_v1(path_queries),
        version => Err(Error::VersionError(
            grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                method: "merge".to_string(),
                known_versions: vec![0, 1],
                received: version,
            },
        )),
    }
}
