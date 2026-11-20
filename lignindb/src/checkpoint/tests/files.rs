//! What a checkpoint's directory is made of: links where the file system
//! makes them and copies where it does not, the files the engine has it
//! carry, and the deletion of a checkpoint.

use std::{
    collections::BTreeMap,
    io::ErrorKind,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use lignin_format::manifest::Manifest;
use lignin_io::{sim::SimVfs, StdVfs, Vfs, VfsLock};
use lignin_store::{
    flush::FlushConfig,
    segment::{read_manifest, FileNames, ManifestStore, SegmentStoreError},
    wal::LogOptions,
};

use super::{
    block, building, checkpointer, checkpoints_in, everything_in, names_of, open, open_on, sim,
    store_files, store_on, store_with_log, turn, write_file, Before, Meddling, ONE_LETTER,
    TWO_LETTERS,
};
use crate::{
    checkpoint::{
        self, create::Step, CheckpointError, CheckpointOptions, CREATING_SUFFIX, DELETING_FILE_NAME,
    },
    snapshot::Versions,
    txn::tests::{
        model::Committed,
        store::{Db, NoSync, Rng, SIZE},
        World,
    },
};

/// A store in a temporary directory of its own, on the file layer that
/// `meddling` makes of the one whose syncs do nothing and of the directory
/// of the store's checkpoints.
fn store_on_meddling(
    meddling: impl FnOnce(Arc<dyn Vfs>, PathBuf) -> Meddling,
) -> (Arc<Db>, Versions) {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().to_path_buf();
    let vfs = meddling(Arc::new(NoSync), path.join("checkpoints"));
    let options = LogOptions {
        target_file_len: 24 << 10,
        ..LogOptions::default()
    };
    Db::on(
        Arc::new(vfs),
        &path,
        256,
        options,
        FlushConfig::new(SIZE),
        Some(dir),
    )
}

/// A store on a file layer that makes no hard link into the directory of
/// its checkpoints.
fn store_without_links(kind: ErrorKind) -> (Arc<Db>, Versions) {
    store_on_meddling(|plain, checkpoints| Meddling::no_links(plain, checkpoints, kind))
}

/// A store on a file layer that runs `before` before it takes the lock of
/// a directory among the store's checkpoints, given the lock's file.
fn store_with_locks_met(before: Before) -> (Arc<Db>, Versions) {
    store_on_meddling(|plain, checkpoints| {
        let among = move |lock: &Path| match lock.starts_with(&checkpoints) {
            true => before(lock),
            false => Ok(()),
        };
        Meddling::before_lock(plain, Box::new(among))
    })
}

/// Every numbered file that `manifest` names, as its path inside the
/// database directory and the length the manifest gives it.
fn named(manifest: &Manifest) -> Vec<(PathBuf, u64)> {
    let dir = Path::new("");
    let files = manifest
        .files()
        .iter()
        .map(|file| (names_of(file.kind()).path(dir, file.id()), file.valid_len()));
    let table = manifest.table_checkpoint().map(|table| {
        (
            FileNames::PAGE_TABLE.path(dir, table.file()),
            table.valid_len(),
        )
    });
    files.chain(table).collect()
}

/// "Across filesystems it falls back to copying."
///
/// The file layer makes no link into the directory of the checkpoints, for
/// each reason a file system has. Every file is copied, up to the length
/// the checkpoint's manifest gives it and no further, though the live file
/// is longer by then. While the files wait to be copied, the live store
/// deletes all of its own: the copies are whole all the same, because the
/// creation holds each file open. The checkpoint opens with its version.
#[test]
fn where_no_link_can_be_made_the_files_are_copied() {
    for kind in [
        ErrorKind::CrossesDevices,
        ErrorKind::Unsupported,
        ErrorKind::TooManyLinks,
        ErrorKind::PermissionDenied,
    ] {
        let (db, versions) = store_without_links(kind);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(61);
        for round in 0..6 {
            block(&mut world, &mut rng, round * 20, 20);
            turn(&db, &versions);
            if round == 3 {
                (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
            }
        }
        let captured = world.model.clone();
        let target = checkpoints_in(&db).join("1");
        let pause = |step: Step| {
            if step == Step::Secured {
                assert!(building(&target).join("LOCK").exists() && !target.exists());
                assert!(
                    store_files(&StdVfs, &building(&target)).is_empty(),
                    "nothing is copied yet"
                );
                for file in store_files(&StdVfs, &db.dir) {
                    std::fs::remove_file(db.dir.join(file)).expect("a live file is deleted");
                }
                // No recovery finds the live store in what is left.
                db.files_are_gone();
            }
        };
        let created = checkpointer(&db, &versions)
            .pausing(&pause)
            .create(&target)
            .expect("a checkpoint of copies");
        let manifest = read_manifest(&StdVfs, &target).expect("the checkpoint's manifest");
        let files = named(&manifest);
        assert_eq!((created.linked, created.copied), (0, files.len()), "{kind}");
        let mut bytes = 0;
        for (file, len) in &files {
            let copied = std::fs::metadata(target.join(file)).expect("a copy");
            assert_eq!(copied.len(), *len, "{}", file.display());
            assert_eq!(copied.nlink(), 1, "{} is a link", file.display());
            bytes += len;
        }
        assert_eq!(created.copied_bytes, bytes);
        assert_eq!(
            store_files(&StdVfs, &target),
            files.iter().map(|(file, _)| file.clone()).collect()
        );
        drop(world);
        let opened = open(&db, &target);
        assert_eq!(opened.versions.latest(), created.version);
        opened.check(&captured);
    }
}

/// The same on two file systems of the machine, where the test has two:
/// the store is in the temporary directory, and the checkpoint beside the
/// test's own program, which whoever runs the tests on a machine like the
/// project's has on another file system.
#[test]
fn a_checkpoint_on_another_file_system_is_copied() {
    let program = std::env::current_exe().expect("the test's program");
    let elsewhere = program.parent().expect("its directory");
    let device = |path: &Path| std::fs::metadata(path).map(|at| at.dev()).ok();
    if device(elsewhere) == device(&std::env::temp_dir()) {
        // One file system: `where_no_link_can_be_made_the_files_are_copied`
        // is the test.
        return;
    }
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(62);
    for round in 0..2 {
        block(&mut world, &mut rng, round * 10, 10);
        turn(&db, &versions);
    }
    let other = tempfile::tempdir_in(elsewhere).expect("a directory on the other file system");
    let target = other.path().join("checkpoint");
    let created = (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(created.linked == 0 && created.copied > 0, "{created:?}");
    // Each copy is the live file up to the length the manifest gives it.
    let manifest = read_manifest(&StdVfs, &target).expect("the checkpoint's manifest");
    let files = named(&manifest);
    assert_eq!(created.copied, files.len());
    for (file, len) in files {
        let copy = std::fs::read(target.join(&file)).expect("a copy");
        let live = std::fs::read(db.dir.join(&file)).expect("the live file");
        assert_eq!(copy.len() as u64, len, "{}", file.display());
        assert!(live.starts_with(&copy), "{}", file.display());
    }
    // And one beside the store is linked.
    let beside = checkpoints_in(&db).join("1");
    let created = (checkpointer(&db, &versions).create(&beside)).expect("a checkpoint");
    assert!(created.linked > 0 && created.copied == 0, "{created:?}");
    for file in store_files(&StdVfs, &beside) {
        let (ours, theirs) = (db.dir.join(&file), beside.join(&file));
        let inode = |path: &Path| std::fs::metadata(path).expect("a file").ino();
        assert_eq!(inode(&ours), inode(&theirs), "{}", file.display());
    }
}

/// A link that fails for another reason is no reason to copy: the creation
/// fails with the error, and takes away what it made.
#[test]
fn a_link_that_fails_for_another_reason_fails_the_creation() {
    let (db, versions) = store_without_links(ErrorKind::StorageFull);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(63), 0, 10);
    turn(&db, &versions);
    let target = checkpoints_in(&db).join("1");
    let failed = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::Io { action: "link", source, .. })
            if source.kind() == ErrorKind::StorageFull),
        "{failed:?}"
    );
    assert!(!target.exists());
    world.check_committed();
}

/// A checkpoint carries the files the engine names beside the store's
/// own, as links or as copies, and refuses a name that is no plain name of
/// a file of the engine.
#[test]
fn a_checkpoint_carries_the_files_the_engine_names() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(64), 0, 10);
    std::fs::write(db.dir.join("ENGINE"), b"what the engine keeps").expect("a file");
    std::fs::write(db.dir.join("theirs"), b"not the engine's").expect("a file");
    let checkpoints = checkpoints_in(&db);
    let carried = ["ENGINE"];
    let maker = checkpointer(&db, &versions).carrying(&carried);

    let target = checkpoints.join("1");
    let created = maker.create(&target).expect("a checkpoint");
    assert_eq!(
        std::fs::read(target.join("ENGINE")).expect("the carried file"),
        b"what the engine keeps"
    );
    let inode = |path: &Path| std::fs::metadata(path).expect("a file").ino();
    assert_eq!(inode(&target.join("ENGINE")), inode(&db.dir.join("ENGINE")));
    assert!(!target.join("theirs").exists());
    assert_eq!(created.linked, store_files(&StdVfs, &target).len() + 1);
    open(&db, &target).check(&world.model);

    // No plain name, a name of the store's, and what is no file: nothing,
    // a directory, and a link to a file, which would be linked as a link
    // that leads nowhere from the target and copied as the file.
    let refused_names = [
        "",
        ".",
        "..",
        "../ENGINE",
        "wal/00000001.log",
        "MANIFEST",
        "MANIFEST.tmp",
        DELETING_FILE_NAME,
        "LOCK",
        "not-there",
        "wal",
        "link",
    ];
    let refuses = |db: &Db, versions: &Versions| {
        std::os::unix::fs::symlink("ENGINE", db.dir.join("link")).expect("a link");
        let checkpoints = checkpoints_in(db);
        for name in refused_names {
            let names = ["ENGINE", name];
            let target = checkpoints.join("refused");
            let refused = checkpointer(db, versions).carrying(&names).create(&target);
            assert!(
                matches!(&refused, Err(CheckpointError::CarriedName { name: said }) if said == name),
                "{name:?}: {refused:?}"
            );
            assert!(!target.exists(), "{name:?} left a directory");
        }
    };
    refuses(&db, &versions);

    // Where no link can be made, the file is copied whole, and the same
    // names are refused.
    let (db, versions) = store_without_links(ErrorKind::CrossesDevices);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(65), 0, 10);
    std::fs::write(db.dir.join("ENGINE"), b"what the engine keeps").expect("a file");
    refuses(&db, &versions);
    let target = checkpoints_in(&db).join("1");
    let created = (checkpointer(&db, &versions)
        .carrying(&carried)
        .create(&target))
    .expect("a checkpoint of copies");
    assert_eq!(created.linked, 0);
    assert_eq!(
        std::fs::read(target.join("ENGINE")).expect("the carried file"),
        b"what the engine keeps"
    );
    assert_ne!(inode(&target.join("ENGINE")), inode(&db.dir.join("ENGINE")));
}

/// "Deleting one removes its directory."
///
/// The directory goes with everything in it, the caller's files and a
/// directory of theirs among them, and the live store is as it was. What is
/// no database of this engine is refused and left as it is, and so is a
/// checkpoint that a handle has open.
#[test]
fn deleting_a_checkpoint_removes_its_directory() {
    let (db, versions) = store_with_log(24 << 10, 128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(66);
    for round in 0..4 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    let checkpoints = checkpoints_in(&db);
    let target = checkpoints.join("1");
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    std::fs::write(target.join("platform_state.bin"), b"the caller's").expect("a file");
    std::fs::create_dir(target.join("theirs")).expect("a directory");
    std::fs::write(target.join("theirs").join("more"), b"the caller's").expect("a file");
    let live = store_files(&StdVfs, &db.dir);

    // A handle has it open: the lock of its directory is held.
    let held = StdVfs.lock(&target.join("LOCK")).expect("the lock");
    let refused = checkpoint::delete(&StdVfs, &target);
    assert!(
        matches!(&refused, Err(CheckpointError::Open { dir }) if *dir == target),
        "{refused:?}"
    );
    assert!(target.join("MANIFEST").exists() && target.join("theirs").join("more").exists());
    drop(held);

    checkpoint::delete(&StdVfs, &target).expect("the checkpoint is deleted");
    assert!(!target.exists());
    assert_eq!(store_files(&StdVfs, &db.dir), live);
    world.check_committed();
    block(&mut world, &mut rng, 4 * 20, 20);
    world.check_committed();

    // What a deletion leaves that was cut short is finished: a directory
    // with the renamed manifest and whatever else, a manifest that was
    // made beside it since among that, or the same manifest under its own
    // name still; an empty directory; and one with an empty lock file
    // alone.
    let a_manifest = std::fs::read(db.dir.join("MANIFEST")).expect("a manifest");
    let leftover = checkpoints.join("leftover");
    let put = |files: &[(&str, &[u8])]| {
        std::fs::create_dir(&leftover).expect("a directory");
        for (name, bytes) in files {
            let path = leftover.join(name);
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
            std::fs::write(path, bytes).expect("a file");
        }
    };
    let leftovers: [&[(&str, &[u8])]; 6] = [
        &[
            (DELETING_FILE_NAME, &a_manifest),
            ("seg/00000001.seg", b"left"),
            ("LOCK", b""),
        ],
        &[
            (DELETING_FILE_NAME, &a_manifest),
            ("MANIFEST", b"made since"),
        ],
        &[(DELETING_FILE_NAME, &a_manifest), ("ENGINE", b"beside it")],
        &[],
        &[("LOCK", b"")],
        // A database that is known by its manifest, with a file that is
        // only named as a renamed one: that file goes with the rest.
        &[
            ("MANIFEST", &a_manifest),
            (DELETING_FILE_NAME, b"no manifest"),
        ],
    ];
    for (index, files) in leftovers.into_iter().enumerate() {
        put(files);
        if index == 2 {
            // One file under both names: a rename of the one to the other
            // changes nothing.
            let renamed = leftover.join(DELETING_FILE_NAME);
            std::fs::hard_link(renamed, leftover.join("MANIFEST")).expect("a second name");
        }
        checkpoint::delete(&StdVfs, &leftover).expect("the leftover is deleted");
        assert!(!leftover.exists(), "leftover {index}");
    }
    // The lock file alone, and somebody has its lock.
    std::fs::create_dir(&leftover).expect("a directory");
    let held = StdVfs.lock(&leftover.join("LOCK")).expect("the lock");
    let refused = checkpoint::delete(&StdVfs, &leftover);
    assert!(
        matches!(&refused, Err(CheckpointError::Open { dir }) if *dir == leftover),
        "{refused:?}"
    );
    drop(held);
    checkpoint::delete(&StdVfs, &leftover).expect("the leftover is deleted");
    assert!(!leftover.exists());

    // No database, and nothing of it is touched, the lock file included: a
    // directory of somebody else's is not known for one of this engine by
    // the names in it. A lock file that holds something; files that are
    // named as a renamed manifest and as a manifest and hold something
    // else; a directory of such a name; and a link of such a name to a
    // manifest.
    let elsewhere = checkpoints.join("a-manifest");
    std::fs::write(&elsewhere, &a_manifest).expect("a file");
    let not_ours: [&[(&str, &[u8])]; 4] = [
        &[("LOCK", b"as it was")],
        &[(DELETING_FILE_NAME, b"renamed"), ("theirs", b"as it was")],
        &[
            ("MANIFEST", b"not one"),
            ("theirs", b"as it was"),
            ("LOCK", b""),
        ],
        &[("MANIFEST.deleting/in-it", b"as it was")],
    ];
    let refused_and_kept = |what: &str| {
        let kept = everything_in(&StdVfs, &leftover);
        let refused = checkpoint::delete(&StdVfs, &leftover);
        assert!(
            matches!(&refused, Err(CheckpointError::NotADatabase { dir, .. }) if *dir == leftover),
            "{what}: {refused:?}"
        );
        assert_eq!(everything_in(&StdVfs, &leftover), kept, "{what}");
        std::fs::remove_dir_all(&leftover).expect("the directory is removed");
    };
    for (index, files) in not_ours.into_iter().enumerate() {
        put(files);
        refused_and_kept(&format!("directory {index}"));
    }
    for name in ["MANIFEST", DELETING_FILE_NAME] {
        put(&[("theirs", b"as it was")]);
        std::os::unix::fs::symlink(&elsewhere, leftover.join(name)).expect("a link");
        refused_and_kept(&format!("a link named {name}"));
    }

    // No directory of its own: nothing, a file, a path with no name, and
    // a link to a checkpoint, which is not followed into it.
    let second = checkpoints.join("2");
    (checkpointer(&db, &versions).create(&second)).expect("a checkpoint");
    let held = everything_in(&StdVfs, &second);
    let (link, file) = (checkpoints.join("link"), checkpoints.join("file"));
    std::os::unix::fs::symlink("2", &link).expect("a link");
    std::fs::write(&file, b"a file").expect("a file");
    let nothing = checkpoints.join("nothing");
    for path in [&nothing, &file, &link, Path::new("/"), Path::new("")] {
        let refused = checkpoint::delete(&StdVfs, path);
        assert!(
            matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if dir == path),
            "{}: {refused:?}",
            path.display()
        );
    }
    assert!(link.is_symlink() && file.exists());
    assert_eq!(everything_in(&StdVfs, &second), held);
    open(&db, &second).check(&world.model);
}

/// What a directory is, is asked again under its lock: one that stopped
/// being a database between the first look and the lock is left as it is,
/// with what was put into it meanwhile.
#[test]
fn a_directory_that_changes_before_its_lock_is_taken_is_not_deleted() {
    let (db, versions) = store_with_log(24 << 10, 128);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(70), 0, 20);
    turn(&db, &versions);
    let target = checkpoints_in(&db).join("1");
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    let files = store_files(&StdVfs, &target);

    let theirs = target.clone();
    let meddling = Meddling::before_lock(
        Arc::new(StdVfs),
        Box::new(move |_| {
            std::fs::remove_file(theirs.join("MANIFEST"))?;
            std::fs::write(theirs.join("theirs"), b"put there meanwhile")
        }),
    );
    let refused = checkpoint::delete(&meddling, &target);
    assert!(
        matches!(&refused, Err(CheckpointError::NotADatabase { dir, .. }) if *dir == target),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read(target.join("theirs")).expect("their file"),
        b"put there meanwhile"
    );
    assert_eq!(store_files(&StdVfs, &target), files);
    assert!(!target.join(DELETING_FILE_NAME).exists());
}

/// A checkpoint on the simulated file system with the files that the
/// engine and Platform keep beside the store's (P-17), all durable, and
/// the model of what it holds.
struct Kept {
    fs: SimVfs,
    db: Arc<Db>,
    model: Committed,
    target: PathBuf,
    /// Everything in the checkpoint's directory.
    held: BTreeMap<PathBuf, Option<Vec<u8>>>,
}

fn kept(seed: u64, twice_named: bool) -> Kept {
    let fs = sim(seed);
    let (db, versions) = store_on(&fs);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(seed), 0, 10);
    (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
    let model = world.model.clone();
    drop(world);
    let target = checkpoints_in(&db).join("1");
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    write_file(&fs, &target.join("ENGINE"), b"what the engine keeps");
    write_file(&fs, &target.join("platform_state.bin"), b"the caller's");
    fs.create_dir(&target.join("theirs")).expect("a directory");
    write_file(&fs, &target.join("theirs").join("more"), b"the caller's");
    if twice_named {
        // The manifest has a second name, that of one that is being
        // deleted: a rename of the one to the other changes nothing.
        let renamed = target.join(DELETING_FILE_NAME);
        (fs.hard_link(&target.join("MANIFEST"), &renamed)).expect("a second name");
    }
    (fs.sync_dir(&target.join("theirs"))).expect("the caller's file is durable");
    (fs.sync_dir(&target)).expect("the files beside the store's are durable");
    let held = everything_in(&fs, &target);
    Kept {
        fs,
        db,
        model,
        target,
        held,
    }
}

/// What a deletion that did not end has left, in the state `left` of the
/// file system: the directory is gone, or it is the whole checkpoint, or
/// it is no database.
#[derive(Debug, Default)]
struct LeftOf {
    gone: usize,
    whole: usize,
    none: usize,
}

impl Kept {
    /// Checks what is left of the checkpoint in `left`, and makes the
    /// deletion again there. A directory that has its manifest has every
    /// file it had, the ones beside the store's among them, and `opens`
    /// says whether it is also opened.
    fn check_left(&self, left: &SimVfs, opens: bool, context: &str, seen: &mut LeftOf) {
        if left.read_dir(&self.target).is_err() {
            seen.gone += 1;
            return;
        }
        let entries = left.read_dir(&self.target).expect("the directory");
        if entries.iter().any(|entry| entry.name == "MANIFEST") {
            // Whatever has the manifest's name has every file with it.
            read_manifest(left, &self.target)
                .unwrap_or_else(|error| panic!("{context}: the manifest: {error}"));
            assert_eq!(everything_in(left, &self.target), self.held, "{context}");
            if opens {
                let vfs: Arc<dyn Vfs> = Arc::new(left.clone());
                let opened = open_on(vfs, &self.target, &self.db)
                    .unwrap_or_else(|error| panic!("{context}: the checkpoint: {error}"));
                opened.check(&self.model);
            }
            seen.whole += 1;
        } else {
            let marked = entries.iter().any(|entry| entry.name == DELETING_FILE_NAME);
            assert!(marked || entries.is_empty(), "{context}: {entries:?}");
            seen.none += 1;
        }
        checkpoint::delete(left, &self.target).unwrap_or_else(|error| panic!("{context}: {error}"));
        assert!(left.read_dir(&self.target).is_err(), "{context}");
        // And durably: a power cut that loses every unsynced change does
        // not bring it back.
        let image = left.crash_image();
        let after = image.recover(&image.lose_all()).expect("a state");
        assert!(after.read_dir(&self.target).is_err(), "{context}");
    }
}

/// Each operation of a deletion is failed in turn. What is left opens as
/// the whole checkpoint, with every file that was beside the store's, or
/// has nothing under the manifest's name and is known for a deletion that
/// was cut short: never a database with something missing. The deletion
/// can be made again, and is durable when it returns. The same holds for
/// a checkpoint whose manifest has the name of a renamed one as well.
#[test]
fn a_deletion_that_the_file_layer_fails_leaves_the_checkpoint_or_no_database() {
    for twice_named in [false, true] {
        a_deletion_is_failed_at_every_operation(twice_named);
    }
}

fn a_deletion_is_failed_at_every_operation(twice_named: bool) {
    use lignin_io::sim::{Fault, FaultRule};

    let kept = |seed| kept(seed, twice_named);
    let whole = kept(67);
    let from = whole.fs.stats().ops;
    checkpoint::delete(&whole.fs, &whole.target).expect("the checkpoint is deleted");
    let ops = whole.fs.stats().ops - from;
    assert!(ops > 12, "a deletion of {ops} operations");
    let image = whole.fs.crash_image();
    let left = image.recover(&image.lose_all()).expect("a state");
    assert!(left.read_dir(&whole.target).is_err(), "it is not durable");

    let mut seen = LeftOf::default();
    for at in 1..=ops {
        let kept = kept(67);
        assert_eq!(kept.fs.stats().ops, from, "the same store");
        (kept.fs).add_rule(FaultRule::new(Fault::Eio).from_op(from + at));
        let deleted = checkpoint::delete(&kept.fs, &kept.target);
        kept.fs.clear_rules();
        let context = format!("a fault at operation {at} of {ops}: {deleted:?}");
        if deleted.is_ok() {
            assert!(kept.fs.read_dir(&kept.target).is_err(), "{context}");
            continue;
        }
        kept.check_left(&kept.fs, true, &context, &mut seen);
    }
    assert!(seen.whole > 2 && seen.none > 4 && seen.gone > 0, "{seen:?}");
}

/// The power is cut after each operation of a deletion. In each state
/// that is looked at of those the crash can leave, the directory is gone,
/// or it is the whole checkpoint with every file that was beside the
/// store's, or it has nothing under the manifest's name: never a database
/// with something missing. In each, the deletion can be made again and
/// ends it. The same holds for a checkpoint whose manifest has the name of
/// a renamed one as well.
///
/// The states are those of both ends, of a file system that writes in
/// order, of one change lost at a time, and eight drawn ones, for every
/// cut. The first two of a cut are also opened.
#[test]
fn a_deletion_that_the_power_cuts_leaves_the_checkpoint_or_no_database() {
    for twice_named in [false, true] {
        a_deletion_is_cut_at_every_operation(twice_named);
    }
}

fn a_deletion_is_cut_at_every_operation(twice_named: bool) {
    use lignin_io::sim::CrashPoint;

    let kept = |seed| kept(seed, twice_named);
    let whole = kept(68);
    let from = whole.fs.stats().ops;
    checkpoint::delete(&whole.fs, &whole.target).expect("the checkpoint is deleted");
    let ops = whole.fs.stats().ops - from;
    assert!(ops > 12, "a deletion of {ops} operations");

    let mut seen = LeftOf::default();
    for cut in 1..=ops {
        let kept = kept(68);
        assert_eq!(kept.fs.stats().ops, from, "the same store");
        kept.fs.crash_at(CrashPoint::AfterOp(from + cut));
        let deleted = checkpoint::delete(&kept.fs, &kept.target);
        assert_eq!(
            deleted.is_ok(),
            cut == ops,
            "cut {cut} of {ops}: {deleted:?}"
        );
        let image = kept.fs.crash_image();
        let mut plans = vec![image.lose_all(), image.keep_all()];
        plans.extend(image.prefixes());
        plans.extend(image.holes());
        plans.extend((0..8).map(|draw| image.sample(draw)));
        for (index, plan) in plans.iter().enumerate() {
            let Ok(left) = image.recover(plan) else {
                continue;
            };
            let context = format!("cut {cut} of {ops}, crash plan {index}");
            let gone = seen.gone;
            kept.check_left(&left, index < 2, &context, &mut seen);
            assert!(
                deleted.is_err() || seen.gone > gone,
                "{context}: the deletion came back"
            );
        }
    }
    assert!(seen.whole > 0 && seen.none > 0 && seen.gone > 0, "{seen:?}");
}

/// Where somebody else has the lock of the directory a checkpoint is built
/// in, another creation is building there: the creation fails and touches
/// nothing in it, and neither does a deletion. Where the lock fails
/// otherwise, the directory that was just made goes.
#[test]
fn a_directory_that_another_creation_builds_in_is_left_to_it() {
    let theirs: Arc<Mutex<Vec<Box<dyn VfsLock>>>> = Arc::default();
    let (taken, came) = (theirs.clone(), AtomicBool::new(false));
    // Another creation that finds the directory as soon as it is made,
    // takes its lock and begins to build in it. It comes once.
    let (db, versions) = store_with_locks_met(Box::new(move |lock| {
        if came.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let held = StdVfs.lock(lock)?;
        taken.lock().expect("their locks").push(held);
        std::fs::write(lock.with_file_name("MANIFEST"), b"theirs")
    }));
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(71), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let built = building(&target);
    let failed = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::Creating { dir }) if *dir == built),
        "{failed:?}"
    );
    assert_eq!(theirs.lock().expect("their locks").len(), 1);
    let left = BTreeMap::from([
        (PathBuf::from("LOCK"), Some(Vec::new())),
        (PathBuf::from("MANIFEST"), Some(b"theirs".to_vec())),
    ]);
    assert_eq!(everything_in(&StdVfs, &built), left);
    assert!(!target.exists());
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
    for path in [&target, &built] {
        let refused = checkpoint::delete(&StdVfs, path);
        assert!(
            matches!(&refused, Err(CheckpointError::Creating { dir }) if *dir == built),
            "{refused:?}"
        );
        assert_eq!(everything_in(&StdVfs, &built), left);
    }
    // Once the lock is free, the directory is what a creation left.
    theirs.lock().expect("their locks").clear();
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(!built.exists());
    open(&db, &target).check(&world.model);

    // A deletion of a checkpoint that is there deletes it, and leaves
    // alone a directory beside it whose lock is held.
    std::fs::create_dir(&built).expect("a directory");
    std::fs::write(built.join("theirs"), b"as it was").expect("a file");
    let held = StdVfs.lock(&built.join("LOCK")).expect("the lock");
    let left = everything_in(&StdVfs, &built);
    checkpoint::delete(&StdVfs, &target).expect("the checkpoint is deleted");
    assert!(!target.exists());
    assert_eq!(everything_in(&StdVfs, &built), left);
    drop(held);

    let (db, versions) = store_with_locks_met(Box::new(|_| Err(ErrorKind::Other.into())));
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(72), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let failed = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::Io { action: "lock", source, .. })
            if source.kind() == ErrorKind::Other),
        "{failed:?}"
    );
    assert!(!target.exists() && !building(&target).exists());
    world.check_committed();
}

/// What a creation that was cut short left under the name it built under
/// is emptied by the next creation at the same target, which builds in it,
/// and removed by a deletion: of the target, with or without a checkpoint
/// there, and of the directory itself. Whatever it holds goes. What has
/// the name and is no directory of its own is not looked into, and no
/// checkpoint is made at a name of that kind.
#[test]
fn what_a_creation_left_is_emptied_by_the_next_and_removed_by_a_deletion() {
    let (db, versions) = store_with_log(24 << 10, 128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(75);
    for round in 0..3 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    let checkpoints = checkpoints_in(&db);
    let target = checkpoints.join("1");
    let built = building(&target);
    assert_eq!(built, checkpoints.join(format!("1{CREATING_SUFFIX}")));
    let a_manifest = std::fs::read(db.dir.join("MANIFEST")).expect("a manifest");
    // A part of a checkpoint, a whole manifest among it, a file under the
    // name of one the creation links, and what somebody else put there.
    let leave = || {
        let files: [(&str, &[u8]); 5] = [
            ("LOCK", b""),
            ("MANIFEST", &a_manifest),
            ("seg/00000001.seg", b"left"),
            ("val/99999999.val", b"left"),
            ("theirs/more", b"somebody's"),
        ];
        for (name, bytes) in files {
            let path = built.join(name);
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
            std::fs::write(path, bytes).expect("a file");
        }
    };

    // The next creation at the same target.
    leave();
    let created = (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(!built.exists());
    let manifest = read_manifest(&StdVfs, &target).expect("the checkpoint's manifest");
    let mut held: Vec<PathBuf> = named(&manifest).into_iter().map(|(file, _)| file).collect();
    held.extend(["LOCK", "MANIFEST"].map(PathBuf::from));
    held.sort();
    let found: Vec<PathBuf> = (everything_in(&StdVfs, &target).into_iter())
        .filter_map(|(path, bytes)| bytes.map(|_| path))
        .collect();
    assert_eq!(
        found, held,
        "the checkpoint holds what it names and no more"
    );
    assert_eq!(created.linked + 2, held.len());
    // Its manifest is the file that the store writes for the same
    // manifest as the first one of a directory.
    let elsewhere = tempfile::tempdir().expect("a directory");
    ManifestStore::create(Arc::new(StdVfs), elsewhere.path(), manifest)
        .expect("the store writes the manifest");
    assert_eq!(
        std::fs::read(target.join("MANIFEST")).expect("the checkpoint's manifest"),
        std::fs::read(elsewhere.path().join("MANIFEST")).expect("the store's")
    );
    open(&db, &target).check(&world.model);

    // A deletion of the target removes the checkpoint and what is left
    // beside it; with no checkpoint there, what is left alone.
    leave();
    checkpoint::delete(&StdVfs, &target).expect("both are deleted");
    assert!(!target.exists() && !built.exists());
    leave();
    checkpoint::delete(&StdVfs, &target).expect("what was left is deleted");
    assert!(!built.exists());
    // A deletion of the directory itself, also of an empty one.
    leave();
    checkpoint::delete(&StdVfs, &built).expect("what was left is deleted");
    assert!(!built.exists());
    std::fs::create_dir(&built).expect("a directory");
    checkpoint::delete(&StdVfs, &built).expect("what was left is deleted");
    assert!(!built.exists());
    for path in [&target, &built] {
        let refused = checkpoint::delete(&StdVfs, path);
        assert!(
            matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if dir == path),
            "{refused:?}"
        );
    }

    // A link of that name to a directory, and a file of that name: neither
    // is a creation's, and nothing is removed through or of them.
    let elsewhere = checkpoints.join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("a directory");
    std::fs::write(elsewhere.join("kept"), b"as it was").expect("a file");
    let kept = everything_in(&StdVfs, &elsewhere);
    std::os::unix::fs::symlink("elsewhere", &built).expect("a link");
    for round in 0..2 {
        let failed = checkpointer(&db, &versions).create(&target);
        assert!(
            matches!(&failed, Err(CheckpointError::NotADirectory { dir }) if *dir == built),
            "{failed:?}"
        );
        for path in [&target, &built] {
            let refused = checkpoint::delete(&StdVfs, path);
            assert!(
                matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if dir == path),
                "{refused:?}"
            );
        }
        assert!(!target.exists());
        assert_eq!(everything_in(&StdVfs, &elsewhere), kept);
        if round == 0 {
            assert!(built.is_symlink());
            std::fs::remove_file(&built).expect("the link is removed");
            std::fs::write(&built, b"a file").expect("a file");
        }
    }
    assert_eq!(std::fs::read(&built).expect("the file"), b"a file");
    std::fs::remove_file(&built).expect("the file is removed");

    // A file at the target: nothing is deleted, and what a creation left
    // beside it stays as well.
    leave();
    std::fs::write(&target, b"a file").expect("a file");
    let kept = everything_in(&StdVfs, &built);
    let refused = checkpoint::delete(&StdVfs, &target);
    assert!(
        matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if *dir == target),
        "{refused:?}"
    );
    assert_eq!(everything_in(&StdVfs, &built), kept);
    std::fs::remove_file(&target).expect("the file is removed");
    checkpoint::delete(&StdVfs, &built).expect("what was left is deleted");

    // No checkpoint is made at a name that creations build under.
    let refused = checkpointer(&db, &versions).create(&built);
    assert!(
        matches!(&refused, Err(CheckpointError::ReservedName { target }) if *target == built),
        "{refused:?}"
    );
    assert!(!built.exists() && !building(&built).exists());
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// A creation that fails takes away the files it made and no other: a file
/// that somebody put into the directory it builds in, under a name the
/// creation was about to use, fails the creation and stays, with the
/// directory, for the next creation at the target to remove.
#[test]
fn a_file_that_somebody_else_put_where_a_checkpoint_is_built_is_not_removed() {
    let (db, versions) = store_without_links(ErrorKind::CrossesDevices);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(73);
    for round in 0..3 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
    let target = checkpoints_in(&db).join("1");
    let built = building(&target);
    // The first file of the log is copied after the table checkpoint, the
    // segments and the value-log files.
    let files = store_files(&StdVfs, &db.dir);
    let theirs = (files.iter())
        .find(|file| file.starts_with("wal"))
        .expect("a log file");
    assert!(files.iter().any(|file| !file.starts_with("wal")));
    let pause = |step: Step| {
        if step == Step::Secured {
            std::fs::write(built.join(theirs), b"theirs").expect("their file");
            std::fs::write(built.join("beside"), b"theirs too").expect("their file");
        }
    };
    let failed = checkpointer(&db, &versions).pausing(&pause).create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::Io { action: "create", source, .. })
            if source.kind() == ErrorKind::AlreadyExists),
        "{failed:?}"
    );
    // The copies that were made are gone, and the directories that have
    // nothing in them. Their files are as they were.
    let left = BTreeMap::from([
        (PathBuf::from("beside"), Some(b"theirs too".to_vec())),
        (PathBuf::from("wal"), None),
        (theirs.clone(), Some(b"theirs".to_vec())),
    ]);
    assert_eq!(everything_in(&StdVfs, &built), left);
    assert!(!target.exists());
    world.check_committed();

    // A manifest that somebody wrote there is theirs too: the creation
    // writes none over it, fails, and leaves it.
    let second = checkpoints_in(&db).join("2");
    let built = building(&second);
    let pause = |step: Step| {
        if step == Step::Secured {
            std::fs::write(built.join("MANIFEST"), b"theirs").expect("their file");
        }
    };
    let failed = checkpointer(&db, &versions).pausing(&pause).create(&second);
    let manifest = built.join(ManifestStore::FILE_NAME);
    assert!(
        matches!(&failed, Err(CheckpointError::Io { action: "create", path, source })
            if *path == manifest && source.kind() == ErrorKind::AlreadyExists),
        "{failed:?}"
    );
    let left = BTreeMap::from([(PathBuf::from("MANIFEST"), Some(b"theirs".to_vec()))]);
    assert_eq!(everything_in(&StdVfs, &built), left);
    assert!(!second.exists());
    world.check_committed();

    // The next creation at each target takes the directory as its own.
    for target in [&target, &second] {
        (checkpointer(&db, &versions).create(target)).expect("a checkpoint");
        assert!(!building(target).exists());
        open(&db, target).check(&world.model);
    }
}

/// A target of which the file layer does not say whether it is there is
/// refused before anything is made for it: only a name that is known to
/// be free is given to a checkpoint.
#[test]
fn a_target_that_cannot_be_looked_at_is_refused() {
    let (db, versions) = store_on_meddling(|plain, checkpoints| {
        let target = checkpoints.join("1");
        let unseen = move |dir: &Path| match dir == target {
            true => Err(ErrorKind::PermissionDenied.into()),
            false => Ok(()),
        };
        Meddling::before_list(plain, Box::new(unseen))
    });
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(78), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let refused = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&refused, Err(CheckpointError::Io { action: "look at", path, source })
            if *path == target && source.kind() == ErrorKind::PermissionDenied),
        "{refused:?}"
    );
    assert!(!target.exists() && !building(&target).exists());
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// The name of the target is looked at again before the directory gets
/// it: what has come there while the checkpoint was built is not replaced.
/// And only the directory the creation built in is renamed: where another
/// is at its path by then, nothing gets the target's name and nothing of
/// the other is removed.
#[test]
fn a_target_that_is_taken_while_the_checkpoint_is_built_is_refused() {
    let (db, versions) = store_with_log(24 << 10, 128);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(76), 0, 20);
    turn(&db, &versions);
    let checkpoints = checkpoints_in(&db);

    type Put = fn(&Path) -> std::io::Result<()>;
    let puts: [(&str, Put); 3] = [
        ("an empty directory", |target| std::fs::create_dir(target)),
        ("a directory with a file", |target| {
            std::fs::create_dir(target)?;
            std::fs::write(target.join("kept"), b"as it was")
        }),
        ("a file", |target| std::fs::write(target, b"as it was")),
    ];
    for (index, (what, put)) in puts.into_iter().enumerate() {
        let target = checkpoints.join(format!("taken-{index}"));
        let pause = |step: Step| {
            if step == Step::Settled {
                assert!(
                    !target.exists(),
                    "{what}: the target is there before the rename"
                );
                put(&target).expect("it is put there");
            }
        };
        let refused = checkpointer(&db, &versions).pausing(&pause).create(&target);
        assert!(
            matches!(&refused, Err(CheckpointError::TargetExists { target: named })
                if *named == target),
            "{what}: {refused:?}"
        );
        assert!(!building(&target).exists(), "{what}");
        match target.is_dir() {
            true => {
                let kept: Vec<_> = (everything_in(&StdVfs, &target).into_keys()).collect();
                let held: &[&str] = if index == 1 { &["kept"] } else { &[] };
                assert_eq!(kept, held.iter().map(PathBuf::from).collect::<Vec<_>>());
            }
            false => assert_eq!(std::fs::read(&target).expect("the file"), b"as it was"),
        }
        assert_eq!(versions.pins_held(), 0, "{what}");
    }

    // The directory that was built in is moved away, and another is put
    // under its name.
    let target = checkpoints.join("replaced");
    let (built, away) = (building(&target), checkpoints.join("away"));
    let pause = |step: Step| {
        if step == Step::Settled {
            std::fs::rename(&built, &away).expect("it is moved away");
            std::fs::create_dir(&built).expect("another directory");
            for name in ["LOCK", "MANIFEST", "theirs"] {
                std::fs::write(built.join(name), b"as it was").expect("a file");
            }
        }
    };
    let refused = checkpointer(&db, &versions).pausing(&pause).create(&target);
    assert!(
        matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
        "{refused:?}"
    );
    assert!(!target.exists());
    // Nothing of the other directory is removed, also under the names of
    // what the creation made in its own.
    let left: BTreeMap<_, _> = ["LOCK", "MANIFEST", "theirs"]
        .map(|name| (PathBuf::from(name), Some(b"as it was".to_vec())))
        .into();
    assert_eq!(everything_in(&StdVfs, &built), left);
    // What was built is whole where it was moved to, and nothing of it
    // was removed.
    open(&db, &away).check(&world.model);
    world.check_committed();
}

/// The directory a checkpoint is built in is looked at right before it
/// gets the name of the target: it has to hold what the creation put into
/// it and nothing else, each a file or a directory of its own, and has to
/// be a directory of its own itself. Somebody removes a linked file, adds
/// a file, renames one, puts a link where a linked file was, where the
/// lock file was, where a directory of it was, or where the directory
/// itself was: nothing gets the name of the target, and the rollback
/// removes no link and nothing through one.
#[test]
fn a_directory_that_is_changed_while_a_checkpoint_is_built_in_it_gets_no_name() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Change {
        ALinkedFileIsRemoved,
        AFileIsAdded,
        AFileIsAddedBesideTheManifest,
        ALinkedFileIsRenamed,
        ALinkedFileIsALink,
        TheLockFileIsALink,
        ADirectoryInItIsALink,
        ItIsALink,
    }
    let (db, versions) = store_with_log(24 << 10, 128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(79);
    for round in 0..3 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
    let checkpoints = checkpoints_in(&db);
    let outside = tempfile::tempdir().expect("a directory");
    let kept = BTreeMap::from([(PathBuf::from("kept"), Some(b"as it was".to_vec()))]);

    for (index, change) in [
        Change::ALinkedFileIsRemoved,
        Change::AFileIsAdded,
        Change::AFileIsAddedBesideTheManifest,
        Change::ALinkedFileIsRenamed,
        Change::ALinkedFileIsALink,
        Change::TheLockFileIsALink,
        Change::ADirectoryInItIsALink,
        Change::ItIsALink,
    ]
    .into_iter()
    .enumerate()
    {
        let target = checkpoints.join(format!("changed-{index}"));
        let built = building(&target);
        let away = outside.path().join(format!("away-{index}"));
        let theirs = outside.path().join(format!("theirs-{index}"));
        std::fs::create_dir(&theirs).expect("a directory");
        std::fs::write(theirs.join("kept"), b"as it was").expect("a file");
        let pause = |step: Step| {
            if step != Step::Settled {
                return;
            }
            let files = store_files(&StdVfs, &built);
            let a_segment = (files.iter())
                .find(|file| file.starts_with("seg"))
                .expect("a segment is linked");
            match change {
                Change::ALinkedFileIsRemoved => {
                    std::fs::remove_file(built.join(a_segment)).expect("it is removed")
                }
                Change::AFileIsAdded => {
                    std::fs::write(built.join("seg").join("99999999.seg"), b"theirs")
                        .expect("a file")
                }
                Change::AFileIsAddedBesideTheManifest => {
                    std::fs::write(built.join("theirs"), b"theirs").expect("a file")
                }
                Change::ALinkedFileIsRenamed => {
                    let renamed = built.join("seg").join("99999999.seg");
                    std::fs::rename(built.join(a_segment), renamed).expect("it is renamed");
                }
                Change::ALinkedFileIsALink => {
                    std::fs::remove_file(built.join(a_segment)).expect("it is removed");
                    std::os::unix::fs::symlink(theirs.join("kept"), built.join(a_segment))
                        .expect("a link");
                }
                Change::TheLockFileIsALink => {
                    std::fs::rename(built.join("LOCK"), &away).expect("it is moved away");
                    std::os::unix::fs::symlink(&away, built.join("LOCK")).expect("a link");
                }
                Change::ADirectoryInItIsALink => {
                    // Where the link leads, there are files of the names
                    // that the creation linked.
                    std::fs::rename(built.join("seg"), &away).expect("it is moved away");
                    for linked in std::fs::read_dir(&away).expect("the directory") {
                        let name = linked.expect("an entry").file_name();
                        std::fs::write(theirs.join(name), b"as it was").expect("a file");
                    }
                    std::os::unix::fs::symlink(&theirs, built.join("seg")).expect("a link");
                }
                Change::ItIsALink => {
                    std::fs::rename(&built, &away).expect("it is moved away");
                    std::os::unix::fs::symlink(&away, &built).expect("a link");
                }
            }
        };
        let refused = checkpointer(&db, &versions).pausing(&pause).create(&target);
        assert!(
            matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
            "{change:?}: {refused:?}"
        );
        assert!(!target.exists(), "{change:?}");
        assert_eq!(versions.pins_held(), 0, "{change:?}");
        let mut kept = kept.clone();
        if change == Change::ADirectoryInItIsALink {
            for linked in std::fs::read_dir(&away).expect("the directory") {
                let name = linked.expect("an entry").file_name();
                kept.insert(PathBuf::from(name), Some(b"as it was".to_vec()));
            }
            assert!(kept.len() > 1, "the segments that were moved away");
        }
        assert_eq!(everything_in(&StdVfs, &theirs), kept, "{change:?}");
        match change {
            Change::ALinkedFileIsRemoved => assert!(!built.exists()),
            Change::AFileIsAdded => {
                let left = BTreeMap::from([
                    (PathBuf::from("seg"), None),
                    (PathBuf::from("seg/99999999.seg"), Some(b"theirs".to_vec())),
                ]);
                assert_eq!(everything_in(&StdVfs, &built), left);
            }
            Change::AFileIsAddedBesideTheManifest => {
                let left = BTreeMap::from([(PathBuf::from("theirs"), Some(b"theirs".to_vec()))]);
                assert_eq!(everything_in(&StdVfs, &built), left);
            }
            // As many files as were linked, and one under a name that
            // the creation did not give: it stays.
            Change::ALinkedFileIsRenamed => {
                let left: Vec<PathBuf> = everything_in(&StdVfs, &built).into_keys().collect();
                assert_eq!(left, ["seg", "seg/99999999.seg"].map(PathBuf::from));
            }
            // The link is theirs, and stays, alone in its directory.
            Change::ALinkedFileIsALink => {
                let left: Vec<PathBuf> = std::fs::read_dir(built.join("seg"))
                    .expect("the directory")
                    .map(|entry| entry.expect("an entry").path())
                    .collect();
                assert!(matches!(&left[..], [link] if link.is_symlink()), "{left:?}");
                let beside: Vec<_> = (std::fs::read_dir(&built).expect("the directory"))
                    .map(|entry| entry.expect("an entry").file_name())
                    .collect();
                assert_eq!(beside, ["seg"]);
            }
            // The directory is not known by a lock file that is a link:
            // nothing of it is removed, and it is not locked again before
            // the link is gone.
            Change::TheLockFileIsALink => {
                assert!(built.join("LOCK").is_symlink() && built.join("MANIFEST").exists());
                assert!(!store_files(&StdVfs, &built).is_empty());
                let refused = checkpointer(&db, &versions).create(&target);
                assert!(
                    matches!(&refused, Err(CheckpointError::NotALock { .. })),
                    "{refused:?}"
                );
                std::fs::remove_file(built.join("LOCK")).expect("the link is removed");
            }
            // The link stays, and nothing is removed through it.
            Change::ADirectoryInItIsALink => assert!(built.join("seg").is_symlink()),
            Change::ItIsALink => {
                // Nothing of what was built is removed through the link.
                assert!(built.is_symlink());
                open(&db, &away).check(&world.model);
                std::fs::remove_file(&built).expect("the link is removed");
            }
        }
        // What is left is taken over by the next creation at the target,
        // which removes a link and does not follow it.
        (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
        assert!(!built.exists(), "{change:?}");
        assert_eq!(everything_in(&StdVfs, &theirs), kept, "{change:?}");
        open(&db, &target).check(&world.model);
    }
    world.check_committed();
}

/// What a creation left is looked at again under its lock, by a deletion
/// and by the next creation: where somebody puts a link in its place
/// between the first look and the lock, the look under the lock does not
/// find the directory whose lock was taken, and nothing that the link
/// leads to is removed. An empty lock file is made there, by the call
/// that takes the lock at that path.
#[test]
fn a_link_that_is_put_where_a_creation_left_a_directory_is_not_followed() {
    let outside = tempfile::tempdir().expect("a directory");
    let theirs = outside.path().join("theirs");
    let with_the_lock_file = BTreeMap::from([
        (PathBuf::from("LOCK"), Some(Vec::new())),
        (PathBuf::from("kept"), Some(b"as it was".to_vec())),
    ]);
    let swapped = Arc::new(AtomicBool::new(false));
    // Before the first lock in a directory that a checkpoint is built in,
    // that directory is moved away and a link to theirs is put there.
    let swap = |theirs: PathBuf, away: PathBuf, swapped: Arc<AtomicBool>| -> Before {
        Box::new(move |lock: &Path| {
            let built = lock.parent().expect("a directory");
            let is_built = built.to_string_lossy().ends_with(CREATING_SUFFIX);
            if is_built && !swapped.swap(true, Ordering::SeqCst) {
                std::fs::rename(built, &away)?;
                std::os::unix::fs::symlink(&theirs, built)?;
            }
            Ok(())
        })
    };
    let leave = |built: &Path| {
        let _ = std::fs::remove_dir_all(&theirs);
        std::fs::create_dir(&theirs).expect("a directory");
        std::fs::write(theirs.join("kept"), b"as it was").expect("a file");
        std::fs::create_dir(built).expect("a directory");
        std::fs::write(built.join("LOCK"), b"").expect("a file");
        std::fs::write(built.join("left"), b"what a creation left").expect("a file");
    };

    // A deletion, of the directory itself and of the target beside it.
    let dir = tempfile::tempdir().expect("a directory");
    let target = dir.path().join("1");
    let built = building(&target);
    for (index, given) in [&built, &target].into_iter().enumerate() {
        let away = outside.path().join(format!("away-{index}"));
        leave(&built);
        swapped.store(false, Ordering::SeqCst);
        let hook = swap(theirs.clone(), away.clone(), swapped.clone());
        let meddling = Meddling::before_lock(Arc::new(StdVfs), hook);
        let refused = checkpoint::delete(&meddling, given);
        assert!(
            matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
            "{refused:?}"
        );
        assert!(swapped.load(Ordering::SeqCst) && built.is_symlink());
        assert_eq!(everything_in(&StdVfs, &theirs), with_the_lock_file);
        assert!(
            away.join("left").exists(),
            "what was moved away is as it was"
        );
        std::fs::remove_file(&built).expect("the link is removed");
    }

    // The next creation at the target.
    let away = outside.path().join("away-creation");
    swapped.store(true, Ordering::SeqCst);
    let (db, versions) = store_with_locks_met(swap(theirs.clone(), away.clone(), swapped.clone()));
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(80), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let built = building(&target);
    leave(&built);
    swapped.store(false, Ordering::SeqCst);
    let refused = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
        "{refused:?}"
    );
    assert!(swapped.load(Ordering::SeqCst) && built.is_symlink() && !target.exists());
    assert_eq!(everything_in(&StdVfs, &theirs), with_the_lock_file);
    assert!(
        away.join("left").exists(),
        "what was moved away is as it was"
    );
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// The look under the lock is for the directory whose lock was taken, and
/// not for any directory at that path: somebody moves what a creation
/// left away once its lock is taken, and puts a directory of their own
/// there. Its lock file is one whose lock they hold, or a link to the
/// lock file of the directory that was moved away. A deletion and the
/// next creation remove nothing of it.
#[test]
fn a_directory_that_is_put_where_a_lock_was_taken_is_not_emptied() {
    let outside = tempfile::tempdir().expect("a directory");
    let theirs = BTreeMap::from([
        (PathBuf::from("LOCK"), Some(Vec::new())),
        (PathBuf::from("theirs"), Some(b"as it was".to_vec())),
    ]);
    let held: Arc<Mutex<Vec<Box<dyn VfsLock>>>> = Arc::default();
    let away: Arc<Mutex<PathBuf>> = Arc::default();
    let swaps = Arc::new(AtomicUsize::new(0));
    // The directory whose lock is being taken, until the swap is made:
    // at the first listing after that lock.
    let locked: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let swapping = |plain: Arc<dyn Vfs>, linked: bool| -> Meddling {
        let before_lock: Before = {
            let locked = locked.clone();
            Box::new(move |lock: &Path| {
                let built = lock.parent().expect("a directory");
                if built.to_string_lossy().ends_with(CREATING_SUFFIX) {
                    *locked.lock().expect("the directory") = Some(built.to_path_buf());
                }
                Ok(())
            })
        };
        let before_list: Before = {
            let (held, away, locked) = (held.clone(), away.clone(), locked.clone());
            let swaps = swaps.clone();
            Box::new(move |_: &Path| {
                let Some(built) = locked.lock().expect("the directory").take() else {
                    return Ok(());
                };
                let moved_to = away.lock().expect("a path").clone();
                std::fs::rename(&built, &moved_to)?;
                std::fs::create_dir(&built)?;
                std::fs::write(built.join("theirs"), b"as it was")?;
                if linked {
                    std::os::unix::fs::symlink(moved_to.join("LOCK"), built.join("LOCK"))?;
                } else {
                    (held.lock().expect("the locks")).push(StdVfs.lock(&built.join("LOCK"))?);
                }
                swaps.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        };
        Meddling {
            before_lock: Some(before_lock),
            before_list: Some(before_list),
            ..Meddling::plain(plain)
        }
    };
    let leave = |built: &Path, moved_to: PathBuf| {
        std::fs::create_dir(built).expect("a directory");
        std::fs::write(built.join("LOCK"), b"").expect("a file");
        std::fs::write(built.join("left"), b"what a creation left").expect("a file");
        *away.lock().expect("a path") = moved_to;
    };
    let check = |built: &Path, moved_to: &Path, linked: bool| {
        assert_eq!(swaps.swap(0, Ordering::SeqCst), 1, "the swap was made");
        assert_eq!(everything_in(&StdVfs, built), theirs);
        assert_eq!(built.join("LOCK").is_symlink(), linked);
        assert!(moved_to.join("left").exists() && moved_to.join("LOCK").exists());
        held.lock().expect("the locks").clear();
    };

    for linked in [false, true] {
        // A deletion, of the directory itself and of the target beside it.
        let dir = tempfile::tempdir().expect("a directory");
        let target = dir.path().join("1");
        let built = building(&target);
        let meddling = swapping(Arc::new(StdVfs), linked);
        for (index, given) in [&built, &target].into_iter().enumerate() {
            let moved_to = outside.path().join(format!("away-{linked}-{index}"));
            leave(&built, moved_to.clone());
            let refused = checkpoint::delete(&meddling, given);
            assert!(
                matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
                "{refused:?}"
            );
            check(&built, &moved_to, linked);
            std::fs::remove_dir_all(&built).expect("their directory is removed");
        }

        // The next creation at the target.
        let (db, versions) = store_on_meddling(|plain, _| swapping(plain, linked));
        let mut world = World::new(&db, &versions);
        block(&mut world, &mut Rng::new(81), 0, 10);
        let target = checkpoints_in(&db).join("1");
        let built = building(&target);
        let moved_to = outside.path().join(format!("away-{linked}-creation"));
        leave(&built, moved_to.clone());
        let refused = checkpointer(&db, &versions).create(&target);
        assert!(
            matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
            "{refused:?}"
        );
        check(&built, &moved_to, linked);
        assert!(!target.exists());
        assert_eq!(versions.pins_held(), 0);
        world.check_committed();
    }
}

/// A directory under the name that a creation builds under is not locked
/// where something else than a file has the name of its lock file, or a
/// name that a file system may take for it: the lock would be taken, and
/// where a link leads nowhere a file would be made, at the end of the
/// link. A creation and a deletion refuse it and touch nothing, also
/// where the link leads to the name of the target.
#[test]
fn a_lock_file_that_is_no_file_is_not_locked() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Is {
        LinkToAFile,
        LinkToTheTarget,
        Directory,
    }
    let (db, versions) = store_with_log(24 << 10, 128);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(82), 0, 10);
    let checkpoints = checkpoints_in(&db);
    let outside = tempfile::tempdir().expect("a directory");
    let their_file = outside.path().join("theirs");
    std::fs::write(&their_file, b"as it was").expect("a file");

    for (index, (name, is)) in [
        ("LOCK", Is::LinkToAFile),
        ("LOCK", Is::LinkToTheTarget),
        ("lock", Is::LinkToTheTarget),
        ("Lock", Is::LinkToAFile),
        ("LOCK", Is::Directory),
        ("LOC\u{212a}", Is::LinkToTheTarget),
    ]
    .into_iter()
    .enumerate()
    {
        let context = format!("{name}: {is:?}");
        let target = checkpoints.join(format!("linked-{index}"));
        let built = building(&target);
        let lock = built.join(name);
        std::fs::create_dir(&built).expect("a directory");
        match is {
            Is::LinkToAFile => std::os::unix::fs::symlink(&their_file, &lock).expect("a link"),
            Is::LinkToTheTarget => std::os::unix::fs::symlink(&target, &lock).expect("a link"),
            Is::Directory => std::fs::create_dir(&lock).expect("a directory"),
        }
        std::fs::write(built.join("left"), b"what was left").expect("a file");
        let untouched = || {
            assert_eq!(lock.is_symlink(), is != Is::Directory, "{context}");
            assert!(std::fs::symlink_metadata(&lock).is_ok(), "{context}");
            assert!(std::fs::symlink_metadata(&target).is_err(), "{context}");
            let left = std::fs::read_dir(&built).expect("the directory").count();
            assert_eq!(left, 2, "{context}");
            assert_eq!(
                std::fs::read(built.join("left")).expect("the file"),
                b"what was left"
            );
            assert_eq!(std::fs::read(&their_file).expect("the file"), b"as it was");
        };

        let refused = checkpointer(&db, &versions).create(&target);
        assert!(
            matches!(&refused, Err(CheckpointError::NotALock { path }) if *path == lock),
            "{context}: {refused:?}"
        );
        untouched();
        assert_eq!(versions.pins_held(), 0);
        for given in [&built, &target] {
            let refused = checkpoint::delete(&*db.vfs, given);
            assert!(
                matches!(&refused, Err(CheckpointError::NotALock { path }) if *path == lock),
                "{context}: {refused:?}"
            );
            untouched();
        }

        // Without it, the directory is what a creation left.
        match is {
            Is::Directory => std::fs::remove_dir(&lock).expect("the directory is removed"),
            _ => std::fs::remove_file(&lock).expect("the link is removed"),
        }
        (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
        assert!(!built.exists(), "{context}");
        open(&db, &target).check(&world.model);
    }
    world.check_committed();
}

/// A file system may keep a name in another spelling than it was given
/// in, and list it so. A checkpoint is made at a target with such a name
/// all the same, with a carried file of such a name, what a creation left
/// under such a name is taken over and removed, and a link under such a
/// name is refused under the lock, where the directory is known by its
/// lock file.
#[test]
fn a_name_that_is_listed_in_another_spelling_is_built_under() {
    let (db, versions) = store_on_meddling(Meddling::respelled);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(83);
    for round in 0..2 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    let checkpoints = checkpoints_in(&db);
    let carried_name = format!("{ONE_LETTER}.engine");
    std::fs::write(db.dir.join(&carried_name), b"what the engine keeps").expect("a file");
    let carried = [carried_name.as_str()];
    let maker = checkpointer(&db, &versions).carrying(&carried);
    let listed = |dir: &Path| -> Vec<String> {
        (db.vfs.read_dir(dir).expect("the directory").into_iter())
            .map(|entry| entry.name.into_string().expect("a name of the tests"))
            .collect()
    };

    // The directory above lists neither the directory that is built in
    // nor the checkpoint under the name that was given.
    let target = checkpoints.join(format!("caf{ONE_LETTER}"));
    let during = |step: Step| {
        if step == Step::Settled {
            assert_eq!(
                listed(&checkpoints),
                [format!("caf{TWO_LETTERS}{CREATING_SUFFIX}")]
            );
        }
    };
    (maker.pausing(&during).create(&target)).expect("a checkpoint");
    assert_eq!(listed(&checkpoints), [format!("caf{TWO_LETTERS}")]);
    assert!(listed(&target).contains(&format!("{TWO_LETTERS}.engine")));
    assert_eq!(
        std::fs::read(target.join(&carried_name)).expect("the carried file"),
        b"what the engine keeps"
    );
    let leave = |built: &Path| {
        std::fs::create_dir(built).expect("a directory");
        std::fs::write(built.join("LOCK"), b"").expect("a file");
        std::fs::write(built.join("left"), b"what a creation left").expect("a file");
    };

    // A deletion looks for the checkpoint under the name it is given,
    // and does not find it listed so: it refuses, and removes nothing
    // beside it either.
    leave(&building(&target));
    let refused = checkpoint::delete(&*db.vfs, &target);
    assert!(
        matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if *dir == target),
        "{refused:?}"
    );
    assert!(building(&target).join("left").exists());
    checkpoint::delete(&*db.vfs, &building(&target)).expect("what was left is removed");
    open(&db, &target).check(&world.model);

    // What a creation left under such a name: a creation takes it over,
    // and a deletion removes it. Not while a link to it is beside it: the
    // directory is known by its lock file, which is then found through
    // the link too.
    let second = checkpoints.join(format!("{ONE_LETTER}-2"));
    leave(&building(&second));
    let alias = checkpoints.join("alias");
    std::os::unix::fs::symlink(building(&second), &alias).expect("a link");
    let refused = maker.create(&second);
    assert!(
        matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == building(&second)),
        "{refused:?}"
    );
    assert!(building(&second).join("left").exists() && !second.exists());
    std::fs::remove_file(&alias).expect("the link is removed");
    maker.create(&second).expect("a checkpoint");
    assert!(!building(&second).exists() && !second.join("left").exists());
    open(&db, &second).check(&world.model);
    let third = checkpoints.join(format!("{ONE_LETTER}-3"));
    for given in [&third, &building(&third)] {
        leave(&building(&third));
        checkpoint::delete(&*db.vfs, given).expect("what was left is removed");
        assert!(!building(&third).exists());
    }

    // A link under such a name is not known for one before the lock. It
    // is under the lock: nothing where it leads is removed, and nothing
    // gets the name of the target. The lock file was made there.
    let outside = tempfile::tempdir().expect("a directory");
    let theirs = outside.path().join("theirs");
    std::fs::create_dir(&theirs).expect("a directory");
    std::fs::write(theirs.join("kept"), b"as it was").expect("a file");
    let with_the_lock_file = BTreeMap::from([
        (PathBuf::from("LOCK"), Some(Vec::new())),
        (PathBuf::from("kept"), Some(b"as it was".to_vec())),
    ]);
    let fourth = checkpoints.join(format!("{ONE_LETTER}-4"));
    let built = building(&fourth);
    std::os::unix::fs::symlink(&theirs, &built).expect("a link");
    let refused = maker.create(&fourth);
    assert!(
        matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
        "{refused:?}"
    );
    for given in [&fourth, &built] {
        let refused = checkpoint::delete(&*db.vfs, given);
        assert!(
            matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
            "{refused:?}"
        );
    }
    assert!(built.is_symlink() && !fourth.exists());
    assert_eq!(everything_in(&StdVfs, &theirs), with_the_lock_file);
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// A creation asks for its syncs as one set, and for two more: the log's
/// own, where the log is not durable through the version, and the one of
/// the directory above the target after the rename. The set has the
/// manifest and every copy, each directory of the checkpoint, the
/// directory it is built in and the one above.
#[test]
fn a_creation_asks_for_one_set_of_syncs_and_the_name() {
    for (copied, durable) in [(false, false), (false, true), (true, false)] {
        let context = format!("copied: {copied}, the log is durable: {durable}");
        let fs = sim(77);
        let plain: Arc<dyn Vfs> = Arc::new(fs.clone());
        let meddling = Arc::new(match copied {
            true => {
                let under = Path::new(super::DIR).join("checkpoints");
                Meddling::no_links(plain, under, ErrorKind::CrossesDevices)
            }
            false => Meddling::plain(plain),
        });
        let (options, flush) = (LogOptions::default(), FlushConfig::new(SIZE));
        let (db, versions) = Db::on(
            meddling.clone(),
            Path::new(super::DIR),
            256,
            options,
            flush,
            None,
        );
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(77);
        for round in 0..3 {
            block(&mut world, &mut rng, round * 10, 10);
        }
        (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
        block(&mut world, &mut rng, 30, 10);
        if durable {
            (db.write_back(&versions).sync_log()).expect("a flush");
        }
        let target = checkpoints_in(&db).join("1");
        meddling.sets.lock().expect("the sets").clear();
        let syncs = fs.stats().syncs;
        let created = (checkpointer(&db, &versions).with_options(super::NO_CATCH_UP))
            .create(&target)
            .unwrap_or_else(|error| panic!("{context}: {error}"));
        let syncs = fs.stats().syncs - syncs;
        let sets = meddling.sets.lock().expect("the sets").clone();
        let dirs = ["wal", "seg", "val", "table"]
            .iter()
            .filter(|dir| fs.read_dir(&target.join(dir)).is_ok())
            .count();
        // The manifest and the copies; the directories of the checkpoint;
        // the directory it was built in and the one above.
        let in_set = 1 + created.copied + dirs + 2;
        assert_eq!(sets, [in_set], "{context}");
        assert_eq!(
            syncs as usize,
            in_set + 1 + usize::from(!durable),
            "{context}"
        );
        assert_eq!(created.copied > 0, copied, "{context}");
        drop(world);
    }
}

/// Of a directory that holds an empty lock file and nothing else, the lock
/// file is all that a deletion removes: what comes into the directory
/// while it does stays, and the directory with it.
#[test]
fn a_file_that_comes_beside_a_lock_file_alone_is_not_removed() {
    let dir = tempfile::tempdir().expect("a directory");
    let theirs = dir.path().join("theirs");
    std::fs::create_dir(&theirs).expect("a directory");
    std::fs::write(theirs.join("LOCK"), b"").expect("a file");
    // The file comes once, before the first removal: a deletion that went
    // on to remove what it then found would take it away.
    let (into, came) = (theirs.clone(), AtomicBool::new(false));
    let meddling = Meddling::before_remove(
        Arc::new(StdVfs),
        Box::new(move |_| match came.swap(true, Ordering::SeqCst) {
            true => Ok(()),
            false => std::fs::write(into.join("data"), b"put there meanwhile"),
        }),
    );
    let failed = checkpoint::delete(&meddling, &theirs);
    assert!(
        matches!(&failed, Err(CheckpointError::Io { action: "remove the directory", path, .. })
            if *path == theirs),
        "{failed:?}"
    );
    let left = BTreeMap::from([(PathBuf::from("data"), Some(b"put there meanwhile".to_vec()))]);
    assert_eq!(everything_in(&StdVfs, &theirs), left);
}

/// The directory above a path, as the file layer is given it.
#[test]
fn the_directory_above_a_target() {
    use crate::checkpoint::files::parent_of;

    assert_eq!(
        parent_of(Path::new("/db/checkpoints/1")),
        Some(Path::new("/db/checkpoints"))
    );
    assert_eq!(
        parent_of(Path::new("checkpoints/1")),
        Some(Path::new("checkpoints"))
    );
    // A target that is one name lies in the directory the process is in.
    assert_eq!(parent_of(Path::new("1")), Some(Path::new(".")));
    assert_eq!(parent_of(Path::new("/")), None);
    assert_eq!(parent_of(Path::new("")), None);
}

/// Every error says what it is.
#[test]
fn the_errors_say_what_happened() {
    use std::error::Error;

    use lignin_format::{wal::LogPlace, CommitVersion, FileId, FormatError};
    use lignin_store::{flush::FlushError, wal::LogError};

    use crate::commit::Stopped;

    let version = |raw| CommitVersion::new(raw).expect("a version");
    let file = FileId::new(3).expect("an id");
    let place = LogPlace::new(file, 64).expect("a place");
    let io = || std::io::Error::other("the device");
    let no_manifest = || SegmentStoreError::NoManifest {
        dir: PathBuf::from("/db"),
    };
    let cases: Vec<(CheckpointError, &str, bool)> = vec![
        (
            CheckpointError::TargetExists {
                target: PathBuf::from("/c/1"),
            },
            "/c/1 is there already",
            false,
        ),
        (
            CheckpointError::NoParent {
                target: PathBuf::from("/"),
            },
            "/ has no directory above it",
            false,
        ),
        (
            CheckpointError::CarriedName {
                name: "../x".to_string(),
            },
            "\"../x\" is not the plain name of a file",
            false,
        ),
        (
            CheckpointError::Stopped(Stopped::Halted {
                version: version(4),
                reason: "why".to_string(),
            }),
            "the session takes no commit, and no checkpoint is made of it",
            false,
        ),
        (
            CheckpointError::LogAhead {
                log: version(5),
                latest: version(4),
            },
            "the log holds commit 5, and the latest published version is 4",
            false,
        ),
        (
            CheckpointError::Manifest(no_manifest()),
            "the manifest could not be read",
            true,
        ),
        (
            CheckpointError::Settle(FlushError::SegmentsNotDurable),
            "the manifest was in doubt, and settling it answered an error",
            true,
        ),
        (
            CheckpointError::Sync(FlushError::SegmentsNotDurable),
            "the log could not be made durable",
            true,
        ),
        (
            CheckpointError::from(LogError::NoActiveFile),
            "the log: ",
            true,
        ),
        (
            CheckpointError::NotDurable {
                end: place,
                durable: None,
            },
            "ends at offset 64 of log file 3, and the log is durable nowhere",
            false,
        ),
        (
            CheckpointError::NotDurable {
                end: place,
                durable: Some(LogPlace::new(file, 128).expect("a place")),
            },
            "and the log is durable up to offset 128 of log file 3",
            false,
        ),
        (
            CheckpointError::LogFile {
                file,
                reason: "is not durable through the end of the checkpoint's log",
            },
            "log file 3 is not durable through the end",
            false,
        ),
        (
            CheckpointError::from(FormatError::Truncated {
                needed: 8,
                available: 0,
            }),
            "the manifest of the checkpoint could not be written",
            true,
        ),
        (
            CheckpointError::ReservedName {
                target: PathBuf::from("/c/1.creating"),
            },
            "/c/1.creating has a name that checkpoints are built under",
            false,
        ),
        (
            CheckpointError::Creating {
                dir: PathBuf::from("/c/1.creating"),
            },
            "/c/1.creating is locked, as a directory that a checkpoint is being built in",
            false,
        ),
        (
            CheckpointError::Replaced {
                dir: PathBuf::from("/c/1.creating"),
            },
            "/c/1.creating was not found to be the directory whose lock is held",
            false,
        ),
        (
            CheckpointError::NotALock {
                path: PathBuf::from("/c/1.creating/LOCK"),
            },
            "/c/1.creating/LOCK is not a file of its own",
            false,
        ),
        (
            CheckpointError::io("link", Path::new("/db/seg/1"))(io()),
            "could not link /db/seg/1: the device",
            true,
        ),
        (
            CheckpointError::NotADirectory {
                dir: PathBuf::from("/c/1"),
            },
            "/c/1 is not a directory of its own, and nothing was removed",
            false,
        ),
        (
            CheckpointError::NotADatabase {
                dir: PathBuf::from("/c/1"),
                source: no_manifest(),
            },
            "/c/1 is no checkpoint, and nothing of it was removed",
            true,
        ),
        (
            CheckpointError::Open {
                dir: PathBuf::from("/c/1"),
            },
            "/c/1 is open, and nothing of it was removed",
            false,
        ),
    ];
    for (error, said, has_source) in cases {
        let text = error.to_string();
        assert!(text.contains(said), "{text:?} does not say {said:?}");
        assert_eq!(error.source().is_some(), has_source, "{text}");
        assert!(!format!("{error:?}").is_empty());
    }
    let (db, versions) = Db::new(16);
    let maker = checkpointer(&db, &versions).with_options(CheckpointOptions::default());
    assert!(format!("{maker:?}").contains("Checkpointer"));
}
