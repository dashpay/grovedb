#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use merk::Merk;
#[cfg(feature = "full")]
use merk::{ed::Decode, tree::TreeInner};
#[cfg(feature = "full")]
use storage::StorageContext;

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
        let mut cost = OperationCost::default();

        let value_opt = cost_return_on_error!(
            &mut cost,
            merk.get(key.as_ref(), allow_cache)
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let value = cost_return_on_error_no_add!(
            &cost,
            value_opt.ok_or_else(|| {
                Error::PathKeyNotFound(format!(
                    "key not found in Merk for get: {}",
                    hex::encode(key)
                ))
            })
        );
        let element = cost_return_on_error_no_add!(
            &cost,
            Self::deserialize(value.as_slice())
                .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))
        );
        Ok(element).wrap_with_cost(cost)
    }

    #[cfg(feature = "full")]
    /// Get an element directly from storage under a key
    /// Merk does not need to be loaded
    pub fn get_from_storage<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        storage: &S,
        key: K,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();
        let node_value_opt = cost_return_on_error!(
            &mut cost,
            storage
                .get(key.as_ref())
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let node_value = cost_return_on_error_no_add!(
            &cost,
            node_value_opt.ok_or_else(|| {
                Error::PathKeyNotFound(format!(
                    "key not found in Merk for get from storage: {}",
                    hex::encode(key)
                ))
            })
        );
        let tree_inner: TreeInner = cost_return_on_error_no_add!(
            &cost,
            Decode::decode(node_value.as_slice()).map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let value = tree_inner.value_as_owned();
        let element = cost_return_on_error_no_add!(
            &cost,
            Self::deserialize(value.as_slice())
                .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))
        );
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
