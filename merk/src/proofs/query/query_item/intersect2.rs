use std::ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive};

// convert every query item to a range set
use crate::proofs::query::query_item::QueryItem;
use crate::proofs::query::query_item::{intersect2::RangeSetItem::Inclusive, QueryItem::Range};

/// Concise query item representation
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
            (RangeSetItem::Inclusive(start), RangeSetItem::UpperUnbounded) => {
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
            (RangeSetItem::Exclusive(start), RangeSetItem::UpperUnbounded) => {
                QueryItem::RangeAfter(RangeFrom {
                    start: start.clone(),
                })
            }
            (RangeSetItem::LowerUnbounded, RangeSetItem::UpperUnbounded) => {
                QueryItem::RangeFull(RangeFull)
            }
            (RangeSetItem::LowerUnbounded, RangeSetItem::Inclusive(end)) => {
                QueryItem::RangeToInclusive(RangeToInclusive { end: end.clone() })
            }
            (RangeSetItem::LowerUnbounded, RangeSetItem::Exclusive(end)) => {
                QueryItem::RangeTo(RangeTo { end: end.clone() })
            }
            // TODO don't panic here return an error
            _ => {
                panic!("invalid range set")
            }
        }
    }
}

/// Represents all possible value types in a range set
pub enum RangeSetItem {
    LowerUnbounded,
    UpperUnbounded,
    Inclusive(Vec<u8>),
    Exclusive(Vec<u8>),
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
                start: RangeSetItem::LowerUnbounded,
                end: RangeSetItem::UpperUnbounded,
            },
            QueryItem::RangeFrom(range) => RangeSet {
                start: RangeSetItem::Inclusive(range.start),
                end: RangeSetItem::UpperUnbounded,
            },
            QueryItem::RangeTo(range) => RangeSet {
                start: RangeSetItem::LowerUnbounded,
                end: RangeSetItem::Exclusive(range.end),
            },
            QueryItem::RangeToInclusive(range) => RangeSet {
                start: RangeSetItem::LowerUnbounded,
                end: RangeSetItem::Inclusive(range.end),
            },
            QueryItem::RangeAfter(range) => RangeSet {
                start: RangeSetItem::Exclusive(range.start),
                end: RangeSetItem::UpperUnbounded,
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
