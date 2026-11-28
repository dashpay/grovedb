//! A value as a text that has all of each element in it.
//!
//! The tests of the engine, in the modules beside this one, run the same
//! calls on two engines and compare what they record of each call, as
//! text. Where what is recorded is a value with elements in it, its
//! `Debug` text can be the same for two values that differ. An element
//! that is written short, as it is in a build with the feature
//! `visualize` of its crate, shows the hex of a value of more than seven
//! bytes by the first and the last four of them, with the first bytes of
//! the value as text beside it where the whole value reads as text, and
//! it shows empty flags as none.
//!
//! [`text`] writes a value as `Debug` writes it, and after it the bytes
//! that each element in the value is stored as, in the order in which the
//! value holds them. The `Debug` text says where the keys, the counts and
//! the elements of the value are, and the bytes say all of each element. A
//! value that holds no element is written as `Debug` writes it, and
//! nothing more.
//!
//! The helpers that take a value of any type into a history are bound by
//! [`Whole`], and so is every type that an implementation here looks into:
//! a value of a new type does not compile there until it is said here
//! which elements it holds. What a test records of a call is the test's
//! choice. Some record a part of an answer, such as whether a read
//! succeeded or the message of a refusal, and that part is what is
//! compared.

use std::{
    collections::BTreeMap,
    fmt::{Debug, Write},
};

use grovedb_version::version::GroveVersion;

use crate::{
    operations::{
        proof::{indexed_axis::AxisEntries, util::ProvedPathKeyValue, VerifiedPathQuery},
        QueryItemOrSumReturnType,
    },
    query_result_type::{QueryResultElement, QueryResultElements},
    AggregateSumQueryResult, AxisKeys, Element, Error, IndexedAxisEntry, PathQueryRun, RangePage,
};

/// A value that says which elements it holds.
pub(super) trait Whole: Debug {
    /// Adds every element that the value holds to `held`, in the order
    /// in which its `Debug` text shows them.
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>);
}

/// The whole of `value` as text: what `Debug` writes of it, and then the
/// bytes of each element it holds.
pub(super) fn text<T: Whole + ?Sized>(value: &T) -> String {
    let mut text = format!("{value:?}");
    let mut held = Vec::new();
    value.elements(&mut held);
    if !held.is_empty() {
        text.push_str(" with the elements");
        for element in held {
            let bytes = element.serialize(GroveVersion::latest());
            let bytes = bytes.expect("the bytes of an element");
            write!(text, " {bytes:?}").expect("a string takes what is written to it");
        }
    }
    text
}

/// The types that hold no element: keys, values that are bytes, counts,
/// sums and texts.
macro_rules! holds_no_element {
    ($($plain:ty),* $(,)?) => {$(
        impl Whole for $plain {
            fn elements<'a>(&'a self, _: &mut Vec<&'a Element>) {}
        }
    )*};
}

holds_no_element!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i16,
    i32,
    i64,
    i128,
    str,
    String
);
holds_no_element!(
    AggregateSumQueryResult,
    AxisKeys,
    Error,
    ProvedPathKeyValue,
    QueryItemOrSumReturnType,
    RangePage,
);

impl Whole for Element {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        held.push(self);
    }
}

impl<T: Whole + ?Sized> Whole for &T {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        (**self).elements(held);
    }
}

impl<T: Whole> Whole for Option<T> {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        if let Some(value) = self {
            value.elements(held);
        }
    }
}

impl<T: Whole, E: Whole> Whole for Result<T, E> {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        match self {
            Ok(value) => value.elements(held),
            Err(refusal) => refusal.elements(held),
        }
    }
}

impl<T: Whole> Whole for [T] {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        for value in self {
            value.elements(held);
        }
    }
}

impl<T: Whole> Whole for Vec<T> {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        self.as_slice().elements(held);
    }
}

impl<T: Whole, const N: usize> Whole for [T; N] {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        self.as_slice().elements(held);
    }
}

impl<K: Whole, V: Whole> Whole for BTreeMap<K, V> {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        for (key, value) in self {
            key.elements(held);
            value.elements(held);
        }
    }
}

macro_rules! of_each_part {
    ($(($($part:ident $at:tt),+))*) => {$(
        impl<$($part: Whole),+> Whole for ($($part,)+) {
            fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
                $(self.$at.elements(held);)+
            }
        }
    )*};
}

of_each_part! {
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
}

impl Whole for QueryResultElement {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        match self {
            Self::ElementResultItem(element)
            | Self::KeyElementPairResultItem((_, element))
            | Self::PathKeyElementTrioResultItem((_, _, element)) => held.push(element),
        }
    }
}

impl Whole for QueryResultElements {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        self.elements.elements(held);
    }
}

/// An entry of an axis: what it is ordered by, its key, and its element.
impl<T: Whole> Whole for IndexedAxisEntry<T> {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        self.ordering_value.elements(held);
        held.push(&self.value);
    }
}

impl Whole for AxisEntries {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        match self {
            Self::Count(entries) => entries.elements(held),
            Self::Sum(entries) => entries.elements(held),
            Self::Avg(entries) | Self::BigSum(entries) => entries.elements(held),
        }
    }
}

/// What a path query returned. The rows of a key selection and the
/// entries of an axis hold elements; every other shape holds keys, counts
/// and sums. Each shape is named here, so that a new one says which of
/// the two it is.
impl Whole for PathQueryRun {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        match self {
            Self::Elements { elements, .. } => elements.elements(held),
            Self::AxisEntries { entries, .. } => entries.elements(held),
            Self::BranchedAxisEntries(branches) => branches.elements(held),
            Self::AggregateCount(_)
            | Self::AggregateSum(_)
            | Self::AggregateCountAndSum { .. }
            | Self::AggregateCountPerKey(_)
            | Self::AggregateSumPerKey(_)
            | Self::AggregateCountAndSumPerKey(_)
            | Self::AxisKeys { .. }
            | Self::BranchedAxisKeys(_)
            | Self::AxisRank(_)
            | Self::AxisAggregate(_)
            | Self::SumBudget(_) => {}
        }
    }
}

/// What the verifier of a path query made of a proof, shape by shape as
/// [`PathQueryRun`].
impl Whole for VerifiedPathQuery {
    fn elements<'a>(&'a self, held: &mut Vec<&'a Element>) {
        match self {
            Self::Elements { elements, .. } => elements.elements(held),
            Self::AxisEntries { entries, .. } => entries.elements(held),
            Self::BranchedAxisEntries { branches, .. } => branches.elements(held),
            Self::AggregateCount { .. }
            | Self::AggregateSum { .. }
            | Self::AggregateCountAndSum { .. }
            | Self::AggregatePerKey { .. }
            | Self::AxisRank { .. }
            | Self::AxisAggregate { .. }
            | Self::SumBudget { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two items that differ in one byte of the middle of a long value
    /// that does not read as text, and an item with no flags beside one
    /// with empty flags, are not written alike: alone, inside what a
    /// read returns, as a refusal, and as what an entry of an axis is
    /// ordered by. (Where an element's `Debug` text is the short one,
    /// each pair has one `Debug` text.)
    #[test]
    fn elements_that_differ_are_written_differently() {
        let mut other = vec![0x80; 32];
        other[16] = 0x81;
        let pairs = [
            (Element::new_item(vec![0x80; 32]), Element::new_item(other)),
            (
                Element::new_item(vec![1, 2, 3]),
                Element::new_item_with_flags(vec![1, 2, 3], Some(Vec::new())),
            ),
        ];
        for (one, two) in pairs {
            assert_ne!(one, two);
            assert_ne!(text(&one), text(&two));
            let read = |element: &Element| {
                let found = QueryResultElement::KeyElementPairResultItem((
                    b"key".to_vec(),
                    element.clone(),
                ));
                let elements = QueryResultElements {
                    elements: vec![found],
                };
                Ok::<_, crate::Error>((Some(element.clone()), elements, 0u16))
            };
            assert_ne!(text(&read(&one)), text(&read(&two)));
            // What is written begins with the `Debug` text.
            assert!(text(&read(&one)).starts_with(&format!("{:?}", read(&one))));
            let refused = |element: &Element| Err::<(), _>(element.clone());
            assert_ne!(text(&refused(&one)), text(&refused(&two)));
            let ordered = |element: &Element| IndexedAxisEntry {
                ordering_value: element.clone(),
                primary_key: b"key".to_vec(),
                value: Element::new_item(vec![7]),
            };
            assert_ne!(text(&ordered(&one)), text(&ordered(&two)));
        }
    }

    /// A value that holds no element is written as `Debug` writes it.
    #[test]
    fn what_holds_no_element_is_written_as_debug_writes_it() {
        let plain = Ok::<_, crate::Error>((
            vec![vec![7u8, 7, 9], Vec::new()],
            Some([3u8; 4]),
            (true, -5i64, "text".to_owned()),
            (),
        ));
        assert_eq!(text(&plain), format!("{plain:?}"));
        let none: Option<Element> = None;
        assert_eq!(text(&none), "None");
        let refused: Result<Element, &str> = Err("refused");
        assert_eq!(text(&refused), format!("{refused:?}"));
    }
}
