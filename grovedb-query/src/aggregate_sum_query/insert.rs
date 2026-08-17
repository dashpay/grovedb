use std::ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive};

use super::AggregateSumQuery;
use crate::QueryItem;

impl AggregateSumQuery {
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
    /// turn the item into a plain range. An exact structural duplicate is
    /// deduplicated; anything else that overlaps an aggregate item is kept
    /// as a separate item. (Aggregate meta-variants are not valid
    /// `AggregateSumQuery` items in the first place, so this only preserves
    /// the invalid shape for downstream rejection instead of laundering it
    /// into a valid-looking plain range.)
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
                    // keep both items instead of silently dropping the
                    // wrapper.
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
