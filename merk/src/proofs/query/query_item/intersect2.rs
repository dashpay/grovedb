use std::{
    cmp::Ordering,
    collections::{btree_set, BTreeSet},
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
    option::IntoIter,
};

use crate::proofs::query::query_item::{
    intersect2::RangeSetItem::{
        ExclusiveEnd, ExclusiveStart, Inclusive, UnboundedEnd, UnboundedStart,
    },
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
#[derive(Clone, Debug)]
pub struct RangeSet {
    start: RangeSetItem,
    end: RangeSetItem,
}

#[derive(Default)]
pub struct QueryItemManyIntersectionResult {
    pub in_both: Option<Vec<QueryItem>>,
    pub ours: Option<Vec<QueryItem>>,
    pub theirs: Option<Vec<QueryItem>>,
}

pub struct QueryItemIntersectionResultTheirsLeftovers {
    pub theirs_left: Option<QueryItem>,
    pub theirs_right: Option<QueryItem>,
}

impl QueryItemManyIntersectionResult {
    fn push_ours(&mut self, our_query_item: QueryItem) {
        let ours_vec = self.ours.get_or_insert(vec![]);
        ours_vec.push(our_query_item);
    }

    fn push_theirs(&mut self, their_query_item: QueryItem) {
        let theirs_vec = self.theirs.get_or_insert(vec![]);
        theirs_vec.push(their_query_item);
    }

    fn push_ours_and_in_both_from_result(
        &mut self,
        query_item_intersection_result: QueryItemIntersectionResult,
    ) -> QueryItemIntersectionResultTheirsLeftovers {
        let QueryItemIntersectionResult {
            in_both,
            ours_left,
            ours_right,
            theirs_left,
            theirs_right,
        } = query_item_intersection_result;
        if let Some(in_both) = in_both {
            let in_both_vec = self.in_both.get_or_insert(vec![]);
            in_both_vec.push(in_both);
        }
        if let Some(ours_left) = ours_left {
            let ours_vec = self.ours.get_or_insert(vec![]);
            ours_vec.push(ours_left);
        }
        if let Some(ours_right) = ours_right {
            let ours_vec = self.ours.get_or_insert(vec![]);
            ours_vec.push(ours_right);
        }

        QueryItemIntersectionResultTheirsLeftovers {
            theirs_left,
            theirs_right,
        }
    }

    fn push_theirs_from_result(
        &mut self,
        query_item_intersection_result: QueryItemIntersectionResultTheirsLeftovers,
    ) {
        let QueryItemIntersectionResultTheirsLeftovers {
            theirs_left,
            theirs_right,
        } = query_item_intersection_result;
        if let Some(theirs_left) = theirs_left {
            let theirs_vec = self.theirs.get_or_insert(vec![]);
            theirs_vec.push(theirs_left);
        }
        if let Some(theirs_right) = theirs_right {
            let theirs_vec = self.theirs.get_or_insert(vec![]);
            theirs_vec.push(theirs_right);
        }
    }

    fn merge_in(&mut self, query_item_many_intersection_result: Self) {
        let QueryItemManyIntersectionResult {
            mut in_both,
            mut ours,
            mut theirs,
        } = query_item_many_intersection_result;
        if let Some(mut in_both) = in_both {
            let in_both_vec = self.in_both.get_or_insert(vec![]);
            in_both_vec.append(&mut in_both);
        }
        if let Some(mut ours) = ours {
            let ours_vec = self.ours.get_or_insert(vec![]);
            ours_vec.append(&mut ours);
        }
        if let Some(mut theirs) = theirs {
            let theirs_vec = self.theirs.get_or_insert(vec![]);
            theirs_vec.append(&mut theirs);
        }
    }
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
            (RangeSetItem::Inclusive(start), RangeSetItem::ExclusiveEnd(end)) => {
                QueryItem::Range(Range {
                    start: start.clone(),
                    end: end.clone(),
                })
            }
            (RangeSetItem::Inclusive(start), RangeSetItem::UnboundedEnd) => {
                QueryItem::RangeFrom(RangeFrom {
                    start: start.clone(),
                })
            }
            (RangeSetItem::ExclusiveStart(start), RangeSetItem::ExclusiveEnd(end)) => {
                QueryItem::RangeAfterTo(Range {
                    start: start.clone(),
                    end: end.clone(),
                })
            }
            (RangeSetItem::ExclusiveStart(start), RangeSetItem::Inclusive(end)) => {
                QueryItem::RangeAfterToInclusive(RangeInclusive::new(start.clone(), end.clone()))
            }
            (RangeSetItem::ExclusiveStart(start), RangeSetItem::UnboundedEnd) => {
                QueryItem::RangeAfter(RangeFrom {
                    start: start.clone(),
                })
            }
            (RangeSetItem::UnboundedStart, RangeSetItem::UnboundedEnd) => {
                QueryItem::RangeFull(RangeFull)
            }
            (RangeSetItem::UnboundedStart, RangeSetItem::Inclusive(end)) => {
                QueryItem::RangeToInclusive(RangeToInclusive { end: end.clone() })
            }
            (RangeSetItem::UnboundedStart, RangeSetItem::ExclusiveEnd(end)) => {
                QueryItem::RangeTo(RangeTo { end: end.clone() })
            }
            _ => {
                // TODO: return proper error, this should be unreachable
                //  if the range set was created from a valid query item,
                //  actually should return None in this case
                unreachable!()
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
                    end: bigger_start.invert(false),
                });
            } else {
                intersection_result.theirs_left = Some(RangeSet {
                    start: smaller_start.clone(),
                    end: bigger_start.invert(false),
                });
            }
            // intersection_result.common.expect("set above").start =
            // bigger_start.clone();
        }

        if self.end != other.end {
            if self.end > other.end {
                // ours right
                intersection_result.ours_right = Some(RangeSet {
                    start: smaller_end.invert(true),
                    end: larger_end.clone(),
                });
            } else {
                intersection_result.theirs_right = Some(RangeSet {
                    start: smaller_end.invert(true),
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
#[derive(Eq, PartialEq, Clone, Debug)]
pub enum RangeSetItem {
    UnboundedStart,
    UnboundedEnd,
    Inclusive(Vec<u8>),
    ExclusiveStart(Vec<u8>),
    ExclusiveEnd(Vec<u8>),
}

impl RangeSetItem {
    pub fn invert(&self, is_start: bool) -> RangeSetItem {
        match &self {
            // TODO: confirm unbounded has no inversions
            RangeSetItem::UnboundedStart => RangeSetItem::UnboundedStart,
            RangeSetItem::UnboundedEnd => RangeSetItem::UnboundedEnd,
            RangeSetItem::Inclusive(v) => {
                if is_start {
                    RangeSetItem::ExclusiveStart(v.clone())
                } else {
                    RangeSetItem::ExclusiveEnd(v.clone())
                }
            }
            RangeSetItem::ExclusiveStart(v) | RangeSetItem::ExclusiveEnd(v) => {
                RangeSetItem::Inclusive(v.clone())
            }
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
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RangeSetItem {
    // TODO: hmm, this is wrong, could be equal right??
    //  but then equal returns the same order as less or greater than.
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (UnboundedStart, _) => Ordering::Less,
            (_, UnboundedStart) => Ordering::Greater,
            (_, UnboundedEnd) => Ordering::Less,
            (UnboundedEnd, _) => Ordering::Greater,

            (Inclusive(v1), Inclusive(v2))
            | (ExclusiveStart(v1), ExclusiveStart(v2))
            | (ExclusiveEnd(v1), ExclusiveEnd(v2)) => {
                if v1 < v2 {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }

            (Inclusive(v1), ExclusiveStart(v2)) | (ExclusiveEnd(v1), Inclusive(v2)) => {
                if v1 < v2 || v1 == v2 {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (Inclusive(v1), ExclusiveEnd(v2)) | (ExclusiveStart(v1), Inclusive(v2)) => {
                if v1 < v2 {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }

            (ExclusiveStart(v1), ExclusiveEnd(v2)) | (ExclusiveEnd(v2), ExclusiveStart(v1)) => {
                // start goes up, end goes down
                // if they are equal, exclusive end is smaller cause it stops just before the
                // number
                if v1 >= v2 {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
        }
    }
}

// need to convert from a query item to a range set
// TODO: remove clones
impl QueryItem {
    /// For this intersection to work ours and theirs must be ordered
    pub fn intersect_many_ordered(
        ours: &mut Vec<Self>,
        theirs: Vec<Self>,
    ) -> QueryItemManyIntersectionResult {
        let mut result = QueryItemManyIntersectionResult::default();
        for our_item in ours.drain(..) {
            // We create an intersection result for this one item
            let mut one_item_pair_intersections = QueryItemManyIntersectionResult::default();
            // We add our item
            // In the end the item might be split up
            one_item_pair_intersections.push_ours(our_item);
            for their_item in theirs.clone() {
                // We take the vector of our item
                // It might be empty if it has already been completely consumed
                // Meaning that all the item was inside of their items
                if let Some(our_item_split_sections) = one_item_pair_intersections.ours.take() {
                    let mut maybe_temp_their_item = Some(their_item);
                    for our_partial_item in our_item_split_sections {
                        if let Some(temp_their_item) = maybe_temp_their_item {
                            let intersection_result = our_partial_item.intersect(&temp_their_item);
                            // ours and in both are guaranteed to be unique
                            let theirs_leftovers = one_item_pair_intersections
                                .push_ours_and_in_both_from_result(intersection_result);
                            // if we assume theirs is ordered
                            // then we can push the left leftover
                            if let Some(theirs_left) = theirs_leftovers.theirs_left {
                                one_item_pair_intersections.push_theirs(theirs_left)
                            }
                            maybe_temp_their_item = theirs_leftovers.theirs_right
                        } else {
                            // there is no more of their item left
                            // just push our partial item
                            one_item_pair_intersections.push_ours(our_partial_item)
                        }
                    }
                    // we need to add the end theirs leftovers
                    if let Some(theirs_left) = maybe_temp_their_item {
                        one_item_pair_intersections.push_theirs(theirs_left)
                    }
                } else {
                    one_item_pair_intersections.push_theirs(their_item)
                }
            }
            result.merge_in(one_item_pair_intersections)
        }
        result
    }

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
                end: RangeSetItem::ExclusiveEnd(range.end.clone()),
            },
            QueryItem::RangeInclusive(range) => RangeSet {
                start: RangeSetItem::Inclusive(range.start().clone()),
                end: RangeSetItem::Inclusive(range.end().clone()),
            },
            QueryItem::RangeFull(..) => RangeSet {
                start: RangeSetItem::UnboundedStart,
                end: RangeSetItem::UnboundedEnd,
            },
            QueryItem::RangeFrom(range) => RangeSet {
                start: RangeSetItem::Inclusive(range.start.clone()),
                end: RangeSetItem::UnboundedEnd,
            },
            QueryItem::RangeTo(range) => RangeSet {
                start: RangeSetItem::UnboundedStart,
                end: RangeSetItem::ExclusiveEnd(range.end.clone()),
            },
            QueryItem::RangeToInclusive(range) => RangeSet {
                start: RangeSetItem::UnboundedStart,
                end: RangeSetItem::Inclusive(range.end.clone()),
            },
            QueryItem::RangeAfter(range) => RangeSet {
                start: RangeSetItem::ExclusiveStart(range.start.clone()),
                end: RangeSetItem::UnboundedEnd,
            },
            QueryItem::RangeAfterTo(range) => RangeSet {
                start: RangeSetItem::ExclusiveStart(range.start.clone()),
                end: RangeSetItem::ExclusiveEnd(range.end.clone()),
            },
            QueryItem::RangeAfterToInclusive(range) => RangeSet {
                start: RangeSetItem::ExclusiveStart(range.start().clone()),
                end: RangeSetItem::Inclusive(range.end().clone()),
            },
        }
    }
}

#[cfg(test)]
mod test {
    use std::ops::{Range, RangeInclusive};

    use crate::proofs::query::query_item::QueryItem;

    #[test]
    pub fn test_range_set_query_item_conversion() {
        assert_eq!(
            QueryItem::Key(vec![5]).to_range_set().to_query_item(),
            QueryItem::Key(vec![5])
        );
        assert_eq!(
            QueryItem::Range(Range {
                start: vec![2],
                end: vec![5]
            })
            .to_range_set()
            .to_query_item(),
            QueryItem::Range(Range {
                start: vec![2],
                end: vec![5]
            })
        );
        assert_eq!(
            QueryItem::RangeInclusive(RangeInclusive::new(vec![2], vec![5]))
                .to_range_set()
                .to_query_item(),
            QueryItem::RangeInclusive(RangeInclusive::new(vec![2], vec![5]))
        );
        assert_eq!(
            QueryItem::RangeFull(..).to_range_set().to_query_item(),
            QueryItem::RangeFull(..)
        );
        assert_eq!(
            QueryItem::RangeFrom(vec![5]..)
                .to_range_set()
                .to_query_item(),
            QueryItem::RangeFrom(vec![5]..)
        );
        assert_eq!(
            QueryItem::RangeTo(..vec![3]).to_range_set().to_query_item(),
            QueryItem::RangeTo(..vec![3])
        );
        assert_eq!(
            QueryItem::RangeToInclusive(..=vec![3])
                .to_range_set()
                .to_query_item(),
            QueryItem::RangeToInclusive(..=vec![3])
        );
        assert_eq!(
            QueryItem::RangeAfter(vec![4]..)
                .to_range_set()
                .to_query_item(),
            QueryItem::RangeAfter(vec![4]..)
        );
        assert_eq!(
            QueryItem::RangeAfterTo(vec![3]..vec![6])
                .to_range_set()
                .to_query_item(),
            QueryItem::RangeAfterTo(vec![3]..vec![6])
        );
        assert_eq!(
            QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])
                .to_range_set()
                .to_query_item(),
            QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])
        );
    }
}
