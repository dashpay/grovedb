//! What a checkpoint's directory is made of: links where the file system
//! makes them and copies where it does not, the files the engine has it
//! carry, and the deletion of a checkpoint.

use std::{
    io::ErrorKind,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{manifest::Manifest, FileKind};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    flush::FlushConfig,
    segment::{read_manifest, FileNames, SegmentStoreError},
    wal::LogOptions,
};

use super::{block, checkpointer, checkpoints_in, open, sim, store_files, store_on, turn, NoLinks};
use crate::{
    checkpoint::{self, create::Step, CheckpointError, CheckpointOptions},
    snapshot::Versions,
    txn::tests::{
        store::{Db, Rng, SIZE},
        World,
    },
};

/// A store in a directory of its own in memory, on a file layer that makes
/// no hard link into the directory of its checkpoints.
fn store_without_links(kind: ErrorKind) -> (Arc<Db>, Versions) {
    let dir = tempfile::tempdir_in(match Path::new("/dev/shm").is_dir() {
        true => PathBuf::from("/dev/shm"),
        false => std::env::temp_dir(),
    })
    .expect("a directory");
    let path = dir.path().to_path_buf();
    let vfs = NoLinks {
        inner: Arc::new(StdVfs),
        under: path.join("checkpoints"),
        kind,
    };
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

/// Every numbered file that `manifest` names, as its path inside the
/// database directory and the length the manifest gives it.
fn named(manifest: &Manifest) -> Vec<(PathBuf, u64)> {
    let dir = Path::new("");
    let files = manifest.files().iter().map(|file| {
        let names = match file.kind() {
            FileKind::Wal => FileNames::WAL,
            FileKind::Segment => FileNames::SEGMENT,
            _ => FileNames::VALUE_LOG,
        };
        (names.path(dir, file.id()), file.valid_len())
    });
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

/// The same on two file systems of the machine, where it has two: the
/// store is in memory and the checkpoint on the disk of the temporary
/// directory.
#[test]
fn a_checkpoint_on_another_file_system_is_copied() {
    let memory = Path::new("/dev/shm");
    let elsewhere = std::env::temp_dir();
    let device = |path: &Path| std::fs::metadata(path).map(|at| at.dev()).ok();
    if device(memory).is_none() || device(memory) == device(&elsewhere) {
        // One file system: `where_no_link_can_be_made_the_files_are_copied`
        // is the test.
        return;
    }
    // Few files: every sync of a copy is one of that disk.
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(62);
    for round in 0..2 {
        block(&mut world, &mut rng, round * 10, 10);
        turn(&db, &versions);
    }
    let other = tempfile::tempdir_in(&elsewhere).expect("a directory on the other file system");
    let target = other.path().join("checkpoint");
    let created = (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    assert!(created.linked == 0 && created.copied > 0, "{created:?}");
    // Each copy is the live file up to the length the manifest gives it.
    // That such a directory opens is shown where the store is in memory,
    // since every sync of an open on this disk can take seconds.
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

    for name in [
        "",
        ".",
        "..",
        "../ENGINE",
        "wal/00000001.log",
        "MANIFEST",
        "LOCK",
    ] {
        let names = [name];
        let target = checkpoints.join("refused");
        let refused = checkpointer(&db, &versions)
            .carrying(&names)
            .create(&target);
        assert!(
            matches!(&refused, Err(CheckpointError::CarriedName { name: said }) if said == name),
            "{name:?}: {refused:?}"
        );
        assert!(!target.exists(), "{name:?} left a directory");
    }
    let missing = ["ENGINE", "not-there"];
    let target = checkpoints.join("missing");
    let failed = checkpointer(&db, &versions)
        .carrying(&missing)
        .create(&target);
    assert!(
        matches!(&failed, Err(CheckpointError::Io { source, .. })
            if source.kind() == ErrorKind::NotFound),
        "{failed:?}"
    );
    assert!(!target.exists());

    // Where no link can be made, the file is copied whole.
    let (db, versions) = store_without_links(ErrorKind::CrossesDevices);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(65), 0, 10);
    std::fs::write(db.dir.join("ENGINE"), b"what the engine keeps").expect("a file");
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
    let (db, versions) = store_with_links();
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

    // What a deletion that was cut short behind the manifest leaves is
    // finished: the lock file alone, or nothing.
    for lock_left in [true, false] {
        let leftover = checkpoints.join("leftover");
        std::fs::create_dir(&leftover).expect("a directory");
        if lock_left {
            std::fs::write(leftover.join("LOCK"), b"").expect("a file");
        }
        checkpoint::delete(&StdVfs, &leftover).expect("the leftover is deleted");
        assert!(!leftover.exists());
    }

    // No database: a directory of the caller's, and nothing at all.
    let not_one = checkpoints.join("not-one");
    std::fs::create_dir(&not_one).expect("a directory");
    std::fs::write(not_one.join("kept"), b"as it was").expect("a file");
    for dir in [&not_one, &checkpoints.join("nothing")] {
        let refused = checkpoint::delete(&StdVfs, dir);
        assert!(
            matches!(
                &refused,
                Err(CheckpointError::NotADatabase {
                    source: SegmentStoreError::NoManifest { .. },
                    ..
                })
            ),
            "{refused:?}"
        );
    }
    assert_eq!(
        std::fs::read(not_one.join("kept")).expect("the file"),
        b"as it was"
    );
}

fn store_with_links() -> (Arc<Db>, Versions) {
    super::store_with_log(24 << 10, 128)
}

/// A deletion is durable when it returns, and one that is cut short can be
/// made again: the manifest goes last, so what is left is still known for
/// what it is.
#[test]
fn a_deletion_that_is_cut_short_can_be_made_again() {
    use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};

    let fs = sim(67);
    let (db, versions) = store_on(&fs);
    let mut world = World::new(&db, &versions);
    block(&mut world, &mut Rng::new(67), 0, 10);
    (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
    let target = checkpoints_in(&db).join("1");
    (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
    let files = store_files(&fs, &target).len();
    assert!(files >= 3);

    // Each attempt removes one file, and its second removal fails, until
    // the attempt that has one file left to remove.
    let removals = OpSet::of(&[OpKind::RemoveFile]);
    let mut attempts = 0;
    loop {
        fs.add_rule(FaultRule::new(Fault::Eio).on(removals).skip(1));
        let deleted = checkpoint::delete(&fs, &target);
        fs.clear_rules();
        attempts += 1;
        let Err(failed) = deleted else {
            break;
        };
        assert!(
            matches!(
                &failed,
                CheckpointError::Io {
                    action: "remove",
                    ..
                }
            ),
            "{failed}"
        );
        // Until everything else is gone, the manifest is there.
        let left = fs.read_dir(&target).expect("the directory");
        let lock_alone = left.iter().all(|entry| entry.name == "LOCK");
        assert_eq!(read_manifest(&fs, &target).is_err(), lock_alone, "{left:?}");
        assert!(attempts < 100, "the deletion does not come to an end");
    }
    // The files, then the manifest, and the lock file alone at the last.
    assert_eq!(attempts, files + 2);
    assert_eq!(
        fs.read_dir(&target).expect_err("it is gone").kind(),
        ErrorKind::NotFound
    );
    // Durable: a power cut that loses every unsynced change does not
    // bring it back.
    let image = fs.crash_image();
    let left = image.recover(&image.lose_all()).expect("a state");
    assert!(left.read_dir(&target).is_err());
    world.check_committed();
}

/// The power is cut after each operation of a deletion. In every state the
/// crash can leave, the directory is gone, or the deletion can be made
/// again and ends it: nothing is left that the call refuses.
#[test]
fn a_deletion_that_the_power_cuts_can_be_made_again() {
    use lignin_io::sim::CrashPoint;

    let build = || {
        let fs = sim(68);
        let (db, versions) = store_on(&fs);
        let mut world = World::new(&db, &versions);
        block(&mut world, &mut Rng::new(68), 0, 10);
        (db.write_back(&versions).checkpoint()).expect("a table checkpoint");
        let target = checkpoints_in(&db).join("1");
        (checkpointer(&db, &versions).create(&target)).expect("a checkpoint");
        fs.create_dir(&target.join("theirs")).expect("a directory");
        (fs.sync_dir(&target)).expect("the caller's directory is durable");
        drop(world);
        (fs, target)
    };
    let (fs, target) = build();
    let from = fs.stats().ops;
    checkpoint::delete(&fs, &target).expect("the checkpoint is deleted");
    let ops = fs.stats().ops - from;
    assert!(ops > 8, "a deletion of {ops} operations");

    let (mut gone, mut finished) = (0, 0);
    for cut in 1..=ops {
        let (fs, target) = build();
        assert_eq!(fs.stats().ops, from, "the same store");
        fs.crash_at(CrashPoint::AfterOp(from + cut));
        let deleted = checkpoint::delete(&fs, &target);
        assert_eq!(
            deleted.is_ok(),
            cut == ops,
            "cut {cut} of {ops}: {deleted:?}"
        );
        let image = fs.crash_image();
        let mut plans = vec![image.lose_all(), image.keep_all()];
        plans.extend(image.prefixes());
        plans.extend(image.holes());
        plans.extend((0..8).map(|draw| image.sample(draw)));
        for (index, plan) in plans.iter().enumerate() {
            let Ok(left) = image.recover(plan) else {
                continue;
            };
            let context = format!("cut {cut} of {ops}, crash plan {index}");
            if left.read_dir(&target).is_err() {
                gone += 1;
                continue;
            }
            assert!(deleted.is_err(), "{context}: the deletion came back");
            checkpoint::delete(&left, &target).unwrap_or_else(|error| panic!("{context}: {error}"));
            assert!(left.read_dir(&target).is_err(), "{context}");
            finished += 1;
        }
    }
    assert!(gone > 0 && finished > 0, "{gone}, {finished}");
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
