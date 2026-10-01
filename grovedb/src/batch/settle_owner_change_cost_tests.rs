//! Costs of an update in place that settles an owner change
//! (`BatchApplyOptions::settle_owner_changes`): the old element is removed
//! and its bytes sectioned to its owner as a deletion sections them, and the
//! new element is inserted and charged to the writer.

#[cfg(feature = "minimal")]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use grovedb_costs::{
        storage_cost::{
            removal::StorageRemovedBytes::{NoStorageRemoval, SectionedStorageRemoval},
            transition::ElementFlagsUpdate,
        },
        OperationCost,
    };
    use grovedb_epoch_based_storage_flags::StorageFlags;
    use grovedb_merk::{
        element::{
            costs::ElementCostExtensions, get::ElementFetchFromStorageExtensions,
            tree_type::ElementTreeTypeExtensions,
        },
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
    use grovedb_version::version::GroveVersion;
    use intmap::IntMap;

    use crate::{
        batch::{
            estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
            BatchApplyOptions, KeyInfoPath, QualifiedGroveDbOp, SubelementsDeletionBehavior,
        },
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
        BackwardReferences, Element, Error, GroveDb,
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

    fn target_path() -> ReferencePathType {
        ReferencePathType::AbsolutePathReference(vec![b"targets".to_vec(), b"target".to_vec()])
    }

    fn long_target_path() -> ReferencePathType {
        ReferencePathType::AbsolutePathReference(vec![
            b"targets".to_vec(),
            b"target with a much longer key".to_vec(),
        ])
    }

    /// How a batch is applied.
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// Today's flags update, without the option.
        Merging,
        /// The settling flags update, with the option.
        Settling,
    }

    fn options(mode: Mode) -> BatchApplyOptions {
        BatchApplyOptions {
            settle_owner_changes: mode == Mode::Settling,
            ..Default::default()
        }
    }

    fn apply(
        db: &TempGroveDb,
        ops: Vec<QualifiedGroveDbOp>,
        mode: Mode,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        db.apply_batch_with_element_flags_update(
            ops,
            Some(options(mode)),
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

    fn write_op(element: &Element) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"tree".to_vec()],
            KEY.to_vec(),
            element.clone(),
        )
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

    /// The bytes the node of `element` at `key1` holds besides its key, as
    /// the apply sizes them.
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
    /// it replaces, and a settled write adds their entries too. Both
    /// estimates cover them through the item's declared capacity.
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
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
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
        let applied = apply(&db, vec![write_op(&new)], Mode::Settling, grove_version)
            .expect("expected the settling update to apply");
        let landed = stored_in_merk(&db, grove_version);
        assert_eq!(
            landed.backward_references().map(|refs| refs.len()),
            Some(3),
            "the item keeps its referrers"
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
        for average_case_tree_type in [Some(TreeType::NormalTree), None] {
            let estimate = estimate(
                vec![write_op(&new)],
                Some(options(Mode::Settling)),
                average_case_tree_type,
                grove_version,
            );
            assert!(
                estimate.storage_cost.added_bytes >= applied.storage_cost.added_bytes,
                "estimate {estimate:?} adds less than the settled apply {applied:?}"
            );
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
                |cost, old_flags, new_flags| {
                    StorageFlags::update_element_flags_settling_owner_changes(
                        cost, old_flags, new_flags,
                    )
                    .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
                },
                |flags, removed_key_bytes, removed_value_bytes| {
                    StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
                        .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
                },
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
                        |cost, old_flags, new_flags| {
                            StorageFlags::update_element_flags(cost, old_flags, new_flags).map_err(
                                |e| Error::JustInTimeElementFlagsClientError(e.to_string()),
                            )
                        },
                        |flags, removed_key_bytes, removed_value_bytes| {
                            StorageFlags::split_removal_bytes(
                                flags,
                                removed_key_bytes,
                                removed_value_bytes,
                            )
                            .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
                        },
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

    #[test]
    fn a_settling_flags_update_is_refused_without_the_option() {
        let grove_version = GroveVersion::latest();
        let old = Element::new_item_with_flags(vec![7; 20], owned_flags(0, OLD_OWNER));
        let new = Element::new_item_with_flags(vec![8; 60], owned_flags(2, NEW_OWNER));
        let db = grove_with(&old, TreeType::NormalTree, grove_version);
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        let error = db
            .apply_batch_with_element_flags_update(
                vec![write_op(&new)],
                None,
                |cost, old_flags, new_flags| {
                    StorageFlags::update_element_flags_settling_owner_changes(
                        cost, old_flags, new_flags,
                    )
                    .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
                },
                |flags, removed_key_bytes, removed_value_bytes| {
                    StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
                        .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
                },
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("expected the settling answer to be refused");
        assert!(
            error.to_string().contains("settle_owner_changes"),
            "unexpected error: {error}"
        );
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_hash
        );
    }

    /// A reference written in the same batch commits to the bytes the
    /// settled element stores: its new flags, not the old owner's.
    #[test]
    fn a_reference_in_the_same_batch_commits_to_the_settled_element() {
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

    /// With the option, the average-case and worst-case estimates of every
    /// write that may settle an owner change are never below the settled
    /// apply; without it they are what they always were.
    #[test]
    fn estimates_with_the_option_cover_a_settled_owner_change() {
        let grove_version = GroveVersion::latest();
        let cases = [
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
        ];
        for (old, new, tree_type) in cases {
            let db = grove_with(&old, tree_type, grove_version);
            let applied = apply(&db, vec![write_op(&new)], Mode::Settling, grove_version)
                .expect("expected the settling update to apply");

            let replace =
                QualifiedGroveDbOp::replace_op(vec![b"tree".to_vec()], KEY.to_vec(), new.clone());
            let mut ops = vec![write_op(&new), replace];
            if let Element::Item(..) = new {
                ops.push(QualifiedGroveDbOp::patch_op(
                    vec![b"tree".to_vec()],
                    KEY.to_vec(),
                    new.clone(),
                    0,
                ));
            }
            if let Element::Reference(path, max_hop, flags) = &new {
                ops.push(QualifiedGroveDbOp::refresh_reference_op(
                    vec![b"tree".to_vec()],
                    KEY.to_vec(),
                    path.clone(),
                    *max_hop,
                    flags.clone(),
                    false,
                    true,
                ));
            }
            for op in ops {
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
                        "estimate {settling:?} of {op:?} is below the settled apply {applied:?}"
                    );
                    assert!(
                        settling.storage_cost.added_bytes >= applied.storage_cost.added_bytes,
                        "estimate {settling:?} of {op:?} adds less than the settled apply \
                         {applied:?}"
                    );
                    // without the option the estimate is what it always was
                    let merging = estimate(
                        vec![op.clone()],
                        Some(options(Mode::Merging)),
                        average_case_tree_type,
                        grove_version,
                    );
                    assert_eq!(
                        merging,
                        estimate(
                            vec![op.clone()],
                            None,
                            average_case_tree_type,
                            grove_version
                        )
                    );
                }
            }
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
}
