//! Whether a query goes below a key: **v1** (`GROVE_V5`+).
//!
//! A key descends when [`super::v0`] says so **and** the query resolves to
//! something at the path below the key (`PathQuery::query_items_at_path`,
//! which takes the first conditional branch that matches, then the default
//! branch, exactly as the trusted read does). The two differ only for a key
//! whose branch holds neither a subquery nor a subquery path: there the tree
//! is a result row. An empty one is charged as a row and reported under the
//! verifier's usual rule for empty trees; a populated one is bound through
//! its child hash like any tree reported without a subquery.
//!
//! The first condition is kept so that this version can only turn a descent
//! into a row, never the reverse, and so that the path below is resolved only
//! for keys the released reading descends into.

use grovedb_version::version::GroveVersion;

use crate::{query::SinglePathSubquery, Error, GroveDb, PathQuery};

impl GroveDb {
    /// `query_descends_on_key` v1. See the module documentation.
    pub(super) fn query_descends_on_key_v1(
        path_query: &PathQuery,
        layer_query: &SinglePathSubquery,
        path: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<bool, Error> {
        if !layer_query.has_subquery_or_matching_in_path_on_key(key) {
            return Ok(false);
        }
        let mut lower_path = Vec::with_capacity(path.len() + 1);
        lower_path.extend_from_slice(path);
        lower_path.push(key);
        Ok(path_query
            .query_items_at_path(&lower_path, grove_version)?
            .is_some())
    }
}
