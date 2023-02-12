// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Get
//! Implements functions in Element for getting

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use integer_encoding::VarInt;
use merk::tree::kv::KV;
#[cfg(feature = "full")]
use merk::Merk;
#[cfg(feature = "full")]
use merk::{ed::Decode, tree::TreeInner};
#[cfg(feature = "full")]
use storage::StorageContext;

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
    ) -> CostResult<Element, Error> {
        Self::get_optional(merk, key.as_ref(), allow_cache).map(|result| {
            let value = result?;
            value.ok_or_else(|| {
                Error::PathKeyNotFound(format!(
                    "key not found in Merk for get: {}",
                    hex::encode(key)
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
    ) -> CostResult<Option<Element>, Error> {
        let mut cost = OperationCost::default();

        let value_opt = cost_return_on_error!(
            &mut cost,
            merk.get(key.as_ref(), allow_cache)
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let element = cost_return_on_error_no_add!(
            &cost,
            value_opt
                .map(|value| {
                    Self::deserialize(value.as_slice()).map_err(|_| {
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
    ) -> CostResult<Element, Error> {
        Self::get_optional_from_storage(storage, key.as_ref()).map(|result| {
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
    ) -> CostResult<Option<Element>, Error> {
        let mut cost = OperationCost::default();
        let key_ref = key.as_ref();
        let node_value_opt = cost_return_on_error!(
            &mut cost,
            storage
                .get(key_ref)
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let maybe_tree_inner: Option<TreeInner> = cost_return_on_error_no_add!(
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
                    Self::deserialize(value.as_slice()).map_err(|_| {
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
                    KV::specialized_value_byte_cost_size_for_key_and_value_lengths(
                        key_ref.len() as u32,
                        value_len,
                        false,
                    )
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
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();

        let element = cost_return_on_error!(&mut cost, Self::get(merk, key.as_ref(), allow_cache));

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
    ) -> CostResult<Option<Hash>, Error> {
        let mut cost = OperationCost::default();

        let value_hash = cost_return_on_error!(
            &mut cost,
            merk.get_value_hash(key.as_ref(), allow_cache)
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );

        Ok(value_hash).wrap_with_cost(cost)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use merk::test_utils::TempMerk;

    use super::*;

    #[test]
    fn test_cache_changes_cost() {
        let mut merk = TempMerk::new();
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None)
            .unwrap()
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, b"another-key", true)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value".to_vec()),
        );

        let cost_with_cache = Element::get(&merk, b"another-key", true)
            .cost_as_result()
            .expect("expected to get cost");
        let cost_without_cache = Element::get(&merk, b"another-key", false)
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
