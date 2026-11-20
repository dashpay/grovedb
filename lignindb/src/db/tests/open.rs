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
    let db = Db::open(dir, &options).expect("a new database");
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
    let foreign = |dir: &Path| {
        let mut image = image(dir);
        image.retain(|path, _| names.iter().any(|name| path.starts_with(name)));
        image
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

    // A session that is not closed, so that the next one recovers.
    let db = Db::open(path, &options()).expect("the database is opened again");
    plant(&db, subtree(9), Merk::sized(b'k', 50, 300));
    halt_background(&db);
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

/// The file layer of `watched` at rest: the background work has written
/// what it had to, and nothing changes any more.
fn at_rest(watched: &Watched) {
    let (mut seen, mut since) = (watched.changes(), Instant::now());
    wait_for("the background work comes to rest", || {
        let now = watched.changes();
        if now != seen {
            (seen, since) = (now, Instant::now());
        }
        since.elapsed() > Duration::from_millis(300)
    });
}

/// P-18: Platform removes the directory of a checkpoint while its handle
/// is open. The handle then closes without a panic, makes no directory
/// and no file, and writes nothing: not through a path, and not through a
/// file it still has open.
#[test]
fn a_handle_whose_directory_was_removed_closes_and_writes_nothing() {
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
        // A drop leaves the reclaimer work that a close would finish.
        let writes = Writes {
            drop: vec![subtree(1)],
            ..Writes::default()
        };
        writes.commit(&db);
        plant(&db, subtree(2), Merk::sized(b'k', 100, 300));
        db.flush().expect("the log is synced");
        at_rest(&watched);
        fs::remove_dir_all(&path).expect("the directory is removed");
        let before = watched.changes();
        match explicit {
            true => {
                let error = db.close().expect_err("nothing could be made durable");
                assert!(matches!(&error, DbError::Removed { dir } if *dir == path));
                assert!(error.to_string().contains("removed or replaced"));
                assert!(error.source().is_none());
            }
            false => drop(db),
        }
        assert_eq!(watched.changes(), before, "the close wrote");
        assert!(!path.exists(), "the directory was made again");
    }
}

/// The same for a directory that holds another database when the handle
/// closes: the path was removed and used again. Nothing of the new
/// database is touched.
#[test]
fn a_handle_whose_directory_holds_another_database_closes_and_writes_nothing() {
    let dir = temp();
    let path = dir.path().join("db");
    let watched = Watched::default();
    let first = Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    };
    let db = Db::open(&path, &first).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 100, 300));
    db.flush().expect("the log is synced");
    at_rest(&watched);
    fs::remove_dir_all(&path).expect("the directory is removed");

    let other = Db::open(&path, &options()).expect("another database in its place");
    plant(&other, subtree(7), Merk::sized(b'k', 10, 20));
    other.close().expect("its session ends");
    let (before, changes) = (image(&path), watched.changes());
    let error = db.close().expect_err("the directory is another database's");
    assert!(matches!(error, DbError::Removed { .. }));
    assert_eq!(watched.changes(), changes, "the close wrote");
    assert_eq!(image(&path), before, "the other database was touched");
    let other = Db::open(&path, &options()).expect("the other database opens");
    let snapshot = other.snapshot();
    assert_eq!(
        seen(&snapshot.operation(), subtree(7), b"k"),
        Some((10, vec![]))
    );
    assert_eq!(seen(&snapshot.operation(), subtree(1), b"k"), None);
}

/// A directory that holds this database's files and is another directory:
/// the one the handle has open was removed, and a copy of it, made while
/// the session was at rest, is in its place. The manifest in it is the
/// session's own, and another opener holds the lock of the copy, as one
/// does between taking the lock and its recovery. The close finds that
/// the lock file there is not the file its guard is on, and writes
/// nothing, though it has a checkpoint to take.
///
/// The session makes no commit between the two. One that goes on writing
/// after its directory was replaced installs its manifests at the path,
/// which no look at the close can undo.
#[test]
fn a_handle_whose_directory_is_a_copy_of_it_closes_and_writes_nothing() {
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
        let options = Options {
            vfs: Arc::new(watched.clone()),
            ..options()
        };
        let db = Db::open(&path, &options).expect("a new database");
        plant(&db, subtree(1), Merk::sized(b'k', 100, 300));
        plant(&db, subtree(2), Merk::sized(b'k', 100, 300));
        db.flush().expect("the log is synced");
        at_rest(&watched);
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
            .close()
            .expect_err("the directory is not the one it opened");
        assert!(matches!(error, DbError::Removed { .. }));
        assert_eq!(watched.changes(), changes, "the close wrote");
        assert_eq!(image(&path), before, "the copy was touched");
        // The copy is the database as it was when it was copied.
        drop(other);
        let copied = Db::open(&path, &options).expect("the copy opens");
        let snapshot = copied.snapshot();
        for name in [1, 2] {
            assert_eq!(
                seen(&snapshot.operation(), subtree(name), b"k"),
                Some((100, vec![]))
            );
        }
    }
}

/// A database is opened, and between the open's look at the directory and
/// its lock someone puts another directory in its place. Whatever that
/// one is refused for, it is left byte for byte as it was put there: its
/// lock file with its content if it has one, and without one if it has
/// none. The lock of a database that is there is taken with no file made.
#[test]
fn a_directory_that_is_put_in_the_place_of_a_database_is_refused_and_left_as_it_is() {
    type Change = fn(&Path);
    type Check = fn(&OpenError) -> bool;
    let replace = |path: &Path| {
        fs::remove_dir_all(path).expect("the directory is removed");
        fs::create_dir(path).expect("another in its place");
    };
    let empty: Change = |_| {};
    let another_engine_with_its_lock: Change = |path| {
        rocksdb_directory(path);
        fs::write(path.join("LOCK"), b"theirs").expect("a lock file with content");
    };
    let another_engine_without_one: Change = |path| {
        rocksdb_directory(path);
        fs::remove_file(path.join("LOCK")).expect("no lock file");
    };
    let none: Check = |error| matches!(error, OpenError::NoDatabase { .. });
    let other: Check = |error| matches!(error, OpenError::OtherEngine { .. });
    let cases = [
        (empty, none),
        (another_engine_with_its_lock, other),
        (another_engine_without_one, other),
    ];
    for (case, (change, refused)) in cases.into_iter().enumerate() {
        let dir = temp();
        let path = dir.path().join("db");
        let db = Db::open(&path, &options()).expect("a new database");
        db.close().expect("the session ends");

        let watched = Watched::default();
        let reopening = Options {
            create_if_missing: false,
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
        assert!(refused(&error), "case {case}: {error:?}");
        assert!(error.left_untouched(), "case {case}");
        let put = (put.lock().expect("the image").take()).expect("the hook ran");
        assert_eq!(image(&path), put, "case {case}");
        assert_eq!(watched.changes(), 0, "case {case}: the open wrote");
    }
}

/// The one way an open can leave a file in a directory it refuses: no
/// lock file was there, the directory was looked at again and seen fit for
/// a new database, and someone put another engine's directory in its
/// place before the lock file was made. The error says so, and that file
/// is all the open added.
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
    assert!(matches!(**found, OpenError::OtherEngine { .. }));
    assert!(!error.left_untouched());
    let text = error.to_string();
    assert!(text.contains("a lock file may have been made") && text.contains("RocksDB"));
    assert!(error.source().is_some());
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
