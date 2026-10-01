//! A replacement whose new value keeps flags of its own: a reference written
//! in the same batch commits to the bytes the replacement stores
//! (`apply_batch.same_batch_reference_target_prediction`), and the
//! replacement is charged for those bytes
//! (`merk_versions.tree.just_in_time_value_update`).

#[cfg(feature = "minimal")]
mod tests {
    use grovedb_costs::{
        storage_cost::{
            removal::StorageRemovedBytes::NoStorageRemoval, transition::ElementFlagsUpdate,
        },
        OperationCost,
    };
    use grovedb_epoch_based_storage_flags::StorageFlags;
    use grovedb_merk::{element::costs::ElementCostExtensions, tree_type::TreeType};
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{BatchApplyOptions, QualifiedGroveDbOp},
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
        Element, Error,
    };

    const OLD_OWNER: [u8; 32] = [1; 32];
    const NEW_OWNER: [u8; 32] = [2; 32];
    const KEY: &[u8] = b"key1";

    fn owned_flags(epoch: u16, owner: [u8; 32]) -> Option<Vec<u8>> {
        Some(StorageFlags::SingleEpochOwned(epoch, owner).to_element_flags())
    }

    fn unowned_flags(epoch: u16) -> Option<Vec<u8>> {
        Some(StorageFlags::SingleEpoch(epoch).to_element_flags())
    }

    fn target() -> ReferencePathType {
        ReferencePathType::AbsolutePathReference(vec![b"tree".to_vec(), KEY.to_vec()])
    }

    /// Drive's flags update, merging or settling owner changes.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Mode {
        Merging,
        Settling,
    }

    fn apply(
        db: &TempGroveDb,
        ops: Vec<QualifiedGroveDbOp>,
        mode: Mode,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        db.apply_batch_with_element_flags_update(
            ops,
            Some(BatchApplyOptions {
                settle_owner_changes: mode == Mode::Settling,
                ..Default::default()
            }),
            |cost, old_flags, new_flags| {
                match mode {
                    Mode::Merging => StorageFlags::update_element_flags(cost, old_flags, new_flags)
                        .map(ElementFlagsUpdate::from),
                    Mode::Settling => StorageFlags::update_element_flags_settling_owner_changes(
                        cost, old_flags, new_flags,
                    ),
                }
                .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
            },
            |flags, removed_key_bytes, removed_value_bytes| {
                StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
                    .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
            },
            None,
            grove_version,
        )
        .cost_as_result()
    }

    /// A grove holding `old` at `[tree]/key1` (a tree of `tree_type`), and,
    /// `with_reference`, a reference to it at `[index, value]/reference`,
    /// deeper than its target as a Drive index reference is.
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
        db.insert(EMPTY_PATH, b"tree", tree, None, None, grove_version)
            .unwrap()
            .expect("expected to insert the tree");
        db.insert(
            [b"tree".as_slice()].as_ref(),
            KEY,
            old.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the old element");
        db.insert(
            EMPTY_PATH,
            b"index",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the index tree");
        db.insert(
            [b"index".as_slice()].as_ref(),
            b"value",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the index value tree");
        if !with_reference {
            return db;
        }
        db.insert(
            [b"index".as_slice(), b"value"].as_ref(),
            b"reference",
            Element::new_reference(target()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the reference");
        db
    }

    fn write_op(element: &Element) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"tree".to_vec()],
            KEY.to_vec(),
            element.clone(),
        )
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

    /// The ways a batch rewrites the references to the element it updates.
    fn reference_ops() -> Vec<(&'static str, Vec<QualifiedGroveDbOp>)> {
        vec![
            (
                "an untrusted refresh of a deeper reference",
                vec![refresh_op(false)],
            ),
            (
                "a trusted refresh of a deeper reference",
                vec![refresh_op(true)],
            ),
            (
                "a rewrite of a deeper reference",
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![b"index".to_vec(), b"value".to_vec()],
                    b"reference".to_vec(),
                    Element::new_reference(target()),
                )],
            ),
            (
                "a refresh and a new reference beside the tree",
                vec![
                    refresh_op(true),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"index".to_vec()],
                        b"another reference".to_vec(),
                        Element::new_reference(target()),
                    ),
                ],
            ),
        ]
    }

    /// Updates whose stored flags are not the new element's as written, nor
    /// the old element's: the flags update merges them.
    fn flag_changing_updates() -> Vec<(&'static str, Element, Element, TreeType)> {
        vec![
            (
                "an item with a sum item, same owner, grown in a later epoch",
                Element::new_item_with_sum_item_with_flags(
                    vec![7; 20],
                    5,
                    owned_flags(0, OLD_OWNER),
                ),
                Element::new_item_with_sum_item_with_flags(
                    vec![8; 45],
                    9,
                    owned_flags(2, OLD_OWNER),
                ),
                TreeType::SumTree,
            ),
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
        ]
    }

    fn issues(db: &TempGroveDb, grove_version: &GroveVersion) -> usize {
        db.visualize_verify_grovedb(None, true, false, grove_version)
            .expect("expected to verify the grove")
            .len()
    }

    #[test]
    fn a_reference_rewritten_in_the_same_batch_commits_to_the_stored_bytes() {
        let grove_version = GroveVersion::latest();
        for (label, old, new, tree_type) in flag_changing_updates() {
            for mode in [Mode::Merging, Mode::Settling] {
                for (reference_label, reference_ops) in reference_ops() {
                    let db = grove_with(&old, tree_type, true, grove_version);
                    let mut ops = vec![write_op(&new)];
                    ops.extend(reference_ops);
                    apply(&db, ops, mode, grove_version).unwrap_or_else(|e| {
                        panic!("{label}, {reference_label}, {mode:?}: expected to apply: {e}")
                    });
                    assert_eq!(
                        issues(&db, grove_version),
                        0,
                        "{label}, {reference_label}, {mode:?}: the reference is stale"
                    );
                }
            }
        }
    }

    /// Grove v1..v3 keep the hand-written prediction, which takes a sum item
    /// to keep its stored flags (consensus-locked).
    #[test]
    fn grove_v3_keeps_the_legacy_sum_item_prediction() {
        let grove_version = &GROVE_V3;
        for (label, old, new, tree_type) in flag_changing_updates() {
            if !old.is_sum_item() {
                continue;
            }
            for (reference_label, reference_ops) in reference_ops() {
                let db = grove_with(&old, tree_type, true, grove_version);
                let references = reference_ops.len();
                let mut ops = vec![write_op(&new)];
                ops.extend(reference_ops);
                apply(&db, ops, Mode::Merging, grove_version).unwrap_or_else(|e| {
                    panic!("{label}, {reference_label}: expected to apply: {e}")
                });
                assert_eq!(
                    issues(&db, grove_version),
                    references,
                    "{label}, {reference_label}: expected every reference stale"
                );
            }
        }
    }

    /// The bytes the node of `element` at `key1` holds besides its key.
    fn value_bytes(element: &Element, tree_type: TreeType, grove_version: &GroveVersion) -> u32 {
        Element::specialized_costs_for_key_value(
            KEY,
            &element
                .serialize(grove_version)
                .expect("expected to serialize"),
            tree_type.inner_node_type(),
            grove_version,
        )
        .expect("expected value bytes")
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
