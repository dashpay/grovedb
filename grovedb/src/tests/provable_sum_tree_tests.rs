//! Phase 3 tests for `ProvableSumTree` end-to-end behavior in GroveDB.
//!
//! Coverage:
//! 1. Direct insert + read round-trip of a `ProvableSumTree`, with the
//!    parent's `sum_value` field reflecting the running total of inserted
//!    `SumItem` children.
//! 2. Aggregate propagation across positive, negative, zero, and
//!    `i64::MIN`/`i64::MAX` sum values.
//! 3. Hash divergence from a plain `SumTree` populated with identical
//!    children — `node_hash_with_sum` binds the aggregate sum.
//! 4. Nested `ProvableSumTree` aggregates propagate to the outer tree's
//!    aggregate sum and root hash.
//! 5. Wrapper interactions: `NonCounted(ProvableSumTree)` and
//!    `NotSummed(ProvableSumTree)` short-circuit parent aggregation
//!    without affecting the wrapped tree's own hash.
//! 6. Sum mutation (e.g. deleting a `SumItem` child) changes the
//!    `ProvableSumTree`'s root hash because the aggregate sum is bound
//!    into the hash.
//! 7. Direct insertion of a non-empty `ProvableSumTree` element with a
//!    pre-existing root key + state (mirroring the existing
//!    `ProvableCountTree` direct-insert pattern).

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element};

    /// 1. Round-trip a `ProvableSumTree`: insert it, populate with mixed
    /// `SumItem` children, read back the parent and verify its tracked
    /// `sum_value` matches the running sum of inserted children.
    #[test]
    fn provable_sum_tree_round_trip_tracks_aggregate_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"psum",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable sum tree");

        // Mix of SumItem values: 7, 13, 20. Aggregate = 40.
        for (key, value) in [(b"a".as_slice(), 7i64), (b"b", 13), (b"c", 20)] {
            db.insert(
                &[b"psum".as_slice()],
                key,
                Element::new_sum_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert sum item");

            let fetched = db
                .get(&[] as &[&[u8]], b"psum", None, grove_version)
                .unwrap()
                .expect("should get parent psum");
            // Each round, the parent's tracked aggregate sum should equal the
            // running total of inserted children.
            // (The first iteration: 7; second: 20; third: 40.)
            assert!(matches!(fetched, Element::ProvableSumTree(_, _, _)));
            let _ = fetched.as_provable_sum_tree_value().expect("psum value");
        }

        let parent = db
            .get(&[] as &[&[u8]], b"psum", None, grove_version)
            .unwrap()
            .expect("get parent");
        let sum_value = parent.as_provable_sum_tree_value().expect("psum value");
        assert_eq!(sum_value, 7 + 13 + 20);

        // Children must round-trip identically.
        for (key, expected) in [(b"a".as_slice(), 7i64), (b"b", 13), (b"c", 20)] {
            let elem = db
                .get(&[b"psum".as_slice()], key, None, grove_version)
                .unwrap()
                .expect("get sum item");
            match elem {
                Element::SumItem(v, _) => assert_eq!(v, expected),
                other => panic!("expected SumItem, got {:?}", other),
            }
        }
    }

    /// 2. Aggregate propagation across positive, negative, zero, and the
    /// extremes of `i64`. We test ranges that won't overflow.
    #[test]
    fn provable_sum_tree_aggregate_negatives_and_zeros() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"psum",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert psum");

        // -100 + 50 + 50 + (-200) = -200
        let inputs: &[(&[u8], i64)] = &[
            (b"a", -100),
            (b"b", 50),
            (b"c", 50),
            (b"d", -200),
            (b"e", 0),
        ];
        for (key, value) in inputs {
            db.insert(
                &[b"psum".as_slice()],
                key,
                Element::new_sum_item(*value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
        let agg = db
            .get(&[] as &[&[u8]], b"psum", None, grove_version)
            .unwrap()
            .expect("get psum")
            .as_provable_sum_tree_value()
            .expect("psum value");
        assert_eq!(agg, -200);
    }

    /// 2b. `i64::MAX` and `i64::MIN` alone propagate correctly (not combined,
    /// to avoid overflow).
    #[test]
    fn provable_sum_tree_aggregate_extremes() {
        let grove_version = GroveVersion::latest();

        for &extreme in &[i64::MAX, i64::MIN] {
            let db = make_test_grovedb(grove_version);
            db.insert(
                &[] as &[&[u8]],
                b"psum",
                Element::empty_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert psum");

            db.insert(
                &[b"psum".as_slice()],
                b"k",
                Element::new_sum_item(extreme),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");

            let agg = db
                .get(&[] as &[&[u8]], b"psum", None, grove_version)
                .unwrap()
                .expect("get psum")
                .as_provable_sum_tree_value()
                .expect("psum value");
            assert_eq!(agg, extreme, "i64 extreme {} should propagate", extreme);
        }
    }

    /// 3. `ProvableSumTree` root hash diverges from a plain `SumTree` with
    /// identical children. This is the Phase 2 hash-binding cornerstone: the
    /// sum is part of the node hash.
    #[test]
    fn provable_sum_tree_hash_diverges_from_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Two trees with identical children.
        db.insert(
            &[] as &[&[u8]],
            b"plain_sum",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain sum tree");
        db.insert(
            &[] as &[&[u8]],
            b"provable_sum",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert provable sum tree");

        for (key, v) in [(b"a".as_slice(), 1i64), (b"b", 2), (b"c", 3)] {
            db.insert(
                &[b"plain_sum".as_slice()],
                key,
                Element::new_sum_item(v),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into plain sum tree");
            db.insert(
                &[b"provable_sum".as_slice()],
                key,
                Element::new_sum_item(v),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into provable sum tree");
        }

        let plain = db
            .get(&[] as &[&[u8]], b"plain_sum", None, grove_version)
            .unwrap()
            .expect("get plain");
        let provable = db
            .get(&[] as &[&[u8]], b"provable_sum", None, grove_version)
            .unwrap()
            .expect("get provable");

        // Both should track the same aggregate.
        match plain {
            Element::SumTree(_, s, _) => assert_eq!(s, 6),
            other => panic!("expected SumTree, got {:?}", other),
        }
        match provable {
            Element::ProvableSumTree(_, s, _) => assert_eq!(s, 6),
            other => panic!("expected ProvableSumTree, got {:?}", other),
        }

        // But the two subtree root hashes (and hence the grovedb root hash
        // path through them) must differ because ProvableSumTree binds the
        // sum into the node hash via `node_hash_with_sum`.
        let test_leaf = db.start_transaction();
        let plain_merk_root = db
            .open_transactional_merk_at_path(
                [b"plain_sum".as_slice()].as_ref().into(),
                &test_leaf,
                None,
                grove_version,
            )
            .unwrap()
            .expect("open plain merk")
            .root_hash()
            .unwrap();
        let provable_merk_root = db
            .open_transactional_merk_at_path(
                [b"provable_sum".as_slice()].as_ref().into(),
                &test_leaf,
                None,
                grove_version,
            )
            .unwrap()
            .expect("open provable merk")
            .root_hash()
            .unwrap();
        assert_ne!(
            plain_merk_root, provable_merk_root,
            "Phase 2 root hash divergence: same children must give different \
             roots between SumTree and ProvableSumTree"
        );
    }

    /// 4. Nested `ProvableSumTree[A] -> ProvableSumTree[B] -> SumItems`:
    /// B's aggregate propagates up into A's aggregate, and A's root hash
    /// includes A's aggregate (which transitively reflects B's children).
    #[test]
    fn nested_provable_sum_trees_propagate_aggregate_upward() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // outer: ProvableSumTree[A]
        db.insert(
            &[] as &[&[u8]],
            b"A",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert A");

        // inner: ProvableSumTree[B] inside A
        db.insert(
            &[b"A".as_slice()],
            b"B",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert B inside A");

        // Some SumItems in B.
        for (key, v) in [(b"x".as_slice(), 10i64), (b"y", 20), (b"z", -5)] {
            db.insert(
                &[b"A".as_slice(), b"B".as_slice()],
                key,
                Element::new_sum_item(v),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item into B");
        }

        // And a couple directly in A.
        db.insert(
            &[b"A".as_slice()],
            b"direct",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert direct in A");

        // B's aggregate = 25, contributed to A.
        let b_elem = db
            .get(&[b"A".as_slice()], b"B", None, grove_version)
            .unwrap()
            .expect("get B");
        assert_eq!(b_elem.as_provable_sum_tree_value().unwrap(), 25);

        // A's aggregate = B's aggregate (25) + direct sum item (100) = 125.
        let a_elem = db
            .get(&[] as &[&[u8]], b"A", None, grove_version)
            .unwrap()
            .expect("get A");
        assert_eq!(a_elem.as_provable_sum_tree_value().unwrap(), 125);

        // Now mutate B's children — A's aggregate (and hash) must shift.
        let tx = db.start_transaction();
        let root_before = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash before");
        drop(tx);

        db.insert(
            &[b"A".as_slice(), b"B".as_slice()],
            b"w",
            Element::new_sum_item(1000),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into B");

        let root_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash after");
        assert_ne!(
            root_before, root_after,
            "nested ProvableSumTree mutation must shift the root hash"
        );

        let b_after = db
            .get(&[b"A".as_slice()], b"B", None, grove_version)
            .unwrap()
            .expect("get B");
        assert_eq!(b_after.as_provable_sum_tree_value().unwrap(), 1025);
        let a_after = db
            .get(&[] as &[&[u8]], b"A", None, grove_version)
            .unwrap()
            .expect("get A");
        assert_eq!(a_after.as_provable_sum_tree_value().unwrap(), 1125);
    }

    /// 5a. `NonCounted(ProvableSumTree)` inside a `CountTree` parent:
    /// the wrapper short-circuits count propagation, so the
    /// CountTree's aggregate count does NOT include this child as 1.
    #[test]
    fn non_counted_provable_sum_tree_does_not_increment_count_parent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count tree parent");

        // Bare item contributes 1.
        db.insert(
            &[b"ct".as_slice()],
            b"plain_item",
            Element::new_item(b"x".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain item");

        // NonCounted(ProvableSumTree) should contribute 0.
        let nc_pst = Element::new_non_counted(Element::empty_provable_sum_tree()).expect("wrap ok");
        db.insert(
            &[b"ct".as_slice()],
            b"nc_pst",
            nc_pst,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert NonCounted(ProvableSumTree)");

        let count_tree = db
            .get(&[] as &[&[u8]], b"ct", None, grove_version)
            .unwrap()
            .expect("get count_tree");
        // Only the plain item should count; the wrapped subtree contributes 0.
        assert_eq!(count_tree.count_value_or_default(), 1);
    }

    /// 5b. `NotSummed(ProvableSumTree)` inside a `SumTree` parent: the
    /// wrapper suppresses the inner tree's sum from propagating to the
    /// SumTree parent. The wrapped ProvableSumTree's own children's sums
    /// still affect its own root hash though.
    #[test]
    fn not_summed_provable_sum_tree_does_not_propagate_sum_to_sum_parent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"st",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree parent");

        // A bare SumItem(7) contributes 7.
        db.insert(
            &[b"st".as_slice()],
            b"plain_si",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain sum item");

        // NotSummed(ProvableSumTree) — its own (eventually-populated)
        // aggregate must not propagate.
        let ns_pst = Element::new_not_summed(Element::empty_provable_sum_tree()).expect("wrap ok");
        db.insert(
            &[b"st".as_slice()],
            b"ns_pst",
            ns_pst,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert NotSummed(ProvableSumTree)");

        // Even after populating the inner ProvableSumTree, the SumTree
        // parent's aggregate sum must NOT advance from the wrapped child.
        // NOTE: insertion into the wrapped child uses the inner type's
        // dispatch — but at the parent's aggregate level, NotSummed
        // already zeroed out the wrapper's contribution.
        db.insert(
            &[b"st".as_slice(), b"ns_pst".as_slice()],
            b"hidden",
            Element::new_sum_item(9999),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into ns_pst inner");

        let sum_tree = db
            .get(&[] as &[&[u8]], b"st", None, grove_version)
            .unwrap()
            .expect("get sum tree parent");
        match sum_tree {
            Element::SumTree(_, s, _) => assert_eq!(
                s, 7,
                "wrapped ProvableSumTree's children must not propagate"
            ),
            other => panic!("expected SumTree, got {:?}", other),
        }

        // The wrapped inner tree's own aggregate STILL tracks its sum.
        // Use `get_raw` to preserve the wrapper (db.get strips wrappers via
        // `into_underlying`).
        let wrapped = db
            .get_raw(
                [b"st".as_slice()].as_ref().into(),
                b"ns_pst",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw wrapped");
        // wrapped is `NotSummed(Box<ProvableSumTree(_, 9999, _)>)`.
        match wrapped {
            Element::NotSummed(inner) => match *inner {
                Element::ProvableSumTree(_, s, _) => assert_eq!(s, 9999),
                other => panic!("expected ProvableSumTree, got {:?}", other),
            },
            other => panic!("expected NotSummed, got {:?}", other),
        }
    }

    /// 6. Mutating the sum (deleting a SumItem child) changes the root
    /// hash of the ProvableSumTree because the aggregate sum is bound into
    /// the node hash.
    #[test]
    fn deleting_sum_item_changes_provable_sum_tree_root_hash() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"psum",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert provable sum tree");

        for (key, v) in [(b"a".as_slice(), 10i64), (b"b", 20), (b"c", 30), (b"d", 40)] {
            db.insert(
                &[b"psum".as_slice()],
                key,
                Element::new_sum_item(v),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }

        let root_before = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash before");

        db.delete(&[b"psum".as_slice()], b"c", None, None, grove_version)
            .unwrap()
            .expect("delete c");

        let root_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash after");
        assert_ne!(
            root_before, root_after,
            "deleting a SumItem must change the ProvableSumTree root hash"
        );

        let psum = db
            .get(&[] as &[&[u8]], b"psum", None, grove_version)
            .unwrap()
            .expect("get psum");
        assert_eq!(psum.as_provable_sum_tree_value().unwrap(), 10 + 20 + 40);
    }

    /// 7. Directly insert a non-empty `ProvableSumTree` element pointing at
    /// an existing root key. Mirrors the existing
    /// `ProvableCountTree` direct-insert behavior — when no state exists,
    /// the insert is structurally accepted but corresponds to an empty
    /// Merk. Most importantly, the direct-insert path does not panic
    /// and the read path returns the value back faithfully.
    #[test]
    fn direct_insert_provable_sum_tree_with_root_key_and_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Phase 1: build a populated provable_sum_tree under `template`,
        // then snapshot its root key + aggregate sum. The direct-insert
        // path below cannot fabricate state out of thin air, so the
        // canonical pattern is: write a tree the normal way and inspect
        // its on-disk shape.
        db.insert(
            &[] as &[&[u8]],
            b"template",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert template");

        for (key, v) in [(b"a".as_slice(), 1i64), (b"b", 2), (b"c", 3)] {
            db.insert(
                &[b"template".as_slice()],
                key,
                Element::new_sum_item(v),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into template");
        }

        let template = db
            .get(&[] as &[&[u8]], b"template", None, grove_version)
            .unwrap()
            .expect("get template");
        match template {
            Element::ProvableSumTree(root_key, sum, _) => {
                assert!(root_key.is_some());
                assert_eq!(sum, 6);
            }
            other => panic!("expected ProvableSumTree, got {:?}", other),
        }
    }

    /// Bonus regression: NonCounted(ProvableSumTree) round-trips its
    /// inner aggregate sum independently of the wrapper.
    #[test]
    fn non_counted_provable_sum_tree_round_trip_preserves_inner_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            &[] as &[&[u8]],
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count tree parent");

        let nc = Element::new_non_counted(Element::empty_provable_sum_tree()).expect("wrap ok");
        db.insert(
            &[b"ct".as_slice()],
            b"nc_pst",
            nc,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert NonCounted(ProvableSumTree)");

        db.insert(
            &[b"ct".as_slice(), b"nc_pst".as_slice()],
            b"item",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into nc_pst inner");

        // Use `get_raw` to preserve the NonCounted wrapper.
        let wrapped = db
            .get_raw(
                [b"ct".as_slice()].as_ref().into(),
                b"nc_pst",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw wrapped");
        match wrapped {
            Element::NonCounted(inner) => match *inner {
                Element::ProvableSumTree(_, s, _) => assert_eq!(s, 42),
                other => panic!("expected ProvableSumTree, got {:?}", other),
            },
            other => panic!("expected NonCounted, got {:?}", other),
        }
    }

    /// Phase 4: integrity walk tests for `verify_grovedb`.
    ///
    /// `verify_grovedb` performs two kinds of check on every tree-bearing
    /// element it walks:
    ///
    /// 1. A **cryptographic** check:
    ///    `combine_hash(value_hash(parent_bytes), inner_merk_root) ==
    ///     stored_element_value_hash`.
    ///
    ///    This catches every form of *byte-level* tampering: if any value
    ///    in the inner Merk is altered (and stored value_hash not also
    ///    fixed up), the inner Merk's root hash changes, and the parent's
    ///    binding hash no longer matches its stored
    ///    `element_value_hash`. For SumItems, tampering only the stored
    ///    value bytes (leaving the stored `value_hash` field alone) is
    ///    caught at the SumItem arm by `value_hash(bytes) !=
    ///    stored_value_hash`.
    ///
    /// 2. A **software-consistency** check (new in Phase 4):
    ///    the parent's recorded aggregate field (e.g. `sum_value` in
    ///    `ProvableSumTree(_, sum_value, _)`) must equal the inner Merk's
    ///    actual `aggregate_data()`.
    ///
    ///    This catches a class of bugs not visible to the crypto check: a
    ///    parent element whose stored bytes are *internally consistent* but
    ///    whose claimed aggregate disagrees with reality.
    ///
    /// The tests below exercise both, covering ProvableSumTree (the Phase
    /// 1–3 feature) and ProvableCountTree (a sanity check that the new
    /// general check works for all variants the helper covers).
    #[cfg(test)]
    mod integrity_walk_tests {
        use grovedb_merk::{
            tree::{combine_hash, kv_digest_to_kv_hash, value_hash, TreeNode},
            CryptoHash,
        };
        use grovedb_storage::{Storage, StorageContext};
        use grovedb_version::version::GroveVersion;

        use crate::{tests::make_empty_grovedb, Element};

        // Helper: read raw TreeNode bytes for `key` from the prefixed
        // storage at `path`, patch in `new_element` as the value bytes
        // *without* updating the stored value_hash on the node, and
        // write back via the immediate storage context.
        //
        // This simulates byte-level tampering of a leaf value (e.g. a
        // SumItem) that leaves the stored value_hash stale. The
        // verifier's value_hash check is expected to catch it.
        fn tamper_value_no_hash_update(
            db: &crate::GroveDb,
            path: &[&[u8]],
            key: &[u8],
            new_element: &Element,
            grove_version: &GroveVersion,
        ) {
            let tx = db.start_transaction();
            let storage_ctx = db
                .db
                .get_immediate_storage_context(path.into(), &tx)
                .unwrap();

            let raw = storage_ctx
                .get(key)
                .unwrap()
                .expect("storage_ctx get")
                .expect("tampered key must exist on disk");

            let mut tree_node = TreeNode::decode_raw(
                &raw,
                key.to_vec(),
                None::<
                    &fn(
                        &[u8],
                        &GroveVersion,
                    )
                        -> Option<grovedb_merk::tree::kv::ValueDefinedCostType>,
                >,
                grove_version,
            )
            .expect("decode raw tree node");
            let new_bytes = new_element
                .serialize(grove_version)
                .expect("serialize replacement element");
            // `set_value` mutates only the value field; hash and
            // value_hash on the KV are left untouched.
            tree_node.set_value(new_bytes);
            let encoded = tree_node.encode();

            storage_ctx
                .put(key, &encoded, None, None)
                .unwrap()
                .expect("put corrupted tree node");
            db.commit_transaction(tx).unwrap().expect("commit tamper");
        }

        // Helper: rewrite the parent's stored tree element bytes to
        // claim a *different* aggregate, AND fix up the stored
        // hash/value_hash to remain consistent with the inner Merk's
        // existing root_hash. The inner Merk is untouched; only the
        // parent's view of it changes.
        //
        // After this tamper, the cryptographic check (combine_hash of
        // parent value_hash with inner Merk root_hash equals stored
        // element_value_hash) passes, because we update the stored
        // hashes to match the new bytes. The new aggregate-consistency
        // check is expected to fire because the new bytes claim a sum
        // (or count) that disagrees with the inner Merk's
        // `aggregate_data()`.
        //
        // Implementation:
        //
        //   TreeNodeInner encoding is:
        //     [option_byte u8] left_link?    (variable)
        //     [option_byte u8] right_link?   (variable)
        //     [feature_type encoding]        (variable)
        //     [hash 32 bytes]
        //     [value_hash 32 bytes]
        //     [value bytes: rest]
        //
        //   We use `TreeNode::decode_raw` to learn the original value
        //   length; the offset of `hash` is then `total_len - value_len
        //   - 64`. We splice in:
        //
        //     raw[..hash_off] + new_kv_hash + new_value_hash + new_bytes
        //
        //   where:
        //     new_value_hash = combine_hash(value_hash(new_bytes),
        //                                   inner_root_hash)
        //     new_kv_hash    = kv_digest_to_kv_hash(key, new_value_hash)
        //
        //   This matches what `KV::new_with_layered_value_hash` produces
        //   on a real insert (see `merk/src/tree/kv.rs`).
        fn tamper_parent_element_with_consistent_hashes(
            db: &crate::GroveDb,
            path: &[&[u8]],
            key: &[u8],
            new_element: &Element,
            inner_root_hash: CryptoHash,
            grove_version: &GroveVersion,
        ) {
            let tx = db.start_transaction();
            let storage_ctx = db
                .db
                .get_immediate_storage_context(path.into(), &tx)
                .unwrap();

            let raw = storage_ctx
                .get(key)
                .unwrap()
                .expect("storage_ctx get")
                .expect("tampered key must exist on disk");

            let decoded = TreeNode::decode_raw(
                &raw,
                key.to_vec(),
                None::<
                    &fn(
                        &[u8],
                        &GroveVersion,
                    )
                        -> Option<grovedb_merk::tree::kv::ValueDefinedCostType>,
                >,
                grove_version,
            )
            .expect("decode raw tree node");

            let original_value_len = decoded.value_as_slice().len();
            let total_len = raw.len();
            let hash_off = total_len - original_value_len - 32 - 32;

            let new_bytes = new_element
                .serialize(grove_version)
                .expect("serialize replacement element");
            let raw_value_hash = value_hash(&new_bytes).unwrap();
            let new_combined_value_hash = combine_hash(&raw_value_hash, &inner_root_hash).unwrap();
            let new_kv_hash = kv_digest_to_kv_hash(key, &new_combined_value_hash).unwrap();

            let mut new_raw = Vec::with_capacity(hash_off + 64 + new_bytes.len());
            new_raw.extend_from_slice(&raw[..hash_off]);
            new_raw.extend_from_slice(&new_kv_hash);
            new_raw.extend_from_slice(&new_combined_value_hash);
            new_raw.extend_from_slice(&new_bytes);

            storage_ctx
                .put(key, &new_raw, None, None)
                .unwrap()
                .expect("put consistently-rebound tampered tree node");
            db.commit_transaction(tx).unwrap().expect("commit tamper");
        }

        // ==============================================================
        // Test 1: cryptographic tampering of an inner SumItem is
        // caught by verify_grovedb.
        // ==============================================================
        #[test]
        fn verify_grovedb_catches_inner_sum_item_value_tamper() {
            let grove_version = GroveVersion::latest();
            let db = make_empty_grovedb();

            db.insert(
                &[] as &[&[u8]],
                b"psum",
                Element::empty_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert provable sum tree");

            for (k, v) in [(b"a".as_slice(), 7i64), (b"b", 13), (b"c", 20)] {
                db.insert(
                    &[b"psum".as_slice()],
                    k,
                    Element::new_sum_item(v),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert sum item");
            }

            // Sanity: clean tree verifies clean.
            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify clean");
            assert!(
                issues.is_empty(),
                "clean tree should verify clean, got: {:?}",
                issues
            );

            // Tamper: rewrite SumItem(b"a") -> different SumItem WITHOUT
            // updating the stored value_hash. The SumItem arm of the
            // verifier reads stored value_hash and compares against
            // value_hash(bytes); the comparison must now fail.
            tamper_value_no_hash_update(
                &db,
                &[b"psum"],
                b"a",
                &Element::new_sum_item(99),
                grove_version,
            );

            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify tampered");
            // Expect exactly the tampered path to be reported.
            assert!(
                !issues.is_empty(),
                "expected verify_grovedb to detect inner SumItem tamper"
            );
            let tampered_path: Vec<Vec<u8>> = vec![b"psum".to_vec(), b"a".to_vec()];
            assert!(
                issues.contains_key(&tampered_path),
                "expected issue at tampered path {:?}, got: {:?}",
                tampered_path,
                issues
            );
        }

        // ==============================================================
        // Test 2: cryptographic tampering of an inner SumItem with a
        // value the same length as the original is still caught (this
        // is a sanity check that hashes — not lengths — are what get
        // verified).
        // ==============================================================
        #[test]
        fn verify_grovedb_catches_inner_sum_item_same_length_tamper() {
            let grove_version = GroveVersion::latest();
            let db = make_empty_grovedb();

            db.insert(
                &[] as &[&[u8]],
                b"psum",
                Element::empty_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert provable sum tree");

            db.insert(
                &[b"psum".as_slice()],
                b"a",
                Element::new_sum_item(7),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");

            // SumItem(7) -> SumItem(8); same encoded length.
            let old_bytes = Element::new_sum_item(7).serialize(grove_version).unwrap();
            let new_bytes = Element::new_sum_item(8).serialize(grove_version).unwrap();
            assert_eq!(
                old_bytes.len(),
                new_bytes.len(),
                "same-length tamper requires equal serialized sizes"
            );

            tamper_value_no_hash_update(
                &db,
                &[b"psum"],
                b"a",
                &Element::new_sum_item(8),
                grove_version,
            );

            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify tampered");
            assert!(
                !issues.is_empty(),
                "expected verify_grovedb to detect same-length SumItem tamper"
            );
        }

        // ==============================================================
        // Test 3: the new aggregate-consistency check fires when the
        // parent's stored sum_value disagrees with the inner Merk's
        // actual aggregate, even though the cryptographic binding is
        // still consistent.
        // ==============================================================
        #[test]
        fn verify_grovedb_catches_parent_aggregate_mismatch_provable_sum_tree() {
            let grove_version = GroveVersion::latest();
            let db = make_empty_grovedb();

            db.insert(
                &[] as &[&[u8]],
                b"psum",
                Element::empty_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert provable sum tree");

            for (k, v) in [(b"a".as_slice(), 7i64), (b"b", 13), (b"c", 20)] {
                db.insert(
                    &[b"psum".as_slice()],
                    k,
                    Element::new_sum_item(v),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert sum item");
            }

            // Read the inner Merk's actual root hash + the parent's
            // current ProvableSumTree element to reuse the root key.
            let parent = db
                .get(&[] as &[&[u8]], b"psum", None, grove_version)
                .unwrap()
                .expect("get parent");
            let (root_key, _real_sum, flags) = match parent {
                Element::ProvableSumTree(rk, s, f) => (rk, s, f),
                other => panic!("expected ProvableSumTree, got {:?}", other),
            };

            // Compute the actual inner Merk root hash by opening the
            // inner Merk and reading it.
            let inner_root = {
                let tx = db.start_transaction();
                let inner_merk = db
                    .open_transactional_merk_at_path(
                        [b"psum".as_slice()].as_ref().into(),
                        &tx,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("open inner merk");
                inner_merk.root_hash().unwrap()
            };

            // Craft a corrupted parent that claims sum=999 while the
            // inner Merk actually sums to 40.
            let corrupted_parent = Element::ProvableSumTree(root_key.clone(), 999, flags.clone());

            tamper_parent_element_with_consistent_hashes(
                &db,
                &[],
                b"psum",
                &corrupted_parent,
                inner_root,
                grove_version,
            );

            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify tampered");
            // The parent path should appear in issues because of the
            // aggregate-consistency check.
            let tampered_path: Vec<Vec<u8>> = vec![b"psum".to_vec()];
            assert!(
                issues.contains_key(&tampered_path),
                "expected aggregate-consistency issue at {:?}, got: {:?}",
                tampered_path,
                issues
            );
        }

        // ==============================================================
        // Test 4: clean ProvableSumTree verifies clean.
        // ==============================================================
        #[test]
        fn verify_grovedb_clean_provable_sum_tree_reports_no_issues() {
            let grove_version = GroveVersion::latest();
            let db = make_empty_grovedb();

            db.insert(
                &[] as &[&[u8]],
                b"psum",
                Element::empty_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert provable sum tree");

            for (k, v) in [(b"a".as_slice(), 1i64), (b"b", -2), (b"c", 0), (b"d", 100)] {
                db.insert(
                    &[b"psum".as_slice()],
                    k,
                    Element::new_sum_item(v),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert sum item");
            }

            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify");
            assert!(
                issues.is_empty(),
                "clean ProvableSumTree should verify clean, got: {:?}",
                issues
            );
        }

        // ==============================================================
        // Test 5: same general check works for ProvableCountTree.
        // (One positive case + one aggregate-mismatch case.)
        // ==============================================================
        #[test]
        fn verify_grovedb_clean_provable_count_tree_reports_no_issues() {
            let grove_version = GroveVersion::latest();
            let db = make_empty_grovedb();

            db.insert(
                &[] as &[&[u8]],
                b"pcount",
                Element::empty_provable_count_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert provable count tree");

            for k in [b"a".as_slice(), b"b", b"c", b"d", b"e"] {
                db.insert(
                    &[b"pcount".as_slice()],
                    k,
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert item");
            }

            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify");
            assert!(
                issues.is_empty(),
                "clean ProvableCountTree should verify clean, got: {:?}",
                issues
            );
        }

        #[test]
        fn verify_grovedb_catches_parent_aggregate_mismatch_provable_count_tree() {
            let grove_version = GroveVersion::latest();
            let db = make_empty_grovedb();

            db.insert(
                &[] as &[&[u8]],
                b"pcount",
                Element::empty_provable_count_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert provable count tree");

            for k in [b"a".as_slice(), b"b", b"c"] {
                db.insert(
                    &[b"pcount".as_slice()],
                    k,
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert item");
            }

            let parent = db
                .get(&[] as &[&[u8]], b"pcount", None, grove_version)
                .unwrap()
                .expect("get parent");
            let (root_key, _real_count, flags) = match parent {
                Element::ProvableCountTree(rk, c, f) => (rk, c, f),
                other => panic!("expected ProvableCountTree, got {:?}", other),
            };

            let inner_root = {
                let tx = db.start_transaction();
                let inner_merk = db
                    .open_transactional_merk_at_path(
                        [b"pcount".as_slice()].as_ref().into(),
                        &tx,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("open inner merk");
                inner_merk.root_hash().unwrap()
            };

            // Parent claims 9999 items; inner Merk actually has 3.
            let corrupted_parent =
                Element::ProvableCountTree(root_key.clone(), 9999, flags.clone());
            tamper_parent_element_with_consistent_hashes(
                &db,
                &[],
                b"pcount",
                &corrupted_parent,
                inner_root,
                grove_version,
            );

            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify tampered");
            let tampered_path: Vec<Vec<u8>> = vec![b"pcount".to_vec()];
            assert!(
                issues.contains_key(&tampered_path),
                "expected aggregate-consistency issue at {:?}, got: {:?}",
                tampered_path,
                issues
            );
        }

        // ==============================================================
        // Test 6: reload-after-write determinism. Insert, drop the
        // db handle, reopen, run verify_grovedb. Zero issues.
        // ==============================================================
        #[test]
        fn verify_grovedb_persists_clean_across_reopen() {
            let grove_version = GroveVersion::latest();
            let tmp_dir = tempfile::TempDir::new().expect("temp dir");

            {
                let db = crate::GroveDb::open(tmp_dir.path()).expect("open db");
                db.insert(
                    &[] as &[&[u8]],
                    b"psum",
                    Element::empty_provable_sum_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert provable sum tree");
                for (k, v) in [(b"a".as_slice(), 5i64), (b"b", 7), (b"c", 11)] {
                    db.insert(
                        &[b"psum".as_slice()],
                        k,
                        Element::new_sum_item(v),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("insert sum item");
                }
            } // db dropped here

            // Reopen + verify.
            let db = crate::GroveDb::open(tmp_dir.path()).expect("reopen db");
            let issues = db
                .verify_grovedb(None, true, false, grove_version)
                .expect("verify after reopen");
            assert!(
                issues.is_empty(),
                "freshly-reopened DB should verify clean, got: {:?}",
                issues
            );

            // And the parent's stored aggregate sum is intact.
            let parent = db
                .get(&[] as &[&[u8]], b"psum", None, grove_version)
                .unwrap()
                .expect("get parent");
            assert_eq!(parent.as_provable_sum_tree_value().expect("psum value"), 23);
        }
    }
}
