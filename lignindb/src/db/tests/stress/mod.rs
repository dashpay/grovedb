//! Readers, parallel writers, write-back, the reclaimer, the cleaner and
//! checkpoints of one database at once, with a model that knows what every
//! version holds (VER-05; `transactions.md` section 15.2, "Combined
//! scenarios for the stress test").
//!
//! # What runs
//!
//! One open database ([`Db`]), with its two background threads, and
//! beside them:
//!
//! | Thread | What it does | Rows of section 13 |
//! | --- | --- | --- |
//! | the block writer | One write transaction after another. Each has two scopes that are open at once, one of them on a second thread for as long as the scopes last; now and then a scope that is rolled back, a scope that is refused for an overlap, a savepoint that is rolled back to, and a rollback of the whole transaction. It writes Merks of its own, generations of subtrees that it enters and drops, and keys that the other writer writes too. | 7a, 8a, 8b, 9, 10, 12 to 14 |
//! | the writer with no transaction | One batch after another, each a commit ([`Db::write`]), beside the open block transaction. Every few commits it makes a checkpoint, opens it as a database, reads all of it and deletes it. | 7b, 8b, 12, 15, 18, 20 |
//! | the reader | Snapshots of three ages, and operations that pin nothing. Each look compares whole subtrees with the model. Every few looks it has the log made durable ([`Db::flush`]), and the physical invariant checker reads the latest version as a reader, with the reclaimer of dropped subtrees held back from the moment the version is pinned to the end of that check. | 1 to 6, and `flush()` beside a commit (DU1a) |
//! | the cleaner | [`Cleaner::run`], as the engine will run it once it is wired into [`Db`]: the segments, and the value log with the owners of values of both kinds. | 5, 10, 14, 17, 19, 20 |
//! | write-back, the reclaimer | The engine's own threads, with small files and low bounds, so that committers are slowed (FL5), table checkpoints are taken and log files go all the time. | 4, 9, 13, 16, 17 |
//!
//! That is four threads of the test, the second scope's among them, and
//! three of background work.
//!
//! # What is checked
//!
//! - **Every read.** [`History`] holds *S(v)* for every version a reader
//!   can ask at. A look reads a subtree whole, through a snapshot or an
//!   operation, and compares: whether it is entered and, where it is,
//!   every node with its value, hashes and links, and every record of the
//!   other columns. A subtree that is not entered is not opened, so
//!   nothing of it is read: a record that a drop left under it is the
//!   checker's to find.
//! - **Every commit outcome** (CF2). A writer notes the latest version
//!   before and after each operation that writes, and before its commit.
//!   The model then says whether a commit outside wrote one of its keys
//!   after its first write: surely (the foreign commit was published
//!   after the operation ended and before the commit began), surely not
//!   (none was published after the operation began), or either (one was
//!   published while the operation ran). A commit that is published
//!   where a conflict is sure, and a conflict where none is possible,
//!   fail the test. Every few blocks a conflict is made sure by
//!   construction: the block writer waits, its write made, until the
//!   other writer has committed the same key.
//! - **Every checkpoint** is opened and holds exactly one version, whole.
//! - **The background work reports no error**: write-back, the reclaimer
//!   and the cleaner. No reclaiming is given up.
//! - **At the end of each epoch** the threads are stopped, everything that
//!   was dropped is reclaimed, and the checker reads the store at rest
//!   ([`Scope::Still`]): the pages, the trees, the page ids, the counts of
//!   live values, the page table against its segments. Then the database
//!   is closed and opened again, and reads what the model has.
//!
//! # How it is run
//!
//! [`a_short_stress`] runs with the crate's tests: two epochs that end by
//! counts, so that it takes seconds, and on any machine its block writer
//! writes as many blocks and each other thread does its least.
//! [`an_hour_of_stress`] is the long form, ignored unless it is asked for:
//!
//! ```text
//! LIGNIN_STRESS_SECONDS=3600 cargo test -p lignindb --release --lib \
//!     db::tests::stress::an_hour_of_stress -- --ignored --nocapture
//! ```
//!
//! `LIGNIN_STRESS_SEED` names the seed of a run, which is printed, and
//! `LIGNIN_STRESS_EPOCH_SECONDS` the length of an epoch. The schedule of
//! the threads is the machine's, so a seed names the workload and not the
//! interleaving.
//!
//! A run holds several hundred files open at once: the segments and the
//! value-log files of the database, which this test keeps small, and as
//! many again of a checkpoint that is opened beside it. That is more than
//! the 256 files a process may open by default on macOS. A run that may
//! not open as many files as it wants ([`Until::open_files`]) fails at its
//! start and says so. `.github/scripts/lignin-ci.sh` raises the limit for
//! the commands that run this test.

pub(super) mod history;
pub(super) mod work;

use std::{
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
};

use lignin_format::{CommitVersion, PageId};
use lignin_store::{
    cleaner::{BothOwners, Cleaner, CleanerConfig, CleanerState, CleanerStats},
    flush::FlushConfig,
};
use lignin_tree::{
    flat::{FlatOwners, FlatTree},
    value::TreeOwners,
};

use self::{
    history::History,
    work::{joined, shared, Expected, Piece, Read, Writer},
};
use super::*;
use crate::{
    checkpoint::{self, Checkpointer},
    db::{store::lock, WriteError},
    snapshot::{LatestLease, Snapshot},
    txn::{
        tests::{
            model::{OwnedBatch, Val, Write},
            store::{mix, Rng, Rules},
        },
        TxnError, Unit, WriteTxn,
    },
    verify::{self, Grove, Report, Scope},
};

/// The keys that both writers write.
const CONTESTED: usize = 2;
/// Every so many blocks the block writer waits for the other writer to
/// commit a key it has written, so that its commit must fail.
const FORCED_EVERY: u64 = 8;
/// The generations a writer keeps entered before it drops the oldest.
const GENERATIONS: usize = 3;
/// Every so many commits the writer with no transaction makes a
/// checkpoint.
const CHECKPOINT_EVERY: u64 = 12;
/// Every so many looks the reader runs the checker on a pinned version.
const VERIFY_EVERY: u64 = 40;
/// Every so many looks the reader has the log made durable.
const FLUSH_EVERY: u64 = 8;
/// The blocks an epoch that ends by the clock writes at least: as many as
/// the short form writes in all.
const LEAST_BLOCKS: u64 = 96;
/// How many commits a snapshot of the reader is held for, at least, by
/// its age.
const AGES: [u64; 3] = [1, 8, 40];

/// How long an epoch runs.
#[derive(Debug, Clone, Copy)]
enum Until {
    /// So many blocks of the block writer, and then until the other
    /// threads have each done their least.
    Blocks(u64),
    /// So long, and no fewer than [`LEAST_BLOCKS`] blocks, and then until
    /// the other threads have each done their least.
    Elapsed(Duration),
}

impl Until {
    /// How many open files a run wants room for: well above what runs
    /// were measured to hold at once, which was under 300 for the short
    /// form and under 600 for the long one.
    fn open_files(self) -> u64 {
        match self {
            Self::Blocks(_) => 512,
            Self::Elapsed(_) => 2_048,
        }
    }
}

/// How many files this process may hold open at once, as a shell that it
/// starts says: the limit is inherited. `None` where no shell answers
/// with a number, and nothing is checked then.
fn open_files_limit() -> Option<u64> {
    let shell = std::process::Command::new("sh")
        .args(["-c", "ulimit -n"])
        .output()
        .ok()?;
    String::from_utf8(shell.stdout).ok()?.trim().parse().ok()
}

/// Fails, with what to do about it, if this process may not hold `wanted`
/// files open at once: a run that meets the limit halfway fails in
/// whichever thread opens a file next, with an error of the file system.
fn may_open(wanted: u64) {
    if let Some(limit) = open_files_limit() {
        assert!(
            limit >= wanted,
            "the stress test wants room for {wanted} open files, and this process may open \
             {limit}: raise the limit of open files (`ulimit -n 4096`), as \
             `.github/scripts/lignin-ci.sh` does"
        );
    }
}

/// What one run of the stress test is given.
#[derive(Debug, Clone, Copy)]
struct Config {
    seed: u64,
    epochs: u64,
    until: Until,
    /// Whether the files are synced where the engine asks for it.
    syncs: bool,
}

/// What the threads of a run did, in counts.
#[derive(Debug, Default)]
struct Tally {
    blocks: AtomicU64,
    published: AtomicU64,
    /// Commits that failed with a conflict that the model says was sure,
    /// and those that it says could have gone either way.
    sure_conflicts: AtomicU64,
    open_conflicts: AtomicU64,
    /// Commits that were published where a conflict could have been.
    open_published: AtomicU64,
    forced: AtomicU64,
    forced_missed: AtomicU64,
    rollbacks: AtomicU64,
    scope_rollbacks: AtomicU64,
    savepoints: AtomicU64,
    overlaps: AtomicU64,
    outside: AtomicU64,
    outside_conflicts: AtomicU64,
    checkpoints: AtomicU64,
    looks: AtomicU64,
    looks_behind: AtomicU64,
    looks_unpinned: AtomicU64,
    verifies: AtomicU64,
    flushes: AtomicU64,
    nodes: AtomicU64,
    records: AtomicU64,
    absent: AtomicU64,
    ticks: AtomicU64,
}

fn add(count: &AtomicU64, by: u64) {
    count.fetch_add(by, Ordering::Relaxed);
}

fn get(count: &AtomicU64) -> u64 {
    count.load(Ordering::Relaxed)
}

/// The word between the two writers by which a conflict is made sure.
#[derive(Debug, Default)]
struct Forced {
    /// The contested key the block writer has written and waits on, plus
    /// one, or zero.
    asked: AtomicU64,
    /// The version of the other writer's commit of that key, or zero.
    answered: AtomicU64,
}

/// The versions that were the latest before and after the operation that
/// made a write.
struct Made {
    write: Write,
    before: u64,
    after: u64,
}

/// What the threads of one epoch share.
struct Run<'a> {
    db: &'a Db,
    history: &'a History,
    tally: &'a Tally,
    /// Set when the epoch ends, and when a thread of the test fails.
    stop: &'a AtomicBool,
    forced: &'a Forced,
    seed: u64,
}

impl Run<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn version(&self) -> u64 {
        self.db.version().get()
    }
}

/// Options for a database that is small in everything: write-back is
/// behind its bounds all the time, files are closed after a few commits,
/// and the pool holds less than the database.
pub(super) fn stress_options(syncs: bool, pool: usize) -> Options {
    let size = PageSize::MIN;
    Options {
        page_size: Some(size),
        cache_size: pool,
        log_file_len: Some(48 << 10),
        flush: Some(FlushConfig {
            max_unwritten: 64,
            max_pending_bytes: 96 << 10,
            max_log_held: 192 << 10,
            checkpoint_log_bytes: 64 << 10,
            segment_len: 128 << 10,
            idle: Duration::from_millis(1),
            ..FlushConfig::new(size)
        }),
        idle: Duration::from_millis(2),
        vfs: match syncs {
            true => Arc::new(StdVfs),
            false => Arc::new(crate::txn::tests::store::NoSync),
        },
        ..Options::default()
    }
}

/// Runs the physical invariant checker on the version `at` of `db`, with
/// the hashes recomputed by the rules the model makes them by. The caller
/// holds what `scope` asks for (`crate::verify`).
pub(super) fn verify(db: &Db, scope: Scope, at: CommitVersion) -> Report {
    let store = &*db.shared.store;
    let catalog = store.catalog();
    let columns = catalog.keyspaces.columns();
    let other = [FlatTree::open(PageId::MIN, store.config())];
    let parts = verify::Store {
        loader: store.loader(),
        table: &store.table,
        segments: &store.segments,
        values: &store.values,
        page_size: store.page_size,
        ids: Some(&store.ids),
        grove: Grove::Directory {
            tree: catalog.directory,
            columns: &columns,
        },
        flat_drops: Some(catalog.flat_drops),
        flat: &other,
        values_counted: true,
    };
    let guard = db.shared.versions.guard();
    let calls = AtomicU64::new(0);
    verify::check(
        &parts,
        &guard,
        verify::Options::new(at, scope),
        Some(&mut Rules(&calls)),
    )
}

fn sound(report: &Report, what: &str) {
    assert!(
        report.findings.is_empty() && report.more_findings == 0,
        "{what}:\n{report}"
    );
}

/// Reads every subtree the history has heard of through a snapshot of the
/// latest version of `db`, and compares each with the history.
pub(super) fn compare_all(db: &Db, history: &History) -> Read {
    let snapshot = db.snapshot();
    compare_at(db, history, &snapshot.operation())
}

/// The same at the version of `operation`.
pub(super) fn compare_at(db: &Db, history: &History, operation: &Operation<'_>) -> Read {
    let at = operation.at().get();
    let state = history.through(at);
    let mut subtrees = state.subtrees();
    subtrees.push(shared());
    let expected: Vec<Expected> = (subtrees.into_iter())
        .map(|subtree| Expected::of(&state, subtree, at))
        .collect();
    drop(state);
    let mut read = Read::default();
    for expected in &expected {
        read += expected.check(db, operation);
    }
    read
}

/// Commits `batch` with no transaction, and tells the model.
pub(super) fn record(db: &Db, history: &History, batch: &OwnedBatch) {
    let committed = batch.with(|batch| db.write(batch, &mut NoStaleHashes));
    let Ok(Committed::Published(published)) = committed else {
        panic!("a commit with no transaction: {committed:?}");
    };
    history.commit(published.version.get(), batch.writes());
}

/// A commit that writes one record, so that the version moves on: a page
/// that the latest commit wrote is freed after the next one.
pub(super) fn tick(db: &Db, history: &History, round: u32) {
    let mut batch = OwnedBatch::default();
    let unit = Unit::new(Column::Meta, shared());
    let value = Val::Flat(round.to_be_bytes().to_vec());
    batch
        .records
        .push(Write::Put(unit, b"tick".to_vec(), value));
    record(db, history, &batch);
}

/// Applies `pieces` through one scope of `txn`, which is then merged, or
/// rolled back if `rolls_back`. With `refused`, a second scope is opened
/// beside the first once that has written, and its write of the same Merk
/// is refused at once (SC3). Returns the writes that are merged.
fn in_scope(
    run: &Run<'_>,
    txn: &WriteTxn<'_>,
    pieces: &[Piece],
    rolls_back: bool,
    refused: Option<&Piece>,
) -> Vec<Made> {
    let scope = txn.scope().expect("a scope");
    let mut made = Vec::new();
    for piece in pieces {
        let before = run.version();
        let read = piece.batch.through(&scope);
        let applied = read.with(|batch| scope.apply(batch));
        applied.unwrap_or_else(|error| panic!("a batch of a scope: {error}"));
        let after = run.version();
        made.extend(piece.writes.iter().cloned().map(|write| Made {
            write,
            before,
            after,
        }));
    }
    if let Some(piece) = refused {
        let second = txn.scope().expect("a second scope");
        let read = piece.batch.through(&second);
        match read.with(|batch| second.apply(batch)) {
            Err(TxnError::ScopeOverlap { .. }) => add(&run.tally.overlaps, 1),
            other => panic!("a write of a Merk that another open scope holds: {other:?}"),
        }
        second.rollback();
    }
    match rolls_back {
        true => {
            scope.rollback();
            add(&run.tally.scope_rollbacks, 1);
            Vec::new()
        }
        false => {
            scope.merge();
            made
        }
    }
}

/// Holds the outcome of a commit against the conflict rule as the model
/// evaluates it (CF2), and returns the version it was published as.
/// `before` was the latest version before the commit began.
fn judge(
    run: &Run<'_>,
    who: &str,
    made: &[Made],
    before: u64,
    outcome: Result<Committed, WriteError>,
) -> Option<u64> {
    let (published, through) = match &outcome {
        Ok(Committed::Published(published)) => {
            assert!(published.miscounted.is_none(), "{who}: {published:?}");
            let version = published.version.get();
            (Some(version), version - 1)
        }
        Ok(Committed::Empty) => {
            assert!(
                made.is_empty(),
                "{who}: writes were made, and nothing is committed"
            );
            return None;
        }
        Err(error) if error.is_conflict() => (None, run.version()),
        Err(error) => panic!("{who}: a commit: {error}"),
    };
    let state = run.history.through(through);
    let sure = made.iter().find_map(|made| {
        let met = state.met(&made.write, made.after, before.min(through));
        met.first().copied()
    });
    let possible =
        (made.iter()).any(|made| !state.met(&made.write, made.before, through).is_empty());
    drop(state);
    let tally = run.tally;
    match (published, sure) {
        (Some(version), Some(met)) => panic!(
            "{who}: version {version} is published, and the commit of version {met} wrote one of \
             its keys after it first wrote that key"
        ),
        (Some(_), None) => add(&tally.open_published, u64::from(possible)),
        (None, Some(_)) => add(&tally.sure_conflicts, 1),
        (None, None) => {
            assert!(
                possible,
                "{who}: a conflict, and no commit up to version {through} wrote a key of the \
                 transaction after it first wrote that key"
            );
            add(&tally.open_conflicts, 1);
        }
    }
    published
}

/// One block: a write transaction with two scopes that are open at once.
fn block(run: &Run<'_>, writer: &mut Writer, number: u64) {
    let (db, tally) = (run.db, run.tally);
    run.history.hold(history::Slot::Block, || run.version());
    let txn = db.begin();
    let first = number as usize % Writer::MERKS;
    let mut here = vec![writer.documents(first)];
    let slot = writer.below(CONTESTED) as u8;
    let forced = number % FORCED_EVERY == FORCED_EVERY - 1;
    if forced || writer.chance(30) {
        here.push(writer.contested(slot));
    }
    let mut there = vec![match writer.chance(60) {
        true => writer.generation(),
        false => writer.documents((first + 1) % Writer::MERKS),
    }];
    if writer.generations() > GENERATIONS {
        there.extend(writer.drop_oldest());
    }
    let rolls_back = writer.chance(12);
    let refused = (number % 7 == 3).then(|| writer.documents(first));

    let (mut made, beside) = thread::scope(|threads| {
        let (txn, there) = (&txn, &there);
        let beside = threads.spawn(move || in_scope(run, txn, there, rolls_back, None));
        let made = in_scope(run, txn, &here, false, refused.as_ref());
        (made, beside.join().expect("the second scope ends"))
    });
    made.extend(beside);
    let mut pieces = here;
    if !rolls_back {
        pieces.extend(there);
    }

    // A savepoint, a change of a third Merk after it, and the rollback to
    // it: nothing of the change is left (WT8).
    if writer.chance(15) {
        txn.set_savepoint().expect("a savepoint");
        let undone = writer.documents((first + 2) % Writer::MERKS);
        let _ = in_scope(run, &txn, &[undone], false, None);
        txn.rollback_to_savepoint().expect("the savepoint is there");
        assert_eq!(txn.savepoints(), 0);
        add(&tally.savepoints, 1);
    }

    // The transaction reads its own flat writes (RD3).
    let mut value = Vec::new();
    for made in &made {
        let (unit, key, expected) = match &made.write {
            Write::Put(unit, key, Val::Flat(value)) => (*unit, key, Some(value)),
            Write::Delete(unit, key) => (*unit, key, None),
            _ => continue,
        };
        value.clear();
        let found = txn
            .get(unit, key, &mut value)
            .expect("a read of its own write");
        assert_eq!(found.then_some(&value), expected, "{key:?} of {unit}");
    }

    if forced {
        // The other writer commits the key this transaction has written,
        // and says so: this commit then fails for sure.
        run.forced.answered.store(0, Ordering::SeqCst);
        run.forced
            .asked
            .store(u64::from(slot) + 1, Ordering::SeqCst);
        let began = Instant::now();
        while run.forced.answered.load(Ordering::SeqCst) == 0 && !run.stopped() {
            if began.elapsed() > Duration::from_secs(20) {
                add(&tally.forced_missed, 1);
                break;
            }
            thread::yield_now();
        }
        run.forced.asked.store(0, Ordering::SeqCst);
        add(
            &tally.forced,
            u64::from(run.forced.answered.load(Ordering::SeqCst) != 0),
        );
    } else if writer.chance(8) {
        db.rollback(&txn).expect("a rollback");
        add(&tally.rollbacks, 1);
        add(&tally.blocks, 1);
        run.history.release(history::Slot::Block);
        return;
    }

    let before = run.version();
    let outcome = db.commit(&txn, &mut NoStaleHashes);
    if let Some(version) = judge(run, "the block", &made, before, outcome) {
        let writes = pieces.iter().flat_map(|piece| piece.writes.iter().cloned());
        run.history.commit(version, writes.collect());
        writer.committed(pieces);
        add(&tally.published, 1);
    }
    add(&tally.blocks, 1);
    run.history.release(history::Slot::Block);
}

/// The writer with no transaction: one batch after another, each a commit
/// of its own, and a checkpoint every few commits.
fn outside(run: &Run<'_>, writer: &mut Writer, dir: &Path, options: &Options) {
    let (db, tally) = (run.db, run.tally);
    let mut commits = 0u64;
    while !run.stopped() {
        run.history.hold(history::Slot::Outside, || run.version());
        let mut pieces = vec![writer.documents(commits as usize % Writer::MERKS)];
        if writer.chance(35) {
            pieces.push(writer.generation());
        }
        if writer.generations() > GENERATIONS {
            pieces.extend(writer.drop_oldest());
        }
        let asked = run.forced.asked.load(Ordering::SeqCst);
        let answers = asked != 0 && run.forced.answered.load(Ordering::SeqCst) == 0;
        if answers {
            pieces.push(writer.contested((asked - 1) as u8));
        } else if writer.chance(12) {
            let slot = writer.below(CONTESTED) as u8;
            pieces.push(writer.contested(slot));
        }
        let batch = joined(&pieces);
        let before = run.version();
        let outcome = work::write(db, &batch);
        // When its transaction first read and wrote is not looked at: it
        // was not before the call.
        let made: Vec<Made> = (pieces.iter())
            .flat_map(|piece| piece.writes.iter().cloned())
            .map(|write| Made {
                write,
                before,
                after: u64::MAX,
            })
            .collect();
        match judge(run, "the write with no transaction", &made, before, outcome) {
            Some(version) => {
                let writes = made.into_iter().map(|made| made.write).collect();
                run.history.commit(version, writes);
                writer.committed(pieces);
                if answers {
                    run.forced.answered.store(version, Ordering::SeqCst);
                }
                add(&tally.outside, 1);
                commits += 1;
                if commits.is_multiple_of(CHECKPOINT_EVERY) {
                    checkpoint(run, &dir.join(format!("checkpoint-{commits}")), options);
                }
            }
            None => add(&tally.outside_conflicts, 1),
        }
        run.history.release(history::Slot::Outside);
    }
}

/// Makes a checkpoint of the database at `target`, opens it as a database
/// of its own, reads every subtree of the one version it holds, closes it
/// and deletes it (CK1, CK7).
fn checkpoint(run: &Run<'_>, target: &Path, options: &Options) {
    let committer = run.db.shared.committer();
    let created = Checkpointer::new(&committer).create(target);
    let created = created.unwrap_or_else(|error| panic!("a checkpoint: {error}"));
    let opened = Db::open(target, options).unwrap_or_else(|error| {
        // What the checkpoint is made of, for whoever reads the failure.
        let manifest = lignin_store::segment::read_manifest(&*options.vfs, target);
        let files = |part: &str| -> Vec<(String, u64)> {
            let entries = std::fs::read_dir(target.join(part))
                .into_iter()
                .flatten()
                .flatten();
            let mut files: Vec<_> = entries
                .map(|entry| {
                    let len = entry.metadata().map_or(0, |metadata| metadata.len());
                    (entry.file_name().to_string_lossy().into_owned(), len)
                })
                .collect();
            files.sort();
            files
        };
        let log_end = manifest.as_ref().ok().map(|manifest| {
            lignin_store::wal::LogReader::new(options.vfs.clone(), target, manifest).into_end()
        });
        if let Ok(kept) = std::env::var("LIGNIN_STRESS_KEEP") {
            // The directory goes with the test: a copy for whoever asked.
            let copied = std::process::Command::new("cp")
                .args(["-r", "--"])
                .arg(target)
                .arg(&kept)
                .status();
            eprintln!("the checkpoint is copied to {kept}: {copied:?}");
        }
        panic!(
            "the checkpoint is opened: {error}\n{created:?}\nits log files: {:?}\nwhat a reader \
             of its log finds: {log_end:#?}\nits manifest: {manifest:#?}",
            files("wal")
        )
    });
    assert_eq!(
        opened.version(),
        created.version,
        "the version of the checkpoint"
    );
    let read = compare_all(&opened, run.history);
    add(&run.tally.nodes, read.nodes);
    add(&run.tally.records, read.records);
    let background = opened.stats().background;
    assert_eq!(
        (background.write_back_errors, background.reclaim_errors),
        (0, 0),
        "the checkpoint: {background:?}"
    );
    opened.close().expect("the checkpoint is closed");
    checkpoint::delete(&*options.vfs, target).expect("the checkpoint is deleted");
    add(&run.tally.checkpoints, 1);
}

/// The reader: snapshots of three ages and operations that pin nothing,
/// each look a comparison of whole subtrees with the history.
fn reader(run: &Run<'_>) {
    let (db, tally) = (run.db, run.tally);
    let mut rng = Rng::new(mix(run.seed, b"reader"));
    let mut lease = LatestLease::new();
    let mut held: [Option<(Snapshot<'_>, u64)>; AGES.len()] = [None, None, None];
    let mut looks = 0u64;
    while !run.stopped() {
        let latest = run.version();
        for (slot, age) in held.iter_mut().zip(AGES) {
            if slot.as_ref().is_none_or(|(_, until)| latest >= *until) {
                // The older snapshot goes before the newer one is taken.
                *slot = None;
                let snapshot = db.snapshot();
                let until = snapshot.version().get() + age + rng.below(age as usize + 1) as u64;
                *slot = Some((snapshot, until));
            }
        }
        let oldest = held
            .iter()
            .flatten()
            .map(|(snapshot, _)| snapshot.version());
        let oldest = oldest.min().expect("a snapshot is held").get();
        run.history.prune(oldest);

        let pinned = rng.below(AGES.len() + 1);
        let operation = match held.get(pinned) {
            Some(Some((snapshot, _))) => snapshot.operation(),
            _ => db.read(&mut lease),
        };
        let at = operation.at().get();
        let state = run.history.through(at);
        let subtrees = state.subtrees();
        let mut expected = vec![Expected::of(&state, shared(), at)];
        for _ in 0..3.min(subtrees.len()) {
            let subtree = subtrees[rng.below(subtrees.len())];
            expected.push(Expected::of(&state, subtree, at));
        }
        drop(state);
        let mut read = Read::default();
        for expected in &expected {
            read += expected.check(db, &operation);
        }
        drop(operation);
        add(&tally.nodes, read.nodes);
        add(&tally.records, read.records);
        add(&tally.absent, read.absent);
        add(&tally.looks, 1);
        add(&tally.looks_unpinned, u64::from(pinned == AGES.len()));
        add(&tally.looks_behind, u64::from(at < run.version()));
        looks += 1;

        if looks.is_multiple_of(FLUSH_EVERY) {
            // The log is made durable beside the commits that append to
            // it (DU1a).
            db.flush().expect("the log is made durable");
            add(&tally.flushes, 1);
        }
        if looks.is_multiple_of(VERIFY_EVERY) {
            // The checker reads a pinned version as a reader, beside the
            // writers, write-back and the cleaner. A step of the reclaimer
            // of what drops left changes the newest version of one page,
            // and a version sees that change only where no later commit
            // wrote the page. So a version that a step is made behind can
            // hold the flat record of a reclaimed Merk in one page and no
            // entry for the Merk in another, which no reader comes to and
            // the checker reports. The version is therefore pinned with
            // the reclaimer held back, and the reclaimer stays held back
            // for the run (`crate::verify`).
            let reclaiming = lock(&db.shared.store.reclaiming);
            let snapshot = db.snapshot();
            let report = verify(db, Scope::Reader, snapshot.version());
            drop(reclaiming);
            drop(snapshot);
            sound(&report, "a pinned version is not sound for a reader");
            add(&tally.verifies, 1);
        }
    }
}

/// Sets the flag it holds when the thread that holds it panics, so that
/// the other threads of the epoch end too.
struct Failing<'a>(&'a AtomicBool);

impl Drop for Failing<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.store(true, Ordering::Release);
        }
    }
}

/// What the cleaner of an epoch did, and the errors it reported.
fn clean<'a>(db: &'a Db, state: &'a CleanerState) -> Cleaner<'a> {
    Cleaner::new(db.shared.write_back(), state).with_stats(&db.shared.store.stats)
}

/// Waits until `done` says so, and fails the test after two minutes.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let began = Instant::now();
    while !done() {
        assert!(
            began.elapsed() < Duration::from_secs(120),
            "waited two minutes for {what}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// One epoch: a session of the database with every thread running, and
/// what is checked when it ends. Returns what the cleaner did.
fn epoch(
    config: &Config,
    number: u64,
    dir: &Path,
    history: &History,
    writers: &mut (Writer, Writer),
    tally: &Tally,
) -> CleanerStats {
    // The pool holds less than the database in most epochs, and all of it
    // in some.
    let pool =
        [1usize << 20, 2 << 20, 6 << 20][(mix(config.seed, &number.to_le_bytes()) % 3) as usize];
    let options = stress_options(config.syncs, pool);
    let home = dir.join("db");
    let db = Db::open(&home, &options).unwrap_or_else(|error| panic!("the database: {error}"));
    if number == 0 {
        // The subtrees that stay: the one both writers write records of,
        // and the Merks of each.
        let mut batch = OwnedBatch::default();
        batch
            .subtrees
            .push(crate::txn::tests::model::OwnedSubtree::Enter(
                shared(),
                None,
            ));
        batch.subtrees.extend(writers.0.enters());
        batch.subtrees.extend(writers.1.enters());
        record(&db, history, &batch);
    }
    // What the session before left, as this one opened it.
    assert_eq!(db.version().get(), history.version());
    compare_all(&db, history);

    let stop = AtomicBool::new(false);
    let forced = Forced::default();
    let run = Run {
        db: &db,
        history,
        tally,
        stop: &stop,
        forced: &forced,
        seed: mix(config.seed, &number.to_be_bytes()),
    };
    let cleaner_state = CleanerState::new(CleanerConfig {
        min_segment_len: 16 << 10,
        segment_len: 128 << 10,
        idle: Duration::from_millis(1),
        ..CleanerConfig::new()
    });
    let cleaner_errors = Mutex::new(Vec::new());
    let before = (
        get(&tally.looks),
        get(&tally.outside),
        get(&tally.checkpoints),
        get(&tally.verifies),
    );
    let (block_writer, outside_writer) = (&mut writers.0, &mut writers.1);
    thread::scope(|threads| {
        let (run, options, cleaner_state, cleaner_errors) =
            (&run, &options, &cleaner_state, &cleaner_errors);
        threads.spawn(move || {
            let _failing = Failing(run.stop);
            let store = &*run.db.shared.store;
            let steps = run.db.shared.steps();
            let cleaner = clean(run.db, cleaner_state);
            cleaner.run(
                |cleaner| {
                    let owners = BothOwners(
                        TreeOwners::new(store.loader(), &store.pool, store.page_size, &steps),
                        FlatOwners::new(store.loader(), &store.pool, store.config(), &steps),
                    );
                    cleaner.pass_values(&owners)
                },
                |error| lock(cleaner_errors).push(error.to_string()),
            );
        });
        // The cleaner's loop ends when the epoch does, however it ends.
        struct Stop<'a, 'c>(Cleaner<'c>, &'a AtomicBool);
        impl Drop for Stop<'_, '_> {
            fn drop(&mut self) {
                self.1.store(true, Ordering::Release);
                self.0.stop();
            }
        }
        let _stop = Stop(clean(run.db, cleaner_state), run.stop);
        threads.spawn(move || {
            let _failing = Failing(run.stop);
            reader(run);
        });
        threads.spawn(move || {
            let _failing = Failing(run.stop);
            outside(run, outside_writer, dir, options);
        });

        let began = Instant::now();
        let mut blocks = 0u64;
        while !run.stopped() {
            block(run, block_writer, number * 1_000_000 + blocks);
            blocks += 1;
            match config.until {
                Until::Blocks(enough) if blocks >= enough => break,
                Until::Elapsed(long) if began.elapsed() >= long && blocks >= LEAST_BLOCKS => {
                    break;
                }
                _ => {}
            }
        }
        // Each of the other threads has done its least before the epoch
        // ends, so that a run of any length holds what `held` asks for.
        let done = |count: &AtomicU64, was: u64, least: u64| get(count) >= was + least;
        until("the other threads to do their least", || {
            run.stopped()
                || (done(&tally.looks, before.0, 2 * VERIFY_EVERY)
                    && done(&tally.outside, before.1, CHECKPOINT_EVERY)
                    && done(&tally.checkpoints, before.2, 1)
                    && done(&tally.verifies, before.3, 1)
                    && cleaner_state.stats().passes >= 3
                    && cleaner_state.stats().value_passes >= 3)
        });
    });
    let cleaner_errors = cleaner_errors.into_inner().expect("the errors");
    assert!(cleaner_errors.is_empty(), "the cleaner: {cleaner_errors:?}");
    let cleaner = clean(&db, &cleaner_state);
    let cleaned = cleaner_state.stats();
    assert_eq!(
        (cleaned.errors, cleaned.segments_unreadable),
        (0, 0),
        "{cleaned:?}"
    );

    // Everything that was dropped is reclaimed. A page that the latest
    // commit wrote is freed after the next one, so commits are made until
    // nothing waits.
    for round in 0u32.. {
        let waiting = db.reclaim().waiting;
        if waiting == 0 && db.stats().reclaimed.waiting == 0 {
            break;
        }
        assert!(round < 2_000, "the drops are not reclaimed: {}", db.stats());
        tick(&db, history, round);
        add(&tally.ticks, 1);
    }
    cleaner.finish().expect("the cleaner's files are closed");
    let stats = db.stats();
    assert_eq!(stats.reclaimed.given_up, 0, "{stats}");
    assert_eq!(
        (
            stats.background.write_back_errors,
            stats.background.reclaim_errors
        ),
        (0, 0),
        "the background work: {:?}\n{stats}",
        stats.background
    );
    assert_eq!(stats.pins, 0, "{stats}");
    assert!(stats.reopen.is_none(), "{stats}");

    // The store at rest: the threads of the database are ended, and the
    // checker reads everything of the latest version.
    halt_background(&db);
    let latest = db.version();
    let report = verify(&db, Scope::Still, latest);
    sound(&report, "the store is not sound at rest");
    assert_eq!(
        report.unreached.cells, 0,
        "a cell that nothing reaches:\n{report}"
    );
    let read = compare_all(&db, history);
    println!(
        "epoch {number}: version {latest}, a pool of {} KiB, {} nodes and {} records at rest; \
         {} merks reclaimed, {} ids released, {} stalls; the cleaner: {} passes, {} blocks moved, \
         {} segments dropped, {} values moved, {} value files dropped",
        pool >> 10,
        read.nodes,
        read.records,
        stats.reclaimed.merks,
        stats.ids_released,
        stats.commits.stalls,
        cleaned.passes,
        cleaned.blocks_moved,
        cleaned.segments_dropped,
        cleaned.values_moved,
        cleaned.value_files_dropped,
    );
    // What a close does once the threads are ended, and the next session.
    db.shared.finish_reclaiming();
    (db.shared.write_back().finish()).expect("the checkpoint of the close");
    drop(db);
    cleaned
}

fn stress(config: Config) -> Tally {
    println!("stress: {config:?}");
    may_open(config.until.open_files());
    let dir = temp();
    let tally = Tally::default();
    let history = History::new(1);
    let mut writers = (Writer::new(1, config.seed), Writer::new(2, config.seed));
    let mut cleaned = CleanerStats::default();
    for number in 0..config.epochs {
        let did = epoch(&config, number, dir.path(), &history, &mut writers, &tally);
        cleaned.blocks_moved += did.blocks_moved;
        cleaned.segments_dropped += did.segments_dropped;
        cleaned.values_moved += did.values_moved;
        cleaned.value_files_dropped += did.value_files_dropped;
        cleaned.links_trimmed += did.links_trimmed;
    }
    // The last session, opened by the engine and closed by it.
    let options = stress_options(config.syncs, 2 << 20);
    let db = Db::open(dir.path().join("db"), &options).expect("the database is opened again");
    compare_all(&db, &history);
    db.close().expect("the session ends");
    println!("{tally:#?}\n{cleaned:?}");
    tally
}

/// What every run holds, whatever its length: each kind of thing the
/// stress is there to do was done.
fn held(tally: &Tally) {
    for (what, count) in [
        ("blocks", &tally.blocks),
        ("commits of blocks", &tally.published),
        ("conflicts made sure", &tally.forced),
        (
            "conflicts that the model says were sure",
            &tally.sure_conflicts,
        ),
        ("scopes rolled back", &tally.scope_rollbacks),
        ("scopes refused for an overlap", &tally.overlaps),
        ("commits with no transaction", &tally.outside),
        ("checkpoints", &tally.checkpoints),
        ("looks of the reader", &tally.looks),
        ("runs of the checker beside the writers", &tally.verifies),
        ("flushes beside the commits", &tally.flushes),
        ("nodes read", &tally.nodes),
        ("records read", &tally.records),
        ("subtrees found dropped", &tally.absent),
    ] {
        assert!(get(count) > 0, "no {what}: {tally:#?}");
    }
    assert!(
        get(&tally.sure_conflicts) >= get(&tally.forced),
        "a conflict that was made sure was not one: {tally:#?}"
    );
}

/// **The short form**, with the crate's tests: two epochs of 48 blocks,
/// with every thread running, each epoch ended by counts.
#[test]
fn a_short_stress() {
    let tally = stress(Config {
        seed: 5,
        epochs: 2,
        until: Until::Blocks(48),
        syncs: false,
    });
    held(&tally);
    assert_eq!(get(&tally.forced_missed), 0, "{tally:#?}");
}

/// **The long form** (VER-05's acceptance: "one hour of stress with no
/// assertion failure"): epochs of a minute for `LIGNIN_STRESS_SECONDS`
/// seconds, an hour unless said otherwise, on files that are synced. See
/// the [module documentation](self) for the command.
#[test]
#[ignore = "the long form of the stress test: LIGNIN_STRESS_SECONDS, an hour by default"]
fn an_hour_of_stress() {
    let number = |name: &str, default: u64| {
        let given = std::env::var(name).ok();
        given.map_or(default, |given| {
            given
                .parse()
                .unwrap_or_else(|_| panic!("{name}={given} is no number"))
        })
    };
    let seconds = number("LIGNIN_STRESS_SECONDS", 3_600);
    let epoch = number("LIGNIN_STRESS_EPOCH_SECONDS", 60).clamp(1, seconds.max(1));
    let clock = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let seed = number(
        "LIGNIN_STRESS_SEED",
        clock.map_or(1, |since| since.as_secs()),
    );
    let began = Instant::now();
    let tally = stress(Config {
        seed,
        epochs: seconds.div_ceil(epoch).max(1),
        until: Until::Elapsed(Duration::from_secs(epoch)),
        syncs: true,
    });
    println!(
        "stress: seed {seed}, {:.0} s",
        began.elapsed().as_secs_f64()
    );
    held(&tally);
}
