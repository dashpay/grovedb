//! What may follow what in a log file, and the places and addresses of the
//! log.

use super::*;
use crate::header::FileHeader;

fn mark(covers: u64) -> Vec<u8> {
    let mut out = Vec::new();
    CheckpointMark::new(version(covers))
        .write(&mut out)
        .unwrap();
    out
}

fn carried() -> Vec<u8> {
    let mut out = Vec::new();
    CarriedValue::new(page(4), page(3), pointer(1, 20, 9))
        .write(&mut out)
        .unwrap();
    out
}

fn place_step() -> Vec<u8> {
    let mut out = Vec::new();
    ValueMove::in_flat_leaf(
        page(4),
        ValuePlace::Pending(pending(45, 9)),
        pointer(1, 20, 9),
    )
    .unwrap()
    .write(&mut out)
    .unwrap();
    out
}

fn freeing(raw_version: u64) -> Vec<u8> {
    let mut out = Vec::new();
    write_reclaim(
        page(4),
        None,
        Some(version(raw_version)),
        [].into_iter(),
        &mut out,
    )
    .unwrap();
    out
}

fn start_record(start: &LogStart) -> Vec<u8> {
    new_log(1, start)[20..].to_vec()
}

fn group(raw_version: u64) -> Vec<u8> {
    part_record(1, raw_version, 0, &freed_entry(9))
}

#[test]
fn a_log_file_begins_with_the_header_of_a_log_and_a_start_record() {
    let log = new_log(3, &LogStart::FIRST);
    for len in 0..20 {
        assert_eq!(
            scan(&log[..len]).err(),
            Some(FormatError::Truncated {
                needed: 20,
                available: len,
            })
        );
    }
    for len in 20..28 {
        assert_eq!(
            scan(&log[..len]).err(),
            Some(FormatError::Truncated {
                needed: 28,
                available: len,
            })
        );
    }
    for len in 28..45 {
        assert_eq!(
            scan(&log[..len]).err(),
            Some(FormatError::Truncated {
                needed: 45,
                available: len,
            })
        );
    }
    let mut whole = scan(&log).unwrap();
    assert_eq!(whole.header().file_id(), Some(file_id(3)));
    assert_eq!(whole.start(), LogStart::FIRST);
    assert_eq!(whole.offset(), 45);
    assert_eq!(whole.next(), None);
    assert_eq!(whole.next(), None);
    let tail = whole.tail();
    assert_eq!(
        (tail.valid_len(), tail.latest(), tail.failure()),
        (45, CommitVersion::EMPTY, None)
    );

    // Another kind of file, and a file whose first record is not a start.
    for kind in [FileKind::Segment, FileKind::ValueLog, FileKind::PageTable] {
        let header = FileHeader::new(kind, V1, 3).unwrap();
        let other = [&header.encode()[..], &log[20..]].concat();
        let not_a_log = wal_error(WalError::NotALog { found: kind });
        assert_eq!(scan(&other).err(), Some(not_a_log));
        assert_eq!(read_file_start(&other).err(), Some(not_a_log));
        assert_eq!(
            LogCursor::new(&header, &LogStart::FIRST).err(),
            Some(not_a_log)
        );
    }
    // A log file's id is a file id: not 0, and not the reserved id.
    for sequence in [0, u32::MAX] {
        let mut unnamed = log.clone();
        unnamed[12..16].copy_from_slice(&sequence.to_le_bytes());
        let checksum = crate::Checksum::of(&[&unnamed[..16]]).encode();
        unnamed[16..20].copy_from_slice(&checksum);
        let no_file = FormatError::InvalidFileSequence {
            kind: FileKind::Wal,
        };
        assert_eq!(scan(&unnamed).err(), Some(no_file));
        assert_eq!(read_file_start(&unnamed).err(), Some(no_file));
    }
    let last = FileHeader::new(FileKind::Wal, V1, FileId::MAX.get()).unwrap();
    let named = [&last.encode()[..], &log[20..]].concat();
    assert_eq!(
        scan(&named).map(|scan| scan.header().file_id()),
        Ok(Some(FileId::MAX))
    );
    let header = &log[..20];
    for first in [mark(0), group(1), carried(), value_record(1, 0, 1, b"x")] {
        assert_eq!(
            scan(&[header, &first].concat()).err(),
            Some(wal_error(WalError::MissingStart))
        );
    }
    let mut damaged = log.clone();
    damaged[30] ^= 4;
    assert!(matches!(
        scan(&damaged).err(),
        Some(FormatError::ChecksumMismatch { .. })
    ));
    assert_eq!(scan(&[0; 64]).err(), Some(FormatError::BadMagic));

    // A second start record is not part of a log.
    assert_eq!(
        refusal(&start_record(&LogStart::FIRST)),
        wal_error(WalError::MisplacedStart)
    );
}

#[test]
fn a_group_is_its_values_and_then_its_parts_with_nothing_between() {
    let value = [value_record(7, 0, 3, b"abc")];
    let interrupted = |kind: u8| wal_error(WalError::GroupInterrupted { kind });
    let unfinished = wal_error(WalError::UnfinishedValue);
    // Records that carry at most 29 bytes of a value.
    let limits = Limits { max_payload: 40 };
    let bytes: Vec<u8> = (0..70).collect();
    let refused = |records: &[Vec<u8>]| {
        tail_after(&LogStart::FIRST, limits, &records.concat())
            .failure()
            .map(|failure| (failure.offset, failure.error))
    };
    let at = |records: &[Vec<u8>]| 45 + records.concat().len() as u64;

    // Whole: two values, one of them in three records, a carried value,
    // and a part.
    let long = [
        value_record(7, 0, 70, &bytes[..29]),
        value_record(7, 1, 70, &bytes[29..58]),
        value_record(7, 2, 70, &bytes[58..]),
    ];
    let whole = [
        &value[..],
        &long[..],
        &[carried(), part_record(1, 1, 0, &[])],
    ]
    .concat();
    assert_eq!(refused(&whole), None);

    // Between the values of a group, and after them, only what belongs to
    // the group.
    for (other, kind) in [(mark(0), 6), (place_step(), 5), (freeing(1), 5)] {
        let records = [value[0].clone(), other];
        assert_eq!(
            refused(&records),
            Some((at(&records[..1]), interrupted(kind)))
        );
        let records = [carried(), records[1].clone()];
        assert_eq!(
            refused(&records),
            Some((at(&records[..1]), interrupted(kind)))
        );
    }
    let records = [value[0].clone(), start_record(&LogStart::FIRST)];
    assert_eq!(
        refused(&records),
        Some((at(&records[..1]), wal_error(WalError::MisplacedStart)))
    );

    // Inside a value, only its next part.
    assert_eq!(
        refused(&long[1..]),
        Some((
            45,
            wal_error(WalError::UnexpectedValuePart {
                expected: 0,
                found: 1,
            })
        ))
    );
    for other in [carried(), mark(0), part_record(1, 1, 0, &[]), place_step()] {
        let records = [long[0].clone(), other];
        assert_eq!(refused(&records), Some((at(&records[..1]), unfinished)));
    }
    let records = [long[0].clone(), long[2].clone()];
    assert_eq!(
        refused(&records),
        Some((
            at(&records[..1]),
            wal_error(WalError::UnexpectedValuePart {
                expected: 1,
                found: 2,
            })
        ))
    );
    let records = [long[0].clone(), value_record(8, 1, 70, &bytes[29..58])];
    assert_eq!(
        refused(&records),
        Some((at(&records[..1]), wal_error(WalError::PartOfAnotherValue)))
    );
    let records = [long[0].clone(), long[1].clone(), long[0].clone()];
    assert_eq!(
        refused(&records),
        Some((
            at(&records[..2]),
            wal_error(WalError::UnexpectedValuePart {
                expected: 2,
                found: 0,
            })
        ))
    );

    // The log ends inside the group.
    for records in [&value[..], &long[..], &long[..1], &[carried()]] {
        let tail = tail_after(&LogStart::FIRST, limits, &records.concat());
        assert_eq!(tail.valid_len(), 45);
        let failure = tail.failure().unwrap();
        assert_eq!(failure.offset, at(records));
        assert_eq!(failure.error, wal_error(WalError::UnfinishedGroup));
    }
}

#[test]
fn the_parts_of_a_group_are_numbered_full_and_in_one_order() {
    // Parts whose payload holds five freed pages at most: 14 + 5 * 5.
    let limits = Limits { max_payload: 40 };
    let pages = |raw_pages: &[u32]| {
        raw_pages
            .iter()
            .flat_map(|raw_page| freed_entry(*raw_page))
            .collect::<Vec<u8>>()
    };
    let refused = |records: &[Vec<u8>]| {
        let tail = tail_after(&LogStart::FIRST, limits, &records.concat());
        assert_eq!(tail.valid_len(), 45);
        tail.failure().map(|failure| failure.error)
    };
    let whole = [
        part_record(0, 1, 0, &pages(&[1, 2, 3, 4, 5])),
        part_record(0, 1, 1, &pages(&[6, 7, 8, 9, 10])),
        part_record(1, 1, 2, &pages(&[11])),
    ];
    let tail = tail_after(&LogStart::FIRST, limits, &whole.concat());
    assert_eq!(tail.failure(), None);
    assert_eq!(tail.latest(), version(1));

    for number in [1, 2, u32::MAX] {
        assert_eq!(
            refused(&[part_record(1, 1, number, &pages(&[1]))]),
            Some(wal_error(WalError::UnexpectedPart {
                expected: 0,
                found: number,
            }))
        );
    }
    for number in [0, 2] {
        assert_eq!(
            refused(&[whole[0].clone(), part_record(1, 1, number, &pages(&[6]))]),
            Some(wal_error(WalError::UnexpectedPart {
                expected: 1,
                found: number,
            }))
        );
    }
    assert_eq!(
        refused(&[whole[0].clone(), part_record(1, 2, 1, &pages(&[6]))]),
        Some(wal_error(WalError::PartOfAnotherGroup))
    );
    // A part that the next entry would have fitted into.
    assert_eq!(
        refused(&[
            part_record(0, 1, 0, &pages(&[1, 2, 3, 4])),
            part_record(1, 1, 1, &pages(&[5])),
        ]),
        Some(wal_error(WalError::PartNotFull))
    );
    // The order of the entries goes on across the parts.
    for next in [1, 5] {
        assert_eq!(
            refused(&[whole[0].clone(), part_record(1, 1, 1, &pages(&[next]))]),
            Some(wal_error(WalError::EntriesOutOfOrder))
        );
    }
    // After a part that is not the last, only the next part.
    for (other, kind) in [
        (mark(0), 6),
        (place_step(), 5),
        (carried(), 3),
        (value_record(7, 0, 1, b"x"), 2),
    ] {
        assert_eq!(
            refused(&[whole[0].clone(), other]),
            Some(wal_error(WalError::GroupInterrupted { kind }))
        );
    }
    assert_eq!(
        refused(&whole[..2]),
        Some(wal_error(WalError::UnfinishedGroup))
    );
    // A part that is too long for a record of this log is not one.
    assert!(matches!(
        refused(&[part_record(1, 1, 0, &pages(&[1, 2, 3, 4, 5, 6]))]),
        Some(FormatError::RecordTooLong { len: 44, max: 40 })
    ));
}

#[test]
fn the_versions_of_the_groups_go_up_by_one_and_nothing_is_ahead_of_them() {
    let start = LogStart::new(900, version(7)).unwrap();
    let refused = |records: &[Vec<u8>]| {
        tail_after(&start, Limits::FORMAT, &records.concat())
            .failure()
            .map(|failure| failure.error)
    };
    let gap = |after: u64, found: u64| {
        Some(wal_error(WalError::VersionGap {
            after: version(after),
            found: version(found),
        }))
    };
    assert_eq!(refused(&[group(8), group(9), group(10)]), None);
    for found in [1, 6, 7, 9, 100] {
        assert_eq!(refused(&[group(found)]), gap(7, found));
    }
    assert_eq!(refused(&[group(8), group(8)]), gap(8, 8));
    assert_eq!(refused(&[group(8), group(10)]), gap(8, 10));
    // A background change and a mark take no version.
    assert_eq!(
        refused(&[place_step(), group(8), mark(8), freeing(8), group(9)]),
        None
    );

    // A mark covers no commit that is not before it, and a page is freed
    // with the version of none either.
    for covers in 0..=7 {
        assert_eq!(refused(&[mark(covers)]), None);
        assert_eq!(refused(&[freeing(covers.max(1))]), None);
    }
    assert_eq!(
        refused(&[mark(8)]),
        Some(wal_error(WalError::CheckpointAhead {
            covers: version(8),
            latest: version(7),
        }))
    );
    assert_eq!(refused(&[group(8), mark(8)]), None);
    assert_eq!(
        refused(&[freeing(8)]),
        Some(wal_error(WalError::FreedAhead {
            version: version(8),
            latest: version(7),
        }))
    );
    assert_eq!(refused(&[group(8), freeing(8)]), None);

    // No commit comes after the last version there is.
    let last = LogStart::new(0, CommitVersion::MAX).unwrap();
    let tail = tail_after(&last, Limits::FORMAT, &group(CommitVersion::MAX.get()));
    assert_eq!(
        tail.failure().map(|failure| failure.error),
        Some(wal_error(WalError::VersionGap {
            after: CommitVersion::MAX,
            found: CommitVersion::MAX,
        }))
    );
}

#[test]
fn a_file_is_read_record_by_record_as_it_is_read_whole() {
    for built in [
        random_log(0xF05_31, Limits::FORMAT, 20),
        random_log(0xF05_32, SMALL, 20),
    ] {
        let (header, start) = read_file_start(&built.log).unwrap();
        assert_eq!(start, built.start);
        let mut cursor = LogCursor::with_limits(&header, &start, built.limits).unwrap();
        assert_eq!((cursor.offset(), cursor.valid_len()), (45, 45));
        assert_eq!(cursor.latest(), start.after());
        let mut ends = Vec::new();
        let mut in_group = 0;
        while (cursor.offset() as usize) < built.log.len() {
            // A reader that has only the frame of the next record knows
            // how much to fetch.
            let at = cursor.offset() as usize;
            let len = record_len(&built.log[at..at + 8], V1).unwrap();
            let before = cursor.clone();
            let step = cursor.step(&built.log[at..at + len]).unwrap();
            assert_eq!(step.len, len);
            assert_eq!(cursor.offset() as usize, at + len);
            assert_eq!(step.ends_item, !cursor.in_group());
            assert_eq!(cursor.finish().is_ok(), step.ends_item);
            assert_eq!(cursor.item_offset(), cursor.valid_len());
            if step.ends_item {
                ends.push(cursor.offset());
                assert_eq!(cursor.valid_len(), cursor.offset());
            } else {
                in_group += 1;
                assert_eq!(cursor.valid_len(), built.item_start(at as u64));
                assert_eq!(cursor.finish(), Err(wal_error(WalError::UnfinishedGroup)));
            }

            // A record that is refused leaves the cursor where it was.
            let mut again = before.clone();
            assert!(again.step(&built.log[at..at + len - 1]).is_err());
            assert_eq!(again, before);
        }
        assert_eq!(ends, built.ends);
        assert!(in_group > 10);
        assert_eq!(cursor.finish(), Ok(()));
    }
}

#[test]
fn a_place_is_a_file_and_an_offset_behind_the_start_record() {
    let invalid = |offset: u64| Err(wal_error(WalError::InvalidPlace { offset }));
    for offset in [0, 19, 20, 44] {
        assert_eq!(LogPlace::new(file_id(3), offset), invalid(offset));
    }
    for offset in [MAX_LOG_OFFSET + 1, u64::MAX] {
        assert_eq!(LogPlace::new(file_id(3), offset), invalid(offset));
    }
    for offset in [45, 46, 81_940, MAX_LOG_OFFSET] {
        let place = LogPlace::new(file_id(3), offset).unwrap();
        assert_eq!((place.file(), place.offset()), (file_id(3), offset));
    }
    assert_eq!(MAX_LOG_OFFSET, (1 << 63) - 1);

    // Places are in the order of the log: by file, then by offset.
    let place = |file: u32, offset: u64| LogPlace::new(file_id(file), offset).unwrap();
    let mut places = [place(2, 900), place(3, 45), place(1, 5000), place(2, 899)];
    places.sort();
    assert_eq!(
        places,
        [place(1, 5000), place(2, 899), place(2, 900), place(3, 45)]
    );
}

#[test]
fn an_address_is_the_base_of_its_file_and_an_offset() {
    assert_eq!(LogAddress::new(0), None);
    assert_eq!(LogAddress::decode([0; 8]), None);
    assert_eq!(LogAddress::ENCODED_LEN, 8);
    for raw in [1, 45, 0x0102_0304_0506_0708, u64::MAX] {
        let at = address(raw);
        assert_eq!(at.get(), raw);
        assert_eq!(at.encode(), raw.to_le_bytes());
        assert_eq!(LogAddress::decode(at.encode()), Some(at));
        assert_eq!(format!("{at:?}"), format!("log@{raw}"));
        assert_eq!(at.to_string(), raw.to_string());
        // The 8 bytes of a pending value are the address.
        let held = at.pending(9).unwrap();
        assert_eq!(held.id(), raw.to_le_bytes());
        assert_eq!(LogAddress::of(&held), Ok(at));
        assert_eq!(ValuePlace::Pending(held).encode()[4..12], raw.to_le_bytes());
    }
    assert!(address(5) < address(6));
    assert!(address(45).pending(0).is_err());
    assert_eq!(address(45).after(10), Ok(address(55)));
    assert_eq!(address(45).after(0), Ok(address(45)));
    assert_eq!(address(u64::MAX).after(0), Ok(address(u64::MAX)));
    assert_eq!(
        address(u64::MAX).after(1),
        Err(wal_error(WalError::AddressOutOfRange))
    );

    let start = LogStart::new(1000, version(4)).unwrap();
    assert_eq!((start.base(), start.after()), (1000, version(4)));
    assert_eq!(start.address(45), Ok(address(1045)));
    assert_eq!(start.offset_of(address(1045)), Some(45));
    assert_eq!(start.offset_of(address(1044)), None);
    assert_eq!(start.offset_of(address(999)), None);
    assert_eq!(
        start.offset_of(address(1000 + MAX_LOG_OFFSET)),
        Some(MAX_LOG_OFFSET)
    );
    assert_eq!(start.offset_of(address(1001 + MAX_LOG_OFFSET)), None);
    for offset in [0, 44, MAX_LOG_OFFSET + 1] {
        let invalid = Err(wal_error(WalError::InvalidPlace { offset }));
        assert_eq!(start.address(offset), invalid);
        assert_eq!(
            start.next(offset, version(4)).map(|_| ()),
            invalid.map(|_| ())
        );
        assert!(start.check_next(offset, version(4), &start).is_err());
    }

    // The first file of a database starts the log at 0, and an address in
    // it is never 0.
    assert_eq!(
        LogStart::FIRST,
        LogStart::new(0, CommitVersion::EMPTY).unwrap()
    );
    assert_eq!(LogStart::FIRST.address(45), Ok(address(45)));
    assert_eq!(
        LogStart::new(MAX_LOG_OFFSET + 1, CommitVersion::EMPTY),
        Err(wal_error(WalError::InvalidBase {
            base: MAX_LOG_OFFSET + 1,
        }))
    );
    let last = LogStart::new(MAX_LOG_OFFSET, version(1)).unwrap();
    assert_eq!(
        last.address(MAX_LOG_OFFSET),
        Ok(address(2 * MAX_LOG_OFFSET))
    );
    assert_eq!(
        last.next(45, version(1)),
        Err(wal_error(WalError::InvalidBase {
            base: MAX_LOG_OFFSET + 45,
        }))
    );

    // Addresses go on from one file to the next.
    let next = start.next(700, version(9)).unwrap();
    assert_eq!((next.base(), next.after()), (1700, version(9)));
    assert_eq!(next.address(45), Ok(address(1745)));
    assert_eq!(start.offset_of(address(1745)), Some(745));
}
