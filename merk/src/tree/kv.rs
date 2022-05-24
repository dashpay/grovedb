use std::io::{Read, Write};

use ed::{Decode, Encode, Result, Terminated};

use super::hash::{kv_hash, Hash, HASH_LENGTH, NULL_HASH};
use crate::tree::{hash::value_hash, kv_digest_to_kv_hash};

// TODO: maybe use something similar to Vec but without capacity field,
//       (should save 16 bytes per entry). also, maybe a shorter length
//       field to save even more. also might be possible to combine key
//       field and value field.

/// Contains a key/value pair, and the hash of the key/value pair.
#[derive(Clone)]
pub struct KV {
    pub(super) key: Vec<u8>,
    pub(super) value: Vec<u8>,
    pub(super) hash: Hash,
    pub(super) value_hash: Hash,
}

impl KV {
    /// Creates a new `KV` with the given key and value and computes its hash.
    #[inline]
    pub fn new(key: Vec<u8>, value: Vec<u8>) -> Self {
        // TODO: length checks?
        let hash = kv_hash(key.as_slice(), value.as_slice());
        let value_hash = value_hash(value.as_slice());
        Self {
            key,
            value,
            hash,
            value_hash,
        }
    }

    /// Creates a new `KV` with the given key, value and value_hash and computes
    /// its hash.
    #[inline]
    pub fn new_with_value_hash(key: Vec<u8>, value: Vec<u8>, value_hash: Hash) -> Self {
        // TODO: length checks?
        let hash = kv_digest_to_kv_hash(key.as_slice(), &value_hash);
        Self {
            key,
            value,
            hash,
            value_hash,
        }
    }

    /// Creates a new `KV` with the given key, value, and hash. The hash is not
    /// checked to be correct for the given key/value.
    #[inline]
    pub fn from_fields(key: Vec<u8>, value: Vec<u8>, hash: Hash, value_hash: Hash) -> Self {
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
    pub fn with_value(mut self, value: Vec<u8>) -> Self {
        // TODO: length check?
        self.value = value;
        self.value_hash = value_hash(self.value());
        self.hash = kv_hash(self.key(), self.value());
        self
    }

    /// Replaces the `KV`'s value with the given value and value hash,
    /// updates the hash and returns the modified `KV`.
    #[inline]
    pub fn with_value_and_value_hash(mut self, value: Vec<u8>, value_hash: Hash) -> Self {
        self.value = value;
        self.value_hash = value_hash;
        self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
        self
    }

    /// Returns the key as a slice.
    #[inline]
    pub fn key(&self) -> &[u8] {
        self.key.as_slice()
    }

    /// Returns the value as a slice.
    #[inline]
    pub fn value(&self) -> &[u8] {
        self.value.as_slice()
    }

    /// Returns the value hash
    #[inline]
    pub const fn value_hash(&self) -> &Hash {
        &self.value_hash
    }

    /// Returns the hash.
    #[inline]
    pub const fn hash(&self) -> &Hash {
        &self.hash
    }

    /// Consumes the `KV` and returns its key without allocating or cloning.
    #[inline]
    pub fn take_key(self) -> Vec<u8> {
        self.key
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
        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6]);

        assert_eq!(kv.key(), &[1, 2, 3]);
        assert_eq!(kv.value(), &[4, 5, 6]);
        assert_ne!(kv.hash(), &super::super::hash::NULL_HASH);
    }

    #[test]
    fn with_value() {
        let kv = KV::new(vec![1, 2, 3], vec![4, 5, 6]).with_value(vec![7, 8, 9]);

        assert_eq!(kv.key(), &[1, 2, 3]);
        assert_eq!(kv.value(), &[7, 8, 9]);
        assert_ne!(kv.hash(), &super::super::hash::NULL_HASH);
    }
}
