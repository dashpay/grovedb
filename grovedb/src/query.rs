use costs::{CostContext, CostsExt, OperationCost};
use merk::proofs::{query::QueryItem, Query};

use crate::Error;

#[derive(Debug, Clone)]
pub struct PathQuery {
    // TODO: Make generic over path type
    pub path: Vec<Vec<u8>>,
    pub query: SizedQuery,
}

#[derive(Debug, Clone)]
pub struct SizedQuery {
    pub query: Query,
    pub limit: Option<u16>,
    pub offset: Option<u16>,
}

impl SizedQuery {
    pub const fn new(query: Query, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            query,
            limit,
            offset,
        }
    }
}

impl PathQuery {
    pub const fn new(path: Vec<Vec<u8>>, query: SizedQuery) -> Self {
        Self { path, query }
    }

    pub const fn new_unsized(path: Vec<Vec<u8>>, query: Query) -> Self {
        let query = SizedQuery::new(query, None, None);
        Self { path, query }
    }

    /// Combines multiple path queries into one equivalent path query
    /// Restriction: all path must be unique and non subset path
    /// i.e. [a, b] + [a, b] (invalid)
    /// [a] + [a, b] (invalid [a, b] is an extension of [a])
    /// [a] + [b] (valid, unique and non subset)
    pub fn merge(path_queries: Vec<&PathQuery>) -> CostContext<Result<Self, Error>> {
        let cost = OperationCost::default();

        if path_queries.len() < 2 {
            return Err(Error::InvalidInput(
                "merge function requires at least 2 path queries",
            ))
            .wrap_with_cost(cost);
        }

        if Self::has_subpaths(&path_queries) {
            return Err(Error::InvalidInput(
                "path query path's should be non subset",
            ))
            .wrap_with_cost(cost);
        }

        let (common_path, next_index) = PathQuery::get_common_path(&path_queries);

        // convert all the paths after the common path to queries
        let queries_for_common_path: Vec<Query> = path_queries
            .iter()
            .map(|path_query| Self::convert_path_to_query(path_query, next_index))
            .collect();

        // merge the queries into one
        let mut merged_query = Query::new();
        queries_for_common_path
            .iter()
            .fold(&mut merged_query, |acc, curr| {
                acc.merge(curr);
                acc
            });

        Ok(PathQuery::new_unsized(common_path, merged_query)).wrap_with_cost(cost)
    }

    /// Checks if any path query is a subset of another by path
    /// i.e [a,b] in [a,b,c]
    /// Also checks for duplicated paths [a,x] and [a,x]
    /// returns false for any other case
    fn has_subpaths(path_queries: &[&PathQuery]) -> bool {
        // TODO: Improve this
        // Naive solution n^2 time complexity
        for i in 0..path_queries.len() {
            for j in 0..path_queries.len() {
                if i == j {
                    // don't compare the same path instance
                    continue;
                }
                let path_one = &path_queries[i].path;
                let path_two = &path_queries[j].path;

                let bigger_path;
                let smaller_path;

                if path_one.len() > path_two.len() {
                    bigger_path = path_one;
                    smaller_path = path_two;
                } else {
                    bigger_path = path_two;
                    smaller_path = path_one;
                }

                let mut is_subpath = true;

                // here we basically want to check if one vector is an extension of another
                // that means it contains all elements at the same index
                // what we have to do is check that they match at all points
                for n in 0..smaller_path.len() {
                    if bigger_path[n] != smaller_path[n] {
                        // we have divergence before exhausting the smaller path
                        // not subset
                        is_subpath = false;
                    }
                }

                if is_subpath == true {
                    return true;
                }
            }
        }

        // didn't find a subpath
        false
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
    fn convert_path_to_query(path_query: &PathQuery, start_index: usize) -> Query {
        let path = &path_query.path;
        let mut last_query = None;

        for i in (start_index..path.len()).rev() {
            let mut current_query = Query::new();
            current_query.insert_key(path[i].clone());
            if last_query.is_none() {
                // add the path queries query as condition
                current_query.add_conditional_subquery(
                    QueryItem::Key(path[i].clone()),
                    None,
                    Some(path_query.query.query.clone()),
                )
            } else {
                current_query.add_conditional_subquery(
                    QueryItem::Key(path[i].clone()),
                    None,
                    last_query,
                )
            }
            last_query = Some(current_query)
        }

        if let Some(final_query) = last_query {
            final_query
        } else {
            path_query.query.query.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use merk::proofs::Query;

    use crate::{
        tests::{make_deep_tree, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    #[test]
    fn test_has_subpaths() {
        let path_query_one = PathQuery::new_unsized(vec![b"a".to_vec()], Query::new());
        let path_query_two =
            PathQuery::new_unsized(vec![b"c".to_vec(), b"b".to_vec()], Query::new());
        let has_subpaths = PathQuery::has_subpaths(&[&path_query_one, &path_query_two]);
        assert_eq!(has_subpaths, false);

        let path_query_one = PathQuery::new_unsized(vec![b"a".to_vec()], Query::new());
        let path_query_two =
            PathQuery::new_unsized(vec![b"a".to_vec(), b"b".to_vec()], Query::new());
        let has_subpaths = PathQuery::has_subpaths(&[&path_query_one, &path_query_two]);
        assert_eq!(has_subpaths, true);

        let path_query_one =
            PathQuery::new_unsized(vec![b"a".to_vec(), b"b".to_vec()], Query::new());
        let path_query_two =
            PathQuery::new_unsized(vec![b"a".to_vec(), b"b".to_vec()], Query::new());
        let has_subpaths = PathQuery::has_subpaths(&[&path_query_one, &path_query_two]);
        assert_eq!(has_subpaths, true);

        let path_query_one = PathQuery::new_unsized(
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()],
            Query::new(),
        );
        let path_query_two = PathQuery::new_unsized(
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
            Query::new(),
        );
        let has_subpaths = PathQuery::has_subpaths(&[&path_query_one, &path_query_two]);
        assert_eq!(has_subpaths, true);
    }

    #[test]
    fn test_same_path_different_query_merge() {
        let temp_db = make_deep_tree();

        // starting with no subquery, just a single path and a key query
        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove_query(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query(proof.as_slice(), &path_query_one).expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db.prove_query(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query(proof.as_slice(), &path_query_two).expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two]).unwrap();
        assert!(matches!(
            merged_path_query,
            Err(Error::InvalidInput(
                "path query path's should be non subset"
            ))
        ));
    }

    #[test]
    fn test_different_same_length_path_with_different_query_merge() {
        let temp_db = make_deep_tree();

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove_query(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query(proof.as_slice(), &path_query_one).expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key4".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], query_two);

        let proof = temp_db.prove_query(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query(proof.as_slice(), &path_query_two).expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two])
            .unwrap()
            .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![TEST_LEAF.to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);

        let proof = temp_db.prove_query(&merged_path_query).unwrap().unwrap();
        let (_, result_set_merged) = GroveDb::verify_query(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 2);

        let keys = [b"key1".to_vec(), b"key4".to_vec()];
        let values = [b"value1".to_vec(), b"value4".to_vec()];
        let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        assert_eq!(result_set_merged, expected_result_set);

        // longer length path queries
        let mut query_one = Query::new();
        query_one.insert_all();
        let path_query_one = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_node_2".to_vec(),
            ],
            query_one,
        );

        let proof = temp_db.prove_query(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query(proof.as_slice(), &path_query_one).expect("should execute proof");
        assert_eq!(result_set_one.len(), 3);

        let mut query_two = Query::new();
        query_two.insert_all();

        let path_query_two = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_node_4".to_vec(),
            ],
            query_two,
        );

        let proof = temp_db.prove_query(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query(proof.as_slice(), &path_query_two).expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let mut query_three = Query::new();
        query_three.insert_range_after(b"key7".to_vec()..);

        let path_query_three = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_node_3".to_vec(),
            ],
            query_three,
        );

        let proof = temp_db.prove_query(&path_query_three).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::verify_query(proof.as_slice(), &path_query_three)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two, &path_query_three])
                .unwrap()
                .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![b"deep_leaf".to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);

        let proof = temp_db.prove_query(&merged_path_query).unwrap().unwrap();
        let (_, result_set_merged) = GroveDb::verify_query(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 7);

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
        assert_eq!(result_set_merged, expected_result_set);
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

        let proof = temp_db.prove_query(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query(proof.as_slice(), &path_query_one).expect("should execute proof");
        assert_eq!(result_set_one.len(), 6);

        let mut query_two = Query::new();
        query_two.insert_all();

        let path_query_two = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_node_4".to_vec(),
            ],
            query_two,
        );

        let proof = temp_db.prove_query(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query(proof.as_slice(), &path_query_two).expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let merged_path_query = PathQuery::merge(vec![&path_query_one, &path_query_two])
            .unwrap()
            .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![b"deep_leaf".to_vec()]);

        let proof = temp_db.prove_query(&merged_path_query).unwrap().unwrap();
        let (_, result_set_merged) = GroveDb::verify_query(proof.as_slice(), &merged_path_query)
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
        assert_eq!(result_set_merged, expected_result_set);
    }

    #[test]
    fn test_same_path_and_different_path_query_merge() {
        let temp_db = make_deep_tree();

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove_query(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query(proof.as_slice(), &path_query_one).expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db.prove_query(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query(proof.as_slice(), &path_query_two).expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let mut query_three = Query::new();
        query_three.insert_all();
        let path_query_three = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
            query_three,
        );

        let proof = temp_db.prove_query(&path_query_three).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::verify_query(proof.as_slice(), &path_query_three)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two, &path_query_three]).unwrap();
        assert!(matches!(
            merged_path_query,
            Err(Error::InvalidInput(
                "path query path's should be non subset"
            ))
        ));
    }
}
