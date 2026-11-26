//! The commit of a block at a pace, on every engine of one run: the median,
//! the 99th and the 99.9th percentile and the longest (PERF-07-F1).
//!
//! ```text
//! lignin-bench latency --scratch DIR --engine rocksdb --engine lignindb \
//!     --background quiet --blocks 100000 --out run.json
//! lignin-bench latency-table run1.json run2.json ...
//! ```
//!
//! For each engine, in the order named and in a fresh database: the
//! contract is set up and `--documents` documents are loaded back to back
//! with the engine as it opens by default, the database is closed and
//! opened again as the run's background asks, `--warm-blocks` blocks are
//! applied untimed, and then `--blocks` blocks of the reference workload's
//! shape, each in a write transaction of its own, with a pause of
//! `--pause-us` after each commit. The time of a commit is an `Instant`
//! around the commit of the block's transaction and nothing else. The
//! measuring thread's own processor clock is read before and after it as
//! well: a commit's time less what the thread ran is what the thread was
//! off a processor for, as the system that runs the harness sees it. A
//! machine under that system that takes a processor away from it is not
//! seen there: the thread's clock goes on. After
//! the last engine the root hashes of the engines are compared: they have
//! applied one stream.
//!
//! # The background
//!
//! | `--background` | LigninDB | RocksDB |
//! | --- | --- | --- |
//! | `default` | as a node opens it | as GroveDB opens it |
//! | `heavy` | segments and log files of 4 MiB, a table checkpoint at each megabyte of log, and a cleaner that works towards a twentieth of dead space | as GroveDB opens it, with a flush of its memtables asked from a second thread every `--flush-blocks` blocks, so that table files are written and compacted beside the commits |
//! | `quiet` | a cleaner that moves nothing, and no table checkpoint of write-back's loop before 64 GiB of log are held: the passes of write-back are left | as GroveDB opens it |
//!
//! Under `quiet` LigninDB keeps every block and every log file it writes,
//! so the setting is one for a short run: the database grows by what the
//! run writes, where under the two others it grows by what the run adds.
//!
//! GroveDB opens RocksDB with fixed options, so the harness cannot make its
//! background heavier from inside; the flush is `GroveDb::flush`, which any
//! caller can ask for. A `--set KEY=VALUE` is added to what the background
//! asks of LigninDB and wins over it.
//!
//! `--neighbour-ms N` adds, for every engine alike, a thread of the harness
//! that appends 4 KiB to a file of its own in the scratch directory and
//! syncs it every `N` milliseconds: syncs of a file that is no engine's.
//!
//! # The machine beside the commits
//!
//! Beside the measured blocks of each engine a thread of the harness
//! sleeps for half a millisecond, again and again, and notes by how much
//! each sleep was longer than asked ([`ClockProbe`]). On a machine that
//! runs every thread that can run, a sleep is late by tens of
//! microseconds. Where the machine, or one under it, keeps threads
//! waiting for a processor, the sleeps are late by milliseconds, and so
//! is the end of a commit that is as long as a sleep: the report says how
//! often, so that a percentile of a millisecond or more can be read
//! against it.
//!
//! # Beside the harness
//!
//! `lignin-testkit/src/bench/latency/` holds what is run around the harness
//! for PERF-07-F1, on Linux (`docs/lignindb/background-and-readers.md`):
//!
//! - `campaign.sh` is the one command: from a clean checkout it makes the
//!   builds, every run and every trace, and writes all the tables of the
//!   task to one file;
//! - `build.sh` makes a build for a comparison, from a clean checkout in a
//!   target directory of its own, and names it by commit and sha256;
//! - `log-allocated-ahead.patch` and `fewer-syncs.patch` are the two
//!   experiments on the engine, which a campaign builds beside the base.
//!   Neither is in the engine: each is a switch of the environment around
//!   a change that is made for a measurement and for nothing else;
//! - `traced.sh` makes one run with the kernel trace of `trace.bt`
//!   (`bpftrace`) beside it: the stacks a thread leaves a processor with,
//!   who wakes it, and the syncs, the long writes and the journal's commits
//!   around it;
//! - `waits.py` makes the tables of what the measuring thread waited for
//!   from such a trace, and `tables.py` the tables of a campaign;
//! - `pairs.sh` runs two latency runs at a time, each on a file system of
//!   its own, in rounds in which the two exchange their file systems: for a
//!   machine that other work shares, where two runs made one after the
//!   other are not made under the same conditions.
//!
//! # What is told of a long commit
//!
//! - The harness names its measuring thread `lignin-commit`. It writes
//!   lines to `/dev/null`, outside every timed region, which a trace of the
//!   system's calls sees: `lignin-mark measured begin` before the first
//!   measured block of an engine and `lignin-mark measured end` after the
//!   last, so that the load and the warm blocks, which the same thread
//!   applies the same way, are told from what is measured; and after a
//!   commit of `--mark-us` microseconds or more,
//!   `lignin-mark commit <block> <nanoseconds>`.
//! - For LigninDB the parts of a commit that the engine counts apart
//!   ([`PARTS`]) are read around every commit: its waits for the commit
//!   lock and for the lock on the counts of values, its append to the log,
//!   the placing of values and the reclaiming after it, and its wait for
//!   room in write-back.
//! - With `--watch-files`, LigninDB is opened on a file layer of the harness
//!   that passes every call on to the system's and times the syncs and the
//!   appends: the report then counts the syncs by what they are of, and
//!   lists for each of the slowest commits the append calls that the
//!   measuring thread made on the log's files during it (the group's, and
//!   the record of each page step the commit makes after it) and which
//!   syncs had a moment in common with the commit. A set of syncs is made one after the other
//!   on that layer, as on Linux the system's layer makes it.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use grovedb_version::version::GroveVersion;
use serde::{Deserialize, Serialize};

use super::{
    engine::{Block, Engine, EngineSettings},
    machine::{self, Machine},
    run::{next_block, unix_now},
    BenchError, EngineChoice, RocksDbEngine, STACK_BYTES,
};
use crate::workload::{drive::Documents, Op, Stream};

/// The schema of a [`LatencyReport`].
pub const SCHEMA: &str = "lignin-bench-latency/1";

/// The name of the measuring thread, by which a trace finds it.
pub const THREAD_NAME: &str = "lignin-commit";

/// What a line that marks a long commit begins with.
pub const MARK: &str = "lignin-mark commit";

/// The lines that mark where the measured blocks of an engine begin and
/// end.
pub const MARK_BEGIN: &str = "lignin-mark measured begin";
/// See [`MARK_BEGIN`].
pub const MARK_END: &str = "lignin-mark measured end";

/// What the engines do beside the commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Background {
    /// As a node opens the engine.
    Default,
    /// As little as an engine can be asked for, for a short run.
    Quiet,
    /// Small files, frequent checkpoints and cleaning.
    Heavy,
}

impl Background {
    fn named(name: &str) -> Option<Self> {
        match name {
            "default" => Some(Background::Default),
            "quiet" => Some(Background::Quiet),
            "heavy" => Some(Background::Heavy),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Background::Default => "default",
            Background::Quiet => "quiet",
            Background::Heavy => "heavy",
        }
    }
}

/// What a latency run applies and how.
#[derive(Clone, Debug)]
pub struct LatencyOptions {
    /// Where the databases are made, each engine in a directory of its own.
    pub scratch: PathBuf,
    /// The engines, measured one after another in this order.
    pub engines: Vec<EngineChoice>,
    /// The workload's seed.
    pub seed: u64,
    /// Documents loaded before anything is measured.
    pub documents: u64,
    /// Owners the documents are spread over.
    pub owners: u64,
    /// Blocks applied untimed before the measured ones.
    pub warm_blocks: u64,
    /// Blocks whose commits are measured.
    pub blocks: u64,
    /// The pause after each commit.
    pub pause: Duration,
    /// What the engines do beside the commits.
    pub background: Background,
    /// How many blocks lie between two flushes asked of RocksDB under the
    /// heavy background.
    pub flush_blocks: u64,
    /// How often the neighbour syncs its file, if there is one.
    pub neighbour: Option<Duration>,
    /// Settings of LigninDB beyond those of the background.
    pub engine: EngineSettings,
    /// Whether LigninDB is opened on the file layer that times its calls.
    pub watch_files: bool,
    /// A commit at least this long is marked; none with zero.
    pub mark: Duration,
    /// How many of the slowest commits a report lists.
    pub slowest: usize,
    /// A name for the build that runs.
    pub label: Option<String>,
    /// The commit to report; found with `git` when not given.
    pub commit: Option<String>,
    /// Keep the databases.
    pub keep: bool,
    /// The GroveVersion to apply.
    pub grove_version: &'static GroveVersion,
}

impl LatencyOptions {
    /// A run of 100,000 blocks beside the default background, on a store of
    /// 100,000 documents, with 5 ms between two blocks, of no engine yet:
    /// [`run`] refuses options that name none.
    pub fn new(scratch: PathBuf) -> Self {
        LatencyOptions {
            scratch,
            engines: Vec::new(),
            seed: 1,
            documents: 100_000,
            owners: 1_000,
            warm_blocks: 200,
            blocks: 100_000,
            pause: Duration::from_micros(5_000),
            background: Background::Default,
            flush_blocks: 10,
            neighbour: None,
            engine: EngineSettings::default(),
            watch_files: false,
            mark: Duration::from_micros(2_000),
            slowest: 12,
            label: None,
            commit: None,
            keep: false,
            grove_version: GroveVersion::latest(),
        }
    }

    fn per_block() -> u64 {
        u64::from(Documents::reference().transitions_per_block)
    }

    fn check(&self) -> Result<(), BenchError> {
        let refuse = |why: &str| Err(BenchError::Check(why.to_string()));
        if self.engines.is_empty() {
            return refuse("a latency run measures at least one engine");
        }
        if self.documents == 0 || !self.documents.is_multiple_of(Self::per_block()) {
            return refuse("--documents takes a multiple of 20 above 0");
        }
        if self.owners == 0 || self.blocks == 0 || self.flush_blocks == 0 {
            return refuse("--owners, --blocks and --flush-blocks take a number above 0");
        }
        if self.neighbour.is_some_and(|every| every.is_zero()) {
            return refuse("--neighbour-ms takes a number above 0");
        }
        Ok(())
    }

    /// The stream: the contract's block, the blocks of the load, the warm
    /// blocks and the measured ones, all of them creates.
    fn stream(&self) -> Stream {
        let documents = Documents {
            documents: self.documents + (self.warm_blocks + self.blocks) * Self::per_block(),
            owners: self.owners,
            ..Documents::reference()
        };
        documents.stream(self.seed)
    }
}

/// What a latency run measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LatencyReport {
    /// [`SCHEMA`].
    pub schema: String,
    /// When the run started, in seconds since the Unix epoch.
    pub started_at_unix: u64,
    /// The machine.
    pub machine: Machine,
    /// The load averages before the run.
    pub load_average_before: Option<[f64; 3]>,
    /// The load averages after it.
    pub load_average_after: Option<[f64; 3]>,
    /// The name given to the build that ran.
    pub label: Option<String>,
    /// The commit the harness was built from.
    pub commit: Option<String>,
    /// The GroveVersion applied.
    pub grove_protocol_version: u32,
    /// The workload's seed.
    pub seed: u64,
    /// Documents loaded before the measured blocks.
    pub documents: u64,
    /// Owners the documents are spread over.
    pub owners: u64,
    /// Blocks applied untimed before the measured ones.
    pub warm_blocks: u64,
    /// The pause after each commit, in microseconds.
    pub pause_us: u64,
    /// What the engines did beside the commits.
    pub background: Background,
    /// The blocks between two flushes asked of RocksDB, under the heavy
    /// background.
    pub flush_blocks: u64,
    /// How often the neighbour synced its file, in milliseconds.
    pub neighbour_ms: Option<u64>,
    /// The engines, in the order they were measured.
    pub engines: Vec<EngineLatency>,
}

/// What a latency run measured of one engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineLatency {
    /// The engine's name.
    pub engine: String,
    /// What the engine was asked beyond its defaults, each as `KEY=VALUE`.
    pub settings: Vec<String>,
    /// The root hash after the last measured block, hex.
    pub root_hash: String,
    /// How long the measured blocks took with their pauses, in seconds.
    pub seconds: f64,
    /// The system's id of the measuring thread, where it has one.
    pub thread_id: Option<u32>,
    /// The time of each commit, in nanoseconds, in the order of the blocks.
    pub commit_ns: Vec<u64>,
    /// The time the operations of each block took, in nanoseconds.
    pub apply_ns: Vec<u64>,
    /// How long the measuring thread ran on a processor during each commit,
    /// in nanoseconds, by the thread's own clock.
    pub ran_ns: Vec<u64>,
    /// The parts of each commit that the engine counts apart, in
    /// nanoseconds and in the order of [`PARTS`], where it counts them.
    pub parts_ns: Option<Vec<[u64; 6]>>,
    /// The turns of the engine's write-back that ended over the measured
    /// blocks, where it has such turns.
    pub turns: Option<u64>,
    /// The flushes the harness asked of the engine beside the commits.
    pub flushes_asked: u64,
    /// The syncs the neighbour made beside the commits.
    pub neighbour_syncs: u64,
    /// What the thread that only sleeps saw beside the commits.
    #[serde(default)]
    pub clock: ClockProbe,
    /// The syncs the engine made through the watching file layer over the
    /// measured blocks, by what they are of: how many, and their time in
    /// nanoseconds. Empty where the layer was not asked for.
    pub syncs: BTreeMap<String, (u64, u64)>,
    /// The slowest commits, slowest first.
    pub slowest: Vec<SlowCommit>,
}

/// What the thread that only sleeps saw beside the measured blocks of one
/// engine: by how much its sleeps of [`ClockProbe::ASKED`] were late.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockProbe {
    /// The sleeps it made.
    pub sleeps: u64,
    /// The sleeps that were late by 0.2, 1, 5 and 20 ms or more.
    pub late: [u64; 4],
    /// The longest a sleep was late by, in nanoseconds.
    pub latest_ns: u64,
    /// By how much the sleeps were late in all, in nanoseconds.
    pub late_ns: u64,
}

impl ClockProbe {
    /// How long each sleep is asked to be.
    pub const ASKED: Duration = Duration::from_micros(500);
    /// The bounds of [`Self::late`], in nanoseconds.
    const BOUNDS: [u64; 4] = [200_000, 1_000_000, 5_000_000, 20_000_000];

    fn note(&mut self, late: Duration) {
        let late = late.as_nanos() as u64;
        self.sleeps += 1;
        self.late_ns += late;
        self.latest_ns = self.latest_ns.max(late);
        for (count, bound) in self.late.iter_mut().zip(Self::BOUNDS) {
            *count += u64::from(late >= bound);
        }
    }

    /// The share of the sleeps that were late by a millisecond or more.
    pub fn late_share(&self) -> f64 {
        self.late[1] as f64 / self.sleeps.max(1) as f64
    }
}

/// Sleeps for [`ClockProbe::ASKED`] again and again until `stop`, and
/// returns by how much the sleeps were late.
fn clock_probe(stop: &AtomicBool) -> ClockProbe {
    let mut probe = ClockProbe::default();
    while !stop.load(Ordering::Acquire) {
        let began = Instant::now();
        std::thread::sleep(ClockProbe::ASKED);
        probe.note(began.elapsed().saturating_sub(ClockProbe::ASKED));
    }
    probe
}

impl EngineLatency {
    /// How long each commit's thread was off a processor during it: the
    /// commit's time less what the thread ran.
    pub fn off_ns(&self) -> Vec<u64> {
        (self.commit_ns.iter().zip(&self.ran_ns))
            .map(|(commit, ran)| commit.saturating_sub(*ran))
            .collect()
    }
}

/// One of the slowest commits of a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlowCommit {
    /// The number of the block among the measured ones, from 0.
    pub block: u64,
    /// The time of the commit, in nanoseconds.
    pub commit_ns: u64,
    /// How long the measuring thread ran on a processor during it.
    pub ran_ns: u64,
    /// The parts of the commit that the engine counts apart, in the order
    /// of [`PARTS`].
    pub parts_ns: Option<[u64; 6]>,
    /// The append calls that the measuring thread made on files of the log
    /// during the commit, as the watching file layer timed them: how many,
    /// their time in all and the longest, in nanoseconds. A commit appends
    /// its group, and after it the record of each page step it makes.
    pub append_calls: Option<(u64, u64, u64)>,
    /// The syncs that had a moment in common with the commit, as the
    /// watching file layer listed them: of what, and how long each took in
    /// nanoseconds.
    pub syncs_beside: Vec<(String, u64)>,
}

/// The parts of a commit that LigninDB counts apart, as a report names
/// them.
pub const PARTS: [&str; 6] = [
    "its wait for the commit lock",
    "its wait for the lock on the counts of values",
    "its append to the log",
    "placing values after it",
    "reclaiming versions after it",
    "its wait for room in write-back",
];

/// The sample that `share` of `sorted` are at or below: the smallest one
/// with at least that share of the samples not above it.
pub fn quantile(sorted: &[u64], share: f64) -> u64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0;
    };
    let wanted = (sorted.len() as f64 * share).ceil() as usize;
    sorted[wanted.clamp(1, last + 1) - 1]
}

/// The median, the 99th and the 99.9th percentile and the longest of
/// `samples`.
pub fn quantiles(samples: &[u64]) -> [u64; 4] {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    [
        quantile(&sorted, 0.5),
        quantile(&sorted, 0.99),
        quantile(&sorted, 0.999),
        sorted.last().copied().unwrap_or(0),
    ]
}

/// What the harness asks of an engine for a latency run.
trait Subject: Engine + Sync {
    /// What `background` asks of the engine beyond its defaults.
    fn asked(background: Background) -> &'static [&'static str];

    /// Whether the harness asks the engine for a flush every so many
    /// blocks under the heavy background.
    const FLUSHED_BESIDE: bool;

    /// Whether the engine can be opened on the watching file layer.
    const WATCHED: bool;

    /// Opens the database in `dir` for the measured blocks.
    fn open_for(
        dir: &Path,
        settings: &EngineSettings,
        watch: Option<&'static Watch>,
    ) -> Result<Self, BenchError>;

    /// The engine's running counts of the parts of its commits, in
    /// nanoseconds and in the order of [`PARTS`], and of the turns of its
    /// write-back that have ended.
    fn look(&self) -> Option<([u64; 6], u64)> {
        None
    }
}

impl Subject for RocksDbEngine {
    const FLUSHED_BESIDE: bool = true;
    const WATCHED: bool = false;

    fn asked(_background: Background) -> &'static [&'static str] {
        &[]
    }

    fn open_for(
        dir: &Path,
        _settings: &EngineSettings,
        _watch: Option<&'static Watch>,
    ) -> Result<Self, BenchError> {
        Self::open(dir)
    }
}

#[cfg(grovedb_engine_seam)]
mod lignin {
    use std::{path::Path, sync::Arc, time::Duration};

    use super::{Background, Subject, Watch, Watching};
    use crate::bench::{
        engine::EngineSettings,
        lignindb::{LigninDbEngine, Tuning},
        BenchError,
    };

    const QUIET: &[&str] = &[
        "checkpoint_log_mb=65536",
        "log_held_mb=65536",
        "cleaner_target=100",
        "cleaner_checkpoint_dead=100",
        "cleaner_seal_dead=100",
        "cleaner_value_target=100",
    ];

    const HEAVY: &[&str] = &[
        "log_file_mb=4",
        "segment_mb=4",
        "checkpoint_log_mb=1",
        "cleaner_target=5",
        "cleaner_checkpoint_dead=5",
        "cleaner_seal_dead=5",
        "cleaner_min_segment_kb=1024",
        "cleaner_segment_mb=4",
        "cleaner_value_target=5",
    ];

    impl Subject for LigninDbEngine {
        const FLUSHED_BESIDE: bool = false;
        const WATCHED: bool = true;

        fn asked(background: Background) -> &'static [&'static str] {
            match background {
                Background::Default => &[],
                Background::Quiet => QUIET,
                Background::Heavy => HEAVY,
            }
        }

        fn open_for(
            dir: &Path,
            settings: &EngineSettings,
            watch: Option<&'static Watch>,
        ) -> Result<Self, BenchError> {
            let mut tuning = Tuning::new(settings)?;
            if let Some(watch) = watch {
                tuning.options.vfs = Arc::new(Watching {
                    inner: Arc::clone(&tuning.options.vfs),
                    watch,
                });
            }
            Self::open_tuned(dir, tuning)
        }

        fn look(&self) -> Option<([u64; 6], u64)> {
            let stats = self.grove().engine().database().stats();
            let commits = &stats.commits;
            let parts = [
                commits.lock_waited,
                commits.counts_waited,
                commits.appending,
                commits.placing,
                commits.reclaiming,
                stats.store.flush.room_waited,
            ];
            let nanos = |part: Duration| u64::try_from(part.as_nanos()).unwrap_or(u64::MAX);
            Some((parts.map(nanos), stats.store.watch.turns_ended))
        }
    }
}

/// What the watching file layer has timed.
#[derive(Debug, Default)]
pub struct Watch {
    seen: Mutex<Seen>,
    /// The append calls the measuring thread made on files of the log
    /// since the last reset: how many, their time in all and the longest,
    /// in nanoseconds.
    appends: [AtomicU64; 3],
}

thread_local! {
    /// Whether the thread is the one whose appends to the log are timed.
    static MEASURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Debug, Default)]
struct Seen {
    /// The syncs under way: a number, when each began, of what.
    under_way: Vec<(u64, Instant, &'static str)>,
    /// The syncs that ended since the last trim: when each began and
    /// ended, of what.
    ended: Vec<(Instant, Instant, &'static str)>,
    /// How many syncs ended, and their time in nanoseconds, by what they
    /// are of.
    counted: BTreeMap<&'static str, (u64, u64)>,
    next: u64,
}

impl Watch {
    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn sync<T>(&self, of: &'static str, sync: impl FnOnce() -> T) -> T {
        let mut seen = self.seen();
        let number = seen.next;
        seen.next += 1;
        let began = Instant::now();
        seen.under_way.push((number, began, of));
        drop(seen);
        let synced = sync();
        let ended = Instant::now();
        let mut seen = self.seen();
        seen.under_way.retain(|(other, ..)| *other != number);
        seen.ended.push((began, ended, of));
        let counted = seen.counted.entry(of).or_default();
        counted.0 += 1;
        counted.1 += (ended - began).as_nanos() as u64;
        synced
    }

    /// Begins the measured blocks on the calling thread: what was counted
    /// before is forgotten.
    fn begin(&self) {
        MEASURING.set(true);
        let mut seen = self.seen();
        seen.ended.clear();
        seen.counted.clear();
    }

    /// Forgets the syncs that ended before `before`.
    fn trim(&self, before: Instant) {
        self.seen().ended.retain(|(_, ended, _)| *ended >= before);
    }

    /// The syncs that had a moment in common with the time from `from` to
    /// `to`, once every sync that began before `to` has ended.
    fn beside(&self, from: Instant, to: Instant) -> Vec<(String, u64)> {
        let seen = loop {
            let seen = self.seen();
            if seen.under_way.iter().all(|(_, began, _)| *began >= to) {
                break seen;
            }
            drop(seen);
            std::thread::sleep(Duration::from_micros(200));
        };
        let over = seen
            .ended
            .iter()
            .filter(|(began, ended, _)| *began < to && *ended > from);
        over.map(|(began, ended, of)| (of.to_string(), (*ended - *began).as_nanos() as u64))
            .collect()
    }

    /// Forgets the append calls of the measuring thread.
    fn reset_appends(&self) {
        for count in &self.appends {
            count.store(0, Ordering::Relaxed);
        }
    }

    fn appends(&self) -> (u64, u64, u64) {
        let [calls, total, longest] = &self.appends;
        (
            calls.load(Ordering::Relaxed),
            total.load(Ordering::Relaxed),
            longest.load(Ordering::Relaxed),
        )
    }

    fn counted(&self) -> BTreeMap<String, (u64, u64)> {
        let counted = &self.seen().counted;
        counted
            .iter()
            .map(|(of, count)| (of.to_string(), *count))
            .collect()
    }
}

/// What a file of a database is, by the directory it is in.
fn kind_of(path: &Path) -> &'static str {
    let name = path.file_name().and_then(|name| name.to_str());
    let dir = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    match (dir, name) {
        (_, Some("MANIFEST.tmp")) => "the manifest's temporary file",
        (Some("wal"), Some(name)) if name.ends_with(".tmp") => "a log file written again",
        (Some("wal"), _) => "a log file",
        (Some("val"), _) => "a value-log file",
        (Some("seg"), _) => "a segment",
        (Some("table"), _) => "a table checkpoint",
        _ => "another file",
    }
}

fn names_of(path: &Path) -> &'static str {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("wal") => "the names of wal/",
        Some("val") => "the names of val/",
        Some("seg") => "the names of seg/",
        Some("table") => "the names of table/",
        _ => "the names of the database directory or of one above it",
    }
}

#[cfg(grovedb_engine_seam)]
mod watching {
    use std::{io, path::Path, sync::Arc, time::Instant};

    use lignin_io::{DirEntry, OpenMode, Vfs, VfsFile, VfsLock};

    use super::{kind_of, names_of, Ordering, Watch, MEASURING};

    /// The file layer `inner`, with every sync and every append to a file
    /// of the log timed for `watch`.
    #[derive(Debug)]
    pub(super) struct Watching {
        pub(super) inner: Arc<dyn Vfs>,
        pub(super) watch: &'static Watch,
    }

    #[derive(Debug)]
    struct Watched {
        file: Box<dyn VfsFile>,
        kind: &'static str,
        watch: &'static Watch,
    }

    impl VfsFile for Watched {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
            self.file.read_at(buf, offset)
        }

        fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
            self.file.write_at(buf, offset)
        }

        fn append(&self, buf: &[u8]) -> io::Result<u64> {
            if self.kind != "a log file" {
                return self.file.append(buf);
            }
            let began = Instant::now();
            let appended = self.file.append(buf);
            let took = began.elapsed().as_nanos() as u64;
            if MEASURING.get() {
                let [calls, total, longest] = &self.watch.appends;
                calls.fetch_add(1, Ordering::Relaxed);
                total.fetch_add(took, Ordering::Relaxed);
                longest.fetch_max(took, Ordering::Relaxed);
            }
            appended
        }

        fn sync_data(&self) -> io::Result<()> {
            self.watch.sync(self.kind, || self.file.sync_data())
        }

        fn sync_all(&self) -> io::Result<()> {
            self.watch.sync(self.kind, || self.file.sync_all())
        }

        fn len(&self) -> io::Result<u64> {
            self.file.len()
        }

        fn set_len(&self, len: u64) -> io::Result<()> {
            self.file.set_len(len)
        }
    }

    impl Vfs for Watching {
        fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
            Ok(Box::new(Watched {
                file: self.inner.open(path, mode)?,
                kind: kind_of(path),
                watch: self.watch,
            }))
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.inner.rename(from, to)
        }

        fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
            self.inner.hard_link(original, link)
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            self.inner.remove_file(path)
        }

        fn create_dir(&self, path: &Path) -> io::Result<()> {
            self.inner.create_dir(path)
        }

        fn remove_dir(&self, path: &Path) -> io::Result<()> {
            self.inner.remove_dir(path)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
            self.inner.read_dir(path)
        }

        fn sync_dir(&self, path: &Path) -> io::Result<()> {
            self.watch
                .sync(names_of(path), || self.inner.sync_dir(path))
        }

        fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
            self.inner.lock(path)
        }

        fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
            self.inner.lock_existing(path)
        }
    }
}

#[cfg(grovedb_engine_seam)]
use watching::Watching;

/// Writes lines where a trace of the thread's calls sees them and nothing
/// keeps them: where the measured blocks begin and end, and after a long
/// commit.
struct Marker {
    sink: Option<std::fs::File>,
    from: Duration,
}

impl Marker {
    fn new(from: Duration) -> Self {
        let sink = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .ok();
        Marker { sink, from }
    }

    fn line(&mut self, line: &str) {
        if let Some(sink) = &mut self.sink {
            let _ = sink.write_all(format!("{line}\n").as_bytes());
        }
    }

    fn mark(&mut self, block: u64, took: Duration) {
        if !self.from.is_zero() && took >= self.from {
            self.line(&format!("{MARK} {block} {}", took.as_nanos()));
        }
    }
}

/// How long the calling thread has run on a processor, in nanoseconds, by
/// its own clock.
fn thread_clock() -> u64 {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    (time.tv_sec as u64) * 1_000_000_000 + time.tv_nsec as u64
}

/// Applies the operations of the next block of `stream` in a write
/// transaction of its own and commits it. Returns how long the operations
/// and the commit took, when the commit began, and how long the thread
/// ran on a processor during it. `watch` is told where the commit begins.
fn block<E: Engine>(
    engine: &E,
    stream: &mut Stream,
    grove_version: &GroveVersion,
    watch: Option<&Watch>,
) -> Result<(Duration, Duration, Instant, u64), BenchError> {
    let block = next_block(stream);
    let Some((Op::Commit, ops)) = block.split_last() else {
        return Err(BenchError::Check("the stream has ended".to_string()));
    };
    let mut transaction = engine.begin_block();
    let began = Instant::now();
    for op in ops {
        transaction.apply(op, grove_version)?;
    }
    let applied = began.elapsed();
    if let Some(watch) = watch {
        watch.reset_appends();
    }
    let ran = thread_clock();
    let began = Instant::now();
    let committed = transaction.commit();
    let took = began.elapsed();
    let ran = thread_clock().saturating_sub(ran);
    committed?;
    Ok((applied, took, began, ran))
}

/// The neighbour: appends 4 KiB to `path` and syncs the file every `every`
/// until `stop`. Returns how many syncs it made.
fn neighbour(path: &Path, every: Duration, stop: &AtomicBool) -> Result<u64, BenchError> {
    let failed = |error| BenchError::io("the neighbour's file", error);
    let mut file = std::fs::File::create(path).map_err(failed)?;
    let mut syncs = 0;
    while !stop.load(Ordering::Acquire) {
        file.write_all(&[0x5a; 4096]).map_err(failed)?;
        file.sync_data().map_err(failed)?;
        syncs += 1;
        std::thread::sleep(every);
    }
    Ok(syncs)
}

/// Asks `engine` for a flush each time `flush_blocks` more blocks are
/// committed, until `stop`. Returns how many it asked for.
fn flushes<E: Subject>(
    engine: &E,
    committed: &AtomicU64,
    flush_blocks: u64,
    stop: &AtomicBool,
) -> Result<u64, BenchError> {
    let (mut asked, mut at) = (0, 0);
    while !stop.load(Ordering::Acquire) {
        if committed.load(Ordering::Acquire) >= at + flush_blocks {
            at = committed.load(Ordering::Acquire);
            engine.flush()?;
            asked += 1;
        } else {
            std::thread::sleep(Duration::from_micros(500));
        }
    }
    Ok(asked)
}

fn measure<E: Subject>(options: &LatencyOptions) -> Result<EngineLatency, BenchError> {
    let dir = options.scratch.join(E::NAME);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|error| BenchError::io("removing an earlier database", error))?;
    }
    std::fs::create_dir_all(&dir)
        .map_err(|error| BenchError::io("creating the database directory", error))?;
    let measured = measure_in::<E>(options, &dir);
    if !options.keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    measured
}

fn measure_in<E: Subject>(
    options: &LatencyOptions,
    dir: &Path,
) -> Result<EngineLatency, BenchError> {
    let grove_version = options.grove_version;
    let mut stream = options.stream();

    let mut engine = E::open(dir)?;
    engine.settle()?;
    let load = 1 + options.documents / LatencyOptions::per_block();
    for _ in 0..load {
        block(&engine, &mut stream, grove_version, None)?;
    }
    engine.wait_for_background()?;
    drop(engine);

    let mut settings = EngineSettings::default();
    for pair in E::asked(options.background) {
        settings.set(pair)?;
    }
    for (key, value) in options.engine.pairs() {
        settings.set(&format!("{key}={value}"))?;
    }
    // The list of the watching layer lives as long as the process: the
    // engine's threads hold it for as long as they run.
    let watch =
        (options.watch_files && E::WATCHED).then(|| &*Box::leak(Box::new(Watch::default())));
    let mut engine = E::open_for(dir, &settings, watch)?;
    engine.settle()?;
    for _ in 0..options.warm_blocks {
        block(&engine, &mut stream, grove_version, None)?;
    }

    let blocks = options.blocks as usize;
    let mut commit_ns = Vec::with_capacity(blocks);
    let mut apply_ns = Vec::with_capacity(blocks);
    let mut ran_ns = Vec::with_capacity(blocks);
    let mut parts_ns = engine.look().map(|_| Vec::with_capacity(blocks));
    let mut slowest: Vec<SlowCommit> = Vec::new();
    let mut marker = Marker::new(options.mark);
    let committed = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let flushed = options.background == Background::Heavy && E::FLUSHED_BESIDE;
    let neighbour_file = options.scratch.join(format!("neighbour-{}", E::NAME));
    if let Some(watch) = watch {
        watch.begin();
    }
    let turns_before = engine.look().map(|(_, turns)| turns);
    let began = Instant::now();

    let (measured, flushes_asked, neighbour_syncs, clock) = std::thread::scope(|scope| {
        let engine = &engine;
        let probe = scope.spawn(|| clock_probe(&stop));
        let flusher = flushed
            .then(|| scope.spawn(|| flushes(engine, &committed, options.flush_blocks, &stop)));
        let (file, stop_beside) = (&neighbour_file, &stop);
        let beside = options
            .neighbour
            .map(|every| scope.spawn(move || neighbour(file, every, stop_beside)));

        let mut measure = || -> Result<(), BenchError> {
            marker.line(MARK_BEGIN);
            for number in 0..options.blocks {
                let before = engine.look();
                let (applied, took, from, ran) = block(engine, &mut stream, grove_version, watch)?;
                let to = from + took;
                marker.mark(number, took);
                let nanos = took.as_nanos() as u64;
                let parts = before.zip(engine.look()).map(|((before, _), (after, _))| {
                    std::array::from_fn(|part| after[part].saturating_sub(before[part]))
                });
                commit_ns.push(nanos);
                apply_ns.push(applied.as_nanos() as u64);
                ran_ns.push(ran);
                if let (Some(all), Some(parts)) = (parts_ns.as_mut(), parts) {
                    all.push(parts);
                }
                let is_slow = slowest.len() < options.slowest
                    || slowest.last().is_some_and(|last| nanos > last.commit_ns);
                if is_slow && options.slowest > 0 {
                    slowest.push(SlowCommit {
                        block: number,
                        commit_ns: nanos,
                        ran_ns: ran,
                        parts_ns: parts,
                        append_calls: watch.map(Watch::appends),
                        syncs_beside: watch.map_or_else(Vec::new, |watch| watch.beside(from, to)),
                    });
                    slowest.sort_by_key(|slow| std::cmp::Reverse(slow.commit_ns));
                    slowest.truncate(options.slowest);
                }
                if let Some(watch) = watch {
                    watch.trim(from);
                }
                committed.fetch_add(1, Ordering::AcqRel);
                std::thread::sleep(options.pause);
            }
            marker.line(MARK_END);
            Ok(())
        };
        let measured = measure();
        stop.store(true, Ordering::Release);
        let join = |helper: Option<std::thread::ScopedJoinHandle<'_, Result<u64, BenchError>>>| {
            let joined = helper.map(|helper| helper.join());
            match joined {
                None => Ok(0),
                Some(Ok(count)) => count,
                Some(Err(_)) => Err(BenchError::Check(
                    "a thread beside the commits panicked".to_string(),
                )),
            }
        };
        let clock = probe.join().unwrap_or_default();
        (measured, join(flusher), join(beside), clock)
    });
    measured?;
    let seconds = began.elapsed().as_secs_f64();
    let turns = turns_before
        .zip(engine.look())
        .map(|(before, (_, after))| after - before);
    let syncs = watch.map_or_else(BTreeMap::new, Watch::counted);
    let _ = std::fs::remove_file(&neighbour_file);
    // A run whose background work failed is no measurement.
    engine.wait_for_background()?;
    let root_hash = engine.root_hash(grove_version)?;

    Ok(EngineLatency {
        engine: E::NAME.to_string(),
        settings: settings.list(),
        root_hash: hex::encode(root_hash),
        seconds,
        thread_id: machine::thread_id(),
        commit_ns,
        apply_ns,
        ran_ns,
        parts_ns,
        turns,
        flushes_asked: flushes_asked?,
        neighbour_syncs: neighbour_syncs?,
        clock,
        syncs,
        slowest,
    })
}

/// Measures every engine of `options` on a thread named [`THREAD_NAME`]
/// and returns the report.
pub fn run(options: &LatencyOptions) -> Result<LatencyReport, BenchError> {
    options.check()?;
    std::fs::create_dir_all(&options.scratch)
        .map_err(|error| BenchError::io("creating the scratch directory", error))?;
    let options = options.clone();
    std::thread::Builder::new()
        .name(THREAD_NAME.to_string())
        .stack_size(STACK_BYTES)
        .spawn(move || run_here(&options))
        .map_err(|error| BenchError::io("starting the measuring thread", error))?
        .join()
        .map_err(|_| BenchError::Check("the measuring thread panicked".to_string()))?
}

fn run_here(options: &LatencyOptions) -> Result<LatencyReport, BenchError> {
    let commit = options
        .commit
        .clone()
        .or_else(|| machine::source_commit(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
    let mut report = LatencyReport {
        schema: SCHEMA.to_string(),
        started_at_unix: unix_now(),
        machine: Machine::detect(None),
        load_average_before: machine::load_average(),
        load_average_after: None,
        label: options.label.clone(),
        commit,
        grove_protocol_version: options.grove_version.protocol_version,
        seed: options.seed,
        documents: options.documents,
        owners: options.owners,
        warm_blocks: options.warm_blocks,
        pause_us: options.pause.as_micros() as u64,
        background: options.background,
        flush_blocks: options.flush_blocks,
        neighbour_ms: options.neighbour.map(|every| every.as_millis() as u64),
        engines: Vec::new(),
    };
    for engine in &options.engines {
        report.engines.push(match engine {
            EngineChoice::RocksDb => measure::<RocksDbEngine>(options)?,
            #[cfg(grovedb_engine_seam)]
            EngineChoice::LigninDb => measure::<super::LigninDbEngine>(options)?,
        });
    }
    report.load_average_after = machine::load_average();
    if let Some(first) = report.engines.first() {
        let differs = report
            .engines
            .iter()
            .find(|engine| engine.root_hash != first.root_hash);
        if let Some(other) = differs {
            return Err(BenchError::Check(format!(
                "{} ends at the root hash {} and {} at {}",
                first.engine, first.root_hash, other.engine, other.root_hash
            )));
        }
    }
    Ok(report)
}

impl LatencyReport {
    /// The report as JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a latency report serializes")
    }

    /// Reads a report from JSON, refusing another schema.
    pub fn from_json(text: &str) -> Result<Self, BenchError> {
        let report: LatencyReport = serde_json::from_str(text)
            .map_err(|error| BenchError::Check(format!("not a latency report: {error}")))?;
        match report.schema == SCHEMA {
            true => Ok(report),
            false => Err(BenchError::Check(format!(
                "the report has the schema {:?}, not {SCHEMA:?}",
                report.schema
            ))),
        }
    }
}

/// How many of the syncs beside a slow commit a table names; a report has
/// them all.
const SYNCS_LISTED: usize = 8;

fn micros(nanos: u64) -> String {
    format!("{:.0}", nanos as f64 / 1e3)
}

const HEAD: &str = "| Build | Engine | Background | Blocks | Median, µs | 99%, µs | 99.9%, µs | \
                    Longest, µs |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n";

/// The group a row belongs to in a table of several runs: the build, the
/// engine and what it did beside the commits.
type Group = (String, String, String);

fn group_of(report: &LatencyReport, engine: &EngineLatency) -> Group {
    let mut background = report.background.name().to_string();
    if let Some(every) = report.neighbour_ms {
        let _ = write!(background, ", a neighbour's sync every {every} ms");
    }
    (
        report.label.clone().unwrap_or_else(|| "-".to_string()),
        engine.engine.clone(),
        background,
    )
}

/// One report as text: its row for each engine, and what is told of the
/// slowest commits.
pub fn table(report: &LatencyReport) -> String {
    let mut out = format!(
        "{} documents, {} warm blocks, a pause of {} µs, on {} ({} processors), commit {}, \
         load {:?} -> {:?}\n\n{HEAD}",
        report.documents,
        report.warm_blocks,
        report.pause_us,
        report.machine.cpu.as_deref().unwrap_or("an unknown cpu"),
        report.machine.logical_cpus,
        report.commit.as_deref().unwrap_or("unknown"),
        report.load_average_before,
        report.load_average_after,
    );
    for engine in &report.engines {
        let (build, name, background) = group_of(report, engine);
        let [median, p99, p999, longest] = quantiles(&engine.commit_ns).map(micros);
        let _ = writeln!(
            out,
            "| {build} | {name} | {background} | {} | {median} | {p99} | {p999} | {longest} |",
            engine.commit_ns.len(),
        );
    }
    for engine in &report.engines {
        let blocks = engine.commit_ns.len().max(1) as f64;
        let _ = write!(
            out,
            "\n{}: {:.1} s, thread {}",
            engine.engine,
            engine.seconds,
            engine
                .thread_id
                .map_or_else(|| "unknown".to_string(), |id| id.to_string()),
        );
        if !engine.settings.is_empty() {
            let _ = write!(out, ", asked {}", engine.settings.join(" "));
        }
        if let Some(turns) = engine.turns {
            let _ = write!(
                out,
                ", {:.2} turns of write-back a block",
                turns as f64 / blocks
            );
        }
        if engine.flushes_asked > 0 {
            let _ = write!(out, ", {} flushes asked", engine.flushes_asked);
        }
        if engine.neighbour_syncs > 0 {
            let _ = write!(out, ", {} syncs of the neighbour", engine.neighbour_syncs);
        }
        let clock = &engine.clock;
        let _ = write!(
            out,
            "\n- the machine beside it: of {} sleeps of {} µs, {} were late by 0.2 ms or more, {} \
             by 1 ms, {} by 5 ms and {} by 20 ms, the latest by {} µs",
            clock.sleeps,
            ClockProbe::ASKED.as_micros(),
            clock.late[0],
            clock.late[1],
            clock.late[2],
            clock.late[3],
            micros(clock.latest_ns),
        );
        let [median, p99, p999, longest] = quantiles(&engine.apply_ns).map(micros);
        let _ = writeln!(
            out,
            "\n- the operations of a block, µs: median {median}, 99% {p99}, 99.9% {p999}, \
             longest {longest}"
        );
        let [median, p99, p999, longest] = quantiles(&engine.off_ns()).map(micros);
        let _ = writeln!(
            out,
            "- the commit, off a processor, µs: median {median}, 99% {p99}, 99.9% {p999}, \
             longest {longest}"
        );
        for (part, name) in PARTS.iter().enumerate() {
            let Some(parts) = &engine.parts_ns else {
                break;
            };
            let of_part: Vec<u64> = parts.iter().map(|parts| parts[part]).collect();
            let [median, p99, p999, longest] = quantiles(&of_part).map(micros);
            let _ = writeln!(
                out,
                "- {name}, as the engine counts it, µs: median {median}, 99% {p99}, 99.9% \
                 {p999}, longest {longest}"
            );
        }
        for (of, (count, nanos)) in &engine.syncs {
            let _ = writeln!(
                out,
                "- syncs of {of}: {:.2} a block, {:.2} ms each",
                *count as f64 / blocks,
                *nanos as f64 / 1e6 / (*count).max(1) as f64
            );
        }
        for slow in &engine.slowest {
            let _ = write!(out, "- block {}: {} µs", slow.block, micros(slow.commit_ns));
            let off = slow.commit_ns.saturating_sub(slow.ran_ns);
            let _ = write!(out, ", {} of them off a processor", micros(off));
            if let Some(parts) = slow.parts_ns {
                let named: Vec<String> = PARTS
                    .iter()
                    .zip(parts)
                    .filter(|(_, nanos)| *nanos >= 1_000)
                    .map(|(name, nanos)| format!("{name} {}", micros(nanos)))
                    .collect();
                if !named.is_empty() {
                    let _ = write!(out, " ({})", named.join(", "));
                }
            }
            if let Some((calls, total, longest)) = slow.append_calls {
                let _ = write!(
                    out,
                    ", {calls} append calls on the log's files in {} µs, the longest {} µs",
                    micros(total),
                    micros(longest)
                );
            }
            if !slow.syncs_beside.is_empty() {
                let beside: Vec<String> = slow
                    .syncs_beside
                    .iter()
                    .take(SYNCS_LISTED)
                    .map(|(of, nanos)| format!("{of} {:.1} ms", *nanos as f64 / 1e6))
                    .collect();
                let _ = write!(out, "; syncs beside it: {}", beside.join(", "));
                if let Some(more) = slow.syncs_beside.len().checked_sub(SYNCS_LISTED + 1) {
                    let _ = write!(out, " and {} more", more + 1);
                }
            }
            out.push('\n');
        }
    }
    out
}

/// What a row of a table of several runs is made of.
struct RunRow {
    group: Group,
    blocks: usize,
    /// The median, the 99th and the 99.9th percentile and the longest of
    /// the commits.
    commit: [u64; 4],
    /// The same of the commits' time off a processor.
    off: [u64; 4],
    clock: ClockProbe,
}

fn run_rows(reports: &[LatencyReport]) -> Vec<RunRow> {
    let mut rows = Vec::new();
    for report in reports {
        for engine in &report.engines {
            rows.push(RunRow {
                group: group_of(report, engine),
                blocks: engine.commit_ns.len(),
                commit: quantiles(&engine.commit_ns),
                off: quantiles(&engine.off_ns()),
                clock: engine.clock,
            });
        }
    }
    rows
}

/// The median of the values of some runs with their range, in microseconds:
/// the middle value, or the mean of the two middle ones.
fn ranged(mut values: Vec<u64>) -> String {
    values.sort_unstable();
    let (Some(least), Some(most)) = (values.first(), values.last()) else {
        return "-".to_string();
    };
    let middle = values.len() / 2;
    let median = match values.len() % 2 {
        0 => (values[middle - 1] + values[middle]) / 2,
        _ => values[middle],
    };
    format!(
        "{} ({} to {})",
        micros(median),
        micros(*least),
        micros(*most)
    )
}

/// Several reports as tables. First a row for each engine of each run, in
/// the order given: the commit's percentiles, then the percentiles of its
/// time off a processor, and what the thread that only sleeps saw beside
/// the run. Then, for each build, engine and background that has more than
/// one run, the median of the runs' values with their range.
pub fn runs_table(reports: &[LatencyReport]) -> String {
    let rows = run_rows(reports);
    let mut out = String::from(
        "| Build | Engine | Background | Blocks | Median, µs | 99%, µs | 99.9%, µs | Longest, µs \
         | Off a processor: 99%, µs | 99.9%, µs | longest, µs | Sleeps late by 1 ms or more | \
         by 5 ms | by 20 ms |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | \
         --- | --- | --- |\n",
    );
    let mut groups: Vec<(&Group, Vec<&RunRow>)> = Vec::new();
    for row in &rows {
        let (build, engine, background) = &row.group;
        let [median, p99, p999, longest] = row.commit.map(micros);
        let [_, off_p99, off_p999, off_longest] = row.off.map(micros);
        let off = format!("{off_p99} | {off_p999} | {off_longest}");
        let share = |late: u64| {
            format!(
                "{:.2}%",
                100.0 * late as f64 / row.clock.sleeps.max(1) as f64
            )
        };
        let _ = writeln!(
            out,
            "| {build} | {engine} | {background} | {} | {median} | {p99} | {p999} | {longest} | \
             {off} | {} | {} | {} |",
            row.blocks,
            share(row.clock.late[1]),
            share(row.clock.late[2]),
            share(row.clock.late[3]),
        );
        match groups.iter_mut().find(|(other, _)| *other == &row.group) {
            Some((_, runs)) => runs.push(row),
            None => groups.push((&row.group, vec![row])),
        }
    }
    groups.retain(|(_, runs)| runs.len() > 1);
    if groups.is_empty() {
        return out;
    }
    out.push_str(
        "\nThe median of the runs' values, and their range:\n\n| Build | Engine | Background | \
         Runs | Median, µs | 99%, µs | 99.9%, µs | Longest, µs | Off a processor: 99%, µs | \
         99.9%, µs | longest, µs |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | \
         --- |\n",
    );
    for ((build, engine, background), runs) in groups {
        let _ = write!(
            out,
            "| {build} | {engine} | {background} | {} |",
            runs.len()
        );
        for column in 0..4 {
            let values = runs.iter().map(|run| run.commit[column]).collect();
            let _ = write!(out, " {} |", ranged(values));
        }
        for column in 1..4 {
            let values = runs.iter().map(|run| run.off[column]).collect();
            let _ = write!(out, " {} |", ranged(values));
        }
        out.push('\n');
    }
    out
}

fn parse<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, BenchError> {
    value
        .parse()
        .map_err(|_| BenchError::Check(format!("{flag} takes a number, not {value:?}")))
}

/// The options of `lignin-bench latency`, and the file to write the report
/// to.
pub fn parse_options(args: &[String]) -> Result<(LatencyOptions, Option<PathBuf>), BenchError> {
    let mut options = LatencyOptions::new(std::env::temp_dir().join("lignin-bench-latency"));
    let mut out = None;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--watch-files" => options.watch_files = true,
            "--keep" => options.keep = true,
            _ => {
                let value = args
                    .next()
                    .ok_or_else(|| BenchError::Check(format!("{flag} needs a value")))?;
                match flag.as_str() {
                    "--out" => out = Some(PathBuf::from(value)),
                    "--scratch" => options.scratch = PathBuf::from(value),
                    "--label" => options.label = Some(value.clone()),
                    "--commit" => options.commit = Some(value.clone()),
                    "--engine" => options.engines.push(
                        EngineChoice::named(value)
                            .ok_or_else(|| BenchError::Check(format!("no engine {value:?}")))?,
                    ),
                    "--background" => {
                        options.background = Background::named(value).ok_or_else(|| {
                            BenchError::Check(format!(
                                "--background takes default, heavy or quiet, not {value:?}"
                            ))
                        })?
                    }
                    "--set" => options.engine.set(value)?,
                    "--seed" => options.seed = parse(flag, value)?,
                    "--documents" => options.documents = parse(flag, value)?,
                    "--owners" => options.owners = parse(flag, value)?,
                    "--warm-blocks" => options.warm_blocks = parse(flag, value)?,
                    "--blocks" => options.blocks = parse(flag, value)?,
                    "--flush-blocks" => options.flush_blocks = parse(flag, value)?,
                    "--slowest" => options.slowest = parse(flag, value)?,
                    "--pause-us" => options.pause = Duration::from_micros(parse(flag, value)?),
                    "--mark-us" => options.mark = Duration::from_micros(parse(flag, value)?),
                    "--neighbour-ms" => {
                        options.neighbour = Some(Duration::from_millis(parse(flag, value)?))
                    }
                    _ => return Err(BenchError::Check(format!("unknown option {flag}"))),
                }
            }
        }
    }
    if options.engines.is_empty() {
        options.engines.push(EngineChoice::RocksDb);
    }
    if options.engine.pairs().any(|(key, _)| key == "pace") {
        return Err(BenchError::Check(
            "a latency run paces every engine by --pause-us, and takes no --set pace".to_string(),
        ));
    }
    options.check()?;
    Ok((options, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_quantile_is_the_smallest_sample_with_its_share_at_or_below_it() {
        let thousand: Vec<u64> = (1..=1000).collect();
        assert_eq!(quantile(&thousand, 0.5), 500);
        assert_eq!(quantile(&thousand, 0.99), 990);
        assert_eq!(quantile(&thousand, 0.999), 999);
        assert_eq!(quantile(&thousand, 1.0), 1000);
        assert_eq!(quantile(&[7], 0.999), 7);
        // A share that falls between two samples takes the upper one.
        assert_eq!(quantile(&[1, 2, 3], 0.5), 2);
        assert_eq!(quantile(&[1, 2, 3, 4], 0.3), 2);
        let fifteen_hundred: Vec<u64> = (1..=1500).collect();
        assert_eq!(quantile(&fifteen_hundred, 0.999), 1499);
        assert_eq!(quantile(&[], 0.5), 0);
        // One long commit in 2,400 is above the 99.9th percentile of
        // 100,000 commits 41 times: the percentile is among them.
        let mut tail: Vec<u64> = vec![1_000; 100_000];
        tail[..41].fill(70_000_000);
        assert_eq!(quantiles(&tail), [1_000, 1_000, 1_000, 70_000_000]);
        tail[..101].fill(70_000_000);
        assert_eq!(quantiles(&tail), [1_000, 1_000, 70_000_000, 70_000_000]);
    }

    #[test]
    fn options_parse() {
        let (options, out) = parse_options(&args(
            "--out r.json --scratch /s --label base --commit abc --engine rocksdb --background \
             heavy --set deferred=0 --seed 7 --documents 400 --owners 4 --warm-blocks 3 --blocks \
             9 --flush-blocks 2 --slowest 5 --pause-us 250 --mark-us 0 --neighbour-ms 4 \
             --watch-files --keep",
        ))
        .unwrap();
        assert_eq!(out, Some(PathBuf::from("r.json")));
        assert_eq!(options.scratch, PathBuf::from("/s"));
        assert_eq!(options.engines, vec![EngineChoice::RocksDb]);
        assert_eq!(options.background, Background::Heavy);
        assert_eq!(options.engine.list(), vec!["deferred=0".to_string()]);
        assert_eq!(
            (options.label.as_deref(), options.commit.as_deref()),
            (Some("base"), Some("abc"))
        );
        assert_eq!(
            (
                options.seed,
                options.documents,
                options.owners,
                options.warm_blocks,
                options.blocks,
                options.flush_blocks,
                options.slowest
            ),
            (7, 400, 4, 3, 9, 2, 5)
        );
        assert_eq!(options.pause, Duration::from_micros(250));
        assert_eq!(options.mark, Duration::ZERO);
        assert_eq!(options.neighbour, Some(Duration::from_millis(4)));
        assert!(options.watch_files && options.keep);

        let (defaults, out) = parse_options(&[]).unwrap();
        assert_eq!(out, None);
        assert_eq!(defaults.engines, vec![EngineChoice::RocksDb]);
        assert_eq!((defaults.blocks, defaults.documents), (100_000, 100_000));
        assert_eq!(defaults.background, Background::Default);

        for (line, message) in [
            ("--blocks", "--blocks needs a value"),
            ("--blocks x", "--blocks takes a number, not \"x\""),
            (
                "--background loud",
                "--background takes default, heavy or quiet, not \"loud\"",
            ),
            (
                "--documents 30",
                "--documents takes a multiple of 20 above 0",
            ),
            ("--neighbour-ms 0", "--neighbour-ms takes a number above 0"),
            (
                "--set pace=block",
                "a latency run paces every engine by --pause-us, and takes no --set pace",
            ),
            ("--frobnicate 1", "unknown option --frobnicate"),
        ] {
            assert_eq!(parse_options(&args(line)).unwrap_err().to_string(), message);
        }
    }

    #[test]
    fn files_are_told_apart_by_their_directory() {
        let db = Path::new("/scratch/lignindb");
        for (path, kind) in [
            ("MANIFEST.tmp", "the manifest's temporary file"),
            ("wal/00000004.log", "a log file"),
            ("wal/00000004.log.tmp", "a log file written again"),
            ("val/00000002.val", "a value-log file"),
            ("seg/00000009.seg", "a segment"),
            ("table/00000001.pt", "a table checkpoint"),
            ("MANIFEST", "another file"),
        ] {
            assert_eq!(kind_of(&db.join(path)), kind, "{path}");
        }
        assert_eq!(names_of(&db.join("wal")), "the names of wal/");
        assert_eq!(
            names_of(db),
            "the names of the database directory or of one above it"
        );
    }

    /// An engine that does nothing but note when its calls were made.
    struct Noting {
        noted: Mutex<Vec<(&'static str, Instant)>>,
    }

    struct NotingBlock<'a>(&'a Noting);

    impl Noting {
        fn note(&self, what: &'static str) {
            self.noted.lock().unwrap().push((what, Instant::now()));
        }
    }

    impl Block for NotingBlock<'_> {
        fn apply(
            &mut self,
            _op: &Op,
            _grove_version: &GroveVersion,
        ) -> Result<grovedb_costs::OperationCost, BenchError> {
            self.0.note("applied");
            Ok(Default::default())
        }

        fn commit(self) -> Result<(), BenchError> {
            self.0.note("commit began");
            self.0.note("commit ended");
            Ok(())
        }
    }

    impl Engine for Noting {
        type Block<'a> = NotingBlock<'a>;

        const NAME: &'static str = "noting";

        fn open(_dir: &Path) -> Result<Self, BenchError> {
            Ok(Noting {
                noted: Mutex::new(Vec::new()),
            })
        }

        fn dir(&self) -> &Path {
            Path::new("")
        }

        fn begin_block(&self) -> NotingBlock<'_> {
            self.note("block began");
            NotingBlock(self)
        }

        fn settle(&mut self) -> Result<(), BenchError> {
            Ok(())
        }

        fn read(
            &self,
            _op: &Op,
            _grove_version: &GroveVersion,
        ) -> Result<grovedb_costs::OperationCost, BenchError> {
            Ok(Default::default())
        }

        fn prove(
            &self,
            _query: &grovedb::PathQuery,
            _grove_version: &GroveVersion,
        ) -> Result<(Vec<u8>, grovedb_costs::OperationCost), BenchError> {
            Ok(Default::default())
        }

        fn root_hash(&self, _grove_version: &GroveVersion) -> Result<[u8; 32], BenchError> {
            Ok([0; 32])
        }

        fn checkpoint(&self, _target: &Path) -> Result<(), BenchError> {
            Ok(())
        }

        fn flush(&self) -> Result<(), BenchError> {
            Ok(())
        }

        fn wait_for_background(&mut self) -> Result<(), BenchError> {
            Ok(())
        }

        fn counters(&mut self) -> Result<crate::bench::Counters, BenchError> {
            Ok(Default::default())
        }
    }

    #[test]
    fn the_time_of_a_commit_is_of_the_commit_and_of_nothing_before_it() {
        let engine = Noting::open(Path::new("")).unwrap();
        let mut stream = LatencyOptions {
            engines: vec![EngineChoice::RocksDb],
            ..small(Path::new(""))
        }
        .stream();
        let (_, took, began, _) =
            block(&engine, &mut stream, GroveVersion::latest(), None).unwrap();
        let noted = engine.noted.lock().unwrap();
        let at = |what: &str| {
            let found = noted.iter().rev().find(|(noted, _)| *noted == what);
            found
                .unwrap_or_else(|| panic!("no call was noted as {what}"))
                .1
        };
        // The clock is read for the commit after the last operation was
        // applied and before the commit is called, and the time ends after
        // the commit has returned: by the order of the readings, whatever
        // their distance.
        assert!(noted.iter().any(|(what, _)| *what == "applied"));
        assert!(
            began >= at("applied"),
            "the time begins before an operation ended"
        );
        assert!(
            began <= at("commit began"),
            "the time begins inside the commit"
        );
        assert!(
            began + took >= at("commit ended"),
            "the time ends inside the commit"
        );
    }

    #[cfg(grovedb_engine_seam)]
    #[test]
    fn the_watching_layer_counts_each_sync_by_what_it_is_of() {
        use lignin_io::{Create, OpenMode, StdVfs, Vfs};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lignindb");
        for sub in ["wal", "val", "seg", "table"] {
            std::fs::create_dir_all(db.join(sub)).unwrap();
        }
        let watch: &'static Watch = Box::leak(Box::new(Watch::default()));
        let layer = Watching {
            inner: std::sync::Arc::new(StdVfs),
            watch,
        };
        let open = |name: &str| {
            layer
                .open(&db.join(name), OpenMode::Append(Create::New))
                .unwrap()
        };
        let (log, value, segment, table, manifest) = (
            open("wal/00000001.log"),
            open("val/00000001.val"),
            open("seg/00000001.seg"),
            open("table/00000001.pt"),
            open("MANIFEST.tmp"),
        );
        // What was synced before the measured blocks is forgotten.
        log.sync_data().unwrap();
        watch.begin();

        let from = Instant::now();
        log.append(b"a group").unwrap();
        log.append(b"a record").unwrap();
        value.append(b"a value").unwrap();
        value.sync_data().unwrap();
        value.sync_data().unwrap();
        manifest.sync_all().unwrap();
        layer.sync_dir(&db).unwrap();
        segment.sync_data().unwrap();
        table.sync_data().unwrap();
        log.sync_data().unwrap();
        layer.sync_dir(&db.join("wal")).unwrap();
        let to = Instant::now();

        let counted = watch.counted();
        let counts: Vec<(&str, u64)> = counted
            .iter()
            .map(|(of, (count, _))| (of.as_str(), *count))
            .collect();
        assert_eq!(
            counts,
            [
                ("a log file", 1),
                ("a segment", 1),
                ("a table checkpoint", 1),
                ("a value-log file", 2),
                ("the manifest's temporary file", 1),
                ("the names of the database directory or of one above it", 1),
                ("the names of wal/", 1),
            ]
        );
        // The appends to the log's files are the measuring thread's two,
        // and not the one to a value-log file.
        let (calls, total, longest) = watch.appends();
        assert_eq!(calls, 2);
        assert!(longest <= total);
        // An append of another thread is not the commit's.
        std::thread::scope(|scope| {
            scope.spawn(|| log.append(b"a page step").unwrap());
        });
        assert_eq!(watch.appends().0, 2);
        watch.reset_appends();
        assert_eq!(watch.appends(), (0, 0, 0));

        // Every sync that was made between two readings of the clock had a
        // moment in common with the time between them, and none that ended
        // before the first reading has.
        let beside: Vec<String> = (watch.beside(from, to).into_iter())
            .map(|(of, _)| of)
            .collect();
        assert_eq!(beside.len(), 8, "{beside:?}");
        assert_eq!(beside[0], "a value-log file");
        assert_eq!(beside[7], "the names of wal/");
        watch.trim(Instant::now());
        assert!(watch.beside(from, Instant::now()).is_empty());
    }

    fn small(scratch: &Path) -> LatencyOptions {
        LatencyOptions {
            documents: 400,
            owners: 4,
            warm_blocks: 2,
            blocks: 12,
            pause: Duration::from_micros(200),
            slowest: 3,
            ..LatencyOptions::new(scratch.to_path_buf())
        }
    }

    #[test]
    fn a_run_times_every_commit_of_its_blocks() {
        let scratch = tempfile::tempdir().unwrap();
        let options = LatencyOptions {
            engines: vec![EngineChoice::RocksDb],
            background: Background::Heavy,
            flush_blocks: 3,
            neighbour: Some(Duration::from_millis(1)),
            ..small(scratch.path())
        };
        let report = run(&options).unwrap();
        let [rocksdb] = &report.engines[..] else {
            panic!("one engine was asked for");
        };
        assert_eq!((rocksdb.commit_ns.len(), rocksdb.apply_ns.len()), (12, 12));
        assert!(rocksdb.commit_ns.iter().all(|nanos| *nanos > 0));
        assert_eq!(rocksdb.parts_ns, None);
        assert_eq!(rocksdb.turns, None);
        assert!(rocksdb.syncs.is_empty());
        // The thread's own clock is read around every commit, and a
        // thread does not run for longer than the commit takes by more
        // than the two clocks are read apart.
        assert_eq!(rocksdb.ran_ns.len(), 12);
        for (ran, commit) in rocksdb.ran_ns.iter().zip(&rocksdb.commit_ns) {
            assert!(*ran > 0 && *ran < commit + 1_000_000, "{ran} of {commit}");
        }
        assert!(rocksdb
            .slowest
            .iter()
            .all(|slow| slow.append_calls.is_none()));
        // A flush is asked for beside the commits, after every three blocks
        // at the most often: how many of the four are asked for follows how
        // the two threads are run.
        assert!(rocksdb.flushes_asked <= 4, "{}", rocksdb.flushes_asked);
        // A sleep that is late by more is late by less as well.
        assert!(rocksdb.clock.late.is_sorted_by(|more, fewer| more >= fewer));
        assert!(rocksdb.clock.late[0] <= rocksdb.clock.sleeps);
        let slowest: Vec<u64> = rocksdb.slowest.iter().map(|slow| slow.commit_ns).collect();
        let mut sorted = rocksdb.commit_ns.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(slowest, sorted[..3]);
        for slow in &rocksdb.slowest {
            assert_eq!(rocksdb.commit_ns[slow.block as usize], slow.commit_ns);
        }
        assert!(!scratch.path().join("rocksdb").exists());
        assert!(!scratch.path().join("neighbour-rocksdb").exists());

        let read = LatencyReport::from_json(&report.to_json()).unwrap();
        assert_eq!(read, report);
        let text = table(&report);
        assert!(text.contains("| - | rocksdb | heavy, a neighbour's sync every 1 ms | 12 |"));
        assert!(LatencyReport::from_json(&report.to_json().replace(SCHEMA, "other/1")).is_err());
    }

    #[cfg(grovedb_engine_seam)]
    #[test]
    fn both_engines_apply_one_stream_and_lignindb_tells_its_syncs() {
        let scratch = tempfile::tempdir().unwrap();
        let options = LatencyOptions {
            engines: vec![EngineChoice::RocksDb, EngineChoice::LigninDb],
            watch_files: true,
            label: Some("head".to_string()),
            background: Background::Quiet,
            ..small(scratch.path())
        };
        let report = run(&options).unwrap();
        let [rocksdb, lignindb] = &report.engines[..] else {
            panic!("two engines were asked for");
        };
        assert_eq!(rocksdb.root_hash, lignindb.root_hash);
        assert_eq!(lignindb.commit_ns.len(), 12);
        assert!(lignindb
            .settings
            .contains(&"cleaner_target=100".to_string()));
        assert_eq!(rocksdb.flushes_asked + lignindb.flushes_asked, 0);

        // Every commit of a block appends its group: the engine counts
        // the append, and the file layer timed the calls of each of the
        // slowest, the group's and those of its page steps.
        let parts = lignindb.parts_ns.as_ref().unwrap();
        assert_eq!(parts.len(), 12);
        for (parts, commit) in parts.iter().zip(&lignindb.commit_ns) {
            let appending = parts[2];
            assert!(appending > 0, "{parts:?}");
            assert!(
                parts.iter().sum::<u64>() <= *commit,
                "{parts:?} of {commit}"
            );
        }
        assert_eq!(lignindb.slowest.len(), 3);
        for slow in &lignindb.slowest {
            let (calls, total, longest) = slow.append_calls.unwrap();
            assert!(calls >= 1 && longest > 0 && longest <= total, "{slow:?}");
            assert!(total <= slow.commit_ns, "{slow:?}");
        }
        // The engine tells the turns of its write-back, and no sync of
        // the run is of a file that the harness cannot name.
        assert!(lignindb.turns.is_some());
        assert!(
            lignindb.syncs.keys().all(|of| of != "another file"),
            "{:?}",
            lignindb.syncs
        );

        let several = runs_table(&[report.clone(), report.clone()]);
        assert!(
            several.contains("| head | lignindb | quiet | 2 |"),
            "{several}"
        );
        assert_eq!(ranged(vec![9_000, 1_000]), "5 (1 to 9)");
        assert_eq!(ranged(vec![9_000, 1_000, 2_000]), "2 (1 to 9)");
        assert_eq!(ranged(Vec::new()), "-");
        let one = runs_table(std::slice::from_ref(&report));
        assert!(!one.contains("The median of the runs' values"), "{one}");
    }
}
