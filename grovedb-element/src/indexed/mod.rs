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
/// tag, with 1..=3 entries and no duplicate tags.
///
/// The enum itself is defined in `grovedb-query` (re-exported here so
/// existing paths keep working), because the query vocabulary names
/// axes too and the tag byte must have exactly one definition. The
/// numeric values are the on-disk tag bytes: `0` = Count, `1` = Sum,
/// `2` = Avg — see the definition for the secondary key layouts.
pub use grovedb_query::axis_query::{IndexAxis, UnknownAxisTag};

impl From<UnknownAxisTag> for ElementError {
    fn from(err: UnknownAxisTag) -> Self {
        ElementError::CorruptedData(err.to_string())
    }
}

/// One entry in a `ProvableCountProvableSumIndexedTree`'s `axes` TLV list:
/// `(axis_tag, secondary_root_key)`. The tag byte matches [`IndexAxis::tag`];
/// the optional `Vec<u8>` is the secondary Merk's root key (`None` while
/// empty).
pub type IndexedTreeAxisEntry = (u8, Option<Vec<u8>>);

/// Canonical (sorted-by-tag, deduped, 1..=3 entries) TLV list of axis
/// entries carried by a `ProvableCountProvableSumIndexedTree` element. See
/// the variant doc comment on [`crate::Element`] for the on-disk
/// encoding.
pub type IndexedTreeAxes = Vec<IndexedTreeAxisEntry>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_axis_tag_converts_to_element_error() {
        // The tag round-trip tests live with the enum in `grovedb-query`;
        // what this crate owns is the error-boundary conversion.
        let err: ElementError = IndexAxis::try_from_tag(9).unwrap_err().into();
        match err {
            ElementError::CorruptedData(msg) => assert_eq!(msg, "unknown axis tag 9"),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }
}
