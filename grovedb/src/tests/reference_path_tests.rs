//! Integration tests for `reference_path.rs` -- covering error paths in
//! `follow_reference` and `follow_reference_once`.

#[cfg(test)]
mod tests {
    use grovedb_merk::{element::insert::ElementInsertToStorageExtensions, tree::NULL_HASH};
    use grovedb_path::SubtreePath;
    use grovedb_version::version::GroveVersion;

    use crate::{
        Element, Error,
        merk_cache::MerkCache,
        reference_path::{ReferencePathType, follow_reference, follow_reference_once},
        tests::{TEST_LEAF, make_test_grovedb},
    };

    /// Helper: extract the `Err` from a `CostResult` whose `Ok` type does not
    /// implement `Debug`. Panics with the given message when the result is `Ok`.
    fn unwrap_cost_err<T>(cost_result: grovedb_costs::CostResult<T, Error>, msg: &str) -> Error {
        match cost_result.unwrap() {
            Err(e) => e,
            Ok(_) => panic!("{}", msg),
        }
    }

    /// Two references that form a cycle: ref_a -> ref_b -> ref_a.
    ///
    /// We insert both reference elements at the Merk level (bypassing GroveDb
    /// validation which would reject dangling/cyclic refs) and then call the
    /// standalone `follow_reference` from `reference_path.rs`.
    ///
    /// Covers line 64: `CyclicReference` in `follow_reference`.
    #[test]
    fn test_cyclic_reference_detected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a subtree to hold our references.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"refs",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        let tx = db.start_transaction();

        // Use MerkCache to insert raw reference elements at the Merk level,
        // which skips GroveDb-level validation.
        {
            let cache = MerkCache::new(&db, &tx, grove_version);
            let path: SubtreePath<&[u8]> = SubtreePath::from(&[TEST_LEAF, b"refs"] as &[&[u8]]);

            // ref_a points to [TEST_LEAF, "refs", "b"]
            let ref_a = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"refs".to_vec(),
                b"b".to_vec(),
            ]));

            // ref_b points to [TEST_LEAF, "refs", "a"]
            let ref_b = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"refs".to_vec(),
                b"a".to_vec(),
            ]));

            let mut merk = cache
                .get_merk(path.derive_owned())
                .unwrap()
                .expect("should open merk");

            // Insert both references via the low-level Merk insert (no
            // reference validation). We use insert_reference with NULL_HASH
            // since the combined value hash is irrelevant for this test.
            merk.for_merk(|m| {
                ref_a
                    .insert_reference(m, b"a", NULL_HASH, None, grove_version)
                    .unwrap()
                    .expect("should insert ref_a at merk level");
            });

            merk.for_merk(|m| {
                ref_b
                    .insert_reference(m, b"b", NULL_HASH, None, grove_version)
                    .unwrap()
                    .expect("should insert ref_b at merk level");
            });

            drop(merk);

            // Now call follow_reference starting at key "a" with its ref type.
            // The chain is: (initial) -> b -> a -> b (cycle detected).
            let err = unwrap_cost_err(
                follow_reference(
                    &cache,
                    path.derive_owned(),
                    b"a",
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"refs".to_vec(),
                        b"b".to_vec(),
                    ]),
                ),
                "should detect cyclic reference",
            );

            assert!(
                matches!(err, Error::CyclicReference),
                "expected CyclicReference, got: {:?}",
                err
            );
        }
    }

    /// Create a chain of references longer than MAX_REFERENCE_HOPS (10).
    /// ref0 -> ref1 -> ref2 -> ... -> ref10 -> ref11 (11 hops total).
    ///
    /// Covers line 107: `ReferenceLimit` in `follow_reference`.
    #[test]
    fn test_reference_hop_limit_exceeded() {
        use crate::operations::get::MAX_REFERENCE_HOPS;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a subtree to hold our chain.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"chain",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        let tx = db.start_transaction();

        {
            let cache = MerkCache::new(&db, &tx, grove_version);
            let path: SubtreePath<&[u8]> = SubtreePath::from(&[TEST_LEAF, b"chain"] as &[&[u8]]);

            let keygen = |i: usize| format!("ref{}", i).into_bytes();

            // Build a chain: ref_i -> ref_{i+1}, for i in 0..=MAX_REFERENCE_HOPS.
            // That gives MAX_REFERENCE_HOPS + 1 reference elements total, so the
            // hop count will exceed the limit.
            //
            // The last element in the chain also points forward (to a
            // non-existent key), but the ReferenceLimit error triggers before
            // the PathKeyNotFound because hops run out first.
            for i in 0..=MAX_REFERENCE_HOPS {
                let ref_element =
                    Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"chain".to_vec(),
                        keygen(i + 1),
                    ]));

                let mut merk = cache
                    .get_merk(path.derive_owned())
                    .unwrap()
                    .expect("should open merk");

                merk.for_merk(|m| {
                    ref_element
                        .insert_reference(m, keygen(i), NULL_HASH, None, grove_version)
                        .unwrap()
                        .expect("should insert reference at merk level");
                });

                drop(merk);
            }

            // follow_reference starts at ref0, which points to ref1 ... ref10
            // -> ref11. After 10 hops the loop exits and we get ReferenceLimit.
            let err = unwrap_cost_err(
                follow_reference(
                    &cache,
                    path.derive_owned(),
                    b"ref0",
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"chain".to_vec(),
                        keygen(1),
                    ]),
                ),
                "should exceed reference hop limit",
            );

            assert!(
                matches!(err, Error::ReferenceLimit),
                "expected ReferenceLimit, got: {:?}",
                err
            );
        }
    }

    /// Insert nothing at the target location and call `follow_reference` with
    /// a reference pointing to a key that does not exist.
    ///
    /// Covers lines 80-84: `PathKeyNotFound` -> `CorruptedReferencePathKeyNotFound`
    /// in `follow_reference`.
    ///
    /// Also covers lines 151-154: same mapping in `follow_reference_once`.
    #[test]
    fn test_reference_to_missing_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a subtree so the path exists, but no key "ghost" inside it.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"container",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        let tx = db.start_transaction();

        {
            let cache = MerkCache::new(&db, &tx, grove_version);
            let path: SubtreePath<&[u8]> =
                SubtreePath::from(&[TEST_LEAF, b"container"] as &[&[u8]]);

            // Test via follow_reference
            let err = unwrap_cost_err(
                follow_reference(
                    &cache,
                    path.derive_owned(),
                    b"origin",
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"container".to_vec(),
                        b"ghost".to_vec(),
                    ]),
                ),
                "should fail with missing key via follow_reference",
            );

            assert!(
                matches!(err, Error::CorruptedReferencePathKeyNotFound(_)),
                "expected CorruptedReferencePathKeyNotFound, got: {:?}",
                err
            );

            // Test via follow_reference_once (covers lines 151-154)
            let err_once = unwrap_cost_err(
                follow_reference_once(
                    &cache,
                    path.derive_owned(),
                    b"origin",
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"container".to_vec(),
                        b"ghost".to_vec(),
                    ]),
                ),
                "should fail with missing key via follow_reference_once",
            );

            assert!(
                matches!(err_once, Error::CorruptedReferencePathKeyNotFound(_)),
                "expected CorruptedReferencePathKeyNotFound from follow_reference_once, got: {:?}",
                err_once
            );
        }
    }

    /// A reference that points to itself triggers the immediate cycle detection
    /// in `follow_reference_once`.
    ///
    /// Covers lines 139-141: `path == referred_path && key == referred_key`
    /// returning `CyclicReference` in `follow_reference_once`.
    #[test]
    fn test_self_referencing_element() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a subtree so the path is valid.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"self_ref",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        let tx = db.start_transaction();

        {
            let cache = MerkCache::new(&db, &tx, grove_version);
            let path: SubtreePath<&[u8]> = SubtreePath::from(&[TEST_LEAF, b"self_ref"] as &[&[u8]]);

            // Call follow_reference_once with a reference that resolves to the
            // same (path, key) it originates from.
            let err = unwrap_cost_err(
                follow_reference_once(
                    &cache,
                    path.derive_owned(),
                    b"me",
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"self_ref".to_vec(),
                        b"me".to_vec(),
                    ]),
                ),
                "should detect self-reference cycle",
            );

            assert!(
                matches!(err, Error::CyclicReference),
                "expected CyclicReference, got: {:?}",
                err
            );
        }
    }
}
