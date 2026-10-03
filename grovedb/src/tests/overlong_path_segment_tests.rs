//! A caller's path with a segment longer than 255 bytes is refused with
//! `InvalidInput` instead of panicking in storage prefix construction (#680).
//!
//! Each public entry point is called at `GROVE_V1`, `GROVE_V3` and the latest
//! version with the overlong segment as the whole path, last under an
//! existing tree, first in a longer path, and in the middle of one. Calls
//! that never reached prefix construction keep their old outcome.

#[cfg(test)]
mod tests {
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::{v1::GROVE_V1, v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
        operations::delete::DeleteUpTreeOptions,
        query::{PathBranchChunkQuery, PathTrunkChunkQuery},
        query_result_type::QueryResultType,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TEST_LEAF},
        AggregateSumPathQuery, BackwardsReferences, Element, Error, GroveDb, PathQuery, Query,
        QueryItem, SizedQuery,
    };

    const OVERLONG: [u8; 256] = [0xAB; 256];
    const KEY: &[u8] = b"k";
    const ELEMENTS: QueryResultType = QueryResultType::QueryElementResultType;

    /// A call at `path` (owned and as segments) and a version.
    type Call = fn(&GroveDb, &[Vec<u8>], &[&[u8]], &GroveVersion) -> Result<(), Error>;

    fn versions() -> [(&'static str, &'static GroveVersion); 3] {
        [
            ("GROVE_V1", &GROVE_V1),
            ("GROVE_V3", &GROVE_V3),
            ("latest", GroveVersion::latest()),
        ]
    }

    /// The overlong segment last (alone and under an existing tree), then
    /// before the last (first and in the middle).
    fn overlong_paths() -> [(bool, Vec<Vec<u8>>); 4] {
        [
            (true, vec![OVERLONG.to_vec()]),
            (true, vec![TEST_LEAF.to_vec(), OVERLONG.to_vec()]),
            (false, vec![OVERLONG.to_vec(), b"x".to_vec()]),
            (
                false,
                vec![TEST_LEAF.to_vec(), OVERLONG.to_vec(), b"x".to_vec()],
            ),
        ]
    }

    fn item() -> Element {
        Element::new_item(b"v".to_vec())
    }

    fn range_full(path: &[Vec<u8>]) -> PathQuery {
        PathQuery::new_unsized(path.to_vec(), Query::new_range_full())
    }

    fn limited(path: &[Vec<u8>]) -> PathQuery {
        PathQuery::new(
            path.to_vec(),
            SizedQuery::new(Query::new_single_key(KEY.to_vec()), Some(1), None),
        )
    }

    fn range() -> QueryItem {
        QueryItem::Range(b"a".to_vec()..b"z".to_vec())
    }

    fn batch(db: &GroveDb, op: QualifiedGroveDbOp, v: &GroveVersion) -> Result<(), Error> {
        db.apply_batch(vec![op], None, None, v).unwrap()
    }

    fn is_refusal(result: &Result<(), Error>) -> bool {
        matches!(
            result,
            Err(Error::InvalidInput(message))
                if *message == "path segment length must be at most 255 bytes"
        )
    }

    /// Calls that build a prefix from every segment of the path they get.
    #[allow(deprecated)] // query_encoded_many
    fn path_calls() -> Vec<(&'static str, Call)> {
        vec![
            ("get", |db, _, p, v| {
                db.get(p, KEY, None, v).unwrap().map(drop)
            }),
            ("get_raw", |db, _, p, v| {
                db.get_raw(p.into(), KEY, None, v).unwrap().map(drop)
            }),
            ("get_raw_optional", |db, _, p, v| {
                db.get_raw_optional(p.into(), KEY, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("get_raw_caching_optional", |db, _, p, v| {
                db.get_raw_caching_optional(p.into(), KEY, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("has_raw", |db, _, p, v| {
                db.has_raw(p, KEY, None, v).unwrap().map(drop)
            }),
            ("find_subtrees", |db, _, p, v| {
                db.find_subtrees(&p.into(), None, v).unwrap().map(drop)
            }),
            ("insert", |db, _, p, v| {
                db.insert(p, KEY, item(), None, None, v).unwrap()
            }),
            ("insert tree", |db, _, p, v| {
                db.insert(p, KEY, Element::empty_tree(), None, None, v)
                    .unwrap()
            }),
            ("insert_if_not_exists", |db, _, p, v| {
                db.insert_if_not_exists(p, KEY, item(), None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("insert_if_changed_value", |db, _, p, v| {
                db.insert_if_changed_value(p, KEY, item(), None, v)
                    .unwrap()
                    .map(drop)
            }),
            (
                "insert_if_not_exists_return_existing_element",
                |db, _, p, v| {
                    db.insert_if_not_exists_return_existing_element(p, KEY, item(), None, v)
                        .unwrap()
                        .map(drop)
                },
            ),
            ("insert reference to the path", |db, path, _, v| {
                let mut target = path.to_vec();
                target.push(KEY.to_vec());
                let reference =
                    Element::new_reference(ReferencePathType::AbsolutePathReference(target));
                db.insert([TEST_LEAF].as_ref(), b"r", reference, None, None, v)
                    .unwrap()
            }),
            ("delete", |db, _, p, v| {
                db.delete(p, KEY, None, None, v).unwrap()
            }),
            ("delete_if_empty_tree", |db, _, p, v| {
                db.delete_if_empty_tree(p, KEY, None, v).unwrap().map(drop)
            }),
            ("delete_up_tree_while_empty", |db, _, p, v| {
                db.delete_up_tree_while_empty(p, KEY, &DeleteUpTreeOptions::default(), None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("clear_subtree", |db, _, p, v| {
                db.clear_subtree(p, None, None, v).map(drop)
            }),
            ("insert_into_count_indexed_tree", |db, _, p, v| {
                db.insert_into_count_indexed_tree(p, KEY, item(), None, v)
                    .unwrap()
            }),
            ("delete_from_count_indexed_tree", |db, _, p, v| {
                db.delete_from_count_indexed_tree(p, KEY, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("reconcile_indexed_tree_secondaries", |db, _, p, v| {
                db.reconcile_indexed_tree_secondaries(p, None, v).unwrap()
            }),
            ("mmr_tree_append", |db, _, p, v| {
                db.mmr_tree_append(p, KEY, b"v".to_vec(), None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("mmr_tree_get_value", |db, _, p, v| {
                db.mmr_tree_get_value(p, KEY, 0, None, v).unwrap().map(drop)
            }),
            ("mmr_tree_leaf_count", |db, _, p, v| {
                db.mmr_tree_leaf_count(p, KEY, None, v).unwrap().map(drop)
            }),
            ("mmr_tree_root_hash", |db, _, p, v| {
                db.mmr_tree_root_hash(p, KEY, None, v).unwrap().map(drop)
            }),
            ("bulk_append", |db, _, p, v| {
                db.bulk_append(p, KEY, b"v".to_vec(), None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("bulk_get_value", |db, _, p, v| {
                db.bulk_get_value(p, KEY, 0, None, v).unwrap().map(drop)
            }),
            ("bulk_get_range", |db, _, p, v| {
                db.bulk_get_range(p, KEY, 0, 1, None, v).unwrap().map(drop)
            }),
            ("bulk_get_chunk", |db, _, p, v| {
                db.bulk_get_chunk(p, KEY, 0, None, v).unwrap().map(drop)
            }),
            ("bulk_get_buffer", |db, _, p, v| {
                db.bulk_get_buffer(p, KEY, None, v).unwrap().map(drop)
            }),
            ("bulk_count", |db, _, p, v| {
                db.bulk_count(p, KEY, None, v).unwrap().map(drop)
            }),
            ("bulk_chunk_count", |db, _, p, v| {
                db.bulk_chunk_count(p, KEY, None, v).unwrap().map(drop)
            }),
            ("prove_bulk_position_range", |db, path, _, v| {
                db.prove_bulk_position_range(path.to_vec(), KEY, 0, 1, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("dense_tree_insert", |db, _, p, v| {
                db.dense_tree_insert(p, KEY, b"v".to_vec(), None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("dense_tree_get", |db, _, p, v| {
                db.dense_tree_get(p, KEY, 0, None, v).unwrap().map(drop)
            }),
            ("dense_tree_count", |db, _, p, v| {
                db.dense_tree_count(p, KEY, None, v).unwrap().map(drop)
            }),
            ("dense_tree_root_hash", |db, _, p, v| {
                db.dense_tree_root_hash(p, KEY, None, v).unwrap().map(drop)
            }),
            ("commitment_tree_anchor", |db, _, p, v| {
                db.commitment_tree_anchor(p, KEY, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("commitment_tree_get_value", |db, _, p, v| {
                db.commitment_tree_get_value(p, KEY, 0, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("commitment_tree_get_range", |db, _, p, v| {
                db.commitment_tree_get_range(p, KEY, 0, 1, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("commitment_tree_count", |db, _, p, v| {
                db.commitment_tree_count(p, KEY, None, v).unwrap().map(drop)
            }),
            ("query", |db, path, _, v| {
                db.query(&range_full(path), true, true, true, ELEMENTS, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_raw", |db, path, _, v| {
                db.query_raw(&range_full(path), true, true, true, ELEMENTS, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_item_value", |db, path, _, v| {
                db.query_item_value(&range_full(path), true, true, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_item_value_or_sum", |db, path, _, v| {
                db.query_item_value_or_sum(&range_full(path), true, true, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_sums", |db, path, _, v| {
                db.query_sums(&range_full(path), true, true, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_keys_optional", |db, path, _, v| {
                db.query_keys_optional(&limited(path), true, true, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_raw_keys_optional", |db, path, _, v| {
                db.query_raw_keys_optional(&limited(path), true, true, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_many_raw", |db, path, _, v| {
                db.query_many_raw(&[&range_full(path)], true, true, true, ELEMENTS, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_encoded_many", |db, path, _, v| {
                db.query_encoded_many(&[&range_full(path)], true, true, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("run_path_query", |db, path, _, v| {
                db.run_path_query(&range_full(path), true, true, true, ELEMENTS, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_aggregate_count", |db, path, _, v| {
                let query = PathQuery::new_aggregate_count_on_range(path.to_vec(), range());
                db.query_aggregate_count(&query, None, v).unwrap().map(drop)
            }),
            ("query_aggregate_sum", |db, path, _, v| {
                let query = PathQuery::new_aggregate_sum_on_range(path.to_vec(), range());
                db.query_aggregate_sum(&query, None, v).unwrap().map(drop)
            }),
            ("query_aggregate_count_and_sum", |db, path, _, v| {
                let query = PathQuery::new_aggregate_count_and_sum_on_range(path.to_vec(), range());
                db.query_aggregate_count_and_sum(&query, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_aggregate_count_per_key", |db, path, _, v| {
                let query = PathQuery::new_aggregate_count_on_range(path.to_vec(), range());
                db.query_aggregate_count_per_key(&query, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_aggregate_sum_per_key", |db, path, _, v| {
                let query = PathQuery::new_aggregate_sum_on_range(path.to_vec(), range());
                db.query_aggregate_sum_per_key(&query, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_aggregate_count_and_sum_per_key", |db, path, _, v| {
                let query = PathQuery::new_aggregate_count_and_sum_on_range(path.to_vec(), range());
                db.query_aggregate_count_and_sum_per_key(&query, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("query_aggregate_sums", |db, path, _, v| {
                let query = AggregateSumPathQuery::new_single_key(path.to_vec(), KEY.to_vec(), 10);
                db.query_aggregate_sums(&query, true, true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("prove_trunk_chunk", |db, path, _, v| {
                db.prove_trunk_chunk(&PathTrunkChunkQuery::new(path.to_vec(), 2), v)
                    .unwrap()
                    .map(drop)
            }),
            ("prove_branch_chunk", |db, path, _, v| {
                let query = PathBranchChunkQuery::new(path.to_vec(), KEY.to_vec(), 2);
                db.prove_branch_chunk(&query, v).unwrap().map(drop)
            }),
            ("batch insert_or_replace_op", |db, path, _, v| {
                let op =
                    QualifiedGroveDbOp::insert_or_replace_op(path.to_vec(), KEY.to_vec(), item());
                batch(db, op, v)
            }),
            ("batch insert_or_replace_op tree", |db, path, _, v| {
                let op = QualifiedGroveDbOp::insert_or_replace_op(
                    path.to_vec(),
                    KEY.to_vec(),
                    Element::empty_tree(),
                );
                batch(db, op, v)
            }),
            ("batch insert_only_op", |db, path, _, v| {
                batch(
                    db,
                    QualifiedGroveDbOp::insert_only_op(path.to_vec(), KEY.to_vec(), item()),
                    v,
                )
            }),
            ("batch insert_if_not_exists_op", |db, path, _, v| {
                let op = QualifiedGroveDbOp::insert_if_not_exists_op(
                    path.to_vec(),
                    KEY.to_vec(),
                    item(),
                );
                batch(db, op, v)
            }),
            ("batch replace_op", |db, path, _, v| {
                batch(
                    db,
                    QualifiedGroveDbOp::replace_op(path.to_vec(), KEY.to_vec(), item()),
                    v,
                )
            }),
            ("batch patch_op", |db, path, _, v| {
                batch(
                    db,
                    QualifiedGroveDbOp::patch_op(path.to_vec(), KEY.to_vec(), item(), 0),
                    v,
                )
            }),
            ("batch refresh_reference_op", |db, path, _, v| {
                let op = QualifiedGroveDbOp::refresh_reference_op(
                    path.to_vec(),
                    KEY.to_vec(),
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"t".to_vec(),
                    ]),
                    None,
                    None,
                    false,
                    true,
                );
                batch(db, op, v)
            }),
            ("batch delete_op", |db, path, _, v| {
                batch(
                    db,
                    QualifiedGroveDbOp::delete_op(path.to_vec(), KEY.to_vec()),
                    v,
                )
            }),
            ("batch delete_tree_op", |db, path, _, v| {
                let op = QualifiedGroveDbOp::delete_tree_op(
                    path.to_vec(),
                    KEY.to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::Error,
                );
                batch(db, op, v)
            }),
            ("batch insert reference to the path", |db, path, _, v| {
                let mut target = path.to_vec();
                target.push(KEY.to_vec());
                let reference =
                    Element::new_reference(ReferencePathType::AbsolutePathReference(target));
                let op = QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"r".to_vec(),
                    reference,
                );
                batch(db, op, v)
            }),
            ("apply_partial_batch", |db, path, _, v| {
                let op =
                    QualifiedGroveDbOp::insert_or_replace_op(path.to_vec(), KEY.to_vec(), item());
                db.apply_partial_batch(vec![op], None, |_, _| Ok(vec![]), None, v)
                    .unwrap()
            }),
        ]
    }

    /// Path-building calls that exist only from `GROVE_V4`; earlier versions
    /// refuse them by version before they read the path.
    fn v4_path_calls() -> Vec<(&'static str, Call)> {
        vec![
            ("drop_flat_subtree", |db, _, p, v| {
                db.drop_flat_subtree(p, KEY, BackwardsReferences::DontCheck, None, v)
                    .unwrap()
            }),
            ("private_document_store_insert", |db, _, p, v| {
                db.private_document_store_insert(p, KEY, b"v".to_vec(), None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("private_document_store_get_value", |db, _, p, v| {
                db.private_document_store_get_value(p, KEY, 0, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("private_document_store_count", |db, _, p, v| {
                db.private_document_store_count(p, KEY, None, v)
                    .unwrap()
                    .map(drop)
            }),
        ]
    }

    /// Calls that first look the last path segment up as a key in its
    /// parent. An overlong last segment is a key no tree has, so they refuse
    /// only an overlong segment before the last, and otherwise keep their old
    /// not-found result.
    fn parent_first_calls() -> Vec<(&'static str, Call)> {
        vec![
            ("follow_reference", |db, _, p, v| {
                db.follow_reference(p.into(), true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("follow_reference_as_stored", |db, _, p, v| {
                db.follow_reference_as_stored(p.into(), true, None, v)
                    .unwrap()
                    .map(drop)
            }),
            ("check_subtree_exists_invalid_path", |db, _, p, v| {
                db.check_subtree_exists_invalid_path(p.into(), None, v)
                    .unwrap()
            }),
            ("is_empty_tree", |db, _, p, v| {
                db.is_empty_tree(p, None, v).unwrap().map(drop)
            }),
            ("batch mmr_tree_append_op", |db, path, _, v| {
                batch(
                    db,
                    QualifiedGroveDbOp::mmr_tree_append_op(path.to_vec(), b"v".to_vec()),
                    v,
                )
            }),
            ("batch bulk_append_op", |db, path, _, v| {
                batch(
                    db,
                    QualifiedGroveDbOp::bulk_append_op(path.to_vec(), b"v".to_vec()),
                    v,
                )
            }),
            ("batch dense_tree_insert_op", |db, path, _, v| {
                batch(
                    db,
                    QualifiedGroveDbOp::dense_tree_insert_op(path.to_vec(), b"v".to_vec()),
                    v,
                )
            }),
        ]
    }

    fn run(
        calls: Vec<(&'static str, Call)>,
        refused_when_last: bool,
        versions: &[(&'static str, &'static GroveVersion)],
    ) -> Vec<String> {
        let mut mismatches = Vec::new();
        for &(version_name, v) in versions {
            let db = make_test_grovedb(v);
            for (overlong_is_last, path) in overlong_paths() {
                let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                for (name, call) in &calls {
                    let result = call(&db, &path, &segments, v);
                    let refused = is_refusal(&result);
                    let expect_refused = refused_when_last || !overlong_is_last;
                    if refused != expect_refused || (!refused && result.is_ok()) {
                        mismatches.push(format!(
                            "{name} at {version_name}, {} segments: {result:?}",
                            path.len()
                        ));
                    }
                }
            }
        }
        mismatches
    }

    #[test]
    fn every_path_building_call_refuses_an_overlong_segment() {
        let mut mismatches = run(path_calls(), true, &versions());
        mismatches.extend(run(
            v4_path_calls(),
            true,
            &[("latest", GroveVersion::latest())],
        ));
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    #[test]
    fn parent_first_calls_refuse_an_overlong_segment_before_the_last() {
        let mismatches = run(parent_first_calls(), false, &versions());
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// The check sits inside the known-version arms, so an unknown version
    /// still answers with the version error it gave before the check existed.
    #[test]
    fn find_subtrees_reports_an_unknown_version_before_the_path() {
        let mut v = GroveVersion::latest().clone();
        v.grovedb_versions
            .operations
            .non_merk_tree
            .subtree_discovery = 2;
        let db = make_test_grovedb(GroveVersion::latest());
        let path: &[&[u8]] = &[&OVERLONG];
        let result = db.find_subtrees(&path.into(), None, &v).unwrap();
        assert!(
            matches!(result, Err(Error::VersionError(_))),
            "got {result:?}"
        );
    }

    #[test]
    fn a_subquery_path_with_an_overlong_segment_is_refused_where_it_is_read() {
        let mut query = Query::new_single_key(TEST_LEAF.to_vec());
        query.set_subquery_path(vec![OVERLONG.to_vec()]);
        query.set_subquery(Query::new_range_full());
        let path_query = PathQuery::new_unsized(vec![], query);
        for (version_name, v) in versions() {
            let db = make_test_grovedb(v);
            let query = db
                .query(&path_query, true, true, true, ELEMENTS, None, v)
                .unwrap();
            assert!(is_refusal(&query.map(drop)), "query at {version_name}");
            let query_raw = db
                .query_raw(&path_query, true, true, true, ELEMENTS, None, v)
                .unwrap();
            assert!(
                is_refusal(&query_raw.map(drop)),
                "query_raw at {version_name}"
            );
        }
    }

    /// Proof generation never opens a subtree it has not proven present, so
    /// it never reached prefix construction for these paths and still proves
    /// them absent, with the V0 and the V1 prover alike.
    #[test]
    fn proofs_over_an_overlong_path_still_prove_absence() {
        for (version_name, v) in versions() {
            let db = make_test_grovedb(v);
            for (_, path) in overlong_paths() {
                let path_query = range_full(&path);
                let proof = db
                    .prove_query(&path_query, None, v)
                    .unwrap()
                    .unwrap_or_else(|e| panic!("{version_name}: {e:?}"));
                let (root_hash, elements) = GroveDb::verify_query(&proof, &path_query, v)
                    .unwrap_or_else(|e| panic!("{version_name}: {e:?}"));
                assert_eq!(root_hash, db.root_hash(None, v).unwrap().unwrap());
                assert!(elements.is_empty(), "{version_name}");
            }
        }
    }
}
