use grovedb_costs::storage_cost::{transition::OperationStorageTransitionType, StorageCost};

use crate::{error::StorageFlagsError, ElementFlags, MergingOwnersStrategy, OwnerId, StorageFlags};

impl StorageFlags {
    /// [`Self::update_element_flags`], except that the owner the new flags name
    /// always owns the element afterwards, whether the update grows it, shrinks
    /// it or keeps its size
    ///
    /// Who pays for a write and who owns its result are separate decisions, and
    /// the writer makes the second one in the flags it writes:
    /// - to leave the element with its owner while someone else pays for the
    ///   write, write flags naming the current owner;
    /// - to hand the element over, write flags naming the new owner.
    ///
    /// The byte accounting does not depend on that choice. The added bytes are
    /// charged to whoever the caller bills, and bytes an update frees are
    /// refunded through the old flags, so they go to the old owner. Bytes that
    /// stay stored move with the element: the owner afterwards is the one
    /// refunded when they are freed later.
    ///
    /// New flags that name no owner leave the owner to
    /// [`Self::update_element_flags`], and every other part of the result
    /// (epochs, bytes per epoch, errors) is what it gives.
    ///
    /// [`Self::update_element_flags`] keeps the old owner on an update that
    /// keeps the size, or that shrinks an element holding bytes of one epoch.
    /// Shipped protocol versions depend on that, so it stays as it is.
    pub fn update_element_flags_with_written_owner(
        cost: &StorageCost,
        old_flags: Option<ElementFlags>,
        new_flags: &mut ElementFlags,
    ) -> Result<bool, StorageFlagsError> {
        // Read before the merge rewrites `new_flags`. Flags that do not decode
        // name no owner here, and the merge reports them.
        let written_owner = StorageFlags::from_element_flags_ref(new_flags)
            .ok()
            .flatten()
            .and_then(|flags| flags.owner_id().copied());
        let changed = Self::update_element_flags(cost, old_flags, new_flags)?;
        let Some(written_owner) = written_owner else {
            return Ok(changed);
        };
        let Some(merged) = StorageFlags::from_element_flags_ref(new_flags)? else {
            return Ok(changed);
        };
        if merged.owner_id() == Some(&written_owner) {
            return Ok(changed);
        }
        *new_flags = merged.with_owner_id(written_owner).to_element_flags();
        Ok(true)
    }

    /// These flags, owned by `owner_id`.
    fn with_owner_id(self, owner_id: OwnerId) -> Self {
        match self {
            StorageFlags::SingleEpoch(base_epoch)
            | StorageFlags::SingleEpochOwned(base_epoch, _) => {
                StorageFlags::SingleEpochOwned(base_epoch, owner_id)
            }
            StorageFlags::MultiEpoch(base_epoch, bytes_per_epoch)
            | StorageFlags::MultiEpochOwned(base_epoch, bytes_per_epoch, _) => {
                StorageFlags::MultiEpochOwned(base_epoch, bytes_per_epoch, owner_id)
            }
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

    use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};

    use crate::StorageFlags;

    const OLD_OWNER: [u8; 32] = [1; 32];
    const NEW_OWNER: [u8; 32] = [2; 32];

    fn grown(added_bytes: u32) -> StorageCost {
        StorageCost {
            added_bytes,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        }
    }

    fn shrunk(owner: [u8; 32], epoch: u16, removed: u32) -> StorageCost {
        StorageCost {
            added_bytes: 0,
            replaced_bytes: 1,
            removed_bytes: StorageRemovedBytes::SectionedStorageRemoval(BTreeMap::from([(
                owner,
                intmap::IntMap::from_iter([(epoch, removed)]),
            )])),
        }
    }

    fn same_size() -> StorageCost {
        grown(0)
    }

    /// What the update leaves in the new flags, and its answer.
    fn with_written_owner(
        cost: &StorageCost,
        old: &StorageFlags,
        new: &StorageFlags,
    ) -> (StorageFlags, bool) {
        let mut new_flags = new.to_element_flags();
        let changed = StorageFlags::update_element_flags_with_written_owner(
            cost,
            Some(old.to_element_flags()),
            &mut new_flags,
        )
        .expect("expected the update to succeed");
        let flags = StorageFlags::from_element_flags_ref(&new_flags)
            .expect("flags must deserialize")
            .expect("flags must be present");
        (flags, changed)
    }

    #[test]
    fn should_give_the_element_to_the_owner_the_new_flags_name_on_every_update() {
        let old = StorageFlags::SingleEpochOwned(0, OLD_OWNER);
        let cases = [
            ("grown", grown(10), Some(BTreeMap::from([(2, 10)]))),
            ("shrunk", shrunk(OLD_OWNER, 0, 10), None),
            ("the same size", same_size(), None),
        ];
        for (label, cost, bytes_per_epoch) in cases {
            for owner in [OLD_OWNER, NEW_OWNER] {
                let new = StorageFlags::SingleEpochOwned(2, owner);
                let (flags, _) = with_written_owner(&cost, &old, &new);
                let expected = match bytes_per_epoch.clone() {
                    Some(bytes_per_epoch) => {
                        StorageFlags::MultiEpochOwned(0, bytes_per_epoch, owner)
                    }
                    None => StorageFlags::SingleEpochOwned(0, owner),
                };
                assert_eq!(flags, expected, "{label}, owner {owner:?}");
            }
        }
    }

    #[test]
    fn should_keep_the_bytes_of_every_epoch_when_the_owner_changes() {
        let old = StorageFlags::MultiEpochOwned(0, BTreeMap::from([(1, 40)]), OLD_OWNER);
        let new = StorageFlags::SingleEpochOwned(2, NEW_OWNER);
        let cases = [
            (shrunk(OLD_OWNER, 1, 30), BTreeMap::from([(1, 10)])),
            (same_size(), BTreeMap::from([(1, 40)])),
        ];
        for (cost, bytes_per_epoch) in cases {
            let (flags, changed) = with_written_owner(&cost, &old, &new);
            assert_eq!(
                flags,
                StorageFlags::MultiEpochOwned(0, bytes_per_epoch, NEW_OWNER)
            );
            assert!(changed);
        }
    }

    #[test]
    fn should_give_an_unowned_element_the_owner_the_new_flags_name() {
        let new = StorageFlags::SingleEpochOwned(2, NEW_OWNER);
        let (flags, changed) =
            with_written_owner(&same_size(), &StorageFlags::SingleEpoch(0), &new);
        assert_eq!(flags, StorageFlags::SingleEpochOwned(0, NEW_OWNER));
        assert!(changed);
        let (flags, changed) = with_written_owner(
            &shrunk([0; 32], 1, 30),
            &StorageFlags::MultiEpoch(0, BTreeMap::from([(1, 40)])),
            &new,
        );
        assert_eq!(
            flags,
            StorageFlags::MultiEpochOwned(0, BTreeMap::from([(1, 10)]), NEW_OWNER)
        );
        assert!(changed);
    }

    #[test]
    fn should_match_update_element_flags_when_the_new_flags_name_no_owner() {
        let old = StorageFlags::SingleEpochOwned(0, OLD_OWNER);
        let new = StorageFlags::SingleEpoch(2);
        for cost in [grown(10), shrunk(OLD_OWNER, 0, 10), same_size()] {
            let mut merged = new.to_element_flags();
            let merged_changed = StorageFlags::update_element_flags(
                &cost,
                Some(old.to_element_flags()),
                &mut merged,
            )
            .expect("expected the update to succeed");
            let mut written = new.to_element_flags();
            let written_changed = StorageFlags::update_element_flags_with_written_owner(
                &cost,
                Some(old.to_element_flags()),
                &mut written,
            )
            .expect("expected the update to succeed");
            assert_eq!(written, merged);
            assert_eq!(written_changed, merged_changed);
        }
    }

    #[test]
    fn should_leave_the_new_flags_of_an_element_without_old_flags() {
        let written = StorageFlags::SingleEpochOwned(2, NEW_OWNER).to_element_flags();
        let mut new_flags = written.clone();
        let changed =
            StorageFlags::update_element_flags_with_written_owner(&grown(10), None, &mut new_flags)
                .expect("expected the update to succeed");
        assert!(!changed);
        assert_eq!(new_flags, written);
    }

    #[test]
    fn should_not_answer_changed_when_the_merge_already_stores_the_new_flags() {
        let old = StorageFlags::SingleEpochOwned(1, OLD_OWNER);
        let new = StorageFlags::MultiEpochOwned(1, BTreeMap::from([(2, 10)]), NEW_OWNER);
        let (flags, changed) = with_written_owner(&grown(10), &old, &new);
        assert_eq!(flags, new);
        assert!(!changed);
    }

    #[test]
    fn should_report_undecodable_new_flags_as_update_element_flags_does() {
        let mut invalid_new = vec![255];
        let error = StorageFlags::update_element_flags_with_written_owner(
            &same_size(),
            Some(StorageFlags::SingleEpoch(1).to_element_flags()),
            &mut invalid_new,
        )
        .expect_err("expected a parse error");
        assert!(error
            .to_string()
            .contains("drive did not understand updated item flag information"));
    }

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
}
