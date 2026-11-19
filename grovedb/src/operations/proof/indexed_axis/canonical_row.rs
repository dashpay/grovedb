//! The canonical indexed-secondary row: its shape, its sort key, and the
//! encode/decode pair every writer and every checker shares.
//!
//! This module is deliberately free of storage and transaction types so it
//! compiles in a **verify-only** build. A light client that never opens a
//! Merk still has to rebuild the canonical row a proof claims, and it
//! rebuilds it from exactly the same definition the mirror wrote with —
//! which is the property that makes the check meaningful rather than a
//! restatement of whatever the prover sent.

use grovedb_element::indexed::{
    compute_avg_fixed_point, compute_big_avg_fixed_point, encode_avg_sort_key,
    encode_big_sum_sort_key, encode_count_sort_key, encode_sum_sort_key, IndexAxis,
};
#[cfg(feature = "engine_generic")]
use grovedb_merk::TreeType;

use crate::{Element, Error};

/// The sum of a primary entry as its secondary rows see it: 64 bits wide
/// under `ProvableSumIndexedTree` and `ProvableCountProvableSumIndexedTree`,
/// 128 bits wide under their big sum counterparts.
///
/// The width is the primary's, not the value's: it decides how the avg key
/// is computed and what sum a row carries, for a sum that would fit 64 bits
/// as much as for one that would not.
///
/// A primary's sum axis follows its width: [`IndexAxis::Sum`] under a narrow
/// primary, [`IndexAxis::BigSum`] under a wide one. The axes validators
/// (`Element::validate_pcpsit_axes`, `Element::validate_pcpbsit_axes`)
/// refuse the other, so a narrow sum never meets the big sum axis and a
/// wide sum never meets the sum axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntrySum {
    /// An `i64` sum, under a primary whose sum is an `i64`.
    Narrow(i64),
    /// An `i128` sum, under a primary whose sum is an `i128`.
    Wide(i128),
}

impl EntrySum {
    /// The sum at full width.
    #[inline]
    pub(crate) fn as_i128(self) -> i128 {
        match self {
            EntrySum::Narrow(sum) => sum as i128,
            EntrySum::Wide(sum) => sum,
        }
    }

    /// The sum as the sum axis keys it. A wide sum has no sum axis, so the
    /// second arm is not reached; it saturates rather than wrap.
    #[inline]
    fn as_sum_axis_value(self) -> i64 {
        match self {
            EntrySum::Narrow(sum) => sum,
            EntrySum::Wide(sum) => sum.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
        }
    }
}

impl From<i64> for EntrySum {
    fn from(sum: i64) -> Self {
        EntrySum::Narrow(sum)
    }
}

/// Whether `primary_tree_type` is an indexed primary whose sum is 128 bits
/// wide.
#[cfg(feature = "engine_generic")]
#[inline]
pub(crate) fn is_wide_indexed_primary(primary_tree_type: TreeType) -> bool {
    matches!(
        primary_tree_type,
        TreeType::ProvableBigSumIndexedTree | TreeType::ProvableCountProvableBigSumIndexedTree
    )
}

/// Whether the serialized element `element_value_bytes` is an indexed tree
/// whose sum is 128 bits wide, read off its discriminant alone and through
/// a `NonCounted` wrapper. What a verifier, which opens no Merk, reads the
/// width of a proved indexed tree from.
pub(crate) fn is_wide_indexed_element_bytes(element_value_bytes: &[u8]) -> Result<bool, Error> {
    let family = grovedb_element::ElementType::from_serialized_value(element_value_bytes)
        .map_err(|e| {
            Error::CorruptedData(format!(
                "cannot determine element type of a proved indexed tree: {e}"
            ))
        })?
        .base();
    Ok(matches!(
        family,
        grovedb_element::ElementType::ProvableBigSumIndexedTree
            | grovedb_element::ElementType::ProvableCountProvableBigSumIndexedTree
    ))
}

/// The `(count, sum)` a primary entry contributes to its indexed primary,
/// which is what its secondary rows are keyed by: the entry's 128-bit sum
/// under a wide primary, its `i64` sum under a narrow one.
///
/// The one definition every writer and every checker reads the entry
/// through, next to [`axis_row_reference`] and [`make_axis_secondary_key`].
#[inline]
pub(crate) fn entry_count_and_sum(entry: &Element, wide_primary: bool) -> (u64, EntrySum) {
    if wide_primary {
        let (count, sum) = entry.count_big_sum_value_or_default();
        (count, EntrySum::Wide(sum))
    } else {
        let (count, sum) = entry.count_sum_value_or_default();
        (count, EntrySum::Narrow(sum))
    }
}

/// The count-axis secondary stores each entry's `count_value` as its
/// sum item, and sum items are `i64`: a count above `i64::MAX` cannot
/// be mirrored faithfully, so it FAILS CLOSED rather than clamping —
/// a clamped total would silently lie through authenticated state.
/// Unreachable for any real tree (a count is bounded by the number of
/// elements), so the guard is a type-level seam, not a live limit.
#[inline]
pub(crate) fn count_value_as_sum(count: u64) -> Result<i64, Error> {
    i64::try_from(count).map_err(|_| {
        Error::CorruptedData(format!(
            "count value {count} exceeds i64::MAX and cannot be mirrored into the \
             count-axis secondary's sum aggregate"
        ))
    })
}

/// Hop budget stamped on every canonical indexed-secondary row.
///
/// The row binds the IMMEDIATE primary node, not a terminal: its
/// committed value hash is
/// `combine_hash(H(canonical_reference_bytes), primary_node_value_hash)`
/// where `primary_node_value_hash` is whatever the primary Merk stores
/// for that key — a simple hash for an item, a layered/combined hash for
/// a tree or a nested reference. Binding the immediate node is what keeps
/// the invariant LOCAL and therefore mirror-maintainable: a mutation to
/// some distant terminal cannot staleness this row without also rewriting
/// the primary entry, which is the event the mirror is driven by.
///
/// This is dedicated indexed-tree behaviour and is NOT a relaxation of
/// ordinary user-reference semantics — an ordinary `max_hop = 1`
/// reference pointing at another reference remains ill-formed and keeps
/// its existing diagnostics. Every consumer of an indexed row must select
/// the immediate-node rule explicitly (that is what
/// [`resolve_indexed_row_target`] and [`verify_indexed_axis_content`] do);
/// nothing may infer it from `max_reference_hop == 1` alone.
///
/// [`resolve_indexed_row_target`]: crate::GroveDb::resolve_indexed_row_target
/// [`verify_indexed_axis_content`]: crate::GroveDb
pub(crate) const INDEXED_SECONDARY_MAX_HOP: grovedb_element::MaxReferenceHop = Some(1);

/// The sum an axis's canonical row carries — the axis PAYLOAD sum, which
/// is not universally the primary's sum:
///
/// - Count → `count_value_as_sum(count)`, so a band TOTAL over the count
///   axis stays one committed scalar (issue #806). A plain `Reference`
///   here would fold to `(1, 0)` and silently zero every band total.
/// - Sum / Avg → the primary entry's own sum.
///
/// Every writer and every checker must agree on this one definition; a
/// divergent copy either false-flags healthy state or makes two entry
/// points commit different roots for identical writes (#809 audit).
///
/// Under a wide primary the big sum and avg rows carry 0: a row is a
/// `ReferenceWithSumItem`, whose sum is an `i64`, and a 128-bit sum cannot
/// be narrowed into it. The ordering is in the key, so ranking is
/// unaffected; what the payload feeds is the band TOTAL, which neither the
/// big sum axis nor the avg axis offers (`AxisQuery::validate` refuses
/// both).
///
/// Fallible only through [`count_value_as_sum`]'s fail-closed guard.
#[inline]
pub(crate) fn axis_payload_sum(
    axis: IndexAxis,
    count: u64,
    sum: impl Into<EntrySum>,
) -> Result<i64, Error> {
    Ok(match axis {
        IndexAxis::Count => count_value_as_sum(count)?,
        IndexAxis::Sum | IndexAxis::Avg => match sum.into() {
            EntrySum::Narrow(sum) => sum,
            EntrySum::Wide(_) => 0,
        },
        IndexAxis::BigSum => 0,
    })
}

/// THE per-axis secondary row — the single definition every writer and
/// every checker uses. The sort KEY encodes the ordering value; the row
/// itself is a canonical one-hop reference back to the primary entry,
/// carrying the axis payload sum so the secondary's dual aggregates fold
/// to `(1, axis_payload_sum)`.
///
/// All three axes share one element family:
/// `ReferenceWithSumItem(SiblingReference(item_key), Some(1), sum)`.
///
/// The `SiblingReference` is interpreted against the row's LOGICAL
/// origin — the indexed primary's path — not against the derived storage
/// prefix the secondary physically lives under (which is not a GroveDB
/// path at all). See [`indexed_row_target_key`] for the decoding side and
/// [`crate::operations::indexed_tree`]'s module docs for the origin rule.
///
/// Callers: the batch mirror row builder, the direct-path mirror, the
/// propagation mirror, `verify_grovedb`'s expected-row check, the axis
/// proof generator, and the average-case cost estimator's worst-case row.
///
/// Fallible only through [`count_value_as_sum`]'s fail-closed guard.
pub(crate) fn axis_row_reference(
    axis: IndexAxis,
    item_key: &[u8],
    count: u64,
    sum: impl Into<EntrySum>,
) -> Result<Element, Error> {
    Ok(Element::new_reference_with_sum_item_with_hops(
        grovedb_element::reference_path::ReferencePathType::SiblingReference(item_key.to_vec()),
        INDEXED_SECONDARY_MAX_HOP,
        axis_payload_sum(axis, count, sum)?,
    ))
}

/// Decode a stored secondary row, enforcing the canonical shape and
/// returning `(target_item_key, carried_sum)`.
///
/// Rejects anything that is not exactly
/// `ReferenceWithSumItem(SiblingReference(_), Some(1), _)` — including
/// the legacy placeholder payloads (`SumItem` / `ItemWithSumItem`), a
/// plain `Reference` (which would fold to `(1, 0)`), a non-sibling
/// reference type, and a wrong hop budget. `describe` labels the caller
/// in the error so a corruption report says where it was caught.
#[cfg(feature = "engine_generic")]
pub(crate) fn decode_axis_row_reference<'a>(
    row: &'a Element,
    describe: &str,
) -> Result<(&'a [u8], i64), Error> {
    match row {
        Element::ReferenceWithSumItem(reference_path, max_hop, sum, _) => {
            if *max_hop != INDEXED_SECONDARY_MAX_HOP {
                return Err(Error::CorruptedData(format!(
                    "{describe}: indexed secondary row carries max_reference_hop {max_hop:?}, \
                     canonical rows are one-hop ({INDEXED_SECONDARY_MAX_HOP:?})"
                )));
            }
            match reference_path {
                grovedb_element::reference_path::ReferencePathType::SiblingReference(key) => {
                    Ok((key.as_slice(), *sum))
                }
                other => Err(Error::CorruptedData(format!(
                    "{describe}: indexed secondary row must be a SiblingReference to its \
                     primary entry, found {other}"
                ))),
            }
        }
        other => Err(Error::CorruptedData(format!(
            "{describe}: indexed secondary row must be ReferenceWithSumItem, found {}",
            other.type_str()
        ))),
    }
}

/// Build the secondary key bytes for an entry at `item_key` under the
/// given axis, given the relevant aggregate values:
/// - count axis → `count_be(8) ‖ item_key`
/// - sum axis   → `sum_sortable_be(8) ‖ item_key`
/// - avg axis   → `avg_sortable_be(16) ‖ item_key`
/// - big sum axis → `big_sum_sortable_be(16) ‖ item_key`
#[inline]
pub(crate) fn make_axis_secondary_key(
    axis: IndexAxis,
    count: u64,
    sum: impl Into<EntrySum>,
    item_key: &[u8],
) -> Vec<u8> {
    let sum = sum.into();
    match axis {
        IndexAxis::Count => {
            let prefix = encode_count_sort_key(count);
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
        IndexAxis::Sum => {
            let prefix = encode_sum_sort_key(sum.as_sum_axis_value());
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
        IndexAxis::BigSum => {
            let prefix = encode_big_sum_sort_key(sum.as_i128());
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
        IndexAxis::Avg => {
            let avg_fp = match sum {
                EntrySum::Narrow(sum) => compute_avg_fixed_point(sum, count),
                EntrySum::Wide(sum) => compute_big_avg_fixed_point(sum, count),
            };
            let prefix = encode_avg_sort_key(avg_fp);
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
    }
}

/// Width in bytes of an axis's sort-key prefix inside a secondary key
/// (`sort_key ‖ item_key`).
#[inline]
pub(crate) fn axis_sort_key_len(axis: IndexAxis) -> usize {
    match axis {
        IndexAxis::Count | IndexAxis::Sum => 8,
        IndexAxis::Avg | IndexAxis::BigSum => 16,
    }
}
