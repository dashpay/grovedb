//! `Db::checkpoint`: the facade's call that makes a checkpoint of an open
//! database (D-242). What a checkpoint is and what its creation keeps is
//! tested in `crate::checkpoint`; here the call is made as a caller of the
//! engine makes it.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use lignin_format::CommitVersion;
use lignin_io::sim::{Event, Fault, FaultRule, OpKind, OpSet, SimConfig, SimVfs};
use lignin_store::{
    flush::{Checkpointed, FlushError},
    wal::SyncPolicy,
};

use super::{full::Session, *};
use crate::{
    backup::tests::world::{check_reads, first, next},
    checkpoint::{self, CheckpointError, CheckpointOptions, CREATING_SUFFIX},
    commit::CommitError,
    db::{Reopen, WriteError},
    space::out_of_space,
};

/// What the database at `dir` holds of each of `subtrees`, opened as a
/// database of its own and closed again, with the version it opened at.
fn read<const N: usize>(dir: &Path, subtrees: [Subtree; N]) -> (CommitVersion, [Option<Items>; N]) {
    let opened = Db::open(dir, &options()).unwrap_or_else(|error| panic!("{error}"));
    assert!(!opened.created(), "the open made a database");
    let version = opened.version();
    let held = {
        let snapshot = opened.snapshot();
        let operation = snapshot.operation();
        subtrees.map(|subtree| items_of(&operation, subtree))
    };
    opened.close().expect("the checkpoint closes");
    (version, held)
}

/// A session on a simulated device, with two application commits after
/// initialization and its background work at rest.
fn session(seed: u64) -> Session {
    let mut session = Session::new(seed, SyncPolicy::Never, true);
    assert!(session.open());
    session.write("w1", first, false);
    assert!(!session.ticks("t1"));
    session.write("w2", next, false);
    session
}

/// A checkpoint that the facade makes is a database of its own: it opens
/// at the version the call returned, with what the database held then,
/// and a commit that is made in the live database afterwards is not in
/// it. In this test, with write-back at rest and no open write
/// transaction, `Some(0)` installs a table checkpoint covering the
/// sampled version. With `None`, the call skips catch-up. And the call
/// refuses a target that is there, and one whose name ends in the suffix
/// reserved for checkpoint creation, as a creation does.
#[test]
fn the_facade_makes_a_checkpoint_that_opens_as_the_database_of_its_version() {
    let dir = temp();
    // Nothing but the calls below writes the log back.
    let db = open_at_rest(dir.path().join("live"), &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
    let held = items_of(&db.snapshot().operation(), subtree(1)).expect("the subtree");
    let target = dir.path().join("checkpoint");

    let created = db
        .checkpoint(&target, CheckpointOptions::default())
        .unwrap_or_else(|error| panic!("the checkpoint: {error}"));
    assert_eq!(created.version, db.version());
    assert!(created.linked > 0 && created.copied == 0, "{created:?}");
    let built = dir.path().join(format!("checkpoint{CREATING_SUFFIX}"));
    assert!(target.is_dir() && !built.exists());

    plant(&db, subtree(2), Merk::sized(b'l', 4, 20));
    assert!(db.version() > created.version);
    assert_eq!(
        read(&target, [subtree(1), subtree(2)]),
        (created.version, [Some(held), None])
    );

    let log_held = || db.stats().store.watch.log_held;
    let catching_up = CheckpointOptions {
        catch_up_above: Some(0),
    };
    assert!(log_held() > 0);
    let second = dir.path().join("second");
    let created = db
        .checkpoint(&second, catching_up)
        .unwrap_or_else(|error| panic!("the second checkpoint: {error}"));
    assert_eq!(created.version, db.version());
    assert!(
        matches!(
            &created.catch_up,
            Some(Ok(Checkpointed::Installed { covers, .. })) if *covers == created.version
        ),
        "{created:?}"
    );
    let (version, [later]) = read(&second, [subtree(2)]);
    assert!(version == created.version && later.is_some());

    plant(&db, subtree(3), Merk::sized(b'm', 4, 20));
    let no_catch_up = CheckpointOptions {
        catch_up_above: None,
    };
    assert!(log_held() > 0);
    let third = dir.path().join("third");
    let created = db
        .checkpoint(&third, no_catch_up)
        .unwrap_or_else(|error| panic!("the third checkpoint: {error}"));
    assert_eq!(created.version, db.version());
    assert!(created.catch_up.is_none(), "{created:?}");

    let refused = db.checkpoint(&target, CheckpointOptions::default());
    assert!(
        matches!(&refused, Err(CheckpointError::TargetExists { .. })),
        "{refused:?}"
    );
    let refused = db.checkpoint(&built, CheckpointOptions::default());
    assert!(
        matches!(&refused, Err(CheckpointError::ReservedName { .. })),
        "{refused:?}"
    );
    assert!(!built.exists());
    assert_eq!(db.reopen(), None);
}

/// Two threads each make three checkpoints while the database's
/// background workers are enabled. Before each call, the maker waits for
/// the writer's published-round counter to advance. Each checkpoint is
/// then opened as a database of its own: it lands on the version its call
/// returned, holds the commit of that version, and does not hold the
/// commit after it. The database counts no error of its background work,
/// and its session has not ended.
#[test]
fn the_facade_makes_checkpoints_beside_commits_and_the_background_work() {
    const MAKERS: u32 = 2;
    const EACH: u32 = 3;

    let dir = temp();
    let db = Db::open(dir.path().join("live"), &options()).expect("a new database");
    // The commit of round `n` enters the subtree `numbered(n)`, and its
    // version is `base + n`: the writer is the one thread that commits.
    let base = plant(&db, numbered(0), Merk::sized(b'k', 6, 40)).version;
    let (done, committed) = (AtomicBool::new(false), AtomicU64::new(0));
    let made = thread::scope(|scope| {
        // Leaving the scope body signals the writer to stop, including on
        // unwind.
        let _ending = Ending(&done);
        scope.spawn(|| {
            let mut round = 0;
            while !done.load(Ordering::SeqCst) {
                round += 1;
                let published = plant(&db, numbered(round), Merk::sized(b'k', 6, 40));
                assert_eq!(published.version.get(), base.get() + u64::from(round));
                committed.store(u64::from(round), Ordering::SeqCst);
            }
        });
        let makers: Vec<_> = (0..MAKERS)
            .map(|maker| {
                let (db, dir, committed) = (&db, &dir, &committed);
                scope.spawn(move || {
                    let mut made = Vec::new();
                    for number in 0..EACH {
                        let before = committed.load(Ordering::SeqCst);
                        wait_for("a commit beside the checkpoints", || {
                            committed.load(Ordering::SeqCst) > before
                        });
                        let target = dir.path().join(format!("checkpoint-{maker}-{number}"));
                        let created = db
                            .checkpoint(&target, CheckpointOptions::default())
                            .unwrap_or_else(|error| panic!("checkpoint {maker}-{number}: {error}"));
                        made.push((target, created.version));
                    }
                    made
                })
            })
            .collect();
        makers
            .into_iter()
            .flat_map(|maker| maker.join().expect("a maker of checkpoints"))
            .collect::<Vec<_>>()
    });
    assert_eq!(made.len(), (MAKERS * EACH) as usize);
    let latest = db.version();
    for (target, version) in &made {
        let at = target.display();
        assert!(*version > base && *version <= latest, "{at}: {version}");
        let round = u32::try_from(version.get() - base.get()).expect("a round");
        let (landed, [its_own, later]) = read(target, [numbered(round), numbered(round + 1)]);
        assert_eq!(landed, *version, "{at}");
        assert!(its_own.is_some(), "{at}: the commit of its version");
        assert!(later.is_none(), "{at}: the commit after its version");
    }
    let stats = db.stats();
    assert_eq!(stats.reopen, None);
    let background = stats.background;
    assert_eq!(
        (
            background.write_back_errors,
            background.reclaim_errors,
            background.cleaner_errors
        ),
        (0, 0, 0),
        "{background:?}"
    );
    db.close().expect("the live database closes");
    for (target, _) in &made {
        checkpoint::delete(&StdVfs, target).expect("the checkpoint is deleted");
        assert!(!target.exists());
    }
}

/// In a session whose commit path has stopped, the call is refused with
/// the commit path's reason, and makes no call to the file layer: after a
/// commit of which the log cannot say whether it holds it
/// ([`Reopen::Stopped`]), and after the test records
/// `Stopped::NotDurable` directly on the commit path
/// ([`Reopen::NotDurable`]).
#[test]
fn the_facade_refuses_a_checkpoint_where_the_commit_path_has_stopped() {
    for unsettled in [true, false] {
        let fs = SimVfs::new(SimConfig::new(61));
        let options = Options {
            vfs: Arc::new(fs.clone()),
            sync: SyncPolicy::Never,
            ..options()
        };
        let db = Db::open("/db", &options).expect("a new database");
        plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
        halt_background(&db);
        let committing = &db.shared.store.committing;
        if unsettled {
            // The append of a commit fails, and so does the sync that
            // would settle whether its group is in the log.
            fs.add_rule(
                FaultRule::new(Fault::Eio)
                    .on(OpKind::Append)
                    .torn_append(false),
            );
            fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
            let writes = Writes {
                records: vec![(Column::Aux, subtree(1), b"a".to_vec(), vec![7; 3000])],
                ..Writes::default()
            };
            let ended = writes.with(|batch| db.write(batch, &mut NoStaleHashes));
            fs.clear_rules();
            assert!(
                matches!(
                    &ended,
                    Err(WriteError::Commit(CommitError::Unsettled { .. }))
                ),
                "{ended:?}"
            );
        } else {
            committing.sync_failed(db.version(), &FlushError::SegmentsNotDurable);
        }
        let stopped = committing.stopped().expect("the commit path has stopped");
        let said = db.reopen().expect("the session has ended");
        assert_eq!(matches!(said, Reopen::Stopped(_)), unsettled, "{said:?}");
        assert_eq!(
            matches!(said, Reopen::NotDurable { .. }),
            !unsettled,
            "{said:?}"
        );

        let ops = fs.stats().ops;
        let refused = db.checkpoint(Path::new("/checkpoint"), CheckpointOptions::default());
        assert!(
            matches!(&refused, Err(CheckpointError::Stopped(reason)) if *reason == stopped),
            "{said:?}: {refused:?}"
        );
        assert_eq!(fs.stats().ops, ops, "{said:?}: the call made a call");
        assert_eq!(db.reopen(), Some(said));
    }
}

/// A session that ended for a sync of a segment that failed
/// ([`Reopen::SegmentsNotDurable`]), and whose commit path has not
/// stopped, is not refused a checkpoint: the call returns one at the
/// latest version, and an open of it reads what the session had
/// published.
#[test]
fn the_facade_makes_a_checkpoint_of_a_session_that_ended_for_a_segment() {
    let session = session(51);
    let db = session.db.as_ref().expect("the session");
    let segment = (session.fs.trace().into_iter().rev())
        .find_map(|event| match event {
            Event::Op(op) if op.kind == OpKind::Append && op.path.contains("/seg/") => {
                Some(op.path)
            }
            _ => None,
        })
        .expect("a segment that was appended to");
    session.fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .on(OpSet::SYNCS)
            .at(&segment)
            .forever(),
    );
    (db.shared.write_back().checkpoint()).expect_err("the sync of the segment is refused");
    session.fs.clear_rules();
    assert_eq!(db.reopen(), Some(Reopen::SegmentsNotDurable));
    assert_eq!(db.shared.store.committing.stopped(), None);

    let created = db
        .checkpoint(Path::new("/checkpoint"), CheckpointOptions::default())
        .unwrap_or_else(|error| panic!("the checkpoint: {error}"));
    assert_eq!(created.version, db.version());
    assert_eq!(db.reopen(), Some(Reopen::SegmentsNotDurable));

    let options = Options {
        create_if_missing: false,
        ..session.options.clone()
    };
    let opened = open_at_rest("/checkpoint", &options)
        .unwrap_or_else(|error| panic!("the checkpoint opens: {error}"));
    assert_eq!(opened.version(), created.version);
    check_reads(&opened, session.told.state(), "the checkpoint");
}

/// If the device refuses creation of the building directory, the caller
/// receives the error and the background error counts are unchanged. The
/// session has not ended, and once the test refuses nothing any more the
/// call makes the checkpoint.
#[test]
fn an_error_of_the_call_is_returned_and_counted_as_no_error_of_the_background_work() {
    let session = session(52);
    let db = session.db.as_ref().expect("the session");
    let before = db.stats().background;
    session
        .fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpKind::CreateDir));
    let refused = db
        .checkpoint(Path::new("/checkpoint"), CheckpointOptions::default())
        .expect_err("the directory is not made");
    session.fs.clear_rules();
    assert!(
        matches!(&refused, CheckpointError::Io { path, .. }
            if path == Path::new("/checkpoint.creating"))
            && out_of_space(&refused),
        "{refused}"
    );
    assert_eq!(db.stats().background, before);
    assert_eq!(db.reopen(), None);
    let listed = |path: &str| session.fs.read_dir(Path::new(path)).is_ok();
    assert!(!listed("/checkpoint") && !listed("/checkpoint.creating"));

    let created = db
        .checkpoint(Path::new("/checkpoint"), CheckpointOptions::default())
        .unwrap_or_else(|error| panic!("the checkpoint: {error}"));
    assert_eq!(created.version, db.version());
    assert_eq!(db.stats().background, before);
    assert!(listed("/checkpoint") && !listed("/checkpoint.creating"));
}
