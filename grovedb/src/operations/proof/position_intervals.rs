//! Interval arithmetic over the u64 position space of the append-only
//! (non-Merk) tree types: `MmrTree`, `BulkAppendTree`, `CommitmentTree`.
//!
//! The completeness and soundness checks for those layers compare the
//! positions a query asks for against the positions a proof actually
//! carries. The naive way to do that — expanding every query range into a
//! set of individual positions bounded by the parent element's count — costs
//! memory and time proportional to that count. The count comes from the
//! parent `Element` bytes inside the proof, which are only bound to the
//! trusted root *after* the lower layer has been verified, so a forged proof
//! could declare an enormous count and make the verifier allocate millions of
//! positions (and format them all into an error string) before the hash
//! chain rejected it (audit finding P05, issue #856).
//!
//! [`PositionIntervals`] keeps the query as a sorted, disjoint list of
//! half-open `[start, end)` ranges instead. Building it costs
//! O(items log items); membership is a binary search; completeness against
//! a proved set costs O(|proved| + |intervals|); and the diagnostics report
//! a bounded number of example positions plus an arithmetic total. Nothing
//! here scales with the parent count, so an unauthenticated count cannot
//! drive allocation.

use std::collections::BTreeSet;

use grovedb_merk::proofs::query::QueryItem;

use crate::Error;

/// Upper bound on the number of individual positions an error message may
/// spell out. The total is always reported arithmetically alongside them.
pub(crate) const MAX_REPORTED_POSITIONS: usize = 16;

/// Sorted, pairwise-disjoint, non-empty half-open position intervals
/// `[start, end)`, clipped to `[0, count)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PositionIntervals {
    ranges: Vec<(u64, u64)>,
}

/// Outcome of a completeness or soundness comparison: how many positions
/// were off, and up to [`MAX_REPORTED_POSITIONS`] examples of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PositionMismatch {
    /// Total number of offending positions (computed arithmetically, never
    /// by enumeration).
    pub total: u64,
    /// The smallest offending positions, at most [`MAX_REPORTED_POSITIONS`].
    pub examples: Vec<u64>,
}

impl PositionMismatch {
    pub(crate) fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Human-readable rendering with a bounded number of positions.
    pub(crate) fn describe(&self) -> String {
        if self.total as usize > self.examples.len() {
            format!(
                "{} positions, first {}: {:?}",
                self.total,
                self.examples.len(),
                self.examples
            )
        } else {
            format!("{} positions: {:?}", self.total, self.examples)
        }
    }
}

fn be_u64(key: &[u8]) -> Result<u64, Error> {
    let arr: [u8; 8] = key
        .try_into()
        .map_err(|_| Error::InvalidInput("position key must be exactly 8 bytes (BE u64)"))?;
    Ok(u64::from_be_bytes(arr))
}

impl PositionIntervals {
    /// Translate query items (with BE u64 keys) into position intervals
    /// bounded by `count`. Ranges that fall entirely outside `[0, count)` or
    /// that are empty contribute nothing. Overlapping and adjacent intervals
    /// are merged so the result is canonical.
    pub(crate) fn from_query_items(items: &[QueryItem], count: u64) -> Result<Self, Error> {
        let mut ranges: Vec<(u64, u64)> = Vec::with_capacity(items.len());

        for item in items {
            let (start, end) = match item {
                QueryItem::Key(key) => {
                    let idx = be_u64(key)?;
                    (idx, idx.saturating_add(1))
                }
                QueryItem::RangeInclusive(range) => (
                    be_u64(range.start())?,
                    be_u64(range.end())?.saturating_add(1),
                ),
                QueryItem::Range(range) => (be_u64(&range.start)?, be_u64(&range.end)?),
                QueryItem::RangeFrom(range) => (be_u64(&range.start)?, u64::MAX),
                QueryItem::RangeTo(range) => (0, be_u64(&range.end)?),
                QueryItem::RangeToInclusive(range) => (0, be_u64(&range.end)?.saturating_add(1)),
                QueryItem::RangeFull(..) => (0, u64::MAX),
                QueryItem::RangeAfter(range) => (be_u64(&range.start)?.saturating_add(1), u64::MAX),
                QueryItem::RangeAfterTo(range) => {
                    (be_u64(&range.start)?.saturating_add(1), be_u64(&range.end)?)
                }
                QueryItem::RangeAfterToInclusive(range) => (
                    be_u64(range.start())?.saturating_add(1),
                    be_u64(range.end())?.saturating_add(1),
                ),
                QueryItem::AggregateCountOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountOnRange is only supported on provable count trees, \
                         not on this tree type",
                    ));
                }
                QueryItem::AggregateSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateSumOnRange is only supported on provable sum trees, \
                         not on this tree type",
                    ));
                }
                QueryItem::AggregateCountAndSumOnRange(_) => {
                    return Err(Error::InvalidInput(
                        "AggregateCountAndSumOnRange is only supported on \
                         ProvableCountProvableSumTree, not on this tree type",
                    ));
                }
            };
            let end = end.min(count);
            if start < end {
                ranges.push((start, end));
            }
        }

        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            match merged.last_mut() {
                // Overlapping or adjacent: extend the previous interval.
                Some((_, prev_end)) if start <= *prev_end => {
                    if end > *prev_end {
                        *prev_end = end;
                    }
                }
                _ => merged.push((start, end)),
            }
        }

        Ok(Self { ranges: merged })
    }

    /// The merged intervals, ascending.
    #[cfg(test)]
    pub(crate) fn ranges(&self) -> &[(u64, u64)] {
        &self.ranges
    }

    /// Number of positions covered, saturating at `u64::MAX`. The prover
    /// charges the row limit with this; a verify-only build has no prover.
    #[cfg_attr(not(feature = "minimal"), allow(dead_code))]
    pub(crate) fn len(&self) -> u64 {
        self.ranges
            .iter()
            .fold(0u64, |acc, (start, end)| acc.saturating_add(end - start))
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Whether `position` lies in one of the intervals. Binary search.
    pub(crate) fn contains(&self, position: u64) -> bool {
        // Find the last interval whose start is <= position.
        let idx = self.ranges.partition_point(|(start, _)| *start <= position);
        idx > 0 && position < self.ranges[idx - 1].1
    }

    /// Completeness: which covered positions are absent from `proved`?
    ///
    /// Cost is O(|proved ∩ covered| + |intervals|): each interval's coverage
    /// is measured by counting the proved positions inside it, and only the
    /// first [`MAX_REPORTED_POSITIONS`] gaps are walked out as examples.
    pub(crate) fn missing_from(&self, proved: &BTreeSet<u64>) -> PositionMismatch {
        let mut total = 0u64;
        let mut examples = Vec::new();

        for &(start, end) in &self.ranges {
            let present = proved.range(start..end).count() as u64;
            let width = end - start;
            if present == width {
                continue;
            }
            total = total.saturating_add(width - present);

            if examples.len() >= MAX_REPORTED_POSITIONS {
                continue;
            }
            // Walk the gaps between consecutive proved positions inside this
            // interval, stopping as soon as enough examples are collected.
            let mut expected = start;
            for &p in proved.range(start..end) {
                if push_gap(&mut examples, expected, p) {
                    break;
                }
                expected = p + 1;
            }
            if examples.len() < MAX_REPORTED_POSITIONS {
                push_gap(&mut examples, expected, end);
            }
        }

        PositionMismatch { total, examples }
    }

    /// Soundness: which of `proved` lie outside every interval?
    ///
    /// Cost is O(|proved| log |intervals|).
    pub(crate) fn extra_in<'a>(&self, proved: impl Iterator<Item = &'a u64>) -> PositionMismatch {
        let mut total = 0u64;
        let mut examples = Vec::new();
        for &p in proved {
            if self.contains(p) {
                continue;
            }
            total = total.saturating_add(1);
            if examples.len() < MAX_REPORTED_POSITIONS {
                examples.push(p);
            }
        }
        // Callers pass an ascending set, so the examples are already the
        // smallest; sort anyway to keep the contract independent of input.
        examples.sort_unstable();
        PositionMismatch { total, examples }
    }
}

/// Append the positions of `[from, to)` to `examples` until the cap is
/// reached. Returns `true` once the cap is hit.
fn push_gap(examples: &mut Vec<u64>, from: u64, to: u64) -> bool {
    let mut p = from;
    while p < to {
        if examples.len() >= MAX_REPORTED_POSITIONS {
            return true;
        }
        examples.push(p);
        p += 1;
    }
    examples.len() >= MAX_REPORTED_POSITIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be(v: u64) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    /// Reference implementation: the enumeration this module replaces.
    fn enumerate(items: &[QueryItem], count: u64) -> BTreeSet<u64> {
        let mut out = BTreeSet::new();
        for p in 0..count {
            if items.iter().any(|item| item.contains(&be(p))) {
                out.insert(p);
            }
        }
        out
    }

    fn all_variants() -> Vec<QueryItem> {
        vec![
            QueryItem::Key(be(3)),
            QueryItem::Key(be(40)),
            QueryItem::Range(be(2)..be(5)),
            QueryItem::RangeInclusive(be(7)..=be(9)),
            QueryItem::RangeFrom(be(15)..),
            QueryItem::RangeTo(..be(1)),
            QueryItem::RangeToInclusive(..=be(1)),
            QueryItem::RangeFull(..),
            QueryItem::RangeAfter(be(12)..),
            QueryItem::RangeAfterTo(be(9)..be(11)),
            QueryItem::RangeAfterToInclusive(be(5)..=be(6)),
            QueryItem::Range(be(9)..be(9)),
            QueryItem::RangeInclusive(be(9)..=be(8)),
            QueryItem::RangeInclusive(be(u64::MAX)..=be(u64::MAX)),
            QueryItem::RangeAfter(be(u64::MAX)..),
        ]
    }

    #[test]
    fn every_variant_matches_enumeration() {
        for item in all_variants() {
            for count in [0u64, 1, 2, 5, 10, 13, 20] {
                let intervals =
                    PositionIntervals::from_query_items(std::slice::from_ref(&item), count)
                        .expect("build");
                let expected = enumerate(std::slice::from_ref(&item), count);
                let actual: BTreeSet<u64> = intervals
                    .ranges()
                    .iter()
                    .flat_map(|(s, e)| *s..*e)
                    .collect();
                assert_eq!(actual, expected, "item {:?} count {}", item, count);
                assert_eq!(intervals.len(), expected.len() as u64);
                assert_eq!(intervals.is_empty(), expected.is_empty());
                for p in 0..count + 2 {
                    assert_eq!(intervals.contains(p), expected.contains(&p));
                }
            }
        }
    }

    #[test]
    fn overlapping_and_adjacent_items_merge() {
        let items = vec![
            QueryItem::Range(be(5)..be(8)),
            QueryItem::Key(be(8)),
            QueryItem::RangeInclusive(be(1)..=be(6)),
            QueryItem::Key(be(20)),
            QueryItem::RangeAfter(be(30)..),
        ];
        let intervals = PositionIntervals::from_query_items(&items, 100).unwrap();
        assert_eq!(intervals.ranges(), &[(1, 9), (20, 21), (31, 100)]);
        assert_eq!(intervals.len(), 8 + 1 + 69);
        assert_eq!(
            intervals
                .ranges()
                .iter()
                .flat_map(|(s, e)| *s..*e)
                .collect::<BTreeSet<_>>(),
            enumerate(&items, 100)
        );
    }

    #[test]
    fn huge_count_costs_nothing_extra() {
        let items = vec![QueryItem::RangeFull(..)];
        let intervals = PositionIntervals::from_query_items(&items, u64::MAX).unwrap();
        assert_eq!(intervals.ranges(), &[(0, u64::MAX)]);
        assert_eq!(intervals.len(), u64::MAX);

        let proved: BTreeSet<u64> = [0u64, 1, 2].into_iter().collect();
        let missing = intervals.missing_from(&proved);
        assert_eq!(missing.total, u64::MAX - 3);
        assert_eq!(missing.examples.len(), MAX_REPORTED_POSITIONS);
        assert_eq!(missing.examples[0], 3);
        assert_eq!(
            missing.examples[MAX_REPORTED_POSITIONS - 1],
            3 + MAX_REPORTED_POSITIONS as u64 - 1
        );
        let text = missing.describe();
        assert!(text.starts_with(&format!("{} positions, first 16: [3, 4", u64::MAX - 3)));
    }

    #[test]
    fn missing_examples_walk_gaps_across_intervals() {
        let items = vec![
            QueryItem::Range(be(0)..be(4)),
            QueryItem::RangeInclusive(be(10)..=be(12)),
        ];
        let intervals = PositionIntervals::from_query_items(&items, 100).unwrap();
        let proved: BTreeSet<u64> = [1u64, 3, 11, 50].into_iter().collect();
        let missing = intervals.missing_from(&proved);
        assert_eq!(missing.total, 4);
        assert_eq!(missing.examples, vec![0, 2, 10, 12]);
        assert_eq!(missing.describe(), "4 positions: [0, 2, 10, 12]");

        let full: BTreeSet<u64> = (0..4).chain(10..=12).collect();
        assert!(intervals.missing_from(&full).is_empty());
    }

    #[test]
    fn extra_reports_positions_outside_every_interval() {
        let items = vec![QueryItem::Range(be(0)..be(4))];
        let intervals = PositionIntervals::from_query_items(&items, 100).unwrap();
        let proved: BTreeSet<u64> = [0u64, 3, 4, 9].into_iter().collect();
        let extra = intervals.extra_in(proved.iter());
        assert_eq!(extra.total, 2);
        assert_eq!(extra.examples, vec![4, 9]);

        let many: BTreeSet<u64> = (100..200).collect();
        let extra = intervals.extra_in(many.iter());
        assert_eq!(extra.total, 100);
        assert_eq!(extra.examples.len(), MAX_REPORTED_POSITIONS);
        assert_eq!(extra.examples[0], 100);
    }

    #[test]
    fn rejects_non_u64_keys_and_aggregate_items() {
        assert!(PositionIntervals::from_query_items(&[QueryItem::Key(vec![1, 2])], 10).is_err());
        assert!(PositionIntervals::from_query_items(
            &[QueryItem::AggregateCountOnRange(Box::new(
                QueryItem::RangeFull(..)
            ))],
            10
        )
        .is_err());
    }
}
