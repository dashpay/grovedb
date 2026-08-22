//! `Query::terminal_keys` — versioned implementations.
//!
//! Terminal keys are the `(path, key)` pairs of a path query below which
//! there are no more subqueries — the keys of the terminal queries of a
//! path query. They shape the absence-proof result set assembled by proof
//! verifiers (`verify_query_with_options` with
//! `absence_proofs_for_non_existing_searched_keys`) and the result set of
//! the `query_keys_optional` family, so how they are computed is
//! version-gated.
//!
//! This crate does not depend on `grovedb-version`; the version dispatch
//! lives in grovedb's `PathQuery::terminal_keys`, which matches on
//! `grove_version.grovedb_versions.path_query_methods.terminal_keys` and
//! calls one of the implementations here:
//!
//! * **[v0]** — legacy walk, frozen for `GROVE_V1`..`GROVE_V3` (all live in
//!   production). Conditional subquery branches are expanded independently
//!   BEFORE the queried items, so selectors that were never queried still
//!   contribute terminal keys, and a `(None, None)` conditional override
//!   silently drops a queried key (issue #689). Kept bug-for-bug so
//!   verifiers on released versions reconstruct identical result sets.
//! * **[v1]** — `GROVE_V4`+. Terminal keys are computed per queried item:
//!   the first conditional branch matching the item's key wins (IndexMap
//!   insertion order), falling back to the default branch — mirroring how
//!   runtime query execution resolves subquery branches
//!   (`subquery_paths_and_value_for_sized_query`).
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use crate::Query;

impl Query {
    /// Maximum subquery nesting depth for terminal-key computation. GroveDB
    /// paths rarely exceed a handful of levels; 64 is generous and prevents
    /// stack overflow from adversarial queries. Shared by [v0] and [v1].
    pub(crate) const MAX_TERMINAL_KEYS_DEPTH: usize = 64;
}
