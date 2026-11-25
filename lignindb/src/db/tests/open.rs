//! Opening, closing and the database directory (P-01 to P-04, P-18, P-35):
//! what an open makes, what it refuses and leaves as it was, and what it
//! never touches.

use std::{error::Error, fs, sync::Arc};

use lignin_format::{
    header::{FileHeader, FileKind},
    CommitVersion, PageSize,
};
use lignin_io::{
    sim::{SimConfig, SimVfs},
    Vfs,
};
use lignin_store::segment::ManifestStore;

use super::*;
use crate::db::{DbError, OpenError, WriteError};

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

#[test]
fn a_new_database_is_made_and_opened_again() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    assert!(db.created());
    assert_eq!(db.dir(), dir.path());
    assert_eq!(db.page_size(), PageSize::MIN);
    // The first commit made the keyspaces.
    assert_eq!(db.version(), version(1));
    assert!(format!("{db:?}").contains("version"));
    let keyspaces = db.keyspaces();
    let stats = db.stats();
    assert_eq!((stats.version, stats.pins), (version(1), 0));
    // The background work takes a guard for each turn it has.
    wait_for("no guard is left behind", || db.stats().guards == 0);
    assert_eq!(stats.directories, 1);
    assert!(stats.reopen.is_none() && db.reopen().is_none());

    let planted = plant(&db, subtree(1), Merk::sized(b'k', 40, 20));
    assert_eq!(planted.version, version(2));
    db.close().expect("the session ends");

    // The next session is at the same version, with the same keyspaces,
    // and takes the page size of the database.
    let again = Options {
        page_size: None,
        create_if_missing: false,
        ..options()
    };
    let db = Db::open(dir.path(), &again).expect("the database is opened again");
    assert!(!db.created());
    assert_eq!((db.version(), db.page_size()), (version(2), PageSize::MIN));
    assert_eq!(db.keyspaces(), keyspaces);
    let snapshot = db.snapshot();
    assert_eq!(
        seen(&snapshot.operation(), subtree(1), b"k"),
        Some((40, vec![]))
    );
    // A session that was closed left nothing to replay.
    assert_eq!(db.stats().recovery.commits, 0);
}

#[test]
fn a_database_that_was_not_closed_is_recovered_from_its_log() {
    let fs = SimVfs::new(SimConfig::new(7));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    let dir = Path::new("/db");
    // With no background work, so that no table checkpoint covers a
    // commit: write-back takes one when it is due, and the cleaner asks
    // for one.
    let db = open_at_rest(dir, &options).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 30, 400));
    plant(&db, subtree(2), Merk::sized(b'k', 30, 8));
    db.flush().expect("the log is synced");
    // The power goes with the session open: nothing was written back that
    // a checkpoint names.
    let crashed = fs.crash(1);
    drop(db);

    let options = Options {
        vfs: Arc::new(crashed),
        ..options
    };
    let db = Db::open(dir, &options).expect("the database is recovered");
    assert_eq!(db.version(), version(3));
    let recovery = db.stats().recovery;
    assert_eq!((recovery.landed, recovery.commits), (version(3), 3));
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(seen(&operation, subtree(1), b"k"), Some((30, vec![])));
    assert_eq!(seen(&operation, subtree(2), b"k"), Some((30, vec![])));
}

#[test]
fn a_directory_with_no_database_is_refused_where_none_is_to_be_made() {
    let dir = temp();
    let options = Options {
        create_if_missing: false,
        ..options()
    };
    // A directory that is not there, and one that holds what is not the
    // engine's.
    let missing = dir.path().join("none");
    let error = Db::open(&missing, &options).expect_err("no database");
    assert!(matches!(&error, OpenError::NoDatabase { dir } if *dir == missing));
    assert!(error.left_untouched() && error.source().is_none());
    assert!(error.to_string().ends_with("holds no database"));
    assert!(!missing.exists());

    fs::write(dir.path().join("notes.txt"), b"not the engine's").expect("a file");
    let before = image(dir.path());
    let error = Db::open(dir.path(), &options).expect_err("no database");
    assert!(matches!(error, OpenError::NoDatabase { .. }));
    assert_eq!(image(dir.path()), before);
}

#[test]
fn the_directories_above_a_new_database_are_made() {
    let dir = temp();
    let nested = dir.path().join("a").join("b").join("db");
    let db = Db::open(&nested, &options()).expect("a new database");
    assert!(db.created() && nested.join(ManifestStore::FILE_NAME).is_file());
}

#[test]
fn one_handle_has_a_database_open_at_a_time() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let error = Db::open(dir.path(), &options()).expect_err("it is open");
    assert!(matches!(&error, OpenError::Locked { dir: locked } if locked == dir.path()));
    assert!(!error.left_untouched());
    assert!(error.to_string().ends_with("is open in another handle"));
    // Dropping the handle releases the lock, and every file: the same
    // path is opened again in the same process (P-04), and the directory
    // can be removed (P-35).
    drop(db);
    let db = Db::open(dir.path(), &options()).expect("the lock was released");
    drop(db);
    let path = dir.path().to_path_buf();
    dir.close().expect("every file was released");
    assert!(!path.exists());
}

#[test]
fn a_database_keeps_its_page_size() {
    let dir = temp();
    drop(Db::open(dir.path(), &options()).expect("a new database"));
    let before = image(dir.path());
    let other = Options {
        page_size: Some(PageSize::DEFAULT),
        ..options()
    };
    let error = Db::open(dir.path(), &other).expect_err("another page size");
    let OpenError::PageSize {
        stored, requested, ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!((*stored, *requested), (PageSize::MIN, PageSize::DEFAULT));
    assert!(error
        .to_string()
        .contains("8192 bytes, and 32768 were asked for"));
    assert!(error.left_untouched());
    assert_eq!(image(dir.path()), before);

    // A new database with no size named has the default one.
    let fresh = temp();
    let unnamed = Options {
        page_size: None,
        ..options()
    };
    let db = Db::open(fresh.path(), &unnamed).expect("a new database");
    assert_eq!(db.page_size(), PageSize::DEFAULT);
}

/// What RocksDB leaves in the directory of a database, by name: the files
/// of a store that was opened once and written to.
fn rocksdb_directory(dir: &Path) {
    for (name, bytes) in [
        ("CURRENT", &b"MANIFEST-000005\n"[..]),
        ("IDENTITY", b"4f8c9c9e-2b1b-4d0e-9a53-1c3d6d1f6f10"),
        ("LOCK", b""),
        (
            "LOG",
            b"2026/11/19-18:00:00.000000 1 RocksDB version: 10.4.2\n",
        ),
        (
            "MANIFEST-000005",
            &[0x56, 0xf2, 0x0d, 0x88, 0x23, 0x00, 0x01, 0x01],
        ),
        ("OPTIONS-000007", b"[Version]\n  rocksdb_version=10.4.2\n"),
        ("000004.log", &[0u8; 64]),
        (
            "000008.sst",
            &[0x88, 0xe2, 0x41, 0xb7, 0x85, 0xf4, 0xcf, 0xf7],
        ),
    ] {
        fs::write(dir.join(name), bytes).expect("a file");
    }
}

/// P-02: a directory that another engine wrote, or a later version of this
/// one, is refused with an error that says which, and is byte for byte as
/// it was afterwards: with the database made if there is none, and
/// without.
#[test]
fn a_directory_of_another_engine_or_format_version_is_refused_and_left_as_it_was() {
    type Check = fn(&OpenError) -> bool;
    let rocksdb: Check = |error| {
        matches!(error, OpenError::OtherEngine { engine: "RocksDB", entry, .. }
            if entry == "000008.sst" || entry == "CURRENT")
    };
    let no_lignin_manifest: Check =
        |error| matches!(error, OpenError::OtherEngine { entry, .. } if entry == "MANIFEST");
    let later_version: Check = |error| matches!(error, OpenError::FormatVersion { found: 2, .. });
    let lost_manifest: Check =
        |error| matches!(error, OpenError::NoManifest { entry, .. } if entry == "seg");

    // What the directory is, what makes it, the error of its open, and
    // what the error says.
    type Case = (&'static str, fn(&Path), Check, &'static str);
    let cases: [Case; 5] = [
        (
            "RocksDB",
            rocksdb_directory,
            rocksdb,
            "another storage engine (RocksDB",
        ),
        (
            "one sst file",
            |dir| fs::write(dir.join("000012.sst"), b"sst").expect("a file"),
            |error| {
                matches!(
                    error,
                    OpenError::OtherEngine {
                        engine: "RocksDB",
                        ..
                    }
                )
            },
            "another storage engine",
        ),
        (
            "a MANIFEST of something else",
            |dir| fs::write(dir.join("MANIFEST"), [0x42; 96]).expect("a file"),
            no_lignin_manifest,
            "its MANIFEST is no LigninDB file",
        ),
        (
            "format version 2",
            |dir| {
                // A database of this build, whose manifest says that a
                // later version wrote it.
                drop(Db::open(dir, &options()).expect("a new database"));
                let path = dir.join("MANIFEST");
                let mut bytes = fs::read(&path).expect("the manifest");
                let header = FileHeader::decode(&bytes).expect("a header");
                assert_eq!(header.kind(), FileKind::Manifest);
                // The header: magic, kind, version at 10, sequence, and
                // its checksum at 16 (`format.md`).
                const VERSION_AT: usize = 10;
                const CHECKSUM_AT: usize = 16;
                bytes[VERSION_AT..VERSION_AT + 2].copy_from_slice(&2u16.to_le_bytes());
                let sum = lignin_format::Checksum::of(&[&bytes[..CHECKSUM_AT]]);
                bytes[CHECKSUM_AT..FileHeader::ENCODED_LEN].copy_from_slice(&sum.encode());
                fs::write(&path, bytes).expect("the manifest is written");
            },
            later_version,
            "format version 2, which this build does not read",
        ),
        (
            "segments and no manifest",
            |dir| {
                drop(Db::open(dir, &options()).expect("a new database"));
                fs::remove_file(dir.join("MANIFEST")).expect("the manifest is removed");
            },
            lost_manifest,
            "has files of a database (seg) and no manifest",
        ),
    ];
    for (name, make, is_expected, text) in cases {
        let dir = temp();
        make(dir.path());
        // What a node keeps beside its database.
        fs::create_dir(dir.path().join("checkpoints")).expect("a directory");
        fs::write(dir.path().join("checkpoints/7"), b"a checkpoint").expect("a file");
        let before = image(dir.path());
        for create_if_missing in [true, false] {
            let options = Options {
                create_if_missing,
                ..options()
            };
            let error = Db::open(dir.path(), &options).expect_err(name);
            assert!(is_expected(&error), "{name}: {error:?}");
            assert!(error.left_untouched(), "{name}");
            assert!(error.to_string().contains(text), "{name}: {error}");
            assert!(error.source().is_none(), "{name}");
            assert_eq!(image(dir.path()), before, "{name}: the directory changed");
        }
    }
}

/// A manifest that is damaged is no other engine's and no later
/// version's: the store says what is wrong with it.
#[test]
fn a_damaged_manifest_is_reported_by_the_store() {
    let dir = temp();
    drop(Db::open(dir.path(), &options()).expect("a new database"));
    let path = dir.path().join("MANIFEST");
    let mut bytes = fs::read(&path).expect("the manifest");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&path, bytes).expect("the manifest is written");
    let error = Db::open(dir.path(), &options()).expect_err("a damaged manifest");
    assert!(matches!(error, OpenError::Manifest(_)), "{error:?}");
    assert!(error.to_string().starts_with("manifest: ") && error.source().is_some());
    assert!(!error.left_untouched());
}

/// P-03: what a directory holds that is not the engine's is as it was
/// after an open that makes the database, commits, a flush, a wipe, the
/// reclaiming of what the wipe left, a close, a recovery and another
/// close.
#[test]
fn entries_that_are_not_the_engines_are_left_alone() {
    let dir = temp();
    let path = dir.path();
    fs::create_dir_all(path.join("checkpoints/41")).expect("a directory");
    fs::write(path.join("checkpoints/41/platform_state.bin"), [7; 300]).expect("a file");
    fs::write(path.join("last_checkpoint_attempt"), b"1795113003").expect("a file");
    fs::write(path.join("last_checkpoint_attempt.tmp"), b"").expect("a file");
    fs::write(path.join("state_sync_restore_in_progress"), b"").expect("a file");
    fs::write(path.join(".fsck"), b"").expect("a file");
    fs::write(path.join("wal.bak"), b"a name like one of the engine's").expect("a file");
    let names = [
        ".fsck",
        "checkpoints",
        "last_checkpoint_attempt",
        "last_checkpoint_attempt.tmp",
        "state_sync_restore_in_progress",
        "wal.bak",
    ];
    // Only these are read: a session that is open removes files of its
    // own while the directory is walked.
    let foreign = |dir: &Path| {
        let mut found = BTreeMap::new();
        for name in names {
            let at = dir.join(name);
            if at.is_dir() {
                found.insert(PathBuf::from(name), None);
                let below = image(&at).into_iter();
                found.extend(below.map(|(path, bytes)| (Path::new(name).join(path), bytes)));
            } else {
                found.insert(PathBuf::from(name), Some(fs::read(&at).expect("a file")));
            }
        }
        found
    };
    let before = foreign(path);
    assert_eq!(before.len(), 8);

    let db = Db::open(path, &options()).expect("a database is made beside them");
    assert!(db.created());
    assert_eq!(
        db.foreign_entries().expect("the directory is listed"),
        names
    );
    for name in 1..6 {
        plant(&db, subtree(name), Merk::sized(b'k', 200, 300));
    }
    db.flush().expect("the log is synced");
    assert!(matches!(db.wipe(), Ok(Committed::Published(_))));
    wait_for("the wipe is reclaimed", || {
        db.reclaim();
        db.stats().reclaimed.merks == 5
    });
    assert_eq!(foreign(path), before, "while the database is open");
    db.close().expect("the session ends");
    assert_eq!(foreign(path), before, "after a close");

    // A session that is not closed, so that the next one recovers: it
    // has no background work, and no table checkpoint covers its commit.
    let db = open_at_rest(path, &options()).expect("the database is opened again");
    plant(&db, subtree(9), Merk::sized(b'k', 50, 300));
    drop(db);
    let db = Db::open(path, &options()).expect("the database is recovered");
    assert!(db.stats().recovery.commits > 0);
    assert_eq!(
        db.foreign_entries().expect("the directory is listed"),
        names
    );
    db.close().expect("the session ends");
    assert_eq!(foreign(path), before, "after a recovery");
}

/// P-18: Platform removes the directory of a checkpoint while its handle
/// is open. The handle then closes without a panic, with the threads of
/// its background work running to the end, makes no directory, and the
/// thread that closes it, by `close` or by dropping it, makes no call
/// that changes a file or makes one durable. (What a turn of the
/// background work that was under way still writes, to files that are
/// gone, is on its own thread and is not counted here.)
#[test]
fn a_handle_whose_directory_was_removed_closes() {
    for explicit in [true, false] {
        let dir = temp();
        let path = dir.path().join("db");
        let watched = Watched::default();
        let options = Options {
            vfs: Arc::new(watched.clone()),
            ..options()
        };
        let db = Db::open(&path, &options).expect("a new database");
        plant(&db, subtree(1), Merk::sized(b'k', 100, 300));
        // The database writes while its directory is taken away, so the
        // removal is made until nothing is left.
        let began = Instant::now();
        while let Err(error) = fs::remove_dir_all(&path) {
            let gone = error.kind() == std::io::ErrorKind::NotFound;
            if gone && !path.exists() {
                break;
            }
            assert!(began.elapsed() < Duration::from_secs(30), "{error}");
        }
        assert!(!path.exists());
        let before = watched.changes_here();
        match explicit {
            true => {
                let error = db.close().expect_err("nothing could be made durable");
                assert!(matches!(&error, DbError::Removed { dir } if *dir == path));
                assert!(error.to_string().contains("removed or replaced"));
                assert!(error.source().is_none());
            }
            false => drop(db),
        }
        assert_eq!(watched.changes_here(), before, "the close wrote");
        assert!(!path.exists(), "the directory was made again");
    }
}

/// A database on `watched` with two Merks, a drop that leaves the close a
/// reclaiming to go on with, and pages that are not written back: its two
/// threads are ended, so that nothing of it runs but what the test calls.
/// What a close does then is `Db::finish`.
fn at_its_close(path: &Path, watched: &Watched) -> Db {
    let options = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let db = Db::open(path, &options).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 100, 300));
    plant(&db, subtree(2), Merk::sized(b'k', 100, 300));
    db.flush().expect("the log is synced");
    halt_background(&db);
    let writes = Writes {
        drop: vec![subtree(1)],
        ..Writes::default()
    };
    writes.commit(&db);
    db
}

/// The same, for what the close writes: nothing, through a path or
/// through a file it still has open, though it has a reclaiming to go on
/// with, pages to write back and a checkpoint to take. A directory that is
/// there is finished, to tell the two apart.
#[test]
fn a_close_writes_nothing_once_its_directory_was_removed() {
    let dir = temp();
    for removed in [false, true] {
        let path = dir.path().join(format!("db-{removed}"));
        let watched = Watched::default();
        let db = at_its_close(&path, &watched);
        if removed {
            fs::remove_dir_all(&path).expect("the directory is removed");
        }
        let before = watched.changes();
        let finished = db.finish();
        match removed {
            false => {
                finished.expect("the close finishes");
                assert!(watched.changes() > before, "the close wrote nothing");
            }
            true => {
                let error = finished.expect_err("nothing could be made durable");
                assert!(matches!(&error, DbError::Removed { dir } if *dir == path));
                assert_eq!(watched.changes(), before, "the close wrote");
                assert!(!path.exists(), "the directory was made again");
            }
        }
    }
}

/// The same for a directory that holds another database when the handle
/// closes: the path was removed and used again. Nothing of the new
/// database is touched.
#[test]
fn a_close_writes_nothing_once_its_directory_holds_another_database() {
    let dir = temp();
    let path = dir.path().join("db");
    let watched = Watched::default();
    let db = at_its_close(&path, &watched);
    fs::remove_dir_all(&path).expect("the directory is removed");

    let other = Db::open(&path, &options()).expect("another database in its place");
    plant(&other, subtree(7), Merk::sized(b'k', 10, 20));
    other.close().expect("its session ends");
    let (before, changes) = (image(&path), watched.changes());
    let error = db
        .finish()
        .expect_err("the directory is another database's");
    assert!(matches!(error, DbError::Removed { .. }));
    assert_eq!(watched.changes(), changes, "the close wrote");
    assert_eq!(image(&path), before, "the other database was touched");
    let other = Db::open(&path, &options()).expect("the other database opens");
    let snapshot = other.snapshot();
    assert_eq!(
        seen(&snapshot.operation(), subtree(7), b"k"),
        Some((10, vec![]))
    );
    assert_eq!(seen(&snapshot.operation(), subtree(2), b"k"), None);
}

/// A directory that holds this database's files and is another directory:
/// the one the handle has open was removed, and a copy of it is in its
/// place. The manifest in it is the session's own, and another opener
/// holds the lock of the copy, as one does between taking the lock and
/// its recovery. The close finds that the lock file there is not the file
/// its guard is on, and writes nothing.
///
/// The session makes no commit between the two. One that goes on writing
/// after its directory was replaced installs its manifests at the path,
/// which no look at the close can undo.
#[test]
fn a_close_writes_nothing_into_a_copy_of_its_directory() {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir(to).expect("a directory");
        for entry in fs::read_dir(from).expect("a directory") {
            let entry = entry.expect("an entry");
            let target = to.join(entry.file_name());
            match entry.path().is_dir() {
                true => copy(&entry.path(), &target),
                false => drop(fs::copy(entry.path(), &target).expect("a copy")),
            }
        }
    }
    for locked_by_another in [true, false] {
        let dir = temp();
        let (path, aside) = (dir.path().join("db"), dir.path().join("aside"));
        let watched = Watched::default();
        let db = at_its_close(&path, &watched);
        copy(&path, &aside);
        fs::remove_dir_all(&path).expect("the directory is removed");
        fs::rename(&aside, &path).expect("the copy takes its place");
        let other = locked_by_another.then(|| {
            let lock = StdVfs.lock_existing(&path.join("LOCK"));
            lock.expect("the lock of the copy is free")
        });
        // What the close goes by: the manifest at the path is the
        // session's own, so only the lock file tells the two apart.
        {
            let store = &*db.shared.store;
            let ours = crate::db::store::lock(&store.manifest);
            let stored = lignin_store::segment::read_manifest(&StdVfs, &path);
            assert_eq!(&stored.expect("a manifest"), ours.manifest().expect("one"));
        }

        let (before, changes) = (image(&path), watched.changes());
        let error = db
            .finish()
            .expect_err("the directory is not the one it opened");
        assert!(matches!(error, DbError::Removed { .. }));
        assert_eq!(watched.changes(), changes, "the close wrote");
        assert_eq!(image(&path), before, "the copy was touched");
        // The copy is the database as it was when it was copied: what its
        // log has, with the drop.
        drop(other);
        let copied = Db::open(&path, &options()).expect("the copy opens");
        let snapshot = copied.snapshot();
        assert_eq!(seen(&snapshot.operation(), subtree(1), b"k"), None);
        assert_eq!(
            seen(&snapshot.operation(), subtree(2), b"k"),
            Some((100, vec![]))
        );
    }
}

/// A database is opened, and between the open's look at the directory and
/// its lock someone puts another directory in its place. Whatever that
/// one is refused for, it is left byte for byte as it was put there: its
/// lock file with its content if it has one, and without one if it has
/// none. The lock of a database that is there is taken with no file made,
/// and an open that began on a database makes none, also where one is to
/// be made if there is none: an empty directory stays empty.
#[test]
fn a_directory_that_is_put_in_the_place_of_a_database_is_refused_and_left_as_it_is() {
    type Change = fn(&Path);
    type Check = fn(&OpenError) -> bool;
    let replace = |path: &Path| {
        fs::remove_dir_all(path).expect("the directory is removed");
        fs::create_dir(path).expect("another in its place");
    };
    let empty: Change = |_| {};
    let empty_with_a_lock_file: Change = |path| {
        fs::write(path.join("LOCK"), b"theirs").expect("a lock file with content");
    };
    let another_engine_with_its_lock: Change = |path| {
        rocksdb_directory(path);
        fs::write(path.join("LOCK"), b"theirs").expect("a lock file with content");
    };
    let another_engine_without_one: Change = |path| {
        rocksdb_directory(path);
        fs::remove_file(path.join("LOCK")).expect("no lock file");
    };
    let gone: Check = |error| matches!(error, OpenError::Replaced { .. });
    let other: Check = |error| matches!(error, OpenError::OtherEngine { .. });
    let cases = [
        (empty, gone),
        (empty_with_a_lock_file, gone),
        (another_engine_with_its_lock, other),
        (another_engine_without_one, other),
    ];
    for (case, (change, refused)) in cases.into_iter().enumerate() {
        for create_if_missing in [false, true] {
            let dir = temp();
            let path = dir.path().join("db");
            let db = Db::open(&path, &options()).expect("a new database");
            db.close().expect("the session ends");

            let watched = Watched::default();
            let reopening = Options {
                create_if_missing,
                vfs: Arc::new(watched.clone()),
                ..options()
            };
            let at = path.clone();
            let put = Arc::new(Mutex::new(None));
            let told = put.clone();
            watched.at_the_next_lock(move || {
                replace(&at);
                change(&at);
                *told.lock().expect("the image") = Some(image(&at));
            });
            let error = Db::open(&path, &reopening).expect_err("the directory was changed");
            let what = format!("case {case}, create {create_if_missing}: {error:?}");
            assert!(refused(&error) && error.left_untouched(), "{what}");
            assert!(error.source().is_none(), "{what}");
            let put = (put.lock().expect("the image").take()).expect("the hook ran");
            assert_eq!(image(&path), put, "{what}");
            assert_eq!(watched.changes(), 0, "{what}: the open wrote");
        }
    }
}

/// Two opens of a place for a new database. The first finds no lock file.
/// Before it looks again, the second makes the database, lock file first.
/// The first then finds a database with its lock file: held, if the
/// second handle is open, and that is what it reports, whatever the size
/// of that database's pages; free, if the second is closed, and then it
/// opens the database that the second made, or is refused for the size of
/// its pages. It does not take the database for one that has lost its
/// lock file.
#[test]
fn an_open_that_another_open_overtakes_is_locked_out_or_opens_its_database() {
    for (held, fits) in [(true, true), (true, false), (false, true), (false, false)] {
        let dir = temp();
        let path = dir.path().join("db");
        let watched = Watched::default();
        let first = Options {
            vfs: Arc::new(watched.clone()),
            ..options()
        };
        let overtaking = Options {
            page_size: Some(match fits {
                true => PageSize::MIN,
                false => PageSize::DEFAULT,
            }),
            ..options()
        };
        assert_eq!(first.page_size, Some(PageSize::MIN));
        let at = path.clone();
        let second = Arc::new(Mutex::new(None));
        let kept = second.clone();
        watched.after_the_next_lock_that_finds_no_file(move || {
            let db = Db::open(&at, &overtaking).expect("the second open makes the database");
            assert!(db.created());
            plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
            match held {
                true => *kept.lock().expect("the handle") = Some(db),
                false => db.close().expect("its session ends"),
            }
        });
        let opened = Db::open(&path, &first);
        let what = format!("held {held}, fits {fits}");
        match (held, fits) {
            (true, _) => {
                let error = opened.expect_err("the second handle is open");
                assert!(
                    matches!(&error, OpenError::Locked { dir } if *dir == path),
                    "{what}: {error:?}"
                );
                let other = second.lock().expect("the handle").take();
                other
                    .expect("the hook ran")
                    .close()
                    .expect("its session ends");
            }
            (false, true) => {
                let db = opened.expect("the database of the second open");
                assert!(!db.created());
                assert_eq!(
                    seen(&db.snapshot().operation(), subtree(1), b"k"),
                    Some((10, vec![]))
                );
            }
            (false, false) => {
                let error = opened.expect_err("pages of another size");
                assert!(
                    matches!(&error, OpenError::PageSize { .. }) && error.left_untouched(),
                    "{what}: {error:?}"
                );
            }
        }
    }
}

/// An open of a place for a new database is overtaken by another, which
/// makes the database and closes. The first open has seen that database
/// when someone puts an empty directory, with a lock file of its own, in
/// its place. The lock of that file is taken, and no database is made
/// under it: once an open has seen a database it makes none. The
/// directory is left byte for byte as it was put there.
#[test]
fn an_open_that_saw_a_database_makes_none_in_what_is_put_in_its_place() {
    let dir = temp();
    let path = dir.path().join("db");
    fs::create_dir(&path).expect("an empty directory");
    let watched = Watched::default();
    let first = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let (at, hooks) = (path.clone(), watched.clone());
    let put = Arc::new(Mutex::new(None));
    let told = put.clone();
    watched.after_the_next_lock_that_finds_no_file(move || {
        let db = Db::open(&at, &options()).expect("the second open makes the database");
        db.close().expect("its session ends");
        // The first open looks next, and sees the database. Then it asks
        // for the lock again.
        hooks.at_the_next_lock(move || {
            fs::remove_dir_all(&at).expect("the database is removed");
            fs::create_dir(&at).expect("another directory in its place");
            fs::write(at.join("LOCK"), b"theirs").expect("a lock file with content");
            *told.lock().expect("the image") = Some(image(&at));
        });
    });
    assert!(first.create_if_missing);
    let error = Db::open(&path, &first).expect_err("the database is gone");
    assert!(matches!(&error, OpenError::Replaced { dir } if *dir == path));
    assert!(error.left_untouched() && error.source().is_none());
    let put = (put.lock().expect("the image").take()).expect("both hooks ran");
    assert_eq!(image(&path), put);
    assert_eq!(watched.changes(), 0, "the open wrote");
}

/// An open has taken the lock of a database when someone moves that
/// database away and puts another database of this engine at the path,
/// with a lock file of its own. The open holds the lock of the first and
/// would read and write the second. It asks its guard under the lock, and
/// ends: the manifest of the second is not opened, a temporary file that
/// an open would remove stays, and both databases are as they were. This
/// holds for an open that began on the database, and for one that began
/// on a place and found the database of another open at its second look.
/// And it holds for a swap in the middle of the look under the lock, after
/// the directory was listed: the guard is asked after the look, so what
/// the look found is acted on only if it was the locked directory's.
#[test]
fn a_database_that_is_put_in_the_place_of_the_one_that_was_locked_is_not_opened() {
    for (overtaken, in_the_look) in [(false, false), (true, false), (false, true), (true, true)] {
        let dir = temp();
        let (path, aside, other) = (
            dir.path().join("db"),
            dir.path().join("aside"),
            dir.path().join("other"),
        );
        let theirs = Db::open(&other, &options()).expect("another database");
        plant(&theirs, subtree(2), Merk::sized(b'k', 30, 40));
        theirs.close().expect("its session ends");
        let temporary = other.join(ManifestStore::TEMP_FILE_NAME);
        fs::write(&temporary, b"of a manifest that was not installed").expect("a file");

        let watched = Watched::default();
        let opening = Options {
            vfs: Arc::new(watched.clone()),
            ..options()
        };
        let made = |at: &Path| {
            let db = Db::open(at, &options()).expect("the database that is opened");
            plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
            db.close().expect("its session ends");
        };
        let put = Arc::new(Mutex::new(None));
        let told = put.clone();
        let (at, to, from) = (path.clone(), aside.clone(), other.clone());
        let swap = move || {
            fs::rename(&at, &to).expect("the locked database is moved away");
            fs::rename(&from, &at).expect("another is put in its place");
            *told.lock().expect("the image") = Some(image(&at));
        };
        // Right after the lock is taken, or after the listing that the
        // look under it begins with.
        let hooks = watched.clone();
        let swap = move || match in_the_look {
            false => swap(),
            true => hooks.after_the_next_listing(swap),
        };
        match overtaken {
            false => {
                made(&path);
                watched.after_the_next_lock_that_is_taken(swap);
            }
            true => {
                fs::create_dir(&path).expect("an empty directory");
                let (at, hooks) = (path.clone(), watched.clone());
                watched.after_the_next_lock_that_finds_no_file(move || {
                    made(&at);
                    hooks.after_the_next_lock_that_is_taken(swap);
                });
            }
        }
        let error = Db::open(&path, &opening).expect_err("another database is at the path");
        let what = format!("overtaken {overtaken}, in the look {in_the_look}: {error:?}");
        assert!(
            matches!(&error, OpenError::Replaced { dir } if *dir == path),
            "{what}"
        );
        assert!(error.left_untouched(), "{what}");
        let put = (put.lock().expect("the image").take()).expect("the hook ran");
        assert!(put.contains_key(Path::new(ManifestStore::TEMP_FILE_NAME)));
        assert_eq!(image(&path), put, "{what}");
        assert_eq!(watched.changes(), 0, "{what}: the open wrote");

        // Each of the two is the database it was.
        for (at, subtree, len) in [(&aside, subtree(1), 10), (&path, subtree(2), 30)] {
            let db = Db::open(at, &options()).expect("the database opens");
            let snapshot = db.snapshot();
            assert_eq!(
                seen(&snapshot.operation(), subtree, b"k"),
                Some((len, vec![])),
                "{what}"
            );
        }
    }
}

/// An open has taken the lock of a database when someone removes the
/// lock file and puts another there. The directory is the same, and the
/// lock that the open holds is of a file with no name: another open can
/// take the one that is there now. The open ends with nothing written,
/// and says what it found.
#[test]
fn a_database_whose_lock_file_is_replaced_under_the_open_is_not_opened() {
    let dir = temp();
    let path = dir.path().join("db");
    let db = Db::open(&path, &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
    db.close().expect("the session ends");

    let watched = Watched::default();
    let opening = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let at = path.clone();
    let put = Arc::new(Mutex::new(None));
    let told = put.clone();
    watched.after_the_next_lock_that_is_taken(move || {
        fs::remove_file(at.join("LOCK")).expect("the lock file is removed");
        fs::write(at.join("LOCK"), b"").expect("another is put there");
        *told.lock().expect("the image") = Some(image(&at));
    });
    let error = Db::open(&path, &opening).expect_err("the lock file is another");
    assert!(matches!(&error, OpenError::Replaced { dir } if *dir == path));
    assert!(error.left_untouched());
    assert!(
        error.to_string().contains("the lock file that was taken"),
        "{error}"
    );
    let put = (put.lock().expect("the image").take()).expect("the hook ran");
    assert_eq!(image(&path), put);
    assert_eq!(watched.changes(), 0, "the open wrote");

    let db = Db::open(&path, &options()).expect("the database opens");
    assert_eq!(
        seen(&db.snapshot().operation(), subtree(1), b"k"),
        Some((10, vec![]))
    );
}

/// A database is to be made in an empty directory. The open has made the
/// lock file there and holds it when someone moves the directory away and
/// puts another empty one at the path. No database is made in that one:
/// it would have no lock file, and the lock that is held is of the other.
/// The open asked for a lock file to be made, so it reports the directory
/// as changed, and the one at the path stays empty.
#[test]
fn a_place_that_is_replaced_after_its_lock_file_was_made_gets_no_database() {
    let dir = temp();
    let (path, aside) = (dir.path().join("db"), dir.path().join("aside"));
    fs::create_dir(&path).expect("an empty directory");
    let watched = Watched::default();
    let options = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let (at, to) = (path.clone(), aside.clone());
    watched.after_the_next_lock_that_is_taken(move || {
        fs::rename(&at, &to).expect("the locked directory is moved away");
        fs::create_dir(&at).expect("another in its place");
    });
    let error = Db::open(&path, &options).expect_err("another directory is at the path");
    let OpenError::Changed { dir: named, found } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(named, &path);
    assert!(found.contains("the lock file that was taken"), "{found}");
    assert!(!error.left_untouched());
    assert!(
        image(&path).is_empty(),
        "the directory at the path gained a file"
    );
    let moved = image(&aside);
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(moved.get(Path::new("LOCK")), Some(&Some(Vec::new())));
}

/// A directory with an entry named `MANIFEST.deleting`, which is what a
/// removal that was begun and cut short leaves of a database, is opened
/// by nobody, and no open makes a database in it, whatever it is told
/// about creating; the directory is left as it is. The same holds with a
/// manifest beside the mark, and with a file of another engine beside
/// it: the mark is what the open says. `checkpoint::delete` finishes the
/// removal, and the path is a place for a new database again.
#[test]
fn a_directory_with_the_mark_of_a_removal_is_not_opened_and_gets_no_database() {
    use crate::checkpoint::{delete, DELETING_FILE_NAME};

    for (beside_a_manifest, beside_rocksdb) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let dir = temp();
        let path = dir.path().join("db");
        let db = Db::open(&path, &options()).expect("a new database");
        plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
        db.close().expect("the session ends");
        let manifest = path.join(ManifestStore::FILE_NAME);
        match beside_a_manifest {
            true => fs::copy(&manifest, path.join(DELETING_FILE_NAME)).map(drop),
            false => fs::rename(&manifest, path.join(DELETING_FILE_NAME)),
        }
        .expect("the mark of a removal");
        if beside_rocksdb {
            fs::write(path.join("CURRENT"), b"MANIFEST-000001\n").expect("a file of RocksDB");
        }
        let before = image(&path);

        for create_if_missing in [true, false] {
            let watched = Watched::default();
            let opening = Options {
                create_if_missing,
                vfs: Arc::new(watched.clone()),
                ..options()
            };
            let error = Db::open(&path, &opening).expect_err("the mark of a removal");
            let what = format!(
                "beside a manifest {beside_a_manifest}, beside RocksDB {beside_rocksdb}, create \
                 {create_if_missing}"
            );
            assert!(
                matches!(&error, OpenError::Deleting { dir } if *dir == path),
                "{what}: {error:?}"
            );
            assert!(error.left_untouched() && error.source().is_none());
            assert!(error.to_string().contains("MANIFEST.deleting"), "{error}");
            assert_eq!(watched.changes(), 0, "{what}: the open wrote");
            assert_eq!(image(&path), before, "{what}");
        }

        delete(&StdVfs, &path).expect("the removal is finished");
        assert!(!path.exists());
        let db = Db::open(&path, &options()).expect("a new database in its place");
        assert!(db.created());
        assert_eq!(seen(&db.snapshot().operation(), subtree(1), b"k"), None);
    }
}

/// The mark of a removal that comes between an open's first look and its
/// lock is found by the look under the lock. A database that was there at
/// the first look is not opened, and the open has written nothing. In a
/// directory with no lock file, where the open makes one before it looks
/// again, no database is made, and the open says that the directory was
/// changed under it.
#[test]
fn the_mark_of_a_removal_that_comes_before_the_lock_is_found_under_it() {
    use crate::checkpoint::DELETING_FILE_NAME;

    let dir = temp();
    let path = dir.path().join("db");
    let db = Db::open(&path, &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
    db.close().expect("the session ends");
    let watched = Watched::default();
    let (manifest, mark) = (
        path.join(ManifestStore::FILE_NAME),
        path.join(DELETING_FILE_NAME),
    );
    watched.at_the_next_lock(move || fs::rename(&manifest, &mark).expect("the mark"));
    let opening = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let error = Db::open(&path, &opening).expect_err("the mark of a removal");
    assert!(
        matches!(&error, OpenError::Deleting { dir } if *dir == path),
        "{error:?}"
    );
    assert!(error.left_untouched());
    assert_eq!(watched.changes(), 0, "the open wrote");
    assert!(path.join(DELETING_FILE_NAME).exists() && path.join("seg").exists());

    let place = dir.path().join("place");
    fs::create_dir(&place).expect("a directory");
    let watched = Watched::default();
    let mark = place.join(DELETING_FILE_NAME);
    watched.at_the_next_lock_that_makes(move || fs::write(&mark, b"").expect("the mark"));
    let opening = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let error = Db::open(&place, &opening).expect_err("the mark of a removal");
    assert!(
        matches!(&error, OpenError::Changed { dir, found }
            if *dir == place && found.contains("MANIFEST.deleting")),
        "{error:?}"
    );
    assert!(!error.left_untouched());
    let left = image(&place);
    assert!(
        !left.contains_key(Path::new("MANIFEST")) && !left.contains_key(Path::new("seg")),
        "{left:?}"
    );
}

/// A database that was copied without its lock file is refused and left
/// as it is: the lock file is made in no directory that holds a database,
/// so that no directory which is put in the place of one, at any moment of
/// the open, is given a file. With the file put there it opens.
#[test]
fn a_database_without_its_lock_file_is_refused_and_no_lock_file_is_made() {
    let dir = temp();
    let path = dir.path().join("db");
    let db = Db::open(&path, &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
    db.close().expect("the session ends");
    fs::remove_file(path.join("LOCK")).expect("the lock file is left out");
    let before = image(&path);

    for create_if_missing in [true, false] {
        let watched = Watched::default();
        let made = Arc::new(Mutex::new(false));
        let told = made.clone();
        watched.at_the_next_lock_that_makes(move || *told.lock().expect("the flag") = true);
        let reopening = Options {
            create_if_missing,
            vfs: Arc::new(watched.clone()),
            ..options()
        };
        let error = Db::open(&path, &reopening).expect_err("no lock file");
        assert!(matches!(&error, OpenError::NoLockFile { dir } if *dir == path));
        assert!(error.left_untouched() && error.source().is_none());
        assert!(error.to_string().contains("no LOCK file"), "{error}");
        assert!(
            !*made.lock().expect("the flag"),
            "a lock file was asked for"
        );
        assert_eq!(watched.changes(), 0, "the open wrote");
        assert_eq!(image(&path), before);
    }

    fs::write(path.join("LOCK"), b"").expect("the lock file is put there");
    let db = Db::open(&path, &options()).expect("the database opens");
    assert_eq!(
        seen(&db.snapshot().operation(), subtree(1), b"k"),
        Some((10, vec![]))
    );
}

/// The one way an open can leave a file in a directory it refuses
/// (D-129): the directory has nothing of any engine in it and no lock
/// file, it was looked at again and seen fit for a new database, and
/// another process writes another engine's files into it before the lock
/// file is made. The open refuses it as changed, and an empty `LOCK` is
/// all it added: nothing else in the directory is altered.
#[test]
fn a_directory_that_is_changed_while_its_lock_file_is_made_is_refused_as_changed() {
    let dir = temp();
    let path = dir.path().join("db");
    fs::create_dir(&path).expect("an empty directory");
    let watched = Watched::default();
    let options = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let at = path.clone();
    let put = Arc::new(Mutex::new(None));
    let told = put.clone();
    watched.at_the_next_lock_that_makes(move || {
        rocksdb_directory(&at);
        fs::remove_file(at.join("LOCK")).expect("no lock file");
        *told.lock().expect("the image") = Some(image(&at));
    });
    let error = Db::open(&path, &options).expect_err("the directory was changed");
    let OpenError::Changed { dir: named, found } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(named, &path);
    assert!(found.contains("RocksDB"), "{found}");
    assert!(!error.left_untouched() && error.source().is_none());
    let text = error.to_string();
    assert!(text.contains("a lock file may have been made") && text.contains("RocksDB"));
    let mut expected = (put.lock().expect("the image").take()).expect("the hook ran");
    expected.insert(PathBuf::from("LOCK"), Some(Vec::new()));
    assert_eq!(image(&path), expected);
}

/// A path with no root is made below the directory the process is in,
/// and the name of every directory that is made is synced in the one
/// above it: also the first, whose directory above has no name in the
/// path.
#[test]
fn a_database_at_a_relative_path_has_its_name_made_durable() {
    let dir = temp();
    for (path, synced) in [
        ("db", vec!["."]),
        ("above/below/db", vec![".", "above", "above/below"]),
    ] {
        let watched = Watched::rooted(dir.path());
        let options = Options {
            vfs: Arc::new(watched.clone()),
            ..options()
        };
        let db = Db::open(path, &options).expect("a new database");
        assert!(db.created() && dir.path().join(path).join("MANIFEST").exists());
        let dirs = watched.synced_dirs();
        for above in &synced {
            assert!(
                dirs.iter().any(|dir| dir == Path::new(above)),
                "{path}: {above} is not synced in {dirs:?}"
            );
        }
        plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
        db.close().expect("the session ends");
        let db = Db::open(path, &options).expect("the database is opened again");
        assert!(!db.created());
    }
}

/// The errors of an open that the file layer makes are told with the
/// path, and a write on a session that takes none says to open again.
#[test]
fn the_errors_say_what_failed() {
    let fs = SimVfs::new(SimConfig::new(3));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    // The directory cannot be listed, cannot be made, and cannot be
    // locked.
    use lignin_io::sim::{Fault, FaultRule, OpKind};
    fs.create_dir(Path::new("/db")).expect("a directory");
    for (kind, dir, action) in [
        (OpKind::ReadDir, "/db", "list"),
        (OpKind::CreateDir, "/new", "create"),
        (OpKind::Lock, "/db", "lock"),
    ] {
        fs.add_rule(FaultRule::new(Fault::Eio).on(kind));
        let error = Db::open(dir, &options).expect_err("the file layer fails");
        fs.clear_rules();
        assert!(
            matches!(&error, OpenError::Io { action: found, .. } if *found == action),
            "{error:?}"
        );
        let text = error.to_string();
        assert!(
            text.starts_with(&format!("cannot {action} {dir}")),
            "{text}"
        );
        assert!(error.source().is_some() && !error.left_untouched());
    }

    let error = WriteError::Commit(crate::commit::CommitError::Conflict);
    assert!(error.is_conflict() && error.reopen().is_none() && error.published().is_none());
    assert!(!error.leaves_transaction_open() && error.source().is_some());
    assert!(error
        .to_string()
        .contains("a commit outside the transaction"));
}

/// The power is cut at every point of the creation of a database at which
/// a cut can leave something else than the point before. In each of the
/// states of the cut that are tried, the next open takes the directory:
/// it opens the database that is there, or makes one. In particular no
/// cut leaves a manifest without the lock file, which every open would
/// refuse (`NoLockFile`): the lock file's name is synced before the
/// manifest is installed.
///
/// The states that are tried: nothing kept that was not synced, all of
/// it, all of it up to each operation, each change of a name alone with
/// what it builds on, and four states drawn from the seed. With the
/// changes of names alone, each later name is tried as the one that
/// survives without any earlier name it does not build on. Several names
/// surviving together, and the contents of files, are not gone through.
///
/// The creation begins in no directory, and in one that an open which
/// failed left with a lock file whose name is not synced: the name is
/// made durable for that one too.
#[test]
fn a_power_cut_at_any_point_of_a_creation_leaves_a_directory_that_opens() {
    use lignin_io::sim::{CrashPlan, CrashPoint};

    for left_over in [false, true] {
        let (mut cuts, mut states, mut made_anew) = (0, 0, 0);
        for cut in 1.. {
            let fs = SimVfs::new(SimConfig::new(77));
            let creating = Options {
                vfs: Arc::new(fs.clone()),
                ..options()
            };
            if left_over {
                fs.create_dir(Path::new("/db")).expect("a directory");
                fs.sync_dir(Path::new("/")).expect("its name is synced");
                drop(fs.lock(Path::new("/db/LOCK")).expect("a lock file"));
            }
            fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + cut));
            let created = Db::open("/db", &creating);
            if !fs.is_powered_off() {
                // The whole creation was made before the cut.
                assert!(created.expect("a new database").created());
                break;
            }
            drop(created);
            cuts += 1;

            let image = fs.crash_image();
            let mut plans = vec![image.lose_all(), image.keep_all()];
            plans.extend((0..=image.ops()).map(|op| image.keep_through(op)));
            plans.extend((0..image.names().len()).map(|name| CrashPlan {
                names: vec![name],
                changes: image.keep_all().changes,
            }));
            plans.extend((0..4).map(|draw| image.sample(draw)));
            plans.dedup();
            for (index, plan) in plans.iter().enumerate() {
                let context = format!(
                    "left over {left_over}, a cut after change {cut}, crash plan {index} ({plan:?})"
                );
                let left = image
                    .recover(plan)
                    .unwrap_or_else(|error| panic!("{context}: no such state: {error:?}"));
                states += 1;
                let options = Options {
                    vfs: Arc::new(left),
                    ..options()
                };
                let db = Db::open("/db", &options)
                    .unwrap_or_else(|error| panic!("{context}: the next open: {error}"));
                made_anew += u32::from(db.created());
                plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
                db.close()
                    .unwrap_or_else(|error| panic!("{context}: the close: {error}"));
            }
        }
        println!(
            "left over {left_over}: {cuts} cuts, {states} states, {made_anew} of them with a \
             database made anew"
        );
        assert!(cuts > 10, "a creation of {cuts} changes");
        assert!(made_anew > 0 && made_anew < states);
    }
}

/// An open that the file layer fails, at each of its file operations in
/// turn, reports a typed error and makes no handle; the open after it,
/// with the file layer whole, has the database: a new one that is made,
/// and one with commits that is recovered.
#[test]
fn an_open_that_fails_at_any_file_operation_is_made_again() {
    use lignin_io::sim::{Fault, FaultRule};
    for existing in [false, true] {
        let mut failed = 0;
        for skip in 0.. {
            let fs = SimVfs::new(SimConfig::new(500 + skip));
            let options = Options {
                vfs: Arc::new(fs.clone()),
                ..options()
            };
            if existing {
                let db = Db::open("/db", &options).expect("a new database");
                plant(&db, subtree(1), Merk::sized(b'k', 60, 400));
                db.flush().expect("the log is synced");
                // The session ends without a close, so the next one has a
                // log to replay.
                halt_background(&db);
                drop(db);
            }
            fs.add_rule(FaultRule::new(Fault::Eio).skip(skip));
            let opened = Db::open("/db", &options);
            fs.clear_rules();
            let context = format!("existing {existing}, operation {skip}");
            match opened {
                // Every operation of the open was let through.
                Ok(db) => {
                    drop(db);
                    break;
                }
                Err(error) => {
                    failed += 1;
                    assert!(!error.to_string().is_empty(), "{context}");
                    assert!(!error.left_untouched(), "{context}: {error}");
                    if let OpenError::Recovery(source) = &error {
                        assert!(!source.is_damage(), "{context}: {error}");
                    }
                }
            }
            let db = Db::open("/db", &options)
                .unwrap_or_else(|error| panic!("{context}: the next open: {error}"));
            let planted = existing.then_some((60, vec![]));
            let snapshot = db.snapshot();
            assert_eq!(
                seen(&snapshot.operation(), subtree(1), b"k"),
                planted,
                "{context}"
            );
            drop(snapshot);
            plant(&db, subtree(2), Merk::sized(b'k', 10, 20));
            db.close()
                .unwrap_or_else(|error| panic!("{context}: the close: {error}"));
        }
        assert!(failed > 20, "{failed} operations of an open failed");
    }
}

/// Every error of the facade says what failed, and names its cause.
#[test]
fn every_error_has_its_text_and_its_source() {
    use crate::db::DbError;
    let io = || std::io::Error::other("the cause");
    let errors = [
        (
            OpenError::Create {
                step: "the log",
                source: Box::new(io()),
            },
            "the first commit of the database (the log): the cause",
            true,
        ),
        (
            OpenError::Catalog {
                what: "it has no layout",
                source: None,
            },
            "the record of the keyspaces: it has no layout",
            false,
        ),
        (
            OpenError::catalog("a keyspace has no root page")(io()),
            "the record of the keyspaces: a keyspace has no root page: the cause",
            true,
        ),
        (
            OpenError::Thread(io()),
            "cannot start the background work: the cause",
            true,
        ),
    ];
    for (error, text, caused) in errors {
        assert_eq!(error.to_string(), text);
        assert_eq!(error.source().is_some(), caused, "{text}");
        assert!(!error.left_untouched() && format!("{error:?}").len() > 8);
    }
    let errors = [(
        DbError::Panicked("reclaim"),
        "the reclaim thread panicked",
        false,
    )];
    for (error, text, caused) in errors {
        assert!(error.to_string().contains(text), "{error}");
        assert_eq!(error.source().is_some(), caused, "{text}");
    }
}

/// The engine refuses any path whose last component ends in `.creating`,
/// ignoring ASCII case, before it calls the file layer, with either value
/// of `create_if_missing`. Here it is refused with a database at the
/// path, with an empty directory and with nothing there, with no call
/// that changes anything. The same database under a name that does not
/// end so is opened.
#[test]
fn a_path_with_the_suffix_reserved_for_checkpoint_creation_is_not_opened_and_gets_no_database() {
    let dir = temp();
    for (case, name) in ["7.creating", "7.CREATING", "7.Creating", ".creating"]
        .into_iter()
        .enumerate()
    {
        for held in ["a database", "an empty directory", "nothing"] {
            let above = dir.path().join(format!("{case}-{}", held.len()));
            fs::create_dir(&above).expect("a directory");
            let (path, plain) = (above.join(name), above.join("7"));
            match held {
                "a database" => {
                    let db = Db::open(&plain, &options()).expect("a new database");
                    plant(&db, subtree(1), Merk::sized(b'k', 10, 20));
                    db.close().expect("the session ends");
                    fs::rename(&plain, &path).expect("the database gets the name");
                }
                "an empty directory" => fs::create_dir(&path).expect("a directory"),
                _ => {}
            }
            let before = image(&above);
            for create_if_missing in [true, false] {
                let watched = Watched::default();
                let opening = Options {
                    create_if_missing,
                    vfs: Arc::new(watched.clone()),
                    ..options()
                };
                let what = format!("{name} with {held}, create {create_if_missing}");
                let error = Db::open(&path, &opening).expect_err("the reserved suffix");
                assert!(
                    matches!(&error, OpenError::Creating { dir } if *dir == path),
                    "{what}: {error:?}"
                );
                assert!(error.left_untouched() && error.source().is_none());
                assert!(error.to_string().contains(".creating"), "{error}");
                assert_eq!(watched.changes(), 0, "{what}: the open wrote");
                assert_eq!(image(&above), before, "{what}");
            }
            if held == "a database" {
                fs::rename(&path, &plain).expect("the database gets its name back");
                let db = Db::open(&plain, &options()).expect("the database opens");
                assert!(!db.created());
                assert!(seen(&db.snapshot().operation(), subtree(1), b"k").is_some());
            }
        }
    }
    // A name that only holds the letters, and one above the last name,
    // are no such name.
    for name in ["creating", "7.creating.old", "7.creating/db"] {
        let path = dir.path().join("plain").join(name);
        let db = Db::open(&path, &options()).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(db.created(), "{name}");
    }
}

/// A store subdirectory (`seg`, `wal`, `val` or `table`) that the database
/// directory lists as a symbolic link is refused at the open and not
/// followed, so the directory the link leads to is as it was, with a file
/// in it that bears a name of the store's and that no manifest names. A
/// store that opened through the link would list that directory and remove
/// such a file.
#[test]
fn a_store_subdirectory_that_is_a_link_is_refused_and_not_followed() {
    use lignin_store::segment::FileNames;

    for names in [
        FileNames::SEGMENT,
        FileNames::WAL,
        FileNames::VALUE_LOG,
        FileNames::PAGE_TABLE,
    ] {
        let place = temp();
        let dir = place.path().join("db");
        let db = Db::open(&dir, &options()).expect("a new database");
        db.close().expect("the session ends");

        // The subdirectory goes outside the database directory, where it
        // gains a file, and a link to it takes its place.
        let subdir = names.dir(&dir);
        let name = subdir.file_name().expect("a name").to_os_string();
        let outside = place.path().join("outside");
        fs::rename(&subdir, &outside).expect("the subdirectory moves");
        let theirs = names.file_name(lignin_format::FileId::MAX);
        fs::write(outside.join(theirs), b"x").expect("a file");
        std::os::unix::fs::symlink(&outside, &subdir).expect("a link");
        let held = image(&outside);

        let error = Db::open(&dir, &options()).expect_err("the link is not followed");
        assert!(
            matches!(&error, OpenError::StoreDirNotADirectory { entry, .. } if *entry == name),
            "{name:?}: {error:?}"
        );
        assert!(error.left_untouched(), "{name:?}");
        assert!(error.to_string().contains("is not followed"), "{error}");
        assert_eq!(image(&outside), held, "{name:?}: the directory was written");
    }
}

/// A named pipe in the place of a file that an open opens by its name is
/// refused before it is opened, so the open does not wait on it: the lock
/// file, the manifest, the two files of a reserve, a file the manifest
/// names in its store subdirectory, and the table checkpoint it names. A
/// link in such a place is not followed either. The directory is left as
/// it was.
#[test]
fn a_pipe_or_a_link_in_the_place_of_a_file_is_refused_and_not_opened() {
    use std::os::unix::fs::FileTypeExt;

    use lignin_store::segment::{read_manifest, FileNames};

    use crate::{
        backup::tests::world::{pipe, within},
        space::Reserve,
    };

    let place = temp();
    let dir = place.path().join("db");
    let db = Db::open(&dir, &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 8, 40));
    db.close().expect("the session ends");

    let manifest = read_manifest(&StdVfs, &dir).expect("the manifest");
    let named = manifest.files().first().expect("a named file");
    let names = match named.kind() {
        FileKind::Wal => FileNames::WAL,
        FileKind::Segment => FileNames::SEGMENT,
        FileKind::ValueLog => FileNames::VALUE_LOG,
        kind => panic!("{kind:?}"),
    };
    let table = (manifest.table_checkpoint()).expect("the table checkpoint of a close");
    let tables = FileNames::PAGE_TABLE;
    let own = |name: &str| (dir.clone(), dir.join(name));
    let cases = [
        own(ManifestStore::FILE_NAME),
        own("LOCK"),
        own(Reserve::FILE_NAME),
        own(Reserve::UNFINISHED_FILE_NAME),
        (names.dir(&dir), names.path(&dir, named.id())),
        (tables.dir(&dir), tables.path(&dir, table.file())),
    ];
    for (lists, entry) in &cases {
        for is_pipe in [true, false] {
            // What is there goes outside the directory, and a pipe, or a
            // link to what went, takes its place.
            let outside = place.path().join("outside");
            let was_there = fs::rename(entry, &outside).is_ok();
            if !was_there {
                fs::write(&outside, b"").expect("a file for the link");
            }
            let held = image(&dir);
            match is_pipe {
                true => pipe(entry),
                false => std::os::unix::fs::symlink(&outside, entry).expect("a link"),
            }
            let what = format!("{} (a pipe: {is_pipe})", entry.display());

            let at = dir.clone();
            let error = within(entry, move || Db::open(&at, &options()).map(drop))
                .unwrap_or_else(|| panic!("{what}: the open waited on it"))
                .expect_err("neither is opened");
            let name = entry.file_name().expect("a name");
            assert!(
                matches!(
                    &error,
                    OpenError::NotARegularFile { dir, entry } if dir == lists && entry == name
                ),
                "{what}: {error:?}"
            );
            assert!(error.left_untouched(), "{what}");
            assert!(error.to_string().contains("a named pipe is not opened"));

            // What took the place of the file is as it was put there, and
            // the directory holds nothing else that it did not hold.
            let kind = fs::symlink_metadata(entry).expect("an entry").file_type();
            assert_eq!((kind.is_fifo(), kind.is_symlink()), (is_pipe, !is_pipe));
            fs::remove_file(entry).expect("what took its place goes");
            assert_eq!(image(&dir), held, "{what}: the directory was written");
            match was_there {
                true => fs::rename(&outside, entry).expect("the file is back"),
                false => fs::remove_file(&outside).expect("the file goes"),
            }
        }
    }

    let db = Db::open(&dir, &options()).expect("the database opens as it was");
    db.close().expect("the session ends");
}
