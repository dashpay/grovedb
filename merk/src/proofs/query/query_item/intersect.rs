use std::ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive};

use crate::{proofs::query::query_item::QueryItem, Op::Put};

#[derive(Default)]
pub struct QueryItemIntersectionResult {
    in_both: Option<QueryItem>,
    ours_left: Option<QueryItem>,
    ours_right: Option<QueryItem>,
    theirs_left: Option<QueryItem>,
    theirs_right: Option<QueryItem>,
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

impl QueryItem {
    pub fn intersect_with_key(&self, their_key: &Vec<u8>) -> QueryItemIntersectionResult {
        match self {
            QueryItem::Key(key) => {
                if key == their_key {
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right: None,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
            QueryItem::Range(range) => {
                if range.contains(their_key) {
                    let ours_left = if range.start == their_key {
                        None
                    } else {
                        Some(QueryItem::Range(Range {
                            start: range.start.clone(),
                            end: their_key.clone(),
                        }))
                    };
                    let ours_right = Some(QueryItem::RangeAfterTo(Range {
                        start: their_key.clone(),
                        end: range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(QueryItem::Key(their_key.clone())),
                        ours_left,
                        ours_right,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    if their_key < range.start {
                        QueryItemIntersectionResult {
                            in_both: None,
                            ours_left: Some(self.clone()),
                            ours_right,
                            theirs_left: None,
                            theirs_right: Some(QueryItem::Key(their_key.clone())),
                        }
                    } else {
                        QueryItemIntersectionResult {
                            in_both: None,
                            ours_left: Some(self.clone()),
                            ours_right,
                            theirs_left: None,
                            theirs_right: Some(QueryItem::Key(their_key.clone())),
                        }
                    }
                }
            }
            QueryItem::RangeAfterTo(range) => {
                if range.contains(their_key) && range.start != their_key {
                    let ours_left = Some(QueryItem::Range(Range {
                        start: range.start.clone(),
                        end: their_key.clone(),
                    }));
                    let ours_right = Some(QueryItem::RangeAfterTo(Range {
                        start: their_key.clone(),
                        end: range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(QueryItem::Key(their_key.clone())),
                        ours_left,
                        ours_right,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                if range_inclusive.contains(&their_key) {
                    let ours_left = if range_inclusive.start() == &their_key {
                        None
                    } else {
                        Some(QueryItem::Range(Range {
                            start: range_inclusive.start().clone(),
                            end: their_key.clone(),
                        }))
                    };
                    let ours_right = if range_inclusive.end() == &their_key {
                        None
                    } else {
                        Some(QueryItem::RangeAfterToInclusive(RangeInclusive::new(
                            their_key.clone(),
                            range_inclusive.end().clone(),
                        )))
                    };
                    QueryItemIntersectionResult {
                        in_both: Some(QueryItem::Key(their_key.clone())),
                        ours_left,
                        ours_right,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                if range_inclusive.contains(their_key) && range_inclusive.start() != their_key {
                    let ours_left = Some(QueryItem::Range(Range {
                        start: range_inclusive.start().clone(),
                        end: their_key.clone(),
                    }));
                    if range_inclusive.end() == their_key {
                        QueryItemIntersectionResult {
                            in_both: Some(QueryItem::Key(their_key.clone())),
                            ours_left,
                            ours_right: None,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    } else {
                        let ours_right = Some(QueryItem::RangeAfterToInclusive(
                            RangeInclusive::new(their_key.clone(), range_inclusive.end().clone()),
                        ));
                        QueryItemIntersectionResult {
                            in_both: Some(QueryItem::Key(their_key.clone())),
                            ours_left,
                            ours_right,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeFull(_) => {
                let ours_left = Some(QueryItem::RangeTo(RangeTo {
                    end: their_key.clone(),
                }));
                let ours_right = Some(QueryItem::RangeAfter(RangeFrom {
                    start: their_key.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(QueryItem::Key(their_key.clone())),
                    ours_left,
                    ours_right,
                    theirs_left: None,
                    theirs_right: None,
                }
            }
            QueryItem::RangeFrom(range_from) => {
                if range_from.contains(their_key) {
                    if range_from.start == their_key {
                        // Just remove first element, by going to a range after
                        QueryItemIntersectionResult {
                            in_both: Some(QueryItem::Key(their_key.clone())),
                            ours_left: Some(QueryItem::RangeAfter(range_from.clone())),
                            ours_right: None,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    } else {
                        let ours_left = Some(QueryItem::Range(Range {
                            start: range_from.start.clone(),
                            end: their_key.clone(),
                        }));
                        let ours_right = Some(QueryItem::RangeAfter(RangeFrom {
                            start: their_key.clone(),
                        }));
                        QueryItemIntersectionResult {
                            in_both: Some(QueryItem::Key(their_key.clone())),
                            ours_left,
                            ours_right,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right: None,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeAfter(range_after) => {
                if range_after.contains(&their_key) && range_after.start != their_key {
                    let ours_left = Some(QueryItem::RangeAfterTo(Range {
                        start: range_from.start.clone(),
                        end: their_key.clone(),
                    }));
                    let ours_right = Some(QueryItem::RangeAfter(RangeFrom {
                        start: their_key.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(QueryItem::Key(their_key.clone())),
                        ours_left,
                        ours_right,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right: None,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeTo(range_to) => {
                if range_to.contains(their_key) {
                    let ours_left = Some(QueryItem::RangeTo(RangeTo {
                        end: their_key.clone(),
                    }));
                    let ours_right = Some(QueryItem::RangeAfterTo(Range {
                        start: their_key.clone(),
                        end: range_to.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(QueryItem::Key(their_key.clone())),
                        ours_left,
                        ours_right,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right: None,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeToInclusive(range_to_inclusive) => {
                if range_to_inclusive.contains(their_key) {
                    if range_to.end == their_key {
                        // Just remove first element, by going to a range after
                        QueryItemIntersectionResult {
                            in_both: Some(QueryItem::Key(their_key.clone())),
                            ours_left: Some(QueryItem::RangeTo(RangeTo {
                                end: their_key.clone(),
                            })),
                            ours_right: None,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    } else {
                        let ours_left = Some(QueryItem::RangeTo(RangeTo {
                            end: their_key.clone(),
                        }));
                        let ours_right = Some(QueryItem::RangeAfterToInclusive(
                            RangeInclusive::new(their_key.clone(), range_to.end.clone()),
                        ));
                        QueryItemIntersectionResult {
                            in_both: Some(QueryItem::Key(their_key.clone())),
                            ours_left,
                            ours_right,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    }
                } else {
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(self.clone()),
                        ours_right: None,
                        theirs_left: Some(QueryItem::Key(their_key.clone())),
                        theirs_right: None,
                    }
                }
            }
        }
    }

    pub fn intersect_with_range_full(&self) -> QueryItemIntersectionResult {
        match self {
            QueryItem::Key(our_key) => {
                let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                    end: our_key.clone(),
                }));
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom {
                    start: our_key.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(QueryItem::Key(their_key.clone())),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeFull(_) => QueryItemIntersectionResult {
                in_both: Some(QueryItem::RangeFull(RangeFull)),
                ours_left: None,
                ours_right: None,
                theirs_left: None,
                theirs_right: None,
            },
            QueryItem::Range(range) => {
                let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                    end: range.start.clone(),
                }));
                let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {
                    start: range.end.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                    end: range_inclusive.start().clone(),
                }));
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom {
                    start: range_inclusive.end().clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeAfterTo(range) => {
                let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {
                    end: range.start.clone(),
                }));
                let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {
                    start: range.end.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {
                    end: range_inclusive.start().clone(),
                }));
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom {
                    start: range_inclusive.end().clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeFrom(range_from) => {
                let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                    end: range_from.start.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right: None,
                }
            }
            QueryItem::RangeAfter(range_after) => {
                let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {
                    end: range_after.start.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right: None,
                }
            }
            QueryItem::RangeTo(range_to) => {
                let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {
                    start: range_to.end.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left: None,
                    theirs_right,
                }
            }
            QueryItem::RangeToInclusive(range_to_inclusive) => {
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom {
                    start: range_to_inclusive.end.clone(),
                }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left: None,
                    theirs_right,
                }
            }
        }
    }

    pub fn intersect_with_range_to(
        &self,
        their_range_to: RangeTo<Vec<u8>>,
    ) -> QueryItemIntersectionResult {
        match self {
            QueryItem::Key(our_key) => QueryItem::RangeTo(their_range_to)
                .intersect_with_key(our_key)
                .flip(),
            QueryItem::RangeFull(_) => QueryItem::RangeTo(their_range_to)
                .intersect_with_range_full()
                .flip(),
            QueryItem::Range(our_range) => {
                if their_range_to.end <= our_range.start {
                    // there is no overlap, their end is not inclusive
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: None,
                        ours_right: Some(self.clone()),
                        theirs_left: Some(QueryItem::RangeTo(their_range_to)),
                        theirs_right: None,
                    }
                } else if their_range_to.end >= our_range.end {
                    // complete overlap for us
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::Range(Range {
                        start: their_range_to.end,
                        end: our_range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::Range(Range {
                        start: our_range.start.clone(),
                        end: their_range_to.end.clone(),
                    }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range.start.clone(),
                    }));
                    let ours_right = Some(QueryItem::Range(Range {
                        start: their_range_to.end,
                        end: our_range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right,
                        theirs_left,
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeInclusive(our_range_inclusive) => {
                if &their_range_to.end <= our_range_inclusive.start() {
                    // there is no overlap, their end is not inclusive
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: None,
                        ours_right: Some(self.clone()),
                        theirs_left: Some(QueryItem::RangeTo(their_range_to)),
                        theirs_right: None,
                    }
                } else if their_range_to.end == our_range_inclusive.end() {
                    // complete overlap for us, except last element
                    let in_both = Some(QueryItem::Range(Range {
                        start: our_range_inclusive.start().clone(),
                        end: our_range_inclusive.end().clone(),
                    }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::Key(their_range_to.end));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else if &their_range_to.end > our_range_inclusive.end() {
                    // complete overlap for us
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::RangeAfterTo(Range {
                        start: their_range_to.end,
                        end: our_range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::Range(Range {
                        start: our_range.start.clone(),
                        end: their_range_to.end.clone(),
                    }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range.start.clone(),
                    }));
                    let ours_right = Some(QueryItem::RangeInclusive(RangeInclusive::new(
                        their_range_to.end,
                        our_range.end.clone(),
                    )));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right,
                        theirs_left,
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeAfterTo(our_range) => {
                if their_range_to.end <= our_range.start {
                    // there is no overlap, their end is not inclusive
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: None,
                        ours_right: Some(self.clone()),
                        theirs_left: Some(QueryItem::RangeTo(their_range_to)),
                        theirs_right: None,
                    }
                } else if their_range_to.end >= our_range.end {
                    // complete overlap for us
                    let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {
                        end: our_range.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::Range(Range {
                        start: their_range_to.end,
                        end: our_range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::RangeAfterTo(Range {
                        start: our_range.start.clone(),
                        end: their_range_to.end.clone(),
                    }));
                    let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {
                        end: our_range.start.clone(),
                    }));
                    let ours_right = Some(QueryItem::Range(Range {
                        start: their_range_to.end,
                        end: our_range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right,
                        theirs_left,
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeAfterToInclusive(our_range_after_to_inclusive) => {
                if &their_range_to.end <= our_range_after_to_inclusive.start() {
                    // there is no overlap, their end is not inclusive
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: None,
                        ours_right: Some(self.clone()),
                        theirs_left: Some(QueryItem::RangeTo(their_range_to)),
                        theirs_right: None,
                    }
                } else if their_range_to.end == our_range_after_to_inclusive.end() {
                    // complete overlap for us, except last element
                    let in_both = Some(QueryItem::RangeAfterTo(Range {
                        start: our_range_after_to_inclusive.start().clone(),
                        end: our_range_after_to_inclusive.end().clone(),
                    }));
                    let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {
                        end: our_range.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::Key(their_range_to.end));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else if &their_range_to.end > our_range_after_to_inclusive.end() {
                    // complete overlap for us
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::RangeAfterTo(Range {
                        start: their_range_to.end,
                        end: our_range.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::RangeAfterTo(Range {
                        start: our_range.start.clone(),
                        end: their_range_to.end.clone(),
                    }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range.start.clone(),
                    }));
                    let ours_right = Some(QueryItem::RangeInclusive(RangeInclusive::new(
                        their_range_to.end,
                        our_range.end.clone(),
                    )));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right,
                        theirs_left,
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeAfter(our_range_after) => {
                // intersecting range after with range to
                // will only overlap if a <= b
                // a is range after, b is range to
                // a is not inclusive, a+ is what we care about
                // we can't have equal right as we don't know what the step size is
                // the overlap will be from a to b, but inverted a right
                // oh, we could also just use theirs but in a range item that doesn't care about
                // the inclusive nature
                if their_range_to.end <= our_range_after.start {
                    // no overlap
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(QueryItem::RangeTo(their_range_to)),
                        ours_right: None,
                        theirs_left: None,
                        theirs_right: Some(QueryItem::RangeAfter(our_range_after.to_owned())),
                    }
                } else {
                    // we have an overlap,
                    // we have a range to from left to a inclusive
                    // then one from a non inclusive to  b non inclusive
                    // finally one from b inclusive to right
                    let in_both = Some(QueryItem::RangeAfterTo(Range {
                        start: our_range_after.start.clone(),
                        end: their_range_to.end.clone(),
                    }));
                    let ours_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {
                        end: our_range_after.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {
                        start: their_range_to.end,
                    }));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left,
                        ours_right: None,
                        theirs_left: None,
                        theirs_right,
                    }
                }
            }
            QueryItem::RangeFrom(our_range_from) => {
                // intersecting range from and range to
                // range_from has a as inclusive
                // check for no overlap first
                if our_range_from.start >= their_range_to.end {
                    // no overlap
                    QueryItemIntersectionResult {
                        in_both: None,
                        ours_left: Some(QueryItem::RangeTo(their_range_to)),
                        ours_right: None,
                        theirs_left: None,
                        theirs_right: Some(QueryItem::RangeFrom(our_range_from.to_owned())),
                    }
                } else {
                    // overlap
                    // we have from left to non inclusive of range from start
                    // for common, we have range_from start inclusive to range to end non inclusive
                    // then range_to end inclusive to right
                    let in_both = Some(QueryItem::Range(Range {
                        start: our_range_from.start.clone(),
                        end: their_range_to.end.clone(),
                    }));
                    let ours_left = Some(QueryItem::RangeTo(RangeTo {
                        end: our_range_from.start.clone(),
                    }));
                    let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {
                        start: their_range_to.end.clone(),
                    }));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left,
                        ours_right: None,
                        theirs_left: None,
                        theirs_right,
                    }
                }
            }
            QueryItem::RangeTo(our_range_to) => {
                // intersecting range to with range to
                // they both start from the same point so there should always be an intersection
                // nothing to our left or their left as we all come from extreme left
                // so in_both and someone's right
                // in_both to the smaller one, then from smaller one to the bigger one
                // in_both will be rangeTo
                if our_range_to.end <= their_range_to.end {
                    // our_range is the smaller one
                    let in_both = Some(QueryItem::RangeTo(our_range_to.to_owned()));
                    let ours_right = Some(QueryItem::Range(Range {
                        start: our_range_to.end.clone(),
                        end: their_range_to.end,
                    }));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    // their range is the smaller one
                    let in_both = Some(QueryItem::RangeTo(their_range_to.to_owned()));
                    let theirs_right = Some(QueryItem::Range(Range {
                        start: our_range_to.end.clone(),
                        end: their_range_to.end,
                    }));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right: None,
                        theirs_left: None,
                        theirs_right,
                    }
                }
            }
            QueryItem::RangeToInclusive(our_range_to_inclusive) => {
                // intersecting range_to_inclusive with range_to
                // similar to the one above, we'd just use a rangeInclusive for one
                if our_range_to_inclusive.end < their_range_to.end {
                    // our_range is the smaller one
                    let in_both = Some(QueryItem::RangeToInclusive(
                        our_range_to_inclusive.to_owned(),
                    ));
                    let ours_right = Some(QueryItem::RangeAfterTo(Range {
                        start: our_range_to_inclusive.end.clone(),
                        end: their_range_to.end,
                    }));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right,
                        theirs_left: None,
                        theirs_right: None,
                    }
                } else {
                    // their range is the smaller one
                    let in_both = Some(QueryItem::RangeTo(their_range_to.clone()));
                    let theirs_right = Some(QueryItem::RangeInclusive(RangeInclusive::new(
                        their_range_to.end,
                        our_range_to_inclusive.end.clone(),
                    )));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right: None,
                        theirs_left: None,
                        theirs_right,
                    }
                }
            }
        }
    }

    pub fn intersect_with_range(&self, their_range: Range<Vec<u8>>) -> QueryItemIntersectionResult {
        match self {
            QueryItem::Key(our_key) => QueryItem::Range(their_range)
                .intersect_with_key(our_key)
                .flip(),
            QueryItem::RangeFull(_) => QueryItem::Range(their_range)
                .intersect_with_range_full()
                .flip(),
            QueryItem::RangeTo(our_range_to) => QueryItem::Range(their_range)
                .intersect_with_range_to(our_range_to.clone())
                .flip(),
            QueryItem::Range(our_range) => {
                // intersecting a range with another range both ends not inclusive
                // might not overlap at all
                if our_range.end <= their_range.start || their_range.end <= our_range.start {
                    // no overlap, determine which is smaller to know right or left
                    if our_range.end <= their_range.start {
                        // our range is to the left
                        QueryItemIntersectionResult {
                            in_both: None,
                            ours_left: None,
                            ours_right: Some(QueryItem::Range(their_range)),
                            theirs_left: Some(QueryItem::Range(our_range.to_owned())),
                            theirs_right: None,
                        }
                    } else {
                        // their range is to the left
                        QueryItemIntersectionResult {
                            in_both: None,
                            ours_left: Some(QueryItem::Range(their_range)),
                            ours_right: None,
                            theirs_left: None,
                            theirs_right: Some(QueryItem::Range(our_range.to_owned())),
                        }
                    }
                } else {
                    // there is an overlap
                    // get the smaller start and construct a range (non inclusive end) to other
                    // start for common construct from start of other (ref
                    // above) to smaller end for right construct from the
                    // inverse of smaller end to bigger end

                    let mut intersection_result = QueryItemIntersectionResult::default();

                    // the starts could be equal tho, in that case nothing on the left
                    if our_range.start < their_range.start {
                        our_right
                    }
                }
            }
            QueryItem::RangeInclusive(_) => {}
            QueryItem::RangeFrom(_) => {}
            QueryItem::RangeToInclusive(_) => {}
            QueryItem::RangeAfter(_) => {}
            QueryItem::RangeAfterTo(_) => {}
            QueryItem::RangeAfterToInclusive(_) => {}
        }
    }

    pub fn intersect(&self, other: &Self) -> QueryItemIntersectionResult {
        match other {
            QueryItem::Key(key) => self.intersect_with_key(key),
            QueryItem::RangeFull(_) => self.intersect_with_range_full(),
            QueryItem::RangeTo(range_to) => self.intersect_with_range_to(range_to.clone()),
            QueryItem::Range(range) => self.intersect_with_range(range.clone()),
            QueryItem::RangeInclusive(_) => {}
            QueryItem::RangeFrom(_) => {}

            QueryItem::RangeToInclusive(_) => {}
            QueryItem::RangeAfter(_) => {}
            QueryItem::RangeAfterTo(_) => {}
            QueryItem::RangeAfterToInclusive(_) => {}
        }
    }
}
