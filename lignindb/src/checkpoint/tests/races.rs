//! A creation beside the other actors of `transactions.md` section 13:
//! commits, write-back with its page steps and its table checkpoints, and
//! another creation. Where the order of two steps matters, the test makes
//! it, by letting the other work in at a step of the creation.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use lignin_io::StdVfs;
use lignin_store::{flush::Checkpointed, segment::FileNames};

use super::{
    block, built_in, checkpointer, checkpoints_in, open, remove_block, store_files, store_with_log,
    turn, History, NO_CATCH_UP,
};
use crate::{
    checkpoint::{create::Step, CheckpointOptions},
    txn::tests::{
        model::OwnedBatch,
        put,
        store::{Outcome, Rng},
        World,
    },
};

/// T-CK3, first half: "the cleaner running during creation deletes no file
/// of the file set" (CK3).
///
/// Between the sampling step and the moment the log's files are in the
/// target, the live store commits blocks, turns its log, writes back and
/// installs a table checkpoint that covers them. That makes the table
/// checkpoint of the sample and the log before the new one's place
/// useless to the live store. The file of the old table checkpoint is
/// deleted, and the checkpoint has it already; the log files are kept,
/// because the creation holds the log, and they go with the table
/// checkpoint after that. The version is pinned for as long. The
/// checkpoint opens at the version of its sample.
#[test]
fn nothing_the_checkpoint_samples_goes_before_it_has_it() {
    let (db, versions) = store_with_log(24 << 10, 256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(21);
    for round in 0..6 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    let taken = (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
    assert!(matches!(taken, Checkpointed::Installed { .. }), "{taken:?}");
    for round in 6..9 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    let sampled = versions.latest();
    let captured = world.model.clone();
    let table_before = store_files(&StdVfs, &db.dir)
        .into_iter()
        .find(|file| file.starts_with("table"))
        .expect("a table checkpoint");
    let target = checkpoints_in(&db).join("1");

    let live = Mutex::new((world, rng, 9u64));
    let dropped = [AtomicUsize::new(0), AtomicUsize::new(0)];
    let pause = |step: Step| {
        let mut live = live.lock().expect("the live store");
        let (world, rng, round) = &mut *live;
        match step {
            Step::Made | Step::Waiting | Step::Linking | Step::Settled => return,
            Step::Sampled => {
                assert_eq!(versions.pins_held(), 1, "the version is pinned");
                assert_eq!(db.oldest_pin(&versions), sampled);
            }
            Step::Secured => assert_eq!(versions.pins_held(), 0, "the pin is given back"),
        }
        for _ in 0..4 {
            block(world, rng, *round * 20, 20);
            turn(&db, &versions);
            *round += 1;
        }
        let taken = (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
        let Checkpointed::Installed {
            covers,
            log_files_dropped,
            ..
        } = taken
        else {
            panic!("{taken:?}");
        };
        assert!(covers > sampled);
        let at = match step {
            Step::Sampled => {
                assert!(
                    !db.dir.join(&table_before).exists(),
                    "the table checkpoint of the sample is still the live store's"
                );
                assert!(built_in(&target).join(&table_before).exists());
                assert!(
                    !built_in(&target).join("wal").exists(),
                    "the log is linked after its sync"
                );
                0
            }
            _ => 1,
        };
        dropped[at].store(log_files_dropped, Ordering::Relaxed);
    };
    let created = checkpointer(&db, &versions)
        .with_options(NO_CATCH_UP)
        .pausing(&pause)
        .create(&target)
        .expect("a checkpoint");
    assert_eq!(created.version, sampled);
    let [held, after] = dropped.map(|count| count.load(Ordering::Relaxed));
    assert_eq!(held, 0, "a log file went while the creation held the log");
    assert!(after > 0, "no log file went once the hold was given back");

    let opened = open(&db, &target);
    assert_eq!(opened.versions.latest(), sampled);
    opened.check(&captured);
    let (world, ..) = live.into_inner().expect("the live store");
    world.check_committed();
}

/// T-CK1 and T-CK4, and two creations at once (CK9).
///
/// One thread commits blocks. One does what write-back's loop does: it
/// places the values that the commits carried, each in a page step that
/// appends its record to the log, writes pages back, turns the log and
/// takes table checkpoints. Two threads make checkpoints meanwhile, each
/// one only after a commit has landed since its last. Every checkpoint
/// opens at exactly one version, the one its creation reported, with the
/// state of that version and with counts of live values that agree with
/// its pages, wherever the page steps were when it was sampled.
#[test]
fn checkpoints_beside_commits_and_page_steps_hold_one_version_each() {
    const CHECKPOINTS: usize = 6;
    let (db, versions) = store_with_log(48 << 10, 256);
    let history = Mutex::new(History::default());
    let committed = AtomicU64::new(0);
    let done = AtomicBool::new(false);
    let checkpoints = checkpoints_in(&db);
    let note = |world: &World<'_>| {
        history.lock().expect("the history").note(&world.model);
        committed.fetch_add(1, Ordering::SeqCst);
    };

    // A thread that fails says so to the others, which would wait for it
    // for good: the test then ends with the failure and not in a hang.
    struct Failing<'a>(&'a AtomicBool);
    impl Drop for Failing<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.0.store(true, Ordering::SeqCst);
            }
        }
    }

    let made = std::thread::scope(|scope| {
        let _failing = Failing(&done);
        scope.spawn(|| {
            let _failing = Failing(&done);
            let mut world = World::new(&db, &versions);
            let mut rng = Rng::new(22);
            note(&world);
            let mut round = 0;
            // The writer goes on until every checkpoint is made, and then
            // makes one more block, so that each has a later version.
            while !done.load(Ordering::SeqCst) || round % 2 == 1 {
                block(&mut world, &mut rng, round * 10, 10);
                note(&world);
                if round % 5 == 4 {
                    remove_block(&mut world, (round - 4) * 10, 10);
                    note(&world);
                }
                round += 1;
                assert!(round < 5_000, "the checkpoints are not made");
            }
        });
        scope.spawn(|| {
            let _failing = Failing(&done);
            let write_back = db.write_back(&versions);
            let mut turns = 0u64;
            while !done.load(Ordering::SeqCst) {
                write_back.roll_log().expect("the log is rolled");
                write_back.pass().expect("a pass of write-back");
                turns += 1;
                if turns.is_multiple_of(8) {
                    write_back.checkpoint().expect("a table checkpoint");
                }
                std::thread::yield_now();
            }
        });
        let makers: Vec<_> = (0..2)
            .map(|maker| {
                let (db, versions, committed, checkpoints, done) =
                    (&db, &versions, &committed, &checkpoints, &done);
                scope.spawn(move || {
                    let _failing = Failing(done);
                    let mut made = Vec::new();
                    let mut seen = 0;
                    for index in 0..CHECKPOINTS / 2 {
                        while committed.load(Ordering::SeqCst) <= seen + 2 {
                            assert!(!done.load(Ordering::SeqCst), "another thread failed");
                            std::thread::yield_now();
                        }
                        seen = committed.load(Ordering::SeqCst);
                        let target = checkpoints.join(format!("{maker}-{index}"));
                        let options = match index % 2 {
                            0 => NO_CATCH_UP,
                            _ => CheckpointOptions {
                                catch_up_above: Some(0),
                            },
                        };
                        let created = checkpointer(db, versions)
                            .with_options(options)
                            .create(&target)
                            .expect("a checkpoint");
                        made.push((target, created));
                    }
                    made
                })
            })
            .collect();
        let made: Vec<_> = makers
            .into_iter()
            .flat_map(|maker| maker.join().expect("a maker of checkpoints"))
            .collect();
        done.store(true, Ordering::SeqCst);
        made
    });

    assert_eq!(made.len(), CHECKPOINTS);
    // The values of the blocks were placed while all of this ran, each in
    // a page step whose record is in the log.
    let placed = db.flush.stats().values_placed;
    assert!(placed > 0, "no page step was made");
    assert!(db.log.appended().changes >= placed);
    assert_eq!(versions.pins_held(), 0);
    let history = history.into_inner().expect("the history");
    let mut versions_made = Vec::new();
    for (target, created) in &made {
        let opened = open(&db, target);
        assert_eq!(opened.versions.latest(), created.version);
        assert_eq!(opened.report.landed, created.version);
        opened.check(history.at(created.version));
        versions_made.push(created.version);
        // Its log ends at the end of a record: the store read it to the
        // length the manifest gives, and found whole items to there.
        assert!(created.log_end.is_some());
    }
    versions_made.sort_unstable();
    assert!(
        versions_made.first() < versions_made.last(),
        "every checkpoint is of one version: {versions_made:?}"
    );
    // The live store is whole after all of it.
    let latest = versions.latest();
    let live = World {
        model: history.at(latest).clone(),
        ..World::new(&db, &versions)
    };
    live.check_committed();
}

/// A checkpoint is made of a store whose log has just turned to a new
/// file, and of one whose log ends behind a commit. The turn made the log
/// durable, so the first kind's log ends where the log was durable to, at
/// the end of the file it turned away from, and the new file is not the
/// checkpoint's. The second kind's log ends where the live log did. In
/// both, the file the sample ends in is named closed where the sample
/// ended, whatever is appended to it afterwards.
#[test]
fn the_log_of_a_checkpoint_ends_where_the_sample_ended() {
    let (db, versions) = store_with_log(1 << 10, 128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(23);
    let checkpoints = checkpoints_in(&db);
    let mut made = Vec::new();
    for round in 0..4u64 {
        block(&mut world, &mut rng, round * 10, 10);
        // Every block is longer than a log file is to be, so the log is
        // due to turn after each.
        if round % 2 == 0 {
            let turned = db.write_back(&versions).roll_log();
            assert!(turned.expect("the log is rolled"), "the log did not turn");
        }
        let (durable, live_end) = (db.log.durable(), db.log.end().expect("an end"));
        let synced = durable.version == versions.latest();
        assert_eq!(synced, round % 2 == 0, "round {round}: {durable:?}");
        let target = checkpoints.join(round.to_string());
        let created = checkpointer(&db, &versions)
            .with_options(NO_CATCH_UP)
            .create(&target)
            .expect("a checkpoint");
        let end = created.log_end.expect("the log has an end");
        match synced {
            // The file the log turned to is not in the checkpoint.
            true => {
                assert_eq!(Some(end), durable.place);
                assert!(end.file() < live_end.file(), "{end:?}, {live_end:?}");
            }
            false => assert_eq!(end, live_end),
        }
        made.push((target, created.version, world.model.clone(), end));
    }
    for (target, version, model, end) in &made {
        let manifest = lignin_store::segment::read_manifest(&StdVfs, target).expect("a manifest");
        let last = (manifest.files().iter())
            .rfind(|file| file.kind() == lignin_format::FileKind::Wal)
            .expect("a log file");
        assert_eq!((last.id(), last.valid_len()), (end.file(), end.offset()));
        let live_len =
            std::fs::metadata(FileNames::WAL.path(&db.dir, end.file())).map(|file| file.len());
        if let Ok(live_len) = live_len {
            assert!(live_len >= end.offset());
        }
        let opened = open(&db, target);
        assert_eq!(opened.versions.latest(), *version);
        opened.check(model);
    }
}

/// "A commit never waits for the fsync" (CK5): where the log is durable
/// through the version already, a creation syncs no file of it, so no
/// commit that syncs the log itself finds a sync of the creation's under
/// way. That is so after every commit where each commit syncs the log, and
/// after a flush where none does.
///
/// The log has records of page steps behind the place it is durable to.
/// No file of the log can be synced while the checkpoint is made: a
/// creation that tried would fail. Its log ends where the log was durable
/// to, and it opens with the state of its version, the values that the
/// page steps had placed among it.
#[test]
fn a_creation_syncs_no_log_that_is_durable_through_its_version() {
    use std::{path::Path, sync::Arc};

    use lignin_io::{
        sim::{Fault, FaultRule, OpKind, OpSet},
        Vfs,
    };
    use lignin_store::{
        flush::FlushConfig,
        wal::{LogOptions, SyncPolicy},
    };

    use super::{open_on, sim, DIR};
    use crate::txn::tests::store::{Db, SIZE};

    for sync in [SyncPolicy::EveryCommit, SyncPolicy::Never] {
        let fs = sim(24);
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let options = LogOptions {
            sync,
            ..LogOptions::default()
        };
        let (db, versions) = Db::on(
            vfs.clone(),
            Path::new(DIR),
            256,
            options,
            FlushConfig::new(SIZE),
            None,
        );
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(24);
        for round in 0..3 {
            block(&mut world, &mut rng, round * 10, 10);
        }
        let write_back = db.write_back(&versions);
        if sync == SyncPolicy::Never {
            // What a flush does.
            write_back.sync_log().expect("the log is synced");
        }
        let durable = db.log.durable();
        assert_eq!(durable.version, versions.latest(), "{sync:?}");
        let placed = db.flush.stats().values_placed;
        write_back.pass().expect("a pass of write-back");
        assert!(db.flush.stats().values_placed > placed, "{sync:?}");
        assert!(
            durable.place < db.log.end(),
            "{sync:?}: no record of a page step"
        );
        assert_eq!(
            db.log.durable(),
            durable,
            "{sync:?}: the pass synced the log"
        );

        let target = checkpoints_in(&db).join("1");
        let syncs = OpSet::of(&[OpKind::SyncData, OpKind::SyncAll]);
        for file in store_files(&fs, Path::new(DIR)) {
            if file.starts_with("wal") {
                let rule = FaultRule::new(Fault::Eio).on(syncs).forever();
                fs.add_rule(rule.at(Path::new(DIR).join(file)));
            }
        }
        let created = checkpointer(&db, &versions)
            .with_options(NO_CATCH_UP)
            .create(&target);
        fs.clear_rules();
        let created = created.unwrap_or_else(|error| panic!("{sync:?}: {error}"));
        assert_eq!(created.version, versions.latest());
        assert_eq!(created.log_end, durable.place, "{sync:?}");
        let opened = open_on(vfs, &target, &db).expect("the checkpoint opens");
        assert_eq!(opened.versions.latest(), created.version);
        opened.check(&world.model);
    }
}

/// "Sampling is one step ... while holding the commit lock" (CK2): a
/// creation takes no sample for as long as a commit has the commit lock,
/// which a commit holds from before its group is in the log until its
/// version is published.
#[test]
fn a_creation_samples_under_the_commit_lock() {
    let (db, versions) = store_with_log(48 << 10, 128);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(25), 0, 20);
    let target = checkpoints_in(&db).join("1");
    let (made, sampled) = (AtomicBool::new(false), AtomicBool::new(false));
    let pause = |step: Step| match step {
        Step::Made => made.store(true, Ordering::SeqCst),
        Step::Linking => sampled.store(true, Ordering::SeqCst),
        _ => {}
    };
    std::thread::scope(|scope| {
        let commits = db.committing.lock();
        let maker = scope.spawn(|| checkpointer(&db, &versions).pausing(&pause).create(&target));
        wait_until("the target is made", || made.load(Ordering::SeqCst));
        // The creation is at its sample now, and stays there. A creation
        // that took none of the commit lock would be past it in far less
        // than this, unless its thread got no processor for as long: then
        // this would pass for it. What the sample reads takes the lock as
        // an argument, so it is not left out by a slip.
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            !sampled.load(Ordering::SeqCst),
            "a sample was taken while a commit had the commit lock"
        );
        drop(commits);
        let created = maker.join().expect("the maker of the checkpoint");
        created.expect("a checkpoint");
    });
    assert!(sampled.load(Ordering::SeqCst));
    open(&db, &target).check(&world.model);
}

/// Waits until `met`, and gives up after a minute: a test that would hang
/// fails.
fn wait_until(what: &str, met: impl Fn() -> bool) {
    let began = Instant::now();
    while !met() {
        assert!(began.elapsed() < Duration::from_secs(60), "{what}");
        std::thread::yield_now();
    }
}

/// "Commits go on after sampling" (CK5): while the creation links files,
/// with the lock on the manifest held, a commit is made and published. It
/// is not in the checkpoint, though the log is durable through it before
/// the log's files are linked: the file the sample ended in is named at
/// the sampled end, and not as far as it is durable.
///
/// Once the log is not durable through the sampled version, so that the
/// creation's own sync covers the later commit, and once it is, so that
/// the creation syncs nothing and a flush covers it.
#[test]
fn a_commit_is_published_while_the_files_are_linked() {
    for synced in [false, true] {
        a_commit_is_published_while_a_creation_links(synced);
    }
}

fn a_commit_is_published_while_a_creation_links(synced: bool) {
    let (db, versions) = store_with_log(24 << 10, 256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(24);
    for round in 0..4 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    match synced {
        true => drop(db.write_back(&versions).sync_log().expect("a flush")),
        false => drop(block(&mut world, &mut rng, 4 * 20, 20)),
    }
    let sampled = versions.latest();
    assert_eq!(db.log.durable().version == sampled, synced);
    let captured = world.model.clone();
    let target = checkpoints_in(&db).join("1");
    let (go, linked) = (AtomicBool::new(false), AtomicBool::new(false));

    let (created, world) = std::thread::scope(|scope| {
        let committer = scope.spawn(|| {
            wait_until("the creation never linked", || go.load(Ordering::SeqCst));
            block(&mut world, &mut rng, 5 * 20, 20);
            world
        });
        let pause = |step: Step| match step {
            Step::Linking => {
                assert!(db.manifest.try_lock().is_err(), "the manifest is free");
                let seg = built_in(&target).join("seg");
                assert!(!seg.exists(), "a file is linked already");
                go.store(true, Ordering::SeqCst);
                wait_until(
                    "no commit was published while the files were linked",
                    || versions.latest() > sampled,
                );
                linked.store(true, Ordering::SeqCst);
            }
            // What a flush does, before the log's files are linked.
            Step::Sampled if synced => drop(db.write_back(&versions).sync_log().expect("a flush")),
            _ => {}
        };
        let created = checkpointer(&db, &versions)
            .with_options(NO_CATCH_UP)
            .pausing(&pause)
            .create(&target)
            .expect("a checkpoint");
        (created, committer.join().expect("the committer"))
    });
    assert!(linked.load(Ordering::SeqCst));
    assert_eq!(created.version, sampled);
    assert_eq!(versions.latest(), sampled.next().expect("a version"));
    // The log was durable through the later commit when its files were
    // linked, and the checkpoint's log ends before it.
    let durable = db.log.durable();
    assert_eq!(durable.version, versions.latest(), "synced: {synced}");
    assert!(created.log_end < durable.place, "synced: {synced}");
    let opened = open(&db, &target);
    assert_eq!(opened.versions.latest(), sampled);
    opened.check(&captured);
    world.check_committed();
}

/// A creation that finds the manifest locked, as write-back holds it over
/// the syncs of a replacement, waits for it with no commit kept out (CK5,
/// CM8): a commit is made while it waits. The sample is taken when the
/// manifest is free, so the commit is in the checkpoint.
#[test]
fn a_creation_that_waits_for_the_manifest_keeps_no_commit_waiting() {
    let (db, versions) = store_with_log(24 << 10, 256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(25);
    for round in 0..3 {
        block(&mut world, &mut rng, round * 20, 20);
        // Nothing is left for a committer to place, so its own turn for
        // write-back asks for no manifest.
        turn(&db, &versions);
    }
    let before = versions.latest();
    let target = checkpoints_in(&db).join("1");
    let (made, locked, waiting) = (
        AtomicBool::new(false),
        AtomicBool::new(false),
        AtomicBool::new(false),
    );
    let pause = |step: Step| match step {
        // The manifest is locked before the creation samples.
        Step::Made => {
            made.store(true, Ordering::SeqCst);
            wait_until("the manifest is never locked", || {
                locked.load(Ordering::SeqCst)
            });
        }
        Step::Waiting => waiting.store(true, Ordering::SeqCst),
        _ => {}
    };

    let created = std::thread::scope(|scope| {
        let maker = scope.spawn(|| {
            checkpointer(&db, &versions)
                .with_options(NO_CATCH_UP)
                .pausing(&pause)
                .create(&target)
        });
        wait_until("the target is never made", || made.load(Ordering::SeqCst));
        let held = db.manifest.lock().expect("the manifest");
        locked.store(true, Ordering::SeqCst);
        wait_until("the creation does not wait for the manifest", || {
            waiting.load(Ordering::SeqCst)
        });
        let batch = OwnedBatch {
            records: vec![put(super::aux(), b"meanwhile", b"committed")],
            ..OwnedBatch::default()
        };
        let outcome = world.write_outside(&batch, &[]);
        assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
        assert!(!built_in(&target).join("MANIFEST").exists() && !target.exists());
        drop(held);
        maker.join().expect("the maker").expect("a checkpoint")
    });
    assert_eq!(created.version, before.next().expect("a version"));
    let opened = open(&db, &target);
    opened.check(&world.model);
}
