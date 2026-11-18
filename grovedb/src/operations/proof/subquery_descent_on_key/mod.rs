//! Whether a query goes below a key of the layer being proved or verified:
//! versioned dispatch.
//!
//! The V1 prover and verifier walk a layer row by row and decide, for every
//! tree they meet, between two readings: the query descends into the tree (a
//! lower layer is proved, or an empty tree is an empty child), or the tree is
//! itself a result row. Both sides must take the same reading, and it has to
//! be the trusted read's, or the proved page is not the page the read
//! returns. The reading is gated on `proof.subquery_descent_on_key`:
//!
//! * **[v0]**: released behaviour, `GROVE_V1`..`GROVE_V4`. A key descends
//!   whenever the layer's query has a default subquery or any conditional
//!   branch matches the key, whatever that branch holds. A branch with
//!   neither a subquery nor a subquery path
//!   (`add_conditional_subquery(item, None, None)`, which exempts keys from
//!   the default subquery) is read as a descent, while the trusted read
//!   returns the element at such a key as a row.
//! * **[v1]**: `GROVE_V5`+. A key descends only when the query has something
//!   below it, as `PathQuery::query_items_at_path` resolves it, which is how
//!   the trusted read picks the branch.
//!
//! Gated because it changes the proof the prover emits, flips
//! accepted/rejected outcomes, and changes the rows a verifier returns.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{query::SinglePathSubquery, Error, GroveDb, PathQuery};

impl GroveDb {
    /// Whether `path_query` goes below `key` in the layer at `path`, whose
    /// own query is `layer_query`, per the grove version.
    ///
    /// The prover and the verifier both classify a tree through this
    /// function, so they cannot disagree about which keys carry a lower
    /// layer.
    pub(crate) fn query_descends_on_key(
        path_query: &PathQuery,
        layer_query: &SinglePathSubquery,
        path: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<bool, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .subquery_descent_on_key
        {
            0 => Ok(Self::query_descends_on_key_v0(layer_query, key)),
            1 => Self::query_descends_on_key_v1(path_query, layer_query, path, key, grove_version),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "query_descends_on_key".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }
}
