//! Queries

use std::{borrow::Cow, cmp::Ordering, fmt};

#[cfg(any(feature = "full", feature = "verify"))]
use grovedb_merk::proofs::query::query_item::QueryItem;
use grovedb_merk::proofs::query::SubqueryBranch;
#[cfg(any(feature = "full", feature = "verify"))]
use grovedb_merk::proofs::Query;
use indexmap::IndexMap;

use crate::operations::proof::util::hex_to_ascii;
#[cfg(any(feature = "full", feature = "verify"))]
use crate::query_result_type::PathKey;
#[cfg(any(feature = "full", feature = "verify"))]
use crate::Error;

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, Clone)]
/// Path query
///
/// Represents a path to a specific GroveDB tree and a corresponding query to
/// apply to the given tree.
pub struct PathQuery {
    /// Path
    // TODO: Make generic over path type
    pub path: Vec<Vec<u8>>,
    /// Query
    pub query: SizedQuery,
}

/// Do we go from left to right
pub type LeftToRight = bool;

/// Do we have subqueries
pub type HasSubqueries = bool;

#[cfg(any(feature = "full", feature = "verify"))]
impl fmt::Display for PathQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PathQuery {{ path: [")?;
        for (i, path_element) in self.path.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", hex_to_ascii(path_element))?;
        }
        write!(f, "], query: {} }}", self.query)
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, Clone)]
/// Holds a query to apply to a tree and an optional limit/offset value.
/// Limit and offset values affect the size of the result set.
pub struct SizedQuery {
    /// Query
    pub query: Query,
    /// Limit
    pub limit: Option<u16>,
    /// Offset
    pub offset: Option<u16>,
}

#[cfg(any(feature = "full", feature = "verify"))]
impl fmt::Display for SizedQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SizedQuery {{ query: {}", self.query)?;
        if let Some(limit) = self.limit {
            write!(f, ", limit: {}", limit)?;
        }
        if let Some(offset) = self.offset {
            write!(f, ", offset: {}", offset)?;
        }
        write!(f, " }}")
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl SizedQuery {
    /// New sized query
    pub const fn new(query: Query, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            query,
            limit,
            offset,
        }
    }

    /// New sized query with one key
    pub fn new_single_key(key: Vec<u8>) -> Self {
        Self {
            query: Query::new_single_key(key),
            limit: None,
            offset: None,
        }
    }

    /// New sized query with one key
    pub fn new_single_query_item(query_item: QueryItem) -> Self {
        Self {
            query: Query::new_single_query_item(query_item),
            limit: None,
            offset: None,
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl PathQuery {
    /// New path query
    pub const fn new(path: Vec<Vec<u8>>, query: SizedQuery) -> Self {
        Self { path, query }
    }

    /// New path query with a single key
    pub fn new_single_key(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        Self {
            path,
            query: SizedQuery::new_single_key(key),
        }
    }

    /// New path query with a single query item
    pub fn new_single_query_item(path: Vec<Vec<u8>>, query_item: QueryItem) -> Self {
        Self {
            path,
            query: SizedQuery::new_single_query_item(query_item),
        }
    }

    /// New unsized path query
    pub const fn new_unsized(path: Vec<Vec<u8>>, query: Query) -> Self {
        let query = SizedQuery::new(query, None, None);
        Self { path, query }
    }

    /// Gets the path of all terminal keys
    pub fn terminal_keys(&self, max_results: usize) -> Result<Vec<PathKey>, Error> {
        let mut result: Vec<(Vec<Vec<u8>>, Vec<u8>)> = vec![];
        self.query
            .query
            .terminal_keys(self.path.clone(), max_results, &mut result)
            .map_err(Error::MerkError)?;
        Ok(result)
    }

    /// Combines multiple path queries into one equivalent path query
    pub fn merge(mut path_queries: Vec<&PathQuery>) -> Result<Self, Error> {
        if path_queries.is_empty() {
            return Err(Error::InvalidInput(
                "merge function requires at least 1 path query",
            ));
        }
        if path_queries.len() == 1 {
            return Ok(path_queries.remove(0).clone());
        }

        let (common_path, next_index) = PathQuery::get_common_path(&path_queries);

        let mut queries_for_common_path_this_level: Vec<Query> = vec![];

        let mut queries_for_common_path_sub_level: Vec<SubqueryBranch> = vec![];

        // convert all the paths after the common path to queries
        path_queries.into_iter().try_for_each(|path_query| {
            if path_query.query.offset.is_some() {
                return Err(Error::NotSupported(
                    "can not merge pathqueries with offsets".to_string(),
                ));
            }
            if path_query.query.limit.is_some() {
                return Err(Error::NotSupported(
                    "can not merge pathqueries with limits, consider setting the limit after the \
                     merge"
                        .to_string(),
                ));
            }
            path_query
                .to_subquery_branch_with_offset_start_index(next_index)
                .map(|unsized_path_query| {
                    if unsized_path_query.subquery_path.is_none() {
                        queries_for_common_path_this_level
                            .push(*unsized_path_query.subquery.unwrap());
                    } else {
                        queries_for_common_path_sub_level.push(unsized_path_query);
                    }
                })
        })?;

        let mut merged_query = Query::merge_multiple(queries_for_common_path_this_level);
        // add conditional subqueries
        for sub_path_query in queries_for_common_path_sub_level {
            let SubqueryBranch {
                subquery_path,
                subquery,
            } = sub_path_query;
            let mut subquery_path =
                subquery_path.ok_or(Error::CorruptedCodeExecution("subquery path must exist"))?;
            let key = subquery_path.remove(0); // must exist
            merged_query.insert_item(QueryItem::Key(key.clone()));
            let rest_of_path = if subquery_path.is_empty() {
                None
            } else {
                Some(subquery_path)
            };
            let subquery_branch = SubqueryBranch {
                subquery_path: rest_of_path,
                subquery,
            };
            merged_query.merge_conditional_boxed_subquery(QueryItem::Key(key), subquery_branch);
        }

        Ok(PathQuery::new_unsized(common_path, merged_query))
    }

    /// Given a set of path queries, this returns an array of path keys that are
    /// common across all the path queries.
    /// Also returns the point at which they stopped being equal.
    fn get_common_path(path_queries: &[&PathQuery]) -> (Vec<Vec<u8>>, usize) {
        let min_path_length = path_queries
            .iter()
            .map(|path_query| path_query.path.len())
            .min()
            .expect("expect path_queries length to be 2 or more");

        let mut common_path = vec![];
        let mut level = 0;

        while level < min_path_length {
            let keys_at_level = path_queries
                .iter()
                .map(|path_query| &path_query.path[level])
                .collect::<Vec<_>>();
            let first_key = keys_at_level[0];

            let keys_are_uniform = keys_at_level.iter().all(|&curr_key| curr_key == first_key);

            if keys_are_uniform {
                common_path.push(first_key.to_vec());
                level += 1;
            } else {
                break;
            }
        }
        (common_path, level)
    }

    /// Given a path and a starting point, a query that is equivalent to the
    /// path is generated example: [a, b, c] =>
    ///     query a
    ///         cond a
    ///             query b
    ///                 cond b
    ///                    query c
    fn to_subquery_branch_with_offset_start_index(
        &self,
        start_index: usize,
    ) -> Result<SubqueryBranch, Error> {
        let path = &self.path;

        match path.len().cmp(&start_index) {
            Ordering::Equal => Ok(SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(self.query.query.clone())),
            }),
            Ordering::Less => Err(Error::CorruptedCodeExecution(
                "invalid start index for path query merge",
            )),
            _ => {
                let (_, remainder) = path.split_at(start_index);

                Ok(SubqueryBranch {
                    subquery_path: Some(remainder.to_vec()),
                    subquery: Some(Box::new(self.query.query.clone())),
                })
            }
        }
    }

    pub fn query_items_at_path<'a>(&'a self, path: &[&[u8]]) -> Option<InternalCowItemsQuery> {
        fn recursive_query_items<'b>(
            query: &'b Query,
            path: &[&[u8]],
        ) -> Option<InternalCowItemsQuery<'b>> {
            if path.is_empty() {
                return Some(InternalCowItemsQuery::from_query(query));
            }

            let key = path[0];
            let path_after_top_removed = &path[1..];

            if let Some(conditional_branches) = &query.conditional_subquery_branches {
                for (query_item, subquery_branch) in conditional_branches {
                    if query_item.contains(key) {
                        if let Some(subquery_path) = &subquery_branch.subquery_path {
                            if path_after_top_removed.len() <= subquery_path.len() {
                                if path_after_top_removed
                                    .iter()
                                    .zip(subquery_path)
                                    .all(|(a, b)| *a == b.as_slice())
                                {
                                    return if path_after_top_removed.len() == subquery_path.len() {
                                        if let Some(subquery) = &subquery_branch.subquery {
                                            Some(InternalCowItemsQuery::from_query(subquery))
                                        } else {
                                            None
                                        }
                                    } else {
                                        Some(InternalCowItemsQuery::from_items_when_in_path(
                                            Cow::Owned(vec![QueryItem::Key(
                                                subquery_path[path_after_top_removed.len()].clone(),
                                            )]),
                                        ))
                                    };
                                }
                            }
                        }

                        return if let Some(subquery) = &subquery_branch.subquery {
                            recursive_query_items(subquery, &path[1..])
                        } else {
                            Some(InternalCowItemsQuery::from_items_when_in_path(Cow::Owned(
                                vec![QueryItem::Key(key.to_vec())],
                            )))
                        };
                    }
                }
            }

            if let Some(subquery_path) = &query.default_subquery_branch.subquery_path {
                if path_after_top_removed.len() <= subquery_path.len() {
                    if path_after_top_removed
                        .iter()
                        .zip(subquery_path)
                        .all(|(a, b)| *a == b.as_slice())
                    {
                        return if path_after_top_removed.len() == subquery_path.len() {
                            if let Some(subquery) = &query.default_subquery_branch.subquery {
                                Some(InternalCowItemsQuery::from_query(subquery))
                            } else {
                                None
                            }
                        } else {
                            Some(InternalCowItemsQuery::from_items_when_in_path(Cow::Owned(
                                vec![QueryItem::Key(
                                    subquery_path[path_after_top_removed.len()].clone(),
                                )],
                            )))
                        };
                    }
                } else if path_after_top_removed
                    .iter()
                    .take(subquery_path.len())
                    .zip(subquery_path)
                    .all(|(a, b)| *a == b.as_slice())
                {
                    if let Some(subquery) = &query.default_subquery_branch.subquery {
                        return recursive_query_items(subquery, &path[subquery_path.len()..]);
                    }
                }
            } else if let Some(subquery) = &query.default_subquery_branch.subquery {
                return recursive_query_items(subquery, path_after_top_removed);
            }

            None
        }

        let self_path_len = self.path.len();
        let given_path_len = path.len();

        match given_path_len.cmp(&self_path_len) {
            Ordering::Less => {
                if path.iter().zip(&self.path).all(|(a, b)| *a == b.as_slice()) {
                    Some(InternalCowItemsQuery::from_items_when_in_path(Cow::Owned(
                        vec![QueryItem::Key(self.path[given_path_len].clone())],
                    )))
                } else {
                    None
                }
            }
            Ordering::Equal => {
                if path.iter().zip(&self.path).all(|(a, b)| *a == b.as_slice()) {
                    Some(InternalCowItemsQuery::from_path_query(self))
                } else {
                    None
                }
            }
            Ordering::Greater => {
                if !self.path.iter().zip(path).all(|(a, b)| a.as_slice() == *b) {
                    return None;
                }
                recursive_query_items(&self.query.query, &path[self_path_len..])
            }
        }
    }
}

/// This represents a query where the items might be borrowed, it is used to get
/// subquery information
#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct InternalCowItemsQuery<'a> {
    /// Items
    pub items: Cow<'a, Vec<QueryItem>>,
    /// Default subquery branch
    pub default_subquery_branch: Cow<'a, SubqueryBranch>,
    /// Conditional subquery branches
    pub conditional_subquery_branches: Option<Cow<'a, IndexMap<QueryItem, SubqueryBranch>>>,
    /// Left to right?
    pub left_to_right: bool,
    /// In the path of the path_query, or in a subquery path
    pub in_path: bool,
}

impl<'a> InternalCowItemsQuery<'a> {
    /// Checks to see if we have a subquery on a specific key
    pub fn has_subquery_on_key(&self, key: &[u8]) -> bool {
        if self.in_path
            || self.default_subquery_branch.subquery.is_some()
            || self.default_subquery_branch.subquery_path.is_some()
        {
            return true;
        }
        if let Some(conditional_subquery_branches) = self.conditional_subquery_branches.as_ref() {
            for query_item in conditional_subquery_branches.keys() {
                if query_item.contains(key) {
                    return true;
                }
            }
        }
        return false;
    }

    pub fn from_items_when_in_path(items: Cow<Vec<QueryItem>>) -> InternalCowItemsQuery {
        InternalCowItemsQuery {
            items,
            default_subquery_branch: Default::default(),
            conditional_subquery_branches: None,
            left_to_right: true,
            in_path: true,
        }
    }

    pub fn from_path_query(path_query: &PathQuery) -> InternalCowItemsQuery {
        Self::from_query(&path_query.query.query)
    }

    pub fn from_query(query: &Query) -> InternalCowItemsQuery {
        InternalCowItemsQuery {
            items: Cow::Borrowed(&query.items),
            default_subquery_branch: Cow::Borrowed(&query.default_subquery_branch),
            conditional_subquery_branches: query
                .conditional_subquery_branches
                .as_ref()
                .map(|conditional_subquery_branches| Cow::Borrowed(conditional_subquery_branches)),
            left_to_right: query.left_to_right,
            in_path: false,
        }
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use std::ops::RangeFull;

    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};

    use crate::{
        query_result_type::QueryResultType,
        tests::{common::compare_result_tuples, make_deep_tree, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    #[test]
    fn test_same_path_different_query_merge() {
        let temp_db = make_deep_tree();

        // starting with no subquery, just a single path and a key query
        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove_query(&path_query_one, None).unwrap().unwrap();
        let (_, result_set_one) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db.prove_query(&path_query_two, None).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two])
            .expect("should merge path queries");

        let proof = temp_db
            .prove_query(&merged_path_query, None)
            .unwrap()
            .unwrap();
        let (_, result_set_tree) = GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set_tree.len(), 2);
    }

    #[test]
    fn test_different_same_length_path_with_different_query_merge() {
        // Tests for
        // [a, c, Q]
        // [a, m, Q]
        let temp_db = make_deep_tree();

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove_query(&path_query_one, None).unwrap().unwrap();
        let (_, result_set_one) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key4".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], query_two);

        let proof = temp_db.prove_query(&path_query_two, None).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two])
            .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![TEST_LEAF.to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);

        let proof = temp_db
            .prove_query(&merged_path_query, None)
            .unwrap()
            .unwrap();
        let (_, result_set_merged) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query)
                .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 2);

        let keys = [b"key1".to_vec(), b"key4".to_vec()];
        let values = [b"value1".to_vec(), b"value4".to_vec()];
        let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        compare_result_tuples(result_set_merged, expected_result_set);

        // longer length path queries
        let mut query_one = Query::new();
        query_one.insert_all();
        let path_query_one = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_2".to_vec(),
            ],
            query_one.clone(),
        );

        let proof = temp_db.prove_query(&path_query_one, None).unwrap().unwrap();
        let (_, result_set_one) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set_one.len(), 3);

        let mut query_two = Query::new();
        query_two.insert_all();

        let path_query_two = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_4".to_vec(),
            ],
            query_two.clone(),
        );

        let proof = temp_db.prove_query(&path_query_two, None).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let mut query_three = Query::new();
        query_three.insert_range_after(b"key7".to_vec()..);

        let path_query_three = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_3".to_vec(),
            ],
            query_three.clone(),
        );

        let proof = temp_db
            .prove_query(&path_query_three, None)
            .unwrap()
            .unwrap();
        let (_, result_set_two) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_three)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        #[rustfmt::skip]
        mod explanation {

    // Tree Structure
    //                                   root
    //              /                      |                       \ (not representing Merk)
    // -----------------------------------------------------------------------------------------
    //         test_leaf            another_test_leaf                deep_leaf
    //       /           \             /         \              /                 \
    // -----------------------------------------------------------------------------------------
    //   innertree     innertree4  innertree2  innertree3  deep_node_1          deep_node_2
    //       |             |           |           |      /          \         /         \
    // -----------------------------------------------------------------------------------------
    //      k2,v2        k4,v4       k3,v3      k4,v4   deeper_1   deeper_2  deeper_3   deeper_4
    //     /     \         |                           |            |         |          |
    //  k1,v1    k3,v3   k5,v5                        /            /          |          |
    // -----------------------------------------------------------------------------------------
    //                                            k2,v2         k5,v5        k8,v8     k10,v10
    //                                           /     \        /    \       /    \       \
    //                                       k1,v1    k3,v3  k4,v4   k6,v6 k7,v7  k9,v9  k11,v11
    //                                                            ↑ (all 3)   ↑     (all 2) ↑
    //                                                      path_query_one    ↑   path_query_two
    //                                                                 path_query_three (2)
    //                                                                   (after 7, so {8,9})

        }

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two, &path_query_three])
                .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![b"deep_leaf".to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);
        let conditional_subquery_branches = merged_path_query
            .query
            .query
            .conditional_subquery_branches
            .clone()
            .expect("expected to have conditional subquery branches");
        assert_eq!(conditional_subquery_branches.len(), 2);
        let (deep_node_1_query_item, deep_node_1_subquery_branch) =
            conditional_subquery_branches.first().unwrap();
        let (deep_node_2_query_item, deep_node_2_subquery_branch) =
            conditional_subquery_branches.last().unwrap();
        assert_eq!(
            deep_node_1_query_item,
            &QueryItem::Key(b"deep_node_1".to_vec())
        );
        assert_eq!(
            deep_node_2_query_item,
            &QueryItem::Key(b"deep_node_2".to_vec())
        );

        assert_eq!(
            deep_node_1_subquery_branch
                .subquery_path
                .as_ref()
                .expect("expected a subquery_path for deep_node_1"),
            &vec![b"deeper_2".to_vec()]
        );
        assert_eq!(
            *deep_node_1_subquery_branch
                .subquery
                .as_ref()
                .expect("expected a subquery for deep_node_1"),
            Box::new(query_one)
        );

        assert!(
            deep_node_2_subquery_branch.subquery_path.is_none(),
            "there should be no subquery path here"
        );
        let deep_node_2_subquery = deep_node_2_subquery_branch
            .subquery
            .as_ref()
            .expect("expected a subquery for deep_node_2")
            .as_ref();

        assert_eq!(deep_node_2_subquery.items.len(), 2);

        let deep_node_2_conditional_subquery_branches = deep_node_2_subquery
            .conditional_subquery_branches
            .as_ref()
            .expect("expected to have conditional subquery branches");
        assert_eq!(deep_node_2_conditional_subquery_branches.len(), 2);

        // deeper 4 was query 2
        let (deeper_4_query_item, deeper_4_subquery_branch) =
            deep_node_2_conditional_subquery_branches.first().unwrap();
        let (deeper_3_query_item, deeper_3_subquery_branch) =
            deep_node_2_conditional_subquery_branches.last().unwrap();

        assert_eq!(deeper_3_query_item, &QueryItem::Key(b"deeper_3".to_vec()));
        assert_eq!(deeper_4_query_item, &QueryItem::Key(b"deeper_4".to_vec()));

        assert!(
            deeper_3_subquery_branch.subquery_path.is_none(),
            "there should be no subquery path here"
        );
        assert_eq!(
            *deeper_3_subquery_branch
                .subquery
                .as_ref()
                .expect("expected a subquery for deeper_3"),
            Box::new(query_three)
        );

        assert!(
            deeper_4_subquery_branch.subquery_path.is_none(),
            "there should be no subquery path here"
        );
        assert_eq!(
            *deeper_4_subquery_branch
                .subquery
                .as_ref()
                .expect("expected a subquery for deeper_4"),
            Box::new(query_two)
        );

        let (result_set_merged, _) = temp_db
            .query_raw(
                &merged_path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
            )
            .value
            .expect("expected to get results");
        assert_eq!(result_set_merged.len(), 7);

        let proof = temp_db
            .prove_query(&merged_path_query, None)
            .unwrap()
            .unwrap();
        let (_, proved_result_set_merged) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query)
                .expect("should execute proof");
        assert_eq!(proved_result_set_merged.len(), 7);

        let keys = [
            b"key4".to_vec(),
            b"key5".to_vec(),
            b"key6".to_vec(),
            b"key8".to_vec(),
            b"key9".to_vec(),
            b"key10".to_vec(),
            b"key11".to_vec(),
        ];
        let values = [
            b"value4".to_vec(),
            b"value5".to_vec(),
            b"value6".to_vec(),
            b"value8".to_vec(),
            b"value9".to_vec(),
            b"value10".to_vec(),
            b"value11".to_vec(),
        ];
        let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        compare_result_tuples(proved_result_set_merged, expected_result_set);
    }

    #[test]
    fn test_different_length_paths_merge() {
        let temp_db = make_deep_tree();

        let mut query_one = Query::new();
        query_one.insert_all();

        let mut subq = Query::new();
        subq.insert_all();
        query_one.set_subquery(subq);

        let path_query_one = PathQuery::new_unsized(
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()],
            query_one,
        );

        let proof = temp_db.prove_query(&path_query_one, None).unwrap().unwrap();
        let (_, result_set_one) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set_one.len(), 6);

        let mut query_two = Query::new();
        query_two.insert_all();

        let path_query_two = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_4".to_vec(),
            ],
            query_two,
        );

        let proof = temp_db.prove_query(&path_query_two, None).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two])
            .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![b"deep_leaf".to_vec()]);

        let proof = temp_db
            .prove_query(&merged_path_query, None)
            .unwrap()
            .unwrap();
        let (_, result_set_merged) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query)
                .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 8);

        let keys = [
            b"key1".to_vec(),
            b"key2".to_vec(),
            b"key3".to_vec(),
            b"key4".to_vec(),
            b"key5".to_vec(),
            b"key6".to_vec(),
            b"key10".to_vec(),
            b"key11".to_vec(),
        ];
        let values = [
            b"value1".to_vec(),
            b"value2".to_vec(),
            b"value3".to_vec(),
            b"value4".to_vec(),
            b"value5".to_vec(),
            b"value6".to_vec(),
            b"value10".to_vec(),
            b"value11".to_vec(),
        ];
        let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        compare_result_tuples(result_set_merged, expected_result_set);
    }

    #[test]
    fn test_same_path_and_different_path_query_merge() {
        let temp_db = make_deep_tree();

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove_query(&path_query_one, None).unwrap().unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db.prove_query(&path_query_two, None).unwrap().unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let mut query_three = Query::new();
        query_three.insert_all();
        let path_query_three = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
            query_three,
        );

        let proof = temp_db
            .prove_query(&path_query_three, None)
            .unwrap()
            .unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_three)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 2);

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two, &path_query_three])
                .expect("should merge three queries");

        let proof = temp_db
            .prove_query(&merged_path_query, None)
            .unwrap()
            .unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 4);
    }

    #[test]
    fn test_equal_path_merge() {
        // [a, b, Q]
        // [a, b, Q2]
        // We should be able to merge this if Q and Q2 have no subqueries.

        let temp_db = make_deep_tree();

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove_query(&path_query_one, None).unwrap().unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db.prove_query(&path_query_two, None).unwrap().unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two])
            .expect("should merge three queries");

        let proof = temp_db
            .prove_query(&merged_path_query, None)
            .unwrap()
            .unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 2);

        // [a, b, Q]
        // [a, b, c, Q2] (rolled up to) [a, b, Q3] where Q3 combines [c, Q2]
        // this should fail as [a, b] is a subpath of [a, b, c]
        let mut query_one = Query::new();
        query_one.insert_all();
        let path_query_one = PathQuery::new_unsized(
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()],
            query_one,
        );

        let proof = temp_db.prove_query(&path_query_one, None).unwrap().unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 2);

        let mut query_one = Query::new();
        query_one.insert_key(b"deeper_1".to_vec());

        let mut subq = Query::new();
        subq.insert_all();
        query_one.set_subquery(subq.clone());

        let path_query_two = PathQuery::new_unsized(
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()],
            query_one,
        );

        let proof = temp_db.prove_query(&path_query_two, None).unwrap().unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 3);

        #[rustfmt::skip]
        mod explanation {

    // Tree Structure
    //                                   root
    //              /                      |                       \ (not representing Merk)
    // -----------------------------------------------------------------------------------------
    //         test_leaf            another_test_leaf                deep_leaf
    //       /           \             /         \              /                 \
    // -----------------------------------------------------------------------------------------
    //   innertree     innertree4  innertree2  innertree3  deep_node_1          deep_node_2
    //       |             |           |           |      /          \         /         \
    // -----------------------------------------------------------------------------------------
    //      k2,v2        k4,v4       k3,v3      k4,v4   deeper_1   deeper_2  deeper_3   deeper_4
    //     /     \         |                           |   ↑  (2)  ↑ |         |          |
    //  k1,v1    k3,v3   k5,v5                        /path_query_1 /          |          |
    // -----------------------------------------------------------------------------------------
    //                                            k2,v2         k5,v5        k8,v8     k10,v10
    //                                           /     \        /    \       /    \       \
    //                                       k1,v1    k3,v3  k4,v4   k6,v6 k7,v7  k9,v9  k11,v11
    //                                            ↑ (3)
    //                                       path_query_2



        }

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two])
            .expect("expected to be able to merge path_query");

        // we expect the common path to be the path of both before merge
        assert_eq!(
            merged_path_query.path,
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()]
        );

        // we expect all items (a range full)
        assert_eq!(merged_path_query.query.query.items.len(), 1);
        assert!(merged_path_query
            .query
            .query
            .items
            .iter()
            .all(|a| a == &QueryItem::RangeFull(RangeFull)));

        // we expect a conditional subquery on deeper 1 for all elements
        let conditional_subquery_branches = merged_path_query
            .query
            .query
            .conditional_subquery_branches
            .as_ref()
            .expect("expected conditional subquery branches");

        assert_eq!(conditional_subquery_branches.len(), 1);
        let (conditional_query_item, conditional_subquery_branch) =
            conditional_subquery_branches.first().unwrap();
        assert_eq!(
            conditional_query_item,
            &QueryItem::Key(b"deeper_1".to_vec())
        );

        assert_eq!(conditional_subquery_branch.subquery, Some(Box::new(subq)));

        assert_eq!(conditional_subquery_branch.subquery_path, None);

        let (result_set_merged, _) = temp_db
            .query_raw(
                &merged_path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
            )
            .value
            .expect("expected to get results");
        assert_eq!(result_set_merged.len(), 4);

        let proof = temp_db
            .prove_query(&merged_path_query, None)
            .unwrap()
            .unwrap();
        let (_, result_set) = GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set.len(), 4);
    }
}
