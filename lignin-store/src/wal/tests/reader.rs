//! The recovery iterator: what it gives of a log, where it stops, and what
//! it takes for damage.

use lignin_format::{
    wal::{self, LogStart, WalError},
    FormatError, FormatVersion,
};
use lignin_io::sim::{Event, OpKind, SimConfig};

use super::*;
use crate::table::LogDurable;

/// A session that wrote several files, the last with an end that was not
/// synced. Returns the file system, what was written, and the last file as
/// the manifest names it.
fn several_files(seed: u64) -> (lignin_io::SimVfs, Vec<Item>, LiveFile) {
    let (fs, mut db) = sim_db(SimConfig::new(seed), small_files(2500));
    let mut rng = Rng(seed);
    let mut written = Vec::new();
    while db.log.end().unwrap().file() < file_id(3) {
        written.push(db.item(&mut rng).unwrap());
        db.roll().unwrap();
    }
    for _ in 0..4 {
        written.push(db.item(&mut rng).unwrap());
    }
    db.log.sync().unwrap();
    db.log.commit(&mut db.manifest).unwrap();
    // Groups of several records behind what was synced.
    for _ in 0..12 {
        written.push(db.commit(&mut rng).unwrap());
        written.push(db.item(&mut rng).unwrap());
    }
    let last = db.named()[2];
    assert_eq!((last.id(), last.state()), (file_id(3), FileState::Open));
    (fs, written, last)
}

/// The offsets inside the group `item` at which one of its records ends
/// and another begins.
fn record_ends_inside(item: &Item) -> Vec<u64> {
    let mut ends = Vec::new();
    let mut at = 0;
    while at < item.bytes.len() {
        at += wal::record_len(&item.bytes[at..], FormatVersion::V1).unwrap();
        if at < item.bytes.len() {
            ends.push(item.at.offset() + at as u64);
        }
    }
    ends
}

#[test]
fn the_recovery_iterator_stops_at_the_first_incomplete_group() {
    let (mut cuts, mut inside_a_group, mut between_records, mut flips) = (0, 0, 0, 0);
    for seed in 1..=3 {
        let (fs, written, last) = several_files(seed);
        let vfs = sim::shared(&fs);
        let dir = sim::db_dir();
        let path = log_path(dir, 3);
        let whole = file_bytes(&fs, &path);
        let valid = last.valid_len() as usize;
        assert!(valid > FIRST as usize && valid < whole.len());
        assert_eq!(read_log(&vfs, dir).unwrap().0, written);

        // What a reader must give of the log when the last file is good up
        // to `len` bytes: the items that lie wholly before that.
        let before = |len: usize| -> Vec<Item> {
            let good =
                |item: &&Item| item.at.file() < file_id(3) || item.end.offset() <= len as u64;
            written.iter().take_while(good).cloned().collect()
        };
        let check = |len: usize, what: &str| {
            let (read, end) = read_log(&vfs, dir).unwrap_or_else(|error| panic!("{what}: {error}"));
            let expected = before(len);
            assert_eq!(read, expected, "{what}");
            let last_item = expected.last().unwrap();
            assert_eq!(end.place(), Some(last_item.end), "{what}");
            let latest = expected.iter().filter_map(|item| item.version).max();
            assert_eq!(Some(end.latest()), latest, "{what}");
        };
        let groups: Vec<&Item> = written
            .iter()
            .filter(|item| item.at.file() == file_id(3) && item.version.is_some())
            .collect();

        // The file cut at every length behind what the manifest vouches
        // for: a group whose last record is missing is not given, however
        // many of its records are whole, and nothing behind it is.
        for len in valid..whole.len() {
            put_file(&fs, &path, &whole[..len]);
            check(len, &format!("seed {seed}, cut at {len}"));
            cuts += 1;
            for group in &groups {
                let (at, end) = (group.at.offset() as usize, group.end.offset() as usize);
                inside_a_group += u64::from(at < len && len < end);
                between_records += u64::from(record_ends_inside(group).contains(&(len as u64)));
            }
        }

        // Each byte behind it with a bit flipped: the item that holds the
        // byte is not given, and nothing behind it is.
        for at in valid..whole.len() {
            let mut damaged = whole.clone();
            damaged[at] ^= 1 << (at % 8);
            put_file(&fs, &path, &damaged);
            check(at, &format!("seed {seed}, a flipped bit at {at}"));
            flips += 1;
        }

        // Zeros from any item's first byte on, as a length that survived
        // without its bytes reads.
        for item in written.iter().filter(|item| item.at.file() == file_id(3)) {
            let at = item.at.offset() as usize;
            if at < valid {
                continue;
            }
            let mut damaged = whole.clone();
            damaged[at..].fill(0);
            put_file(&fs, &path, &damaged);
            check(at, &format!("seed {seed}, zeros from {at}"));
        }
        put_file(&fs, &path, &whole);
        assert_eq!(read_log(&vfs, dir).unwrap().0, written);
    }
    println!(
        "{cuts} cuts, {inside_a_group} inside a group, {between_records} between two records \
         of a group, {flips} flipped bits"
    );
    assert!(cuts > 5_000 && flips > 5_000);
    assert!(inside_a_group > 3_000 && between_records > 20);
}

#[test]
fn a_reader_gives_what_a_scan_of_each_file_gives_whatever_it_fetches_at_a_time() {
    let (fs, written, _) = several_files(7);
    let vfs = sim::shared(&fs);
    let dir = sim::db_dir();

    // The format's own reader, on each file as a whole.
    let mut scanned = Vec::new();
    for file in 1..=3 {
        let bytes = file_bytes(&fs, &log_path(dir, file));
        let mut scan = wal::scan(&bytes).unwrap();
        let mut at = scan.offset();
        for item in scan.by_ref() {
            let mut again = Vec::new();
            match item {
                LogItem::Commit(group) => again.extend_from_slice(group.as_bytes()),
                LogItem::Change(change) => change.write(&mut again).unwrap(),
                LogItem::Mark(mark) => mark.write(&mut again).unwrap(),
            }
            scanned.push((place(file, at), again));
            at += scanned.last().unwrap().1.len() as u64;
        }
        assert_eq!(scan.tail().failure(), None);
    }
    let of = |items: &[Item]| -> Vec<(LogPlace, Vec<u8>)> {
        items
            .iter()
            .map(|item| (item.at, item.bytes.clone()))
            .collect()
    };
    assert_eq!(of(&written), scanned);

    let (_, whole_end) = read_log(&vfs, dir).unwrap();
    for window in [1, 2, 7, 44, 45, 46, 100, 512, 4096, 1 << 20] {
        let (read, end) = read_log_with(&vfs, dir, window).unwrap();
        assert_eq!(read, written, "a window of {window} bytes");
        assert_eq!(end, whole_end, "a window of {window} bytes");
    }

    // Each group comes with the address its values are named by.
    let manifest = read_manifest(&fs, dir).unwrap();
    let mut reader = LogReader::new(vfs.clone(), dir, &manifest).with_window(64);
    let mut groups = 0;
    while let Some(read) = reader
        .next_item(|read| match read.item {
            LogItem::Commit(group) => Some((read.at, group.address())),
            _ => None,
        })
        .unwrap()
    {
        let Some((at, address)) = read else { continue };
        let ends = whole_end.files();
        let file = ends.iter().find(|end| end.file() == at.file()).unwrap();
        assert_eq!(file.start().address(at.offset()).unwrap(), address);
        groups += 1;
    }
    assert!(groups > 20);
    assert_eq!(reader.end(), Some(&whole_end));
}

#[test]
fn a_failure_below_the_valid_length_is_damage_and_not_an_end() {
    let (fs, written, last) = several_files(11);
    let vfs = sim::shared(&fs);
    let dir = sim::db_dir();
    let valid = last.valid_len() as usize;
    let in_first = written.iter().find(|item| item.version.is_some()).unwrap();
    assert_eq!(in_first.at.file(), file_id(1));

    for (file, at) in [
        // In the file the last session left open, below what was synced.
        (3, valid - 1),
        (3, FIRST as usize),
        // In a closed file.
        (1, in_first.at.offset() as usize + 9),
    ] {
        let path = log_path(dir, file);
        let whole = file_bytes(&fs, &path);
        let mut damaged = whole.clone();
        damaged[at] ^= 0x40;
        put_file(&fs, &path, &damaged);
        let error = read_log(&vfs, dir).unwrap_err();
        let item = written
            .iter()
            .find(|item| item.at.file() == file_id(file) && item.end.offset() > at as u64)
            .unwrap();
        assert!(
            matches!(&error, LogError::Damaged { path: found, offset, valid_len, source }
                if *found == path
                    && *offset == item.at.offset()
                    && *valid_len >= item.end.offset()
                    && matches!(source, FormatError::ChecksumMismatch { .. })),
            "{file} at {at}: {error}"
        );
        // After an error the reader gives no item and no end of the log.
        let manifest = read_manifest(&fs, dir).unwrap();
        let mut reader = LogReader::new(vfs.clone(), dir, &manifest);
        while reader.next_item(|_| ()).is_ok_and(|read| read.is_some()) {}
        assert!(matches!(
            reader.next_item(|_| ()),
            Err(LogError::ReaderFailed)
        ));
        assert!(reader.end().is_none());
        assert!(matches!(reader.into_end(), Err(LogError::ReaderFailed)));
        put_file(&fs, &path, &whole);
    }

    // A file that is shorter than its valid length.
    for file in [1, 3] {
        let path = log_path(dir, file);
        let whole = file_bytes(&fs, &path);
        let valid_len = named(&read_manifest(&fs, dir).unwrap())[file as usize - 1].valid_len();
        put_file(&fs, &path, &whole[..valid_len as usize - 1]);
        let error = read_log(&vfs, dir).unwrap_err();
        assert!(
            matches!(&error, LogError::FileTooShort { valid_len: named, len, .. }
                if *named == valid_len && *len == valid_len - 1),
            "{error}"
        );
        put_file(&fs, &path, &whole);
    }

    // A closed file ends at its valid length, whatever follows it.
    let path = log_path(dir, 1);
    let mut longer = file_bytes(&fs, &path);
    longer.extend_from_slice(&written[0].bytes);
    longer.extend_from_slice(b"and bytes that are no record");
    put_file(&fs, &path, &longer);
    assert_eq!(read_log(&vfs, dir).unwrap().0, written);

    // A file that is missing.
    fs.remove_file(&log_path(dir, 2)).unwrap();
    let error = read_log(&vfs, dir).unwrap_err();
    assert!(
        error
            .io_error()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "{error}"
    );
}

#[test]
fn a_file_continues_the_log_only_where_the_one_before_it_stops_being_valid() {
    let mut rng = Rng(21);
    let (first, first_ends, first_latest) = crafted_file(1, LogStart::FIRST, 6, &mut rng);
    let next = LogStart::FIRST
        .next(first.len() as u64, first_latest)
        .unwrap();
    let (second, second_ends, second_latest) = crafted_file(2, next, 5, &mut rng);
    let after = next.next(second.len() as u64, second_latest).unwrap();
    let (third, third_ends, _) = crafted_file(3, after, 4, &mut rng);
    let open = |file: u32, valid_len: u64| log_file(file, FileState::Open, valid_len);
    let closed = |file: u32, valid_len: usize| log_file(file, FileState::Closed, valid_len as u64);
    let read = |files: &[(LiveFile, &[u8])]| {
        let (_fs, vfs) = crafted(files);
        read_log(&vfs, sim::db_dir())
    };
    let counts = |end: &LogEnd| -> Vec<(u32, u64)> {
        end.files()
            .iter()
            .map(|file| (file.file().get(), file.valid_len()))
            .collect()
    };
    let (len1, len2, len3) = (first.len() as u64, second.len() as u64, third.len() as u64);

    // The three files, closed or open, are one log.
    for files in [
        [
            (closed(1, first.len()), &first[..]),
            (closed(2, second.len()), &second[..]),
            (open(3, third_ends[1]), &third[..]),
        ],
        [
            (open(1, HEADER), &first[..]),
            (open(2, HEADER), &second[..]),
            (open(3, HEADER), &third[..]),
        ],
    ] {
        let (items, end) = read(&files).unwrap();
        assert_eq!(items.len(), 15);
        assert_eq!(counts(&end), [(1, len1), (2, len2), (3, len3)]);
        assert_eq!(end.unread(), []);
        assert_eq!(
            end.next_start().unwrap(),
            after.next(len3, end.latest()).unwrap()
        );
    }

    // The first file lost its end in a crash. The second was written for
    // the whole of it, so the log ends in the first, and what the second
    // and the third hold is not read.
    for lost in [first_ends[4], first_ends[2], FIRST] {
        let (items, end) = read(&[
            (open(1, HEADER), &first[..lost as usize]),
            (open(2, HEADER), &second[..]),
            (open(3, HEADER), &third[..]),
        ])
        .unwrap();
        assert_eq!(
            items.len(),
            first_ends.iter().filter(|end| **end <= lost).count()
        );
        assert_eq!(counts(&end), [(1, lost)]);
        assert_eq!(end.unread(), [file_id(2), file_id(3)]);
    }
    // The same for a torn item at the end of the first file.
    let (_, end) = read(&[
        (open(1, first_ends[3]), &first[..first.len() - 3]),
        (open(2, HEADER), &second[..]),
    ])
    .unwrap();
    assert_eq!(counts(&end), [(1, first_ends[4])]);
    assert_eq!(end.unread(), [file_id(2)]);

    // The second file lost its end: the log ends there, behind all of the
    // first.
    let (items, end) = read(&[
        (closed(1, first.len()), &first[..]),
        (open(2, FIRST), &second[..second_ends[1] as usize + 5]),
        (open(3, HEADER), &third[..]),
    ])
    .unwrap();
    assert_eq!(items.len(), 6 + 2);
    assert_eq!(counts(&end), [(1, len1), (2, second_ends[1])]);
    assert_eq!(end.unread(), [file_id(3)]);

    // A file with no start record, or with a part of one, holds nothing.
    for len in [HEADER as usize, HEADER as usize + 9, FIRST as usize - 1] {
        let (items, end) = read(&[
            (closed(1, first.len()), &first[..]),
            (open(2, HEADER), &second[..len]),
        ])
        .unwrap();
        assert_eq!(items.len(), 6);
        assert_eq!(end.unread(), [file_id(2)]);
        assert_eq!(end.place(), Some(place(1, len1)));
        assert_eq!(end.next_start().unwrap(), next);
    }
    // Also as the only file, and before others.
    let (items, end) = read(&[(open(1, HEADER), &first[..HEADER as usize])]).unwrap();
    assert_eq!((items.len(), end.files().len()), (0, 0));
    assert_eq!(end.unread(), [file_id(1)]);
    assert_eq!(end.place(), None);
    assert_eq!(end.latest(), CommitVersion::EMPTY);
    assert_eq!(end.next_start().unwrap(), LogStart::FIRST);
    let (_, end) = read(&[
        (open(1, HEADER), &first[..HEADER as usize]),
        (open(2, HEADER), &second[..]),
    ])
    .unwrap();
    assert_eq!(end.unread(), [file_id(1), file_id(2)]);

    // The first file of a log that has dropped its earlier ones is read
    // from where it says it begins.
    let (items, end) = read(&[
        (closed(2, second.len()), &second[..]),
        (open(3, FIRST), &third[..]),
    ])
    .unwrap();
    assert_eq!(items.len(), 9);
    assert_eq!(end.files()[0].start(), next);

    // Where the manifest vouches for the start record of a file, a record
    // that is not the one the log before it asks for is damage: the
    // manifest was written when the file before it was whole.
    let error = read(&[
        (open(1, HEADER), &first[..first_ends[4] as usize]),
        (open(2, FIRST), &second[..]),
    ])
    .unwrap_err();
    assert!(
        matches!(&error, LogError::Damaged { offset, valid_len, source, .. }
            if *offset == HEADER
                && *valid_len == FIRST
                && matches!(source, FormatError::Wal(WalError::NotTheNextFile { .. }))),
        "{error}"
    );
    let mut torn = second.clone();
    torn[30] ^= 1;
    let error = read(&[
        (closed(1, first.len()), &first[..]),
        (open(2, FIRST), &torn[..]),
    ])
    .unwrap_err();
    assert!(matches!(error, LogError::Damaged { offset, .. } if offset == HEADER));
    // Without the manifest's word for it, that file holds nothing.
    let (_, end) = read(&[
        (closed(1, first.len()), &first[..]),
        (open(2, HEADER), &torn[..]),
    ])
    .unwrap();
    assert_eq!(end.unread(), [file_id(2)]);

    // A closed file must hold whole items up to its valid length.
    let inside = second_ends[2] as usize - 4;
    let error = read(&[
        (closed(1, first.len()), &first[..]),
        (closed(2, inside), &second[..]),
    ])
    .unwrap_err();
    assert!(
        matches!(&error, LogError::Damaged { offset, valid_len, .. }
            if *offset == second_ends[1] && *valid_len == inside as u64),
        "{error}"
    );
}

#[test]
fn a_file_that_is_not_the_log_file_of_its_name_is_refused() {
    let mut rng = Rng(22);
    let (first, _, _) = crafted_file(1, LogStart::FIRST, 3, &mut rng);
    let (other, _, _) = crafted_file(5, LogStart::FIRST, 3, &mut rng);
    let read = |named: LiveFile, bytes: &[u8]| {
        let (_fs, vfs) = crafted(&[(named, bytes)]);
        read_log(&vfs, sim::db_dir()).unwrap_err()
    };
    let open = log_file(1, FileState::Open, HEADER);

    // The file of another id.
    let error = read(open, &other);
    assert!(
        matches!(&error, LogError::WrongFileId { found: 5, .. }),
        "{error}"
    );
    // A file of another kind, and bytes that are no file of the engine.
    let mut segment = first.clone();
    let header = FileHeader::new(FileKind::Segment, FormatVersion::CURRENT, 1).unwrap();
    segment[..HEADER as usize].copy_from_slice(&header.encode());
    let error = read(open, &segment);
    assert!(
        matches!(&error, LogError::Damaged { offset: 0, source, .. }
            if matches!(source, FormatError::Wal(WalError::NotALog { .. }))),
        "{error}"
    );
    let mut garbage = first.clone();
    garbage[3] ^= 0xFF;
    let error = read(open, &garbage);
    assert!(
        matches!(&error, LogError::Damaged { offset: 0, source, .. }
            if matches!(source, FormatError::BadMagic)),
        "{error}"
    );
}

#[test]
fn the_end_of_a_log_says_how_far_the_manifest_vouches_for_it() {
    let mut rng = Rng(23);
    let (first, first_ends, first_latest) = crafted_file(1, LogStart::FIRST, 6, &mut rng);
    let next = LogStart::FIRST
        .next(first.len() as u64, first_latest)
        .unwrap();
    let (second, second_ends, _) = crafted_file(2, next, 5, &mut rng);
    let durable = |files: &[(LiveFile, &[u8])]| {
        let (_fs, vfs) = crafted(files);
        let (_, end) = read_log(&vfs, sim::db_dir()).unwrap();
        (end.durable(), end)
    };
    let open = |file: u32, valid_len: u64| log_file(file, FileState::Open, valid_len);

    assert_eq!(
        durable(&[]).0,
        LogDurable {
            version: CommitVersion::EMPTY,
            place: None
        }
    );
    // Nothing but headers.
    let (found, _) = durable(&[
        (open(1, HEADER), &first[..]),
        (open(2, HEADER), &second[..]),
    ]);
    assert_eq!(found.place, None);
    assert_eq!(found.version, CommitVersion::EMPTY);

    // A place inside the first file: the second counts for nothing, even
    // if its own bytes were synced.
    let (found, end) = durable(&[
        (open(1, first_ends[2]), &first[..]),
        (open(2, HEADER), &second[..]),
    ]);
    assert_eq!(found.place, Some(place(1, first_ends[2])));
    assert_eq!(found.version, end.files()[0].named_latest());
    assert!(found.version <= first_latest);
    assert_eq!(end.files()[0].named_len(), first_ends[2]);
    assert_eq!(end.files()[0].latest(), first_latest);

    // All of the first file, and a place in the second.
    let closed = log_file(1, FileState::Closed, first.len() as u64);
    let (found, _) = durable(&[(closed, &first[..]), (open(2, HEADER), &second[..])]);
    assert_eq!(found.place, Some(place(1, first.len() as u64)));
    assert_eq!(found.version, first_latest);
    let (found, end) = durable(&[(closed, &first[..]), (open(2, second_ends[3]), &second[..])]);
    assert_eq!(found.place, Some(place(2, second_ends[3])));
    assert_eq!(found.version, end.files()[1].named_latest());
    let (found, end) = durable(&[
        (closed, &first[..]),
        (open(2, second.len() as u64), &second[..]),
    ]);
    assert_eq!(found.place, end.place());
    assert_eq!(found.version, end.latest());
}

#[test]
fn a_valid_length_inside_a_record_is_not_what_a_manifest_of_this_log_says() {
    let mut rng = Rng(21);
    let (bytes, ends, _) = crafted_file(1, LogStart::FIRST, 6, &mut rng);
    let read = |state: FileState, valid_len: u64| {
        let (_fs, vfs) = crafted(&[(log_file(1, state, valid_len), &bytes)]);
        read_log(&vfs, sim::db_dir())
    };
    // A manifest vouches for the header alone, or for whole items.
    for valid_len in ends.iter().copied().chain([HEADER, FIRST]) {
        let (items, end) = read(FileState::Open, valid_len).unwrap();
        assert_eq!(items.len(), 6);
        assert_eq!(
            end.durable().place,
            LogPlace::new(file_id(1), valid_len).ok()
        );
    }
    // Inside the start record.
    for valid_len in [HEADER + 1, 30, FIRST - 1] {
        let error = read(FileState::Open, valid_len).unwrap_err();
        assert!(
            matches!(&error, LogError::NotAtRecordEnd { valid_len: found, offset, .. }
                if *found == valid_len && *offset == HEADER),
            "{valid_len}: {error}"
        );
    }
    // Inside an item of an open file, which is read whole behind it.
    let mut begins = FIRST;
    for &end in &ends {
        for valid_len in [begins + 1, end - 1] {
            let error = read(FileState::Open, valid_len).unwrap_err();
            assert!(
                matches!(&error, LogError::NotAtRecordEnd { valid_len: found, offset, .. }
                    if *found == valid_len && *offset == begins),
                "{valid_len}: {error}"
            );
            // A closed file ends at its valid length: the item is cut.
            let error = read(FileState::Closed, valid_len).unwrap_err();
            assert!(
                matches!(&error, LogError::Damaged { offset, .. } if *offset == begins),
                "{valid_len}: {error}"
            );
        }
        begins = end;
    }
}

#[test]
fn a_file_the_manifest_vouches_for_behind_the_end_of_the_log_is_not_passed_over() {
    let mut rng = Rng(22);
    let dir = sim::db_dir();
    let (first, first_ends, first_latest) = crafted_file(1, LogStart::FIRST, 4, &mut rng);
    let next = LogStart::FIRST
        .next(first.len() as u64, first_latest)
        .unwrap();
    let (second, _, second_latest) = crafted_file(2, next, 3, &mut rng);
    let after = next.next(second.len() as u64, second_latest).unwrap();
    let (third, third_ends, _) = crafted_file(3, after, 3, &mut rng);
    let open = |file: u32, valid_len: u64| log_file(file, FileState::Open, valid_len);
    let whole = log_file(1, FileState::Closed, first.len() as u64);
    let behind_end = |files: &[(LiveFile, &[u8])], valid_len: u64| {
        let (_fs, vfs) = crafted(files);
        let error = read_log(&vfs, dir).unwrap_err();
        assert!(
            matches!(&error, LogError::VouchedBehindEnd { path, valid_len: found }
                if *path == log_path(dir, 3) && *found == valid_len),
            "{error}"
        );
    };

    // The second file has no start record, and the manifest vouches for
    // the start record of the third, or for items in it.
    let header = &second[..HEADER as usize];
    for valid_len in [FIRST, third_ends[0], third.len() as u64] {
        behind_end(
            &[
                (whole, &first),
                (open(2, HEADER), header),
                (open(3, valid_len), &third),
            ],
            valid_len,
        );
    }
    // The second file was written for an end of the first that is gone.
    let cut = &first[..first_ends[1] as usize];
    behind_end(
        &[
            (open(1, first_ends[0]), cut),
            (open(2, HEADER), &second),
            (open(3, FIRST), &third),
        ],
        FIRST,
    );

    // With nothing vouched for behind the end, the log ends there, and the
    // files behind it hold nothing of it.
    let (_fs, vfs) = crafted(&[
        (open(1, first_ends[0]), cut),
        (open(2, HEADER), &second),
        (open(3, HEADER), &third),
    ]);
    let (items, end) = read_log(&vfs, dir).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(end.unread(), [file_id(2), file_id(3)]);
}

#[test]
fn a_record_that_is_there_whole_and_is_no_record_is_not_waited_for() {
    let mut rng = Rng(23);
    let dir = sim::db_dir();
    let (mut bytes, ends, latest) = crafted_file(1, LogStart::FIRST, 5, &mut rng);
    let good = bytes.len() as u64;
    // A frame whose checksum verifies, around a part whose last entry
    // lacks a byte: the format calls it truncated, and no further byte of
    // the file completes it.
    let mut whole = Vec::new();
    let address = LogStart::FIRST.address(good).unwrap();
    let mut group = GroupWriter::new(latest.next().unwrap(), address, &mut whole).unwrap();
    group.freed(page(5)).unwrap();
    group.finish();
    assert_eq!(
        wal::record_len(&whole, FormatVersion::V1).unwrap(),
        whole.len()
    );
    let frame_len = lignin_format::RecordHeader::ENCODED_LEN;
    let mut payload = whole[frame_len..whole.len() - 1].to_vec();
    // The part says how long its entries are, behind its kind, its flags,
    // its version and its number.
    let entries_len = 1 + 1 + 8 + 4;
    let stated = u32::from_le_bytes(payload[entries_len..entries_len + 4].try_into().unwrap());
    payload[entries_len..entries_len + 4].copy_from_slice(&(stated - 1).to_le_bytes());
    lignin_format::header::write_record(&payload, &mut bytes).unwrap();
    let malformed_end = bytes.len();
    bytes.extend(rng.bytes(300_000));

    // How far into the file the reader reads, fetching 64 bytes at a time.
    const WINDOW: usize = 64;
    let reads = |bytes: &[u8], valid_len: u64| {
        let named = log_file(1, FileState::Open, valid_len);
        let (fs, vfs) = crafted_on(sim::traced(1, 512), &[(named, bytes)]);
        let read = read_log_with(&vfs, dir, WINDOW);
        let read_to = fs.trace().into_iter().filter_map(|event| match event {
            Event::Op(op) if op.kind == OpKind::Read && op.path.ends_with(&log_name(1)) => {
                Some(op.offset + op.len)
            }
            _ => None,
        });
        (read, read_to.max().unwrap())
    };
    let (read, read_to) = reads(&bytes, ends[2]);
    let (items, end) = read.unwrap();
    assert_eq!(items.len(), 5);
    assert_eq!(end.place(), Some(place(1, good)));
    // What lies behind the record is not fetched for it: the reader reads
    // as far as it does when little lies behind the record, which is what
    // the items before it made it fetch.
    let little = malformed_end + 3000;
    let (short, short_to) = reads(&bytes[..little], ends[2]);
    assert_eq!(short.unwrap().0.len(), 5);
    assert!(short_to < little as u64, "read to {short_to}");
    assert_eq!(read_to, short_to);

    // Below the valid length it is damage, and is not waited for either.
    let (read, read_to) = reads(&bytes, malformed_end as u64);
    let error = read.unwrap_err();
    assert!(
        matches!(&error, LogError::Damaged { offset, source: FormatError::Truncated { .. }, .. }
            if *offset == good),
        "{error}"
    );
    assert_eq!(read_to, short_to);
}

#[test]
fn the_file_with_the_first_id_begins_the_log_or_holds_nothing_of_it() {
    let mut rng = Rng(24);
    let dir = sim::db_dir();
    let later = LogStart::new(5000, version(7)).unwrap();
    let (bytes, ends, _) = crafted_file(1, later, 3, &mut rng);
    // No file was ever before the one with the first id. With its start
    // record vouched for, the file is not one this log wrote.
    let (_fs, vfs) = crafted(&[(log_file(1, FileState::Open, ends[0]), &bytes)]);
    let error = read_log(&vfs, dir).unwrap_err();
    assert!(
        matches!(&error, LogError::Damaged { offset, source, .. } if *offset == HEADER
            && matches!(source, FormatError::Wal(WalError::NotTheNextFile { .. }))),
        "{error}"
    );
    // Behind the valid length it is a start record that fits nothing, and
    // the file holds nothing of the log.
    let (_fs, vfs) = crafted(&[(log_file(1, FileState::Open, HEADER), &bytes)]);
    let (items, end) = read_log(&vfs, dir).unwrap();
    assert!(items.is_empty());
    assert_eq!(end.unread(), [file_id(1)]);
    assert_eq!(end.next_start().unwrap(), LogStart::FIRST);

    // Any other file may be the first that is read: the files before it
    // were dropped.
    let (bytes, ends, _) = crafted_file(2, later, 3, &mut rng);
    let (_fs, vfs) = crafted(&[(log_file(2, FileState::Open, ends[0]), &bytes)]);
    assert_eq!(read_log(&vfs, dir).unwrap().0.len(), 3);
}

#[test]
fn an_item_that_begins_behind_the_last_address_of_a_file_is_refused_and_not_misread() {
    // A file that begins at the last address a file can begin at. Its
    // first item begins there too, and the item behind it does not.
    let start = LogStart::new(wal::MAX_LOG_OFFSET, version(3)).unwrap();
    let mut bytes = Vec::new();
    wal::write_file_start(file_id(2), &start, &mut bytes).unwrap();
    bytes.extend(mark(version(3)));
    let one = bytes.len();
    bytes.extend(mark(version(1)));

    for window in [1, 16, 4096] {
        // The first item reads, and the file may end behind it.
        let (_fs, vfs) = crafted(&[(log_file(2, FileState::Open, HEADER), &bytes[..one])]);
        let (items, end) = read_log_with(&vfs, sim::db_dir(), window).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(end.place(), Some(place(2, one as u64)));
        // No file continues this one: the log has no address left.
        assert!(end.next_start().is_err());

        // The reader gives an item the addresses of a file that begins
        // with it, and no file begins behind that address: the second
        // item is an error, and is not read with another address.
        let (_fs, vfs) = crafted(&[(log_file(2, FileState::Open, HEADER), &bytes)]);
        let error = read_log_with(&vfs, sim::db_dir(), window).unwrap_err();
        assert!(
            matches!(&error, LogError::Format { offset, .. } if *offset == one as u64),
            "{error}"
        );
    }
}
