//! Trait separation, recursive dispatch, derive attributes, and explicit Serde opt-in.
#![cfg(all(feature = "std", feature = "derive"))]

use bincode::{
    de::{BorrowUntrustedDecoder, UntrustedDecoder},
    error::DecodeError,
    DecodeUntrusted,
};
use std::{
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque},
    marker::PhantomData,
};

#[derive(
    Debug, PartialEq, Eq, Ord, PartialOrd, Hash, Clone, bincode::Encode, bincode::DecodeUntrusted,
)]
struct External(u8);

#[derive(Debug, PartialEq, bincode::Encode, bincode::DecodeUntrusted)]
struct Message<T: Ord + std::hash::Hash> {
    // These names must not be rewritten by the macro's trait selection.
    decode: Vec<Option<T>>,
    borrow_decode: (T, Box<T>),
    map: HashMap<T, T>,
    tree: BTreeMap<T, T>,
    array: [T; 2],
}

#[derive(Debug, PartialEq, bincode::Encode, bincode::DecodeUntrusted)]
enum Envelope {
    Empty,
    Tuple(Message<External>),
    Named { value: Option<External> },
}

#[test]
fn untrusted_only_types_compose_and_round_trip_on_every_reader() {
    let config = bincode::config::standard();
    for message in [
        Envelope::Empty,
        Envelope::Named {
            value: Some(External(7)),
        },
        Envelope::Tuple(Message {
            decode: vec![None, Some(External(1))],
            borrow_decode: (External(2), Box::new(External(3))),
            map: HashMap::from([(External(4), External(5))]),
            tree: BTreeMap::from([(External(6), External(7))]),
            array: [External(8), External(9)],
        }),
    ] {
        let bytes = bincode::encode_to_vec(&message, config).unwrap();
        let (owned, used): (Envelope, _) =
            bincode::decode_from_slice_untrusted(&bytes, config).unwrap();
        assert_eq!(owned, message);
        assert_eq!(used, bytes.len());
        let (borrowed, used): (Envelope, _) =
            bincode::borrow_decode_from_slice_untrusted(&bytes, config).unwrap();
        assert_eq!(borrowed, message);
        assert_eq!(used, bytes.len());
        let read: Envelope = bincode::decode_from_reader_untrusted(
            bincode::de::read::SliceReader::new(&bytes),
            config,
        )
        .unwrap();
        assert_eq!(read, message);
        let read: Envelope =
            bincode::decode_from_std_read_untrusted(&mut bytes.as_slice(), config).unwrap();
        assert_eq!(read, message);
    }
}

#[derive(Debug, PartialEq, Eq, Ord, PartialOrd, Hash)]
struct Different(u8);
impl<C> bincode::Decode<C> for Different {
    fn decode<D: bincode::de::Decoder<Context = C>>(_: &mut D) -> Result<Self, DecodeError> {
        Err(DecodeError::Other("ordinary decoder was called"))
    }
}
impl<C> DecodeUntrusted<C> for Different {
    fn decode_untrusted<D: UntrustedDecoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        u8::decode_untrusted(decoder).map(Self)
    }
}
bincode::impl_borrow_decode_untrusted!(Different);

#[test]
fn containers_dispatch_to_untrusted_implementations() {
    fn check<T: DecodeUntrusted<()> + for<'de> bincode::BorrowDecodeUntrusted<'de, ()>>(
        bytes: &[u8],
    ) {
        let config = bincode::config::standard();
        assert!(bincode::decode_from_slice_untrusted::<T, _>(bytes, config).is_ok());
        assert!(bincode::borrow_decode_from_slice_untrusted::<T, _>(bytes, config).is_ok());
    }
    check::<Vec<Different>>(&[1, 7]);
    check::<Option<Different>>(&[1, 7]);
    check::<Result<Different, Different>>(&[0, 7]);
    check::<(Different, Different)>(&[7, 8]);
    check::<[Different; 2]>(&[7, 8]);
    check::<HashMap<Different, Different>>(&[1, 7, 8]);
    check::<BTreeMap<Different, Different>>(&[1, 7, 8]);
    check::<HashSet<Different>>(&[1, 7]);
    check::<BTreeSet<Different>>(&[1, 7]);
    check::<VecDeque<Different>>(&[1, 7]);
    check::<BinaryHeap<Different>>(&[1, 7]);
    check::<Box<[Different]>>(&[1, 7]);
    check::<std::rc::Rc<[Different]>>(&[1, 7]);
    check::<std::sync::Arc<[Different]>>(&[1, 7]);
    check::<Box<Different>>(&[7]);
    check::<std::rc::Rc<Different>>(&[7]);
    check::<std::sync::Arc<Different>>(&[7]);
    check::<std::sync::Mutex<Different>>(&[7]);
    check::<std::sync::RwLock<Different>>(&[7]);
    check::<std::cell::Cell<Different>>(&[7]);
    check::<std::cell::RefCell<Different>>(&[7]);
    check::<std::num::Wrapping<Different>>(&[7]);
    check::<std::cmp::Reverse<Different>>(&[7]);
    check::<std::ops::Range<Different>>(&[7, 8]);
    check::<std::ops::RangeInclusive<Different>>(&[7, 8]);
    check::<std::ops::Bound<Different>>(&[1, 7]);
    assert!(
        bincode::decode_from_slice::<Vec<Different>, _>(&[1, 7], bincode::config::standard())
            .is_err()
    );
}

#[derive(Debug, PartialEq, bincode::Encode, bincode::BorrowDecodeUntrusted)]
struct Borrowed<'a> {
    name: &'a str,
    parts: Vec<&'a [u8]>,
}

#[test]
fn borrowed_fields_refer_to_the_original_buffer() {
    let value = Borrowed {
        name: "client",
        parts: vec![b"payload"],
    };
    let config = bincode::config::standard();
    let bytes = bincode::encode_to_vec(&value, config).unwrap();
    let (decoded, used): (Borrowed<'_>, _) =
        bincode::borrow_decode_from_slice_untrusted(&bytes, config).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(used, bytes.len());
    let range = bytes.as_ptr_range();
    assert!(range.contains(&decoded.name.as_ptr()));
    assert!(range.contains(&decoded.parts[0].as_ptr()));
    let bytes = bincode::encode_to_vec("client", config).unwrap();
    let (borrowed, _): (std::borrow::Cow<'_, str>, _) =
        bincode::borrow_decode_from_slice_untrusted(&bytes, config).unwrap();
    assert!(matches!(borrowed, std::borrow::Cow::Borrowed("client")));
    let (owned, _): (std::borrow::Cow<'_, str>, _) =
        bincode::decode_from_slice_untrusted(&bytes, config).unwrap();
    assert!(matches!(owned, std::borrow::Cow::Owned(_)));
}

#[test]
fn enum_discriminants_and_empty_enums_retain_validation() {
    #[derive(Debug, bincode::DecodeUntrusted)]
    enum Empty {}
    let config = bincode::config::standard();
    assert!(matches!(
        bincode::decode_from_slice_untrusted::<Empty, _>(&[], config),
        Err(DecodeError::EmptyEnum { .. })
    ));
    // Noncanonical u16 encoding of the unit variant's discriminant remains valid.
    assert_eq!(
        bincode::decode_from_slice_untrusted::<Envelope, _>(&[251, 0, 0], config).unwrap(),
        (Envelope::Empty, 3)
    );
    assert!(matches!(
        bincode::borrow_decode_from_slice_untrusted::<Envelope, _>(&[250], config),
        Err(DecodeError::UnexpectedVariant { .. })
    ));
}

#[derive(Debug, PartialEq)]
struct ContextValue(u8);
impl DecodeUntrusted<u8> for ContextValue {
    fn decode_untrusted<D: UntrustedDecoder<Context = u8>>(d: &mut D) -> Result<Self, DecodeError> {
        let context = *d.context();
        Ok(Self(u8::decode_untrusted(d)? + context))
    }
}
impl<'de> bincode::BorrowDecodeUntrusted<'de, u8> for ContextValue {
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = u8>>(
        d: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode_untrusted(d)
    }
}

#[derive(Debug, PartialEq, bincode::DecodeUntrusted)]
#[bincode(decode_context = "u8")]
enum ContextEnum<T> {
    Value(T),
}

use bincode as wire;
#[derive(Debug, PartialEq, bincode::DecodeUntrusted)]
#[bincode(
    crate = "wire",
    decode_bounds = "T: wire::DecodeUntrusted<__Context>",
    borrow_decode_bounds = "T: wire::BorrowDecodeUntrusted<'__de, __Context>"
)]
struct Renamed<T>(T, PhantomData<T>);

#[test]
fn contexts_aliases_and_custom_bounds_are_preserved() {
    let config = bincode::config::standard();
    let (value, _): (ContextEnum<ContextValue>, _) =
        bincode::decode_from_slice_untrusted_with_context(&[0, 7], config, 10u8).unwrap();
    assert_eq!(value, ContextEnum::Value(ContextValue(17)));
    let (value, _): (ContextEnum<ContextValue>, _) =
        bincode::borrow_decode_from_slice_untrusted_with_context(&[0, 7], config, 10u8).unwrap();
    assert_eq!(value, ContextEnum::Value(ContextValue(17)));
    let value: ContextEnum<ContextValue> =
        bincode::decode_from_std_read_untrusted_with_context(&mut [0, 7].as_slice(), config, 10u8)
            .unwrap();
    assert_eq!(value, ContextEnum::Value(ContextValue(17)));
    let (value, _): (Renamed<External>, _) =
        bincode::decode_from_slice_untrusted(&[7], config).unwrap();
    assert_eq!(value, Renamed(External(7), PhantomData));
}

#[cfg(feature = "serde")]
mod serde_tests {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Reviewed {
        values: Vec<u8>,
    }
    // This test opts in the complete, known Vec<u8> Serde graph.
    impl<'de> bincode::serde::DeserializeUntrusted<'de> for Reviewed {}
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct ReviewedBorrowed<'a> {
        value: &'a str,
    }
    impl<'de: 'a, 'a> bincode::serde::DeserializeUntrusted<'de> for ReviewedBorrowed<'a> {}
    #[derive(Debug, PartialEq, bincode::BorrowDecodeUntrusted)]
    struct MixedBorrowed<'a> {
        #[bincode(with_serde)]
        value: ReviewedBorrowed<'a>,
    }
    #[derive(Debug, PartialEq, bincode::Encode, bincode::DecodeUntrusted)]
    struct Mixed {
        #[bincode(with_serde)]
        value: Reviewed,
    }

    struct Seed;
    impl<'de> serde::de::DeserializeSeed<'de> for Seed {
        type Value = u8;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<u8, D::Error> {
            serde::Deserialize::deserialize(d)
        }
    }
    impl<'de> bincode::serde::DeserializeSeedUntrusted<'de> for Seed {}

    #[test]
    fn explicit_serde_support_works_through_fields_constructors_and_seeds() {
        let config = bincode::config::standard();
        let value = Mixed {
            value: Reviewed { values: vec![7, 8] },
        };
        let bytes = bincode::encode_to_vec(&value, config).unwrap();
        let (decoded, used): (Mixed, _) =
            bincode::decode_from_slice_untrusted(&bytes, config).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(used, bytes.len());
        let mut decoder =
            bincode::serde::BorrowedSerdeDecoder::from_slice_untrusted(&bytes, config, ());
        assert_eq!(decoder.decode::<Reviewed>().unwrap(), value.value);
        let mut input = bytes.as_slice();
        let mut decoder =
            bincode::serde::OwnedSerdeDecoder::from_std_read_untrusted(&mut input, config);
        assert_eq!(decoder.decode::<Reviewed>().unwrap(), value.value);
        assert_eq!(
            bincode::serde::seed_decode_from_slice_untrusted(Seed, &[7], config).unwrap(),
            (7, 1)
        );
        let mut decoder =
            bincode::serde::BorrowedSerdeDecoder::from_slice_untrusted(&[7], config, ());
        assert_eq!(decoder.decode_seed(Seed).unwrap(), 7);
        let (value, used): (MixedBorrowed<'_>, _) =
            bincode::borrow_decode_from_slice_untrusted(&[1, b'x'], config).unwrap();
        assert_eq!(value.value.value, "x");
        assert_eq!(used, 2);
    }
}
