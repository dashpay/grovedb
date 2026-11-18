//! Writing a group, and what a group that is too long for one record
//! becomes.

use super::*;
use crate::delta::DeltaError;

fn tree(raw_page: u32) -> Vec<u8> {
    let mut run = Vec::new();
    let mut writer = DeltaRunWriter::new(page(raw_page), None, &mut run);
    writer.push(&Delta::Remove { slot: slot(1) }).unwrap();
    writer.finish().unwrap();
    run
}

fn flat(raw_page: u32) -> Vec<u8> {
    let mut run = Vec::new();
    let mut writer = FlatRunWriter::new(page(raw_page), &mut run);
    writer.push(&FlatChange::Remove { key: b"k" }).unwrap();
    writer.finish().unwrap();
    run
}

fn tree_run(group: &mut GroupWriter<'_>, raw_page: u32) -> Result<(), FormatError> {
    group.tree_run(&DeltaRun::decode(&tree(raw_page), V1).unwrap())
}

fn flat_run(group: &mut GroupWriter<'_>, raw_page: u32) -> Result<(), FormatError> {
    group.flat_run(&FlatRun::decode(&flat(raw_page), V1).unwrap())
}

#[test]
fn a_group_is_written_in_one_order_and_a_refused_call_leaves_it_as_it_was() {
    let mut out = Vec::new();
    assert_eq!(
        GroupWriter::new(CommitVersion::EMPTY, address(45), &mut out).err(),
        Some(wal_error(WalError::NotACommit))
    );

    let mut group = GroupWriter::new(version(1), address(45), &mut out).unwrap();
    assert_eq!(group.encoded_len(), 0);
    let out_of_order = wal_error(WalError::EntriesOutOfOrder);
    let late = wal_error(WalError::ValueAfterEntries);

    // Each call that is refused is followed by the same call for a later
    // page or pointer, which is taken.
    macro_rules! refused {
        ($call:expr, $error:expr) => {{
            let before = group.encoded_len();
            assert_eq!($call, Err($error));
            assert_eq!(group.encoded_len(), before);
        }};
    }
    group.value(page(4), b"value").unwrap();
    tree_run(&mut group, 4).unwrap();
    refused!(group.value(page(4), b"value").map(|_| ()), late);
    let carried = CarriedValue::new(page(4), page(3), pointer(1, 20, 9));
    refused!(group.carried(&carried).map(|_| ()), late);

    refused!(tree_run(&mut group, 4), out_of_order);
    refused!(tree_run(&mut group, 3), out_of_order);
    refused!(flat_run(&mut group, 4), out_of_order);
    refused!(group.freed(page(4)), out_of_order);
    flat_run(&mut group, 5).unwrap();
    refused!(group.freed(page(5)), out_of_order);
    refused!(tree_run(&mut group, 5), out_of_order);
    group.freed(page(6)).unwrap();
    refused!(group.freed(page(6)), out_of_order);
    refused!(flat_run(&mut group, 2), out_of_order);

    // A run that its writer gives up, or leaves empty, is not in the group.
    refused!(
        group.tree_run_with(page(8), None, |run| {
            run.push(&Delta::Remove { slot: slot(1) })?;
            Err(FormatError::MissingId)
        }),
        FormatError::MissingId
    );
    refused!(
        group.tree_run_with(page(8), None, |_| Ok(())),
        FormatError::Delta(DeltaError::EmptyRun)
    );
    refused!(
        group.flat_run_with(page(8), |_| Ok(())),
        wal_error(WalError::EmptyFlatRun)
    );
    refused!(
        group.flat_run_with(page(8), |run| run.push(&FlatChange::RemoveRange {
            key: b"k",
            count: 1,
        })),
        wal_error(WalError::InvalidRange)
    );
    group
        .tree_run_with(page(8), Some(3), |_| Ok(()))
        .expect("a run that only creates its page");

    let (low, high) = (pointer(3, 20, 4), pointer(3, 21, 4));
    group.released(&high).unwrap();
    refused!(group.released(&high), out_of_order);
    refused!(group.released(&low), out_of_order);
    refused!(group.freed(page(9)), out_of_order);
    refused!(tree_run(&mut group, 9), out_of_order);
    group.acquired(&low).unwrap();
    refused!(group.acquired(&low), out_of_order);
    refused!(group.released(&pointer(9, 20, 4)), out_of_order);
    group.acquired(&high).unwrap();

    let len = group.finish();
    assert_eq!(len, out.len());
    let mut log = new_log(1, &LogStart::FIRST);
    log.extend_from_slice(&out);
    let (items, tail) = items(&log);
    assert_eq!(tail.failure(), None);
    let [LogItem::Commit(group)] = items[..] else {
        panic!("one group");
    };
    let entries: Vec<_> = group.entries().collect();
    assert_eq!(
        entries.iter().map(Entry::page).collect::<Vec<_>>(),
        [
            Some(page(4)),
            Some(page(5)),
            Some(page(6)),
            Some(page(8)),
            None,
            None,
            None
        ]
    );
    assert!(matches!(entries[3], Entry::Tree(run) if run.created() == Some(3)));
    assert_eq!(
        entries[4..],
        [
            Entry::Released(high),
            Entry::Acquired(low),
            Entry::Acquired(high)
        ]
    );
}

#[test]
fn a_group_that_is_given_up_leaves_nothing() {
    let mut out = b"before".to_vec();
    let mut group = GroupWriter::new(version(1), address(45), &mut out).unwrap();
    group.value(page(4), b"value").unwrap();
    group.freed(page(4)).unwrap();
    assert!(group.encoded_len() > 0);
    group.abandon();
    assert_eq!(out, b"before");

    // A group with nothing in it is one part with no entry.
    let mut out = b"before".to_vec();
    let group = GroupWriter::new(version(1), address(45), &mut out).unwrap();
    assert_eq!(group.finish(), PART_HEADER_LEN);
    assert_eq!(out[6..], part_record(1, 1, 0, &[]));
}

#[test]
fn a_group_too_long_for_one_record_is_cut_between_entries_in_one_way() {
    let mut rng = SplitMix64::new(0x5917);
    let mut several = 0;
    for round in 0..300 {
        // Entries of drawn lengths: runs of removes, freed pages, pointers.
        let mut out = Vec::new();
        let at = address(45 + round);
        let mut group = GroupWriter::with_limits(version(1), at, SMALL, &mut out).unwrap();
        let mut written = Vec::new();
        for raw_page in 1..=(rng.next_u64() % 40) as u32 {
            if rng.next_u64().is_multiple_of(3) {
                group.freed(page(raw_page)).unwrap();
                written.push(freed_entry(raw_page));
                continue;
            }
            let mut run = Vec::new();
            let mut writer = DeltaRunWriter::new(page(raw_page), None, &mut run);
            for raw_slot in 1..=1 + (rng.next_u64() % 58) as u16 {
                writer
                    .push(&Delta::Remove {
                        slot: slot(raw_slot),
                    })
                    .unwrap();
            }
            writer.finish().unwrap();
            group
                .tree_run(&DeltaRun::decode(&run, V1).unwrap())
                .unwrap();
            written.push(run_entry(1, &run));
        }
        for released in sorted_pointers(&mut rng, 9) {
            group.released(&released).unwrap();
            written.push(pointer_entry(4, &released));
        }
        group.finish();

        // The parts: each is as full as the next entry lets it be, they
        // are numbered from 0, and the last one says so.
        let mut parts = Vec::new();
        let mut at = 0;
        while at < out.len() {
            let (Record::Part(part), len) = Record::decode_with(&out[at..], V1, SMALL).unwrap()
            else {
                panic!("a part");
            };
            parts.push((part, len - 8));
            at += len;
        }
        several += usize::from(parts.len() > 1);
        let mut entry = 0;
        for (number, (part, payload_len)) in parts.iter().enumerate() {
            assert_eq!(part.part(), number as u32);
            assert_eq!(part.version(), version(1));
            assert_eq!(part.is_last(), number + 1 == parts.len());
            assert!(*payload_len <= SMALL.max_payload);
            let held = part.entries().count();
            assert_eq!(part.is_empty(), held == 0);
            assert!(held > 0 || parts.len() == 1);
            let payload: usize = 14
                + written[entry..entry + held]
                    .iter()
                    .map(Vec::len)
                    .sum::<usize>();
            assert_eq!(payload, *payload_len);
            entry += held;
            if let Some(next) = written.get(entry) {
                assert!(payload + next.len() > SMALL.max_payload, "round {round}");
            }
        }
        assert_eq!(entry, written.len());

        // A reader takes the parts as one group, with the entries in the
        // order they were written.
        let mut log = new_log(1, &LogStart::FIRST);
        log.extend_from_slice(&out);
        let (items, tail) = items_with(&log, SMALL);
        assert_eq!(tail.failure(), None);
        let [LogItem::Commit(group)] = items[..] else {
            panic!("one group");
        };
        assert_eq!(group.entries().count(), written.len());
        assert_eq!(
            written_again(&items[0], &LogStart::FIRST, 45, SMALL),
            out,
            "round {round}"
        );
    }
    assert!(several > 150, "{several}");
}

#[test]
fn an_entry_too_long_for_a_record_is_refused() {
    let mut out = Vec::new();
    let mut group = GroupWriter::with_limits(version(1), address(45), SMALL, &mut out).unwrap();
    group.freed(page(1)).unwrap();
    let before = group.encoded_len();

    let max = SMALL.max_payload - 14 - 5;
    assert_eq!(max, 181);
    let run_of = |records: usize| {
        let mut run = Vec::new();
        let mut writer = DeltaRunWriter::new(page(2), None, &mut run);
        for raw_slot in 1..=records as u16 {
            writer
                .push(&Delta::Remove {
                    slot: slot(raw_slot),
                })
                .unwrap();
        }
        writer.finish().unwrap();
        run
    };
    // 4 bytes and 3 for each record: 59 records are 181 bytes.
    let longest = run_of(59);
    let too_long = run_of(60);
    assert_eq!((longest.len(), too_long.len()), (181, 184));
    assert_eq!(
        group.tree_run(&DeltaRun::decode(&too_long, V1).unwrap()),
        Err(wal_error(WalError::RunTooLong { len: 184, max: 181 }))
    );
    assert_eq!(group.encoded_len(), before);
    group
        .tree_run(&DeltaRun::decode(&longest, V1).unwrap())
        .unwrap();
    group.finish();

    // The longest run is a part of its own.
    let (Record::Part(first), len) = Record::decode_with(&out, V1, SMALL).unwrap() else {
        panic!("a part");
    };
    assert_eq!(first.entries().collect::<Vec<_>>(), [Entry::Freed(page(1))]);
    let (Record::Part(second), second_len) = Record::decode_with(&out[len..], V1, SMALL).unwrap()
    else {
        panic!("a part");
    };
    assert_eq!(second_len, 8 + SMALL.max_payload);
    assert!(second.is_last());

    assert_eq!(MAX_RUN_LEN, MAX_RECORD_PAYLOAD_LEN - 14 - 5);
    assert_eq!(MAX_RUN_LEN, (1 << 27) - 23);

    // Entries that fill a part to its last byte are one part, and the
    // next entry begins another: 3 freed pages and a run of 166 bytes are
    // the 186 bytes a part holds.
    let mut out = Vec::new();
    let mut group = GroupWriter::with_limits(version(1), address(45), SMALL, &mut out).unwrap();
    for raw_page in 1..=3 {
        group.freed(page(raw_page)).unwrap();
    }
    let mut filling = run_of(54);
    filling[..4].copy_from_slice(&page(4).encode());
    assert_eq!(filling.len(), 166);
    group
        .tree_run(&DeltaRun::decode(&filling, V1).unwrap())
        .unwrap();
    assert_eq!(group.encoded_len(), 8 + SMALL.max_payload);
    group.freed(page(5)).unwrap();
    assert_eq!(group.finish(), 8 + SMALL.max_payload + PART_HEADER_LEN + 5);
    let (Record::Part(full), len) = Record::decode_with(&out, V1, SMALL).unwrap() else {
        panic!("a part");
    };
    assert_eq!(
        (len, full.is_last(), full.entries().count()),
        (208, false, 4)
    );
    let mut log = new_log(1, &LogStart::FIRST);
    log.extend_from_slice(&out);
    assert_eq!(items_with(&log, SMALL).1.failure(), None);

    // The same entries with the cut one entry earlier are not the group.
    let entries = [
        freed_entry(1),
        freed_entry(2),
        freed_entry(3),
        run_entry(1, &filling),
        freed_entry(5),
    ];
    let early = [
        part_record(0, 1, 0, &entries[..3].concat()),
        part_record(1, 1, 1, &entries[3..].concat()),
    ]
    .concat();
    assert_eq!(
        tail_after(&LogStart::FIRST, SMALL, &early)
            .failure()
            .map(|failure| failure.error),
        Some(wal_error(WalError::PartNotFull))
    );
    let exact = [
        part_record(0, 1, 0, &entries[..4].concat()),
        part_record(1, 1, 1, &entries[4..].concat()),
    ]
    .concat();
    assert_eq!(exact, out);
}
