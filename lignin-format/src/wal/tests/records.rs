//! One record, read by itself: each check with its error, in order.

use super::*;
use crate::{tree_page::TreePageError, value_log::ValueLogError};

fn error_of(payload: &[u8]) -> FormatError {
    decode(&framed(payload)).expect_err("refused")
}

fn le32(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn le64(value: u64) -> [u8; 8] {
    value.to_le_bytes()
}

fn record_length(kind: u8, len: usize) -> FormatError {
    wal_error(WalError::RecordLength { kind, len })
}

const NO_VERSION: u64 = u64::MAX;

#[test]
fn the_frame_is_checked_before_the_kind() {
    let mut mark = Vec::new();
    CheckpointMark::new(version(4)).write(&mut mark).unwrap();

    for len in 0..8 {
        assert_eq!(
            decode(&mark[..len]),
            Err(FormatError::Truncated {
                needed: 8,
                available: len,
            })
        );
    }
    for len in 8..mark.len() {
        assert_eq!(
            decode(&mark[..len]),
            Err(FormatError::Truncated {
                needed: mark.len(),
                available: len,
            })
        );
        assert_eq!(record_len(&mark[..len], V1), Ok(mark.len()));
    }
    assert_eq!(
        decode(&mark),
        Ok((Record::Mark(CheckpointMark::new(version(4))), 17))
    );

    // Bytes behind the record are not looked at.
    let mut longer = mark.clone();
    longer.extend_from_slice(&[0xAB; 9]);
    assert_eq!(decode(&longer), decode(&mark));

    // The length is checked against the longest record before anything of
    // that length is asked for.
    let too_long = (MAX_RECORD_PAYLOAD_LEN as u32 + 1).to_le_bytes();
    let refused = FormatError::RecordTooLong {
        len: MAX_RECORD_PAYLOAD_LEN as u64 + 1,
        max: MAX_RECORD_PAYLOAD_LEN as u64,
    };
    assert_eq!(decode(&[&too_long[..], &[0; 4]].concat()), Err(refused));
    assert_eq!(
        record_len(&[&too_long[..], &[0; 4]].concat(), V1),
        Err(refused)
    );
    assert_eq!(
        record_len(&[0; 7], V1),
        Err(FormatError::Truncated {
            needed: 8,
            available: 7,
        })
    );

    // The checksum is checked before the kind: a flipped bit in the kind is
    // damage, not an unknown kind.
    for at in 0..mark.len() {
        for bit in 0..8 {
            let mut damaged = mark.clone();
            damaged[at] ^= 1 << bit;
            let error = decode(&damaged).expect_err("damage");
            assert!(
                matches!(
                    error,
                    FormatError::ChecksumMismatch { .. }
                        | FormatError::Truncated { .. }
                        | FormatError::RecordTooLong { .. }
                ),
                "bit {bit} of {at}: {error:?}"
            );
        }
    }

    // Zero-filled bytes are not a record.
    assert!(matches!(
        decode(&[0; 64]),
        Err(FormatError::ChecksumMismatch { .. })
    ));
}

#[test]
fn every_kind_code_is_a_record_or_an_error() {
    assert_eq!(error_of(&[]), record_length(0, 0));
    for code in 0..=u8::MAX {
        let error = error_of(&[code]);
        match code {
            1..=4 | 6 => assert_eq!(error, record_length(code, 1), "{code}"),
            5 => assert_eq!(error, record_length(5, 1)),
            _ => assert_eq!(
                error,
                FormatError::UnknownRecordKind { code: code.into() },
                "{code}"
            ),
        }
    }

    let start = LogStart::new(77, version(3)).unwrap();
    let mut records = new_log(1, &start)[20..].to_vec();
    assert_eq!(decode(&records).unwrap().0.kind_code(), 1);
    assert_eq!(decode(&records).unwrap().0, Record::Start(start));
    records = value_record(7, 0, 3, b"abc");
    assert_eq!(decode(&records).unwrap().0.kind_code(), 2);
    let mut carried = Vec::new();
    CarriedValue::new(page(1), page(2), pointer(3, 20, 4))
        .write(&mut carried)
        .unwrap();
    assert_eq!(decode(&carried).unwrap().0.kind_code(), 3);
    assert_eq!(decode(&part_record(1, 1, 0, &[])).unwrap().0.kind_code(), 4);
    let mut change = Vec::new();
    ValueMove::in_flat_leaf(
        page(1),
        ValuePlace::Pending(pending(45, 4)),
        pointer(3, 20, 4),
    )
    .unwrap()
    .write(&mut change)
    .unwrap();
    assert_eq!(decode(&change).unwrap().0.kind_code(), 5);
    let mut mark = Vec::new();
    CheckpointMark::new(CommitVersion::EMPTY)
        .write(&mut mark)
        .unwrap();
    assert_eq!(decode(&mark).unwrap().0.kind_code(), 6);
}

#[test]
fn each_check_of_a_start_record_and_of_a_mark_has_its_error() {
    let start = |base: u64, after: u64| [&[1][..], &le64(base), &le64(after)].concat();
    assert_eq!(
        decode(&framed(&start(MAX_LOG_OFFSET, 9))).unwrap().0,
        Record::Start(LogStart::new(MAX_LOG_OFFSET, version(9)).unwrap())
    );
    assert_eq!(error_of(&start(0, 0)[..16]), record_length(1, 16));
    assert_eq!(
        error_of(&[&start(0, 0)[..], &[0]].concat()),
        record_length(1, 18)
    );
    assert_eq!(
        error_of(&start(MAX_LOG_OFFSET + 1, 0)),
        wal_error(WalError::InvalidBase {
            base: MAX_LOG_OFFSET + 1,
        })
    );
    assert_eq!(error_of(&start(0, NO_VERSION)), FormatError::MissingId);

    let mark = |covers: u64| [&[6][..], &le64(covers)].concat();
    assert_eq!(error_of(&mark(1)[..8]), record_length(6, 8));
    assert_eq!(
        error_of(&[&mark(1)[..], &[0]].concat()),
        record_length(6, 10)
    );
    assert_eq!(error_of(&mark(NO_VERSION)), FormatError::MissingId);
    assert_eq!(
        decode(&framed(&mark(0))).unwrap().0,
        Record::Mark(CheckpointMark::new(CommitVersion::EMPTY))
    );
}

#[test]
fn each_check_of_a_value_record_and_of_a_carried_value_has_its_error() {
    let value = |owner: u32, part: u16, len: u32, data: &[u8]| {
        [
            &[2][..],
            &le32(owner),
            &part.to_le_bytes(),
            &le32(len),
            data,
        ]
        .concat()
    };
    let record = framed(&value(7, 0, 3, b"abc"));
    let (Record::Value(read), len) = decode(&record).unwrap() else {
        panic!("a value record");
    };
    assert_eq!(len, VALUE_RECORD_HEADER_LEN + 3);
    assert_eq!(
        (
            read.owner(),
            read.part(),
            read.value_len(),
            read.data(),
            read.is_last()
        ),
        (page(7), 0, 3, &b"abc"[..], true)
    );

    assert_eq!(error_of(&value(7, 0, 3, b"")[..10]), record_length(2, 10));
    assert_eq!(error_of(&value(0, 0, 3, b"abc")), FormatError::MissingId);
    assert_eq!(
        error_of(&value(7, 0, 0, b"")),
        FormatError::ValueLog(ValueLogError::EmptyValue)
    );
    assert_eq!(
        error_of(&value(7, 1, 3, b"abc")),
        wal_error(WalError::NoSuchValuePart {
            part: 1,
            value_len: 3,
        })
    );
    for data in [&b"ab"[..], b"abcd", b""] {
        assert_eq!(
            error_of(&value(7, 0, 3, data)),
            wal_error(WalError::WrongValuePartLength {
                expected: 3,
                found: data.len(),
            })
        );
    }

    let carried = |owner: u32, source_owner: u32, source: [u8; 16]| {
        [&[3][..], &le32(owner), &le32(source_owner), &source].concat()
    };
    let source = pointer(13, 64, 300);
    assert_eq!(
        decode(&framed(&carried(9, 7, source.encode()))).unwrap().0,
        Record::Carried(CarriedValue::new(page(9), page(7), source))
    );
    assert_eq!(
        error_of(&carried(9, 7, source.encode())[..24]),
        record_length(3, 24)
    );
    assert_eq!(
        error_of(&[&carried(9, 7, source.encode())[..], &[0]].concat()),
        record_length(3, 26)
    );
    assert_eq!(
        error_of(&carried(0, 7, source.encode())),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&carried(9, 0, source.encode())),
        FormatError::MissingId
    );
    assert_eq!(error_of(&carried(9, 7, [0; 16])), FormatError::MissingId);
    // The source is in the value log: a pending value is not a source.
    assert_eq!(
        error_of(&carried(
            9,
            7,
            ValuePlace::Pending(pending(45, 300)).encode()
        )),
        FormatError::ReservedFileId
    );
    let into_the_header = [&le32(13)[..], &le64(19), &le32(300)].concat();
    assert_eq!(
        error_of(&carried(9, 7, into_the_header.try_into().unwrap())),
        FormatError::ValueLog(ValueLogError::InvalidOffset { offset: 19 })
    );
    let empty = [&le32(13)[..], &le64(64), &le32(0)].concat();
    assert_eq!(
        error_of(&carried(9, 7, empty.try_into().unwrap())),
        FormatError::ValueLog(ValueLogError::EmptyValue)
    );
}

#[test]
fn each_check_of_a_part_has_its_error() {
    let part = |flags: u8, raw_version: u64, number: u32, entries: &[u8]| {
        [&[4, flags][..], &le64(raw_version), &le32(number), entries].concat()
    };
    let record = framed(&part(1, 5, 0, &freed_entry(9)));
    let (Record::Part(read), len) = decode(&record).unwrap() else {
        panic!("a part");
    };
    assert_eq!(len, PART_HEADER_LEN + 5);
    assert_eq!(
        (read.version(), read.part(), read.is_last(), read.is_empty()),
        (version(5), 0, true, false)
    );
    assert_eq!(read.entries().collect::<Vec<_>>(), [Entry::Freed(page(9))]);

    for len in 1..14 {
        assert_eq!(error_of(&part(1, 5, 0, &[])[..len]), record_length(4, len));
    }
    for flags in [2, 3, 0x80, 0xFF] {
        assert_eq!(
            error_of(&part(flags, 5, 0, &[])),
            FormatError::NonZeroReserved
        );
    }
    assert_eq!(
        error_of(&part(1, NO_VERSION, 0, &[])),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&part(1, 0, 0, &[])),
        wal_error(WalError::NotACommit)
    );

    // A part with no entry is the only part of its group.
    assert!(
        decode(&framed(&part(1, 5, 0, &[]))).unwrap().0
            == decode(&part_record(1, 5, 0, &[])).unwrap().0
    );
    for (flags, number) in [(0, 0), (1, 1), (0, 1)] {
        assert_eq!(
            error_of(&part(flags, 5, number, &[])),
            wal_error(WalError::EmptyPart)
        );
    }

    // The entries.
    for code in [0, 6, 7, 0xFF] {
        assert_eq!(
            error_of(&part(1, 5, 0, &[code])),
            FormatError::UnknownRecordKind { code: code.into() }
        );
    }
    assert_eq!(
        error_of(&part(1, 5, 0, &freed_entry(0))),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&part(1, 5, 0, &freed_entry(9)[..4])),
        FormatError::Truncated {
            needed: PART_HEADER_LEN + 5,
            available: PART_HEADER_LEN + 4,
        }
    );
    for kind in [4, 5] {
        let entry = pointer_entry(kind, &pointer(3, 20, 4));
        assert!(decode(&framed(&part(1, 5, 0, &entry))).is_ok());
        assert_eq!(
            error_of(&part(1, 5, 0, &entry[..16])),
            FormatError::Truncated {
                needed: PART_HEADER_LEN + 17,
                available: PART_HEADER_LEN + 16,
            }
        );
        let mut no_file = entry.clone();
        no_file[1..5].fill(0);
        assert_eq!(error_of(&part(1, 5, 0, &no_file)), FormatError::MissingId);
        // The counts are of files: a pending value is in none.
        let mut pending_value = entry.clone();
        pending_value[1..5].fill(0xFF);
        assert_eq!(
            error_of(&part(1, 5, 0, &pending_value)),
            FormatError::ReservedFileId
        );
    }

    // A run is as long as its entry says, and is a run.
    let run = random_tree_run(&mut SplitMix64::new(1), page(4), true, usize::MAX);
    assert!(decode(&framed(&part(1, 5, 0, &run_entry(1, &run)))).is_ok());
    let cut = run_entry(1, &run);
    assert_eq!(
        error_of(&part(1, 5, 0, &cut[..cut.len() - 1])),
        FormatError::Truncated {
            needed: PART_HEADER_LEN + cut.len(),
            available: PART_HEADER_LEN + cut.len() - 1,
        }
    );
    assert_eq!(
        error_of(&part(1, 5, 0, &run_entry(1, &run[..4]))),
        FormatError::Delta(crate::delta::DeltaError::EmptyRun)
    );
    assert_eq!(
        error_of(&part(1, 5, 0, &run_entry(2, &run[..4]))),
        wal_error(WalError::EmptyFlatRun)
    );
    assert_eq!(
        error_of(&part(1, 5, 0, &run_entry(1, &[4, 0, 0, 0, 99]))),
        FormatError::UnknownRecordKind { code: 99 }
    );
    let too_large = [&[4, 0, 0, 0, 2, 1, 0][..], &u16::MAX.to_le_bytes()].concat();
    assert!(matches!(
        error_of(&part(1, 5, 0, &run_entry(1, &too_large))),
        FormatError::TreePage(TreePageError::CellTooLarge { .. })
    ));

    // The pages in ascending order, each once, then the released pointers,
    // then the acquired ones, each by file and offset.
    let (low, high) = (pointer(3, 20, 4), pointer(3, 21, 4));
    let other_file = pointer(4, 20, 4);
    let in_order: [&[Vec<u8>]; 5] = [
        &[freed_entry(1), freed_entry(2)],
        &[freed_entry(9), pointer_entry(4, &low)],
        &[pointer_entry(4, &low), pointer_entry(4, &high)],
        &[pointer_entry(4, &high), pointer_entry(4, &other_file)],
        &[pointer_entry(4, &high), pointer_entry(5, &low)],
    ];
    for entries in in_order {
        assert!(decode(&framed(&part(1, 5, 0, &entries.concat()))).is_ok());
    }
    let out_of_order: [&[Vec<u8>]; 7] = [
        &[freed_entry(2), freed_entry(1)],
        &[freed_entry(2), freed_entry(2)],
        &[run_entry(1, &run), freed_entry(4)],
        &[pointer_entry(4, &low), freed_entry(9)],
        &[pointer_entry(4, &high), pointer_entry(4, &low)],
        &[pointer_entry(4, &low), pointer_entry(4, &low)],
        &[pointer_entry(5, &low), pointer_entry(4, &high)],
    ];
    for entries in out_of_order {
        assert_eq!(
            error_of(&part(1, 5, 0, &entries.concat())),
            wal_error(WalError::EntriesOutOfOrder)
        );
    }
}

#[test]
fn each_check_of_a_change_has_its_error() {
    let change = |what: u8, fields: &[u8]| [&[5, what][..], fields].concat();
    let from = ValuePlace::Pending(pending(45, 9));
    let to = pointer(13, 364, 9);

    for what in [0, 4, 0xFF] {
        assert_eq!(
            error_of(&change(what, &[])),
            FormatError::UnknownRecordKind { code: what.into() }
        );
    }

    let tree = |raw_page: u32, raw_slot: u16, from: [u8; 16], to: [u8; 16]| {
        [&le32(raw_page)[..], &raw_slot.to_le_bytes(), &from, &to].concat()
    };
    let good = tree(7, 1, from.encode(), to.encode());
    assert_eq!(
        decode(&framed(&change(1, &good))).unwrap().0,
        Record::Change(Change::Value(
            ValueMove::in_tree(page(7), slot(1), from, to).unwrap()
        ))
    );
    assert_eq!(error_of(&change(1, &good[..37])), record_length(5, 39));
    assert_eq!(
        error_of(&change(1, &[&good[..], &[0]].concat())),
        record_length(5, 41)
    );
    assert_eq!(
        error_of(&change(1, &tree(0, 1, from.encode(), to.encode()))),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&change(1, &tree(7, 0, from.encode(), to.encode()))),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&change(1, &tree(7, 1, [0; 16], to.encode()))),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&change(1, &tree(7, 1, from.encode(), [0; 16]))),
        FormatError::MissingId
    );
    // A step gives a cell a place in the value log, never a pending value.
    let still_pending = ValuePlace::Pending(pending(46, 9)).encode();
    assert_eq!(
        error_of(&change(1, &tree(7, 1, from.encode(), still_pending))),
        FormatError::ReservedFileId
    );
    assert_eq!(
        error_of(&change(1, &tree(7, 1, to.encode(), still_pending))),
        FormatError::ReservedFileId
    );
    // A value keeps its length, and moves.
    let invalid = wal_error(WalError::InvalidChange);
    assert_eq!(
        error_of(&change(
            1,
            &tree(7, 1, from.encode(), pointer(13, 364, 8).encode())
        )),
        invalid
    );
    assert_eq!(
        error_of(&change(1, &tree(7, 1, to.encode(), to.encode()))),
        invalid
    );

    let flat =
        |raw_page: u32, from: [u8; 16], to: [u8; 16]| [&le32(raw_page)[..], &from, &to].concat();
    let good = flat(7, from.encode(), to.encode());
    assert_eq!(
        decode(&framed(&change(2, &good))).unwrap().0,
        Record::Change(Change::Value(
            ValueMove::in_flat_leaf(page(7), from, to).unwrap()
        ))
    );
    assert_eq!(error_of(&change(2, &good[..35])), record_length(5, 37));
    assert_eq!(
        error_of(&change(2, &[&good[..], &[0]].concat())),
        record_length(5, 39)
    );
    assert_eq!(
        error_of(&change(2, &flat(0, from.encode(), to.encode()))),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&change(2, &flat(7, to.encode(), to.encode()))),
        invalid
    );
    assert_eq!(
        error_of(&change(2, &flat(7, from.encode(), still_pending))),
        FormatError::ReservedFileId
    );
    // What a cell held before a move is a pointer or a pending value, and
    // nothing else.
    let mut no_id = from.encode();
    no_id[4..12].fill(0);
    assert_eq!(
        error_of(&change(2, &flat(7, no_id, to.encode()))),
        FormatError::MissingId
    );

    // Reclaim.
    let removes = random_tree_run(&mut SplitMix64::new(3), page(7), true, usize::MAX);
    let flat_removes = random_flat_run(&mut SplitMix64::new(3), page(7), true, usize::MAX);
    let reclaim =
        |flags: u8, raw_page: u32, freed: Option<u64>, released: &[ValuePointer], run: &[u8]| {
            let mut fields = vec![flags];
            fields.extend_from_slice(&le32(raw_page));
            if let Some(freed) = freed {
                fields.extend_from_slice(&le64(freed));
            }
            fields.extend_from_slice(&(released.len() as u16).to_le_bytes());
            released
                .iter()
                .for_each(|pointer| fields.extend_from_slice(&pointer.encode()));
            fields.extend_from_slice(run);
            change(3, &fields)
        };
    let (low, high) = (pointer(3, 20, 4), pointer(3, 21, 4));
    for (flags, freed, run) in [
        (1, Some(2), &[][..]),
        (2, None, &removes[..]),
        (3, Some(2), &removes[..]),
        (4, None, &flat_removes[..]),
        (5, Some(9), &flat_removes[..]),
    ] {
        let record = framed(&reclaim(flags, 7, freed, &[low, high], run));
        let (Record::Change(Change::Reclaim(read)), _) = decode(&record).unwrap() else {
            panic!("a reclaim");
        };
        assert_eq!(read.page(), page(7));
        assert_eq!(read.freed(), freed.map(version));
        assert_eq!(read.released().collect::<Vec<_>>(), [low, high]);
        assert_eq!(read.released().len(), 2);
        assert_eq!(
            read.removed().map(|removed| removed.page()),
            (!run.is_empty()).then_some(page(7))
        );
    }
    assert_eq!(
        error_of(&change(3, &[])),
        FormatError::Truncated {
            needed: 11,
            available: 10
        }
    );
    for flags in [8, 0x10, 0x80, 0xF9] {
        assert_eq!(
            error_of(&reclaim(flags, 7, Some(2), &[], &[])),
            FormatError::NonZeroReserved
        );
    }
    assert_eq!(error_of(&reclaim(6, 7, None, &[], &removes)), invalid);
    assert_eq!(error_of(&reclaim(7, 7, Some(2), &[], &removes)), invalid);
    assert_eq!(
        error_of(&reclaim(1, 0, Some(2), &[], &[])),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&reclaim(1, 7, Some(NO_VERSION), &[], &[])),
        FormatError::MissingId
    );
    assert_eq!(
        error_of(&reclaim(1, 7, Some(0), &[], &[])),
        wal_error(WalError::NotACommit)
    );
    // The page is neither freed nor is anything taken out of it.
    assert_eq!(error_of(&reclaim(0, 7, None, &[low], &[])), invalid);
    // Bytes that no flag announces.
    assert_eq!(
        error_of(&reclaim(1, 7, Some(2), &[], &[0])),
        record_length(5, 18)
    );
    // Fewer pointers than the count says.
    let mut short = reclaim(1, 7, Some(2), &[low, high], &[]);
    short.truncate(short.len() - 1);
    assert!(matches!(error_of(&short), FormatError::Truncated { .. }));
    for released in [[high, low], [low, low]] {
        assert_eq!(
            error_of(&reclaim(1, 7, Some(2), &released, &[])),
            wal_error(WalError::EntriesOutOfOrder)
        );
    }
    let mut no_file = reclaim(1, 7, Some(2), &[low], &[]);
    let at = no_file.len() - 16;
    no_file[at..at + 4].fill(0);
    assert_eq!(error_of(&no_file), FormatError::MissingId);
    // A pending value is counted in no file, so no step releases one.
    let mut pending_value = reclaim(1, 7, Some(2), &[low], &[]);
    pending_value[at..at + 4].fill(0xFF);
    assert_eq!(error_of(&pending_value), FormatError::ReservedFileId);

    // The run is of the page, and only takes cells or entries out.
    assert_eq!(
        error_of(&reclaim(2, 8, None, &[], &removes)),
        wal_error(WalError::WrongPage {
            found: page(7),
            expected: page(8),
        })
    );
    assert_eq!(
        error_of(&reclaim(4, 8, None, &[], &flat_removes)),
        wal_error(WalError::WrongPage {
            found: page(7),
            expected: page(8),
        })
    );
    let mut rng = SplitMix64::new(8);
    let mut refused = [0, 0];
    for _ in 0..200 {
        let tree = random_tree_run(&mut rng, page(7), false, usize::MAX);
        let run = DeltaRun::decode(&tree, V1).unwrap();
        let only_removes = run.created().is_none()
            && run
                .deltas()
                .all(|delta| matches!(delta, Delta::Remove { .. }));
        let record = framed(&reclaim(2, 7, None, &[], &tree));
        let read = decode(&record).map(|_| ());
        assert_eq!(read.is_ok(), only_removes, "{run:?}");
        if !only_removes {
            assert_eq!(read, Err(invalid));
            refused[0] += 1;
        }
        let flat = random_flat_run(&mut rng, page(7), false, usize::MAX);
        let run = FlatRun::decode(&flat, V1).unwrap();
        let only_removes = run.changes().all(|change| {
            matches!(
                change,
                FlatChange::Remove { .. } | FlatChange::RemoveRange { .. }
            )
        });
        let record = framed(&reclaim(4, 7, None, &[], &flat));
        let read = decode(&record).map(|_| ());
        assert_eq!(read.is_ok(), only_removes, "{run:?}");
        if !only_removes {
            assert_eq!(read, Err(invalid));
            refused[1] += 1;
        }
    }
    assert!(refused[0] > 50 && refused[1] > 50, "{refused:?}");
    assert_eq!(
        error_of(&reclaim(2, 7, None, &[], &removes[..4])),
        FormatError::Delta(crate::delta::DeltaError::EmptyRun)
    );
    assert_eq!(
        error_of(&reclaim(4, 7, None, &[], &flat_removes[..4])),
        wal_error(WalError::EmptyFlatRun)
    );
}

#[test]
fn every_error_has_its_message() {
    let cases = [
        (
            WalError::NotALog {
                found: FileKind::Segment,
            },
            "a Segment file, not a log",
        ),
        (
            WalError::MissingStart,
            "a log file that does not begin with a start record",
        ),
        (
            WalError::MisplacedStart,
            "a start record that is not the first record",
        ),
        (
            WalError::InvalidBase { base: 9 },
            "a log file that starts at address 9",
        ),
        (
            WalError::NotTheNextFile {
                expected_base: 100,
                found_base: 120,
                expected_after: version(4),
                found_after: version(5),
            },
            "a log file that starts at address 120 after version 5, where the log before it \
             ends at 100 after version 4",
        ),
        (
            WalError::InvalidPlace { offset: 44 },
            "offset 44 is no place in a log file",
        ),
        (
            WalError::AddressOutOfRange,
            "a log address past the end of the log",
        ),
        (
            WalError::RecordLength { kind: 6, len: 8 },
            "a record of kind 6 with a payload of 8 bytes",
        ),
        (
            WalError::NoSuchValuePart {
                part: 2,
                value_len: 30,
            },
            "part 2 of a value of 30 bytes",
        ),
        (
            WalError::UnexpectedValuePart {
                expected: 1,
                found: 3,
            },
            "part 3 of a value where part 1 belongs",
        ),
        (
            WalError::WrongValuePartLength {
                expected: 10,
                found: 9,
            },
            "a part of a value with 9 bytes, where the part has 10",
        ),
        (
            WalError::PartOfAnotherValue,
            "a part of another value inside a value",
        ),
        (
            WalError::UnfinishedValue,
            "a value whose last part is missing",
        ),
        (
            WalError::NotAValue { kind: 6 },
            "a record of kind 6 where a value must stand",
        ),
        (
            WalError::WrongValueLength {
                expected: 5,
                found: 6,
            },
            "a value of 6 bytes where the pending value states 5",
        ),
        (
            WalError::WrongOwner {
                expected: page(7),
                found: page(9),
            },
            "a value owned by page 9, read for a cell of page 7",
        ),
        (
            WalError::NotACommit,
            "version 0 where the version of a commit must stand",
        ),
        (
            WalError::VersionGap {
                after: version(4),
                found: version(6),
            },
            "a commit group of version 6 after the group of version 4",
        ),
        (
            WalError::UnexpectedPart {
                expected: 1,
                found: 0,
            },
            "part 0 of a group where part 1 belongs",
        ),
        (
            WalError::PartOfAnotherGroup,
            "a part of another commit inside a group",
        ),
        (
            WalError::GroupInterrupted { kind: 6 },
            "a record of kind 6 inside a commit group",
        ),
        (
            WalError::UnfinishedGroup,
            "the log ends inside a commit group",
        ),
        (WalError::EmptyPart, "a part of a group with no entry"),
        (
            WalError::PartNotFull,
            "a part of a group that the next entry fits into",
        ),
        (
            WalError::TooManyParts,
            "a group of more parts than can be numbered",
        ),
        (WalError::EntriesOutOfOrder, "entries out of order"),
        (
            WalError::ValueAfterEntries,
            "a value behind the first entry of its group",
        ),
        (
            WalError::RunTooLong { len: 300, max: 181 },
            "a run of 300 bytes exceeds the maximum of 181",
        ),
        (
            WalError::InvalidChange,
            "a background change that no page step makes",
        ),
        (
            WalError::FreedAhead {
                version: version(9),
                latest: version(8),
            },
            "a page freed with version 9, where the latest commit is 8",
        ),
        (
            WalError::CheckpointAhead {
                covers: version(9),
                latest: version(8),
            },
            "a checkpoint mark that covers version 9, where the latest commit is 8",
        ),
        (WalError::EmptyFlatRun, "a flat run with no record"),
        (
            WalError::MisplacedImage,
            "an image that is not the first record of its flat run",
        ),
        (
            WalError::ImageLength {
                stated: 8,
                found: 9,
            },
            "an image of 9 bytes that states a length of 8",
        ),
        (
            WalError::FlatFieldTooLong { len: 65_536 },
            "a field of 65536 bytes in a record of a flat run",
        ),
        (
            WalError::PlaceLength { len: 15 },
            "the place of a value out of line in 15 bytes",
        ),
        (WalError::InvalidRange, "a range of fewer than two entries"),
        (
            WalError::NoPage,
            "a flat run that starts from no image, and no page",
        ),
        (
            WalError::WrongPage {
                found: page(7),
                expected: page(8),
            },
            "a run or an image of page 7 where one of page 8 belongs",
        ),
        (
            WalError::WrongPageKind,
            "a change for the other kind of flat page",
        ),
        (WalError::KeyAbsent, "a key that must be in the page is not"),
        (
            WalError::KeyPresent,
            "a key that must not be in the page is",
        ),
        (
            WalError::RangeTooLong,
            "a range that runs past the last entry",
        ),
        (
            WalError::NoSuchPlace,
            "no entry holds the place the value is moved from",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        assert_eq!(FormatError::from(error).to_string(), format!("log: {text}"));
        assert!(std::error::Error::source(&error).is_none());
    }
}
