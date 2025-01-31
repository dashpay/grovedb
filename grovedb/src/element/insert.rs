//! Insert
//! Implements functions in Element for inserting into Merk

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::{BatchEntry, Error as MerkError, Merk, MerkOptions, Op, TreeFeatureType};
use grovedb_storage::StorageContext;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};
use integer_encoding::VarInt;

use crate::{Element, Element::SumItem, Error, Hash};

#[derive(Debug)]
pub struct Delta<'e> {
    pub new: &'e Element,
    pub old: Option<Element>,
}

impl Delta<'_> {
    pub(crate) fn has_changed(&self) -> bool {
        self.old.as_ref().map(|o| o != self.new).unwrap_or(true)
    }
}

impl Element {
    #[cfg(feature = "minimal")]
    /// Insert an element in Merk under a key; path should be resolved and
    /// proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        use grovedb_version::dispatch_version;

        dispatch_version!(
            "element.insert",
            grove_version.grovedb_versions.element.insert,
            0 | 1 => {}
        );

        let serialized = cost_return_on_error_default!(self.serialize(grove_version));

        if !merk.tree_type.allows_sum_item() && self.is_sum_item() {
            return Err(Error::InvalidInput("cannot add sum item to non sum tree"))
                .wrap_with_cost(Default::default());
        }

        let merk_feature_type =
            cost_return_on_error_default!(self.get_feature_type(merk.tree_type));
        let batch_operations = if matches!(self, SumItem(..)) {
            let value_cost =
                cost_return_on_error_default!(self.get_specialized_cost(grove_version));

            let cost = value_cost
                + self.get_flags().as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
            [(
                key,
                Op::PutWithSpecializedCost(serialized, cost, merk_feature_type),
            )]
        } else {
            [(key, Op::Put(serialized, merk_feature_type))]
        };
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch_operations,
            &[],
            options,
            &|key, value| {
                // it is possible that a normal item was being replaced with a
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "minimal")]
    /// Add to batch operations a "Put" op with key and serialized element.
    /// Return CostResult.
    pub fn insert_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_into_batch_operations
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let entry = if matches!(self, SumItem(..)) {
            let value_cost =
                cost_return_on_error_default!(self.get_specialized_cost(grove_version));

            let cost = value_cost
                + self.get_flags().as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
            (
                key,
                Op::PutWithSpecializedCost(serialized, cost, feature_type),
            )
        } else {
            (key, Op::Put(serialized, feature_type))
        };
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    #[cfg(feature = "minimal")]
    /// Insert an element in Merk under a key if it doesn't yet exist; path
    /// should be resolved and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert_if_not_exists<'db, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: &[u8],
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists",
            grove_version.grovedb_versions.element.insert_if_not_exists
        );

        let mut cost = OperationCost::default();
        let exists = cost_return_on_error!(
            &mut cost,
            self.element_at_key_already_exists(merk, key, grove_version)
        );
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(&mut cost, self.insert(merk, key, options, grove_version));
            Ok(true).wrap_with_cost(cost)
        }
    }

    #[cfg(feature = "minimal")]
    /// Adds a "Put" op to batch operations with the element and key if it
    /// doesn't exist yet. Returns CostResult.
    pub fn insert_if_not_exists_into_batch_operations<
        'db,
        S: StorageContext<'db>,
        K: AsRef<[u8]>,
    >(
        &self,
        merk: &mut Merk<S>,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_if_not_exists_into_batch_operations
        );

        let mut cost = OperationCost::default();
        let exists = cost_return_on_error!(
            &mut cost,
            self.element_at_key_already_exists(merk, key.as_ref(), grove_version)
        );
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(
                &mut cost,
                self.insert_into_batch_operations(
                    key,
                    batch_operations,
                    feature_type,
                    grove_version
                )
            );
            Ok(true).wrap_with_cost(cost)
        }
    }

    #[cfg(feature = "minimal")]
    /// Insert an element in Merk under a key if the value is different from
    /// what already exists, returning delta.
    pub fn insert_if_changed_value<'db, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: &[u8],
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<Delta, Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_changed_value",
            grove_version
                .grovedb_versions
                .element
                .insert_if_changed_value
        );

        let mut cost = OperationCost::default();
        let previous_element = cost_return_on_error!(
            &mut cost,
            // TODO: keep it like that for v0, v1 shall do `get_optional`
            Self::get_optional_from_storage(&merk.storage, key, grove_version)
        );
        let delta = Delta {
            new: self,
            old: previous_element,
        };

        if delta.has_changed() {
            cost_return_on_error!(&mut cost, self.insert(merk, key, options, grove_version));
        }

        Ok(delta).wrap_with_cost(cost)
    }

    #[cfg(feature = "minimal")]
    /// Adds a "Put" op to batch operations with the element and key if the
    /// value is different from what already exists; Returns CostResult.
    /// The bool represents if we indeed inserted.
    /// If the value changed we return the old element.
    pub fn insert_if_changed_value_into_batch_operations<
        'db,
        S: StorageContext<'db>,
        K: AsRef<[u8]>,
    >(
        &self,
        merk: &mut Merk<S>,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_changed_value_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_if_changed_value_into_batch_operations
        );

        let mut cost = OperationCost::default();
        let previous_element = cost_return_on_error!(
            &mut cost,
            Self::get_optional_from_storage(&merk.storage, key.as_ref(), grove_version)
        );
        let needs_insert = match &previous_element {
            None => true,
            Some(previous_element) => previous_element != self,
        };
        if !needs_insert {
            Ok((false, None)).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(
                &mut cost,
                self.insert_into_batch_operations(
                    key,
                    batch_operations,
                    feature_type,
                    grove_version
                )
            );
            Ok((true, previous_element)).wrap_with_cost(cost)
        }
    }

    #[cfg(feature = "minimal")]
    /// Insert a reference element in Merk under a key.
    pub fn insert_reference<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        referenced_value: Hash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_reference",
            grove_version.grovedb_versions.element.insert_reference
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let mut cost = OperationCost::default();
        let merk_feature_type = cost_return_on_error!(
            &mut cost,
            self.get_feature_type(merk.tree_type)
                .wrap_with_cost(OperationCost::default())
        );

        let batch_operations = [(
            key,
            Op::PutCombinedReference(serialized, referenced_value, merk_feature_type),
        )];
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch_operations,
            &[],
            options,
            &|key, value| {
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "minimal")]
    /// Insert a reference element in Merk under a key returning a delta.
    pub fn insert_reference_if_changed_value<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        referenced_value: Hash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<Delta, Error> {
        check_grovedb_v0_with_cost!(
            "insert_reference",
            grove_version.grovedb_versions.element.insert_reference
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let mut cost = OperationCost::default();

        let previous_element = cost_return_on_error!(
            &mut cost,
            Self::get_optional(&merk, &key, true, grove_version)
        );
        let delta = Delta {
            new: self,
            old: previous_element,
        };

        if delta.has_changed() {
            let merk_feature_type = cost_return_on_error!(
                &mut cost,
                self.get_feature_type(merk.tree_type)
                    .wrap_with_cost(OperationCost::default())
            );

            let batch_operations = [(
                key,
                Op::PutCombinedReference(serialized, referenced_value, merk_feature_type),
            )];
            let tree_type = merk.tree_type;
            cost_return_on_error!(
                &mut cost,
                merk.apply_with_specialized_costs::<_, Vec<u8>>(
                    &batch_operations,
                    &[],
                    options,
                    &|key, value| {
                        Self::specialized_costs_for_key_value(
                            key,
                            value,
                            tree_type.inner_node_type(),
                            grove_version,
                        )
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                    },
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(e.to_string()))
            );
        }

        Ok(delta).wrap_with_cost(cost)
    }

    #[cfg(feature = "minimal")]
    /// Adds a "Put" op to batch operations with reference and key. Returns
    /// CostResult.
    pub fn insert_reference_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        referenced_value: Hash,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_reference_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_reference_into_batch_operations
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let entry = (
            key,
            Op::PutCombinedReference(serialized, referenced_value, feature_type),
        );
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    #[cfg(feature = "minimal")]
    /// Insert a tree element in Merk under a key.
    pub fn insert_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        subtree_root_hash: Hash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_subtree",
            grove_version.grovedb_versions.element.insert_subtree
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let cost = OperationCost::default();
        let merk_feature_type =
            cost_return_on_error_no_add!(cost, self.get_feature_type(merk.tree_type));

        let tree_cost =
            cost_return_on_error_no_add!(cost, self.get_specialized_cost(grove_version));

        let cost = tree_cost
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
        let batch_operations = [(
            key,
            Op::PutLayeredReference(serialized, cost, subtree_root_hash, merk_feature_type),
        )];
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch_operations,
            &[],
            options,
            &|key, value| {
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "minimal")]
    /// Insert a tree element in Merk under a key, returning delta.
    /// If a meaningful overwrite happens the delta will represent it.
    pub fn insert_subtree_if_changed<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        subtree_root_hash: Hash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<Delta, Error> {
        use grovedb_version::dispatch_version;

        dispatch_version!(
            "insert_subtree_if_changed",
            grove_version
                .grovedb_versions
                .element
                .insert_subtree_if_changed,
            0 => {}
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let mut cost = OperationCost::default();

        let previous_element = cost_return_on_error!(
            &mut cost,
            Self::get_optional(&merk, &key, true, grove_version)
        );

        let delta = Delta {
            new: self,
            old: previous_element,
        };

        if delta.has_changed() {
            let merk_feature_type =
                cost_return_on_error_no_add!(cost, self.get_feature_type(merk.tree_type));

            let tree_cost =
                cost_return_on_error_no_add!(cost, self.get_specialized_cost(grove_version));

            let specialized_cost = tree_cost
                + self.get_flags().as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
            let batch_operations = [(
                key,
                Op::PutLayeredReference(
                    serialized,
                    specialized_cost,
                    subtree_root_hash,
                    merk_feature_type,
                ),
            )];
            let tree_type = merk.tree_type;
            cost_return_on_error!(
                &mut cost,
                merk.apply_with_specialized_costs::<_, Vec<u8>>(
                    &batch_operations,
                    &[],
                    options,
                    &|key, value| {
                        Self::specialized_costs_for_key_value(
                            key,
                            value,
                            tree_type.inner_node_type(),
                            grove_version,
                        )
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                    },
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(e.to_string()))
            );
        }

        Ok(delta).wrap_with_cost(cost)
    }

    #[cfg(feature = "minimal")]
    /// Adds a "Put" op to batch operations for a subtree and key
    pub fn insert_subtree_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        subtree_root_hash: Hash,
        is_replace: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_subtree_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_subtree_into_batch_operations
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let tree_cost = cost_return_on_error_default!(self.get_specialized_cost(grove_version));

        let cost = tree_cost
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });

        // Replacing is more efficient, but should lead to the same costs
        let entry = if is_replace {
            (
                key,
                Op::ReplaceLayeredReference(serialized, cost, subtree_root_hash, feature_type),
            )
        } else {
            (
                key,
                Op::PutLayeredReference(serialized, cost, subtree_root_hash, feature_type),
            )
        };
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }
}

#[cfg(all(feature = "minimal"))]
#[cfg(test)]
mod tests {
    use grovedb_merk::test_utils::{empty_path_merk, empty_path_merk_read_only, TempMerk};
    use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch};

    use super::*;

    #[test]
    fn test_success_insert() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new(grove_version);
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value".to_vec()),
        );
    }

    #[test]
    fn test_insert_if_changed_value_does_not_insert_when_value_does_not_change() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new(grove_version);

        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        merk.commit(grove_version);

        let element = Element::new_item(b"value".to_vec());
        let delta = element
            .insert_if_changed_value(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        merk.commit(grove_version);

        assert!(!delta.has_changed());
        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value".to_vec()),
        );
    }

    #[test]
    fn test_insert_if_changed_value_inserts_when_value_changed() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let batch = StorageBatch::new();
        let tx = storage.start_transaction();

        let mut merk = empty_path_merk(&*storage, &tx, &batch, grove_version);

        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        storage
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .unwrap();

        let batch = StorageBatch::new();
        let mut merk = empty_path_merk(&*storage, &tx, &batch, grove_version);
        let element = Element::new_item(b"value2".to_vec());
        let delta = element
            .insert_if_changed_value(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        assert!(delta.has_changed());
        assert_eq!(delta.old, Some(Element::new_item(b"value".to_vec())),);

        storage
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .unwrap();
        let merk = empty_path_merk_read_only(&*storage, &tx, grove_version);

        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value2".to_vec()),
        );
    }

    #[test]
    fn test_insert_if_changed_value_inserts_when_no_value() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new(grove_version);
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        let element = Element::new_item(b"value2".to_vec());
        let delta = element
            .insert_if_changed_value(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        assert!(delta.has_changed());
        assert_eq!(delta.old, None);

        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value2".to_vec()),
        );
    }
}
