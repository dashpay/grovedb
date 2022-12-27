use std::ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive};
use crate::proofs::query::query_item::QueryItem;

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
                        Some(QueryItem::Range(Range { start: range.start.clone(), end: their_key.clone() }))
                    };
                    let ours_right = Some(QueryItem::RangeAfterTo(Range { start: their_key.clone(), end: range.end.clone() }));
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
            QueryItem::RangeAfterTo(range) => {
                if range.contains(their_key) && range.start != their_key {
                    let ours_left = Some(QueryItem::Range(Range { start: range.start.clone(), end: their_key.clone() }));
                    let ours_right = Some(QueryItem::RangeAfterTo(Range { start: their_key.clone(), end: range.end.clone() }));
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
                        Some(QueryItem::Range(Range { start: range_inclusive.start().clone(), end: their_key.clone() }))
                    };
                    let ours_right = if range_inclusive.end() == &their_key {
                        None
                    } else {
                        Some(QueryItem::RangeAfterToInclusive(RangeInclusive::new(their_key.clone(), range_inclusive.end().clone())))
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
                    let ours_left = Some(QueryItem::Range(Range { start: range_inclusive.start().clone(), end: their_key.clone() }));
                    if range_inclusive.end() == their_key {
                        QueryItemIntersectionResult {
                            in_both: Some(QueryItem::Key(their_key.clone())),
                            ours_left,
                            ours_right: None,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    } else {
                        let ours_right = Some(QueryItem::RangeAfterToInclusive(RangeInclusive::new(their_key.clone(), range_inclusive.end().clone())));
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
                let ours_left = Some(QueryItem::RangeTo(RangeTo { end: their_key.clone() }));
                let ours_right = Some(QueryItem::RangeAfter(RangeFrom { start: their_key.clone() }));
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
                        let ours_left = Some(QueryItem::Range(Range { start: range_from.start.clone(), end: their_key.clone() }));
                        let ours_right = Some(QueryItem::RangeAfter(RangeFrom { start: their_key.clone() }));
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
                    let ours_left = Some(QueryItem::RangeAfterTo(Range { start: range_from.start.clone(), end: their_key.clone() }));
                    let ours_right = Some(QueryItem::RangeAfter(RangeFrom { start: their_key.clone() }));
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
                    let ours_left = Some(QueryItem::RangeTo(RangeTo { end: their_key.clone() }));
                    let ours_right = Some(QueryItem::RangeAfterTo(Range { start: their_key.clone(), end: range_to.end.clone() }));
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
                            ours_left: Some(QueryItem::RangeTo(RangeTo { end: their_key.clone() })),
                            ours_right: None,
                            theirs_left: None,
                            theirs_right: None,
                        }
                    } else {
                        let ours_left = Some(QueryItem::RangeTo(RangeTo { end: their_key.clone() }));
                        let ours_right = Some(QueryItem::RangeAfterToInclusive(RangeInclusive::new(their_key.clone(), range_to.end.clone())));
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
                let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_key.clone() }));
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom { start: our_key.clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(QueryItem::Key(their_key.clone())),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeFull(_) => {
                QueryItemIntersectionResult {
                    in_both: Some(QueryItem::RangeFull(RangeFull)),
                    ours_left: None,
                    ours_right: None,
                    theirs_left: None,
                    theirs_right: None,
                }
            }
            QueryItem::Range(range) => {
                let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: range.start.clone() }));
                let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {start: range.end.clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: range_inclusive.start().clone() }));
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom {start: range_inclusive.end().clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeAfterTo(range) => {
                let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive { end: range.start.clone() }));
                let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {start: range.end.clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive { end: range_inclusive.start().clone() }));
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom {start: range_inclusive.end().clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right,
                }
            }
            QueryItem::RangeFrom(range_from) => {
                let theirs_left = Some(QueryItem::RangeTo(RangeTo {end: range_from.start.clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right: None,
                }
            }
            QueryItem::RangeAfter(range_after) => {
                let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive {end: range_after.start.clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left,
                    theirs_right: None,
                }
            }
            QueryItem::RangeTo(range_to) => {
                let theirs_right = Some(QueryItem::RangeFrom(RangeFrom {start: range_to.end.clone() }));
                QueryItemIntersectionResult {
                    in_both: Some(self.clone()),
                    ours_left: None,
                    ours_right: None,
                    theirs_left: None,
                    theirs_right,
                }
            }
            QueryItem::RangeToInclusive(range_to_inclusive) => {
                let theirs_right = Some(QueryItem::RangeAfter(RangeFrom {start: range_to_inclusive.end.clone() }));
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

    pub fn intersect_with_range_to(&self, their_range_to: RangeTo<Vec<u8>>) -> QueryItemIntersectionResult {
        match self {
            QueryItem::Key(our_key) => {
                QueryItem::RangeTo(their_range_to).intersect_with_key(our_key).flip()
            }
            QueryItem::RangeFull(_) => {
                QueryItem::RangeTo(their_range_to).intersect_with_range_full().flip()
            }
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
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_range.start.clone() }));
                    let theirs_right = Some(QueryItem::Range(Range { start: their_range_to.end, end: our_range.end.clone() }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::Range(Range { start: our_range.start.clone(), end: their_range_to.end.clone() }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_range.start.clone() }));
                    let ours_right = Some(QueryItem::Range(Range { start: their_range_to.end, end: our_range.end.clone()}));
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
                    let in_both = Some(QueryItem::Range(Range { start: our_range_inclusive.start().clone(), end: our_range_inclusive.end().clone() }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_range.start.clone() }));
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
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_range.start.clone() }));
                    let theirs_right = Some(QueryItem::RangeAfterTo(Range { start: their_range_to.end, end: our_range.end.clone() }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::Range(Range { start: our_range.start.clone(), end: their_range_to.end.clone() }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_range.start.clone() }));
                    let ours_right = Some(QueryItem::RangeInclusive(RangeInclusive::new(their_range_to.end, our_range.end.clone())));
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
                    let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive { end: our_range.start.clone() }));
                    let theirs_right = Some(QueryItem::Range(Range { start: their_range_to.end, end: our_range.end.clone() }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::RangeAfterTo(Range { start: our_range.start.clone(), end: their_range_to.end.clone() }));
                    let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive { end: our_range.start.clone() }));
                    let ours_right = Some(QueryItem::Range(Range { start: their_range_to.end, end: our_range.end.clone()}));
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
                    let in_both = Some(QueryItem::RangeAfterTo(Range { start: our_range_after_to_inclusive.start().clone(), end: our_range_after_to_inclusive.end().clone() }));
                    let theirs_left = Some(QueryItem::RangeToInclusive(RangeToInclusive { end: our_range.start.clone() }));
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
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_range.start.clone() }));
                    let theirs_right = Some(QueryItem::RangeAfterTo(Range { start: their_range_to.end, end: our_range.end.clone() }));
                    QueryItemIntersectionResult {
                        in_both: Some(self.clone()),
                        ours_left: None,
                        ours_right: None,
                        theirs_left,
                        theirs_right,
                    }
                } else {
                    // partial overlap
                    let in_both = Some(QueryItem::RangeAfterTo(Range { start: our_range.start.clone(), end: their_range_to.end.clone() }));
                    let theirs_left = Some(QueryItem::RangeTo(RangeTo { end: our_range.start.clone() }));
                    let ours_right = Some(QueryItem::RangeInclusive(RangeInclusive::new(their_range_to.end, our_range.end.clone())));
                    QueryItemIntersectionResult {
                        in_both,
                        ours_left: None,
                        ours_right,
                        theirs_left,
                        theirs_right: None,
                    }
                }
            }
            QueryItem::RangeAfter(range_after) => {

            }


            QueryItem::RangeFrom(range_from) => {

            }

            QueryItem::RangeTo(range_to) => {

            }
            QueryItem::RangeToInclusive(range_to_inclusive) => {

            }
        }
    }

    pub fn intersect(&self, other: &Self) -> QueryItemIntersectionResult {
        match other {
            QueryItem::Key(key) => { self.intersect_with_key(key)}
            QueryItem::RangeFull(_) => {
                self.intersect_with_range_full()
            }
            QueryItem::RangeTo(range_to) => {
                self.intersect_with_range_to(range_to.clone())
            }
            QueryItem::Range(_) => {}
            QueryItem::RangeInclusive(_) => {}
            QueryItem::RangeFrom(_) => {}

            QueryItem::RangeToInclusive(_) => {}
            QueryItem::RangeAfter(_) => {}
            QueryItem::RangeAfterTo(_) => {}
            QueryItem::RangeAfterToInclusive(_) => {}
        }
    }
}