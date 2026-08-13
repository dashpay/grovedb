use std::{
    cmp::{max, min},
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

use crate::query_item::QueryItem;

impl QueryItem {
    /// Merge two overlapping query items into one that covers both ranges.
    ///
    /// Neither `self` nor `other` may be an aggregate meta-variant
    /// (`AggregateCountOnRange`, `AggregateSumOnRange`,
    /// `AggregateCountAndSumOnRange`): the result is always a plain
    /// key/range variant, so merging an aggregate would silently erase the
    /// aggregate wrapper and change the meaning of the query. Callers
    /// (`Query::insert_item` / `AggregateSumQuery::insert_item`) must keep
    /// aggregate items out of merging.
    ///
    /// # Panics
    ///
    /// Panics if `self` or `other` is an aggregate meta-variant. This is an
    /// invariant-enforcement panic (violating the precondition would
    /// otherwise silently change query semantics); it is enforced in release
    /// builds too because `merge` is public API.
    pub fn merge(&self, other: &Self) -> Self {
        assert!(
            !self.is_aggregate() && !other.is_aggregate(),
            "QueryItem::merge must not be called with aggregate meta-variants; merging would \
             drop the aggregate wrapper"
        );

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
    ///
    /// # Panics
    ///
    /// Panics if `self` or `other` is an aggregate meta-variant — see
    /// [`Self::merge`].
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

    #[test]
    #[should_panic(expected = "must not be called with aggregate meta-variants")]
    fn test_merge_rejects_aggregate_meta_variants() {
        let aggregate = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let key = QueryItem::Key(b"extra".to_vec());

        let _ = aggregate.merge(&key);
    }
}
