//! The files an open database holds open, and what an open, a commit,
//! write-back and the creation of a checkpoint do when the process may
//! open no more files (OPS-05-F3; `docs/lignindb/operator/tuning.md`,
//! "Open files").
//!
//! A database that is open holds one descriptor for its lock file, one
//! for each segment, value-log file and log file that its manifest names,
//! and one more for each file it is writing. The count is read in
//! `/proc/self/fd`, on Linux. The limit is lowered in a child process,
//! never in this one: the child is this test program, run on one test of
//! its own by a shell that sets `ulimit -n`, or one that takes the files
//! the limit leaves with descriptors of `/dev/null`.

use std::{ffi::OsString, process::Command};

use lignin_format::{manifest::Manifest, FileHeader, FileKind};
use lignin_store::{cleaner::CleanerConfig, flush::Checkpointed, segment::read_manifest};

use super::*;
use crate::{
    checkpoint::{CheckpointOptions, BUILDING_DIR_NAME, CREATING_SUFFIX},
    db::{open::LOCK_FILE, Reopen},
};

/// The sessions [`stored`] writes, each with one segment and one
/// value-log file of its own.
const SESSIONS: u8 = 3;

/// What the system says of an open refused for the limit, on Linux and
/// on macOS.
const AT_THE_LIMIT: &str = "Too many open files";

/// The limit of open files of a child that takes the descriptors it may
/// still open: room for the program and its databases, and few enough to
/// take them all at once, whatever limit the tests run with.
const CHILD_LIMIT: u32 = 256;

/// The variable that says what a child does.
const CHILD_MODE: &str = "LIGNIN_OPEN_FILES_MODE";

/// The variable that names the database a child opens.
const CHILD_DIR: &str = "LIGNIN_OPEN_FILES_DIR";

/// The variable that says how many descriptors a child leaves free.
const CHILD_FREE: &str = "LIGNIN_OPEN_FILES_FREE";

/// What a child prints in front of each line that is for its parent.
const SAYS: &str = "open-files-child: ";

/// Options with a cleaner that moves nothing: the files of the store
/// stay as the writes left them.
fn still() -> Options {
    Options {
        cleaner: Some(CleanerConfig {
            target_dead_percent: 100,
            value_dead_percent: 100,
            value_ceiling_dead_percent: 100,
            ..CleanerConfig::new()
        }),
        ..options()
    }
}

/// The Merk that session `round` of [`stored`] plants, with values that
/// the value log holds.
fn merk() -> Merk {
    Merk::sized(b'k', 12, 1500)
}

/// A database in `dir` that [`SESSIONS`] sessions wrote and closed, each
/// a Merk of its own. A session appends to no file of an earlier one, so
/// each wrote a segment and a value-log file.
fn stored(dir: &Path) {
    for round in 1..=SESSIONS {
        let db = Db::open(dir, &still()).expect("a session");
        plant(&db, subtree(round), merk());
        db.close().expect("the session ends");
    }
}

/// Whether the database in `dir`, opened in this process, has every
/// Merk of `planted` and is sound.
fn holds(dir: &Path, planted: impl IntoIterator<Item = u8>) {
    let db = Db::open(dir, &still()).expect("the database opens");
    assert!(db.verify(None).is_sound(), "{}", dir.display());
    let snapshot = db.snapshot();
    for name in planted {
        let items = items_of(&snapshot.operation(), subtree(name));
        assert_eq!(items, Some(merk().items()), "subtree {name}");
    }
    drop(snapshot);
    db.close().expect("the session ends");
}

/// Whether the database in `dir`, opened in this process, has no subtree
/// `name`.
fn lacks(dir: &Path, name: u8) {
    let db = Db::open(dir, &still()).expect("the database opens");
    assert_eq!(
        items_of(&db.snapshot().operation(), subtree(name)),
        None,
        "subtree {name}"
    );
    db.close().expect("the session ends");
}

/// The files of the kinds the store holds open that `manifest` names,
/// by their paths in `dir`.
fn named(dir: &Path, manifest: &Manifest) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = (manifest.files().iter())
        .filter_map(|file| {
            let names = match file.kind() {
                FileKind::Wal => lignin_store::segment::FileNames::WAL,
                FileKind::Segment => lignin_store::segment::FileNames::SEGMENT,
                FileKind::ValueLog => lignin_store::segment::FileNames::VALUE_LOG,
                FileKind::Manifest | FileKind::PageTable => return None,
            };
            Some(names.path(dir, file.id()))
        })
        .collect();
    paths.sort();
    paths
}

#[cfg(target_os = "linux")]
fn count_of(manifest: &Manifest, kind: FileKind) -> usize {
    (manifest.files().iter())
        .filter(|file| file.kind() == kind)
        .count()
}

/// The files below `dir` that this process holds descriptors of, one
/// entry for each descriptor, by the path each was opened by.
#[cfg(target_os = "linux")]
fn held_in(dir: &Path) -> Vec<PathBuf> {
    let at = fs::canonicalize(dir).expect("the directory");
    let mut held: Vec<PathBuf> = fs::read_dir("/proc/self/fd")
        .expect("the descriptors of the process")
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
        .filter_map(|target| Some(dir.join(target.strip_prefix(&at).ok()?)))
        .collect();
    held.sort();
    held
}

/// An open database holds a descriptor for its lock file and for each
/// segment, value-log file and log file its manifest names, and nothing
/// else at rest: the manifest and the table checkpoint are read and
/// closed. A checkpoint that is opened beside it holds the same of its
/// own and adds nothing to the live database's. A file the session is
/// writing is held twice, to read and to append.
#[cfg(target_os = "linux")]
#[test]
fn an_open_database_holds_one_descriptor_for_each_file_its_manifest_names() {
    let base = temp();
    let dir = base.path().join("live");
    stored(&dir);
    let db = Db::open(&dir, &still()).expect("the database");
    halt_background(&db);
    let manifest = read_manifest(&StdVfs, &dir).expect("the manifest");
    assert_eq!(
        count_of(&manifest, FileKind::Segment),
        usize::from(SESSIONS)
    );
    assert_eq!(
        count_of(&manifest, FileKind::ValueLog),
        usize::from(SESSIONS)
    );
    assert!(count_of(&manifest, FileKind::Wal) >= 1);
    assert!(manifest.table_checkpoint().is_some());
    let mut expected = named(&dir, &manifest);
    expected.push(dir.join(LOCK_FILE));
    expected.sort();
    assert_eq!(held_in(&dir), expected);

    let target = base.path().join("checkpoint");
    db.checkpoint(&target, CheckpointOptions::default())
        .expect("a checkpoint");
    let checkpoint = Db::open(&target, &still()).expect("the checkpoint");
    halt_background(&checkpoint);
    let of_checkpoint = read_manifest(&StdVfs, &target).expect("its manifest");
    for kind in [FileKind::Segment, FileKind::ValueLog] {
        assert_eq!(count_of(&of_checkpoint, kind), count_of(&manifest, kind));
    }
    let mut its_own = named(&target, &of_checkpoint);
    its_own.push(target.join(LOCK_FILE));
    its_own.sort();
    assert_eq!(held_in(&target), its_own);
    assert_eq!(held_in(&dir), expected);
    drop(checkpoint);

    // A turn of write-back writes the pages of a commit into a new
    // segment and its values into a new value-log file: the session
    // writes both, and holds each to read and to append.
    plant(&db, subtree(SESSIONS + 1), merk());
    (db.shared.write_back().tick()).expect("a turn of write-back");
    let manifest = read_manifest(&StdVfs, &dir).expect("the manifest");
    let mut written: Vec<PathBuf> = named(&dir, &manifest)
        .into_iter()
        .filter(|path| !expected.contains(path) && !path.starts_with(dir.join("wal")))
        .collect();
    assert_eq!(written.len(), 2, "{written:?}");
    let mut now = named(&dir, &manifest);
    now.push(dir.join(LOCK_FILE));
    now.append(&mut written);
    now.sort();
    assert_eq!(held_in(&dir), now);
}

/// The lines a child printed for its parent. A child that printed any has
/// to have ended as a test that passed.
fn said(output: &std::process::Output) -> Vec<String> {
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(!err.contains("panicked"), "the child panicked: {err}");
    let said: Vec<String> = (out.lines())
        .filter_map(|line| line.split_once(SAYS).map(|(_, said)| said.to_string()))
        .collect();
    assert!(
        said.is_empty() || output.status.success(),
        "the child said {said:?} and ended with {}: {err}",
        output.status
    );
    said
}

/// What a child is given: the database it works on, the limit of open
/// files of its process, how many descriptors it leaves free where it
/// takes them, and what it does.
struct Child<'a> {
    test: &'a str,
    dir: &'a Path,
    limit: Option<u32>,
    free: usize,
    mode: &'a str,
}

impl Child<'_> {
    /// This test program, run on the one test of the child by a shell that
    /// limits the files it may open.
    fn run(&self) -> std::process::Output {
        let program = std::env::current_exe().expect("the test's program");
        let limit: OsString =
            (self.limit).map_or_else(OsString::new, |limit| limit.to_string().into());
        Command::new("sh")
            .args([
                "-c",
                "[ -z \"$1\" ] || ulimit -n \"$1\" || exit 99; shift; exec \"$@\"",
            ])
            .arg("sh")
            .arg(limit)
            .arg(program)
            .args(["--exact", self.test, "--nocapture", "--test-threads=1"])
            .env(CHILD_DIR, self.dir)
            .env(CHILD_FREE, self.free.to_string())
            .env(CHILD_MODE, self.mode)
            .output()
            .expect("the child runs")
    }

    /// What it said.
    fn said(&self) -> Vec<String> {
        said(&self.run())
    }
}

/// The child of [`an_open_at_the_limit_fails_and_changes_nothing_named`]
/// at `limit`, on the database in `dir`.
fn opening(dir: &Path, limit: Option<u32>) -> Vec<String> {
    let test = "db::tests::open_files::child_opens_at_the_limit";
    Child {
        test,
        dir,
        limit,
        free: 0,
        mode: "",
    }
    .said()
}

/// The line of `said` that begins with `prefix`, without it.
fn line(said: &[String], prefix: &str) -> String {
    let found = said.iter().find_map(|line| line.strip_prefix(prefix));
    found
        .unwrap_or_else(|| panic!("no {prefix:?} in {said:#?}"))
        .to_string()
}

/// Prints a line for the parent.
fn say(what: impl fmt::Display) {
    println!("\n{SAYS}{what}");
}

/// An answer for the parent: `Ok(())`, or the words of the error.
fn told<T, E: fmt::Display>(result: Result<T, E>) -> Result<(), String> {
    result.map(drop).map_err(|error| error.to_string())
}

/// The child of [`an_open_at_the_limit_fails_and_changes_nothing_named`]:
/// it opens the database its parent names and says how that went. Run on
/// its own it does nothing.
#[test]
fn child_opens_at_the_limit() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let opening = Options {
        create_if_missing: false,
        ..still()
    };
    match Db::open(&dir, &opening) {
        Ok(db) => {
            say(format_args!("opened {}", db.verify(None).is_sound()));
            // The session ends as a process that is killed: nothing of a
            // close runs, so what the parent finds is what the open left.
            halt_background(&db);
            drop(db);
        }
        Err(error) => say(format_args!("refused {error}")),
    }
}

/// Whether the file at `path` of a directory's image, by its path in it,
/// is a log file with its header alone, under the name of the id in it.
fn header_only_log(path: &Path, bytes: Option<&[u8]>) -> bool {
    let id = (path.strip_prefix("wal").ok())
        .and_then(|name| lignin_store::segment::FileNames::WAL.parse(name.as_os_str()));
    let bytes = bytes.unwrap_or_default();
    let header = FileHeader::decode(bytes).ok();
    bytes.len() == FileHeader::ENCODED_LEN
        && header.is_some_and(|header| {
            header.kind() == FileKind::Wal && Some(header.sequence()) == id.map(|id| id.get())
        })
}

/// What an open that was refused may have left in a directory whose
/// every entry was `before`: nothing that was there is changed or gone,
/// and at most one log file is new, with its header alone, which the
/// manifest, unchanged, does not name.
fn left_as_it_was(dir: &Path, before: &BTreeMap<PathBuf, Option<Vec<u8>>>) -> Result<(), String> {
    let after = image(dir);
    for (path, was) in before {
        if after.get(path) != Some(was) {
            return Err(format!("{} changed", path.display()));
        }
    }
    let new: Vec<&PathBuf> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    match new.as_slice() {
        [] => Ok(()),
        [log] if header_only_log(log, after[*log].as_deref()) => Ok(()),
        _ => Err(format!("new entries {new:?}")),
    }
}

/// The log files in `dir` that its manifest does not name.
fn unnamed_logs(dir: &Path) -> Vec<PathBuf> {
    let manifest = read_manifest(&StdVfs, dir).expect("the manifest");
    let named = named(dir, &manifest);
    (fs::read_dir(dir.join("wal")).expect("the log's directory"))
        .map(|entry| entry.expect("an entry").path())
        .filter(|log| !named.contains(log))
        .collect()
}

/// An open of a database, and of a checkpoint of it, in a process that
/// may open too few files fails with the error of the file it could not
/// open or list, which names it and the system's reason. The database was
/// closed, so the open has nothing to write back before it begins its
/// log. The manifest and every file it names are as they were; at the
/// one place where the open fails after it made the session's first log
/// file, that file is left, with its header alone, and the first open
/// that succeeds removes it. With the limit raised the same open
/// succeeds and the database verifies. The limits are tried one by one
/// from below the least that runs the program, so every descriptor the
/// open takes, up to the first open that succeeds, is the one that meets
/// the limit once.
#[test]
fn an_open_at_the_limit_fails_and_changes_nothing_named() {
    let base = temp();
    let dir = base.path().join("live");
    stored(&dir);
    let target = base.path().join("checkpoint");
    let db = Db::open(&dir, &still()).expect("the database");
    db.checkpoint(&target, CheckpointOptions::default())
        .expect("a checkpoint");
    db.close().expect("the session ends");

    for at in [&dir, &target] {
        let before = image(at);
        let mut refused = Vec::new();
        let mut opened = None;
        for limit in 3..256 {
            let said = opening(at, Some(limit));
            let Some(line) = said.first() else {
                // Too few for the program itself to start.
                assert!(refused.is_empty(), "the child said nothing at {limit}");
                continue;
            };
            if let Some(sound) = line.strip_prefix("opened ") {
                assert_eq!(sound, "true", "the open at {limit} does not verify");
                assert_eq!(
                    unnamed_logs(at),
                    Vec::<PathBuf>::new(),
                    "after the open at {limit}"
                );
                opened = Some(limit);
                break;
            }
            let error = line.strip_prefix("refused ").expect("an answer");
            assert!(error.contains(AT_THE_LIMIT), "at {limit}: {error}");
            assert!(
                error.contains(&*at.to_string_lossy()),
                "at {limit}: {error}"
            );
            if let Err(left) = left_as_it_was(at, &before) {
                panic!("at {limit}, refused with {error}: {left}");
            }
            refused.push(error.to_string());
        }
        assert!(opened.is_some(), "no limit opened {}", at.display());
        for kind in ["seg", "val", "wal"] {
            let in_kind = at.join(kind).to_string_lossy().into_owned();
            assert!(
                refused.iter().any(|error| error.contains(&in_kind)),
                "no open failed in {kind}: {refused:#?}"
            );
        }
        // The message a node met: the value log's open of one of its
        // files, by its name, and the system's reason.
        let manifest = read_manifest(&StdVfs, at).expect("the manifest");
        let value_files: Vec<PathBuf> = (named(at, &manifest).into_iter())
            .filter(|path| path.starts_with(at.join("val")))
            .collect();
        let met = |error: &String| {
            value_files.iter().any(|file| {
                let opened = format!(
                    "the value log was not recovered: cannot open {}: ",
                    file.display()
                );
                error.contains(&opened) && error.ends_with(&format!("{AT_THE_LIMIT} (os error 24)"))
            })
        };
        assert!(refused.iter().any(met), "{refused:#?}");
        // With no limit lowered the open succeeds too.
        assert_eq!(opening(at, None), ["opened true"]);
        holds(at, 1..=SESSIONS);
    }
}

/// Takes every descriptor the process may still open, with files of
/// `/dev/null`, and gives back `free` of them. The process is a child
/// with [`CHILD_LIMIT`].
fn take_descriptors(free: usize) -> Vec<fs::File> {
    let mut taken = Vec::new();
    loop {
        match fs::File::open("/dev/null") {
            Ok(file) => taken.push(file),
            Err(error) => {
                assert!(error.to_string().contains(AT_THE_LIMIT), "{error}");
                break;
            }
        }
        assert!(
            taken.len() <= CHILD_LIMIT as usize,
            "the limit is not {CHILD_LIMIT}"
        );
    }
    taken.truncate(taken.len().saturating_sub(free));
    taken
}

/// Commits `merk` into `subtree(name)`, entering it if `enter`, and says
/// whether it was published.
fn commit_of(db: &Db, name: u8, merk: Merk, enter: bool) -> Result<(), String> {
    let writes = Writes {
        enter: if enter {
            vec![(subtree(name), None)]
        } else {
            Vec::new()
        },
        merks: vec![(subtree(name), merk)],
        ..Writes::default()
    };
    match writes.with(|batch| db.write(batch, &mut NoStaleHashes)) {
        Ok(Committed::Published(_)) => Ok(()),
        other => Err(format!("{other:?}")),
    }
}

/// Commits the Merk of [`merk`] into the new subtree `subtree(name)`.
fn commit(db: &Db, name: u8) -> Result<(), String> {
    commit_of(db, name, merk(), true)
}

/// The child of [`a_checkpoint_at_the_limit_fails_and_the_session_goes_on`]:
/// with the session's threads ended, so that nothing but the calls below
/// opens a file, it takes the descriptors the process has but the number
/// its parent names, makes a checkpoint, commits, gives the descriptors
/// back and, where the checkpoint is not at its target, makes it again;
/// then takes them again and opens the checkpoint, commits, gives them
/// back and opens it. Run on its own it does nothing.
#[test]
fn child_makes_a_checkpoint_at_the_limit() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let free: usize = (std::env::var(CHILD_FREE).expect("the count"))
        .parse()
        .expect("a count");
    let target = dir.with_extension("checkpoint");
    let staging = dir.with_extension(format!("checkpoint{CREATING_SUFFIX}"));
    let db = Db::open(&dir, &still()).expect("the database");
    halt_background(&db);

    let taken = take_descriptors(free);
    let made = db.checkpoint(&target, CheckpointOptions::default());
    let published = commit(&db, SESSIONS + 1);
    drop(taken);
    let failed = made.is_err();
    match made {
        Ok(created) => say(format_args!("created, {} copied", created.copied)),
        Err(error) => say(format_args!("refused {error}")),
    }
    let left = match fs::read_dir(&staging) {
        Ok(entries) => {
            let mut left: Vec<String> = entries
                .map(|entry| {
                    entry
                        .expect("an entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            left.sort();
            format!("true {left:?}")
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => "false []".to_string(),
        Err(error) => panic!("the staging directory: {error}"),
    };
    say(format_args!("target {}", target.exists()));
    say(format_args!("staging {left}"));
    say(format_args!("committed {published:?}"));
    if failed && !target.exists() {
        let again = db.checkpoint(&target, CheckpointOptions::default());
        say(format_args!("again {:?}", told(again)));
    }
    say(format_args!("staging after {}", staging.exists()));

    let taken = take_descriptors(0);
    let opened = Db::open(&target, &still()).map(drop);
    let published = commit(&db, SESSIONS + 2);
    drop(taken);
    say(format_args!("checkpoint opened {:?}", told(opened)));
    say(format_args!("committed beside {published:?}"));
    let opened = Db::open(&target, &still()).map(|checkpoint| checkpoint.verify(None).is_sound());
    say(format_args!(
        "checkpoint reopened {:?}",
        opened.map_err(|error| error.to_string())
    ));
    db.flush().expect("the session's commits are durable");
}

/// The creation of a checkpoint in a process that may open no more files
/// fails with the error of the call that met the limit, which names a
/// path of the creation and the system's reason. A commit of the live
/// session goes on beside it and is published. Where the creation failed
/// before it gave the checkpoint its name, nothing is at the target, what
/// it left under the staging name is what a creation puts there, and once
/// the descriptors are back the next creation at the target takes that
/// over and succeeds; where it failed after, the whole checkpoint is at
/// the target. An open of the checkpoint at the limit fails as an open
/// does, the live session's commit beside it is published, and the open
/// succeeds once the descriptors are back. Each count of free descriptors
/// from none up to the first that lets the creation through is tried, and
/// with none it fails.
#[test]
fn a_checkpoint_at_the_limit_fails_and_the_session_goes_on() {
    let mut created_with = None;
    for free in 0..64 {
        let base = temp();
        let dir = base.path().join("live");
        stored(&dir);
        let test = "db::tests::open_files::child_makes_a_checkpoint_at_the_limit";
        let limit = Some(CHILD_LIMIT);
        let said = Child {
            test,
            dir: &dir,
            limit,
            free,
            mode: "",
        }
        .said();
        let line = |prefix| line(&said, prefix);
        let checkpoint = dir.with_extension("checkpoint");
        assert_eq!(line("committed "), "Ok(())", "{said:#?}");
        assert!(
            line("checkpoint opened ").contains(AT_THE_LIMIT),
            "{said:#?}"
        );
        assert_eq!(line("committed beside "), "Ok(())", "{said:#?}");
        assert_eq!(line("checkpoint reopened "), "Ok(true)", "{said:#?}");
        let first = said.first().expect("an answer");
        if let Some(copied) = first.strip_prefix("created, ") {
            assert!(free > 0, "a creation with no descriptor free succeeded");
            // The checkpoint made at the limit, of the version before the
            // commit beside it.
            assert_eq!(line("target "), "true", "{said:#?}");
            assert_eq!(line("staging after "), "false", "{said:#?}");
            holds(&checkpoint, 1..=SESSIONS);
            lacks(&checkpoint, SESSIONS + 1);
            created_with = Some((free, copied == "0 copied"));
            break;
        }
        let error = first.strip_prefix("refused ").expect("an answer");
        assert!(error.contains(AT_THE_LIMIT), "with {free} free: {error}");
        assert!(
            error.contains(&*base.path().to_string_lossy()),
            "with {free} free: {error}"
        );
        if free == 0 {
            // Its first look at the target.
            let looked = format!("could not look at {}: ", checkpoint.display());
            assert!(error.starts_with(&looked), "with none free: {error}");
            assert_eq!(line("staging "), "false []", "{said:#?}");
        }
        match line("target ").as_str() {
            // It failed after the rename: the checkpoint is whole there,
            // of the version before the commit beside it, and the staging
            // directory can be left where its removal failed too.
            "true" => {
                holds(&checkpoint, 1..=SESSIONS);
                lacks(&checkpoint, SESSIONS + 1);
            }
            _ => {
                let staging = line("staging ");
                let admitted = [
                    "false []".to_string(),
                    "true []".to_string(),
                    format!("true [{LOCK_FILE:?}]"),
                    format!("true [{BUILDING_DIR_NAME:?}]"),
                    format!("true [{LOCK_FILE:?}, {BUILDING_DIR_NAME:?}]"),
                ];
                assert!(admitted.contains(&staging), "with {free} free: {staging}");
                assert_eq!(line("again "), "Ok(())", "{said:#?}");
                assert_eq!(line("staging after "), "false", "{said:#?}");
                holds(&checkpoint, 1..=SESSIONS + 1);
            }
        }
        holds(&dir, 1..=SESSIONS + 2);
        eprintln!("with {free} free: {error}; staging {}", line("staging "));
    }
    let (free, linked) = created_with.expect("no count of free descriptors let a creation through");
    // With every file linked, a creation on Linux takes the two locks,
    // its manifest and one directory at a time: four descriptors.
    #[cfg(target_os = "linux")]
    if linked {
        assert_eq!(free, 4);
    }
    let _ = (free, linked);
}

/// The Merk that [`child_commits_at_the_limit`] writes over each of the
/// stored ones, so that what they held is dead for the cleaner.
fn rewritten() -> Merk {
    Merk::sized(b'k', 12, 1700)
}

/// The child of
/// [`a_session_at_the_limit_publishes_its_commits_and_write_back_goes_on`]:
/// with the session's threads ended, and every descriptor the process may
/// open taken, it commits and makes a turn of write-back, a table
/// checkpoint and a pass of the cleaner, flushes, gives the descriptors
/// back and makes the turn, the checkpoint and the pass again. In the
/// mode `log` its log files are small, so that the commits fill the file
/// the log writes to many times over. Run on its own it does nothing.
#[test]
fn child_commits_at_the_limit() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let small_log = std::env::var(CHILD_MODE).as_deref() == Ok("log");
    let options = Options {
        log_file_len: small_log.then_some(64 << 10),
        // A cleaner that moves every block it finds dead.
        cleaner: Some(CleanerConfig {
            target_dead_percent: 0,
            min_segment_len: 4 << 10,
            ..CleanerConfig::new()
        }),
        ..still()
    };
    let db = Db::open(&dir, &options).expect("the database");
    halt_background(&db);
    let write_back = db.shared.write_back();
    // What the stored Merks held is dead once this is written back: the
    // cleaner has segments and value-log files to give back.
    for name in 1..=SESSIONS {
        commit_of(&db, name, rewritten(), false).expect("a commit");
    }
    write_back.tick().expect("a turn of write-back");
    write_back.checkpoint().expect("a table checkpoint");

    let taken = take_descriptors(0);
    for name in 10..40 {
        say(format_args!("committed {name} {:?}", commit(&db, name)));
    }
    say(format_args!("turn {:?}", told(write_back.tick())));
    say(format_args!("table {:?}", told(write_back.checkpoint())));
    let cleaned = crate::db::with_cleaner(&db.shared, |cleaner, _| cleaner.pass());
    say(format_args!("cleaner {:?}", told(cleaned)));
    say(format_args!("flush {:?}", told(db.flush())));
    say(format_args!(
        "refusal {:?}",
        db.shared
            .store
            .refusal()
            .map(|reopen: Reopen| format!("{reopen:?}"))
    ));
    drop(taken);
    // Nothing turned the log since the commits: no thread runs.
    let longest = (fs::read_dir(dir.join("wal")).expect("the log's directory"))
        .map(|entry| entry.expect("an entry").metadata().expect("a file").len())
        .max();
    say(format_args!(
        "longest log file {}",
        longest.expect("a log file")
    ));
    let turn = write_back.tick().map(|tick| tick.pass.written);
    say(format_args!(
        "pages written after {:?}",
        turn.map_err(|error| error.to_string())
    ));
    let table = write_back
        .checkpoint()
        .map(|done| matches!(done, Checkpointed::Installed { .. }));
    say(format_args!(
        "table installed after {:?}",
        table.map_err(|error| error.to_string())
    ));
    let cleaned = crate::db::with_cleaner(&db.shared, |cleaner, _| cleaner.pass());
    let gave_back = cleaned.map(|pass| pass.blocks_moved + (pass.retired + pass.dropped) as u64);
    say(format_args!(
        "cleaner gave back after {:?}",
        gave_back.map_err(|error| error.to_string())
    ));
    say(format_args!("committed after {:?}", commit(&db, 40)));
    say(format_args!("flush after {:?}", told(db.flush())));
}

/// A session that meets the limit while it runs publishes the commits
/// that need no new file: a commit appends to a log file that is open,
/// and where the log cannot make its next file it goes on in the one that
/// is full. A turn of write-back, a table checkpoint and a pass of the
/// cleaner fail with the error of the file they could not make: a new
/// segment, or a new log file where the log is due to turn, or the next
/// manifest. The session is not ended for it: once the descriptors are
/// back the turn writes pages back, the table checkpoint is installed and
/// the cleaner gives segments back, and every commit is in the database
/// the next open finds.
#[test]
fn a_session_at_the_limit_publishes_its_commits_and_write_back_goes_on() {
    // Write-back's segment and value-log file of the stored session are
    // open still, so the pages go into them, and the turn fails at the
    // manifest that names their new lengths; where the log is due to
    // turn, it fails at the log's next file first.
    for (mode, failed_in) in [("pages", "MANIFEST.tmp"), ("log", "wal")] {
        let base = temp();
        let dir = base.path().join("live");
        stored(&dir);
        let test = "db::tests::open_files::child_commits_at_the_limit";
        let limit = Some(CHILD_LIMIT);
        let said = Child {
            test,
            dir: &dir,
            limit,
            free: 0,
            mode,
        }
        .said();
        eprintln!("{mode}: {said:#?}");
        let line = |prefix: &str| line(&said, prefix);
        for name in 10..40 {
            assert_eq!(
                line(&format!("committed {name} ")),
                "Ok(())",
                "{mode}: {said:#?}"
            );
        }
        let in_kind = dir.join(failed_in).to_string_lossy().into_owned();
        let turn = line("turn ");
        assert!(
            turn.contains(AT_THE_LIMIT) && turn.contains(&in_kind),
            "{mode}: {said:#?}"
        );
        assert!(line("table ").contains(AT_THE_LIMIT), "{mode}: {said:#?}");
        let cleaner = line("cleaner ");
        let segment = format!(
            "the cleaner's segments: cannot create {}",
            dir.join("seg").display()
        );
        assert!(
            cleaner.contains(&segment) && cleaner.contains(AT_THE_LIMIT),
            "{mode}: {said:#?}"
        );
        assert_eq!(line("flush "), "Ok(())", "{mode}: {said:#?}");
        assert_eq!(line("refusal "), "None", "{mode}: {said:#?}");
        let longest: u64 = line("longest log file ").parse().expect("a length");
        if mode == "log" {
            assert!(longest > 4 * (64 << 10), "{said:#?}");
        }
        let written = line("pages written after ");
        let written: u64 = (written
            .strip_prefix("Ok(")
            .and_then(|rest| rest.strip_suffix(')')))
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("{mode}: {said:#?}"));
        assert!(written > 0, "{mode}: {said:#?}");
        assert_eq!(
            line("table installed after "),
            "Ok(true)",
            "{mode}: {said:#?}"
        );
        let gave_back = line("cleaner gave back after ");
        assert!(
            gave_back.starts_with("Ok(") && gave_back != "Ok(0)",
            "{mode}: {said:#?}"
        );
        assert_eq!(line("committed after "), "Ok(())", "{mode}: {said:#?}");
        assert_eq!(line("flush after "), "Ok(())", "{mode}: {said:#?}");
        let db = Db::open(&dir, &still()).expect("the database opens");
        assert!(db.verify(None).is_sound());
        let snapshot = db.snapshot();
        for name in 1..=SESSIONS {
            let items = items_of(&snapshot.operation(), subtree(name));
            assert_eq!(items, Some(rewritten().items()), "{mode}: subtree {name}");
        }
        drop(snapshot);
        db.close().expect("the session ends");
        holds(&dir, 10..=40);
    }
}
