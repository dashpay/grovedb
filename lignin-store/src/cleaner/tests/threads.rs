//! The cleaner on threads of its own, beside a committer, write-back's
//! loop and a reader that holds a version pin and an operation guard.
//!
//! The reader reads every page at its pinned version twice. Through the
//! store's pool it is given what a reader of the store is given, and reads
//! every value of it from where its cell says. Through a pool of its own,
//! which has nothing, it takes the chain from the page table and the
//! blocks from the segments. What it reads is what the commits up to its
//! version left. Now and then the file system is frozen as a crash at that
//! moment would leave it, and every state that such a crash can leave must
//! recover to one committed version, whole.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

use lignin_io::sim::SimConfig;

use super::{crash::new_store_with, *};
use crate::{
    cache::{BufferPool, PoolConfig},
    flush::tests::{
        cells_of,
        crash::{check_image, Ran, Tally, PLANS},
        same, stored, Cell, StopOnDrop, Stored,
    },
    load::{LoadBuffers, LoadError, PageLoader},
    segment::tests::sim::sim_fs,
};

struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// Says that the committer is done when it goes, however it goes.
struct Done<'a>(&'a AtomicBool);

impl Drop for Done<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Ends the cleaner's loop when it goes.
struct StopCleaner<'s>(Cleaner<'s>);

impl Drop for StopCleaner<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

const PAGE_IDS: u64 = 6;
const ROUNDS: u64 = 500;

/// What the reader came to.
#[derive(Debug, Default)]
struct Read {
    /// Pins it took.
    pins: AtomicU64,
    /// Pins that were on a version before the latest when they were given
    /// back: commits had gone by.
    behind: AtomicU64,
    /// Pages it read from the segments, with nothing in memory.
    cold: AtomicU64,
    /// Values it read from the value log, of the pages that the store's
    /// pool gave it.
    values: AtomicU64,
}

/// Whether the page in `frame`, which was read from the segments, says
/// what `cells` do, as far as its block alone tells: a value that is not
/// in its cell is as long as the one the commits left.
///
/// The value log is not read for it. The block can be older than the page
/// that a reader of the store is given: a cleaning of the value log
/// rewrites a pointer in the pool, where the page stays until write-back
/// has written it again, and the file that the block points into goes
/// once nothing a reader is given points into it.
fn block_says(id: PageId, frame: &[u8], cells: &[Cell]) -> bool {
    let found = stored(id, frame);
    found.len() == cells.len()
        && found.iter().zip(cells).all(|(found, cell)| {
            let (slot, key, value_hash, stored) = found;
            let said = cell.said();
            (*slot, key.as_slice(), value_hash) == (said.0, said.1, said.2)
                && match stored {
                    Stored::Inline(bytes) => bytes == said.3,
                    Stored::Placed(pointer) => pointer.value_len() as usize == said.3.len(),
                    Stored::Pending(pending) => pending.value_len() as usize == said.3.len(),
                }
        })
}

/// One sweep of the reader over every page at `at`, which it has pinned.
/// `seen` is the newest version of each page that it has read from the
/// segments under this pin: what it is given never goes back.
fn sweep(db: &Db, at: CommitVersion, seen: &mut BTreeMap<PageId, u64>, read: &Read) {
    // A pool with nothing in it and room for a few pages.
    let cold = BufferPool::new(PoolConfig {
        page_size: db.size,
        capacity: 8 * (db.size.get() + 512),
    })
    .expect("a pool");
    let loader = PageLoader::new(&cold, &db.table, &db.segments, db.size).expect("a loader");
    let mut buffers = LoadBuffers::new();
    for id in (1..=PAGE_IDS as u32).map(page) {
        // What the commits up to `at` left of the page.
        let expected = {
            let model = db.model.lock().expect("the model");
            (model.at(id, at)).map(|(born, cells)| (born, cells.to_vec()))
        };
        // Through the store's pool: the page as `at` has it, exactly.
        match (db.frame(id, at), &expected) {
            (Ok(frame), Some((born, cells))) => {
                assert_eq!(frame.born().get(), *born, "page {id} at {at}");
                let found = cells_of(id, &frame, Some(&frame), &db.values, &db.log);
                assert!(same(&found, cells), "page {id} at {at}: {found:?}");
                let placed = found.iter().filter(|cell| cell.pointer.is_some()).count();
                read.values.fetch_add(placed as u64, Ordering::Relaxed);
            }
            (Err(LoadError::NoPage { .. }), None) => {}
            (found, expected) => panic!(
                "page {id} at {at}: {:?}, not {expected:?}",
                found.map(|frame| frame.born())
            ),
        }
        // From the segments: the newest version at or before `at` that
        // was written back, which is one the commits made, and never an
        // older one than the pin was given before.
        match loader.get(id, at, &mut buffers) {
            Ok(frame) => {
                let born = frame.born();
                let newest = expected.as_ref().map(|(born, _)| *born);
                assert!(Some(born.get()) <= newest, "page {id} at {at}: born {born}");
                let before = seen.insert(id, born.get());
                assert!(before <= Some(born.get()), "page {id} at {at} went back");
                let model = db.model.lock().expect("the model");
                let (was_born, cells) = model.at(id, born).expect("a version of the page");
                assert_eq!(was_born, born.get(), "page {id}: no commit bore {born}");
                assert!(block_says(id, &frame, cells), "page {id} of {born}");
                read.cold.fetch_add(1, Ordering::Relaxed);
            }
            Err(LoadError::NoPage { .. }) => {
                assert!(!seen.contains_key(&id), "page {id} at {at} is gone");
            }
            Err(error) => panic!("page {id} at {at}: {error}"),
        }
    }
}

/// **A committer, write-back, a reader with a pin and the cleaner at
/// once** (tests T-RD1, T-PN2 and T-PN5 on threads).
#[test]
fn the_cleaner_runs_beside_commits_write_back_and_a_reader_with_a_pin() {
    let fs = sim_fs(SimConfig::new(29));
    // The committer is made to wait for write-back often, so that the
    // threads take turns however fast each of them is.
    let config = FlushConfig {
        max_unwritten: 3,
        checkpoint_log_bytes: 1500,
        idle: std::time::Duration::from_millis(1),
        ..small_segments()
    };
    let db = new_store_with(&fs, config);
    let wb = db.wb();
    let state = CleanerState::new(eager());
    let stats = crate::stats::Stats::new();
    let cleaner = Cleaner::new(wb, &state).with_stats(&stats);
    let (done, waits) = (AtomicBool::new(false), AtomicBool::new(false));
    let frozen = Mutex::new(Vec::new());
    let read = Read::default();

    let freeze = || {
        let durable = db.log.durable().version;
        let image = fs.crash_image();
        frozen.lock().expect("the images").push((durable, image));
    };

    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|error| panic!("a turn of write-back failed: {error}")));
        let _stop = StopOnDrop(wb);
        scope.spawn(|| {
            cleaner.run(
                |cleaner| cleaner.pass_values(&Cells(&db)),
                |error| panic!("a pass of the cleaner failed: {error}"),
            )
        });
        let _stop_cleaner = StopCleaner(cleaner);
        // The reader: an operation guard, then a pin on the latest
        // version, held until two commits have gone by or the committer
        // waits for it, with a sweep before that and two after.
        scope.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                db.versions.hold_guard();
                let at = db.versions.pin_latest();
                let mut seen = BTreeMap::new();
                for sweeps in 0..3 {
                    sweep(&db, at, &mut seen, &read);
                    // The next sweep is another operation.
                    db.versions.release_guard();
                    while sweeps == 0
                        && db.latest().get() < at.get() + 2
                        && !done.load(Ordering::SeqCst)
                        && !waits.load(Ordering::SeqCst)
                    {
                        std::thread::yield_now();
                    }
                    db.versions.hold_guard();
                }
                read.behind
                    .fetch_add(u64::from(at < db.latest()), Ordering::Relaxed);
                db.versions.unpin();
                db.versions.release_guard();
                read.pins.fetch_add(1, Ordering::SeqCst);
                // Nothing is pinned for a moment: what the pin kept goes.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        scope.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                freeze();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });

        let _done = Done(&done);
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        // For each page, the slots in use, or `None` while it is no page.
        let mut cells: Vec<Option<u16>> = vec![None; PAGE_IDS as usize + 1];
        for round in 0..ROUNDS {
            if round % 8 == 0 {
                db.pool.reclaim(&db.versions);
            }
            let id = 1 + rng.below(PAGE_IDS);
            let at = |slot_id: u16| (id as u32, slot_id);
            let op = match (cells[id as usize], rng.below(12)) {
                // A page is made, and a page that has lived a while is
                // freed, so that its id comes round again.
                (None, _) => {
                    cells[id as usize] = Some(1);
                    insert(at(1), &round.to_be_bytes(), long(round as u8, 400))
                }
                (Some(used), 0) if used >= 3 => {
                    cells[id as usize] = None;
                    Op::Free {
                        page: page(id as u32),
                    }
                }
                (Some(used), 1..=3) => Op::Hash {
                    page: page(id as u32),
                    slot: slot(1 + rng.below(u64::from(used)) as u16),
                    byte: round as u8,
                },
                (Some(used), 4..=7) => Op::Value {
                    page: page(id as u32),
                    slot: slot(1 + rng.below(u64::from(used)) as u16),
                    value: long(round as u8, 300 + rng.below(200) as usize),
                },
                (Some(used), _) if used < 8 => {
                    cells[id as usize] = Some(used + 1);
                    let value = match rng.below(3) {
                        0 => inline(b"in the cell"),
                        _ => long(round as u8, 300 + rng.below(200) as usize),
                    };
                    insert(at(used + 1), &round.to_be_bytes(), value)
                }
                (Some(_), _) => hash(at(1), round as u8),
            };
            db.commit(&[op]);
            wb.place_ready().expect("placed");
            wb.wait_for_room().expect("room");
            // The threads take turns by construction: every few commits
            // the committer waits for a pin of the reader to end and for
            // passes of the cleaner to be made.
            if round % 8 == 7 {
                let (pinned, passed) = (read.pins.load(Ordering::SeqCst), state.stats());
                let started = std::time::Instant::now();
                waits.store(true, Ordering::SeqCst);
                let caught_up = || {
                    let did = state.stats();
                    read.pins.load(Ordering::SeqCst) > pinned
                        && did.passes >= passed.passes + 2
                        && did.value_passes >= passed.value_passes + 2
                };
                while !caught_up() {
                    assert!(
                        started.elapsed() < std::time::Duration::from_secs(120),
                        "the reader and the cleaner do not come round: {read:?}"
                    );
                    std::thread::yield_now();
                }
                waits.store(false, Ordering::SeqCst);
            }
        }
    });

    // What the threads left is whole.
    db.versions.unpin();
    db.versions.release_guard();
    db.check_warm();
    let did = state.stats();
    let flushed = db.stats();
    println!(
        "{did:?}\n{read:?}\n{} images",
        frozen.lock().expect("the images").len()
    );
    assert_eq!(did.errors, 0, "{did:?}");
    assert!(
        did.blocks_moved > 10 && did.segments_dropped > 10,
        "{did:?}"
    );
    assert!(did.links_trimmed > 0, "{did:?}");
    assert!(
        did.values_moved > 2 && did.value_files_dropped > 2,
        "{did:?}"
    );
    assert_eq!(did.segments_unreadable, 0);
    assert_eq!(
        stats.cleaner_bytes(),
        did.bytes_moved + did.bytes_lost + did.value_bytes_copied
    );
    assert!(flushed.checkpoints > 0, "{flushed:?}");
    let pins = read.pins.load(Ordering::SeqCst);
    assert!(pins >= ROUNDS / 8, "{read:?}");
    assert!(
        read.behind.load(Ordering::Relaxed) >= ROUNDS / 16,
        "{read:?}"
    );
    assert!(read.cold.load(Ordering::Relaxed) > pins, "{read:?}");
    assert!(read.values.load(Ordering::Relaxed) > 0, "{read:?}");

    // And so is every state that a crash would have left on the way.
    let mut ran = Ran::of(&fs, &db);
    let frozen = frozen.into_inner().expect("the images");
    assert!(frozen.len() >= 20, "{} images", frozen.len());
    let mut tally = Tally::default();
    let step = frozen.len().div_ceil(40);
    for (durable, image) in frozen.iter().step_by(step) {
        ran.durable = *durable;
        check_image(image, &ran, &PLANS, &mut tally);
    }
    assert!(tally.states > tally.crashes, "{tally:?}");

    // The session ends with nothing dead that the cleaner can reach.
    rest(&db, &cleaner);
    db.check_cold();
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    for segment in &space.segments {
        assert!(
            segment.is_written() || segment.dead_bytes() == 0,
            "{}",
            describe(&db, &space)
        );
    }
}
