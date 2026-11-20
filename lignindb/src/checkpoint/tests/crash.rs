//! The power is cut, and the file layer fails, at every operation of a
//! creation (CK4, CK10; T-DU1 for a checkpoint).
//!
//! The file layer is the simulated one (`lignin_io::sim`). A crash keeps,
//! loses or tears each change that was not synced, as a plan says: both
//! ends, what a file system that writes in order leaves, one change lost
//! at a time, and drawn samples. Each state is opened twice, as the live
//! store and as the checkpoint, by the store's recovery. `opener.rs` opens
//! such states with the engine's own open.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::CommitVersion;
use lignin_io::{
    sim::{CrashImage, CrashPlan, CrashPoint, Fault, FaultRule, OpKind, OpSet, SimVfs},
    Vfs,
};
use lignin_store::{
    flush::{Checkpointed, FlushConfig},
    wal::LogOptions,
};

use super::{
    block, building, checkpointer, checkpoints_in, open_on, sim, store_files, History, Meddling,
    DIR, NO_CATCH_UP,
};
use crate::{
    checkpoint::{self, create::Step, CheckpointError, CheckpointOptions},
    snapshot::Versions,
    txn::tests::{
        store::{Db, Rng, SIZE},
        World,
    },
};

/// How the files of a checkpoint come into its directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Files {
    Linked,
    /// No link can be made into the directory of the checkpoints, as when
    /// it is on another file system.
    Copied,
}

/// A store with what a creation samples.
struct Made {
    fs: SimVfs,
    db: Arc<Db>,
    versions: Versions,
    history: History,
    /// The version the log is durable through before the creation.
    durable: CommitVersion,
    target: PathBuf,
}

/// A store on a simulated file system, the same for the same `seed`: two
/// blocks under a table checkpoint, which made the log durable through
/// them, one block that was written back since, and two that the log has
/// and no sync has covered.
fn made(seed: u64, files: Files) -> Made {
    let fs = sim(seed);
    let plain: Arc<dyn Vfs> = Arc::new(fs.clone());
    let vfs: Arc<dyn Vfs> = match files {
        Files::Linked => plain,
        Files::Copied => Arc::new(Meddling::no_links(
            plain,
            Path::new(DIR).join("checkpoints"),
            ErrorKind::CrossesDevices,
        )),
    };
    let (options, flush) = (LogOptions::default(), FlushConfig::new(SIZE));
    let (db, versions) = Db::on(vfs, Path::new(DIR), 256, options, flush, None);
    let mut history = History::default();
    {
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(seed);
        history.note(&world.model);
        for round in 0..5 {
            block(&mut world, &mut rng, round * 8, 8);
            history.note(&world.model);
            let write_back = db.write_back(&versions);
            match round {
                1 => {
                    let taken = write_back.checkpoint().expect("a table checkpoint");
                    assert!(matches!(taken, Checkpointed::Installed { .. }), "{taken:?}");
                }
                2 => drop(write_back.pass().expect("a pass of write-back")),
                _ => {}
            }
        }
    }
    let durable = db.log.durable().version;
    assert!(durable.get() >= 3 && durable < versions.latest());
    let target = checkpoints_in(&db).join("1");
    Made {
        fs,
        db,
        versions,
        history,
        durable,
        target,
    }
}

/// Some of the states a crash can leave of `image`.
pub(super) fn plans(image: &CrashImage) -> Vec<CrashPlan> {
    fn some(plans: impl Iterator<Item = CrashPlan>) -> impl Iterator<Item = CrashPlan> {
        let plans: Vec<CrashPlan> = plans.collect();
        let stride = plans.len().div_ceil(6).max(1);
        plans.into_iter().step_by(stride)
    }
    let mut plans = vec![image.lose_all(), image.keep_all()];
    plans.extend(some(image.prefixes()));
    plans.extend(some(image.holes()));
    plans.extend((0..8).map(|draw| image.sample(draw)));
    plans
}

/// What a crash left: how many of its states had the checkpoint at the
/// target, how many had nothing there, and in how many of them something
/// was left under the name the checkpoint was built under.
#[derive(Debug, Default)]
struct Left {
    whole: usize,
    none: usize,
    built: usize,
}

impl Made {
    /// Opens each state that a crash now can leave, as the checkpoint and
    /// as the live store, and checks both. `created` says whether the
    /// creation had returned a checkpoint before the crash.
    fn check_crash(&self, created: bool, context: &str, left: &mut Left) {
        let sampled = self.versions.latest();
        let image = self.fs.crash_image();
        for (index, plan) in plans(&image).iter().enumerate() {
            let Ok(state) = image.recover(plan) else {
                continue;
            };
            let context = format!("{context}, crash plan {index}");
            let vfs: Arc<dyn Vfs> = Arc::new(state.clone());
            // What has the name of the target is the checkpoint, whole.
            // Nothing else ever has it: no directory without a manifest,
            // and none that is empty.
            match state.read_dir(&self.target) {
                Ok(_) => {
                    let opened = open_on(vfs.clone(), &self.target, &self.db)
                        .unwrap_or_else(|error| panic!("{context}: the checkpoint: {error}"));
                    assert_eq!(opened.versions.latest(), sampled, "{context}");
                    opened.check(self.history.at(sampled));
                    left.whole += 1;
                }
                Err(error) => {
                    assert_eq!(error.kind(), ErrorKind::NotFound, "{context}: {error}");
                    assert!(!created, "{context}: the checkpoint that was made is gone");
                    left.none += 1;
                }
            }
            let built = state.read_dir(&building(&self.target)).is_ok();
            assert!(!(built && created), "{context}: both names are there");
            left.built += usize::from(built);
            // The live store lands on a committed version, at or after
            // what was durable, and on the checkpoint's once it is made.
            let live = open_on(vfs, Path::new(DIR), &self.db)
                .unwrap_or_else(|error| panic!("{context}: the live store: {error}"));
            let landed = live.versions.latest();
            assert!(
                self.durable <= landed && landed <= sampled,
                "{context}: the live store landed on {landed}"
            );
            if created {
                assert_eq!(landed, sampled, "{context}");
            }
            if index < 3 {
                live.check(self.history.at(landed));
            }
            if index < 2 {
                self.check_leftover(&state, &live, &context);
            }
        }
    }

    /// What a crash left is taken over by the next creation at the target,
    /// if nothing has its name, and removed by a deletion: the creation
    /// leaves the target alone, as the checkpoint of the live store that
    /// opened, and the deletion leaves neither name.
    fn check_leftover(&self, state: &SimVfs, live: &super::Opened, context: &str) {
        let built = building(&self.target);
        let gone = |path: &Path| matches!(state.read_dir(path), Err(error) if error.kind() == ErrorKind::NotFound);
        if gone(&self.target) {
            let created = checkpointer(&live.db, &live.versions)
                .with_options(NO_CATCH_UP)
                .create(&self.target)
                .unwrap_or_else(|error| panic!("{context}: the next creation: {error}"));
            assert_eq!(created.version, live.versions.latest(), "{context}");
            let vfs: Arc<dyn Vfs> = Arc::new(state.clone());
            let opened = open_on(vfs, &self.target, &self.db)
                .unwrap_or_else(|error| panic!("{context}: the next checkpoint: {error}"));
            assert_eq!(opened.versions.latest(), created.version, "{context}");
        }
        assert!(!gone(&self.target) && gone(&built), "{context}");
        checkpoint::delete(state, &self.target)
            .unwrap_or_else(|error| panic!("{context}: the deletion: {error}"));
        assert!(gone(&self.target) && gone(&built), "{context}");
    }
}

fn the_power_is_cut_at_every_operation(files: Files) {
    let whole = made(31, files);
    let from = whole.fs.stats().ops;
    let created = checkpointer(&whole.db, &whole.versions)
        .with_options(NO_CATCH_UP)
        .create(&whole.target)
        .expect("a checkpoint");
    assert_eq!(created.copied > 0, files == Files::Copied, "{created:?}");
    assert_eq!(created.linked > 0, files == Files::Linked, "{created:?}");
    let ops = whole.fs.stats().ops - from;
    assert!(ops > 12, "a creation of {ops} operations");

    let mut left = Left::default();
    for cut in 1..=ops {
        let made = made(31, files);
        assert_eq!(made.fs.stats().ops, from, "the same store");
        made.fs.crash_at(CrashPoint::AfterOp(from + cut));
        let created = checkpointer(&made.db, &made.versions)
            .with_options(NO_CATCH_UP)
            .create(&made.target);
        assert_eq!(
            created.is_ok(),
            cut == ops,
            "cut {cut} of {ops}: {created:?}"
        );
        made.check_crash(created.is_ok(), &format!("cut {cut} of {ops}"), &mut left);
    }
    assert!(
        left.whole > 0 && left.none > 0 && left.built > 0,
        "{left:?}"
    );
}

/// The power is cut after each operation of a creation. In every state the
/// crash can leave, nothing has the name of the target, or the whole
/// checkpoint has it: it opens at the version of its sample with that
/// version's state. The live store opens at a committed version in every
/// one of them. Once the creation has returned, the checkpoint is there in
/// every state, and the live store is at its version (CK4). What a state
/// has under the name the checkpoint was built under is removed by a
/// deletion of the target, and by the next creation there.
#[test]
fn a_power_cut_at_any_operation_of_a_creation_leaves_no_checkpoint_or_a_whole_one() {
    the_power_is_cut_at_every_operation(Files::Linked);
}

/// The same for a checkpoint whose files are copied: a copy is durable
/// before the directory that holds it gets the name of the target.
#[test]
fn a_power_cut_at_any_operation_of_a_copying_creation_leaves_no_checkpoint_or_a_whole_one() {
    the_power_is_cut_at_every_operation(Files::Copied);
}

fn the_file_layer_fails_at_every_operation(files: Files, fault: Fault) -> (usize, usize) {
    let whole = made(41, files);
    let from = whole.fs.stats().ops;
    let made_whole = checkpointer(&whole.db, &whole.versions).with_options(NO_CATCH_UP);
    made_whole.create(&whole.target).expect("a checkpoint");
    let ops = whole.fs.stats().ops - from;

    let (mut failed, mut sync_failed) = (0, 0);
    for at in 1..=ops {
        let context = format!("{fault:?} at operation {at} of {ops}");
        let made = made(41, files);
        let sampled = made.versions.latest();
        let mut world = World::new(&made.db, &made.versions);
        world.model = made.history.at(sampled).clone();
        made.fs.add_rule(FaultRule::new(fault).from_op(from + at));
        let maker = checkpointer(&made.db, &made.versions).with_options(NO_CATCH_UP);
        let created = maker.create(&made.target);
        made.fs.clear_rules();
        let open_at = |version| {
            let vfs: Arc<dyn Vfs> = Arc::new(made.fs.clone());
            let opened = open_on(vfs, &made.target, &made.db)
                .unwrap_or_else(|error| panic!("{context}: the checkpoint: {error}"));
            assert_eq!(opened.versions.latest(), version, "{context}");
            opened.check(made.history.at(version));
        };
        let error = match created {
            // The fault met no operation of the creation that could take
            // it, or one of an attempt the creation does not depend on.
            Ok(created) => {
                assert_eq!(created.version, sampled, "{context}");
                open_at(sampled);
                continue;
            }
            Err(error) => error,
        };
        failed += 1;
        // Nothing has the name of the target, nothing is left under the
        // name the checkpoint was built under, and the live store is as it
        // was and goes on (CK10).
        for path in [&made.target, &building(&made.target)] {
            let listed = made.fs.read_dir(path);
            assert!(
                matches!(&listed, Err(error) if error.kind() == ErrorKind::NotFound),
                "{context}: {error} left {listed:?}"
            );
        }
        assert_eq!(made.versions.pins_held(), 0, "{context}");
        world.check_committed();
        block(&mut world, &mut Rng::new(at), 1_000, 8);
        world.check_committed();
        // The creation can be made again, unless it was a sync of the log
        // that failed: that stays failed until the store is opened again
        // (`lignin_store::wal`).
        match (error, maker.create(&made.target)) {
            (_, Ok(created)) => {
                assert_eq!(created.version, sampled.next().expect("a version"));
                let vfs: Arc<dyn Vfs> = Arc::new(made.fs.clone());
                let opened = open_on(vfs, &made.target, &made.db)
                    .unwrap_or_else(|error| panic!("{context}: the checkpoint: {error}"));
                opened.check(&world.model);
            }
            (CheckpointError::Sync(first), Err(CheckpointError::Sync(_))) => {
                sync_failed += 1;
                let listed = made.fs.read_dir(&made.target);
                assert!(listed.is_err(), "{context}: {first} left {listed:?}");
            }
            (first, Err(second)) => panic!("{context}: {first}, and then {second}"),
        }
    }
    (failed, sync_failed)
}

/// Each operation of a creation is failed in turn, with an error of the
/// device and with a full disk. The creation reports the error, nothing
/// has the name of the target or the name the checkpoint was built under,
/// the live store reads and commits as before, and the creation succeeds
/// when it is made again.
#[test]
fn a_fault_at_any_operation_of_a_creation_leaves_the_live_store_as_it_was() {
    for fault in [Fault::Eio, Fault::Enospc] {
        let (failed, sync_failed) = the_file_layer_fails_at_every_operation(Files::Linked, fault);
        assert!(
            failed > 12 && sync_failed > 0,
            "{fault:?}: {failed}, {sync_failed}"
        );
    }
}

/// The same where the files are copied: a read of a live file, and the
/// appends and the sync of a copy, fail as well.
#[test]
fn a_fault_at_any_operation_of_a_copying_creation_leaves_the_live_store_as_it_was() {
    for fault in [Fault::Eio, Fault::Enospc] {
        let (failed, sync_failed) = the_file_layer_fails_at_every_operation(Files::Copied, fault);
        assert!(
            failed > 20 && sync_failed > 0,
            "{fault:?}: {failed}, {sync_failed}"
        );
    }
}

/// The directory has the name of the target, and that name cannot be made
/// durable. The creation fails, and it removes nothing from a directory
/// that has, or can come back with, the name of the target:
///
/// - where the name is taken back and that is durable, everything goes;
/// - where the name is taken back and that cannot be made durable, the
///   checkpoint stays whole under the name it was built under;
/// - where the name cannot be taken back, it stays whole at the target.
///
/// In every state a power cut then leaves, the target is the whole
/// checkpoint or is not there. What was left is removed by the next
/// creation at the target, or by a deletion.
#[test]
fn a_name_that_cannot_be_made_durable_is_taken_back_or_left_with_the_whole_checkpoint() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Fails {
        /// The sync after the rename, once.
        TheSync,
        /// Every sync of the directory above after the set's.
        EverySync,
        /// The sync after the rename, and every rename after the first.
        TheSyncAndTheRenameBack,
    }
    for fails in [
        Fails::TheSync,
        Fails::EverySync,
        Fails::TheSyncAndTheRenameBack,
    ] {
        let made = made(61, Files::Linked);
        let sampled = made.versions.latest();
        let above = made.target.parent().expect("a directory").to_path_buf();
        let built = building(&made.target);
        // The first sync of the directory above is the set's.
        let sync = FaultRule::new(Fault::Eio)
            .on(OpSet::of(&[OpKind::SyncDir]))
            .skip(1);
        let rename = FaultRule::new(Fault::Eio)
            .on(OpSet::of(&[OpKind::Rename]))
            .skip(1);
        match fails {
            Fails::TheSync => made.fs.add_rule(sync.at(&above)),
            Fails::EverySync => made.fs.add_rule(sync.forever().at(&above)),
            Fails::TheSyncAndTheRenameBack => {
                made.fs.add_rule(sync.at(&above));
                made.fs.add_rule(rename.forever());
            }
        }
        let maker = checkpointer(&made.db, &made.versions).with_options(NO_CATCH_UP);
        let failed = maker.create(&made.target);
        made.fs.clear_rules();
        assert!(
            matches!(&failed, Err(CheckpointError::Io { action: "sync the directory", path, .. })
                if *path == above),
            "{fails:?}: {failed:?}"
        );
        let there = |path: &Path| made.fs.read_dir(path).is_ok();
        let (at_target, at_built) = match fails {
            Fails::TheSync => (false, false),
            Fails::EverySync => (false, true),
            Fails::TheSyncAndTheRenameBack => (true, false),
        };
        assert_eq!(
            (there(&made.target), there(&built)),
            (at_target, at_built),
            "{fails:?}"
        );
        // The states a power cut leaves are looked at before anything else
        // is done on this file system: an open of what stayed syncs.
        let mut left = Left::default();
        made.check_crash(false, &format!("{fails:?}"), &mut left);
        assert!(left.none > 0, "{fails:?}: {left:?}");
        // Of the states of a file system that writes in order, some have
        // the first rename and not what came after it, unless the name
        // was taken back durably. The checkpoint is at the target in
        // them, whole, and nothing is under the other name.
        let image = made.fs.crash_image();
        let mut at_target = 0;
        for plan in image.prefixes() {
            let Ok(state) = image.recover(&plan) else {
                continue;
            };
            if state.read_dir(&made.target).is_err() {
                continue;
            }
            assert!(state.read_dir(&built).is_err(), "{fails:?}: both names");
            if at_target == 0 {
                let opened = open_on(Arc::new(state), &made.target, &made.db)
                    .unwrap_or_else(|error| panic!("{fails:?}: at the target: {error}"));
                assert_eq!(opened.versions.latest(), sampled, "{fails:?}");
                opened.check(made.history.at(sampled));
            }
            at_target += 1;
        }
        assert_eq!(at_target > 0, fails != Fails::TheSync, "{fails:?}");
        // What stayed is the whole checkpoint, under whichever name.
        let kept = image.recover(&image.keep_all()).expect("a state");
        for path in [&made.target, &built] {
            if kept.read_dir(path).is_ok() {
                let opened = open_on(Arc::new(kept.clone()), path, &made.db)
                    .unwrap_or_else(|error| panic!("{fails:?}: what stayed: {error}"));
                assert_eq!(opened.versions.latest(), sampled, "{fails:?}");
                opened.check(made.history.at(sampled));
            }
        }
        // What stayed in this state is removed as what any crash leaves.
        match fails {
            Fails::TheSyncAndTheRenameBack => {
                let refused = maker.create(&made.target);
                assert!(
                    matches!(refused, Err(CheckpointError::TargetExists { .. })),
                    "{refused:?}"
                );
                checkpoint::delete(&made.fs, &made.target).expect("the checkpoint is deleted");
            }
            Fails::TheSync | Fails::EverySync => {}
        }
        maker.create(&made.target).expect("a checkpoint");
        assert!(there(&made.target) && !there(&built), "{fails:?}");
    }
}

/// A creation that gave its directory the name of the target and took it
/// back, with neither rename durable, has left the whole checkpoint under
/// the other name, and a power cut can still bring it back at the target.
/// **Whoever empties that directory makes its name durable first**: the
/// next creation at the target, and a deletion, are each cut after every
/// operation, and in every state that is looked at the target is the whole
/// checkpoint of one of the two creations, or is not there. A creation or
/// a deletion that emptied the directory first would leave it at the
/// target with a part of what it held, or with nothing: a place that an
/// open makes a database in.
#[test]
fn what_a_failed_creation_left_is_not_emptied_before_its_name_is_durable() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Next {
        Creation,
        Deletion,
    }
    // A store whose creation failed at the sync after the rename, and at
    // the sync after the rename back.
    let failed = || {
        let made = made(63, Files::Linked);
        let above = made.target.parent().expect("a directory").to_path_buf();
        let sync = FaultRule::new(Fault::Eio)
            .on(OpSet::of(&[OpKind::SyncDir]))
            .skip(1);
        made.fs.add_rule(sync.forever().at(&above));
        let failed = checkpointer(&made.db, &made.versions)
            .with_options(NO_CATCH_UP)
            .create(&made.target);
        made.fs.clear_rules();
        assert!(
            matches!(
                &failed,
                Err(CheckpointError::Io {
                    action: "sync the directory",
                    ..
                })
            ),
            "{failed:?}"
        );
        assert!(made.fs.read_dir(&building(&made.target)).is_ok());
        made
    };
    let next = |made: &Made, next: Next| match next {
        Next::Creation => checkpointer(&made.db, &made.versions)
            .with_options(NO_CATCH_UP)
            .create(&made.target)
            .map(drop),
        Next::Deletion => checkpoint::delete(&made.fs, &made.target),
    };
    for what in [Next::Creation, Next::Deletion] {
        let whole = failed();
        // Both renames wait to be made durable: a power cut now can leave
        // the directory under either name.
        let image = whole.fs.crash_image();
        let at_target = |plan: &CrashPlan| {
            (image.recover(plan)).is_ok_and(|state| state.read_dir(&whole.target).is_ok())
        };
        assert!(image.prefixes().any(|plan| at_target(&plan)), "{what:?}");
        assert!(!at_target(&image.lose_all()) && !at_target(&image.keep_all()));
        let from = whole.fs.stats().ops;
        next(&whole, what).unwrap_or_else(|error| panic!("{what:?}: {error}"));
        let ops = whole.fs.stats().ops - from;
        assert!(ops > 6, "{what:?} of {ops} operations");

        let (mut at_the_target, mut nothing) = (0, 0);
        for cut in 1..=ops {
            let made = failed();
            assert_eq!(made.fs.stats().ops, from, "the same store");
            let sampled = made.versions.latest();
            made.fs.crash_at(CrashPoint::AfterOp(from + cut));
            let done = next(&made, what);
            assert_eq!(
                done.is_ok(),
                cut == ops,
                "{what:?}, cut {cut} of {ops}: {done:?}"
            );
            let image = made.fs.crash_image();
            let mut plans = plans(&image);
            plans.extend(image.prefixes());
            plans.extend(image.holes());
            for (index, plan) in plans.iter().enumerate() {
                let Ok(state) = image.recover(plan) else {
                    continue;
                };
                let context = format!("{what:?}, cut {cut} of {ops}, crash plan {index}");
                match state.read_dir(&made.target) {
                    Err(error) => {
                        assert_eq!(error.kind(), ErrorKind::NotFound, "{context}: {error}");
                        nothing += 1;
                    }
                    // Both creations sampled the same version.
                    Ok(_) => {
                        let opened = open_on(Arc::new(state), &made.target, &made.db)
                            .unwrap_or_else(|error| panic!("{context}: the checkpoint: {error}"));
                        assert_eq!(opened.versions.latest(), sampled, "{context}");
                        if at_the_target % 16 == 0 {
                            opened.check(made.history.at(sampled));
                        }
                        at_the_target += 1;
                    }
                }
            }
        }
        assert!(
            at_the_target > 0 && nothing > 0,
            "{what:?}: {at_the_target}, {nothing}"
        );
    }
}

/// **Everything of a checkpoint is durable under the name it is built
/// under before it gets the name of the target.** Right before the rename,
/// a power cut that loses every change that was not synced leaves nothing
/// at the target, and under the other name a directory that opens as the
/// checkpoint: every file, every name in it, its manifest and its own name
/// were in the set of syncs. A creation that renamed first, or left one
/// sync out of the set, fails here.
#[test]
fn a_checkpoint_is_whole_and_durable_before_it_gets_its_name() {
    for files in [Files::Linked, Files::Copied] {
        let made = made(81, files);
        let sampled = made.versions.latest();
        let built = building(&made.target);
        let reached = std::sync::atomic::AtomicBool::new(false);
        let pause = |step: Step| {
            if step != Step::Settled {
                return;
            }
            reached.store(true, std::sync::atomic::Ordering::SeqCst);
            let image = made.fs.crash_image();
            let left = image.recover(&image.lose_all()).expect("a state");
            assert!(
                matches!(left.read_dir(&made.target), Err(error)
                    if error.kind() == ErrorKind::NotFound),
                "{files:?}: the target is there before the rename"
            );
            let names: Vec<_> = (left.read_dir(&built).expect("the directory"))
                .into_iter()
                .map(|entry| entry.name)
                .collect();
            assert!(
                names.iter().any(|name| name == "LOCK"),
                "{files:?}: {names:?}"
            );
            let vfs: Arc<dyn Vfs> = Arc::new(left);
            let opened = open_on(vfs, &built, &made.db)
                .unwrap_or_else(|error| panic!("{files:?}: what was built: {error}"));
            assert_eq!(opened.versions.latest(), sampled, "{files:?}");
            opened.check(made.history.at(sampled));
        };
        checkpointer(&made.db, &made.versions)
            .with_options(NO_CATCH_UP)
            .pausing(&pause)
            .create(&made.target)
            .expect("a checkpoint");
        assert!(
            reached.load(std::sync::atomic::Ordering::SeqCst),
            "{files:?}"
        );
    }
}

/// A catch-up that the file layer fails changes nothing of the checkpoint
/// but how much log its first open replays (CK6): the creation reports it
/// and goes on.
#[test]
fn a_catch_up_that_fails_is_reported_and_the_checkpoint_is_made() {
    let made = made(51, Files::Linked);
    let sampled = made.versions.latest();
    // The segment that write-back fills takes no block from here on, so
    // its pass fails.
    let segment = store_files(&made.fs, Path::new(DIR))
        .into_iter()
        .rfind(|file| file.starts_with("seg"))
        .expect("a segment");
    let appends = OpSet::of(&[OpKind::Append]);
    let rule = FaultRule::new(Fault::Eio).on(appends).forever();
    made.fs.add_rule(rule.at(Path::new(DIR).join(segment)));
    let eager = CheckpointOptions {
        catch_up_above: Some(0),
    };
    let created = checkpointer(&made.db, &made.versions)
        .with_options(eager)
        .create(&made.target);
    made.fs.clear_rules();
    let created = created.expect("a checkpoint");
    assert!(matches!(created.catch_up, Some(Err(_))), "{created:?}");
    assert_eq!(created.version, sampled);
    let vfs: Arc<dyn Vfs> = Arc::new(made.fs.clone());
    let opened = open_on(vfs, &made.target, &made.db).expect("the checkpoint opens");
    assert_eq!(opened.report.commits, sampled.get() - made.durable.get());
    opened.check(made.history.at(sampled));
}
