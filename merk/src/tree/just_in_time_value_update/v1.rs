use grovedb_costs::storage_cost::{
    removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    transition::ElementFlagsUpdate,
    StorageCost,
};

use crate::{
    merk::defaults::MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES,
    tree::{kv::ValueDefinedCostType, TreeNode},
    Error,
};

impl TreeNode {
    /// Version 1 of the just-in-time value update, selected by
    /// `merk_versions.tree.just_in_time_value_update == 1` (grove v4+).
    ///
    /// It differs from version 0 only in what an `Unchanged` answer keeps:
    /// when the value the update stores is not the size the last
    /// measurement took (the first one sizes the new value with the OLD
    /// value's flags), the replacement is measured again from the stored
    /// bytes, so the recorded cost is the cost of what is written. The
    /// stored value's own cost is also measured once rather than on every
    /// measurement, which leaves every figure unchanged.
    pub(super) fn just_in_time_tree_node_value_update_v1(
        &mut self,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        get_temp_new_value_with_old_flags: &impl Fn(
            &Vec<u8>,
            &Vec<u8>,
        ) -> Result<Option<Vec<u8>>, Error>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<
            (ElementFlagsUpdate, Option<ValueDefinedCostType>),
            Error,
        >,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> Result<(), Error> {
        let mut i = 0;

        if let Some(old_value) = self.old_value.clone() {
            // At this point the tree value can be updated based on client requirements
            // For example to store the costs
            // todo: clean up clones
            let original_new_value = self.value_ref().clone();

            // The stored value does not change while the update runs, so its
            // cost is measured once.
            let old_value_bytes = old_specialized_cost(self.key_as_ref(), &old_value)?;
            let old_cost =
                |_key: &Vec<u8>, _value: &Vec<u8>| -> Result<u32, Error> { Ok(old_value_bytes) };

            let new_value_with_old_flags = if self.inner.kv.value_defined_cost.is_none() {
                // for items
                get_temp_new_value_with_old_flags(&old_value, &original_new_value)?
            } else {
                // don't do this for sum items or trees
                None
            };

            let (mut current_tree_plus_hook_size, mut storage_costs) = self
                .kv_with_parent_hook_size_and_storage_cost_change_for_value(
                    &old_cost,
                    new_value_with_old_flags,
                )?;

            loop {
                if let BasicStorageRemoval(removed_bytes) =
                    storage_costs.value_storage_cost.removed_bytes
                {
                    let (_, value_removed_bytes) =
                        section_removal_bytes(&old_value, 0, removed_bytes)?;
                    storage_costs.value_storage_cost.removed_bytes = value_removed_bytes;
                }

                let (flags_update, value_defined_cost) = update_tree_value_based_on_costs(
                    &storage_costs.value_storage_cost,
                    &old_value,
                    self.value_mut_ref(),
                )?;
                match flags_update {
                    ElementFlagsUpdate::Unchanged => {
                        // The value is stored as it stands. When that is not
                        // the size the last measurement took, the
                        // replacement is measured from its own bytes.
                        if self.value_encoding_length_with_parent_to_child_reference()
                            != current_tree_plus_hook_size
                        {
                            storage_costs =
                                self.kv_with_parent_hook_size_and_storage_cost(&old_cost)?.1;
                        }
                        break;
                    }
                    ElementFlagsUpdate::SettleOwnerChange => {
                        // The value now holds the new element as the client
                        // left it, and the update is accounted as the removal
                        // of the old element plus the insertion of this one.
                        self.inner.kv.value_defined_cost = value_defined_cost;
                        self.known_storage_cost = Some(self.settled_owner_change_storage_cost(
                            &old_value,
                            old_value_bytes,
                            section_removal_bytes,
                        )?);
                        self.old_value = Some(self.value_ref().clone());
                        return Ok(());
                    }
                    ElementFlagsUpdate::Changed => {
                        self.inner.kv.value_defined_cost = value_defined_cost;
                        let after_update_tree_plus_hook_size =
                            self.value_encoding_length_with_parent_to_child_reference();
                        if after_update_tree_plus_hook_size == current_tree_plus_hook_size {
                            break;
                        }
                        // we are calling this with merged flags that are were put in through value
                        // mut ref
                        let new_size_and_storage_costs =
                            self.kv_with_parent_hook_size_and_storage_cost(&old_cost)?;
                        current_tree_plus_hook_size = new_size_and_storage_costs.0;
                        storage_costs = new_size_and_storage_costs.1;
                        self.set_value(original_new_value.clone())
                    }
                }
                if i > MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES {
                    return Err(Error::CyclicError(
                        "updated value based on costs too many times",
                    ));
                }
                i += 1;
            }

            if let BasicStorageRemoval(removed_bytes) =
                storage_costs.value_storage_cost.removed_bytes
            {
                let (_, value_removed_bytes) = section_removal_bytes(&old_value, 0, removed_bytes)?;
                storage_costs.value_storage_cost.removed_bytes = value_removed_bytes;
            }
            self.known_storage_cost = Some(storage_costs);
        } else {
            let (_, storage_costs) =
                self.kv_with_parent_hook_size_and_storage_cost(old_specialized_cost)?;
            self.known_storage_cost = Some(storage_costs);
        }

        self.old_value = Some(self.value_ref().clone());

        Ok(())
    }
}
