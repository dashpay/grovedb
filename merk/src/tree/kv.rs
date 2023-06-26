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

//! Merk tree key-values

#[cfg(feature = "full")]
use std::io::{Read, Write};

#[cfg(feature = "full")]
use ed::{Decode, Encode, Result, Terminated};
#[cfg(feature = "full")]
use grovedb_costs::{CostContext, CostsExt, OperationCost};
#[cfg(feature = "full")]
use integer_encoding::VarInt;

#[cfg(feature = "full")]
use super::hash::{CryptoHash, HASH_LENGTH, NULL_HASH};
use crate::tree::kv::ValueDefinedCostType::{LayeredValueDefinedCost, SpecializedValueDefinedCost};
#[cfg(feature = "full")]
use crate::{
    tree::{
        hash::{combine_hash, kv_digest_to_kv_hash, value_hash, HASH_LENGTH_X2},
        tree_feature_type::{TreeFeatureType, TreeFeatureType::BasicMerk},
    },
    Link, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
};

// TODO: maybe use something similar to Vec but without capacity field,
//       (should save 16 bytes per entry). also, maybe a shorter length
//       field to save even more. also might be possible to combine key
//       field and value field.

/// It is possible to predefine the value cost of specific types
#[cfg(feature = "full")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueDefinedCostType {
    /// There is a predefined cost used to remove the root key from a sub tree
    /// In order to keep node costs associated to the user performing
    /// modifications This should be used for trees
    LayeredValueDefinedCost(u32),
    /// There is a predefined cost used to make the sum item cost constant
    /// This should be used for sum items
    SpecializedValueDefinedCost(u32),
}

#[cfg(feature = "full")]
/// Contains a key/value pair, and the hash of the key/value pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KV {
    pub(super) key: Vec<u8>,
    pub(super) value: Vec<u8>,
    pub(super) feature_type: TreeFeatureType,
    /// The value defined cost is only used on insert
    /// Todo: find another way to do this without this attribute.
    pub(crate) value_defined_cost: Option<ValueDefinedCostType>,
    pub(super) hash: CryptoHash,
    pub(super) value_hash: CryptoHash,
}

#[cfg(feature = "full")]
impl KV {
    /// Creates a new `KV` with the given key and value and computes its hash.
    #[inline]
    pub fn new(
        key: Vec<u8>,
        value: Vec<u8>,
        value_defined_cost: Option<ValueDefinedCostType>,
        feature_type: TreeFeatureType,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        let value_hash = value_hash(value.as_slice()).unwrap_add_cost(&mut cost);
        let kv_hash = kv_digest_to_kv_hash(key.as_slice(), &value_hash).unwrap_add_cost(&mut cost);
        Self {
            key,
            value,
            feature_type,
            value_defined_cost,
            hash: kv_hash,
            value_hash,
        }
        .wrap_with_cost(cost)
    }

    /// Creates a new `KV` with the given key, value and value_hash and computes
    /// its hash.
    #[inline]
    pub fn new_with_value_hash(
        key: Vec<u8>,
        value: Vec<u8>,
        value_hash: CryptoHash,
        feature_type: TreeFeatureType,
    ) -> CostContext<Self> {
        // TODO: length checks?
        kv_digest_to_kv_hash(key.as_slice(), &value_hash).map(|hash| Self {
            key,
            value,
            feature_type,
            value_defined_cost: None,
            hash,
            value_hash,
        })
    }

    /// Creates a new `KV` with a given key, value and supplied_value_hash
    /// Combines the supplied_value_hash + hash(value) as the KV value_hash
    #[inline]
    pub fn new_with_combined_value_hash(
        key: Vec<u8>,
        value: Vec<u8>,
        supplied_value_hash: CryptoHash,
        feature_type: TreeFeatureType,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        let actual_value_hash = value_hash(value.as_slice()).unwrap_add_cost(&mut cost);
        let combined_value_hash =
            combine_hash(&actual_value_hash, &supplied_value_hash).unwrap_add_cost(&mut cost);

        kv_digest_to_kv_hash(key.as_slice(), &combined_value_hash)
            .map(|hash| Self {
                key,
                value,
                feature_type,
                value_defined_cost: None,
                hash,
                value_hash: combined_value_hash,
            })
            .add_cost(cost)
    }

    /// Creates a new `KV` with layered value hash
    pub fn new_with_layered_value_hash(
        key: Vec<u8>,
        value: Vec<u8>,
        value_cost: u32,
        supplied_value_hash: CryptoHash,
        feature_type: TreeFeatureType,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        let actual_value_hash = value_hash(value.as_slice()).unwrap_add_cost(&mut cost);
        let combined_value_hash =
            combine_hash(&actual_value_hash, &supplied_value_hash).unwrap_add_cost(&mut cost);

        kv_digest_to_kv_hash(key.as_slice(), &combined_value_hash)
            .map(|hash| Self {
                key,
                value,
                feature_type,
                value_defined_cost: Some(LayeredValueDefinedCost(value_cost)),
                hash,
                value_hash: combined_value_hash,
            })
            .add_cost(cost)
    }

    /// Creates a new `KV` with the given key, value, and hash. The hash is not
    /// checked to be correct for the given key/value.
    #[inline]
    pub fn from_fields(
        key: Vec<u8>,
        value: Vec<u8>,
        hash: CryptoHash,
        value_hash: CryptoHash,
        feature_type: TreeFeatureType,
    ) -> Self {
        Self {
            key,
            value,
            feature_type,
            value_defined_cost: None,
            hash,
            value_hash,
        }
    }

    /// Replaces the `KV`'s value with the given value, updates the hash,
    /// value hash and returns the modified `KV`.
    #[inline]
    pub fn put_value_then_update(mut self, value: Vec<u8>) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        // TODO: length check?
        self.value = value;
        self.value_hash = value_hash(self.value_as_slice()).unwrap_add_cost(&mut cost);
        self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash()).unwrap_add_cost(&mut cost);
        self.wrap_with_cost(cost)
    }

    /// Replaces the `KV`'s value with the given value, updates the hash,
    /// value hash and returns the modified `KV`.
    /// This is used when we want a fixed cost, for example in sum trees
    #[inline]
    pub fn put_value_with_fixed_cost_then_update(
        mut self,
        value: Vec<u8>,
        value_cost: u32,
    ) -> CostContext<Self> {
        self.value_defined_cost = Some(SpecializedValueDefinedCost(value_cost));
        self.put_value_then_update(value)
    }

    /// Replaces the `KV`'s value with the given value and value hash,
    /// updates the hash and returns the modified `KV`.
    #[inline]
    pub fn put_value_and_reference_value_hash_then_update(
        mut self,
        value: Vec<u8>,
        reference_value_hash: CryptoHash,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        let actual_value_hash = value_hash(value.as_slice()).unwrap_add_cost(&mut cost);
        let combined_value_hash =
            combine_hash(&actual_value_hash, &reference_value_hash).unwrap_add_cost(&mut cost);
        self.value = value;
        self.value_hash = combined_value_hash;
        self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash()).unwrap_add_cost(&mut cost);
        self.wrap_with_cost(cost)
    }

    /// Replaces the `KV`'s value with the given value and value hash,
    /// updates the hash and returns the modified `KV`.
    #[inline]
    pub fn put_value_with_reference_value_hash_and_value_cost_then_update(
        mut self,
        value: Vec<u8>,
        reference_value_hash: CryptoHash,
        value_cost: u32,
    ) -> CostContext<Self> {
        self.value_defined_cost = Some(LayeredValueDefinedCost(value_cost));
        self.put_value_and_reference_value_hash_then_update(value, reference_value_hash)
    }

    /// Returns the key as a slice.
    #[inline]
    pub fn key(&self) -> &[u8] {
        self.key.as_slice()
    }

    /// Returns the key as a slice.
    #[inline]
    pub fn key_as_ref(&self) -> &Vec<u8> {
        &self.key
    }

    /// Returns the value as a slice.
    #[inline]
    pub fn value_as_slice(&self) -> &[u8] {
        self.value.as_slice()
    }

    /// Returns the value hash
    #[inline]
    pub const fn value_hash(&self) -> &CryptoHash {
        &self.value_hash
    }

    /// Returns the hash.
    #[inline]
    pub const fn hash(&self) -> &CryptoHash {
        &self.hash
    }

    /// Consumes the `KV` and returns its key without allocating or cloning.
    #[inline]
    pub fn take_key(self) -> Vec<u8> {
        self.key
    }

    /// Get the key costs for the node, this has the parent to child hooks
    #[inline]
    pub fn node_key_byte_cost_size(not_prefixed_key_len: u32) -> u32 {
        HASH_LENGTH_U32
            + not_prefixed_key_len
            + (not_prefixed_key_len + HASH_LENGTH_U32).required_space() as u32
    }

    /// Get the key costs for the node, this has the parent to child hooks
    #[inline]
    pub fn node_value_byte_cost_size(
        not_prefixed_key_len: u32,
        raw_value_len: u32,
        is_sum_node: bool,
    ) -> u32 {
        // Sum trees are either 1 or 9 bytes. While they might be more or less on disk,
        // costs can not take advantage of the varint aspect of the feature.
        let feature_len = if is_sum_node { 9 } else { 1 };

        let value_size = raw_value_len + HASH_LENGTH_U32_X2 + feature_len;
        // The node will be a child of another node which stores it's key and hash
        // That will be added during propagation
        let parent_to_child_cost = Link::encoded_link_size(not_prefixed_key_len, is_sum_node);

        value_size + value_size.required_space() as u32 + parent_to_child_cost
    }

    /// Get the costs for the node, this has the parent to child hooks
    #[inline]
    pub fn node_byte_cost_size_for_key_and_raw_value_lengths(
        not_prefixed_key_len: u32,
        raw_value_len: u32,
        is_sum_node: bool,
    ) -> u32 {
        let node_value_size =
            Self::node_value_byte_cost_size(not_prefixed_key_len, raw_value_len, is_sum_node);
        let node_key_size = Self::node_key_byte_cost_size(not_prefixed_key_len);
        // Each node stores the key and value, the value hash and node hash
        node_value_size + node_key_size
    }

    /// Get the costs for the node, this has the parent to child hooks
    #[inline]
    pub fn layered_node_byte_cost_size_for_key_and_value_lengths(
        not_prefixed_key_len: u32,
        value_len: u32,
        is_sum_node: bool, // this means the node is contained in a sumtree
    ) -> u32 {
        // Sum trees are either 1 or 9 bytes. While they might be more or less on disk,
        // costs can not take advantage of the varint aspect of the feature.
        let feature_len = if is_sum_node { 9 } else { 1 };

        // Each node stores the key and value, and the node hash
        // the value hash on a layered node is not stored directly in the node
        // The required space is set to 2, even though it could be potentially 1
        let node_value_size = value_len + feature_len + HASH_LENGTH_U32 + 2;
        // Hash length is for the key prefix
        let node_key_size = HASH_LENGTH_U32
            + not_prefixed_key_len
            + (not_prefixed_key_len + HASH_LENGTH_U32).required_space() as u32;

        let node_size = node_value_size + node_key_size;
        // The node will be a child of another node which stores it's key and hash
        // That will be added during propagation
        let parent_to_child_cost = Link::encoded_link_size(not_prefixed_key_len, is_sum_node);
        node_size + parent_to_child_cost
    }

    /// Get the costs for the node, this has the parent to child hooks
    /// Layered compared to specialized pays for one less hash
    #[inline]
    pub fn layered_value_byte_cost_size_for_key_and_value_lengths(
        not_prefixed_key_len: u32,
        value_len: u32,
        is_sum_node: bool,
    ) -> u32 {
        // Sum trees are either 1 or 9 bytes. While they might be more or less on disk,
        // costs can not take advantage of the varint aspect of the feature.
        let feature_len = if is_sum_node { 9 } else { 1 };
        // Each node stores the key and value, and the node hash
        // the value hash on a layered node is not stored directly in the node
        // The required space is set to 2. However in reality it could be 1 or 2.
        // This is because the underlying tree pays for the value cost and it's required
        // length. The value could be a key, and keys can only be 256 bytes.
        // There is no point to pay for the value_hash because it is already being paid
        // by the parent to child reference hook of the root of the underlying
        // tree
        let node_value_size = value_len + feature_len + HASH_LENGTH_U32 + 2;
        // The node will be a child of another node which stores it's key and hash
        // That will be added during propagation
        let parent_to_child_cost = Link::encoded_link_size(not_prefixed_key_len, is_sum_node);
        node_value_size + parent_to_child_cost
    }

    /// Get the costs for the node, this has the parent to child hooks
    #[inline]
    pub fn specialized_value_byte_cost_size_for_key_and_value_lengths(
        not_prefixed_key_len: u32,
        inner_value_len: u32,
        is_sum_node: bool,
    ) -> u32 {
        // Sum trees are either 1 or 9 bytes. While they might be more or less on disk,
        // costs can not take advantage of the varint aspect of the feature.
        let feature_len = if is_sum_node { 9 } else { 1 };
        // Each node stores the key and value, and the node hash and the value hash
        let node_value_size = inner_value_len + feature_len + HASH_LENGTH_U32_X2;
        let node_value_size = node_value_size + node_value_size.required_space() as u32;
        // The node will be a child of another node which stores it's key and hash
        // That will be added during propagation
        let parent_to_child_cost = Link::encoded_link_size(not_prefixed_key_len, is_sum_node);
        node_value_size + parent_to_child_cost
    }

    /// Get the costs for the value with known value_len and non prefixed key
    /// len sizes, this has the parent to child hooks
    #[inline]
    pub fn value_byte_cost_size_for_key_and_value_lengths(
        not_prefixed_key_len: u32,
        value_len: u32,
        is_sum_node: bool,
    ) -> u32 {
        // encoding a reference encodes the key last and doesn't encode the size of the
        // key. so no need for a varint required space calculation for the
        // reference.

        // however we do need the varint required space for the cost of the key in
        // rocks_db
        let parent_to_child_reference_len =
            Link::encoded_link_size(not_prefixed_key_len, is_sum_node);
        value_len + value_len.required_space() as u32 + parent_to_child_reference_len
    }

    /// Get the costs for the value with known raw value_len and non prefixed
    /// key len sizes, this has the parent to child hooks
    #[inline]
    pub(crate) fn value_byte_cost_size_for_key_and_raw_value_lengths(
        not_prefixed_key_len: u32,
        raw_value_len: u32,
        is_sum_node: bool,
    ) -> u32 {
        let sum_tree_len = if is_sum_node { 9 } else { 1 }; // 1 for option, 0 or 9 for sum feature
        let value_len = raw_value_len + HASH_LENGTH_U32_X2 + sum_tree_len;
        Self::value_byte_cost_size_for_key_and_value_lengths(
            not_prefixed_key_len,
            value_len,
            is_sum_node,
        )
    }

    /// Get the costs for the value, this has the parent to child hooks
    #[inline]
    pub(crate) fn value_byte_cost_size(&self) -> u32 {
        let key_len = self.key.len() as u32;
        let value_len = self.encoding_cost() as u32;
        Self::value_byte_cost_size_for_key_and_value_lengths(
            key_len,
            value_len,
            self.feature_type.is_sum_feature(),
        )
    }

    /// This function is used to calculate the cost of groveDB tree nodes
    /// It pays for the parent hook.
    /// Trees have the root key of the underlying tree as values.
    /// This key cost will be already taken by the underlying tree.
    /// If the tree is empty then the value hash is empty too.
    /// The value hash is also paid for by the top element of the underlying
    /// tree. Only the key_value_hash should be paid for by the actual tree
    /// node
    #[inline]
    pub(crate) fn layered_value_byte_cost_size(&self, value_cost: u32) -> u32 {
        let key_len = self.key.len() as u32;
        let is_sum_node = self.feature_type.is_sum_feature();

        Self::layered_value_byte_cost_size_for_key_and_value_lengths(
            key_len,
            value_cost,
            is_sum_node,
        )
    }

    /// This function is used to calculate the cost of groveDB sum item nodes
    /// The difference with layered nodes is that the value hash is payed for by
    /// the node in the specialized nodes and by the parent in the layered
    /// ones
    #[inline]
    pub(crate) fn specialized_value_byte_cost_size(&self, value_cost: u32) -> u32 {
        let key_len = self.key.len() as u32;
        let is_sum_node = self.feature_type.is_sum_feature();

        Self::specialized_value_byte_cost_size_for_key_and_value_lengths(
            key_len,
            value_cost,
            is_sum_node,
        )
    }

    /// Costs based on predefined types (Trees, SumTrees, SumItems) that behave
    /// differently than items or references
    #[inline]
    pub(crate) fn predefined_value_byte_cost_size(
        &self,
        value_defined_cost_type: &ValueDefinedCostType,
    ) -> u32 {
        match value_defined_cost_type {
            SpecializedValueDefinedCost(cost) => self.specialized_value_byte_cost_size(*cost),
            LayeredValueDefinedCost(cost) => self.layered_value_byte_cost_size(*cost),
        }
    }

    #[inline]
    fn encoding_cost(&self) -> usize {
        debug_assert!(self.key().len() < 256, "Key length must be less than 256");
        HASH_LENGTH_X2 + self.value.len() + self.feature_type.encoding_cost()
    }
}

#[cfg(feature = "full")]
// TODO: Fix encoding and decoding of kv
impl Encode for KV {
    #[inline]
    fn encode_into<W: Write>(&self, out: &mut W) -> Result<()> {
        let _ = &self.feature_type.encode_into(out)?;
        out.write_all(&self.hash[..])?;
        out.write_all(&self.value_hash[..])?;
        out.write_all(self.value.as_slice())?;
        Ok(())
    }

    #[inline]
    fn encoding_length(&self) -> Result<usize> {
        debug_assert!(self.key().len() < 256, "Key length must be less than 256");
        Ok(HASH_LENGTH + HASH_LENGTH + self.value.len() + self.feature_type.encoding_length()?)
    }
}

#[cfg(feature = "full")]
impl Decode for KV {
    #[inline]
    fn decode<R: Read>(input: R) -> Result<Self> {
        let mut kv = Self {
            key: Vec::with_capacity(0),
            value: Vec::with_capacity(128),
            feature_type: BasicMerk,
            value_defined_cost: None,
            hash: NULL_HASH,
            value_hash: NULL_HASH,
        };
        Self::decode_into(&mut kv, input)?;
        Ok(kv)
    }

    #[inline]
    fn decode_into<R: Read>(&mut self, mut input: R) -> Result<()> {
        self.key.clear();

        self.feature_type = TreeFeatureType::decode(&mut input)?;
        input.read_exact(&mut self.hash[..])?;
        input.read_exact(&mut self.value_hash[..])?;

        self.value.clear();
        input.read_to_end(self.value.as_mut())?;

        Ok(())
    }
}

#[cfg(feature = "full")]
impl Terminated for KV {}

#[cfg(feature = "full")]
#[cfg(test)]
mod test {
    use super::*;
    use crate::tree::tree_feature_type::TreeFeatureType::SummedMerk;

    #[test]
    fn new_kv() {
        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6], None, BasicMerk).unwrap();

        assert_eq!(kv.key(), &[1, 2, 3]);
        assert_eq!(kv.value_as_slice(), &[4, 5, 6]);
        assert_ne!(kv.hash(), &super::super::hash::NULL_HASH);
    }

    #[test]
    fn with_value() {
        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6], None, BasicMerk)
            .unwrap()
            .put_value_then_update(vec![7, 8, 9])
            .unwrap();

        assert_eq!(kv.key(), &[1, 2, 3]);
        assert_eq!(kv.value_as_slice(), &[7, 8, 9]);
        assert_ne!(kv.hash(), &super::super::hash::NULL_HASH);
    }

    #[test]
    fn encode_and_decode_kv() {
        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6], None, BasicMerk).unwrap();
        let mut encoded_kv = vec![];
        kv.encode_into(&mut encoded_kv).expect("encoded");
        let mut decoded_kv = KV::decode(encoded_kv.as_slice()).unwrap();
        decoded_kv.key = vec![1, 2, 3];

        assert_eq!(kv, decoded_kv);

        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6], None, SummedMerk(20)).unwrap();
        let mut encoded_kv = vec![];
        kv.encode_into(&mut encoded_kv).expect("encoded");
        let mut decoded_kv = KV::decode(encoded_kv.as_slice()).unwrap();
        decoded_kv.key = vec![1, 2, 3];

        assert_eq!(kv, decoded_kv);
    }
}
