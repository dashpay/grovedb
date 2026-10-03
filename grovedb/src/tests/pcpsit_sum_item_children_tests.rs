//! A bare `SumItem` as a `ProvableCountProvableSumIndexedTree` child
//! (`GROVE_V4`, `insert.validate_indexed_child_for_variant: 1`).
//!
//! The shape these tests build is a per-group counter ranked two levels up:
//! authors ranked by how many posts they have, how many likes those posts
//! have, and how many likes a post gets on average, above each author's posts
//! ranked by their likes. Every post is one `SumItem` holding its like count,
//! counting one and adding its likes, and a like rewrites it in place.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        operations::proof::{indexed_axis::AxisEntries, VerifiedPathQuery},
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    fn pcpsit(axes: &[IndexAxis]) -> Element {
        Element::empty_provable_count_provable_sum_indexed_tree(
            axes.iter().map(|axis| (axis.tag(), None)).collect(),
        )
        .expect("axes are canonical")
    }

    /// `authors` (count, sum and average axes) holding one count-sum value
    /// tree per author, each holding a `postId` tree (sum axis) of one
    /// `SumItem` per post.
    fn build(db: &GroveDb, posts: &[(&[u8], &[u8], i64)], v: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"authors",
            pcpsit(&[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert the author ranking");
        let mut authors: Vec<&[u8]> = posts.iter().map(|(author, ..)| *author).collect();
        authors.dedup();
        for author in authors {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"authors"].as_ref(),
                author,
                Element::empty_count_sum_tree(),
                None,
                v,
            )
            .unwrap()
            .expect("insert an author");
            db.insert(
                [TEST_LEAF, b"authors", author].as_ref(),
                b"postId",
                pcpsit(&[IndexAxis::Sum]),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert an author's post ranking");
        }
        for (author, post, likes) in posts {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"authors", author, b"postId"].as_ref(),
                post,
                Element::new_sum_item(*likes),
                None,
                v,
            )
            .unwrap()
            .expect("insert a post's counter");
        }
    }

    /// The top `k` of `path` on `axis`, proved and verified against the root.
    fn top_k(
        db: &GroveDb,
        path: &[&[u8]],
        axis: IndexAxis,
        k: u16,
        v: &GroveVersion,
    ) -> Vec<(i128, Vec<u8>)> {
        let path_query = PathQuery::new_axis_top_k(
            path.iter().map(|segment| segment.to_vec()).collect(),
            axis,
            k,
            0,
            true,
        );
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let VerifiedPathQuery::AxisEntries {
            root_hash, entries, ..
        } = GroveDb::verify_path_query(&proof, &path_query, v).expect("verify")
        else {
            panic!("expected axis entries");
        };
        assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
        match entries {
            AxisEntries::Count(entries) => entries
                .into_iter()
                .map(|entry| (entry.ordering_value as i128, entry.primary_key))
                .collect(),
            AxisEntries::Sum(entries) => entries
                .into_iter()
                .map(|entry| (entry.ordering_value as i128, entry.primary_key))
                .collect(),
            AxisEntries::Avg(entries) => entries
                .into_iter()
                .map(|entry| (entry.ordering_value, entry.primary_key))
                .collect(),
        }
    }

    fn keys(ranking: &[(i128, Vec<u8>)]) -> Vec<&[u8]> {
        ranking.iter().map(|(_, key)| key.as_slice()).collect()
    }

    fn assert_verify_passes(db: &GroveDb, v: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, v)
            .expect("verify_grovedb must not return a hard error");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues: {issues:?}"
        );
    }

    #[test]
    fn should_rank_groups_of_sum_items_by_count_sum_and_average() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build(
            &db,
            &[
                (b"alice", b"p1", 3),
                (b"alice", b"p2", 5),
                (b"bob", b"b1", 10),
            ],
            v,
        );

        // Each SumItem counts one post and adds its likes.
        let alice = db
            .get([TEST_LEAF, b"authors"].as_ref(), b"alice", None, v)
            .unwrap()
            .expect("alice");
        assert_eq!(alice.count_sum_value_or_default(), (2, 8));

        let authors: &[&[u8]] = &[TEST_LEAF, b"authors"];
        assert_eq!(
            top_k(&db, authors, IndexAxis::Sum, 2, v),
            vec![(10, b"bob".to_vec()), (8, b"alice".to_vec())],
            "most likes"
        );
        assert_eq!(
            top_k(&db, authors, IndexAxis::Count, 2, v),
            vec![(2, b"alice".to_vec()), (1, b"bob".to_vec())],
            "most posts"
        );
        assert_eq!(
            keys(&top_k(&db, authors, IndexAxis::Avg, 2, v)),
            vec![b"bob".as_slice(), b"alice"],
            "most likes per post: bob 10, alice 4"
        );
        assert_eq!(
            top_k(
                &db,
                &[TEST_LEAF, b"authors", b"alice", b"postId"],
                IndexAxis::Sum,
                2,
                v
            ),
            vec![(5, b"p2".to_vec()), (3, b"p1".to_vec())],
            "alice's posts by likes"
        );
        assert_verify_passes(&db, v);
    }

    #[test]
    fn should_rerank_every_level_when_a_sum_item_is_rewritten_in_a_batch() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build(
            &db,
            &[
                (b"alice", b"p1", 3),
                (b"alice", b"p2", 5),
                (b"bob", b"b1", 10),
            ],
            v,
        );

        // Ten more likes on p1, written the way a counter is: one replace.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    TEST_LEAF.to_vec(),
                    b"authors".to_vec(),
                    b"alice".to_vec(),
                    b"postId".to_vec(),
                ],
                b"p1".to_vec(),
                Element::new_sum_item(13),
            )],
            None,
            None,
            v,
        )
        .unwrap()
        .expect("rewrite p1's counter");

        let authors: &[&[u8]] = &[TEST_LEAF, b"authors"];
        assert_eq!(
            top_k(&db, authors, IndexAxis::Sum, 2, v),
            vec![(18, b"alice".to_vec()), (10, b"bob".to_vec())],
            "alice now has the most likes"
        );
        assert_eq!(
            top_k(&db, authors, IndexAxis::Count, 2, v),
            vec![(2, b"alice".to_vec()), (1, b"bob".to_vec())],
            "a like moves no post count"
        );
        assert_eq!(
            keys(&top_k(&db, authors, IndexAxis::Avg, 2, v)),
            vec![b"bob".as_slice(), b"alice"],
            "bob still leads per post: 10 against 9"
        );
        assert_eq!(
            top_k(
                &db,
                &[TEST_LEAF, b"authors", b"alice", b"postId"],
                IndexAxis::Sum,
                1,
                v
            ),
            vec![(13, b"p1".to_vec())],
            "p1 leads alice's posts"
        );
        assert_verify_passes(&db, v);
    }
}
