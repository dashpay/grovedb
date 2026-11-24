//! Background work and readers beside one writer, on GroveDB's own paths
//! (PERF-07): what a commit of a block costs its thread while write-back,
//! the cleaner and the reclaimer run, how reads scale with the number of
//! reading threads while the writer applies blocks, and how long the
//! slowest reads take.
//!
//! Both tests are measurements that every normal run leaves out. They want
//! a release build and a directory on a disk:
//!
//! ```text
//! TMPDIR=<a directory on a disk> cargo test --release -p lignin-testkit --test background -- \
//!     --ignored --nocapture --test-threads 1
//! ```
//!
//! The store is the plan's reference workload at a smaller size
//! ([`Documents::reference`]): documents of 400 bytes with one reference
//! each in the index of their owner, one document and one reference a state
//! transition, 20 transitions a block, each block one write transaction.
//!
//! | Setting | Default | Is |
//! | --- | --- | --- |
//! | `BG_DOCS` | 100000 | the documents that are loaded before anything is measured, about 100 to an owner, in whole blocks of 20: at least one block, and what is left over of a block is not loaded |
//! | `BG_STORE` | none | a directory in which the loaded database is kept: the first run makes it there, and every run measures on a copy of it |
//! | `BG_WARM_BLOCKS` | 200 | the blocks that are applied before the measured ones, so that the pool holds the pages a block touches |
//! | `BG_BLOCKS` | 3000 | the blocks whose commits are measured |
//! | `BG_CACHE_MB` | 512 | the buffer pool |
//! | `BG_BACKGROUND` | `default` | `heavy` for small segments and log files, a table checkpoint every megabyte of log and a cleaner that works towards a twentieth of dead space; `quiet` for a cleaner that moves nothing and no table checkpoint of write-back's loop before 64 GiB of log are held, which leaves the passes of write-back |
//! | `BG_SNAPSHOT_BLOCKS` | 0 | the writer's thread holds a snapshot of the engine and takes a new one every so many blocks, so that page versions are kept for a reader and given up in bursts; 0 holds none |
//! | `BG_PACE_US` | 0 | the pause of the writer between two blocks; 0 is back to back |
//! | `BG_SYNC_MS` | 0 | how long every sync of a file or a directory takes longer than the disk makes it, as on a device whose flush takes that long |
//! | `BG_READERS` | 1, 2, 4, 8, 12, 16, 24 up to the cores less one, and the cores less one | the numbers of reading threads |
//! | `BG_SECS` | 3 | the length of one run of readers |
//! | `BG_KINDS` | `get,proof,control` | the reads to measure. `control` is no read: each thread hashes bytes of its own, so its row is what the machine gives threads that share nothing, two of which may be on one core |
//! | `BG_WRITER` | `both` | `one` for the runs beside the writer only, `none` for those without |
//!
//! Each database of a measurement has a list of the syncs it makes
//! through its file layer, from its open on. The writer measurement
//! prints each of the slowest commits with the syncs whose time and the
//! commit's have a moment in common: of which file, and how long each
//! took. A sync is taken to last from a reading of the clock just before
//! its call to one just after the call returns, and a commit likewise.
//! That is of the whole commit, and says nothing of which part of the
//! commit a sync was beside.
//!
//! The time of a commit is given whole, and as the time its thread ran on a
//! processor during it (the thread's own clock) and the rest. The rest is
//! what the thread slept for and what it was kept off a processor for; a
//! wait that spins is in the first. The two clocks are read one after the
//! other.
//!
//! A reader's processor time is read from `/proc/thread-self/schedstat`,
//! as the snapshot layer's own measurement reads it
//! (`lignindb/src/snapshot/tests/scaling.rs`), so that a machine that
//! gives a thread less than a core is seen.

use std::{
    hint::black_box,
    io,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use grovedb::{engine::lignin::LigninEngine, GroveDb, PathQuery};
use grovedb_version::version::GroveVersion;
use lignin_io::{DirEntry, OpenMode, StdVfs, Vfs, VfsFile, VfsLock};
use lignin_store::{cleaner::CleanerConfig, flush::FlushConfig};
use lignin_testkit::workload::{apply, drive::Documents, Op, Rng, Stream};
use lignindb::{db::DbStats, Options};

type Grove = GroveDb<LigninEngine>;

const SEED: u64 = 1;
const STACK: usize = 64 << 20;

fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn text(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// The nanoseconds the calling thread has been on a processor, where the
/// system says.
fn on_cpu_nanos() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    stat.split_whitespace().next()?.parse().ok()
}

/// Durations by their size, in 32 steps to each power of two of
/// nanoseconds from 32 ns up and in one step to each power of two below.
/// A quantile is given as the upper edge of its step, or as the longest
/// duration if that is shorter: within a thirtieth for one of 32 ns and
/// more.
struct Histogram {
    counts: Vec<u64>,
    total: u64,
    longest: u64,
}

impl Histogram {
    const STEPS: u32 = 32;
    const BITS: u32 = 5;

    fn new() -> Self {
        Self {
            counts: vec![0; 64 * Self::STEPS as usize],
            total: 0,
            longest: 0,
        }
    }

    fn slot(nanos: u64) -> usize {
        let nanos = nanos.max(1);
        let power = 63 - nanos.leading_zeros();
        let step = match power {
            0..Self::BITS => 0,
            _ => (nanos >> (power - Self::BITS)) & u64::from(Self::STEPS - 1),
        };
        (power * Self::STEPS) as usize + step as usize
    }

    /// The upper edge of a slot, in nanoseconds.
    fn edge(slot: usize) -> u64 {
        let (power, step) = (
            slot as u32 / Self::STEPS,
            slot as u64 % u64::from(Self::STEPS),
        );
        match power {
            0..Self::BITS => 1 << (power + 1),
            _ => (1 << power) + ((step + 1) << (power - Self::BITS)),
        }
    }

    fn add(&mut self, nanos: u64) {
        self.counts[Self::slot(nanos)] += 1;
        self.total += 1;
        self.longest = self.longest.max(nanos);
    }

    fn merge(&mut self, other: &Self) {
        for (count, more) in self.counts.iter_mut().zip(&other.counts) {
            *count += more;
        }
        self.total += other.total;
        self.longest = self.longest.max(other.longest);
    }

    /// The duration that `share` of the samples are at or below, in
    /// microseconds.
    fn quantile(&self, share: f64) -> f64 {
        let wanted = ((self.total as f64) * share).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (slot, count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= wanted {
                return Self::edge(slot).min(self.longest) as f64 / 1e3;
            }
        }
        self.longest as f64 / 1e3
    }

    fn row(&self) -> String {
        format!(
            "{:.1} | {:.1} | {:.1} | {:.1} | {:.1}",
            self.quantile(0.5),
            self.quantile(0.9),
            self.quantile(0.99),
            self.quantile(0.999),
            self.longest as f64 / 1e3
        )
    }
}

/// The syncs that one database makes through its file layer
/// ([`SlowSync`]), so that a commit can be told which of them were under
/// way beside it. Every sync of that database is listed, from its open on.
///
/// A sync is taken to last from a reading of the clock just before its
/// call, the pause included, to a reading just after the call returns.
#[derive(Debug, Default)]
struct Syncs {
    listed: Mutex<Listed>,
}

#[derive(Debug, Default)]
struct Listed {
    /// The syncs that have begun and whose end is not listed: a number,
    /// when, of what.
    under_way: Vec<(u64, Instant, String)>,
    /// The syncs whose end is listed: when each began and ended, and of
    /// what. They are kept until [`Syncs::forget_ended`].
    ended: Vec<(Instant, Instant, String)>,
    next: u64,
}

impl Syncs {
    /// Makes the sync `sync` of `of`, `pause` longer than it takes, and
    /// lists it.
    fn make<T>(&self, of: &str, pause: Duration, sync: impl FnOnce() -> T) -> T {
        let mut listed = self.listed.lock().expect("the syncs");
        let number = listed.next;
        listed.next += 1;
        let began = Instant::now();
        listed.under_way.push((number, began, of.to_owned()));
        drop(listed);
        thread::sleep(pause);
        let synced = sync();
        let ended = Instant::now();
        let mut listed = self.listed.lock().expect("the syncs");
        listed.under_way.retain(|(other, ..)| *other != number);
        listed.ended.push((began, ended, of.to_owned()));
        synced
    }

    /// Forgets the syncs whose end is listed.
    fn forget_ended(&self) {
        self.listed.lock().expect("the syncs").ended.clear();
    }

    /// For each of `commits`, given as when it began and ended, the syncs
    /// whose time and the commit's have a moment in common: of what, and
    /// how long each took, in milliseconds. Waits until the end of every
    /// sync that began before the last of the commits ended is listed, so
    /// each sync is judged by both of its readings.
    fn beside(&self, commits: &[(Instant, Instant)]) -> Vec<Vec<String>> {
        let Some(last) = commits.iter().map(|(_, ended)| *ended).max() else {
            return Vec::new();
        };
        let listed = loop {
            let listed = self.listed.lock().expect("the syncs");
            if listed.under_way.iter().all(|(_, began, _)| *began >= last) {
                break listed;
            }
            drop(listed);
            thread::sleep(Duration::from_millis(1));
        };
        let beside = |(from, to): &(Instant, Instant)| {
            let over = listed
                .ended
                .iter()
                .filter(|(began, ended, _)| began < to && ended > from);
            over.map(|(began, ended, of)| format!("{of} {:.1}", ms(*ended - *began)))
                .collect::<Vec<_>>()
        };
        commits.iter().map(beside).collect()
    }
}

/// The name of a file or a directory in the listing of syncs: its last
/// two names.
fn named(path: &Path) -> String {
    let mut names: Vec<_> = path.iter().rev().take(2).collect();
    names.reverse();
    names
        .iter()
        .map(|name| name.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The file layer of the system, with every sync made to take `pause`
/// longer, as on a device whose flush takes that long, and listed
/// ([`Syncs`]). A set of syncs is made one after the other here, also
/// where the system's own file layer would have them share a flush.
#[derive(Debug)]
struct SlowSync {
    pause: Duration,
    syncs: Arc<Syncs>,
}

#[derive(Debug)]
struct SlowFile {
    file: Box<dyn VfsFile>,
    name: String,
    pause: Duration,
    syncs: Arc<Syncs>,
}

impl VfsFile for SlowFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.file.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.file.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.file.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.syncs
            .make(&self.name, self.pause, || self.file.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.syncs
            .make(&self.name, self.pause, || self.file.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
}

impl Vfs for SlowSync {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let file = StdVfs.open(path, mode)?;
        Ok(Box::new(SlowFile {
            file,
            name: named(path),
            pause: self.pause,
            syncs: Arc::clone(&self.syncs),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdVfs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        StdVfs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let of = format!("the names of {}", named(path));
        self.syncs.make(&of, self.pause, || StdVfs.sync_dir(path))
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock_existing(path)
    }
}

/// The options of the run, and the list of the syncs that a database
/// opened with them makes.
fn options() -> (Options, Arc<Syncs>) {
    let mut options = Options {
        cache_size: setting("BG_CACHE_MB", 512usize) << 20,
        ..Options::default()
    };
    let pause = Duration::from_millis(setting("BG_SYNC_MS", 0));
    let syncs = Arc::new(Syncs::default());
    options.vfs = Arc::new(SlowSync {
        pause,
        syncs: Arc::clone(&syncs),
    });
    let size = lignin_format::PageSize::DEFAULT;
    match text("BG_BACKGROUND", "default").as_str() {
        "quiet" => {
            options.flush = Some(FlushConfig {
                checkpoint_log_bytes: u64::MAX,
                max_log_held: 64 << 30,
                ..FlushConfig::new(size)
            });
            options.cleaner = Some(CleanerConfig {
                target_dead_percent: 100,
                checkpoint_dead_percent: 100,
                seal_dead_percent: 100,
                value_dead_percent: 100,
                ..CleanerConfig::new()
            });
        }
        "heavy" => {
            options.log_file_len = Some(4 << 20);
            options.flush = Some(FlushConfig {
                segment_len: 4 << 20,
                checkpoint_log_bytes: 1 << 20,
                ..FlushConfig::new(size)
            });
            options.cleaner = Some(CleanerConfig {
                target_dead_percent: 5,
                checkpoint_dead_percent: 5,
                seal_dead_percent: 5,
                min_segment_len: 1 << 20,
                segment_len: 4 << 20,
                value_dead_percent: 5,
                ..CleanerConfig::new()
            });
        }
        _ => {}
    }
    (options, syncs)
}

fn open(dir: &Path) -> (Grove, Arc<Syncs>) {
    let (options, syncs) = options();
    let engine = LigninEngine::open_with(dir, &options).expect("the database opens");
    (GroveDb::from_engine(engine), syncs)
}

fn stats(db: &Grove) -> DbStats {
    db.engine().database().stats()
}

/// The operations of the next block of `stream`, its commit left out.
fn next_block(stream: &mut Stream) -> Vec<Op> {
    let mut block = Vec::new();
    for op in stream.by_ref() {
        if matches!(op, Op::Commit) {
            break;
        }
        block.push(op);
    }
    assert!(!block.is_empty(), "the stream has ended");
    block
}

/// How long the calling thread has run on a processor, by its own clock.
fn on_cpu() -> Duration {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

/// One block that was applied and committed.
struct Applied {
    /// How long the operations took.
    operations: Duration,
    /// When the commit began and ended.
    commit: (Instant, Instant),
    /// How long the thread ran on a processor during the commit.
    ran: Duration,
}

/// Applies one block in a write transaction of its own and commits it.
fn block(db: &Grove, ops: &[Op], version: &GroveVersion) -> Applied {
    let transaction = db.start_transaction();
    let began = Instant::now();
    for op in ops {
        apply(db, op, Some(&transaction), version)
            .value
            .expect("an operation of the block");
    }
    let operations = began.elapsed();
    let (began, ran) = (Instant::now(), on_cpu());
    db.commit_transaction(transaction)
        .value
        .expect("the commit of the block");
    let ended = Instant::now();
    Applied {
        operations,
        commit: (began, ended),
        ran: on_cpu().saturating_sub(ran),
    }
}

struct Loaded {
    documents: Documents,
    stream: Stream,
    loaded: u64,
    /// The syncs of the database, from its open on.
    syncs: Arc<Syncs>,
}

/// The workload: a stream of the reference workload's shape that never
/// ends in a run, with `BG_DOCS` documents to load, and the blocks that
/// load them.
fn workload() -> (Documents, u64, u64) {
    let per_block = u64::from(Documents::reference().transitions_per_block);
    let asked: u64 = setting("BG_DOCS", 100_000);
    let loaded = (asked / per_block).max(1) * per_block;
    let documents = Documents {
        documents: loaded + 100_000_000,
        owners: (loaded / 100).max(1),
        ..Documents::reference()
    };
    // The block that sets the contract up comes first and holds no
    // document.
    let blocks = 1 + loaded / per_block;
    (documents, loaded, blocks)
}

/// Applies the blocks that load the documents to the database in `dir`,
/// with the options of a default session, and closes it.
fn load_into(dir: &Path, version: &GroveVersion) {
    let (documents, loaded, blocks) = workload();
    let engine = LigninEngine::open_with(dir, &Options::default()).expect("a new database");
    let db: Grove = GroveDb::from_engine(engine);
    let mut stream = documents.stream(SEED);
    let began = Instant::now();
    for _ in 0..blocks {
        block(&db, &next_block(&mut stream), version);
    }
    println!(
        "loaded {loaded} documents in {blocks} blocks in {:.1} s",
        began.elapsed().as_secs_f64()
    );
    drop(db);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a directory");
    for entry in std::fs::read_dir(from).expect("a directory") {
        let entry = entry.expect("an entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a kind").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a copy");
        }
    }
}

/// A database with `BG_DOCS` documents in `dir`, opened with the options
/// of the run, warm, and the stream at the block after the last one it
/// holds. With `BG_STORE` the loaded database is made once, there, and
/// `dir` gets a copy of it.
fn load(dir: &Path, version: &GroveVersion) -> (Grove, Loaded) {
    let (documents, loaded, blocks) = workload();
    match std::env::var_os("BG_STORE") {
        Some(kept) => {
            // The database is loaded in a directory of this run's own
            // beside its place, and gets its name when the load is whole.
            // Whoever loads it at the same time has another directory,
            // and the first to end gives the name.
            let kept = Path::new(&kept);
            std::fs::create_dir_all(kept).expect("the directory of kept stores");
            let whole = kept.join(format!("{loaded}-documents"));
            if !whole.exists() {
                let loading = tempfile::Builder::new()
                    .prefix(&format!("{loaded}-loading-"))
                    .tempdir_in(kept)
                    .expect("a directory to load in");
                let store = loading.path().join("store");
                load_into(&store, version);
                if std::fs::rename(&store, &whole).is_err() {
                    assert!(whole.exists(), "the loaded database got no name");
                }
            }
            copy_dir(&whole, dir);
        }
        None => load_into(dir, version),
    }
    let (db, syncs) = open(dir);
    let mut stream = documents.stream(SEED);
    for _ in 0..blocks {
        next_block(&mut stream);
    }
    let warm: u64 = setting("BG_WARM_BLOCKS", 200);
    for _ in 0..warm {
        block(&db, &next_block(&mut stream), version);
    }
    let loaded = Loaded {
        documents,
        stream,
        loaded,
        syncs,
    };
    (db, loaded)
}

fn machine() {
    let cores = thread::available_parallelism().map_or(1, |cores| cores.get());
    println!(
        "{cores} cores, background {}, pool {} MiB, every sync {} ms longer",
        text("BG_BACKGROUND", "default"),
        setting("BG_CACHE_MB", 512usize),
        setting("BG_SYNC_MS", 0u64)
    );
}

fn on_a_big_stack(run: impl FnOnce() + Send + 'static) {
    thread::Builder::new()
        .stack_size(STACK)
        .spawn(run)
        .expect("a thread")
        .join()
        .expect("the measurement");
}

fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1e3
}

/// The parts of one commit's time that the engine counts apart, between
/// two looks at the engine with no other committer: the waits for the
/// commit lock and for the lock on the counts of values, the append to
/// the log, the placing of values and the reclaiming after the commit,
/// and the wait for room.
fn parts_of(before: &DbStats, after: &DbStats) -> [Duration; 6] {
    let (b, a) = (&before.commits, &after.commits);
    [
        a.lock_waited - b.lock_waited,
        a.counts_waited - b.counts_waited,
        a.appending - b.appending,
        a.placing - b.placing,
        a.reclaiming - b.reclaiming,
        after.store.flush.room_waited - before.store.flush.room_waited,
    ]
}

/// What the engine counted between two looks, as one line.
fn since(before: &DbStats, after: &DbStats) -> String {
    let (b, a) = (&before.store, &after.store);
    let [lock, counts, appending, placing, reclaiming, room] = parts_of(before, after);
    format!(
        "commits {}, log {} B, flush {} B in {} base and {} delta blocks, cleaner {} B in {} \
         passes, table checkpoints {} (put off {}), values placed {}, steps put off {}, room \
         waits {} ({} stalled, {:.2} ms), page reads {}, cache hits {}, values read from the \
         value log {} ({} B); a committer waited \
         {:.2} ms for the commit lock and {:.2} ms for the counts lock, appended for {:.2} ms, \
         placed for {:.2} ms and reclaimed for {:.2} ms",
        after.commits.published - before.commits.published,
        a.log_bytes - b.log_bytes,
        a.flush_bytes - b.flush_bytes,
        a.flush.base_blocks - b.flush.base_blocks,
        a.flush.delta_blocks - b.flush.delta_blocks,
        a.cleaner_bytes - b.cleaner_bytes,
        after.cleaner.passes - before.cleaner.passes,
        a.flush.checkpoints - b.flush.checkpoints,
        a.flush.checkpoints_put_off - b.flush.checkpoints_put_off,
        a.flush.values_placed - b.flush.values_placed,
        a.flush.steps_put_off - b.flush.steps_put_off,
        a.flush.room_waits - b.flush.room_waits,
        a.flush.room_stalls - b.flush.room_stalls,
        ms(room),
        a.page_reads - b.page_reads,
        a.cache_hits - b.cache_hits,
        a.value_reads - b.value_reads,
        a.value_read_bytes - b.value_read_bytes,
        ms(lock),
        ms(counts),
        ms(appending),
        ms(placing),
        ms(reclaiming),
    )
}

#[test]
#[ignore = "a measurement: minutes, a release build and a disk"]
fn writer_latency_beside_background_work() {
    on_a_big_stack(|| {
        let version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("a directory");
        machine();
        let (db, loaded) = load(dir.path(), version);
        let Loaded {
            mut stream, syncs, ..
        } = loaded;
        let blocks: u64 = setting("BG_BLOCKS", 3_000);
        let pace = Duration::from_micros(setting("BG_PACE_US", 0));
        let renew: u64 = setting("BG_SNAPSHOT_BLOCKS", 0);
        let engine = db.engine().database();
        let mut snapshot = None;

        syncs.forget_ended();
        let before = stats(&db);
        let (mut commits, mut applies) = (Histogram::new(), Histogram::new());
        let mut own = Histogram::new();
        let (mut running, mut waiting) = (Histogram::new(), Histogram::new());
        let mut parts: [Histogram; 6] = std::array::from_fn(|_| Histogram::new());
        let mut slowest: Vec<(u64, u64, String, (Instant, Instant))> = Vec::new();
        let began = Instant::now();
        for number in 0..blocks {
            if renew > 0 && number % renew == 0 {
                drop(snapshot.take());
                snapshot = Some(engine.snapshot());
            }
            let ops = next_block(&mut stream);
            let look = stats(&db);
            let Applied {
                operations: applied,
                commit: (began, ended),
                ran,
            } = block(&db, &ops, version);
            let committed = ended - began;
            let then = stats(&db);
            let nanos = committed.as_nanos() as u64;
            applies.add(applied.as_nanos() as u64);
            commits.add(nanos);
            running.add(ran.as_nanos() as u64);
            waiting.add(committed.saturating_sub(ran).as_nanos() as u64);
            let spent = parts_of(&look, &then);
            for (part, time) in parts.iter_mut().zip(spent) {
                part.add(time.as_nanos() as u64);
            }
            own.add(committed.saturating_sub(spent.iter().sum()).as_nanos() as u64);
            if slowest.len() < 12 || nanos > slowest[slowest.len() - 1].0 {
                slowest.push((nanos, number, since(&look, &then), (began, ended)));
                slowest.sort_by_key(|slow| std::cmp::Reverse(slow.0));
                slowest.truncate(12);
            }
            if !pace.is_zero() {
                thread::sleep(pace);
            }
        }
        let seconds = began.elapsed().as_secs_f64();
        let after = stats(&db);
        drop(snapshot);

        println!(
            "\n### {blocks} blocks of 20 transitions in {seconds:.1} s, pause {pace:?}, a \
             snapshot renewed every {renew} blocks\n"
        );
        println!("| µs | median | 90% | 99% | 99.9% | longest |");
        println!("| --- | --- | --- | --- | --- | --- |");
        println!("| the commit of a block | {} |", commits.row());
        println!("| the commit, on a processor | {} |", running.row());
        println!("| the commit, off the processor | {} |", waiting.row());
        println!("| the operations of a block | {} |", applies.row());
        println!("| the commit without the six rows below | {} |", own.row());
        let names = [
            "its wait for the commit lock",
            "its wait for the lock on the counts of values",
            "its append to the log",
            "placing values after it",
            "reclaiming versions after it",
            "its wait for room",
        ];
        for (name, part) in names.iter().zip(&parts) {
            println!("| {name} | {} |", part.row());
        }
        println!("\nthe engine, over the blocks: {}", since(&before, &after));
        println!("\nthe slowest commits (µs, block, what the engine counted around it):");
        let commits: Vec<_> = slowest.iter().map(|(.., commit)| *commit).collect();
        let beside = syncs.beside(&commits);
        for ((nanos, number, counted, _), syncs) in slowest.iter().zip(beside) {
            let syncs = match syncs.is_empty() {
                true => "no sync beside it".to_owned(),
                false => format!("syncs beside it (ms): {}", syncs.join(", ")),
            };
            println!(
                "- {:.1} at block {number}: {counted}; {syncs}",
                *nanos as f64 / 1e3
            );
        }
        println!(
            "\n{}",
            after
                .background
                .last_error
                .as_deref()
                .unwrap_or("no background error")
        );
        drop(db);
    });
}

#[derive(Clone, Copy)]
enum Kind {
    Get,
    Proof,
    Control,
}

/// What a thread of the control row does in place of a read: it hashes
/// 4 KiB of a megabyte of its own, at a place that moves. It reads and
/// writes nothing that another thread reads or writes.
fn control(own: &mut [u8], at: u64) {
    let from = (at as usize % (own.len() / 4096)) * 4096;
    let hash = blake3::hash(&own[from..from + 4096]);
    own[from..from + 32].copy_from_slice(hash.as_bytes());
}

struct Run {
    rate: f64,
    per_cpu_second: Option<f64>,
    share: Option<f64>,
    commits: f64,
    times: Histogram,
}

/// Reads for `length` on `readers` threads, beside the writer if there is
/// one.
fn run(
    db: &Grove,
    loaded: &Mutex<Loaded>,
    kind: Kind,
    readers: u32,
    writer: bool,
    length: Duration,
) -> Run {
    let version = GroveVersion::latest();
    let (documents, count) = {
        let loaded = loaded.lock().expect("the stream");
        (loaded.documents, loaded.loaded)
    };
    let primary = documents.primary_path(SEED);
    let pace = Duration::from_micros(setting("BG_PACE_US", 0));
    let start = Barrier::new(readers as usize + usize::from(writer) + 1);
    let go = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let total = AtomicU64::new(0);
    let cpu_nanos = AtomicU64::new(0);
    let committed = AtomicU64::new(0);
    let times = Mutex::new(Histogram::new());
    let mut seconds = 0.0;
    thread::scope(|scope| {
        for reader in 0..readers {
            let (start, go, stop, primary) = (&start, &go, &stop, &primary);
            let (total, cpu_nanos, times) = (&total, &cpu_nanos, &times);
            let work = move || {
                let mut rng = Rng::derive(SEED, 0x7265_6164, u64::from(reader));
                let mut own = Histogram::new();
                let mut reads = 0u64;
                let mut bytes = vec![reader as u8; 1 << 20];
                wait_for(start, go);
                let before = on_cpu_nanos();
                while !stop.load(Ordering::Relaxed) {
                    let index = rng.below(count);
                    let began = Instant::now();
                    match kind {
                        Kind::Get => {
                            let key = documents.document_id(SEED, index);
                            let element = db.get(primary.as_slice(), &key, None, version);
                            black_box(element.value.expect("a document"));
                        }
                        Kind::Proof => {
                            let query: PathQuery =
                                documents.point_query(SEED, index).to_path_query();
                            let proof = db.prove_query(&query, None, version);
                            black_box(proof.value.expect("a proof"));
                        }
                        Kind::Control => control(&mut bytes, index),
                    }
                    own.add(began.elapsed().as_nanos() as u64);
                    reads += 1;
                }
                if let (Some(before), Some(after)) = (before, on_cpu_nanos()) {
                    cpu_nanos.fetch_add(after - before, Ordering::Relaxed);
                }
                total.fetch_add(reads, Ordering::Relaxed);
                times.lock().expect("the times").merge(&own);
            };
            thread::Builder::new()
                .stack_size(STACK)
                .spawn_scoped(scope, work)
                .expect("a reader");
        }
        if writer {
            let (start, go, stop, committed) = (&start, &go, &stop, &committed);
            let work = move || {
                let mut loaded = loaded.lock().expect("the stream");
                wait_for(start, go);
                while !stop.load(Ordering::Relaxed) {
                    let ops = next_block(&mut loaded.stream);
                    block(db, &ops, version);
                    if !stop.load(Ordering::Relaxed) {
                        committed.fetch_add(1, Ordering::Relaxed);
                    }
                    if !pace.is_zero() {
                        thread::sleep(pace);
                    }
                }
            };
            thread::Builder::new()
                .stack_size(STACK)
                .spawn_scoped(scope, work)
                .expect("the writer");
        }
        start.wait();
        let started = Instant::now();
        go.store(true, Ordering::SeqCst);
        thread::sleep(length);
        stop.store(true, Ordering::Relaxed);
        seconds = started.elapsed().as_secs_f64();
    });
    let reads = total.load(Ordering::Relaxed) as f64;
    let cpu_seconds = cpu_nanos.load(Ordering::Relaxed) as f64 / 1e9;
    let measured = cpu_seconds > 0.0;
    Run {
        rate: reads / seconds,
        per_cpu_second: measured.then(|| reads / cpu_seconds),
        share: measured.then(|| cpu_seconds / seconds / f64::from(readers)),
        commits: committed.load(Ordering::Relaxed) as f64 / seconds,
        times: times.into_inner().expect("the times"),
    }
}

/// [`run`], made again while it was to be beside the writer and the writer
/// committed no block in it: such a run was not beside a writer.
fn beside(
    db: &Grove,
    loaded: &Mutex<Loaded>,
    kind: Kind,
    readers: u32,
    writer: bool,
    length: Duration,
) -> Run {
    for _ in 0..12 {
        let run = run(db, loaded, kind, readers, writer, length);
        if !writer || run.commits > 0.0 {
            return run;
        }
    }
    panic!("the writer committed no block in twelve runs");
}

/// Waits until every thread of the run is there, and then for the signal
/// that the run's clock has been read.
fn wait_for(start: &Barrier, go: &AtomicBool) {
    start.wait();
    while !go.load(Ordering::SeqCst) {
        std::hint::spin_loop();
    }
}

#[test]
#[ignore = "a measurement: minutes, a release build, a disk and all the cores"]
fn reader_throughput_beside_one_writer() {
    on_a_big_stack(|| {
        let version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("a directory");
        machine();
        let (db, loaded) = load(dir.path(), version);
        let loaded = Mutex::new(loaded);
        let cores = thread::available_parallelism().map_or(1, |cores| cores.get() as u32);
        let most = cores.saturating_sub(1).max(1);
        let threads: Vec<u32> = match std::env::var("BG_READERS") {
            Ok(list) => list
                .split(',')
                .filter_map(|n| n.trim().parse().ok())
                .collect(),
            Err(_) => {
                let mut counts: Vec<u32> = [1, 2, 4, 8, 12, 16, 24]
                    .into_iter()
                    .filter(|n| *n < most)
                    .collect();
                counts.push(most);
                counts
            }
        };
        assert!(!threads.is_empty() && threads.iter().all(|n| *n >= 1));
        let length = Duration::from_secs_f64(setting("BG_SECS", 3.0));
        let kinds = text("BG_KINDS", "get,proof,control");
        let writers = text("BG_WRITER", "both");
        let optional = |value: Option<f64>, scale: f64| {
            value.map_or_else(|| "?".to_owned(), |value| format!("{:.0}", value * scale))
        };

        for (writer, which) in [(true, "one"), (false, "none")] {
            if writers != "both" && writers != which {
                continue;
            }
            let all = [
                (Kind::Get, "get"),
                (Kind::Proof, "proof"),
                (Kind::Control, "control"),
            ];
            for (kind, name) in all {
                if !kinds.split(',').any(|kind| kind.trim() == name) {
                    continue;
                }
                let before = stats(&db);
                println!("\n### {name}, {which} writer, {length:?} a run\n");
                println!(
                    "| readers | reads/s | speedup | reads per CPU second of a reader | of \
                     one reader's | core share % | commits/s | read µs: median | 90% | 99% | \
                     99.9% | longest |"
                );
                println!(
                    "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
                );
                let mut alone: Option<(f64, Option<f64>)> = None;
                for &readers in &threads {
                    if alone.is_none() && readers != 1 {
                        let one = beside(&db, &loaded, kind, 1, writer, length);
                        alone = Some((one.rate, one.per_cpu_second));
                    }
                    let run = beside(&db, &loaded, kind, readers, writer, length);
                    let (alone_rate, alone_cpu) =
                        *alone.get_or_insert((run.rate, run.per_cpu_second));
                    let of_one = match (run.per_cpu_second, alone_cpu) {
                        (Some(each), Some(one)) => format!("{:.0}%", each / one * 100.0),
                        _ => "?".to_owned(),
                    };
                    println!(
                        "| {readers} | {:.0} | {:.2} | {} | {of_one} | {} | {:.0} | {} |",
                        run.rate,
                        run.rate / alone_rate,
                        optional(run.per_cpu_second, 1.0),
                        optional(run.share, 100.0),
                        run.commits,
                        run.times.row(),
                    );
                }
                println!(
                    "\nthe engine, over these runs: {}",
                    since(&before, &stats(&db))
                );
            }
        }
        drop(db);
    });
}

#[test]
fn a_histogram_gives_its_quantiles_within_a_thirtieth() {
    let mut times = Histogram::new();
    for nanos in 1..=10_000u64 {
        times.add(nanos * 1_000);
    }
    for (share, micros) in [(0.5, 5_000.0), (0.9, 9_000.0), (0.99, 9_900.0)] {
        let given = times.quantile(share);
        assert!(
            given >= micros && given <= micros * 1.04,
            "{share}: {given}"
        );
    }
    assert_eq!(times.quantile(1.0), 10_000.0);
    let mut more = Histogram::new();
    more.add(50_000_000);
    times.merge(&more);
    assert_eq!(times.total, 10_001);
    assert_eq!(times.quantile(1.0), 50_000.0);
    assert_eq!(Histogram::slot(0), Histogram::slot(1));
    assert!(Histogram::edge(Histogram::slot(5)) >= 5);
}
