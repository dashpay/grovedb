//! A replacement whose new value keeps flags of its own: a reference written
//! in the same batch commits to the bytes the replacement stores
//! (`apply_batch.same_batch_reference_target_prediction`), and the
//! replacement is charged for those bytes
//! (`merk_versions.tree.just_in_time_value_update`).

#[cfg(feature = "minimal")]
mod tests {
    use grovedb_costs::{
        storage_cost::removal::StorageRemovedBytes::{BasicStorageRemoval, NoStorageRemoval},
        OperationCost,
    };
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{
            settle_test_support::{
                apply, flags_update, options, owned_flags, split_removal_bytes, unowned_flags,
                value_bytes, write_op, Mode, KEY, NEW_OWNER, OLD_OWNER,
            },
            QualifiedGroveDbOp,
        },
        reference_path::ReferencePathType,
        tests::{make_empty_grovedb, TempGroveDb},
        Element, Error,
    };

    fn target() -> ReferencePathType {
        ReferencePathType::AbsolutePathReference(vec![b"tree".to_vec(), KEY.to_vec()])
    }

    fn insert(db: &TempGroveDb, path: &[&[u8]], key: &[u8], element: Element, gv: &GroveVersion) {
        db.insert(path, key, element, None, None, gv)
            .unwrap()
            .expect("expected to insert");
    }

    /// A grove holding `old` at `[tree]/key1` (a tree of `tree_type`), next
    /// to an `index` tree with a `value` tree under it and a `zindex` tree
    /// that sorts after `tree`, and, `with_reference`, a reference to it at
    /// `[index, value]/reference`, deeper than its target as a Drive index
    /// reference is.
    fn grove_with(
        old: &Element,
        tree_type: TreeType,
        with_reference: bool,
        grove_version: &GroveVersion,
    ) -> TempGroveDb {
        let db = make_empty_grovedb();
        let tree = match tree_type {
            TreeType::SumTree => Element::empty_sum_tree(),
            _ => Element::empty_tree(),
        };
        insert(&db, &[], b"tree", tree, grove_version);
        insert(&db, &[b"tree"], KEY, old.clone(), grove_version);
        insert(&db, &[], b"index", Element::empty_tree(), grove_version);
        insert(
            &db,
            &[b"index"],
            b"value",
            Element::empty_tree(),
            grove_version,
        );
        insert(&db, &[], b"zindex", Element::empty_tree(), grove_version);
        if with_reference {
            insert(
                &db,
                &[b"index", b"value"],
                b"reference",
                Element::new_reference(target()),
                grove_version,
            );
        }
        db
    }

    fn refresh_op(trusted: bool) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::refresh_reference_op(
            vec![b"index".to_vec(), b"value".to_vec()],
            b"reference".to_vec(),
            target(),
            None,
            None,
            false,
            trusted,
        )
    }

    fn reference_op(path: Vec<Vec<u8>>, key: &[u8]) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            path,
            key.to_vec(),
            Element::new_reference(target()),
        )
    }

    /// The ways a batch rewrites the references to the element it updates:
    /// whether the grove holds the deeper reference (which a way then
    /// rewrites too), and how many of the references resolve before the
    /// batch writes the element.
    fn reference_ops() -> Vec<(&'static str, Vec<QualifiedGroveDbOp>, bool, usize)> {
        vec![
            (
                "an untrusted refresh of a deeper reference",
                vec![refresh_op(false)],
                true,
                1,
            ),
            (
                "a trusted refresh of a deeper reference",
                vec![refresh_op(true)],
                true,
                1,
            ),
            (
                "a rewrite of a deeper reference",
                vec![reference_op(
                    vec![b"index".to_vec(), b"value".to_vec()],
                    b"reference",
                )],
                true,
                1,
            ),
            (
                "a refresh and a new reference in a tree before the target's",
                vec![
                    refresh_op(true),
                    reference_op(vec![b"index".to_vec()], b"another"),
                ],
                true,
                2,
            ),
            (
                "a refresh and a new reference at the root",
                vec![refresh_op(true), reference_op(vec![], b"root reference")],
                true,
                1,
            ),
            (
                "a new reference at the root",
                vec![reference_op(vec![], b"root reference")],
                false,
                0,
            ),
            (
                "a new reference in a tree after the target's",
                vec![reference_op(vec![b"zindex".to_vec()], b"another")],
                false,
                0,
            ),
        ]
    }

    /// Updates whose stored flags are not the new element's as written, nor
    /// the old element's: the flags update merges them.
    fn flag_changing_updates() -> Vec<(&'static str, Element, Element, TreeType)> {
        let mut updates = vec![];
        for growth in [1, 3, 25] {
            updates.push((
                "an item with a sum item, same owner, grown in a later epoch",
                Element::new_item_with_sum_item_with_flags(
                    vec![7; 20],
                    5,
                    owned_flags(0, OLD_OWNER),
                ),
                Element::new_item_with_sum_item_with_flags(
                    vec![8; 20 + growth],
                    9,
                    owned_flags(2, OLD_OWNER),
                ),
                TreeType::SumTree,
            ));
        }
        updates.extend([
            (
                "a sum item gaining an owner in the same epoch",
                Element::new_sum_item_with_flags(5, unowned_flags(0)),
                Element::new_sum_item_with_flags(900, owned_flags(0, NEW_OWNER)),
                TreeType::SumTree,
            ),
            (
                "a sum item gaining an owner in a later epoch",
                Element::new_sum_item_with_flags(5, unowned_flags(0)),
                Element::new_sum_item_with_flags(900, owned_flags(2, NEW_OWNER)),
                TreeType::SumTree,
            ),
            (
                "an item, same owner, grown in a later epoch",
                Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER)),
                Element::new_item_with_flags(vec![8; 45], owned_flags(2, OLD_OWNER)),
                TreeType::NormalTree,
            ),
            (
                "an item gaining an owner in a later epoch",
                Element::new_item_with_flags(vec![7; 20], unowned_flags(0)),
                Element::new_item_with_flags(vec![8; 45], owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
        ]);
        updates
    }

    fn issues(db: &TempGroveDb, grove_version: &GroveVersion) -> usize {
        db.visualize_verify_grovedb(None, true, false, grove_version)
            .expect("expected to verify the grove")
            .len()
    }

    /// Applies `new` and `reference_ops` to a fresh grove holding `old` (and,
    /// `with_reference`, the deeper reference), and counts the stale
    /// references.
    fn stale_references(
        old: &Element,
        new: &Element,
        tree_type: TreeType,
        reference_ops: Vec<QualifiedGroveDbOp>,
        with_reference: bool,
        mode: Mode,
        grove_version: &GroveVersion,
    ) -> Result<usize, Error> {
        let db = grove_with(old, tree_type, with_reference, grove_version);
        let mut ops = vec![write_op(new)];
        ops.extend(reference_ops);
        apply(&db, ops, mode, grove_version)?;
        Ok(issues(&db, grove_version))
    }

    #[test]
    fn a_reference_rewritten_in_the_same_batch_commits_to_the_stored_bytes() {
        let grove_version = GroveVersion::latest();
        for (label, old, new, tree_type) in flag_changing_updates() {
            for mode in [Mode::Merging, Mode::Settling] {
                for (reference_label, reference_ops, with_reference, _) in reference_ops() {
                    let stale = stale_references(
                        &old,
                        &new,
                        tree_type,
                        reference_ops,
                        with_reference,
                        mode,
                        grove_version,
                    )
                    .unwrap_or_else(|e| {
                        panic!("{label}, {reference_label}, {mode:?}: expected to apply: {e}")
                    });
                    assert_eq!(
                        stale, 0,
                        "{label} ({new:?}), {reference_label}, {mode:?}: a reference is stale"
                    );
                }
            }
        }
    }

    /// Grove v1..v3 keep the hand-written prediction without the option,
    /// which takes a sum item to keep its stored flags: a reference that
    /// resolves before the batch writes the sum item commits to bytes that
    /// are never stored (consensus-locked).
    #[test]
    fn grove_v3_keeps_the_legacy_sum_item_prediction() {
        let grove_version = &GROVE_V3;
        for (label, old, new, tree_type) in flag_changing_updates() {
            if !old.is_sum_item() {
                continue;
            }
            for (reference_label, reference_ops, with_reference, resolving_first) in reference_ops()
            {
                let stale = stale_references(
                    &old,
                    &new,
                    tree_type,
                    reference_ops,
                    with_reference,
                    Mode::Merging,
                    grove_version,
                )
                .unwrap_or_else(|e| panic!("{label}, {reference_label}: expected to apply: {e}"));
                assert_eq!(
                    stale, resolving_first,
                    "{label} ({new:?}), {reference_label}: expected the legacy stale references"
                );
            }
        }
    }

    /// Settling owner changes is new on every grove version and has no
    /// legacy prediction to keep: with the option, grove v1..v3 predict a
    /// same-batch reference's target through Merk's own update too.
    #[test]
    fn grove_v3_predicts_the_stored_bytes_with_the_option() {
        let grove_version = &GROVE_V3;
        for (label, old, new, tree_type) in flag_changing_updates() {
            for (reference_label, reference_ops, with_reference, _) in reference_ops() {
                let stale = stale_references(
                    &old,
                    &new,
                    tree_type,
                    reference_ops,
                    with_reference,
                    Mode::Settling,
                    grove_version,
                )
                .unwrap_or_else(|e| panic!("{label}, {reference_label}: expected to apply: {e}"));
                assert_eq!(
                    stale, 0,
                    "{label} ({new:?}), {reference_label}: a reference is stale"
                );
            }
        }
    }

    /// A sum item replaced by an item, or the reverse, applies with a
    /// same-batch reference to it as it does alone.
    #[test]
    fn a_sum_item_and_an_item_replace_each_other_under_a_same_batch_reference() {
        let swaps = [
            (
                Element::new_sum_item_with_flags(5, owned_flags(0, OLD_OWNER)),
                Element::new_item_with_flags(b"x".to_vec(), owned_flags(1, OLD_OWNER)),
            ),
            (
                Element::new_item_with_flags(b"x".to_vec(), owned_flags(0, OLD_OWNER)),
                Element::new_sum_item_with_flags(5, owned_flags(1, OLD_OWNER)),
            ),
        ];
        for (old, new) in swaps {
            for (reference_label, reference_ops, with_reference, _) in reference_ops() {
                let stale = stale_references(
                    &old,
                    &new,
                    TreeType::SumTree,
                    reference_ops,
                    with_reference,
                    Mode::Merging,
                    GroveVersion::latest(),
                )
                .unwrap_or_else(|e| panic!("{old:?} -> {new:?}, {reference_label}: {e}"));
                assert_eq!(stale, 0, "{old:?} -> {new:?}, {reference_label}");
            }
            // grove v1..v3 refuse it when the reference resolves first
            // (consensus-locked)
            let error = stale_references(
                &old,
                &new,
                TreeType::SumTree,
                vec![refresh_op(true)],
                true,
                Mode::Merging,
                &GROVE_V3,
            )
            .expect_err("expected the legacy refusal");
            assert!(
                error.to_string().contains("is not supported"),
                "unexpected error {error}"
            );
        }
    }

    /// A deterministic flags update that stamps the measured storage change
    /// into the new flags, so the stored bytes depend on every measurement
    /// the update takes.
    fn stamp_costs(
        cost: &grovedb_costs::storage_cost::StorageCost,
        _old_flags: Option<Vec<u8>>,
        new_flags: &mut Vec<u8>,
    ) -> Result<bool, Error> {
        let removed = match cost.removed_bytes {
            BasicStorageRemoval(bytes) => bytes,
            _ => 0,
        };
        let mut stamp = cost.added_bytes.to_be_bytes().to_vec();
        stamp.extend(removed.to_be_bytes());
        stamp.extend(cost.replaced_bytes.to_be_bytes());
        if *new_flags == stamp {
            return Ok(false);
        }
        *new_flags = stamp;
        Ok(true)
    }

    /// A reference that resolves after the batch has written its target
    /// commits to the bytes the target stores, whatever the flags update.
    #[test]
    fn a_reference_to_a_target_written_earlier_in_the_batch_commits_to_its_stored_bytes() {
        let grove_version = GroveVersion::latest();
        for (target_path, reference_path) in [
            (vec![b"a".to_vec(), b"b".to_vec()], vec![b"c".to_vec()]),
            (vec![b"a".to_vec()], vec![b"c".to_vec(), b"d".to_vec()]),
        ] {
            let db = make_empty_grovedb();
            insert(&db, &[], b"a", Element::empty_tree(), grove_version);
            insert(&db, &[b"a"], b"b", Element::empty_tree(), grove_version);
            insert(&db, &[], b"c", Element::empty_tree(), grove_version);
            insert(&db, &[b"c"], b"d", Element::empty_tree(), grove_version);
            let path: Vec<&[u8]> = target_path.iter().map(Vec::as_slice).collect();
            insert(
                &db,
                &path,
                b"k",
                Element::new_item_with_flags(b"v".to_vec(), Some(vec![0])),
                grove_version,
            );
            let mut qualified_target = target_path.clone();
            qualified_target.push(b"k".to_vec());
            db.apply_batch_with_element_flags_update(
                vec![
                    QualifiedGroveDbOp::replace_op(
                        target_path.clone(),
                        b"k".to_vec(),
                        Element::new_item_with_flags(b"vv".to_vec(), Some(vec![0])),
                    ),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        reference_path.clone(),
                        b"r".to_vec(),
                        Element::new_reference(ReferencePathType::AbsolutePathReference(
                            qualified_target,
                        )),
                    ),
                ],
                None,
                stamp_costs,
                |_flags, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected the batch to apply");
            assert_eq!(
                issues(&db, grove_version),
                0,
                "target at {target_path:?}, reference at {reference_path:?}"
            );
        }
    }

    /// A reference a partial batch's add-on writes to an item its initial
    /// segment updated commits to the bytes that item stores.
    #[test]
    fn an_add_on_reference_commits_to_the_bytes_the_initial_segment_stored() {
        let grove_version = GroveVersion::latest();
        let old =
            Element::new_item_with_sum_item_with_flags(vec![7; 20], 5, owned_flags(0, OLD_OWNER));
        let new =
            Element::new_item_with_sum_item_with_flags(vec![8; 21], 9, owned_flags(2, OLD_OWNER));
        for mode in [Mode::Merging, Mode::Settling] {
            let db = grove_with(&old, TreeType::SumTree, false, grove_version);
            db.apply_partial_batch_with_element_flags_update(
                vec![write_op(&new)],
                Some(options(mode)),
                flags_update(mode),
                split_removal_bytes,
                |_cost, _leftover_operations| Ok(vec![reference_op(vec![], b"root reference")]),
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected the partial batch to apply");
            assert_eq!(issues(&db, grove_version), 0, "{mode:?}");
        }
    }

    /// A partial batch whose initial segment updates a flagged item and
    /// whose add-on rewrites that item together with the deeper reference to
    /// it. The reference sits deeper, so the add-on resolves it before it
    /// writes the item again: it has to predict that second write rather
    /// than read the bytes the initial segment stored. The item ends exactly
    /// as the two updates applied as separate batches leave it, and every
    /// reference verifies, with the merging update and with the settling one,
    /// including a second update that changes the owner.
    #[test]
    fn an_add_on_rewriting_an_initial_segment_target_predicts_its_new_bytes() {
        let grove_version = GroveVersion::latest();
        let item = |value: u8, len: usize, sum: i64, flags| {
            Element::new_item_with_sum_item_with_flags(vec![value; len], sum, flags)
        };
        let old = item(7, 20, 5, owned_flags(0, OLD_OWNER));
        let first = item(8, 21, 9, owned_flags(1, OLD_OWNER));
        let cases = [
            (Mode::Merging, item(9, 23, 11, owned_flags(2, OLD_OWNER))),
            (Mode::Settling, item(9, 23, 11, owned_flags(2, OLD_OWNER))),
            (Mode::Settling, item(9, 23, 11, owned_flags(2, NEW_OWNER))),
        ];
        for (mode, second) in cases {
            for reference in [refresh_op(true), refresh_op(false)] {
                let label = format!("{mode:?}, second {second:?}, {reference:?}");

                let sequential = grove_with(&old, TreeType::SumTree, true, grove_version);
                apply(&sequential, vec![write_op(&first)], mode, grove_version)
                    .unwrap_or_else(|e| panic!("{label}: first batch: {e}"));
                apply(
                    &sequential,
                    vec![write_op(&second), reference.clone()],
                    mode,
                    grove_version,
                )
                .unwrap_or_else(|e| panic!("{label}: second batch: {e}"));

                let partial = grove_with(&old, TreeType::SumTree, true, grove_version);
                partial
                    .apply_partial_batch_with_element_flags_update(
                        vec![write_op(&first)],
                        Some(options(mode)),
                        flags_update(mode),
                        split_removal_bytes,
                        |_cost, _leftover_operations| {
                            Ok(vec![write_op(&second), reference.clone()])
                        },
                        None,
                        grove_version,
                    )
                    .cost_as_result()
                    .unwrap_or_else(|e| panic!("{label}: partial batch: {e}"));

                assert_eq!(issues(&partial, grove_version), 0, "{label}");
                let stored = |db: &TempGroveDb| {
                    db.get([b"tree".as_slice()].as_ref(), KEY, None, grove_version)
                        .unwrap()
                        .expect("expected the item")
                };
                assert_eq!(stored(&partial), stored(&sequential), "{label}");
                assert_eq!(
                    stored(&partial).get_flags(),
                    stored(&sequential).get_flags(),
                    "{label}"
                );
            }
        }
    }

    /// Replacements whose new flags stand as written (the flags update
    /// answers `Unchanged`) but are not the length of the old ones.
    fn unchanged_flags_of_another_length() -> Vec<(&'static str, Element, Element, Option<Mode>)> {
        vec![
            (
                "longer flags, no flags update",
                Element::new_item_with_flags(b"v".to_vec(), Some(vec![0])),
                Element::new_item_with_flags(b"w".to_vec(), Some(vec![1, 2, 3])),
                None,
            ),
            (
                "flags dropped, no flags update",
                Element::new_item_with_flags(b"v".to_vec(), Some(vec![0])),
                Element::new_item(b"w".to_vec()),
                None,
            ),
            (
                "an unowned item gaining an owner in the same epoch, grown",
                Element::new_item_with_flags(vec![7; 20], unowned_flags(0)),
                Element::new_item_with_flags(vec![8; 45], owned_flags(0, NEW_OWNER)),
                Some(Mode::Merging),
            ),
        ]
    }

    fn replace(
        db: &TempGroveDb,
        new: &Element,
        mode: Option<Mode>,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        match mode {
            Some(mode) => apply(db, vec![write_op(new)], mode, grove_version),
            None => db
                .apply_batch(vec![write_op(new)], None, None, grove_version)
                .cost_as_result(),
        }
    }

    #[test]
    fn a_replacement_keeping_flags_of_another_length_is_charged_for_its_bytes() {
        let grove_version = GroveVersion::latest();
        for (label, old, new, mode) in unchanged_flags_of_another_length() {
            // Rewriting `old` over itself replaces its own bytes plus what
            // the update rewrites around it.
            let rewrite_db = grove_with(&old, TreeType::NormalTree, false, grove_version);
            let rewrite = replace(&rewrite_db, &old, mode, grove_version)
                .unwrap_or_else(|e| panic!("{label}: expected to rewrite: {e}"));
            let old_bytes = value_bytes(&old, TreeType::NormalTree, grove_version);
            let new_bytes = value_bytes(&new, TreeType::NormalTree, grove_version);
            let around = rewrite.storage_cost.replaced_bytes - old_bytes;

            let db = grove_with(&old, TreeType::NormalTree, false, grove_version);
            let cost = replace(&db, &new, mode, grove_version)
                .unwrap_or_else(|e| panic!("{label}: expected to apply: {e}"));
            assert_eq!(
                cost.storage_cost.replaced_bytes,
                around + old_bytes.min(new_bytes),
                "{label}"
            );
            assert_eq!(
                cost.storage_cost.added_bytes,
                new_bytes.saturating_sub(old_bytes),
                "{label}"
            );
            assert_eq!(
                cost.storage_cost.removed_bytes.total_removed_bytes(),
                old_bytes.saturating_sub(new_bytes),
                "{label}"
            );
            assert_eq!(issues(&db, grove_version), 0, "{label}");
        }
    }

    /// Grove v1..v3 keep the measurement taken with the old flags, which the
    /// commit refuses (consensus-locked).
    #[test]
    fn grove_v3_refuses_a_replacement_keeping_flags_of_another_length() {
        let grove_version = &GROVE_V3;
        for (label, old, new, mode) in unchanged_flags_of_another_length() {
            let db = grove_with(&old, TreeType::NormalTree, false, grove_version);
            let error = replace(&db, &new, mode, grove_version)
                .expect_err(&format!("{label}: expected the legacy cost mismatch"));
            assert!(
                error.to_string().contains("cost mismatch"),
                "{label}: unexpected error {error}"
            );
        }
        // the same replacement with flags of the same length is accepted
        let db = grove_with(
            &Element::new_item_with_flags(b"v".to_vec(), Some(vec![0])),
            TreeType::NormalTree,
            false,
            grove_version,
        );
        let cost = replace(
            &db,
            &Element::new_item_with_flags(b"w".to_vec(), Some(vec![1])),
            None,
            grove_version,
        )
        .expect("expected a same-length replacement to apply");
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    }
}
