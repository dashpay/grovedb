use std::io::{Read, Write};

use costs::{CostContext, CostsExt, OperationCost};
use ed::{Decode, Encode, Result, Terminated};
use integer_encoding::VarInt;

use super::hash::{CryptoHash, HASH_LENGTH, NULL_HASH};
use crate::tree::{hash::value_hash, kv_digest_to_kv_hash};

// TODO: maybe use something similar to Vec but without capacity field,
//       (should save 16 bytes per entry). also, maybe a shorter length
//       field to save even more. also might be possible to combine key
//       field and value field.

/// Contains a key/value pair, and the hash of the key/value pair.
#[derive(Clone, Debug)]
pub struct KV {
    pub(super) key: Vec<u8>,
    pub(super) value: Vec<u8>,
    pub(super) hash: CryptoHash,
    pub(super) value_hash: CryptoHash,
}

impl KV {
    /// Creates a new `KV` with the given key and value and computes its hash.
    #[inline]
    pub fn new(key: Vec<u8>, value: Vec<u8>) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        let value_hash = value_hash(value.as_slice()).unwrap_add_cost(&mut cost);
        let kv_hash = kv_digest_to_kv_hash(key.as_slice(), &value_hash).unwrap_add_cost(&mut cost);
        Self {
            key,
            value,
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
    ) -> CostContext<Self> {
        // TODO: length checks?
        kv_digest_to_kv_hash(key.as_slice(), &value_hash).map(|hash| Self {
            key,
            value,
            hash,
            value_hash,
        })
    }

    /// Creates a new `KV` with the given key, value, and hash. The hash is not
    /// checked to be correct for the given key/value.
    #[inline]
    pub fn from_fields(
        key: Vec<u8>,
        value: Vec<u8>,
        hash: CryptoHash,
        value_hash: CryptoHash,
    ) -> Self {
        Self {
            key,
            value,
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

    /// Replaces the `KV`'s value with the given value and value hash,
    /// updates the hash and returns the modified `KV`.
    #[inline]
    pub fn put_value_and_value_hash_then_update(
        mut self,
        value: Vec<u8>,
        value_hash: CryptoHash,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        self.value = value;
        self.value_hash = value_hash;
        self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash()).unwrap_add_cost(&mut cost);
        self.wrap_with_cost(cost)
    }

    /// Returns the key as a slice.
    #[inline]
    pub fn key(&self) -> &[u8] {
        self.key.as_slice()
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

    #[inline]
    pub(crate) fn value_encoding_length_with_parent_to_child_reference(&self) -> usize {
        // encoding a reference encodes the key last and doesn't encode the size of the
        // key. so no need for a varint required space calculation for the
        // reference.

        // however we do need the varint required space for the cost of the key in
        // rocks_db
        let key_len = self.key.len();
        let value_len = self.encoding_length().unwrap();
        // 3 = 2 + 1
        // 2 for child lengths
        // 1 for key_len size in encoding
        let parent_to_child_reference_len = key_len + HASH_LENGTH + 3;
        value_len + value_len.required_space() + parent_to_child_reference_len
    }
}

impl Encode for KV {
    #[inline]
    fn encode_into<W: Write>(&self, out: &mut W) -> Result<()> {
        out.write_all(&self.hash[..])?;
        out.write_all(&self.value_hash[..])?;
        out.write_all(self.value.as_slice())?;
        Ok(())
    }

    #[inline]
    fn encoding_length(&self) -> Result<usize> {
        debug_assert!(self.key().len() < 256, "Key length must be less than 256");
        Ok(HASH_LENGTH + HASH_LENGTH + self.value.len())
    }
}

impl Decode for KV {
    #[inline]
    fn decode<R: Read>(input: R) -> Result<Self> {
        let mut kv = Self {
            key: Vec::with_capacity(0),
            value: Vec::with_capacity(128),
            hash: NULL_HASH,
            value_hash: NULL_HASH,
        };
        Self::decode_into(&mut kv, input)?;
        Ok(kv)
    }

    #[inline]
    fn decode_into<R: Read>(&mut self, mut input: R) -> Result<()> {
        self.key.clear();

        input.read_exact(&mut self.hash[..])?;
        input.read_exact(&mut self.value_hash[..])?;

        self.value.clear();
        input.read_to_end(self.value.as_mut())?;

        Ok(())
    }
}

impl Terminated for KV {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn new_kv() {
        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6]).unwrap();

        assert_eq!(kv.key(), &[1, 2, 3]);
        assert_eq!(kv.value_as_slice(), &[4, 5, 6]);
        assert_ne!(kv.hash(), &super::super::hash::NULL_HASH);
    }

    #[test]
    fn with_value() {
        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6])
            .unwrap()
            .put_value_then_update(vec![7, 8, 9])
            .unwrap();

        assert_eq!(kv.key(), &[1, 2, 3]);
        assert_eq!(kv.value_as_slice(), &[7, 8, 9]);
        assert_ne!(kv.hash(), &super::super::hash::NULL_HASH);
    }
}
