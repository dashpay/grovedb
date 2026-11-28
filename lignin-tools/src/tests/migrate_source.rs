//! `migrate` and `migrate --survey` refuse a source directory that holds an
//! entry of LigninDB's, alone or beside a RocksDB database, before anything
//! of it is opened, locked or written.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use rocksdb::{Options as RocksOptions, DB};

use super::{fixture, run, temp};
use crate::{Outcome, ToolError};

/// Every entry under `dir`, by its path: its kind, its bytes where it is a
/// file, and when it was last modified; and the same of `dir` itself.
fn tree(dir: &Path) -> Vec<(PathBuf, fs::FileType, Vec<u8>, SystemTime)> {
    let at = |path: &Path| {
        let metadata = fs::symlink_metadata(path).expect("its metadata");
        let bytes = match metadata.is_file() {
            true => fs::read(path).expect("its bytes"),
            false => Vec::new(),
        };
        let modified = metadata.modified().expect("a time");
        (path.to_path_buf(), metadata.file_type(), bytes, modified)
    };
    let mut found = vec![at(dir)];
    let mut pending: Vec<PathBuf> = (found[0].1.is_dir())
        .then(|| dir.to_path_buf())
        .into_iter()
        .collect();
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).expect("a listing") {
            let path = entry.expect("an entry").path();
            let entry = at(&path);
            if entry.1.is_dir() {
                pending.push(path);
            }
            found.push(entry);
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// A RocksDB database with the four column families of a GroveDB in `dir`,
/// as an open for writing makes it, closed.
fn rocksdb_in(dir: &Path) {
    let mut options = RocksOptions::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let families = [rocksdb::DEFAULT_COLUMN_FAMILY_NAME, "aux", "roots", "meta"];
    drop(DB::open_cf(&options, dir, families).expect("a RocksDB database"));
}

/// The two paths of the command that open a source.
#[derive(Clone, Copy, Debug)]
enum Via {
    Survey,
    Migration,
}

/// Runs `migrate SOURCE --survey` or `migrate SOURCE TARGET` on `source`
/// and returns its refusal. It leaves every entry of the source as it was,
/// its names, kinds, bytes and times, writes no report line, and makes no
/// target.
fn refused(source: &Path, via: Via) -> String {
    let before = tree(source);
    let around = temp();
    let target = around.path().join("target");
    let line = match via {
        Via::Survey => format!("migrate {} --survey", source.display()),
        Via::Migration => format!("migrate {} {}", source.display(), target.display()),
    };
    let (outcome, report) = run(&line);
    let refusal = match outcome {
        Err(ToolError::Migrate(words)) => words,
        other => panic!("{line}: {other:?}\n{report}"),
    };
    assert_eq!(report, "", "{line}");
    assert_eq!(tree(source), before, "{line}: the source changed");
    let made: Vec<_> = (fs::read_dir(around.path()).expect("a listing"))
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert!(made.is_empty(), "{line}: {made:?}");
    refusal
}

/// A LigninDB database is no source: the refusal names the directory and
/// the entry of LigninDB's that decided.
fn lignindb_alone_is_refused(via: Via) {
    let dir = temp();
    let source = fixture(dir.path(), 3);
    let expected = format!(
        "the source {}: it holds `MANIFEST`, an entry that only a LigninDB database makes, and \
         this command reads a RocksDB database alone: nothing was opened or locked. Find out \
         which database there holds the node's state",
        source.display()
    );
    assert_eq!(refused(&source, via), expected);
}

#[test]
fn the_survey_refuses_a_source_of_lignindb_alone() {
    lignindb_alone_is_refused(Via::Survey);
}

#[test]
fn the_migration_refuses_a_source_of_lignindb_alone() {
    lignindb_alone_is_refused(Via::Migration);
}

/// A RocksDB database that an open of RocksDB made beside a LigninDB
/// database, as an open with RocksDB as the build's default engine does,
/// is not read in place of the LigninDB one: the refusal is GroveDB's own
/// words for entries of both engines, which name the directory and an
/// entry of each. The entries are looked at before the lock file is: a
/// lock file that the probe would refuse is not reached.
fn both_engines_are_refused(via: Via) {
    let dir = temp();
    let source = fixture(dir.path(), 3);
    rocksdb_in(&source);
    assert!(source.join("CURRENT").is_file() && source.join("MANIFEST").is_file());
    let expected = format!(
        "the source {0}: the directory {0} holds entries of both engines (`CURRENT` is \
         RocksDB's and `MANIFEST` is LigninDB's): it is opened as neither",
        source.display()
    );
    assert_eq!(refused(&source, via), expected);
    fs::remove_file(source.join("LOCK")).expect("the lock file");
    fs::create_dir(source.join("LOCK")).expect("a directory named LOCK");
    assert_eq!(refused(&source, via), expected);
}

#[test]
fn the_survey_refuses_a_source_with_entries_of_both_engines() {
    both_engines_are_refused(Via::Survey);
}

#[test]
fn the_migration_refuses_a_source_with_entries_of_both_engines() {
    both_engines_are_refused(Via::Migration);
}

/// A source that is not listed is refused with the words of the look, and
/// a missing and an empty directory are refused as before. A RocksDB
/// database alone is taken.
#[test]
fn other_sources_are_answered_as_before() {
    let dir = temp();
    let file = dir.path().join("file");
    fs::write(&file, b"").expect("a file");
    for via in [Via::Survey, Via::Migration] {
        let refusal = refused(&file, via);
        assert!(
            refusal.starts_with(&format!(
                "the source {0}: the directory {0} could not be listed to see which engine's \
                 database it holds: ",
                file.display()
            )),
            "{refusal}"
        );
    }
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).expect("a directory");
    for source in [dir.path().join("none"), empty] {
        let survey = format!("migrate {} --survey", source.display());
        match run(&survey) {
            (Err(ToolError::Migrate(words)), _) => assert_eq!(
                words,
                format!(
                    "the source {}: it holds no RocksDB database (no file CURRENT)",
                    source.display()
                )
            ),
            other => panic!("{survey}: {other:?}"),
        }
    }
    let source = dir.path().join("rocksdb");
    rocksdb_in(&source);
    let survey = format!("migrate {} --survey", source.display());
    let (outcome, report) = run(&survey);
    assert!(
        matches!(outcome, Ok(Outcome::Done)),
        "{outcome:?}\n{report}"
    );
    assert!(
        report.starts_with(&format!(
            "the source {}: RocksDB identity ",
            source.display()
        )),
        "{report}"
    );
    assert!(report.ends_with("the source can be migrated\n"), "{report}");
}
