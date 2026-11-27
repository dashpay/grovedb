//! The look at a directory's entries that says which engine's database it
//! holds and opens nothing (`grovedb::engine::look_at_entries`), in each build
//! that compiles it: with RocksDB alone, with LigninDB alone and with both.

#![cfg(any(feature = "rocksdb_storage", feature = "lignin_storage"))]

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use grovedb::engine::{look_at_entries as look, EngineKind};

/// Every entry under `dir`, by its path: whether it is a directory, its
/// bytes where it is a file, and when it was last modified.
fn files(dir: &Path) -> Vec<(PathBuf, bool, Vec<u8>, SystemTime)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).expect("a listing") {
            let path = entry.expect("an entry").path();
            let metadata = fs::symlink_metadata(&path).expect("its metadata");
            let modified = metadata.modified().expect("a time");
            let bytes = match metadata.is_file() {
                true => fs::read(&path).expect("its bytes"),
                false => Vec::new(),
            };
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            found.push((path, metadata.is_dir(), bytes, modified));
        }
    }
    found.sort();
    found
}

fn said(dir: &Path) -> Option<(EngineKind, String)> {
    let found = look(dir).expect("a look");
    found.map(|(kind, entry)| (kind, entry.to_string_lossy().into_owned()))
}

/// What a directory with one entry of each name says: the names of each
/// engine say that engine, a directory under a name as a file does, and a
/// lock file or a log says nothing. So do a directory with no entry and no
/// directory at all.
#[test]
fn each_name_says_its_engine() {
    let dir = tempfile::tempdir().expect("a directory");
    assert_eq!(said(&dir.path().join("none")), None);
    assert_eq!(said(dir.path()), None);
    let rocksdb = [
        "CURRENT",
        "IDENTITY",
        "MANIFEST-000005",
        "OPTIONS-000007",
        "000009.sst",
    ];
    let lignin = [
        "MANIFEST",
        "MANIFEST.deleting",
        "wal",
        "seg",
        "val",
        "table",
    ];
    let neither = [
        "LOCK",
        "LOG",
        "LOG.old.1",
        "MANIFEST.tmp",
        "manifest",
        "current",
    ];
    let cases = (rocksdb
        .iter()
        .map(|name| (*name, Some(EngineKind::RocksDb))))
    .chain(lignin.iter().map(|name| (*name, Some(EngineKind::Lignin))))
    .chain(neither.iter().map(|name| (*name, None)));
    for (name, engine) in cases {
        for as_dir in [false, true] {
            let at = tempfile::tempdir().expect("a directory");
            match as_dir {
                true => fs::create_dir(at.path().join(name)).expect("a directory"),
                false => fs::write(at.path().join(name), b"").expect("a file"),
            }
            let before = files(at.path());
            let expected = engine.map(|engine| (engine, name.to_owned()));
            assert_eq!(said(at.path()), expected, "{name}");
            assert_eq!(files(at.path()), before, "{name}");
        }
    }
    assert_eq!(EngineKind::RocksDb.to_string(), "RocksDB");
    assert_eq!(EngineKind::Lignin.to_string(), "LigninDB");
    assert_eq!(EngineKind::RocksDb.other(), EngineKind::Lignin);
    assert_eq!(EngineKind::Lignin.other(), EngineKind::RocksDb);
}

/// The entry that is named is the first of its engine in the order of the
/// names as bytes.
#[test]
fn the_first_entry_in_the_order_of_the_names_is_named() {
    let dir = tempfile::tempdir().expect("a directory");
    for name in [
        "OPTIONS-000007",
        "LOCK",
        "IDENTITY",
        "CURRENT",
        "000009.sst",
    ] {
        fs::write(dir.path().join(name), b"").expect("a file");
    }
    let expected = Some((EngineKind::RocksDb, "000009.sst".to_owned()));
    assert_eq!(said(dir.path()), expected);
    let dir = tempfile::tempdir().expect("a directory");
    for name in ["wal", "seg", "MANIFEST.deleting", "LOCK"] {
        fs::create_dir(dir.path().join(name)).expect("a directory");
    }
    let expected = Some((EngineKind::Lignin, "MANIFEST.deleting".to_owned()));
    assert_eq!(said(dir.path()), expected);
}

/// A directory with entries of both engines is no answer: the look fails
/// with words that name the directory and an entry of each, and writes
/// nothing. So does a path it cannot list.
#[test]
fn entries_of_both_engines_and_a_path_that_is_not_listed_are_errors() {
    let dir = tempfile::tempdir().expect("a directory");
    for name in ["CURRENT", "LOCK", "MANIFEST", "MANIFEST-000005"] {
        fs::write(dir.path().join(name), b"").expect("a file");
    }
    let before = files(dir.path());
    let error = look(dir.path()).expect_err("entries of both engines");
    assert!(
        matches!(error, grovedb_storage::Error::NotSupported(_)),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "not supported: the directory {} holds entries of both engines (`CURRENT` is \
             RocksDB's and `MANIFEST` is LigninDB's): it is opened as neither",
            dir.path().display()
        )
    );
    assert_eq!(files(dir.path()), before);

    let file = dir.path().join("CURRENT");
    let error = look(&file).expect_err("a file is not listed");
    assert!(
        matches!(error, grovedb_storage::Error::StorageError(_)),
        "{error}"
    );
    let words = error.to_string();
    assert!(
        words.starts_with(&format!(
            "storage error: the directory {} could not be listed to see which engine's database \
             it holds: ",
            file.display()
        )),
        "{words}"
    );
}

/// The look says the engine of a database that the engine made: what an
/// open that creates a database leaves, and nothing of the other engine.
#[test]
fn a_database_says_the_engine_that_made_it() {
    use grovedb::Engine;
    let made = |make: fn(&Path)| {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("db");
        make(&path);
        let found = look(&path).expect("a look").map(|(kind, _)| kind);
        (dir, found)
    };
    #[cfg(feature = "rocksdb_storage")]
    {
        let (_dir, found) =
            made(|path| drop(grovedb::RocksDbEngine::open(path).expect("a database")));
        assert_eq!(found, Some(EngineKind::RocksDb));
    }
    #[cfg(feature = "lignin_storage")]
    {
        let (_dir, found) =
            made(|path| drop(grovedb::LigninEngine::open(path).expect("a database")));
        assert_eq!(found, Some(EngineKind::Lignin));
    }
}

/// The name a removal of LigninDB gives its manifest is LigninDB's own, in
/// a build that has LigninDB: the look in a build without it writes the
/// name out.
#[cfg(feature = "lignin_storage")]
#[test]
fn the_name_of_a_manifest_under_removal_is_lignindbs() {
    use std::ffi::OsString;
    let dir = tempfile::tempdir().expect("a directory");
    let name = lignindb::checkpoint::DELETING_FILE_NAME;
    fs::write(dir.path().join(name), b"").expect("a file");
    let found = look(dir.path()).expect("a look");
    assert_eq!(found, Some((EngineKind::Lignin, OsString::from(name))));
}
