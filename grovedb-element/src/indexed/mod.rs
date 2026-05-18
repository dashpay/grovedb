//! Indexed-tree primitives shared by `Element::ProvableSumIndexedTree`,
//! `Element::ProvableCountIndexedTree`, and
//! `Element::ProvableCountProvableSumIndexedTree`.
//!
//! - [`IndexAxis`] enumerates the axes a `ProvableCountProvableSumIndexedTree`
//!   can index by (count, sum, average).
//! - [`sort_keys`] supplies the fixed-width, order-preserving byte
//!   encoders used to prefix secondary-Merk keys.
//!
//! See the per-item docs and the `Element` doc comments for the full
//! design.

pub mod sort_keys;

pub use sort_keys::{
    compute_avg_fixed_point, decode_avg_sort_key, decode_count_sort_key, decode_sum_sort_key,
    encode_avg_sort_key, encode_count_sort_key, encode_sum_sort_key, AVG_FIXED_POINT_SCALE,
};

use crate::error::ElementError;

/// Axis tag for a `ProvableCountProvableSumIndexedTree` secondary entry.
///
/// The TLV-encoded `axes` field on a `ProvableCountProvableSumIndexedTree`
/// is a canonical list of `(tag, secondary_root_key)` pairs sorted by
/// tag, with 1..=3 entries and no duplicate tags. The numeric values
/// below are the on-disk tag bytes:
///
/// - `0` = `Count`: secondary keyed by `(count_be ‖ original_key)`.
/// - `1` = `Sum`:   secondary keyed by `(sum_sortable_be ‖ original_key)`.
/// - `2` = `Avg`:   secondary keyed by `(avg_sortable_be ‖ original_key)`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum IndexAxis {
    /// Count axis. Secondary entries are ordered by aggregate count.
    Count = 0,
    /// Sum axis. Secondary entries are ordered by aggregate sum (signed).
    Sum = 1,
    /// Average axis. Secondary entries are ordered by the fixed-point
    /// average `floor(sum * SCALE / count)`. See
    /// [`sort_keys::AVG_FIXED_POINT_SCALE`] for the scale rationale.
    Avg = 2,
}

impl IndexAxis {
    /// On-disk tag byte for this axis.
    #[inline]
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Inverse of [`tag`]: parse an on-disk tag byte into the axis. Returns
    /// `Err(ElementError::CorruptedData)` for any byte outside the
    /// `0..=2` range.
    #[inline]
    pub fn try_from_tag(b: u8) -> Result<Self, ElementError> {
        match b {
            0 => Ok(IndexAxis::Count),
            1 => Ok(IndexAxis::Sum),
            2 => Ok(IndexAxis::Avg),
            _ => Err(ElementError::CorruptedData(format!("unknown axis tag {b}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_tag_round_trip() {
        for a in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            assert_eq!(IndexAxis::try_from_tag(a.tag()).unwrap(), a);
        }
    }

    #[test]
    fn axis_tag_rejects_unknown_byte() {
        assert!(IndexAxis::try_from_tag(3).is_err());
        assert!(IndexAxis::try_from_tag(255).is_err());
    }

    #[test]
    fn axis_ordering_is_canonical() {
        // The canonical order required by the on-disk TLV: Count < Sum < Avg.
        assert!(IndexAxis::Count < IndexAxis::Sum);
        assert!(IndexAxis::Sum < IndexAxis::Avg);
    }
}
