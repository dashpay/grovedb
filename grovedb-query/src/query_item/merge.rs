use std::{
    cmp::{max, min},
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

use crate::query_item::QueryItem;

impl QueryItem {
    /// Merge two overlapping query items into one that covers both ranges.
    pub fn merge(&self, other: &Self) -> Self {
        if self.is_key() && other.is_key() && self == other {
            return self.clone();
        }

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

    /// Merges another QueryItem into this one in-place.
    pub fn merge_assign(&mut self, other: &Self) {
        *self = self.merge(other);
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;

    #[test]
    fn test_merge_of_two_equal_keys_must_be_the_same_key() {
        let value = vec![
            3, 207, 99, 250, 114, 92, 207, 167, 120, 9, 236, 164, 124, 63, 102, 237, 201, 35, 86,
            5, 23, 169, 147, 150, 61, 132, 155, 33, 225, 145, 85, 138,
        ];

        let key1 = QueryItem::Key(value.clone());
        let key2 = key1.clone();

        let merged = key1.merge(&key2);

        assert_matches!(merged, QueryItem::Key(v) if v == value);
    }

    // -- start_non_inclusive branches --

    #[test]
    fn merge_range_after_with_upper_unbounded_gives_range_after() {
        // RangeAfter(3..) merged with RangeFrom(5..) => RangeAfter(3..)
        // The non-inclusive lower bound (3) is strictly less, so it wins as min
        let a = QueryItem::RangeAfter(vec![3]..);
        let b = QueryItem::RangeFrom(vec![5]..);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeAfter(r) if r.start == vec![3]);
    }

    #[test]
    fn merge_range_after_with_inclusive_end_gives_range_after_to_inclusive() {
        // RangeAfterToInclusive(0..=5) has non-inclusive lower bound strictly
        // less than RangeInclusive(1..=10), so start_non_inclusive=true and
        // both uppers are bounded+inclusive, max picks 10.
        let a = QueryItem::RangeAfterToInclusive(vec![0]..=vec![5]);
        let b = QueryItem::RangeInclusive(vec![1]..=vec![10]);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeAfterToInclusive(r)
            if *r.start() == vec![0] && *r.end() == vec![10]);
    }

    #[test]
    fn merge_range_after_with_exclusive_end_gives_range_after_to() {
        let a = QueryItem::RangeAfterTo(vec![1]..vec![10]);
        let b = QueryItem::RangeAfterTo(vec![2]..vec![8]);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeAfterTo(r)
            if r.start == vec![1] && r.end == vec![10]);
    }

    // -- lower_unbounded branches --

    #[test]
    fn merge_unbounded_lower_and_upper_gives_range_full() {
        let a = QueryItem::RangeTo(..vec![5]);
        let b = QueryItem::RangeFrom(vec![1]..);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeFull(..));
    }

    #[test]
    fn merge_unbounded_lower_with_inclusive_end_gives_range_to_inclusive() {
        let a = QueryItem::RangeTo(..vec![5]);
        let b = QueryItem::RangeInclusive(vec![1]..=vec![10]);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeToInclusive(r) if r.end == vec![10]);
    }

    #[test]
    fn merge_unbounded_lower_with_exclusive_end_gives_range_to() {
        let a = QueryItem::RangeTo(..vec![5]);
        let b = QueryItem::Range(vec![1]..vec![10]);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeTo(r) if r.end == vec![10]);
    }

    // -- bounded lower branches --

    #[test]
    fn merge_bounded_lower_with_upper_unbounded_gives_range_from() {
        let a = QueryItem::Range(vec![1]..vec![5]);
        let b = QueryItem::RangeFrom(vec![3]..);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeFrom(r) if r.start == vec![1]);
    }

    #[test]
    fn merge_bounded_with_inclusive_end_gives_range_inclusive() {
        let a = QueryItem::Range(vec![1]..vec![5]);
        let b = QueryItem::RangeInclusive(vec![3]..=vec![8]);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::RangeInclusive(r)
            if *r.start() == vec![1] && *r.end() == vec![8]);
    }

    #[test]
    fn merge_bounded_with_exclusive_end_gives_range() {
        let a = QueryItem::Range(vec![1]..vec![5]);
        let b = QueryItem::Range(vec![3]..vec![8]);
        let merged = a.merge(&b);
        assert_matches!(merged, QueryItem::Range(r) if r.start == vec![1] && r.end == vec![8]);
    }

    // -- merge_assign --

    #[test]
    fn merge_assign_updates_in_place() {
        let mut a = QueryItem::Range(vec![1]..vec![5]);
        let b = QueryItem::Range(vec![3]..vec![8]);
        a.merge_assign(&b);
        assert_matches!(a, QueryItem::Range(r) if r.start == vec![1] && r.end == vec![8]);
    }
}
