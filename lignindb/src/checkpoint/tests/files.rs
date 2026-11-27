//! What a checkpoint's directory is made of: links where the file system
//! makes them and copies where it does not, the files the engine has it
//! carry, and the deletion of a checkpoint.

use std::{
    collections::{BTreeMap, BTreeSet},
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
    block, built_in, checkpointer, checkpoints_in, everything_in, names_of, open, open_on, sim,
    staging, store_files, store_on, store_with_log, turn, write_file, Before, Locks, Meddling,
    ONE_LETTER, TWO_LETTERS,
};
use crate::{
    checkpoint::{
        self, create::Step, CheckpointError, CheckpointOptions, BUILDING_DIR_NAME, CREATING_SUFFIX,
        DELETING_FILE_NAME,
    },
    snapshot::Versions,
    txn::tests::{
        model::Committed,
        store::{Db, NoSync, Rng, SIZE},
        World,
    },
};

/// Makes the directory `staged` as a creation that was cut short leaves
/// its staging directory: with the directory it built in, which holds a
/// file, and with its lock file unless `without_lock`.
pub(super) fn leave_built(staged: &Path, without_lock: bool) {
    let built = staged.join(BUILDING_DIR_NAME);
    std::fs::create_dir_all(&built).expect("the directories");
    std::fs::write(built.join("left"), b"what a creation left").expect("a file");
    if !without_lock {
        std::fs::write(staged.join("LOCK"), b"").expect("a file");
    }
}

/// The file that [`leave_built`] puts into the directory that was built
/// in, in the staging directory `staged`.
fn left_in(staged: &Path) -> PathBuf {
    staged.join(BUILDING_DIR_NAME).join("left")
}

/// A store in a temporary directory of its own, on the file layer that
/// `meddling` makes of the one whose syncs do nothing and of the directory
/// of the store's checkpoints.
pub(super) fn store_on_meddling(
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
                assert!(staging(&target).join("LOCK").exists() && !target.exists());
                assert!(
                    store_files(&StdVfs, &staging(&target)).is_empty(),
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

/// Where somebody else has the lock of the staging directory, another
/// creation is building there: the creation fails and touches nothing in
/// it, and neither does a deletion. Where the lock fails otherwise, the
/// directory that was just made goes.
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
        let theirs = lock.with_file_name(BUILDING_DIR_NAME);
        std::fs::create_dir(&theirs)?;
        std::fs::write(theirs.join("MANIFEST"), b"theirs")
    }));
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(71), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let built = staging(&target);
    let failed = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::Creating { dir }) if *dir == built),
        "{failed:?}"
    );
    assert_eq!(theirs.lock().expect("their locks").len(), 1);
    let left = BTreeMap::from([
        (PathBuf::from("LOCK"), Some(Vec::new())),
        (PathBuf::from(BUILDING_DIR_NAME), None),
        (
            Path::new(BUILDING_DIR_NAME).join("MANIFEST"),
            Some(b"theirs".to_vec()),
        ),
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
    leave_built(&built, false);
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
    assert!(!target.exists() && !staging(&target).exists());
    world.check_committed();
}

/// The staging directory that a creation which was cut short left is
/// taken over by the next creation at the same target, which removes what
/// was built in it and builds in it, and removed by a deletion: of the
/// target, with or without a checkpoint there, and of the directory
/// itself. What was built in it goes, whatever that holds. What has the
/// name and is no directory of its own is not looked into, and a target
/// that is given a name of that kind is refused.
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
    let built = staging(&target);
    assert_eq!(built, checkpoints.join(format!("1{CREATING_SUFFIX}")));
    let a_manifest = std::fs::read(db.dir.join("MANIFEST")).expect("a manifest");
    // The lock file of the staging directory, and in the directory that
    // was built in: its own, a part of a checkpoint, a whole manifest
    // among it, a file under the name of one the creation links, and what
    // somebody else put there.
    let leave = || {
        let files: [(&str, &[u8]); 6] = [
            ("LOCK", b""),
            ("checkpoint.creating/LOCK", b""),
            ("checkpoint.creating/MANIFEST", &a_manifest),
            ("checkpoint.creating/seg/00000001.seg", b"left"),
            ("checkpoint.creating/val/99999999.val", b"left"),
            ("checkpoint.creating/theirs/more", b"somebody's"),
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

    // A target that is given a name that creations build under, in
    // whatever case of its ASCII letters, is refused.
    let in_capitals = checkpoints.join("1.CREATING");
    for reserved in [&built, &in_capitals, &checkpoints.join("1.Creating")] {
        let refused = checkpointer(&db, &versions).create(reserved);
        assert!(
            matches!(&refused, Err(CheckpointError::ReservedName { target }) if target == reserved),
            "{refused:?}"
        );
        assert!(!reserved.exists() && !staging(reserved).exists());
    }
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// A creation that fails takes away the files it made and no other: a file
/// that somebody put into the directory it builds in, under a name the
/// creation was about to use, fails the creation and stays, with that
/// directory and the staging directory it is in, for the next creation at
/// the target to remove.
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
    let built = built_in(&target);
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
    let built = built_in(&second);
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

    // The next creation at each target takes over what was built there,
    // their files in it among that.
    for target in [&target, &second] {
        (checkpointer(&db, &versions).create(target)).expect("a checkpoint");
        assert!(!staging(target).exists());
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
    assert!(!target.exists() && !staging(&target).exists());
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// The name of the target is looked at again before the directory gets
/// it: what has come there while the checkpoint was built is not replaced.
/// And only the directory the creation built in is renamed: where another
/// is at its path by then, or at the path of the staging directory it is
/// in, nothing gets the target's name and nothing of the other is removed.
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
        assert!(!staging(&target).exists(), "{what}");
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

    // The staging directory, or the directory that was built in, is moved
    // away, and another is put under its name.
    for (index, inner) in [false, true].into_iter().enumerate() {
        let target = checkpoints.join(format!("replaced-{index}"));
        let away = checkpoints.join(format!("away-{index}"));
        let built = match inner {
            true => built_in(&target),
            false => staging(&target),
        };
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
        // Nothing of the other directory is removed, also under the names
        // of what the creation made in its own.
        let left: BTreeMap<_, _> = ["LOCK", "MANIFEST", "theirs"]
            .map(|name| (PathBuf::from(name), Some(b"as it was".to_vec())))
            .into();
        assert_eq!(everything_in(&StdVfs, &built), left);
        // What was built is whole where it was moved to, and nothing of
        // it was removed.
        let whole = match inner {
            true => away.clone(),
            false => away.join(BUILDING_DIR_NAME),
        };
        open(&db, &whole).check(&world.model);
        // The next creation at the target leaves the other directory as
        // it is, too: no creation puts such entries into a staging
        // directory, and what stands in the place of the one that was
        // built in goes only with a staging directory of a creation's.
        let again = checkpointer(&db, &versions).create(&target);
        match inner {
            false => {
                assert!(
                    matches!(&again, Err(CheckpointError::NotStaging { dir, found })
                        if *dir == built && found == "LOCK"),
                    "{again:?}"
                );
                assert_eq!(everything_in(&StdVfs, &built), left);
            }
            true => {
                again.expect("a checkpoint");
                open(&db, &target).check(&world.model);
            }
        }
    }
    world.check_committed();
}

/// The directory a checkpoint is built in is looked at right before it
/// gets the name of the target: it has to hold what the creation put into
/// it and nothing else, each a file or a directory of its own, and has to
/// be a directory of its own itself, in a staging directory that is one
/// too and lists nothing but its lock file and that directory. Somebody
/// removes a linked file, adds a file, renames one, puts a link where a
/// linked file was, where the lock file was, where a directory of it was,
/// or where the directory itself was; or adds a file to the staging
/// directory, or puts a link where that was: nothing gets the name of the
/// target, and the rollback removes no link and nothing through one.
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
        AFileIsAddedToTheStagingDirectory,
        TheStagingDirectoryIsALink,
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
        Change::AFileIsAddedToTheStagingDirectory,
        Change::TheStagingDirectoryIsALink,
    ]
    .into_iter()
    .enumerate()
    {
        let target = checkpoints.join(format!("changed-{index}"));
        let (staged, built) = (staging(&target), built_in(&target));
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
                Change::AFileIsAddedToTheStagingDirectory => {
                    std::fs::write(staged.join("theirs"), b"theirs").expect("a file")
                }
                Change::TheStagingDirectoryIsALink => {
                    std::fs::rename(&staged, &away).expect("it is moved away");
                    std::os::unix::fs::symlink(&away, &staged).expect("a link");
                }
            }
        };
        let refused = checkpointer(&db, &versions).pausing(&pause).create(&target);
        let not_found = match change {
            Change::AFileIsAddedToTheStagingDirectory | Change::TheStagingDirectoryIsALink => {
                &staged
            }
            _ => &built,
        };
        assert!(
            matches!(&refused, Err(CheckpointError::Replaced { dir }) if dir == not_found),
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
            Change::ALinkedFileIsRemoved => assert!(!staged.exists()),
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
            // nothing of it is removed by the creation that built in it.
            // The next creation removes it with the link, which it does
            // not follow.
            Change::TheLockFileIsALink => {
                assert!(built.join("LOCK").is_symlink() && built.join("MANIFEST").exists());
                assert!(!store_files(&StdVfs, &built).is_empty());
            }
            // The link stays, and nothing is removed through it.
            Change::ADirectoryInItIsALink => assert!(built.join("seg").is_symlink()),
            Change::ItIsALink => {
                // Nothing of what was built is removed through the link.
                // A link under that name is no entry of a creation's:
                // the staging directory is not built in before it is
                // gone.
                assert!(built.is_symlink());
                open(&db, &away).check(&world.model);
                let refused = checkpointer(&db, &versions).create(&target);
                assert!(
                    matches!(&refused, Err(CheckpointError::NotStaging { dir, found })
                        if *dir == staged && found == BUILDING_DIR_NAME),
                    "{refused:?}"
                );
                assert!(built.is_symlink());
                open(&db, &away).check(&world.model);
                std::fs::remove_file(&built).expect("the link is removed");
            }
            // What the creation made is gone, and their file stays, with
            // the staging directory and its lock file: no creation builds
            // in it before their file is gone.
            Change::AFileIsAddedToTheStagingDirectory => {
                let left = BTreeMap::from([
                    (PathBuf::from("LOCK"), Some(Vec::new())),
                    (PathBuf::from("theirs"), Some(b"theirs".to_vec())),
                ]);
                assert_eq!(everything_in(&StdVfs, &staged), left);
                let refused = checkpointer(&db, &versions).create(&target);
                assert!(
                    matches!(&refused, Err(CheckpointError::NotStaging { dir, found })
                        if *dir == staged && found == "theirs"),
                    "{refused:?}"
                );
                assert_eq!(everything_in(&StdVfs, &staged), left);
                std::fs::remove_file(staged.join("theirs")).expect("their file is removed");
            }
            Change::TheStagingDirectoryIsALink => {
                // Nothing of what was built is removed through the link.
                assert!(staged.is_symlink());
                open(&db, &away.join(BUILDING_DIR_NAME)).check(&world.model);
                std::fs::remove_file(&staged).expect("the link is removed");
            }
        }
        // What is left is taken over by the next creation at the target,
        // which removes a link and does not follow it.
        (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
        assert!(!staged.exists(), "{change:?}");
        assert_eq!(everything_in(&StdVfs, &theirs), kept, "{change:?}");
        open(&db, &target).check(&world.model);
    }
    world.check_committed();
}

/// What a creation left has listed its lock file, a file, when somebody
/// moves that directory away and puts a link in its place, right before
/// the call that takes the lock (`lock_existing`).
/// Where the link leads to a directory with no lock file, the taking
/// fails and makes none. Where it leads to one with a lock file, that
/// lock is taken, and the look under the lock does not find the directory
/// whose lock it is. Neither changes what the link leads to.
#[test]
fn a_link_that_is_put_where_a_creation_left_a_directory_changes_nothing_where_it_leads() {
    let outside = tempfile::tempdir().expect("a directory");
    let theirs = outside.path().join("theirs");
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
    let leave = |built: &Path, with_a_lock_file: bool| {
        let _ = std::fs::remove_dir_all(&theirs);
        std::fs::create_dir(&theirs).expect("a directory");
        std::fs::write(theirs.join("kept"), b"as it was").expect("a file");
        if with_a_lock_file {
            std::fs::write(theirs.join("LOCK"), b"theirs").expect("a file");
        }
        leave_built(built, false);
    };
    let as_it_was = |with_a_lock_file: bool| {
        let mut theirs = BTreeMap::from([(PathBuf::from("kept"), Some(b"as it was".to_vec()))]);
        if with_a_lock_file {
            theirs.insert(PathBuf::from("LOCK"), Some(b"theirs".to_vec()));
        }
        theirs
    };
    let is_refused = |refused: &Result<(), CheckpointError>, built: &Path, with_a_lock_file| {
        let lock = built.join("LOCK");
        match (refused, with_a_lock_file) {
            (Err(CheckpointError::Replaced { dir }), true) => dir == built,
            (
                Err(CheckpointError::Io {
                    action,
                    path,
                    source,
                }),
                false,
            ) => *action == "lock" && *path == lock && source.kind() == ErrorKind::NotFound,
            _ => false,
        }
    };

    for with_a_lock_file in [false, true] {
        // A deletion, of the directory itself and of the target beside it.
        let dir = tempfile::tempdir().expect("a directory");
        let target = dir.path().join("1");
        let built = staging(&target);
        for (index, given) in [&built, &target].into_iter().enumerate() {
            let away = outside
                .path()
                .join(format!("away-{with_a_lock_file}-{index}"));
            leave(&built, with_a_lock_file);
            swapped.store(false, Ordering::SeqCst);
            let hook = swap(theirs.clone(), away.clone(), swapped.clone());
            let meddling = Meddling::before_lock(Arc::new(StdVfs), hook);
            let refused = checkpoint::delete(&meddling, given);
            assert!(
                is_refused(&refused, &built, with_a_lock_file),
                "{refused:?}"
            );
            assert!(swapped.load(Ordering::SeqCst) && built.is_symlink());
            assert_eq!(everything_in(&StdVfs, &theirs), as_it_was(with_a_lock_file));
            assert!(left_in(&away).exists(), "what was moved away is as it was");
            std::fs::remove_file(&built).expect("the link is removed");
        }

        // The next creation at the target.
        let away = outside
            .path()
            .join(format!("away-{with_a_lock_file}-creation"));
        swapped.store(true, Ordering::SeqCst);
        let (db, versions) =
            store_with_locks_met(swap(theirs.clone(), away.clone(), swapped.clone()));
        let mut world = World::new(&db, &versions);
        block(&mut world, &mut Rng::new(80), 0, 10);
        let target = checkpoints_in(&db).join("1");
        let built = staging(&target);
        leave(&built, with_a_lock_file);
        swapped.store(false, Ordering::SeqCst);
        let refused = checkpointer(&db, &versions).create(&target).map(drop);
        assert!(
            is_refused(&refused, &built, with_a_lock_file),
            "{refused:?}"
        );
        assert!(swapped.load(Ordering::SeqCst) && built.is_symlink() && !target.exists());
        assert_eq!(everything_in(&StdVfs, &theirs), as_it_was(with_a_lock_file));
        assert!(left_in(&away).exists(), "what was moved away is as it was");
        assert_eq!(versions.pins_held(), 0);
        world.check_committed();
    }
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
        leave_built(built, false);
        *away.lock().expect("a path") = moved_to;
    };
    let check = |built: &Path, moved_to: &Path, linked: bool| {
        assert_eq!(swaps.swap(0, Ordering::SeqCst), 1, "the swap was made");
        assert_eq!(everything_in(&StdVfs, built), theirs);
        assert_eq!(built.join("LOCK").is_symlink(), linked);
        assert!(left_in(moved_to).exists() && moved_to.join("LOCK").exists());
        held.lock().expect("the locks").clear();
    };

    for linked in [false, true] {
        // A deletion, of the directory itself and of the target beside it.
        let dir = tempfile::tempdir().expect("a directory");
        let target = dir.path().join("1");
        let built = staging(&target);
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
        let built = staging(&target);
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

/// A staging directory is not locked where it lists something else than a
/// file under the name of its lock file, or an entry under a name that a
/// file system which does not tell capital letters from small ones takes
/// for that one: neither is an entry of a creation's
/// ([`CheckpointError::NotStaging`]). And it is not locked where it lists
/// nothing under that name and the name leads to something all the same
/// ([`CheckpointError::NotALock`]). A creation and a deletion of the
/// directory refuse it, a deletion of the target beside it passes it
/// over, and none of them removes or makes anything, also where a link
/// leads to the name of the target.
#[test]
fn a_lock_file_that_is_no_file_is_not_locked() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Is {
        LinkToAFile,
        LinkToTheTarget,
        Directory,
    }
    let unlisted: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let (db, versions) = store_on_meddling(|plain, checkpoints| Meddling {
        unlisted: unlisted.clone(),
        ..Meddling::folding(plain, checkpoints)
    });
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(82), 0, 10);
    let checkpoints = checkpoints_in(&db);
    let outside = tempfile::tempdir().expect("a directory");
    let their_file = outside.path().join("theirs");
    std::fs::write(&their_file, b"as it was").expect("a file");

    for (index, (name, is, listed)) in [
        ("LOCK", Is::LinkToAFile, true),
        ("LOCK", Is::LinkToTheTarget, true),
        ("LOCK", Is::Directory, true),
        ("lock", Is::LinkToTheTarget, true),
        ("Lock", Is::LinkToAFile, true),
        ("LOC\u{212a}", Is::LinkToTheTarget, true),
        ("LOCK", Is::LinkToAFile, false),
        ("LOCK", Is::LinkToTheTarget, false),
        ("lock", Is::Directory, false),
    ]
    .into_iter()
    .enumerate()
    {
        let context = format!("{name}: {is:?}, listed: {listed}");
        let target = checkpoints.join(format!("linked-{index}"));
        let built = staging(&target);
        let lock = built.join("LOCK");
        let theirs = built.join(name);
        leave_built(&built, true);
        match is {
            Is::LinkToAFile => std::os::unix::fs::symlink(&their_file, &theirs).expect("a link"),
            Is::LinkToTheTarget => std::os::unix::fs::symlink(&target, &theirs).expect("a link"),
            Is::Directory => std::fs::create_dir(&theirs).expect("a directory"),
        }
        *unlisted.lock().expect("a path") = (!listed).then(|| theirs.clone());
        let untouched = || {
            assert_eq!(theirs.is_symlink(), is != Is::Directory, "{context}");
            assert!(std::fs::symlink_metadata(&theirs).is_ok(), "{context}");
            assert!(std::fs::symlink_metadata(&target).is_err(), "{context}");
            let left = std::fs::read_dir(&built).expect("the directory").count();
            assert_eq!(left, 2, "{context}");
            assert_eq!(
                std::fs::read(left_in(&built)).expect("the file"),
                b"what a creation left"
            );
            assert_eq!(std::fs::read(&their_file).expect("the file"), b"as it was");
        };
        let is_refused = |refused: Result<(), CheckpointError>| {
            match (&refused, listed) {
                (Err(CheckpointError::NotStaging { dir, found }), true) => {
                    assert!(*dir == built && found == name, "{context}: {refused:?}")
                }
                (Err(CheckpointError::NotALock { path }), false) => {
                    assert_eq!(*path, lock, "{context}")
                }
                _ => panic!("{context}: {refused:?}"),
            }
            untouched();
        };

        is_refused(checkpointer(&db, &versions).create(&target).map(drop));
        assert_eq!(versions.pins_held(), 0);
        is_refused(checkpoint::delete(&*db.vfs, &built));
        // Nothing is at the target. A directory beside it that is no
        // creation's is passed over; one whose lock cannot be taken is
        // the error of the call.
        let deleted = checkpoint::delete(&*db.vfs, &target);
        match listed {
            true => {
                assert!(
                    matches!(&deleted, Err(CheckpointError::NotADirectory { dir })
                        if *dir == target),
                    "{context}: {deleted:?}"
                );
                untouched();
            }
            false => is_refused(deleted),
        }

        // Without it, the directory is what a creation left.
        *unlisted.lock().expect("a path") = None;
        match is {
            Is::Directory => std::fs::remove_dir(&theirs).expect("the directory is removed"),
            _ => std::fs::remove_file(&theirs).expect("the link is removed"),
        }
        (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
        assert!(!built.exists(), "{context}");
        open(&db, &target).check(&world.model);
    }
    world.check_committed();
}

/// What a creation left with no lock file in it gets one, and is taken
/// over and removed, with what the directory that was built in holds
/// here: a file, a link and a directory.
#[test]
fn what_a_creation_left_without_a_lock_file_gets_one() {
    let (db, versions) = store_with_log(24 << 10, 128);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(84), 0, 10);
    let checkpoints = checkpoints_in(&db);
    let outside = tempfile::tempdir().expect("a directory");
    let their_file = outside.path().join("theirs");
    std::fs::write(&their_file, b"as it was").expect("a file");
    let leave = |staged: &Path| {
        leave_built(staged, true);
        let built = staged.join(BUILDING_DIR_NAME);
        std::fs::create_dir(built.join("r\u{e9}sum\u{e9}")).expect("a directory");
        std::os::unix::fs::symlink(&their_file, built.join("old-lock\u{e9}")).expect("a link");
    };

    let target = checkpoints.join("1");
    leave(&staging(&target));
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(!staging(&target).exists());
    open(&db, &target).check(&world.model);

    let second = checkpoints.join("2");
    for given in [&second, &staging(&second)] {
        leave(&staging(&second));
        checkpoint::delete(&*db.vfs, given).expect("what was left is removed");
        assert!(!staging(&second).exists() && !second.exists());
    }
    assert_eq!(std::fs::read(&their_file).expect("the file"), b"as it was");
    world.check_committed();
}

/// Somebody else makes the lock file in what a creation left, and locks
/// it, right before the call that found none there does: the call finds
/// the lock held, as it does where the lock file was there, and removes
/// nothing.
#[test]
fn a_lock_file_that_somebody_else_makes_first_is_theirs() {
    let taken = Locks::default();
    let theirs = taken.clone();
    let came_first = AtomicBool::new(false);
    let (db, versions) = store_with_locks_met(Box::new(move |lock: &Path| {
        if !came_first.swap(true, Ordering::SeqCst) {
            (theirs.lock().expect("the locks")).push(StdVfs.lock(lock)?);
        }
        Ok(())
    }));
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(86), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let built = staging(&target);
    leave_built(&built, true);
    let is_theirs = |refused: Result<(), CheckpointError>| {
        assert!(
            matches!(&refused, Err(CheckpointError::Creating { dir }) if *dir == built),
            "{refused:?}"
        );
        assert!(left_in(&built).exists() && built.join("LOCK").exists());
    };

    is_theirs(checkpointer(&db, &versions).create(&target).map(drop));
    assert_eq!(taken.lock().expect("the locks").len(), 1);
    assert_eq!(versions.pins_held(), 0);
    is_theirs(checkpoint::delete(&*db.vfs, &built));
    // Nothing is at the target, and the lock of what is beside it is held.
    is_theirs(checkpoint::delete(&*db.vfs, &target));
    assert!(!target.exists());

    // Once they give the lock back, it is what a creation left.
    taken.lock().expect("the locks").clear();
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(!built.exists());
    open(&db, &target).check(&world.model);
    world.check_committed();
}

/// The name of the lock file leads to something when the lock of what a
/// creation left is to be taken, though the directory lists nothing under
/// it, and to nothing once the directory has been listed again: the look
/// is made once more, a lock file is made, and the directory is taken
/// over. Here a link under another spelling of the name comes after the
/// first look, which found nothing but what a creation puts there, and
/// goes before the third listing.
#[test]
fn a_lock_file_is_made_at_the_second_try() {
    let listings = Arc::new(AtomicUsize::new(0));
    let outside = tempfile::tempdir().expect("a directory");
    let their_file = outside.path().join("theirs");
    std::fs::write(&their_file, b"as it was").expect("a file");
    let (db, versions) = store_on_meddling(|plain, checkpoints| {
        let (listings, theirs) = (listings.clone(), their_file.clone());
        let before_list: Before = Box::new(move |listed: &Path| {
            if !listed.to_string_lossy().ends_with(CREATING_SUFFIX) {
                return Ok(());
            }
            match listings.fetch_add(1, Ordering::SeqCst) {
                1 => std::os::unix::fs::symlink(&theirs, listed.join("lock"))?,
                2 => std::fs::remove_file(listed.join("lock"))?,
                _ => {}
            }
            Ok(())
        });
        Meddling {
            before_list: Some(before_list),
            ..Meddling::folding(plain, checkpoints)
        }
    });
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(87), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let built = staging(&target);
    leave_built(&built, true);

    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(listings.load(Ordering::SeqCst) >= 3 && !built.exists());
    assert_eq!(std::fs::read(&their_file).expect("the file"), b"as it was");
    open(&db, &target).check(&world.model);
    world.check_committed();
}

/// The lock file that what a creation left gets is made by the call that
/// locks it. It is not made first and opened again to be locked: where
/// new files are made so that they cannot be opened again for writing,
/// that second open would be refused, and the directory could be neither
/// removed nor taken over.
#[test]
fn a_lock_file_is_made_by_the_call_that_locks_it() {
    let (db, versions) = store_on_meddling(|plain, _| Meddling {
        made_read_only: Some(Mutex::default()),
        ..Meddling::plain(plain)
    });
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(88), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let built = staging(&target);
    let leave = || leave_built(&built, true);

    for given in [&built, &target] {
        leave();
        checkpoint::delete(&*db.vfs, given).expect("what was left is removed");
        assert!(!built.exists() && !target.exists());
    }
    leave();
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(!built.exists());
    open(&db, &target).check(&world.model);
    world.check_committed();
}

/// A deletion of a target that the directory above does not list finds
/// out, by a removal that follows no link, whether the name leads to
/// something all the same. A link that leads nowhere, a file and a
/// directory with something in it refuse the call, and what a creation
/// left beside the target stays. An empty directory is removed, as any
/// empty directory is, and what is beside it then goes too.
#[test]
fn a_target_that_is_there_and_not_listed_keeps_what_is_beside_it() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Is {
        LinkToNowhere,
        File,
        FullDirectory,
        EmptyDirectory,
    }
    let dir = tempfile::tempdir().expect("a directory");
    for (index, is) in [
        Is::LinkToNowhere,
        Is::File,
        Is::FullDirectory,
        Is::EmptyDirectory,
    ]
    .into_iter()
    .enumerate()
    {
        let target = dir.path().join(format!("{index}"));
        let built = staging(&target);
        match is {
            Is::LinkToNowhere => {
                std::os::unix::fs::symlink(dir.path().join("nowhere"), &target).expect("a link")
            }
            Is::File => std::fs::write(&target, b"theirs").expect("a file"),
            Is::FullDirectory | Is::EmptyDirectory => {
                std::fs::create_dir(&target).expect("a directory")
            }
        }
        if is == Is::FullDirectory {
            std::fs::write(target.join("theirs"), b"theirs").expect("a file");
        }
        leave_built(&built, false);

        let vfs = Meddling::not_listing(Arc::new(StdVfs), target.clone());
        let deleted = checkpoint::delete(&vfs, &target);
        if is == Is::EmptyDirectory {
            deleted.expect("the empty directory and what was beside it are removed");
            assert!(std::fs::symlink_metadata(&target).is_err() && !built.exists());
            continue;
        }
        assert!(
            matches!(&deleted, Err(CheckpointError::NotADirectory { dir }) if *dir == target),
            "{is:?}: {deleted:?}"
        );
        assert!(std::fs::symlink_metadata(&target).is_ok(), "{is:?}");
        assert!(left_in(&built).exists(), "{is:?}");
    }
}

/// A file system may keep a name in another spelling than it was given
/// in, and list it so. A checkpoint is made at a target with such a name
/// all the same, with a carried file of such a name: the directory that
/// the creation made is known by its lock file. Not where a link to it is
/// beside it. What was there before is found only under the spelling that
/// is listed: a deletion of the checkpoint, and the taking over and the
/// removal of what a creation left. Under the other spelling they are
/// refused, and nothing is locked, made or removed.
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
    let given = |name: &str| checkpoints.join(format!("{ONE_LETTER}{name}"));
    let as_listed = |name: &str| checkpoints.join(format!("{TWO_LETTERS}{name}"));

    // The directory above lists neither the directory that is built in
    // nor the checkpoint under the name that was given.
    let target = given("-1");
    let during = |step: Step| {
        if step == Step::Settled {
            assert_eq!(
                listed(&checkpoints),
                [format!("{TWO_LETTERS}-1{CREATING_SUFFIX}")]
            );
        }
    };
    (maker.pausing(&during).create(&target)).expect("a checkpoint");
    assert_eq!(listed(&checkpoints), [format!("{TWO_LETTERS}-1")]);
    assert!(listed(&target).contains(&format!("{TWO_LETTERS}.engine")));
    assert_eq!(
        std::fs::read(target.join(&carried_name)).expect("the carried file"),
        b"what the engine keeps"
    );
    open(&db, &target).check(&world.model);

    // A link to the directory that is built in, beside it: the lock file
    // is then found through the link too, and the directory is not known
    // by it. Nothing gets the name of the target.
    let second = given("-2");
    let alias = checkpoints.join("alias");
    let link = |step: Step| {
        if step == Step::Settled {
            std::os::unix::fs::symlink(staging(&second), &alias).expect("a link");
        }
    };
    let refused = maker.pausing(&link).create(&second);
    assert!(
        matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == staging(&second)),
        "{refused:?}"
    );
    assert!(!second.exists() && built_in(&second).join("MANIFEST").exists());
    std::fs::remove_file(&alias).expect("the link is removed");

    // What that creation left, and the checkpoint: under the spelling
    // that was given they are not found, and nothing of them goes.
    let left = BTreeSet::from([
        format!("{TWO_LETTERS}-1"),
        format!("{TWO_LETTERS}-2.creating"),
    ]);
    let refused = maker.create(&second);
    assert!(
        matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if *dir == staging(&second)),
        "{refused:?}"
    );
    for (deleted, said) in [
        (&second, &second),
        (&staging(&second), &staging(&second)),
        (&target, &target),
    ] {
        let refused = checkpoint::delete(&*db.vfs, deleted);
        assert!(
            matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if dir == said),
            "{refused:?}"
        );
    }
    assert_eq!(BTreeSet::from_iter(listed(&checkpoints)), left);
    assert!(built_in(&second).join("MANIFEST").exists());
    open(&db, &target).check(&world.model);
    assert_eq!(versions.pins_held(), 0);

    // Under the spelling that is listed, what was left is taken over by
    // a creation, and removed by a deletion, and the checkpoint is
    // deleted.
    maker.create(&as_listed("-2")).expect("a checkpoint");
    assert_eq!(
        BTreeSet::from_iter(listed(&checkpoints)),
        BTreeSet::from([format!("{TWO_LETTERS}-1"), format!("{TWO_LETTERS}-2")])
    );
    open(&db, &second).check(&world.model);
    for deleted in [as_listed("-3"), staging(&as_listed("-3"))] {
        let built = staging(&given("-3"));
        leave_built(&built, false);
        checkpoint::delete(&*db.vfs, &deleted).expect("what was left is removed");
        assert!(!built.exists());
    }
    checkpoint::delete(&*db.vfs, &as_listed("-1")).expect("the checkpoint is deleted");
    assert_eq!(listed(&checkpoints), [format!("{TWO_LETTERS}-2")]);

    // A link under such a name is no directory that was there: it is
    // refused before a lock is taken, and nothing is made where it leads.
    let outside = tempfile::tempdir().expect("a directory");
    let theirs = outside.path().join("theirs");
    std::fs::create_dir(&theirs).expect("a directory");
    std::fs::write(theirs.join("kept"), b"as it was").expect("a file");
    let as_it_was = BTreeMap::from([(PathBuf::from("kept"), Some(b"as it was".to_vec()))]);
    let fourth = given("-4");
    let built = staging(&fourth);
    std::os::unix::fs::symlink(&theirs, &built).expect("a link");
    let refused = maker.create(&fourth);
    assert!(
        matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if *dir == built),
        "{refused:?}"
    );
    for (deleted, said) in [(&fourth, &fourth), (&built, &built)] {
        let refused = checkpoint::delete(&*db.vfs, deleted);
        assert!(
            matches!(&refused, Err(CheckpointError::NotADirectory { dir }) if dir == said),
            "{refused:?}"
        );
    }
    assert!(built.is_symlink() && !fourth.exists());
    assert_eq!(everything_in(&StdVfs, &theirs), as_it_was);
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// What a creation left is found under the name that the directory above
/// lists, and by nothing else. Once its lock is taken, that directory
/// lists it in another spelling, as it would had somebody renamed it so
/// meanwhile: it is not known by its lock file, as a directory is that
/// the creation made itself, and nothing in it is removed.
#[test]
fn what_was_there_is_not_known_by_its_lock_file_alone() {
    let respelling = Arc::new(AtomicBool::new(false));
    let (db, versions) = store_on_meddling(|plain, checkpoints| {
        let from_the_lock = respelling.clone();
        let among = checkpoints.clone();
        let before_lock: Before = Box::new(move |lock: &Path| {
            if lock.starts_with(&among) {
                from_the_lock.store(true, Ordering::SeqCst);
            }
            Ok(())
        });
        Meddling {
            respelling: respelling.clone(),
            before_lock: Some(before_lock),
            ..Meddling::respelled(plain, checkpoints)
        }
    });
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(85), 0, 10);
    let checkpoints = checkpoints_in(&db);
    let target = checkpoints.join(format!("{ONE_LETTER}-1"));
    let built = staging(&target);
    leave_built(&built, false);
    let is_left = |refused: Result<(), CheckpointError>| {
        assert!(
            matches!(&refused, Err(CheckpointError::Replaced { dir }) if *dir == built),
            "{refused:?}"
        );
        assert!(
            respelling.swap(false, Ordering::SeqCst),
            "the lock was taken"
        );
        assert_eq!(
            std::fs::read(left_in(&built)).expect("the file"),
            b"what a creation left"
        );
        assert!(built.join("LOCK").exists() && !target.exists());
    };

    is_left(checkpointer(&db, &versions).create(&target).map(drop));
    assert_eq!(versions.pins_held(), 0);
    for given in [&built, &target] {
        is_left(checkpoint::delete(&*db.vfs, given));
    }
    world.check_committed();
}

/// A creation asks for its syncs as two sets, and for one more: the log's
/// own, where the log is not durable through the version. The first set
/// has the manifest and every copy, each directory of the checkpoint, the
/// directory it is built in, the staging directory that one is in and the
/// directory above. The second, after the rename, has the two directories
/// of the rename: the one above the target and the staging directory.
/// Nothing is synced for the removal of the staging directory.
#[test]
fn a_creation_asks_for_two_sets_of_syncs() {
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
        // the directory it was built in, the staging directory and the
        // one above.
        let in_set = 1 + created.copied + dirs + 3;
        assert_eq!(sets, [in_set, 2], "{context}");
        assert_eq!(
            syncs as usize,
            in_set + 2 + usize::from(!durable),
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
            "/c/1.creating/LOCK is not listed as a file, and none was made there",
            false,
        ),
        (
            CheckpointError::NotStaging {
                dir: PathBuf::from("/c/1.creating"),
                found: "CURRENT".into(),
            },
            "/c/1.creating holds \"CURRENT\", which no creation of a checkpoint puts into its \
             staging directory: the directory is not taken for what a creation left, and \
             nothing in it was removed",
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
