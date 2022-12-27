use std::{
    cmp::Ordering,
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

// convert every query item to a range set
use crate::proofs::query::query_item::QueryItem;
use crate::proofs::query::query_item::{
    intersect2::RangeSetItem::{Inclusive, Unbounded, Exclusive},
    QueryItem::Range,
};

// TODO: Refactor into nice units

pub struct RangeSetIntersection {
    left: Option<RangeSet>,
    common: Option<RangeSet>,
    right: Option<RangeSet>,
}

/// Concise query item representation
#[derive(Clone)]
pub struct RangeSet {
    start: RangeSetItem,
    end: RangeSetItem,
}

impl RangeSet {
    // TODO: convert to impl of From/To trait
    pub fn to_query_item(&self) -> QueryItem {
        match (&self.start, &self.end) {
            (RangeSetItem::Inclusive(start), RangeSetItem::Inclusive(end)) => {
                if start == end {
                    QueryItem::Key(start.clone())
                } else {
                    QueryItem::RangeInclusive(RangeInclusive::new(start.clone(), end.clone()))
                }
            }
            (RangeSetItem::Inclusive(start), RangeSetItem::Exclusive(end)) => {
                QueryItem::Range(Range {
                    start: start.clone(),
                    end: end.clone(),
                })
            }
            (RangeSetItem::Inclusive(start), RangeSetItem::Unbounded) => {
                QueryItem::RangeFrom(RangeFrom {
                    start: start.clone(),
                })
            }
            (RangeSetItem::Exclusive(start), RangeSetItem::Exclusive(end)) => {
                QueryItem::RangeAfterTo(Range {
                    start: start.clone(),
                    end: end.clone(),
                })
            }
            (RangeSetItem::Exclusive(start), RangeSetItem::Inclusive(end)) => {
                QueryItem::RangeAfterToInclusive(RangeInclusive::new(start.clone, end.clone()))
            }
            (RangeSetItem::Exclusive(start), RangeSetItem::Unbounded) => {
                QueryItem::RangeAfter(RangeFrom {
                    start: start.clone(),
                })
            }
            (RangeSetItem::Unbounded, RangeSetItem::Unbounded) => QueryItem::RangeFull(RangeFull),
            (RangeSetItem::Unbounded, RangeSetItem::Inclusive(end)) => {
                QueryItem::RangeToInclusive(RangeToInclusive { end: end.clone() })
            }
            (RangeSetItem::Unbounded, RangeSetItem::Exclusive(end)) => {
                QueryItem::RangeTo(RangeTo { end: end.clone() })
            }
        }
    }

    pub fn intersect(&self, other: RangeSet) -> RangeSetIntersection {
        // Current version assumes that the range set does not overlap
        // TODO: Handle non overlapping range sets

        // need to get 3 things, 3 range sets to be precise, optional range sets
        // how to we perform this intersection
        // need to check both starts and see which is smaller, hence need to implement
        // ord for range set item

        // assume they are equal and progressively update the common boundary
        let mut intersection_result = RangeSetIntersection {
            left: None,
            common: Some(self.clone()),
            right: None,
        };

        // if the comparison of the start are not equal then we have value for left
        if self.start != other.start {
            // now we need to know the smaller one, basically perform an
            // ordering and invert the other one

        }

        if self.end != other.end {
            // now we need to know the bigger one and basically perform an
            // inversion of the other one
        }

        // the common part is the bigger and smaller side of the two

        intersection_result
    }
}

/// Represents all possible value types in a range set
// TODO: need specific unbounded values??
#[derive(Eq, PartialEq, Clone)]
pub enum RangeSetItem {
    Unbounded,
    Inclusive(Vec<u8>),
    Exclusive(Vec<u8>),
}

impl RangeSetItem {
    pub fn invert(&self) -> RangeSetItem {
        match &self {
            RangeSetItem::Unbounded => RangeSetItem::Unbounded,
            RangeSetItem::Inclusive(v) => RangeSetItem::Exclusive(v.clone()),
            RangeSetItem::Exclusive(v) => RangeSetItem::Inclusive(v.clone()),
        }
    }

    // TODO: maybe compare start and compare end??
    pub fn compare(
        item_one: &RangeSetItem,
        item_two: &RangeSetItem,
        direction_is_left: bool,
    ) -> (&RangeSetItem, &RangeSetItem) {
        // returns a tuple (smaller, bigger)
        match (item_one, item_two, direction_is_left) {
            // since direction is left, unbounded is the smaller one
            (Unbounded, _, true) => (item_one, item_two),
            (_, Unbounded, true) => (item_two, item_one),
            // since direction is right, unbounded is the larger one
            (Unbounded, _, false) => (item_two, item_one),
            (_, Unbounded, false) => (item_one, item_two),
            (Inclusive(v1), Inclusive(v2), _) => {
                // TODO: maybe change to match with cmp??
                if v1 < v2 {
                    (item_one, item_two)
                } else {
                    (item_two, item_one)
                }
            }
            (Inclusive(v1), Exclusive(v2), true) => {
                // if we are in the start direction, inclusive is less that exclusive
                // what if v1 and v2 are equal
                if v1 < v2 {
                    (item_one, item_two)
                } else if {
                    (item_two, item_one)
                }
            }
        }
    }
}

// need to convert from a query item to a range set
// TODO: remove clones
impl QueryItem {
    // TODO: convert to impl of From/To trait
    pub fn to_range_set(&self) -> RangeSet {
        match QueryItem {
            QueryItem::Key(start) => RangeSet {
                start: RangeSetItem::Inclusive(start),
                end: RangeSetItem::Inclusive(start.clone()),
            },
            QueryItem::Range(range) => RangeSet {
                start: RangeSetItem::Inclusive(range.start),
                end: RangeSetItem::Exclusive(range.end),
            },
            QueryItem::RangeInclusive(range) => RangeSet {
                start: RangeSetItem::Inclusive(range.start().clone()),
                end: RangeSetItem::Inclusive(range.end().clone()),
            },
            QueryItem::RangeFull(..) => RangeSet {
                start: RangeSetItem::Unbounded,
                end: RangeSetItem::Unbounded,
            },
            QueryItem::RangeFrom(range) => RangeSet {
                start: RangeSetItem::Inclusive(range.start),
                end: RangeSetItem::Unbounded,
            },
            QueryItem::RangeTo(range) => RangeSet {
                start: RangeSetItem::Unbounded,
                end: RangeSetItem::Exclusive(range.end),
            },
            QueryItem::RangeToInclusive(range) => RangeSet {
                start: RangeSetItem::Unbounded,
                end: RangeSetItem::Inclusive(range.end),
            },
            QueryItem::RangeAfter(range) => RangeSet {
                start: RangeSetItem::Exclusive(range.start),
                end: RangeSetItem::Unbounded,
            },
            QueryItem::RangeAfterTo(range) => RangeSet {
                start: RangeSetItem::Exclusive(range.start),
                end: RangeSetItem::Exclusive(range.end),
            },
            QueryItem::RangeAfterToInclusive(range) => RangeSet {
                start: RangeSetItem::Exclusive(range.start().clone()),
                end: RangeSetItem::Inclusive(range.end().clone()),
            },
        }
    }
}
