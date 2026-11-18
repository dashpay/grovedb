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
    encode_avg_sort_key, encode_count_sort_key, encode_sum_sort_key, IndexAxis,
};

use crate::{Element, Error};

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
/// Fallible only through [`count_value_as_sum`]'s fail-closed guard.
#[inline]
pub(crate) fn axis_payload_sum(axis: IndexAxis, count: u64, sum: i64) -> Result<i64, Error> {
    Ok(match axis {
        IndexAxis::Count => count_value_as_sum(count)?,
        IndexAxis::Sum | IndexAxis::Avg => sum,
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
    sum: i64,
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
#[cfg(feature = "minimal")]
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
#[inline]
pub(crate) fn make_axis_secondary_key(
    axis: IndexAxis,
    count: u64,
    sum: i64,
    item_key: &[u8],
) -> Vec<u8> {
    match axis {
        IndexAxis::Count => {
            let prefix = encode_count_sort_key(count);
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
        IndexAxis::Sum => {
            let prefix = encode_sum_sort_key(sum);
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
        IndexAxis::Avg => {
            let avg_fp = grovedb_element::indexed::compute_avg_fixed_point(sum, count);
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
        IndexAxis::Avg => 16,
    }
}
