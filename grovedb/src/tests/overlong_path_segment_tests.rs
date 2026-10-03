//! A caller's path with a segment longer than 255 bytes is refused with
//! `InvalidInput` instead of panicking in storage prefix construction (#680).
//!
//! Each public entry point is called with the overlong segment as the whole
//! path, as the last segment under an existing tree, first in a longer path,
//! and in the middle of one. No subtree can have such a path, so each of these
//! calls panicked before. Calls that never reached prefix construction keep
//! their old outcome.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        operations::delete::DeleteUpTreeOptions,
        query::{PathBranchChunkQuery, PathTrunkChunkQuery},
        query_result_type::QueryResultType,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        AggregateSumPathQuery, BackwardsReferences, Element, Error, GroveDb, PathQuery, Query,
        QueryItem, SizedQuery,
    };

    const OVERLONG: [u8; 256] = [0xAB; 256];
    const KEY: &[u8] = b"k";

    fn overlong_paths() -> Vec<Vec<Vec<u8>>> {
        vec![
            vec![OVERLONG.to_vec()],
            vec![TEST_LEAF.to_vec(), OVERLONG.to_vec()],
            vec![OVERLONG.to_vec(), b"x".to_vec()],
            vec![TEST_LEAF.to_vec(), OVERLONG.to_vec(), b"x".to_vec()],
        ]
    }

    fn segments(path: &[Vec<u8>]) -> Vec<&[u8]> {
        path.iter().map(Vec::as_slice).collect()
    }

    fn item() -> Element {
        Element::new_item(b"v".to_vec())
    }

    fn db_with_test_leaves() -> (TempGroveDb, &'static GroveVersion) {
        let grove_version = GroveVersion::latest();
        (make_test_grovedb(grove_version), grove_version)
    }

    #[track_caller]
    fn assert_refused<T: std::fmt::Debug>(call: &str, path: &[Vec<u8>], result: Result<T, Error>) {
        match result {
            Err(Error::InvalidInput(message)) => assert_eq!(
                message,
                "path segment length must be at most 255 bytes",
                "{call} at a path of {} segments",
                path.len()
            ),
            other => panic!(
                "{call} at a path of {} segments: expected the overlong-segment refusal, got \
                 {other:?}",
                path.len()
            ),
        }
    }

    #[test]
    fn element_reads_refuse_an_overlong_path_segment() {
        let (db, v) = db_with_test_leaves();
        for path in overlong_paths() {
            let s = segments(&path);
            let p = s.as_slice();
            assert_refused("get", &path, db.get(p, KEY, None, v).unwrap());
            assert_refused(
                "get_raw",
                &path,
                db.get_raw(p.into(), KEY, None, v).unwrap(),
            );
            assert_refused(
                "get_raw_optional",
                &path,
                db.get_raw_optional(p.into(), KEY, None, v).unwrap(),
            );
            assert_refused(
                "get_raw_caching_optional",
                &path,
                db.get_raw_caching_optional(p.into(), KEY, true, None, v)
                    .unwrap(),
            );
            assert_refused("has_raw", &path, db.has_raw(p, KEY, None, v).unwrap());
            assert_refused(
                "find_subtrees",
                &path,
                db.find_subtrees(&p.into(), None, v).unwrap(),
            );
        }
    }

    #[test]
    fn element_writes_and_deletes_refuse_an_overlong_path_segment() {
        let (db, v) = db_with_test_leaves();
        for path in overlong_paths() {
            let s = segments(&path);
            let p = s.as_slice();
            assert_refused(
                "insert",
                &path,
                db.insert(p, KEY, item(), None, None, v).unwrap(),
            );
            assert_refused(
                "insert tree",
                &path,
                db.insert(p, KEY, Element::empty_tree(), None, None, v)
                    .unwrap(),
            );
            assert_refused(
                "insert_if_not_exists",
                &path,
                db.insert_if_not_exists(p, KEY, item(), None, v).unwrap(),
            );
            assert_refused(
                "insert_if_changed_value",
                &path,
                db.insert_if_changed_value(p, KEY, item(), None, v).unwrap(),
            );
            assert_refused(
                "insert_if_not_exists_return_existing_element",
                &path,
                db.insert_if_not_exists_return_existing_element(p, KEY, item(), None, v)
                    .unwrap(),
            );
            assert_refused("delete", &path, db.delete(p, KEY, None, None, v).unwrap());
            assert_refused(
                "delete_if_empty_tree",
                &path,
                db.delete_if_empty_tree(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "delete_up_tree_while_empty",
                &path,
                db.delete_up_tree_while_empty(p, KEY, &DeleteUpTreeOptions::default(), None, v)
                    .unwrap(),
            );
            assert_refused(
                "drop_flat_subtree",
                &path,
                db.drop_flat_subtree(p, KEY, BackwardsReferences::DontCheck, None, v)
                    .unwrap(),
            );
            assert_refused("clear_subtree", &path, db.clear_subtree(p, None, None, v));
            assert_refused(
                "insert_into_count_indexed_tree",
                &path,
                db.insert_into_count_indexed_tree(p, KEY, item(), None, v)
                    .unwrap(),
            );
            assert_refused(
                "delete_from_count_indexed_tree",
                &path,
                db.delete_from_count_indexed_tree(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "reconcile_indexed_tree_secondaries",
                &path,
                db.reconcile_indexed_tree_secondaries(p, None, v).unwrap(),
            );
        }
    }

    #[test]
    fn non_merk_tree_operations_refuse_an_overlong_path_segment() {
        let (db, v) = db_with_test_leaves();
        let value = || b"v".to_vec();
        for path in overlong_paths() {
            let s = segments(&path);
            let p = s.as_slice();
            assert_refused(
                "mmr_tree_append",
                &path,
                db.mmr_tree_append(p, KEY, value(), None, v).unwrap(),
            );
            assert_refused(
                "mmr_tree_get_value",
                &path,
                db.mmr_tree_get_value(p, KEY, 0, None, v).unwrap(),
            );
            assert_refused(
                "mmr_tree_leaf_count",
                &path,
                db.mmr_tree_leaf_count(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "mmr_tree_root_hash",
                &path,
                db.mmr_tree_root_hash(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "bulk_append",
                &path,
                db.bulk_append(p, KEY, value(), None, v).unwrap(),
            );
            assert_refused(
                "bulk_get_value",
                &path,
                db.bulk_get_value(p, KEY, 0, None, v).unwrap(),
            );
            assert_refused(
                "bulk_get_range",
                &path,
                db.bulk_get_range(p, KEY, 0, 1, None, v).unwrap(),
            );
            assert_refused(
                "bulk_get_chunk",
                &path,
                db.bulk_get_chunk(p, KEY, 0, None, v).unwrap(),
            );
            assert_refused(
                "bulk_get_buffer",
                &path,
                db.bulk_get_buffer(p, KEY, None, v).unwrap(),
            );
            assert_refused("bulk_count", &path, db.bulk_count(p, KEY, None, v).unwrap());
            assert_refused(
                "bulk_chunk_count",
                &path,
                db.bulk_chunk_count(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "prove_bulk_position_range",
                &path,
                db.prove_bulk_position_range(path.clone(), KEY, 0, 1, None, v)
                    .unwrap(),
            );
            assert_refused(
                "dense_tree_insert",
                &path,
                db.dense_tree_insert(p, KEY, value(), None, v).unwrap(),
            );
            assert_refused(
                "dense_tree_get",
                &path,
                db.dense_tree_get(p, KEY, 0, None, v).unwrap(),
            );
            assert_refused(
                "dense_tree_count",
                &path,
                db.dense_tree_count(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "dense_tree_root_hash",
                &path,
                db.dense_tree_root_hash(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "commitment_tree_anchor",
                &path,
                db.commitment_tree_anchor(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "commitment_tree_get_value",
                &path,
                db.commitment_tree_get_value(p, KEY, 0, None, v).unwrap(),
            );
            assert_refused(
                "commitment_tree_get_range",
                &path,
                db.commitment_tree_get_range(p, KEY, 0, 1, None, v).unwrap(),
            );
            assert_refused(
                "commitment_tree_count",
                &path,
                db.commitment_tree_count(p, KEY, None, v).unwrap(),
            );
            assert_refused(
                "private_document_store_insert",
                &path,
                db.private_document_store_insert(p, KEY, value(), None, v)
                    .unwrap(),
            );
            assert_refused(
                "private_document_store_get_value",
                &path,
                db.private_document_store_get_value(p, KEY, 0, None, v)
                    .unwrap(),
            );
            assert_refused(
                "private_document_store_count",
                &path,
                db.private_document_store_count(p, KEY, None, v).unwrap(),
            );
        }
    }

    #[test]
    #[allow(deprecated)] // query_encoded_many
    fn path_queries_refuse_an_overlong_path_segment() {
        let (db, v) = db_with_test_leaves();
        let result_type = QueryResultType::QueryElementResultType;
        for path in overlong_paths() {
            let query = PathQuery::new_unsized(path.clone(), Query::new_range_full());
            let limited = PathQuery::new(
                path.clone(),
                SizedQuery::new(Query::new_single_key(KEY.to_vec()), Some(1), None),
            );
            assert_refused(
                "query",
                &path,
                db.query(&query, true, true, true, result_type, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_raw",
                &path,
                db.query_raw(&query, true, true, true, result_type, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_item_value",
                &path,
                db.query_item_value(&query, true, true, true, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_item_value_or_sum",
                &path,
                db.query_item_value_or_sum(&query, true, true, true, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_sums",
                &path,
                db.query_sums(&query, true, true, true, None, v).unwrap(),
            );
            assert_refused(
                "query_keys_optional",
                &path,
                db.query_keys_optional(&limited, true, true, true, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_raw_keys_optional",
                &path,
                db.query_raw_keys_optional(&limited, true, true, true, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_many_raw",
                &path,
                db.query_many_raw(&[&query], true, true, true, result_type, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_encoded_many",
                &path,
                db.query_encoded_many(&[&query], true, true, true, None, v)
                    .unwrap(),
            );
            assert_refused(
                "run_path_query",
                &path,
                db.run_path_query(&query, true, true, true, result_type, None, v)
                    .unwrap(),
            );

            let range = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
            let count = PathQuery::new_aggregate_count_on_range(path.clone(), range.clone());
            let sum = PathQuery::new_aggregate_sum_on_range(path.clone(), range.clone());
            let count_and_sum =
                PathQuery::new_aggregate_count_and_sum_on_range(path.clone(), range);
            assert_refused(
                "query_aggregate_count",
                &path,
                db.query_aggregate_count(&count, None, v).unwrap(),
            );
            assert_refused(
                "query_aggregate_sum",
                &path,
                db.query_aggregate_sum(&sum, None, v).unwrap(),
            );
            assert_refused(
                "query_aggregate_count_and_sum",
                &path,
                db.query_aggregate_count_and_sum(&count_and_sum, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_aggregate_count_per_key",
                &path,
                db.query_aggregate_count_per_key(&count, None, v).unwrap(),
            );
            assert_refused(
                "query_aggregate_sum_per_key",
                &path,
                db.query_aggregate_sum_per_key(&sum, None, v).unwrap(),
            );
            assert_refused(
                "query_aggregate_count_and_sum_per_key",
                &path,
                db.query_aggregate_count_and_sum_per_key(&count_and_sum, None, v)
                    .unwrap(),
            );
            assert_refused(
                "query_aggregate_sums",
                &path,
                db.query_aggregate_sums(
                    &AggregateSumPathQuery::new_single_key(path.clone(), KEY.to_vec(), 10),
                    true,
                    true,
                    None,
                    v,
                )
                .unwrap(),
            );
            assert_refused(
                "prove_trunk_chunk",
                &path,
                db.prove_trunk_chunk(&PathTrunkChunkQuery::new(path.clone(), 2), v)
                    .unwrap(),
            );
            assert_refused(
                "prove_branch_chunk",
                &path,
                db.prove_branch_chunk(&PathBranchChunkQuery::new(path.clone(), KEY.to_vec(), 2), v)
                    .unwrap(),
            );
        }
    }

    #[test]
    fn a_subquery_path_with_an_overlong_segment_is_refused_where_it_is_read() {
        let (db, v) = db_with_test_leaves();
        let mut query = Query::new_single_key(TEST_LEAF.to_vec());
        query.set_subquery_path(vec![OVERLONG.to_vec()]);
        query.set_subquery(Query::new_range_full());
        let path_query = PathQuery::new_unsized(vec![], query);
        let path = vec![TEST_LEAF.to_vec(), OVERLONG.to_vec()];
        let result_type = QueryResultType::QueryElementResultType;
        assert_refused(
            "query",
            &path,
            db.query(&path_query, true, true, true, result_type, None, v)
                .unwrap(),
        );
        assert_refused(
            "query_raw",
            &path,
            db.query_raw(&path_query, true, true, true, result_type, None, v)
                .unwrap(),
        );
    }

    #[test]
    fn batches_refuse_an_overlong_path_segment() {
        let (db, v) = db_with_test_leaves();
        let apply = |ops| db.apply_batch(ops, None, None, v).unwrap();
        for path in overlong_paths() {
            let at = |op: fn(Vec<Vec<u8>>, Vec<u8>, Element) -> QualifiedGroveDbOp| {
                vec![op(path.clone(), KEY.to_vec(), item())]
            };
            assert_refused(
                "insert_or_replace_op",
                &path,
                apply(at(QualifiedGroveDbOp::insert_or_replace_op)),
            );
            assert_refused(
                "insert_if_not_exists_op",
                &path,
                apply(at(QualifiedGroveDbOp::insert_if_not_exists_op)),
            );
            assert_refused(
                "replace_op",
                &path,
                apply(at(QualifiedGroveDbOp::replace_op)),
            );
            assert_refused(
                "insert_or_replace_op tree",
                &path,
                apply(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    path.clone(),
                    KEY.to_vec(),
                    Element::empty_tree(),
                )]),
            );
            assert_refused(
                "delete_op",
                &path,
                apply(vec![QualifiedGroveDbOp::delete_op(
                    path.clone(),
                    KEY.to_vec(),
                )]),
            );
            assert_refused(
                "apply_partial_batch",
                &path,
                db.apply_partial_batch(
                    at(QualifiedGroveDbOp::insert_or_replace_op),
                    None,
                    |_, _| Ok(vec![]),
                    None,
                    v,
                )
                .unwrap(),
            );
        }
    }

    /// An append op names its tree by its last path segment. An overlong last
    /// segment is a key no tree has, as before; an overlong segment before it
    /// is in the parent's path and is refused.
    #[test]
    fn append_ops_refuse_an_overlong_segment_in_the_parent_path() {
        let (db, v) = db_with_test_leaves();
        let ops = |path: &Vec<Vec<u8>>| {
            [
                QualifiedGroveDbOp::mmr_tree_append_op(path.clone(), b"v".to_vec()),
                QualifiedGroveDbOp::bulk_append_op(path.clone(), b"v".to_vec()),
                QualifiedGroveDbOp::dense_tree_insert_op(path.clone(), b"v".to_vec()),
            ]
        };

        let tree_key_overlong = vec![TEST_LEAF.to_vec(), OVERLONG.to_vec()];
        for op in ops(&tree_key_overlong) {
            let result = db.apply_batch(vec![op], None, None, v).unwrap();
            assert!(
                matches!(result, Err(Error::PathKeyNotFound(_))),
                "an overlong tree key is an absent key, got {result:?}"
            );
        }

        let parent_overlong = vec![TEST_LEAF.to_vec(), OVERLONG.to_vec(), b"x".to_vec()];
        for op in ops(&parent_overlong) {
            assert_refused(
                "append op",
                &parent_overlong,
                db.apply_batch(vec![op], None, None, v).unwrap(),
            );
        }
    }

    #[test]
    fn a_reference_to_an_overlong_path_is_refused() {
        let (db, v) = db_with_test_leaves();
        for path in overlong_paths() {
            let mut target = path.clone();
            target.push(KEY.to_vec());
            let reference =
                Element::new_reference(ReferencePathType::AbsolutePathReference(target));
            assert_refused(
                "insert reference",
                &path,
                db.insert([TEST_LEAF].as_ref(), b"r", reference.clone(), None, None, v)
                    .unwrap(),
            );
            assert_refused(
                "batch insert reference",
                &path,
                db.apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"r".to_vec(),
                        reference,
                    )],
                    None,
                    None,
                    v,
                )
                .unwrap(),
            );
        }
    }

    /// Proof generation never opens a subtree it has not proven present, so
    /// it never reached prefix construction for these paths and still proves
    /// them absent.
    #[test]
    fn proofs_over_an_overlong_path_still_prove_absence() {
        let (db, v) = db_with_test_leaves();
        for path in overlong_paths() {
            let path_query = PathQuery::new_unsized(path.clone(), Query::new_range_full());
            let proof = db
                .prove_query(&path_query, None, v)
                .unwrap()
                .expect("an absent path is proven absent");
            let (root_hash, elements) =
                GroveDb::verify_query(&proof, &path_query, v).expect("the absence proof verifies");
            assert_eq!(root_hash, db.root_hash(None, v).unwrap().unwrap());
            assert!(elements.is_empty());
        }
    }
}
