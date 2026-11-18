//! The values of a group: one record or several, or the name of a carried
//! value, and what stands at the address of a pending value.

use super::{value::read_with, *};
use crate::value_log::ValueLogError;

/// Records that carry at most `part` bytes of a value each.
fn parts_of(part: usize) -> Limits {
    Limits {
        max_payload: 11 + part,
    }
}

fn group_with(limits: Limits, owner: u32, value: &[u8]) -> (Vec<u8>, PendingValue) {
    let mut out = Vec::new();
    let mut group = GroupWriter::with_limits(version(1), address(1000), limits, &mut out).unwrap();
    let pending = group.value(page(owner), value).unwrap();
    group.finish();
    (out, pending)
}

#[test]
fn a_value_is_cut_into_records_of_one_size() {
    for part in 1..=7usize {
        let limits = parts_of(part);
        for len in 1..=40usize {
            let value: Vec<u8> = (0..len).map(|at| at as u8 ^ 0x5A).collect();
            let (group, pending) = group_with(limits, 7, &value);
            assert_eq!(pending.value_len(), len as u32);
            assert_eq!(LogAddress::of(&pending), Ok(address(1000)));

            let parts = len.div_ceil(part);
            let extent = len + parts * VALUE_RECORD_HEADER_LEN;
            let (read, read_len) = read_with(&group, V1, limits).unwrap();
            assert_eq!(read_len, extent);
            let LoggedValue::Bytes(bytes) = read else {
                panic!("the bytes");
            };
            assert_eq!((bytes.owner(), bytes.value_len()), (page(7), len as u32));
            assert_eq!(read.owner(), page(7));
            assert_eq!(read.value_len(), len as u32);
            assert_eq!(read.bytes(), Some(bytes));
            assert_eq!(read.carried(), None);
            assert_eq!(bytes.to_vec(), value);
            assert_eq!(bytes.parts().count(), parts);
            assert_eq!(
                bytes.contiguous(),
                (parts == 1).then_some(&value[..]),
                "{len} in parts of {part}"
            );

            // Each record is a value record of its part, full but for the
            // last, and the next record follows it at once.
            let mut at = 0;
            for number in 0..parts {
                let (Record::Value(record), record_len) =
                    Record::decode_with(&group[at..], V1, limits).unwrap()
                else {
                    panic!("a value record");
                };
                let expected = &value[number * part..len.min((number + 1) * part)];
                assert_eq!(record.data(), expected);
                assert_eq!(record.part(), number as u16);
                assert_eq!(record.value_len(), len as u32);
                assert_eq!(record.owner(), page(7));
                assert_eq!(record.is_last(), number + 1 == parts);
                assert_eq!(record_len, VALUE_RECORD_HEADER_LEN + expected.len());
                at += record_len;
            }
            assert_eq!(at, extent);
            assert!(matches!(decode(&group[at..]), Ok((Record::Part(_), _))));
        }
    }
}

#[test]
fn the_parts_of_a_value_come_whole_in_order_and_from_one_value() {
    let limits = parts_of(4);
    let record =
        |owner: u32, part: u16, len: u32, data: &[u8]| value_record(owner, part, len, data);
    let read = |records: &[Vec<u8>]| read_with(&records.concat(), V1, limits).map(|(_, len)| len);
    let whole = [
        record(7, 0, 10, b"abcd"),
        record(7, 1, 10, b"efgh"),
        record(7, 2, 10, b"ij"),
    ];
    assert_eq!(read(&whole), Ok(whole.concat().len()));

    assert_eq!(
        read(&whole[1..]),
        Err(wal_error(WalError::UnexpectedValuePart {
            expected: 0,
            found: 1,
        }))
    );
    assert_eq!(
        read(&[whole[0].clone(), whole[2].clone()]),
        Err(wal_error(WalError::UnexpectedValuePart {
            expected: 1,
            found: 2,
        }))
    );
    assert_eq!(
        read(&[whole[0].clone(), whole[0].clone()]),
        Err(wal_error(WalError::UnexpectedValuePart {
            expected: 1,
            found: 0,
        }))
    );
    for other in [record(8, 1, 10, b"efgh"), record(7, 1, 11, b"efgh")] {
        assert_eq!(
            read(&[whole[0].clone(), other]),
            Err(wal_error(WalError::PartOfAnotherValue))
        );
    }

    // The value ends before its last part.
    let two = whole[..2].concat();
    assert_eq!(
        read(&whole[..2]),
        Err(FormatError::Truncated {
            needed: two.len() + 8,
            available: two.len(),
        })
    );
    let mut mark = Vec::new();
    CheckpointMark::new(CommitVersion::EMPTY)
        .write(&mut mark)
        .unwrap();
    assert_eq!(
        read(&[whole[0].clone(), mark.clone()]),
        Err(wal_error(WalError::UnfinishedValue))
    );
    // A damaged later part is damage, counted from the value's first byte.
    let mut damaged = whole.concat();
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    assert!(matches!(
        read_with(&damaged, V1, limits),
        Err(FormatError::ChecksumMismatch { .. })
    ));

    // What stands at an address is a value or a carried value.
    assert_eq!(
        read(&[mark]),
        Err(wal_error(WalError::NotAValue { kind: 6 }))
    );
    assert_eq!(
        read(&[part_record(1, 1, 0, &[])]),
        Err(wal_error(WalError::NotAValue { kind: 4 }))
    );
    assert_eq!(
        read(&[]),
        Err(FormatError::Truncated {
            needed: 8,
            available: 0,
        })
    );
}

#[test]
fn a_pending_value_leads_to_its_bytes_or_to_the_value_it_was_carried_from() {
    let text = b"a value too long for its cell";
    let start = LogStart::new(5000, version(3)).unwrap();
    let mut log = new_log(1, &start);
    let carried = CarriedValue::new(page(9), page(7), pointer(13, 64, 300));
    let mut held = Vec::new();
    push_group(&mut log, &start, Limits::FORMAT, 4, |group| {
        held.push(group.value(page(7), text).unwrap());
        held.push(group.carried(&carried).unwrap());
        held.push(group.value(page(8), b"x").unwrap());
    });

    // The cell of each value holds the address of the value's first
    // record, which is the place of that record in its file.
    let offsets: Vec<u64> = held
        .iter()
        .map(|pending| {
            let address = LogAddress::of(pending).unwrap();
            start.offset_of(address).expect("in this file")
        })
        .collect();
    assert_eq!(offsets, [45, 45 + 19 + 29, 45 + 19 + 29 + 33]);
    assert_eq!(LogAddress::of(&held[0]), Ok(address(5045)));
    assert_eq!(held[1].value_len(), 300);

    let at = |pending: &PendingValue| {
        let offset = start.offset_of(LogAddress::of(pending).unwrap()).unwrap() as usize;
        let len = logged_read_len(pending.value_len(), V1) as usize;
        &log[offset..log.len().min(offset + len)]
    };
    let read = read_pending_value(&held[0], at(&held[0]), page(7), V1).unwrap();
    assert_eq!(
        read.bytes().and_then(|bytes| bytes.contiguous()),
        Some(&text[..])
    );
    let read = read_pending_value(&held[1], at(&held[1]), page(9), V1).unwrap();
    assert_eq!(read, LoggedValue::Carried(carried));
    let read = read_pending_value(&held[2], at(&held[2]), page(8), V1).unwrap();
    assert_eq!(
        read.bytes().map(|bytes| bytes.to_vec()),
        Some(b"x".to_vec())
    );

    // The same addresses are what a reader of the file finds.
    let (items, _) = items(&log);
    let [LogItem::Commit(group)] = items[..] else {
        panic!("one group");
    };
    let found: Vec<_> = group
        .values()
        .map(|(at, value)| at.pending(value.value_len()).unwrap())
        .collect();
    assert_eq!(found, held);

    // A pending value that leads to another value is not that value.
    assert_eq!(
        read_pending_value(&held[0], at(&held[0]), page(8), V1),
        Err(wal_error(WalError::WrongOwner {
            expected: page(8),
            found: page(7),
        }))
    );
    assert_eq!(
        read_pending_value(&held[1], at(&held[1]), page(7), V1),
        Err(wal_error(WalError::WrongOwner {
            expected: page(7),
            found: page(9),
        }))
    );
    let longer = LogAddress::of(&held[0]).unwrap().pending(30).unwrap();
    assert_eq!(
        read_pending_value(&longer, at(&held[0]), page(7), V1),
        Err(wal_error(WalError::WrongValueLength {
            expected: 30,
            found: 29,
        }))
    );
    let shorter = LogAddress::of(&held[1]).unwrap().pending(299).unwrap();
    assert_eq!(
        read_pending_value(&shorter, at(&held[1]), page(9), V1),
        Err(wal_error(WalError::WrongValueLength {
            expected: 299,
            found: 300,
        }))
    );
    let part_at = &log[offsets[2] as usize + 20..];
    assert_eq!(
        read_pending_value(&held[2], part_at, page(8), V1),
        Err(wal_error(WalError::NotAValue { kind: 4 }))
    );
    assert!(matches!(
        read_pending_value(&held[0], &at(&held[0])[..40], page(7), V1),
        Err(FormatError::Truncated { .. })
    ));
}

#[test]
fn the_bytes_to_read_at_an_address_cover_either_record() {
    assert_eq!(logged_extent_len(1, V1), 20);
    assert_eq!(logged_read_len(1, V1), 33);
    assert_eq!(logged_extent_len(14, V1), 33);
    assert_eq!(logged_read_len(14, V1), 33);
    assert_eq!(logged_extent_len(15, V1), 34);
    assert_eq!(logged_read_len(15, V1), 34);

    assert_eq!(MAX_VALUE_PART_LEN, (1 << 27) - 4 - 11);
    let full = MAX_VALUE_PART_LEN as u32;
    assert_eq!(logged_extent_len(full, V1), u64::from(full) + 19);
    assert_eq!(
        logged_extent_len(full + 1, V1),
        u64::from(full) + 1 + 2 * 19
    );
    assert_eq!(logged_read_len(full + 1, V1), u64::from(full) + 39);
    assert_eq!(
        logged_extent_len(u32::MAX, V1),
        u64::from(u32::MAX) + 33 * 19
    );
}

#[test]
fn a_value_that_cannot_be_logged_is_refused_and_leaves_the_group_as_it_was() {
    let mut out = vec![0xEE; 5];
    let mut group =
        GroupWriter::with_limits(version(1), address(45), parts_of(1), &mut out).unwrap();
    assert_eq!(
        group.value(page(7), b""),
        Err(FormatError::ValueLog(ValueLogError::EmptyValue))
    );
    assert_eq!(group.encoded_len(), 0);
    // More parts than a record can number.
    let long = vec![7; (1 << 16) + 1];
    assert_eq!(
        group.value(page(7), &long),
        Err(FormatError::ValueLog(ValueLogError::ValueTooLong {
            len: long.len() as u64,
        }))
    );
    assert_eq!(group.encoded_len(), 0);
    assert!(group.value(page(7), &long[..1 << 16]).is_ok());
    assert_eq!(group.encoded_len(), (1 << 16) * 20);
    group.abandon();
    assert_eq!(out, [0xEE; 5]);
}

#[test]
fn a_value_one_byte_above_the_longest_record_is_two_records() {
    let len = MAX_VALUE_PART_LEN + 1;
    let value: Vec<u8> = (0..len).map(|at| (at % 251) as u8).collect();
    let mut log = new_log(1, &LogStart::FIRST);
    let mut held = None;
    push_group(&mut log, &LogStart::FIRST, Limits::FORMAT, 1, |group| {
        held = Some(group.value(page(9), &value).unwrap());
    });
    let pending = held.unwrap();
    assert_eq!(pending.value_len() as usize, len);
    assert_eq!(log.len(), 45 + len + 2 * 19 + PART_HEADER_LEN);

    // The first record is as long as a record gets: its checksum covers
    // 128 MiB. The second carries the last byte.
    let first = &log[45..];
    assert_eq!(record_len(first, V1), Ok(8 + MAX_RECORD_PAYLOAD_LEN));
    assert_eq!(first[..4], (MAX_RECORD_PAYLOAD_LEN as u32).to_le_bytes());
    assert_eq!(
        4 + MAX_RECORD_PAYLOAD_LEN,
        crate::checksum::MAX_CHECKSUMMED_LEN
    );
    let second = &first[8 + MAX_RECORD_PAYLOAD_LEN..];
    assert_eq!(second[..4], 12u32.to_le_bytes());
    assert_eq!(second[8..19], [2, 9, 0, 0, 0, 1, 0, 0xF2, 0xFF, 0xFF, 0x07]);
    assert_eq!(second[19], value[len - 1]);

    let read = read_pending_value(&pending, first, page(9), V1).unwrap();
    let bytes = read.bytes().expect("the bytes");
    assert_eq!(bytes.contiguous(), None);
    let parts: Vec<_> = bytes.parts().collect();
    assert_eq!(parts.len(), 2);
    assert!(parts[0] == &value[..MAX_VALUE_PART_LEN]);
    assert_eq!(parts[1], &value[MAX_VALUE_PART_LEN..]);

    let (whole, tail) = items(&log);
    assert_eq!(tail.failure(), None);
    assert_eq!(whole.len(), 1);

    // Cut inside the second record, the group is not whole.
    let (cut, tail) = items(&log[..45 + 8 + MAX_RECORD_PAYLOAD_LEN + 5]);
    assert!(cut.is_empty());
    assert_eq!(tail.valid_len(), 45);
}
