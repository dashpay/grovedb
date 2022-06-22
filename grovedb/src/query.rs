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
        let combined_query = Query::merge(p1.query.query.clone(), p2.query.query.clone());
        PathQuery::new_unsized(p1.path.clone(), combined_query)
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
    fn test_path_query_merge() {
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
    }
}
