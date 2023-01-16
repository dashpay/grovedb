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

//! Insert
//! Implements functions in Element for inserting into Merk

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use integer_encoding::VarInt;
#[cfg(feature = "full")]
use merk::{BatchEntry, Error as MerkError, Merk, MerkOptions, Op, TreeFeatureType};
#[cfg(feature = "full")]
use storage::StorageContext;

#[cfg(feature = "full")]
use crate::{element::TREE_COST_SIZE, Element, Error, Hash};

impl Element {
    #[cfg(feature = "full")]
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
    ) -> CostResult<(), Error> {
        let cost = OperationCost::default();

        let serialized = cost_return_on_error_no_add!(&cost, self.serialize());

        if !merk.is_sum_tree && self.is_sum_item() {
            return Err(Error::InvalidInput("cannot add sum item to non sum tree"))
                .wrap_with_cost(Default::default());
        }

        let merk_feature_type =
            cost_return_on_error_no_add!(&cost, self.get_feature_type(merk.is_sum_tree));

        let batch_operations = [(key, Op::Put(serialized, merk_feature_type))];
        let uses_sum_nodes = merk.is_sum_tree;
        merk.apply_with_tree_costs::<_, Vec<u8>>(&batch_operations, &[], options, &|key, value| {
            Self::tree_costs_for_key_value(key, value, uses_sum_nodes)
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
        })
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "full")]
    /// Add to batch operations a "Put" op with key and serialized element.
    /// Return CostResult.
    pub fn insert_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let entry = (key, Op::Put(serialized, feature_type));
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    #[cfg(feature = "full")]
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
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();
        let exists =
            cost_return_on_error!(&mut cost, self.element_at_key_already_exists(merk, key));
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(&mut cost, self.insert(merk, key, options));
            Ok(true).wrap_with_cost(cost)
        }
    }

    #[cfg(feature = "full")]
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
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();
        let exists = cost_return_on_error!(
            &mut cost,
            self.element_at_key_already_exists(merk, key.as_ref())
        );
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(
                &mut cost,
                self.insert_into_batch_operations(key, batch_operations, feature_type)
            );
            Ok(true).wrap_with_cost(cost)
        }
    }

    #[cfg(feature = "full")]
    /// Insert a reference element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert_reference<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        referenced_value: Hash,
        options: Option<MerkOptions>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let mut cost = OperationCost::default();
        let merk_feature_type = cost_return_on_error!(
            &mut cost,
            self.get_feature_type(merk.is_sum_tree)
                .wrap_with_cost(OperationCost::default())
        );

        let batch_operations = [(
            key,
            Op::PutCombinedReference(serialized, referenced_value, merk_feature_type),
        )];
        let uses_sum_nodes = merk.is_sum_tree;
        merk.apply_with_tree_costs::<_, Vec<u8>>(&batch_operations, &[], options, &|key, value| {
            Self::tree_costs_for_key_value(key, value, uses_sum_nodes)
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
        })
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "full")]
    /// Adds a "Put" op to batch operations with reference and key. Returns
    /// CostResult.
    pub fn insert_reference_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        referenced_value: Hash,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
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

    #[cfg(feature = "full")]
    /// Insert a tree element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        subtree_root_hash: Hash,
        options: Option<MerkOptions>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let cost = OperationCost::default();
        let merk_feature_type =
            cost_return_on_error_no_add!(&cost, self.get_feature_type(merk.is_sum_tree));

        let tree_cost = cost_return_on_error_no_add!(&cost, self.get_tree_cost());

        let cost = tree_cost
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
        let batch_operations = [(
            key,
            Op::PutLayeredReference(serialized, cost, subtree_root_hash, merk_feature_type),
        )];
        let uses_sum_nodes = merk.is_sum_tree;
        merk.apply_with_tree_costs::<_, Vec<u8>>(&batch_operations, &[], options, &|key, value| {
            Self::tree_costs_for_key_value(key, value, uses_sum_nodes)
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
        })
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "full")]
    /// Adds a "Put" op to batch operations for a subtree and key
    pub fn insert_subtree_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        subtree_root_hash: Hash,
        is_replace: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };
        let cost = TREE_COST_SIZE
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

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use merk::test_utils::TempMerk;

    use super::*;

    #[test]
    fn test_success_insert() {
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
    }
}
