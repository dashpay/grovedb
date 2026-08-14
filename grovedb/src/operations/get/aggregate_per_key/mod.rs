//! Trusted per-key aggregate reads — one entry point per aggregate axis.
//!
//! A **carrier** aggregate query is an outer fan-out: the top-level query
//! items are `Key`/`Range*` and the `default_subquery_branch.subquery`
//! resolves (after walking the optional `subquery_path`) to a leaf
//! aggregate. The read returns one entry per matched outer key. A
//! **leaf** query owns the aggregate item directly and collapses to a
//! single entry — see [`carrier`] for why that entry carries an empty
//! stand-in key.
//!
//! One module per axis, mirroring how the proof side splits
//! `operations::proof::aggregate_{count,sum,count_and_sum}`:
//!
//! - [`count`] — `query_aggregate_count_per_key` → `(key, u64)`
//! - [`sum`] — `query_aggregate_sum_per_key` → `(key, i64)`
//! - [`count_and_sum`] — `query_aggregate_count_and_sum_per_key` →
//!   `(key, u64, i64)`
//!
//! Every part of the carrier walk is aggregate-agnostic — only the
//! merk-level primitive that terminates each per-key descent differs by
//! axis — so all three entry points delegate to the single driver in
//! [`carrier`] and supply that primitive as an argument. Each entry point
//! module therefore holds only its own version gate, shape validation,
//! leaf-shape delegation, and axis-specific error text.
//!
//! Results from this module are **not** independently verifiable. The
//! verifiable counterparts are
//! `verify_aggregate_{count,sum,count_and_sum}_query_per_key` on the
//! proof side; the two surfaces return the same shapes on purpose, so a
//! caller can swap one for the other and compare element-for-element.

mod carrier;
mod count;
mod count_and_sum;
mod sum;
