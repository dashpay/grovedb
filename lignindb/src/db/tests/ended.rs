//! OPS-08, D-151 and D-157: the store ends a session, whichever step met
//! what it cannot undo, and for nothing else
//! (`docs/lignindb/disk-full-and-backup.md`, DF8).
//!
//! Two things stay failed in a store until it is opened again: a sync of
//! the log that was refused, and a sync of a segment that was refused. In
//! each test of them here the step that meets one is not a commit and not
//! a flush: a sync of the log as a table checkpoint makes it, a table
//! checkpoint, the creation of a checkpoint of the database. The device
//! has room again right after. The session says that it has ended to
//! whoever asks next, the next commit is refused with its transaction
//! whole, and a close and an open resume with everything that was
//! published.
//!
//! A replacement of the manifest that did not finish does not stay
//! failed: a commit, a close or write-back, whichever comes by next, has
//! the manifest opened again, and the session goes on. A look at the
//! session settles nothing.

use std::{cell::RefCell, error::Error, path::Path, rc::Rc, sync::mpsc, thread, time::Duration};

use lignin_io::{
    sim::{Event, Fault, FaultRule, OpKind, OpSet},
    Vfs,
};
use lignin_store::{
    flush::{Checkpointed, FlushError},
    segment::ManifestStore,
    wal::{LogError, SyncPolicy},
};

use super::{
    full::{stays_failed, Session},
    *,
};
use crate::{
    backup::tests::world::{commit, first, next},
    checkpoint::{CheckpointError, Checkpointer},
    commit::CommitError,
    db::{DbError, Reopen, WriteError},
    space::out_of_space,
};

/// A session on a simulated device with two commits, of which write-back
/// has written the first and the log holds the second alone. Its
/// background work is at rest.
fn session(seed: u64, sync: SyncPolicy) -> Session {
    let mut session = Session::new(seed, sync, true);
    assert!(session.open());
    session.write("w1", first, false);
    assert!(!session.ticks("t1"));
    session.write("w2", next, false);
    assert_eq!(session.told.latest, 3);
    assert_eq!(session.db.as_ref().and_then(Db::reopen), None);
    session
}

/// The file of the store under `dir` that the session wrote to last.
fn written_last(session: &Session, dir: &str) -> String {
    let mut ops = session.fs.trace().into_iter().rev();
    ops.find_map(|event| match event {
        Event::Op(op) if op.kind == OpKind::Append && op.path.contains(dir) => Some(op.path),
        _ => None,
    })
    .unwrap_or_else(|| panic!("nothing was appended under {dir}"))
}

/// What holds of a session that the store has ended for `said`, on a
/// device with room: it says so to whoever asks, readers read on, a commit
/// is refused with its transaction whole, and after a close and an open
/// the next commit is taken, with everything that was published.
fn resumes_after_a_reopen(session: &mut Session, said: &Reopen) {
    let db = session.db.as_ref().expect("the session that has ended");
    assert_eq!(db.reopen().as_ref(), Some(said));
    assert_eq!(db.stats().reopen.as_ref(), Some(said));
    session.check("in the session that has ended");
    session.check_cut("in the session that has ended");

    // A commit is refused with the reason the session gives, and its
    // transaction is whole (`commit` looks at that).
    let published = session.told.latest;
    let db = session.db.as_ref().expect("the session that has ended");
    let refused = commit(db, &next(session.told.state()), true).expect_err("the session has ended");
    assert!(
        matches!(&refused, WriteError::Reopen(reason) if reason == said),
        "{refused:?}"
    );
    assert_eq!(db.version().get(), published);
    assert_eq!(db.reopen().as_ref(), Some(said));
    session.write("refused", next, true);
    assert!(
        session.db.is_none(),
        "the caller closes a session that has ended"
    );
    assert_eq!(session.told.latest, published);
    session.write("taken", next, false);
    assert_eq!(session.told.latest, published + 1);
    let db = session.db.as_ref().expect("the next session");
    assert_eq!(db.reopen(), None);
    assert!(!session.ticks("t"), "write-back works in the next session");
    session.flush();
    assert_eq!(session.told.durable, session.told.latest);
    session.check("in the next session");
    session.check_cut("in the next session");
    assert!(session.close(), "the next session closes");
}

/// Whether nothing is at `path`.
fn gone(session: &Session, path: &str) -> bool {
    session.fs.read_dir(Path::new(path)).is_err()
}

/// The log's sync is refused where the background work makes it: for a
/// table checkpoint, and when it cuts a failed append off. No commit made
/// it, under a policy that asks none of a commit.
#[test]
fn a_sync_of_the_log_that_the_background_work_was_refused_ends_the_session() {
    let mut session = session(31, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    let log = written_last(&session, "/wal/");
    session
        .fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpSet::SYNCS).at(&log));
    let error = db
        .shared
        .write_back()
        .sync_log()
        .expect_err("the sync is refused");
    assert!(out_of_space(&error) && stays_failed(&error), "{error}");
    session.fs.clear_rules();

    let said = db.reopen().expect("the session has ended");
    assert!(
        matches!(&said, Reopen::NotDurable { version, reason }
            if *version == db.version() && reason.contains(&log)),
        "{said:?}"
    );
    assert!(
        said.to_string().contains("a sync of the log failed"),
        "{said}"
    );
    // The first reason stands: a flush, which records its own failed sync
    // on the commit path, does not replace it.
    let flushed = db.flush().expect_err("the log makes nothing durable");
    assert!(matches!(
        &flushed,
        DbError::Flush(FlushError::Log(LogError::SyncFailed { .. }))
    ));
    assert_eq!(db.reopen(), Some(said.clone()));
    resumes_after_a_reopen(&mut session, &said);
}

/// The creation of a checkpoint syncs the log of the database it is of
/// (CK4). Where that sync is refused the creation fails, and so has the
/// session: no hook in the creation says so, the log does.
#[test]
fn a_checkpoint_whose_sync_of_the_log_was_refused_ends_the_session() {
    let mut session = session(32, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    let log = written_last(&session, "/wal/");
    session
        .fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpSet::SYNCS).at(&log));
    let committer = db.shared.committer();
    let error = Checkpointer::new(&committer)
        .create(Path::new("/checkpoint"))
        .expect_err("the sync is refused");
    assert!(matches!(&error, CheckpointError::Sync(_)), "{error}");
    assert!(out_of_space(&error) && stays_failed(&error), "{error}");
    session.fs.clear_rules();
    assert!(gone(&session, "/checkpoint"));

    let said = db.reopen().expect("the session has ended");
    assert!(matches!(&said, Reopen::NotDurable { version, .. } if *version == db.version()));
    // A second creation is refused before it makes anything: the reason
    // is in the commit path's record, which a creation reads.
    let again = Checkpointer::new(&committer)
        .create(Path::new("/checkpoint"))
        .expect_err("the session has ended");
    assert!(matches!(&again, CheckpointError::Stopped(_)), "{again}");
    assert!(gone(&session, "/checkpoint"));
    resumes_after_a_reopen(&mut session, &said);
}

/// A segment's sync is refused when write-back takes a table checkpoint,
/// for as long as the device is full. No table checkpoint is installed in
/// the session any more, so its log would only grow: it has ended,
/// although every commit of it can still be made durable.
#[test]
fn a_sync_of_a_segment_that_write_back_was_refused_ends_the_session() {
    let mut session = session(33, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    let segment = written_last(&session, "/seg/");
    session.fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .on(OpSet::SYNCS)
            .at(&segment)
            .forever(),
    );
    let write_back = db.shared.write_back();
    let error = (write_back.checkpoint()).expect_err("the sync of the segment is refused");
    assert!(out_of_space(&error), "{error}");
    session.fs.clear_rules();

    assert_eq!(db.reopen(), Some(Reopen::SegmentsNotDurable));
    let said = Reopen::SegmentsNotDurable;
    assert!(
        said.to_string().contains("a sync of a segment failed"),
        "{said}"
    );
    // With room, every table checkpoint is answered with what stays
    // failed.
    let again = (write_back.checkpoint()).expect_err("no table checkpoint is installed");
    assert!(matches!(again, FlushError::SegmentsNotDurable), "{again}");
    assert!(!out_of_space(&again) && stays_failed(&again));
    // Nothing that was published is lost by it: a flush makes it durable.
    let flushed = db.flush().expect("the log is synced");
    assert_eq!(flushed.version, db.version());
    session.told.durable = session.told.latest;
    resumes_after_a_reopen(&mut session, &said);
}

/// A session gives one reason for having ended, the first that was seen
/// where it is asked, whatever else has failed by then or fails later: the
/// store's own (a sync of a segment) and the commit path's (here a sync of
/// the log that it was told of), in either order.
#[test]
fn a_session_gives_the_first_reason_it_had() {
    for commits_first in [false, true] {
        let session = session(40, SyncPolicy::Never);
        let db = session.db.as_ref().expect("the session");
        let segment = written_last(&session, "/seg/");
        session.fs.add_rule(
            FaultRule::new(Fault::Enospc)
                .on(OpSet::SYNCS)
                .at(&segment)
                .forever(),
        );
        let write_back = db.shared.write_back();
        (write_back.checkpoint()).expect_err("the sync of the segment is refused");
        session.fs.clear_rules();
        let stop = || {
            let committing = &db.shared.store.committing;
            committing.sync_failed(db.version(), &FlushError::SegmentsNotDurable);
            assert!(
                committing.stopped().is_some(),
                "the commit path has stopped"
            );
        };

        if commits_first {
            stop();
        }
        let said = db.reopen().expect("the session has ended");
        assert_eq!(
            matches!(said, Reopen::NotDurable { .. }),
            commits_first,
            "{said:?}"
        );
        assert_eq!(
            said == Reopen::SegmentsNotDurable,
            !commits_first,
            "{said:?}"
        );
        if !commits_first {
            stop();
        }
        for _ in 0..2 {
            assert_eq!(db.reopen().as_ref(), Some(&said));
            assert_eq!(db.stats().reopen.as_ref(), Some(&said));
            let refused =
                commit(db, &next(session.told.state()), false).expect_err("the session has ended");
            assert!(
                matches!(&refused, WriteError::Reopen(reason) if *reason == said),
                "{refused:?}"
            );
        }
    }
}

/// The commit path stops the session by itself in the moment in which a
/// look has found the store's failed sync of a segment and has not kept
/// its reason yet, and somebody is given the commit path's reason then.
/// It stays the reason: the look that was under way keeps no other.
#[test]
fn a_reason_that_was_given_stays_when_the_store_has_one_too() {
    let session = session(41, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    let segment = written_last(&session, "/seg/");
    session.fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .on(OpSet::SYNCS)
            .at(&segment)
            .forever(),
    );
    let write_back = db.shared.write_back();
    (write_back.checkpoint()).expect_err("the sync of the segment is refused");
    session.fs.clear_rules();

    let given = Rc::new(RefCell::new(None));
    let (shared, version, gives) = (db.shared.clone(), db.version(), given.clone());
    crate::db::store::BEFORE_A_REASON_IS_KEPT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let store = &shared.store;
            (store.committing).sync_failed(version, &FlushError::SegmentsNotDurable);
            *gives.borrow_mut() = store.refusal();
        }));
    });
    let said = db.reopen().expect("the session has ended");
    let given = given.borrow_mut().take().expect("the look was under way");
    assert!(matches!(given, Reopen::NotDurable { .. }), "{given:?}");
    assert_eq!(said, given);
    for _ in 0..2 {
        assert_eq!(db.reopen().as_ref(), Some(&given));
        assert_eq!(db.stats().reopen.as_ref(), Some(&given));
    }
    let refused =
        commit(db, &next(session.told.state()), false).expect_err("the session has ended");
    assert!(
        matches!(&refused, WriteError::Reopen(reason) if *reason == given),
        "{refused:?}"
    );
}

/// The rule that refuses one replacement of the manifest of those that
/// the next table checkpoint makes, the one that `earlier` of them come
/// before: its rename, or the sync of the directory after it.
fn refuse_a_replacement(session: &Session, kind: OpKind, earlier: u64) {
    let rule = FaultRule::new(Fault::Enospc).on(kind);
    session.fs.add_rule(match kind {
        OpKind::Rename => rule.skip(earlier),
        // The directory is synced for the files a table checkpoint makes
        // as well: the sync after the rename of that replacement is the
        // one.
        _ => rule.at("/db").skip(sync_dirs_before_the_rename(earlier)),
    });
}

/// How many times a table checkpoint replaces the manifest before the
/// replacement that installs it, in a session like those of these tests:
/// found in sessions that are made for it, as the first replacement whose
/// refusal the checkpoint answers with an error.
///
/// It is not the first one. A checkpoint that finds a commit in the file
/// the log ends in turns the log where it begins, and replaces the
/// manifest for the file it turns to (`lignin_store::flush`). A refusal
/// there fails no checkpoint of such a session, and the checkpoint
/// returns with the manifest in no doubt: both are asserted here.
fn replacements_before_the_installation() -> u64 {
    for earlier in 0..4 {
        let probe = session(34, SyncPolicy::Never);
        let db = probe.db.as_ref().expect("the session");
        let rule = FaultRule::new(Fault::Enospc).on(OpKind::Rename);
        probe.fs.add_rule(rule.skip(earlier));
        let checkpointed = db.shared.write_back().checkpoint();
        assert_eq!(
            probe.fs.stats().faults,
            1,
            "a table checkpoint replaces the manifest {earlier} times, and fails for none"
        );
        if let Err(error) = checkpointed {
            assert!(out_of_space(&error), "replacement {earlier}: {error}");
            return earlier;
        }
        let manifest = db.shared.store.manifest.lock().expect("the manifest");
        assert!(manifest.manifest().is_ok(), "replacement {earlier}");
    }
    panic!("a table checkpoint replaces the manifest more than four times before it is installed");
}

/// A session of [`session`] whose manifest a table checkpoint has just
/// left in doubt, by a refusal of `kind`, on a device with room again.
fn in_doubt(seed: u64, kind: OpKind) -> Session {
    let session = session(seed, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    // The replacement by which the checkpoint is installed.
    refuse_a_replacement(&session, kind, replacements_before_the_installation());
    let write_back = db.shared.write_back();
    let error = (write_back.checkpoint()).expect_err("the manifest is not replaced");
    assert!(out_of_space(&error), "{kind}: {error}");
    let manifest = db.shared.store.manifest.lock().expect("the manifest");
    assert!(
        manifest.manifest().is_err(),
        "{kind}: the manifest is in no doubt"
    );
    drop(manifest);
    session.fs.clear_rules();
    session
}

/// A table checkpoint that turns the log replaces the manifest for the
/// file it turns to before it replaces it to install itself. Where that
/// earlier replacement is refused, in its rename or in the sync of the
/// directory after it, the checkpoint is installed all the same, the
/// manifest is in no doubt when the checkpoint returns, and the session
/// goes on (DF8a).
#[test]
fn a_table_checkpoint_is_installed_though_its_earlier_replacement_was_refused() {
    let installing = replacements_before_the_installation();
    assert!(
        installing > 0,
        "a table checkpoint of these sessions replaces the manifest once"
    );
    for kind in [OpKind::Rename, OpKind::SyncDir] {
        for earlier in 0..installing {
            let mut session = session(40, SyncPolicy::Never);
            let db = session.db.as_ref().expect("the session");
            refuse_a_replacement(&session, kind, earlier);
            let checkpointed = db.shared.write_back().checkpoint();
            assert_eq!(session.fs.stats().faults, 1, "{kind}, {earlier}");
            session.fs.clear_rules();
            assert!(
                matches!(checkpointed, Ok(Checkpointed::Installed { .. })),
                "{kind}, {earlier}: {checkpointed:?}"
            );
            let manifest = db.shared.store.manifest.lock().expect("the manifest");
            assert!(manifest.manifest().is_ok(), "{kind}, {earlier}");
            drop(manifest);
            assert_eq!(db.reopen(), None, "{kind}, {earlier}");

            let published = session.told.latest;
            session.write("taken", next, false);
            assert_eq!(session.told.latest, published + 1, "{kind}, {earlier}");
            assert!(!session.ticks("t"));
            session.flush();
            assert_eq!(session.told.durable, session.told.latest);
            session.check("after the checkpoint");
            session.check_cut("after the checkpoint");
            assert!(session.close(), "{kind}, {earlier}");
        }
    }
}

/// The replacement of the manifest is refused when write-back installs a
/// table checkpoint: in its rename, or in the sync of the directory after
/// it. Which manifest a crash would leave is not known. That ends no
/// session (D-157). A look at the session says so, settles nothing and
/// makes no operation on a file, under the commit lock as well: the
/// manifest is in doubt as before. The next commit settles the manifest,
/// and is taken. (A creation of a checkpoint that comes first settles it
/// as well: `checkpoint::tests::acceptance`.)
#[test]
fn a_manifest_in_doubt_is_settled_and_ends_no_session() {
    for kind in [OpKind::Rename, OpKind::SyncDir] {
        let mut session = in_doubt(34, kind);
        let db = session.db.as_ref().expect("the session");
        let in_doubt = || {
            let manifest = db.shared.store.manifest.lock().expect("the manifest");
            assert!(manifest.manifest().is_err(), "{kind}");
        };
        in_doubt();

        let ops = session.fs.stats().ops;
        assert_eq!(db.reopen(), None, "{kind}");
        assert_eq!(db.stats().reopen, None, "{kind}");
        let commits = db.shared.store.committing.lock();
        assert_eq!(db.reopen(), None, "{kind}");
        assert_eq!(db.stats().reopen, None, "{kind}");
        drop(commits);
        assert_eq!(
            session.fs.stats().ops,
            ops,
            "{kind}: a look made an operation"
        );
        // The looks have settled nothing.
        in_doubt();

        let published = session.told.latest;
        session.write("taken", next, false);
        assert_eq!(session.told.latest, published + 1, "{kind}");
        let db = session.db.as_ref().expect("the session goes on");
        assert_eq!(db.reopen(), None, "{kind}");
        let committer = db.shared.committer();
        Checkpointer::new(&committer)
            .create(Path::new("/checkpoint"))
            .unwrap_or_else(|error| panic!("{kind}: {error}"));
        assert!(!session.ticks("t"));
        session.flush();
        assert_eq!(session.told.durable, session.told.latest);
        session.check("in the session that went on");
        session.check_cut("in the session that went on");
        assert!(session.close(), "{kind}");
    }
}

/// The creation of a checkpoint is the first to come by, after a table
/// checkpoint that was refused every replacement of the manifest. On a
/// device that still refuses the sync of a directory the manifest cannot
/// be opened again: the creation fails with that error and leaves
/// nothing, the manifest is in doubt as before, and the session has not
/// ended. With room the next creation settles the manifest and is made.
#[test]
fn a_creation_of_a_checkpoint_settles_the_manifest_first() {
    let mut session = session(34, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    // Every rename onto the manifest is refused, so the last replacement
    // that the table checkpoint tries leaves the manifest in doubt.
    let manifest = Path::new("/db").join(ManifestStore::FILE_NAME);
    session.fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .on(OpKind::Rename)
            .at(manifest)
            .forever(),
    );
    let error = (db.shared.write_back().checkpoint()).expect_err("the manifest is not replaced");
    assert!(out_of_space(&error), "{error}");
    session.fs.clear_rules();
    let in_doubt = || {
        let manifest = db.shared.store.manifest.lock().expect("the manifest");
        manifest.manifest().is_err()
    };
    assert!(in_doubt());

    session
        .fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpKind::SyncDir).forever());
    let committer = db.shared.committer();
    let refused = Checkpointer::new(&committer)
        .create(Path::new("/checkpoint"))
        .expect_err("no manifest is opened");
    assert!(matches!(&refused, CheckpointError::Settle(_)), "{refused}");
    assert!(out_of_space(&refused), "{refused}");
    assert!(gone(&session, "/checkpoint") && gone(&session, "/checkpoint.creating"));
    assert!(in_doubt());
    assert_eq!(db.reopen(), None);

    session.fs.clear_rules();
    Checkpointer::new(&committer)
        .create(Path::new("/checkpoint"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(!in_doubt());
    assert!(gone(&session, "/checkpoint.creating"));
    assert_eq!(db.reopen(), None);
    assert!(!session.ticks("t"));
    session.flush();
    assert_eq!(session.told.durable, session.told.latest);
    session.check("in the session that went on");
    session.check_cut("in the session that went on");
    assert!(session.close());
}

/// A commit is the first to come by, on a device that still refuses the
/// sync of a directory: the manifest cannot be opened again. The commit is
/// refused with that error and its transaction whole, and the session has
/// not ended. With room the next commit settles the manifest and is
/// taken.
#[test]
fn a_commit_that_cannot_settle_the_manifest_is_refused_and_can_be_made_again() {
    let mut session = in_doubt(37, OpKind::Rename);
    session
        .fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpKind::SyncDir).forever());
    let published = session.told.latest;
    let db = session.db.as_ref().expect("the session");
    for _ in 0..2 {
        let refused =
            commit(db, &next(session.told.state()), true).expect_err("no manifest is opened");
        assert!(
            matches!(&refused, WriteError::Commit(CommitError::NotReady(_))),
            "{refused:?}"
        );
        assert!(out_of_space(&refused) && refused.reopen().is_none());
        assert_eq!(db.reopen(), None);
    }
    session.write("refused", next, true);
    assert_eq!(session.told.latest, published);
    let answered = session.told.log.last().map(String::as_str);
    assert_eq!(answered, Some("refused!notready"));
    assert!(session.db.is_some(), "the session goes on");
    session.check("with the manifest in doubt");
    session.check_cut("with the manifest in doubt");

    session.fs.clear_rules();
    session.write("taken", next, false);
    assert_eq!(session.told.latest, published + 1);
    assert!(session.db.is_some());
    assert!(!session.ticks("t"));
    session.flush();
    session.check("in the session that went on");
    assert!(session.close());
}

/// A commit looks at the manifest without waiting for it. While somebody
/// holds it, as write-back does over a sync when it replaces it and as
/// whoever settles it does while it is opened again, a commit finds
/// nothing in doubt and is made: the commit itself writes the log, and no
/// manifest. The next commit that finds the manifest free settles it.
#[test]
fn a_commit_waits_for_no_holder_of_the_manifest() {
    let mut session = in_doubt(39, OpKind::Rename);
    let store = session
        .db
        .as_ref()
        .expect("the session")
        .shared
        .store
        .clone();
    let in_doubt = || {
        let manifest = store.manifest.lock().expect("the manifest");
        manifest.manifest().is_err()
    };
    assert!(in_doubt());
    let published = session.told.latest;
    let batch = next(session.told.state());
    let committed = {
        let db = session.db.as_ref().expect("the session");
        let held = store.manifest.lock().expect("the manifest");
        let (made, waits) = mpsc::channel();
        thread::scope(|threads| {
            let batch = &batch;
            threads.spawn(move || {
                let _ = made.send(commit(db, batch, false));
            });
            // The time is no part of what is tested: a commit that waits
            // for the manifest would wait for good.
            let committed = waits.recv_timeout(Duration::from_secs(300));
            drop(held);
            committed.expect("the commit waited for the manifest")
        })
    };
    let mut after = session.told.state().clone();
    after.commit(&batch);
    session.settle("beside", after, committed);
    assert_eq!(session.told.latest, published + 1);
    assert!(
        in_doubt(),
        "a commit that did not wait settled the manifest"
    );
    session.write("settles", next, false);
    assert_eq!(session.told.latest, published + 2);
    assert!(!in_doubt());
    let db = session.db.as_ref().expect("the session goes on");
    assert_eq!(db.reopen(), None);
    session.check("in the session that went on");
    assert!(session.close());
}

/// A close of a session whose manifest is in doubt opens the manifest
/// again first, and closes as any session does. Where the manifest cannot
/// be opened again, the close finishes nothing and answers that error,
/// and not that the directory was removed.
#[test]
fn a_close_settles_a_manifest_in_doubt_first() {
    for refused_again in [false, true] {
        let mut session = in_doubt(38, OpKind::SyncDir);
        if refused_again {
            session
                .fs
                .add_rule(FaultRule::new(Fault::Enospc).on(OpKind::SyncDir).forever());
        }
        let db = session.db.take().expect("the session");
        match (db.finish(), refused_again) {
            (Ok(()), false) => session.told.durable = session.told.latest,
            (Err(error), true) => {
                assert!(matches!(&error, DbError::Flush(_)), "{error:?}");
                assert!(out_of_space(&error) && error.source().is_some(), "{error}");
            }
            (closed, _) => panic!("refused again {refused_again}: {closed:?}"),
        }
        drop(db);
        session.fs.clear_rules();

        // The next open finds out which manifest there is.
        let published = session.told.latest;
        session.write("taken", next, false);
        assert_eq!(session.told.latest, published + 1);
        assert!(!session.ticks("t"));
        session.flush();
        session.check("in the next session");
        session.check_cut("in the next session");
        assert!(session.close());
    }
}

/// How many syncs of the database directory a table checkpoint makes
/// before the rename of the replacement of the manifest that `earlier`
/// replacements come before, in a session like those of these tests:
/// counted in one that is made for it.
fn sync_dirs_before_the_rename(earlier: u64) -> u64 {
    let probe = session(34, SyncPolicy::Never);
    let db = probe.db.as_ref().expect("the session");
    let before = probe.fs.trace().len();
    let write_back = db.shared.write_back();
    write_back
        .checkpoint()
        .expect("a table checkpoint with room");
    let (mut syncs, mut renames) = (0, 0);
    for event in probe.fs.trace().into_iter().skip(before) {
        let Event::Op(op) = event else { continue };
        match op.kind {
            OpKind::Rename if renames == earlier => return syncs,
            OpKind::Rename => renames += 1,
            OpKind::SyncDir if op.path == "/db" => syncs += 1,
            _ => {}
        }
    }
    panic!("the table checkpoint replaced the manifest {renames} times");
}

/// The look waits for no commit. Asked by a thread that holds the commit
/// lock, as the hooks of a commit are, it says what is recorded and
/// records nothing: a commit that is being made records what it meets
/// itself. Asked with the lock free, it records what the store holds.
#[test]
fn the_look_waits_for_no_commit() {
    let mut session = session(36, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    let log = written_last(&session, "/wal/");
    session
        .fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpSet::SYNCS).at(&log));
    let write_back = db.shared.write_back();
    write_back.sync_log().expect_err("the sync is refused");
    session.fs.clear_rules();

    let commits = db.shared.store.committing.lock();
    assert_eq!(db.reopen(), None);
    assert_eq!(db.stats().reopen, None);
    drop(commits);
    let said = db.reopen().expect("the session has ended");
    assert!(matches!(said, Reopen::NotDurable { .. }), "{said:?}");
    resumes_after_a_reopen(&mut session, &said);
}

/// A session that the store has not ended says nothing, however often it
/// is asked, and the look costs no operation on a file.
#[test]
fn a_session_that_goes_on_is_asked_at_no_cost() {
    let session = session(35, SyncPolicy::Never);
    let db = session.db.as_ref().expect("the session");
    let before = session.fs.stats().ops;
    for _ in 0..3 {
        assert_eq!(db.reopen(), None);
        assert_eq!(db.stats().reopen, None);
    }
    assert_eq!(session.fs.stats().ops, before);
}

/// The file that the close of [`session`] turns the log to, read from
/// the trace of a close with no fault: the close makes the file, and
/// appends its header, then its start record, then the mark of the table
/// checkpoint.
fn turned_to_by_a_close(seed: u64) -> String {
    let mut twin = session(seed, SyncPolicy::Never);
    let active = written_last(&twin, "/wal/");
    let from = twin.fs.trace().len();
    assert!(twin.close(), "a close with no fault");
    let appended: Vec<String> = (twin.fs.trace().into_iter().skip(from))
        .filter_map(|event| match event {
            Event::Op(op) if op.kind == OpKind::Append && op.path.contains("/wal/") => {
                Some(op.path)
            }
            _ => None,
        })
        .collect();
    let to = appended.first().expect("the close appends to the log");
    assert_ne!(*to, active, "the close turns the log");
    let appends = appended.iter().filter(|path| *path == to).count();
    assert!(
        appends >= 3,
        "a header, a start record and a mark: {appended:?}"
    );
    to.clone()
}

/// A close turns the log where its table checkpoint begins, and the
/// device refuses the start record of the file it turns to, for room.
/// What a refused append leaves in the file is the device's to say, and
/// each seed here is a device: one leaves nothing of the record and the
/// log stays in the file it was in, another leaves all of it, and then
/// the log has turned and takes no item until it is rolled again. The
/// device has room for everything else. A close of a session that has
/// not ended fails for room or not at all (DF8), and here it has room
/// for all it does after the one refusal, on every device.
#[test]
fn a_close_that_is_refused_the_start_of_a_log_file_fails_for_room_or_not_at_all() {
    use lignin_io::sim::{Effect, Outcome};

    let (mut whole, mut cut) = (0, 0);
    for seed in 35..51 {
        let spare = turned_to_by_a_close(seed);
        let mut session = session(seed, SyncPolicy::Never);
        // The header is the first append to the file, and the start
        // record the second.
        session.fs.add_rule(
            FaultRule::new(Fault::Enospc)
                .on(OpSet::WRITES)
                .at(&spare)
                .skip(1)
                .torn_append(true),
        );
        let closed = session.close();
        let left = session
            .fs
            .trace()
            .into_iter()
            .find_map(|event| match event {
                Event::Op(op) if op.kind == OpKind::Append && op.path == spare => {
                    match op.outcome {
                        Outcome::Injected {
                            effect: Effect::AppendTorn(left),
                            ..
                        } => Some((left, op.len)),
                        _ => None,
                    }
                }
                _ => None,
            });
        let (left, len) = left.unwrap_or_else(|| panic!("seed {seed}: no start record refused"));
        match left == len {
            true => whole += 1,
            false => cut += 1,
        }
        session.fs.clear_rules();
        assert!(
            closed,
            "seed {seed}: a close with room for everything after the refusal, \
             which left {left} of {len} bytes"
        );
        assert!(session.open(), "seed {seed}: the database opens again");
        session.check("opened again");
        assert!(session.close(), "seed {seed}: the next session closes");
    }
    assert!(
        whole > 0 && cut > 0,
        "{whole} devices left the whole record and {cut} less"
    );
}
