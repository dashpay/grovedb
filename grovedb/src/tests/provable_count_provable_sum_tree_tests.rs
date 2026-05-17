//! End-to-end behavior tests for `ProvableCountProvableSumTree` in GroveDB.
//!
//! `ProvableCountProvableSumTree` is the dual-axis cousin of
//! `ProvableCountTree` (count baked into node hash) and `ProvableSumTree`
//! (sum baked into node hash). Both aggregates land in every node's hash
//! via `node_hash_with_count_and_sum`, so a single tree supports BOTH
//! `AggregateCountOnRange` AND `AggregateSumOnRange` proofs against the
//! same root hash.
//!
//! Coverage:
//! 1. Direct insert + read round-trip of a
//!    `ProvableCountProvableSumTree`, with parent count/sum reflecting
//!    children's aggregates.
//! 2. Aggregate propagation across positive, negative, zero, extremes.
//! 3. Hash divergence from `ProvableCountSumTree` (which only hashes the
//!    count) and from `ProvableSumTree` (which only hashes the sum) over
//!    the same content.
//! 4. Headline: the same tree produces VERIFIABLE count proofs AND
//!    verifiable sum proofs against the same root hash.
//! 5. Wrapper interactions: `NonCounted` / `NotSummed` /
//!    `NotCountedOrSummed` wrap correctly and behave per their contracts.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{query::QueryItem, Query};
    use grovedb_version::version::GroveVersion;

    use crate::{
        reference_path::ReferencePathType, tests::make_test_grovedb, Element, GroveDb, PathQuery,
    };

    /// 1. Round-trip a `ProvableCountProvableSumTree`: insert it, populate
    /// with mixed `SumItem` children, verify the parent tracks BOTH count
    /// (number of children) AND running sum simultaneously.
    #[test]
    fn provable_count_provable_sum_tree_round_trip_tracks_count_and_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count provable sum tree");

        // Mix of SumItem values: 7, 13, 20. Aggregate = (count=3, sum=40).
        let mut expected_count: u64 = 0;
        let mut expected_sum: i64 = 0;
        for (key, value) in [(b"a".as_slice(), 7i64), (b"b", 13), (b"c", 20)] {
            db.insert(
                &[b"pcps".as_slice()],
                key,
                Element::new_sum_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert sum item");

            expected_count += 1;
            expected_sum += value;

            let fetched = db
                .get(&[] as &[&[u8]], b"pcps", None, grove_version)
                .unwrap()
                .expect("should get parent pcps");
            assert!(matches!(
                fetched,
                Element::ProvableCountProvableSumTree(_, _, _, _)
            ));
            let (running_count, running_sum) = fetched
                .as_provable_count_provable_sum_tree_value()
                .expect("pcps value");
            assert_eq!(
                running_count,
                expected_count,
                "ProvableCountProvableSumTree count must equal running total after inserting {:?}",
                std::str::from_utf8(key).unwrap_or("<non-utf8>")
            );
            assert_eq!(
                running_sum,
                expected_sum,
                "ProvableCountProvableSumTree sum must equal running total after inserting {:?}",
                std::str::from_utf8(key).unwrap_or("<non-utf8>")
            );
        }

        // Children round-trip.
        for (key, expected) in [(b"a".as_slice(), 7i64), (b"b", 13), (b"c", 20)] {
            let elem = db
                .get(&[b"pcps".as_slice()], key, None, grove_version)
                .unwrap()
                .expect("get sum item");
            match elem {
                Element::SumItem(v, _) => assert_eq!(v, expected),
                other => panic!("expected SumItem, got {:?}", other),
            }
        }
    }

    /// 2. Aggregate propagation across negative + zero + extremes.
    #[test]
    fn provable_count_provable_sum_tree_aggregate_negatives_and_zeros() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        // -10, +5, 0, -3 → count = 4, sum = -8.
        for (key, value) in [(b"a".as_slice(), -10i64), (b"b", 5), (b"c", 0), (b"d", -3)] {
            db.insert(
                &[b"pcps".as_slice()],
                key,
                Element::new_sum_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }

        let parent = db
            .get(&[] as &[&[u8]], b"pcps", None, grove_version)
            .unwrap()
            .expect("get parent");
        let (count, sum) = parent
            .as_provable_count_provable_sum_tree_value()
            .expect("pcps value");
        assert_eq!(count, 4);
        assert_eq!(sum, -10 + 5 + 0 + -3);
    }

    /// 3. Hash divergence from `ProvableCountSumTree` AND `ProvableSumTree`
    /// over the same content. `ProvableCountSumTree` hashes ONLY the count;
    /// `ProvableSumTree` hashes ONLY the sum; `ProvableCountProvableSumTree`
    /// hashes BOTH — so its root must differ from both flavors even with
    /// identical children.
    #[test]
    fn pcps_root_hash_diverges_from_pcst_and_pst_over_same_content() {
        let grove_version = GroveVersion::latest();

        fn root_hash_for(
            tree: Element,
            grove_version: &GroveVersion,
        ) -> grovedb_merk::tree::CryptoHash {
            let db = make_test_grovedb(grove_version);
            db.insert(&[] as &[&[u8]], b"root", tree, None, None, grove_version)
                .unwrap()
                .expect("insert tree");
            for (key, value) in [(b"a".as_slice(), 7i64), (b"b", -13), (b"c", 42)] {
                db.insert(
                    &[b"root".as_slice()],
                    key,
                    Element::new_sum_item(value),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert sum item");
            }
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("root_hash")
        }

        let pcps_root = root_hash_for(
            Element::empty_provable_count_provable_sum_tree(),
            grove_version,
        );
        let pcst_root = root_hash_for(Element::empty_provable_count_sum_tree(), grove_version);
        let pst_root = root_hash_for(Element::empty_provable_sum_tree(), grove_version);

        assert_ne!(
            pcps_root, pcst_root,
            "ProvableCountProvableSumTree root must differ from ProvableCountSumTree (the count-only \
             flavor) over the same content — this is the point of the new variant: it binds the sum \
             into the hash too"
        );
        assert_ne!(
            pcps_root, pst_root,
            "ProvableCountProvableSumTree root must differ from ProvableSumTree (the sum-only \
             flavor) over the same content — the count is also bound into the hash"
        );
    }

    /// 4. Headline crossover: a single tree produces BOTH a verifiable
    /// count proof AND a verifiable sum proof against the SAME root hash.
    ///
    /// This is what `ProvableCountProvableSumTree` exists for — the count
    /// and sum proof modules both accept the new variant and verify
    /// against the shared root hash. Both emitters dispatch on the host
    /// tree's `TreeType`: for `ProvableCountProvableSumTree`, they emit
    /// dual-axis Node variants (`HashWithCountAndSum`, `KVDigestCountSum`)
    /// so the verifier can reconstruct `node_hash_with_count_and_sum`.
    /// Each proof returns its corresponding aggregate; both verify
    /// against the exact same GroveDB root hash.
    #[test]
    fn pcps_supports_both_count_and_sum_proofs_against_same_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert pcps");

        // Populate with 5 sum items: keys "0".."4" with values 10, 20, 30, 40, 50.
        for i in 0u8..5 {
            db.insert(
                &[b"pcps".as_slice()],
                &[b'0' + i],
                Element::new_sum_item((i as i64 + 1) * 10),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }

        // Tree's full-range aggregate is (count=5, sum=150).
        let parent = db
            .get(&[] as &[&[u8]], b"pcps", None, grove_version)
            .unwrap()
            .expect("get parent");
        let (count, sum) = parent
            .as_provable_count_provable_sum_tree_value()
            .expect("pcps value");
        assert_eq!(count, 5);
        assert_eq!(sum, 150);

        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");

        // === Count proof: aggregate count over the full key range.
        let count_inner_range = QueryItem::Range(b"0".to_vec()..b":".to_vec());
        let count_query = PathQuery::new_unsized(
            vec![b"pcps".to_vec()],
            Query::new_aggregate_count_on_range(count_inner_range),
        );
        let count_proof = db
            .prove_query(&count_query, None, grove_version)
            .unwrap()
            .expect("prove count");
        let (proven_count_root, proven_count) =
            GroveDb::verify_aggregate_count_query(&count_proof, &count_query, grove_version)
                .expect("verify count");
        assert_eq!(
            proven_count_root, root_hash,
            "count proof must verify against the GroveDB root"
        );
        assert_eq!(proven_count, 5);

        // === Sum proof: aggregate sum over the same range.
        let sum_inner_range = QueryItem::Range(b"0".to_vec()..b":".to_vec());
        let sum_query = PathQuery::new_unsized(
            vec![b"pcps".to_vec()],
            Query::new_aggregate_sum_on_range(sum_inner_range),
        );
        let sum_proof = db
            .prove_query(&sum_query, None, grove_version)
            .unwrap()
            .expect("prove sum");
        let (proven_sum_root, proven_sum) =
            GroveDb::verify_aggregate_sum_query(&sum_proof, &sum_query, grove_version)
                .expect("verify sum");
        assert_eq!(
            proven_sum_root, root_hash,
            "sum proof must verify against the SAME GroveDB root — this is the headline contract \
             of ProvableCountProvableSumTree: both proof flavors share one root hash"
        );
        assert_eq!(proven_sum, 150);
    }

    /// 5. Wrapper compatibility: a `NonCounted(ProvableCountProvableSumTree)`
    /// is acceptable inside a count-bearing parent (PCPS is count-bearing,
    /// and `NonCounted` suppresses its count contribution to the parent).
    #[test]
    fn non_counted_pcps_inserts_into_pcps_parent_without_incrementing_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"outer",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer");

        // A non-counted PCPS as inner: count contribution suppressed.
        let nc_inner = Element::new_non_counted(Element::empty_provable_count_provable_sum_tree())
            .expect("wrap NonCounted");
        db.insert(
            &[b"outer".as_slice()],
            b"inner",
            nc_inner,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert non-counted inner pcps");

        // Outer's count should be 0 — the NonCounted wrapper suppresses
        // the implicit +1 from a tree subtree.
        let outer = db
            .get(&[] as &[&[u8]], b"outer", None, grove_version)
            .unwrap()
            .expect("get outer");
        let (count, sum) = outer
            .as_provable_count_provable_sum_tree_value()
            .expect("pcps");
        assert_eq!(
            count, 0,
            "NonCounted wrapper must suppress the inner tree's +1 contribution"
        );
        assert_eq!(sum, 0);
    }

    /// 6. `NotSummed(ProvableCountProvableSumTree)` is insertable in
    /// sum-bearing parents and suppresses sum propagation while counts
    /// still propagate. (PCPS is both count- AND sum-bearing, so it
    /// qualifies as both NotSummed inner and as NotSummed parent.)
    #[test]
    fn not_summed_pcps_inserts_into_pcps_parent_and_zeros_sum_contribution() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"outer",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer");

        // Inner: a not-summed PCPS that itself contains a sum item with
        // value 100. Its own internal sum is 100 but it contributes 0 to
        // the outer's sum.
        let inner = Element::empty_provable_count_provable_sum_tree();
        let ns_inner = Element::new_not_summed(inner).expect("wrap NotSummed");
        db.insert(
            &[b"outer".as_slice()],
            b"inner",
            ns_inner,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert not-summed inner pcps");

        // Add a sum item to the inner.
        db.insert(
            &[b"outer".as_slice(), b"inner".as_slice()],
            b"a",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum item into inner");

        let outer = db
            .get(&[] as &[&[u8]], b"outer", None, grove_version)
            .unwrap()
            .expect("get outer");
        let (outer_count, outer_sum) = outer
            .as_provable_count_provable_sum_tree_value()
            .expect("pcps");
        // The NotSummed-wrapped inner contributes +1 to the parent count
        // (NotSummed only suppresses sum) but 0 to the parent sum.
        assert_eq!(
            outer_count, 1,
            "NotSummed wrapper allows count to propagate"
        );
        assert_eq!(
            outer_sum, 0,
            "NotSummed wrapper suppresses sum propagation to the parent"
        );

        // The inner's own sum still reflects the +100 child.
        let inner_fetched = db
            .get(&[b"outer".as_slice()], b"inner", None, grove_version)
            .unwrap()
            .expect("get inner");
        // Inner is a NotSummed-wrapped tree; underlying() unwraps.
        let inner_unwrapped = inner_fetched.into_underlying();
        let (inner_count, inner_sum) = inner_unwrapped
            .as_provable_count_provable_sum_tree_value()
            .expect("pcps inner");
        assert_eq!(inner_count, 1);
        assert_eq!(inner_sum, 100);
    }

    /// 7. `NotCountedOrSummed(PCPS)` is insertable in PCPS parents and
    /// suppresses BOTH count and sum contributions.
    #[test]
    fn not_counted_or_summed_pcps_inserts_into_pcps_parent_and_zeros_both_axes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"outer",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer");

        let ncos_inner =
            Element::new_not_counted_or_summed(Element::empty_provable_count_provable_sum_tree())
                .expect("wrap NotCountedOrSummed");
        db.insert(
            &[b"outer".as_slice()],
            b"inner",
            ncos_inner,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ncos inner pcps");

        // Add a sum item to the inner. With NCOS wrapper, neither the
        // +1 count nor the +50 sum should propagate.
        db.insert(
            &[b"outer".as_slice(), b"inner".as_slice()],
            b"a",
            Element::new_sum_item(50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum item");

        let outer = db
            .get(&[] as &[&[u8]], b"outer", None, grove_version)
            .unwrap()
            .expect("get outer");
        let (count, sum) = outer
            .as_provable_count_provable_sum_tree_value()
            .expect("pcps");
        assert_eq!(count, 0, "NotCountedOrSummed must suppress count");
        assert_eq!(sum, 0, "NotCountedOrSummed must suppress sum");
    }

    /// 6. References under a `ProvableCountProvableSumTree` parent must
    /// survive the GroveDB proof post-processor's reference rewrite as
    /// `KVRefValueHashCountSum` — carrying BOTH the count and sum
    /// aggregates that the merk-layer node hash committed via
    /// `node_hash_with_count_and_sum`. The previous post-processor only
    /// looked for `ProvableCountedMerkNode` and `ProvableSummedMerkNode`
    /// features, so a PCPS reference's
    /// `ProvableCountedAndProvableSummedMerkNode` feature would fall
    /// through to plain `KVRefValueHash` and drop both axes from the
    /// proof. This test pins that the proof round-trips end-to-end:
    /// generated against a PCPS-host Reference, the verifier
    /// reconstructs the root hash exactly.
    #[test]
    fn pcps_reference_proof_round_trips_against_same_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Container PCPS at root.
        db.insert(
            &[] as &[&[u8]],
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert pcps");

        // Target sum-item in a separate ProvableSumTree branch so the
        // Reference resolution exercises a real dereference (not the
        // identity case).
        db.insert(
            &[] as &[&[u8]],
            b"sums",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sums");
        db.insert(
            &[b"sums".as_slice()],
            b"target",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");

        // Insert a Reference at b"r" under PCPS pointing at b"sums/target".
        // Inside a PCPS parent the proof emit will use
        // `KVRefValueHashCountSum` for this Reference; the post-processor
        // must recognise the
        // `ProvableCountedAndProvableSummedMerkNode(count, sum)` feature
        // and emit the dual-axis ref Node carrying BOTH aggregates.
        db.insert(
            &[b"pcps".as_slice()],
            b"r",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"sums".to_vec(),
                b"target".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference");

        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");

        // Prove the reference. The proof must verify against the same
        // root hash — if the PCPS ref was downgraded to plain
        // `KVRefValueHash`, the reconstructed node_hash_with_count_and_sum
        // wouldn't match and the verifier would surface a root-hash
        // mismatch.
        let mut query = Query::new();
        query.insert_key(b"r".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"pcps".to_vec()], query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove pcps reference");
        let (proven_root, proved) =
            GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
        assert_eq!(
            proven_root, root_hash,
            "PCPS reference proof must verify against the GroveDB root — root mismatch here \
             means the dual-axis ref was downgraded and dropped its hash-bound aggregates"
        );
        // The verified result follows the reference and surfaces the
        // SumItem at the target — so we should see exactly one entry
        // with value=42. Result tuple shape is
        // `(path, key, Option<Element>)`.
        assert_eq!(proved.len(), 1, "expected one result, got {:?}", proved);
        let (_path, _key, resolved) = &proved[0];
        match resolved {
            Some(Element::SumItem(v, _)) => assert_eq!(*v, 42),
            Some(other) => panic!(
                "expected SumItem at the resolved reference, got {:?}",
                other
            ),
            None => panic!("expected resolved value, got absence"),
        }
    }
}
