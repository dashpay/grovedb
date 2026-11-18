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

/// SCALE for the fixed-point average: 10^19, the largest power of ten whose
/// worst-case product `|i64::MIN| × SCALE` still fits in `i128`
/// (≈ 9.2×10^37 against `i128::MAX` ≈ 1.7×10^38 — proven by the
/// compile-time assertion below).
///
/// The scale is chosen to maximize ordering resolution. Two distinct
/// averages `s1/c1 ≠ s2/c2` differ by at least `1/(c1·c2)`, so they can
/// collapse onto one fixed-point value only when `c1·c2 > SCALE` — at
/// 10^19 that requires both counts above ~3.16 billion. A collapse is
/// benign (the full secondary key is `sort_key ‖ item_key`, so tied
/// averages order deterministically by item key), but at
/// billions-of-rows table sizes the previous 10^15 scale started
/// collapsing averages it should distinguish: its threshold was two
/// children of ~31.6 million rows each.
///
/// Fixed-point values at this scale do NOT round-trip through `f64`
/// (anything with `|avg| > ~0.0009` exceeds 2^53). That is deliberate:
/// the index never touches floats — ordering, encoding and consensus all
/// operate on the `i128` — and a consumer wanting a float view divides by
/// SCALE and accepts ordinary `f64` rounding of the display value.
pub const AVG_FIXED_POINT_SCALE: i128 = 10_000_000_000_000_000_000;

/// Compile-time proof of the overflow bound `|i64::MIN| × SCALE ≤
/// i128::MAX` that [`compute_avg_fixed_point`] relies on. A future scale
/// bump that breaks the bound fails the build here instead of silently
/// saturating at run time.
const _: () = assert!(AVG_FIXED_POINT_SCALE.checked_mul(1i128 << 63).is_some());

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
///   `saturating_mul`. Saturation is unreachable: `|sum| ≤ |i64::MIN| =
///   2^63` by type, and `2^63 × SCALE ≤ i128::MAX` is proven at compile
///   time by the assertion next to [`AVG_FIXED_POINT_SCALE`] (headroom
///   ≈ 1.84×). The saturating form is a defensive last line, not a
///   reachable code path.
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

/// `i128` big-endian with the sign bit flipped: the sort key of the sum axis
/// of a tree whose sums are 128 bits wide (`ProvableBigSumIndexedTree`,
/// `ProvableCountProvableBigSumIndexedTree`). Lexicographic order on the
/// bytes equals signed numeric order on the source `i128`.
///
/// The same encoding as [`encode_avg_sort_key`], under the name of what it
/// orders here.
#[inline]
pub fn encode_big_sum_sort_key(sum: i128) -> [u8; 16] {
    encode_avg_sort_key(sum)
}

/// Inverse of [`encode_big_sum_sort_key`].
#[inline]
pub fn decode_big_sum_sort_key(bytes: &[u8; 16]) -> i128 {
    decode_avg_sort_key(bytes)
}

/// The fixed-point average `floor(sum * SCALE / count)` of a 128-bit sum,
/// as an `i128`, saturating at the ends of the `i128` range.
///
/// For a sum that fits an `i64` this is [`compute_avg_fixed_point`] exactly.
/// A wider sum times the scale can pass the `i128` range, so the product is
/// never formed: with `sum = q * count + r` (`0 <= r < count`), the average
/// is `q * SCALE + floor(r * SCALE / count)`, and `r * SCALE` fits a `u128`
/// since both factors are below `2^64`. The result is exact whenever it fits
/// an `i128`: every average inside the `i64` range (the compile-time bound
/// next to [`AVG_FIXED_POINT_SCALE`]) and those past it up to a magnitude of
/// `i128::MAX / SCALE`, about 1.7 × 10^19. Only an average whose fixed-point
/// form is outside the `i128` range, which takes entries that are themselves
/// trees with 128-bit sums, saturates to `i128::MIN` or `i128::MAX`: such
/// entries keep their relative order with every entry inside the range and
/// tie among themselves, where the secondary key's item-key suffix orders
/// them.
///
/// `0 / 0` is `0`, as for the narrow average.
#[inline]
pub fn compute_big_avg_fixed_point(sum: i128, count: u64) -> i128 {
    if count == 0 {
        return 0;
    }
    let count_i128 = count as i128;
    let quotient = sum.div_euclid(count_i128);
    // `rem_euclid` is in `0..count`, so it and its product with the scale
    // fit a `u128`.
    let remainder = sum.rem_euclid(count_i128) as u128;
    // Below the scale, since `remainder < count`.
    let fraction = ((remainder * AVG_FIXED_POINT_SCALE as u128) / count as u128) as i128;
    if let Some(average) = quotient
        .checked_mul(AVG_FIXED_POINT_SCALE)
        .and_then(|whole| whole.checked_add(fraction))
    {
        return average;
    }
    if quotient > 0 {
        return i128::MAX;
    }
    // A negative quotient times the scale can pass `i128::MIN` while the
    // average, the fraction added, does not. Form it one whole unit higher:
    // `(q + 1) * SCALE + (fraction - SCALE)` is the same value, and its
    // first term lies above the average.
    (quotient + 1)
        .checked_mul(AVG_FIXED_POINT_SCALE)
        .and_then(|whole| whole.checked_add(fraction - AVG_FIXED_POINT_SCALE))
        .unwrap_or(i128::MIN)
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
        // 10 / 2 = 5; with SCALE: 5 * 10^19.
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
        // Pin behavior at the extreme: the worst-case i64 sum must NOT
        // saturate. The compile-time assertion next to the constant proves
        // `2^63 × SCALE ≤ i128::MAX` (with SCALE = 10^19 the product is
        // ≈ 9.2e37 against i128::MAX ≈ 1.7e38, headroom ≈ 1.84×); this
        // pins the same fact at run time from the public API.
        let max_sum = i64::MAX;
        let v = compute_avg_fixed_point(max_sum, 1);
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

    /// The wide average equals the narrow one for every sum that fits an
    /// `i64`, including negative sums that round toward negative infinity.
    #[test]
    fn big_avg_matches_the_narrow_avg_inside_the_i64_range() {
        for sum in [
            0i64,
            1,
            -1,
            7,
            -7,
            1_000_003,
            -1_000_003,
            i64::MAX,
            i64::MIN,
            i64::MAX - 1,
            i64::MIN + 1,
        ] {
            for count in [1u64, 2, 3, 7, 1_000, u32::MAX as u64, u64::MAX] {
                assert_eq!(
                    compute_big_avg_fixed_point(sum as i128, count),
                    compute_avg_fixed_point(sum, count),
                    "sum {sum} count {count}"
                );
            }
        }
        assert_eq!(compute_big_avg_fixed_point(i128::MAX, 0), 0);
    }

    /// Past the `i64` range the wide average is exact while it fits an
    /// `i128`, keeps the order of the averages, and saturates beyond.
    /// A negative average just inside the `i128` range: its whole part
    /// times the scale lies below `i128::MIN`, and the fraction brings it
    /// back in. It is exact, not saturated.
    #[test]
    fn big_avg_is_exact_up_to_the_negative_edge() {
        let sum: i128 = -170_141_183_460_469_231_731;
        // sum / 10 at 19 fractional digits is sum * 10^18, which fits.
        let exact = sum * 10i128.pow(18);
        assert!(sum
            .div_euclid(10)
            .checked_mul(AVG_FIXED_POINT_SCALE)
            .is_none());
        assert_eq!(compute_big_avg_fixed_point(sum, 10), exact);
        assert!(exact > i128::MIN);

        // One step lower the average itself is out of range and saturates,
        // and the order of the three is kept.
        assert_eq!(compute_big_avg_fixed_point(sum - 10, 10), i128::MIN);
        assert!(compute_big_avg_fixed_point(sum + 10, 10) > compute_big_avg_fixed_point(sum, 10));

        // The last representable average and the first that is not.
        let floor = i128::MIN / AVG_FIXED_POINT_SCALE;
        assert_eq!(
            compute_big_avg_fixed_point(floor, 1),
            floor * AVG_FIXED_POINT_SCALE
        );
        assert_eq!(compute_big_avg_fixed_point(floor - 1, 1), i128::MIN);
    }

    #[test]
    fn big_avg_is_exact_while_it_fits_and_saturates_beyond() {
        // Three entries at i64::MAX: the sum does not fit an i64, the
        // average is i64::MAX exactly.
        let sum = 3 * (i64::MAX as i128);
        assert_eq!(
            compute_big_avg_fixed_point(sum, 3),
            (i64::MAX as i128) * AVG_FIXED_POINT_SCALE
        );
        // A fractional average: (3 * MAX + 1) / 3 = MAX + 1/3.
        assert_eq!(
            compute_big_avg_fixed_point(sum + 1, 3),
            (i64::MAX as i128) * AVG_FIXED_POINT_SCALE + AVG_FIXED_POINT_SCALE / 3
        );
        // Negative: floor toward negative infinity.
        assert_eq!(
            compute_big_avg_fixed_point(-sum - 1, 3),
            -(i64::MAX as i128) * AVG_FIXED_POINT_SCALE - AVG_FIXED_POINT_SCALE / 3 - 1
        );
        // Order is kept across the ends of the range.
        let ascending = [
            compute_big_avg_fixed_point(i128::MIN, 1),
            compute_big_avg_fixed_point(-sum, 3),
            compute_big_avg_fixed_point(-1, 2),
            compute_big_avg_fixed_point(0, 5),
            compute_big_avg_fixed_point(1, 2),
            compute_big_avg_fixed_point(sum, 3),
            compute_big_avg_fixed_point(i128::MAX, 1),
        ];
        for pair in ascending.windows(2) {
            assert!(pair[0] <= pair[1], "{} then {}", pair[0], pair[1]);
        }
        // An average beyond what the fixed point can hold saturates.
        assert_eq!(compute_big_avg_fixed_point(i128::MAX, 1), i128::MAX);
        assert_eq!(compute_big_avg_fixed_point(i128::MIN, 1), i128::MIN);
        assert_eq!(compute_big_avg_fixed_point(i128::MAX, 2), i128::MAX);
        // The largest count with the largest remainder does not overflow.
        let near = (u64::MAX as i128) - 1;
        assert_eq!(
            compute_big_avg_fixed_point(near, u64::MAX),
            ((near as u128 * AVG_FIXED_POINT_SCALE as u128) / u64::MAX as u128) as i128
        );
    }

    /// The wide sum key orders as the sums do, across the `i64` range.
    #[test]
    fn big_sum_sort_key_orders_as_the_sums_do() {
        let sums = [
            i128::MIN,
            3 * (i64::MIN as i128),
            i64::MIN as i128,
            -1,
            0,
            1,
            i64::MAX as i128,
            3 * (i64::MAX as i128),
            i128::MAX,
        ];
        for pair in sums.windows(2) {
            assert!(encode_big_sum_sort_key(pair[0]) < encode_big_sum_sort_key(pair[1]));
        }
        for sum in sums {
            assert_eq!(decode_big_sum_sort_key(&encode_big_sum_sort_key(sum)), sum);
        }
    }
}
