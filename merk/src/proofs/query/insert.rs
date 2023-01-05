use std::{
    borrow::BorrowMut,
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

use crate::proofs::{query::query_item::QueryItem, Query};

#[cfg(any(feature = "full", feature = "verify"))]
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
    /// All other items in the query will be discarded as you are now getting
    /// back all elements.
    pub fn insert_all(&mut self) {
        let range = QueryItem::RangeFull(RangeFull);
        self.insert_item(range);
    }

    /// Adds the `QueryItem` to the query, first checking to see if it collides
    /// with any existing ranges or keys. All colliding items will be removed
    /// then merged together so that the query includes the minimum number of
    /// items (with no items covering any duplicate parts of keyspace) while
    /// still including every key or range that has been added to the query.
    pub fn insert_item(&mut self, mut item: QueryItem) {
        // since `QueryItem::eq` considers items equal if they collide at all
        // (including keys within ranges or ranges which partially overlap),
        // `items.take` will remove the first item which collides

        self.items = self
            .items
            .iter()
            .filter_map(|our_item| {
                if our_item.collides_with(&item) {
                    item.merge_assign(&our_item);
                    None
                } else {
                    Some(our_item.clone()) // todo: manage this without a clone
                }
            })
            .collect();

        self.items.insert(item);
    }
}
