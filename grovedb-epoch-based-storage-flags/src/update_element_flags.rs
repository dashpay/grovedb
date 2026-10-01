use grovedb_costs::storage_cost::{
    transition::{ElementFlagsUpdate, OperationStorageTransitionType},
    StorageCost,
};

use crate::{error::StorageFlagsError, ElementFlags, MergingOwnersStrategy, StorageFlags};

impl StorageFlags {
    /// [`Self::update_element_flags`], except that an update whose new flags
    /// name a different owner than the old flags settles the owner change
    ///
    /// When both the old and the new flags name an owner and the owners
    /// differ, the new flags are left as the writer wrote them and the update
    /// answers [`ElementFlagsUpdate::SettleOwnerChange`]: the batch then
    /// accounts it as the removal of the old element, sectioned to the old
    /// owner through its epoch map by [`Self::split_removal_bytes`], plus the
    /// insertion of the new element, charged to the writer. Every other update
    /// (same owner, or no owner on either side) is answered by
    /// [`Self::update_element_flags`], unchanged.
    ///
    /// The batch accepts the answer only when its options set
    /// `settle_owner_changes`.
    pub fn update_element_flags_settling_owner_changes(
        cost: &StorageCost,
        old_flags: Option<ElementFlags>,
        new_flags: &mut ElementFlags,
    ) -> Result<ElementFlagsUpdate, StorageFlagsError> {
        if Self::update_changes_owner(cost, old_flags.as_ref(), new_flags) {
            return Ok(ElementFlagsUpdate::SettleOwnerChange);
        }
        Self::update_element_flags(cost, old_flags, new_flags).map(ElementFlagsUpdate::from)
    }

    /// Whether `cost` updates an element in place and the old and new flags
    /// both parse and name different owners. Flags that do not parse are left
    /// to [`Self::update_element_flags`], which reports them.
    fn update_changes_owner(
        cost: &StorageCost,
        old_flags: Option<&ElementFlags>,
        new_flags: &ElementFlags,
    ) -> bool {
        let is_update_in_place = matches!(
            cost.transition_type(),
            OperationStorageTransitionType::OperationUpdateBiggerSize
                | OperationStorageTransitionType::OperationUpdateSmallerSize
                | OperationStorageTransitionType::OperationUpdateSameSize
        );
        let owner_of = |flags: &ElementFlags| {
            StorageFlags::from_element_flags_ref(flags)
                .ok()
                .flatten()
                .and_then(|storage_flags| storage_flags.owner_id().copied())
        };
        match (old_flags.and_then(owner_of), owner_of(new_flags)) {
            (Some(old_owner), Some(new_owner)) => is_update_in_place && old_owner != new_owner,
            _ => false,
        }
    }

    pub fn update_element_flags(
        cost: &StorageCost,
        old_flags: Option<ElementFlags>,
        new_flags: &mut ElementFlags,
    ) -> Result<bool, StorageFlagsError> {
        // if there were no flags before then the new flags are used
        let Some(old_flags) = old_flags else {
            return Ok(false);
        };

        // This could be none only because the old element didn't exist
        // If they were empty we get an error
        let maybe_old_storage_flags =
            StorageFlags::from_element_flags_ref(&old_flags).map_err(|mut e| {
                e.add_info("drive did not understand flags of old item being updated");
                e
            })?;
        let new_storage_flags = StorageFlags::from_element_flags_ref(new_flags)
            .map_err(|mut e| {
                e.add_info("drive did not understand updated item flag information");
                e
            })?
            .ok_or(StorageFlagsError::RemovingFlagsError(
                "removing flags from an item with flags is not allowed".to_string(),
            ))?;
        let old_storage_flags =
            maybe_old_storage_flags
                .clone()
                .ok_or(StorageFlagsError::RemovingFlagsError(
                    "old storage flags missing during update".to_string(),
                ))?;
        let old_epoch_index_map = old_storage_flags.epoch_index_map();
        let new_epoch_index_map = new_storage_flags.epoch_index_map();
        if old_epoch_index_map.is_some() || new_epoch_index_map.is_some() {
            // println!("> old:{:?} new:{:?}", old_epoch_index_map,
            // new_epoch_index_map);
        }

        match &cost.transition_type() {
            OperationStorageTransitionType::OperationUpdateBiggerSize => {
                // In the case that the owners do not match up this means that there has been a
                // transfer  of ownership of the underlying document, the value
                // held is transferred to the new owner
                // println!(">---------------------combine_added_bytes:{}", cost.added_bytes);
                // println!(">---------------------apply_batch_with_add_costs old_flags:{:?}
                // new_flags:{:?}", maybe_old_storage_flags, new_storage_flags);
                let combined_storage_flags = StorageFlags::optional_combine_added_bytes(
                    maybe_old_storage_flags.clone(),
                    new_storage_flags.clone(),
                    cost.added_bytes,
                    MergingOwnersStrategy::UseTheirs,
                )
                .map_err(|mut e| {
                    e.add_info("drive could not combine storage flags (new flags were bigger)");
                    e
                })?;
                // println!(
                //     ">added_bytes:{} old:{} new:{} --> combined:{}",
                //     cost.added_bytes,
                //     if maybe_old_storage_flags.is_some() {
                //         maybe_old_storage_flags.as_ref().unwrap().to_string()
                //     } else {
                //         "None".to_string()
                //     },
                //     new_storage_flags,
                //     combined_storage_flags
                // );
                // if combined_storage_flags.epoch_index_map().is_some() {
                //     //println!("     --------> bigger_combined_flags:{:?}",
                // combined_storage_flags.epoch_index_map()); }
                let combined_flags = combined_storage_flags.to_element_flags();
                // it's possible they got bigger in the same epoch
                if combined_flags == *new_flags {
                    // they are the same there was no update
                    Ok(false)
                } else {
                    *new_flags = combined_flags;
                    Ok(true)
                }
            }
            OperationStorageTransitionType::OperationUpdateSmallerSize => {
                // println!(
                //     ">removing_bytes:{:?} old:{} new:{}",
                //     cost.removed_bytes,
                //     if maybe_old_storage_flags.is_some() {
                //         maybe_old_storage_flags.as_ref().unwrap().to_string()
                //     } else {
                //         "None".to_string()
                //     },
                //     new_storage_flags,
                // );
                // In the case that the owners do not match up this means that there has been a
                // transfer  of ownership of the underlying document, the value
                // held is transferred to the new owner
                let combined_storage_flags = StorageFlags::optional_combine_removed_bytes(
                    maybe_old_storage_flags.clone(),
                    new_storage_flags.clone(),
                    &cost.removed_bytes,
                    MergingOwnersStrategy::UseTheirs,
                )
                .map_err(|mut e| {
                    e.add_info("drive could not combine storage flags (new flags were smaller)");
                    e
                })?;
                // println!(
                //     ">removed_bytes:{:?} old:{:?} new:{:?} --> combined:{:?}",
                //     cost.removed_bytes,
                //     maybe_old_storage_flags,
                //     new_storage_flags,
                //     combined_storage_flags
                // );
                if combined_storage_flags.epoch_index_map().is_some() {
                    // println!("     --------> smaller_combined_flags:{:?}",
                    // combined_storage_flags.epoch_index_map());
                }
                let combined_flags = combined_storage_flags.to_element_flags();
                // it's possible they got bigger in the same epoch
                if combined_flags == *new_flags {
                    // they are the same there was no update
                    Ok(false)
                } else {
                    *new_flags = combined_flags;
                    Ok(true)
                }
            }
            OperationStorageTransitionType::OperationUpdateSameSize => {
                if let Some(old_storage_flags) = maybe_old_storage_flags {
                    // if there were old storage flags we should just keep them
                    *new_flags = old_storage_flags.to_element_flags();
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use grovedb_costs::storage_cost::{
        removal::StorageRemovedBytes, transition::ElementFlagsUpdate, StorageCost,
    };

    use crate::StorageFlags;

    #[test]
    fn update_element_flags_returns_false_when_old_flags_missing() {
        let cost = StorageCost {
            added_bytes: 10,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };
        let mut new_flags = StorageFlags::SingleEpoch(1).to_element_flags();

        let changed = StorageFlags::update_element_flags(&cost, None, &mut new_flags)
            .expect("expected success");

        assert!(!changed);
        assert_eq!(new_flags, StorageFlags::SingleEpoch(1).to_element_flags());
    }

    #[test]
    fn update_element_flags_bigger_size_updates_flags() {
        let old = StorageFlags::SingleEpoch(1).to_element_flags();
        let mut new_flags = StorageFlags::SingleEpoch(2).to_element_flags();
        let cost = StorageCost {
            added_bytes: 10,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };

        let changed = StorageFlags::update_element_flags(&cost, Some(old), &mut new_flags)
            .expect("expected success");

        assert!(changed);
        assert_eq!(
            StorageFlags::from_element_flags_ref(&new_flags).expect("flags must deserialize"),
            Some(StorageFlags::MultiEpoch(1, BTreeMap::from([(2, 10)])))
        );
    }

    #[test]
    fn update_element_flags_bigger_size_returns_false_when_unchanged() {
        let old = StorageFlags::SingleEpoch(1).to_element_flags();
        let mut new_flags =
            StorageFlags::MultiEpoch(1, BTreeMap::from([(2, 10)])).to_element_flags();
        let cost = StorageCost {
            added_bytes: 10,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };

        let changed = StorageFlags::update_element_flags(&cost, Some(old), &mut new_flags)
            .expect("expected success");

        assert!(!changed);
    }

    #[test]
    fn update_element_flags_smaller_size_updates_flags() {
        let owner = [1u8; 32];
        let old =
            StorageFlags::MultiEpochOwned(1, BTreeMap::from([(2, 20)]), owner).to_element_flags();
        let mut new_flags = StorageFlags::SingleEpochOwned(2, owner).to_element_flags();

        let mut per_owner = BTreeMap::new();
        per_owner.insert(owner, intmap::IntMap::from_iter([(2u16, 5u32)]));
        let cost = StorageCost {
            added_bytes: 0,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::SectionedStorageRemoval(per_owner),
        };

        let changed = StorageFlags::update_element_flags(&cost, Some(old), &mut new_flags)
            .expect("expected success");

        assert!(changed);
        assert_eq!(
            StorageFlags::from_element_flags_ref(&new_flags).expect("flags must deserialize"),
            Some(StorageFlags::MultiEpochOwned(
                1,
                BTreeMap::from([(2, 15)]),
                owner
            ))
        );
    }

    #[test]
    fn update_element_flags_same_size_keeps_old_flags() {
        let old = StorageFlags::SingleEpoch(9).to_element_flags();
        let mut new_flags = StorageFlags::SingleEpoch(1).to_element_flags();
        let cost = StorageCost {
            added_bytes: 0,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };

        let changed = StorageFlags::update_element_flags(&cost, Some(old), &mut new_flags)
            .expect("expected success");

        assert!(changed);
        assert_eq!(new_flags, StorageFlags::SingleEpoch(9).to_element_flags());
    }

    #[test]
    fn update_element_flags_non_update_transition_returns_false() {
        let old = StorageFlags::SingleEpoch(1).to_element_flags();
        let mut new_flags = StorageFlags::SingleEpoch(2).to_element_flags();
        let cost = StorageCost {
            added_bytes: 1,
            replaced_bytes: 0,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };

        let changed = StorageFlags::update_element_flags(&cost, Some(old), &mut new_flags)
            .expect("expected success");

        assert!(!changed);
        assert_eq!(new_flags, StorageFlags::SingleEpoch(2).to_element_flags());
    }

    #[test]
    fn update_element_flags_errors_when_new_flags_removed() {
        let old = StorageFlags::SingleEpoch(1).to_element_flags();
        let mut new_flags = vec![];
        let cost = StorageCost {
            added_bytes: 0,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };

        let error = StorageFlags::update_element_flags(&cost, Some(old), &mut new_flags)
            .expect_err("expected error");

        assert!(error
            .to_string()
            .contains("removing flags from an item with flags is not allowed"));
    }

    #[test]
    fn update_element_flags_adds_context_for_parse_errors() {
        let mut new_flags = StorageFlags::SingleEpoch(1).to_element_flags();
        let cost = StorageCost {
            added_bytes: 0,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };

        let old_error = StorageFlags::update_element_flags(&cost, Some(vec![255]), &mut new_flags)
            .expect_err("expected old flag parse error");
        assert!(old_error
            .to_string()
            .contains("drive did not understand flags of old item being updated"));

        let mut invalid_new = vec![255];
        let new_error = StorageFlags::update_element_flags(
            &cost,
            Some(StorageFlags::SingleEpoch(1).to_element_flags()),
            &mut invalid_new,
        )
        .expect_err("expected new flag parse error");
        assert!(new_error
            .to_string()
            .contains("drive did not understand updated item flag information"));
    }

    fn update_cost(added_bytes: u32, replaced_bytes: u32, removed_bytes: u32) -> StorageCost {
        StorageCost {
            added_bytes,
            replaced_bytes,
            removed_bytes: if removed_bytes > 0 {
                StorageRemovedBytes::BasicStorageRemoval(removed_bytes)
            } else {
                StorageRemovedBytes::NoStorageRemoval
            },
        }
    }

    #[test]
    fn settling_update_settles_an_owner_change_of_any_size() {
        let old_owner = [1u8; 32];
        let new_owner = [2u8; 32];
        let old = StorageFlags::SingleEpochOwned(1, old_owner).to_element_flags();
        let written = StorageFlags::SingleEpochOwned(4, new_owner).to_element_flags();
        for cost in [
            update_cost(10, 50, 0),
            update_cost(0, 50, 10),
            update_cost(0, 50, 0),
        ] {
            let mut new_flags = written.clone();
            let update = StorageFlags::update_element_flags_settling_owner_changes(
                &cost,
                Some(old.clone()),
                &mut new_flags,
            )
            .expect("expected success");
            assert_eq!(update, ElementFlagsUpdate::SettleOwnerChange);
            // the writer's flags are kept as written, never merged with the
            // old owner's epochs
            assert_eq!(new_flags, written);
        }
    }

    #[test]
    fn settling_update_settles_an_owner_change_of_multi_epoch_flags() {
        let old = StorageFlags::MultiEpochOwned(1, BTreeMap::from([(2, 20), (3, 7)]), [1u8; 32])
            .to_element_flags();
        let written = StorageFlags::SingleEpochOwned(5, [2u8; 32]).to_element_flags();
        let mut new_flags = written.clone();
        let update = StorageFlags::update_element_flags_settling_owner_changes(
            &update_cost(4, 60, 0),
            Some(old),
            &mut new_flags,
        )
        .expect("expected success");
        assert_eq!(update, ElementFlagsUpdate::SettleOwnerChange);
        assert_eq!(new_flags, written);
    }

    /// Every update that does not change the owner gets exactly what
    /// `update_element_flags` gives it.
    #[test]
    fn settling_update_matches_update_element_flags_without_an_owner_change() {
        let owner = [1u8; 32];
        let cases = [
            // same owner, bigger, smaller and same size
            (
                Some(StorageFlags::SingleEpochOwned(1, owner)),
                StorageFlags::SingleEpochOwned(2, owner),
                update_cost(10, 50, 0),
            ),
            (
                Some(StorageFlags::MultiEpochOwned(
                    1,
                    BTreeMap::from([(2, 20)]),
                    owner,
                )),
                StorageFlags::SingleEpochOwned(2, owner),
                StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 50,
                    removed_bytes: StorageRemovedBytes::SectionedStorageRemoval(BTreeMap::from([
                        (owner, intmap::IntMap::from_iter([(2u16, 5u32)])),
                    ])),
                },
            ),
            (
                Some(StorageFlags::SingleEpochOwned(1, owner)),
                StorageFlags::SingleEpochOwned(2, owner),
                update_cost(0, 50, 0),
            ),
            // no owner on the old side, on the new side, or on either
            (
                Some(StorageFlags::SingleEpoch(1)),
                StorageFlags::SingleEpochOwned(2, owner),
                update_cost(10, 50, 0),
            ),
            (
                Some(StorageFlags::SingleEpochOwned(1, owner)),
                StorageFlags::SingleEpoch(2),
                update_cost(0, 50, 0),
            ),
            (
                Some(StorageFlags::SingleEpoch(1)),
                StorageFlags::SingleEpoch(2),
                update_cost(10, 50, 0),
            ),
            // no old flags at all
            (
                None,
                StorageFlags::SingleEpochOwned(2, owner),
                update_cost(10, 50, 0),
            ),
        ];
        for (old, new, cost) in cases {
            let old = old.map(|old| old.to_element_flags());
            let mut expected_flags = new.to_element_flags();
            let expected =
                StorageFlags::update_element_flags(&cost, old.clone(), &mut expected_flags)
                    .expect("expected success");
            let mut new_flags = new.to_element_flags();
            let update = StorageFlags::update_element_flags_settling_owner_changes(
                &cost,
                old,
                &mut new_flags,
            )
            .expect("expected success");
            assert_eq!(update, ElementFlagsUpdate::from(expected));
            assert_eq!(new_flags, expected_flags);
        }
    }

    #[test]
    fn settling_update_does_not_settle_outside_an_update_in_place() {
        let old = StorageFlags::SingleEpochOwned(1, [1u8; 32]).to_element_flags();
        let written = StorageFlags::SingleEpochOwned(2, [2u8; 32]).to_element_flags();
        let mut new_flags = written.clone();
        let update = StorageFlags::update_element_flags_settling_owner_changes(
            &update_cost(10, 0, 0),
            Some(old),
            &mut new_flags,
        )
        .expect("expected success");
        assert_eq!(update, ElementFlagsUpdate::Unchanged);
        assert_eq!(new_flags, written);
    }

    #[test]
    fn settling_update_reports_unreadable_flags_as_update_element_flags_does() {
        let cost = update_cost(0, 50, 0);
        let owned = StorageFlags::SingleEpochOwned(1, [1u8; 32]).to_element_flags();

        let mut new_flags = owned.clone();
        let old_error = StorageFlags::update_element_flags_settling_owner_changes(
            &cost,
            Some(vec![255]),
            &mut new_flags,
        )
        .expect_err("expected old flag parse error");
        assert!(old_error
            .to_string()
            .contains("drive did not understand flags of old item being updated"));

        let mut removed_flags = vec![];
        let removed_error = StorageFlags::update_element_flags_settling_owner_changes(
            &cost,
            Some(owned),
            &mut removed_flags,
        )
        .expect_err("expected an error for removed flags");
        assert!(removed_error
            .to_string()
            .contains("removing flags from an item with flags is not allowed"));
    }
}
