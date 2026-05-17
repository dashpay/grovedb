//! Merk tree hash

#[cfg(any(feature = "minimal", feature = "verify"))]
use grovedb_costs::{CostContext, CostsExt, OperationCost};
// Re-export from grovedb-query
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_query::proofs::{CryptoHash, HASH_LENGTH, NULL_HASH};
#[cfg(any(feature = "minimal", feature = "verify"))]
use integer_encoding::*;

/// 2x length of a `Hash`
#[cfg(feature = "minimal")]
pub const HASH_LENGTH_X2: usize = 64;
/// Length of a `Hash` as u32
#[cfg(feature = "minimal")]
pub const HASH_LENGTH_U32: u32 = 32;
/// 2x length of a `Hash` as u32
#[cfg(feature = "minimal")]
pub const HASH_LENGTH_U32_X2: u32 = 64;
/// Hash block size
#[cfg(feature = "minimal")]
pub const HASH_BLOCK_SIZE: usize = 64;
/// Hash block size as u32
#[cfg(feature = "minimal")]
pub const HASH_BLOCK_SIZE_U32: u32 = 64;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Hashes a value
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();

    let mut val_length_buf = [0u8; 10];
    let val_length_len = value.len().encode_var(&mut val_length_buf);
    hasher.update(&val_length_buf[..val_length_len]);
    hasher.update(value);

    let hashes = 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes as u32,
        ..Default::default()
    })
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Hashes a key/value pair.
///
/// The result is Hash(key_len, key, Hash(value_len, value))
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut cost = OperationCost::default();

    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();

    let mut key_length_buf = [0u8; 10];
    let key_length_len = key.len().encode_var(&mut key_length_buf);
    hasher.update(&key_length_buf[..key_length_len]);
    hasher.update(key);

    let value_hash = value_hash(value);
    hasher.update(value_hash.unwrap_add_cost(&mut cost).as_slice());

    let hashes = 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());

    cost.hash_node_calls += hashes as u32;
    hash.wrap_with_cost(cost)
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Computes the kv hash given a kv digest
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();

    let mut key_length_buf = [0u8; 10];
    let key_length_len = key.len().encode_var(&mut key_length_buf);
    hasher.update(&key_length_buf[..key_length_len]);
    hasher.update(key);

    hasher.update(value_hash.as_slice());

    let hashes = 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes as u32,
        ..Default::default()
    })
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Hashes a node based on the hash of its key/value pair, the hash of its left
/// child (if any), and the hash of its right child (if any).
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);
    hasher.update(left);
    hasher.update(right);

    // hashes will always be 2
    let hashes = 2; // 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes,
        ..Default::default()
    })
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Combines two hash values into one
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);
    hasher.update(hash_two);

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: 1, // as this will fit on exactly 1 block
        ..Default::default()
    })
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Combines three hash values into one.
///
/// Used by `CountIndexedTree` / `ProvableCountIndexedTree` elements: the
/// element's `combined_value_hash` is
/// `Blake3(actual_value_hash ‖ primary_root_hash ‖ secondary_root_hash)`.
/// Order is normative — the inputs MUST be supplied as
/// `(value_hash, primary_root_hash, secondary_root_hash)`.
///
/// Cost: Blake3 compresses input in 64-byte blocks. 96 bytes spans two
/// blocks (one full 64-byte block plus a 32-byte partial block), so the
/// hasher performs two block compressions — one more than `combine_hash`,
/// which fits its 64-byte input in a single block.
pub fn combine_hash_three(
    hash_one: &CryptoHash,
    hash_two: &CryptoHash,
    hash_three: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);
    hasher.update(hash_two);
    hasher.update(hash_three);

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: 2, // 96 bytes spans two 64-byte blocks
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_hash_three_is_deterministic() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        let h1 = combine_hash_three(&a, &b, &c).value().to_owned();
        let h2 = combine_hash_three(&a, &b, &c).value().to_owned();
        assert_eq!(h1, h2);
    }

    #[test]
    fn combine_hash_three_is_order_sensitive() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        let abc = combine_hash_three(&a, &b, &c).value().to_owned();
        let acb = combine_hash_three(&a, &c, &b).value().to_owned();
        let bac = combine_hash_three(&b, &a, &c).value().to_owned();
        assert_ne!(abc, acb);
        assert_ne!(abc, bac);
    }

    #[test]
    fn combine_hash_three_distinct_from_combine_hash_of_combine_hash() {
        // The H1-A composition is a single three-input Blake3 call. It must
        // NOT be equivalent to combine_hash(a, combine_hash(b, c)) — that's
        // the composition that was rejected during design review (would have
        // doubled the hash work). This regression test guards the choice.
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        let three = combine_hash_three(&a, &b, &c).value().to_owned();
        let inner = combine_hash(&b, &c).value().to_owned();
        let nested = combine_hash(&a, &inner).value().to_owned();
        assert_ne!(three, nested);
    }

    #[test]
    fn combine_hash_three_handles_null_hash_slots() {
        // For a CountIndexedTree element with both child Merks empty, the
        // composition is Blake3(value_hash || NULL_HASH || NULL_HASH).
        // Verify it is well-defined and stable across calls.
        let value_hash = [0xAA; 32];
        let h1 = combine_hash_three(&value_hash, &NULL_HASH, &NULL_HASH)
            .value()
            .to_owned();
        let h2 = combine_hash_three(&value_hash, &NULL_HASH, &NULL_HASH)
            .value()
            .to_owned();
        assert_eq!(h1, h2);
        assert_ne!(h1, NULL_HASH);
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Hashes a node for ProvableCountTree, including the aggregate count
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);
    hasher.update(left);
    hasher.update(right);
    hasher.update(&count.to_be_bytes());

    // hashes will always be 2
    let hashes = 2; // 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes,
        ..Default::default()
    })
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Hashes a node for ProvableSumTree, including the aggregate sum.
///
/// Parallel to `node_hash_with_count` but for sum-bearing aggregates.
/// The i64 sum is appended via its big-endian byte representation (8 bytes,
/// fixed-width, deterministic). This is content-binding only — no order
/// preservation is needed since the bytes are part of the hash input.
/// Negative sums hash via their two's-complement big-endian form, which is
/// deterministic regardless of the platform.
pub fn node_hash_with_sum(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    sum: i64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);
    hasher.update(left);
    hasher.update(right);
    hasher.update(&sum.to_be_bytes());

    // hashes will always be 2 (same shape as node_hash_with_count)
    let hashes = 2;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes,
        ..Default::default()
    })
}

#[cfg(test)]
#[cfg(feature = "minimal")]
mod node_hash_with_sum_tests {
    use grovedb_costs::CostsExt;

    use super::{node_hash, node_hash_with_sum, CryptoHash, HASH_LENGTH};

    fn h(byte: u8) -> CryptoHash {
        [byte; HASH_LENGTH]
    }

    #[test]
    fn node_hash_with_sum_differs_from_node_hash_at_zero_sum() {
        // The sum bytes are appended even when zero, so the hash must
        // differ from a plain `node_hash` with the same kv/l/r inputs.
        let kv = h(1);
        let l = h(2);
        let r = h(3);
        let with_sum = node_hash_with_sum(&kv, &l, &r, 0).unwrap();
        let without_sum = node_hash(&kv, &l, &r).unwrap();
        assert_ne!(
            with_sum, without_sum,
            "node_hash_with_sum at sum=0 must NOT equal node_hash"
        );
    }

    #[test]
    fn node_hash_with_sum_different_sums_produce_different_hashes() {
        let kv = h(4);
        let l = h(5);
        let r = h(6);
        let a = node_hash_with_sum(&kv, &l, &r, 0).unwrap();
        let b = node_hash_with_sum(&kv, &l, &r, 1).unwrap();
        let c = node_hash_with_sum(&kv, &l, &r, -1).unwrap();
        let d = node_hash_with_sum(&kv, &l, &r, 42).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(b, c);
        assert_ne!(b, d);
        assert_ne!(c, d);
    }

    #[test]
    fn node_hash_with_sum_extremes_distinct() {
        let kv = h(7);
        let l = h(8);
        let r = h(9);
        let min = node_hash_with_sum(&kv, &l, &r, i64::MIN).unwrap();
        let max = node_hash_with_sum(&kv, &l, &r, i64::MAX).unwrap();
        let zero = node_hash_with_sum(&kv, &l, &r, 0).unwrap();
        assert_ne!(min, max);
        assert_ne!(min, zero);
        assert_ne!(max, zero);
    }

    #[test]
    fn node_hash_with_sum_is_deterministic() {
        let kv = h(0xaa);
        let l = h(0xbb);
        let r = h(0xcc);
        let a = node_hash_with_sum(&kv, &l, &r, -7).unwrap();
        let b = node_hash_with_sum(&kv, &l, &r, -7).unwrap();
        assert_eq!(a, b);
    }
}
