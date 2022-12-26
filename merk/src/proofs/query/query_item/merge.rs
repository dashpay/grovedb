use std::cmp::{max, min};
use std::ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive};
use crate::proofs::query::query_item::QueryItem;

#[cfg(any(feature = "full", feature = "verify"))]
impl QueryItem {
    pub(crate) fn merge(self, other: Self) -> Self {
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
}