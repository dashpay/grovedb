//! Lowering an [`AxisQuery`]'s bounded traversal into the secondary's
//! own Merk query.
//!
//! This is prover/verifier agreement material: both sides build the
//! range from the query through *this* function, so they cannot drift
//! on which secondary entries a proof is about. The secondary's keys
//! are `sort_key ‖ original_key`, so an inclusive bound on the
//! aggregate becomes a byte range that brackets every key-suffix at the
//! boundary sort key: inclusive at `lo`, exclusive at the *successor*
//! of `hi`. When `hi` is already the axis maximum there is no
//! successor, and the range is open-ended instead.
//!
//! Compiled under both `minimal` (prover) and `verify` (light-client
//! verifier).

use grovedb_element::indexed::{
    encode_avg_sort_key, encode_count_sort_key, encode_sum_sort_key, IndexAxis,
};
use grovedb_merk::proofs::{
    query::{query_item::QueryItem as MerkQueryItem, AxisQuery, AxisTraversal},
    Query as MerkQuery,
};

use crate::Error;

/// Lower a [`AxisTraversal::Bounded`] traversal into the secondary's
/// Merk query. Errors on any other traversal (top-k and rank are served
/// by the count-offset paginated primitives; range aggregates by the
/// aggregate-on-range primitives) and on a query that fails
/// [`AxisQuery::validate`].
pub(crate) fn axis_bounded_merk_query(axis_query: &AxisQuery) -> Result<MerkQuery, Error> {
    let AxisTraversal::Bounded { lo, hi, .. } = axis_query.traversal else {
        return Err(Error::InvalidQuery(
            "only a bounded traversal lowers to a secondary Merk query",
        ));
    };
    axis_query
        .validate()
        .map_err(crate::query::shape::read_mode_validation_error)?;

    let (lo_bytes, hi_exclusive) = match axis_query.axis {
        IndexAxis::Count => {
            let lo = lo.clamp(0, u64::MAX as i128) as u64;
            let hi = hi.clamp(0, u64::MAX as i128) as u64;
            (
                encode_count_sort_key(lo).to_vec(),
                hi.checked_add(1)
                    .map(|next| encode_count_sort_key(next).to_vec()),
            )
        }
        IndexAxis::Sum => {
            let lo = lo.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            let hi = hi.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            (
                encode_sum_sort_key(lo).to_vec(),
                hi.checked_add(1)
                    .map(|next| encode_sum_sort_key(next).to_vec()),
            )
        }
        IndexAxis::Avg => (
            encode_avg_sort_key(lo).to_vec(),
            hi.checked_add(1)
                .map(|next| encode_avg_sort_key(next).to_vec()),
        ),
    };

    let mut query = MerkQuery::new();
    match hi_exclusive {
        Some(hi_bytes) => query.insert_item(MerkQueryItem::Range(lo_bytes..hi_bytes)),
        None => query.insert_item(MerkQueryItem::RangeFrom(lo_bytes..)),
    }
    query.left_to_right = !axis_query.descending;
    Ok(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grovedb_merk::proofs::query::AggregateFold;

    #[test]
    fn bounded_lowering_brackets_the_boundary_sort_keys() {
        let q = axis_bounded_merk_query(&AxisQuery::bounded(IndexAxis::Count, 3, 7, 10, false))
            .expect("bounded lowers");
        assert!(q.left_to_right);
        match &q.items[0] {
            MerkQueryItem::Range(range) => {
                assert_eq!(range.start, encode_count_sort_key(3).to_vec());
                // Exclusive at the successor of hi: covers every
                // original-key suffix at hi itself.
                assert_eq!(range.end, encode_count_sort_key(8).to_vec());
            }
            other => panic!("expected Range, got {other:?}"),
        }
    }

    #[test]
    fn bounded_lowering_at_axis_max_is_open_ended() {
        let q = axis_bounded_merk_query(&AxisQuery::bounded(
            IndexAxis::Sum,
            0,
            i64::MAX as i128,
            1,
            true,
        ))
        .expect("bounded at max lowers");
        assert!(!q.left_to_right);
        assert!(matches!(&q.items[0], MerkQueryItem::RangeFrom(_)));
    }

    #[test]
    fn non_bounded_traversals_do_not_lower() {
        assert!(axis_bounded_merk_query(&AxisQuery::top_k(IndexAxis::Count, 1, 0, true)).is_err());
        assert!(
            axis_bounded_merk_query(&AxisQuery::aggregate_over_value_range(
                IndexAxis::Sum,
                0,
                1,
                AggregateFold::Total
            ))
            .is_err()
        );
    }
}
