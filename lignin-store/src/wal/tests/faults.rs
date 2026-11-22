//! The log when the file layer fails: what a failed append, a failed sync
//! and a failed turn leave, and that the log on disk agrees with what each
//! call reported, whatever crash follows.

use lignin_io::sim::{
    Chance, Effect, Event, Fault, FaultOdds, FaultRule, OpKind, OpSet, Outcome, SimConfig,
    SimError, UnknownFate,
};

use std::{io, sync::Mutex};

use lignin_io::{DirEntry, OpenMode, VfsFile, VfsLock};

use super::*;
use crate::segment::SegmentStoreError;

const FEW_STATES: sim::Plans = sim::Plans {
    every_plan_limit: 300,
    samples: 8,
};

fn wal_path(file: u32) -> PathBuf {
    log_path(sim::db_dir(), file)
}

fn injected(error: &LogError) -> Option<Fault> {
    SimError::injected(error.io_error()?)
}

/// A session on a simulated file system that records every operation.
fn traced_run(seed: u64, sector_size: u64, options: LogOptions) -> Run {
    Run::new(sim::traced(seed, sector_size), options, FEW_STATES)
}

/// The operations made since operation `after`, as their kind and the name
/// of what they were made on.
fn ops_since(fs: &lignin_io::SimVfs, after: u64) -> Vec<(OpKind, String)> {
    fs.trace()
        .into_iter()
        .filter_map(|event| match event {
            Event::Op(op) if op.op > after => {
                let name = op.path.rsplit('/').next().unwrap_or_default().to_owned();
                Some((op.kind, name))
            }
            _ => None,
        })
        .collect()
}

/// What the last injected fault left behind.
fn last_effect(fs: &lignin_io::SimVfs) -> Effect {
    match sim::failed_ops(fs).last().map(|op| op.outcome) {
        Some(Outcome::Injected { effect, .. }) => effect,
        other => panic!("no injected fault: {other:?}"),
    }
}

/// Fails the next append to log file `file`, and says whether the bytes it
/// wrote stay in the file.
fn fail_append(run: &Run, file: u32, fault: Fault, torn: bool) {
    let rule = FaultRule::new(fault)
        .on(OpKind::Append)
        .at(wal_path(file))
        .torn_append(torn);
    run.fs.add_rule(rule);
}

/// Appends a group that the file layer fails, and returns its bytes with
/// the place and the version it was written for and how the append ended.
fn failing_commit(run: &mut Run) -> (Item, Result<Appended, LogError>) {
    let db = run.db.as_ref().unwrap();
    let at = db.log.next_group().unwrap();
    let bytes = group(&at, &mut run.rng);
    let end = place(
        at.place().file().get(),
        at.place().offset() + bytes.len() as u64,
    );
    let appended = db.log.append_group(&at, &bytes);
    let item = Item {
        at: at.place(),
        end,
        bytes,
        version: Some(at.version()),
    };
    (item, appended)
}

#[test]
fn an_append_that_was_taken_back_is_cut_off_by_the_next_sync_and_comes_back_in_no_crash_after_it() {
    let (mut none, mut part, mut all) = (0, 0, 0);
    let mut came_back = 0;
    for seed in 1..=40 {
        let fault = [Fault::Eio, Fault::Enospc][seed as usize % 2];
        let mut run = traced_run(seed, 512, LogOptions::default());
        for _ in 0..3 {
            run.append();
        }
        if seed % 3 == 0 {
            run.sync();
        }
        let written = run.model.items.len();
        fail_append(&run, 1, fault, false);
        let before = run.fs.stats().ops;
        let (failed, appended) = failing_commit(&mut run);
        let error = appended.unwrap_err();
        assert!(matches!(error, LogError::TakenBack { .. }), "{error}");
        assert_eq!(injected(&error), Some(fault));
        match last_effect(&run.fs) {
            Effect::AppendUndone(0) => none += 1,
            Effect::AppendUndone(len) if len == failed.bytes.len() as u64 => all += 1,
            Effect::AppendUndone(_) => part += 1,
            other => panic!("{other:?}"),
        }
        // The log looked at the file's length and did nothing else: it
        // syncs nothing where a page step may be holding its lock.
        assert_eq!(
            ops_since(&run.fs, before),
            [OpKind::Append, OpKind::Len].map(|op| (op, log_name(1)))
        );
        // Until the log is synced, a crash can bring back what the append
        // wrote and took back: the whole group, as the last item.
        run.model.took_back(failed.clone());
        run.crash_states_with(|read| came_back += u64::from(read.last() == Some(&failed)));

        // The file takes no more items, and the log does not turn before
        // a manifest names the file closed. It makes no call of the file
        // layer to say so.
        let db = run.db.as_ref().unwrap();
        assert!(db.log.is_due());
        assert_eq!(db.log.end(), Some(failed.at));
        assert!(matches!(db.log.next_group(), Err(LogError::NoActiveFile)));
        assert!(matches!(
            db.log.append_change(&change(&mut Rng(1))),
            Err(LogError::NoActiveFile)
        ));
        assert!(matches!(
            db.log.append_mark(CommitVersion::EMPTY),
            Err(LogError::NoActiveFile)
        ));
        let before = run.fs.stats().ops;
        let error = db.log.rotate().unwrap_err();
        assert!(
            matches!(&error, LogError::CutNotDurable { path } if *path == wal_path(1)),
            "{error}"
        );
        assert_eq!(ops_since(&run.fs, before), []);

        // A sync of the log makes the file durable as it is: one sync of
        // the file, also when everything before the group was durable
        // already. From then on no crash brings the group back, which is
        // when a commit reports that it failed.
        assert!(db.log.waits_for_sync());
        match seed % 3 {
            0 => {
                assert_eq!(db.log.durable().place, Some(failed.at));
                db.log.sync_through(failed.at).unwrap();
                run.synced();
            }
            1 => {
                db.log.settle().unwrap();
                run.synced();
            }
            _ => run.sync(),
        }
        assert_eq!(
            ops_since(&run.fs, before),
            [(OpKind::SyncData, log_name(1))]
        );
        // With nothing left to settle, settling does nothing.
        let db = run.db.as_ref().unwrap();
        assert!(!db.log.waits_for_sync());
        db.log.settle().unwrap();
        assert_eq!(
            ops_since(&run.fs, before),
            [(OpKind::SyncData, log_name(1))]
        );
        run.note_durable();
        assert!(run.model.maybe.is_none());
        run.crash_states_with(|read| assert_ne!(read.last(), Some(&failed)));

        // The log still does not turn: the cut is a manifest that names
        // the file closed where its last good item ends, and whoever keeps
        // the files makes it. Then the log goes on in the next file, and
        // the next commit takes the version of the failed one.
        let db = run.db.as_ref().unwrap();
        assert!(matches!(
            db.log.rotate(),
            Err(LogError::CutNotDurable { .. })
        ));
        assert!(matches!(db.log.next_group(), Err(LogError::NoActiveFile)));
        run.roll();
        let db = run.db.as_ref().unwrap();
        assert_eq!(
            db.named()[0],
            log_file(1, FileState::Closed, failed.at.offset())
        );
        let at = db.log.next_group().unwrap();
        assert_eq!(
            (at.place(), at.version()),
            (place(2, FIRST), failed.version.unwrap())
        );
        run.append();
        run.append();
        assert_eq!(run.model.items.len(), written + 2);
        run.check_crash_states();
        run.sync();
        run.note_durable();
        assert_eq!(run.model.durable, written + 2);
        run.check_crash_states();
        run.restart();
        assert_eq!(run.stats.other_errors, 0);
    }
    assert!(none > 0 && part > 3 && all > 3, "{none} {part} {all}");
    assert!(came_back > 0, "{came_back}");
}

#[test]
fn a_cut_that_cannot_be_made_durable_leaves_the_append_unsettled() {
    for seed in 1..=24 {
        let mut run = traced_run(seed, 512, LogOptions::default());
        for _ in 0..3 {
            run.append();
        }
        // In every other run a sync has failed already, and the log has
        // gone on taking items: it cannot be synced in this session.
        let failed_before = seed % 2 == 0;
        let fail_sync = FaultRule::new(Fault::Eio).on(OpSet::SYNCS).at(wal_path(1));
        if failed_before {
            run.fs.add_rule(fail_sync.clone());
            run.sync();
            assert_eq!(run.stats.sync_failed, 1);
            run.append();
        }
        if run.db.is_none() {
            continue;
        }
        fail_append(&run, 1, Fault::Enospc, false);
        let (failed, appended) = failing_commit(&mut run);
        assert!(matches!(appended, Err(LogError::TakenBack { .. })));
        run.model.took_back(failed.clone());

        // The sync that would keep the group from coming back fails.
        if !failed_before {
            run.fs.add_rule(fail_sync);
        }
        let db = run.db.as_mut().unwrap();
        let before = run.fs.stats().ops;
        let error = db.log.settle().unwrap_err();
        assert!(
            matches!(&error, LogError::Unsettled { path, .. } if *path == wal_path(1)),
            "{error}"
        );
        assert_eq!(
            ops_since(&run.fs, before).len(),
            usize::from(!failed_before),
            "a sync that failed is not tried again"
        );
        assert!(matches!(db.log.sync(), Err(LogError::SyncFailed { .. })));
        assert!(matches!(db.log.settle(), Err(LogError::Unsettled { .. })));

        // Nothing keeps a crash from bringing the group back, so the log
        // takes nothing more and says so.
        let unsettled = |result: Result<(), LogError>| {
            let error = result.unwrap_err();
            assert!(
                matches!(&error, LogError::Unsettled { path, .. } if *path == wal_path(1)),
                "{error}"
            );
        };
        unsettled(db.log.next_group().map(drop));
        unsettled(db.log.append_change(&change(&mut Rng(2))).map(drop));
        unsettled(db.log.append_mark(CommitVersion::EMPTY).map(drop));
        unsettled(db.log.rotate().map(drop));
        unsettled(db.log.create().map(drop));
        assert!(matches!(
            db.log.roll(&mut db.manifest),
            Err(LogError::SyncFailed { .. })
        ));
        run.model.cut_by_sync = false;
        run.check_crash_states();

        // The next session settles it, as it settles any unsettled append.
        run.fs.clear_rules();
        run.restart();
        run.check_crash_states();
        run.append();
        run.sync();
        run.note_durable();
        run.check_crash_states();
        let read = Run::read(&run.fs).unwrap();
        run.model.check(&read, true).unwrap();
        assert!(!read.contains(&failed));
    }
}

#[test]
fn an_append_that_stays_in_the_file_is_in_the_log_exactly_when_all_of_it_is_there() {
    let (mut whole, mut torn) = (0, 0);
    for seed in 1..=40 {
        let mut run = traced_run(seed, 512, LogOptions::default());
        for _ in 0..3 {
            run.append();
        }
        fail_append(&run, 1, Fault::Eio, true);
        let before = run.fs.stats().ops;
        let (failed, appended) = failing_commit(&mut run);
        let Effect::AppendTorn(left) = last_effect(&run.fs) else {
            panic!("the bytes stay");
        };
        if left == failed.bytes.len() as u64 {
            // Every byte is there, and they are the group: the commit has
            // happened, whatever the file layer said.
            assert_eq!(appended.unwrap().end, failed.end);
            assert_eq!(
                ops_since(&run.fs, before),
                [OpKind::Append, OpKind::Len, OpKind::Read].map(|op| (op, log_name(1)))
            );
            run.model.items.push(failed.clone());
            whole += 1;
        } else {
            // A part of the group stays, and no crash makes it the group:
            // the commit has failed, and nothing has to be synced to say
            // so. With nothing left the file has the length it had, which
            // the log cannot tell from an append that was taken back.
            let error = appended.unwrap_err();
            match left {
                0 => assert!(matches!(error, LogError::TakenBack { .. }), "{error}"),
                _ => assert!(matches!(error, LogError::NotAppended { .. }), "{error}"),
            }
            // What stays is read back: a recovery takes nothing from it.
            let looked = [OpKind::Append, OpKind::Len, OpKind::Read];
            let looked = &looked[..if left == 0 { 2 } else { 3 }];
            let ops = ops_since(&run.fs, before);
            assert_eq!(ops.len(), looked.len());
            for (op, kind) in ops.iter().zip(looked) {
                assert_eq!(*op, (*kind, log_name(1)));
            }
            torn += 1;
        }
        run.check_crash_states();

        // Either way the handle is not trusted with another append. A
        // group that is in the log needs no cut, and the log turns as it
        // does from a full file. Behind a group that is not, the log turns
        // once a manifest names the file closed.
        let db = run.db.as_ref().unwrap();
        assert!(db.log.is_due());
        assert!(matches!(db.log.next_group(), Err(LogError::NoActiveFile)));
        if left == failed.bytes.len() as u64 {
            assert!(db.log.rotate().unwrap());
        } else {
            assert!(matches!(
                db.log.rotate(),
                Err(LogError::CutNotDurable { .. })
            ));
            run.roll();
            assert_eq!(
                run.db.as_ref().unwrap().named()[0],
                log_file(1, FileState::Closed, failed.at.offset())
            );
        }
        let db = run.db.as_ref().unwrap();
        let at = db.log.next_group().unwrap();
        let next = match left == failed.bytes.len() as u64 {
            true => failed.version.unwrap().next().unwrap(),
            false => failed.version.unwrap(),
        };
        assert_eq!(at.version(), next);
        run.append();
        run.check_crash_states();
        run.sync();
        run.note_durable();
        assert_eq!(run.model.durable, run.model.items.len());
        run.check_crash_states();
        run.restart();
        assert_eq!(run.stats.other_errors, 0);
    }
    assert!(whole > 2 && torn > 10, "{whole} {torn}");
}

#[test]
fn nothing_is_appended_behind_a_failed_append_until_the_cut_is_made() {
    let mut torn = 0;
    for seed in 1..=24 {
        // Small sectors, so that a part of the group stays more often
        // than all of it or none.
        let mut run = traced_run(seed, 16, LogOptions::default());
        for _ in 0..3 {
            run.append();
        }
        fail_append(&run, 1, Fault::Eio, true);
        let (failed, appended) = failing_commit(&mut run);
        let Err(LogError::NotAppended { .. }) = appended else {
            continue;
        };
        torn += 1;
        // No sync of the file succeeds any more, so whoever keeps the
        // files cannot make the cut.
        let syncs_fail = FaultRule::new(Fault::Eio)
            .on(OpSet::SYNCS)
            .at(wal_path(1))
            .forever();
        run.fs.add_rule(syncs_fail);
        let db = run.db.as_mut().unwrap();
        for _ in 0..2 {
            assert!(matches!(
                db.log.roll(&mut db.manifest),
                Err(LogError::SyncFailed { .. })
            ));
            // The file the log would turn to is named and ready, and the
            // log does not turn to it: it takes nothing.
            assert!(matches!(
                db.log.rotate(),
                Err(LogError::CutNotDurable { .. })
            ));
            assert!(matches!(db.log.next_group(), Err(LogError::NoActiveFile)));
            assert!(matches!(
                db.log.append_change(&change(&mut Rng(5))),
                Err(LogError::NoActiveFile)
            ));
            assert!(matches!(
                db.log.append_mark(CommitVersion::EMPTY),
                Err(LogError::NoActiveFile)
            ));
        }
        assert_eq!(db.log.end(), Some(failed.at));
        run.check_crash_states();

        // The next session makes the cut with its first roll, and goes on.
        run.fs.clear_rules();
        run.restart();
        assert_eq!(
            run.db.as_ref().unwrap().named()[0],
            log_file(1, FileState::Closed, failed.at.offset())
        );
        run.append();
        run.sync();
        run.note_durable();
        run.check_crash_states();
        let read = Run::read(&run.fs).unwrap();
        run.model.check(&read, true).unwrap();
        assert!(!read.contains(&failed));
    }
    assert!(torn > 3, "{torn}");
}

#[test]
fn an_append_that_a_manifest_has_cut_off_is_settled_whatever_a_later_sync_does() {
    let mut run = traced_run(3, 512, LogOptions::default());
    for _ in 0..3 {
        run.append();
    }
    fail_append(&run, 1, Fault::Enospc, false);
    let (failed, appended) = failing_commit(&mut run);
    assert!(matches!(appended, Err(LogError::TakenBack { .. })));
    run.model.took_back(failed.clone());
    // Whoever keeps the files makes the cut before the committer has
    // settled its append, and the log goes on in its next file.
    run.roll();
    assert!(run.model.maybe.is_none());
    run.append();
    assert_eq!(
        run.db.as_ref().unwrap().log.end().unwrap().file(),
        file_id(2)
    );

    // That file cannot be synced. The group is out of the log for good
    // all the same, and to settle it nothing is synced.
    let syncs_fail = FaultRule::new(Fault::Eio).on(OpSet::SYNCS).at(wal_path(2));
    run.fs.add_rule(syncs_fail);
    let db = run.db.as_ref().unwrap();
    let before = run.fs.stats().ops;
    db.log.settle().unwrap();
    assert_eq!(ops_since(&run.fs, before), []);
    // The sync that fails leaves the commits of the next file not
    // durable, and the log goes on taking items.
    assert!(matches!(db.log.sync(), Err(LogError::SyncFailed { .. })));
    db.log.settle().unwrap();
    assert!(db.log.next_group().is_ok());
    run.check_crash_states();
    run.crash_states_with(|read| assert!(!read.contains(&failed)));
}

#[test]
fn a_roll_goes_on_until_the_log_has_a_file_that_takes_items_and_one_to_turn_to() {
    let mut whole = 0;
    for seed in 1..=16 {
        let mut run = traced_run(seed, 512, small_files(600));
        while !run.db.as_ref().unwrap().log.is_due() {
            run.append();
        }
        // The start record of the next file reaches the file whole, and
        // the file layer says the append failed.
        fail_append(&run, 2, Fault::Eio, true);
        run.roll();
        if last_effect(&run.fs) != Effect::AppendTorn(FIRST - HEADER) {
            continue;
        }
        whole += 1;
        // The log has turned to a file that takes nothing, so it has no
        // file that takes items, and the roll turned it again: the log
        // ends in the file after it, and the one after that is named and
        // ready. The roll synced the file that takes nothing first, so
        // the manifest names it closed behind its start record.
        assert_eq!(run.stats.other_errors, 0);
        let db = run.db.as_ref().unwrap();
        assert_eq!(db.log.end(), Some(place(3, FIRST)));
        assert!(!db.log.is_due());
        assert!(!db.log.rotate().unwrap());
        let named = db.named();
        assert_eq!(named[1], log_file(2, FileState::Closed, FIRST));
        assert_eq!(named.last(), Some(&log_file(4, FileState::Open, HEADER)));
        run.append();
        run.check_crash_states();
        run.sync();
        run.note_durable();
        run.check_crash_states();
        run.restart();
    }
    assert!(whole > 2, "{whole}");
}

#[test]
fn an_append_that_cannot_be_settled_ends_the_session() {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Open {
        Len,
        Read,
    }
    let mut seen = Vec::new();
    for (seed, open) in (1..=60).flat_map(|seed| [Open::Len, Open::Read].map(|o| (seed, o))) {
        let mut run = traced_run(seed, 64, LogOptions::default());
        for _ in 0..3 {
            run.append();
        }
        run.sync();
        run.append();
        let follow = match open {
            Open::Len => OpKind::Len,
            Open::Read => OpKind::Read,
        };
        fail_append(&run, 1, Fault::Eio, open == Open::Read);
        run.fs
            .add_rule(FaultRule::new(Fault::Eio).on(follow).at(wal_path(1)));
        let (failed, appended) = failing_commit(&mut run);
        let whole = last_effect(&run.fs) == Effect::AppendTorn(failed.bytes.len() as u64)
            || !matches!(
                sim::failed_ops(&run.fs).last().unwrap().kind,
                OpKind::Append
            );
        if open == Open::Read && matches!(appended, Err(LogError::TakenBack { .. })) {
            // Nothing of the group stayed, so there was nothing to read
            // back.
            assert!(!whole);
            continue;
        }
        let error = appended.unwrap_err();
        assert!(
            matches!(error, LogError::Unsettled { .. }),
            "{open:?}: {error}"
        );
        assert_eq!(injected(&error), Some(Fault::Eio));
        assert_eq!(sim::failed_ops(&run.fs).last().unwrap().kind, follow);
        seen.push(open);

        // The log takes nothing more, and says why.
        let db = run.db.as_mut().unwrap();
        let unsettled = |result: Result<(), LogError>| {
            let error = result.unwrap_err();
            assert!(matches!(&error, LogError::Unsettled { path, .. } if *path == wal_path(1)));
            assert_eq!(injected(&error), None, "the first report has the cause");
        };
        unsettled(db.log.next_group().map(drop));
        unsettled(db.log.append_change(&change(&mut Rng(2))).map(drop));
        unsettled(db.log.append_mark(CommitVersion::EMPTY).map(drop));
        unsettled(db.log.rotate().map(drop));
        unsettled(db.log.create().map(drop));
        unsettled(db.log.roll(&mut db.manifest).map(drop));

        // The group may be in the log or not, now and after a crash, and
        // the next open settles which.
        run.model.maybe = Some(failed);
        run.check_crash_states();
        run.restart();
        run.check_crash_states();
        run.append();
        run.sync();
        run.note_durable();
        run.check_crash_states();
        let read = Run::read(&run.fs).unwrap();
        run.model.check(&read, true).unwrap();
    }
    for open in [Open::Len, Open::Read] {
        assert!(
            seen.iter().filter(|seen| **seen == open).count() > 2,
            "{open:?}"
        );
    }
}

/// What whoever answers for a session asks, to learn that its log makes
/// nothing durable any more: it may not have made the sync that failed.
#[test]
fn the_log_says_that_a_sync_failed_and_makes_none_to_say_so() {
    let options = LogOptions {
        sync: SyncPolicy::EveryCommit,
        ..LogOptions::default()
    };
    let mut run = Run::new(sim::traced(6, 64), options, FEW_STATES);
    for _ in 0..3 {
        run.append();
    }
    run.note_durable();
    let db = run.db.as_ref().unwrap();
    let before = run.fs.stats().ops;
    assert!(db.log.sync_failed().is_none());
    assert_eq!(ops_since(&run.fs, before), []);

    // The device has no room for one sync, and has room again.
    run.fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .on(OpSet::SYNCS)
            .at(wal_path(1)),
    );
    let (item, appended) = failing_commit(&mut run);
    let appended = appended.unwrap();
    run.model.items.push(item);
    let db = run.db.as_ref().unwrap();
    let error = db.log.sync_for_commit(&appended).unwrap_err();
    assert_eq!(injected(&error), Some(Fault::Enospc));
    run.fs.clear_rules();

    // The log says so from then on, with the file and with what the file
    // layer said. Asking makes no operation, and changes nothing: a sync
    // is refused as it was before the question, and the log takes items.
    let durable = db.log.durable();
    let before = run.fs.stats().ops;
    for _ in 0..2 {
        let said = db.log.sync_failed().expect("a sync failed");
        assert!(
            matches!(&said, LogError::SyncFailed { path, source }
                if *path == wal_path(1) && source.kind() == io::ErrorKind::StorageFull),
            "{said}"
        );
        assert_eq!(said.to_string(), error.to_string());
        assert_eq!(injected(&said), None);
    }
    assert!(matches!(db.log.sync(), Err(LogError::SyncFailed { .. })));
    assert!(db.log.sync_failed().is_some());
    assert_eq!(ops_since(&run.fs, before), []);
    assert_eq!(db.log.durable(), durable);
    let (item, appended) = failing_commit(&mut run);
    appended.unwrap();
    run.model.items.push(item);
    run.check_crash_states();

    // It is of the session: the log of the next one has no failed sync.
    run.restart();
    assert!(run.db.as_ref().unwrap().log.sync_failed().is_none());
    run.check_crash_states();
}

#[test]
fn a_sync_that_fails_is_never_made_good_in_its_session_and_the_next_writes_the_file_again() {
    for (seed, fate) in [
        (1, UnknownFate::Lost),
        (2, UnknownFate::Kept),
        (3, UnknownFate::Drawn(0)),
        (4, UnknownFate::Lost),
        (5, UnknownFate::Drawn(1)),
    ] {
        let options = LogOptions {
            sync: SyncPolicy::EveryCommit,
            ..LogOptions::default()
        };
        let config = SimConfig {
            unknown: fate,
            ..sim::traced(seed, 64)
        };
        let mut run = Run::new(config, options, FEW_STATES);
        for _ in 0..4 {
            run.append();
        }
        run.note_durable();
        let durable = run.db.as_ref().unwrap().log.durable();
        assert_eq!(run.model.durable, 4);

        // The fsync of a commit fails: the group is in the log, and the
        // commit is not known to be durable.
        run.fs
            .add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).at(wal_path(1)));
        let (item, appended) = failing_commit(&mut run);
        let appended = appended.unwrap();
        run.model.items.push(item);
        let db = run.db.as_mut().unwrap();
        let error = db.log.sync_for_commit(&appended).unwrap_err();
        assert!(matches!(&error, LogError::SyncFailed { path, .. } if *path == wal_path(1)));
        assert_eq!(injected(&error), Some(Fault::Eio));
        assert_eq!(db.log.durable(), durable);

        // No later sync of the session succeeds, and none is made: what
        // was not durable then cannot be made durable in that file.
        let before = run.fs.stats().ops;
        for result in [
            db.log.sync(),
            db.log.sync_through(appended.end),
            db.log.sync_if_due().map(|synced| synced.unwrap()),
            db.log.roll(&mut db.manifest).map(|_| durable),
        ] {
            let error = result.unwrap_err();
            assert!(matches!(error, LogError::SyncFailed { .. }), "{error}");
            assert_eq!(injected(&error), None);
        }
        assert_eq!(ops_since(&run.fs, before), []);
        assert_eq!(db.log.durable(), durable);
        // The log still takes items.
        let (item, appended) = failing_commit(&mut run);
        appended.unwrap();
        run.model.items.push(item);
        run.check_crash_states();

        // The session ends with no crash. The next one cannot sync those
        // items either: a sync that succeeds now covers nothing that was
        // appended before the failure. It writes the file again, into a
        // new file that takes its name.
        let before = run.fs.stats().ops;
        run.restart();
        assert_eq!(run.model.items.len(), 6);
        assert_eq!(run.model.durable, 6, "the roll of the new session");
        let ops = ops_since(&run.fs, before);
        let temporary = format!("{}.tmp", log_name(1));
        let on = |name: &str| -> Vec<OpKind> {
            let named = ops.iter().filter(|(_, on)| on == name);
            named.map(|(op, _)| *op).collect()
        };
        assert_eq!(
            on(&temporary),
            [
                OpKind::RemoveFile,
                OpKind::Open,
                OpKind::Append,
                OpKind::SyncAll,
                OpKind::Rename
            ]
        );
        assert!(
            !on(&log_name(1)).iter().any(|op| matches!(
                op,
                OpKind::Append
                    | OpKind::Write
                    | OpKind::SetLen
                    | OpKind::SyncData
                    | OpKind::SyncAll
            )),
            "the file of the earlier session is not written and not relied on: {ops:?}"
        );
        // Whatever became of the bytes the failed sync left unknown, every
        // state a crash leaves now has all six items.
        run.check_crash_states();
        let db = run.db.as_ref().unwrap();
        let end = run.model.items.last().unwrap().end;
        assert_eq!(db.named()[0], log_file(1, FileState::Closed, end.offset()));
        for _ in 0..3 {
            run.append();
            run.note_durable();
            run.check_crash_states();
        }
        run.crash(seed);
        assert_eq!(run.model.items.len(), 9, "everything was durable");
        assert_eq!(run.stats.other_errors, 0);
    }
}

#[test]
fn a_file_of_an_earlier_session_is_written_again_until_it_is_durable() {
    let temporary = format!("{}.tmp", wal_path(1).display());
    for (op, on) in [
        (OpKind::Open, temporary.clone()),
        (OpKind::Append, temporary.clone()),
        (OpKind::SyncAll, temporary.clone()),
        (OpKind::Rename, temporary.clone()),
        (OpKind::SyncDir, format!("{}/wal", sim::DB)),
    ] {
        let mut run = traced_run(7, 64, LogOptions::default());
        for _ in 0..5 {
            run.append();
        }
        // The session ends with nothing of its items synced.
        run.db = None;
        let mut db = Db::open(sim::shared(&run.fs), sim::db_dir(), LogOptions::default()).unwrap();
        run.fs.add_rule(FaultRule::new(Fault::Eio).on(op).at(&on));
        let error = db.roll().unwrap_err();
        assert!(matches!(error, LogError::Io { .. }), "{op}: {error}");
        assert_eq!(injected(&error), Some(Fault::Eio));
        assert_eq!(db.log.durable().place, None, "{op}");
        run.db = Some(db);
        run.check_crash_states();

        // It can be done again, and is done then.
        run.roll();
        run.note_durable();
        assert_eq!(run.model.durable, 5, "{op}");
        let names = sim::shared(&run.fs)
            .read_dir(&FileNames::WAL.dir(sim::db_dir()))
            .unwrap();
        assert!(
            !names
                .iter()
                .any(|entry| entry.name.to_string_lossy().ends_with(".tmp")),
            "{op}: {names:?}"
        );
        run.check_crash_states();
        run.append();
        run.restart();
        assert_eq!(run.model.items.len(), 6);
    }

    // What an interrupted rewrite left behind is removed by the next open.
    let mut run = traced_run(8, 64, LogOptions::default());
    run.append();
    run.db = None;
    put_file(&run.fs, Path::new(&temporary), b"half a copy");
    run.restart();
    assert!(run
        .fs
        .open(Path::new(&temporary), lignin_io::OpenMode::Read)
        .is_err());
    assert_eq!(run.model.items.len(), 1);
}

#[test]
fn a_turn_that_fails_leaves_the_log_where_it_was() {
    let (mut taken_back, mut torn, mut whole) = (0, 0, 0);
    for seed in 1..=60 {
        let stays = seed % 2 == 0;
        let mut run = traced_run(seed, 8, small_files(600));
        while !run.db.as_ref().unwrap().log.is_due() {
            run.append();
        }
        let end = run.db.as_ref().unwrap().log.end().unwrap();
        fail_append(&run, 2, Fault::Eio, stays);
        let turned = run.db.as_ref().unwrap().log.rotate();
        let left = match last_effect(&run.fs) {
            Effect::AppendUndone(left) | Effect::AppendTorn(left) => left,
            other => panic!("{other:?}"),
        };
        let db = run.db.as_ref().unwrap();
        let in_the_file = stays && left == 25;
        if in_the_file {
            // The start record is in the file: the log has turned, and
            // turns again before it takes an item.
            assert!(turned.unwrap());
            assert_eq!(db.log.end(), Some(place(2, FIRST)));
            assert!(db.log.is_due());
            assert!(matches!(db.log.next_group(), Err(LogError::NoActiveFile)));
            whole += 1;
        } else {
            let error = turned.unwrap_err();
            assert!(matches!(&error, LogError::NotAppended { path, .. } if *path == wal_path(2)));
            // The log still ends where it did, in a file that takes items.
            assert_eq!(db.log.end(), Some(end));
            db.log.next_group().unwrap();
            torn += u32::from(stays);
            taken_back += u32::from(!stays);
        }
        // The file it would have turned to is given up either way: its
        // handle may append no more.
        assert!(matches!(db.log.rotate(), Err(LogError::NoSpare)));
        run.check_crash_states();
        if !in_the_file {
            run.append();
        }
        run.roll();
        run.append();
        run.check_crash_states();
        let db = run.db.as_ref().unwrap();
        assert_eq!(db.log.end().unwrap().file(), file_id(3));
        let on_disk = run.fs.open(&wal_path(2), lignin_io::OpenMode::Read).is_ok();
        assert_eq!(on_disk, in_the_file, "a file that was given up is deleted");
        let named = db.named().iter().any(|file| file.id() == file_id(2));
        assert_eq!(named, in_the_file);
        run.restart();
        run.append();
        run.check_crash_states();
        assert_eq!(run.stats.other_errors, 0);
    }
    assert!(
        taken_back > 20 && torn > 10 && whole > 2,
        "{taken_back} {torn} {whole}"
    );
}

#[test]
fn a_file_that_cannot_be_created_or_named_or_deleted_leaves_the_log_as_it_was() {
    // A creation that fails uses up no id: the next one makes the file
    // that it could not make.
    for op in [OpKind::Open, OpKind::Append] {
        let mut run = traced_run(9, 64, small_files(500));
        while !run.db.as_ref().unwrap().log.is_due() {
            run.append();
        }
        let db = run.db.as_mut().unwrap();
        assert!(db.log.rotate().unwrap());
        run.fs
            .add_rule(FaultRule::new(Fault::Eio).on(op).at(wal_path(3)));
        let error = db.log.create().unwrap_err();
        assert!(matches!(error, LogError::Io { .. }), "{op}: {error}");
        assert!(run
            .fs
            .open(&wal_path(3), lignin_io::OpenMode::Read)
            .is_err());
        assert_eq!(db.log.create().unwrap(), file_id(3));
        db.log.commit(&mut db.manifest).unwrap();
        let last = db.manifest.manifest().unwrap().last_file_ids().wal;
        assert_eq!(last, Some(file_id(3)));
        run.check_crash_states();
        run.append();
        run.restart();
    }

    // A file that was created is made durable with the manifest that
    // names it, in one set of syncs. Where its sync fails, no manifest
    // names it: it is given up and removed, and the next creation makes a
    // file under the same id.
    let mut run = traced_run(9, 64, small_files(500));
    while !run.db.as_ref().unwrap().log.is_due() {
        run.append();
    }
    let db = run.db.as_mut().unwrap();
    assert!(db.log.rotate().unwrap());
    run.fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::SyncData)
            .at(wal_path(3)),
    );
    assert_eq!(db.log.create().unwrap(), file_id(3));
    let error = db.log.commit(&mut db.manifest).unwrap_err();
    assert!(matches!(error, LogError::Io { .. }), "{error}");
    assert!(db.named().iter().all(|file| file.id() != file_id(3)));
    assert!(!db.log.has_file_ready());
    assert!(run
        .fs
        .open(&wal_path(3), lignin_io::OpenMode::Read)
        .is_err());
    assert_eq!(db.log.create().unwrap(), file_id(3));
    db.log.commit(&mut db.manifest).unwrap();
    assert!(db.log.has_file_ready());
    let last = db.manifest.manifest().unwrap().last_file_ids().wal;
    assert_eq!(last, Some(file_id(3)));
    run.check_crash_states();
    run.append();
    run.restart();

    // The syncs of a new file are made, and the rename of the manifest
    // that names it fails: the manifest is in doubt, and when it is
    // opened again the old one is in force. The file is durable and is
    // one of the log's, so the next manifest names it.
    let mut run = traced_run(12, 64, small_files(500));
    while !run.db.as_ref().unwrap().log.is_due() {
        run.append();
    }
    let manifest = sim::db_dir().join(ManifestStore::TEMP_FILE_NAME);
    let db = run.db.as_mut().unwrap();
    assert!(db.log.rotate().unwrap());
    assert_eq!(db.log.create().unwrap(), file_id(3));
    run.fs
        .add_rule(FaultRule::new(Fault::Eio).on(OpKind::Rename).at(&manifest));
    let error = db.log.commit(&mut db.manifest).unwrap_err();
    assert!(matches!(error, LogError::Io { .. }), "{error}");
    assert!(db.manifest.manifest().is_err(), "the manifest is in doubt");
    db.manifest = ManifestStore::open(db.vfs.clone(), &db.dir).unwrap();
    db.log.manifest_replaced(&db.manifest).unwrap();
    assert!(db.named().iter().all(|file| file.id() != file_id(3)));
    assert!(!db.log.has_file_ready());
    assert!(matches!(
        db.log.create(),
        Err(LogError::SpareExists { file }) if file == file_id(3)
    ));
    db.log.commit(&mut db.manifest).unwrap();
    assert!(db.log.has_file_ready());
    assert!(db.named().contains(&log_file(3, FileState::Open, HEADER)));
    run.check_crash_states();
    while !run.db.as_ref().unwrap().log.is_due() {
        run.append();
    }
    assert!(run.db.as_ref().unwrap().log.rotate().unwrap());
    run.append();
    assert_eq!(
        run.db.as_ref().unwrap().log.end().unwrap().file(),
        file_id(3)
    );
    run.check_crash_states();
    run.restart();

    // A manifest that cannot be replaced.
    let mut run = traced_run(10, 64, small_files(500));
    while !run.db.as_ref().unwrap().log.is_due() {
        run.append();
    }
    let db = run.db.as_mut().unwrap();
    assert!(db.log.rotate().unwrap());
    db.log.create().unwrap();
    let manifest = sim::db_dir().join(ManifestStore::TEMP_FILE_NAME);
    run.fs
        .add_rule(FaultRule::new(Fault::Eio).on(OpKind::Write).at(&manifest));
    let error = db.log.commit(&mut db.manifest).unwrap_err();
    assert!(matches!(error, LogError::Io { .. }), "{error}");
    while !run.db.as_ref().unwrap().log.is_due() {
        run.append();
    }
    let db = run.db.as_mut().unwrap();
    assert!(matches!(
        db.log.rotate(),
        Err(LogError::SpareNotNamed { file }) if file == file_id(3)
    ));
    // Once its rename has begun, the manifest is in doubt until it is
    // opened again.
    run.fs
        .add_rule(FaultRule::new(Fault::Eio).on(OpKind::Rename).at(&manifest));
    let error = db.log.commit(&mut db.manifest).unwrap_err();
    assert!(matches!(error, LogError::Io { .. }), "{error}");
    let error = db.log.roll(&mut db.manifest).unwrap_err();
    assert!(
        matches!(
            error,
            LogError::Manifest(SegmentStoreError::ManifestInDoubt { .. })
        ),
        "{error}"
    );
    assert!(error.io_error().is_none());
    assert!(std::error::Error::source(&error).is_some());
    run.check_crash_states();
    run.restart();
    run.append();
    run.check_crash_states();

    // A dropped file that cannot be deleted is deleted by a later call.
    let mut run = traced_run(11, 64, small_files(500));
    while run.db.as_ref().unwrap().log.end().unwrap().file() < file_id(3) {
        run.append();
        run.roll();
    }
    let db = run.db.as_mut().unwrap();
    let end = db.log.end().unwrap();
    db.log.sync().unwrap();
    assert_eq!(db.log.drop_before(end).unwrap(), [file_id(1), file_id(2)]);
    run.fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::RemoveFile)
            .at(wal_path(1)),
    );
    let error = db.log.commit(&mut db.manifest).unwrap_err();
    assert!(
        matches!(&error, LogError::NotDeleted { path, .. } if *path == wal_path(1)),
        "{error}"
    );
    assert_eq!(injected(&error), Some(Fault::Eio));
    assert!(std::error::Error::source(&error).is_some());
    // The new manifest is in force, and the other file is gone.
    assert_eq!(db.named()[0].id(), file_id(3));
    assert!(run.fs.open(&wal_path(1), lignin_io::OpenMode::Read).is_ok());
    assert!(run
        .fs
        .open(&wal_path(2), lignin_io::OpenMode::Read)
        .is_err());
    db.log.manifest_replaced(&db.manifest).unwrap();
    assert!(run
        .fs
        .open(&wal_path(1), lignin_io::OpenMode::Read)
        .is_err());
}

/// The steps of one session, the same for every run that starts from the
/// same seed.
fn workload(run: &mut Run, steps: u32) {
    for _ in 0..steps {
        run.step();
    }
}

#[test]
fn a_fault_at_any_operation_of_a_session_leaves_a_log_that_agrees_with_what_was_reported() {
    let options = LogOptions {
        target_file_len: 900,
        sync: SyncPolicy::Interval(std::time::Duration::ZERO),
    };
    let mut clean = traced_run(31, 64, options);
    let first = clean.fs.stats().ops + 1;
    workload(&mut clean, 150);
    let last = clean.fs.stats().ops;
    assert!(last - first > 300, "{first}..{last}");
    let clean_items = clean.model.items.len();

    let mut total = Stats::default();
    let mut effects = std::collections::BTreeMap::new();
    for op in first..=last {
        for (fault, torn) in [
            (Fault::Eio, false),
            (Fault::Eio, true),
            (Fault::Enospc, false),
            (Fault::Enospc, true),
        ] {
            let mut run = traced_run(31, 64, options);
            run.fs
                .add_rule(FaultRule::new(fault).from_op(op).torn_append(torn));
            let mut checked = false;
            for step in 0..150 {
                run.step();
                // Right after the fault, now and then, and at the end.
                let fired = run.fs.stats().faults > 0;
                if (fired && !checked) || step % 16 == 0 {
                    run.check_crash_states();
                    checked = fired;
                }
            }
            if let Some(failed) = sim::failed_ops(&run.fs).first() {
                let Outcome::Injected { effect, .. } = failed.outcome else {
                    panic!("an injected fault");
                };
                let effect = match effect {
                    Effect::Nothing => "nothing",
                    Effect::Wrote(_) => "wrote",
                    Effect::AppendUndone(_) => "append undone",
                    Effect::AppendTorn(_) => "append torn",
                    Effect::SyncFailed(_) => "sync failed",
                };
                *effects.entry((failed.kind, effect)).or_insert(0u32) += 1;
            }
            run.check_crash_states();
            run.restart();
            let read = Run::read(&run.fs).unwrap();
            run.model.check(&read, true).unwrap();
            if run.fs.stats().faults == 0 {
                assert_eq!(run.model.items.len(), clean_items);
            }
            // One fault stops no log for good: the next session appends.
            let before = run.model.items.len();
            run.append();
            assert_eq!(run.model.items.len(), before + 1, "{}", run.at());
            for (sum, part) in [
                (&mut total.states, run.stats.states),
                (&mut total.not_appended, run.stats.not_appended),
                (&mut total.unsettled, run.stats.unsettled),
                (&mut total.sync_failed, run.stats.sync_failed),
                (&mut total.other_errors, run.stats.other_errors),
                (&mut total.restarts, run.stats.restarts),
            ] {
                *sum += part;
            }
        }
    }
    println!("operations {first}..={last}: {total:?}\n{effects:?}");
    assert!(total.states > 60_000, "{total:?}");
    assert!(
        total.not_appended > 100 && total.sync_failed > 50,
        "{total:?}"
    );
    let kinds: std::collections::BTreeSet<OpKind> = effects.keys().map(|(kind, _)| *kind).collect();
    for kind in [
        OpKind::Append,
        OpKind::SyncData,
        OpKind::SyncAll,
        OpKind::SyncDir,
        OpKind::Open,
        OpKind::Rename,
        OpKind::Write,
    ] {
        assert!(kinds.contains(&kind), "{kind}: {effects:?}");
    }
}

#[test]
fn faults_and_crashes_as_they_fall_leave_a_log_that_agrees_with_what_was_reported() {
    let mut total = Stats::default();
    for seed in 1..=36 {
        let fate = [
            UnknownFate::Lost,
            UnknownFate::Kept,
            UnknownFate::Drawn(seed),
        ][seed as usize % 3];
        let options = LogOptions {
            target_file_len: 1500,
            sync: [
                SyncPolicy::Never,
                SyncPolicy::EveryCommit,
                SyncPolicy::Interval(std::time::Duration::ZERO),
            ][seed as usize % 4 % 3],
        };
        let config = SimConfig {
            sector_size: [64, 512, 16][seed as usize % 3],
            unknown: fate,
            faults: FaultOdds {
                eio: Chance::one_in(45),
                enospc: Chance::one_in(70),
                torn_append: Chance::one_in(3),
                ..FaultOdds::NONE
            },
            ..SimConfig::new(500 + seed)
        };
        let mut run = Run::new(config, options, FEW_STATES);
        for step in 0..260 {
            run.step();
            run.check_crash_states();
            match step % 50 {
                24 => run.restart(),
                49 => run.crash(step),
                _ => {}
            }
        }
        run.fs.set_faults(FaultOdds::NONE);
        run.faults = FaultOdds::NONE;
        run.restart();
        let read = Run::read(&run.fs).unwrap();
        run.model.check(&read, true).unwrap();
        let stats = run.stats;
        println!("seed {seed}: {stats:?}");
        for (sum, part) in [
            (&mut total.states, stats.states),
            (&mut total.appended, stats.appended),
            (&mut total.not_appended, stats.not_appended),
            (&mut total.unsettled, stats.unsettled),
            (&mut total.sync_failed, stats.sync_failed),
            (&mut total.other_errors, stats.other_errors),
            (&mut total.restarts, stats.restarts),
            (&mut total.crashes, stats.crashes),
            (&mut total.lost_in_a_crash, stats.lost_in_a_crash),
            (&mut total.turns, stats.turns),
        ] {
            *sum += part;
        }
    }
    println!("{total:?}");
    assert!(
        total.states > 100_000 && total.appended > 3_000,
        "{total:?}"
    );
    assert!(
        total.not_appended > 40 && total.sync_failed > 20,
        "{total:?}"
    );
    assert!(total.unsettled > 0 && total.crashes > 150, "{total:?}");
}

/// A file layer that fails one append after it has written other bytes in
/// the place of the ones it was given, and that says of every file that it
/// has another length when it is told to. And one that, while it is told
/// to, writes what it is given behind the header of a log file and answers
/// that it has no room, or that has no room for the record of a page step
/// in a log file, writes nothing of it, and takes everything else.
#[derive(Debug, Default)]
pub(crate) struct Lies {
    swap: Mutex<Option<Vec<u8>>>,
    len: Mutex<Option<u64>>,
    pub(crate) kept_and_refused: Mutex<bool>,
    pub(crate) steps_refused: Mutex<bool>,
}

#[derive(Debug)]
pub(crate) struct Swapping {
    pub(crate) inner: Arc<dyn Vfs>,
    pub(crate) lies: Arc<Lies>,
}

#[derive(Debug)]
struct SwappingFile {
    inner: Box<dyn VfsFile>,
    lies: Arc<Lies>,
    /// Whether the file is a log file.
    log: bool,
}

impl Vfs for Swapping {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let inner = self.inner.open(path, mode)?;
        let lies = self.lies.clone();
        let log = path.extension().is_some_and(|extension| extension == "log");
        Ok(Box::new(SwappingFile { inner, lies, log }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.inner.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.inner.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.inner.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.inner.lock_existing(path)
    }
}

impl VfsFile for SwappingFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.inner.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        if self.log && *self.lies.kept_and_refused.lock().unwrap() && self.inner.len()? > 0 {
            self.inner.append(buf)?;
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "no room, says the device that wrote the bytes",
            ));
        }
        let step = || {
            use lignin_format::{wal::Record, FormatVersion};
            let record = Record::decode(buf, FormatVersion::CURRENT);
            matches!(record, Ok((Record::Change(_), _)))
        };
        if self.log && *self.lies.steps_refused.lock().unwrap() && step() {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "no room for the record of a page step",
            ));
        }
        let Some(other) = self.lies.swap.lock().unwrap().take() else {
            return self.inner.append(buf);
        };
        self.inner.append(&other)?;
        Err(io::Error::other("the device wrote something else"))
    }

    fn sync_data(&self) -> io::Result<()> {
        self.inner.sync_data()
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn len(&self) -> io::Result<u64> {
        match *self.lies.len.lock().unwrap() {
            Some(len) => Ok(len),
            None => self.inner.len(),
        }
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
}

/// A database on the operating system's files, behind a file layer that
/// lies when it is told to.
fn lied_to() -> (TempDir, Arc<Lies>, Db) {
    let dir = tempfile::tempdir().unwrap();
    let lies = Arc::new(Lies::default());
    let vfs: Arc<dyn Vfs> = Arc::new(Swapping {
        inner: Arc::new(FaultyVfs::without_syncs(Arc::new(StdVfs))),
        lies: lies.clone(),
    });
    let db = Db::create(vfs, dir.path(), LogOptions::default()).unwrap();
    (dir, lies, db)
}

#[test]
fn what_does_not_fit_in_memory_is_an_error_of_the_reader() {
    let (_dir, lies, db) = lied_to();
    db.commit(&mut Rng(42)).unwrap();
    // The file says it is longer than any memory, and the reader is told
    // to fetch all of a file at once.
    *lies.len.lock().unwrap() = Some(u64::MAX);
    let manifest = db.manifest.manifest().unwrap();
    let mut reader = LogReader::new(db.vfs.clone(), &db.dir, manifest).with_window(usize::MAX);
    let error = reader.next_item(|_| ()).unwrap_err();
    // All of the file, or as much as an address of this machine counts.
    let most = (u64::MAX - FIRST).min(usize::MAX as u64);
    assert!(
        matches!(&error, LogError::ItemTooLong { path, offset, len }
            if path.ends_with(log_name(1)) && *offset == FIRST && *len == most),
        "{error}"
    );
    assert!(reader.end().is_none());
}

#[test]
fn an_append_that_leaves_other_bytes_is_settled_by_what_a_recovery_would_take_of_them() {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Left {
        /// Another item of the same length.
        AnItem,
        /// Another item, and behind it bytes that are nothing.
        AnItemAndMore,
        /// Bytes that are nothing.
        Nothing,
    }
    for left in [Left::AnItem, Left::AnItemAndMore, Left::Nothing] {
        let (_dir, lies, mut db) = lied_to();
        let swap = &lies.swap;
        let mut rng = Rng(41);
        let first = db.commit(&mut rng).unwrap();

        // A mark that covers the commit is an item a recovery takes behind
        // it, and it is not what the log appends.
        let other = mark(version(1));
        let appended = match left {
            Left::AnItem => {
                *swap.lock().unwrap() = Some(other.clone());
                db.log.append_mark(CommitVersion::EMPTY)
            }
            Left::AnItemAndMore | Left::Nothing => {
                let at = db.log.next_group().unwrap();
                let bytes = group(&at, &mut rng);
                let mut written = rng.bytes(bytes.len() as u64);
                if left == Left::AnItemAndMore {
                    written[..other.len()].copy_from_slice(&other);
                }
                *swap.lock().unwrap() = Some(written);
                db.log.append_group(&at, &bytes)
            }
        };
        let error = appended.unwrap_err();
        let (read, _) = db.read();
        if left == Left::Nothing {
            // No recovery takes anything from there: the item is not in
            // the log, and the log goes on in its next file.
            assert!(matches!(error, LogError::NotAppended { .. }), "{error}");
            assert_eq!(read, [first]);
            assert!(!db.log.waits_for_sync());
            assert!(matches!(
                db.log.rotate(),
                Err(LogError::CutNotDurable { .. })
            ));
            assert!(db.roll().unwrap());
            assert_eq!(db.named()[0].state(), FileState::Closed);
            continue;
        }
        // A recovery reads on behind the last item the log knows of, so
        // the log cannot say where it ends.
        assert!(
            matches!(error, LogError::Unsettled { .. }),
            "{left:?}: {error}"
        );
        assert_eq!(read.len(), 2, "{left:?}");
        assert_eq!((read[0].clone(), &read[1].bytes), (first, &other));
        assert!(matches!(
            db.log.next_group(),
            Err(LogError::Unsettled { .. })
        ));
        assert!(matches!(db.log.rotate(), Err(LogError::Unsettled { .. })));
    }
}

/// A device that stays full uses up no log file id (D-204): a creation
/// that is refused where the file is made, or where its header is
/// appended, leaves no file, and the creation after it asks for the same
/// one, however often that goes on. With room the file is made under the
/// id that every refused creation had asked for.
#[test]
fn a_creation_that_is_refused_again_and_again_uses_up_no_log_file_id() {
    for op in [OpKind::Open, OpKind::Append] {
        let mut run = traced_run(9, 64, small_files(500));
        while !run.db.as_ref().unwrap().log.is_due() {
            run.append();
        }
        let db = run.db.as_mut().unwrap();
        assert!(db.log.rotate().unwrap());
        let full = FaultRule::new(Fault::Enospc).on(op).at(wal_path(3));
        run.fs.add_rule(full.torn_append(false).forever());
        for _ in 0..40 {
            let error = db.log.create().unwrap_err();
            let said = error.to_string();
            assert!(said.contains("00000003.log"), "{op}: {said}");
            assert!(
                run.fs.open(&wal_path(3), OpenMode::Read).is_err(),
                "{op}: a refused creation left a file"
            );
        }
        assert!(!db.log.file_ids_used_up());
        run.fs.clear_rules();
        assert_eq!(db.log.create().unwrap(), file_id(3));
        db.log.commit(&mut db.manifest).unwrap();
        let last = db.manifest.manifest().unwrap().last_file_ids().wal;
        assert_eq!(last, Some(file_id(3)));
        run.check_crash_states();
        run.append();
        run.restart();
    }
}

/// An id is used up by a file that is found under it, and by nothing
/// else: the creation that finds the name taken fails and leaves the file
/// as it is, and the one after it takes the next id.
#[test]
fn a_log_file_id_is_used_up_by_a_file_that_is_found_under_it() {
    let mut run = traced_run(9, 64, small_files(500));
    while !run.db.as_ref().unwrap().log.is_due() {
        run.append();
    }
    let db = run.db.as_mut().unwrap();
    assert!(db.log.rotate().unwrap());
    put_file(&run.fs, &wal_path(3), b"not the log's");
    let error = db.log.create().unwrap_err();
    let kind = error.io_error().map(io::Error::kind);
    assert_eq!(kind, Some(io::ErrorKind::AlreadyExists), "{error}");
    assert_eq!(file_bytes(&run.fs, &wal_path(3)), b"not the log's");
    assert_eq!(db.log.create().unwrap(), file_id(4));
    db.log.commit(&mut db.manifest).unwrap();
    let last = db.manifest.manifest().unwrap().last_file_ids().wal;
    assert_eq!(last, Some(file_id(4)));
    assert_eq!(file_bytes(&run.fs, &wal_path(3)), b"not the log's");
}

/// After the last id there is none: the creation fails, the log says
/// that its ids are used up, and it goes on taking items in the file it
/// has. The ids do not begin again.
#[test]
fn log_file_ids_run_out_at_the_largest_one_and_do_not_begin_again() {
    let fs = sim::sim_fs(SimConfig::new(3));
    let vfs = sim::shared(&fs);
    let dir = sim::db_dir();
    let last = lignin_format::manifest::LastFileIds {
        wal: FileId::new(FileId::MAX.get() - 1),
        ..Default::default()
    };
    let manifest = Manifest::new(PageSize::DEFAULT, last, None, Vec::new()).expect("a manifest");
    ManifestStore::create(vfs.clone(), dir, manifest).expect("a manifest");
    let mut db = Db::open(vfs, dir, LogOptions::default()).expect("the log");
    assert!(!db.log.file_ids_used_up());

    // The session's first file takes the last id. The file to turn to
    // after it has none.
    let error = db.roll().expect_err("a file after the last id");
    assert!(matches!(error, LogError::FileIdsExhausted), "{error}");
    assert!(db.log.file_ids_used_up());
    let mut rng = Rng(5);
    db.commit(&mut rng)
        .expect("the log takes items in its last file");
    assert_eq!(db.log.end().expect("the end").file(), FileId::MAX);
    for _ in 0..3 {
        let error = db.log.create().expect_err("an id after the last");
        assert!(matches!(error, LogError::FileIdsExhausted), "{error}");
    }
    let entries = fs.read_dir(&FileNames::WAL.dir(dir)).expect("a listing");
    let names: Vec<_> = entries.iter().map(|entry| entry.name.clone()).collect();
    assert_eq!(
        names,
        ["4294967294.log"],
        "a file under an id that began again"
    );
}
