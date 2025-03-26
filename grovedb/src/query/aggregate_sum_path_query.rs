use std::fmt;
use bincode::{Decode, Encode};
use grovedb_merk::proofs::aggregate_sum_query::AggregateSumQuery;
use grovedb_merk::proofs::query::QueryItem;
use grovedb_version::check_grovedb_v0;
use grovedb_version::version::GroveVersion;
use crate::operations::proof::util::hex_to_ascii;
use crate::Error;

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// Path query
///
/// Represents a path to a specific GroveDB tree and a corresponding query to
/// apply to the given tree.
pub struct AggregateSumPathQuery {
    /// Path
    pub path: Vec<Vec<u8>>,
    /// The aggregate sum query
    pub aggregate_sum_query: AggregateSumQuery,
}

impl fmt::Display for AggregateSumPathQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AggregateSumPathQuery {{ path: [")?;
        for (i, path_element) in self.path.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", hex_to_ascii(path_element))?;
        }
        write!(f, "], aggregate sum query: {} }}", self.aggregate_sum_query)
    }
}

impl AggregateSumPathQuery {
    /// New path query
    pub const fn new(path: Vec<Vec<u8>>, aggregate_sum_query: AggregateSumQuery) -> Self {
        Self { path, aggregate_sum_query }
    }

    /// New path query with a single key
    pub fn new_single_key(path: Vec<Vec<u8>>, key: Vec<u8>, sum_limit: u64) -> Self {
        Self {
            path,
            aggregate_sum_query: AggregateSumQuery::new_single_key(key, sum_limit),
        }
    }

    /// New path query with a single query item
    pub fn new_single_query_item(path: Vec<Vec<u8>>, query_item: QueryItem, sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self {
            path,
            aggregate_sum_query: AggregateSumQuery::new_single_query_item(query_item, sum_limit, limit_of_items_to_check),
        }
    }

    /// Combines multiple aggregate sum queries into one equivalent aggregate sum query
    pub fn merge(
        mut path_queries: Vec<&AggregateSumPathQuery>,
        grove_version: &GroveVersion,
    ) -> Result<Self, Error> {
        check_grovedb_v0!(
            "merge",
            grove_version.grovedb_versions.aggregate_sum_path_query_methods.merge
        );
        if path_queries.is_empty() {
            return Err(Error::InvalidInput(
                "merge function requires at least 1 path query",
            ));
        }
        if path_queries.len() == 1 {
            return Ok(path_queries.remove(0).clone());
        }

        // Use the path from the first query as the reference
        let common_path = &path_queries[0].path;

        // Verify all paths are equal
        if !path_queries.iter().all(|q| &q.path == common_path) {
            return Err(Error::InvalidInput("all path queries must have the same path"));
        }

        // Extract aggregate_sum_query values and clone them
        let aggregate_queries: Vec<AggregateSumQuery> = path_queries
            .iter()
            .map(|q| q.aggregate_sum_query.clone())
            .collect();

        // Merge all aggregate_sum_query values
        let merged_query = AggregateSumQuery::merge_multiple(aggregate_queries)?;

        // Return a new AggregateSumPathQuery with the common path and merged query
        Ok(AggregateSumPathQuery {
            path: common_path.clone(),
            aggregate_sum_query: merged_query,
        })
    }
}