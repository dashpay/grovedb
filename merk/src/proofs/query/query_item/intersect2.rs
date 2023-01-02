use std::{
    cmp::Ordering,
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

use crate::proofs::query::query_item::{
    intersect2::RangeSetItem::{Exclusive, Inclusive, Unbounded},
    QueryItem,
};

// TODO: Refactor into nice units
pub struct RangeSetIntersection {
    in_both: Option<RangeSet>,
    ours_left: Option<RangeSet>,
    ours_right: Option<RangeSet>,
    theirs_left: Option<RangeSet>,
    theirs_right: Option<RangeSet>,
}

/// Concise query item representation
#[derive(Clone)]
pub struct RangeSet {
    start: RangeSetItem,
    end: RangeSetItem,
}

pub struct QueryItemIntersectionResult {
    pub in_both: Option<QueryItem>,
    pub ours_left: Option<QueryItem>,
    pub ours_right: Option<QueryItem>,
    pub theirs_left: Option<QueryItem>,
    pub theirs_right: Option<QueryItem>,
}

impl From<RangeSetIntersection> for QueryItemIntersectionResult {
    fn from(range_set_intersection: RangeSetIntersection) -> Self {
        Self {
            in_both: range_set_intersection.in_both.map(|a| a.to_query_item()),
            ours_left: range_set_intersection.ours_left.map(|a| a.to_query_item()),
            ours_right: range_set_intersection.ours_right.map(|a| a.to_query_item()),
            theirs_left: range_set_intersection
                .theirs_left
                .map(|a| a.to_query_item()),
            theirs_right: range_set_intersection
                .theirs_right
                .map(|a| a.to_query_item()),
        }
    }
}

impl QueryItemIntersectionResult {
    fn flip(self) -> Self {
        QueryItemIntersectionResult {
            in_both: self.in_both,
            ours_left: self.theirs_left,
            ours_right: self.theirs_right,
            theirs_left: self.ours_left,
            theirs_right: self.ours_right,
        }
    }
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
                QueryItem::RangeAfterToInclusive(RangeInclusive::new(start.clone(), end.clone()))
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
        // how to detect non-overlapping sets??
        // the end of one of the sets is smaller than the start of another
        if self.end < other.start || other.end < self.start {
            // the sets do not overlap
            // no common element
            if self.end < other.start {
                // self is at the left
                return RangeSetIntersection {
                    in_both: None,
                    ours_left: Some(self.clone()),
                    ours_right: None,
                    theirs_right: Some(other.clone()),
                    theirs_left: None,
                };
            } else {
                return RangeSetIntersection {
                    in_both: None,
                    ours_left: None,
                    ours_right: Some(self.clone()),
                    theirs_left: Some(other.clone()),
                    theirs_right: None,
                };
            }
        }

        let (smaller_start, bigger_start) =
            RangeSetItem::order_items(&self.start, &other.start, self.start.cmp(&other.start));

        let (smaller_end, larger_end) =
            RangeSetItem::order_items(&self.end, &other.end, self.end.cmp(&other.end));

        // need to get 3 things, 3 range sets to be precise, optional range sets
        // how to we perform this intersection
        // need to check both starts and see which is smaller, hence need to implement
        // ord for range set item

        // assume they are equal and progressively update the common boundary
        let mut intersection_result = RangeSetIntersection {
            in_both: Some(self.clone()),
            ours_left: None,
            ours_right: None,
            theirs_left: None,
            theirs_right: None,
        };

        // if the comparison of the start are not equal then we have value for left
        if self.start != other.start {
            if &self.start == smaller_start {
                // ours left
                intersection_result.ours_left = Some(RangeSet {
                    start: smaller_start.clone(),
                    end: bigger_start.invert(),
                });
            } else {
                intersection_result.theirs_left = Some(RangeSet {
                    start: smaller_start.clone(),
                    end: bigger_start.invert(),
                });
            }
            // intersection_result.common.expect("set above").start =
            // bigger_start.clone();
        }

        if self.end != other.end {
            if self.end > other.end {
                // ours right
                intersection_result.ours_right = Some(RangeSet {
                    start: smaller_end.invert(),
                    end: larger_end.clone(),
                });
            } else {
                intersection_result.theirs_right = Some(RangeSet {
                    start: smaller_end.invert(),
                    end: larger_end.clone(),
                });
            }
            // now we need to know the bigger one and basically perform an
            // inversion of the other one
            // intersection_result.common.expect("set above").end =
            // smaller_end.clone()
        }

        intersection_result.in_both = Some(RangeSet {
            start: bigger_start.clone(),
            end: smaller_end.clone(),
        });

        intersection_result
    }
}

/// Represents all possible value types in a range set
#[derive(Eq, PartialEq, Ord, Clone)]
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

    // need to create a new function that takes an ordering and returns a tuple with
    // the elements
    pub fn order_items<'a>(
        item_one: &'a RangeSetItem,
        item_two: &'a RangeSetItem,
        order: Ordering,
    ) -> (&'a RangeSetItem, &'a RangeSetItem) {
        match order {
            Ordering::Less => (item_one, item_two),
            _ => (item_two, item_one),
        }
    }
}

impl PartialOrd for RangeSetItem {
    // TODO: hmm, this is wrong, could be equal right??
    //  but then equal returns the same other as less or greater than.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Unbounded, _) => Some(Ordering::Less),
            (_, Unbounded) => Some(Ordering::Greater),
            (Inclusive(v1), Inclusive(v2)) | (Exclusive(v1), Exclusive(v2)) => {
                if v1 < v2 {
                    Some(Ordering::Less)
                } else {
                    Some(Ordering::Greater)
                }
            }
            (Inclusive(v1), Exclusive(v2)) => {
                if v1 < v2 || v1 == v2 {
                    Some(Ordering::Less)
                } else {
                    Some(Ordering::Greater)
                }
            }
            (Exclusive(v1), Inclusive(v2)) => {
                if v1 < v2 {
                    Some(Ordering::Less)
                } else {
                    // they are equal of v2 is less
                    // inclusive always wins
                    Some(Ordering::Greater)
                }
            }
        }
    }
}

// need to convert from a query item to a range set
// TODO: remove clones
impl QueryItem {
    pub fn intersect(&self, other: &Self) -> QueryItemIntersectionResult {
        self.to_range_set().intersect(other.to_range_set()).into()
    }

    // TODO: convert to impl of From/To trait
    pub fn to_range_set(&self) -> RangeSet {
        match self {
            QueryItem::Key(start) => RangeSet {
                start: RangeSetItem::Inclusive(start.clone()),
                end: RangeSetItem::Inclusive(start.clone()),
            },
            QueryItem::Range(range) => RangeSet {
                start: RangeSetItem::Inclusive(range.start.clone()),
                end: RangeSetItem::Exclusive(range.end.clone()),
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
                start: RangeSetItem::Inclusive(range.start.clone()),
                end: RangeSetItem::Unbounded,
            },
            QueryItem::RangeTo(range) => RangeSet {
                start: RangeSetItem::Unbounded,
                end: RangeSetItem::Exclusive(range.end.clone()),
            },
            QueryItem::RangeToInclusive(range) => RangeSet {
                start: RangeSetItem::Unbounded,
                end: RangeSetItem::Inclusive(range.end.clone()),
            },
            QueryItem::RangeAfter(range) => RangeSet {
                start: RangeSetItem::Exclusive(range.start.clone()),
                end: RangeSetItem::Unbounded,
            },
            QueryItem::RangeAfterTo(range) => RangeSet {
                start: RangeSetItem::Exclusive(range.start.clone()),
                end: RangeSetItem::Exclusive(range.end.clone()),
            },
            QueryItem::RangeAfterToInclusive(range) => RangeSet {
                start: RangeSetItem::Exclusive(range.start().clone()),
                end: RangeSetItem::Inclusive(range.end().clone()),
            },
        }
    }
}
