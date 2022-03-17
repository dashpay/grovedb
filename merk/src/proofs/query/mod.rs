mod map;

use std::{
    cmp,
    cmp::{max, min, Ordering},
    collections::BTreeSet,
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

use anyhow::{bail, Result};
pub use map::*;
use storage::{rocksdb_storage::RawPrefixedTransactionalIterator, RawIterator};
#[cfg(feature = "full")]
use {super::Op, std::collections::LinkedList};

use super::{tree::execute, Decoder, Node};
use crate::tree::{Fetch, Hash, Link, RefWalker};

/// `Query` represents one or more keys or ranges of keys, which can be used to
/// resolve a proof which will include all of the requested values.
#[derive(Debug, Default, Clone)]
pub struct Query {
    items: BTreeSet<QueryItem>,
    pub subquery_key: Option<Vec<u8>>,
    pub subquery: Option<Box<Query>>,
    pub left_to_right: bool,
}

type ProofOffsetLimit = (LinkedList<Op>, (bool, bool), Option<u16>, Option<u16>);

impl Query {
    /// Creates a new query which contains no items.
    pub fn new() -> Self {
        Self::new_with_direction(true)
    }

    pub fn new_with_direction(left_to_right: bool) -> Self {
        Self {
            left_to_right,
            ..Self::default()
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &QueryItem> {
        self.items.iter()
    }

    pub fn rev_iter(&self) -> impl Iterator<Item = &QueryItem> {
        self.items.iter().rev()
    }

    /// Sets the subquery_key for the query. This causes every element that is
    /// returned by the query to be subqueried to the subquery_key.
    pub fn set_subquery_key(&mut self, key: Vec<u8>) {
        self.subquery_key = Some(key);
    }

    /// Sets the subquery for the query. This causes every element that is
    /// returned by the query to be subqueried or subqueried to the
    /// subquery_key/subquery if a subquery is present.
    pub fn set_subquery(&mut self, subquery: Self) {
        self.subquery = Some(Box::new(subquery));
    }

    /// Adds an individual key to the query, so that its value (or its absence)
    /// in the tree will be included in the resulting proof.
    ///
    /// If the key or a range including the key already exists in the query,
    /// this will have no effect. If the query already includes a range that has
    /// a non-inclusive bound equal to the key, the bound will be changed to be
    /// inclusive.
    pub fn insert_key(&mut self, key: Vec<u8>) {
        let key = QueryItem::Key(key);
        self.items.insert(key);
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
        while let Some(existing) = self.items.take(&item) {
            item = item.merge(existing);
        }

        self.items.insert(item);
    }
}

impl<Q: Into<QueryItem>> From<Vec<Q>> for Query {
    fn from(other: Vec<Q>) -> Self {
        let items = other.into_iter().map(Into::into).collect();
        Self {
            items,
            subquery_key: None,
            subquery: None,
            left_to_right: true,
        }
    }
}

impl From<Query> for Vec<QueryItem> {
    fn from(q: Query) -> Self {
        q.into_iter().collect()
    }
}

impl IntoIterator for Query {
    type IntoIter = <BTreeSet<QueryItem> as IntoIterator>::IntoIter;
    type Item = QueryItem;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

/// A `QueryItem` represents a key or range of keys to be included in a proof.
#[derive(Clone, Debug)]
pub enum QueryItem {
    Key(Vec<u8>),
    Range(Range<Vec<u8>>),
    RangeInclusive(RangeInclusive<Vec<u8>>),
    RangeFull(RangeFull),
    RangeFrom(RangeFrom<Vec<u8>>),
    RangeTo(RangeTo<Vec<u8>>),
    RangeToInclusive(RangeToInclusive<Vec<u8>>),
    RangeAfter(RangeFrom<Vec<u8>>),
    RangeAfterTo(Range<Vec<u8>>),
    RangeAfterToInclusive(RangeInclusive<Vec<u8>>),
}

impl QueryItem {
    pub fn lower_bound(&self) -> (Option<&[u8]>, bool) {
        match self {
            QueryItem::Key(key) => (Some(key.as_slice()), false),
            QueryItem::Range(range) => (Some(range.start.as_ref()), false),
            QueryItem::RangeInclusive(range) => (Some(range.start().as_ref()), false),
            QueryItem::RangeFull(_) => (None, false),
            QueryItem::RangeFrom(range) => (Some(range.start.as_ref()), false),
            QueryItem::RangeTo(_) => (None, false),
            QueryItem::RangeToInclusive(_) => (None, false),
            QueryItem::RangeAfter(range) => (Some(range.start.as_ref()), true),
            QueryItem::RangeAfterTo(range) => (Some(range.start.as_ref()), true),
            QueryItem::RangeAfterToInclusive(range) => (Some(range.start().as_ref()), true),
        }
    }

    pub const fn lower_unbounded(&self) -> bool {
        match self {
            QueryItem::Key(_) => false,
            QueryItem::Range(_) => false,
            QueryItem::RangeInclusive(_) => false,
            QueryItem::RangeFull(_) => true,
            QueryItem::RangeFrom(_) => false,
            QueryItem::RangeTo(_) => true,
            QueryItem::RangeToInclusive(_) => true,
            QueryItem::RangeAfter(_) => false,
            QueryItem::RangeAfterTo(_) => false,
            QueryItem::RangeAfterToInclusive(_) => false,
        }
    }

    pub fn upper_bound(&self) -> (Option<&[u8]>, bool) {
        match self {
            QueryItem::Key(key) => (Some(key.as_slice()), true),
            QueryItem::Range(range) => (Some(range.end.as_ref()), false),
            QueryItem::RangeInclusive(range) => (Some(range.end().as_ref()), true),
            QueryItem::RangeFull(_) => (None, true),
            QueryItem::RangeFrom(_) => (None, true),
            QueryItem::RangeTo(range) => (Some(range.end.as_ref()), false),
            QueryItem::RangeToInclusive(range) => (Some(range.end.as_ref()), true),
            QueryItem::RangeAfter(_) => (None, true),
            QueryItem::RangeAfterTo(range) => (Some(range.end.as_ref()), false),
            QueryItem::RangeAfterToInclusive(range) => (Some(range.end().as_ref()), true),
        }
    }

    pub const fn upper_unbounded(&self) -> bool {
        match self {
            QueryItem::Key(_) => false,
            QueryItem::Range(_) => false,
            QueryItem::RangeInclusive(_) => false,
            QueryItem::RangeFull(_) => true,
            QueryItem::RangeFrom(_) => true,
            QueryItem::RangeTo(_) => false,
            QueryItem::RangeToInclusive(_) => false,
            QueryItem::RangeAfter(_) => true,
            QueryItem::RangeAfterTo(_) => false,
            QueryItem::RangeAfterToInclusive(_) => false,
        }
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        let (lower_bound, lower_bound_non_inclusive) = self.lower_bound();
        let (upper_bound, upper_bound_inclusive) = self.upper_bound();
        (self.lower_unbounded()
            || Some(key) > lower_bound
            || (Some(key) == lower_bound && !lower_bound_non_inclusive))
            && (self.upper_unbounded()
                || Some(key) < upper_bound
                || (Some(key) == upper_bound && upper_bound_inclusive))
    }

    fn merge(self, other: Self) -> Self {
        // TODO: don't copy into new vecs
        let lower_unbounded = self.lower_unbounded() || other.lower_unbounded();
        let upper_unbounded = self.upper_unbounded() || other.upper_unbounded();

        let (start, start_non_inclusive) = min(self.lower_bound(), other.lower_bound());
        let (end, end_inclusive) = max(self.upper_bound(), other.upper_bound());

        if start_non_inclusive {
            return if upper_unbounded {
                Self::RangeAfter(RangeFrom {
                    start: start.expect("start should be bounded").to_vec(),
                })
            } else if end_inclusive {
                Self::RangeAfterToInclusive(RangeInclusive::new(
                    start.expect("start should be bounded").to_vec(),
                    end.expect("end should be bounded").to_vec(),
                ))
            } else {
                // upper is bounded and not inclusive
                Self::RangeAfterTo(Range {
                    start: start.expect("start should be bounded").to_vec(),
                    end: end.expect("end should be bounded").to_vec(),
                })
            };
        }

        if lower_unbounded {
            return if upper_unbounded {
                Self::RangeFull(RangeFull)
            } else if end_inclusive {
                Self::RangeToInclusive(RangeToInclusive {
                    end: end.expect("end should be bounded").to_vec(),
                })
            } else {
                // upper is bounded and not inclusive
                Self::RangeTo(RangeTo {
                    end: end.expect("end should be bounded").to_vec(),
                })
            };
        }

        // Lower is bounded
        if upper_unbounded {
            Self::RangeFrom(RangeFrom {
                start: start.expect("start should be bounded").to_vec(),
            })
        } else if end_inclusive {
            Self::RangeInclusive(RangeInclusive::new(
                start.expect("start should be bounded").to_vec(),
                end.expect("end should be bounded").to_vec(),
            ))
        } else {
            // upper is bounded and not inclusive
            Self::Range(Range {
                start: start.expect("start should be bounded").to_vec(),
                end: end.expect("end should be bounded").to_vec(),
            })
        }
    }

    pub const fn is_range(&self) -> bool {
        !matches!(self, QueryItem::Key(_))
    }

    pub fn seek_for_iter(&self, iter: &mut RawPrefixedTransactionalIterator, left_to_right: bool) {
        match self {
            QueryItem::Key(_) => {}
            QueryItem::Range(Range { start, end }) => {
                if left_to_right {
                    iter.seek(start);
                } else {
                    iter.seek(end);
                    iter.prev();
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                iter.seek(if left_to_right {
                    range_inclusive.start()
                } else {
                    range_inclusive.end()
                });
            }
            QueryItem::RangeFull(..) => {
                if left_to_right {
                    iter.seek_to_first();
                } else {
                    iter.seek_to_last();
                }
            }
            QueryItem::RangeFrom(RangeFrom { start }) => {
                if left_to_right {
                    iter.seek(start);
                } else {
                    iter.seek_to_last();
                }
            }
            QueryItem::RangeTo(RangeTo { end }) => {
                if left_to_right {
                    iter.seek_to_first();
                } else {
                    iter.seek(end);
                    iter.prev();
                }
            }
            QueryItem::RangeToInclusive(RangeToInclusive { end }) => {
                if left_to_right {
                    iter.seek_to_first();
                } else {
                    iter.seek_for_prev(end);
                }
            }
            QueryItem::RangeAfter(RangeFrom { start }) => {
                if left_to_right {
                    iter.seek(start);
                    // if the key is the same as start we should go to next
                    if let Some(key) = iter.key() {
                        if key == start {
                            iter.next()
                        }
                    }
                } else {
                    iter.seek_to_last();
                }
            }
            QueryItem::RangeAfterTo(Range { start, end }) => {
                if left_to_right {
                    iter.seek(start);
                    // if the key is the same as start we should go to next
                    if let Some(key) = iter.key() {
                        if key == start {
                            iter.next()
                        }
                    }
                } else {
                    iter.seek(end);
                    iter.prev();
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                if left_to_right {
                    let start = range_inclusive.start();
                    iter.seek(start);
                    // if the key is the same as start we should go to next
                    if let Some(key) = iter.key() {
                        if key == start {
                            iter.next();
                        }
                    }
                } else {
                    let end = range_inclusive.end();
                    iter.seek_for_prev(end);
                }
            }
        };
    }

    fn compare(a: &[u8], b: &[u8]) -> cmp::Ordering {
        for (ai, bi) in a.iter().zip(b.iter()) {
            match ai.cmp(bi) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }

        // if every single element was equal, compare length
        a.len().cmp(&b.len())
    }

    pub fn iter_is_valid_for_type(
        &self,
        iter: &RawPrefixedTransactionalIterator,
        limit: Option<u16>,
        left_to_right: bool,
    ) -> bool {
        match self {
            QueryItem::Key(_) => true,
            QueryItem::Range(Range { start, end }) => {
                let basic_valid =
                    (limit == None || limit.unwrap() > 0) && iter.valid() && iter.key().is_some();
                let valid = basic_valid
                    && if left_to_right {
                        iter.key() < Some(end)
                    } else {
                        iter.key() >= Some(start)
                    };
                valid
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                let basic_valid = iter.valid() && iter.key().is_some();
                let valid = basic_valid
                    && if left_to_right {
                        iter.key() <= Some(range_inclusive.end())
                    } else {
                        iter.key() >= Some(range_inclusive.start())
                    };
                valid
            }
            QueryItem::RangeFull(..) => {
                let valid =
                    (limit == None || limit.unwrap() > 0) && iter.valid() && iter.key().is_some();
                valid
            }
            QueryItem::RangeFrom(RangeFrom { start }) => {
                let basic_valid =
                    (limit == None || limit.unwrap() > 0) && iter.valid() && iter.key().is_some();
                let valid = basic_valid
                    && if left_to_right {
                        true
                    } else {
                        iter.key() >= Some(start)
                    };
                valid
            }
            QueryItem::RangeTo(RangeTo { end }) => {
                let basic_valid =
                    (limit == None || limit.unwrap() > 0) && iter.valid() && iter.key().is_some();
                let valid = basic_valid
                    && if left_to_right {
                        iter.key() < Some(end)
                    } else {
                        true
                    };
                valid
            }
            QueryItem::RangeToInclusive(RangeToInclusive { end }) => {
                let basic_valid =
                    (limit == None || limit.unwrap() > 0) && iter.valid() && iter.key().is_some();
                let valid = basic_valid
                    && if left_to_right {
                        iter.key() <= Some(end)
                    } else {
                        true
                    };
                valid
            }
            QueryItem::RangeAfter(RangeFrom { start }) => {
                let basic_valid =
                    (limit == None || limit.unwrap() > 0) && iter.valid() && iter.key().is_some();
                let valid = basic_valid
                    && if left_to_right {
                        true
                    } else {
                        iter.key() > Some(start)
                    };
                valid
            }
            QueryItem::RangeAfterTo(Range { start, end }) => {
                let basic_valid =
                    (limit == None || limit.unwrap() > 0) && iter.valid() && iter.key().is_some();
                let valid = basic_valid
                    && if left_to_right {
                        iter.key() < Some(end)
                    } else {
                        iter.key() > Some(start)
                    };
                valid
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                let basic_valid = (limit == None || limit.unwrap() > 0) && iter.valid();
                if !basic_valid {
                    return false;
                }
                let valid = match iter.key() {
                    None => false,
                    Some(key) => {
                        if left_to_right {
                            let end = range_inclusive.end().as_slice();
                            match Self::compare(key, end) {
                                Ordering::Less => true,
                                Ordering::Equal => true,
                                Ordering::Greater => false,
                            }
                        } else {
                            let start = range_inclusive.start().as_slice();
                            match Self::compare(key, start) {
                                Ordering::Less => false,
                                Ordering::Equal => false,
                                Ordering::Greater => true,
                            }
                        }
                    }
                };
                valid
            }
        }
    }
}

impl PartialEq for QueryItem {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl PartialEq<&[u8]> for QueryItem {
    fn eq(&self, other: &&[u8]) -> bool {
        matches!(self.partial_cmp(other), Some(Ordering::Equal))
    }
}

impl Eq for QueryItem {}

impl Ord for QueryItem {
    fn cmp(&self, other: &Self) -> Ordering {
        let cmp_lu = if self.lower_unbounded() {
            if other.lower_unbounded() {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        } else if other.lower_unbounded() {
            Ordering::Greater
        } else {
            // confirmed the bounds are not unbounded, hence safe to unwrap
            // as bound cannot be None
            self.lower_bound()
                .0
                .expect("should be bounded")
                .cmp(other.upper_bound().0.expect("should be bounded"))
        };

        let cmp_ul = if self.upper_unbounded() {
            if other.upper_unbounded() {
                Ordering::Equal
            } else {
                Ordering::Greater
            }
        } else if other.upper_unbounded() {
            Ordering::Less
        } else {
            // confirmed the bounds are not unbounded, hence safe to unwrap
            // as bound cannot be None
            self.upper_bound()
                .0
                .expect("should be bounded")
                .cmp(other.lower_bound().0.expect("should be bounded"))
        };

        let self_inclusive = self.upper_bound().1;
        let other_inclusive = other.upper_bound().1;

        match (cmp_lu, cmp_ul) {
            (Ordering::Less, Ordering::Less) => Ordering::Less,
            (Ordering::Less, Ordering::Equal) => match self_inclusive {
                true => Ordering::Equal,
                false => Ordering::Less,
            },
            (Ordering::Less, Ordering::Greater) => Ordering::Equal,
            (Ordering::Equal, _) => match other_inclusive {
                true => Ordering::Equal,
                false => Ordering::Greater,
            },
            (Ordering::Greater, _) => Ordering::Greater,
        }
    }
}

impl PartialOrd for QueryItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialOrd<&[u8]> for QueryItem {
    fn partial_cmp(&self, other: &&[u8]) -> Option<Ordering> {
        let other = Self::Key(other.to_vec());
        Some(self.cmp(&other))
    }
}

impl From<Vec<u8>> for QueryItem {
    fn from(key: Vec<u8>) -> Self {
        Self::Key(key)
    }
}

impl Link {
    /// Creates a `Node::Hash` from this link. Panics if the link is of variant
    /// `Link::Modified` since its hash has not yet been computed.
    #[cfg(feature = "full")]
    const fn to_hash_node(&self) -> Node {
        let hash = match self {
            Link::Reference { hash, .. } => hash,
            Link::Modified { .. } => {
                panic!("Cannot convert Link::Modified to proof hash node");
            }
            Link::Uncommitted { hash, .. } => hash,
            Link::Loaded { hash, .. } => hash,
        };
        Node::Hash(*hash)
    }
}

impl<'a, S> RefWalker<'a, S>
where
    S: Fetch + Sized + Clone,
{
    /// Creates a `Node::KV` from the key/value pair of the root node.
    pub(crate) fn to_kv_node(&self) -> Node {
        Node::KV(self.tree().key().to_vec(), self.tree().value().to_vec())
    }

    /// Creates a `Node::KVHash` from the hash of the key/value pair of the root
    /// node.
    pub(crate) fn to_kvhash_node(&self) -> Node {
        Node::KVHash(*self.tree().kv_hash())
    }

    /// Creates a `Node::KVDigest` from the key/value_hash pair of the root
    /// node.
    pub(crate) fn to_kvdigest_node(&self) -> Node {
        Node::KVDigest(self.tree().key().to_vec(), *self.tree().value_hash())
    }

    /// Creates a `Node::Hash` from the hash of the node.
    pub(crate) fn to_hash_node(&self) -> Node {
        Node::Hash(self.tree().hash())
    }

    #[cfg(feature = "full")]
    #[allow(dead_code)] // TODO: remove when proofs will be enabled
    pub(crate) fn create_full_proof(
        &mut self,
        query: &[QueryItem],
        limit: Option<u16>,
        offset: Option<u16>,
    ) -> Result<(LinkedList<Op>, (bool, bool))> {
        let (linked_list, (left, right), ..) = self.create_proof(query, limit, offset, true)?;
        Ok((linked_list, (left, right)))
    }

    /// Generates a proof for the list of queried keys. Returns a tuple
    /// containing the generated proof operators, and a tuple representing if
    /// any keys were queried were less than the left edge or greater than the
    /// right edge, respectively.
    #[cfg(feature = "full")]
    pub(crate) fn create_proof(
        &mut self,
        query: &[QueryItem],
        limit: Option<u16>,
        offset: Option<u16>,
        left_to_right: bool,
    ) -> Result<ProofOffsetLimit> {
        // TODO: don't copy into vec, support comparing QI to byte slice
        let node_key = QueryItem::Key(self.tree().key().to_vec());
        let mut search = query.binary_search_by(|key| key.cmp(&node_key));

        let mut current_node_in_query: bool;

        let (mut left_items, mut right_items) = match search {
            Ok(index) => {
                current_node_in_query = true;
                let item = &query[index];
                let left_bound = item.lower_bound().0;
                let right_bound = item.upper_bound().0;

                // if range starts before this node's key, include it in left
                // child's query
                let left_query = if left_bound == None || left_bound < Some(self.tree().key()) {
                    &query[..=index]
                } else {
                    &query[..index]
                };

                // if range ends after this node's key, include it in right
                // child's query
                let right_query = if right_bound == None || right_bound > Some(self.tree().key()) {
                    &query[index..]
                } else {
                    &query[index + 1..]
                };

                (left_query, right_query)
            }
            Err(index) => {
                current_node_in_query = false;
                (&query[..index], &query[index..])
            }
        };

        // when the limit hits zero, the rest of the query batch should be cleared
        // so empty the left, right query batch, and set the current node to not found
        if let Some(current_limit) = limit {
            if current_limit == 0 {
                left_items = &[];
                search = Err(Default::default());
                right_items = &[];
            }
        }

        let (mut proof, left_absence, mut new_limit, new_offset) =
            self.create_child_proof(true, left_items, limit, offset, left_to_right)?;

        if let Some(current_limit) = new_limit {
            // if after generating proof for the left subtree, the limit becomes 0
            // clear the current node and clear the right batch
            if current_limit == 0 {
                right_items = &[];
                search = Err(Default::default());
            } else if current_node_in_query {
                // if limit is not zero, reserve a limit slot for the current node
                // before generating proof for the right subtree
                new_limit = Some(current_limit - 1);
                // if after limit slot reservation, limit becomes 0, right query
                // should be cleared
                if current_limit - 1 == 0 {
                    right_items = &[];
                }
            }
        }

        let (mut right_proof, right_absence, new_limit, new_offset) =
            self.create_child_proof(false, right_items, new_limit, new_offset, left_to_right)?;

        let (has_left, has_right) = (!proof.is_empty(), !right_proof.is_empty());

        proof.push_back(match search {
            Ok(index) => {
                // if the current key is part of the non inclusive bounds of the query
                // push the KVDigest
                let item = &query[index];
                let (start_key, start_not_inclusive) = item.lower_bound();
                let (end_key, end_inclusive) = item.upper_bound();
                if start_key.is_some()
                    && start_key.unwrap() == self.tree().key()
                    && start_not_inclusive
                    || end_key.is_some() && end_key.unwrap() == self.tree().key() && !end_inclusive
                {
                    Op::Push(self.to_kvdigest_node())
                } else {
                    Op::Push(self.to_kv_node())
                }
            }
            Err(_) => {
                if left_absence.1 || right_absence.0 {
                    Op::Push(self.to_kvdigest_node())
                } else {
                    Op::Push(self.to_kvhash_node())
                }
            }
        });

        if has_left {
            proof.push_back(Op::Parent);
        }

        if has_right {
            proof.append(&mut right_proof);
            proof.push_back(Op::Child);
        }

        Ok((
            proof,
            (left_absence.0, right_absence.1),
            new_limit,
            new_offset,
        ))
    }

    /// Similar to `create_proof`. Recurses into the child on the given side and
    /// generates a proof for the queried keys.
    #[cfg(feature = "full")]
    fn create_child_proof(
        &mut self,
        left: bool,
        query: &[QueryItem],
        limit: Option<u16>,
        offset: Option<u16>,
        left_to_right: bool,
    ) -> Result<ProofOffsetLimit> {
        Ok(if !query.is_empty() {
            if let Some(mut child) = self.walk(left)? {
                child.create_proof(query, limit, offset, left_to_right)?
            } else {
                (LinkedList::new(), (true, true), limit, offset)
            }
        } else if let Some(link) = self.tree().link(left) {
            let mut proof = LinkedList::new();
            proof.push_back(Op::Push(link.to_hash_node()));
            (proof, (false, false), limit, offset)
        } else {
            (LinkedList::new(), (false, false), limit, offset)
        })
    }
}

pub fn verify(bytes: &[u8], expected_hash: Hash) -> Result<Map> {
    let ops = Decoder::new(bytes);
    let mut map_builder = MapBuilder::new();

    let root = execute(ops, true, |node| map_builder.insert(node))?;

    if root.hash() != expected_hash {
        bail!(
            "Proof did not match expected hash\n\tExpected: {:?}\n\tActual: {:?}",
            expected_hash,
            root.hash()
        );
    }

    Ok(map_builder.build())
}

pub fn execute_proof(bytes: &[u8]) -> Result<(Hash, Map)> {
    let ops = Decoder::new(bytes);
    let mut map_builder = MapBuilder::new();

    let root = execute(ops, true, |node| map_builder.insert(node))?;

    Ok((root.hash(), map_builder.build()))
}

/// Verifies the encoded proof with the given query and expected hash.
///
/// Every key in `keys` is checked to either have a key/value pair in the proof,
/// or to have its absence in the tree proven.
///
/// Returns `Err` if the proof is invalid, or a list of proven values associated
/// with `keys`. For example, if `keys` contains keys `A` and `B`, the returned
/// list will contain 2 elements, the value of `A` and the value of `B`. Keys
/// proven to be absent in the tree will have an entry of `None`, keys that have
/// a proven value will have an entry of `Some(value)`.
#[deprecated]
pub fn verify_query(
    bytes: &[u8],
    query: &Query,
    expected_hash: Hash,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut output = Vec::with_capacity(query.len());
    let mut last_push = None;
    let mut query = query.iter().peekable();
    let mut in_range = false;

    let ops = Decoder::new(bytes);

    let root = execute(ops, true, |node| {
        let mut execute_node = |key: &Vec<u8>, value: Option<&Vec<u8>>| -> Result<_> {
            while let Some(item) = query.peek() {
                // get next item in query
                let query_item = *item;
                let (lower_bound, start_non_inclusive) = query_item.lower_bound();

                // we have not reached next queried part of tree
                if *query_item > key.as_slice() {
                    // continue to next push
                    break;
                } else if start_non_inclusive
                    && lower_bound.is_some()
                    && lower_bound.unwrap() == key.as_slice()
                {
                    // we intersect with the query_item but at the start which is non inclusive
                    // continue to the next push
                    break;
                }

                if !in_range {
                    // this is the first data we have encountered for this query
                    // item. ensure lower bound of query item is proven
                    match last_push {
                        // lower bound is proven - we have an exact match
                        // ignoring the case when the lower bound is unbounded
                        // as it's not possible the get an exact key match for
                        // an unbounded value
                        _ if Some(key.as_slice()) == query_item.lower_bound().0 => {}

                        // lower bound is proven - this is the leftmost node
                        // in the tree
                        None => {}

                        // lower bound is proven - the preceding tree node
                        // is lower than the bound
                        Some(Node::KV(..)) => {}
                        Some(Node::KVDigest(..)) => {}

                        // cannot verify lower bound - we have an abridged
                        // tree so we cannot tell what the preceding key was
                        Some(_) => {
                            bail!("Cannot verify lower bound of queried range");
                        }
                    }
                }

                if query_item.upper_bound().0 != None
                    && Some(key.as_slice()) >= query_item.upper_bound().0
                {
                    // at or past upper bound of range (or this was an exact
                    // match on a single-key queryitem), advance to next query
                    // item
                    query.next();
                    in_range = false;
                } else {
                    // have not reached upper bound, we expect more values
                    // to be proven in the range (and all pushes should be
                    // unabridged until we reach end of range)
                    in_range = true;
                }

                // this push matches the queried item
                if query_item.contains(key) {
                    if let Some(val) = value {
                        // add data to output
                        output.push((key.clone(), val.clone()));

                        // continue to next push
                        break;
                    } else {
                        bail!("Proof is missing data for query");
                    }
                }
                // continue to next queried item
            }
            Ok(())
        };

        if let Node::KV(key, value) = node {
            execute_node(key, Some(value))?;
        } else if let Node::KVDigest(key, _) = node {
            execute_node(key, None)?;
        } else if in_range {
            // we encountered a queried range but the proof was abridged (saw a
            // non-KV push), we are missing some part of the range
            bail!("Proof is missing data for query");
        }

        last_push = Some(node.clone());

        Ok(())
    })?;

    // we have remaining query items, check absence proof against right edge of
    // tree
    if query.peek().is_some() {
        match last_push {
            // last node in tree was less than queried item
            Some(Node::KV(..)) => {}
            Some(Node::KVDigest(..)) => {}

            // proof contains abridged data so we cannot verify absence of
            // remaining query items
            _ => bail!("Proof is missing data for query"),
        }
    }

    if root.hash() != expected_hash {
        bail!(
            "Proof did not match expected hash\n\tExpected: {:?}\n\tActual: {:?}",
            expected_hash,
            root.hash()
        );
    }

    Ok(output)
}

#[allow(deprecated)]
#[cfg(test)]
mod test {
    use super::{
        super::{encoding::encode_into, *},
        *,
    };
    use crate::{
        proofs::query::QueryItem::RangeAfter,
        test_utils::make_tree_seq,
        tree::{NoopCommit, PanicSource, RefWalker, Tree},
    };

    fn make_3_node_tree() -> Tree {
        let mut tree = Tree::new(vec![5], vec![5])
            .attach(true, Some(Tree::new(vec![3], vec![3])))
            .attach(false, Some(Tree::new(vec![7], vec![7])));
        tree.commit(&mut NoopCommit {}).expect("commit failed");
        tree
    }

    fn make_6_node_tree() -> Tree {
        let two_tree = Tree::new(vec![2], vec![2]);
        let four_tree = Tree::new(vec![4], vec![4]);
        let mut three_tree = Tree::new(vec![3], vec![3])
            .attach(true, Some(two_tree))
            .attach(false, Some(four_tree));
        three_tree
            .commit(&mut NoopCommit {})
            .expect("commit failed");

        let seven_tree = Tree::new(vec![7], vec![7]);
        let mut eight_tree = Tree::new(vec![8], vec![8]).attach(true, Some(seven_tree));
        eight_tree
            .commit(&mut NoopCommit {})
            .expect("commit failed");

        let mut root_tree = Tree::new(vec![5], vec![5])
            .attach(true, Some(three_tree))
            .attach(false, Some(eight_tree));
        root_tree.commit(&mut NoopCommit {}).expect("commit failed");

        root_tree
    }

    fn verify_keys_test(keys: Vec<Vec<u8>>, expected_result: Vec<Option<Vec<u8>>>) {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, _) = walker
            .create_full_proof(
                keys.clone()
                    .into_iter()
                    .map(QueryItem::Key)
                    .collect::<Vec<_>>()
                    .as_slice(),
                None,
                None,
            )
            .expect("failed to create proof");
        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let expected_hash = [
            148, 227, 127, 84, 149, 54, 117, 188, 32, 85, 176, 25, 96, 127, 170, 90, 148, 196, 218,
            30, 5, 109, 112, 3, 120, 138, 194, 28, 27, 49, 119, 125,
        ];

        let mut query = Query::new();
        for key in keys.iter() {
            query.insert_key(key.clone());
        }

        let result = verify_query(bytes.as_slice(), &query, expected_hash).expect("verify failed");

        let mut values = std::collections::HashMap::new();
        for (key, value) in result {
            assert!(values.insert(key, value).is_none());
        }

        for (key, expected_value) in keys.iter().zip(expected_result.iter()) {
            assert_eq!(values.get(key), expected_value.as_ref());
        }
    }

    #[test]
    fn root_verify() {
        verify_keys_test(vec![vec![5]], vec![Some(vec![5])]);
    }

    #[test]
    fn single_verify() {
        verify_keys_test(vec![vec![3]], vec![Some(vec![3])]);
    }

    #[test]
    fn double_verify() {
        verify_keys_test(vec![vec![3], vec![5]], vec![Some(vec![3]), Some(vec![5])]);
    }

    #[test]
    fn double_verify_2() {
        verify_keys_test(vec![vec![3], vec![7]], vec![Some(vec![3]), Some(vec![7])]);
    }

    #[test]
    fn triple_verify() {
        verify_keys_test(
            vec![vec![3], vec![5], vec![7]],
            vec![Some(vec![3]), Some(vec![5]), Some(vec![7])],
        );
    }

    #[test]
    fn left_edge_absence_verify() {
        verify_keys_test(vec![vec![2]], vec![None]);
    }

    #[test]
    fn right_edge_absence_verify() {
        verify_keys_test(vec![vec![8]], vec![None]);
    }

    #[test]
    fn inner_absence_verify() {
        verify_keys_test(vec![vec![6]], vec![None]);
    }

    #[test]
    fn absent_and_present_verify() {
        verify_keys_test(vec![vec![5], vec![6]], vec![Some(vec![5]), None]);
    }

    #[test]
    fn node_variant_conversion() {
        let mut tree = make_6_node_tree();
        let walker = RefWalker::new(&mut tree, PanicSource {});

        assert_eq!(walker.to_kv_node(), Node::KV(vec![5], vec![5]));
        assert_eq!(
            walker.to_kvhash_node(),
            Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])
        );
        assert_eq!(
            walker.to_kvdigest_node(),
            Node::KVDigest(
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            ),
        );
        assert_eq!(
            walker.to_hash_node(),
            Node::Hash([
                47, 88, 45, 83, 28, 53, 123, 233, 238, 140, 130, 174, 250, 220, 210, 37, 3, 215,
                82, 177, 190, 30, 154, 156, 35, 214, 144, 79, 40, 41, 218, 142
            ])
        );
    }

    #[test]
    fn empty_proof() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, absence) = walker
            .create_full_proof(vec![].as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84,
                143, 196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let res = verify_query(bytes.as_slice(), &Query::new(), tree.hash()).unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn root_proof() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::Key(vec![5])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84,
                143, 196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(res, vec![(vec![5], vec![5])]);
    }

    #[test]
    fn leaf_proof() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::Key(vec![3])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84,
                143, 196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(res, vec![(vec![3], vec![3])]);
    }

    #[test]
    fn double_leaf_proof() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::Key(vec![3]), QueryItem::Key(vec![7])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![7], vec![7]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(res, vec![(vec![3], vec![3]), (vec![7], vec![7]),]);
    }

    #[test]
    fn all_nodes_proof() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![
            QueryItem::Key(vec![3]),
            QueryItem::Key(vec![5]),
            QueryItem::Key(vec![7]),
        ];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![7], vec![7]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![(vec![3], vec![3]), (vec![5], vec![5]), (vec![7], vec![7]),]
        );
    }

    #[test]
    fn global_edge_absence_proof() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::Key(vec![8])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(res, vec![]);
    }

    #[test]
    fn absence_proof() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::Key(vec![6])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(res, vec![]);
    }

    #[test]
    fn doc_proof() {
        let mut tree = Tree::new(vec![5], vec![5])
            .attach(
                true,
                Some(
                    Tree::new(vec![2], vec![2])
                        .attach(true, Some(Tree::new(vec![1], vec![1])))
                        .attach(
                            false,
                            Some(
                                Tree::new(vec![4], vec![4])
                                    .attach(true, Some(Tree::new(vec![3], vec![3]))),
                            ),
                        ),
                ),
            )
            .attach(
                false,
                Some(
                    Tree::new(vec![9], vec![9])
                        .attach(
                            true,
                            Some(
                                Tree::new(vec![7], vec![7])
                                    .attach(true, Some(Tree::new(vec![6], vec![6])))
                                    .attach(false, Some(Tree::new(vec![8], vec![8]))),
                            ),
                        )
                        .attach(
                            false,
                            Some(
                                Tree::new(vec![11], vec![11])
                                    .attach(true, Some(Tree::new(vec![10], vec![10]))),
                            ),
                        ),
                ),
            );
        tree.commit(&mut NoopCommit {}).unwrap();

        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![
            QueryItem::Key(vec![1]),
            QueryItem::Key(vec![2]),
            QueryItem::Key(vec![3]),
            QueryItem::Key(vec![4]),
        ];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![1], vec![1]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![2], vec![2]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![4], vec![4]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                12, 156, 232, 212, 220, 65, 226, 32, 91, 101, 248, 64, 225, 206, 63, 12, 153, 191,
                183, 10, 233, 251, 249, 76, 184, 200, 88, 57, 219, 2, 250, 113
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        assert_eq!(
            bytes,
            vec![
                3, 1, 1, 0, 1, 1, 3, 1, 2, 0, 1, 2, 16, 3, 1, 3, 0, 1, 3, 3, 1, 4, 0, 1, 4, 16, 17,
                2, 61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52,
                238, 68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197, 16, 1, 12, 156,
                232, 212, 220, 65, 226, 32, 91, 101, 248, 64, 225, 206, 63, 12, 153, 191, 183, 10,
                233, 251, 249, 76, 184, 200, 88, 57, 219, 2, 250, 113, 17
            ]
        );

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![1], vec![1]),
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
            ]
        );
    }

    #[test]
    fn query_item_cmp() {
        assert!(QueryItem::Key(vec![10]) < QueryItem::Key(vec![20]));
        assert_eq!(QueryItem::Key(vec![10]), QueryItem::Key(vec![10]));
        assert!(QueryItem::Key(vec![20]) > QueryItem::Key(vec![10]));

        assert!(QueryItem::Key(vec![10]) < QueryItem::Range(vec![20]..vec![30]));
        assert_eq!(
            QueryItem::Key(vec![10]),
            QueryItem::Range(vec![10]..vec![20])
        );
        assert_eq!(
            QueryItem::Key(vec![15]),
            QueryItem::Range(vec![10]..vec![20])
        );
        assert!(QueryItem::Key(vec![20]) > QueryItem::Range(vec![10]..vec![20]));
        assert_eq!(
            QueryItem::Key(vec![20]),
            QueryItem::RangeInclusive(vec![10]..=vec![20])
        );
        assert!(QueryItem::Key(vec![30]) > QueryItem::Range(vec![10]..vec![20]));

        assert!(QueryItem::Range(vec![10]..vec![20]) < QueryItem::Range(vec![30]..vec![40]));
        assert!(QueryItem::Range(vec![10]..vec![20]) < QueryItem::Range(vec![20]..vec![30]));
        assert_eq!(
            QueryItem::RangeInclusive(vec![10]..=vec![20]),
            QueryItem::Range(vec![20]..vec![30])
        );
        assert_eq!(
            QueryItem::Range(vec![15]..vec![25]),
            QueryItem::Range(vec![20]..vec![30])
        );
        assert!(QueryItem::Range(vec![20]..vec![30]) > QueryItem::Range(vec![10]..vec![20]));
    }

    #[test]
    fn query_item_merge() {
        let mine = QueryItem::Range(vec![10]..vec![30]);
        let other = QueryItem::Range(vec![15]..vec![20]);
        assert_eq!(mine.merge(other), QueryItem::Range(vec![10]..vec![30]));

        let mine = QueryItem::RangeInclusive(vec![10]..=vec![30]);
        let other = QueryItem::Range(vec![20]..vec![30]);
        assert_eq!(
            mine.merge(other),
            QueryItem::RangeInclusive(vec![10]..=vec![30])
        );

        let mine = QueryItem::Key(vec![5]);
        let other = QueryItem::Range(vec![1]..vec![10]);
        assert_eq!(mine.merge(other), QueryItem::Range(vec![1]..vec![10]));

        let mine = QueryItem::Key(vec![10]);
        let other = QueryItem::RangeInclusive(vec![1]..=vec![10]);
        assert_eq!(
            mine.merge(other),
            QueryItem::RangeInclusive(vec![1]..=vec![10])
        );
    }

    #[test]
    fn query_insert() {
        let mut query = Query::new();
        query.insert_key(vec![2]);
        query.insert_range(vec![3]..vec![5]);
        query.insert_range_inclusive(vec![5]..=vec![7]);
        query.insert_range(vec![4]..vec![6]);
        query.insert_key(vec![5]);

        let mut iter = query.items.iter();
        assert_eq!(format!("{:?}", iter.next()), "Some(Key([2]))");
        assert_eq!(
            format!("{:?}", iter.next()),
            "Some(RangeInclusive([3]..=[7]))"
        );
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn range_proof() {
        let mut tree = make_tree_seq(10);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            ]
        );
    }

    #[test]
    fn range_proof_inclusive() {
        let mut tree = make_tree_seq(10);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeInclusive(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..=vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
            ]
        );
    }

    #[test]
    fn range_from_proof() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                85, 217, 56, 226, 204, 53, 103, 145, 201, 33, 178, 80, 207, 194, 104, 128, 199,
                145, 156, 208, 152, 255, 209, 24, 140, 222, 204, 193, 211, 26, 118, 58
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![7], vec![7]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![8], vec![8]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![(vec![5], vec![5]), (vec![7], vec![7]), (vec![8], vec![8])]
        );

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), Some(1), None)
            .expect("create_proof errored");

        let equivalent_queryitems = vec![QueryItem::Key(vec![5])];
        let (equivalent_proof, equivalent_absence) = walker
            .create_full_proof(equivalent_queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let res = verify(bytes.as_slice(), tree.hash()).unwrap();
        let mut iter = res.all();
        assert_eq!(iter.next(), Some((&vec![5], &(false, vec![5]))));
        assert_eq!(iter.next(), None,);

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), Some(2), None)
            .expect("create_proof errored");

        let equivalent_queryitems = vec![
            QueryItem::Key(vec![5]),
            QueryItem::Key(vec![6]),
            QueryItem::Key(vec![7]),
        ];
        let (equivalent_proof, equivalent_absence) = walker
            .create_full_proof(equivalent_queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let res = verify(bytes.as_slice(), tree.hash()).unwrap();
        let mut iter = res.all();
        assert_eq!(iter.next(), Some((&vec![5], &(false, vec![5]))));
        assert_eq!(iter.next(), Some((&vec![7], &(true, vec![7]))),);
        assert_eq!(iter.next(), None,);

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), Some(100), None)
            .expect("create_proof errored");

        let equivalent_queryitems = vec![QueryItem::RangeFrom(vec![5]..)];
        let (equivalent_proof, equivalent_absence) = walker
            .create_full_proof(equivalent_queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let res = verify(bytes.as_slice(), tree.hash()).unwrap();
        let mut iter = res.all();
        assert_eq!(iter.next(), Some((&vec![5], &(false, vec![5]))));
        assert_eq!(iter.next(), Some((&vec![7], &(true, vec![7]))),);
        assert_eq!(iter.next(), Some((&vec![8], &(true, vec![8]))),);
    }

    #[test]
    fn range_to_proof() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeTo(..vec![6])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![2], vec![2]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![4], vec![4]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
            ]
        );
    }

    #[test]
    fn range_to_proof_inclusive() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![2], vec![2]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![4], vec![4]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
            ]
        );
    }

    #[test]
    fn range_after_proof() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![RangeAfter(vec![3]..)];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
                241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![4], vec![4]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![7], vec![7]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![8], vec![8]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![4], vec![4]),
                (vec![5], vec![5]),
                (vec![7], vec![7]),
                (vec![8], vec![8]),
            ]
        );
    }

    #[test]
    fn range_after_to_proof() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
                241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![4], vec![4]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(res, vec![(vec![4], vec![4]), (vec![5], vec![5]),]);
    }

    #[test]
    fn range_after_to_proof_inclusive() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        (
            iter.next(),
            Some(&Op::Push(Node::Hash([
                121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
                241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30,
            ]))),
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![4], vec![4]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![7], vec![7]))));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![(vec![4], vec![4]), (vec![5], vec![5]), (vec![7], vec![7])]
        );
    }

    #[test]
    fn range_full_proof() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeFull(..)];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![2], vec![2]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![4], vec![4]))));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![5], vec![5]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![7], vec![7]))));
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![8], vec![8]))));
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));

        assert!(iter.next().is_none());
        assert_eq!(absence, (true, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
                (vec![7], vec![7]),
                (vec![8], vec![8]),
            ]
        );
    }

    #[test]
    fn proof_with_limit() {
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::RangeFrom(vec![2]..)];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), Some(1), None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![2], vec![2]))));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                126, 128, 159, 241, 207, 26, 88, 61, 163, 18, 218, 189, 45, 220, 124, 96, 118, 68,
                61, 95, 230, 75, 145, 218, 178, 227, 63, 137, 79, 153, 182, 12
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                56, 181, 68, 232, 233, 83, 180, 104, 74, 123, 143, 25, 174, 80, 132, 201, 61, 108,
                131, 89, 204, 90, 128, 199, 164, 25, 3, 146, 39, 127, 12, 105
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                133, 188, 175, 131, 60, 89, 221, 135, 133, 53, 205, 110, 58, 56, 128, 58, 1, 227,
                75, 122, 83, 20, 125, 44, 149, 44, 62, 130, 252, 134, 105, 200
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify(bytes.as_slice(), tree.hash()).unwrap();
        let mut m = res.all();
        assert_eq!(m.next(), Some((&vec![2], &(true, vec![2]))));
        assert_eq!(m.next(), None);
    }

    #[test]
    fn range_proof_missing_upper_bound() {
        let mut tree = make_tree_seq(10);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 6, 5],
        )];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(
            res,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            ]
        );
    }

    #[test]
    fn range_proof_missing_lower_bound() {
        let mut tree = make_tree_seq(10);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let queryitems = vec![
            // 7 is not inclusive
            QueryItem::Range(vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7]),
        ];
        let (proof, absence) = walker
            .create_full_proof(queryitems.as_slice(), None, None)
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KV(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in queryitems {
            query.insert_item(item);
        }
        let res = verify_query(bytes.as_slice(), &query, tree.hash()).unwrap();
        assert_eq!(res, vec![(vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),]);
    }

    #[test]
    fn query_from_vec() {
        let queryitems = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let query = Query::from(queryitems);

        let mut expected = BTreeSet::new();
        expected.insert(QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        ));
        assert_eq!(query.items, expected);
    }

    #[test]
    fn query_into_vec() {
        let mut query = Query::new();
        query.insert_item(QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        ));
        let query_vec: Vec<QueryItem> = query.into();
        let expected = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        assert_eq!(
            query_vec.get(0).unwrap().lower_bound(),
            expected.get(0).unwrap().lower_bound()
        );
        assert_eq!(
            query_vec.get(0).unwrap().upper_bound(),
            expected.get(0).unwrap().upper_bound()
        );
    }

    #[test]
    fn query_item_from_vec_u8() {
        let queryitems: Vec<u8> = vec![42];
        let query = QueryItem::from(queryitems);

        let expected = QueryItem::Key(vec![42]);
        assert_eq!(query, expected);
    }

    #[test]
    fn verify_ops() {
        let mut tree = Tree::new(vec![5], vec![5]);
        tree.commit(&mut NoopCommit {}).expect("commit failed");

        let root_hash = tree.hash();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, _) = walker
            .create_full_proof(vec![QueryItem::Key(vec![5])].as_slice(), None, None)
            .expect("failed to create proof");
        let mut bytes = vec![];

        encode_into(proof.iter(), &mut bytes);

        let map = verify(&bytes, root_hash).unwrap();
        assert_eq!(
            map.get(vec![5].as_slice()).unwrap().unwrap(),
            vec![5].as_slice()
        );
    }

    #[test]
    #[should_panic(expected = "verify failed")]
    fn verify_ops_mismatched_hash() {
        let mut tree = Tree::new(vec![5], vec![5]);
        tree.commit(&mut NoopCommit {}).expect("commit failed");

        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, _) = walker
            .create_full_proof(vec![QueryItem::Key(vec![5])].as_slice(), None, None)
            .expect("failed to create proof");
        let mut bytes = vec![];

        encode_into(proof.iter(), &mut bytes);

        let _map = verify(&bytes, [42; 32]).expect("verify failed");
    }

    #[test]
    #[should_panic(expected = "verify failed")]
    fn verify_query_mismatched_hash() {
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});
        let keys = vec![vec![5], vec![7]];
        let (proof, _) = walker
            .create_full_proof(
                keys.clone()
                    .into_iter()
                    .map(QueryItem::Key)
                    .collect::<Vec<_>>()
                    .as_slice(),
                None,
                None,
            )
            .expect("failed to create proof");
        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let mut query = Query::new();
        for key in keys.iter() {
            query.insert_key(key.clone());
        }

        let _result = verify_query(bytes.as_slice(), &query, [42; 32]).expect("verify failed");
    }
}
