//! The just-in-time value update a replacement of a stored value runs: the
//! client callbacks carry the old storage flags into the size measurement,
//! rewrite the new value's flags, and section the bytes the replacement
//! frees.
//!
//! Each `v*.rs` defines a `just_in_time_tree_node_value_update_v*`. The
//! dispatcher below selects it by
//! `GroveVersion::merk_versions.tree.just_in_time_value_update`:
//!
//! - version 0 keeps the first measurement, taken with the new value
//!   carrying the OLD value's flags, when the flags-update callback answers
//!   `Unchanged`. A new value stored with flags of a different length is
//!   then charged for bytes it does not hold, and the commit fails with a
//!   storage cost mismatch. Grove v1..v3 are consensus-locked to this.
//! - version 1 measures the replacement again from the bytes it stores when
//!   their size differs from the last measurement taken, and restores the
//!   put's value-defined cost each time it restores the put's value. Every
//!   update whose cost version 0 records for the bytes it stores is charged
//!   exactly as version 0 charges it.

mod v0;
mod v1;

use grovedb_costs::storage_cost::{
    key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes,
    transition::ElementFlagsUpdate, StorageCost,
};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{
    tree::{
        kv::{ValueDefinedCostType, KV},
        TreeFeatureType, TreeNode, TreeNodeInner, NULL_HASH,
    },
    Error,
};

/// The put whose final bytes [`TreeNode::provided_value_hash_put_final_value`]
/// predicts: how it installs the new value on the stored node, which decides
/// the value-defined cost the just-in-time value update measures with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredictedPut {
    /// An `Op::Put`: installed as [`TreeNode::put_value`] installs an
    /// ordinary value at the grove version (merk tree version 0 keeps the
    /// value-defined cost the stored node was loaded with).
    Ordinary,
    /// An [`Op::PutWithProvidedValueHash`](crate::tree::Op::PutWithProvidedValueHash),
    /// which drops any value-defined cost.
    ProvidedValueHash,
    /// An `Op::PutWithSpecializedCost`, at the op's own cost.
    SpecializedCost(u32),
}

impl TreeNode {
    /// The value bytes an apply finally stores when `put` of `new_value`
    /// replaces the stored `old_value` — after the just-in-time value update
    /// has run the client callbacks (storage-flags carry-over and rewrite).
    ///
    /// Runs the apply's own steps on a detached node: the node holding
    /// `old_value`, loaded with `old_value_defined_cost` as the apply loads
    /// it, takes the new value and feature type exactly as `put` installs
    /// them, and then goes through the just-in-time value update
    /// (`just_in_time_tree_node_value_update`). With the same
    /// (deterministic) callbacks the result is byte-for-byte what the apply
    /// writes, so a caller that must commit to the final bytes BEFORE the
    /// apply — a reference holding its target's hash — can compute them.
    #[allow(clippy::too_many_arguments)]
    pub fn provided_value_hash_put_final_value(
        key: Vec<u8>,
        old_value: Vec<u8>,
        old_value_defined_cost: Option<ValueDefinedCostType>,
        new_value: Vec<u8>,
        feature_type: TreeFeatureType,
        put: PredictedPut,
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
        grove_version: &GroveVersion,
    ) -> Result<Vec<u8>, Error> {
        // The stored node as loaded (no hashes are needed: the update only
        // measures sizes and consults the callbacks); its value is installed
        // by the put below, and the stored bytes are its old value.
        let mut kv = KV::from_fields(key, Vec::new(), NULL_HASH, NULL_HASH, feature_type);
        kv.value_defined_cost = old_value_defined_cost;
        // The put, as the apply performs it.
        let mut kv = match put {
            PredictedPut::Ordinary => Self::install_ordinary_value(kv, new_value, grove_version)?,
            PredictedPut::ProvidedValueHash => kv.put_ordinary_value_no_update_of_hashes(new_value),
            PredictedPut::SpecializedCost(value_cost) => kv
                .put_value_with_fixed_cost_no_update_of_hashes(
                    new_value,
                    ValueDefinedCostType::SpecializedValueDefinedCost(value_cost),
                ),
        };
        kv.feature_type = feature_type;
        let mut node = TreeNode {
            inner: Box::new(TreeNodeInner {
                left: None,
                right: None,
                kv,
            }),
            old_value: Some(old_value),
            known_storage_cost: None,
        };
        node.just_in_time_tree_node_value_update(
            old_specialized_cost,
            get_temp_new_value_with_old_flags,
            update_tree_value_based_on_costs,
            section_removal_bytes,
            grove_version,
        )?;
        Ok(node.inner.kv.value)
    }

    /// Runs the just-in-time value update on a node that has taken a new
    /// value over a stored one. See the module docs for the version
    /// semantics.
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
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        match grove_version.merk_versions.tree.just_in_time_value_update {
            0 => self.just_in_time_tree_node_value_update_v0(
                old_specialized_cost,
                get_temp_new_value_with_old_flags,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            ),
            1 => self.just_in_time_tree_node_value_update_v1(
                old_specialized_cost,
                get_temp_new_value_with_old_flags,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "just_in_time_tree_node_value_update".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }

    /// The storage an update that settles an owner change records: the old
    /// element removed, key included, with the removal sectioned through its
    /// flags exactly as a deletion sections it, and the new element as this
    /// node now holds it added, key included. `old_value_bytes` is the
    /// stored value's own cost.
    fn settled_owner_change_storage_cost(
        &self,
        old_value: &Vec<u8>,
        old_value_bytes: u32,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> Result<KeyValueStorageCost, Error> {
        let key_bytes = KV::node_key_byte_cost_size(self.key().len() as u32);
        let (removed_key_bytes, removed_value_bytes) =
            section_removal_bytes(old_value, key_bytes, old_value_bytes)?;
        Ok(KeyValueStorageCost {
            key_storage_cost: StorageCost {
                added_bytes: key_bytes,
                replaced_bytes: 0,
                removed_bytes: removed_key_bytes,
            },
            value_storage_cost: StorageCost {
                added_bytes: self.value_encoding_length_with_parent_to_child_reference(),
                replaced_bytes: 0,
                removed_bytes: removed_value_bytes,
            },
            new_node: false,
            needs_value_verification: self.inner.kv.value_defined_cost.is_none(),
            prepaid: false,
        })
    }
}

#[cfg(all(test, feature = "full"))]
mod tests {
    use grovedb_costs::storage_cost::{
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        transition::ElementFlagsUpdate,
        StorageCost,
    };
    use grovedb_element::{BackwardReferences, Element};
    use grovedb_version::version::GroveVersion;

    use crate::{
        merk::NodeType,
        test_utils::TempMerk,
        tree::{
            just_in_time_value_update::PredictedPut, kv::ValueDefinedCostType, kv::KV,
            TreeFeatureType::BasicMerkNode, TreeNode, TreeNodeInner, NULL_HASH,
        },
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
    ) -> Result<(ElementFlagsUpdate, Option<ValueDefinedCostType>), Error> {
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
        Ok((ElementFlagsUpdate::Changed, None))
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
                None,
                new.clone(),
                BasicMerkNode,
                PredictedPut::ProvidedValueHash,
                &old_cost,
                &no_temp_value,
                &mut stamp_costs,
                &mut basic_removal,
                grove_version,
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

    /// An update whose flags update settles an owner change is accounted as
    /// the removal of the old value plus the insertion of the new one: every
    /// byte of both, key included, with the old value sectioned by the
    /// removal callback exactly as a deletion sections it.
    #[test]
    fn a_settled_owner_change_removes_the_old_value_and_inserts_the_new_one() {
        let grove_version = GroveVersion::latest();
        let key = b"key".to_vec();
        let item = |value: &[u8], flags: u8| {
            Element::new_item_with_flags(value.to_vec(), Some(vec![flags]))
                .serialize(grove_version)
                .unwrap()
        };
        for (old, new) in [
            (item(b"old", 1), item(b"a much longer new value", 2)),
            (item(b"a much longer old value", 1), item(b"new", 2)),
            (item(b"same", 1), item(b"size", 2)),
        ] {
            let kv = KV::from_fields(
                key.clone(),
                old.clone(),
                NULL_HASH,
                NULL_HASH,
                BasicMerkNode,
            );
            let mut node = TreeNode::new_with_tree_inner(TreeNodeInner {
                left: None,
                right: None,
                kv,
            });
            node.inner.kv = node
                .inner
                .kv
                .put_ordinary_value_no_update_of_hashes(new.clone());

            let mut sectioned = vec![];
            node.just_in_time_tree_node_value_update(
                &old_cost,
                &no_temp_value,
                &mut |_cost, _old, _new| Ok((ElementFlagsUpdate::SettleOwnerChange, None)),
                &mut |value, key_bytes, value_bytes| {
                    sectioned.push((value.clone(), key_bytes, value_bytes));
                    basic_removal(value, key_bytes, value_bytes)
                },
                grove_version,
            )
            .expect("expected the update to settle");

            let key_bytes = KV::node_key_byte_cost_size(key.len() as u32);
            let old_value_bytes = old_cost(&key, &old).unwrap();
            let new_value_bytes = old_cost(&key, &new).unwrap();
            // (a shrink also sections its freed bytes before the flags
            // update is asked, as it always did)
            assert_eq!(
                sectioned.last(),
                Some(&(old.clone(), key_bytes, old_value_bytes))
            );
            let storage_cost = node.known_storage_cost.expect("expected a storage cost");
            assert_eq!(
                storage_cost.key_storage_cost,
                StorageCost {
                    added_bytes: key_bytes,
                    replaced_bytes: 0,
                    removed_bytes: BasicStorageRemoval(key_bytes),
                }
            );
            assert_eq!(
                storage_cost.value_storage_cost,
                StorageCost {
                    added_bytes: new_value_bytes,
                    replaced_bytes: 0,
                    removed_bytes: BasicStorageRemoval(old_value_bytes),
                }
            );
            assert!(!storage_cost.new_node);
            assert_eq!(node.inner.kv.value, new);
            assert_eq!(node.old_value, Some(new));
        }
    }
}
