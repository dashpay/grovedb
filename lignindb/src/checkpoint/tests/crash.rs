//! The power is cut, and the file layer fails, at every operation of a
//! creation (CK4, CK10; T-DU1 for a checkpoint).
//!
//! The file layer is the simulated one (`lignin_io::sim`). A crash keeps,
//! loses or tears each change that was not synced, as a plan says: both
//! ends, what a file system that writes in order leaves, one change lost
//! at a time, and drawn samples. Each state is opened twice, as the live
//! store and as the checkpoint, by the store's recovery.

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
    segment::read_manifest,
    wal::LogOptions,
};

use super::{
    block, checkpointer, checkpoints_in, open_on, sim, store_files, History, Meddling, DIR,
    NO_CATCH_UP,
};
use crate::{
    checkpoint::{self, CheckpointError, CheckpointOptions},
    snapshot::Versions,
    txn::tests::{
        store::{Db, Rng, SIZE},
        World,
    },
};

/// How the files of a checkpoint come into its directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Files {
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
fn plans(image: &CrashImage) -> Vec<CrashPlan> {
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

/// What a crash left: how many of its states had a checkpoint, and how
/// many had none.
#[derive(Debug, Default)]
struct Left {
    whole: usize,
    none: usize,
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
            // A directory with a manifest is a checkpoint, whole.
            if read_manifest(&state, &self.target).is_ok() {
                let opened = open_on(vfs.clone(), &self.target, &self.db)
                    .unwrap_or_else(|error| panic!("{context}: the checkpoint: {error}"));
                assert_eq!(opened.versions.latest(), sampled, "{context}");
                opened.check(self.history.at(sampled));
                left.whole += 1;
            } else {
                assert!(!created, "{context}: the checkpoint that was made is gone");
                left.none += 1;
            }
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
        }
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
    assert!(left.whole > 0 && left.none > 0, "{left:?}");
}

/// The power is cut after each operation of a creation. In every state the
/// crash can leave, the target has no manifest, or it is the whole
/// checkpoint: it opens at the version of its sample with that version's
/// state. The live store opens at a committed version in every one of
/// them. Once the creation has returned, the checkpoint is there in every
/// state, and the live store is at its version (CK4).
#[test]
fn a_power_cut_at_any_operation_of_a_creation_leaves_no_checkpoint_or_a_whole_one() {
    the_power_is_cut_at_every_operation(Files::Linked);
}

/// The same for a checkpoint whose files are copied: a copy is durable
/// before the manifest names it.
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

    let (mut failed, mut sync_failed, mut lock_left) = (0, 0, 0);
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
        // Nothing is left of the target, and the live store is as it was
        // and goes on (CK10). One failure leaves the target with its lock
        // file: where the creation has the lock and cannot see what the
        // target holds, it removes nothing from it. A deletion ends that.
        let listed = made.fs.read_dir(&made.target);
        if matches!(&error, CheckpointError::Io { action: "list", path, .. } if *path == made.target)
        {
            let names: Vec<_> = (listed.iter().flatten())
                .map(|entry| entry.name.clone())
                .collect();
            assert_eq!(names, ["LOCK"], "{context}: {error}");
            checkpoint::delete(&made.fs, &made.target)
                .unwrap_or_else(|error| panic!("{context}: the leftover: {error}"));
            lock_left += 1;
        }
        let listed = made.fs.read_dir(&made.target);
        assert!(
            matches!(&listed, Err(error) if error.kind() == ErrorKind::NotFound),
            "{context}: {error} left {listed:?}"
        );
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
    // A listing fails for an error of the device, and not for a full disk.
    assert_eq!(
        lock_left,
        usize::from(matches!(fault, Fault::Eio)),
        "{fault:?}"
    );
    (failed, sync_failed)
}

/// Each operation of a creation is failed in turn, with an error of the
/// device and with a full disk. The creation reports the error, nothing is
/// left of the target, the live store reads and commits as before, and the
/// creation succeeds when it is made again.
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

/// A manifest is written, the sync that makes it durable fails, and so
/// does the sync after its removal: the creation fails, and it leaves
/// every file in the target. A power cut can bring that manifest back, and
/// it then names files that are all there: what opens is the whole
/// checkpoint, and never one with a file missing.
#[test]
fn a_manifest_whose_removal_is_not_durable_keeps_its_files() {
    let made = made(61, Files::Linked);
    // The names in the target are made durable, and from then on no sync
    // of the target succeeds.
    let syncs = OpSet::of(&[OpKind::SyncDir]);
    let rule = FaultRule::new(Fault::Eio).on(syncs).skip(1).forever();
    made.fs.add_rule(rule.at(&made.target));
    let failed = checkpointer(&made.db, &made.versions)
        .with_options(NO_CATCH_UP)
        .create(&made.target);
    made.fs.clear_rules();
    assert!(
        matches!(failed, Err(CheckpointError::Install(_))),
        "{failed:?}"
    );
    assert!(read_manifest(&made.fs, &made.target).is_err());
    let kept = store_files(&made.fs, &made.target);
    assert!(kept.iter().any(|file| file.starts_with("wal")), "{kept:?}");
    assert!(kept.iter().any(|file| file.starts_with("seg")), "{kept:?}");
    let mut left = Left::default();
    made.check_crash(false, "a removal that is not durable", &mut left);
    assert!(left.whole > 0 && left.none > 0, "{left:?}");
}

/// The name of the target is durable before its manifest is written, as
/// the store asks of a database directory (`lignin_store::segment`). The
/// directory above the target cannot be synced, and nothing can be
/// renamed: the creation fails at the sync, which it would not reach if it
/// wrote the manifest first, and would not make if it left the name of
/// the target to the manifest's own sync.
#[test]
fn the_name_of_the_target_is_durable_before_its_manifest_is_written() {
    let made = made(71, Files::Linked);
    let above = made.target.parent().expect("a directory").to_path_buf();
    let sync = FaultRule::new(Fault::Eio).on(OpSet::of(&[OpKind::SyncDir]));
    made.fs.add_rule(sync.forever().at(&above));
    let rename = FaultRule::new(Fault::Eio).on(OpSet::of(&[OpKind::Rename]));
    made.fs.add_rule(rename.forever());
    let maker = checkpointer(&made.db, &made.versions).with_options(NO_CATCH_UP);
    let failed = maker.create(&made.target);
    made.fs.clear_rules();
    assert!(
        matches!(&failed, Err(CheckpointError::Io { action: "sync the directory", path, .. })
            if *path == above),
        "{failed:?}"
    );
    assert!(made.fs.read_dir(&made.target).is_err());
    maker.create(&made.target).expect("a checkpoint");
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
