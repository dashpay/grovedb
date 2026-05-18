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

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Hashes a node for `ProvableCountProvableSumTree`, baking BOTH the
/// aggregate count AND the aggregate sum into the node hash.
///
/// Combined analogue of [`node_hash_with_count`] and [`node_hash_with_sum`].
/// The u64 count is appended in big-endian (8 fixed bytes), followed by the
/// i64 sum in big-endian (another 8 fixed bytes). Fixed-width encoding makes
/// the hash deterministic regardless of how large the count/sum values are —
/// varint encoding would expose the prover's choice of size and open a
/// malleability surface. Negative sums hash via their two's-complement
/// big-endian form (deterministic across platforms).
///
/// Hash layout: `Blake3(kv || left || right || count_be8 || sum_be8)`.
///
/// This is the hash function that diverges a `ProvableCountProvableSumTree`
/// root from an equivalently-populated `ProvableCountSumTree` (which hashes
/// only the count) and from a `ProvableSumTree` (which hashes only the sum).
pub fn node_hash_with_count_and_sum(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
    sum: i64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);
    hasher.update(left);
    hasher.update(right);
    hasher.update(&count.to_be_bytes());
    hasher.update(&sum.to_be_bytes());

    // The input is kv (32) + left (32) + right (32) + count (8) + sum (8) =
    // 112 bytes, still fits in 2 Blake3 blocks like the count/sum-only paths.
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
mod tests {
    use grovedb_costs::CostsExt;

    use super::{
        node_hash, node_hash_with_count, node_hash_with_count_and_sum, node_hash_with_sum,
        CryptoHash, HASH_LENGTH,
    };

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

    // node_hash_with_count_and_sum tests — mirror the sum tests but cover
    // the dual-axis hash function. Each test asserts the function commits
    // both aggregates into the hash so verification can detect tampering
    // on either axis.

    #[test]
    fn node_hash_with_count_and_sum_is_deterministic() {
        let kv = h(0xaa);
        let l = h(0xbb);
        let r = h(0xcc);
        let a = node_hash_with_count_and_sum(&kv, &l, &r, 7, -3).unwrap();
        let b = node_hash_with_count_and_sum(&kv, &l, &r, 7, -3).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn node_hash_with_count_and_sum_differs_from_plain_and_singletons() {
        // The dual-axis hash MUST be distinct from every other hash flavor
        // (plain, count-only, sum-only) for the same kv/l/r inputs — that
        // distinctness is what makes the ProvableCountProvableSumTree root
        // hash diverge from the ProvableCountTree, ProvableSumTree, and
        // plain-tree roots over the same contents.
        let kv = h(1);
        let l = h(2);
        let r = h(3);
        let dual = node_hash_with_count_and_sum(&kv, &l, &r, 0, 0).unwrap();
        let plain = node_hash(&kv, &l, &r).unwrap();
        let count_only = node_hash_with_count(&kv, &l, &r, 0).unwrap();
        let sum_only = node_hash_with_sum(&kv, &l, &r, 0).unwrap();
        assert_ne!(dual, plain);
        assert_ne!(dual, count_only);
        assert_ne!(dual, sum_only);
    }

    #[test]
    fn node_hash_with_count_and_sum_sensitive_to_each_input() {
        let kv = h(4);
        let l = h(5);
        let r = h(6);
        let baseline = node_hash_with_count_and_sum(&kv, &l, &r, 10, 20).unwrap();
        // Changing kv changes the hash.
        let mut_kv = node_hash_with_count_and_sum(&h(40), &l, &r, 10, 20).unwrap();
        assert_ne!(mut_kv, baseline);
        // Changing left changes the hash.
        let mut_l = node_hash_with_count_and_sum(&kv, &h(50), &r, 10, 20).unwrap();
        assert_ne!(mut_l, baseline);
        // Changing right changes the hash.
        let mut_r = node_hash_with_count_and_sum(&kv, &l, &h(60), 10, 20).unwrap();
        assert_ne!(mut_r, baseline);
        // Changing count changes the hash (with sum unchanged).
        let mut_c = node_hash_with_count_and_sum(&kv, &l, &r, 11, 20).unwrap();
        assert_ne!(mut_c, baseline);
        // Changing sum changes the hash (with count unchanged).
        let mut_s = node_hash_with_count_and_sum(&kv, &l, &r, 10, 21).unwrap();
        assert_ne!(mut_s, baseline);
    }

    #[test]
    fn node_hash_with_count_and_sum_distinguishes_axis_swap() {
        // (count=A, sum=B) and (count=B, sum=A) hash to different values —
        // the encoding orders count before sum, so the byte layout
        // differentiates the two arrangements even when A and B fit both
        // axes (e.g. small positive integers).
        let kv = h(7);
        let l = h(8);
        let r = h(9);
        let ab = node_hash_with_count_and_sum(&kv, &l, &r, 3, 5).unwrap();
        let ba = node_hash_with_count_and_sum(&kv, &l, &r, 5, 3).unwrap();
        assert_ne!(ab, ba);
    }

    #[test]
    fn node_hash_with_count_and_sum_extremes_distinct() {
        let kv = h(0xfe);
        let l = h(0xfd);
        let r = h(0xfc);
        let max_max = node_hash_with_count_and_sum(&kv, &l, &r, u64::MAX, i64::MAX).unwrap();
        let max_min = node_hash_with_count_and_sum(&kv, &l, &r, u64::MAX, i64::MIN).unwrap();
        let zero_zero = node_hash_with_count_and_sum(&kv, &l, &r, 0, 0).unwrap();
        let zero_neg_one = node_hash_with_count_and_sum(&kv, &l, &r, 0, -1).unwrap();
        assert_ne!(max_max, max_min);
        assert_ne!(max_max, zero_zero);
        assert_ne!(zero_zero, zero_neg_one);
        // Negative sums hash deterministically via two's-complement big-endian.
        let neg_one_a = node_hash_with_count_and_sum(&kv, &l, &r, 42, -1).unwrap();
        let neg_one_b = node_hash_with_count_and_sum(&kv, &l, &r, 42, -1).unwrap();
        assert_eq!(neg_one_a, neg_one_b);
    }
}
