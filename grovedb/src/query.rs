use std::{collections::HashMap, path::Path};

use merk::proofs::{query::QueryItem, Query};

#[derive(Debug, Clone)]
pub struct PathQuery {
    // TODO: Make generic over path type
    pub path: Vec<Vec<u8>>,
    pub query: SizedQuery,
}

// If a subquery exists :
// limit should be applied to the elements returned by the subquery
// offset should be applied to the first item that will subqueried (first in the
// case of a range)
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

    pub fn merge(p1: &PathQuery, p2: &PathQuery) -> Self {
        if p1.path == p2.path {
            let combined_query = Query::merge(p1.query.query.clone(), p2.query.query.clone());
            PathQuery::new_unsized(p1.path.clone(), combined_query)
        } else {
            let paths: Vec<&[Vec<u8>]> = vec![&p1.path, &p2.path];
            let path_queries = vec![p1, p2];
            // let queries = vec![p1.query.query.clone(), p2.query.query.clone()];

            let (common_path, next_index) = PathQuery::get_common_path(vec![&p1.path, &p2.path]);
            dbg!(&common_path);
            // dbg!(next_index);
            // dbg!(p1.path.len());

            let query = PathQuery::build_query(path_queries, next_index, p1.path.len() - 1);
            dbg!(&query);
            // at this point, we need to create a new function that takes the paths and the
            // index then builds up the query.
            PathQuery::new_unsized(common_path, query)
        }
    }

    // when building conditional, we want to recurse on build query
    // but when we get to the end of the path, we want to get the query for that
    // path we have an array of paths and queries, we need to preserve the index
    // values we care about actually, we need to know the position of a path
    // maybe it makes more sense to pass the path query that way we always have
    // access to the query

    fn build_query(path_queries: Vec<&PathQuery>, start_index: usize, last_index: usize) -> Query {
        dbg!(start_index);
        dbg!(last_index);
        let mut level = start_index;
        let keys_at_level = path_queries
            .iter()
            .map(|&path_query| &path_query.path[level])
            .collect::<Vec<_>>();

        // we need to group the paths based on their distinct nature
        let mut path_branches: HashMap<_, Vec<usize>> = HashMap::new();
        for (path_index, key) in keys_at_level.iter().enumerate() {
            if path_branches.contains_key(key) {
                // get the current element then add the new path index to it
                let current_path_index_array = path_branches
                    .get_mut(key)
                    .expect("confirmed hashmap contains key");
                current_path_index_array.push(path_index);
            } else {
                path_branches.insert(key, vec![path_index]);
            }
        }

        dbg!(&path_branches);

        // the key in path_branches represents all the paths that have the same keys up
        // to that point based on the assumption that all paths are equal, then
        // if two paths have the same values at that point when
        // we should combine their queries and use the resulting query for the
        // conditional.

        // for each grouped key, we want to create a path
        let mut query = Query::new();
        for (key, value) in path_branches.drain() {
            dbg!("loop started");
            query.insert_key(key.to_vec());

            dbg!("start_index at loop", start_index);
            dbg!("last_index at loop", last_index);
            let next_query = if start_index == last_index {
                // use the query from the path query
                dbg!("they are equal should not call build again");
                path_queries[value[0]].query.query.clone()
            } else {
                dbg!("I got called");
                let mut new_path_queries = vec![];
                for a in value {
                    new_path_queries.push(path_queries[a]);
                }
                Self::build_query(new_path_queries, start_index + 1, last_index)
            };
            dbg!("end of call");

            query.add_conditional_subquery(QueryItem::Key(key.to_vec()), None, Some(next_query));
        }
        dbg!("loop ended");

        query
    }

    fn get_common_path(paths: Vec<&[Vec<u8>]>) -> (Vec<Vec<u8>>, usize) {
        if paths.len() == 0 {
            return (vec![], 0);
        }

        let mut common_path = vec![];
        let mut level = 0;

        while level < paths[0].len() {
            let keys_at_level = paths.iter().map(|path| &path[level]).collect::<Vec<_>>();
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
}

#[cfg(test)]
mod tests {
    use merk::proofs::{query::QueryItem, Query};

    use crate::{
        tests::{make_deep_tree, ANOTHER_TEST_LEAF, TEST_LEAF},
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

        let proof = temp_db.prove(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) = GroveDb::execute_proof(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db.prove(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::execute_proof(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query = PathQuery::merge(&path_query_one, &path_query_two);
        assert_eq!(
            merged_path_query.path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(merged_path_query.query.query.items.len(), 2);

        let proof = temp_db.prove(&merged_path_query).unwrap().unwrap();
        let (_, result_set_merged) = GroveDb::execute_proof(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 2);

        let keys = [b"key1".to_vec(), b"key2".to_vec()];
        let values = [b"value1".to_vec(), b"value2".to_vec()];
        let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        assert_eq!(result_set_merged, expected_result_set);

        // TODO: add test for range
    }

    #[test]
    fn test_different_path_different_query_merge() {
        let temp_db = make_deep_tree();

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db.prove(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) = GroveDb::execute_proof(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key4".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], query_two);

        let proof = temp_db.prove(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::execute_proof(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query = PathQuery::merge(&path_query_one, &path_query_two);
        assert_eq!(merged_path_query.path, vec![TEST_LEAF.to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);

        let proof = temp_db.prove(&merged_path_query).unwrap().unwrap();
        let (_, result_set_merged) = GroveDb::execute_proof(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 2);

        let keys = [b"key1".to_vec(), b"key4".to_vec()];
        let values = [b"value1".to_vec(), b"value4".to_vec()];
        let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        assert_eq!(result_set_merged, expected_result_set);

        // different from base
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

        let proof = temp_db.prove(&path_query_one).unwrap().unwrap();
        let (_, result_set_one) = GroveDb::execute_proof(proof.as_slice(), &path_query_one)
            .expect("should execute proof");
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

        let proof = temp_db.prove(&path_query_two).unwrap().unwrap();
        let (_, result_set_two) = GroveDb::execute_proof(proof.as_slice(), &path_query_two)
            .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let merged_path_query = PathQuery::merge(&path_query_one, &path_query_two);
        assert_eq!(merged_path_query.path, vec![b"deep_leaf".to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);

        let proof = temp_db.prove(&merged_path_query).unwrap().unwrap();
        let (_, result_set_merged) = GroveDb::execute_proof(proof.as_slice(), &merged_path_query)
            .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 5);

        let keys = [
            b"key4".to_vec(),
            b"key5".to_vec(),
            b"key6".to_vec(),
            b"key10".to_vec(),
            b"key11".to_vec(),
        ];
        let values = [
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
}
