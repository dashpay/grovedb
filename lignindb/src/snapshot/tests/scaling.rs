//! Reader throughput by the number of reader threads, with one writer
//! committing all the time: the acceptance measurement of snapshot reads.
//!
//! It is a test that every normal run leaves out:
//! `TMPDIR=<a directory on disk> cargo test --release -p lignindb --lib -- --ignored --nocapture reader_throughput`,
//! with these settings in the environment:
//!
//! | Setting | Default | Is |
//! | --- | --- | --- |
//! | `SECS` | 1 | the length of one run; three are made of each, and the median is given |
//! | `THREADS` | 1, 2, 4, 8, 14, 20, 28, 56 up to the cores, and the cores | the numbers of reader threads |
//! | `COMMITS` | 2000 | the commits the writer makes in a second |
//! | `KINDS` | the first four | the kinds of read to measure, by name, with commas between them |
//! | `SPREAD` | 64 | the pages of its own that a reader of `many pages` goes round, four at least |
//! | `POOL` | twice the pages | the size of the pool, as the bytes of that many pages |
//! | `WRITER` | both | `one` for the runs beside a writer only, `none` for those without |
//!
//! One read is what a read with no transaction does (`transactions.md`
//! RD2): take a snapshot, begin an operation, read some pages at the
//! pinned version, and drop both. Four kinds are measured:
//!
//! - `pins`: the snapshot and the operation alone, no page
//! - `lease`: the same, and the snapshot takes its lease
//! - `own pages`: four pages that no other reader reads
//! - `shared root`: one page that every reader reads, then three of its own
//! - `many pages`: four pages of `SPREAD` that no other reader reads, the
//!   next four at each read. It is measured when `KINDS` names it. With a
//!   `POOL` smaller than `SPREAD`, a page that a read asks for can have
//!   left the pool, whatever the number of readers: it is then loaded,
//!   and a frame can be evicted for it, on the thread of the reader that
//!   asked, and so on several threads at once. An eviction
//!   looks through the lanes in which threads announce their pins
//!   (`lignin_store::cache`). The pool's counts of misses and evictions
//!   are printed at the end.
//!
//! The pool has at most 128 lanes. `THREADS` above that, on a machine
//! with fewer cores, is the measurement of readers that share lanes: the
//! reads in a second of a reader's processor time are what to read then,
//! since the threads share the cores too.
//!
//! The writer writes the shared page and two others again in every commit,
//! writes them back, and asks for a reclaim. Each kind is also run with no
//! writer.
//!
//! A machine that other work shares gives a reader less than a whole core,
//! so beside the reads in a second of the clock the table has the reads in
//! a second of the readers' own processor time, from
//! `/proc/thread-self/schedstat`, and the share of a core each reader got.
//!
//! The last column of a table is the number of cells that the readers of
//! the run start at when they look for a vacant cell for a pin or a guard
//! (`snapshot::slots`): it is the number of readers where each starts at a
//! cell of its own.
//!
//! Every thread of a run waits for one signal, which is given after the
//! clock was read, so the reads and the commits that are counted are
//! those that began between that reading and the signal that stops them.
//! A run with a writer that made no commit is made again.
//!
//! The store has one page for all readers and four for each reader, or
//! `SPREAD` where `many pages` is measured, of 8 KiB, in a pool of twice
//! as many frames unless `POOL` says otherwise. It keeps the pages of the
//! latest version only and trims its page table, so its memory does not
//! grow with the length of the run; its segment file grows by three small
//! blocks a commit. The peak of memory is printed at the end, and so is the
//! number of reads that were counted in all the runs together: with
//! `KINDS`, `WRITER` and `THREADS` set to one thing each, that is what a
//! count of the whole process, such as `perf stat -e instructions:u`, is
//! divided by.

use std::{
    collections::BTreeSet,
    hint::black_box,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Barrier, Mutex, PoisonError,
    },
    thread,
    time::{Duration, Instant},
};

use lignin_store::load::LoadBuffers;

use super::store::{page, Changes, Rng, Store};
use crate::snapshot::{slots::home_of_this_thread, Versions};

const READER_PAGES: u32 = 3;
const ROOT: u32 = 1;

/// The reads that were counted in every run of the measurement so far.
static READS: AtomicU64 = AtomicU64::new(0);

/// The pages of a store that `readers` readers read, each with `own`
/// pages that no other reader reads.
fn pages_for(readers: u32, own: u32) -> u32 {
    1 + readers * own
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pins,
    Lease,
    OwnPages,
    SharedRoot,
    ManyPages,
}

impl Kind {
    const ALL: [(Self, &'static str); 4] = [
        (Self::Pins, "pins"),
        (Self::Lease, "lease"),
        (Self::OwnPages, "own pages"),
        (Self::SharedRoot, "shared root"),
    ];

    const MANY_PAGES: &'static str = "many pages";
}

/// The pages reader `reader` reads in reads of `kind`, of the `each`
/// pages that are its own: all of them in every read, but for
/// [`Kind::ManyPages`], which reads the next four of them each time.
fn path(kind: Kind, reader: u32, each: u32) -> Vec<u32> {
    let own = 2 + reader * each;
    match kind {
        Kind::Pins | Kind::Lease => Vec::new(),
        Kind::OwnPages => (own..=own + READER_PAGES).collect(),
        Kind::SharedRoot => std::iter::once(ROOT)
            .chain(own..own + READER_PAGES)
            .collect(),
        Kind::ManyPages => (own..own + each).collect(),
    }
}

/// The nanoseconds the calling thread has been on a processor, where the
/// system says.
fn on_cpu_nanos() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    stat.split_whitespace().next()?.parse().ok()
}

fn status_line(field: &str) -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with(field))?;
    Some(
        line.split_whitespace()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

struct Run {
    /// The reads of all readers in a second of the clock.
    rate: f64,
    /// The reads of all readers in a second of their processor time.
    per_cpu_second: Option<f64>,
    /// The share of a core that a reader got.
    share: Option<f64>,
    commits: f64,
    /// How many cells the readers start at.
    cells: usize,
}

/// Reads for `length` on `readers` threads, beside a writer that makes
/// `commits` commits in a second.
fn run(
    store: &Store,
    versions: &Versions,
    kind: Kind,
    readers: u32,
    commits: u32,
    length: Duration,
) -> Run {
    let pages = store.pages_at(versions.latest()).len() as u32;
    let each = own_pages();
    let reads_of = store.reads(versions);
    let start = Barrier::new(readers as usize + usize::from(commits > 0) + 1);
    let go = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let total = AtomicU64::new(0);
    let cpu_nanos = AtomicU64::new(0);
    let committed = AtomicU64::new(0);
    let homes = Mutex::new(BTreeSet::new());
    let mut seconds = 0.0;
    thread::scope(|scope| {
        for reader in 0..readers {
            let (start, go, stop) = (&start, &go, &stop);
            let (total, cpu_nanos, homes) = (&total, &cpu_nanos, &homes);
            scope.spawn(move || {
                let home = home_of_this_thread();
                homes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(home);
                let path = path(kind, reader, each);
                let take = match kind {
                    Kind::ManyPages => READER_PAGES as usize + 1,
                    _ => path.len(),
                };
                let mut buffers = LoadBuffers::new();
                let mut reads = 0u64;
                wait_for(start, go);
                let before = on_cpu_nanos();
                while !stop.load(Ordering::Relaxed) {
                    let snapshot = reads_of.snapshot();
                    let operation = snapshot.operation();
                    if kind == Kind::Lease {
                        black_box(snapshot.lease());
                    }
                    let from = match kind {
                        Kind::ManyPages => reads as usize * take % path.len(),
                        _ => 0,
                    };
                    for step in 0..take {
                        let id = path[(from + step) % path.len()];
                        let frame = operation.page(page(id), &mut buffers).expect("a page");
                        assert!(frame.born() <= snapshot.version());
                        black_box(frame[64]);
                    }
                    reads += 1;
                }
                if let (Some(before), Some(after)) = (before, on_cpu_nanos()) {
                    cpu_nanos.fetch_add(after - before, Ordering::Relaxed);
                }
                total.fetch_add(reads, Ordering::Relaxed);
            });
        }
        if commits > 0 {
            let (start, go, stop, committed) = (&start, &go, &stop, &committed);
            scope.spawn(move || {
                let mut rng = Rng::new(7);
                let every = Duration::from_secs(1) / commits;
                wait_for(start, go);
                let mut due = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let mut write = vec![page(ROOT)];
                    while write.len() < 3 {
                        let other = page(2 + rng.below(u64::from(pages - 1)) as u32);
                        if !write.contains(&other) {
                            write.push(other);
                        }
                    }
                    let changes = Changes {
                        write,
                        ..Changes::default()
                    };
                    store.commit(versions, &changes);
                    store.write_back();
                    if !stop.load(Ordering::Relaxed) {
                        committed.fetch_add(1, Ordering::Relaxed);
                    }
                    due += every;
                    thread::sleep(due.saturating_duration_since(Instant::now()));
                }
            });
        }
        start.wait();
        let started = Instant::now();
        go.store(true, Ordering::SeqCst);
        thread::sleep(length);
        stop.store(true, Ordering::Relaxed);
        seconds = started.elapsed().as_secs_f64();
    });
    READS.fetch_add(total.load(Ordering::Relaxed), Ordering::Relaxed);
    let reads = total.load(Ordering::Relaxed) as f64;
    let cpu_seconds = cpu_nanos.load(Ordering::Relaxed) as f64 / 1e9;
    let measured = cpu_seconds > 0.0;
    Run {
        rate: reads / seconds,
        per_cpu_second: measured.then(|| reads / cpu_seconds),
        share: measured.then(|| cpu_seconds / seconds / f64::from(readers)),
        commits: committed.load(Ordering::Relaxed) as f64 / seconds,
        cells: homes
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .len(),
    }
}

/// Waits until every thread of the run is there, and then for the signal
/// that the run's clock has been read.
fn wait_for(start: &Barrier, go: &AtomicBool) {
    start.wait();
    while !go.load(Ordering::SeqCst) {
        std::hint::spin_loop();
    }
}

/// The median of three runs. A run beside a writer that made no commit
/// was not a run beside a writer, and is made again.
fn median(
    store: &Store,
    versions: &Versions,
    kind: Kind,
    readers: u32,
    commits: u32,
    length: Duration,
) -> Run {
    let mut runs = Vec::new();
    let mut tries = 0;
    while runs.len() < 3 {
        tries += 1;
        assert!(tries <= 12, "the writer made no commit in most runs");
        let run = run(store, versions, kind, readers, commits, length);
        if commits == 0 || run.commits > 0.0 {
            runs.push(run);
        }
    }
    runs.sort_by(|a, b| a.rate.total_cmp(&b.rate));
    runs.swap_remove(1)
}

/// Whether `KINDS` names the kind `name`. With no `KINDS`, the first four
/// kinds are measured.
fn measured(name: &str) -> bool {
    match std::env::var("KINDS") {
        Ok(kinds) => kinds.split(',').any(|kind| kind.trim() == name),
        Err(_) => name != Kind::MANY_PAGES,
    }
}

/// The pages that each reader has of its own.
fn own_pages() -> u32 {
    match measured(Kind::MANY_PAGES) {
        true => setting("SPREAD", 64).max(READER_PAGES + 1),
        false => READER_PAGES + 1,
    }
}

fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[test]
#[ignore = "a measurement: minutes, and all the cores of the machine"]
fn reader_throughput_by_the_number_of_readers() {
    let cores = thread::available_parallelism().map_or(1, |cores| cores.get() as u32);
    let length = Duration::from_secs_f64(setting("SECS", 1.0));
    let commits: u32 = setting("COMMITS", 2_000);
    let threads: Vec<u32> = match std::env::var("THREADS") {
        Ok(list) => list.split(',').filter_map(|n| n.parse().ok()).collect(),
        Err(_) => {
            let mut counts: Vec<u32> = [1, 2, 4, 8, 14, 20, 28, 56]
                .into_iter()
                .filter(|n| *n < cores)
                .collect();
            counts.push(cores);
            counts
        }
    };
    let most = threads.iter().copied().max().expect("a number of readers");
    assert!(threads.iter().all(|n| *n >= 1));

    let pages = pages_for(most, own_pages());
    let frames: usize = setting("POOL", 2 * pages as usize);
    let store = Store::without_history(frames);
    let versions = store.versions();
    store.commit(&versions, &Changes::create(pages as usize));
    store.write_back();

    println!(
        "{cores} cores, {length:?} a run, median of 3, {commits} commits/s, {pages} pages, \
         {frames} frames\n"
    );
    let optional = |value: Option<f64>, scale: f64| {
        value.map_or_else(|| "?".to_owned(), |value| format!("{:.0}", value * scale))
    };
    let writer = std::env::var("WRITER").unwrap_or_else(|_| "both".to_owned());
    let beside = [(commits, "one"), (0, "none")]
        .into_iter()
        .filter(|(_, which)| writer == "both" || writer == *which)
        .map(|(commits, _)| commits);
    for commits in beside {
        let kinds = (Kind::ALL.into_iter()).chain([(Kind::ManyPages, Kind::MANY_PAGES)]);
        for (kind, name) in kinds.filter(|(_, name)| measured(name)) {
            let with = if commits > 0 {
                "one writer"
            } else {
                "no writer"
            };
            println!("### {name}, {with}\n");
            println!(
                "| readers | reads/s | speedup | reads per CPU second of a reader | \
                 of one reader's | core share % | commits/s | cells the readers start at |"
            );
            println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
            // What one reader does alone is what the others are held to.
            let mut alone = None;
            for &readers in &threads {
                let run = median(&store, &versions, kind, readers, commits, length);
                if alone.is_none() {
                    alone = Some(if readers == 1 {
                        (run.rate, run.per_cpu_second)
                    } else {
                        let one = median(&store, &versions, kind, 1, commits, length);
                        (one.rate, one.per_cpu_second)
                    });
                }
                let (rate, per_cpu) = alone.expect("measured above");
                let against = run.per_cpu_second.zip(per_cpu).map(|(now, one)| now / one);
                println!(
                    "| {readers} | {:.0} | {:.1} | {} | {} % | {} | {:.0} | {} |",
                    run.rate,
                    run.rate / rate,
                    optional(run.per_cpu_second, 1.0),
                    optional(against, 100.0),
                    optional(run.share, 100.0),
                    run.commits,
                    run.cells,
                );
            }
            println!();
        }
    }
    println!(
        "pool: {:?}\n{:?}\npeak memory: {}\nreads counted in all runs: {}",
        store.pool.stats(),
        store.counts,
        status_line("VmHWM:").unwrap_or_else(|| "?".to_owned()),
        READS.load(Ordering::Relaxed)
    );
}
