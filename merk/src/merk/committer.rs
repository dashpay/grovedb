use grovedb_costs::storage_cost::{
    removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    StorageCost,
};

use crate::{
    merk::{defaults::MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES, BatchValue},
    tree::{kv::ValueDefinedCostType, Commit, TreeNode},
    Error,
};

pub struct MerkCommitter {
    /// The batch has a key, maybe a value, with the value bytes, maybe the left
    /// child size and maybe the right child size, then the
    /// key_value_storage_cost
    pub(in crate::merk) batch: Vec<BatchValue>,
    pub(in crate::merk) height: u8,
    pub(in crate::merk) levels: u8,
}

impl MerkCommitter {
    pub(in crate::merk) fn new(height: u8, levels: u8) -> Self {
        Self {
            batch: Vec::with_capacity(10000),
            height,
            levels,
        }
    }
}

impl Commit for MerkCommitter {
    fn write(
        &mut self,
        tree: &mut TreeNode,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
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
    ) -> Result<(), Error> {
        let tree_size = tree.encoding_length();
        let (mut current_tree_plus_hook_size, mut storage_costs) =
            tree.kv_with_parent_hook_size_and_storage_cost(old_specialized_cost)?;
        let mut i = 0;

        if let Some(old_value) = tree.old_value.clone() {
            // At this point the tree value can be updated based on client requirements
            // For example to store the costs
            loop {
                let (flags_changed, value_defined_cost) = update_tree_value_based_on_costs(
                    &storage_costs.value_storage_cost,
                    &old_value,
                    tree.value_mut_ref(),
                )?;
                if !flags_changed {
                    break;
                } else {
                    tree.inner.kv.value_defined_cost = value_defined_cost;
                    let after_update_tree_plus_hook_size =
                        tree.value_encoding_length_with_parent_to_child_reference();
                    if after_update_tree_plus_hook_size == current_tree_plus_hook_size {
                        break;
                    }
                    let new_size_and_storage_costs =
                        tree.kv_with_parent_hook_size_and_storage_cost(old_specialized_cost)?;
                    current_tree_plus_hook_size = new_size_and_storage_costs.0;
                    storage_costs = new_size_and_storage_costs.1;
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
        }

        // Update old tree size after generating value storage_cost cost
        tree.old_size_with_parent_to_child_hook = current_tree_plus_hook_size;
        tree.old_value = Some(tree.value_ref().clone());

        let mut buf = Vec::with_capacity(tree_size);
        tree.encode_into(&mut buf);

        let left_child_sizes = tree.child_ref_and_sum_size(true);
        let right_child_sizes = tree.child_ref_and_sum_size(false);
        self.batch.push((
            tree.key().to_vec(),
            tree.feature_type().sum_length(),
            Some((buf, left_child_sizes, right_child_sizes)),
            Some(storage_costs),
        ));
        Ok(())
    }

    fn prune(&self, tree: &TreeNode) -> (bool, bool) {
        // keep N top levels of tree
        let prune = (self.height - tree.height()) >= self.levels;
        (prune, prune)
    }
}
