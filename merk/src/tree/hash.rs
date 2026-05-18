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

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Compute the canonical digest of an indexed-tree's `axes` TLV.
///
/// Used by `Element::ProvableCountProvableSumIndexedTree`: the element's
/// `combined_value_hash` is
/// `combine_hash_three(value_hash, primary_root_hash, axes_digest)`, where
/// `axes_digest` binds the list of `(axis_tag, secondary_root_hash)` pairs
/// into the element hash.
///
/// Encoding: `axis_count_u8 ‖ (axis_tag_u8 ‖ secondary_root_hash_32) for
/// each axis in canonical order`. Empty secondaries (no entries yet) pass
/// `NULL_HASH` in their hash slot. The leading 1-byte length prefix makes
/// a 1-axis digest distinguishable from a 2-axis digest truncated to one
/// entry.
///
/// **The caller is responsible for supplying `axes` in canonical order**
/// (sorted by tag, no duplicates, 1..=3 entries). This function does NOT
/// re-sort or validate — by the time we hash, the constructor / decoder
/// has already enforced the invariant.
///
/// Cost: payload is `1 + 33 * N` bytes; Blake3 compresses in 64-byte
/// blocks, so `hash_node_calls = ceil((1 + 33 * N) / 64)`. For N=1 that's
/// 1 block; for N=2 it's 2 blocks; for N=3 it's still 2 blocks (100
/// bytes).
pub fn axes_digest(axes: &[(u8, CryptoHash)]) -> CostContext<CryptoHash> {
    let n = axes.len();
    // Length in bytes of the digest payload. The cast is safe in practice
    // (the constructor caps N at 3) but we use u8 arithmetic to keep the
    // wire format explicit.
    let payload_bytes = 1usize + 33 * n;
    // Each 64-byte block triggers one Blake3 compression. We round up.
    let hashes = ((payload_bytes + 63) / 64) as u32;

    let mut hasher = blake3::Hasher::new();
    // Length prefix: distinguish a 1-axis digest from a 2-axis digest
    // truncated to a single entry. Cast is safe — N is bounded by 3
    // upstream; we still mask to avoid implicit promotion.
    hasher.update(&[(n as u8)]);
    for (tag, hash) in axes {
        hasher.update(&[*tag]);
        hasher.update(hash);
    }

    let res = hasher.finalize();
    let mut out: CryptoHash = Default::default();
    out.copy_from_slice(res.as_bytes());
    out.wrap_with_cost(OperationCost {
        hash_node_calls: hashes,
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
        // For an indexed-tree element with both child Merks empty, the
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

    // -- axes_digest --

    #[test]
    fn axes_digest_is_deterministic() {
        let axes = [(0u8, [1u8; 32]), (1, [2u8; 32]), (2, [3u8; 32])];
        let a = axes_digest(&axes).value().to_owned();
        let b = axes_digest(&axes).value().to_owned();
        assert_eq!(a, b);
    }

    /// A 1-axis digest must differ from a 2-axis digest with the same
    /// leading entry: the length prefix byte separates them.
    #[test]
    fn axes_digest_length_prefix_matters() {
        let one = [(0u8, [1u8; 32])];
        let two = [(0u8, [1u8; 32]), (1, [2u8; 32])];
        let a = axes_digest(&one).value().to_owned();
        let b = axes_digest(&two).value().to_owned();
        assert_ne!(a, b);
    }

    /// Tag order matters even though both inputs contain the same set of
    /// (tag, hash) pairs — the canonical-order contract is encoded in the
    /// hash because we hash the sequence as-given.
    #[test]
    fn axes_digest_tag_order_matters() {
        let count_then_sum = [(0u8, [1u8; 32]), (1, [2u8; 32])];
        let sum_then_count = [(1u8, [2u8; 32]), (0, [1u8; 32])];
        let a = axes_digest(&count_then_sum).value().to_owned();
        let b = axes_digest(&sum_then_count).value().to_owned();
        assert_ne!(a, b);
    }

    /// Three axes with all-`NULL_HASH` secondary hashes (the canonical
    /// "empty PCPSIT" shape) is well-defined and distinct from any
    /// 1- or 2-axis form.
    #[test]
    fn axes_digest_three_null_axes() {
        let axes = [(0u8, NULL_HASH), (1, NULL_HASH), (2, NULL_HASH)];
        let h = axes_digest(&axes).value().to_owned();
        assert_ne!(h, NULL_HASH);

        // Distinct from the 1-axis NULL form.
        let one = [(0u8, NULL_HASH)];
        assert_ne!(axes_digest(&one).value().to_owned(), h);
    }

    /// Pin the per-N cost model. Payload bytes = 1 + 33N; blocks =
    /// ceil(payload / 64).
    #[test]
    fn axes_digest_hash_call_counts() {
        let one = [(0u8, [0u8; 32])];
        let cost1 = axes_digest(&one).cost();
        // 1 + 33 = 34 bytes → 1 block.
        assert_eq!(cost1.hash_node_calls, 1);

        let two = [(0u8, [0u8; 32]), (1, [0u8; 32])];
        let cost2 = axes_digest(&two).cost();
        // 1 + 66 = 67 bytes → 2 blocks (one full 64, one partial 3).
        assert_eq!(cost2.hash_node_calls, 2);

        let three = [(0u8, [0u8; 32]), (1, [0u8; 32]), (2, [0u8; 32])];
        let cost3 = axes_digest(&three).cost();
        // 1 + 99 = 100 bytes → 2 blocks (one full 64, one partial 36).
        assert_eq!(cost3.hash_node_calls, 2);
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
mod node_hash_with_sum_tests {
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
        let swapped = node_hash_with_count_and_sum(&kv, &l, &r, 5, 3).unwrap();
        assert_ne!(ab, swapped);
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
