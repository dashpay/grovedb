#[cfg(any(feature = "full", feature = "verify"))]
mod merge;
// TODO: potentially rename
// mod intersect;
pub mod intersect2;

use std::{
    cmp,
    cmp::{max, min, Ordering},
    hash::Hash,
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

use costs::{CostContext, CostsExt, OperationCost};
use storage::RawIterator;

use crate::Error;

#[cfg(any(feature = "full", feature = "verify"))]
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

#[cfg(any(feature = "full", feature = "verify"))]
impl Hash for QueryItem {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.enum_value().hash(state);
        self.value_hash(state);
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl QueryItem {
    pub fn processing_footprint(&self) -> u32 {
        match self {
            QueryItem::Key(key) => key.len() as u32,
            QueryItem::RangeFull(_) => 0u32,
            _ => {
                self.lower_bound().0.map_or(0u32, |x| x.len() as u32)
                    + self.upper_bound().0.map_or(0u32, |x| x.len() as u32)
            }
        }
    }

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

    fn enum_value(&self) -> u32 {
        match self {
            QueryItem::Key(_) => 0,
            QueryItem::Range(_) => 1,
            QueryItem::RangeInclusive(_) => 2,
            QueryItem::RangeFull(_) => 3,
            QueryItem::RangeFrom(_) => 4,
            QueryItem::RangeTo(_) => 5,
            QueryItem::RangeToInclusive(_) => 6,
            QueryItem::RangeAfter(_) => 7,
            QueryItem::RangeAfterTo(_) => 8,
            QueryItem::RangeAfterToInclusive(_) => 9,
        }
    }

    fn value_hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            QueryItem::Key(key) => key.hash(state),
            QueryItem::Range(range) => range.hash(state),
            QueryItem::RangeInclusive(range) => range.hash(state),
            QueryItem::RangeFull(range) => range.hash(state),
            QueryItem::RangeFrom(range) => range.hash(state),
            QueryItem::RangeTo(range) => range.hash(state),
            QueryItem::RangeToInclusive(range) => range.hash(state),
            QueryItem::RangeAfter(range) => range.hash(state),
            QueryItem::RangeAfterTo(range) => range.hash(state),
            QueryItem::RangeAfterToInclusive(range) => range.hash(state),
        }
    }

    pub const fn is_key(&self) -> bool {
        matches!(self, QueryItem::Key(_))
    }

    pub const fn is_range(&self) -> bool {
        !matches!(self, QueryItem::Key(_))
    }

    pub const fn is_unbounded_range(&self) -> bool {
        !matches!(
            self,
            QueryItem::Key(_) | QueryItem::Range(_) | QueryItem::RangeInclusive(_)
        )
    }

    pub fn keys(&self) -> Result<Vec<Vec<u8>>, Error> {
        match self {
            QueryItem::Key(key) => Ok(vec![key.clone()]),
            QueryItem::Range(Range { start, end }) => {
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                let start = range_inclusive.start();
                let end = range_inclusive.end();
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..=end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            _ => Err(Error::InvalidOperation(
                "distinct keys are not available for unbounded ranges",
            )),
        }
    }

    pub fn keys_consume(self) -> Result<Vec<Vec<u8>>, Error> {
        match self {
            QueryItem::Key(key) => Ok(vec![key]),
            QueryItem::Range(Range { start, end }) => {
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                let start = range_inclusive.start();
                let end = range_inclusive.end();
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..=end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            _ => Err(Error::InvalidOperation(
                "distinct keys are not available for unbounded ranges",
            )),
        }
    }

    pub fn seek_for_iter<I: RawIterator>(
        &self,
        iter: &mut I,
        left_to_right: bool,
    ) -> CostContext<()> {
        match self {
            QueryItem::Key(start) => iter.seek(start),
            QueryItem::Range(Range { start, end }) => {
                if left_to_right {
                    iter.seek(start)
                } else {
                    iter.seek(end).flat_map(|_| iter.prev())
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => iter.seek(if left_to_right {
                range_inclusive.start()
            } else {
                range_inclusive.end()
            }),
            QueryItem::RangeFull(..) => {
                if left_to_right {
                    iter.seek_to_first()
                } else {
                    iter.seek_to_last()
                }
            }
            QueryItem::RangeFrom(RangeFrom { start }) => {
                if left_to_right {
                    iter.seek(start)
                } else {
                    iter.seek_to_last()
                }
            }
            QueryItem::RangeTo(RangeTo { end }) => {
                if left_to_right {
                    iter.seek_to_first()
                } else {
                    iter.seek(end).flat_map(|_| iter.prev())
                }
            }
            QueryItem::RangeToInclusive(RangeToInclusive { end }) => {
                if left_to_right {
                    iter.seek_to_first()
                } else {
                    iter.seek_for_prev(end)
                }
            }
            QueryItem::RangeAfter(RangeFrom { start }) => {
                if left_to_right {
                    let mut cost = OperationCost::default();
                    iter.seek(start).unwrap_add_cost(&mut cost);
                    if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                        // if the key is the same as start we should go to next
                        if key == start {
                            iter.next().unwrap_add_cost(&mut cost)
                        }
                    }
                    ().wrap_with_cost(cost)
                } else {
                    iter.seek_to_last()
                }
            }
            QueryItem::RangeAfterTo(Range { start, end }) => {
                if left_to_right {
                    let mut cost = OperationCost::default();
                    iter.seek(start).unwrap_add_cost(&mut cost);
                    if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                        // if the key is the same as start we тshould go to next
                        if key == start {
                            iter.next().unwrap_add_cost(&mut cost);
                        }
                    }
                    ().wrap_with_cost(cost)
                } else {
                    iter.seek(end).flat_map(|_| iter.prev())
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                if left_to_right {
                    let mut cost = OperationCost::default();
                    let start = range_inclusive.start();
                    iter.seek(start).unwrap_add_cost(&mut cost);
                    if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                        // if the key is the same as start we тshould go to next
                        if key == start {
                            iter.next().unwrap_add_cost(&mut cost);
                        }
                    }
                    ().wrap_with_cost(cost)
                } else {
                    let end = range_inclusive.end();
                    iter.seek_for_prev(end)
                }
            }
        }
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

    pub fn iter_is_valid_for_type<I: RawIterator>(
        &self,
        iter: &I,
        limit: Option<u16>,
        left_to_right: bool,
    ) -> CostContext<bool> {
        let mut cost = OperationCost::default();

        // Check that if limit is set it's greater than 0 and iterator points to a valid
        // place.
        let basic_valid =
            limit.map(|l| l > 0).unwrap_or(true) && iter.valid().unwrap_add_cost(&mut cost);

        if !basic_valid {
            return false.wrap_with_cost(cost);
        }

        // Key should also be something, otherwise terminate early.
        let key = if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
            key
        } else {
            return false.wrap_with_cost(cost);
        };

        let is_valid = match self {
            QueryItem::Key(start) => key == start,
            QueryItem::Range(Range { start, end }) => {
                if left_to_right {
                    key < end
                } else {
                    key >= start
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                if left_to_right {
                    key <= range_inclusive.end()
                } else {
                    key >= range_inclusive.start()
                }
            }
            QueryItem::RangeFull(..) => {
                true // requires only basic validation which is done above
            }
            QueryItem::RangeFrom(RangeFrom { start }) => left_to_right || key >= start,
            QueryItem::RangeTo(RangeTo { end }) => !left_to_right || key < end,
            QueryItem::RangeToInclusive(RangeToInclusive { end }) => !left_to_right || key <= end,
            QueryItem::RangeAfter(RangeFrom { start }) => left_to_right || key > start,
            QueryItem::RangeAfterTo(Range { start, end }) => {
                if left_to_right {
                    key < end
                } else {
                    key > start
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
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

        is_valid.wrap_with_cost(cost)
    }

    pub fn collides_with(&self, other: &Self) -> bool {
        self.intersect(other).in_both.is_some()
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl PartialEq for QueryItem {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl PartialEq<&[u8]> for QueryItem {
    fn eq(&self, other: &&[u8]) -> bool {
        matches!(self.partial_cmp(other), Some(Ordering::Equal))
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl Eq for QueryItem {}

#[cfg(any(feature = "full", feature = "verify"))]
impl Ord for QueryItem {
    fn cmp(&self, other: &Self) -> Ordering {
        match (
            self.lower_unbounded(),
            other.lower_unbounded(),
            self.upper_unbounded(),
            other.upper_unbounded(),
        ) {
            // all unbounded, both are range all
            (true, true, true, true) => Ordering::Equal,
            // we are unbounded at the bottom, they are not
            (true, false, true, true)
            | (true, false, false, true)
            | (true, false, false, false)
            | (true, false, true, false) => Ordering::Less,
            // they are unbounded at the bottom, we are not
            (false, true, true, true)
            | (false, true, false, true)
            | (false, true, false, false)
            | (false, true, true, false) => Ordering::Greater,
            // we are both unbounded at the beginning
            // we are unbounded at the top, they are not (they are smaller)
            // since they are smaller we are greater than them
            (true, true, true, false) => Ordering::Greater,
            // we are bounded at the top, they are unbounded (they are bigger)
            // since they are bigger we are less than them
            (true, true, false, true) => Ordering::Less,
            // we are both bounded at the top
            (true, true, false, false) => {
                match self
                    .upper_bound()
                    .0
                    .expect("upper bound left should be bounded")
                    .cmp(
                        other
                            .upper_bound()
                            .0
                            .expect("upper bound right should be bounded"),
                    ) {
                    // for example we have our upper bound at 5, they have it at 6
                    // we are smaller than them
                    Ordering::Less => Ordering::Less,
                    Ordering::Equal => {
                        // check inclusiveness
                        // for example we have our upper bound at 5 excluded (false)
                        // they have it at 5 included (true)
                        // we are smaller than them
                        self.upper_bound().1.cmp(&other.upper_bound().1)
                    }
                    // for example we have our upper bound at 7, they have it at 6
                    // we are bigger than them
                    Ordering::Greater => Ordering::Greater,
                }
            }
            // we are both bounded at the beginning
            (false, false, true, true)
            | (false, false, false, true)
            | (false, false, false, false)
            | (false, false, true, false) => {
                match self
                    .lower_bound()
                    .0
                    .expect("lower bound left should be bounded")
                    .cmp(
                        other
                            .lower_bound()
                            .0
                            .expect("lower bound right should be bounded"),
                    ) {
                    Ordering::Less => Ordering::Less,
                    Ordering::Equal => {
                        match self.lower_bound().1.cmp(&other.lower_bound().1) {
                            // true means excluded
                            // less means:
                            // ours excluded false
                            // theirs excluded true
                            // ours : [3, 4, 5, 6]
                            // theirs: [4, 5, 6, 8]
                            // ours here is less
                            Ordering::Less => Ordering::Less,
                            Ordering::Equal => {
                                // lower bounds were equal
                                match (self.upper_unbounded(), other.upper_unbounded()) {
                                    // both unbounded, equal
                                    (true, true) => Ordering::Equal,
                                    // they are unbounded at the top, they are bigger than us
                                    // we are smaller than then them
                                    (false, true) => Ordering::Less,
                                    // we are unbounded at the top, they are less than us
                                    // we are bigger than them
                                    (true, false) => Ordering::Greater,
                                    // both are bounded
                                    (false, false) => {
                                        match self
                                            .upper_bound()
                                            .0
                                            .expect("upper bound left should be bounded")
                                            .cmp(
                                                other
                                                    .upper_bound()
                                                    .0
                                                    .expect("upper bound right should be bounded"),
                                            ) {
                                            Ordering::Less => Ordering::Less,
                                            Ordering::Equal => {
                                                self.upper_bound().1.cmp(&other.upper_bound().1)
                                            }
                                            Ordering::Greater => Ordering::Greater,
                                        }
                                    }
                                }
                            }
                            Ordering::Greater => Ordering::Greater,
                        }
                    }
                    Ordering::Greater => Ordering::Greater,
                }
            }
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl PartialOrd for QueryItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl PartialOrd<&[u8]> for QueryItem {
    fn partial_cmp(&self, other: &&[u8]) -> Option<Ordering> {
        let other = Self::Key(other.to_vec());
        Some(self.cmp(&other))
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl From<Vec<u8>> for QueryItem {
    fn from(key: Vec<u8>) -> Self {
        Self::Key(key)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod test {
    use crate::proofs::query::query_item::QueryItem;

    #[test]
    fn query_item_collides() {
        assert!(!QueryItem::Key(vec![10]).collides_with(&QueryItem::Key(vec![20])));
        assert!(QueryItem::Key(vec![10]).collides_with(&QueryItem::Key(vec![10])));
        assert!(!QueryItem::Key(vec![20]).collides_with(&QueryItem::Key(vec![10])));

        assert!(!QueryItem::Key(vec![10]).collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(QueryItem::Key(vec![10]).collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(QueryItem::Key(vec![15]).collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(!QueryItem::Key(vec![20]).collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(
            QueryItem::Key(vec![20]).collides_with(&QueryItem::RangeInclusive(vec![10]..=vec![20]))
        );
        assert!(!QueryItem::Key(vec![30]).collides_with(&QueryItem::Range(vec![10]..vec![20])));

        assert!(!QueryItem::Range(vec![10]..vec![20])
            .collides_with(&QueryItem::Range(vec![30]..vec![40])));
        assert!(!QueryItem::Range(vec![10]..vec![20])
            .collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(QueryItem::RangeInclusive(vec![10]..=vec![20])
            .collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(QueryItem::Range(vec![15]..vec![25])
            .collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(!QueryItem::Range(vec![20]..vec![30])
            .collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(QueryItem::RangeFrom(vec![2]..).collides_with(&QueryItem::Key(vec![5])));
    }

    #[test]
    fn query_item_cmp() {
        assert!(QueryItem::Key(vec![10]) < QueryItem::Key(vec![20]));
        assert_eq!(QueryItem::Key(vec![10]), QueryItem::Key(vec![10]));
        assert!(QueryItem::Key(vec![20]) > QueryItem::Key(vec![10]));

        assert!(QueryItem::Key(vec![10]) < QueryItem::Range(vec![20]..vec![30]));
        // assert_eq!(
        //     QueryItem::Key(vec![10]),
        //     QueryItem::Range(vec![10]..vec![20])
        // );
        // assert_eq!(
        //     QueryItem::Key(vec![15]),
        //     QueryItem::Range(vec![10]..vec![20])
        // );
        assert!(QueryItem::Key(vec![20]) > QueryItem::Range(vec![10]..vec![20]));
        // assert_eq!(
        //     QueryItem::Key(vec![20]),
        //     QueryItem::RangeInclusive(vec![10]..=vec![20])
        // );
        assert!(QueryItem::Key(vec![30]) > QueryItem::Range(vec![10]..vec![20]));

        assert!(QueryItem::Range(vec![10]..vec![20]) < QueryItem::Range(vec![30]..vec![40]));
        assert!(QueryItem::Range(vec![10]..vec![20]) < QueryItem::Range(vec![20]..vec![30]));
        // assert_eq!(
        //     QueryItem::RangeInclusive(vec![10]..=vec![20]),
        //     QueryItem::Range(vec![20]..vec![30])
        // );
        // assert_eq!(
        //     QueryItem::Range(vec![15]..vec![25]),
        //     QueryItem::Range(vec![20]..vec![30])
        // );
        assert!(QueryItem::Range(vec![20]..vec![30]) > QueryItem::Range(vec![10]..vec![20]));
    }
}
