//! Versioned dispatch for the ordinary value replacements on a
//! [`TreeNode`]: [`TreeNode::put_value`] (`Op::Put`) and
//! [`TreeNode::put_value_and_reference_value_hash`]
//! (`Op::PutCombinedReference`).
//!
//! Each `v*.rs` defines the `*_v*` implementations. The dispatchers below
//! select the implementation by
//! `GroveVersion::merk_versions.tree.put_value`:
//!
//! - version 0 keeps whatever `value_defined_cost` metadata the node was
//!   stamped with when it was loaded from storage. If the predecessor was a
//!   specialized element (a tree or a sum item) the replacement is charged
//!   from the predecessor's fixed cost size and the physical-size check is
//!   skipped (issue #908). Grove v1..v3 are consensus-locked to this.
//! - version 1 clears the metadata so an ordinary replacement is charged
//!   from its own serialized bytes and verified against what is written.
//!
//! The specialized replacements (`put_value_with_fixed_cost` and the
//! layered variants) always stamp the metadata from the new operation and
//! are not versioned here.

mod v0;
mod v1;

use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{
    tree::{kv::ValueDefinedCostType, CryptoHash, TreeFeatureType, TreeNode},
    Error,
};

impl TreeNode {
    /// Replaces the root node's value with the given ordinary value and
    /// returns the modified `Tree`. See the module docs for the version
    /// semantics.
    #[inline]
    pub fn put_value(
        self,
        value: Vec<u8>,
        feature_type: TreeFeatureType,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        get_temp_new_value_with_old_flags: &impl Fn(
            &Vec<u8>,
            &Vec<u8>,
        ) -> Result<Option<Vec<u8>>, Error>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &grovedb_costs::storage_cost::StorageCost,
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
        grove_version: &GroveVersion,
    ) -> CostResult<Self, Error> {
        match grove_version.merk_versions.tree.put_value {
            0 => self.put_value_v0(
                value,
                feature_type,
                old_specialized_cost,
                get_temp_new_value_with_old_flags,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            ),
            1 => self.put_value_v1(
                value,
                feature_type,
                old_specialized_cost,
                get_temp_new_value_with_old_flags,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "put_value".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(Default::default()),
        }
    }

    /// Replaces the root node's value with the given ordinary value and
    /// reference value hash and returns the modified `Tree`. See the module
    /// docs for the version semantics.
    #[inline]
    pub fn put_value_and_reference_value_hash(
        self,
        value: Vec<u8>,
        value_hash: CryptoHash,
        feature_type: TreeFeatureType,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        get_temp_new_value_with_old_flags: &impl Fn(
            &Vec<u8>,
            &Vec<u8>,
        ) -> Result<Option<Vec<u8>>, Error>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &grovedb_costs::storage_cost::StorageCost,
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
        grove_version: &GroveVersion,
    ) -> CostResult<Self, Error> {
        match grove_version.merk_versions.tree.put_value {
            0 => self.put_value_and_reference_value_hash_v0(
                value,
                value_hash,
                feature_type,
                old_specialized_cost,
                get_temp_new_value_with_old_flags,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            ),
            1 => self.put_value_and_reference_value_hash_v1(
                value,
                value_hash,
                feature_type,
                old_specialized_cost,
                get_temp_new_value_with_old_flags,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "put_value_and_reference_value_hash".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(Default::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::storage_cost::{
        removal::StorageRemovedBytes::{BasicStorageRemoval, NoStorageRemoval},
        StorageCost,
    };
    use grovedb_element::reference_path::ReferencePathType;
    use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, GroveVersion};

    use grovedb_element::Element;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{test_utils::TempStorage, RocksDbStorage},
        Storage, StorageBatch, StorageContext,
    };

    use crate::{
        element::{costs::ElementCostExtensions, insert::ElementInsertToStorageExtensions},
        tree::hash::NULL_HASH,
        Error, Merk, TreeType,
    };

    const KEY: &[u8] = b"replaced";

    fn big_item() -> Element {
        Element::new_item(vec![9u8; 200])
    }

    fn small_item() -> Element {
        Element::new_item(vec![9u8; 5])
    }

    fn reference() -> Element {
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            b"somewhere".to_vec(),
            vec![1u8; 100],
        ]))
    }

    /// Inserts `element` at `KEY` through the default `Element` insert
    /// entry point for its kind (no custom callbacks).
    fn put<'db, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        element: &Element,
        grove_version: &GroveVersion,
    ) -> Result<StorageCost, Error> {
        let result = if element.is_any_tree() {
            element.insert_subtree(merk, KEY, NULL_HASH, None, grove_version)
        } else if element.is_reference() {
            element.insert_reference(merk, KEY, NULL_HASH, None, grove_version)
        } else {
            element.insert(merk, KEY, None, grove_version)
        };
        result.cost_as_result().map(|cost| cost.storage_cost)
    }

    /// Applies one element put the way GroveDB does: the Merk is opened on a
    /// fresh storage batch with the element cost callback that stamps loaded
    /// nodes with their element's `value_defined_cost`, the put is applied,
    /// and the batch is committed. The committed batch prices every written
    /// node (and runs the physical-size verification), so its cost is the
    /// operation's storage cost.
    fn step<'db>(
        storage: &'db RocksDbStorage,
        tx: &'db <RocksDbStorage as Storage<'db>>::Transaction,
        tree_type: TreeType,
        element: &Element,
        grove_version: &GroveVersion,
    ) -> Result<StorageCost, Error> {
        let batch = StorageBatch::new();
        let mut merk = Merk::open_base(
            storage
                .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), tx)
                .unwrap(),
            tree_type,
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .unwrap()
        .expect("open merk");
        put(&mut merk, element, grove_version)?;
        drop(merk);
        storage
            .commit_multi_context_batch(batch, Some(tx))
            .cost_as_result()
            .map(|cost| cost.storage_cost)
            .map_err(Error::StorageError)
    }

    /// Inserts `before`, then (from a fresh Merk, so the node is reloaded
    /// from storage and stamped with `before`'s `value_defined_cost`)
    /// replaces it with `after` and returns that replacement's storage cost.
    fn replace(
        tree_type: TreeType,
        before: &Element,
        after: &Element,
        grove_version: &GroveVersion,
    ) -> Result<StorageCost, Error> {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        step(&storage, &tx, tree_type, before, grove_version).expect("predecessor");
        step(&storage, &tx, tree_type, after, grove_version)
    }

    /// The storage cost of `element`'s node bytes in a `tree_type` Merk,
    /// as the old-value cost callback computes it from stored bytes.
    fn node_cost(element: &Element, tree_type: TreeType, grove_version: &GroveVersion) -> u32 {
        let bytes = element.serialize(grove_version).expect("serialize");
        Element::specialized_costs_for_key_value(
            KEY,
            &bytes,
            tree_type.inner_node_type(),
            grove_version,
        )
        .expect("cost")
    }

    fn cases() -> Vec<(&'static str, TreeType, Element, Element, Element)> {
        vec![
            // (name, merk type, specialized predecessor, ordinary control predecessor, replacement)
            (
                "sum item -> item",
                TreeType::SumTree,
                Element::new_sum_item(5),
                small_item(),
                big_item(),
            ),
            (
                "sum item -> reference",
                TreeType::SumTree,
                Element::new_sum_item(5),
                small_item(),
                reference(),
            ),
            (
                "tree -> item",
                TreeType::NormalTree,
                Element::empty_tree(),
                small_item(),
                big_item(),
            ),
            (
                "sum tree -> item",
                TreeType::NormalTree,
                Element::empty_sum_tree(),
                small_item(),
                big_item(),
            ),
            (
                "tree -> reference",
                TreeType::NormalTree,
                Element::empty_tree(),
                small_item(),
                reference(),
            ),
        ]
    }

    #[test]
    fn v4_charges_ordinary_replacement_from_its_own_bytes() {
        for (name, tree_type, specialized, ordinary, after) in cases() {
            let subject = replace(tree_type, &specialized, &after, &GROVE_V4).expect(name);
            let control = replace(tree_type, &ordinary, &after, &GROVE_V4).expect(name);

            let new_cost = node_cost(&after, tree_type, &GROVE_V4);
            let old_cost = node_cost(&specialized, tree_type, &GROVE_V4);
            assert!(
                new_cost > old_cost,
                "{name}: replacement must outgrow the predecessor"
            );

            // Growth over the stored predecessor is charged as added bytes,
            // measured from the replacement's own serialized bytes.
            assert_eq!(subject.added_bytes, new_cost - old_cost, "{name}: added");
            assert_eq!(subject.removed_bytes, NoStorageRemoval, "{name}: removed");
            // Same bytes on disk as the ordinary -> ordinary control, same
            // total charge.
            assert_eq!(
                subject.added_bytes + subject.replaced_bytes,
                control.added_bytes + control.replaced_bytes,
                "{name}: total"
            );
            // And the control itself obeys the same formula.
            assert_eq!(
                control.added_bytes,
                new_cost - node_cost(&ordinary, tree_type, &GROVE_V4),
                "{name}: control added"
            );
        }
    }

    #[test]
    fn v4_added_bytes_mirror_reverse_removed_bytes() {
        for (name, tree_type, specialized, _ordinary, after) in cases() {
            let forward = replace(tree_type, &specialized, &after, &GROVE_V4).expect(name);
            let reverse = replace(tree_type, &after, &specialized, &GROVE_V4).expect(name);
            assert_eq!(
                reverse.removed_bytes,
                BasicStorageRemoval(forward.added_bytes),
                "{name}: reverse must free what forward charged"
            );
            assert_eq!(reverse.added_bytes, 0, "{name}");
        }
    }

    /// GROVE_V3 is live: the replacement is charged at the predecessor's
    /// fixed size (no added bytes) even though the full item is written.
    #[test]
    fn v3_keeps_predecessor_fixed_cost() {
        for (name, tree_type, specialized, _ordinary, after) in cases() {
            let legacy = replace(tree_type, &specialized, &after, &GROVE_V3).expect(name);
            assert_eq!(legacy.added_bytes, 0, "{name}: legacy added");
            assert_eq!(
                legacy.removed_bytes, NoStorageRemoval,
                "{name}: legacy removed"
            );
            let fixed = replace(tree_type, &specialized, &after, &GROVE_V4).expect(name);
            assert_eq!(
                fixed.replaced_bytes, legacy.replaced_bytes,
                "{name}: the replaced part (predecessor size) is version-independent"
            );
        }
    }

    /// Specialized puts stamp the metadata from the new op on every version,
    /// and ordinary -> ordinary replacements never had stale metadata.
    #[test]
    fn unaffected_replacements_identical_across_versions() {
        for (name, tree_type, before, after) in [
            ("item -> item", TreeType::SumTree, small_item(), big_item()),
            (
                "item -> reference",
                TreeType::SumTree,
                small_item(),
                reference(),
            ),
            (
                "item -> sum item",
                TreeType::SumTree,
                big_item(),
                Element::new_sum_item(5),
            ),
            (
                "item -> tree",
                TreeType::NormalTree,
                big_item(),
                Element::empty_tree(),
            ),
            (
                "sum item -> sum item",
                TreeType::SumTree,
                Element::new_sum_item(5),
                Element::new_sum_item(6),
            ),
            (
                "tree -> sum tree",
                TreeType::NormalTree,
                Element::empty_tree(),
                Element::empty_sum_tree(),
            ),
        ] {
            let v3 = replace(tree_type, &before, &after, &GROVE_V3).expect(name);
            let v4 = replace(tree_type, &before, &after, &GROVE_V4).expect(name);
            assert_eq!(v3, v4, "{name}");
        }
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut future = GROVE_V4.clone();
        future.merk_versions.tree.put_value = 2;
        for (name, tree_type, _specialized, ordinary, after) in cases() {
            let err = replace(tree_type, &ordinary, &after, &future)
                .expect_err("unknown put_value version must be refused");
            // `Element::insert` wraps merk errors, so match on the message.
            assert!(
                err.to_string().contains("put_value") && err.to_string().contains("received: 2"),
                "{name}: {err}"
            );
        }
        // A brand-new node never goes through `put_value`, so first inserts
        // are unaffected by the gate.
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        step(&storage, &tx, TreeType::SumTree, &big_item(), &future).expect("fresh insert");
    }
}
