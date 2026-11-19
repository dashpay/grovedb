//! Write-back on several threads at once: a committer, the background
//! loop, a second thread that makes passes and takes table checkpoints of
//! its own, and a moving value. Now and then the file system is frozen as
//! a crash at that moment would leave it, and every state that such a
//! crash can leave must recover to one committed version, whole.

use std::sync::atomic::AtomicBool;

use lignin_io::sim::SimConfig;

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
struct Done<'a>(&'a AtomicBool);

impl Drop for Done<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

const PAGES: u64 = 6;
const ROUNDS: u64 = 400;

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
    let frozen = Mutex::new(Vec::new());

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
            while !done.load(Ordering::SeqCst) {
                freeze();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });

        let _done = Done(&done);
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        // For each page, the slots in use: the next cell goes behind them.
        let mut cells = vec![0u16; PAGES as usize + 1];
        let mut held: Option<(Txn<'_>, u64)> = None;
        for round in 0..ROUNDS {
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
                10 => db.versions.pin(db.latest()),
                25 => db.versions.unpin(),
                _ => {}
            }
            if round % 8 == 0 {
                db.pool.reclaim(&db.versions);
                freeze();
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
                1 if used > 0 => Op::Value {
                    page: page(id as u32),
                    slot: any(&mut rng),
                    value: long(round as u8, 300 + rng.below(400) as usize),
                },
                _ if used < 12 => {
                    cells[id as usize] += 1;
                    let value = match rng.below(3) {
                        0 => inline(b"in the cell"),
                        _ => long(round as u8, 300 + rng.below(500) as usize),
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
    let stats = db.stats();
    assert!(stats.checkpoints > 0, "{stats:?}");
    assert!(stats.values_placed > 20, "{stats:?}");
    assert!(
        stats.delta_blocks > 0 && stats.held_for_readers > 0,
        "{stats:?}"
    );

    // And so is every state that a crash would have left on the way.
    let mut ran = Ran::of(&fs, &db);
    let frozen = frozen.into_inner().expect("the images");
    assert!(
        frozen.len() >= ROUNDS as usize / 8,
        "{} images",
        frozen.len()
    );
    let mut tally = Tally::default();
    let step = frozen.len().div_ceil(40);
    for (durable, image) in frozen.iter().step_by(step) {
        ran.durable = *durable;
        check_image(image, &ran, &PLANS, &mut tally);
    }
    assert!(tally.states > tally.crashes, "{tally:?}");

    let (covers, ..) = installed(wb.checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    db.check_cold();
}
