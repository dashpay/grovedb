#[cfg(any(feature = "full", feature = "verify"))]
mod intersect;
mod merge;
// TODO: potentially rename
mod intersect2;

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
