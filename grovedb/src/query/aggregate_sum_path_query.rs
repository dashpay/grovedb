use crate::operations::proof::util::hex_to_ascii;
use crate::Error;
use bincode::{Decode, Encode};
use grovedb_merk::proofs::aggregate_sum_query::AggregateSumQuery;
use grovedb_merk::proofs::query::QueryItem;
use grovedb_version::check_grovedb_v0;
use grovedb_version::version::GroveVersion;
use std::fmt;

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
        Self {
            path,
            aggregate_sum_query,
        }
    }

    /// New path query with a single key
    pub fn new_single_key(path: Vec<Vec<u8>>, key: Vec<u8>, sum_limit: u64) -> Self {
        Self {
            path,
            aggregate_sum_query: AggregateSumQuery::new_single_key(key, sum_limit),
        }
    }

    /// New path query with a single query item
    pub fn new_single_query_item(
        path: Vec<Vec<u8>>,
        query_item: QueryItem,
        sum_limit: u64,
        limit_of_items_to_check: Option<u16>,
    ) -> Self {
        Self {
            path,
            aggregate_sum_query: AggregateSumQuery::new_single_query_item(
                query_item,
                sum_limit,
                limit_of_items_to_check,
            ),
        }
    }

    /// Combines multiple aggregate sum path queries into one equivalent aggregate sum path query.
    /// All path queries must share the same path.
    pub fn merge(
        mut path_queries: Vec<&AggregateSumPathQuery>,
        grove_version: &GroveVersion,
    ) -> Result<Self, Error> {
        check_grovedb_v0!(
            "merge",
            grove_version
                .grovedb_versions
                .aggregate_sum_path_query_methods
                .merge
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
            return Err(Error::InvalidInput(
                "all path queries must have the same path",
            ));
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

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::aggregate_sum_query::AggregateSumQuery;
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_version::version::GroveVersion;

    use super::*;

    #[test]
    fn display_includes_path_and_query() {
        let q = AggregateSumPathQuery::new(
            vec![b"root".to_vec(), b"leaf".to_vec()],
            AggregateSumQuery::new(42, None),
        );
        let s = format!("{}", q);
        assert!(s.contains("AggregateSumPathQuery"));
        assert!(s.contains("root"));
        assert!(s.contains("leaf"));
        assert!(s.contains("42"));
    }

    #[test]
    fn new_single_key_constructor() {
        let q = AggregateSumPathQuery::new_single_key(vec![b"p".to_vec()], b"mykey".to_vec(), 100);
        assert_eq!(q.path, vec![b"p".to_vec()]);
        assert_eq!(q.aggregate_sum_query.sum_limit, 100);
        assert_eq!(
            q.aggregate_sum_query.items,
            vec![QueryItem::Key(b"mykey".to_vec())]
        );
    }

    #[test]
    fn new_single_query_item_constructor() {
        let q = AggregateSumPathQuery::new_single_query_item(
            vec![b"p".to_vec()],
            QueryItem::RangeFull(..),
            50,
            Some(10),
        );
        assert_eq!(q.aggregate_sum_query.sum_limit, 50);
        assert_eq!(q.aggregate_sum_query.limit_of_items_to_check, Some(10));
    }

    #[test]
    fn merge_empty_returns_error() {
        let grove_version = GroveVersion::latest();
        let err = AggregateSumPathQuery::merge(vec![], grove_version).unwrap_err();
        assert!(
            format!("{}", err).contains("at least 1"),
            "expected 'at least 1' error, got: {}",
            err
        );
    }

    #[test]
    fn merge_single_returns_clone() {
        let grove_version = GroveVersion::latest();
        let q =
            AggregateSumPathQuery::new(vec![b"path".to_vec()], AggregateSumQuery::new(42, Some(5)));
        let merged = AggregateSumPathQuery::merge(vec![&q], grove_version).unwrap();
        assert_eq!(merged, q);
    }

    #[test]
    fn merge_mismatched_paths_returns_error() {
        let grove_version = GroveVersion::latest();
        let q1 =
            AggregateSumPathQuery::new(vec![b"path_a".to_vec()], AggregateSumQuery::new(10, None));
        let q2 =
            AggregateSumPathQuery::new(vec![b"path_b".to_vec()], AggregateSumQuery::new(10, None));
        let err = AggregateSumPathQuery::merge(vec![&q1, &q2], grove_version).unwrap_err();
        assert!(
            format!("{}", err).contains("same path"),
            "expected 'same path' error, got: {}",
            err
        );
    }

    #[test]
    fn merge_two_queries_sums_limits() {
        let grove_version = GroveVersion::latest();
        let q1 = AggregateSumPathQuery::new(
            vec![b"p".to_vec()],
            AggregateSumQuery::new_with_keys(vec![vec![1]], 10, Some(2)),
        );
        let q2 = AggregateSumPathQuery::new(
            vec![b"p".to_vec()],
            AggregateSumQuery::new_with_keys(vec![vec![2]], 20, Some(3)),
        );
        let merged = AggregateSumPathQuery::merge(vec![&q1, &q2], grove_version).unwrap();
        assert_eq!(merged.path, vec![b"p".to_vec()]);
        assert_eq!(merged.aggregate_sum_query.sum_limit, 30);
        assert_eq!(merged.aggregate_sum_query.limit_of_items_to_check, Some(5));
        assert_eq!(merged.aggregate_sum_query.items.len(), 2);
    }
}
