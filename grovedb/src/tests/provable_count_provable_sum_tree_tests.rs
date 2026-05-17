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
//! 5. Wrapper interactions:
//!    - `NonCounted` / `NotCountedOrSummed` REJECTED inside a PCPS
//!      parent — PCPS commits its count (and sum) into every node
//!      hash, so a suppressed-child wrapper would create a
//!      cryptographically-committed count/sum that disagrees with the
//!      actual element contents. Parallels the rejection rule from
//!      PR #672 for `ProvableCountTree` / `ProvableCountSumTree`.
//!    - `NotSummed` still ACCEPTED in a PCPS parent (consistent with
//!      PR #672 deferring the NotSummed-in-Provable* question).

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

    /// 5a. Wrapper rejection: a `NonCounted(...)` child must NOT be
    /// insertable under a `ProvableCountProvableSumTree` parent. PCPS
    /// commits its aggregate count into every node hash via
    /// `node_hash_with_count_and_sum`, so a `NonCounted` child would
    /// commit a cryptographic count that diverges from the actual
    /// number of stored elements — the same footgun PR #672 closed
    /// for `ProvableCountTree` / `ProvableCountSumTree`. This test
    /// pins the rejection at the GroveDB insert surface.
    #[test]
    fn non_counted_rejected_under_provable_count_provable_sum_tree_parent() {
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
        .expect("insert outer pcps");

        // NonCounted wrapping a sum-bearing tree (a fresh SumTree) —
        // the wrapper is well-formed at construction; what we're
        // testing is the *parent's* rejection at insert. Use SumTree
        // as the inner since NonCounted accepts any tree variant
        // structurally.
        let nc_inner =
            Element::new_non_counted(Element::empty_sum_tree()).expect("wrap NonCounted");
        let result = db
            .insert(
                &[b"outer".as_slice()],
                b"inner",
                nc_inner,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "NonCounted must be rejected under a ProvableCountProvableSumTree parent — got Ok"
        );
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

    /// 5b. Wrapper rejection: a `NotCountedOrSummed(...)` child must NOT
    /// be insertable under a `ProvableCountProvableSumTree` parent.
    /// PCPS commits BOTH its aggregate count and its aggregate sum
    /// into every node hash via `node_hash_with_count_and_sum`; a
    /// `NotCountedOrSummed` child would commit cryptographic
    /// aggregates on both axes that diverge from the actual element
    /// contents. Parallels PR #672's rejection rule for
    /// `ProvableCountSumTree`.
    #[test]
    fn not_counted_or_summed_rejected_under_provable_count_provable_sum_tree_parent() {
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
        .expect("insert outer pcps");

        // NotCountedOrSummed wrapping a sum-bearing tree (a fresh
        // SumTree, the simplest acceptable inner). The wrapper is
        // well-formed at construction; what we're testing is the
        // *parent's* rejection at insert.
        let ncos_inner = Element::new_not_counted_or_summed(Element::empty_sum_tree())
            .expect("wrap NotCountedOrSummed");
        let result = db
            .insert(
                &[b"outer".as_slice()],
                b"inner",
                ncos_inner,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "NotCountedOrSummed must be rejected under a ProvableCountProvableSumTree parent — \
             got Ok"
        );
    }

    /// Shared body of the PCPS reference proof round-trip tests
    /// below. Parametrized on grove version so we exercise both the
    /// v1 ref-rewrite loop (`GroveVersion::latest()`) and the v0
    /// ref-rewrite loop (`GROVE_V2`). Both loops have the same defect
    /// fixed in this PR — without the `KVRefValueHashCountSum`
    /// dispatch arm, a PCPS Reference proof would surface a
    /// "lower layer hash" mismatch at the verifier.
    fn pcps_reference_proof_round_trip_with(grove_version: &GroveVersion) {
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

    /// V1 dispatch (the latest grove version) — exercises the v1
    /// ref-rewrite loop's `KVRefValueHashCountSum` arm.
    #[test]
    fn pcps_reference_proof_round_trips_against_same_root() {
        pcps_reference_proof_round_trip_with(GroveVersion::latest());
    }

    /// V0 dispatch (`GROVE_V2`) MUST REJECT a PCPS-rooted proof.
    /// V0 proofs are LOCKED to the wire format shipped with grove
    /// v1/v2; `ProvableCountProvableSumTree` (PCPS) was added after
    /// the V0 envelope shipped and needs dual-axis Node variants
    /// (`KVRefValueHashCountSum`, `HashWithCountAndSum`, etc.) that
    /// the V0 post-processor doesn't emit. The V0 entry point in
    /// `prove_subqueries` rejects a PCPS-rooted leaf merk at
    /// dispatch time with `Error::NotSupported`; PCPS users must
    /// produce proofs via V1 (grove v3+).
    ///
    /// This test pins the rejection so we don't accidentally re-add
    /// V0 PCPS support (which would mean modifying the V0 prover —
    /// a violation of the V0-locked contract).
    /// Batch operation exercising the PCPS arms in
    /// `grovedb/src/batch/mod.rs`: the `LayeredValueDefinedCost`
    /// flag-update closure and the `InsertTreeWithRootHash` propagation
    /// branch both gained `Element::ProvableCountProvableSumTree` arms
    /// in this PR. This test inserts a PCPS subtree + child items in
    /// a single batch — the propagation step converts the original
    /// PCPS insert op into an `InsertTreeWithRootHash`, which triggers
    /// the new arm at `batch/mod.rs:3264`.
    ///
    /// Asserts the batch applies cleanly and the resulting PCPS
    /// aggregate reflects the children's count and sum.
    #[test]
    fn pcps_batch_apply_propagates_aggregate() {
        use crate::{batch::QualifiedGroveDbOp, tests::TEST_LEAF};
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"pcps".to_vec(),
                Element::empty_provable_count_provable_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"pcps".to_vec()],
                b"a".to_vec(),
                Element::new_sum_item(10),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"pcps".to_vec()],
                b"b".to_vec(),
                Element::new_sum_item(20),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"pcps".to_vec()],
                b"c".to_vec(),
                Element::new_sum_item(30),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch apply on PCPS host");

        // Verify aggregates propagated correctly: count = 3 children,
        // sum = 10 + 20 + 30 = 60.
        let parent = db
            .get(&[TEST_LEAF], b"pcps", None, grove_version)
            .unwrap()
            .expect("get parent PCPS");
        let (count, sum) = parent
            .as_provable_count_provable_sum_tree_value()
            .expect("pcps value");
        assert_eq!(count, 3, "PCPS count after batch must reflect 3 children");
        assert_eq!(sum, 60, "PCPS sum after batch must be 10 + 20 + 30 = 60");
    }

    #[test]
    fn pcps_proof_rejected_on_v0_envelope() {
        use grovedb_version::version::v2::GROVE_V2;
        let grove_version: &GroveVersion = &GROVE_V2;

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
        db.insert(
            &[b"pcps".as_slice()],
            b"a",
            Element::new_item(vec![1, 2, 3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let mut q = Query::new();
        q.insert_key(b"a".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"pcps".to_vec()], q);
        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        let err = result.expect_err("V0 envelope must refuse PCPS proofs");
        assert!(
            matches!(err, crate::Error::NotSupported(ref msg) if msg.contains("V1 proof envelopes")),
            "expected NotSupported with V1-envelope message; got {:?}",
            err
        );
    }

    /// Regression for the GroveDB post-processing loop fix: a regular
    /// `prove_query` on a PCPS host with Item children must:
    ///   1. Emit each Item as `Node::KVCountSum` (committed via
    ///      `proof_node_type` for the dual-axis host).
    ///   2. In the GroveDB post-processing loop, preserve the
    ///      `KVCountSum` node type (do not rewrite to `Node::KV`)
    ///      so the dual-axis count+sum stay hash-bound.
    ///   3. Decrement `overall_limit` and set `has_a_result_at_level`
    ///      for each matched PCPS Item — same as `KVCount` / `KVSum`
    ///      for the single-axis hosts.
    ///
    /// Before fix: the GroveDB post-processing loop only matched
    /// `KV | KVValueHash | KVCount | KVSum | KVValueHashFeatureType`
    /// in its Item-class arm and `KVCount | KVSum |
    /// KVValueHashFeatureType` in its `should_preserve_node_type`
    /// allowlist. A PCPS Item arriving as `Node::KVCountSum` would
    /// hash-verify but skip the Item-class branch via the loop's
    /// `_ => continue` fall-through — so `overall_limit` wouldn't
    /// decrement and `has_a_result_at_level` wouldn't be set.
    ///
    /// Smoke check (single-layer): the proof round-trips and the
    /// hash chain stays intact. The over-prove behavior is bounded
    /// by the merk-level limit, so a single-layer query is robust
    /// against this bug — but the `has_a_result_at_level` failure
    /// mode below exposes the real harm.
    #[test]
    fn pcps_regular_query_with_limit_round_trips() {
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
        for c in b'a'..=b'e' {
            db.insert(
                &[b"pcps".as_slice()],
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
        }
        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");

        let mut query = Query::new();
        query.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let path_query = PathQuery::new(
            vec![b"pcps".to_vec()],
            crate::SizedQuery::new(query, Some(2), None),
        );
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (proven_root, proved) =
            GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
        assert_eq!(proven_root, root_hash);
        assert_eq!(proved.len(), 2);
        let keys: Vec<Vec<u8>> = proved.iter().map(|(_p, k, _v)| k.clone()).collect();
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);
    }

    /// Regression for the `has_a_result_at_level` half of the
    /// post-processing fix: a **multi-layer** query whose **subquery**
    /// targets a PCPS host with Items must surface the PCPS Items in
    /// the final result set.
    ///
    /// Before fix: the outer post-processing loop iterates over the
    /// PCPS layer's merk_proof.proof, sees `Node::KVCountSum` ops
    /// for the PCPS Items, doesn't match any Item-class arm
    /// (`KV | KVValueHash | KVCount | KVSum | KVValueHashFeatureType`),
    /// falls through to the `_ => continue` arm. `overall_limit`
    /// doesn't decrement and — critically for multi-layer queries —
    /// `has_a_result_at_level` doesn't get set. The outer layer
    /// records the PCPS layer as if it returned nothing, even though
    /// the merk-level proof contains real items. End-to-end verify
    /// then sees zero results from the PCPS subtree.
    ///
    /// This test stages a 2-layer query (outer Tree → PCPS subquery)
    /// and asserts the inner PCPS Items actually surface in the
    /// verified result set. Without the fix, the assertion on
    /// `proved.len() > 0` fails (the PCPS layer is silently pruned).
    #[test]
    fn pcps_subquery_items_surface_in_verified_result() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Outer container: a plain Tree at root key "outer".
        db.insert(
            &[] as &[&[u8]],
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer tree");
        // PCPS host as a child of the outer Tree at "outer/pcps".
        db.insert(
            &[b"outer".as_slice()],
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert pcps under outer");
        // PCPS-host Items: each emits as Node::KVCountSum via the
        // dual-axis proof_node_type dispatch.
        for c in b'a'..=b'c' {
            db.insert(
                &[b"outer".as_slice(), b"pcps".as_slice()],
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert pcps item");
        }
        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");

        // 2-layer query: outer path matches the "pcps" key, with a
        // subquery that ranges over all Items inside the PCPS host.
        let mut subquery = Query::new();
        subquery.insert_range_inclusive(b"a".to_vec()..=b"c".to_vec());
        let mut outer_query = Query::new();
        outer_query.insert_key(b"pcps".to_vec());
        outer_query.default_subquery_branch.subquery = Some(Box::new(subquery));

        let path_query = PathQuery::new_unsized(vec![b"outer".to_vec()], outer_query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove 2-layer");
        let (proven_root, proved) =
            GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
        assert_eq!(
            proven_root, root_hash,
            "multi-layer PCPS-subquery proof must verify against GroveDB root"
        );
        assert_eq!(
            proved.len(),
            3,
            "PCPS subquery items must surface in the verified result set. \
             Without the post-processing fix the outer loop sees KVCountSum ops, \
             falls into the `_ => continue` arm, doesn't set has_a_result_at_level, \
             and the PCPS layer gets silently pruned. Got {} items.",
            proved.len(),
        );
        let keys: Vec<Vec<u8>> = proved.iter().map(|(_p, k, _v)| k.clone()).collect();
        assert_eq!(
            keys,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
            "all three PCPS Items must surface in sorted order; got {:?}",
            keys
        );
    }

    /// The new `AggregateCountAndSumOnRange` (combined) variant
    /// returns BOTH the count AND the signed sum from a SINGLE proof
    /// against a PCPS host, in contrast to
    /// `pcps_supports_both_count_and_sum_proofs_against_same_root`
    /// which runs two separate proofs to get the same numbers. Both
    /// counts AND sums must match `pcps_supports_both_*`'s values.
    #[test]
    fn pcps_combined_count_and_sum_proof_returns_both_axes_from_one_proof() {
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

        // Same fixture as the separate-proofs test: keys "0".."4"
        // with values 10, 20, 30, 40, 50. count = 5, sum = 150.
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

        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");

        // ONE combined-aggregate query produces BOTH count and sum.
        let inner_range = QueryItem::Range(b"0".to_vec()..b":".to_vec());
        let combined_query = PathQuery::new_unsized(
            vec![b"pcps".to_vec()],
            Query::new_aggregate_count_and_sum_on_range(inner_range),
        );
        let combined_proof = db
            .prove_query(&combined_query, None, grove_version)
            .unwrap()
            .expect("prove combined");
        let (proven_root, proven_count, proven_sum) =
            GroveDb::verify_aggregate_count_and_sum_query(
                &combined_proof,
                &combined_query,
                grove_version,
            )
            .expect("verify combined");

        assert_eq!(
            proven_root, root_hash,
            "combined-aggregate proof must verify against the GroveDB root"
        );
        assert_eq!(
            proven_count, 5,
            "combined count must match the 5-key fixture"
        );
        assert_eq!(
            proven_sum, 150,
            "combined sum must match the 5-key fixture (10+20+30+40+50)"
        );
    }

    /// Combined-aggregate queries are PCPS-only: the merk-level prover
    /// rejects every other count-bearing tree type with
    /// `InvalidProofError`. This pins the rejection at the GroveDB
    /// envelope (the rejection path bubbles up as `MerkError(...)`
    /// wrapping `InvalidProofError`).
    #[test]
    fn combined_aggregate_query_rejected_on_provable_count_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Host is ProvableCountSumTree (NOT PCPS) — single-axis,
        // commits only count into the node hash.
        db.insert(
            &[] as &[&[u8]],
            b"pcst",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert pcst");
        db.insert(
            &[b"pcst".as_slice()],
            b"a",
            Element::new_sum_item(1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        let inner_range = QueryItem::Range(b"0".to_vec()..b":".to_vec());
        let pq = PathQuery::new_unsized(
            vec![b"pcst".to_vec()],
            Query::new_aggregate_count_and_sum_on_range(inner_range),
        );
        let res = db.prove_query(&pq, None, grove_version).unwrap();
        let err = res.expect_err(
            "combined-aggregate proof on a non-PCPS host must fail at the merk-level prover",
        );
        // The merk-level rejection bubbles up wrapped in
        // CorruptedData (from the `.map_err` wrapping in the v1
        // dispatcher). Accept either CorruptedData containing the
        // PCPS phrase or any error whose Debug repr contains it.
        let s = format!("{:?}", err);
        assert!(
            s.contains("ProvableCountProvableSumTree"),
            "expected PCPS-only error, got: {}",
            s
        );
    }

    /// V0 envelopes predate the combined-aggregate feature: prove on
    /// `GROVE_V2` (which selects `prove_query_non_serialized: 0`)
    /// returns `NotSupported` with the V1-envelope message.
    #[test]
    fn combined_aggregate_query_rejected_on_v0_envelope() {
        use grovedb_version::version::v2::GROVE_V2;
        let grove_version: &GroveVersion = &GROVE_V2;
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

        let inner_range = QueryItem::Range(b"0".to_vec()..b":".to_vec());
        let pq = PathQuery::new_unsized(
            vec![b"pcps".to_vec()],
            Query::new_aggregate_count_and_sum_on_range(inner_range),
        );
        let res = db.prove_query(&pq, None, grove_version).unwrap();
        let err = res.expect_err("V0 envelope must refuse combined-aggregate proofs");
        assert!(
            matches!(err, crate::Error::NotSupported(ref msg) if msg.contains("V1 proof envelopes")),
            "expected NotSupported with V1-envelope message; got {:?}",
            err
        );
    }
}
