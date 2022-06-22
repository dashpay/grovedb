use std::path::Path;

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
        // let mut common_path = vec![];
        let min_length = vec![p1, p2].into_iter().map(|q| q.path.len()).min();

        if p1.path == p2.path {
            let combined_query = Query::merge(p1.query.query.clone(), p2.query.query.clone());
            PathQuery::new_unsized(p1.path.clone(), combined_query)
        } else {
            let (common_path, next_index) =
                PathQuery::get_common_path(vec![p1.path.clone(), p2.path.clone()]);
            dbg!(common_path);
            dbg!(next_index);
            // at this point, we need to create a new function that takes the paths and the
            // index then builds up the query.
            panic!("path lengths are not the same");
        }
    }

    fn get_common_path(paths: Vec<Vec<Vec<u8>>>) -> (Vec<Vec<u8>>, usize) {
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
        query_two.insert_key(b"key4".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], query_two);

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

        let keys = [b"key1".to_vec(), b"key4".to_vec()];
        let values = [b"value1".to_vec(), b"value4".to_vec()];
        let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        assert_eq!(result_set_merged, expected_result_set);
    }
}
