//! Regression tests: a direct insert must not close a reference cycle by
//! overwriting a position its own chain runs through.
//!
//! Start from `A -> B` with `B` an item. Overwriting `B` with a reference back
//! to `A` used to succeed: the write resolved its chain from the target `A`,
//! reached `B`, and read the *stored* `B` — still the item — so the chain
//! looked acyclic even though the state being committed was `A -> B -> A`.
//! Every later read of either key (`get`, `prove_query`, `verify_grovedb`)
//! then failed with `CyclicReference`: the write was accepted and the state
//! was poisoned. `GroveDb::follow_reference_as_stored` started its `visited`
//! set empty at the target, while the MerkCache follower
//! (`reference_path::follow_reference`) and the batch resolver
//! (`follow_reference_get_value_hash`) already account for the referrer's own
//! position.
//!
//! `GROVE_V3` is live, so the rejection is gated to `GROVE_V4`
//! (`add_element_on_transaction: 2`, whose chain walk is seeded with the
//! position being written); V3 keeps accepting the overwrite, and that legacy
//! outcome is pinned below.

#[cfg(test)]
mod tests {
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::QualifiedGroveDbOp,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, Query, SizedQuery,
    };

    const A: &[u8] = b"A";
    const B: &[u8] = b"B";
    const C: &[u8] = b"C";
    const D: &[u8] = b"D";
    const B_VALUE: &[u8] = b"b";
    const D_VALUE: &[u8] = b"d";

    fn sibling_ref(key: &[u8]) -> Element {
        Element::new_reference(ReferencePathType::SiblingReference(key.to_vec()))
    }

    fn sibling_ref_with_sum(key: &[u8], sum: i64) -> Element {
        Element::new_reference_with_sum_item(ReferencePathType::SiblingReference(key.to_vec()), sum)
    }

    fn insert(
        db: &TempGroveDb,
        key: &[u8],
        element: Element,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
    }

    fn get(db: &TempGroveDb, key: &[u8], grove_version: &GroveVersion) -> Result<Element, Error> {
        db.get([TEST_LEAF].as_ref(), key, None, grove_version)
            .unwrap()
    }

    fn root(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash")
    }

    /// `TEST_LEAF/B` is an item and `TEST_LEAF/A` a sibling reference to it.
    fn db_with_a_to_b(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        insert(&db, B, Element::new_item(B_VALUE.to_vec()), grove_version).expect("insert item B");
        insert(&db, A, sibling_ref(B), grove_version).expect("insert A -> B");
        db
    }

    fn key_query(key: &[u8]) -> PathQuery {
        let mut query = Query::new();
        query.insert_key(key.to_vec());
        PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(query, None, None))
    }

    /// Prove `key` and verify the proof against the live root, returning the
    /// element the verifier surfaced for it.
    fn prove_and_verify(
        db: &TempGroveDb,
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> Option<Element> {
        let path_query = key_query(key);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (proved_root, mut results) =
            GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
        assert_eq!(
            proved_root,
            root(db, grove_version),
            "proof must bind the live root"
        );
        assert_eq!(results.len(), 1, "exactly one row for {key:?}");
        let (_, proved_key, element) = results.remove(0);
        assert_eq!(proved_key, key.to_vec());
        element
    }

    /// Every surface must still show the acyclic `A -> B(item)` state.
    fn assert_a_to_b_intact(db: &TempGroveDb, grove_version: &GroveVersion) {
        let item_b = Element::new_item(B_VALUE.to_vec());
        assert_eq!(
            get(db, A, grove_version).expect("get A"),
            item_b,
            "A must still resolve to the item"
        );
        assert_eq!(
            get(db, B, grove_version).expect("get B"),
            item_b,
            "B must still be the item"
        );
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb must run to completion");
        assert!(
            issues.is_empty(),
            "verify_grovedb must be clean, got: {issues:?}"
        );
        assert_eq!(prove_and_verify(db, A, grove_version), Some(item_b.clone()));
        assert_eq!(prove_and_verify(db, B, grove_version), Some(item_b));
    }

    fn assert_cyclic(err: Error) {
        assert!(
            matches!(err, Error::CyclicReference),
            "expected CyclicReference, got {err:?}"
        );
    }

    #[test]
    fn direct_overwrite_closing_a_two_hop_cycle_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = db_with_a_to_b(grove_version);
        let root_before = root(&db, grove_version);

        let err = insert(&db, B, sibling_ref(A), grove_version)
            .expect_err("overwriting B with a reference back to A closes A -> B -> A");
        assert_cyclic(err);

        assert_eq!(
            root(&db, grove_version),
            root_before,
            "a rejected write must not move the root"
        );
        assert_a_to_b_intact(&db, grove_version);
    }

    /// The written position may sit several hops down the chain:
    /// `C -> A -> B(item)`, then `B := ref C` would close `B -> C -> A -> B`.
    #[test]
    fn direct_overwrite_closing_a_longer_cycle_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = db_with_a_to_b(grove_version);
        insert(&db, C, sibling_ref(A), grove_version).expect("insert C -> A");
        let root_before = root(&db, grove_version);

        let err =
            insert(&db, B, sibling_ref(C), grove_version).expect_err("B -> C -> A -> B is a cycle");
        assert_cyclic(err);

        assert_eq!(root(&db, grove_version), root_before);
        assert_a_to_b_intact(&db, grove_version);
        assert_eq!(
            get(&db, C, grove_version).expect("get C"),
            Element::new_item(B_VALUE.to_vec())
        );
    }

    /// The degenerate one-hop case: a reference to its own key, written over
    /// an item, resolved against the stored item and was accepted.
    #[test]
    fn direct_overwrite_with_a_self_reference_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = db_with_a_to_b(grove_version);
        let root_before = root(&db, grove_version);

        let err = insert(&db, B, sibling_ref(B), grove_version).expect_err("B -> B is a cycle");
        assert_cyclic(err);

        assert_eq!(root(&db, grove_version), root_before);
        assert_a_to_b_intact(&db, grove_version);
    }

    /// `ReferenceWithSumItem` shares the resolution arm with `Reference`.
    #[test]
    fn direct_overwrite_with_a_reference_with_sum_item_closing_a_cycle_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(&db, B, Element::new_item(B_VALUE.to_vec()), grove_version).expect("insert item B");
        insert(&db, A, sibling_ref_with_sum(B, 5), grove_version).expect("insert A -> B");
        let root_before = root(&db, grove_version);

        let err = insert(&db, B, sibling_ref_with_sum(A, 7), grove_version)
            .expect_err("A -> B -> A is a cycle");
        assert_cyclic(err);

        assert_eq!(root(&db, grove_version), root_before);
        assert_a_to_b_intact(&db, grove_version);
    }

    /// Re-pointing a reference is the common case and is not a cycle:
    /// `A -> B(item)`, then `A := ref D(item)` yields `A -> D`.
    #[test]
    fn direct_overwrite_repointing_a_reference_is_accepted() {
        let grove_version = GroveVersion::latest();
        let db = db_with_a_to_b(grove_version);
        insert(&db, D, Element::new_item(D_VALUE.to_vec()), grove_version).expect("insert item D");

        insert(&db, A, sibling_ref(D), grove_version).expect("A -> D does not close a cycle");

        let item_d = Element::new_item(D_VALUE.to_vec());
        assert_eq!(get(&db, A, grove_version).expect("get A"), item_d);
        assert_eq!(
            get(&db, B, grove_version).expect("get B"),
            Element::new_item(B_VALUE.to_vec())
        );
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(prove_and_verify(&db, A, grove_version), Some(item_d));
    }

    /// Overwriting a referenced item with a reference that leads elsewhere
    /// is not a cycle either: `A -> B(item)`, then `B := ref D(item)` yields
    /// `A -> B -> D`, and reads follow the new hop. (`A`'s own commitment is
    /// now stale — it bound the item — and refreshing referrers is the
    /// writer's job, as for any overwritten terminal, so the integrity walk
    /// is deliberately not consulted here.)
    #[test]
    fn direct_overwrite_of_a_referenced_item_with_an_acyclic_reference_is_accepted() {
        let grove_version = GroveVersion::latest();
        let db = db_with_a_to_b(grove_version);
        insert(&db, D, Element::new_item(D_VALUE.to_vec()), grove_version).expect("insert item D");

        insert(&db, B, sibling_ref(D), grove_version).expect("B -> D does not close a cycle");

        let item_d = Element::new_item(D_VALUE.to_vec());
        assert_eq!(get(&db, A, grove_version).expect("get A"), item_d);
        assert_eq!(get(&db, B, grove_version).expect("get B"), item_d);
        assert_eq!(prove_and_verify(&db, B, grove_version), Some(item_d));
    }

    /// A fresh reference joining an existing chain is not a cycle: with
    /// `A -> B(item)`, `D := ref A` yields `D -> A -> B`. The seeded walk
    /// must not refuse a chain merely because it runs through other
    /// references.
    #[test]
    fn direct_insert_of_a_reference_joining_an_existing_chain_is_accepted() {
        let grove_version = GroveVersion::latest();
        let db = db_with_a_to_b(grove_version);

        insert(&db, D, sibling_ref(A), grove_version).expect("D -> A -> B does not close a cycle");

        let item_b = Element::new_item(B_VALUE.to_vec());
        assert_eq!(get(&db, D, grove_version).expect("get D"), item_b);
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(prove_and_verify(&db, D, grove_version), Some(item_b));
        assert_a_to_b_intact(&db, grove_version);
    }

    /// The batch resolver finds the overwriting op in `ops_by_qualified_paths`
    /// and already refuses the cycle; the direct path now agrees with it.
    #[test]
    fn batch_overwrite_closing_the_cycle_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = db_with_a_to_b(grove_version);
        let root_before = root(&db, grove_version);

        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            B.to_vec(),
            sibling_ref(A),
        );
        let err = db
            .apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect_err("batch overwrite closes A -> B -> A");
        assert_cyclic(err);

        assert_eq!(root(&db, grove_version), root_before);
        assert_a_to_b_intact(&db, grove_version);
    }

    /// Consensus pin: `GROVE_V3` is live and its direct insert resolves the
    /// chain from the target only, so the overwrite is still accepted there
    /// and poisons both keys. A V4 node replaying V3 blocks must reproduce
    /// that outcome.
    #[test]
    fn grove_v3_direct_overwrite_still_closes_the_cycle() {
        let grove_version = &GROVE_V3;
        let db = db_with_a_to_b(grove_version);

        insert(&db, B, sibling_ref(A), grove_version)
            .expect("GROVE_V3 must keep accepting the overwrite");

        for key in [A, B] {
            let err = get(&db, key, grove_version).expect_err("poisoned key");
            assert_cyclic(err);
        }
        let err = db
            .verify_grovedb(None, true, false, grove_version)
            .expect_err("the integrity walk hits the cycle");
        assert_cyclic(err);
    }
}
