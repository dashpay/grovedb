//! Sort-key encoders for indexed-tree secondaries.
//!
//! Each secondary Merk in an indexed-tree's TLV is keyed by
//! `(axis_sort_key ‖ original_key)`. The `axis_sort_key` family below produces
//! fixed-width big-endian byte slices whose **lexicographic** order matches
//! the **numeric** order of the underlying axis value (count / sum /
//! computed average). That means a plain Merk range query on the secondary
//! is equivalent to an ordered range query on the axis value — no extra
//! decoding required on the read path.
//!
//! Sign-bit toggling makes the signed encoders order-preserving: flipping
//! bit 63 of an `i64` (or bit 127 of an `i128`) maps the two's-complement
//! signed range onto an unsigned range with the same ordering, so a
//! lexicographic compare of the resulting `[u8; N]` matches a numeric
//! compare of the source integer.

/// SCALE for fixed-point average: 10^15. Picked to match the exactly-
/// representable integer range of float64 (2^53 ≈ 9.0×10^15). Averages
/// computed as `sum * SCALE / count` therefore round-trip through `f64`
/// without losing precision over the practical sum / count domain.
pub const AVG_FIXED_POINT_SCALE: i128 = 1_000_000_000_000_000;

/// `u64` big-endian. Lexicographic order on the bytes equals numeric order
/// on the source `u64`.
#[inline]
pub fn encode_count_sort_key(count: u64) -> [u8; 8] {
    count.to_be_bytes()
}

/// Inverse of [`encode_count_sort_key`].
#[inline]
pub fn decode_count_sort_key(bytes: &[u8; 8]) -> u64 {
    u64::from_be_bytes(*bytes)
}

/// `i64` big-endian with the sign bit flipped so the lexicographic order
/// on the bytes equals the **signed** numeric order on the source `i64`.
///
/// Without the sign-bit flip, negative two's-complement values would have
/// `0xFF…` leading bytes and sort *above* positive values, which is the
/// opposite of the numeric ordering. Flipping bit 63 swaps the high bit
/// of the sign byte so negatives become low and positives become high.
#[inline]
pub fn encode_sum_sort_key(sum: i64) -> [u8; 8] {
    let unsigned = (sum as u64) ^ 0x8000_0000_0000_0000;
    unsigned.to_be_bytes()
}

/// Inverse of [`encode_sum_sort_key`].
#[inline]
pub fn decode_sum_sort_key(bytes: &[u8; 8]) -> i64 {
    let unsigned = u64::from_be_bytes(*bytes) ^ 0x8000_0000_0000_0000;
    unsigned as i64
}

/// Compute the fixed-point average `floor(sum * SCALE / count)` as `i128`.
///
/// - `0 / 0` is defined to be `0` (the conventional choice for an empty
///   index where no entries have contributed yet).
/// - The intermediate `sum * SCALE` is performed in `i128` with
///   `saturating_mul`; the only way it could saturate is if `|sum|` were
///   to exceed `i128::MAX / SCALE`, which is `≈ 1.7×10^23` — orders of
///   magnitude beyond any realistic `i64` aggregation. Saturation is
///   therefore a defensive last line of defense; in practice the operation
///   never saturates.
/// - Euclidean division is used explicitly so negative fractional results
///   round toward negative infinity. Rust's `/` operator truncates signed
///   integers toward zero and would place negative averages one fixed-point
///   bucket too high.
#[inline]
pub fn compute_avg_fixed_point(sum: i64, count: u64) -> i128 {
    if count == 0 {
        return 0;
    }
    let sum_i128 = sum as i128;
    let count_i128 = count as i128;
    sum_i128
        .saturating_mul(AVG_FIXED_POINT_SCALE)
        .div_euclid(count_i128)
}

/// `i128` big-endian with the sign bit flipped (bit 127). Lexicographic
/// order on the bytes equals signed numeric order on the source `i128`.
///
/// Mirrors [`encode_sum_sort_key`] but on the wider type so the
/// fixed-point average can keep its `SCALE`-multiplied precision intact.
#[inline]
pub fn encode_avg_sort_key(avg_fixed: i128) -> [u8; 16] {
    let unsigned = (avg_fixed as u128) ^ 0x8000_0000_0000_0000_0000_0000_0000_0000;
    unsigned.to_be_bytes()
}

/// Inverse of [`encode_avg_sort_key`].
#[inline]
pub fn decode_avg_sort_key(bytes: &[u8; 16]) -> i128 {
    let unsigned = u128::from_be_bytes(*bytes) ^ 0x8000_0000_0000_0000_0000_0000_0000_0000;
    unsigned as i128
}

#[cfg(test)]
mod tests {
    use core::cmp::Ordering;

    use super::*;

    // ----- count -----

    #[test]
    fn count_round_trip() {
        for v in [0u64, 1, 42, u64::MAX / 2, u64::MAX] {
            let enc = encode_count_sort_key(v);
            assert_eq!(decode_count_sort_key(&enc), v);
        }
    }

    #[test]
    fn count_lexicographic_matches_numeric() {
        let sorted_values: Vec<u64> = vec![0, 1, 2, 100, 1_000_000, u64::MAX - 1, u64::MAX];
        let mut encoded: Vec<[u8; 8]> = sorted_values
            .iter()
            .copied()
            .map(encode_count_sort_key)
            .collect();
        let original = encoded.clone();
        encoded.sort();
        assert_eq!(
            encoded, original,
            "lexicographic byte order must equal numeric u64 order"
        );
    }

    // ----- sum -----

    #[test]
    fn sum_round_trip() {
        for v in [
            i64::MIN,
            i64::MIN + 1,
            -1_000_000,
            -1,
            0,
            1,
            1_000_000,
            i64::MAX - 1,
            i64::MAX,
        ] {
            let enc = encode_sum_sort_key(v);
            assert_eq!(decode_sum_sort_key(&enc), v);
        }
    }

    #[test]
    fn sum_lexicographic_matches_signed_numeric() {
        // Order: negative < zero < positive (the whole point of the sign-bit
        // flip). Includes the boundary values to lock in the wrap-around.
        let sorted_values: Vec<i64> = vec![
            i64::MIN,
            i64::MIN + 1,
            -1_000_000,
            -1,
            0,
            1,
            1_000_000,
            i64::MAX - 1,
            i64::MAX,
        ];
        let encoded: Vec<[u8; 8]> = sorted_values
            .iter()
            .copied()
            .map(encode_sum_sort_key)
            .collect();
        let mut shuffled = encoded.clone();
        shuffled.reverse();
        shuffled.sort();
        assert_eq!(
            shuffled, encoded,
            "byte order must match signed numeric order"
        );
    }

    /// `i64::MIN` is the most-negative representable signed value. Its
    /// encoding must be all zeros (`0x00..00`) because flipping bit 63 of
    /// `0x8000_0000_0000_0000` yields `0x0000_0000_0000_0000`. This pins
    /// the edge case so any future refactor of the flip stays correct.
    #[test]
    fn sum_min_edge_case() {
        let enc = encode_sum_sort_key(i64::MIN);
        assert_eq!(enc, [0u8; 8]);
        assert_eq!(decode_sum_sort_key(&enc), i64::MIN);

        let enc_max = encode_sum_sort_key(i64::MAX);
        assert_eq!(enc_max, [0xFF; 8]);
        assert_eq!(decode_sum_sort_key(&enc_max), i64::MAX);

        // Zero straddles the midpoint: the flip turns 0x00..00 into 0x80..00.
        let enc_zero = encode_sum_sort_key(0);
        assert_eq!(enc_zero[0], 0x80);
        assert!(enc_zero.iter().skip(1).all(|&b| b == 0));
    }

    #[test]
    fn sum_order_negative_lt_zero_lt_positive() {
        let neg = encode_sum_sort_key(-1);
        let zero = encode_sum_sort_key(0);
        let pos = encode_sum_sort_key(1);
        assert!(neg < zero);
        assert!(zero < pos);
        assert_eq!(neg.cmp(&zero), Ordering::Less);
    }

    // ----- avg -----

    #[test]
    fn avg_zero_over_zero_is_zero() {
        assert_eq!(compute_avg_fixed_point(0, 0), 0);
        assert_eq!(compute_avg_fixed_point(100, 0), 0);
        assert_eq!(compute_avg_fixed_point(-100, 0), 0);
    }

    #[test]
    fn avg_basic_arithmetic() {
        // 10 / 2 = 5; with SCALE: 5 * 10^15.
        assert_eq!(compute_avg_fixed_point(10, 2), 5 * AVG_FIXED_POINT_SCALE);
        // -10 / 2 = -5.
        assert_eq!(compute_avg_fixed_point(-10, 2), -5 * AVG_FIXED_POINT_SCALE);
        // 7 / 3 = 2.333… → floor toward zero for positives = 2.333… * SCALE.
        let v = compute_avg_fixed_point(7, 3);
        let expected = (7i128 * AVG_FIXED_POINT_SCALE) / 3i128;
        assert_eq!(v, expected);
    }

    #[test]
    fn avg_round_trip_through_encoding() {
        for (sum, count) in [
            (0i64, 0u64),
            (0, 1),
            (10, 2),
            (-10, 2),
            (i64::MAX, 1),
            (i64::MIN, 1),
            (i64::MAX, 7),
            (i64::MIN, 7),
            (1, 1_000_000),
            (-1, 1_000_000),
        ] {
            let v = compute_avg_fixed_point(sum, count);
            let enc = encode_avg_sort_key(v);
            assert_eq!(decode_avg_sort_key(&enc), v);
        }
    }

    #[test]
    fn avg_lexicographic_matches_signed_numeric() {
        // Construct a set of avg values spanning the negative / zero /
        // positive range and confirm the encoded bytes sort in the same
        // numeric order.
        let avg_values: Vec<i128> = vec![
            i128::MIN + 1,
            -1_000_000 * AVG_FIXED_POINT_SCALE,
            -1,
            0,
            1,
            1_000_000 * AVG_FIXED_POINT_SCALE,
            i128::MAX,
        ];
        let encoded: Vec<[u8; 16]> = avg_values
            .iter()
            .copied()
            .map(encode_avg_sort_key)
            .collect();
        let mut sorted = encoded.clone();
        sorted.sort();
        assert_eq!(
            sorted, encoded,
            "lexicographic byte order must equal signed i128 order"
        );
    }

    #[test]
    fn avg_saturating_at_boundary() {
        // Pin behavior at the documented saturation boundary: a sum that
        // would overflow `i128::MAX / SCALE` is saturating-clamped, not
        // panicking. With the current SCALE (10^15) the i128 boundary is
        // unreachable from any i64 sum.
        let max_sum = i64::MAX;
        let v = compute_avg_fixed_point(max_sum, 1);
        // (i64::MAX as i128) * 10^15 fits in i128 (≈ 9.2e33, well under
        // i128::MAX ≈ 1.7e38), so the multiplication does NOT saturate.
        let expected = (max_sum as i128) * AVG_FIXED_POINT_SCALE;
        assert_eq!(v, expected);
    }

    #[test]
    fn negative_average_uses_floor_bucket() {
        let expected_floor = (-AVG_FIXED_POINT_SCALE).div_euclid(3);
        let computed = compute_avg_fixed_point(-1, 3);

        assert_eq!(computed, expected_floor);
        assert_eq!(
            encode_avg_sort_key(computed),
            encode_avg_sort_key(expected_floor),
            "negative fractional averages must remain in the mathematical floor bucket"
        );
    }
}
