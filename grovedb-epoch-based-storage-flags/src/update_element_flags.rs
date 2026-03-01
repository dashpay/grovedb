use grovedb_costs::storage_cost::{transition::OperationStorageTransitionType, StorageCost};

use crate::{error::StorageFlagsError, ElementFlags, MergingOwnersStrategy, StorageFlags};

impl StorageFlags {
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
        let binding = maybe_old_storage_flags.clone().unwrap();
        let old_epoch_index_map = binding.epoch_index_map();
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
