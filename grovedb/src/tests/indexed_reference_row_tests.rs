//! Corruption and tamper coverage for canonical indexed-secondary rows.
//!
//! Every indexed secondary row is a canonical one-hop
//! `ReferenceWithSumItem(SiblingReference(primary_key), Some(1),
//! axis_payload_sum)` written as a COMBINED reference, so its committed
//! value hash is
//! `combine_hash(H(reference bytes), primary_node_value_hash)`.
//!
//! Each test here breaks exactly ONE part of that and asserts
//! `verify_grovedb` names it. The point of separate sentinels is that a
//! corruption report tells an operator which half is wrong — a stale
//! commitment (the primary moved without the mirror running) needs a
//! different response from a malformed reference (the row was written by
//! something that isn't the mirror).

#[cfg(test)]
mod tests {
    use grovedb_element::{indexed::IndexAxis, reference_path::ReferencePathType};
    use grovedb_merk::element::{
        get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{Storage, StorageBatch};
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::indexed_tree::make_axis_secondary_key,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    /// A PCIT at `[TEST_LEAF, "cidx"]` holding one item-shaped entry
    /// `"a"`, which is the shape whose row we then damage.
    fn pcit_with_one_entry(grove_version: &GroveVersion) -> crate::tests::TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert entry");
        db
    }

    /// Overwrite the row at the canonical secondary key with `row`,
    /// bound to `target_hash`. Passing the honest target hash isolates a
    /// row-CONTENT change; passing a different one isolates a
    /// COMMITMENT change.
    fn overwrite_row(
        db: &GroveDb,
        secondary_key: &[u8],
        row: Element,
        target_hash: Option<[u8; 32]>,
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"cidx".as_ref()];
        let path: SubtreePath<&[u8]> = (&path_segments).into();

        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(
                    [TEST_LEAF].as_ref().into(),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open parent");
            let cidx = Element::get(&parent_merk, b"cidx", true, grove_version)
                .unwrap()
                .expect("cidx element");
            match cidx.underlying() {
                Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
                other => panic!("not a PCIT element: {other:?}"),
            }
        };

        {
            let mut secondary = db
                .open_indexed_secondary_at_path(
                    path,
                    IndexAxis::Count,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open secondary");
            // Bind to the honest primary node hash unless the caller is
            // deliberately moving the commitment.
            let bind_to = target_hash.unwrap_or_else(|| {
                let primary = db
                    .open_transactional_merk_at_path(
                        [TEST_LEAF, b"cidx".as_ref()].as_ref().into(),
                        &tx,
                        Some(&batch),
                        grove_version,
                    )
                    .unwrap()
                    .expect("open primary");
                primary
                    .get_value_hash(
                        b"a",
                        true,
                        None::<&fn(&[u8], &GroveVersion) -> _>,
                        grove_version,
                    )
                    .unwrap()
                    .expect("value hash read")
                    .expect("entry present")
            });
            row.insert_reference(&mut secondary, secondary_key, bind_to, None, grove_version)
                .unwrap()
                .expect("write row");
        }

        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    /// The sentinel path `verify_grovedb` files an issue under for the
    /// count axis of the PCIT built above.
    fn sentinel_path(kind: &str) -> Vec<Vec<u8>> {
        vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            format!("__cidx_{kind}__").into_bytes(),
            b"a".to_vec(),
        ]
    }

    fn assert_only_issue(db: &GroveDb, kind: &str, grove_version: &GroveVersion) {
        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        let want = sentinel_path(kind);
        assert!(
            issues.contains_key(&want),
            "expected a `__cidx_{kind}__` issue, got {:?}",
            issues
                .keys()
                .map(|k| k
                    .iter()
                    .map(|s| String::from_utf8_lossy(s).to_string())
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }

    /// Baseline: an untouched tree verifies clean, and its row really is
    /// the canonical shape. Without this the corruption tests below could
    /// pass for the wrong reason.
    #[test]
    fn a_healthy_indexed_tree_stores_canonical_reference_rows() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(issues.is_empty(), "healthy tree reported: {issues:?}");

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"cidx".as_ref()];
        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(
                    [TEST_LEAF].as_ref().into(),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .unwrap();
            match Element::get(&parent_merk, b"cidx", true, grove_version)
                .unwrap()
                .unwrap()
                .underlying()
            {
                Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
                other => panic!("not a PCIT: {other:?}"),
            }
        };
        let secondary = db
            .open_indexed_secondary_at_path(
                (&path_segments).into(),
                IndexAxis::Count,
                secondary_root_key,
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .unwrap();
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        let row = Element::get(&secondary, key.as_slice(), true, grove_version)
            .unwrap()
            .expect("row present");
        assert_eq!(
            row,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"a".to_vec()),
                Some(1),
                // The count axis carries the COUNT as its payload sum, so
                // a band Total stays one committed scalar (#806).
                1,
            ),
            "a healthy count-axis row must be the canonical one-hop reference"
        );
    }

    /// A legacy placeholder row — the representation this change
    /// replaced — must be rejected, not silently accepted.
    #[test]
    fn a_legacy_placeholder_row_is_reported_as_non_canonical() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(&db, &key, Element::new_sum_item(1), None, grove_version);
        assert_only_issue(&db, "secondary_non_canonical_row", grove_version);
    }

    /// A plain `Reference` folds to `(1, 0)` in a PCPS secondary, which
    /// would silently zero the band Total. It is not canonical.
    #[test]
    fn a_plain_reference_row_is_reported_as_non_canonical() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference(ReferencePathType::SiblingReference(b"a".to_vec())),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_non_canonical_row", grove_version);
    }

    /// A non-sibling reference type would make row size grow with grove
    /// depth and breaks the logical-origin rule.
    #[test]
    fn a_non_sibling_reference_row_is_reported_as_non_canonical() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"cidx".to_vec(),
                    b"a".to_vec(),
                ]),
                Some(1),
                1,
            ),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_non_canonical_row", grove_version);
    }

    /// The hop budget is part of the binding rule: one hop means the row
    /// binds the IMMEDIATE primary node. A different budget is a
    /// different binding and must not be accepted as canonical.
    #[test]
    fn a_multi_hop_row_is_reported_as_non_canonical() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"a".to_vec()),
                Some(2),
                1,
            ),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_non_canonical_row", grove_version);
    }

    /// Canonical shape, but pointing at a different primary key than the
    /// secondary-key suffix encodes. The suffix and the reference are two
    /// independent encodings of the same fact and must agree.
    #[test]
    fn a_row_referencing_the_wrong_primary_key_is_reported() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"somewhere-else".to_vec()),
                Some(1),
                1,
            ),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_wrong_reference_target", grove_version);
    }

    /// Canonical shape and target, wrong carried sum. On the count axis
    /// the payload sum is the COUNT, so a wrong one corrupts band totals
    /// while leaving the sort position — and therefore every key-only
    /// check — looking correct.
    #[test]
    fn a_row_with_the_wrong_payload_sum_is_reported() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"a".to_vec()),
                Some(1),
                99,
            ),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_wrong_payload_sum", grove_version);
    }

    /// The headline case for the reference representation: every BYTE of
    /// the row is canonical and correct, and only the commitment is
    /// stale. This is what a value-only primary update produces if the
    /// mirror fails to refresh, and it is invisible to any check that
    /// compares serialized rows alone.
    #[test]
    fn a_row_bound_to_a_stale_target_hash_is_reported() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        let canonical = Element::new_reference_with_sum_item_with_hops(
            ReferencePathType::SiblingReference(b"a".to_vec()),
            Some(1),
            1,
        );
        overwrite_row(&db, &key, canonical, Some([0xEE; 32]), grove_version);
        assert_only_issue(&db, "secondary_stale_target_hash", grove_version);
    }

    /// A value-only update must refresh the row's commitment through the
    /// real write path — the case the pre-reference mirror was free to
    /// skip because `(count, sum)` did not move.
    ///
    /// Driven through the public API on BOTH entry points, because the
    /// direct and batch mirrors are separate implementations and a fix to
    /// one does not imply the other.
    #[test]
    fn a_value_only_update_refreshes_the_row_on_both_write_paths() {
        let grove_version = GroveVersion::latest();

        // Direct path.
        let db = pcit_with_one_entry(grove_version);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"a-completely-different-value".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("value-only update");
        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "direct value-only update left the row stale: {issues:?}"
        );

        // Batch path.
        let db = pcit_with_one_entry(grove_version);
        db.apply_batch(
            vec![crate::batch::QualifiedGroveDbOp::replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"a".to_vec(),
                Element::new_item(b"another-completely-different-value".to_vec()),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch value-only update");
        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "batch value-only update left the row stale: {issues:?}"
        );
    }

    /// A deep mutation under a TREE-shaped entry moves that entry's
    /// committed value hash (its child root changed) while leaving its
    /// count alone. The row must be refreshed even though nothing about
    /// its sort position moved.
    ///
    /// This reaches the mirror only via the synthesized propagation op on
    /// the primary entry, which the aggregate-only mirror skipped as
    /// unchanged — so it is a distinct path from the value-only case
    /// above, not a restatement of it.
    #[test]
    fn a_deep_mutation_under_a_tree_entry_refreshes_the_row() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("tree child");
        // Populate it once so the entry has a non-null child root, then
        // change that root WITHOUT changing the child's count.
        db.insert(
            [TEST_LEAF, b"cidx", b"sub"].as_ref(),
            b"k",
            Element::new_item(b"first".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(issues.is_empty(), "after populate: {issues:?}");

        db.insert(
            [TEST_LEAF, b"cidx", b"sub"].as_ref(),
            b"k",
            Element::new_item(b"second".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("deep value-only update");

        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "a deep mutation that moved the child root left the row stale: {issues:?}"
        );
    }

    /// An axis PROOF must not merely assume that a committed reference
    /// points at the key encoded in the row it sits in.
    ///
    /// This is the row-level analogue of the `verify_grovedb` target
    /// check above, and it needs its own coverage: `verify_grovedb` reads
    /// the reference path directly out of storage, while a verifier never
    /// sees those bytes — it only sees the row's committed reference
    /// hash. The check works by rebuilding the canonical row from the
    /// AUTHENTICATED primary value and comparing hashes, so a row whose
    /// reference points elsewhere cannot survive it.
    ///
    /// Damaging the row makes the secondary root move, so the tampered
    /// state is rejected either as a broken chain or as a non-canonical
    /// row — both are correct refusals, and asserting on "rejected"
    /// rather than on one message keeps the test from pinning which guard
    /// happens to fire first.
    #[test]
    fn an_axis_proof_over_a_mis_targeted_row_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        // A second entry, so the mis-targeted reference points at a key
        // that genuinely exists — the interesting case, since a dangling
        // reference would fail earlier and for a duller reason.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"b",
            Element::new_item(b"other".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("second entry");

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let honest = db
            .prove_indexed_count_top_k(path, 4, true, None, grove_version)
            .unwrap()
            .expect("honest proof");
        GroveDb::verify_indexed_count_top_k(&honest, path, 4, true, grove_version)
            .expect("the honest proof must verify");

        // Point "a"'s row at "b" while leaving its sort position alone.
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"b".to_vec()),
                Some(1),
                1,
            ),
            None,
            grove_version,
        );

        let tampered = db
            .prove_indexed_count_top_k(path, 4, true, None, grove_version)
            .unwrap();
        let rejected = match tampered {
            // The prover itself refuses to build a proof over a row it
            // cannot square with its own key suffix.
            Err(_) => true,
            Ok(bytes) => {
                GroveDb::verify_indexed_count_top_k(&bytes, path, 4, true, grove_version).is_err()
            }
        };
        assert!(
            rejected,
            "a row whose reference points at a different primary key than its own \
             secondary-key suffix must not produce a verifiable proof"
        );
    }

    /// Every direct non-Merk append API must refresh the canonical row of
    /// the entry it rewrites.
    ///
    /// These four share a shape: they write the updated element straight
    /// into the primary Merk and only then start propagating, so the
    /// propagation walk — which mirrors entries it discovers as it climbs
    /// — never sees the entry that moved. An append leaves `(count, sum)`
    /// untouched (a non-Merk child contributes a constant count of `1`),
    /// so under the old aggregate-only rows this was genuinely a no-op.
    /// Under canonical rows it is not: the append rewrites the entry's
    /// non-Merk root, and therefore its commitment.
    ///
    /// Each API gets its own case rather than one shared loop, because
    /// each has its own copy of the write-then-propagate sequence and a
    /// fix to one does not imply the others.
    #[test]
    fn every_non_merk_append_refreshes_the_row_it_rewrites() {
        let grove_version = GroveVersion::latest();

        let pcit_with_child = |child: Element, key: &[u8]| {
            let db = make_test_grovedb(grove_version);
            db.insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create PCIT");
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key,
                child,
                None,
                grove_version,
            )
            .unwrap()
            .expect("non-Merk child");
            db
        };

        // MMR.
        let db = pcit_with_child(Element::empty_mmr_tree(), b"mmr");
        db.mmr_tree_append(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"mmr",
            b"leaf".to_vec(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("mmr append");
        let issues = db.verify_grovedb(None, true, false, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "mmr_tree_append left the row stale: {issues:?}"
        );

        // Bulk-append.
        let db = pcit_with_child(
            Element::empty_bulk_append_tree(4).expect("bulk tree"),
            b"bulk",
        );
        db.bulk_append(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"bulk",
            b"v".to_vec(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("bulk append");
        let issues = db.verify_grovedb(None, true, false, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "bulk_append left the row stale: {issues:?}"
        );

        // Dense.
        let db = pcit_with_child(Element::empty_dense_tree(4), b"dense");
        db.dense_tree_insert(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"dense",
            b"v".to_vec(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense insert");
        let issues = db.verify_grovedb(None, true, false, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "dense_tree_insert left the row stale: {issues:?}"
        );
    }

    /// A NESTED INDEXED TREE as a primary entry.
    ///
    /// Its committed value hash is a three-way
    /// `combine_hash_three(H(value), primary_root, secondary_root)`, which
    /// no single child-hash witness can express. The target-chain
    /// commitment enum carries the pieces instead, so this shape proves
    /// and verifies like any other rather than being refused.
    #[test]
    fn a_nested_indexed_tree_primary_proves_and_resolves() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer PCIT");
        // A PCIT nested inside a PCIT: the inner element is the primary
        // entry whose commitment the outer row must bind.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("inner PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx", b"inner"].as_ref(),
            b"leaf",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("inner entry");
        let issues = db.verify_grovedb(None, true, true, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "nested indexed tree reported: {issues:?}"
        );

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_indexed_count_top_k(path, 5, true, None, grove_version)
            .unwrap()
            .expect("a nested indexed-tree primary must be provable");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 5, true, grove_version)
            .expect("verify");
        assert_eq!(
            result.root_hash,
            db.root_hash(None, grove_version).unwrap().unwrap()
        );
        let entries = match &result.entries {
            crate::operations::proof::indexed_axis::AxisEntries::Count(v) => v,
            other => panic!("expected count entries, got {other:?}"),
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].primary_key, b"inner".to_vec());
        assert!(
            matches!(
                entries[0].value.underlying(),
                Element::ProvableCountIndexedTree(..)
            ),
            "the row must resolve to the nested indexed element itself, got {}",
            entries[0].value.type_str()
        );
    }

    /// A REFERENCE-SHAPED primary entry resolves through to its terminal
    /// on both the direct and the proved path, and the two agree.
    ///
    /// The row still BINDS the immediate primary node — that is what keeps
    /// the mirror's invariant local — while the value handed back is what
    /// `db.get` on that key would return. Both halves matter, so both are
    /// asserted.
    #[test]
    fn a_reference_shaped_primary_resolves_to_its_terminal() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"terminal-value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("terminal");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"ref",
            Element::new_reference(ReferencePathType::UpstreamRootHeightReference(
                1,
                vec![b"target".to_vec()],
            )),
            None,
            grove_version,
        )
        .unwrap()
        .expect("reference-shaped primary");
        let issues = db.verify_grovedb(None, true, true, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "reference-shaped primary reported: {issues:?}"
        );

        let expected = Element::new_item(b"terminal-value".to_vec());

        // Direct read.
        let direct = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, grove_version)
            .unwrap()
            .expect("direct top_k");
        assert_eq!(direct.len(), 1);
        assert_eq!(
            direct[0].value, expected,
            "a direct read must resolve a reference-shaped primary to its terminal"
        );

        // Proved read must agree.
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_indexed_count_top_k(path, 5, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 5, true, grove_version)
            .expect("verify");
        let entries = match &result.entries {
            crate::operations::proof::indexed_axis::AxisEntries::Count(v) => v,
            other => panic!("expected count entries, got {other:?}"),
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].value, expected,
            "a proved read must resolve to the same terminal the direct read returned"
        );
        assert_eq!(entries[0].primary_key, b"ref".to_vec());
    }

    /// Ordinary user references keep their own semantics. The one-hop
    /// immediate-node rule is dedicated indexed-tree behaviour, so it must
    /// not leak into how a normal reference elsewhere in the grove is
    /// treated: an ordinary reference still resolves to its TERMINAL.
    #[test]
    fn ordinary_user_references_are_unaffected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"terminal-value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("target");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"hop",
            Element::new_reference(ReferencePathType::SiblingReference(b"target".to_vec())),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("first hop");
        // A two-hop chain: an ordinary reference follows through to the
        // terminal, which is exactly the semantics an indexed row does
        // NOT use.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"entry",
            Element::new_reference(ReferencePathType::SiblingReference(b"hop".to_vec())),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("second hop");

        let got = db
            .get([TEST_LEAF].as_ref(), b"entry", None, grove_version)
            .unwrap()
            .expect("resolve");
        assert_eq!(
            got,
            Element::new_item(b"terminal-value".to_vec()),
            "an ordinary reference chain must still resolve to its terminal"
        );
        let issues = db.verify_grovedb(None, true, true, grove_version).unwrap();
        assert!(
            issues.is_empty(),
            "ordinary references reported: {issues:?}"
        );
    }
}
