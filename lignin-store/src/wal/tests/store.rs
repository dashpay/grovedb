//! The log on the operating system's files: what an append is, where a
//! group goes, turning from file to file, syncing, and the next session.

use lignin_format::wal::{LogStart, LoggedValue};

use super::*;
use crate::table::LogDurable;

#[test]
fn a_commit_is_one_append_to_the_log_and_nothing_else() {
    let std = StdDb::new(LogOptions::default());
    let mut rng = Rng(1);
    let mut written = Vec::new();
    let (mut longest, mut records) = (0, 0);
    for _ in 0..200 {
        let item = std.db.commit(&mut rng).expect("a commit");
        assert_eq!(
            std.faulty.take_calls(),
            [(Op::Append, log_name(1))],
            "one append, and no sync, for a group of {} bytes",
            item.bytes.len()
        );
        assert_eq!(std.db.log.sync_for_commit(&appended(&item)).unwrap(), None);
        assert_eq!(std.faulty.take_calls(), []);
        longest = longest.max(item.bytes.len());
        records += lignin_format::wal::scan(&file_image(&item))
            .unwrap()
            .count();
        written.push(item);
    }
    assert!(longest > 2000, "groups of several records: {longest}");
    assert_eq!(records, 200);

    // A background change and a mark are one append each as well.
    std.db.change(&mut rng).expect("a change");
    assert_eq!(std.faulty.take_calls(), [(Op::Append, log_name(1))]);
    std.db.mark(version(200)).expect("a mark");
    assert_eq!(std.faulty.take_calls(), [(Op::Append, log_name(1))]);

    let (read, end) = std.db.read();
    assert_eq!(read[..200], written[..]);
    assert_eq!(read.len(), 202);
    assert_eq!(end.latest(), version(200));
}

fn appended(item: &Item) -> Appended {
    Appended {
        at: item.at,
        end: item.end,
    }
}

/// A log file that holds `item` as its first item, for a scan.
fn file_image(item: &Item) -> Vec<u8> {
    let after = item.version.map_or(0, |version| version.get() - 1);
    let start = LogStart::new(0, version(after)).unwrap();
    let mut image = Vec::new();
    lignin_format::wal::write_file_start(file_id(1), &start, &mut image).unwrap();
    image.extend_from_slice(&item.bytes);
    image
}

#[test]
fn a_commit_that_syncs_is_one_append_and_one_sync() {
    let std = StdDb::new(LogOptions {
        sync: SyncPolicy::EveryCommit,
        ..LogOptions::default()
    });
    let mut rng = Rng(2);
    for commit in 1..=20 {
        let item = std.db.commit(&mut rng).expect("a commit");
        let durable = std.db.log.sync_for_commit(&appended(&item)).unwrap();
        assert_eq!(
            std.faulty.take_calls(),
            [(Op::Append, log_name(1)), (Op::SyncData, log_name(1))]
        );
        let durable = durable.expect("the policy syncs");
        assert_eq!(durable.version, version(commit));
        assert_eq!(durable.place, Some(item.end));
        assert_eq!(std.db.log.durable(), durable);
    }
}

#[test]
fn the_policy_says_when_a_commit_syncs() {
    use std::time::Duration;

    assert!(!SyncPolicy::Never.is_due(Duration::MAX));
    assert!(SyncPolicy::EveryCommit.is_due(Duration::ZERO));
    let interval = SyncPolicy::Interval(Duration::from_millis(10));
    assert!(!interval.is_due(Duration::from_millis(9)));
    assert!(interval.is_due(Duration::from_millis(10)));
    assert_eq!(LogOptions::default().sync, SyncPolicy::Never);
    assert_eq!(
        LogOptions::default().target_file_len,
        DEFAULT_TARGET_FILE_LEN
    );

    let mut rng = Rng(3);
    for (policy, syncs) in [
        (SyncPolicy::Never, false),
        (SyncPolicy::Interval(Duration::MAX), false),
        (SyncPolicy::Interval(Duration::ZERO), true),
        (SyncPolicy::EveryCommit, true),
    ] {
        let std = StdDb::new(LogOptions {
            sync: policy,
            ..LogOptions::default()
        });
        assert_eq!(std.db.log.options().sync, policy);
        let item = std.db.commit(&mut rng).expect("a commit");
        let synced = std.db.log.sync_for_commit(&appended(&item)).unwrap();
        assert_eq!(synced.is_some(), syncs, "{policy:?}");
        let timer = policy != SyncPolicy::Never && syncs;
        assert_eq!(std.db.log.sync_if_due().unwrap().is_some(), timer);
        let calls = std.faulty.take_calls();
        let synced_file = calls.contains(&(Op::SyncData, log_name(1)));
        assert_eq!(synced_file, syncs, "{policy:?}: {calls:?}");
    }
}

#[test]
fn a_group_is_appended_at_the_place_it_was_written_for_or_not_at_all() {
    let std = StdDb::new(LogOptions::default());
    let log = &std.db.log;
    let mut rng = Rng(4);

    let at = log.next_group().unwrap();
    assert_eq!(at.place(), place(1, FIRST));
    assert_eq!(at.address().get(), FIRST);
    assert_eq!(at.version(), version(1));
    assert_eq!(log.end(), Some(at.place()));

    // A place that is asked for and not used holds nothing back.
    let again = log.next_group().unwrap();
    assert_eq!(again.place(), at.place());
    let first = log.append_group(&again, &empty_group(&again)).unwrap();
    assert_eq!(first.at, at.place());

    // The place is taken now: the group written for it is not appended
    // anywhere else.
    let stale = empty_group(&at);
    let error = log.append_group(&at, &stale).unwrap_err();
    assert!(
        matches!(error, LogError::PlaceTaken { expected, found }
            if expected == at.place() && found == Some(first.end)),
        "{error}"
    );

    // Any other append moves the end as well.
    let at = log.next_group().unwrap();
    let written = group(&at, &mut rng);
    std.db.change(&mut rng).unwrap();
    assert!(matches!(
        log.append_group(&at, &written),
        Err(LogError::PlaceTaken { .. })
    ));
    let at = log.next_group().unwrap();
    std.db.mark(version(1)).unwrap();
    assert!(matches!(
        log.append_group(&at, &group(&at, &mut rng)),
        Err(LogError::PlaceTaken { .. })
    ));
    std.faulty.take_calls();

    // Nothing of a refused group reaches the file.
    let at = log.next_group().unwrap();
    assert_eq!(at.version(), version(2));
    log.append_group(&at, &group(&at, &mut rng)).unwrap();
    assert_eq!(std.faulty.take_calls(), [(Op::Append, log_name(1))]);
    let (read, end) = std.db.read();
    let versions: Vec<_> = read.iter().filter_map(|item| item.version).collect();
    assert_eq!(versions, [version(1), version(2)]);
    assert_eq!(end.place(), log.end());

    // A place of another log means nothing here.
    let other = StdDb::new(LogOptions::default());
    let theirs = other.db.log.next_group().unwrap();
    assert!(matches!(
        log.append_group(&theirs, &empty_group(&theirs)),
        Err(LogError::WrongStore)
    ));
}

#[test]
fn what_recovery_would_not_take_is_not_appended() {
    let std = StdDb::new(LogOptions::default());
    let log = &std.db.log;
    let mut rng = Rng(5);
    std.db.commit(&mut rng).unwrap();
    let before = std.db.read();
    std.faulty.take_calls();

    let at = log.next_group().unwrap();
    let good = group(&at, &mut rng);

    // A group of another version, or for a version that is not the next.
    let mut other = Vec::new();
    GroupWriter::new(version(7), at.address(), &mut other)
        .unwrap()
        .finish();
    let error = log.append_group(&at, &other).unwrap_err();
    assert!(
        matches!(&error, LogError::Refused { offset: 0, source }
        if matches!(source, lignin_format::FormatError::Wal(
            lignin_format::wal::WalError::VersionGap { .. }
        ))),
        "{error}"
    );

    // A group that is cut short, at every length, and one with a flipped
    // bit.
    for len in 0..good.len() {
        let error = log.append_group(&at, &good[..len]).unwrap_err();
        assert!(matches!(error, LogError::Refused { .. }), "{len}: {error}");
    }
    let mut flipped = good.clone();
    flipped[good.len() / 2] ^= 0x10;
    assert!(matches!(
        log.append_group(&at, &flipped),
        Err(LogError::Refused { .. })
    ));

    // More than one item, and an item of another kind.
    let mut two = good.clone();
    two.extend_from_slice(&change(&mut rng));
    let error = log.append_group(&at, &two).unwrap_err();
    assert!(
        matches!(error, LogError::NotOneItem { len, item_len }
            if len == two.len() && item_len == good.len()),
        "{error}"
    );
    let a_change = change(&mut rng);
    assert!(matches!(
        log.append_group(&at, &a_change),
        Err(LogError::WrongItem { .. })
    ));
    assert!(matches!(
        log.append_change(&good),
        Err(LogError::WrongItem { .. })
    ));
    assert!(matches!(
        log.append_change(&mark(version(1))),
        Err(LogError::WrongItem { .. })
    ));
    let mut twice = a_change.clone();
    twice.extend_from_slice(&a_change);
    assert!(matches!(
        log.append_change(&twice),
        Err(LogError::NotOneItem { .. })
    ));
    assert!(matches!(
        log.append_change(&[]),
        Err(LogError::Refused { offset: 0, .. })
    ));

    // A mark for a commit the log does not hold.
    assert!(matches!(
        log.append_mark(version(2)),
        Err(LogError::Refused { .. })
    ));

    assert_eq!(std.faulty.take_calls(), [], "nothing reached the file");
    assert_eq!(std.db.read(), before);

    // The place is still good.
    log.append_group(&at, &good).unwrap();
    assert_eq!(std.db.read().0.len(), 2);
}

#[test]
fn the_log_turns_to_the_next_file_where_this_one_ends() {
    let mut std = StdDb::new(small_files(4000));
    let mut rng = Rng(6);
    let mut written = Vec::new();
    // A new log has the file it ends in and the file it turns to next.
    assert_eq!(
        std.db.named(),
        [
            log_file(1, FileState::Open, HEADER),
            log_file(2, FileState::Open, HEADER)
        ]
    );
    assert!(!std.db.log.is_due());
    assert!(!std.db.log.rotate().unwrap(), "the file is not full");

    // The log goes on in a full file until it is turned.
    while !std.db.log.is_due() {
        written.push(std.db.item(&mut rng).unwrap());
    }
    for _ in 0..3 {
        written.push(std.db.item(&mut rng).unwrap());
    }
    let first_end = std.db.log.end().unwrap();
    let first_latest = std.db.log.latest();
    assert!(first_end.offset() > 4000 && first_end.file() == file_id(1));
    std.faulty.take_calls();

    // The turn is one append, of the start record, and no sync.
    assert!(std.db.log.rotate().unwrap());
    assert_eq!(std.faulty.take_calls(), [(Op::Append, log_name(2))]);
    assert!(!std.db.log.is_due());
    assert_eq!(std.db.log.end(), Some(place(2, FIRST)));
    assert_eq!(std.db.log.latest(), first_latest);
    let at = std.db.log.next_group().unwrap();
    assert_eq!(at.address().get(), first_end.offset() + FIRST);
    while !std.db.log.is_due() {
        written.push(std.db.item(&mut rng).unwrap());
    }

    // It turns only to a file that a durable manifest names.
    assert!(matches!(std.db.log.rotate(), Err(LogError::NoSpare)));
    assert_eq!(std.db.log.create().unwrap(), file_id(3));
    assert!(matches!(
        std.db.log.create(),
        Err(LogError::SpareExists { file }) if file == file_id(3)
    ));
    assert!(matches!(
        std.db.log.rotate(),
        Err(LogError::SpareNotNamed { file }) if file == file_id(3)
    ));
    written.push(std.db.item(&mut rng).unwrap());
    let second_end = std.db.log.end().unwrap();
    let second_latest = std.db.log.latest();
    std.db.log.commit(&mut std.db.manifest).unwrap();
    // Nothing was synced, so the manifest vouches for the headers only.
    assert_eq!(
        std.db.named(),
        [1, 2, 3].map(|file| log_file(file, FileState::Open, HEADER))
    );
    assert!(std.db.log.rotate().unwrap());
    for _ in 0..5 {
        written.push(std.db.item(&mut rng).unwrap());
    }

    let (read, log_end) = std.db.read();
    assert_eq!(read, written);
    let [first, second, third] = log_end.files() else {
        panic!("three files: {log_end:?}");
    };
    assert_eq!(first.start(), LogStart::FIRST);
    assert_eq!(
        (first.file(), first.valid_len()),
        (file_id(1), first_end.offset())
    );
    assert_eq!(
        second.start(),
        LogStart::new(first_end.offset(), first_latest).unwrap()
    );
    assert_eq!(second.valid_len(), second_end.offset());
    assert_eq!(
        third.start(),
        LogStart::new(first_end.offset() + second_end.offset(), second_latest).unwrap()
    );
    assert_eq!(log_end.place(), std.db.log.end());
    assert_eq!(log_end.latest(), std.db.log.latest());
    assert_eq!(log_end.unread(), []);

    // A sync makes the files durable in the order of the log, and the
    // manifest then names closed the ones the log has turned away from.
    std.faulty.take_calls();
    let durable = std.db.log.sync().unwrap();
    assert_eq!(durable.place, std.db.log.end());
    assert_eq!(
        std.faulty.take_calls(),
        [1, 2, 3].map(|file| (Op::SyncData, log_name(file)))
    );
    std.db.log.commit(&mut std.db.manifest).unwrap();
    assert_eq!(
        std.db.named(),
        [
            log_file(1, FileState::Closed, first_end.offset()),
            log_file(2, FileState::Closed, second_end.offset()),
            log_file(3, FileState::Open, std.db.log.end().unwrap().offset())
        ]
    );
    assert_eq!(std.db.log.files_named().unwrap(), std.db.named());
    assert_eq!(std.db.read().0, written);
}

#[test]
fn roll_keeps_a_file_ready_and_turns_when_it_is_due() {
    let mut std = StdDb::new(small_files(3000));
    let mut rng = Rng(7);
    let mut written = Vec::new();
    let mut turns = 0;
    for round in 0..40 {
        written.push(std.db.item(&mut rng).unwrap());
        if round % 3 == 0 {
            let was_due = std.db.log.is_due();
            assert_eq!(std.db.roll().unwrap(), was_due);
            turns += u32::from(was_due);
        }
    }
    assert!(turns >= 3, "{turns}");
    let (read, end) = std.db.read();
    assert_eq!(read, written);
    assert_eq!(end.files().len() as u32, turns + 1);
    // Every file but the last two is closed: the one the log ends in and
    // the one it turns to next are open.
    let named = std.db.named();
    assert_eq!(named.len() as u32, turns + 2);
    let open = |file: &&LiveFile| file.state() == FileState::Open;
    assert_eq!(named.iter().filter(open).count(), 2);
    assert_eq!(end.unread(), [named.last().unwrap().id()]);
}

#[test]
fn a_sync_covers_what_was_appended_before_it_and_says_how_far() {
    let std = StdDb::new(LogOptions::default());
    let log = &std.db.log;
    let mut rng = Rng(8);
    // Nothing of a new log is durable but the headers of its files.
    let start = LogDurable {
        version: CommitVersion::EMPTY,
        place: None,
    };
    assert_eq!(log.durable(), start);

    let first = std.db.commit(&mut rng).unwrap();
    let second = std.db.commit(&mut rng).unwrap();
    assert_eq!(log.durable(), start);
    std.faulty.take_calls();

    let durable = log.sync_through(first.end).unwrap();
    assert_eq!(durable.version, version(2));
    assert_eq!(durable.place, Some(second.end));
    assert_eq!(std.faulty.take_calls(), [(Op::SyncData, log_name(1))]);

    // What is durable already is not synced again.
    assert_eq!(log.sync_through(second.end).unwrap(), durable);
    assert_eq!(log.sync_through(first.at).unwrap(), durable);
    assert_eq!(std.faulty.take_calls(), []);
    assert_eq!(log.sync().unwrap(), durable);
    assert_eq!(std.faulty.take_calls(), []);

    let third = std.db.change(&mut rng).unwrap();
    assert_eq!(log.durable(), durable);
    let durable = log.sync().unwrap();
    assert_eq!(
        (durable.version, durable.place),
        (version(2), Some(third.end))
    );
    assert_eq!(
        std.faulty.take_calls(),
        [(Op::Append, log_name(1)), (Op::SyncData, log_name(1))]
    );
}

#[test]
fn a_pending_value_is_read_from_the_log_by_its_address() {
    let mut std = StdDb::new(small_files(2500));
    let mut rng = Rng(9);
    let mut values = Vec::new();
    let mut carried = 0;
    while values.len() < 40 {
        let at = std.db.log.next_group().unwrap();
        let (bytes, in_group) = group_with_values(&at, &mut rng);
        std.db.log.append_group(&at, &bytes).unwrap();
        values.extend(in_group);
        std.db.roll().unwrap();
    }
    std.faulty.take_calls();

    let mut buf = Vec::new();
    for (pending, value) in &values {
        let read = std.db.log.read_pending(pending, page(9), &mut buf).unwrap();
        let LoggedValue::Bytes(bytes) = read else {
            panic!("the bytes of a value");
        };
        assert_eq!(bytes.to_vec(), *value);
        let calls = std.faulty.take_calls();
        assert_eq!(calls.len(), 1, "one positional read: {calls:?}");
        assert_eq!(calls[0].0, Op::Read);

        // A cell of another page does not own the value.
        let error = std.db.log.read_pending(pending, page(8), &mut buf);
        assert!(matches!(error, Err(LogError::Format { .. })));
        std.faulty.take_calls();
    }

    // A carried value is read as its name.
    let at = std.db.log.next_group().unwrap();
    let mut bytes = Vec::new();
    let mut writer = GroupWriter::new(at.version(), at.address(), &mut bytes).unwrap();
    let name = CarriedValue::new(page(9), page(8), pointer(2, 64, 300));
    let pending = writer.carried(&name).unwrap();
    writer.finish();
    std.db.log.append_group(&at, &bytes).unwrap();
    let read = std
        .db
        .log
        .read_pending(&pending, page(9), &mut buf)
        .unwrap();
    assert_eq!(read.carried(), Some(name));
    carried += 1;
    assert_eq!(carried, 1);

    // An address that no file of the log holds.
    let nowhere = LogAddress::new(u64::from(u32::MAX) << 8)
        .unwrap()
        .pending(5)
        .unwrap();
    assert!(matches!(
        std.db.log.read_pending(&nowhere, page(9), &mut buf),
        Err(LogError::UnknownAddress { .. })
    ));
    let before_first = LogAddress::new(7).unwrap().pending(5).unwrap();
    assert!(matches!(
        std.db.log.read_pending(&before_first, page(9), &mut buf),
        Err(LogError::UnknownAddress { .. })
    ));

    // A value in a file that was dropped is not read.
    std.db.log.sync().unwrap();
    let dropped = std.db.log.drop_before(std.db.log.end().unwrap()).unwrap();
    assert!(!dropped.is_empty());
    let (pending, _) = &values[0];
    assert!(matches!(
        std.db.log.read_pending(pending, page(9), &mut buf),
        Err(LogError::UnknownAddress { .. })
    ));
}

#[test]
fn files_before_the_place_that_is_kept_are_dropped_and_then_deleted() {
    let mut std = StdDb::new(small_files(2000));
    let mut rng = Rng(10);
    let mut written = Vec::new();
    while std.db.log.end().unwrap().file() < file_id(5) {
        written.push(std.db.item(&mut rng).unwrap());
        std.db.roll().unwrap();
    }
    let wal = FileNames::WAL.dir(&std.db.dir);
    let on_disk = || {
        let mut names: Vec<_> = std::fs::read_dir(&wal)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    assert_eq!(on_disk(), (1..=6).map(log_name).collect::<Vec<_>>());

    // A file that is not in the log cannot be kept from.
    assert!(matches!(
        std.db.log.drop_before(place(9, FIRST)),
        Err(LogError::UnknownFile { file }) if file == file_id(9)
    ));
    // Nor the file the log would turn to next, which holds no place.
    assert!(matches!(
        std.db.log.drop_before(place(6, FIRST)),
        Err(LogError::UnknownFile { .. })
    ));

    // Nothing lies before the first file.
    assert_eq!(std.db.log.drop_before(place(1, FIRST)).unwrap(), []);
    let keep = written
        .iter()
        .find(|item| item.at.file() == file_id(3))
        .unwrap();
    assert_eq!(
        std.db.log.drop_before(keep.end).unwrap(),
        [file_id(1), file_id(2)]
    );
    assert_eq!(std.db.log.drop_before(keep.end).unwrap(), []);
    // The files are there until a manifest no longer names them.
    assert_eq!(on_disk().len(), 6);
    std.db.log.commit(&mut std.db.manifest).unwrap();
    assert_eq!(on_disk(), (3..=6).map(log_name).collect::<Vec<_>>());

    let (read, end) = std.db.read();
    let kept: Vec<_> = written
        .iter()
        .filter(|item| item.at.file() >= file_id(3))
        .cloned()
        .collect();
    assert_eq!(read, kept);
    assert_eq!(end.files()[0].file(), file_id(3));

    // A file whose end is not durable stays, and so does every file
    // behind it.
    while std.db.log.end().unwrap().file() < file_id(7) {
        written.push(std.db.item(&mut rng).unwrap());
        if std.db.log.is_due() {
            std.db.log.create().ok();
            std.db.log.commit(&mut std.db.manifest).unwrap();
            std.db.log.rotate().unwrap();
        }
    }
    // The log is kept only from a place that is durable: a crash could
    // take any other away, and the start of its file with it.
    let end = std.db.log.end().unwrap();
    let durable = std.db.log.durable().place.unwrap();
    assert_eq!(durable.file(), file_id(4), "the last roll synced it");
    let error = std.db.log.drop_before(end).unwrap_err();
    assert!(
        matches!(error, LogError::NotDurable { place, durable: found }
            if place == end && found == Some(durable)),
        "{error}"
    );
    assert_eq!(std.db.log.drop_before(durable).unwrap(), [file_id(3)]);
    std.db.log.sync().unwrap();
    assert_eq!(
        std.db.log.drop_before(end).unwrap(),
        [file_id(4), file_id(5), file_id(6)]
    );
}

#[test]
fn the_next_session_goes_on_in_a_file_of_its_own() {
    let mut std = StdDb::new(small_files(3000));
    let mut rng = Rng(11);
    let mut written = Vec::new();
    for _ in 0..12 {
        written.push(std.db.item(&mut rng).unwrap());
        std.db.roll().unwrap();
    }
    for _ in 0..3 {
        written.push(std.db.item(&mut rng).unwrap());
    }
    let synced = std.db.log.sync().unwrap();
    std.db.log.commit(&mut std.db.manifest).unwrap();
    for _ in 0..3 {
        written.push(std.db.item(&mut rng).unwrap());
    }
    let end = std.db.log.end().unwrap();
    let latest = std.db.log.latest();
    let files = std.db.named().len() as u32;
    let last = std.db.named()[files as usize - 2];
    assert_eq!((last.id(), last.state()), (end.file(), FileState::Open));
    assert!(last.valid_len() < end.offset(), "the end was not synced");

    // The session ends: the process is gone and nothing is lost.
    let mut std = std.reopen(small_files(3000));
    assert_eq!(std.db.log.latest(), latest);
    assert_eq!(
        std.db.log.end(),
        Some(end),
        "where the last session's log ends"
    );
    assert!(std.db.log.is_due());
    assert!(matches!(
        std.db.log.next_group(),
        Err(LogError::NoActiveFile)
    ));
    assert!(matches!(
        std.db.change(&mut rng),
        Err(LogError::NoActiveFile)
    ));
    // How far the manifest vouches for the log.
    assert_eq!(std.db.log.durable(), synced);
    assert_eq!(
        synced.place,
        Some(place(end.file().get(), last.valid_len()))
    );

    // The first roll makes durable the file the last session left open,
    // by writing it again into a file that takes its name. It names it
    // closed where the log was found to end in it, drops the file that
    // session had not turned to yet, and turns to a new one.
    assert!(std.db.roll().unwrap());
    let calls = std.faulty.take_calls();
    let old = log_name(end.file().get());
    let copy = format!("{old}.tmp");
    for op in [Op::Open, Op::Append, Op::SyncAll, Op::Rename] {
        assert!(calls.contains(&(op, copy.clone())), "{op:?}: {calls:?}");
    }
    assert!(
        !calls
            .iter()
            .any(|(op, name)| *name == old && *op != Op::Read),
        "a file of an earlier session is only read: {calls:?}"
    );
    assert!(!FileNames::WAL.dir(&std.db.dir).join(&copy).exists());
    let named = std.db.named();
    let closed = named[named.len() - 3];
    assert_eq!(
        closed,
        log_file(end.file().get(), FileState::Closed, end.offset())
    );
    assert_eq!(
        named[named.len() - 2..],
        [files + 1, files + 2].map(|file| log_file(file, FileState::Open, HEADER))
    );
    assert!(!named.iter().any(|file| file.id() == file_id(files)));
    assert!(!FileNames::WAL.path(&std.db.dir, file_id(files)).exists());

    let at = std.db.log.next_group().unwrap();
    assert_eq!(at.place(), place(files + 1, FIRST));
    assert_eq!(at.version(), latest.next().unwrap());
    for _ in 0..4 {
        written.push(std.db.item(&mut rng).unwrap());
    }
    assert_eq!(std.db.read().0, written);

    // And the session after that reads the same log.
    let std = std.reopen(LogOptions::default());
    assert_eq!(std.db.read().0, written);
    assert_eq!(
        std.db.log.latest(),
        written
            .iter()
            .filter_map(|item| item.version)
            .max()
            .unwrap()
    );
}

#[test]
fn an_open_removes_the_log_files_no_manifest_names() {
    let std = StdDb::new(LogOptions::default());
    let mut rng = Rng(12);
    std.db.commit(&mut rng).unwrap();
    let wal = FileNames::WAL.dir(&std.db.dir);
    let header = |id: u32| {
        FileHeader::new(FileKind::Wal, lignin_format::FormatVersion::CURRENT, id)
            .unwrap()
            .encode()
    };
    // A file that was created and never named, and things that are no log
    // files.
    std::fs::write(wal.join(log_name(3)), header(3)).unwrap();
    std::fs::write(wal.join("notes.txt"), b"kept").unwrap();
    std::fs::write(wal.join("3.log"), b"kept").unwrap();
    let mut std = std.reopen(LogOptions::default());
    assert!(!wal.join(log_name(3)).exists());
    assert!(wal.join("notes.txt").exists() && wal.join("3.log").exists());
    assert!(wal.join(log_name(1)).exists() && wal.join(log_name(2)).exists());

    // A file that was dropped and not yet deleted: its id is one the
    // manifest records, and whatever it holds goes.
    std.db.roll().unwrap();
    assert!(
        !wal.join(log_name(2)).exists(),
        "it held nothing of the log"
    );
    let mut dropped = header(2).to_vec();
    dropped.extend_from_slice(b"items of a file that is gone");
    std::fs::write(wal.join(log_name(2)), &dropped).unwrap();
    let std = std.reopen(LogOptions::default());
    assert!(!wal.join(log_name(2)).exists());

    // A file above every id the manifest records that holds more than a
    // header was not written by this database.
    let mut foreign = header(9).to_vec();
    foreign.extend_from_slice(b"items");
    std::fs::write(wal.join(log_name(9)), &foreign).unwrap();
    let Db { vfs, dir, .. } = std.db;
    let error = Db::open(vfs, &dir, LogOptions::default()).err().unwrap();
    assert!(
        matches!(&error, LogError::UnnamedFileHoldsItems { path, len }
            if path.ends_with(log_name(9)) && *len == 25),
        "{error}"
    );
    assert!(wal.join(log_name(9)).exists(), "nothing was removed");
}

#[test]
fn the_end_of_another_log_does_not_open_this_one() {
    let std = StdDb::new(LogOptions::default());
    let mut rng = Rng(13);
    let mut std = std;
    let (_, stale) = std.db.read();
    std.db.commit(&mut rng).unwrap();
    std.db.log.sync().unwrap();
    std.db.log.commit(&mut std.db.manifest).unwrap();
    let (_, end) = std.db.read();
    let other = StdDb::new(LogOptions::default());

    // The end of the log of another directory, whatever it holds.
    let error = LogStore::open(&other.db.manifest, &end, LogOptions::default()).unwrap_err();
    assert!(
        matches!(&error, LogError::WrongDirectory { expected, found }
            if *expected == other.db.dir && *found == std.db.dir),
        "{error}"
    );
    // An end that was read from an earlier manifest: the file has another
    // length in this one.
    let error = LogStore::open(&std.db.manifest, &stale, LogOptions::default()).unwrap_err();
    assert!(
        matches!(error, LogError::WrongLogEnd { file } if file == file_id(1)),
        "{error}"
    );
    // An end that knows nothing of a file the manifest names, and one that
    // knows a file the manifest does not name.
    let nothing = LogReader::new(std.db.vfs.clone(), &std.db.dir, &empty_manifest())
        .into_end()
        .unwrap();
    let error = LogStore::open(&std.db.manifest, &nothing, LogOptions::default());
    assert!(matches!(error, Err(LogError::WrongLogEnd { .. })));
    let dir = tempfile::tempdir().unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(FaultyVfs::without_syncs(Arc::new(StdVfs)));
    let empty = ManifestStore::create(vfs.clone(), dir.path(), empty_manifest()).unwrap();
    let wal = dir.path().join("wal");
    std::fs::create_dir(&wal).unwrap();
    for file in std::fs::read_dir(std.db.dir.join("wal")).unwrap() {
        let file = file.unwrap();
        std::fs::copy(file.path(), wal.join(file.file_name())).unwrap();
    }
    let copied = LogReader::new(vfs, dir.path(), std.db.manifest.manifest().unwrap())
        .into_end()
        .unwrap();
    assert_eq!(copied.files().len(), end.files().len());
    let error = LogStore::open(&empty, &copied, LogOptions::default());
    assert!(matches!(error, Err(LogError::WrongLogEnd { .. })));

    // A manifest of another directory is not this log's.
    let mut theirs = other.db.manifest;
    assert!(matches!(
        std.db.log.commit(&mut theirs),
        Err(LogError::WrongDirectory { .. })
    ));
    assert!(matches!(
        std.db.log.manifest_replaced(&theirs),
        Err(LogError::WrongDirectory { .. })
    ));
    assert!(matches!(
        std.db.log.roll(&mut theirs),
        Err(LogError::WrongDirectory { .. })
    ));
    assert_eq!(std.db.log.dir(), std.db.dir);
}

/// The group of a block of 20 transitions on the reference workload, as
/// `log-format.md` sizes it: about 146 pages with a handful of changed
/// hashes each, 20 values of 400 bytes, and the pointers 16 carried cells
/// stop holding.
fn block_group(at: &GroupPlace, out: &mut Vec<u8>) {
    let mut group = GroupWriter::new(at.version(), at.address(), out).unwrap();
    for value in 0..20u8 {
        group.value(page(9), &[value; 400]).unwrap();
    }
    for raw in 1..=146u32 {
        group
            .tree_run_with(page(raw), None, |run| {
                for slot in 1..=5u16 {
                    run.push(&Delta::Hash {
                        slot: SlotId::new(slot).unwrap(),
                        field: HashField::KeyValue,
                        hash: &[raw as u8; 32],
                    })?;
                }
                Ok(())
            })
            .unwrap();
    }
    for released in 0..16 {
        group
            .released(&pointer(3, 20 + 500 * released, 400))
            .unwrap();
    }
    group.finish();
}

/// What it costs to read a group as recovery will before it is appended,
/// against what it costs to write the group.
///
/// `cargo test --release -p lignin-store --lib -- --ignored --nocapture reading_a_group`
#[test]
#[ignore = "a measurement, for a release build"]
fn reading_a_group_before_it_is_appended_costs_what_writing_its_bytes_costs() {
    use std::time::Instant;

    use super::super::store::{check_item, ItemKind};

    let std = StdDb::new(LogOptions::default());
    let at = std.db.log.next_group().unwrap();
    let mut bytes = Vec::new();
    block_group(&at, &mut bytes);
    let len = bytes.len();
    assert!(check_item(at.cursor(), &bytes, ItemKind::Group).is_ok());

    const ROUNDS: u32 = 2_000;
    let (mut writing, mut reading) = (u128::MAX, u128::MAX);
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..ROUNDS {
            bytes.clear();
            block_group(&at, &mut bytes);
            std::hint::black_box(&bytes);
        }
        writing = writing.min(start.elapsed().as_nanos());
        let start = Instant::now();
        for _ in 0..ROUNDS {
            std::hint::black_box(check_item(at.cursor(), &bytes, ItemKind::Group).is_ok());
        }
        reading = reading.min(start.elapsed().as_nanos());
    }
    let per_group = |total: u128| total as f64 / f64::from(ROUNDS) / 1000.0;
    println!(
        "a group of {len} bytes: {:.1} µs to write, {:.1} µs to read before the append ({:.0}%)",
        per_group(writing),
        per_group(reading),
        100.0 * reading as f64 / writing as f64
    );
    assert!(reading < writing * 3, "{reading} ns against {writing} ns");
}

#[test]
fn the_end_of_the_log_never_goes_back() {
    // A log with no record has no end.
    let dir = tempfile::tempdir().unwrap();
    let faulty = FaultyVfs::without_syncs(Arc::new(StdVfs));
    let vfs: Arc<dyn Vfs> = Arc::new(faulty.clone());
    ManifestStore::create(vfs.clone(), dir.path(), empty_manifest()).unwrap();
    let mut db = Db::open(vfs.clone(), dir.path(), small_files(1500)).unwrap();
    assert_eq!(db.log.end(), None);
    assert_eq!(db.log.latest(), CommitVersion::EMPTY);

    let mut rng = Rng(14);
    let mut ends = vec![db.log.end()];
    let note = |db: &Db, ends: &mut Vec<Option<LogPlace>>| {
        let end = db.log.end();
        assert!(end >= *ends.last().unwrap(), "{end:?} after {ends:?}");
        ends.push(end);
        end
    };
    db.roll().unwrap();
    assert_eq!(note(&db, &mut ends), Some(place(1, FIRST)));
    for round in 0..60 {
        // It is the place behind the last item whose append returned.
        let item = db.item(&mut rng).unwrap();
        assert_eq!(note(&db, &mut ends), Some(item.end));
        // What is refused moves nothing.
        assert!(db.log.append_change(b"no record").is_err());
        assert_eq!(note(&db, &mut ends), Some(item.end));
        match round % 6 {
            // A turn puts it behind the start record of the next file.
            0 if db.log.is_due() => {
                assert!(db.roll().unwrap());
                let file = item.end.file().get() + 1;
                assert_eq!(note(&db, &mut ends), Some(place(file, FIRST)));
            }
            // An append that fails leaves it where it was, and the turn
            // that follows moves it on.
            3 => {
                let file = item.end.file().get();
                faulty.fail(Op::Append, &log_name(file));
                let error = db.change(&mut rng).unwrap_err();
                assert!(matches!(error, LogError::TakenBack { .. }), "{error}");
                assert_eq!(note(&db, &mut ends), Some(item.end));
                assert!(db.roll().unwrap());
                assert_eq!(note(&db, &mut ends), Some(place(file + 1, FIRST)));
            }
            // Syncs and manifests move nothing.
            _ => {
                db.log.sync().unwrap();
                db.log.commit(&mut db.manifest).unwrap();
                assert_eq!(note(&db, &mut ends), Some(item.end));
            }
        }
    }
    // The next session begins where this one ended, before it has a file
    // of its own, and goes on from there.
    let last = db.log.end();
    drop(db);
    let mut db = Db::open(vfs, dir.path(), LogOptions::default()).unwrap();
    assert_eq!(note(&db, &mut ends), last);
    db.roll().unwrap();
    assert!(note(&db, &mut ends) > last);
    let item = db.item(&mut rng).unwrap();
    assert_eq!(note(&db, &mut ends), Some(item.end));
    assert!(ends.len() > 150 && ends.is_sorted());
}

#[test]
fn the_log_is_kept_from_the_place_of_every_hold() {
    let mut std = StdDb::new(small_files(1500));
    let mut rng = Rng(15);
    let mut written = Vec::new();
    while std.db.log.end().unwrap().file() < file_id(6) {
        written.push(std.db.item(&mut rng).unwrap());
        std.db.roll().unwrap();
    }
    std.db.log.sync().unwrap();
    let end = std.db.log.end().unwrap();
    let in_file = |file: u32| {
        let item = written.iter().find(|item| item.at.file() == file_id(file));
        item.unwrap().end
    };
    let last_of = |file: u32| {
        let item = written.iter().rfind(|item| item.at.file() == file_id(file));
        item.unwrap().end
    };

    // The whole log is kept, as while no checkpoint is in force.
    let whole = std.db.log.hold_from(None).unwrap();
    assert_eq!(whole.place(), None);
    assert_eq!(std.db.log.drop_before(end).unwrap(), []);

    // A hold on a place keeps its file and every later one, whatever the
    // caller says it needs: also when the place is the end of its file.
    let third = std.db.log.hold_from(Some(last_of(3))).unwrap();
    assert_eq!(third.place(), Some(last_of(3)));
    assert_eq!(std.db.log.drop_before(end).unwrap(), [], "the whole log");
    drop(whole);
    assert_eq!(
        std.db.log.drop_before(end).unwrap(),
        [file_id(1), file_id(2)]
    );
    assert_eq!(std.db.log.drop_before(end).unwrap(), []);
    // The caller's own place holds as well: the earlier of the two counts.
    let fifth = std.db.log.hold_from(Some(in_file(5))).unwrap();
    drop(third);
    assert_eq!(std.db.log.drop_before(in_file(4)).unwrap(), [file_id(3)]);
    assert_eq!(std.db.log.drop_before(end).unwrap(), [file_id(4)]);
    // Two holds: the earlier one counts, until it is dropped.
    let sixth = std.db.log.hold_from(Some(end)).unwrap();
    assert_eq!(std.db.log.drop_before(end).unwrap(), []);
    drop(fifth);
    assert_eq!(std.db.log.drop_before(end).unwrap(), [file_id(5)]);
    drop(sixth);
    assert_eq!(std.db.log.drop_before(end).unwrap(), []);

    // A hold cannot bring back a file that is gone, or keep one that is no
    // part of the log.
    for gone in [in_file(1), place(7, FIRST), place(40, FIRST)] {
        let error = std.db.log.hold_from(Some(gone)).unwrap_err();
        assert!(
            matches!(error, LogError::UnknownFile { file } if file == gone.file()),
            "{error}"
        );
    }
    std.db.log.commit(&mut std.db.manifest).unwrap();
    let kept: Vec<_> = written
        .iter()
        .filter(|item| item.at.file() == file_id(6))
        .cloned()
        .collect();
    assert_eq!(std.db.read().0, kept);
}

/// A database whose log has no file yet: the session has not rolled.
fn unrolled() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(FaultyVfs::without_syncs(Arc::new(StdVfs)));
    ManifestStore::create(vfs.clone(), dir.path(), empty_manifest()).unwrap();
    let db = Db::open(vfs, dir.path(), LogOptions::default()).unwrap();
    (dir, db)
}

#[test]
fn a_sync_through_a_place_covers_the_place_or_fails() {
    let (_dir, mut db) = unrolled();
    let beyond_end = |db: &Db, beyond: LogPlace| {
        let end = db.log.end();
        let error = db.log.sync_through(beyond).unwrap_err();
        assert!(
            matches!(error, LogError::BeyondEnd { place, end: found }
                if place == beyond && found == end),
            "{error}"
        );
    };
    // A log that holds nothing was appended to nowhere.
    assert_eq!(db.log.end(), None);
    beyond_end(&db, place(1, FIRST));
    db.roll().unwrap();

    let mut rng = Rng(31);
    db.commit(&mut rng).unwrap();
    let end = db.log.end().unwrap();
    // Behind the end of the file the log ends in, and in a file it has
    // not turned to.
    beyond_end(&db, place(1, end.offset() + 1));
    beyond_end(&db, place(2, FIRST));
    assert_eq!(db.log.durable().place, None, "nothing was synced for them");

    // Up to its end, the log is durable through the place when the call
    // returns.
    assert_eq!(db.log.sync_through(end).unwrap().place, Some(end));
}

#[test]
fn a_file_that_the_manifest_stops_naming_is_not_turned_to() {
    let (_dir, mut db) = unrolled();
    let spare = db.log.create().unwrap();
    assert!(matches!(
        db.log.rotate(),
        Err(LogError::SpareNotNamed { file }) if file == spare
    ));
    db.log.commit(&mut db.manifest).unwrap();
    assert_eq!(db.named(), [log_file(1, FileState::Open, HEADER)]);

    // Something replaces the manifest with one that names no log file,
    // and tells the log as it must. A file that no manifest names would
    // not be read after a crash, whatever was appended to it.
    let without_log = |current: &Manifest| -> Result<Manifest, LogError> {
        let others = current.files().iter().copied();
        let others = others.filter(|file| file.kind() != FileKind::Wal);
        Ok(Manifest::new(
            current.page_size(),
            current.last_file_ids(),
            current.table_checkpoint(),
            others.collect(),
        )
        .unwrap())
    };
    db.manifest.update(without_log).unwrap();
    db.log.manifest_replaced(&db.manifest).unwrap();
    assert!(matches!(db.log.rotate(), Err(LogError::NoSpare)));
    assert!(matches!(db.log.next_group(), Err(LogError::NoActiveFile)));
    // No manifest may name the file again, so the log has given it up.
    assert!(!db.dir.join("wal").join(log_name(1)).exists());
    assert_eq!(db.log.files_named().unwrap(), []);

    // The log goes on in a file with an id of its own.
    assert!(db.roll().unwrap());
    assert_eq!(db.log.end(), Some(place(2, FIRST)));
    assert_eq!(db.named()[0].id(), file_id(2));
}

#[test]
fn a_file_length_that_leaves_no_room_for_an_item_is_taken_as_one_that_does() {
    for target_file_len in [0, HEADER, FIRST] {
        let mut std = StdDb::new(small_files(target_file_len));
        assert_eq!(std.db.log.options().target_file_len, FIRST + 1);
        // A file that has its start record is not full, so the roll that
        // turned to it has left the next one ready.
        assert!(!std.db.log.is_due());
        assert!(!std.db.log.rotate().unwrap());
        let mut rng = Rng(33);
        for file in 1..=4 {
            let item = std.db.item(&mut rng).unwrap();
            assert_eq!(item.at, place(file, FIRST));
            assert!(std.db.log.is_due());
            assert!(std.db.roll().unwrap());
        }
        assert_eq!(std.db.read().0.len(), 4);
    }
}

#[test]
fn an_end_that_was_read_before_a_file_grew_does_not_open_the_log() {
    let std = StdDb::new(LogOptions::default());
    let mut rng = Rng(34);
    std.db.commit(&mut rng).unwrap();
    // The end of the log is read, and then something is appended: the
    // manifest names the file as before, and the file is longer. A log
    // that opened with that end would name the file closed where the end
    // was read, and what was appended since would be gone.
    let (_, stale) = std.db.read();
    std.db.commit(&mut rng).unwrap();
    let error = LogStore::open(&std.db.manifest, &stale, LogOptions::default()).unwrap_err();
    assert!(
        matches!(error, LogError::WrongLogEnd { file } if file == file_id(1)),
        "{error}"
    );
    let (_, end) = std.db.read();
    assert_eq!(end.files()[0].file_len(), end.files()[0].valid_len());
    LogStore::open(&std.db.manifest, &end, LogOptions::default()).unwrap();
}

#[test]
fn a_log_that_begins_makes_one_file_durable_with_the_manifest_that_names_it() {
    let mut std = StdDb::new(LogOptions::default());
    let mut rng = Rng(36);
    let first = std.db.commit(&mut rng).unwrap();
    std.db.log.sync().unwrap();
    std.db.log.commit(&mut std.db.manifest).unwrap();

    // The next session: the log is opened where it ends, and begun.
    let mut std = std.reopen(LogOptions::default());
    assert!(std.db.log.begin(&mut std.db.manifest).unwrap());
    let syncs = [Op::SyncData, Op::SyncAll, Op::SyncDir];
    let calls = std.faulty.take_calls();
    let synced: Vec<_> = calls.iter().filter(|(op, _)| syncs.contains(op)).collect();
    let dir = std
        .db
        .dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // The file of the earlier session that was made ready and never
    // turned to holds nothing of the log, so this session's file is the
    // third. It is synced with its name and with the manifest that names
    // it, as one list, and then the manifest gets its name.
    assert_eq!(
        synced,
        [
            &(Op::SyncData, log_name(3)),
            &(Op::SyncDir, "wal".to_owned()),
            &(Op::SyncAll, ManifestStore::TEMP_FILE_NAME.to_owned()),
            &(Op::SyncDir, dir),
        ]
    );
    let renames = calls.iter().filter(|(op, _)| *op == Op::Rename).count();
    assert_eq!(renames, 1, "one manifest");
    assert_eq!(
        std.db.named(),
        [
            log_file(1, FileState::Closed, first.end.offset()),
            log_file(3, FileState::Open, HEADER),
        ]
    );

    // The log takes items, and has no file to turn to until a roll makes
    // one.
    assert_eq!(std.db.log.end(), Some(place(3, FIRST)));
    assert!(!std.db.log.has_file_ready());
    let second = std.db.commit(&mut rng).unwrap();
    assert!(!std.db.log.begin(&mut std.db.manifest).unwrap());
    assert!(!std.db.log.has_file_ready());
    assert!(!std.db.roll().unwrap());
    assert!(std.db.log.has_file_ready());
    assert_eq!(std.db.read().0, [first, second]);
}

#[test]
fn an_early_turn_leaves_a_file_that_holds_a_commit_when_a_file_is_ready() {
    let mut std = StdDb::new(LogOptions::default());
    let mut rng = Rng(37);
    let log = &std.db.log;
    assert!(log.has_file_ready());
    // A file with no commit in it is not left, whatever else it holds.
    assert!(!log.rotate_early().unwrap());
    let mark = std.db.mark(log.latest()).unwrap();
    let change = std.db.change(&mut rng).unwrap();
    assert!(!log.rotate_early().unwrap());
    assert_eq!(log.end(), Some(change.end));

    // With a commit it is, though it is far from full, and nothing is
    // synced for the turn.
    let first = std.db.commit(&mut rng).unwrap();
    assert!(!log.is_due());
    std.faulty.take_calls();
    assert!(log.rotate_early().unwrap());
    let calls = std.faulty.take_calls();
    assert_eq!(calls, [(Op::Append, log_name(2))]);
    assert_eq!(log.end(), Some(place(2, FIRST)));
    assert!(!log.has_file_ready());

    // With no file ready nothing is turned, and that is no error.
    let second = std.db.commit(&mut rng).unwrap();
    assert_eq!(second.at, place(2, FIRST));
    assert!(!log.rotate_early().unwrap());
    assert_eq!(log.end(), Some(second.end));
    assert!(!std.db.roll().unwrap());
    assert!(std.db.log.has_file_ready());
    assert!(std.db.log.rotate_early().unwrap());
    let third = std.db.commit(&mut rng).unwrap();
    assert_eq!(third.at, place(3, FIRST));
    assert_eq!(std.db.read().0, [mark, change, first, second, third]);
}

#[test]
fn an_end_that_was_read_under_another_state_of_a_file_does_not_open_the_log() {
    let mut rng = Rng(35);
    let (bytes, ends, _) = crafted_file(1, LogStart::FIRST, 6, &mut rng);
    let named_len = ends[3];
    let under = |state| {
        let ids = lignin_format::manifest::LastFileIds {
            wal: Some(file_id(1)),
            ..Default::default()
        };
        let named = vec![log_file(1, state, named_len)];
        Manifest::new(PageSize::DEFAULT, ids, None, named).expect("a manifest")
    };
    let read_under = |vfs: &Arc<dyn Vfs>, state| {
        LogReader::new(vfs.clone(), sim::db_dir(), &under(state))
            .into_end()
            .expect("the log reads")
    };
    let refused = |manifest: &ManifestStore, end: &LogEnd| {
        let error = LogStore::open(manifest, end, LogOptions::default()).unwrap_err();
        assert!(
            matches!(error, LogError::WrongLogEnd { file } if file == file_id(1)),
            "{error}"
        );
    };

    // Read as closed, and named open now. The reader stopped at the valid
    // length, and an open file has whole items behind it that are the
    // log's: a log that opened there would name the file closed in front
    // of them.
    let (_fs, vfs) = crafted(&[(log_file(1, FileState::Open, named_len), &bytes)]);
    let manifest = ManifestStore::open(vfs.clone(), sim::db_dir()).unwrap();
    let as_closed = read_under(&vfs, FileState::Closed);
    assert_eq!(as_closed.place(), Some(place(1, named_len)));
    assert_eq!(as_closed.files()[0].named_state(), FileState::Closed);
    refused(&manifest, &as_closed);
    let as_named = read_under(&vfs, FileState::Open);
    assert_eq!(as_named.place(), Some(place(1, bytes.len() as u64)));
    LogStore::open(&manifest, &as_named, LogOptions::default()).unwrap();

    // Read as open, and named closed now, with nothing behind the valid
    // length: the two ends say the same of the log, and the end is still
    // not one that was read by this manifest.
    let whole = &bytes[..named_len as usize];
    let (_fs, vfs) = crafted(&[(log_file(1, FileState::Closed, named_len), whole)]);
    let manifest = ManifestStore::open(vfs.clone(), sim::db_dir()).unwrap();
    let as_open = read_under(&vfs, FileState::Open);
    assert_eq!(as_open.place(), Some(place(1, named_len)));
    refused(&manifest, &as_open);
    let as_named = read_under(&vfs, FileState::Closed);
    assert_eq!(as_named.place(), as_open.place());
    LogStore::open(&manifest, &as_named, LogOptions::default()).unwrap();
}

#[test]
fn a_roll_makes_the_cut_for_an_append_that_fails_while_it_runs() {
    let mut std = StdDb::new(LogOptions::default());
    let mut rng = Rng(51);
    std.db.commit(&mut rng).unwrap();
    let (log, manifest) = (&std.db.log, &mut std.db.manifest);
    let faulty = &std.faulty;
    let mut end = None;
    // The roll is stopped in its first sync, which covers the log as it
    // is now.
    let in_sync = faulty.stop_after(0, Op::SyncData, &log_name(1));
    std::thread::scope(|scope| {
        let rolling = scope.spawn(|| log.roll(manifest));
        in_sync.wait();
        // Meanwhile a group is appended, and the append after it fails.
        let at = log.next_group().unwrap();
        let appended = log.append_group(&at, &group(&at, &mut rng)).unwrap();
        end = Some(appended.end);
        faulty.fail(Op::Append, &log_name(1));
        let at = log.next_group().unwrap();
        let failed = log.append_group(&at, &group(&at, &mut rng));
        assert!(matches!(failed, Err(LogError::TakenBack { .. })));
        in_sync.go_on();
        // The roll finds the file cut short behind what it had synced. It
        // makes the file durable to its end, names it closed and turns.
        assert!(rolling.join().unwrap().unwrap());
    });
    let end = end.unwrap();
    assert_eq!(
        std.db.named()[0],
        log_file(1, FileState::Closed, end.offset())
    );
    assert_eq!(std.db.log.end(), Some(place(2, FIRST)));
    assert!(!std.db.log.is_due() && !std.db.log.waits_for_sync());
    assert_eq!(std.db.named().len(), 3, "a file to turn to is ready");
    assert_eq!(std.db.read().0.len(), 2);
}

#[test]
fn the_log_is_full_where_no_file_can_begin() {
    // A log whose files begin just before the last address a file can
    // begin at: the session's file begins 55 bytes before it.
    let last = lignin_format::wal::MAX_LOG_OFFSET;
    let start = LogStart::new(last - 100, version(3)).unwrap();
    let mut bytes = Vec::new();
    lignin_format::wal::write_file_start(file_id(2), &start, &mut bytes).unwrap();
    let (fs, vfs) = crafted(&[(log_file(2, FileState::Open, FIRST), &bytes)]);
    let mut db = Db::open(vfs, sim::db_dir(), small_files(100)).unwrap();
    db.roll().unwrap();
    assert_eq!(db.log.end(), Some(place(3, FIRST)));

    // Four marks begin at or before that address, and the fifth would
    // begin behind it: no reader could give it an address, so it is not
    // appended.
    let marks: Vec<Item> = (0..4).map(|_| db.mark(version(3)).unwrap()).collect();
    let error = db.log.append_mark(version(3)).unwrap_err();
    assert!(
        matches!(&error, LogError::Format { offset, source, .. }
        if *offset == marks[3].end.offset()
            && matches!(source, lignin_format::FormatError::Wal(
                lignin_format::wal::WalError::AddressOutOfRange
            ))),
        "{error}"
    );
    assert!(db.log.next_group().is_err());
    assert_eq!(db.log.end(), Some(marks[3].end));
    // What was appended reads, to the end of the file.
    let (items, end) = db.read();
    assert_eq!(items, marks);
    assert_eq!(end.place(), Some(marks[3].end));
    // No file can follow: the log has no address left, so the turn that
    // is due is refused.
    assert!(end.next_start().is_err());
    assert!(db.log.is_due());
    assert!(matches!(db.roll(), Err(LogError::Format { .. })));
    drop(fs);
}
