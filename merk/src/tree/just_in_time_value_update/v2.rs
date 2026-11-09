use grovedb_costs::storage_cost::{
    removal::StorageRemovedBytes, transition::ElementFlagsUpdate, StorageCost,
};

use crate::{
    tree::{kv::ValueDefinedCostType, TreeNode},
    Error,
};

impl TreeNode {
    /// Version 2 of the just-in-time value update, selected by
    /// `merk_versions.tree.just_in_time_value_update == 2` (grove v5+).
    ///
    /// Version 1, plus the [`ElementFlagsUpdate::AccountAsReplacement`] answer
    /// version 1 refuses: the update is recorded as the removal of the old
    /// element plus the insertion of the new one, as the callback left it.
    pub(super) fn just_in_time_tree_node_value_update_v2(
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
        let old_value = self.old_value.clone();
        let mut replacement_value_defined_cost = None;
        let outcome = self.just_in_time_tree_node_value_update_v1(
            old_specialized_cost,
            get_temp_new_value_with_old_flags,
            &mut |cost: &StorageCost, old: &Vec<u8>, new: &mut Vec<u8>| {
                let answer = update_tree_value_based_on_costs(cost, old, new)?;
                if answer.0 == ElementFlagsUpdate::AccountAsReplacement {
                    replacement_value_defined_cost = Some(answer.1.clone());
                }
                Ok(answer)
            },
            section_removal_bytes,
        );
        // Version 1 refuses the answer as soon as it gets it, before touching
        // the node, so the node holds the new element as the callback left it
        // and its refusal is this version's way out of the loop. Any other
        // outcome is version 1's own.
        let (Some(value_defined_cost), Some(old_value)) =
            (replacement_value_defined_cost, old_value)
        else {
            return outcome;
        };
        self.inner.kv.value_defined_cost = value_defined_cost;
        let old_value_bytes = old_specialized_cost(self.key_as_ref(), &old_value)?;
        self.known_storage_cost = Some(self.replacement_accounting_storage_cost(
            &old_value,
            old_value_bytes,
            section_removal_bytes,
        )?);
        self.old_value = Some(self.value_ref().clone());
        Ok(())
    }
}
