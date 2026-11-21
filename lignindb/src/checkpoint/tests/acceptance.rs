//! The acceptance tests of TXN-06, one for each sentence of its "Done
//! when" but the timings (`reference.rs`), and the tests that
//! `transactions.md` 15.2 asks of it for one thread.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{manifest::FileState, wal::GroupWriter};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{flush::Checkpointed, segment::read_manifest};

use super::{
    block, bytes_of, checkpointer, checkpoints_in, names_of, open, open_on, remove_block,
    root_hash, sim, store_files, store_on, store_with_log, turn, write_documents, History, DIR,
    NO_CATCH_UP,
};
use crate::{
    checkpoint::{CheckpointError, CheckpointOptions, DEFAULT_CATCH_UP_ABOVE},
    commit::{CommitError, NoStaleHashes, Stopped},
    txn::tests::{
        model::OwnedBatch,
        put,
        store::{Db, Outcome, Rng},
        World,
    },
};

/// "It returns the same root hash."
///
/// A checkpoint is made of a store with blocks that write-back has written
/// and blocks that only the log holds, the live store goes on, and the
/// checkpoint is opened as any directory is: it is at the version it was
/// made of, with the root hash that version had, every record and every
/// node of it, and nothing of what came later.
#[test]
fn a_checkpoint_opens_with_the_state_and_the_root_hash_of_its_version() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(1);
    for round in 0..6 {
        block(&mut world, &mut rng, round * 20, 20);
        if round % 2 == 1 {
            (db.write_back(&versions).pass()).expect("a pass of write-back");
        }
    }

    let target = checkpoints_in(&db).join("1");
    let created = (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert_eq!(created.version, versions.latest());
    assert_eq!(created.log_end, db.log.end());
    assert!(created.linked > 0 && created.copied == 0, "{created:?}");
    assert!(created.catch_up.is_none(), "{created:?}");
    let captured = world.model.clone();
    let hash = root_hash(&db, &versions, created.version).expect("the grove has a root");

    for round in 6..9 {
        block(&mut world, &mut rng, round * 20, 20);
    }
    remove_block(&mut world, 0, 20);
    assert_ne!(root_hash(&db, &versions, versions.latest()), Some(hash));

    let opened = open(&db, &target);
    assert_eq!(opened.versions.latest(), created.version);
    assert_eq!(opened.report.landed, created.version);
    assert_eq!(
        root_hash(&opened.db, &opened.versions, created.version),
        Some(hash)
    );
    opened.check(&captured);
    world.check_committed();
}

/// "It stays readable while the live database keeps writing and cleaning."
/// With it T-CK3's second half: "afterwards deleting any live file leaves
/// the checkpoint readable" (CK7).
///
/// The checkpoint is open while the live store commits, removes, writes
/// back, takes table checkpoints and lets go of log files and of the table
/// checkpoint the two shared. It reads its version throughout. Then every
/// file of the live store is deleted, and the checkpoint reads on, and
/// opens again from its own directory alone.
#[test]
fn a_checkpoint_stays_readable_while_the_live_database_writes_and_cleans() {
    let (db, versions) = store_with_log(24 << 10, 256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(3);
    for round in 0..8 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
        if round == 4 {
            let taken = (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
            assert!(matches!(taken, Checkpointed::Installed { .. }), "{taken:?}");
        }
    }

    let target = checkpoints_in(&db).join("1");
    let created = (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    let captured = world.model.clone();
    let shared = store_files(&StdVfs, &target);
    let kinds: BTreeSet<&Path> = shared.iter().filter_map(|file| file.parent()).collect();
    let (wal, seg, table) = (Path::new("wal"), Path::new("seg"), Path::new("table"));
    assert!(
        kinds.is_superset(&[wal, seg, table].into()),
        "the checkpoint shares {shared:?}"
    );
    assert_eq!(created.linked, shared.len());
    let opened = open(&db, &target);
    opened.check(&captured);

    for round in 8..24 {
        block(&mut world, &mut rng, round * 20, 20);
        if round % 3 == 0 {
            remove_block(&mut world, (round - 8) * 20, 20);
        }
        turn(&db, &versions);
        if round % 4 == 3 {
            let taken = (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
            assert!(matches!(taken, Checkpointed::Installed { .. }), "{taken:?}");
        }
        opened.check(&captured);
    }
    world.check_committed();
    // The live store has let go of log files and of the table checkpoint
    // that the checkpoint holds.
    let gone = |kind: &Path| {
        let of_kind = shared.iter().filter(|file| file.starts_with(kind));
        of_kind.filter(|file| !db.dir.join(file).exists()).count()
    };
    assert!(gone(wal) > 0 && gone(table) == 1, "of {shared:?}");

    // Every file the live store has goes, and so does the store.
    drop(world);
    for file in store_files(&StdVfs, &db.dir) {
        std::fs::remove_file(db.dir.join(file)).expect("a live file is deleted");
    }
    // No recovery finds the live store in what is left.
    db.files_are_gone();
    opened.check(&captured);
    drop(opened);
    let again = open(&db, &target);
    assert_eq!(again.versions.latest(), created.version);
    again.check(&captured);
}

/// A checkpoint is a database of its own (CK7, P-15): it takes commits,
/// which the live store does not see, it changes no byte of a file the
/// two share, and a checkpoint can be made of it in turn. A file that the
/// caller puts into its directory is left alone (P-17), and a checkpoint
/// made in the directory of the store holds nothing of the checkpoints
/// beside it (P-14).
#[test]
fn a_checkpoint_is_a_database_of_its_own() {
    let (db, versions) = store_with_log(24 << 10, 256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(4);
    for round in 0..5 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    let checkpoints = checkpoints_in(&db);
    let target = checkpoints.join("1");
    let created = (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    let captured = world.model.clone();
    std::fs::write(target.join("platform_state.bin"), b"the caller's").expect("a file");
    let shared = store_files(&StdVfs, &target);
    let live_bytes = |file: &Path| bytes_of(&StdVfs, &db.dir.join(file));
    let before: Vec<Vec<u8>> = shared.iter().map(|file| live_bytes(file)).collect();

    let opened = open(&db, &target);
    let mut theirs = opened.world(&captured);
    let mut their_rng = Rng::new(44);
    for round in 0..4 {
        block(&mut theirs, &mut their_rng, 10_000 + round * 20, 20);
        turn(&opened.db, &opened.versions);
    }
    remove_block(&mut theirs, 0, 20);
    let finished = opened.db.write_back(&opened.versions).finish();
    assert!(
        matches!(finished, Ok(Checkpointed::Installed { .. })),
        "{finished:?}"
    );
    theirs.check_committed();
    // The live store has what it had, byte for byte in every shared file.
    for (file, bytes) in shared.iter().zip(&before) {
        assert_eq!(&live_bytes(file), bytes, "{}", file.display());
    }
    world.check_committed();
    assert_eq!(versions.latest(), created.version);

    // A checkpoint of the checkpoint, and one more of the live store.
    let second = opened.db.dir.join("of-the-checkpoint");
    let made = (checkpointer(&opened.db, &opened.versions).create(&second)).expect("a checkpoint");
    assert_eq!(made.version, opened.versions.latest());
    open(&opened.db, &second).check(&theirs.model);
    block(&mut world, &mut rng, 5 * 20, 20);
    let third = checkpoints.join("2");
    (checkpointer(&db, &versions).create(&third)).expect("a checkpoint");
    let entries: BTreeSet<String> = std::fs::read_dir(&third)
        .expect("the checkpoint's directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .into_string()
                .expect("a name")
        })
        .collect();
    let own = ["LOCK", "MANIFEST", "seg", "table", "val", "wal"];
    assert!(
        entries.iter().all(|entry| own.contains(&entry.as_str())),
        "{entries:?}"
    );
    open(&db, &third).check(&world.model);

    // The first checkpoint opens again with what it committed, and with the
    // caller's file as it was.
    let their_model = theirs.model.clone();
    drop(theirs);
    drop(opened);
    open(&db, &target).check(&their_model);
    let kept = std::fs::read(target.join("platform_state.bin")).expect("the caller's file");
    assert_eq!(kept, b"the caller's");
}

/// T-CK2: "an open write transaction's writes are in no checkpoint" (CK8).
/// The checkpoint does not wait for the transaction either: write-back's
/// catch-up is put off by the page the transaction holds back, and the
/// checkpoint is made all the same. The transaction commits afterwards.
#[test]
fn the_writes_of_an_open_transaction_are_in_no_checkpoint() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(5);
    for round in 0..3 {
        block(&mut world, &mut rng, round * 20, 20);
    }
    // The documents of the last blocks wait to be placed, and the
    // transaction copies their pages.
    let tx = world.begin();
    write_documents(&world, &tx, &mut rng, 500, 20, 400);
    world.write(&tx, vec![put(super::aux(), b"uncommitted", b"not in it")]);
    let captured = world.model.clone();

    let target = checkpoints_in(&db).join("1");
    let eager = CheckpointOptions {
        catch_up_above: Some(0),
    };
    let created = (checkpointer(&db, &versions)
        .with_options(eager)
        .create(&target))
    .expect("a checkpoint beside an open transaction");
    assert!(
        matches!(created.catch_up, Some(Ok(Checkpointed::PutOff { blocked })) if blocked > 0),
        "{created:?}"
    );
    assert_eq!(created.version, versions.latest());
    let opened = open(&db, &target);
    opened.check(&captured);

    world.check(&tx);
    let outcome = world.commit(tx, &[]);
    assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
    world.check_committed();
    opened.check(&captured);
}

/// "A checkpoint whose directory is removed while it is open closes
/// cleanly, and the live database is unaffected."
///
/// The checkpoint is open and has commits of its own that are not written
/// back. Its directory is removed as Platform removes it (P-18). The
/// handle still reads, and closing it brings nothing back: write-back ends
/// with an error and no panic, and the directory is still gone. The live
/// store has every file it had, byte for byte, reads and writes as before,
/// and a checkpoint can be made under the same name again.
///
/// What the close of the handle writes to its own files, which have no
/// name any more, is not looked at here: a close that writes nothing is
/// the engine's (TXN-07).
#[test]
fn a_checkpoint_whose_directory_is_removed_while_it_is_open_closes_cleanly() {
    let (db, versions) = store_with_log(24 << 10, 256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(6);
    for round in 0..5 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    let target = checkpoints_in(&db).join("1");
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    let captured = world.model.clone();
    let live_files = || -> Vec<(PathBuf, Vec<u8>)> {
        (store_files(&StdVfs, &db.dir).into_iter())
            .chain([PathBuf::from("MANIFEST")])
            .map(|file| {
                let bytes = std::fs::read(db.dir.join(&file)).expect("a live file");
                (file, bytes)
            })
            .collect()
    };
    let live_before = live_files();
    assert!(live_before.len() >= 3);

    let opened = open(&db, &target);
    let mut theirs = opened.world(&captured);
    block(&mut theirs, &mut Rng::new(66), 10_000, 20);
    turn(&opened.db, &opened.versions);
    block(&mut theirs, &mut Rng::new(67), 10_020, 20);

    std::fs::remove_dir_all(&target).expect("the directory is removed");
    assert!(!target.exists());
    theirs.check_committed();
    // The close of a session: write-back's last turn, and the handle goes.
    let finished = opened.db.write_back(&opened.versions).finish();
    assert!(finished.is_err(), "{finished:?}");
    drop(theirs);
    drop(opened);
    assert!(!target.exists(), "the close brought the directory back");

    assert!(live_files() == live_before, "a live file has changed");
    world.check_committed();
    block(&mut world, &mut rng, 5 * 20, 20);
    turn(&db, &versions);
    world.check_committed();
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint under the same name");
    open(&db, &target).check(&world.model);
}

/// "If much log is not yet written back to pages, the flusher catches up
/// first so the checkpoint opens quickly" (CK6).
///
/// With no catch-up the first open of a checkpoint replays every commit
/// that no table checkpoint covers. With one, write-back takes its table
/// checkpoint before the sample, and the open replays nothing.
#[test]
fn write_back_catches_up_before_the_sample_when_much_log_is_held() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(7);
    for round in 0..6 {
        block(&mut world, &mut rng, round * 20, 20);
    }
    let held = db.write_back(&versions).log_held();
    assert!(held > 0 && held < DEFAULT_CATCH_UP_ABOVE, "{held} bytes");
    let checkpoints = checkpoints_in(&db);
    let commits = versions.latest().get();

    // The default leaves this little log to the open.
    let lazy = checkpoints.join("lazy");
    let made = checkpointer(&db, &versions);
    assert_eq!(made.options(), CheckpointOptions::default());
    let created = made.create(&lazy).expect("a checkpoint");
    assert!(created.catch_up.is_none() && created.table.is_none());
    let opened = open(&db, &lazy);
    assert_eq!(opened.report.commits, commits);
    opened.check(&world.model);

    let eager = checkpoints.join("eager");
    let options = CheckpointOptions {
        catch_up_above: Some(held - 1),
    };
    let created = (made.with_options(options).create(&eager)).expect("a checkpoint");
    let Some(Ok(Checkpointed::Installed { covers, .. })) = created.catch_up else {
        panic!("{created:?}");
    };
    assert_eq!(covers, versions.latest());
    assert_eq!(created.table.map(|table| table.covers()), Some(covers));
    let opened = open(&db, &eager);
    assert_eq!((opened.report.commits, opened.report.covers), (0, covers));
    opened.check(&world.model);

    // Nothing is held now, so nothing is caught up with.
    let created =
        (made.with_options(options).create(&checkpoints.join("idle"))).expect("a checkpoint");
    assert!(created.catch_up.is_none(), "{created:?}");
}

/// "It makes the version durable" (CK4), and "afterwards the checkpoint
/// holds nothing in the live database" (CK3, CK7).
///
/// Nothing of the store's last blocks is synced when the checkpoint is
/// made. When the creation has returned, a power cut that loses every
/// change that is not durable leaves the checkpoint whole, at its version,
/// and the live store at that version too. The creation gave back its
/// version pin and its hold on the log, and every file the checkpoint's
/// manifest names is closed or sealed, with the length the manifest gives
/// it or more.
#[test]
fn a_checkpoint_is_durable_when_it_is_made_and_so_is_its_version() {
    let fs = sim(8);
    let (db, versions) = store_on(&fs);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(8);
    let mut history = History::default();
    for round in 0..5 {
        block(&mut world, &mut rng, round * 20, 20);
        history.note(&world.model);
        if round == 2 {
            (db.write_back(&versions).pass()).expect("a pass of write-back");
        }
    }
    assert!(db.log.durable().version < versions.latest());

    let target = checkpoints_in(&db).join("1");
    let created = (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert_eq!(db.log.durable().version, created.version);
    assert_eq!(versions.pins_held(), 0);
    let manifest = read_manifest(&fs, &target).expect("the checkpoint's manifest");
    assert_eq!(
        manifest.page_size(),
        read_manifest(&fs, Path::new(DIR))
            .expect("the live manifest")
            .page_size()
    );
    for file in manifest.files() {
        assert_ne!(file.state(), FileState::Open, "{file:?}");
        let names = names_of(file.kind());
        let len = bytes_of(&fs, &names.path(&target, file.id())).len() as u64;
        assert!(len >= file.valid_len(), "{file:?} has {len} bytes");
    }

    // The live store goes on, and then the power is cut.
    block(&mut world, &mut rng, 5 * 20, 20);
    history.note(&world.model);
    let image = fs.crash_image();
    let left = image.recover(&image.lose_all()).expect("a state");
    let vfs: Arc<dyn Vfs> = Arc::new(left);
    let checkpoint = open_on(vfs.clone(), &target, &db).expect("the checkpoint opens");
    assert_eq!(checkpoint.versions.latest(), created.version);
    checkpoint.check(history.at(created.version));
    let live = open_on(vfs, Path::new(DIR), &db).expect("the live store opens");
    assert_eq!(live.versions.latest(), created.version);
    live.check(history.at(created.version));
}

/// A session that takes no commit any more is not checkpointed: what its
/// log holds is not what it has published. Neither is one whose log holds
/// a group that was never published. Both leave no directory.
#[test]
fn a_session_whose_log_is_ahead_or_that_has_stopped_is_refused() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(9), 0, 10);
    let before = versions.latest();
    let checkpoints = checkpoints_in(&db);

    // A group of the next version, appended and not published.
    let at = db.log.next_group().expect("a place");
    let mut bytes = Vec::new();
    let group = GroupWriter::new(at.version(), at.address(), &mut bytes).expect("a group");
    group.finish();
    db.log.append_group(&at, &bytes).expect("an append");

    let target = checkpoints.join("ahead");
    let refused = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&refused, Err(CheckpointError::LogAhead { log, latest })
            if *log == at.version() && *latest == before),
        "{refused:?}"
    );
    assert!(!target.exists(), "a refused checkpoint leaves a directory");
    assert_eq!(versions.pins_held(), 0);

    // The commit that finds the group stops the session.
    let committer = db.committer(&versions);
    let batch = OwnedBatch {
        records: vec![put(super::aux(), b"k", b"v")],
        ..OwnedBatch::default()
    };
    let failed = batch.with(|batch| committer.write(batch, &mut NoStaleHashes));
    assert!(matches!(failed, Err(CommitError::Failed(_))), "{failed:?}");
    let target = checkpoints.join("stopped");
    let refused = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(
            &refused,
            Err(CheckpointError::Stopped(Stopped::Halted { .. }))
        ),
        "{refused:?}"
    );
    assert!(!target.exists(), "a refused checkpoint leaves a directory");
}

/// An existing target is refused and left as it is (P-14), and so is a
/// target with no directory to be made in.
#[test]
fn an_existing_target_is_refused_and_left_untouched() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(2), 0, 10);
    let target = checkpoints_in(&db).join("taken");
    std::fs::create_dir(&target).expect("a directory");
    std::fs::write(target.join("kept"), b"as it was").expect("a file");

    let refused = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&refused, Err(CheckpointError::TargetExists { target: named }) if *named == target),
        "{refused:?}"
    );
    let entries: Vec<_> = std::fs::read_dir(&target)
        .expect("the directory")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(entries, ["kept"]);
    assert_eq!(
        std::fs::read(target.join("kept")).expect("the file"),
        b"as it was"
    );
    // A file in the way is refused the same.
    let refused = checkpointer(&db, &versions).create(&target.join("kept"));
    assert!(
        matches!(refused, Err(CheckpointError::TargetExists { .. })),
        "{refused:?}"
    );
    for nowhere in ["/", ""] {
        let refused = checkpointer(&db, &versions).create(Path::new(nowhere));
        assert!(
            matches!(refused, Err(CheckpointError::NoParent { .. })),
            "{nowhere:?}: {refused:?}"
        );
    }
    // The directory above the target is the caller's to make.
    let orphan = db.dir.join("not-there").join("1");
    let refused = checkpointer(&db, &versions).create(&orphan);
    assert!(
        matches!(&refused, Err(CheckpointError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound),
        "{refused:?}"
    );
    assert!(!orphan.parent().expect("a parent").exists());
    world.check_committed();
}

/// A manifest that a replacement left in doubt is not sampled: the
/// creation is refused, with nothing left of the target, until write-back
/// has found out which manifest is in force. Then it is made.
#[test]
fn a_manifest_in_doubt_is_not_sampled() {
    use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};
    use lignin_store::segment::SegmentStoreError;

    let fs = sim(10);
    let (db, versions) = store_on(&fs);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(10);
    for round in 0..3 {
        block(&mut world, &mut rng, round * 10, 10);
    }
    (db.write_back(&versions).pass()).expect("a pass of write-back");
    db.segments.sync().expect("the segment is synced");
    // The rename of the next manifest fails: the directory has the old one
    // or the new one, and the store does not know which.
    let renames = OpSet::of(&[OpKind::Rename]);
    fs.add_rule(FaultRule::new(Fault::Eio).on(renames));
    {
        let mut manifest = db.manifest.lock().expect("the manifest");
        let failed = db.segments.commit(&mut manifest);
        assert!(
            failed.is_err() && manifest.manifest().is_err(),
            "{failed:?}"
        );
    }
    fs.clear_rules();

    let target = checkpoints_in(&db).join("1");
    let maker = checkpointer(&db, &versions).with_options(NO_CATCH_UP);
    let refused = maker.create(&target);
    assert!(
        matches!(
            &refused,
            Err(CheckpointError::Manifest(
                SegmentStoreError::ManifestInDoubt { .. }
            ))
        ),
        "{refused:?}"
    );
    assert!(
        fs.read_dir(&target).is_err(),
        "a refused checkpoint leaves a directory"
    );
    assert_eq!(versions.pins_held(), 0);

    // Write-back's next table checkpoint opens the manifest again.
    (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
    let created = maker.create(&target).expect("a checkpoint");
    assert_eq!(created.version, versions.latest());
    let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
    let opened = open_on(vfs, &target, &db).expect("the checkpoint opens");
    opened.check(&world.model);
}
