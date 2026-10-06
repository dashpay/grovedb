//! Costs of an update in place that settles an owner change
//! (`BatchApplyOptions::settle_owner_changes`): the old element is removed
//! and its bytes sectioned to its owner as a deletion sections them, and the
//! new element is inserted and charged to the writer.

#[cfg(feature = "minimal")]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use grovedb_costs::{
        storage_cost::removal::StorageRemovedBytes::{NoStorageRemoval, SectionedStorageRemoval},
        OperationCost,
    };
    use grovedb_epoch_based_storage_flags::StorageFlags;
    use grovedb_merk::{
        element::{get::ElementFetchFromStorageExtensions, tree_type::ElementTreeTypeExtensions},
        estimated_costs::{
            average_case_costs::{
                EstimatedLayerCount::ApproximateElements,
                EstimatedLayerInformation,
                EstimatedLayerSizes::{AllItems, AllSubtrees},
                EstimatedSumTrees::NoSumTrees,
            },
            worst_case_costs::WorstCaseLayerInformation::MaxElementsNumber,
        },
        tree::kv::KV,
        tree_type::TreeType,
    };
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};
    use intmap::IntMap;

    use crate::batch::settle_test_support::{
        apply, merging_flags_update, options, owned_flags, settling_flags_update,
        split_removal_bytes, unowned_flags, value_bytes, write_op, Mode, KEY, NEW_OWNER, OLD_OWNER,
    };
    use crate::{
        batch::{
            estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
            BatchApplyOptions, KeyInfoPath, QualifiedGroveDbOp, SubelementsDeletionBehavior,
        },
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
        BackwardReferences, Element, Error, GroveDb,
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

    /// The element at `key1` as its Merk stores it, a backward-references
    /// element's referrer list included (public reads strip it).
    fn stored_in_merk(db: &TempGroveDb, grove_version: &GroveVersion) -> Element {
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(
                [b"tree".as_slice()].as_ref().into(),
                &tx,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected to open the tree");
        Element::get(&merk, KEY, true, grove_version)
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

    /// Writes `new` over `old` with the settling update, checks the update
    /// was accounted as the deletion of `old` plus the insertion of `new`,
    /// and returns the grove and the cost.
    fn assert_settles(
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
        let cost = apply(&db, vec![write_op(new)], Mode::Settling, grove_version)
            .expect("expected the settling update to apply");

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
    fn owner_change_on_growth_settles() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
        assert_settles(&old, &new, TreeType::NormalTree, grove_version);
    }

    #[test]
    fn owner_change_on_shrink_settles() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 60], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 5], owned_flags(2, NEW_OWNER));
        assert_settles(&old, &new, TreeType::NormalTree, grove_version);
    }

    #[test]
    fn owner_change_at_the_same_size_settles() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 20], owned_flags(2, NEW_OWNER));
        assert_eq!(
            value_bytes(&old, TreeType::NormalTree, grove_version),
            value_bytes(&new, TreeType::NormalTree, grove_version)
        );
        assert_settles(&old, &new, TreeType::NormalTree, grove_version);
    }

    /// An owner change in the old element's own epoch settles too: the old
    /// owner gets back every byte they paid for.
    #[test]
    fn owner_change_in_the_same_epoch_settles() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 30], owned_flags(0, NEW_OWNER));
        assert_settles(&old, &new, TreeType::NormalTree, grove_version);
    }

    /// Settling owner changes does not depend on the grove version: on grove
    /// v1..v3, whose Merk runs version 0 of the just-in-time value update,
    /// an owner change settles exactly as it does on the latest version.
    #[test]
    fn owner_changes_settle_on_grove_v3() {
        let grove_version = &GROVE_V3;
        let item = |len: usize, epoch, owner| {
            Element::new_item_with_flags(vec![8; len], owned_flags(epoch, owner))
        };
        for (old, new, tree_type) in [
            (
                item(20, 0, OLD_OWNER),
                item(60, 2, NEW_OWNER),
                TreeType::NormalTree,
            ),
            (
                item(60, 0, OLD_OWNER),
                item(5, 2, NEW_OWNER),
                TreeType::NormalTree,
            ),
            (
                item(20, 0, OLD_OWNER),
                item(20, 2, NEW_OWNER),
                TreeType::NormalTree,
            ),
            (
                item(20, 0, OLD_OWNER),
                item(30, 0, NEW_OWNER),
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
                    vec![8; 45],
                    9,
                    owned_flags(2, NEW_OWNER),
                ),
                TreeType::SumTree,
            ),
        ] {
            assert_settles(&old, &new, tree_type, grove_version);
        }
    }

    #[test]
    fn owner_change_of_multi_epoch_flags_refunds_every_epoch() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        // the old owner grows it in epoch 1 and again in epoch 2: the same
        // owner merges as before
        for (epoch, len) in [(1, 50), (2, 90)] {
            apply(
                &db,
                vec![write_op(&Element::new_item_with_flags(
                    vec![7; len],
                    owned_flags(epoch, OLD_OWNER),
                ))],
                Mode::Settling,
                grove_version,
            )
            .expect("expected the old owner to grow the element");
        }
        let grown = stored(&db, grove_version);
        let StorageFlags::MultiEpochOwned(0, epochs, OLD_OWNER) = storage_flags_of(&grown) else {
            panic!("expected multi epoch flags of the old owner");
        };
        assert_eq!(epochs.keys().copied().collect::<Vec<_>>(), vec![1, 2]);

        let new = Element::new_item_with_flags(vec![8; 40], owned_flags(4, NEW_OWNER));
        let (_, cost) = assert_settles(&grown, &new, TreeType::NormalTree, grove_version);
        // every epoch the old owner paid in is refunded
        assert_eq!(
            removed_from(&cost, OLD_OWNER)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    /// Owner changes of the other element types: an item with a sum item,
    /// a sum item, a reference, a reference with a sum item and a tree.
    #[test]
    fn owner_changes_of_every_flagged_element_type_settle() {
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
            assert_settles(&old, &new, tree_type, grove_version);
        }
    }

    /// A backward-references item keeps the referrers registered on the item
    /// it replaces, and a settled write adds their entries too. The
    /// worst-case estimate covers them through the item's declared capacity,
    /// wherever the referrers sit; the average case charges the typical
    /// shape, which covers referrers beside the item.
    #[test]
    fn estimates_cover_the_referrers_a_settled_item_carries_over() {
        let grove_version = GroveVersion::latest();
        let family_item = |value: Vec<u8>, flags| {
            Element::ItemWithBackwardsReferences(
                value,
                BackwardReferences::with_max_incoming(4),
                flags,
            )
        };
        let old = family_item(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = family_item(vec![8; 20], owned_flags(2, NEW_OWNER));
        let siblings = |db: &TempGroveDb| {
            for referrer in [b"r1".as_slice(), b"r2", b"r3"] {
                db.insert(
                    [b"tree".as_slice()].as_ref(),
                    referrer,
                    Element::new_bidirectional_reference(ReferencePathType::SiblingReference(
                        KEY.to_vec(),
                    )),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("expected to register a referrer");
            }
        };
        // Referrers far from the item under long keys, whose entries hold
        // absolute paths much longer than the item's own.
        let distant = |db: &TempGroveDb| {
            let (outer, inner) = (vec![b'x'; 200], vec![b'y'; 200]);
            db.insert(
                [b"targets".as_slice()].as_ref(),
                &outer,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected to insert a tree");
            db.insert(
                [b"targets".as_slice(), outer.as_slice()].as_ref(),
                &inner,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected to insert a tree");
            for i in 0..3u8 {
                let mut referrer = vec![b'z'; 199];
                referrer.push(i);
                db.insert(
                    [b"targets".as_slice(), outer.as_slice(), inner.as_slice()].as_ref(),
                    &referrer,
                    Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(
                        vec![b"tree".to_vec(), KEY.to_vec()],
                    )),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("expected to register a referrer");
            }
        };
        let placements: [(&str, &dyn Fn(&TempGroveDb)); 2] =
            [("siblings", &siblings), ("distant", &distant)];
        for (placement, register_referrers) in placements {
            let db = grove_with(&old, TreeType::NormalTree, grove_version);
            register_referrers(&db);
            let applied = apply(&db, vec![write_op(&new)], Mode::Settling, grove_version)
                .expect("expected the settling update to apply");
            let landed = stored_in_merk(&db, grove_version);
            assert_eq!(
                landed.backward_references().map(|refs| refs.len()),
                Some(3),
                "{placement}: the item keeps its referrers"
            );
            assert_eq!(
                storage_flags_of(&landed),
                StorageFlags::SingleEpochOwned(2, NEW_OWNER)
            );
            // the referrer entries are part of what the settled apply adds
            assert_eq!(
                applied.storage_cost.added_bytes,
                key_bytes() + value_bytes(&landed, TreeType::NormalTree, grove_version)
            );
            assert!(
                applied.storage_cost.added_bytes
                    > key_bytes() + value_bytes(&new, TreeType::NormalTree, grove_version)
            );
            let worst_case = estimate(
                vec![write_op(&new)],
                Some(options(Mode::Settling)),
                None,
                grove_version,
            );
            assert!(
                worst_case.storage_cost.added_bytes >= applied.storage_cost.added_bytes,
                "{placement}: worst-case estimate {worst_case:?} adds less than the settled \
                 apply {applied:?}"
            );
            // The average case charges the typical referrer shape: it covers
            // referrers beside the item, and stays below the worst case.
            let average_case = estimate(
                vec![write_op(&new)],
                Some(options(Mode::Settling)),
                Some(TreeType::NormalTree),
                grove_version,
            );
            assert!(
                average_case.storage_cost.added_bytes < worst_case.storage_cost.added_bytes,
                "{placement}: average-case estimate {average_case:?} is not below the worst \
                 case {worst_case:?}"
            );
            if placement == "siblings" {
                assert!(
                    average_case.storage_cost.added_bytes >= applied.storage_cost.added_bytes,
                    "{placement}: average-case estimate {average_case:?} adds less than the \
                     settled apply {applied:?}"
                );
            }
        }
    }

    /// A partial batch settles an owner change as a full batch does, and
    /// hands its add-on callback the settled cost.
    #[test]
    fn a_partial_batch_settles_an_owner_change() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
        let (full_db, full_cost) = assert_settles(&old, &new, TreeType::NormalTree, grove_version);

        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        let mut initial_cost = None;
        let cost = db
            .apply_partial_batch_with_element_flags_update(
                vec![write_op(&new)],
                Some(options(Mode::Settling)),
                settling_flags_update,
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
        let settling_db = grove_with(old, tree_type, grove_version);
        let settling = apply(
            &settling_db,
            vec![write_op(new)],
            Mode::Settling,
            grove_version,
        )
        .expect("expected the settling update to apply");
        assert_eq!(settling, merging);
        assert_eq!(
            stored(&settling_db, grove_version),
            stored(&merging_db, grove_version)
        );
        assert_eq!(
            settling_db.root_hash(None, grove_version).unwrap().unwrap(),
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
            (unowned_flags(0), owned_flags(2, NEW_OWNER)),
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

    /// Without the option, an owner change is merged into the new owner's
    /// flags as it always was: the new owner takes the old epochs and pays
    /// only the growth, and the old owner is refunded nothing.
    #[test]
    fn owner_change_without_the_option_merges_as_before() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        let cost = apply(&db, vec![write_op(&new)], Mode::Merging, grove_version)
            .expect("expected the merging update to apply");
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
        let StorageFlags::MultiEpochOwned(0, epochs, NEW_OWNER) =
            storage_flags_of(&stored(&db, grove_version))
        else {
            panic!("expected the new owner to take the old owner's epochs");
        };
        assert_eq!(epochs.keys().copied().collect::<Vec<_>>(), vec![2]);
        let stored_element = stored(&db, grove_version);
        assert_eq!(
            cost.storage_cost.added_bytes,
            value_bytes(&stored_element, TreeType::NormalTree, grove_version)
                - value_bytes(&old, TreeType::NormalTree, grove_version)
        );
    }

    /// The option alone changes nothing: a flags update that never settles
    /// is applied exactly as without it.
    #[test]
    fn the_option_alone_changes_nothing() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        for len in [60, 5, 20] {
            let new = Element::new_item_with_flags(vec![8; len], owned_flags(2, NEW_OWNER));
            let mut costs = vec![];
            let mut hashes = vec![];
            for settle_owner_changes in [false, true] {
                let db = grove_with(&old, TreeType::NormalTree, grove_version);
                costs.push(
                    db.apply_batch_with_element_flags_update(
                        vec![write_op(&new)],
                        Some(BatchApplyOptions {
                            settle_owner_changes,
                            ..Default::default()
                        }),
                        merging_flags_update,
                        split_removal_bytes,
                        None,
                        grove_version,
                    )
                    .cost_as_result()
                    .expect("expected the update to apply"),
                );
                hashes.push(db.root_hash(None, grove_version).unwrap().unwrap());
            }
            assert_eq!(costs[0], costs[1]);
            assert_eq!(hashes[0], hashes[1]);
        }
    }

    /// A settling answer without the option refuses the batch as an invalid
    /// batch, whether the apply meets it first, a same-batch reference's
    /// prediction does, or a partial batch's initial segment does.
    #[test]
    fn a_settling_flags_update_is_refused_without_the_option() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
        let reference = QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"targets".to_vec()],
            b"reference".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"tree".to_vec(),
                KEY.to_vec(),
            ])),
        );
        let assert_refused = |error: Error| {
            assert!(
                matches!(error, Error::InvalidBatchOperation(reason) if reason.contains("settle_owner_changes")),
                "unexpected error: {error}"
            );
        };
        for ops in [vec![write_op(&new)], vec![write_op(&new), reference]] {
            let db = grove_with(&old, TreeType::NormalTree, grove_version);
            let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
            assert_refused(
                db.apply_batch_with_element_flags_update(
                    ops,
                    None,
                    settling_flags_update,
                    split_removal_bytes,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect_err("expected the settling answer to be refused"),
            );
            assert_eq!(
                db.root_hash(None, grove_version).unwrap().unwrap(),
                root_hash
            );
        }
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        assert_refused(
            db.apply_partial_batch_with_element_flags_update(
                vec![write_op(&new)],
                None,
                settling_flags_update,
                split_removal_bytes,
                |_cost, _leftover_operations| Ok(vec![]),
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("expected the settling answer to be refused"),
        );
    }

    /// A write that keeps the stored flags brings no owner of its own, so a
    /// settling answer for it is refused, whatever the callback does with the
    /// flags: an untrusted refresh, which writes the stored reference's flags
    /// back, even when the callback rewrites them to another owner of the
    /// same length (in a subtree and at the grove's root), and a flagged
    /// tree's root update when a write under it propagates. The estimators
    /// charge neither a settlement, so neither may settle. A write that
    /// brings flags of its own still settles.
    #[test]
    fn a_write_that_keeps_the_stored_flags_cannot_settle() {
        use grovedb_costs::storage_cost::transition::ElementFlagsUpdate;

        let grove_version = GroveVersion::latest();
        let assert_refused = |error: Error| {
            assert!(
                matches!(error, Error::InvalidBatchOperation(reason) if reason.contains("keeps the stored flags")),
                "unexpected error: {error}"
            );
        };
        let rewriting_to_a_new_owner = |_cost: &grovedb_costs::storage_cost::StorageCost,
                                        _old: Option<Vec<u8>>,
                                        new: &mut Vec<u8>| {
            *new = owned_flags(0, NEW_OWNER).expect("owned flags");
            Ok(ElementFlagsUpdate::SettleOwnerChange)
        };
        let always_settling =
            |_cost: &grovedb_costs::storage_cost::StorageCost,
             _old: Option<Vec<u8>>,
             _new: &mut Vec<u8>| { Ok(ElementFlagsUpdate::SettleOwnerChange) };
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
                Some(options(Mode::Settling)),
                rewriting_to_a_new_owner,
                split_removal_bytes,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("an untrusted refresh must not settle"),
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
                Some(options(Mode::Settling)),
                rewriting_to_a_new_owner,
                split_removal_bytes,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("an untrusted refresh at the root must not settle"),
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
                Some(options(Mode::Settling)),
                always_settling,
                split_removal_bytes,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("a tree's root update must not settle"),
        );
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_hash
        );

        // A write bringing flags of its own settles.
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 20], owned_flags(0, NEW_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        db.apply_batch_with_element_flags_update(
            vec![write_op(&new)],
            Some(options(Mode::Settling)),
            always_settling,
            split_removal_bytes,
            None,
            grove_version,
        )
        .unwrap()
        .expect("a write bringing its own flags settles");
    }

    /// A reference written in the same batch commits to the bytes the
    /// settled element stores: its new flags, not the old owner's, on the
    /// latest grove version and on grove v1..v3 alike.
    #[test]
    fn a_reference_in_the_same_batch_commits_to_the_settled_element() {
        for grove_version in [GroveVersion::latest(), &GROVE_V3] {
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
                    Mode::Settling,
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

    fn average_case_layers(tree_type: TreeType) -> HashMap<KeyInfoPath, EstimatedLayerInformation> {
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: ApproximateElements(2),
                estimated_layer_sizes: AllSubtrees(7, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_path([b"tree".as_slice()]),
            EstimatedLayerInformation {
                tree_type,
                estimated_layer_count: ApproximateElements(2),
                estimated_layer_sizes: AllItems(4, 100, Some(35)),
            },
        );
        paths
    }

    fn worst_case_layers() -> HashMap<
        KeyInfoPath,
        grovedb_merk::estimated_costs::worst_case_costs::WorstCaseLayerInformation,
    > {
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(2));
        paths.insert(
            KeyInfoPath::from_known_path([b"tree".as_slice()]),
            MaxElementsNumber(2),
        );
        paths
    }

    fn estimate(
        ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        average_case_tree_type: Option<TreeType>,
        grove_version: &GroveVersion,
    ) -> OperationCost {
        let estimated_costs_type = match average_case_tree_type {
            Some(tree_type) => AverageCaseCostsType(average_case_layers(tree_type)),
            None => WorstCaseCostsType(worst_case_layers()),
        };
        GroveDb::estimated_case_operations_for_batch(
            estimated_costs_type,
            ops,
            batch_apply_options,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected an estimate")
    }

    /// Updates of every element kind an estimate may see settle: the old
    /// element, the new one, and the tree they are in.
    fn estimate_cases() -> [(Element, Element, TreeType); 7] {
        [
            (
                Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER)),
                Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
            (
                Element::new_item_with_flags(vec![7; 60], owned_flags(0, OLD_OWNER)),
                Element::new_item_with_flags(vec![8; 5], owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
            (
                Element::new_item_with_sum_item_with_flags(
                    vec![7; 20],
                    5,
                    owned_flags(0, OLD_OWNER),
                ),
                Element::new_item_with_sum_item_with_flags(
                    vec![8; 45],
                    1,
                    owned_flags(2, NEW_OWNER),
                ),
                TreeType::SumTree,
            ),
            (
                Element::new_sum_item_with_flags(5, owned_flags(0, OLD_OWNER)),
                Element::new_sum_item_with_flags(1, owned_flags(2, NEW_OWNER)),
                TreeType::SumTree,
            ),
            (
                Element::new_reference_with_flags(target_path(), owned_flags(0, OLD_OWNER)),
                Element::new_reference_with_flags(long_target_path(), owned_flags(2, NEW_OWNER)),
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
                    1,
                    owned_flags(2, NEW_OWNER),
                ),
                TreeType::SumTree,
            ),
            (
                Element::empty_tree_with_flags(owned_flags(0, OLD_OWNER)),
                Element::empty_tree_with_flags(owned_flags(2, NEW_OWNER)),
                TreeType::NormalTree,
            ),
        ]
    }

    /// Every write of `new` at `key1` that may settle an owner change.
    fn settling_writes(new: &Element) -> Vec<(&'static str, QualifiedGroveDbOp)> {
        let path = vec![b"tree".to_vec()];
        let mut ops = vec![
            ("insert_or_replace", write_op(new)),
            (
                "insert_or_replace_dont_check",
                write_op(new).dont_check_for_backwards_references(),
            ),
            (
                "replace",
                QualifiedGroveDbOp::replace_op(path.clone(), KEY.to_vec(), new.clone()),
            ),
            (
                "replace_dont_check",
                QualifiedGroveDbOp::replace_op(path.clone(), KEY.to_vec(), new.clone())
                    .dont_check_for_backwards_references(),
            ),
        ];
        if let Element::Item(..) = new {
            ops.push((
                "patch",
                QualifiedGroveDbOp::patch_op(path.clone(), KEY.to_vec(), new.clone(), 0),
            ));
            ops.push((
                "patch_dont_check",
                QualifiedGroveDbOp::patch_op(path.clone(), KEY.to_vec(), new.clone(), 0)
                    .dont_check_for_backwards_references(),
            ));
        }
        if let Element::Reference(reference_path, max_hop, flags) = new {
            ops.push((
                "refresh_trusted",
                QualifiedGroveDbOp::refresh_reference_op(
                    path,
                    KEY.to_vec(),
                    reference_path.clone(),
                    *max_hop,
                    flags.clone(),
                    false,
                    true,
                ),
            ));
        }
        ops
    }

    /// With the option, the average-case and worst-case estimates of every
    /// write that may settle an owner change are never below the settled
    /// apply of that same write.
    #[test]
    fn estimates_with_the_option_cover_a_settled_owner_change() {
        let grove_version = GroveVersion::latest();
        for (old, new, tree_type) in estimate_cases() {
            for (label, op) in settling_writes(&new) {
                let db = grove_with(&old, tree_type, grove_version);
                let applied = apply(&db, vec![op.clone()], Mode::Settling, grove_version)
                    .unwrap_or_else(|e| panic!("expected {label} of {new:?} to apply: {e}"));
                for average_case_tree_type in [Some(tree_type), None] {
                    let settling = estimate(
                        vec![op.clone()],
                        Some(options(Mode::Settling)),
                        average_case_tree_type,
                        grove_version,
                    );
                    assert!(
                        settling
                            .storage_cost
                            .worse_or_eq_than(&applied.storage_cost),
                        "estimate {settling:?} of {label} of {new:?} is below the settled apply \
                         {applied:?}"
                    );
                }
            }
        }
    }

    /// The estimates of those writes without the option on the latest grove
    /// version, as they were before it existed: (case, write, average case,
    /// seeks, added bytes, replaced bytes, loaded bytes, hash calls), taken
    /// from develop at a720f7c1. The worst-case rows whose replaced bytes
    /// saturate at `u32::MAX` still pin every other figure.
    const ESTIMATES_BEFORE_THE_OPTION: &[(usize, &str, bool, u32, u32, u32, u64, u32)] = &[
        (0, "insert_or_replace", true, 34, 243, 4289, 6170, 79),
        (
            0,
            "insert_or_replace",
            false,
            506892,
            243,
            4294967295,
            27857252525,
            87703315,
        ),
        (
            0,
            "insert_or_replace_dont_check",
            true,
            13,
            243,
            1047,
            1379,
            23,
        ),
        (
            0,
            "insert_or_replace_dont_check",
            false,
            12,
            243,
            409674,
            394925,
            275,
        ),
        (0, "replace", true, 34, 0, 4568, 6170, 80),
        (
            0,
            "replace",
            false,
            506892,
            206,
            4294967295,
            27857252525,
            87703315,
        ),
        (0, "replace_dont_check", true, 13, 0, 1326, 1379, 24),
        (0, "replace_dont_check", false, 12, 206, 409711, 394925, 275),
        (0, "patch", true, 34, 0, 4568, 6170, 80),
        (
            0,
            "patch",
            false,
            506892,
            206,
            4294967295,
            27857252525,
            87703315,
        ),
        (0, "patch_dont_check", true, 13, 0, 1326, 1379, 24),
        (0, "patch_dont_check", false, 12, 206, 409711, 394925, 275),
        (1, "insert_or_replace", true, 34, 187, 4289, 6170, 78),
        (
            1,
            "insert_or_replace",
            false,
            506892,
            187,
            4294967295,
            27857252525,
            87703314,
        ),
        (
            1,
            "insert_or_replace_dont_check",
            true,
            13,
            187,
            1047,
            1379,
            22,
        ),
        (
            1,
            "insert_or_replace_dont_check",
            false,
            12,
            187,
            409674,
            394925,
            274,
        ),
        (1, "replace", true, 34, 0, 4513, 6170, 79),
        (
            1,
            "replace",
            false,
            506892,
            150,
            4294967295,
            27857252525,
            87703314,
        ),
        (1, "replace_dont_check", true, 13, 0, 1271, 1379, 23),
        (1, "replace_dont_check", false, 12, 150, 409711, 394925, 274),
        (1, "patch", true, 34, 0, 4513, 6170, 79),
        (
            1,
            "patch",
            false,
            506892,
            150,
            4294967295,
            27857252525,
            87703314,
        ),
        (1, "patch_dont_check", true, 13, 0, 1271, 1379, 23),
        (1, "patch_dont_check", false, 12, 150, 409711, 394925, 274),
        (2, "insert_or_replace", true, 34, 245, 4545, 6578, 79),
        (
            2,
            "insert_or_replace",
            false,
            506892,
            229,
            4294967295,
            27857252525,
            87703315,
        ),
        (
            2,
            "insert_or_replace_dont_check",
            true,
            13,
            245,
            1095,
            1451,
            23,
        ),
        (
            2,
            "insert_or_replace_dont_check",
            false,
            12,
            229,
            409674,
            394925,
            275,
        ),
        (2, "replace", true, 34, 0, 4790, 6578, 79),
        (
            2,
            "replace",
            false,
            506892,
            192,
            4294967295,
            27857252525,
            87703315,
        ),
        (2, "replace_dont_check", true, 13, 0, 1340, 1451, 23),
        (2, "replace_dont_check", false, 12, 192, 409711, 394925, 275),
        (3, "insert_or_replace", true, 34, 198, 4545, 6578, 78),
        (
            3,
            "insert_or_replace",
            false,
            506892,
            182,
            4294967295,
            27857252525,
            87703314,
        ),
        (
            3,
            "insert_or_replace_dont_check",
            true,
            13,
            198,
            1095,
            1451,
            22,
        ),
        (
            3,
            "insert_or_replace_dont_check",
            false,
            12,
            182,
            409674,
            394925,
            274,
        ),
        (3, "replace", true, 34, 0, 4751, 6578, 78),
        (
            3,
            "replace",
            false,
            506892,
            0,
            4294967295,
            27857252525,
            87703314,
        ),
        (3, "replace_dont_check", true, 13, 0, 1301, 1451, 22),
        (3, "replace_dont_check", false, 12, 0, 409864, 394925, 274),
        (4, "insert_or_replace", true, 34, 223, 4289, 6170, 79),
        (
            4,
            "insert_or_replace",
            false,
            506892,
            223,
            4294967295,
            27857252525,
            87703315,
        ),
        (
            4,
            "insert_or_replace_dont_check",
            true,
            13,
            223,
            1047,
            1379,
            23,
        ),
        (
            4,
            "insert_or_replace_dont_check",
            false,
            12,
            223,
            409674,
            394925,
            275,
        ),
        (4, "replace", true, 34, 0, 4512, 6170, 79),
        (
            4,
            "replace",
            false,
            506892,
            186,
            4294967295,
            27857252525,
            87703315,
        ),
        (4, "replace_dont_check", true, 13, 0, 1270, 1379, 23),
        (4, "replace_dont_check", false, 12, 186, 409711, 394925, 275),
        (4, "refresh_trusted", true, 13, 0, 1270, 1379, 23),
        (4, "refresh_trusted", false, 12, 186, 409711, 394925, 275),
        (5, "insert_or_replace", true, 34, 240, 4545, 6578, 79),
        (
            5,
            "insert_or_replace",
            false,
            506892,
            224,
            4294967295,
            27857252525,
            87703315,
        ),
        (
            5,
            "insert_or_replace_dont_check",
            true,
            13,
            240,
            1095,
            1451,
            23,
        ),
        (
            5,
            "insert_or_replace_dont_check",
            false,
            12,
            224,
            409674,
            394925,
            275,
        ),
        (5, "replace", true, 34, 0, 4785, 6578, 79),
        (
            5,
            "replace",
            false,
            506892,
            187,
            4294967295,
            27857252525,
            87703315,
        ),
        (5, "replace_dont_check", true, 13, 0, 1335, 1451, 23),
        (5, "replace_dont_check", false, 12, 187, 409711, 394925, 275),
        (6, "insert_or_replace", true, 34, 151, 4289, 6170, 80),
        (
            6,
            "insert_or_replace",
            false,
            506892,
            151,
            4294967295,
            27857252525,
            87703316,
        ),
        (
            6,
            "insert_or_replace_dont_check",
            true,
            13,
            151,
            1047,
            1379,
            24,
        ),
        (
            6,
            "insert_or_replace_dont_check",
            false,
            12,
            151,
            409674,
            394925,
            276,
        ),
        (6, "replace", true, 34, 0, 4440, 6170, 80),
        (
            6,
            "replace",
            false,
            506892,
            0,
            4294967295,
            27857252525,
            87703316,
        ),
        (6, "replace_dont_check", true, 13, 0, 1198, 1379, 24),
        (6, "replace_dont_check", false, 12, 0, 409825, 394925, 276),
    ];

    /// The same estimates on grove v3 (no backward-references fan-out),
    /// taken from develop at a720f7c1.
    const GROVE_V3_ESTIMATES_BEFORE_THE_OPTION: &[(usize, &str, bool, u32, u32, u32, u64, u32)] = &[
        (0, "insert_or_replace", true, 13, 243, 1047, 1379, 23),
        (0, "insert_or_replace", false, 12, 243, 409674, 394925, 275),
        (
            0,
            "insert_or_replace_dont_check",
            true,
            13,
            243,
            1047,
            1379,
            23,
        ),
        (
            0,
            "insert_or_replace_dont_check",
            false,
            12,
            243,
            409674,
            394925,
            275,
        ),
        (0, "replace", true, 13, 0, 1326, 1379, 24),
        (0, "replace", false, 12, 206, 409711, 394925, 275),
        (0, "replace_dont_check", true, 13, 0, 1326, 1379, 24),
        (0, "replace_dont_check", false, 12, 206, 409711, 394925, 275),
        (0, "patch", true, 13, 0, 1326, 1379, 24),
        (0, "patch", false, 12, 206, 409711, 394925, 275),
        (0, "patch_dont_check", true, 13, 0, 1326, 1379, 24),
        (0, "patch_dont_check", false, 12, 206, 409711, 394925, 275),
        (1, "insert_or_replace", true, 13, 187, 1047, 1379, 22),
        (1, "insert_or_replace", false, 12, 187, 409674, 394925, 274),
        (
            1,
            "insert_or_replace_dont_check",
            true,
            13,
            187,
            1047,
            1379,
            22,
        ),
        (
            1,
            "insert_or_replace_dont_check",
            false,
            12,
            187,
            409674,
            394925,
            274,
        ),
        (1, "replace", true, 13, 0, 1271, 1379, 23),
        (1, "replace", false, 12, 150, 409711, 394925, 274),
        (1, "replace_dont_check", true, 13, 0, 1271, 1379, 23),
        (1, "replace_dont_check", false, 12, 150, 409711, 394925, 274),
        (1, "patch", true, 13, 0, 1271, 1379, 23),
        (1, "patch", false, 12, 150, 409711, 394925, 274),
        (1, "patch_dont_check", true, 13, 0, 1271, 1379, 23),
        (1, "patch_dont_check", false, 12, 150, 409711, 394925, 274),
        (2, "insert_or_replace", true, 13, 245, 1095, 1451, 23),
        (2, "insert_or_replace", false, 12, 229, 409674, 394925, 275),
        (
            2,
            "insert_or_replace_dont_check",
            true,
            13,
            245,
            1095,
            1451,
            23,
        ),
        (
            2,
            "insert_or_replace_dont_check",
            false,
            12,
            229,
            409674,
            394925,
            275,
        ),
        (2, "replace", true, 13, 0, 1340, 1451, 23),
        (2, "replace", false, 12, 192, 409711, 394925, 275),
        (2, "replace_dont_check", true, 13, 0, 1340, 1451, 23),
        (2, "replace_dont_check", false, 12, 192, 409711, 394925, 275),
        (3, "insert_or_replace", true, 13, 198, 1095, 1451, 22),
        (3, "insert_or_replace", false, 12, 182, 409674, 394925, 274),
        (
            3,
            "insert_or_replace_dont_check",
            true,
            13,
            198,
            1095,
            1451,
            22,
        ),
        (
            3,
            "insert_or_replace_dont_check",
            false,
            12,
            182,
            409674,
            394925,
            274,
        ),
        (3, "replace", true, 13, 0, 1301, 1451, 22),
        (3, "replace", false, 12, 0, 409864, 394925, 274),
        (3, "replace_dont_check", true, 13, 0, 1301, 1451, 22),
        (3, "replace_dont_check", false, 12, 0, 409864, 394925, 274),
        (4, "insert_or_replace", true, 13, 223, 1047, 1379, 23),
        (4, "insert_or_replace", false, 12, 223, 409674, 394925, 275),
        (
            4,
            "insert_or_replace_dont_check",
            true,
            13,
            223,
            1047,
            1379,
            23,
        ),
        (
            4,
            "insert_or_replace_dont_check",
            false,
            12,
            223,
            409674,
            394925,
            275,
        ),
        (4, "replace", true, 13, 0, 1270, 1379, 23),
        (4, "replace", false, 12, 186, 409711, 394925, 275),
        (4, "replace_dont_check", true, 13, 0, 1270, 1379, 23),
        (4, "replace_dont_check", false, 12, 186, 409711, 394925, 275),
        (4, "refresh_trusted", true, 13, 0, 1270, 1379, 23),
        (4, "refresh_trusted", false, 12, 186, 409711, 394925, 275),
        (5, "insert_or_replace", true, 13, 240, 1095, 1451, 23),
        (5, "insert_or_replace", false, 12, 224, 409674, 394925, 275),
        (
            5,
            "insert_or_replace_dont_check",
            true,
            13,
            240,
            1095,
            1451,
            23,
        ),
        (
            5,
            "insert_or_replace_dont_check",
            false,
            12,
            224,
            409674,
            394925,
            275,
        ),
        (5, "replace", true, 13, 0, 1335, 1451, 23),
        (5, "replace", false, 12, 187, 409711, 394925, 275),
        (5, "replace_dont_check", true, 13, 0, 1335, 1451, 23),
        (5, "replace_dont_check", false, 12, 187, 409711, 394925, 275),
        (6, "insert_or_replace", true, 13, 151, 1047, 1379, 24),
        (6, "insert_or_replace", false, 12, 151, 409674, 394925, 276),
        (
            6,
            "insert_or_replace_dont_check",
            true,
            13,
            151,
            1047,
            1379,
            24,
        ),
        (
            6,
            "insert_or_replace_dont_check",
            false,
            12,
            151,
            409674,
            394925,
            276,
        ),
        (6, "replace", true, 13, 0, 1198, 1379, 24),
        (6, "replace", false, 12, 0, 409825, 394925, 276),
        (6, "replace_dont_check", true, 13, 0, 1198, 1379, 24),
        (6, "replace_dont_check", false, 12, 0, 409825, 394925, 276),
    ];

    /// Without the option, every estimate is what it was before the option
    /// existed, on the latest grove version and on grove v3.
    #[test]
    fn estimates_without_the_option_are_unchanged() {
        for (grove_version, pins) in [
            (GroveVersion::latest(), ESTIMATES_BEFORE_THE_OPTION),
            (&GROVE_V3, GROVE_V3_ESTIMATES_BEFORE_THE_OPTION),
        ] {
            let mut pinned = pins.iter();
            for (case, (_, new, tree_type)) in estimate_cases().into_iter().enumerate() {
                for (label, op) in settling_writes(&new) {
                    for average_case in [true, false] {
                        let &(
                            pinned_case,
                            pinned_label,
                            pinned_average_case,
                            seek_count,
                            added_bytes,
                            replaced_bytes,
                            storage_loaded_bytes,
                            hash_node_calls,
                        ) = pinned.next().expect("expected a pinned estimate");
                        assert_eq!(
                            (pinned_case, pinned_label, pinned_average_case),
                            (case, label, average_case)
                        );
                        let before = OperationCost {
                            seek_count,
                            storage_cost: grovedb_costs::storage_cost::StorageCost {
                                added_bytes,
                                replaced_bytes,
                                removed_bytes: NoStorageRemoval,
                            },
                            storage_loaded_bytes,
                            hash_node_calls,
                            sinsemilla_hash_calls: 0,
                        };
                        for batch_apply_options in [None, Some(options(Mode::Merging))] {
                            assert_eq!(
                                estimate(
                                    vec![op.clone()],
                                    batch_apply_options,
                                    average_case.then_some(tree_type),
                                    grove_version,
                                ),
                                before,
                                "case {case}, {label}, average case {average_case}"
                            );
                        }
                    }
                }
            }
            assert!(pinned.next().is_none(), "expected every pinned estimate");
        }
    }

    /// A replacement estimated without the option charges no added bytes,
    /// so it would be below a settled owner change; the option raises it to
    /// the insertion the settled apply records.
    #[test]
    fn a_replace_estimate_without_the_option_is_below_a_settled_owner_change() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 20], owned_flags(2, NEW_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        let applied = apply(&db, vec![write_op(&new)], Mode::Settling, grove_version)
            .expect("expected the settling update to apply");
        let replace =
            QualifiedGroveDbOp::replace_op(vec![b"tree".to_vec()], KEY.to_vec(), new.clone());
        let merging = estimate(
            vec![replace.clone()],
            None,
            Some(TreeType::NormalTree),
            grove_version,
        );
        assert_eq!(merging.storage_cost.added_bytes, 0);
        assert!(merging.storage_cost.added_bytes < applied.storage_cost.added_bytes);
        let settling = estimate(
            vec![replace],
            Some(options(Mode::Settling)),
            Some(TreeType::NormalTree),
            grove_version,
        );
        assert_eq!(
            settling.storage_cost.added_bytes,
            applied.storage_cost.added_bytes
        );
        assert_eq!(
            settling.storage_cost.replaced_bytes,
            merging.storage_cost.replaced_bytes
        );
    }

    /// A flagged tree written together with a write under it reaches the
    /// estimators as the `InsertTreeWithRootHash` its propagation makes of
    /// it; with the option that op too is charged the insertion a settled
    /// owner change records, whatever tree it is and whatever tree it lands
    /// in.
    #[test]
    fn estimates_with_the_option_cover_a_settled_tree_written_with_its_children() {
        let grove_version = GroveVersion::latest();
        let trees: [fn(Option<Vec<u8>>) -> Element; 7] = [
            Element::empty_tree_with_flags,
            Element::empty_sum_tree_with_flags,
            Element::empty_big_sum_tree_with_flags,
            Element::empty_count_tree_with_flags,
            Element::empty_count_sum_tree_with_flags,
            Element::empty_provable_count_tree_with_flags,
            Element::empty_provable_count_sum_tree_with_flags,
        ];
        for parent_tree in [
            Element::empty_tree(),
            Element::empty_sum_tree(),
            Element::empty_big_sum_tree(),
            Element::empty_count_tree(),
        ] {
            let parent_tree_type = parent_tree.tree_type().expect("expected a tree");
            for tree in trees {
                let tree_type = tree(None).tree_type().expect("expected a tree");
                let db = make_empty_grovedb();
                db.insert(
                    EMPTY_PATH,
                    b"parent",
                    parent_tree.clone(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("expected to insert the parent tree");
                db.insert(
                    [b"parent".as_slice()].as_ref(),
                    b"tree",
                    tree(owned_flags(0, OLD_OWNER)),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("expected to insert the tree");
                db.insert(
                    [b"parent".as_slice(), b"tree"].as_ref(),
                    b"child",
                    Element::new_item(b"child".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("expected to insert the child");
                let ops = vec![
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"parent".to_vec()],
                        b"tree".to_vec(),
                        tree(owned_flags(2, NEW_OWNER)),
                    ),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"parent".to_vec(), b"tree".to_vec()],
                        b"another child".to_vec(),
                        Element::new_item(b"another child".to_vec()),
                    ),
                ];
                let applied = apply(&db, ops.clone(), Mode::Settling, grove_version)
                    .expect("expected the settling update to apply");

                let mut average_case_layers = HashMap::new();
                average_case_layers.insert(
                    KeyInfoPath(vec![]),
                    EstimatedLayerInformation {
                        tree_type: TreeType::NormalTree,
                        estimated_layer_count: ApproximateElements(2),
                        estimated_layer_sizes: AllSubtrees(7, NoSumTrees, None),
                    },
                );
                average_case_layers.insert(
                    KeyInfoPath::from_known_path([b"parent".as_slice()]),
                    EstimatedLayerInformation {
                        tree_type: parent_tree_type,
                        estimated_layer_count: ApproximateElements(2),
                        estimated_layer_sizes: AllSubtrees(4, NoSumTrees, Some(35)),
                    },
                );
                average_case_layers.insert(
                    KeyInfoPath::from_known_path([b"parent".as_slice(), b"tree"]),
                    EstimatedLayerInformation {
                        tree_type,
                        estimated_layer_count: ApproximateElements(2),
                        estimated_layer_sizes: AllItems(13, 13, None),
                    },
                );
                let mut worst_case_layers = HashMap::new();
                worst_case_layers.insert(KeyInfoPath(vec![]), MaxElementsNumber(2));
                worst_case_layers.insert(
                    KeyInfoPath::from_known_path([b"parent".as_slice()]),
                    MaxElementsNumber(2),
                );
                worst_case_layers.insert(
                    KeyInfoPath::from_known_path([b"parent".as_slice(), b"tree"]),
                    MaxElementsNumber(2),
                );
                // The worst case sizes the item written under the tree as
                // under a normal tree whatever the tree is, so only a normal
                // tree's batch is bounded as a whole there; the worst-case
                // settled tree itself is bounded for every tree in
                // `estimated_costs`' tests.
                let mut estimated_costs_types = vec![AverageCaseCostsType(average_case_layers)];
                if tree_type == TreeType::NormalTree {
                    estimated_costs_types.push(WorstCaseCostsType(worst_case_layers));
                }
                for estimated_costs_type in estimated_costs_types {
                    let estimate = GroveDb::estimated_case_operations_for_batch(
                        estimated_costs_type,
                        ops.clone(),
                        Some(options(Mode::Settling)),
                        |_cost, _old_flags, _new_flags| Ok(false),
                        |_flags, _removed_key_bytes, _removed_value_bytes| {
                            Ok((NoStorageRemoval, NoStorageRemoval))
                        },
                        grove_version,
                    )
                    .cost_as_result()
                    .expect("expected an estimate");
                    assert!(
                        estimate.storage_cost.added_bytes >= applied.storage_cost.added_bytes,
                        "a {tree_type:?} in a {parent_tree_type:?}: estimate {estimate:?} is \
                         below the settled apply {applied:?}"
                    );
                }
            }
        }
    }
}
