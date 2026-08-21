use std::ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive};

use crate::{query_item::QueryItem, Query};

impl Query {
    /// Adds an individual key to the query, so that its value (or its absence)
    /// in the tree will be included in the resulting proof.
    ///
    /// If the key or a range including the key already exists in the query,
    /// this will have no effect. If the query already includes a range that has
    /// a non-inclusive bound equal to the key, the bound will be changed to be
    /// inclusive.
    pub fn insert_key(&mut self, key: Vec<u8>) {
        let key = QueryItem::Key(key);
        self.insert_item(key);
    }

    /// Adds multiple individual keys to the query, so that its value (or its
    /// absence) in the tree will be included in the resulting proof.
    ///
    /// If the key or a range including the key already exists in the query,
    /// this will have no effect. If the query already includes a range that has
    /// a non-inclusive bound equal to the key, the bound will be changed to be
    /// inclusive.
    pub fn insert_keys(&mut self, keys: Vec<Vec<u8>>) {
        for key in keys {
            let key = QueryItem::Key(key);
            self.insert_item(key);
        }
    }

    /// Adds a range to the query, so that all the entries in the tree with keys
    /// in the range will be included in the resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be joined together.
    pub fn insert_range(&mut self, range: Range<Vec<u8>>) {
        let range = QueryItem::Range(range);
        self.insert_item(range);
    }

    /// Adds an inclusive range to the query, so that all the entries in the
    /// tree with keys in the range will be included in the resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be merged together.
    pub fn insert_range_inclusive(&mut self, range: RangeInclusive<Vec<u8>>) {
        let range = QueryItem::RangeInclusive(range);
        self.insert_item(range);
    }

    /// Adds a range until a certain included value to the query, so that all
    /// the entries in the tree with keys in the range will be included in the
    /// resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be joined together.
    pub fn insert_range_to_inclusive(&mut self, range: RangeToInclusive<Vec<u8>>) {
        let range = QueryItem::RangeToInclusive(range);
        self.insert_item(range);
    }

    /// Adds a range from a certain included value to the query, so that all
    /// the entries in the tree with keys in the range will be included in the
    /// resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be joined together.
    pub fn insert_range_from(&mut self, range: RangeFrom<Vec<u8>>) {
        let range = QueryItem::RangeFrom(range);
        self.insert_item(range);
    }

    /// Adds a range until a certain non included value to the query, so that
    /// all the entries in the tree with keys in the range will be included
    /// in the resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be joined together.
    pub fn insert_range_to(&mut self, range: RangeTo<Vec<u8>>) {
        let range = QueryItem::RangeTo(range);
        self.insert_item(range);
    }

    /// Adds a range after the first value, so that all the entries in the tree
    /// with keys in the range will be included in the resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be joined together.
    pub fn insert_range_after(&mut self, range: RangeFrom<Vec<u8>>) {
        let range = QueryItem::RangeAfter(range);
        self.insert_item(range);
    }

    /// Adds a range after the first value, until a certain non included value
    /// to the query, so that all the entries in the tree with keys in the
    /// range will be included in the resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be joined together.
    pub fn insert_range_after_to(&mut self, range: Range<Vec<u8>>) {
        let range = QueryItem::RangeAfterTo(range);
        self.insert_item(range);
    }

    /// Adds a range after the first value, until a certain included value to
    /// the query, so that all the entries in the tree with keys in the
    /// range will be included in the resulting proof.
    ///
    /// If a range including the range already exists in the query, this will
    /// have no effect. If the query already includes a range that overlaps with
    /// the range, the ranges will be joined together.
    pub fn insert_range_after_to_inclusive(&mut self, range: RangeInclusive<Vec<u8>>) {
        let range = QueryItem::RangeAfterToInclusive(range);
        self.insert_item(range);
    }

    /// Adds a range of all potential values to the query, so that the query
    /// will return all values
    ///
    /// All other plain key/range items in the query will be discarded as you
    /// are now getting back all elements. Aggregate meta-items
    /// (`AggregateCountOnRange` etc.) are kept — see [`Self::insert_item`].
    pub fn insert_all(&mut self) {
        let range = QueryItem::RangeFull(RangeFull);
        self.insert_item(range);
    }

    /// Adds the `QueryItem` to the query, first checking to see if it collides
    /// with any existing ranges or keys. All colliding items will be removed
    /// then merged together so that the query includes the minimum number of
    /// items (with no items covering any duplicate parts of keyspace) while
    /// still including every key or range that has been added to the query.
    ///
    /// Aggregate meta-variants (`AggregateCountOnRange`,
    /// `AggregateSumOnRange`, `AggregateCountAndSumOnRange`) are **never**
    /// range-merged: merging would erase the aggregate wrapper and silently
    /// turn the query into a plain range query. An exact structural
    /// duplicate is deduplicated; anything else that overlaps an aggregate
    /// item is kept as a separate item, producing a multi-item query that
    /// the `validate_aggregate_*` entry points reject at prove/execute time.
    pub fn insert_item(&mut self, mut item: QueryItem) {
        let item_is_aggregate = item.is_aggregate();

        self.items = self
            .items
            .iter()
            .filter_map(|our_item| {
                if our_item == &item {
                    // Exact structural duplicate (key, range, or aggregate):
                    // drop the existing copy, `item` replaces it below.
                    None
                } else if item_is_aggregate || our_item.is_aggregate() {
                    // Aggregate wrappers are not mergeable with anything —
                    // keep both items so the semantic conflict surfaces as a
                    // validation error instead of a silent wrapper drop.
                    Some(our_item.clone())
                } else if our_item.collides_with(&item) {
                    item.merge_assign(our_item);
                    None
                } else {
                    Some(our_item.clone()) // todo: manage this without a clone
                }
            })
            .collect();

        // Insert item at the correct sorted position.
        // `QueryItem::cmp` compares by covered range, so an aggregate item
        // and a plain item covering the same range are Ord-equal while being
        // kept as distinct items above; binary_search may therefore return
        // either Ok or Err. We use unwrap_or_else to extract the insertion
        // index from either variant without panicking.
        let pos = self.items.binary_search(&item).unwrap_or_else(|e| e);
        self.items.insert(pos, item);
    }

    /// Performs an insert_item on each item in the vector.
    pub fn insert_items(&mut self, items: Vec<QueryItem>) {
        for item in items {
            self.insert_item(item)
        }
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;

    mod insert_item {
        use super::*;

        #[test]
        fn test_insert_item_adds_only_one_key_for_equal_key_items() {
            let value = vec![
                3, 207, 99, 250, 114, 92, 207, 167, 120, 9, 236, 164, 124, 63, 102, 237, 201, 35,
                86, 5, 23, 169, 147, 150, 61, 132, 155, 33, 225, 145, 85, 138,
            ];

            let mut query = Query::new();

            query.insert_key(value.clone());
            query.insert_key(value.clone());

            assert_matches!(query.items.as_slice(), [QueryItem::Key(v)] if v == &value);
        }

        #[test]
        fn test_insert_key_into_aggregate_query_preserves_aggregate_wrapper() {
            // Regression test: inserting a key that falls inside the inner
            // range of an aggregate meta-item used to merge the two items,
            // silently dropping the aggregate wrapper and turning the query
            // into a plain range query.
            let mut query = Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ));

            query.insert_key(b"extra".to_vec());

            assert_matches!(
                query.items.as_slice(),
                [
                    QueryItem::AggregateCountAndSumOnRange(inner),
                    QueryItem::Key(k),
                ] if matches!(inner.as_ref(), QueryItem::Range(r) if r.start == b"a" && r.end == b"z")
                    && k == b"extra"
            );
            // The resulting malformed shape must be rejected downstream.
            assert!(query.validate_aggregate_count_and_sum_on_range().is_err());
        }

        #[test]
        fn test_insert_range_into_aggregate_count_query_preserves_aggregate_wrapper() {
            let mut query =
                Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"m".to_vec()));

            // Overlapping plain range must not be merged into the aggregate.
            query.insert_range(b"f".to_vec()..b"z".to_vec());

            assert_matches!(
                query.items.as_slice(),
                [
                    QueryItem::AggregateCountOnRange(_),
                    QueryItem::Range(r),
                ] if r.start == b"f" && r.end == b"z"
            );
            assert!(query.validate_aggregate_count_on_range().is_err());
        }

        #[test]
        fn test_insert_all_into_aggregate_sum_query_preserves_aggregate_wrapper() {
            let mut query = Query::new_aggregate_sum_on_range(QueryItem::RangeInclusive(
                b"a".to_vec()..=b"z".to_vec(),
            ));

            // RangeFull collides with everything; the aggregate must survive.
            query.insert_all();

            assert_matches!(
                query.items.as_slice(),
                [QueryItem::RangeFull(_), QueryItem::AggregateSumOnRange(_)]
                    | [QueryItem::AggregateSumOnRange(_), QueryItem::RangeFull(_)]
            );
            assert!(query.validate_aggregate_sum_on_range().is_err());
        }

        #[test]
        fn test_insert_aggregate_item_into_plain_query_keeps_both_items() {
            let mut query = Query::new();
            query.insert_range(b"a".to_vec()..b"z".to_vec());

            query.insert_item(QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
                b"b".to_vec()..b"c".to_vec(),
            ))));

            assert_matches!(
                query.items.as_slice(),
                [QueryItem::Range(_), QueryItem::AggregateSumOnRange(_)]
                    | [QueryItem::AggregateSumOnRange(_), QueryItem::Range(_)]
            );
            assert!(query.validate_aggregate_sum_on_range().is_err());
        }

        #[test]
        fn test_insert_identical_aggregate_item_twice_dedupes() {
            let aggregate = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            )));

            let mut query = Query::new();
            query.insert_item(aggregate.clone());
            query.insert_item(aggregate.clone());

            assert_eq!(query.items.as_slice(), [aggregate]);
        }

        #[test]
        fn test_insert_colliding_non_identical_aggregates_keeps_both() {
            // Two overlapping aggregates of the same kind stay separate; the
            // malformed multi-item shape is rejected by the validator instead
            // of being silently merged.
            let mut query =
                Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"m".to_vec()));

            query.insert_item(QueryItem::AggregateCountOnRange(Box::new(
                QueryItem::Range(b"f".to_vec()..b"z".to_vec()),
            )));

            assert_eq!(query.items.len(), 2);
            assert!(query
                .items
                .iter()
                .all(|item| item.is_aggregate_count_on_range()));
            assert!(query.validate_aggregate_count_on_range().is_err());
        }
    }
}
