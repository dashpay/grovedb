//! Whether a query goes below a key: **v0** (released behaviour,
//! `GROVE_V1`..`GROVE_V4`).
//!
//! The layer's own view decides: a default subquery, any conditional branch
//! that matches the key, or the next component of a path being followed.
//! A matching conditional branch counts even when it holds neither a subquery
//! nor a subquery path, which the trusted read treats as "return this
//! element". With this reading:
//!
//! * an empty tree at such a key is proved and verified as an empty child. It
//!   is never reported, and it is charged against the global limit only, so
//!   under a per-instance cap the verified page runs one row past the page
//!   the trusted read returns;
//! * a populated tree at such a key cannot be proved: the prover descends
//!   into it and finds no query there.
//!
//! This is a known gap, kept because `GROVE_V4` is live: closing it changes
//! the proofs a prover emits and the rows a verifier returns. [`super::v1`]
//! closes it from `GROVE_V5`.

use crate::{query::SinglePathSubquery, GroveDb};

impl GroveDb {
    /// `query_descends_on_key` v0. See the module documentation.
    pub(super) fn query_descends_on_key_v0(layer_query: &SinglePathSubquery, key: &[u8]) -> bool {
        layer_query.has_subquery_or_matching_in_path_on_key(key)
    }
}
