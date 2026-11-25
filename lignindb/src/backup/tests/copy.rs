use std::{error::Error, path::Path, sync::Arc, time::Duration};

use lignin_format::{manifest::FileState, FileKind, PageSize};
use lignin_io::{
    sim::{CrashPoint, Event, Fault, FaultRule, OpKind, OpSet, Outcome, SimConfig, SimVfs},
    Create, OpenMode, Vfs,
};
use lignin_store::segment::{read_manifest, FileNames, ManifestStore};

use super::world::{check_reads, write, State};
use crate::{
    backup::{Backup, BackupError, Copied},
    db::OpenError,
    space::out_of_space,
    txn::tests::store::Rng,
    Db, Options,
};

const SOURCE: &str = "/db";
const TARGET: &str = "/backups/one";

fn options(fs: &SimVfs) -> Options {
    Options {
        page_size: Some(PageSize::MIN),
        cache_size: 16 << 20,
        idle: Duration::from_millis(20),
        vfs: Arc::new(fs.clone()),
        ..Options::default()
    }
}

/// A file system for a target: empty but for the directory that backups
/// are made in.
fn target_fs(seed: u64) -> SimVfs {
    let fs = SimVfs::new(SimConfig::new(seed));
    fs.create_dir(Path::new("/backups")).expect("a directory");
    fs.sync_dir(Path::new("/")).expect("its name is durable");
    fs
}

/// A database in `/db` of a file system of its own, written by `writes`
/// commits drawn from `seed`, with the state it holds. With `closed` its
/// session was ended by a close. Without, the file system is what a
/// process that is killed leaves: nothing was finished, and nobody holds
/// its lock.
fn source(seed: u64, writes: u64, closed: bool) -> (SimVfs, State) {
    let fs = SimVfs::new(SimConfig::new(seed));
    let db = Db::open(SOURCE, &options(&fs)).expect("a new database");
    let mut state = State::new();
    let mut rng = Rng::new(seed);
    for number in 1..=writes {
        write(&db, &mut state, &mut rng, number);
    }
    if closed {
        db.close().expect("the session ends");
        return (fs, state);
    }
    // Everything that was written is in the files, as after a process
    // that exits, and nothing of a close was done.
    let image = fs.crash_image();
    let killed = image.recover(&image.keep_all()).expect("the plan fits");
    drop(db);
    (killed, state)
}

/// Opens the database in `dir` of `fs`, which must hold one, and compares
/// it with `state`.
fn check_database(fs: &SimVfs, dir: &str, state: &State, context: &str) {
    let options = Options {
        create_if_missing: false,
        ..options(fs)
    };
    let db = Db::open(dir, &options).unwrap_or_else(|error| panic!("{context}: {error}"));
    check_reads(&db, state, context);
    db.close()
        .unwrap_or_else(|error| panic!("{context}: {error}"));
}

/// Whether `dir` of `fs` holds no database: it is not there, or nothing
/// opens it as one.
fn is_no_database(fs: &SimVfs, dir: &str) -> bool {
    let options = Options {
        create_if_missing: false,
        ..options(fs)
    };
    match Db::open(dir, &options) {
        Err(OpenError::NoDatabase { .. } | OpenError::NoManifest { .. }) => true,
        Err(other) => panic!("{dir}: {other}"),
        Ok(_) => false,
    }
}

fn between<'a>(from: &SimVfs, to: &SimVfs) -> Backup<'a> {
    Backup::between(Arc::new(from.clone()), Arc::new(to.clone()))
}

fn copy(from: &SimVfs, to: &SimVfs) -> Result<Copied, BackupError> {
    between(from, to).copy(Path::new(SOURCE), Path::new(TARGET))
}

/// A copy of a database that was closed, and of one whose process was
/// killed, opens at the version the source opens at, with every record,
/// and shares nothing with it: the source can go, and the copy is
/// restored from by the same call.
#[test]
fn a_copy_opens_at_the_version_of_its_source_with_every_record() {
    for (seed, closed) in [(1, true), (2, true), (3, false), (4, false)] {
        let context = format!("seed {seed}, closed {closed}");
        let (fs, state) = source(seed, 30, closed);
        let before = fs.fingerprint();
        let to = target_fs(seed);
        let copied = copy(&fs, &to).unwrap_or_else(|error| panic!("{context}: {error}"));
        assert_eq!(copied.version.get(), state.version(), "{context}");
        assert!(
            copied.files >= 3 && copied.bytes > 0,
            "{context}: {copied:?}"
        );
        assert_eq!((copied.carried, copied.carried_bytes), (0, 0));
        assert_eq!(
            fs.fingerprint(),
            before,
            "{context}: the source was changed"
        );

        // The copy is durable when the call returns: whatever a power cut
        // keeps of what was not synced, the copy is whole.
        let image = to.crash_image();
        let cut = image.recover(&image.lose_all()).expect("the plan fits");
        check_database(
            &cut,
            TARGET,
            &state,
            &format!("{context}, after a power cut"),
        );

        // A restore is the same call the other way, into a third place,
        // and what is restored is written like any database.
        let restored = SimVfs::new(SimConfig::new(seed + 100));
        Backup::between(Arc::new(to.clone()), Arc::new(restored.clone()))
            .copy(Path::new(TARGET), Path::new(SOURCE))
            .unwrap_or_else(|error| panic!("{context}: the restore: {error}"));
        check_database(&to, TARGET, &state, &format!("{context}, the copy"));
        check_database(&restored, SOURCE, &state, &format!("{context}, restored"));
        let db = Db::open(SOURCE, &options(&restored)).expect("the restored database");
        let original = state.clone();
        let mut state = state;
        let mut rng = Rng::new(seed + 7);
        for number in 100..110 {
            write(&db, &mut state, &mut rng, number);
        }
        check_reads(
            &db,
            &state,
            &format!("{context}, written after the restore"),
        );
        db.close().expect("the session ends");
        // The source is as it was, and opens.
        check_database(&fs, SOURCE, &original, &context);
    }
}

/// What a copy refuses, each with nothing made: a target that is there, a
/// target with no directory above it, a source that is open, a source
/// with no lock file, a directory that is no database.
#[test]
fn a_copy_refuses_what_it_cannot_make_a_backup_of() {
    let (fs, state) = source(11, 6, true);
    let to = target_fs(11);
    let untouched = to.fingerprint();

    // A target that is there: a directory, and a file.
    to.create_dir(Path::new(TARGET)).expect("a directory");
    let taken = to.fingerprint();
    let error = copy(&fs, &to).expect_err("the target is there");
    assert!(matches!(&error, BackupError::TargetExists { target } if target == Path::new(TARGET)));
    assert!(error.to_string().contains("is there already") && error.source().is_none());
    assert_eq!(to.fingerprint(), taken);
    to.remove_dir(Path::new(TARGET)).expect("it is removed");
    drop(
        to.open(Path::new(TARGET), OpenMode::Write(Create::New))
            .expect("a file"),
    );
    let error = copy(&fs, &to).expect_err("the target is a file");
    assert!(matches!(error, BackupError::TargetExists { .. }), "{error}");
    to.remove_file(Path::new(TARGET)).expect("it is removed");
    assert_eq!(to.fingerprint(), untouched);

    // A target with no directory above it.
    let error = between(&fs, &to)
        .copy(Path::new(SOURCE), Path::new("/"))
        .expect_err("the root has no directory above it");
    assert!(matches!(error, BackupError::NoParent { .. }), "{error}");
    assert!(error.to_string().contains("no directory above it"));
    let error = between(&fs, &to)
        .copy(Path::new(SOURCE), Path::new("/nowhere/one"))
        .expect_err("the directory above is not there");
    assert!(
        matches!(&error, BackupError::Io { action, .. } if *action == "create the directory"),
        "{error}"
    );
    assert!(error.source().is_some() && error.to_string().contains("cannot create"));
    assert_eq!(to.fingerprint(), untouched);

    // A target inside the source: nothing is made in a directory that is
    // only read.
    let source_was = fs.fingerprint();
    let error = Backup::new(Arc::new(fs.clone()))
        .copy(Path::new(SOURCE), Path::new("/db/backup"))
        .expect_err("the target is inside the source");
    assert!(
        matches!(
            &error,
            BackupError::TargetInSource { dir, target }
                if dir == Path::new(SOURCE) && target == Path::new("/db/backup")
        ),
        "{error}"
    );
    assert!(error.to_string().contains("is inside /db") && error.source().is_none());
    assert_eq!(fs.fingerprint(), source_was);

    // A source that a handle has open.
    let db = Db::open(SOURCE, &options(&fs)).expect("the database");
    let error = copy(&fs, &to).expect_err("the source is open");
    assert!(
        matches!(&error, BackupError::Open { dir } if dir == Path::new(SOURCE)),
        "{error}"
    );
    assert!(error.to_string().contains("is open in a handle"));
    assert_eq!(to.fingerprint(), untouched);
    db.close().expect("the session ends");

    // A directory that is no database, with a lock file and with none.
    let error = between(&to, &fs)
        .copy(Path::new("/backups"), Path::new("/copy"))
        .expect_err("no lock file");
    assert!(matches!(&error, BackupError::NoLockFile { .. }), "{error}");
    assert!(error.to_string().contains("holds no LOCK file"));
    drop(
        to.lock(Path::new("/backups/LOCK"))
            .expect("a lock file is made"),
    );
    let error = between(&to, &fs)
        .copy(Path::new("/backups"), Path::new("/copy"))
        .expect_err("no manifest");
    assert!(
        matches!(&error, BackupError::NotADatabase { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("is no database") && error.source().is_some());
    let error = between(&to, &fs)
        .copy(Path::new("/not-there"), Path::new("/copy"))
        .expect_err("no directory");
    assert!(matches!(&error, BackupError::NoLockFile { .. }), "{error}");
    assert!(fs.read_dir(Path::new("/copy")).is_err(), "nothing was made");

    // The source that was refused for being open is copied once it is
    // closed, and is the database it was.
    let copied = copy(&fs, &to).expect("the copy is made");
    assert_eq!(copied.version.get(), state.version());
    check_database(&to, TARGET, &state, "after the refusals");
}

/// A file that the manifest of the source names and that is missing, or
/// shorter than the manifest vouches for, ends the copy before anything is
/// made.
#[test]
fn a_source_with_a_file_missing_or_short_is_refused_before_anything_is_made() {
    for seed in [21, 22] {
        let (fs, _) = source(seed, 12, true);
        let manifest = read_manifest(&fs, Path::new(SOURCE)).expect("the manifest");
        let names = |kind| match kind {
            lignin_format::FileKind::Wal => FileNames::WAL,
            lignin_format::FileKind::Segment => FileNames::SEGMENT,
            _ => FileNames::VALUE_LOG,
        };
        let mut tried = Vec::new();
        for file in manifest.files() {
            // A file of no byte cannot be made shorter.
            if file.valid_len() == 0 {
                continue;
            }
            tried.push(file.kind());
            let path = names(file.kind()).path(Path::new(SOURCE), file.id());
            // Short by one byte, in a copy of the file system.
            let image = fs.crash_image();
            let damaged = image.recover(&image.keep_all()).expect("the plan fits");
            let open = damaged
                .open(&path, OpenMode::Write(Create::Never))
                .expect("the file");
            open.set_len(file.valid_len() - 1).expect("it is cut");
            drop(open);
            let to = target_fs(seed);
            let untouched = to.fingerprint();
            let error = copy(&damaged, &to).expect_err("a file is short");
            let BackupError::Short {
                path: named,
                valid_len,
                len,
            } = &error
            else {
                panic!("{error}");
            };
            assert_eq!(
                (named, *valid_len, *len),
                (&path, file.valid_len(), Some(file.valid_len() - 1))
            );
            assert!(error.to_string().contains("the manifest vouches for"));
            assert_eq!(to.fingerprint(), untouched, "nothing was made");

            damaged.remove_file(&path).expect("the file is removed");
            let error = copy(&damaged, &to).expect_err("a file is missing");
            assert!(
                matches!(&error, BackupError::Short { len: None, .. }),
                "{error}"
            );
            assert!(error.to_string().contains("is not there") && error.source().is_none());
            assert_eq!(to.fingerprint(), untouched, "nothing was made");
        }
        // A file of each kind that a manifest names was tried. That a
        // close leaves a log file, a segment and, with the values these
        // seeds draw, a value-log file, is what a close does. How many
        // of each it leaves, and how long they are, is the store's
        // layout and the schedule of the source's background work: a
        // log file can hold its header and nothing else.
        for kind in [FileKind::Wal, FileKind::Segment, FileKind::ValueLog] {
            assert!(
                tried.contains(&kind),
                "seed {seed}: no file of the kind {kind:?} was tried: {tried:?}"
            );
        }
    }
}

/// What lies behind the valid length of a file that the manifest names
/// closed or sealed is not the database's, and a copy does not have it. A
/// file that it names open is copied whole.
#[test]
fn a_copy_holds_each_file_as_far_as_the_manifest_says_it_is_the_databases() {
    let (fs, state) = source(31, 20, false);
    // The database is opened and closed once more, so that the files of
    // the session that was killed are named closed, and then bytes are
    // put behind the end of each of them, as a database that shares them
    // with a checkpoint appends to them.
    Db::open(SOURCE, &options(&fs))
        .expect("the database is recovered")
        .close()
        .expect("the session ends");
    let manifest = read_manifest(&fs, Path::new(SOURCE)).expect("the manifest");
    let names = |kind| match kind {
        lignin_format::FileKind::Wal => FileNames::WAL,
        lignin_format::FileKind::Segment => FileNames::SEGMENT,
        _ => FileNames::VALUE_LOG,
    };
    let (mut closed, mut open) = (0, 0);
    for file in manifest.files() {
        let path = names(file.kind()).path(Path::new(SOURCE), file.id());
        if file.state() == FileState::Open {
            open += 1;
            continue;
        }
        closed += 1;
        let behind = fs
            .open(&path, OpenMode::Append(Create::Never))
            .expect("the file");
        behind.append(&[0xab; 700]).expect("bytes behind the end");
    }
    assert!(closed >= 3 && open >= 1, "{closed} closed, {open} open");

    let to = target_fs(31);
    let copied = copy(&fs, &to).expect("the copy is made");
    let mut bytes = 0;
    for file in manifest.files() {
        let from = names(file.kind()).path(Path::new(SOURCE), file.id());
        let path = names(file.kind()).path(Path::new(TARGET), file.id());
        let len = |fs: &SimVfs, path: &Path| {
            let file = fs.open(path, OpenMode::Read).expect("the file");
            file.len().expect("its length")
        };
        let copied = len(&to, &path);
        match file.state() {
            FileState::Open => assert_eq!(copied, len(&fs, &from), "{}", path.display()),
            _ => assert_eq!(copied, file.valid_len(), "{}", path.display()),
        }
        bytes += copied;
    }
    let table = manifest
        .table_checkpoint()
        .map_or(0, |table| table.valid_len());
    assert_eq!(copied.bytes, bytes + table);
    assert_eq!(
        read_manifest(&to, Path::new(TARGET)).expect("the manifest of the copy"),
        manifest,
        "the manifest is the source's"
    );
    check_database(&to, TARGET, &state, "the copy");
    // The source reads its closed files to their valid lengths too.
    check_database(&fs, SOURCE, &state, "the source");
}

/// The files that the caller names are carried beside the store's, whole,
/// and a name that is no plain file of the source is refused before
/// anything is made.
#[test]
fn a_copy_carries_the_files_it_is_told_to() {
    let (fs, state) = source(41, 8, true);
    let write = |name: &str, bytes: &[u8]| {
        let path = Path::new(SOURCE).join(name);
        let file = fs
            .open(&path, OpenMode::Append(Create::New))
            .expect("a file");
        file.append(bytes).expect("its bytes");
        file.sync_all().expect("it is durable");
    };
    write("platform_state.bin", &[7; 5000]);
    write("empty.marker", &[]);
    fs.create_dir(&Path::new(SOURCE).join("checkpoints"))
        .expect("a directory");
    fs.sync_dir(Path::new(SOURCE))
        .expect("the names are durable");

    let to = target_fs(41);
    let untouched = to.fingerprint();
    for name in [
        "checkpoints",
        "missing",
        "seg/00000001.seg",
        "../db/platform_state.bin",
        ManifestStore::FILE_NAME,
        ManifestStore::TEMP_FILE_NAME,
        "MANIFEST.deleting",
        "LOCK",
    ] {
        let names = [name];
        let error = between(&fs, &to)
            .carrying(&names)
            .copy(Path::new(SOURCE), Path::new(TARGET))
            .expect_err("no file to carry");
        assert!(
            matches!(&error, BackupError::CarriedName { name: refused } if refused == name),
            "{name}: {error}"
        );
        assert!(error.to_string().contains("is not the plain name"));
        assert_eq!(to.fingerprint(), untouched, "{name}: nothing was made");
    }
    let twice = ["empty.marker", "empty.marker"];
    let error = between(&fs, &to)
        .carrying(&twice)
        .copy(Path::new(SOURCE), Path::new(TARGET))
        .expect_err("a name twice");
    assert!(matches!(error, BackupError::CarriedName { .. }), "{error}");

    let names = ["platform_state.bin", "empty.marker"];
    let backup = between(&fs, &to).carrying(&names);
    assert!(format!("{backup:?}").contains("platform_state.bin"));
    let copied = backup
        .copy(Path::new(SOURCE), Path::new(TARGET))
        .expect("the copy is made");
    assert_eq!((copied.carried, copied.carried_bytes), (2, 5000));
    let read = |name: &str| {
        let file = to
            .open(&Path::new(TARGET).join(name), OpenMode::Read)
            .expect("the carried file");
        let mut bytes = vec![0; file.len().expect("its length") as usize];
        file.read_exact_at(&mut bytes, 0).expect("its bytes");
        bytes
    };
    assert_eq!(read("platform_state.bin"), vec![7; 5000]);
    assert_eq!(read("empty.marker"), Vec::<u8>::new());
    // What nobody named stays behind.
    let entries = to.read_dir(Path::new(TARGET)).expect("the copy");
    assert!(entries.iter().all(|entry| entry.name != "checkpoints"));
    // The power is cut: the carried files are as durable as the rest.
    let image = to.crash_image();
    let cut = image.recover(&image.lose_all()).expect("the plan fits");
    let file = cut
        .open(
            &Path::new(TARGET).join("platform_state.bin"),
            OpenMode::Read,
        )
        .expect("the carried file");
    assert_eq!(file.len().expect("its length"), 5000);
    check_database(&cut, TARGET, &state, "the copy");
}

/// A carried file is in the copy as it is in the source, whatever its
/// name: one that has an open refuse the source as another engine's
/// directory has it refuse the copy.
#[test]
fn a_carried_file_that_an_open_refuses_is_carried_all_the_same() {
    let (fs, _) = source(43, 3, true);
    let path = Path::new(SOURCE).join("CURRENT");
    let file = fs
        .open(&path, OpenMode::Append(Create::New))
        .expect("a file");
    file.append(b"MANIFEST-000001").expect("its bytes");
    drop(file);
    let refused = |fs: &SimVfs, dir: &str| {
        matches!(
            Db::open(dir, &options(fs)),
            Err(OpenError::OtherEngine { .. })
        )
    };
    assert!(refused(&fs, SOURCE), "the source opens");
    let to = target_fs(43);
    let copied = between(&fs, &to)
        .carrying(&["CURRENT"])
        .copy(Path::new(SOURCE), Path::new(TARGET))
        .expect("the copy is made");
    assert_eq!(copied.carried, 1);
    assert!(refused(&to, TARGET), "the copy opens");
}

/// A target that was made and cannot be listed is taken away again, so
/// that the copy can be made once the device answers.
#[test]
fn a_target_that_cannot_be_listed_is_taken_away() {
    let (fs, state) = source(45, 4, true);
    let to = target_fs(45);
    to.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::ReadDir)
            .at(TARGET)
            .forever(),
    );
    let error = copy(&fs, &to).expect_err("the listing fails");
    assert!(
        matches!(&error, BackupError::Io { action, .. } if *action == "list"),
        "{error}"
    );
    assert!(to.stats().faults > 0, "no listing was failed");
    to.clear_rules();
    assert!(to.read_dir(Path::new(TARGET)).is_err(), "the target stays");
    let above = to.read_dir(Path::new("/backups")).expect("the directory");
    assert!(above.is_empty(), "{above:?}");
    let copied = copy(&fs, &to).expect("the copy is made");
    assert_eq!(copied.version.get(), state.version());
    check_database(&to, TARGET, &state, "after a listing that failed");
}

/// A directory in which a database was being made, cut at each operation
/// of the open that makes it: the copy opens as the source does. Where the
/// source holds a manifest and no commit yet, the copy is of version 0,
/// and its first open makes the database, as the next open of the source
/// does.
#[test]
fn a_copy_of_a_database_that_was_being_made_opens_as_the_source_does() {
    let opened = {
        let fs = SimVfs::new(SimConfig::new(47));
        let db = Db::open(SOURCE, &options(&fs)).expect("a new database");
        let ops = fs.stats().ops;
        drop(db);
        ops
    };
    let opens_at = |fs: &SimVfs, dir: &str, context: &str| {
        let db = Db::open(dir, &options(fs)).unwrap_or_else(|error| panic!("{context}: {error}"));
        let version = db.version().get();
        db.close()
            .unwrap_or_else(|error| panic!("{context}: {error}"));
        version
    };
    let (mut unmade, mut made) = (0, 0);
    for after in 1..=opened {
        let context = format!("cut after operation {after}");
        let fs = SimVfs::new(SimConfig::new(47));
        fs.crash_at(CrashPoint::AfterOp(after));
        drop(Db::open(SOURCE, &options(&fs)));
        // What a process leaves that was killed there.
        let image = fs.crash_image();
        let killed = image.recover(&image.keep_all()).expect("the plan fits");
        let to = target_fs(47);
        let copied = match copy(&killed, &to) {
            Ok(copied) => copied,
            // Before its lock file and its manifest, it is no database.
            Err(BackupError::NoLockFile { .. } | BackupError::NotADatabase { .. }) => continue,
            Err(other) => panic!("{context}: {other}"),
        };
        let landed = opens_at(&to, TARGET, &context);
        assert_eq!(landed, opens_at(&killed, SOURCE, &context), "{context}");
        match copied.version.get() {
            0 => {
                unmade += 1;
                assert_eq!(landed, 1, "{context}");
            }
            version => {
                made += 1;
                assert_eq!(landed, version, "{context}");
            }
        }
    }
    assert!(
        unmade > 0 && made > 0,
        "{unmade} copies of a database that was being made, {made} of one that was"
    );
}

/// OPS-08: the disk under the target is full at each operation of a copy
/// that can need space, for that operation alone and from it on. The copy
/// fails with an error that says so, the source is as it was, and the
/// target is no database: it is gone, or left without a manifest. A power
/// cut then leaves no database either, or the whole copy: the removal of
/// a manifest that was written is not durable on a device that refuses
/// the sync. With room again, the same call makes the copy.
#[test]
fn a_full_disk_under_the_target_fails_the_copy_and_leaves_no_database() {
    let (fs, state) = source(51, 14, true);
    let source_was = fs.fingerprint();
    // What was refused first in each run: whether for good, the kind of
    // the operation, and what it was on.
    let mut refused: Vec<(bool, OpKind, &str)> = Vec::new();
    for forever in [false, true] {
        for skip in 0.. {
            let to = target_fs(51);
            let empty = to.fingerprint();
            let rule = FaultRule::new(Fault::Enospc).skip(skip);
            to.add_rule(match forever {
                true => rule.forever(),
                false => rule,
            });
            let copied = copy(&fs, &to);
            if to.stats().faults == 0 {
                copied.expect("no fault, and no copy");
                break;
            }
            let first = to.trace().into_iter().find_map(|event| match event {
                Event::Op(op) if matches!(op.outcome, Outcome::Injected { .. }) => {
                    Some((forever, op.kind, place(&op.path)))
                }
                _ => None,
            });
            let first = first.expect("an operation was refused");
            if !refused.contains(&first) {
                refused.push(first);
            }
            let context =
                format!("space gone at operation {skip} of those that need it, forever {forever}");
            let Err(error) = copied else {
                panic!("{context}: a copy was made on a full disk");
            };
            assert!(
                matches!(error, BackupError::Io { .. } | BackupError::Install(_)),
                "{context}: {error}"
            );
            assert!(out_of_space(&error), "{context}: {error}");
            assert_eq!(
                fs.fingerprint(),
                source_was,
                "{context}: the source was changed"
            );
            assert!(is_no_database(&to, TARGET), "{context}: a database is left");
            if !forever {
                // Everything the copy made is gone again.
                assert_eq!(to.fingerprint(), empty, "{context}: something is left");
            }
            // A power cut now leaves no database either, or brings back
            // a manifest whose removal was not durable, with every file
            // it names.
            let image = to.crash_image();
            for plan in [image.lose_all(), image.keep_all(), image.sample(skip)] {
                let cut = image.recover(&plan).expect("the plan fits");
                if !is_no_database(&cut, TARGET) {
                    check_database(&cut, TARGET, &state, &context);
                }
            }
            // There is room again. What a full disk left of the target is
            // removed by whoever asked for the copy, and the copy is made.
            to.clear_rules();
            if to.read_dir(Path::new(TARGET)).is_ok() {
                let again = copy(&fs, &to).expect_err("the target is there");
                assert!(
                    matches!(again, BackupError::TargetExists { .. }),
                    "{context}: {again}"
                );
                remove_all(&to, Path::new(TARGET));
            }
            copy(&fs, &to).unwrap_or_else(|error| panic!("{context}: with room again: {error}"));
            if skip % 7 == 0 {
                check_database(&to, TARGET, &state, &context);
            }
        }
    }
    // The sweep refused each kind of operation by which a copy needs
    // room, on each kind of file that a copy makes, once and for good.
    // How many operations that is in all follows from how many files
    // the source has, which is the store's layout.
    let mut needed = vec![
        (OpKind::CreateDir, "a directory"),
        (OpKind::SyncDir, "a directory"),
        (OpKind::Lock, "the lock file"),
        (OpKind::Open, "the manifest"),
        (OpKind::Write, "the manifest"),
        (OpKind::SyncAll, "the manifest"),
        (OpKind::Rename, "the manifest"),
    ];
    for file in [
        "a log file",
        "a segment",
        "a value-log file",
        "a table checkpoint",
    ] {
        needed.extend([OpKind::Open, OpKind::Append, OpKind::SyncAll].map(|kind| (kind, file)));
    }
    for forever in [false, true] {
        for (kind, on) in &needed {
            assert!(
                refused.contains(&(forever, *kind, *on)),
                "no run was refused {kind} of {on}, forever {forever}: {refused:?}"
            );
        }
    }
}

/// What a copy makes at `path` of a target.
fn place(path: &str) -> &'static str {
    match path {
        _ if path.ends_with("LOCK") => "the lock file",
        _ if path.contains("MANIFEST") => "the manifest",
        _ if path.contains("/wal/") => "a log file",
        _ if path.contains("/seg/") => "a segment",
        _ if path.contains("/val/") => "a value-log file",
        _ if path.contains("/table/") => "a table checkpoint",
        _ => "a directory",
    }
}

/// Removes `path` with everything in it, as an operator removes what a
/// copy that failed has left.
fn remove_all(fs: &SimVfs, path: &Path) {
    for entry in fs.read_dir(path).expect("a directory") {
        let entry_path = path.join(&entry.name);
        match entry.kind {
            lignin_io::EntryKind::Dir => remove_all(fs, &entry_path),
            _ => fs.remove_file(&entry_path).expect("a file is removed"),
        }
    }
    fs.remove_dir(path).expect("the directory is removed");
}

/// OPS-08: the power is cut after each operation of a copy. Whatever the
/// cut keeps of what was not synced, the target is the whole copy or no
/// database, and once the call has returned it is the whole copy.
#[test]
fn a_power_cut_during_a_copy_leaves_the_whole_copy_or_no_database() {
    let (fs, state) = source(61, 10, true);
    let before = target_fs(61).stats().ops;
    let whole = {
        let to = target_fs(61);
        copy(&fs, &to).expect("the copy is made");
        to.stats().ops - before
    };
    let (mut databases, mut none) = (0, 0);
    for after in 1..=whole {
        let to = target_fs(61);
        to.crash_at(CrashPoint::AfterOp(before + after));
        let copied = copy(&fs, &to);
        let returned = copied.is_ok();
        assert_eq!(
            returned,
            after == whole,
            "cut after operation {after}: {copied:?}"
        );
        let image = to.crash_image();
        let mut plans = vec![image.lose_all(), image.keep_all()];
        plans.extend(image.prefixes().take(12));
        plans.extend(image.holes().take(12));
        plans.extend((0..6).map(|draw| image.sample(draw)));
        for (index, plan) in plans.iter().enumerate() {
            let Ok(cut) = image.recover(plan) else {
                continue;
            };
            let context = format!("cut after operation {after}, plan {index}");
            if returned || !is_no_database(&cut, TARGET) {
                check_database(&cut, TARGET, &state, &context);
                databases += 1;
            } else {
                none += 1;
            }
        }
    }
    assert!(
        databases > 10 && none > 100,
        "{databases} copies, {none} times none"
    );
}

/// A failed file operation that is no lack of space fails the copy the
/// same way, and the error does not say that space is missing.
#[test]
fn a_copy_that_the_device_fails_says_what_failed() {
    let (fs, _) = source(71, 6, true);
    let to = target_fs(71);
    to.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).skip(1));
    let error = copy(&fs, &to).expect_err("a sync failed");
    assert!(
        matches!(&error, BackupError::Io { action, .. } if *action == "sync"),
        "{error}"
    );
    assert!(!out_of_space(&error), "{error}");
    assert!(is_no_database(&to, TARGET));
    // The source cannot be read.
    let to = target_fs(72);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(lignin_io::sim::OpKind::Read)
            .skip(3),
    );
    let error = copy(&fs, &to).expect_err("a read failed");
    fs.clear_rules();
    assert!(
        matches!(&error, BackupError::Io { action, .. } if *action == "read"),
        "{error}"
    );
    assert!(is_no_database(&to, TARGET));
}

/// A database on the file system of the machine, in `dir`, written and
/// closed.
fn on_disk(dir: &Path, seed: u64) {
    let options = Options {
        page_size: Some(PageSize::MIN),
        cache_size: 16 << 20,
        idle: Duration::from_millis(20),
        ..Options::default()
    };
    let db = Db::open(dir, &options).expect("a new database");
    let mut state = State::new();
    let mut rng = Rng::new(seed);
    for number in 1..=4 {
        write(&db, &mut state, &mut rng, number);
    }
    db.close().expect("the session ends");
}

/// The path in `source` of the first file its manifest names, and the
/// store subdirectory that holds it.
fn first_named(source: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let manifest = read_manifest(&lignin_io::StdVfs, source).expect("the manifest");
    let file = manifest.files().first().expect("a named file");
    let names = match file.kind() {
        FileKind::Wal => FileNames::WAL,
        FileKind::Segment => FileNames::SEGMENT,
        FileKind::ValueLog => FileNames::VALUE_LOG,
        kind => panic!("{kind:?}"),
    };
    (names.path(source, file.id()), names.dir(source))
}

/// A source in which a symbolic link or a named pipe stands where the copy
/// reads a file (the lock file, the manifest, a file the manifest names
/// and the table checkpoint it names), or a link where the store
/// subdirectory of a named file is, is refused
/// before anything is made. The link is not followed, whatever it leads
/// to (here, to the very entry it replaced), so that a copy holds no byte
/// from outside its source; and the pipe is not opened, so that the copy
/// does not wait on it.
#[test]
fn a_source_with_a_link_or_a_pipe_where_the_copy_reads_is_refused() {
    use std::{fs, os::unix::fs::symlink};

    use super::world::{pipe, within};

    let place = tempfile::tempdir().expect("a directory");
    let source = place.path().join("db");
    on_disk(&source, 91);
    let (named, subdirectory) = first_named(&source);
    let table = {
        let manifest = read_manifest(&lignin_io::StdVfs, &source).expect("the manifest");
        let table = (manifest.table_checkpoint()).expect("the table checkpoint of a close");
        FileNames::PAGE_TABLE.path(&source, table.file())
    };
    let target = place.path().join("copy");
    let copy = {
        let (source, target) = (source.clone(), target.clone());
        move || Backup::new(Arc::new(lignin_io::StdVfs)).copy(&source, &target)
    };

    copy().expect("the copy is made");
    fs::remove_dir_all(&target).expect("the copy goes");

    let (lock, manifest) = (source.join("LOCK"), source.join(ManifestStore::FILE_NAME));
    for (entry, is_pipe) in [
        (&lock, false),
        (&lock, true),
        (&manifest, false),
        (&manifest, true),
        (&named, false),
        (&named, true),
        (&table, false),
        (&table, true),
        (&subdirectory, false),
    ] {
        // The entry goes outside the source, and a link to it, or a
        // pipe, takes its place.
        let outside = place.path().join("outside");
        fs::rename(entry, &outside).expect("the entry moves");
        match is_pipe {
            true => pipe(entry),
            false => symlink(&outside, entry).expect("a link"),
        }
        let what = format!("{} (a pipe: {is_pipe})", entry.display());

        let error = within(entry, copy.clone())
            .unwrap_or_else(|| panic!("{what}: the copy waited on it"))
            .expect_err("neither is opened");
        assert!(
            matches!(&error, BackupError::NotRegular { path } if path == entry),
            "{what}: {error}"
        );
        assert!(error.to_string().contains("nothing is copied"), "{what}");
        assert!(!target.exists(), "{what}: something was made");

        fs::remove_file(entry).expect("what took its place goes");
        fs::rename(&outside, entry).expect("the entry is back");
    }

    copy().expect("the copy is made");
}
