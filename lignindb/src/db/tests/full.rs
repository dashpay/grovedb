//! OPS-08: a device that is full at every write point never corrupts the
//! database (`docs/lignindb/disk-full-and-backup.md`, DF1 to DF9).
//!
//! One session through the facade is scripted: a database is made,
//! written with and without a transaction, written back, flushed,
//! checkpointed, reclaimed after a drop, wiped, closed and opened again.
//! It is run once to count the file operations that can need room, and
//! then once for each of them with the simulated device full at that
//! operation: for that operation alone, from it on with syncs refused as
//! well, and from it on with syncs that succeed. A database of the sweep
//! is opened with neither of its threads (`open_at_rest`), and the script
//! makes every turn of write-back and of the reclaimer. So nothing but the
//! script and the device decides what a run does, and each run makes the
//! file operations of the run with no fault up to the one that is refused:
//! a run that does not is a failure of the sweep, which names the first
//! operation that differed.
//!
//! Whatever was refused, the session behaves as its caller would: it goes
//! on where the engine lets it, and closes the database and opens it again
//! where the engine says so, or where the open itself is refused. A model
//! knows what every commit was answered with. Then, at each of those
//! operations:
//!
//! - every reader reads the latest version that was published, whole;
//! - the physical checker (`crate::verify`) finds the store sound as it
//!   is, with the device still full;
//! - whatever a power cut at that moment leaves opens at a version that
//!   the answers allow, with that version's records, sound;
//! - a checkpoint that was made is the database of its version, and what
//!   a creation that was refused left is no database;
//! - with room again one of two holds, and the session says which
//!   (D-151): `Db::reopen` says nothing and the next commit is taken, or
//!   it says that the session has ended, and after a close and an open the
//!   next commit is taken. Nothing else: no commit is refused with room in
//!   a session that has not said so, and no call is answered with what
//!   stays failed until the database is opened again while `Db::reopen`
//!   says nothing. The session that met the refusal is asked this before
//!   anything ends it: at once where one operation was refused, and with
//!   room for that moment before the script closes it where the device
//!   stays full. The session that is open when the script has ended is
//!   asked again;
//! - write-back, a flush and a close work again, and the database closes
//!   and opens once more as what it was.

use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::{atomic::AtomicU64, Arc},
};

use lignin_io::sim::{
    Event, Fault, FaultRule, OpKind, OpSet, Outcome as OpOutcome, SimConfig, SimVfs, UnknownFate,
};
use lignin_store::{
    flush::{FlushError, Stores, Tick, WriteBack},
    segment::{read_manifest, SegmentStoreError},
    wal::{LogError, SyncPolicy},
};
use lignin_tree::value::{RegroupPolicy, Regrouping};

use super::*;
use crate::{
    backup::{
        tests::world::{
            batch, check_reads, commit, first, height, hosted, key, next, value, State,
        },
        Backup,
    },
    checkpoint::{CheckpointError, Checkpointer},
    commit::CommitError,
    db::{DbError, OpenError, Reopen, WriteError},
    space::out_of_space,
    txn::tests::{
        model::{self, OwnedBatch, OwnedSubtree},
        put,
        store::Rules,
    },
    verify,
};

const DIR: &str = "/db";
const CHECKPOINT: &str = "/checkpoint";

/// How the device is made full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Mode {
    /// One operation is refused for room, and the next finds some.
    Once,
    /// From one operation on nothing that needs room succeeds, a sync
    /// included.
    Full,
    /// The same, and syncs succeed: what was written before the device
    /// filled is made durable.
    FullButSyncs,
}

impl Mode {
    pub(super) fn rule(self, skip: u64) -> FaultRule {
        let rule = FaultRule::new(Fault::Enospc).skip(skip);
        match self {
            Self::Once => rule,
            Self::Full => rule.forever(),
            Self::FullButSyncs => rule.on(OpSet::WRITES | OpSet::NAMES).forever(),
        }
    }
}

/// What a session was told of its writes: the model of what the database
/// holds, and of what a recovery may land on.
pub(super) struct Told {
    /// The state of each version that was published and that a recovery
    /// may still land on, and of the version the database was last opened
    /// at.
    versions: BTreeMap<u64, State>,
    pub(super) latest: u64,
    /// The newest version that is known to be durable.
    pub(super) durable: u64,
    /// A commit of which the engine could not say whether it is in the
    /// log, with the state it would give: the next open has it as the
    /// version after the latest, or not at all.
    unsettled: Option<State>,
    /// What happened, in a few words each.
    pub(super) log: Vec<String>,
}

impl Told {
    fn new() -> Self {
        Self {
            versions: BTreeMap::from([(1, State::new())]),
            latest: 1,
            durable: 0,
            unsettled: None,
            log: Vec::new(),
        }
    }

    pub(super) fn state(&self) -> &State {
        &self.versions[&self.latest]
    }

    pub(super) fn note(&mut self, what: impl Into<String>) {
        self.log.push(what.into());
    }

    /// The state of the version `landed` that an open found, which must be
    /// one that the answers allow: after a power cut any version from the
    /// newest durable one to the latest, and otherwise the latest; or, in
    /// both cases, the commit that was left unsettled.
    ///
    /// "Otherwise the latest" is asked of the simulator, where every read
    /// sees every byte that was written until the power is cut, whatever
    /// became of its sync. It is not what DF1 promises of a device: where
    /// the system failed to write what a session wrote, and a refused
    /// sync is that, an open with no power cut is allowed what an open
    /// after one is (`docs/lignindb/sync-handles.md`, section 6).
    fn allowed(&self, landed: u64, cut: bool, context: &str) -> &State {
        if let Some(unsettled) = &self.unsettled
            && unsettled.version() == landed
        {
            return unsettled;
        }
        let oldest = match cut {
            true => self.durable.max(1),
            false => self.latest,
        };
        assert!(
            oldest <= landed && landed <= self.latest,
            "{context}: the database opened at version {landed}, and the answers allow {oldest} \
             to {}",
            self.latest
        );
        &self.versions[&landed]
    }
}

/// What a session of these tests saw, for the count of what a sweep saw.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Seen {
    /// An open was refused for room.
    pub(super) open_refused: bool,
    /// A session said that it takes no write any more.
    pub(super) over: bool,
    /// A commit was answered as unsettled.
    pub(super) unsettled: bool,
    /// A commit was answered as failed, or refused until the cut.
    pub(super) failed: bool,
    /// When there was room again, the session that was open had said that
    /// it had ended, and the writer was taken by the next one.
    pub(super) reopened: bool,
    /// When there was room again, the session that was open took the next
    /// commit.
    pub(super) went_on: bool,
    /// The one operation that was refused was a step of a replacement of
    /// the manifest, which no session ends for.
    pub(super) settled: bool,
    /// The session that met the refusal had not said that it had ended,
    /// and took a commit as soon as there was room.
    pub(super) resumed: bool,
}

pub(super) struct Session {
    pub(super) fs: SimVfs,
    pub(super) options: Options,
    pub(super) db: Option<Db>,
    pub(super) told: Told,
    /// Whether the background work of the database is at rest, and the
    /// session makes its turns. A database that is opened while it is, is
    /// opened with neither of its threads.
    halted: bool,
    /// The state that a checkpoint in [`CHECKPOINT`] holds if there is
    /// one, and whether its creation returned: one that was refused may
    /// have left its target whole.
    checkpointed: Option<(State, bool)>,
    /// Whether a power cut was looked at since the device first refused.
    cut_seen: bool,
    /// How the device of a sweep is made full. A session that is no run
    /// of a sweep has none, and is not asked by itself whether it resumes.
    pub(super) mode: Option<Mode>,
    /// Whether the session that met the refusal was asked whether it
    /// resumes with room ([`Self::resumes`]).
    asked: bool,
    /// Whether write-back's loop makes its next turn before the session
    /// makes its next commit, after a turn that failed: what a turn can
    /// settle is settled then, a manifest in doubt among it.
    pub(super) loop_first: bool,
    pub(super) seen: Seen,
}

/// One turn of write-back as the background loop of a database makes it.
pub(super) fn tick(db: &Db) -> Result<Tick, FlushError> {
    let shared = &*db.shared;
    let store = &*shared.store;
    let plain = shared.write_back();
    let regrouping = Regrouping::new(
        store.loader(),
        &store.pool,
        &store.values,
        store.page_size,
        shared.steps(),
        RegroupPolicy::default(),
    );
    let stores = Stores {
        page_values: Some(&regrouping),
        ..plain.stores()
    };
    let write_back = WriteBack::new(stores, &store.flush, store.page_size).unwrap_or(plain);
    write_back.tick()
}

/// The physical check of a database in which nothing runs, with every
/// stored hash made again by the rules of the tests' model.
pub(super) fn check_still(db: &Db) -> verify::Report {
    let at = db.shared.versions.latest();
    let calls = AtomicU64::new(0);
    db.check(at, verify::Scope::Still, Some(&mut Rules(&calls)))
}

/// Fails if the checker finds anything in a store at rest.
pub(super) fn sound(db: &Db, context: &str) {
    let report = check_still(db);
    assert!(
        report.findings.is_empty() && report.more_findings == 0,
        "{context}: the store is not sound:\n{report}"
    );
}

fn brief(error: &dyn std::fmt::Display) -> String {
    let text = error.to_string();
    let mut brief: String = text.chars().take(72).collect();
    if text.contains("ENOSPC") && !brief.contains("ENOSPC") {
        brief.push_str("..ENOSPC");
    }
    brief
}

/// Whether `error` is one of the answers of a store in which something
/// stays failed until it is opened again, whatever room there is (DF8): a
/// sync of the log failed, so that the log makes nothing durable, or a
/// sync of a segment failed, so that no table checkpoint is installed.
pub(super) fn stays_failed(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(error) = next {
        let log = error.downcast_ref::<LogError>();
        let flush = error.downcast_ref::<FlushError>();
        if matches!(log, Some(LogError::SyncFailed { .. }))
            || matches!(flush, Some(FlushError::SegmentsNotDurable))
        {
            return true;
        }
        next = error.source();
    }
    false
}

/// Whether `error` says that the log has no file that takes items.
fn no_log_file(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(error) = next {
        if matches!(
            error.downcast_ref::<LogError>(),
            Some(LogError::NoActiveFile)
        ) {
            return true;
        }
        next = error.source();
    }
    false
}

/// What holds of every call of a session that failed with `error`, but a
/// commit, which the model looks at: the device refused it, or the
/// session had ended. And a session in which something stays failed says
/// so from that call on.
fn answered(db: &Db, what: &str, error: &(dyn std::error::Error + 'static)) {
    let over = db.reopen();
    assert!(
        out_of_space(error) || over.is_some(),
        "{what} failed, and not for room, in a session that goes on: {error}"
    );
    assert!(
        !stays_failed(error) || over.is_some(),
        "{what} was answered with what stays failed until the database is opened again, and \
         the session does not say that it has ended: {error}"
    );
    assert!(
        !matches!(
            error.downcast_ref::<DbError>(),
            Some(DbError::Removed { .. })
        ),
        "{what}: {error}"
    );
}

impl Session {
    /// A session on an empty file system. With `halted` its databases are
    /// opened with neither of their threads, so that the session makes
    /// every turn of the background work, and every file operation is
    /// recorded.
    pub(super) fn new(seed: u64, sync: SyncPolicy, halted: bool) -> Self {
        Self::fated(seed, sync, halted, SimConfig::new(seed).unknown)
    }

    /// As [`Self::new`], on a device that does to what a failed sync left
    /// unknown what `fate` says, once a later sync succeeds.
    pub(super) fn fated(seed: u64, sync: SyncPolicy, halted: bool, fate: UnknownFate) -> Self {
        let fs = SimVfs::new(SimConfig {
            trace_ops: halted,
            unknown: fate,
            ..SimConfig::new(seed)
        });
        let options = Options {
            vfs: Arc::new(fs.clone()),
            sync,
            ..options()
        };
        Self {
            fs,
            options,
            db: None,
            told: Told::new(),
            halted,
            checkpointed: None,
            cut_seen: false,
            mode: None,
            asked: false,
            loop_first: false,
            seen: Seen::default(),
        }
    }

    /// Brings the background work of the database to rest for good: from
    /// here on the session makes its turns, and the store can be checked
    /// as it is.
    pub(super) fn rest(&mut self) {
        if !self.halted {
            if let Some(db) = &self.db {
                halt_background(db);
            }
            self.halted = true;
        }
    }

    /// Opens the database, if it is not open. Says whether it is open.
    pub(super) fn open(&mut self) -> bool {
        if self.db.is_some() {
            return true;
        }
        let opened = match self.halted {
            true => open_at_rest(DIR, &self.options),
            false => Db::open(DIR, &self.options),
        };
        match opened {
            Ok(db) => {
                let landed = db.version().get();
                let state = self.told.allowed(landed, false, "an open").clone();
                let told = &mut self.told;
                told.unsettled = None;
                told.versions = BTreeMap::from([(landed, state)]);
                told.latest = landed;
                // What a session is opened at is durable before its first
                // commit is published (DU3).
                told.durable = landed;
                told.note(format!("open@{landed}"));
                self.db = Some(db);
                // A session that is opened after the device first refused
                // is not the one that met the refusal.
                self.asked |= self.fs.stats().faults > 0;
                true
            }
            Err(error) => {
                assert!(
                    out_of_space(&error),
                    "an open was refused, and not for room: {error}"
                );
                assert!(
                    matches!(
                        error,
                        OpenError::Io { .. }
                            | OpenError::Manifest(_)
                            | OpenError::Recovery(_)
                            | OpenError::Create { .. }
                    ),
                    "{error:?}"
                );
                self.seen.open_refused = true;
                self.told.note(format!("open!{}", brief(&error)));
                false
            }
        }
    }

    /// The turns of write-back that its loop would make until one moves
    /// nothing on, or fails. A failure is told as the loop tells it. Says
    /// whether one failed.
    pub(super) fn ticks(&mut self, what: &str) -> bool {
        if !self.halted {
            return false;
        }
        let Some(db) = &self.db else { return false };
        let mut said = format!("{what}..");
        let mut failed = false;
        for _ in 0..8 {
            match tick(db) {
                Ok(tick) if tick.progressed() => {}
                Ok(_) => {
                    said = format!("{what}.");
                    break;
                }
                Err(error) => {
                    let troubles = &db.shared.store.troubles;
                    troubles.write_back_failed(&error);
                    if self.loop_first
                        && let Err(again) = tick(db)
                    {
                        troubles.write_back_failed(&again);
                    }
                    answered(db, "a turn of write-back", &error);
                    said = format!("{what}!{}", brief(&error));
                    failed = true;
                    break;
                }
            }
        }
        self.told.note(said);
        self.after_step();
        failed
    }

    /// Commits the batch that `build` makes of the state the session has,
    /// with no transaction or in one ([`commit`]), and takes the answer
    /// into the model.
    pub(super) fn write(
        &mut self,
        name: &str,
        build: impl FnOnce(&State) -> OwnedBatch,
        in_txn: bool,
    ) {
        if !self.open() {
            return;
        }
        let db = self.db.as_ref().expect("the database is open");
        let batch = build(self.told.state());
        let ended = commit(db, &batch, in_txn);
        let mut after = self.told.state().clone();
        after.commit(&batch);
        self.settle(name, after, ended);
    }

    /// Wipes the database, and takes the answer into the model.
    fn wipe(&mut self) {
        if !self.open() {
            return;
        }
        let db = self.db.as_ref().expect("the database is open");
        let ended = db.wipe();
        let mut after = State::new();
        after.model.version = self.told.latest + 1;
        self.settle("wipe", after, ended);
    }

    /// Takes the answer `ended` of a commit that would have made `after`
    /// of the database.
    pub(super) fn settle(
        &mut self,
        name: &str,
        after: State,
        ended: Result<Committed, WriteError>,
    ) {
        let next = self.told.latest + 1;
        assert_eq!(after.version(), next, "{name}: the model's version");
        let told = &mut self.told;
        let publish = |told: &mut Told, version: u64| {
            assert_eq!(version, next, "{name}: the version of the commit");
            told.versions.insert(next, after.clone());
            told.latest = next;
        };
        match &ended {
            Ok(Committed::Published(commit)) => {
                publish(told, commit.version.get());
                if self.options.sync == SyncPolicy::EveryCommit {
                    told.durable = next;
                }
                told.note(format!("{name}=v{next}"));
            }
            Ok(Committed::Empty) => panic!("{name}: an empty commit"),
            Err(error) => {
                let said = match error {
                    WriteError::Reopen(_) => "reopen",
                    WriteError::Commit(CommitError::NotDurable {
                        published: commit, ..
                    }) => {
                        publish(told, commit.version.get());
                        "notdurable"
                    }
                    WriteError::Commit(CommitError::Unsettled { version, .. }) => {
                        assert_eq!(version.get(), next, "{name}");
                        told.unsettled = Some(after.clone());
                        self.seen.unsettled = true;
                        "unsettled"
                    }
                    WriteError::Commit(CommitError::Failed(_)) => {
                        self.seen.failed = true;
                        "failed"
                    }
                    WriteError::Commit(CommitError::NotReady(_)) => {
                        self.seen.failed = true;
                        "notready"
                    }
                    WriteError::Commit(other) => {
                        panic!("{name}: a full device is answered with {other:?}")
                    }
                };
                // An error that the device caused says so, unless it only
                // tells that the session had ended before, or that the log
                // has no file that takes items: then it was an append of
                // write-back beside the commit that the device refused,
                // before the commit took its transaction or after
                // (`crate::commit`, "The log is looked at once more").
                let beside = matches!(
                    error,
                    WriteError::Commit(CommitError::Failed(_) | CommitError::NotReady(_))
                ) && no_log_file(error);
                let told_of_room = !beside && !matches!(error, WriteError::Reopen(_));
                assert_eq!(out_of_space(error), told_of_room, "{name}: {error:?}");
                assert_eq!(
                    error.reopen().is_some(),
                    matches!(said, "reopen" | "notdurable" | "unsettled"),
                    "{name}: {error:?}"
                );
                told.note(format!("{name}!{said}"));
            }
        }
        let db = self.db.as_ref().expect("the database is open");
        assert_eq!(db.version().get(), self.told.latest, "{name}: the version");
        self.after_step();
        self.session_over();
    }

    /// Ends a session that says it takes no write any more, as its caller
    /// does.
    fn session_over(&mut self) {
        if self.db.as_ref().is_some_and(|db| db.reopen().is_some()) {
            self.seen.over = true;
            self.told.note("over");
            self.close();
        }
    }

    pub(super) fn flush(&mut self) {
        let Some(db) = &self.db else { return };
        match db.flush() {
            Ok(flushed) => {
                assert!(flushed.version.get() >= self.told.latest);
                self.told.durable = self.told.durable.max(self.told.latest);
                self.told.note("flush");
            }
            Err(error) => {
                assert!(
                    out_of_space(&error),
                    "a flush failed, and not for room: {error}"
                );
                assert!(
                    db.reopen().is_some(),
                    "a flush failed, and the session goes on"
                );
                self.told.note(format!("flush!{}", brief(&error)));
            }
        }
        self.after_step();
        self.session_over();
    }

    /// The rounds of the reclaiming of what drops left that its loop
    /// would make.
    fn reclaim(&mut self) {
        if !self.halted {
            return;
        }
        let Some(db) = &self.db else { return };
        let mut rounds = 0;
        loop {
            let round = db.reclaim();
            rounds += 1;
            if !round.more || rounds > 64 {
                let errors = db.stats().background.reclaim_errors;
                self.told
                    .note(format!("reclaim({rounds},w{},e{errors})", round.waiting));
                break;
            }
        }
        self.after_step();
    }

    /// Makes a checkpoint of the database in [`CHECKPOINT`], once.
    fn checkpoint(&mut self) {
        let Some(db) = &self.db else { return };
        if self.checkpointed.is_some() {
            return;
        }
        let state = self.told.state().clone();
        let committer = db.shared.committer();
        match Checkpointer::new(&committer).create(Path::new(CHECKPOINT)) {
            Ok(created) => {
                assert_eq!(created.version.get(), self.told.latest);
                self.told.durable = self.told.latest;
                self.told.note(format!("ckpt@{}", created.version));
                self.checkpointed = Some((state, true));
            }
            Err(error) => {
                // A creation that meets a manifest in doubt before anybody
                // has settled it fails for that, and can be made again
                // (DF8).
                let in_doubt = matches!(
                    &error,
                    CheckpointError::Manifest(SegmentStoreError::ManifestInDoubt { .. })
                );
                if !in_doubt {
                    answered(db, "the creation of a checkpoint", &error);
                }
                self.told.note(format!("ckpt!{}", brief(&error)));
                self.checkpointed = Some((state, false));
            }
        }
        self.after_step();
    }

    /// Ends the session by the steps of [`Db::close`]: the threads end
    /// after the turn they are in, and then the close finishes what it
    /// can. The session is asked whether it has ended between the two.
    /// Asked while its threads run, it can say nothing and its close can
    /// still answer with what stays failed (DF8): a turn of write-back
    /// that is under way meets a refused sync of a segment after the
    /// answer, and a look that finds the commit lock held by the reclaimer
    /// records nothing. Says whether the close did everything a close
    /// does.
    pub(super) fn close(&mut self) -> bool {
        self.resumes();
        let Some(db) = self.db.take() else {
            return false;
        };
        if !self.halted {
            halt_background(&db);
        }
        let over = db.reopen();
        let closed = db.finish();
        match closed {
            Ok(()) => {
                // A close makes the log durable, in a session whose log
                // still can be.
                assert!(
                    over.is_none(),
                    "a session that had ended for writes closed with no error: {over:?}"
                );
                self.told.durable = self.told.latest;
                self.told.note("close");
                true
            }
            Err(error) => {
                assert!(
                    over.is_some() || out_of_space(&error),
                    "a close failed, and not for room, of a session that had not ended: {error}"
                );
                assert!(
                    !matches!(error, DbError::Removed { .. }),
                    "a close of a directory that is the database's: {error}"
                );
                self.told.note(format!("close!{}", brief(&error)));
                false
            }
        }
    }

    /// What holds whenever the session is asked: every reader reads the
    /// latest published version, and the store is sound at rest.
    pub(super) fn check(&mut self, context: &str) {
        let Some(db) = &self.db else { return };
        check_reads(db, self.told.state(), context);
        if self.halted {
            sound(db, context);
        }
    }

    /// Nothing of the background work fails on a device that has refused
    /// nothing: no turn of write-back, and no call of a reclaimer.
    fn untroubled(&self) {
        let Some(db) = &self.db else { return };
        let background = db.stats().background;
        if self.fs.stats().faults > 0 {
            return;
        }
        assert_eq!(
            (background.write_back_errors, background.reclaim_errors),
            (0, 0),
            "background work failed on a device that refused nothing: {:?}",
            background.last_error
        );
    }

    /// What is looked at at the end of every step: the background work,
    /// and, once, after the device first refused an operation in the step,
    /// the session that met the refusal, before anything ends it: what its
    /// readers read, the store as it is, and what a power cut leaves now.
    /// Where the device refused that one operation, it has room again, and
    /// the session is asked whether it resumes.
    fn after_step(&mut self) {
        self.untroubled();
        if self.halted && !self.cut_seen && self.fs.stats().faults > 0 {
            self.cut_seen = true;
            self.check("when the device filled");
            self.check_cut("when the device filled");
            if self.mode == Some(Mode::Once) {
                self.resumes();
            }
        }
    }

    /// The session that met the refusal, on a device with room: it has
    /// said that it has ended, or it takes a commit now (D-151). It is
    /// asked once, before it is closed. A device that stays full has room
    /// for this commit, and is full again afterwards.
    fn resumes(&mut self) {
        if self.mode.is_none() || self.asked || !self.cut_seen {
            return;
        }
        let Some(db) = &self.db else { return };
        self.asked = true;
        if db.reopen().is_some() {
            return;
        }
        self.fs.clear_rules();
        let before = self.told.latest;
        self.write("resumes", next, false);
        assert!(
            self.db.is_some() && self.told.latest == before + 1,
            "with room again a commit is not taken, by the session that met the refusal and had \
             not said that it had ended"
        );
        self.seen.resumed = true;
        if let Some(mode @ (Mode::Full | Mode::FullButSyncs)) = self.mode {
            self.fs.add_rule(mode.rule(0));
        }
    }

    /// The power is cut now: whatever that leaves opens at a version that
    /// the answers allow, with the records of that version, and is sound.
    /// Once the device is still full when the power is back: the open is
    /// refused for room, or made, and the open after it lands on the
    /// version that the same cut opens at with nothing refused, either
    /// way.
    ///
    /// Whatever a cut leaves opens: of a database that was being made as
    /// well, at any operation of the open that makes it (DF4).
    pub(super) fn check_cut(&mut self, context: &str) {
        let image = self.fs.crash_image();
        let plans = [
            image.lose_all(),
            image.keep_all(),
            image.sample(1),
            image.sample(2),
        ];
        for (index, plan) in plans.iter().enumerate() {
            let context = format!("{context}, a power cut that leaves plan {index}");
            let recover = || image.recover(plan).expect("the plan fits its image");
            let on = |fs: &SimVfs| Options {
                vfs: Arc::new(fs.clone()),
                ..self.options.clone()
            };
            let fs = recover();
            let options = on(&fs);
            let mut with_room = None;
            if index == 0 {
                // The version that this cut opens at with nothing refused,
                // in a second recovery of it.
                let apart = recover();
                let reference = open_after_a_cut(&on(&apart), &context).version();
                with_room = Some(reference);
                fs.add_rule(Mode::Full.rule(0));
                match open_at_rest(DIR, &options) {
                    Ok(db) => {
                        let landed = db.version();
                        assert_eq!(landed, reference, "{context}: an open on the full device");
                    }
                    Err(error) => assert!(out_of_space(&error), "{context}: {error}"),
                }
                fs.clear_rules();
            }
            let db = open_after_a_cut(&options, &context);
            assert!(
                with_room.is_none_or(|reference| reference == db.version()),
                "{context}: after an open on the full device the database opens at {}, and with \
                 nothing refused at {with_room:?}",
                db.version()
            );
            let state = self.told.allowed(db.version().get(), true, &context);
            check_reads(&db, state, &context);
            sound(&db, &context);
        }
    }

    /// A checkpoint that was made is the database of its version, by a
    /// backup that is copied from it and opened. What a creation that was
    /// refused left is no database, or the whole checkpoint.
    fn check_checkpoint(&self, context: &str) {
        let Some((state, made)) = &self.checkpointed else {
            return;
        };
        let left = read_manifest(&self.fs, Path::new(CHECKPOINT));
        assert!(
            left.is_ok() || !made,
            "{context}: the checkpoint has no manifest"
        );
        if left.is_err() {
            let options = Options {
                create_if_missing: false,
                ..self.options.clone()
            };
            let opened = open_at_rest(CHECKPOINT, &options);
            assert!(
                matches!(
                    opened,
                    Err(OpenError::NoDatabase { .. } | OpenError::NoManifest { .. })
                ),
                "{context}: what a refused creation left: {:?}",
                opened.err()
            );
            return;
        }
        let elsewhere = SimVfs::new(SimConfig::new(77));
        let copied = Backup::between(Arc::new(self.fs.clone()), Arc::new(elsewhere.clone()))
            .copy(Path::new(CHECKPOINT), Path::new("/backup"))
            .unwrap_or_else(|error| panic!("{context}: the copy of the checkpoint: {error}"));
        assert_eq!(copied.version.get(), state.version(), "{context}");
        let options = Options {
            vfs: Arc::new(elsewhere),
            create_if_missing: false,
            ..self.options.clone()
        };
        let db =
            open_at_rest("/backup", &options).unwrap_or_else(|error| panic!("{context}: {error}"));
        let context = format!("{context}, the backup of the checkpoint");
        check_reads(&db, state, &context);
        sound(&db, &context);
    }
}

/// Opens what a power cut left, in the file system of `options`, with
/// neither of its threads.
fn open_after_a_cut(options: &Options, context: &str) -> Db {
    open_at_rest(DIR, options).unwrap_or_else(|error| panic!("{context}: {error}"))
}

/// The session of the sweep. Each write is of the database as the session
/// has it by then: one that an earlier refusal left without what it was to
/// change makes what it can.
fn script(session: &mut Session) {
    session.open();
    session.ticks("t0");
    session.write("w1", next, false);
    session.ticks("t1");
    session.write(
        "w2",
        |state| {
            if !state.has(1) {
                return first(state);
            }
            // Values in cells and out of line are written again, and a
            // flat record with them.
            let one = state.merk(1, |items| {
                for number in 10..15 {
                    items.insert(key(b'a', number), value(number, 2, 30));
                }
                for number in 30..33 {
                    items.remove(&key(b'a', number));
                }
            });
            let two = state.merk(2, |items| {
                for number in 3..5 {
                    items.insert(key(b'b', number), value(number, 2, 3000));
                }
            });
            let note = put(model::unit(Column::Aux, 1), b"note", &value(7, 2, 3000));
            batch(vec![one, two], vec![note, height(2)], Vec::new())
        },
        false,
    );
    session.ticks("t2");
    session.flush();
    session.write(
        "w3",
        |state| {
            if !state.has(1) {
                return first(state);
            }
            // A hosted Merk grows, in a transaction.
            let merks = hosted(
                state,
                3,
                |_| {},
                |items| {
                    for number in 5..25 {
                        items.insert(key(b'c', number), value(number, 3, 600));
                    }
                },
            );
            batch(merks, vec![height(3)], Vec::new())
        },
        true,
    );
    session.write(
        "w4",
        |state| {
            if !state.has(1) {
                return first(state);
            }
            // The Merk 2 goes, with its values out of line, and the Merk 4
            // comes.
            let four = state.merk(4, |items| {
                for number in 0..6 {
                    items.insert(key(b'd', number), value(number, 4, 1500));
                }
            });
            let mut subtrees = Vec::new();
            if state.has(2) {
                subtrees.push(OwnedSubtree::Drop(model::subtree(2)));
            }
            if !state.has(4) {
                subtrees.push(OwnedSubtree::Enter(model::subtree(4), None));
            }
            batch(vec![four], vec![height(4)], subtrees)
        },
        false,
    );
    session.reclaim();
    session.ticks("t4");
    session.checkpoint();
    session.write(
        "w5",
        |state| {
            if !state.has(1) {
                return first(state);
            }
            let four = state.has(4).then(|| {
                state.merk(4, |items| {
                    for number in 0..4 {
                        items.insert(key(b'd', number), value(number, 5, 2500));
                    }
                    items.remove(&key(b'd', 5));
                })
            });
            batch(four.into_iter().collect(), vec![height(5)], Vec::new())
        },
        false,
    );
    session.ticks("t5");
    session.close();
    session.open();
    session.ticks("t6");
    session.write(
        "w6",
        |state| {
            if !state.has(1) {
                return first(state);
            }
            if state.has(5) {
                return batch(Vec::new(), vec![height(6)], Vec::new());
            }
            let five = state.merk(5, |items| {
                for number in 0..6 {
                    items.insert(key(b'e', number), value(number, 6, 1500));
                }
            });
            batch(
                vec![five],
                vec![height(6)],
                vec![OwnedSubtree::Enter(model::subtree(5), None)],
            )
        },
        false,
    );
    session.ticks("t7");
    session.wipe();
    session.reclaim();
    session.ticks("t8");
    session.write("w8", next, false);
    session.ticks("t9");
}

/// One file operation of a run, as far as it has to be the same in every
/// run: its kind, its path and its length, and whether it was refused.
type Change = (OpKind, String, u64, bool);

/// The operations of a run that can change a file or a name, or make one
/// durable: every one that a device can refuse for room is among them.
fn changes(fs: &SimVfs) -> Vec<Change> {
    let changing = OpSet::WRITES | OpSet::SYNCS | OpSet::NAMES;
    let ops = fs.trace().into_iter().filter_map(|event| match event {
        Event::Op(op) if changing.contains(op.kind) => {
            let refused = matches!(op.outcome, OpOutcome::Injected { .. });
            Some((op.kind, op.path, op.len, refused))
        }
        _ => None,
    });
    ops.collect()
}

/// The kind of file an operation was on, for the count of what a sweep
/// saw.
fn place(path: &str) -> &'static str {
    match path {
        _ if path.starts_with(CHECKPOINT) => "the checkpoint",
        _ if path.ends_with("LOCK") => "the lock file",
        _ if path.contains("MANIFEST") => "the manifest",
        _ if path.contains("/wal/") => "a log file",
        _ if path.contains("/seg/") => "a segment",
        _ if path.contains("/val/") => "a value-log file",
        _ if path.contains("/table/") => "a table checkpoint",
        _ => "a directory",
    }
}

/// Everything that is checked of one run once its script has ended with
/// the device refused: with the device as it is, with room again, and
/// after the database was opened again. Returns what was refused.
fn epilogue(session: &mut Session, baseline: &[Change]) -> String {
    // Up to the operation that was refused, the run made the operations
    // that the run with no fault made: no thread ran in its sessions, so
    // nothing but the script and the device decided what it did. The
    // operation that was refused is then the one at its place in that
    // run, and the sweep refuses each operation of that run that its
    // device can refuse in one run.
    let made = changes(&session.fs);
    let refused = made
        .iter()
        .position(|(.., refused)| *refused)
        .expect("an operation was refused");
    let same =
        |one: &Change, other: &Change| (&one.0, &one.1, one.2) == (&other.0, &other.1, other.2);
    let differs =
        (0..=refused).find(|&at| baseline.get(at).is_none_or(|other| !same(&made[at], other)));
    if let Some(at) = differs {
        panic!(
            "the run did not come to its refusal, at its file operation {refused}, by the \
             operations of the run with no fault: its operation {at} was {:?}, and there {:?}",
            made[at],
            baseline.get(at)
        );
    }
    let (kind, path, ..) = &made[refused];
    let renames_manifest =
        |(kind, path, ..): &Change| *kind == OpKind::Rename && place(path) == "the manifest";
    let after_rename =
        *kind == OpKind::SyncDir && refused > 0 && renames_manifest(&made[refused - 1]);
    let what = match after_rename {
        true => "sync_dir after the manifest's rename".to_string(),
        false => format!("{} of {}", kind, place(path)),
    };
    // A replacement of the manifest that the device refused once ends no
    // session (D-157): the next commit settles it, or write-back does.
    let settled =
        (renames_manifest(&made[refused]) || after_rename) && session.fs.stats().faults == 1;

    // The device is still full, if it is.
    session.check("with the device as the script left it");
    session.check_checkpoint("with the device as the script left it");
    session.check_cut("with the device as the script left it");

    // There is room again. One of two holds, and the session says which
    // (D-151): it takes the next commit, or it has said that it has ended,
    // and the session after it takes the next commit. A session that was
    // refused its open, or that its caller had closed, is the second.
    session.fs.clear_rules();
    session.told.note("|room|");
    match session.db.as_ref().map(Db::reopen) {
        Some(None) => {
            session.seen.went_on = true;
            session.asked = true;
            session.told.note("goes-on");
        }
        Some(Some(reason)) => {
            session.seen.over = true;
            session.seen.reopened = true;
            session.told.note(match reason {
                Reopen::NotDurable { .. } => "reopen:log-sync",
                Reopen::SegmentsNotDurable => "reopen:segment-sync",
                Reopen::Stopped(_) => "reopen:stopped",
            });
            session.close();
        }
        None => session.seen.reopened = true,
    }
    session.seen.settled = settled;
    assert!(
        !(settled && session.seen.over),
        "a session ended for a replacement of the manifest that was refused once"
    );
    let stayed = session.db.is_some();
    // An open settles a commit that was left unsettled: the commit that
    // is asked for now is the one after what the open found.
    assert!(session.open(), "with room again the database does not open");
    let before = session.told.latest;
    session.write("w9", next, false);
    assert!(
        session.db.is_some() && session.told.latest == before + 1,
        "with room again a commit is not taken, by a session that {}",
        match stayed {
            true => "had not said that it had ended",
            false => "was opened for it",
        }
    );
    // Write-back, a flush and a look at a power cut, in the session that
    // took the commit.
    assert!(!session.ticks("t10"), "write-back fails with room");
    session.flush();
    assert_eq!(
        session.told.durable, session.told.latest,
        "a flush with room"
    );
    session.check("with room again");
    session.check_cut("with room again");

    // The database closes and opens again as what it was, write-back
    // works, and the database is written and made durable once more.
    assert!(session.close(), "a close with room");
    assert!(session.open(), "the database does not open again");
    session.check("opened again");
    session.reclaim();
    assert!(!session.ticks("t12"), "write-back fails with room");
    let before = session.told.latest;
    session.write("w10", next, false);
    assert_eq!(
        session.told.latest,
        before + 1,
        "the last write is not taken"
    );
    session.flush();
    assert_eq!(session.told.durable, session.told.latest, "the last flush");
    session.check("at the end");
    assert!(session.close(), "the last close");
    assert_eq!(session.told.durable, session.told.latest, "the last close");
    what
}

/// The seed of the device of every run of a sweep.
const SEED: u64 = 11;

/// One sweep: how the device is made full, what the session asks of its
/// commits, and what the device and write-back do after a refusal.
#[derive(Debug, Clone, Copy)]
struct Sweep {
    mode: Mode,
    sync: SyncPolicy,
    /// What the device does to what a failed sync left unknown, once a
    /// later sync succeeds.
    fate: UnknownFate,
    /// [`Session::loop_first`].
    loop_first: bool,
}

impl std::fmt::Display for Sweep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}, {:?}, {:?}", self.mode, self.sync, self.fate)?;
        match self.loop_first {
            true => f.write_str(", write-back first"),
            false => Ok(()),
        }
    }
}

/// What a run of the script with the device full found.
struct Ran {
    /// What was refused.
    what: String,
    seen: Seen,
}

impl Sweep {
    /// A sweep on the device as the simulator has it by default, where
    /// the session is asked before write-back makes another turn.
    fn of(mode: Mode, sync: SyncPolicy) -> Self {
        Self {
            mode,
            sync,
            fate: SimConfig::new(SEED).unknown,
            loop_first: false,
        }
    }

    fn session(self) -> Session {
        let mut session = Session::fated(SEED, self.sync, true, self.fate);
        session.loop_first = self.loop_first;
        session.mode = Some(self.mode);
        session
    }

    /// One run of the script with the device full from the operation
    /// after the first `skip` that can need room. `None` if the script
    /// makes no more than `skip` of them. Otherwise what the run found, or
    /// what did not hold.
    fn refuse(self, skip: u64, baseline: &[Change]) -> Option<Result<Ran, String>> {
        let mut session = self.session();
        session.fs.add_rule(self.mode.rule(skip));
        let ended = catch_unwind(AssertUnwindSafe(|| {
            script(&mut session);
            (session.fs.stats().faults > 0).then(|| epilogue(&mut session, baseline))
        }));
        let notes = session.told.log.join(" ");
        match ended {
            Ok(what) => Some(Ok(Ran {
                what: what?,
                seen: session.seen,
            })),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| text.to_string()))
                    .unwrap_or_else(|| "a panic".to_string());
                Some(Err(format!(
                    "{self}, operation {skip} of those that can need room: {message}\n    {notes}"
                )))
            }
        }
    }

    /// Runs the script once with no fault and then once for each operation
    /// that can need room, and returns how many were refused.
    fn run(self) -> u64 {
        let no_fault = || {
            let mut session = self.session();
            script(&mut session);
            session.check("with no fault");
            session.check_checkpoint("with no fault");
            assert_eq!(session.told.latest, 9, "the script's commits");
            let stats = session.fs.stats();
            let said = format!(
                "{self}: the script makes {} file operations, {} of them syncs: {}",
                stats.ops,
                stats.syncs,
                session.told.log.join(" ")
            );
            (changes(&session.fs), said)
        };
        // The run with no fault is the measure of every other. The
        // script alone decides what it does, so a second one makes the
        // same operations.
        let (baseline, said) = no_fault();
        let (again, _) = no_fault();
        let longer = baseline.len().max(again.len());
        if let Some(at) = (0..longer).find(|&at| baseline.get(at) != again.get(at)) {
            panic!(
                "{self}: two runs with no fault differ at their file operation {at}: {:?} and {:?}",
                baseline.get(at),
                again.get(at)
            );
        }
        eprintln!("{said}");
        let mut failures = Vec::new();
        let mut seen: BTreeMap<String, [u64; 9]> = BTreeMap::new();
        let mut skip = 0;
        while let Some(ended) = self.refuse(skip, &baseline) {
            match ended {
                Ok(ran) => {
                    let saw = ran.seen;
                    let counts = seen.entry(ran.what).or_default();
                    let flags = [
                        true,
                        saw.failed,
                        saw.unsettled,
                        saw.over,
                        saw.open_refused,
                        saw.went_on,
                        saw.reopened,
                        saw.settled,
                        saw.resumed,
                    ];
                    for (count, flag) in counts.iter_mut().zip(flags) {
                        *count += u64::from(flag);
                    }
                }
                Err(failure) => failures.push(failure),
            }
            skip += 1;
        }
        eprintln!(
            "{self}: {skip} operations were refused for room. For each kind: the runs, and those \
             in which a commit failed or waited for the cut, a commit was unsettled, a session \
             said that it had ended, an open was refused, the session took the next commit when \
             there was room, the next commit was taken after the database was opened again, \
             the refusal was of a replacement of the manifest and nothing else was refused, \
             the session that met the refusal took a commit as soon as there was room:"
        );
        let mut all = [0; 9];
        let row = |what: &str, counts: &[u64; 9]| {
            let counts: Vec<String> = counts.iter().map(|count| format!("{count:4}")).collect();
            eprintln!("  {what:36} {}", counts.join(" "));
        };
        for (what, counts) in &seen {
            row(what, counts);
            for (sum, count) in all.iter_mut().zip(counts) {
                *sum += count;
            }
        }
        row("all", &all);
        // A sweep that refuses one operation meets the replacements of the
        // manifest among them: the check that none of them ends a session
        // was made of something.
        assert!(
            self.mode != Mode::Once || all[7] > 0,
            "{self}: no run refused a replacement of the manifest"
        );
        // The session that met a refusal was asked with room, and not
        // only the one that was open in the end.
        assert!(all[8] > 0, "{self}: no session resumed where it was");
        assert!(
            failures.is_empty(),
            "{self}: {} of {skip} runs did not hold what they should. The first:\n{}",
            failures.len(),
            failures
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        skip
    }
}

/// The acceptance test of OPS-08: simulated ENOSPC at every write point
/// never corrupts the database. One operation is refused in each run.
#[test]
fn a_device_that_refuses_one_operation_for_room_never_corrupts_the_database() {
    let refused = Sweep::of(Mode::Once, SyncPolicy::Never).run();
    assert!(refused > 200, "{refused} operations");
}

/// The same, with the device full from the operation on, syncs included,
/// until the script has ended.
#[test]
fn a_device_that_stays_full_never_corrupts_the_database() {
    let refused = Sweep::of(Mode::Full, SyncPolicy::Never).run();
    assert!(refused > 200, "{refused} operations");
}

/// The same, with syncs that succeed on the full device.
#[test]
fn a_device_that_stays_full_and_still_syncs_never_corrupts_the_database() {
    let refused = Sweep::of(Mode::FullButSyncs, SyncPolicy::Never).run();
    assert!(refused > 100, "{refused} operations");
}

/// The same on a device that loses everything a refused sync had not
/// made durable, although a later sync of the same file succeeds: a sync
/// that failed cannot be made good (`lignin_io::vfs`).
#[test]
fn a_device_that_loses_what_a_refused_sync_covered_never_corrupts_the_database() {
    let lost = |mode, sync| Sweep {
        fate: UnknownFate::Lost,
        ..Sweep::of(mode, sync)
    };
    let once = lost(Mode::Once, SyncPolicy::Never).run();
    let full = lost(Mode::Full, SyncPolicy::EveryCommit).run();
    assert!(once > 200 && full > 200, "{once} and {full} operations");
}

/// The same where write-back's loop makes its next turn before the
/// session makes its next commit. A replacement of the manifest that did
/// not finish is settled by whichever of the two comes first (DF8a): in
/// the other sweeps that is the commit by which the session is asked
/// whether it resumes, and here it is write-back, which opens the
/// manifest again where it next needs it (`lignin_store::flush`). On a
/// device that loses what the refused sync of the directory covered, and
/// on one that keeps it.
#[test]
fn a_device_whose_refusals_write_back_meets_again_first_never_corrupts_the_database() {
    let first = |fate| Sweep {
        fate,
        loop_first: true,
        ..Sweep::of(Mode::Once, SyncPolicy::Never)
    };
    let lost = first(UnknownFate::Lost).run();
    let kept = first(UnknownFate::Kept).run();
    assert!(lost > 200 && kept > 200, "{lost} and {kept} operations");
}

/// The same under a policy that syncs the log at every commit, where a
/// commit can be published and not durable (CM5).
#[test]
fn a_device_that_fills_under_commits_that_sync_never_corrupts_the_database() {
    let once = Sweep::of(Mode::Once, SyncPolicy::EveryCommit).run();
    let full = Sweep::of(Mode::Full, SyncPolicy::EveryCommit).run();
    assert!(once > 200 && full > 200, "{once} and {full} operations");
}
