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
    assert_eq!(std.db.log.end(), None);
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
    std.db.commit(&mut rng).unwrap();
    std.db.log.sync().unwrap();
    std.db.log.commit(&mut std.db.manifest).unwrap();
    let (_, end) = std.db.read();
    let other = StdDb::new(LogOptions::default());

    // The other database names its file with another length.
    let error = LogStore::open(&other.db.manifest, &end, LogOptions::default()).unwrap_err();
    assert!(
        matches!(error, LogError::WrongLogEnd { file } if file == file_id(1)),
        "{error}"
    );
    // An end that knows nothing of a file the manifest names, and one that
    // knows a file the manifest does not name.
    let error = LogStore::open(&std.db.manifest, &LogEnd::default(), LogOptions::default());
    assert!(matches!(error, Err(LogError::WrongLogEnd { .. })));
    let dir = tempfile::tempdir().unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(FaultyVfs::without_syncs(Arc::new(StdVfs)));
    let empty = ManifestStore::create(vfs, dir.path(), empty_manifest()).unwrap();
    let error = LogStore::open(&empty, &end, LogOptions::default());
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
