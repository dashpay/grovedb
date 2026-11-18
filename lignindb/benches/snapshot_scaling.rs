//! Reader throughput by the number of reader threads, with one writer
//! committing all the time: the acceptance measurement of snapshot reads.
//!
//! `cargo bench -p lignindb --bench snapshot_scaling`, with `SECS=0.5` for
//! the length of one run (three are made of each, and the median is given)
//! and `THREADS=1,2,4` for the thread counts.
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
//! writes them back, and asks for a reclaim, as fast as it can. Each kind
//! is also run with no writer.

#[path = "../tests/store/mod.rs"]
mod store;

use std::{
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use lignin_store::load::LoadBuffers;
use lignindb::snapshot::Versions;
use store::{page, Changes, Rng, Store};

const READER_PAGES: u32 = 3;
const MAX_READERS: u32 = 64;
const ROOT: u32 = 1;
const PAGES: u32 = 1 + MAX_READERS * (READER_PAGES + 1);

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

/// Reads for `length` on `readers` threads, and returns the reads of all
/// of them in a second and the commits made meanwhile.
fn run(
    store: &Store,
    versions: &Versions,
    kind: Kind,
    readers: u32,
    writer: bool,
    length: Duration,
) -> (f64, u64) {
    let stop = AtomicBool::new(false);
    let total = AtomicU64::new(0);
    let commits = AtomicU64::new(0);
    let started = Instant::now();
    thread::scope(|scope| {
        for reader in 0..readers {
            let (stop, total) = (&stop, &total);
            scope.spawn(move || {
                let pages = path(kind, reader);
                let mut buffers = LoadBuffers::new();
                let mut reads = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let snapshot = versions.snapshot(store.loader());
                    let operation = snapshot.operation();
                    if kind == Kind::Lease {
                        black_box(snapshot.lease());
                    }
                    for &id in &pages {
                        let frame = operation.page(page(id), &mut buffers).expect("a page");
                        assert!(frame.born() <= snapshot.version());
                        black_box(frame[64]);
                    }
                    reads += 1;
                }
                total.fetch_add(reads, Ordering::Relaxed);
            });
        }
        if writer {
            let (stop, commits) = (&stop, &commits);
            scope.spawn(move || {
                let mut rng = Rng::new(7);
                while !stop.load(Ordering::Relaxed) {
                    let mut write = vec![page(ROOT)];
                    while write.len() < 3 {
                        let other = page(2 + rng.below(u64::from(PAGES - 1)) as u32);
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
                    commits.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        thread::sleep(length);
        stop.store(true, Ordering::Relaxed);
    });
    let seconds = started.elapsed().as_secs_f64();
    (
        total.load(Ordering::Relaxed) as f64 / seconds,
        commits.load(Ordering::Relaxed),
    )
}

fn main() {
    let cores = thread::available_parallelism().map_or(1, |cores| cores.get() as u32);
    let length = std::env::var("SECS")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .unwrap_or(1.0);
    let length = Duration::from_secs_f64(length);
    let threads: Vec<u32> = match std::env::var("THREADS") {
        Ok(list) => list.split(',').filter_map(|n| n.parse().ok()).collect(),
        Err(_) => [1, 2, 4, 8, 14, 20, 28, 56]
            .into_iter()
            .filter(|n| *n <= cores)
            .collect(),
    };
    assert!(threads.iter().all(|n| (1..=MAX_READERS).contains(n)));

    let store = Store::new(2 * PAGES as usize, false);
    let versions = store.versions();
    store.commit(
        &versions,
        &Changes {
            create: PAGES as usize,
            ..Changes::default()
        },
    );
    store.write_back();

    println!("{cores} cores, {length:?} a run, median of 3\n");
    for writer in [true, false] {
        for (kind, name) in Kind::ALL {
            let with = if writer { "one writer" } else { "no writer" };
            println!("### {name}, {with}\n");
            println!("| readers | reads/s | per reader | speedup | commits/s |");
            println!("| --- | --- | --- | --- | --- |");
            let mut single = None;
            for &readers in &threads {
                let mut runs: Vec<(f64, u64)> = (0..3)
                    .map(|_| run(&store, &versions, kind, readers, writer, length))
                    .collect();
                runs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let (rate, commits) = runs[1];
                let base = *single.get_or_insert(rate / f64::from(readers));
                println!(
                    "| {readers} | {rate:.0} | {:.0} | {:.1} | {:.0} |",
                    rate / f64::from(readers),
                    rate / base,
                    commits as f64 / length.as_secs_f64(),
                );
            }
            println!();
        }
    }
    let stats = store.pool.stats();
    println!("pool: {stats:?}, {:?}", store.counts);
}
