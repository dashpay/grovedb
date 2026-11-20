//! What a checkpoint's directory is made of: links where the file system
//! makes them and copies where it does not, the files the engine has it
//! carry, and the deletion of a checkpoint.

use std::{
    collections::BTreeMap,
    io::ErrorKind,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use lignin_format::manifest::Manifest;
use lignin_io::{sim::SimVfs, StdVfs, Vfs, VfsLock};
use lignin_store::{
    flush::FlushConfig,
    segment::{read_manifest, FileNames, SegmentStoreError},
    wal::LogOptions,
};

use super::{
    block, checkpointer, checkpoints_in, everything_in, names_of, open, open_on, sim, store_files,
    store_on, store_with_log, turn, write_file, Before, Meddling,
};
use crate::{
    checkpoint::{self, create::Step, CheckpointError, CheckpointOptions, DELETING_FILE_NAME},
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
                assert!(
                    store_files(&StdVfs, &target).is_empty(),
                    "nothing is copied yet"
                );
                for file in store_files(&StdVfs, &db.dir) {
                    std::fs::remove_file(db.dir.join(file)).expect("a live file is deleted");
                }
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

/// Where somebody else has the lock of the target that was just made, the
/// target is theirs: the creation fails and touches nothing in it. Where
/// the lock fails and nobody is in the target, the target goes.
#[test]
fn a_target_whose_lock_somebody_else_has_is_left_to_them() {
    let theirs: Arc<Mutex<Vec<Box<dyn VfsLock>>>> = Arc::default();
    let taken = theirs.clone();
    // An opener that finds the directory as soon as it is made, takes its
    // lock and begins a database of its own in it.
    let (db, versions) = store_with_locks_met(Box::new(move |lock| {
        let held = StdVfs.lock(lock)?;
        taken.lock().expect("their locks").push(held);
        std::fs::write(lock.with_file_name("MANIFEST"), b"theirs")
    }));
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(71), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let failed = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::Io { action: "lock", source, .. })
            if source.kind() == ErrorKind::WouldBlock),
        "{failed:?}"
    );
    assert_eq!(theirs.lock().expect("their locks").len(), 1);
    let left = BTreeMap::from([
        (PathBuf::from("LOCK"), Some(Vec::new())),
        (PathBuf::from("MANIFEST"), Some(b"theirs".to_vec())),
    ]);
    assert_eq!(everything_in(&StdVfs, &target), left);
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();

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
    assert!(!target.exists());
    world.check_committed();

    // An opener that is done with the directory, and has given its lock
    // back, before the creation takes it: the creation has the lock, and
    // the directory is the opener's all the same.
    let (db, versions) = store_with_locks_met(Box::new(|lock| {
        let held = StdVfs.lock(lock)?;
        std::fs::write(lock.with_file_name("MANIFEST"), b"theirs")?;
        std::fs::write(lock.with_file_name("MANIFEST.tmp"), b"theirs too")?;
        drop(held);
        Ok(())
    }));
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(74), 0, 10);
    let target = checkpoints_in(&db).join("1");
    let failed = checkpointer(&db, &versions).create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::TargetExists { target: theirs }) if *theirs == target),
        "{failed:?}"
    );
    let left = BTreeMap::from([
        (PathBuf::from("LOCK"), Some(Vec::new())),
        (PathBuf::from("MANIFEST"), Some(b"theirs".to_vec())),
        (PathBuf::from("MANIFEST.tmp"), Some(b"theirs too".to_vec())),
    ]);
    assert_eq!(everything_in(&StdVfs, &target), left);
    assert_eq!(versions.pins_held(), 0);
    world.check_committed();
}

/// A creation that fails takes away the files it made and no other: a file
/// that somebody put into the target under a name the creation was about
/// to use fails the creation, and stays.
#[test]
fn a_file_that_somebody_else_put_into_the_target_is_not_removed() {
    let (db, versions) = store_without_links(ErrorKind::CrossesDevices);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(73);
    for round in 0..3 {
        block(&mut world, &mut rng, round * 20, 20);
        turn(&db, &versions);
    }
    (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
    let target = checkpoints_in(&db).join("1");
    // The first file of the log is copied after the table checkpoint, the
    // segments and the value-log files.
    let files = store_files(&StdVfs, &db.dir);
    let theirs = (files.iter())
        .find(|file| file.starts_with("wal"))
        .expect("a log file");
    assert!(files.iter().any(|file| !file.starts_with("wal")));
    // They also keep a file under the name a manifest is written to
    // first. The creation fails before it writes one, so that is not its
    // own to remove either.
    let pause = |step: Step| {
        if step == Step::Secured {
            std::fs::write(target.join(theirs), b"theirs").expect("their file");
            std::fs::write(target.join("MANIFEST.tmp"), b"theirs too").expect("their file");
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
        (PathBuf::from("MANIFEST.tmp"), Some(b"theirs too".to_vec())),
        (PathBuf::from("wal"), None),
        (theirs.clone(), Some(b"theirs".to_vec())),
    ]);
    assert_eq!(everything_in(&StdVfs, &target), left);
    world.check_committed();

    // A manifest that somebody wrote into the target while it was made is
    // theirs too: the creation writes none over it, fails, and leaves it.
    let second = checkpoints_in(&db).join("2");
    let pause = |step: Step| {
        if step == Step::Secured {
            std::fs::write(second.join("MANIFEST"), b"theirs").expect("their file");
        }
    };
    let failed = checkpointer(&db, &versions).pausing(&pause).create(&second);
    assert!(
        matches!(
            &failed,
            Err(CheckpointError::Install(
                SegmentStoreError::ManifestExists { .. }
            ))
        ),
        "{failed:?}"
    );
    let left = BTreeMap::from([(PathBuf::from("MANIFEST"), Some(b"theirs".to_vec()))]);
    assert_eq!(everything_in(&StdVfs, &second), left);
    world.check_committed();
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
    let into = theirs.clone();
    let meddling = Meddling::before_remove(
        Arc::new(StdVfs),
        Box::new(move |_| std::fs::write(into.join("data"), b"put there meanwhile")),
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
            CheckpointError::Install(no_manifest()),
            "the manifest of the checkpoint was not made durable",
            true,
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
