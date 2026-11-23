//! The mark of a restore that did not finish, where a commit that fails
//! or a process that stops leaves it (VER-04, for GROVE-07).
//!
//! A session of state sync that may commit before its end marks its
//! database with its first commit (`GroveDb::has_incomplete_restore`),
//! and the commit at its end takes the mark away. The engine's own tests
//! hold the mark where a session is dropped or refused
//! (`grovedb/src/engine/lignin/tests/sync.rs`). The ways here need a
//! commit that fails or a process that stops, and the databases of the
//! engine crash campaign have both: GroveDB on the LigninDB engine on the
//! simulated file layer, opened as the campaign opens them
//! (`lignin_testkit::crash::engine::grove`).
//!
//! A database of the engine has three threads beside the one that calls
//! it, and they write and sync the log too. So the faults are not rules of
//! the simulated file layer, which fail the next call of any thread: they
//! are made by a file layer of these tests ([`Faulty`]) that fails what
//! the calling thread does to the log, and what any thread does to it
//! after that. What became of a commit is read from the version of the
//! database and not from what the call answered.

// The engine the golden corpus is pinned to has no LigninDB under GroveDB
// (`build.rs`).
#![cfg(grovedb_engine_seam)]
// GroveDB's error is what its calls return.
#![allow(clippy::result_large_err)]

use std::{
    collections::{BTreeSet, VecDeque},
    io,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    thread::{self, ThreadId},
};

use grovedb::{
    engine::lignin::LigninEngine,
    replication::{MultiStateSyncSession, RestoreCommitMode, CURRENT_STATE_SYNC_VERSION},
    Element, Error,
};
use grovedb_merk::store::lignin::hash::MerkHashRules;
use grovedb_version::version::GroveVersion;
use lignin_format::PageSize;
use lignin_io::{
    sim::{SimConfig, SimVfs},
    DirEntry, OpenMode, Vfs, VfsFile, VfsLock,
};
use lignin_store::wal::SyncPolicy;
use lignin_testkit::crash::engine::{
    grove::{self, db_dir, Grove, Tuning},
    model::{Places, State},
};
use lignindb::Db;

const VERSION: u16 = CURRENT_STATE_SYNC_VERSION;

/// The mode of a session that may commit before its end, with a budget of
/// one byte and one subtree in flight.
const PARTS: RestoreCommitMode = RestoreCommitMode::Incremental {
    budget_bytes: 1,
    max_subtrees_in_flight: 1,
};

/// The `subtrees_batch_size` of a session.
const BATCH: usize = 4;

type Session<'db> = Pin<Box<MultiStateSyncSession<'db, LigninEngine>>>;

/// What the file layer of these tests fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failing {
    Nothing,
    /// Every append to a log file that the thread of the test makes, with
    /// none of its bytes left.
    Appends,
    /// Every sync of a log file, by any thread, once the thread of the
    /// test has appended to one: `appended` says whether it has.
    Syncs {
        appended: bool,
    },
}

/// The simulated file layer, with the log of the database behind a gate.
///
/// Every append to a log file and every sync of one is made under one
/// lock, so the order of those calls is one order for all threads, and
/// what is failed goes by it: the append that a commit of the test's
/// thread makes, or the first sync of the log after such an append,
/// whichever thread makes it, and every one after.
#[derive(Clone, Debug)]
struct Faulty {
    fs: Arc<SimVfs>,
    gate: Arc<Gate>,
}

#[derive(Debug)]
struct Gate {
    test: ThreadId,
    failing: Mutex<Failing>,
}

#[derive(Debug)]
struct LogFile {
    file: Box<dyn VfsFile>,
    gate: Arc<Gate>,
}

fn failed() -> io::Error {
    io::Error::other("a fault of the test")
}

impl Faulty {
    fn new(seed: u64) -> Self {
        let gate = Gate {
            test: thread::current().id(),
            failing: Mutex::new(Failing::Nothing),
        };
        Self {
            fs: Arc::new(SimVfs::new(SimConfig::new(seed))),
            gate: Arc::new(gate),
        }
    }

    fn fail(&self, what: Failing) {
        *self.gate.failing.lock().expect("no thread panicked") = what;
    }

    fn vfs(&self) -> Arc<dyn Vfs> {
        Arc::new(self.clone())
    }
}

impl Vfs for Faulty {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let file = self.fs.open(path, mode)?;
        let of_the_log = path.components().any(|part| part.as_os_str() == "wal");
        Ok(match of_the_log {
            true => Box::new(LogFile {
                file,
                gate: self.gate.clone(),
            }),
            false => file,
        })
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.fs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.fs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.fs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.fs.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.fs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.fs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.fs.sync_dir(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.fs.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.fs.lock_existing(path)
    }
}

impl LogFile {
    fn sync(&self, sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>) -> io::Result<()> {
        let failing = self.gate.failing.lock().expect("no thread panicked");
        match *failing {
            Failing::Syncs { appended: true } => Err(failed()),
            _ => sync(&*self.file),
        }
    }
}

impl VfsFile for LogFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.file.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.file.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        let mut failing = self.gate.failing.lock().expect("no thread panicked");
        if thread::current().id() == self.gate.test {
            match &mut *failing {
                Failing::Appends => return Err(failed()),
                Failing::Syncs { appended } => *appended = true,
                Failing::Nothing => {}
            }
        }
        self.file.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.sync(|file| file.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync(|file| file.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
}

/// A database whose background work is left to the engine's defaults.
fn tuning(sync: SyncPolicy) -> Tuning {
    Tuning {
        sync,
        ..Tuning::at_rest(PageSize::MIN)
    }
}

fn opened(files: Arc<dyn Vfs>, sync: SyncPolicy, create_if_missing: bool) -> Grove {
    grove::open(files, db_dir(), &tuning(sync), create_if_missing).expect("a database")
}

fn root_of(db: &Grove, v: &GroveVersion) -> [u8; 32] {
    db.root_hash(None, v).unwrap().expect("a root hash")
}

/// A grove to restore: four trees, each with rows and with two trees of
/// rows. `places` are its subtrees: a walk of another database that is
/// told of them lists what it holds under their paths, also where the
/// walk does not reach them.
struct Source {
    db: Grove,
    root: [u8; 32],
    places: Places,
}

fn source(v: &GroveVersion) -> Source {
    let db = opened(Faulty::new(1).vfs(), SyncPolicy::Never, true);
    let put = |path: &[&[u8]], key: &[u8], element: Element| {
        let inserted = db.insert(path, key, element, None, None, v);
        inserted.unwrap().expect("an element");
    };
    let rows = |path: &[&[u8]], fill: u8| {
        for row in 0..10u8 {
            put(path, &[b'r', row], Element::new_item(vec![fill ^ row; 60]));
        }
    };
    for tree in 0..4u8 {
        let outer = [b't', tree];
        put(&[], &outer, Element::empty_tree());
        rows(&[&outer], tree);
        for child in 0..2u8 {
            let inner = [b'c', child];
            put(&[&outer], &inner, Element::empty_tree());
            rows(&[&outer, &inner], 0x40 | tree << 2 | child);
        }
    }
    let root = root_of(&db, v);
    let (_, places) = grove::read_committed(&db, v, &Places::new()).expect("a walk");
    assert_eq!(places.len(), 1 + 4 + 8);
    Source { db, root, places }
}

/// What the database holds, with everything it has under the paths of the
/// source's subtrees.
fn held(db: &Grove, source: &Source, v: &GroveVersion) -> State {
    let read = grove::read_committed(db, v, &source.places);
    read.expect("a walk").0
}

/// A session into a database, with what it has asked of its source and
/// was not given yet.
struct Restore<'s, 'db> {
    source: &'s Source,
    session: Session<'db>,
    asked: VecDeque<Vec<u8>>,
    given: Option<(Vec<u8>, Vec<u8>)>,
}

impl<'s, 'db> Restore<'s, 'db> {
    fn begin(source: &'s Source, target: &'db Grove, v: &GroveVersion) -> Result<Self, Error> {
        let session =
            target.start_snapshot_syncing_with_mode(source.root, BATCH, VERSION, PARTS, v)?;
        Ok(Self {
            source,
            session,
            // The first request of a session is the root hash of the grove.
            asked: VecDeque::from([source.root.to_vec()]),
            given: None,
        })
    }

    /// Gives the session what it asked for first, and takes in what it
    /// asks for next.
    fn step(&mut self, v: &GroveVersion) -> Result<(), Error> {
        let request = self.asked.pop_front().expect("the session asks for more");
        let fetched = self.source.db.fetch_chunk(&request, None, VERSION, v);
        let payload = fetched.expect("the source serves the request");
        self.given = Some((request.clone(), payload.clone()));
        let more = self.session.apply_chunk(&request, &payload, VERSION, v)?;
        self.asked.extend(more);
        Ok(())
    }

    /// Gives the session once more what it was given last.
    fn again(&mut self, v: &GroveVersion) -> Result<(), Error> {
        let (request, payload) = self.given.clone().expect("a chunk was given");
        let applied = self.session.apply_chunk(&request, &payload, VERSION, v);
        applied.map(|_| ())
    }

    /// Goes on until the session has committed `commits` times.
    fn until_commits(&mut self, commits: usize, v: &GroveVersion) {
        while self.session.intermediate_commits() < commits {
            self.step(v).expect("the chunk is taken");
        }
        assert!(!self.session.is_sync_completed());
    }

    /// Goes on until the session has asked for nothing more, and returns
    /// it for the commit at its end.
    fn ended(mut self, v: &GroveVersion) -> Session<'db> {
        while !self.asked.is_empty() {
            self.step(v).expect("the chunk is taken");
        }
        assert!(self.session.is_sync_completed());
        self.session
    }

    /// Goes on until a chunk is refused.
    fn until_refused(&mut self, v: &GroveVersion) -> Error {
        loop {
            if let Err(error) = self.step(v) {
                return error;
            }
        }
    }
}

/// The rules that the physical checker finds broken in the database in
/// `files`, which nothing has open: every finding it has, none left out.
fn findings(files: Arc<dyn Vfs>, v: &GroveVersion) -> BTreeSet<String> {
    let options = tuning(SyncPolicy::Never).options(files, false);
    let db = Db::open(db_dir(), &options).expect("the database opens again");
    let mut rules = MerkHashRules::for_check(v);
    let report = db.close_and_verify(Some(&mut rules)).expect("a close");
    assert_eq!(report.more_findings, 0, "{:?}", report.findings);
    let rule = |finding: &lignindb::verify::Finding| format!("{:?}", finding.rule);
    report.findings.iter().map(rule).collect()
}

/// What is held of a database that a session left with its mark, once
/// nothing has it open: opened again it says that it holds a restore that
/// did not finish and starts no session, and is closed. The physical
/// checker then finds nothing in it but value hashes: those of elements
/// that commit to a Merk the session had not restored.
fn assert_marked(files: Arc<dyn Vfs>, source: &Source, v: &GroveVersion) {
    let again = opened(files.clone(), SyncPolicy::Never, false);
    assert!(again.has_incomplete_restore().expect("a read of the mark"));
    let refused = Restore::begin(source, &again, v).err();
    let refused = refused.expect("a marked database starts no session");
    assert!(
        refused.to_string().contains("holds an incomplete restore"),
        "{refused}"
    );
    drop(again);
    let found = findings(files, v);
    let by_design = BTreeSet::from(["Hash(Value)".to_string()]);
    assert!(found.is_subset(&by_design), "{found:?}");
}

/// The grove of these tests is large enough for them: a session that
/// restores it in parts commits more than three times before its end. It
/// holds the mark from the first of those commits to the commit at its
/// end, which takes the mark away and leaves the grove of the source,
/// sound.
#[test]
fn a_session_in_parts_holds_the_mark_until_its_end() {
    let v = GroveVersion::latest();
    let source = source(v);
    let files = Faulty::new(2);
    let target = opened(files.vfs(), SyncPolicy::Never, true);
    assert!(!target.has_incomplete_restore().expect("a read of the mark"));
    let mut restore = Restore::begin(&source, &target, v).expect("a session starts");
    restore.until_commits(1, v);
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    let session = restore.ended(v);
    let commits = session.intermediate_commits();
    assert!(commits > 3, "{commits} commits before the end");
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    let committed = target.commit_session(session, v);
    committed.expect("the session commits");
    assert!(!target.has_incomplete_restore().expect("a read of the mark"));
    assert_eq!(
        held(&target, &source, v).difference(&held(&source.db, &source, v)),
        None
    );
    drop(target);
    assert_eq!(findings(files.vfs(), v), BTreeSet::new());
}

/// **A later commit of the session fails.** The session has committed
/// twice. Then every append that the session's thread makes to the log
/// fails and leaves none of its bytes, so its next commit is not in the
/// log: the chunk that brought it is refused, the session counts two
/// commits and takes no chunk any more, and the database is at the
/// version of the second. It holds the mark, as it stands and opened
/// again.
#[test]
fn a_later_commit_of_a_session_that_fails_leaves_the_mark() {
    let v = GroveVersion::latest();
    let source = source(v);
    let files = Faulty::new(3);
    let target = opened(files.vfs(), SyncPolicy::Never, true);
    let mut restore = Restore::begin(&source, &target, v).expect("a session starts");
    restore.until_commits(2, v);
    let before = grove::version_of(&target);

    files.fail(Failing::Appends);
    let failed = restore.until_refused(v).to_string();
    assert!(
        failed.contains("failed to commit intermediate sync batch"),
        "{failed}"
    );
    assert_eq!(restore.session.intermediate_commits(), 2);
    assert_eq!(grove::version_of(&target), before);
    let after = restore
        .again(v)
        .expect_err("a failed session takes no chunk");
    assert!(after.to_string().contains("session has failed"), "{after}");
    files.fail(Failing::Nothing);

    drop(restore);
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    drop(target);
    assert_marked(files.vfs(), &source, v);
}

/// **The process stops.** The session has committed at least twice when
/// the files are taken as they are. What a process that is killed leaves,
/// every change it made, holds the mark. And a machine that stops then
/// and loses a part of what was not synced, as a file system that writes
/// in order loses it, leaves one of two: a database that holds the mark,
/// or the database as it was before the session, at its version and with
/// nothing more under the paths of the restore, which is sound. Every
/// such state is looked at.
#[test]
fn a_process_that_stops_during_a_session_leaves_the_mark() {
    let v = GroveVersion::latest();
    let source = source(v);
    let files = Faulty::new(4);
    let target = opened(files.vfs(), SyncPolicy::Never, true);
    let before = (grove::version_of(&target), held(&target, &source, v));
    assert_ne!(before.1.root, source.root);
    let mut restore = Restore::begin(&source, &target, v).expect("a session starts");
    restore.until_commits(2, v);
    restore.step(v).expect("the chunk is taken");
    assert!(!restore.session.is_sync_completed());
    let image = files.fs.crash_image();

    let state = |plan: &lignin_io::sim::CrashPlan| -> Arc<dyn Vfs> {
        Arc::new(image.recover(plan).expect("a plan of the image"))
    };
    assert_marked(state(&image.keep_all()), &source, v);

    let mut plans = vec![image.lose_all()];
    plans.extend(image.prefixes());
    let (mut marked, mut as_before) = (0, 0);
    for plan in &plans {
        let left = state(plan);
        let db = opened(left.clone(), SyncPolicy::Never, false);
        if db.has_incomplete_restore().expect("a read of the mark") {
            drop(db);
            assert_marked(left, &source, v);
            marked += 1;
        } else {
            assert_eq!(grove::version_of(&db), before.0, "{plan:?}");
            assert_eq!(
                held(&db, &source, v).difference(&before.1),
                None,
                "{plan:?}"
            );
            drop(db);
            assert_eq!(findings(left, v), BTreeSet::new(), "{plan:?}");
            as_before += 1;
        }
    }
    println!(
        "{} crash plans: {marked} leave the mark, {as_before} the database as it was",
        plans.len()
    );
    // The plan that keeps every change in order is among them.
    assert!(marked > 0);
    drop(restore);
    drop(target);
}

/// **A commit is answered with an error and is published.** The commits
/// of the database sync the log, and from the session's first append to
/// the log on, every sync of the log fails: the first commit of the
/// session is published and answered with the error of its sync. The
/// chunk that brought it is refused, the session counts no commit and
/// takes no chunk any more, and the database is at the version after: it
/// holds the mark of a session that says it committed nothing, as it
/// stands and opened again.
#[test]
fn a_commit_that_is_published_and_answered_with_an_error_leaves_the_mark() {
    let v = GroveVersion::latest();
    let source = source(v);
    let files = Faulty::new(5);
    let target = opened(files.vfs(), SyncPolicy::EveryCommit, true);
    let mut restore = Restore::begin(&source, &target, v).expect("a session starts");
    let before = grove::version_of(&target);

    files.fail(Failing::Syncs { appended: false });
    let failed = restore.until_refused(v).to_string();
    assert!(
        failed.contains("failed to commit intermediate sync batch"),
        "{failed}"
    );
    assert_eq!(restore.session.intermediate_commits(), 0);
    assert_eq!(grove::version_of(&target), before + 1);
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    let after = restore
        .again(v)
        .expect_err("a failed session takes no chunk");
    assert!(after.to_string().contains("session has failed"), "{after}");
    files.fail(Failing::Nothing);

    drop(restore);
    drop(target);
    assert_marked(files.vfs(), &source, v);
}

/// The commit at the end of a session that is answered in that way is
/// published too, and it is the one that takes the mark away: the session
/// is answered with an error, and the database holds the whole grove of
/// the source and no mark, as it stands and opened again, and is sound.
#[test]
fn the_commit_at_the_end_that_is_published_and_answered_with_an_error_takes_the_mark_away() {
    let v = GroveVersion::latest();
    let source = source(v);
    let files = Faulty::new(6);
    let target = opened(files.vfs(), SyncPolicy::EveryCommit, true);
    let restore = Restore::begin(&source, &target, v).expect("a session starts");
    let session = restore.ended(v);
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    let before = grove::version_of(&target);

    files.fail(Failing::Syncs { appended: false });
    let committed = target.commit_session(session, v);
    let failed = committed.expect_err("the sync of the commit fails");
    assert!(
        failed
            .to_string()
            .contains("failed to commit sync transaction"),
        "{failed}"
    );
    assert_eq!(grove::version_of(&target), before + 1);
    assert!(!target.has_incomplete_restore().expect("a read of the mark"));
    assert_eq!(root_of(&target, v), source.root);
    files.fail(Failing::Nothing);

    drop(target);
    let again = opened(files.vfs(), SyncPolicy::Never, false);
    assert!(!again.has_incomplete_restore().expect("a read of the mark"));
    assert_eq!(
        held(&again, &source, v).difference(&held(&source.db, &source, v)),
        None
    );
    drop(again);
    assert_eq!(findings(files.vfs(), v), BTreeSet::new());
}
