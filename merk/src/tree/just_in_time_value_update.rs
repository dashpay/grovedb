use grovedb_costs::storage_cost::{
    removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    StorageCost,
};

use crate::{
    merk::defaults::MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES,
    tree::{
        kv::{ValueDefinedCostType, KV},
        TreeFeatureType, TreeNode, TreeNodeInner, NULL_HASH,
    },
    Error,
};

impl TreeNode {
    /// The value bytes an apply finally stores when an
    /// [`Op::PutWithProvidedValueHash`](crate::tree::Op::PutWithProvidedValueHash)
    /// of `new_value` replaces the stored `old_value` — after the
    /// just-in-time value update has run the client callbacks (storage-flags
    /// carry-over and rewrite).
    ///
    /// Runs the apply's own steps on a detached node: the node holding
    /// `old_value` takes the new value and feature type exactly as
    /// `put_value_with_provided_value_hash` installs them (which drops any
    /// value-defined cost the predecessor carried), and then goes through
    /// [`Self::just_in_time_tree_node_value_update`]. With the same
    /// (deterministic) callbacks the result is byte-for-byte what the apply
    /// writes, so a caller that must commit to the final bytes BEFORE the
    /// apply — a backward-references referrer holding its target's hash —
    /// can compute them.
    #[allow(clippy::too_many_arguments)]
    pub fn provided_value_hash_put_final_value(
        key: Vec<u8>,
        old_value: Vec<u8>,
        new_value: Vec<u8>,
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
    ) -> Result<Vec<u8>, Error> {
        // The stored node (no hashes are needed: the update only measures
        // sizes and consults the callbacks).
        let kv = KV::from_fields(key, old_value, NULL_HASH, NULL_HASH, feature_type);
        let mut node = TreeNode::new_with_tree_inner(TreeNodeInner {
            left: None,
            right: None,
            kv,
        });
        // The put, as `put_value_with_provided_value_hash` performs it.
        node.inner.kv = node
            .inner
            .kv
            .put_ordinary_value_no_update_of_hashes(new_value);
        node.inner.kv.feature_type = feature_type;
        node.just_in_time_tree_node_value_update(
            old_specialized_cost,
            get_temp_new_value_with_old_flags,
            update_tree_value_based_on_costs,
            section_removal_bytes,
        )?;
        Ok(node.inner.kv.value)
    }

    pub(in crate::tree) fn just_in_time_tree_node_value_update(
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
        let mut i = 0;

        if let Some(old_value) = self.old_value.clone() {
            // At this point the tree value can be updated based on client requirements
            // For example to store the costs
            // todo: clean up clones
            let original_new_value = self.value_ref().clone();

            let new_value_with_old_flags = if self.inner.kv.value_defined_cost.is_none() {
                // for items
                get_temp_new_value_with_old_flags(&old_value, &original_new_value)?
            } else {
                // don't do this for sum items or trees
                None
            };

            let (mut current_tree_plus_hook_size, mut storage_costs) = self
                .kv_with_parent_hook_size_and_storage_cost_change_for_value(
                    old_specialized_cost,
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

                let (flags_changed, value_defined_cost) = update_tree_value_based_on_costs(
                    &storage_costs.value_storage_cost,
                    &old_value,
                    self.value_mut_ref(),
                )?;
                if !flags_changed {
                    break;
                } else {
                    self.inner.kv.value_defined_cost = value_defined_cost;
                    let after_update_tree_plus_hook_size =
                        self.value_encoding_length_with_parent_to_child_reference();
                    if after_update_tree_plus_hook_size == current_tree_plus_hook_size {
                        break;
                    }
                    // we are calling this with merged flags that are were put in through value mut
                    // ref
                    let new_size_and_storage_costs =
                        self.kv_with_parent_hook_size_and_storage_cost(old_specialized_cost)?;
                    current_tree_plus_hook_size = new_size_and_storage_costs.0;
                    storage_costs = new_size_and_storage_costs.1;
                    self.set_value(original_new_value.clone())
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

#[cfg(all(test, feature = "full"))]
mod tests {
    use grovedb_costs::storage_cost::{
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    };
    use grovedb_element::{BackwardReferences, Element};
    use grovedb_version::version::GroveVersion;

    use crate::{
        merk::NodeType,
        test_utils::TempMerk,
        tree::{kv::ValueDefinedCostType, kv::KV, TreeFeatureType::BasicMerkNode, TreeNode},
        Error, Op,
    };

    // The signature is the callback type Merk's apply takes.
    #[allow(clippy::ptr_arg)]
    fn old_cost(key: &Vec<u8>, value: &Vec<u8>) -> Result<u32, Error> {
        Ok(KV::node_value_byte_cost_size(
            key.len() as u32,
            value.len() as u32,
            NodeType::NormalNode,
        ))
    }

    fn no_temp_value(_old: &Vec<u8>, _new: &Vec<u8>) -> Result<Option<Vec<u8>>, Error> {
        Ok(None)
    }

    /// Stamps the measured storage change into the new element's flags, so
    /// the final bytes depend on every size the update measures (and the
    /// stamp's own length forces the re-measuring loop to run).
    fn stamp_costs(
        cost: &StorageCost,
        _old: &Vec<u8>,
        new: &mut Vec<u8>,
    ) -> Result<(bool, Option<ValueDefinedCostType>), Error> {
        let grove_version = GroveVersion::latest();
        let mut element = Element::deserialize(new, grove_version)
            .map_err(|e| Error::ClientCorruptionError(e.to_string()))?;
        let removed = match cost.removed_bytes {
            BasicStorageRemoval(bytes) => bytes,
            _ => 0,
        };
        let mut stamp = cost.added_bytes.to_be_bytes().to_vec();
        stamp.extend(removed.to_be_bytes());
        stamp.extend(cost.replaced_bytes.to_be_bytes());
        element.set_flags(Some(stamp));
        *new = element
            .serialize(grove_version)
            .map_err(|e| Error::ClientCorruptionError(e.to_string()))?;
        Ok((true, None))
    }

    fn basic_removal(
        _value: &Vec<u8>,
        key_bytes: u32,
        value_bytes: u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
        Ok((
            BasicStorageRemoval(key_bytes),
            BasicStorageRemoval(value_bytes),
        ))
    }

    fn apply(merk: &mut TempMerk, op: Op, grove_version: &GroveVersion) {
        merk.apply_with_costs_just_in_time_value_update::<_, Vec<u8>>(
            &[(b"key".to_vec(), op)],
            &[],
            None,
            &old_cost,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            &no_temp_value,
            &mut stamp_costs,
            &mut basic_removal,
            grove_version,
        )
        .unwrap()
        .expect("apply");
    }

    /// The detached prediction stores exactly what a real provided-hash put
    /// over a stored value stores, for a growing and a shrinking write.
    #[test]
    fn provided_value_hash_put_final_value_matches_the_apply() {
        let grove_version = GroveVersion::latest();
        let item = |value: &[u8], flags: u8| {
            Element::ItemWithBackwardsReferences(
                value.to_vec(),
                BackwardReferences::default(),
                Some(vec![flags]),
            )
            .serialize(grove_version)
            .unwrap()
        };
        for (old, new) in [
            (item(b"old", 1), item(b"a much longer new value", 2)),
            (item(b"a much longer old value", 1), item(b"new", 2)),
        ] {
            let mut merk = TempMerk::new(grove_version);
            apply(
                &mut merk,
                Op::Put(old.clone(), BasicMerkNode),
                grove_version,
            );
            merk.commit(grove_version);

            let predicted = TreeNode::provided_value_hash_put_final_value(
                b"key".to_vec(),
                old.clone(),
                new.clone(),
                BasicMerkNode,
                &old_cost,
                &no_temp_value,
                &mut stamp_costs,
                &mut basic_removal,
            )
            .expect("prediction");
            assert_ne!(predicted, new, "the callback rewrote the flags");

            apply(
                &mut merk,
                Op::PutWithProvidedValueHash(new, [0; 32], None, BasicMerkNode),
                grove_version,
            );
            let stored = merk
                .get(
                    b"key",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version,
                )
                .unwrap()
                .unwrap()
                .expect("stored");
            assert_eq!(predicted, stored);
        }
    }
}
