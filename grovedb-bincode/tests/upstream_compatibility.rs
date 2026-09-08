//! Verify that the package move preserves the published 2.0.1 wire format.
#![cfg(all(feature = "std", feature = "derive"))]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Debug,
};

#[derive(Debug, PartialEq, bincode::Encode, bincode::Decode, bincode::DecodeUntrusted)]
enum LocalRecord {
    Empty,
    Fields {
        signed: i128,
        unsigned: u128,
        path: Vec<Vec<u8>>,
        flags: Option<Vec<u8>>,
        counts: BTreeMap<String, u64>,
    },
    Wrapped(Box<LocalRecord>),
}

#[derive(Debug, PartialEq, bincode_upstream::Encode, bincode_upstream::Decode)]
#[bincode(crate = "bincode_upstream")]
enum UpstreamRecord {
    Empty,
    Fields {
        signed: i128,
        unsigned: u128,
        path: Vec<Vec<u8>>,
        flags: Option<Vec<u8>>,
        counts: BTreeMap<String, u64>,
    },
    Wrapped(Box<UpstreamRecord>),
}

macro_rules! records {
    ($record:ident) => {{
        let mut records = vec![
            $record::Empty,
            $record::Wrapped(Box::new($record::Wrapped(Box::new($record::Empty)))),
        ];
        for (signed, unsigned) in [
            (0, 0),
            (-1, 250),
            (125, 251),
            (-126, u16::MAX as u128),
            (i16::MIN as i128, u16::MAX as u128 + 1),
            (i32::MAX as i128, u32::MAX as u128 + 1),
            (i64::MIN as i128, u64::MAX as u128 + 1),
            (i128::MIN, u128::MAX),
            (i128::MAX, u64::MAX as u128),
        ] {
            records.push($record::Fields {
                signed,
                unsigned,
                path: vec![Vec::new(), vec![0, 1, 255], vec![7; 65_536]],
                flags: Some(vec![3; 251]),
                counts: BTreeMap::from([
                    (String::new(), 0),
                    ("a".to_string(), u64::MAX),
                    ("tree".to_string(), 65_536),
                ]),
            });
        }
        records.push($record::Fields {
            signed: 0,
            unsigned: 0,
            path: vec![],
            flags: None,
            counts: BTreeMap::new(),
        });
        records
    }};
}

fn compare<C: bincode::config::Config, U: bincode_upstream::config::Config>(local: C, upstream: U) {
    for (local_record, upstream_record) in records!(LocalRecord)
        .into_iter()
        .zip(records!(UpstreamRecord))
    {
        let local_bytes = bincode::encode_to_vec(&local_record, local).unwrap();
        let upstream_bytes = bincode_upstream::encode_to_vec(&upstream_record, upstream).unwrap();
        assert_eq!(local_bytes, upstream_bytes);

        let (decoded_local, consumed): (LocalRecord, usize) =
            bincode::decode_from_slice(&upstream_bytes, local).unwrap();
        assert_eq!(decoded_local, local_record);
        assert_eq!(consumed, upstream_bytes.len());

        let (decoded_upstream, consumed): (UpstreamRecord, usize) =
            bincode_upstream::decode_from_slice(&local_bytes, upstream).unwrap();
        assert_eq!(decoded_upstream, upstream_record);
        assert_eq!(consumed, local_bytes.len());

        let (untrusted, consumed): (LocalRecord, _) =
            bincode::decode_from_slice_untrusted(&upstream_bytes, local).unwrap();
        assert_eq!(untrusted, local_record);
        assert_eq!(consumed, upstream_bytes.len());

        let streamed: LocalRecord =
            bincode::decode_from_std_read(&mut upstream_bytes.as_slice(), local).unwrap();
        assert_eq!(streamed, local_record);
    }

    let borrowed = ("GroveDB", &[0u8, 127, 255][..]);
    let upstream_bytes = bincode_upstream::encode_to_vec(borrowed, upstream).unwrap();
    let local_bytes = bincode::encode_to_vec(borrowed, local).unwrap();
    assert_eq!(local_bytes, upstream_bytes);
    let (decoded, consumed): ((&str, &[u8]), usize) =
        bincode::borrow_decode_from_slice(&upstream_bytes, local).unwrap();
    assert_eq!(decoded, borrowed);
    assert_eq!(consumed, upstream_bytes.len());
    let (decoded, consumed): ((&str, &[u8]), usize) =
        bincode_upstream::borrow_decode_from_slice(&local_bytes, upstream).unwrap();
    assert_eq!(decoded, borrowed);
    assert_eq!(consumed, local_bytes.len());
}

fn compare_collection<T, C, U>(value: &T, local: C, upstream: U)
where
    T: Debug
        + PartialEq
        + bincode::Encode
        + bincode::Decode<()>
        + bincode::DecodeUntrusted<()>
        + for<'de> bincode::BorrowDecodeUntrusted<'de, ()>
        + for<'de> bincode::BorrowDecode<'de, ()>
        + bincode_upstream::Encode
        + bincode_upstream::Decode<()>,
    C: bincode::config::Config + Copy,
    U: bincode_upstream::config::Config + Copy,
{
    // Encode the same value to avoid relying on HashMap/Set iteration order
    // across separately constructed maps. Their wire format is unordered.
    let expected = bincode_upstream::encode_to_vec(value, upstream).unwrap();
    assert_eq!(bincode::encode_to_vec(value, local).unwrap(), expected);
    let original = bincode_upstream::decode_from_slice::<T, _>(&expected, upstream);
    let owned = bincode::decode_from_slice::<T, _>(&expected, local);
    let untrusted_owned = bincode::decode_from_slice_untrusted::<T, _>(&expected, local);
    let untrusted_borrowed = bincode::borrow_decode_from_slice_untrusted::<T, _>(&expected, local);
    let untrusted_streamed =
        bincode::decode_from_std_read_untrusted::<T, _, _>(&mut expected.as_slice(), local);
    let borrowed = bincode::borrow_decode_from_slice::<T, _>(&expected, local);
    let streamed = bincode::decode_from_std_read::<T, _, _>(&mut expected.as_slice(), local);
    match original {
        Ok((decoded, consumed)) => {
            assert_eq!(&decoded, value);
            assert_eq!(consumed, expected.len());
            for result in [owned, untrusted_owned, untrusted_borrowed] {
                let (decoded, consumed) = result.unwrap();
                assert_eq!(&decoded, value);
                assert_eq!(consumed, expected.len());
            }
            assert_eq!(&untrusted_streamed.unwrap(), value);
            let (decoded, consumed) = borrowed.unwrap();
            assert_eq!(&decoded, value);
            assert_eq!(consumed, expected.len());
            assert_eq!(&streamed.unwrap(), value);
        }
        Err(bincode_upstream::error::DecodeError::LimitExceeded) => {
            for result in [owned, untrusted_owned, untrusted_borrowed] {
                assert!(matches!(
                    result,
                    Err(bincode::error::DecodeError::LimitExceeded)
                ));
            }
            assert!(matches!(
                untrusted_streamed,
                Err(bincode::error::DecodeError::LimitExceeded)
            ));
            assert!(matches!(
                borrowed,
                Err(bincode::error::DecodeError::LimitExceeded)
            ));
            assert!(matches!(
                streamed,
                Err(bincode::error::DecodeError::LimitExceeded)
            ));
        }
        Err(error) => panic!("valid control failed: {error}"),
    }
}

fn compare_collections<C, U>(local: C, upstream: U)
where
    C: bincode::config::Config + Copy,
    U: bincode_upstream::config::Config + Copy,
{
    for len in [0, 1, 250, 251, 1023, 1024, 1025, 4097, 65_536] {
        compare_collection(&vec![42u8; len], local, upstream);
    }
    compare_collection(&vec![(); 1025], local, upstream);
    compare_collection(&vec![Box::new(()); 1025], local, upstream);
    compare_collection(&vec![Vec::<u8>::new(); 1025], local, upstream);
    compare_collection(&vec![vec![1u8, 2, 3]; 1025], local, upstream);
    compare_collection(&(0..1025u16).collect::<HashSet<_>>(), local, upstream);
    compare_collection(
        &(0..1025u16)
            .map(|i| (i, vec![i as u8; 3]))
            .collect::<HashMap<_, _>>(),
        local,
        upstream,
    );
}

#[test]
fn collection_growth_preserves_values_and_configured_limits() {
    compare_collections(
        bincode::config::standard(),
        bincode_upstream::config::standard(),
    );
    compare_collections(
        bincode::config::standard().with_big_endian(),
        bincode_upstream::config::standard().with_big_endian(),
    );
    compare_collections(
        bincode::config::legacy(),
        bincode_upstream::config::legacy(),
    );
    compare_collections(
        bincode::config::legacy().with_big_endian(),
        bincode_upstream::config::legacy().with_big_endian(),
    );
    compare_collections(
        bincode::config::standard().with_limit::<1024>(),
        bincode_upstream::config::standard().with_limit::<1024>(),
    );
    compare_collections(
        bincode::config::legacy()
            .with_big_endian()
            .with_limit::<1024>(),
        bincode_upstream::config::legacy()
            .with_big_endian()
            .with_limit::<1024>(),
    );
}

#[test]
fn borrowed_values_and_duplicate_map_keys_remain_compatible() {
    let config = bincode::config::standard();
    let original = bincode_upstream::config::standard();
    let values = vec!["", "GroveDB", "borrowed data"];
    let bytes = bincode_upstream::encode_to_vec(&values, original).unwrap();
    let (decoded, consumed): (Vec<&str>, _) =
        bincode::borrow_decode_from_slice(&bytes, config).unwrap();
    assert_eq!(decoded, values);
    assert_eq!(consumed, bytes.len());
    let (decoded, consumed): (Vec<&str>, _) =
        bincode::borrow_decode_from_slice_untrusted(&bytes, config).unwrap();
    assert_eq!(decoded, values);
    assert_eq!(consumed, bytes.len());

    // A sequence of pairs has the same representation as a map, and can
    // express repeated keys. Preserve last-value-wins decoding.
    let entries = vec![(1u8, 2u8), (1, 3), (2, 4), (1, 5)];
    let bytes = bincode_upstream::encode_to_vec(&entries, original).unwrap();
    let (expected, _): (HashMap<u8, u8>, _) =
        bincode_upstream::decode_from_slice(&bytes, original).unwrap();
    assert_eq!(expected[&1], 5);
    let (decoded, consumed): (HashMap<u8, u8>, _) =
        bincode::decode_from_slice_untrusted(&bytes, config).unwrap();
    assert_eq!(decoded, expected);
    assert_eq!(consumed, bytes.len());
    let (decoded, _): (HashMap<u8, u8>, _) =
        bincode::borrow_decode_from_slice_untrusted(&bytes, config).unwrap();
    assert_eq!(decoded, expected);
    let (decoded, consumed): (HashMap<u8, u8>, _) =
        bincode::decode_from_slice(&bytes, config).unwrap();
    assert_eq!(decoded, expected);
    assert_eq!(consumed, bytes.len());
    let (decoded, _): (HashMap<u8, u8>, _) =
        bincode::borrow_decode_from_slice(&bytes, config).unwrap();
    assert_eq!(decoded, expected);
}

#[test]
fn matches_upstream_in_all_integer_and_byte_order_configurations() {
    compare(
        bincode::config::standard(),
        bincode_upstream::config::standard(),
    );
    compare(
        bincode::config::standard().with_big_endian(),
        bincode_upstream::config::standard().with_big_endian(),
    );
    compare(
        bincode::config::legacy(),
        bincode_upstream::config::legacy(),
    );
    compare(
        bincode::config::legacy().with_big_endian(),
        bincode_upstream::config::legacy().with_big_endian(),
    );
}
