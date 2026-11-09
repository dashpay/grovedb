//! Costs of an update in place accounted as a replacement
//! (`ElementFlagsUpdate::AccountAsReplacement`): the old element is removed
//! and its bytes sectioned to its owner as a deletion sections them, and the
//! new element is inserted and charged to the writer.

#[cfg(feature = "minimal")]
mod tests {
    use std::collections::BTreeMap;

    use grovedb_costs::{
        storage_cost::removal::StorageRemovedBytes::SectionedStorageRemoval, OperationCost,
    };
    use grovedb_epoch_based_storage_flags::StorageFlags;
    use grovedb_merk::{
        element::tree_type::ElementTreeTypeExtensions, tree::kv::KV, tree_type::TreeType,
    };
    use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, GroveVersion};
    use intmap::IntMap;

    use crate::batch::replacement_test_support::{
        apply, owned_flags, replacing_flags_update, split_removal_bytes, unowned_flags,
        value_bytes, write_op, Mode, KEY, NEW_OWNER, OLD_OWNER,
    };
    use crate::{
        batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
        Element, Error,
    };

    fn target_path() -> ReferencePathType {
        ReferencePathType::AbsolutePathReference(vec![b"targets".to_vec(), b"target".to_vec()])
    }

    fn long_target_path() -> ReferencePathType {
        ReferencePathType::AbsolutePathReference(vec![
            b"targets".to_vec(),
            b"target with a much longer key".to_vec(),
        ])
    }

    /// A grove holding `old` at `[tree]/key1` next to an unflagged sibling,
    /// in a tree of `tree_type`, and a `targets` tree whose items references
    /// can point at.
    fn grove_with(old: &Element, tree_type: TreeType, grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"targets",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the targets tree");
        for target in [b"target".as_slice(), b"target with a much longer key"] {
            db.insert(
                [b"targets".as_slice()].as_ref(),
                target,
                Element::new_item(b"target value".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected to insert a target");
        }
        let tree = match tree_type {
            TreeType::SumTree => Element::empty_sum_tree(),
            _ => Element::empty_tree(),
        };
        db.insert(EMPTY_PATH, b"tree", tree, None, None, grove_version)
            .unwrap()
            .expect("expected to insert the tree");
        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key0",
            Element::new_item(b"sibling".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the sibling");
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
        db
    }

    fn delete_op(element: &Element) -> QualifiedGroveDbOp {
        match element.tree_type() {
            Some(tree_type) => QualifiedGroveDbOp::delete_tree_op(
                vec![b"tree".to_vec()],
                KEY.to_vec(),
                tree_type,
                SubelementsDeletionBehavior::Error,
            ),
            None => QualifiedGroveDbOp::delete_op(vec![b"tree".to_vec()], KEY.to_vec()),
        }
    }

    fn stored(db: &TempGroveDb, grove_version: &GroveVersion) -> Element {
        db.get_raw(
            [b"tree".as_slice()].as_ref().into(),
            KEY,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected the element")
    }

    fn storage_flags_of(element: &Element) -> StorageFlags {
        StorageFlags::from_element_flags_ref(element.get_flags().as_ref().expect("expected flags"))
            .expect("expected valid flags")
            .expect("expected storage flags")
    }

    fn key_bytes() -> u32 {
        KV::node_key_byte_cost_size(KEY.len() as u32)
    }

    /// The bytes `cost` removes from `owner`, by epoch.
    fn removed_from(cost: &OperationCost, owner: [u8; 32]) -> BTreeMap<u16, u32> {
        match &cost.storage_cost.removed_bytes {
            SectionedStorageRemoval(by_owner) => by_owner
                .get(&owner)
                .map(|by_epoch| {
                    by_epoch
                        .iter()
                        .map(|(epoch, bytes)| (epoch, *bytes))
                        .collect()
                })
                .unwrap_or_default(),
            _ => BTreeMap::new(),
        }
    }

    /// What a deletion of an element holding `flags` removes from its owner:
    /// its key and value bytes, sectioned through its epoch map.
    fn deletion_sections(flags: &StorageFlags, value_bytes: u32) -> BTreeMap<u16, u32> {
        let (key_removal, value_removal) =
            flags.split_storage_removed_bytes(key_bytes(), value_bytes);
        let mut sections = BTreeMap::new();
        for removal in [key_removal, value_removal] {
            if let SectionedStorageRemoval(by_owner) = removal {
                let by_epoch: &IntMap<u16, u32> = by_owner
                    .get(flags.owner_id().expect("expected an owner"))
                    .expect("expected the owner's section");
                for (epoch, bytes) in by_epoch.iter() {
                    *sections.entry(epoch).or_insert(0) += *bytes;
                }
            }
        }
        sections
    }

    /// Writes `new` over `old` with the replacing update, checks the update
    /// was accounted as the deletion of `old` plus the insertion of `new`,
    /// and returns the grove and the cost.
    fn assert_replaces(
        old: &Element,
        new: &Element,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> (TempGroveDb, OperationCost) {
        // Rewriting `old` over itself replaces its own bytes plus whatever
        // the update rewrites around it (its parent nodes and tree element).
        let rewrite_db = grove_with(old, tree_type, grove_version);
        let rewrite = apply(
            &rewrite_db,
            vec![write_op(old)],
            Mode::Merging,
            grove_version,
        )
        .expect("expected to rewrite the old element");
        let replaced_around_it =
            rewrite.storage_cost.replaced_bytes - value_bytes(old, tree_type, grove_version);
        assert_eq!(rewrite.storage_cost.added_bytes, 0);

        let delete_db = grove_with(old, tree_type, grove_version);
        let deletion = apply(
            &delete_db,
            vec![delete_op(old)],
            Mode::Merging,
            grove_version,
        )
        .expect("expected to delete the old element");

        let db = grove_with(old, tree_type, grove_version);
        let cost = apply(&db, vec![write_op(new)], Mode::Replacing, grove_version)
            .expect("expected the replacing update to apply");

        // the new element is inserted: key and value added, charged to the
        // writer
        assert_eq!(
            cost.storage_cost.added_bytes,
            key_bytes() + value_bytes(new, tree_type, grove_version)
        );
        // nothing of the element is replaced
        assert_eq!(cost.storage_cost.replaced_bytes, replaced_around_it);
        // the old element is removed from its owner exactly as a deletion
        // removes it
        let old_flags = storage_flags_of(old);
        assert_eq!(
            removed_from(&cost, OLD_OWNER),
            deletion_sections(&old_flags, value_bytes(old, tree_type, grove_version))
        );
        assert_eq!(
            removed_from(&cost, OLD_OWNER),
            removed_from(&deletion, OLD_OWNER)
        );
        assert!(removed_from(&cost, NEW_OWNER).is_empty());
        // the new element is stored as written, with fresh flags
        assert_eq!(&stored(&db, grove_version), new);
        let issues = db
            .visualize_verify_grovedb(None, true, false, grove_version)
            .expect("expected to verify the grove");
        assert!(issues.is_empty(), "grove issues: {issues:?}");
        (db, cost)
    }

    #[test]
    fn owner_change_on_growth_is_a_replacement() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
        assert_replaces(&old, &new, TreeType::NormalTree, grove_version);
    }

    #[test]
    fn owner_change_on_shrink_is_a_replacement() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 60], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 5], owned_flags(2, NEW_OWNER));
        assert_replaces(&old, &new, TreeType::NormalTree, grove_version);
    }

    #[test]
    fn owner_change_at_the_same_size_is_a_replacement() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 20], owned_flags(2, NEW_OWNER));
        assert_eq!(
            value_bytes(&old, TreeType::NormalTree, grove_version),
            value_bytes(&new, TreeType::NormalTree, grove_version)
        );
        assert_replaces(&old, &new, TreeType::NormalTree, grove_version);
    }

    /// An owner change in the old element's own epoch is a replacement too: the old
    /// owner gets back every byte they paid for.
    #[test]
    fn owner_change_in_the_same_epoch_is_a_replacement() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 30], owned_flags(0, NEW_OWNER));
        assert_replaces(&old, &new, TreeType::NormalTree, grove_version);
    }

    #[test]
    fn replacement_accounting_is_refused_below_grove_v5() {
        for grove_version in [&GROVE_V3, &GROVE_V4] {
            let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
            let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
            let db = grove_with(&old, TreeType::NormalTree, grove_version);

            let applied = apply(&db, vec![write_op(&new)], Mode::Replacing, grove_version)
                .expect_err("expected the batch to be refused");
            assert!(
                applied.to_string().contains("version 2"),
                "unexpected error {applied}"
            );
            assert_eq!(stored(&db, grove_version), old);

            let partial = db
                .apply_partial_batch_with_element_flags_update(
                    vec![write_op(&new)],
                    None,
                    replacing_flags_update,
                    split_removal_bytes,
                    |_cost, _ops| Ok(vec![]),
                    None,
                    grove_version,
                )
                .cost_as_result()
                .expect_err("expected the partial batch to be refused");
            assert!(
                partial.to_string().contains("version 2"),
                "unexpected error {partial}"
            );
            assert_eq!(stored(&db, grove_version), old);
        }
    }

    /// Owner changes of the other element types: an item with a sum item,
    /// a sum item, a reference, a reference with a sum item and a tree.
    #[test]
    fn owner_changes_of_every_flagged_element_type_are_replacements() {
        let grove_version = GroveVersion::latest();
        let cases = [
            (
                Element::new_item_with_sum_item_with_flags(
                    vec![7; 20],
                    5,
                    owned_flags(0, OLD_OWNER),
                ),
                Element::new_item_with_sum_item_with_flags(
                    vec![8; 45],
                    9,
                    owned_flags(2, NEW_OWNER),
                ),
                TreeType::SumTree,
            ),
            (
                Element::new_item_with_sum_item_with_flags(
                    vec![7; 45],
                    5,
                    owned_flags(0, OLD_OWNER),
                ),
                Element::new_item_with_sum_item_with_flags(
                    vec![8; 3],
                    -4,
                    owned_flags(2, NEW_OWNER),
                ),
                TreeType::SumTree,
            ),
            (
                Element::new_sum_item_with_flags(5, owned_flags(0, OLD_OWNER)),
                Element::new_sum_item_with_flags(900, owned_flags(2, NEW_OWNER)),
                TreeType::SumTree,
            ),
            (
                Element::new_reference_with_flags(target_path(), owned_flags(0, OLD_OWNER)),
                Element::new_reference_with_flags(long_target_path(), owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
            (
                Element::new_reference_with_flags(long_target_path(), owned_flags(0, OLD_OWNER)),
                Element::new_reference_with_flags(target_path(), owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
            (
                Element::new_reference_with_sum_item_with_flags(
                    target_path(),
                    5,
                    owned_flags(0, OLD_OWNER),
                ),
                Element::new_reference_with_sum_item_with_flags(
                    long_target_path(),
                    7,
                    owned_flags(2, NEW_OWNER),
                ),
                TreeType::SumTree,
            ),
            (
                Element::empty_tree_with_flags(owned_flags(0, OLD_OWNER)),
                Element::empty_tree_with_flags(owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
            (
                Element::empty_sum_tree_with_flags(owned_flags(0, OLD_OWNER)),
                Element::empty_sum_tree_with_flags(owned_flags(2, NEW_OWNER)),
                TreeType::SumTree,
            ),
        ];
        for (old, new, tree_type) in cases {
            assert_replaces(&old, &new, tree_type, grove_version);
        }
    }

    /// A partial batch accounts an owner change as a replacement as a full batch does, and
    /// hands its add-on callback the replacement cost.
    #[test]
    fn a_partial_batch_accounts_an_owner_change_as_a_replacement() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
        let (full_db, full_cost) = assert_replaces(&old, &new, TreeType::NormalTree, grove_version);

        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        let mut initial_cost = None;
        let cost = db
            .apply_partial_batch_with_element_flags_update(
                vec![write_op(&new)],
                None,
                replacing_flags_update,
                split_removal_bytes,
                |cost, _leftover_operations| {
                    initial_cost = Some(cost.clone());
                    Ok(vec![])
                },
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("expected the partial batch to apply");
        let initial_cost = initial_cost.expect("expected the add-on callback to run");
        assert_eq!(
            removed_from(&initial_cost, OLD_OWNER),
            removed_from(&full_cost, OLD_OWNER)
        );
        assert_eq!(cost.storage_cost, full_cost.storage_cost);
        assert_eq!(stored(&db, grove_version), new);
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            full_db.root_hash(None, grove_version).unwrap().unwrap()
        );
    }

    /// Applies `new` over `old` in both modes and checks they agree on
    /// everything: cost, stored element and root hash.
    fn assert_same_in_both_modes(
        old: &Element,
        new: &Element,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) {
        let merging_db = grove_with(old, tree_type, grove_version);
        let merging = apply(
            &merging_db,
            vec![write_op(new)],
            Mode::Merging,
            grove_version,
        )
        .expect("expected the merging update to apply");
        let replacing_db = grove_with(old, tree_type, grove_version);
        let replacing = apply(
            &replacing_db,
            vec![write_op(new)],
            Mode::Replacing,
            grove_version,
        )
        .expect("expected the replacing update to apply");
        assert_eq!(replacing, merging);
        assert_eq!(
            stored(&replacing_db, grove_version),
            stored(&merging_db, grove_version)
        );
        assert_eq!(
            replacing_db
                .root_hash(None, grove_version)
                .unwrap()
                .unwrap(),
            merging_db.root_hash(None, grove_version).unwrap().unwrap()
        );
    }

    #[test]
    fn same_owner_updates_are_unchanged() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        for (len, epoch) in [(60, 2), (5, 2), (20, 2), (60, 0), (5, 0)] {
            let new = Element::new_item_with_flags(vec![8; len], owned_flags(epoch, OLD_OWNER));
            assert_same_in_both_modes(&old, &new, TreeType::NormalTree, grove_version);
        }
        let old = Element::new_sum_item_with_flags(5, owned_flags(0, OLD_OWNER));
        let new = Element::new_sum_item_with_flags(900, owned_flags(2, OLD_OWNER));
        assert_same_in_both_modes(&old, &new, TreeType::SumTree, grove_version);
    }

    #[test]
    fn updates_without_an_owner_on_either_side_are_unchanged() {
        let grove_version = GroveVersion::latest();
        let cases = [
            (owned_flags(0, OLD_OWNER), unowned_flags(2)),
            (unowned_flags(0), unowned_flags(2)),
            (None, owned_flags(2, NEW_OWNER)),
        ];
        for (old_flags, new_flags) in cases {
            for len in [60, 5, 20] {
                let old = Element::new_item_with_flags(vec![7; 20], old_flags.clone());
                let new = Element::new_item_with_flags(vec![8; len], new_flags.clone());
                assert_same_in_both_modes(&old, &new, TreeType::NormalTree, grove_version);
            }
        }
    }

    /// Naming an owner over flags that name none is no handover, as nobody
    /// is owed a refund: the update is not accounted as a replacement, and
    /// the owner the new flags name owns the element at every size.
    #[test]
    fn an_unowned_item_takes_the_written_owner_without_a_replacement() {
        let grove_version = GroveVersion::latest();
        for len in [60, 5, 20] {
            let old = Element::new_item_with_flags(vec![7; 20], unowned_flags(0));
            let new = Element::new_item_with_flags(vec![8; len], owned_flags(2, NEW_OWNER));
            let db = grove_with(&old, TreeType::NormalTree, grove_version);
            let cost = apply(&db, vec![write_op(&new)], Mode::Replacing, grove_version)
                .expect("expected the update to apply");
            assert_eq!(
                storage_flags_of(&stored(&db, grove_version)).owner_id(),
                Some(&NEW_OWNER),
                "{len} bytes"
            );
            assert!(
                cost.storage_cost.added_bytes
                    < key_bytes() + value_bytes(&new, TreeType::NormalTree, grove_version),
                "{len} bytes: not every byte is added"
            );
        }
    }

    /// A write that keeps the stored flags brings no owner of its own, so a
    /// AccountAsReplacement answer for it is refused, whatever the callback does with the
    /// flags: an untrusted refresh, which writes the stored reference's flags
    /// back, even when the callback rewrites them to another owner of the
    /// same length (in a subtree and at the grove's root), and a flagged
    /// tree's root update when a write under it propagates. The estimators
    /// charge neither as a replacement, so neither may be one. A write that
    /// brings flags of its own still may be one.
    #[test]
    fn a_write_that_keeps_the_stored_flags_cannot_be_a_replacement() {
        use grovedb_costs::storage_cost::transition::ElementFlagsUpdate;

        let grove_version = GroveVersion::latest();
        let assert_refused = |error: Error| {
            assert!(
                matches!(error, Error::InvalidBatchOperation(reason) if reason.contains("are the stored flags")),
                "unexpected error: {error}"
            );
        };
        let rewriting_to_a_new_owner = |_cost: &grovedb_costs::storage_cost::StorageCost,
                                        _old: Option<Vec<u8>>,
                                        new: &mut Vec<u8>| {
            *new = owned_flags(0, NEW_OWNER).expect("owned flags");
            Ok(ElementFlagsUpdate::AccountAsReplacement)
        };
        let always_replacing =
            |_cost: &grovedb_costs::storage_cost::StorageCost,
             _old: Option<Vec<u8>>,
             _new: &mut Vec<u8>| { Ok(ElementFlagsUpdate::AccountAsReplacement) };
        assert_eq!(
            owned_flags(0, OLD_OWNER).map(|flags| flags.len()),
            owned_flags(0, NEW_OWNER).map(|flags| flags.len())
        );

        // An untrusted refresh of a reference owned by the old owner.
        let old = Element::new_reference_with_flags(target_path(), owned_flags(0, OLD_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_refused(
            db.apply_batch_with_element_flags_update(
                vec![QualifiedGroveDbOp::refresh_reference_op(
                    vec![b"tree".to_vec()],
                    KEY.to_vec(),
                    target_path(),
                    None,
                    owned_flags(0, NEW_OWNER),
                    false,
                    false,
                )],
                None,
                rewriting_to_a_new_owner,
                split_removal_bytes,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("an untrusted refresh must not be a replacement"),
        );
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_hash
        );

        // The same refresh of a reference at the grove's root.
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        db.insert(
            EMPTY_PATH,
            b"root reference",
            Element::new_reference_with_flags(target_path(), owned_flags(0, OLD_OWNER)),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the root reference");
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_refused(
            db.apply_batch_with_element_flags_update(
                vec![QualifiedGroveDbOp::refresh_reference_op(
                    vec![],
                    b"root reference".to_vec(),
                    target_path(),
                    None,
                    owned_flags(0, NEW_OWNER),
                    false,
                    false,
                )],
                None,
                rewriting_to_a_new_owner,
                split_removal_bytes,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("an untrusted refresh at the root must not be a replacement"),
        );
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_hash
        );

        // A flagged tree whose root a write under it updates.
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"flagged",
            Element::empty_tree_with_flags(owned_flags(0, OLD_OWNER)),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert the flagged tree");
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_refused(
            db.apply_batch_with_element_flags_update(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![b"flagged".to_vec()],
                    b"child".to_vec(),
                    Element::new_item(vec![1]),
                )],
                None,
                always_replacing,
                split_removal_bytes,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("a tree's root update must not be a replacement"),
        );
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_hash
        );

        // A write bringing flags of its own is accepted.
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 20], owned_flags(0, NEW_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        db.apply_batch_with_element_flags_update(
            vec![write_op(&new)],
            None,
            always_replacing,
            split_removal_bytes,
            None,
            grove_version,
        )
        .unwrap()
        .expect("a write bringing its own flags is accepted");

        // A write bringing flags byte-equal to the stored ones cannot be told
        // from one keeping them, and hands nothing over.
        let same_flags = Element::new_item_with_flags(vec![8; 20], owned_flags(0, OLD_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        assert_refused(
            db.apply_batch_with_element_flags_update(
                vec![write_op(&same_flags)],
                None,
                always_replacing,
                split_removal_bytes,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("a write with the stored flags must not be a replacement"),
        );
    }

    /// A reference written in the same batch commits to the bytes the
    /// replacing element stores: its new flags, not the old owner's.
    #[test]
    fn a_reference_in_the_same_batch_commits_to_the_replacing_element() {
        let grove_version = GroveVersion::latest();
        let cases = [
            (
                Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER)),
                Element::new_item_with_flags(vec![8; 20], owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
            (
                Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER)),
                Element::new_item_with_flags(vec![8; 70], owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
            (
                Element::new_sum_item_with_flags(5, owned_flags(0, OLD_OWNER)),
                Element::new_sum_item_with_flags(900, owned_flags(2, NEW_OWNER)),
                TreeType::SumTree,
            ),
            (
                Element::new_item_with_sum_item_with_flags(
                    vec![7; 20],
                    5,
                    owned_flags(0, OLD_OWNER),
                ),
                Element::new_item_with_sum_item_with_flags(
                    vec![8; 20],
                    9,
                    owned_flags(2, NEW_OWNER),
                ),
                TreeType::SumTree,
            ),
        ];
        for (old, new, tree_type) in cases {
            let db = grove_with(&old, tree_type, grove_version);
            apply(
                &db,
                vec![
                    write_op(&new),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"targets".to_vec()],
                        b"reference".to_vec(),
                        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                            b"tree".to_vec(),
                            KEY.to_vec(),
                        ])),
                    ),
                ],
                Mode::Replacing,
                grove_version,
            )
            .expect("expected the batch to apply");
            assert_eq!(stored(&db, grove_version), new);
            let issues = db
                .visualize_verify_grovedb(None, true, false, grove_version)
                .expect("expected to verify the grove");
            assert!(issues.is_empty(), "reference issues: {issues:?}");
        }
    }
}
