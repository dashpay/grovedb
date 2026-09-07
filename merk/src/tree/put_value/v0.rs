//! Version 0 of the ordinary value replacements. Keeps the `value_defined_cost`
//! metadata the node was loaded with, so a specialized predecessor's fixed
//! cost size leaks into the charge for an ordinary replacement (issue #908).
//! Consensus-locked for grove v1..v3; do not change.

use grovedb_costs::{
    cost_return_on_error_no_add,
    storage_cost::{removal::StorageRemovedBytes, StorageCost},
    CostResult, CostsExt, OperationCost,
};

use crate::{
    tree::{kv::ValueDefinedCostType, CryptoHash, TreeFeatureType, TreeNode},
    Error,
};

impl TreeNode {
    /// Version 0 of [`TreeNode::put_value`].
    #[inline]
    pub(super) fn put_value_v0(
        mut self,
        value: Vec<u8>,
        feature_type: TreeFeatureType,
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
            (bool, Option<ValueDefinedCostType>),
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
    ) -> CostResult<Self, Error> {
        let mut cost = OperationCost::default();

        self.inner.kv = self.inner.kv.put_value_no_update_of_hashes(value);
        self.inner.kv.feature_type = feature_type;

        if self.old_value.is_some() {
            // we are replacing a value
            // in this case there is a possibility that the client would want to update the
            // element flags based on the change of values
            cost_return_on_error_no_add!(
                cost,
                self.just_in_time_tree_node_value_update(
                    old_specialized_cost,
                    get_temp_new_value_with_old_flags,
                    update_tree_value_based_on_costs,
                    section_removal_bytes
                )
            );
        }

        self.inner.kv = self.inner.kv.update_hashes().unwrap_add_cost(&mut cost);
        Ok(self).wrap_with_cost(cost)
    }

    /// Version 0 of [`TreeNode::put_value_and_reference_value_hash`].
    #[inline]
    pub(super) fn put_value_and_reference_value_hash_v0(
        mut self,
        value: Vec<u8>,
        value_hash: CryptoHash,
        feature_type: TreeFeatureType,
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
            (bool, Option<ValueDefinedCostType>),
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
    ) -> CostResult<Self, Error> {
        let mut cost = OperationCost::default();

        self.inner.kv = self.inner.kv.put_value_no_update_of_hashes(value);
        self.inner.kv.feature_type = feature_type;

        if self.old_value.is_some() {
            // we are replacing a value
            // in this case there is a possibility that the client would want to update the
            // element flags based on the change of values
            cost_return_on_error_no_add!(
                cost,
                self.just_in_time_tree_node_value_update(
                    old_specialized_cost,
                    get_temp_new_value_with_old_flags,
                    update_tree_value_based_on_costs,
                    section_removal_bytes
                )
            );
        }

        self.inner.kv = self
            .inner
            .kv
            .update_hashes_using_reference_value_hash(value_hash)
            .unwrap_add_cost(&mut cost);
        Ok(self).wrap_with_cost(cost)
    }
}
