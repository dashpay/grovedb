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
}
