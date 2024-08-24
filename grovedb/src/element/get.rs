//! Get
//! Implements functions in Element for getting

#[cfg(feature = "full")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::tree::kv::KV;
#[cfg(feature = "full")]
use grovedb_merk::Merk;
#[cfg(feature = "full")]
use grovedb_merk::{ed::Decode, tree::TreeNodeInner};
#[cfg(feature = "full")]
use grovedb_storage::StorageContext;
use grovedb_version::{
    check_grovedb_v0_with_cost, error::GroveVersionError, version::GroveVersion,
};
use integer_encoding::VarInt;

use crate::element::{SUM_ITEM_COST_SIZE, SUM_TREE_COST_SIZE, TREE_COST_SIZE};
#[cfg(feature = "full")]
use crate::{Element, Error, Hash};

impl Element {
    #[cfg(feature = "full")]
    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &Merk<S>,
        key: K,
        allow_cache: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        check_grovedb_v0_with_cost!("get", grove_version.grovedb_versions.element.get);
        Self::get_optional(merk, key.as_ref(), allow_cache, grove_version).map(|result| {
            let value = result?;
            value.ok_or_else(|| {
                Error::PathKeyNotFound(format!(
                    "get: key \"{}\" not found in Merk that has a root key [{}] and is of type {}",
                    hex::encode(key),
                    merk.root_key()
                        .map(hex::encode)
                        .unwrap_or("None".to_string()),
                    merk.merk_type
                ))
            })
        })
    }

    #[cfg(feature = "full")]
    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get_optional<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &Merk<S>,
        key: K,
        allow_cache: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error> {
        check_grovedb_v0_with_cost!(
            "get_optional",
            grove_version.grovedb_versions.element.get_optional
        );
        let mut cost = OperationCost::default();

        let value_opt = cost_return_on_error!(
            &mut cost,
            merk.get(
                key.as_ref(),
                allow_cache,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version
            )
            .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let element = cost_return_on_error_no_add!(
            &cost,
            value_opt
                .map(|value| {
                    Self::deserialize(value.as_slice(), grove_version).map_err(|_| {
                        Error::CorruptedData(String::from("unable to deserialize element"))
                    })
                })
                .transpose()
        );

        Ok(element).wrap_with_cost(cost)
    }

    #[cfg(feature = "full")]
    /// Get an element directly from storage under a key
    /// Merk does not need to be loaded
    /// Errors if element doesn't exist
    pub fn get_from_storage<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        storage: &S,
        key: K,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        check_grovedb_v0_with_cost!(
            "get_from_storage",
            grove_version.grovedb_versions.element.get_from_storage
        );
        Self::get_optional_from_storage(storage, key.as_ref(), grove_version).map(|result| {
            let value = result?;
            value.ok_or_else(|| {
                Error::PathKeyNotFound(format!(
                    "key not found in Merk for get from storage: {}",
                    hex::encode(key)
                ))
            })
        })
    }

    #[cfg(feature = "full")]
    /// Get an element directly from storage under a key
    /// Merk does not need to be loaded
    pub fn get_optional_from_storage<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        storage: &S,
        key: K,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error> {
        check_grovedb_v0_with_cost!(
            "get_optional_from_storage",
            grove_version
                .grovedb_versions
                .element
                .get_optional_from_storage
        );
        let mut cost = OperationCost::default();
        let key_ref = key.as_ref();
        let node_value_opt = cost_return_on_error!(
            &mut cost,
            storage
                .get(key_ref)
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let maybe_tree_inner: Option<TreeNodeInner> = cost_return_on_error_no_add!(
            &cost,
            node_value_opt
                .map(|node_value| {
                    Decode::decode(node_value.as_slice())
                        .map_err(|e| Error::CorruptedData(e.to_string()))
                })
                .transpose()
        );

        let value = maybe_tree_inner.map(|tree_inner| tree_inner.value_as_owned());
        let element = cost_return_on_error_no_add!(
            &cost,
            value
                .as_ref()
                .map(|value| {
                    Self::deserialize(value.as_slice(), grove_version).map_err(|_| {
                        Error::CorruptedData(String::from("unable to deserialize element"))
                    })
                })
                .transpose()
        );
        match &element {
            Some(Element::Item(..)) | Some(Element::Reference(..)) => {
                // while the loaded item might be a sum item, it is given for free
                // as it would be very hard to know in advance
                cost.storage_loaded_bytes = KV::value_byte_cost_size_for_key_and_value_lengths(
                    key_ref.len() as u32,
                    value.as_ref().unwrap().len() as u32,
                    false,
                )
            }
            Some(Element::SumItem(_, flags)) => {
                let cost_size = SUM_ITEM_COST_SIZE;
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = cost_size + flags_len;
                cost.storage_loaded_bytes =
                    KV::node_value_byte_cost_size(key_ref.len() as u32, value_len, false)
            }
            Some(Element::Tree(_, flags)) | Some(Element::SumTree(_, _, flags)) => {
                let tree_cost_size = if element.as_ref().unwrap().is_sum_tree() {
                    SUM_TREE_COST_SIZE
                } else {
                    TREE_COST_SIZE
                };
                let flags_len = flags.as_ref().map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = tree_cost_size + flags_len;
                cost.storage_loaded_bytes =
                    KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                        key_ref.len() as u32,
                        value_len,
                        false,
                    )
            }
            None => {}
        }
        Ok(element).wrap_with_cost(cost)
    }

    #[cfg(feature = "full")]
    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get_with_absolute_refs<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &Merk<S>,
        path: &[&[u8]],
        key: K,
        allow_cache: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        check_grovedb_v0_with_cost!(
            "get_with_absolute_refs",
            grove_version
                .grovedb_versions
                .element
                .get_with_absolute_refs
        );
        let mut cost = OperationCost::default();

        let element = cost_return_on_error!(
            &mut cost,
            Self::get(merk, key.as_ref(), allow_cache, grove_version)
        );

        let absolute_element = cost_return_on_error_no_add!(
            &cost,
            element.convert_if_reference_to_absolute_reference(path, Some(key.as_ref()))
        );

        Ok(absolute_element).wrap_with_cost(cost)
    }

    #[cfg(feature = "full")]
    /// Get an element's value hash from Merk under a key
    pub fn get_value_hash<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &Merk<S>,
        key: K,
        allow_cache: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Hash>, Error> {
        check_grovedb_v0_with_cost!(
            "get_value_hash",
            grove_version.grovedb_versions.element.get_value_hash
        );
        let mut cost = OperationCost::default();

        let value_hash = cost_return_on_error!(
            &mut cost,
            merk.get_value_hash(
                key.as_ref(),
                allow_cache,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version
            )
            .map_err(|e| Error::CorruptedData(e.to_string()))
        );

        Ok(value_hash).wrap_with_cost(cost)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use grovedb_path::SubtreePath;
    use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch};

    use super::*;

    #[test]
    fn test_cache_changes_cost() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let batch = StorageBatch::new();
        let ctx = storage
            .get_storage_context(SubtreePath::empty(), Some(&batch))
            .unwrap();
        let mut merk = Merk::open_base(
            ctx,
            false,
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .unwrap()
        .unwrap();
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .unwrap();

        let ctx = storage
            .get_storage_context(SubtreePath::empty(), None)
            .unwrap();
        let mut merk = Merk::open_base(
            ctx,
            false,
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value".to_vec()),
        );

        // Warm up cache because the Merk was reopened.
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        let cost_with_cache = Element::get(&merk, b"another-key", true, grove_version)
            .cost_as_result()
            .expect("expected to get cost");
        let cost_without_cache = Element::get(&merk, b"another-key", false, grove_version)
            .cost_as_result()
            .expect("expected to get cost");
        assert_ne!(cost_with_cache, cost_without_cache);

        assert_eq!(
            cost_with_cache,
            OperationCost {
                seek_count: 0,
                storage_cost: Default::default(),
                storage_loaded_bytes: 0,
                hash_node_calls: 0,
            }
        );

        assert_eq!(
            cost_without_cache,
            OperationCost {
                seek_count: 1,
                storage_cost: Default::default(),
                storage_loaded_bytes: 75,
                hash_node_calls: 0,
            }
        );
    }
}
