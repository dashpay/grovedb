//! Reader throughput by the number of reader threads, with one writer
//! committing all the time: the acceptance measurement of snapshot reads.
//!
//! `TMPDIR=<a directory on disk> cargo bench -p lignindb --bench snapshot_scaling`,
//! with these settings in the environment:
//!
//! | Setting | Default | Is |
//! | --- | --- | --- |
//! | `SECS` | 1 | the length of one run; three are made of each, and the median is given |
//! | `THREADS` | 1, 2, 4, 8, 14, 20, 28, 56 up to the cores, and the cores | the numbers of reader threads |
//! | `COMMITS` | 2000 | the commits the writer makes in a second |
//!
//! One read is what a read with no transaction does (`transactions.md`
//! RD2): take a snapshot, begin an operation, read some pages at the
//! pinned version, and drop both. Four kinds are measured:
//!
//! - `pins`: the snapshot and the operation alone, no page
//! - `lease`: the same, and the snapshot takes its lease
//! - `own pages`: four pages that no other reader reads
//! - `shared root`: one page that every reader reads, then three of its own
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
//! Every thread of a run starts at one signal, and the reads and the
//! commits are those between that signal and the one that stops them. A
//! run with a writer that made no commit is made again.
//!
//! The store has one page for all readers and four for each reader, of
//! 8 KiB, in a pool of twice as many frames. It keeps the pages of the
//! latest version only and trims its page table, so its memory does not
//! grow with the length of the run; its segment file grows by three small
//! blocks a commit. The peak of memory is printed at the end.

#[path = "../tests/store/mod.rs"]
mod store;

use std::{
    hint::black_box,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Barrier,
    },
    thread,
    time::{Duration, Instant},
};

use lignin_store::load::LoadBuffers;
use lignindb::snapshot::Versions;
use store::{page, Changes, Rng, Store};

const READER_PAGES: u32 = 3;
const ROOT: u32 = 1;

/// The pages of a store that `readers` readers read.
fn pages_for(readers: u32) -> u32 {
    1 + readers * (READER_PAGES + 1)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pins,
    Lease,
    OwnPages,
    SharedRoot,
}

impl Kind {
    const ALL: [(Self, &'static str); 4] = [
        (Self::Pins, "pins"),
        (Self::Lease, "lease"),
        (Self::OwnPages, "own pages"),
        (Self::SharedRoot, "shared root"),
    ];
}

/// The pages reader `reader` reads in one read of `kind`.
fn path(kind: Kind, reader: u32) -> Vec<u32> {
    let own = 2 + reader * (READER_PAGES + 1);
    match kind {
        Kind::Pins | Kind::Lease => Vec::new(),
        Kind::OwnPages => (own..=own + READER_PAGES).collect(),
        Kind::SharedRoot => std::iter::once(ROOT)
            .chain(own..own + READER_PAGES)
            .collect(),
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
    let reads_of = store.reads(versions);
    let start = Barrier::new(readers as usize + usize::from(commits > 0) + 1);
    let stop = AtomicBool::new(false);
    let total = AtomicU64::new(0);
    let cpu_nanos = AtomicU64::new(0);
    let committed = AtomicU64::new(0);
    let mut seconds = 0.0;
    thread::scope(|scope| {
        for reader in 0..readers {
            let (start, stop, total, cpu_nanos) = (&start, &stop, &total, &cpu_nanos);
            scope.spawn(move || {
                let path = path(kind, reader);
                let mut buffers = LoadBuffers::new();
                let mut reads = 0u64;
                start.wait();
                let before = on_cpu_nanos();
                while !stop.load(Ordering::Relaxed) {
                    let snapshot = reads_of.snapshot();
                    let operation = snapshot.operation();
                    if kind == Kind::Lease {
                        black_box(snapshot.lease());
                    }
                    for &id in &path {
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
            let (start, stop, committed) = (&start, &stop, &committed);
            scope.spawn(move || {
                let mut rng = Rng::new(7);
                let every = Duration::from_secs(1) / commits;
                start.wait();
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

fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn main() {
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

    let pages = pages_for(most);
    let store = Store::without_history(2 * pages as usize);
    let versions = store.versions();
    store.commit(&versions, &Changes::create(pages as usize));
    store.write_back();

    println!("{cores} cores, {length:?} a run, median of 3, {commits} commits/s, {pages} pages\n");
    let optional = |value: Option<f64>, scale: f64| {
        value.map_or_else(|| "?".to_owned(), |value| format!("{:.0}", value * scale))
    };
    for commits in [commits, 0] {
        for (kind, name) in Kind::ALL {
            let with = if commits > 0 {
                "one writer"
            } else {
                "no writer"
            };
            println!("### {name}, {with}\n");
            println!(
                "| readers | reads/s | speedup | reads per CPU second of a reader | \
                 of one reader's | core share % | commits/s |"
            );
            println!("| --- | --- | --- | --- | --- | --- | --- |");
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
                    "| {readers} | {:.0} | {:.1} | {} | {} % | {} | {:.0} |",
                    run.rate,
                    run.rate / rate,
                    optional(run.per_cpu_second, 1.0),
                    optional(against, 100.0),
                    optional(run.share, 100.0),
                    run.commits,
                );
            }
            println!();
        }
    }
    println!(
        "pool: {:?}\n{:?}\npeak memory: {}",
        store.pool.stats(),
        store.counts,
        status_line("VmHWM:").unwrap_or_else(|| "?".to_owned())
    );
}
