use integer_encoding::*;

/// The length of a `Hash` (in bytes).
pub const HASH_LENGTH: usize = 32;

/// A zero-filled `Hash`.
pub const NULL_HASH: Hash = [0; HASH_LENGTH];

/// A cryptographic hash digest.
pub type Hash = [u8; HASH_LENGTH];

/// Hashes a value
pub fn value_hash(value: &[u8]) -> Hash {
    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();

    let val_length = value.len().encode_var_vec();
    hasher.update(val_length.as_slice());
    hasher.update(value);

    let res = hasher.finalize();
    let mut hash: Hash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash
}

/// Hashes a key/value pair.
///
/// The result is Hash(key_len, key, Hash(value_len, value))
pub fn kv_hash(key: &[u8], value: &[u8]) -> Hash {
    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();

    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);

    let value_hash = value_hash(value);
    hasher.update(value_hash.as_slice());

    let res = hasher.finalize();
    let mut hash: Hash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash
}

/// Computes the kv hash given a kv digest
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &Hash) -> Hash {
    let mut hasher = blake3::Hasher::new();

    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);

    hasher.update(value_hash.as_slice());

    let res = hasher.finalize();
    let mut hash: Hash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash
}

/// Hashes a node based on the hash of its key/value pair, the hash of its left
/// child (if any), and the hash of its right child (if any).
pub fn node_hash(kv: &Hash, left: &Hash, right: &Hash) -> Hash {
    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);
    hasher.update(left);
    hasher.update(right);

    let res = hasher.finalize();
    let mut hash: Hash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash
}
