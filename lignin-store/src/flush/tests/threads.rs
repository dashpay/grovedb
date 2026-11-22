//! Write-back on several threads at once: a committer, the background
//! loop, a second thread that makes passes and takes table checkpoints of
//! its own, and a moving value. Now and then the file system is frozen as
//! a crash at that moment would leave it, and every state that such a
//! crash can leave must recover to one committed version, whole.
//!
//! What the test counts, it has made. The scheduler decides where a pass,
//! a checkpoint and a turn of the loop fall among the commits, and no
//! assertion here is about that:
//!
//! - **The images.** The committer asks the freezer for an image at the
//!   top of every fourth round and goes on with the round. It begins the
//!   next round once the image is taken. So there is one image for each
//!   such round, taken while that round was made: before, inside or after
//!   its commit and its page steps, and wherever the loop, the pass and
//!   the checkpoint of the other threads were then, which nothing holds
//!   still for it.
//! - **The states.** At three rounds the committer waits until a table
//!   checkpoint covers what it committed, which the other threads reach
//!   with no help of its own, and then builds what the counts are of
//!   ([`built`]).

use std::{
    collections::BTreeSet,
    sync::{atomic::AtomicBool, Condvar},
    time::{Duration, Instant},
};

use lignin_io::sim::{CrashImage, SimConfig, SimVfs};

use super::{
    crash::{check_image, log_options, Ran, Tally, PLANS},
    *,
};
use crate::segment::tests::sim::{db_dir, shared, sim_fs};

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
struct Done<'a>(&'a AtomicBool, &'a Images);

impl Drop for Done<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        self.1.end();
    }
}

/// How long a wait for another thread lasts before the test fails with
/// what it waited for. It keeps a thread that died from hanging the test.
/// Nothing is asserted of how soon a thread comes.
const LONG: Duration = Duration::from_secs(300);

#[derive(Default)]
struct Asked {
    asked: u64,
    taken: u64,
    ended: bool,
}

/// The images the committer has asked the freezer for, and those that
/// are taken.
#[derive(Default)]
struct Images {
    asked: Mutex<Asked>,
    changed: Condvar,
}

impl Images {
    fn with<R>(&self, change: impl FnOnce(&mut Asked) -> R) -> R {
        let changed = change(&mut self.asked.lock().expect("the images asked for"));
        self.changed.notify_all();
        changed
    }

    /// Asks for one more image.
    fn ask(&self) {
        self.with(|asked| asked.asked += 1);
    }

    /// Waits until every image that was asked for is taken.
    fn wait(&self) {
        let began = Instant::now();
        let mut asked = self.asked.lock().expect("the images asked for");
        while asked.taken < asked.asked {
            assert!(began.elapsed() < LONG, "the freezer took no image");
            let waited = self.changed.wait_timeout(asked, Duration::from_secs(1));
            asked = waited.expect("the images asked for").0;
        }
    }

    /// For the freezer: waits until an image is asked for. `false` once
    /// the committer has gone and none is.
    fn next(&self) -> bool {
        let mut asked = self.asked.lock().expect("the images asked for");
        while asked.taken == asked.asked && !asked.ended {
            asked = self.changed.wait(asked).expect("the images asked for");
        }
        asked.taken < asked.asked
    }

    fn took(&self) {
        self.with(|asked| asked.taken += 1);
    }

    fn end(&self) {
        self.with(|asked| asked.ended = true);
    }
}

/// Waits until a table checkpoint that covers every commit made so far
/// is in force. The caller commits nothing meanwhile and holds no private
/// copy, so the thread that takes checkpoints comes to it by itself: its
/// next checkpoint that begins from now on writes every page and covers
/// the latest version. Each call that follows a commit is one more
/// checkpoint installed.
fn settle(db: &Db) {
    let (latest, began) = (db.latest(), Instant::now());
    while !(db.state.in_force()).is_some_and(|(covers, _)| covers >= latest) {
        assert!(
            began.elapsed() < LONG,
            "no table checkpoint covered version {latest}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// What the committer builds at a round where it holds no transaction
/// and no version is pinned, on the page `at`, which has a cell in slot
/// 1, with `long` cells of the latest versions holding an out-of-line
/// value. Returns how many table checkpoints it waited for, and an image
/// of the device with a commit that no sync had reached, with the version
/// that was durable before it and that commit's.
///
/// 1. **Every value has its place.** A table checkpoint covers a version
///    only once every page of it is written, and a page with a pending
///    value is not written. So when one covers the latest version, each
///    of the `long` cells holds a pointer.
/// 2. **A version is held for its readers.** With write-back's files
///    locked, as a pass that is slow holds them, no pass is made: two
///    commits change the page, and the pass after them finds two versions
///    of it that are not written. No reader can ask for the first, and it
///    is kept in the pool and not written.
/// 3. **A run of deltas is written.** The page is changed in one hash and
///    written back, twice. Its chain takes two runs before a base image
///    is due, so if the first of the two was written as a base image, the
///    second is a run behind it.
/// 4. **An image has a commit that is not durable.** The log is synced
///    for a table checkpoint and by a roll of the log, and by nothing
///    else in this store. A checkpoint holds the lock on the checkpoints
///    and a roll the manifest's: with both locked, a commit is in the
///    log and no sync has reached it when the image is taken.
fn built(
    db: &Db,
    fs: &SimVfs,
    at: u32,
    long: usize,
    byte: u8,
) -> (u64, (CommitVersion, CommitVersion, CrashImage)) {
    let change = |turn: u8| db.commit(&[hash((at, 1), byte.wrapping_add(turn))]);

    settle(db);
    let placed: usize = (1..=PAGES as u32)
        .filter_map(|id| db.frame(page(id), db.latest()).ok())
        .map(|frame| {
            let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
            tree.cells()
                .filter(|(_, cell)| cell.value_pointer().is_some())
                .count()
        })
        .sum();
    assert_eq!(placed, long, "the cells that hold a pointer");

    let held = db.stats().held_for_readers;
    {
        let _no_pass = db.state.files.lock().expect("write-back's files");
        change(1);
        change(2);
    }
    settle(db);
    let stats = db.stats();
    assert!(stats.held_for_readers > held, "{stats:?}");

    let runs = stats.delta_blocks;
    for turn in 3..5 {
        change(turn);
        settle(db);
    }
    let stats = db.stats();
    assert!(stats.delta_blocks > runs, "{stats:?}");

    let no_checkpoint = db.state.checkpoints.lock().expect("the checkpoints");
    let no_roll = db.manifest.lock().expect("the manifest");
    let durable = db.log.durable().version;
    let latest = change(5);
    let image = fs.crash_image();
    let synced = db.log.durable().version;
    drop(no_roll);
    drop(no_checkpoint);
    assert!(synced < latest, "{synced} is durable of {latest}");
    (4, (durable, latest, image))
}

const PAGES: u64 = 6;
const ROUNDS: u64 = 400;
/// The rounds at whose top an image is asked for: every fourth.
const IMAGES: usize = ROUNDS as usize / 4;
/// The rounds from which the committer builds its states ([`built`]) as
/// soon as it holds no transaction and no version is pinned.
const BUILDS: [u64; 3] = [110, 230, 350];

#[test]
fn passes_checkpoints_and_commits_at_once_leave_states_that_recover() {
    let fs = sim_fs(SimConfig::new(23));
    // The committer is made to wait for write-back often, so that the
    // threads take turns however fast each of them is.
    let config = FlushConfig {
        max_chain_deltas: 2,
        max_unwritten: 3,
        checkpoint_log_bytes: 1500,
        idle: std::time::Duration::from_millis(1),
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::create(shared(&fs), db_dir(), config, log_options());
    let wb = db.wb();
    let done = AtomicBool::new(false);
    let images = Images::default();
    let frozen = Mutex::new(Vec::new());
    // The latest version when each image was known to be taken.
    let mut ends = Vec::new();
    // The images of a commit that no sync had reached, how many table
    // checkpoints the committer waited for, and the most cells it found
    // with a value that had its place.
    let (mut unsynced, mut settled, mut most_long) = (Vec::new(), 0, 0);

    let freeze = || {
        let durable = db.log.durable().version;
        let image = fs.crash_image();
        frozen.lock().expect("the images").push((durable, image));
    };

    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|error| panic!("a turn of the loop failed: {error}")));
        let _stop = StopOnDrop(wb);
        scope.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                wb.pass().expect("a pass");
                wb.checkpoint().expect("a checkpoint, or one put off");
                std::thread::yield_now();
            }
        });
        scope.spawn(|| {
            while images.next() {
                freeze();
                images.took();
            }
        });

        let _done = Done(&done, &images);
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        // For each page, the slots in use: the next cell goes behind them.
        let mut cells = vec![0u16; PAGES as usize + 1];
        // The cells whose value is out of line.
        let mut long_cells = BTreeSet::new();
        let mut held: Option<(Txn<'_>, u64)> = None;
        let (mut asked, mut pinned, mut builds) = (false, false, 0);
        for round in 0..=ROUNDS {
            // The image of the round before is taken before this round
            // begins, so an image holds no commit of a later round.
            if asked {
                images.wait();
                ends.push(db.latest());
                asked = false;
            }
            if round == ROUNDS {
                break;
            }
            if round % 4 == 0 {
                images.ask();
                asked = true;
            }
            builds += usize::from(BUILDS.contains(&round));
            // Every fifth transaction holds a copy of a page over some
            // rounds before it commits, and holds back what waits for it.
            match &held {
                Some((_, until)) if *until > round => {
                    std::thread::yield_now();
                    continue;
                }
                None if rng.below(5) == 0 => {
                    // A page that exists: a copy of one that does not
                    // would make it, with nothing in the log.
                    let copied = 1 + rng.below(PAGES);
                    if cells[copied as usize] > 0 {
                        let mut txn = db.begin();
                        txn.copy(page(copied as u32));
                        held = Some((txn, round + 1 + rng.below(3)));
                        continue;
                    }
                }
                _ => {}
            }
            // A snapshot pins a version for a while, so that the versions
            // behind it get blocks of their own.
            match round % 40 {
                10 => {
                    db.versions.pin(db.latest());
                    pinned = true;
                }
                25 => {
                    db.versions.unpin();
                    pinned = false;
                }
                _ => {}
            }
            if round % 8 == 0 {
                db.pool.reclaim(&db.versions);
            }
            if builds > 0 && held.is_none() && !pinned {
                builds -= 1;
                let at = (1..=PAGES).find(|&id| cells[id as usize] > 0);
                let at = at.expect("a page with a cell") as u32;
                let (checkpoints, image) = built(&db, &fs, at, long_cells.len(), round as u8);
                settled += checkpoints;
                most_long = long_cells.len();
                unsynced.push(image);
                wb.place_ready().expect("placed");
                wb.wait_for_room().expect("room");
            }

            // Now and then a flat leaf is written whole, with a value
            // that write-back places by the place its entry holds.
            if held.is_none() && round % 16 == 5 {
                let value = long(round as u8, 300 + rng.below(300) as usize);
                let entries: [(&[u8], Val); 2] = [(b"flat", inline(b"leaf")), (b"long", value)];
                db.commit_flat(page(20), &entries, &BTreeMap::new());
                wb.place_ready().expect("placed");
                wb.wait_for_room().expect("room");
                continue;
            }

            let id = 1 + rng.below(PAGES);
            let used = cells[id as usize];
            let any = |rng: &mut Rng| slot(1 + rng.below(u64::from(used)) as u16);
            let op = match rng.below(4) {
                0 if used > 0 => Op::Hash {
                    page: page(id as u32),
                    slot: any(&mut rng),
                    byte: round as u8,
                },
                1 if used > 0 => {
                    let at = 1 + rng.below(u64::from(used)) as u16;
                    long_cells.insert((id, at));
                    Op::Value {
                        page: page(id as u32),
                        slot: slot(at),
                        value: long(round as u8, 300 + rng.below(400) as usize),
                    }
                }
                _ if used < 12 => {
                    cells[id as usize] += 1;
                    let value = match rng.below(3) {
                        0 => inline(b"in the cell"),
                        _ => {
                            long_cells.insert((id, used + 1));
                            long(round as u8, 300 + rng.below(500) as usize)
                        }
                    };
                    insert((id as u32, used + 1), &round.to_be_bytes(), value)
                }
                _ => hash((id as u32, 1), round as u8),
            };
            match held.take() {
                Some((txn, _)) => txn.commit(&[op]),
                None => db.commit(&[op]),
            };
            wb.place_ready().expect("placed");
            wb.wait_for_room().expect("room");

            // A value that has its place is moved, as the cleaner does.
            let moved = 1 + rng.below(PAGES);
            let slots = cells[moved as usize];
            if rng.below(4) == 0 && slots > 0 {
                let at = slot(1 + rng.below(u64::from(slots)) as u16);
                let frame = db.frame(page(moved as u32), db.latest()).expect("the page");
                let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
                let placed = tree.cell(at).and_then(|cell| cell.value_pointer());
                drop(frame);
                if placed.is_some() {
                    db.move_value(page(moved as u32), at, false);
                }
            }
        }
        if let Some((txn, _)) = held.take() {
            txn.roll_back();
        }
    });

    // What the threads left is whole.
    db.versions.unpin();
    assert_eq!(db.stats().runs_refused, 0);
    db.check_chains();
    db.check_warm();
    // The committer built its states at each of the rounds for it, where
    // a version held for its readers and a run of deltas were counted. It
    // waited for a table checkpoint of its own each time, and every value
    // that was out of line then had been placed.
    let stats = db.stats();
    assert_eq!(unsynced.len(), BUILDS.len());
    assert!(stats.checkpoints >= settled, "{settled}: {stats:?}");
    assert!(most_long > 20, "{most_long} cells");
    assert!(stats.values_placed >= most_long as u64, "{stats:?}");

    // And so is every state that a crash would have left on the way: one
    // image for each round that asked for one, and no commit in it that
    // was made after the round.
    let mut ran = Ran::of(&fs, &db);
    let frozen = frozen.into_inner().expect("the images");
    assert_eq!((frozen.len(), ends.len()), (IMAGES, IMAGES));
    let mut tally = Tally::default();
    let step = frozen.len().div_ceil(40);
    for ((durable, image), latest) in frozen.iter().zip(&ends).step_by(step) {
        (ran.durable, ran.latest) = (*durable, *latest);
        check_image(image, &ran, &PLANS, &mut tally);
    }
    // A crash can lose or keep a commit that no sync had reached.
    for (durable, latest, image) in &unsynced {
        let before = tally.states;
        (ran.durable, ran.latest) = (*durable, *latest);
        check_image(image, &ran, &PLANS, &mut tally);
        assert!(tally.states > before + 1, "{tally:?}");
    }
    assert!(tally.states > tally.crashes, "{tally:?}");

    let (covers, ..) = installed(wb.checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    db.check_cold();
}
