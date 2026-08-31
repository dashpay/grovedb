//! `Element::path_query_push` — versioned implementations.
//!
//! `path_query_push` is the per-result callback of the trusted (non-proof)
//! query walk: when an outer query item resolves to a tree with a subquery,
//! it recurses via `Element::get_path_query` and settles the outer query's
//! remaining `limit`/`offset` from the inner result. How an *empty* inner
//! result is charged against the outer limit changes which elements a
//! `PathQuery` returns, so it is version-gated:
//!
//! * **[v0]** — legacy accounting, frozen for `GROVE_V1`..`GROVE_V3` (all
//!   live in production). An empty inner result always consumes one limit
//!   slot when `decrease_limit_on_range_with_no_sub_elements` is set — even
//!   when the emptiness was caused by `offset` skipping rows that did match
//!   (issue #690), so e.g. `limit=2, offset=1` can return a single element.
//! * **[v1]** — `GROVE_V4`+. Charges an empty inner result only when
//!   nothing was skipped (`skipped == 0`, issue #690 — offset-consumed
//!   subqueries are pagination, not absence), serves per-instance
//!   limits (`Query::limit`) and reconciles subquery descents by total
//!   consumed budget (rows plus empty-subtree charges) instead of
//!   returned rows, aligning the read path's global-limit accounting
//!   with the prover's shared-counter accounting.
//!
//!   (An intermediate carrying only the #690 guard was once gated here
//!   for `GROVE_V4`; since no grove version ever shipped it, it was
//!   folded into this v1 rather than kept as a dead dispatch arm.)
//!
//! Proof generation rejects non-zero offsets and never calls
//! `path_query_push`, so this gate has no proof surface.
//!
//! The dispatcher lives in the `ElementQueryExtensions::path_query_push`
//! trait implementation in [`super::query`], which matches on
//! `grove_version.grovedb_versions.element.path_query_push`.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

pub(crate) use v0::path_query_push_v0;
pub(crate) use v1::path_query_push_v1;
