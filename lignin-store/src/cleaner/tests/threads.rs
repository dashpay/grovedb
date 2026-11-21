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
//!
//! **What the test asserts is given by construction, and by no share of
//! what the threads happened to do.** How often they take turns is the
//! schedule's: the counts of it are printed and not asserted. Two things
//! make the rest:
//!
//! - [`Fixture`]: the store begins with work that only the cleaner does,
//!   in pages that the committer leaves alone. The reader's first pin is
//!   two commits behind before the cleaner makes its first pass, and is
//!   held until that work is done.
//! - A hand-shake: the committer goes on after every [`TURN`] commits only
//!   once the reader holds a pin on the latest version or the one before,
//!   and the reader gives a pin back only after two commits have gone by
//!   and the cleaner has made a whole pass meanwhile. It sweeps at the
//!   pinned version all that while.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use lignin_io::sim::SimConfig;

use super::{
    crash::new_store_with,
    values::{value_files, value_on_disk},
    *,
};
use crate::{
    cache::{BufferPool, PoolConfig},
    cleaner::state::AtSwitch,
    flush::tests::{
        cells_of,
        crash::{check_image, Ran, Tally, PLANS},
        installed, same, stored, Cell, StopOnDrop, Stored,
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

/// The pages that the committer makes, changes and frees.
const PAGE_IDS: u64 = 6;
/// The pages of [`Fixture`], which the committer leaves alone: one that is
/// freed, one whose block stays, and one with values in the value log.
const GOES: u32 = 7;
const STAYS: u32 = 8;
const VALUES: u32 = 9;
const ROUNDS: u64 = 500;
/// The committer waits for the reader after every so many commits.
const TURN: u64 = 8;
const TURNS: u64 = ROUNDS / TURN;
/// How many of the blocks that the cleaner moves have the file system
/// frozen between their copy and the switch of their link.
const SWITCHES: usize = 4;

// At least two commits follow every turn, the last one too: the pin that
// the reader holds at a turn is behind when it is given back.
const _: () = assert!(TURN >= 2 && ROUNDS % TURN >= 2);

/// What the reader came to.
#[derive(Debug, Default)]
struct Read {
    /// Pins it took.
    pins: AtomicU64,
    /// Pins that were on a version before the latest when they were given
    /// back: commits had gone by.
    behind: AtomicU64,
    /// Those of them that were held, behind, while the cleaner made a
    /// whole pass over the segments and one over the value log.
    beside: AtomicU64,
    /// Pages it read from the segments, with nothing in memory.
    cold: AtomicU64,
    /// Values it read from the value log, of the pages that the store's
    /// pool gave it.
    values: AtomicU64,
    /// The version of the last pin it has swept at.
    swept_at: AtomicU64,
}

/// What the store holds before the threads begin, in three pages that the
/// committer leaves alone. It leaves the cleaner work that nothing else
/// does and that no schedule takes away:
///
/// - The chain of a page that was freed. Write-back trims the chains of
///   the pages it writes, and the cleaner alone trims this one.
/// - The block of that page beside the block of a page that stays, in one
///   segment that the table checkpoint in force points into. Once the
///   chain is trimmed the segment holds a dead block, so the block that
///   stays is moved, and the segment is deleted after a table checkpoint
///   that does not point into it.
/// - A value-log file with a value that is replaced beside one that is
///   live. The live one is moved, and the file goes once no pin is on a
///   version before the move.
///
/// It is made in two steps ([`Self::make`], [`Self::go_by`]), and the
/// reader's first pin is taken between them: that pin's version reads the
/// block that is moved and both values, and is two commits behind when
/// the cleaner begins.
struct Fixture {
    /// The segment of the two blocks.
    segment: FileId,
    /// The value-log file of the two values.
    values: FileId,
    /// The table checkpoints that were taken before the threads began.
    checkpoints: u64,
}

impl Fixture {
    fn make(db: &Db) -> Self {
        db.commit(&[
            insert((GOES, 1), b"goes", inline(&noise(7, 60))),
            insert((STAYS, 1), b"stays", inline(&noise(8, 60))),
        ]);
        db.wb().pass().expect("a pass");
        db.commit(&[Op::Free { page: page(GOES) }]);
        db.commit(&[
            insert((VALUES, 1), b"one", long(1, 300)),
            insert((VALUES, 2), b"two", long(2, 300)),
        ]);
        db.wb().pass().expect("a pass");
        installed(db.wb().checkpoint().expect("a checkpoint"));

        let block_of = |id: u32| {
            let chain = db.chain(page(id));
            chain.iter().find_map(Link::location).expect("a block")
        };
        let (goes, stays) = (block_of(GOES), block_of(STAYS));
        assert_eq!(goes.file, stays.file, "one segment holds both blocks");
        assert_eq!(db.shape(page(GOES)), "BF");
        let [replaced, live] = [1, 2].map(|at| db.pointer(page(VALUES), slot(at)));
        assert_eq!(replaced.file(), live.file(), "one file holds both values");
        Self {
            segment: stays.file,
            values: live.file(),
            checkpoints: db.stats().checkpoints,
        }
    }

    /// Two commits, which leave a pin on the version before them behind.
    /// The first replaces a value, so the value-log file holds a dead
    /// value beside a live one.
    fn go_by(&self, db: &Db) {
        db.commit(&[Op::Value {
            page: page(VALUES),
            slot: slot(1),
            value: long(3, 300),
        }]);
        db.wb().pass().expect("a pass");
        db.commit(&[hash((VALUES, 1), 4)]);
        db.wb().pass().expect("a pass");
        assert_eq!(db.pointer(page(VALUES), slot(2)).file(), self.values);
    }

    /// Whether the cleaner has done what the fixture leaves it and a pin
    /// does not hold back: the chain of the freed page is trimmed, the
    /// block that stays has been moved and its segment is deleted, and no
    /// cell of the latest version of the fixture's page points into the
    /// value-log file.
    fn cleaned(&self, db: &Db) -> bool {
        let moved = (db.chain(page(STAYS)).iter())
            .filter_map(Link::location)
            .all(|block| block.file != self.segment);
        let repointed = (db.pointers(page(VALUES)).iter()).all(|at| at.file() != self.values);
        db.chain(page(GOES)).is_empty()
            && moved
            && !segment_ids(db).contains(&self.segment)
            && !on_disk(db, self.segment)
            && repointed
    }

    /// Whether the value-log file is deleted.
    fn values_gone(&self, db: &Db) -> bool {
        !value_files(db).contains(&self.values) && !value_on_disk(db, self.values)
    }
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
    for id in (1..=VALUES).map(page) {
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
    let fixture = Fixture::make(&db);
    let wb = db.wb();
    let state = CleanerState::new(eager());
    let stats = crate::stats::Stats::new();
    let cleaner = Cleaner::new(wb, &state).with_stats(&stats);
    let done = AtomicBool::new(false);
    let frozen = Mutex::new(Vec::new());
    let read = Read::default();

    // The version the log was last seen durable through: an image is of
    // that moment or a later one.
    let durable = Arc::new(AtomicU64::new(0));
    let freeze = || {
        let seen = db.log.durable().version;
        durable.fetch_max(seen.get(), Ordering::SeqCst);
        let image = fs.crash_image();
        frozen.lock().expect("the images").push((seen, image));
    };
    // The file system between the copy of a block and the switch of its
    // link, for the first blocks that are moved.
    let switches = Arc::new(Mutex::new(Vec::new()));
    let at_switch = {
        let (fs, durable, switches) = (fs.clone(), durable.clone(), switches.clone());
        AtSwitch(Box::new(move |_, _| {
            let mut images = switches.lock().expect("the images");
            if images.len() < SWITCHES {
                let seen = version(durable.load(Ordering::SeqCst));
                images.push((seen, fs.crash_image()));
            }
        }))
    };
    *state.at_switch.lock().expect("the hook") = Some(at_switch);

    // The reader's first pin, with its first sweep: the block that the
    // fixture has the cleaner move is read where it is now. Two commits
    // go by before the cleaner makes its first pass.
    let pin = || {
        db.versions.hold_guard();
        let at = db.versions.pin_latest();
        let mut seen = BTreeMap::new();
        sweep(&db, at, &mut seen, &read);
        // The next sweep is another operation.
        db.versions.release_guard();
        (at, seen)
    };
    let first = pin();
    fixture.go_by(&db);
    durable.store(db.log.durable().version.get(), Ordering::SeqCst);

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
        // version, and sweeps at it, one operation each, until two commits
        // have gone by and the cleaner has then made a whole pass over the
        // segments and one over the value log. Its last sweep begins
        // after that. Nothing but the end of the test cuts a pin short,
        // so every pin that the committer goes on after is behind when it
        // is given back.
        scope.spawn(|| {
            let mut held = Some(first);
            loop {
                let is_first = held.is_some();
                let (at, mut seen) = held.take().unwrap_or_else(pin);
                read.swept_at.store(at.get(), Ordering::SeqCst);
                // The cleaner's counts when the pin was found two commits
                // behind. A pass is counted when it begins: of two that
                // began since, the first has ended.
                let mut when_behind: Option<CleanerStats> = None;
                loop {
                    if when_behind.is_none() && db.latest().get() >= at.get() + 2 {
                        when_behind = Some(state.stats());
                    }
                    let passed = when_behind.as_ref().is_some_and(|then| {
                        let now = state.stats();
                        now.passes >= then.passes + 2 && now.value_passes >= then.value_passes + 2
                    });
                    db.versions.hold_guard();
                    // The first pin is held until the cleaner has done
                    // what the store began with: it trims, moves the block
                    // that this pin's version reads and deletes the
                    // segment that the first sweep read it from.
                    let whole = passed && (!is_first || fixture.cleaned(&db));
                    let last = whole || done.load(Ordering::SeqCst);
                    sweep(&db, at, &mut seen, &read);
                    db.versions.release_guard();
                    if last {
                        read.beside.fetch_add(u64::from(whole), Ordering::SeqCst);
                        break;
                    }
                }
                let behind = at < db.latest();
                db.versions.unpin();
                read.behind.fetch_add(u64::from(behind), Ordering::SeqCst);
                read.pins.fetch_add(1, Ordering::SeqCst);
                if done.load(Ordering::SeqCst) {
                    break;
                }
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
        // What the committer waits for comes without it: the other
        // threads go on while it waits.
        let wait_for = |met: &dyn Fn() -> bool, what: &str| {
            let started = std::time::Instant::now();
            while !met() {
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(120),
                    "{what}: {read:?}, {:?}",
                    state.stats()
                );
                std::thread::yield_now();
            }
        };
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
            // A write transaction reads the pages it copies in an
            // operation of its own: the segments it reads from are not
            // deleted under it.
            crate::flush::Published::guarded(&db.versions, &mut || {
                db.commit(std::slice::from_ref(&op));
            });
            wb.place_ready().expect("placed");
            wb.wait_for_room().expect("room");
            // At a turn the committer goes on once the reader has swept
            // at a pin on the latest version or on the one before. The
            // reader comes to that by itself whatever it was at: a pin
            // that is two commits behind ends once the cleaner has begun
            // two more passes of each kind, which it does whoever waits,
            // and the next pin is on the latest version. The first pin
            // also waits for what the fixture leaves the cleaner, which
            // needs no commit: the operations it waits for are the
            // reader's sweeps, and a table checkpoint is put off by
            // nothing while the committer waits.
            if round % TURN == TURN - 1 {
                freeze();
                let latest = db.latest().get();
                wait_for(
                    &|| read.swept_at.load(Ordering::SeqCst) + 1 >= latest,
                    "the reader does not come round",
                );
            }
        }
        // Every pin that is held now is on a version after the moves of
        // the fixture's values: the file that they were moved out of
        // goes.
        wait_for(
            &|| fixture.values_gone(&db),
            "the value-log file is not deleted",
        );
    });

    // What the threads left is whole.
    db.versions.unpin();
    db.versions.release_guard();
    *state.at_switch.lock().expect("the hook") = None;
    db.check_warm();
    let did = state.stats();
    let flushed = db.stats();
    let frozen = frozen.into_inner().expect("the images");
    let switches = std::mem::take(&mut *switches.lock().expect("the images"));
    println!(
        "{did:?}\n{read:?}\n{} images, {} at a switch",
        frozen.len(),
        switches.len()
    );
    assert_eq!(did.errors, 0, "{did:?}");
    assert_eq!(did.segments_unreadable, 0);
    assert_eq!(
        stats.cleaner_bytes(),
        did.bytes_moved + did.bytes_lost + did.value_bytes_copied
    );

    // The cleaner did what the store began with, which only it does, and
    // the committer did not go on after its first turn before that. The
    // deletion of the segment took a table checkpoint that does not point
    // into it.
    assert!(fixture.cleaned(&db) && fixture.values_gone(&db));
    assert!(did.links_trimmed >= 1, "{did:?}");
    assert!(
        did.blocks_moved >= 1 && did.segments_dropped >= 1,
        "{did:?}"
    );
    assert!(
        did.values_moved >= 1 && did.value_files_dropped >= 1,
        "{did:?}"
    );
    assert!(flushed.checkpoints > fixture.checkpoints, "{flushed:?}");

    // The reader was behind by construction: the first pin, and the pin
    // that the committer found at each turn, which is another one every
    // time, were given back after two commits or more. All of them but
    // the one of the last turn, which the end of the test can cut short,
    // were swept at again after a whole pass of the cleaner that began
    // when they were behind.
    let count = |of: &AtomicU64| of.load(Ordering::SeqCst);
    let pins = count(&read.pins);
    assert!(count(&read.behind) > TURNS, "{read:?}");
    assert!(count(&read.beside) >= TURNS, "{read:?}");
    // Every sweep, two or more to a pin, reads the page that stays and
    // the page of the two values from the segments, and its two values
    // from the value log.
    assert!(count(&read.cold) >= 4 * pins, "{read:?}");
    assert!(count(&read.values) >= 4 * pins, "{read:?}");

    // And so is every state that a crash would have left on the way. The
    // committer froze the file system at every turn.
    let mut ran = Ran::of(&fs, &db);
    assert!(frozen.len() as u64 >= TURNS, "{} images", frozen.len());
    let mut tally = Tally::default();
    let step = frozen.len().div_ceil(40);
    for (durable, image) in frozen.iter().step_by(step) {
        ran.durable = *durable;
        check_image(image, &ran, &PLANS, &mut tally);
    }
    // And a crash between the copy of a block and the switch of its
    // link. The fixture has a block moved, so there is such an image.
    // How many states the images can leave is the schedule's, and is
    // printed: a table checkpoint that write-back's loop takes at that
    // moment syncs the copy with every other segment.
    let mut moving = Tally::default();
    for (durable, image) in &switches {
        ran.durable = *durable;
        check_image(image, &ran, &PLANS, &mut moving);
    }
    assert!(moving.crashes >= 1, "{moving:?}");
    println!("{tally:?}\n{moving:?}");

    // The session ends with nothing dead that the cleaner can reach.
    rest(&db, &cleaner);
    db.check_cold();
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    for segment in &space.segments {
        assert!(
            segment.has_writer || segment.dead_bytes() == 0,
            "{}",
            describe(&db, &space)
        );
    }
}
