//! Verify that the package move preserves the published 2.0.1 wire format.
#![cfg(all(feature = "std", feature = "derive"))]

use std::collections::BTreeMap;

#[derive(Debug, PartialEq, bincode::Encode, bincode::Decode)]
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
