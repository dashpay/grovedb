//! A write-back that fails: each row of the table in the module
//! documentation, "A write-back that fails".

use super::*;

const DURABLE: &[u8] = b"durable!";

fn with_fate(seed: u64, unknown: UnknownFate) -> SimVfs {
    SimVfs::new(SimConfig {
        sector_size: 4,
        unknown,
        ..SimConfig::new(seed)
    })
}

/// A file system in which storage has nothing of what a failed write-back
/// was to write, and in it a file of eight durable bytes, open for writing.
/// Its four operations are numbers 1 to 4.
fn losing(seed: u64) -> (SimVfs, Box<dyn VfsFile>) {
    let fs = with_fate(seed, UnknownFate::Lost);
    let writer = create_durable(&fs, "/file", DURABLE);
    (fs, writer)
}

/// A write-back that fails after the next change of a file, with reads that
/// go on seeing the change.
fn rule() -> WriteBackRule {
    WriteBackRule::new(Fault::Eio).forgotten(false)
}

fn open(fs: &SimVfs, mode: OpenMode) -> Box<dyn VfsFile> {
    fs.open(p("/file"), mode).unwrap()
}

/// The error of a sync that was told of a write-back.
#[track_caller]
fn told(synced: io::Result<()>) -> Fault {
    let error = synced.expect_err("the sync is told of a write-back that failed");
    SimError::injected(&error).unwrap_or_else(|| panic!("the sync was not told: {error}"))
}

fn write_backs(fs: &SimVfs) -> Vec<WriteBackRecord> {
    fs.trace()
        .into_iter()
        .filter_map(|event| match event {
            Event::WriteBack(record) => Some(record),
            _ => None,
        })
        .collect()
}

/// What a crash leaves of the file at `path`, which is the same under every
/// plan.
#[track_caller]
fn after_a_crash(fs: &SimVfs, path: &str) -> Vec<u8> {
    let image = fs.crash_image();
    let left: BTreeSet<Vec<u8>> = every_plan(&image)
        .iter()
        .map(|plan| read(&recover(&image, plan), path))
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    left.into_iter().next().unwrap()
}

#[test]
fn no_write_back_fails_unless_a_rule_or_the_odds_say_so() {
    assert_eq!(SimConfig::new(1).write_back, WriteBackOdds::NONE);
    assert_eq!(WriteBackOdds::default(), WriteBackOdds::NONE);

    let (fs, writer) = losing(1);
    let reader = open(&fs, OpenMode::Read);
    for round in 0..200u64 {
        writer.write_at(&round.to_le_bytes(), 8).unwrap();
        writer.set_len(16 + round % 3).unwrap();
        reader.len().unwrap();
        if round % 7 == 0 {
            reader.sync_data().unwrap();
        }
    }
    assert_eq!(fs.stats().write_backs, 0);
    assert_eq!(fs.stats().faults, 0);
    assert!(fs.trace().is_empty());
    check(&fs);
}

#[test]
fn a_write_back_fails_and_no_call_reports_it() {
    let (fs, writer) = losing(2);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    assert_eq!(fs.stats().write_backs, 1);

    assert_eq!(read_handle(&*writer), b"durable!lost");
    writer.write_at(b"more", 12).unwrap();
    writer.set_len(20).unwrap();
    assert_eq!(writer.len().unwrap(), 20);
    assert_eq!(fs.read_dir(p("/")).unwrap().len(), 1);
    assert_eq!(fs.stats().faults, 0);

    let failed = WriteBackRecord {
        after_op: 5,
        path: "/file".into(),
        fault: Fault::Eio,
        told: Told::OpenHandles,
        handles: 1,
        forgotten: false,
        changes: vec![ChangeFate {
            op: 5,
            path: "/file".into(),
            kind: FileChangeKind::Write { offset: 8, len: 4 },
            fate: Fate::Lost,
        }],
    };
    assert_eq!(fs.trace(), [Event::WriteBack(failed)]);
    check(&fs);
}

#[test]
fn a_rule_fails_the_write_back_after_the_operations_it_names() {
    let fs = with_fate(3, UnknownFate::Lost);
    let a = create_durable(&fs, "/a", b"a");
    let b = create_durable(&fs, "/b", b"b");
    fs.add_write_back(rule().at("/a").skip(1).times(2));

    b.write_at(b"x", 1).unwrap();
    a.write_at(b"1", 1).unwrap();
    a.len().unwrap();
    assert_eq!(fs.stats().write_backs, 0);
    a.write_at(b"2", 2).unwrap();
    assert_eq!(fs.stats().write_backs, 1);
    // Nothing of the file is unsynced now, and this changes nothing, so
    // there is nothing a write-back could fail to write.
    a.set_len(3).unwrap();
    assert_eq!(fs.stats().write_backs, 1);
    a.write_at(b"3", 3).unwrap();
    assert_eq!(fs.stats().write_backs, 2);
    a.write_at(b"4", 4).unwrap();
    assert_eq!(fs.stats().write_backs, 2);
    let lost = |failed: &WriteBackRecord| failed.changes.len();
    assert_eq!(
        write_backs(&fs).iter().map(lost).collect::<Vec<_>>(),
        [2, 1]
    );

    let from = fs.stats().ops + 2;
    fs.add_write_back(rule().on(OpKind::Len).from_op(from));
    b.len().unwrap();
    assert_eq!(fs.stats().write_backs, 2);
    b.len().unwrap();
    let failed = write_backs(&fs);
    assert_eq!(
        (failed.len(), failed[2].after_op, &*failed[2].path),
        (3, from, "/b")
    );

    // The first rule whose turn it is decides, and the operation counts for
    // the others.
    fs.add_write_back(rule().told(Told::Writers));
    fs.add_write_back(rule().skip(1));
    a.write_at(b"5", 5).unwrap();
    a.write_at(b"6", 6).unwrap();
    let who: Vec<Told> = write_backs(&fs)[3..]
        .iter()
        .map(|failed| failed.told)
        .collect();
    assert_eq!(who, [Told::Writers, Told::OpenHandles]);

    fs.add_write_back(rule().forever());
    a.write_at(b"7", 7).unwrap();
    a.write_at(b"8", 8).unwrap();
    assert_eq!(fs.stats().write_backs, 7);
    fs.clear_rules();
    a.write_at(b"9", 9).unwrap();
    assert_eq!(fs.stats().write_backs, 7);
    check(&fs);
}

#[test]
fn what_storage_has_is_settled_when_the_write_back_fails() {
    let fates: [(UnknownFate, &[u8]); 2] = [
        (UnknownFate::Lost, b"0123456789"),
        (UnknownFate::Kept, b"ab23cd6789"),
    ];
    for (unknown, left) in fates {
        let fs = with_fate(4, unknown);
        let file = create_durable(&fs, "/file", b"0123456789");
        file.write_at(b"ab", 0).unwrap();
        fs.add_write_back(rule());
        file.write_at(b"cd", 4).unwrap();

        assert!(fs.crash_image().changes().is_empty());
        assert_eq!(after_a_crash(&fs, "/file"), left);
        assert_eq!(read_handle(&*file), b"ab23cd6789");
        let lost = |change: &ChangeFate| change.fate == Fate::Lost;
        let all_lost = unknown == UnknownFate::Lost;
        let each: Vec<bool> = write_backs(&fs)[0].changes.iter().map(lost).collect();
        assert_eq!(each, [all_lost, all_lost]);

        // A change that is made after the failure is unsynced as any other.
        file.write_at(b"ef", 8).unwrap();
        let image = fs.crash_image();
        assert_eq!(image.changes().len(), 1);
        let with_it = [&left[..8], b"ef"].concat();
        assert_eq!(read(&recover(&image, &image.keep_all()), "/file"), with_it);
        assert_eq!(read(&recover(&image, &image.lose_all()), "/file"), left);
        // The handle is told whatever storage has.
        assert_eq!(told(file.sync_data()), Fault::Eio);
        check(&fs);
    }
}

#[test]
fn what_storage_has_by_a_draw_is_what_a_crash_could_have_left() {
    let mut left = BTreeSet::new();
    for draw in 0..if cfg!(miri) { 6 } else { 64 } {
        let fs = with_fate(5, UnknownFate::Drawn(draw));
        let file = create_durable(&fs, "/file", b"0123456789");
        file.write_at(b"abcdefghijkl", 2).unwrap();
        file.set_len(7).unwrap();
        file.write_at(b"XY", 6).unwrap();
        let before = fs.crash_image();
        fs.add_write_back(rule().on(OpKind::Len));
        file.len().unwrap();

        let failed = &write_backs(&fs)[0];
        let mut survivors: Vec<(u64, Survivor)> = Vec::new();
        for (change, fate) in failed.changes.iter().enumerate() {
            if let Fate::Survives { position, tear } = &fate.fate {
                let tear = tear.clone();
                survivors.push((*position, Survivor { change, tear }));
            }
        }
        survivors.sort_by_key(|(position, _)| *position);
        let plan = CrashPlan {
            names: Vec::new(),
            changes: survivors.into_iter().map(|(_, kept)| kept).collect(),
        };
        let as_a_crash = read(&recover(&before, &plan), "/file");
        assert_eq!(after_a_crash(&fs, "/file"), as_a_crash, "draw {draw}");
        left.insert(as_a_crash);
    }
    let more_than = if cfg!(miri) { 1 } else { 3 };
    assert!(left.len() > more_than, "{} states", left.len());
}

#[test]
fn changes_that_are_forgotten_cease_to_be_visible_to_reads() {
    for forgotten in [true, false] {
        let fs = with_fate(6, UnknownFate::Lost);
        let writer = create_durable(&fs, "/file", b"0123456789");
        let reader = open(&fs, OpenMode::Read);
        writer.write_at(b"ab", 0).unwrap();
        writer.write_at(b"tail", 10).unwrap();
        assert_eq!(read_handle(&*reader), b"ab23456789tail");
        fs.add_write_back(rule().on(OpKind::Len).forgotten(forgotten));
        assert_eq!(writer.len().unwrap(), 14);

        let seen: &[u8] = if forgotten {
            b"0123456789\0\0\0\0"
        } else {
            b"ab23456789tail"
        };
        assert_eq!(read_handle(&*reader), seen);
        assert_eq!(read_handle(&*writer), seen);
        assert_eq!(read(&fs, "/file"), seen);
        assert_eq!(write_backs(&fs)[0].forgotten, forgotten);
        // Storage has the same either way.
        assert_eq!(after_a_crash(&fs, "/file"), b"0123456789");
        check(&fs);
    }
}

#[test]
fn a_length_that_was_cut_is_not_forgotten_either() {
    let fs = with_fate(7, UnknownFate::Lost);
    let writer = create_durable(&fs, "/file", b"0123456789");
    writer.set_len(4).unwrap();
    fs.add_write_back(rule().on(OpKind::Len).forgotten(true));
    assert_eq!(writer.len().unwrap(), 4);

    assert_eq!(read_handle(&*writer), b"0123");
    assert_eq!(after_a_crash(&fs, "/file"), b"0123456789");
    check(&fs);
}

#[test]
fn an_append_lands_behind_every_earlier_one_also_when_they_are_forgotten() {
    let fs = with_fate(8, UnknownFate::Lost);
    let log = fs.open(p("/log"), OpenMode::Append(Create::New)).unwrap();
    fs.sync_dir(p("/")).unwrap();
    let reader = fs.open(p("/log"), OpenMode::Read).unwrap();
    assert_eq!(log.append(b"first").unwrap(), 0);
    fs.add_write_back(rule().told(Told::Writers).forgotten(true));
    assert_eq!(log.append(b"second").unwrap(), 5);

    assert_eq!(read_handle(&*reader), [0; 11]);
    assert_eq!(log.len().unwrap(), 11);
    assert_eq!(log.append(b"third").unwrap(), 11);
    assert_eq!(read_handle(&*reader), b"\0\0\0\0\0\0\0\0\0\0\0third");
    // What a sync that is not told makes durable is what reads see.
    reader.sync_data().unwrap();
    assert_eq!(after_a_crash(&fs, "/log"), read_handle(&*reader));
    check(&fs);
}

#[test]
fn every_handle_that_was_open_is_told_once_by_its_next_sync() {
    let (fs, writer) = losing(9);
    let reader = open(&fs, OpenMode::Read);
    let second = open(&fs, OpenMode::Write(Create::Never));
    fs.add_write_back(WriteBackRule::new(Fault::Enospc));
    writer.write_at(b"lost", 8).unwrap();
    assert_eq!(write_backs(&fs)[0].handles, 3);

    for (handle, all) in [(&reader, false), (&second, true), (&writer, false)] {
        let synced = if all {
            handle.sync_all()
        } else {
            handle.sync_data()
        };
        let error = synced.unwrap_err();
        assert_eq!(SimError::injected(&error), Some(Fault::Enospc));
        assert_eq!(error.kind(), ErrorKind::StorageFull);
        // Once: from then on the handle says that its own sync failed.
        let again = handle.sync_data().unwrap_err();
        assert_eq!(
            (again.kind(), SimError::of(&again)),
            (ErrorKind::Other, None)
        );
    }
    assert_eq!(fs.stats().faults, 3);
    let reported = |event: Event| match event {
        Event::Reported { op, path, after_op } => Some((op, path, after_op)),
        _ => None,
    };
    let reported: Vec<_> = fs.trace().into_iter().filter_map(reported).collect();
    let file = String::from("/file");
    assert_eq!(
        reported,
        [(8, file.clone(), 7), (10, file.clone(), 7), (12, file, 7)]
    );
    check(&fs);
}

#[test]
fn a_handle_opened_afterwards_is_told_if_no_handle_was_told_before() {
    let (fs, writer) = losing(10);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();

    let later = open(&fs, OpenMode::Read);
    let later_still = open(&fs, OpenMode::Read);
    assert_eq!(told(later.sync_data()), Fault::Eio);
    // This one was opened before any handle was told.
    assert_eq!(told(later_still.sync_data()), Fault::Eio);
    assert_eq!(told(writer.sync_data()), Fault::Eio);
    // And this one after.
    open(&fs, OpenMode::Read).sync_data().unwrap();
    check(&fs);
}

#[test]
fn a_handle_opened_after_another_was_told_syncs_with_ok_for_bytes_that_are_gone() {
    let (fs, writer) = losing(11);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    assert_eq!(told(writer.sync_data()), Fault::Eio);

    let later = open(&fs, OpenMode::Write(Create::Never));
    later.write_at(b"kept", 12).unwrap();
    later.sync_data().unwrap();
    later.sync_all().unwrap();
    assert_eq!(read_handle(&*later), b"durable!lostkept");
    assert_eq!(after_a_crash(&fs, "/file"), b"durable!\0\0\0\0kept");
    check(&fs);
}

#[test]
fn the_record_is_gone_with_the_last_handle_of_the_file() {
    // The contract's sequence: the write-back fails, the last handle is
    // dropped, and the file is opened again and synced.
    let (fs, writer) = losing(12);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    drop(writer);
    let reopened = open(&fs, OpenMode::Write(Create::Never));
    reopened.sync_all().unwrap();
    assert_eq!(read_handle(&*reopened), b"durable!lost");
    assert_eq!(after_a_crash(&fs, "/file"), DURABLE);
    assert_eq!(fs.stats().faults, 0);
    check(&fs);

    // A handle that stays open keeps the record.
    let (fs, writer) = losing(12);
    let stays_open = open(&fs, OpenMode::Read);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    drop(writer);
    let reopened = open(&fs, OpenMode::Write(Create::Never));
    assert_eq!(told(reopened.sync_all()), Fault::Eio);
    drop(stays_open);
    check(&fs);

    // A guard on the file is no handle of it.
    let (fs, writer) = losing(12);
    let _guard = fs.lock(p("/file")).unwrap();
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    drop(writer);
    open(&fs, OpenMode::Write(Create::Never))
        .sync_all()
        .unwrap();
    check(&fs);
}

#[test]
fn dropping_every_handle_loses_nothing_that_the_write_back_had_not_lost() {
    let (fs, writer) = losing(13);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    writer.write_at(b"kept", 12).unwrap();
    drop(writer);

    assert_eq!(read(&fs, "/file"), b"durable!lostkept");
    let image = fs.crash_image();
    let kept = read(&recover(&image, &image.keep_all()), "/file");
    assert_eq!(kept, b"durable!\0\0\0\0kept");
}

#[test]
fn a_sync_that_is_told_is_a_sync_that_failed() {
    let (fs, writer) = losing(14);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    writer.write_at(b"since", 12).unwrap();
    assert_eq!(told(writer.sync_data()), Fault::Eio);

    // The change made since the failure is left unknown.
    let unknown = |change: &FileChange| (change.op, change.unknown);
    let image = fs.crash_image();
    assert_eq!(
        image.changes().iter().map(unknown).collect::<Vec<_>>(),
        [(6, true)]
    );
    let failed = (OpKind::SyncData, Fault::Eio, Effect::SyncFailed(1));
    assert_eq!(injected(&fs), [failed]);
    // The handle syncs no more.
    assert_eq!(kind(writer.sync_all()), ErrorKind::Other);
    assert_eq!(fs.stats().faults, 1);

    // A later sync through another handle covers what came after it, and
    // settles the unknown change as the configuration says.
    let other = open(&fs, OpenMode::Write(Create::Never));
    other.write_at(b"!", 17).unwrap();
    other.sync_data().unwrap();
    assert_eq!(after_a_crash(&fs, "/file"), b"durable!\0\0\0\0\0\0\0\0\0!");
    assert!(matches!(fs.trace().last(), Some(Event::Settled { .. })));
    check(&fs);
}

#[test]
fn a_sync_that_is_told_is_not_one_a_fault_rule_looks_at() {
    let (fs, writer) = losing(15);
    fs.add_write_back(WriteBackRule::new(Fault::Enospc).told(Told::Writers));
    writer.write_at(b"lost", 8).unwrap();
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));

    assert_eq!(told(writer.sync_data()), Fault::Enospc);
    // The rule fails the next sync that would have succeeded.
    let other = open(&fs, OpenMode::Read);
    let by_the_rule = other.sync_data().unwrap_err();
    assert_eq!(SimError::injected(&by_the_rule), Some(Fault::Eio));
    assert_eq!(fs.stats().faults, 2);
    check(&fs);
}

#[test]
fn with_the_writers_told_no_other_handle_is() {
    let (fs, writer) = losing(16);
    let idle_writer = open(&fs, OpenMode::Write(Create::Never));
    let reader = open(&fs, OpenMode::Read);
    fs.add_write_back(rule().told(Told::Writers));
    writer.write_at(b"lost", 8).unwrap();
    assert_eq!(write_backs(&fs)[0].handles, 1);

    reader.sync_data().unwrap();
    idle_writer.sync_all().unwrap();
    open(&fs, OpenMode::Read).sync_data().unwrap();
    assert_eq!(told(writer.sync_data()), Fault::Eio);
    assert_eq!(after_a_crash(&fs, "/file"), DURABLE);
    check(&fs);

    // Each handle that made one of the changes is told.
    let (fs, first) = losing(16);
    let second = open(&fs, OpenMode::Write(Create::Never));
    first.write_at(b"one", 8).unwrap();
    fs.add_write_back(rule().told(Told::Writers));
    second.write_at(b"two", 11).unwrap();
    assert_eq!(write_backs(&fs)[0].handles, 2);
    assert_eq!(told(second.sync_data()), Fault::Eio);
    assert_eq!(told(first.sync_data()), Fault::Eio);
    check(&fs);

    // A handle that wrote and was closed leaves nobody to tell.
    let (fs, writer) = losing(16);
    let reader = open(&fs, OpenMode::Read);
    writer.write_at(b"lost", 8).unwrap();
    drop(writer);
    fs.add_write_back(rule().told(Told::Writers).on(OpKind::Len));
    reader.len().unwrap();
    assert_eq!(write_backs(&fs)[0].handles, 0);
    reader.sync_data().unwrap();
    assert_eq!(after_a_crash(&fs, "/file"), DURABLE);
    check(&fs);
}

#[test]
fn a_write_back_that_fails_changes_what_a_crash_can_leave() {
    let (fs, writer) = losing(17);
    writer.write_at(b"lost", 8).unwrap();
    let changes = fs.stats().changes;
    fs.add_write_back(rule().on(OpKind::Len));
    fs.crash_at(CrashPoint::AfterChange(changes + 1));
    // The operation changes nothing. The write-back that fails after it
    // does, and the power goes off after both.
    writer.len().unwrap();
    assert_eq!(fs.stats().changes, changes + 1);
    assert!(fs.is_powered_off());
    let trace = fs.trace();
    assert!(matches!(
        &trace[..],
        [Event::WriteBack(_), Event::PowerOff { after_op: 6 }]
    ));
    assert_eq!(after_a_crash(&fs, "/file"), DURABLE);

    // An operation that made a change, and whose write-back then failed,
    // is counted once.
    let (fs, writer) = losing(17);
    let changes = fs.stats().changes;
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    assert_eq!(fs.stats().changes, changes + 1);
}

#[test]
fn a_crash_takes_the_rules_and_the_record_with_it() {
    let (fs, writer) = losing(18);
    fs.add_write_back(rule().forever());
    writer.write_at(b"lost", 8).unwrap();

    let after = fs.crash(0);
    let file = after
        .open(p("/file"), OpenMode::Write(Create::Never))
        .unwrap();
    file.write_at(b"new", 0).unwrap();
    file.sync_data().unwrap();
    assert_eq!(after.stats().write_backs, 0);
    assert_eq!(after_a_crash(&after, "/file"), b"newable!");
}

#[test]
fn a_write_back_after_a_failed_sync_settles_the_unknown_changes_too() {
    let (fs, writer) = losing(30);
    writer.write_at(b"one", 8).unwrap();
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
    writer.sync_data().unwrap_err();
    fs.clear_rules();
    let other = open(&fs, OpenMode::Write(Create::Never));
    fs.add_write_back(rule().told(Told::Writers));
    other.write_at(b"two", 11).unwrap();
    assert!(fs.crash_image().changes().is_empty());
    // Both handles wrote. The one that syncs no more is not to be told.
    assert_eq!(write_backs(&fs)[0].handles, 1);

    // Nothing is unknown any more: a sync that is not told covers what
    // came after the failure.
    let late = open(&fs, OpenMode::Write(Create::Never));
    late.write_at(b"new", 14).unwrap();
    late.sync_data().unwrap();
    assert_eq!(after_a_crash(&fs, "/file"), b"durable!\0\0\0\0\0\0new");

    let stuck = writer.sync_data().unwrap_err();
    assert_eq!(
        (stuck.kind(), SimError::of(&stuck)),
        (ErrorKind::Other, None)
    );
    assert_eq!(told(other.sync_data()), Fault::Eio);
    let reported = |event: &&Event| matches!(event, Event::Reported { .. });
    assert_eq!(fs.trace().iter().filter(reported).count(), 1);
    check(&fs);
}

#[test]
fn with_the_writers_told_a_writer_whose_changes_were_synced_is_not() {
    let (fs, first) = losing(31);
    let second = open(&fs, OpenMode::Write(Create::Never));
    first.write_at(b"one", 8).unwrap();
    first.sync_data().unwrap();
    fs.add_write_back(rule().told(Told::Writers));
    second.write_at(b"two", 11).unwrap();

    assert_eq!(write_backs(&fs)[0].handles, 1);
    first.sync_data().unwrap();
    assert_eq!(told(second.sync_data()), Fault::Eio);
    assert_eq!(after_a_crash(&fs, "/file"), b"durable!one");
    check(&fs);
}

#[test]
fn a_write_back_fails_for_one_file_and_no_other() {
    let fs = with_fate(32, UnknownFate::Lost);
    let a = create_durable(&fs, "/a", b"aaaa");
    let b = create_durable(&fs, "/b", b"bbbb");
    b.write_at(b"B", 0).unwrap();
    fs.add_write_back(rule().at("/a").forgotten(true));
    a.write_at(b"A", 0).unwrap();

    assert_eq!(read_handle(&*a), b"aaaa");
    assert_eq!(read_handle(&*b), b"Bbbb");
    let image = fs.crash_image();
    let unsynced: Vec<&str> = image.changes().iter().map(|change| &*change.path).collect();
    assert_eq!(unsynced, ["/b"]);
    b.sync_data().unwrap();
    assert_eq!(told(a.sync_data()), Fault::Eio);
    check(&fs);
}

/// A sync that is told of a later write-back leaves the record of an
/// earlier one that nobody was told of.
#[test]
fn a_handle_opened_later_is_told_of_the_write_back_nobody_was_told_of() {
    let (fs, writer) = losing(33);
    let reader = open(&fs, OpenMode::Read);
    fs.add_write_back(rule());
    writer.write_at(b"one", 8).unwrap();
    fs.add_write_back(WriteBackRule::new(Fault::Enospc).told(Told::Writers));
    writer.write_at(b"two", 11).unwrap();

    // The writer is told of the second, and nobody yet of the first.
    assert_eq!(told(writer.sync_data()), Fault::Enospc);
    let later = open(&fs, OpenMode::Read);
    assert_eq!(told(later.sync_data()), Fault::Eio);
    // Now one was, and the reader, which was open at the first, is told as
    // every handle is that was open then.
    open(&fs, OpenMode::Read).sync_data().unwrap();
    assert_eq!(told(reader.sync_data()), Fault::Eio);
    check(&fs);
}

#[test]
fn forgetting_takes_from_reads_what_an_earlier_failure_had_left_them() {
    let fs = with_fate(34, UnknownFate::Lost);
    let file = create_durable(&fs, "/file", b"00");
    fs.add_write_back(rule());
    file.write_at(b"X", 0).unwrap();
    assert_eq!(read_handle(&*file), b"X0");
    // The second write-back fails over the write at 1 alone, and reads
    // lose the byte at 0 with it.
    fs.add_write_back(rule().forgotten(true));
    file.write_at(b"Y", 1).unwrap();
    assert_eq!(read_handle(&*file), b"00");
    assert_eq!(after_a_crash(&fs, "/file"), b"00");
    check(&fs);

    // So with a change that a sync settled as lost.
    let fs = with_fate(34, UnknownFate::Lost);
    let file = create_durable(&fs, "/file", b"00");
    file.write_at(b"X", 0).unwrap();
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
    file.sync_data().unwrap_err();
    let other = open(&fs, OpenMode::Write(Create::Never));
    other.sync_data().unwrap();
    assert_eq!(read_handle(&*other), b"X0");
    fs.add_write_back(rule().forgotten(true));
    other.write_at(b"Y", 1).unwrap();
    assert_eq!(read_handle(&*other), b"00");
    check(&fs);
}

#[test]
fn a_forgotten_append_leaves_the_file_its_length_and_storage_none() {
    let fs = with_fate(35, UnknownFate::Lost);
    let log = fs.open(p("/log"), OpenMode::Append(Create::New)).unwrap();
    fs.sync_dir(p("/")).unwrap();
    fs.add_write_back(rule().forgotten(true));
    log.append(b"x").unwrap();

    let mut byte = [9];
    assert_eq!(log.read_at(&mut byte, 0).unwrap(), 1);
    assert_eq!(byte, [0]);
    assert_eq!(log.len().unwrap(), 1);
    assert_eq!(after_a_crash(&fs, "/log"), b"");
    check(&fs);
}

#[test]
fn a_length_set_after_a_cut_was_lost_is_durable_over_what_storage_has() {
    let fs = with_fate(36, UnknownFate::Lost);
    let writer = create_durable(&fs, "/file", b"abcd");
    fs.add_write_back(rule().told(Told::Writers).forgotten(true));
    writer.set_len(1).unwrap();
    assert_eq!(read_handle(&*writer), b"a");

    let other = open(&fs, OpenMode::Write(Create::Never));
    other.set_len(4).unwrap();
    other.sync_data().unwrap();
    assert_eq!(read_handle(&*other), b"a\0\0\0");
    assert_eq!(after_a_crash(&fs, "/file"), b"abcd");
    check(&fs);
}

#[test]
fn a_rule_looks_at_an_open_and_at_an_operation_that_is_refused() {
    let (fs, writer) = losing(37);
    writer.write_at(b"lost", 8).unwrap();
    fs.add_write_back(rule().on(OpKind::Open));
    // An open that fails is no operation on an open file.
    let refused = fs.open(p("/file"), OpenMode::Append(Create::Never));
    assert_eq!(kind(refused), ErrorKind::ResourceBusy);
    assert_eq!(fs.stats().write_backs, 0);
    // The handle an open gives is open when the write-back fails after it.
    let opened = open(&fs, OpenMode::Read);
    let failed = &write_backs(&fs)[0];
    assert_eq!((failed.after_op, failed.handles), (fs.stats().ops, 2));
    assert_eq!(told(opened.sync_data()), Fault::Eio);
    check(&fs);

    let (fs, writer) = losing(37);
    let reader = open(&fs, OpenMode::Read);
    writer.write_at(b"lost", 8).unwrap();
    fs.add_write_back(rule());
    assert_eq!(
        kind(reader.write_at(b"refused", 0)),
        ErrorKind::InvalidInput
    );
    assert_eq!(fs.stats().write_backs, 1);
    check(&fs);
}

#[test]
fn of_two_rules_whose_turn_it_is_the_first_decides() {
    let (fs, writer) = losing(38);
    let first = WriteBackRule::new(Fault::Enospc)
        .told(Told::Writers)
        .forgotten(false);
    fs.add_write_back(first);
    fs.add_write_back(WriteBackRule::new(Fault::Eio).forgotten(true));
    writer.write_at(b"lost", 8).unwrap();

    let failed = write_backs(&fs);
    let decided = (failed[0].fault, failed[0].told, failed[0].forgotten);
    assert_eq!(failed.len(), 1);
    assert_eq!(decided, (Fault::Enospc, Told::Writers, false));
    // The operation counted for the second rule, which is spent as well.
    writer.write_at(b"more", 12).unwrap();
    assert_eq!(fs.stats().write_backs, 1);
    check(&fs);
}

#[test]
fn a_handle_is_told_of_the_last_write_back_that_marked_it() {
    let (fs, writer) = losing(39);
    let reader = open(&fs, OpenMode::Read);
    fs.add_write_back(WriteBackRule::new(Fault::Eio).forgotten(false));
    writer.write_at(b"one", 8).unwrap();
    fs.add_write_back(WriteBackRule::new(Fault::Enospc).forgotten(false));
    writer.write_at(b"two", 11).unwrap();
    let marked: Vec<u64> = write_backs(&fs)
        .iter()
        .map(|failed| failed.handles)
        .collect();
    assert_eq!(marked, [2, 2]);

    // Each handle is told once, of the second. Nobody is told of the first.
    assert_eq!(told(writer.sync_data()), Fault::Enospc);
    assert_eq!(told(reader.sync_all()), Fault::Enospc);
    open(&fs, OpenMode::Read).sync_data().unwrap();
    let reported = |event: Event| match event {
        Event::Reported { after_op, .. } => Some(after_op),
        _ => None,
    };
    let of: Vec<u64> = fs.trace().into_iter().filter_map(reported).collect();
    assert_eq!(of, [7, 7]);
    check(&fs);
}

#[test]
fn a_sync_all_that_is_told_leaves_every_unsynced_change_unknown() {
    let (fs, writer) = losing(40);
    fs.add_write_back(rule());
    writer.write_at(b"lost", 8).unwrap();
    writer.write_at(b"one", 12).unwrap();
    writer.set_len(20).unwrap();
    writer.write_at(b"two", 15).unwrap();

    assert_eq!(told(writer.sync_all()), Fault::Eio);
    let failed = (OpKind::SyncAll, Fault::Eio, Effect::SyncFailed(3));
    assert_eq!(injected(&fs), [failed]);
    let image = fs.crash_image();
    let unknown = image.changes().iter().filter(|change| change.unknown);
    assert_eq!(unknown.count(), 3);
    check(&fs);
}

#[test]
fn a_rule_and_a_report_go_by_the_path_a_handle_was_opened_by() {
    let (fs, writer) = losing(41);
    fs.hard_link(p("/file"), p("/alias")).unwrap();
    let alias = fs.open(p("/alias"), OpenMode::Read).unwrap();
    fs.add_write_back(rule().at("/alias").on(OpKind::Len));
    writer.write_at(b"lost", 8).unwrap();
    writer.len().unwrap();
    assert_eq!(fs.stats().write_backs, 0);
    alias.len().unwrap();

    // The write-back that fails is the file's, whichever name it has.
    let failed = &write_backs(&fs)[0];
    assert_eq!((&*failed.path, failed.handles), ("/alias", 2));
    assert_eq!(failed.changes[0].path, "/file");
    assert_eq!(told(writer.sync_data()), Fault::Eio);
    assert_eq!(told(alias.sync_data()), Fault::Eio);
    let reported = |event: Event| match event {
        Event::Reported { path, .. } => Some(path),
        _ => None,
    };
    let by: Vec<String> = fs.trace().into_iter().filter_map(reported).collect();
    assert_eq!(by, ["/file", "/alias"]);
    check(&fs);
}

/// The error sequence Linux keeps for a file (`lib/errseq.c`): a counter
/// that a write-back error moves on if a handle has seen the value it has,
/// and the mark of whether one has. A handle takes a sample when it is
/// opened, and a sync compares the handle's sample with the sequence.
#[derive(Default)]
struct ErrSeq {
    value: u64,
    seen: bool,
}

impl ErrSeq {
    fn set(&mut self) {
        if self.seen || self.value == 0 {
            self.value += 1;
        }
        self.seen = false;
    }

    fn sample(&self) -> u64 {
        if self.seen {
            self.value
        } else {
            0
        }
    }

    /// Whether a sync through a handle with the sample `since` is told.
    fn check_and_advance(&mut self, since: &mut u64) -> bool {
        let told = *since != self.value;
        if told {
            self.seen = true;
            *since = self.value;
        }
        told
    }
}

/// A handle as the test knows it: its sample of the error sequence, whether
/// a sync has failed on it, how many write-backs had failed when it was
/// opened, and what the sequence was like then.
struct Opened {
    file: Box<dyn VfsFile>,
    since: u64,
    failed: bool,
    failures: u64,
    writes: bool,
    while_unseen: bool,
    after_one_was_told: bool,
    after_the_last_close: bool,
}

/// Handles of one file are opened and closed, write and sync as a seed
/// says, and write-backs fail between. Each sync ends as the error sequence
/// says it does, with two things that are the file layer's and not the
/// kernel's: the sequence starts afresh when the file's last handle is
/// closed, and a handle on which a sync has failed asks the sequence
/// nothing more, because the file layer fails its syncs itself.
#[test]
fn who_is_told_is_who_the_error_sequence_of_linux_tells() {
    #[derive(Debug, Default)]
    struct Met {
        told_open_then: u32,
        told_opened_later: u32,
        ok_after_one_was_told: u32,
        ok_after_the_last_close: u32,
        ok_otherwise: u32,
        stuck: u32,
    }
    let mut met = Met::default();
    for seed in 0..if cfg!(miri) { 2 } else { 40 } {
        let fs = with_fate(100 + seed, UnknownFate::Lost);
        drop(create_durable(&fs, "/file", DURABLE));
        let mut rng = SimRng::derive(seed, 4_000, 0);
        let mut sequence = ErrSeq::default();
        let mut handles: Vec<Opened> = Vec::new();
        let (mut dirty, mut failures, mut dropped_unseen) = (false, 0, false);
        for step in 0..if cfg!(miri) { 150 } else { 600u64 } {
            let at = rng.below(handles.len().max(1) as u64) as usize;
            match rng.below(12) {
                0..=2 if handles.len() < 5 => {
                    let writes = rng.below(2) == 0;
                    let mode = if writes {
                        OpenMode::Write(Create::Never)
                    } else {
                        OpenMode::Read
                    };
                    let failed_before = sequence.value > 0;
                    handles.push(Opened {
                        file: open(&fs, mode),
                        since: sequence.sample(),
                        failed: false,
                        failures,
                        writes,
                        while_unseen: failed_before && !sequence.seen,
                        after_one_was_told: failed_before && sequence.seen,
                        after_the_last_close: dropped_unseen,
                    });
                }
                3 if !handles.is_empty() => {
                    handles.swap_remove(at);
                    if handles.is_empty() {
                        dropped_unseen = sequence.value > 0 && !sequence.seen;
                        sequence = ErrSeq::default();
                    }
                }
                4..=6 if handles.get(at).is_some_and(|handle| handle.writes) => {
                    let offset = 8 + rng.below(8);
                    handles[at].file.write_at(&[step as u8], offset).unwrap();
                    dirty = true;
                }
                7 | 8 if dirty && !handles.is_empty() => {
                    fs.add_write_back(rule().on(OpKind::Len));
                    handles[at].file.len().unwrap();
                    sequence.set();
                    (dirty, failures, dropped_unseen) = (false, failures + 1, false);
                }
                9..=11 if !handles.is_empty() => {
                    let handle = &mut handles[at];
                    let synced = match rng.below(2) {
                        0 => handle.file.sync_data(),
                        _ => handle.file.sync_all(),
                    };
                    let since_it_opened = failures - handle.failures;
                    if handle.failed {
                        let error = synced.unwrap_err();
                        assert_eq!(error.kind(), ErrorKind::Other, "seed {seed} step {step}");
                        assert_eq!(SimError::of(&error), None, "seed {seed} step {step}");
                        met.stuck += 1;
                    } else if sequence.check_and_advance(&mut handle.since) {
                        assert!(synced.is_err(), "seed {seed} step {step}: not told");
                        assert_eq!(told(synced), Fault::Eio);
                        handle.failed = true;
                        if since_it_opened > 0 {
                            met.told_open_then += 1;
                        } else {
                            assert!(handle.while_unseen);
                            met.told_opened_later += 1;
                        }
                    } else {
                        assert!(synced.is_ok(), "seed {seed} step {step}: {synced:?}");
                        dirty = false;
                        if handle.after_one_was_told {
                            met.ok_after_one_was_told += 1;
                        } else if handle.after_the_last_close {
                            met.ok_after_the_last_close += 1;
                        } else {
                            met.ok_otherwise += 1;
                        }
                    }
                }
                _ => {}
            }
            assert_eq!(fs.stats().write_backs, failures, "seed {seed} step {step}");
        }
        check(&fs);
    }
    println!("{met:?}");
    if !cfg!(miri) {
        let each = [
            met.told_open_then,
            met.told_opened_later,
            met.ok_after_one_was_told,
            met.ok_after_the_last_close,
            met.ok_otherwise,
            met.stuck,
        ];
        assert!(each.iter().all(|met| *met >= 20), "{met:?}");
    }
}

/// Appends to a log, syncs it every fifth time, and opens it again when a
/// sync fails.
fn by_chance(seed: u64, odds: WriteBackOdds) -> SimVfs {
    let fs = SimVfs::new(SimConfig {
        write_back: odds,
        ..SimConfig::new(seed)
    });
    let reopen = || fs.open(p("/log"), OpenMode::Append(Create::IfMissing));
    let mut log = reopen().unwrap();
    for round in 0..if cfg!(miri) { 60 } else { 300u64 } {
        log.append(&round.to_le_bytes()).unwrap();
        if round % 5 == 4 && log.sync_data().is_err() {
            drop(log);
            log = reopen().unwrap();
        }
    }
    drop(log);
    fs
}

#[test]
fn write_backs_fail_by_chance_as_the_seed_says() {
    let odds = WriteBackOdds {
        fail: Chance::one_in(6),
        fault: Fault::Enospc,
        ..WriteBackOdds::NONE
    };
    let schedule = |seed: u64| by_chance(seed, odds.clone()).trace_digest();
    assert_eq!(schedule(20), schedule(20));
    assert_ne!(schedule(20), schedule(21));

    let fs = by_chance(20, odds.clone());
    let failed = write_backs(&fs);
    assert_eq!(failed.len() as u64, fs.stats().write_backs);
    let forgotten = failed.iter().filter(|failed| failed.forgotten).count();
    let floor = if cfg!(miri) { 1 } else { 10 };
    assert!(
        forgotten >= floor && failed.len() - forgotten >= floor,
        "{forgotten} of {} forgotten",
        failed.len()
    );
    let as_the_odds_say = |failed: &WriteBackRecord| {
        failed.fault == Fault::Enospc && failed.told == Told::OpenHandles
    };
    assert!(failed.iter().all(as_the_odds_say));
    assert!(fs.stats().faults as usize >= floor);
    check(&fs);

    // The odds are switched on and off as the faults are.
    let fs = by_chance(20, WriteBackOdds::NONE);
    assert_eq!(fs.stats().write_backs, 0);
    let log = fs.open(p("/log"), OpenMode::Append(Create::Never)).unwrap();
    fs.set_write_back(WriteBackOdds {
        fail: Chance::ALWAYS,
        ..odds
    });
    assert_eq!(fs.config().write_back.fail, Chance::ALWAYS);
    log.append(b"lost").unwrap();
    // The odds are no rule.
    fs.clear_rules();
    log.append(b"lost as well").unwrap();
    fs.set_write_back(WriteBackOdds::NONE);
    log.append(b"kept").unwrap();
    assert_eq!(fs.stats().write_backs, 2);
}

#[test]
fn the_odds_fail_a_write_back_after_the_operations_they_name() {
    let fs = SimVfs::new(SimConfig {
        write_back: WriteBackOdds {
            fail: Chance::ALWAYS,
            ops: OpKind::Len.into(),
            ..WriteBackOdds::NONE
        },
        unknown: UnknownFate::Lost,
        ..SimConfig::new(42)
    });
    let file = create_durable(&fs, "/file", DURABLE);
    file.write_at(b"lost", 8).unwrap();
    assert_eq!(fs.stats().write_backs, 0);
    file.len().unwrap();
    assert_eq!(fs.stats().write_backs, 1);
    assert_eq!(told(file.sync_data()), Fault::Eio);
    check(&fs);
}

#[test]
fn the_trace_says_which_write_back_failed_and_which_sync_was_told() {
    let (fs, writer) = losing(19);
    let _reader = open(&fs, OpenMode::Read);
    fs.add_write_back(WriteBackRule::new(Fault::Eio).forgotten(true));
    writer.write_at(b"lost", 8).unwrap();
    writer.write_at(b"since", 12).unwrap();
    writer.sync_data().unwrap_err();
    // The writer syncs no more, so it is not told of the second.
    let second = WriteBackRule::new(Fault::Enospc)
        .on(OpKind::Len)
        .told(Told::Writers)
        .forgotten(false);
    fs.add_write_back(second);
    writer.len().unwrap();

    let trace = fs.trace();
    let lines: Vec<String> = trace.iter().map(Event::to_string).collect();
    let expected = [
        "after #6 the write-back of /file failed with EIO, reported by no call; told: \
         OpenHandles, 2 marked; forgotten at once:\n  #6 write offset 8 len 4 /file: lost",
        "#8 the sync of /file is told of the write-back that failed after #6",
        "#8 sync_data /file: injected EIO, 1 unsynced changes left unknown",
        "after #9 the write-back of /file failed with ENOSPC, reported by no call; told: \
         Writers, 0 marked; not forgotten:\n  #7 write offset 12 len 5 /file: lost",
    ];
    assert_eq!(lines, expected, "{}", lines.join("\n"));

    // Every part of the two events is in the digest.
    let Event::WriteBack(failed) = &trace[0] else {
        panic!("{:?}", trace[0]);
    };
    let failed = failed.clone();
    let events = vec![
        Event::WriteBack(failed.clone()),
        Event::WriteBack(WriteBackRecord {
            after_op: 7,
            ..failed.clone()
        }),
        Event::WriteBack(WriteBackRecord {
            path: "/other".into(),
            ..failed.clone()
        }),
        Event::WriteBack(WriteBackRecord {
            fault: Fault::Enospc,
            ..failed.clone()
        }),
        Event::WriteBack(WriteBackRecord {
            told: Told::Writers,
            ..failed.clone()
        }),
        Event::WriteBack(WriteBackRecord {
            handles: 1,
            ..failed.clone()
        }),
        Event::WriteBack(WriteBackRecord {
            forgotten: false,
            ..failed.clone()
        }),
        Event::WriteBack(WriteBackRecord {
            changes: Vec::new(),
            ..failed
        }),
        trace[1].clone(),
        Event::Reported {
            op: 9,
            path: "/file".into(),
            after_op: 6,
        },
        Event::Reported {
            op: 8,
            path: "/other".into(),
            after_op: 6,
        },
        Event::Reported {
            op: 8,
            path: "/file".into(),
            after_op: 5,
        },
    ];
    let digests: BTreeSet<u64> = events
        .iter()
        .map(|event| digest(std::slice::from_ref(event)))
        .collect();
    assert_eq!(digests.len(), events.len());
}
