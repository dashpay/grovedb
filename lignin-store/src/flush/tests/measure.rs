//! How long a pending value waits, and how much log write-back holds
//! meanwhile, when blocks are applied back to back (`transactions.md`
//! 18.8, open point 2).
//!
//! No committer exists yet, so the engine cannot run the reference
//! workload through commits. Blocks of its shape are committed to the real
//! store instead: 20 transitions to a block, each of which changes a hash
//! in the page at the root and in one page of each of three levels under
//! it, writes one value of 460 bytes out of line into a leaf, and four
//! times in five carries a placed value from one leaf to another, where it
//! is pending again (`log-format.md` section 9: 7.29 pages dirtied, one
//! value written and 0.8 carried for each transition; here 6.6 pages).
//!
//! The blocks are back to back in the worst way there is for write-back:
//! the transaction of the next block has made every one of its private
//! copies before write-back gets its turn, which is one turn of its loop
//! for a block ([`WriteBack::tick`]).

use std::collections::HashMap;

use super::*;

const TRANSITIONS: u64 = 20;
const VALUE_LEN: usize = 460;
const LEAVES: u64 = 256;
const FIRST_LEAF: u32 = 1000;
const CELLS_TO_A_LEAF: usize = 20;
/// The cells of the page at the root that take a value out of line, one
/// of them in every block, where a run has such values.
const HOT_CELLS: u64 = 12;

struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// How the committer and write-back take turns.
#[derive(Debug, Clone, Copy, Default)]
struct Turns {
    /// The committer calls `place_ready` once its commit is published.
    places_after_commit: bool,
    /// The committer asks for room when it holds no private copy.
    waits_between: bool,
    /// Every block writes one more value out of line into the page at the
    /// root, which every block copies. The reference workload has no such
    /// page.
    hot_value: bool,
}

#[derive(Debug, Default)]
struct Measured {
    /// For every value that was placed, how many blocks after the one
    /// that wrote it.
    waits: Vec<u64>,
    /// How many values were pending when the run ended, and the age of
    /// the oldest in blocks.
    pending_at_end: (usize, u64),
    /// After each block, the bytes of log behind the oldest change that is
    /// in no block: the log that pending values and unwritten pages hold.
    log_behind: Vec<u64>,
    /// After each block, the bytes of log that no table checkpoint covers:
    /// the log that write-back holds in all.
    log_held: Vec<u64>,
    /// The table checkpoints that were installed while the blocks came.
    checkpoints: u64,
    /// The bytes of log one block wrote, on average.
    block_bytes: u64,
    /// The most bytes of pending values that frames held after a block.
    peak_pending_bytes: usize,
}

impl Measured {
    fn longest_wait(&self) -> u64 {
        self.waits.iter().copied().max().unwrap_or(0)
    }

    fn mean_wait(&self) -> f64 {
        self.waits.iter().sum::<u64>() as f64 / self.waits.len().max(1) as f64
    }

    fn most_log_behind(&self) -> u64 {
        self.log_behind.iter().copied().max().unwrap_or(0)
    }

    fn most_log_held(&self) -> u64 {
        self.log_held.iter().copied().max().unwrap_or(0)
    }
}

/// The bytes of log behind the oldest commit of which a change is in no
/// block yet.
fn log_behind(db: &Db) -> u64 {
    let Some(floor) = db.wb().floor() else {
        return 0;
    };
    let kept = db.state.kept.lock().expect("what is kept");
    let before = kept.logged_before(floor.get());
    let logged = db.state.logged_in_all.load(Ordering::Relaxed);
    before.map_or(0, |before| logged - before)
}

/// A cell of a leaf: its slot, the seed of its value, and the block that
/// put it there.
type LeafCell = (u16, u8, u64);

/// The pages of the store and what the cells of its leaves hold.
struct Shape {
    rng: Rng,
    /// For each leaf, its next free slot and its cells: slot and the seed
    /// of the value, with the block that put it there.
    leaves: Vec<(u16, Vec<LeafCell>)>,
    keys: u64,
}

impl Shape {
    fn new() -> Self {
        Self {
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            leaves: (0..LEAVES).map(|_| (1, Vec::new())).collect(),
            keys: 0,
        }
    }

    /// The commit that makes the upper pages, each with the cell whose
    /// hash the transitions change, and the cells of the root that a run
    /// with a hot value writes into.
    fn first(&self) -> Vec<Op> {
        let mut ops: Vec<Op> = (1..=85)
            .map(|id| insert((id, 1), b"node", inline(b"n")))
            .collect();
        ops.extend(
            (0..HOT_CELLS)
                .map(|cell| insert((1, 2 + cell as u16), &[b'h', cell as u8], inline(b"h"))),
        );
        ops
    }

    fn leaf_with_room(&mut self) -> usize {
        loop {
            let leaf = self.rng.below(LEAVES) as usize;
            if self.leaves[leaf].1.len() < CELLS_TO_A_LEAF {
                return leaf;
            }
        }
    }

    fn put(&mut self, leaf: usize, seed: u8, block: u64, ops: &mut Vec<Op>) {
        let (next, cells) = &mut self.leaves[leaf];
        let slot = *next;
        *next += 1;
        cells.push((slot, seed, block));
        self.keys += 1;
        ops.push(insert(
            (FIRST_LEAF + leaf as u32, slot),
            &self.keys.to_be_bytes(),
            long(seed, VALUE_LEN),
        ));
    }

    /// The changes of block `block`. `is_placed` says whether the cell in
    /// a slot of a leaf holds a pointer by now: only such a cell is
    /// carried.
    fn block(&mut self, block: u64, turns: Turns, is_placed: impl Fn(u32, u16) -> bool) -> Vec<Op> {
        let mut ops = Vec::new();
        for transition in 0..TRANSITIONS {
            let byte = (block * TRANSITIONS + transition) as u8;
            let path = [
                1,
                2 + self.rng.below(4) as u32,
                6 + self.rng.below(16) as u32,
                22 + self.rng.below(64) as u32,
            ];
            ops.extend(path.map(|id| hash((id, 1), byte)));
            let leaf = self.leaf_with_room();
            self.put(leaf, byte, block, &mut ops);
            if self.rng.below(5) < 4 {
                let from = self.rng.below(LEAVES) as usize;
                let id = FIRST_LEAF + from as u32;
                let carried = self.leaves[from]
                    .1
                    .iter()
                    .position(|&(slot, _, put)| put < block && is_placed(id, slot));
                if let Some(at) = carried {
                    let (slot, seed, _) = self.leaves[from].1.remove(at);
                    ops.push(Op::Remove {
                        page: page(id),
                        slot: super::slot(slot),
                    });
                    let to = self.leaf_with_room();
                    self.put(to, seed, block, &mut ops);
                }
            }
        }
        if turns.hot_value {
            ops.push(Op::Value {
                page: page(1),
                slot: slot(2 + (block % HOT_CELLS) as u16),
                value: long(block as u8, VALUE_LEN),
            });
        }
        ops
    }
}

/// Applies `blocks` blocks back to back, with the turns of `turns`.
fn apply(blocks: u64, turns: Turns, config: FlushConfig) -> Measured {
    let db = Db::with(config);
    let wb = db.wb();
    let mut shape = Shape::new();
    db.commit(&shape.first());
    wb.pass().expect("a pass");
    let logged_before = db.state.logged_in_all.load(Ordering::Relaxed);

    let mut measured = Measured::default();
    let mut written_in: HashMap<[u8; 8], u64> = HashMap::new();
    for block in 1..=blocks {
        let ops = shape.block(block, turns, |id, slot| {
            !db.pending(page(id))
                .iter()
                .any(|(pending, _)| pending.get() == slot)
        });
        // The transaction of this block has all its copies before
        // write-back gets its turn.
        let mut txn = db.begin();
        for id in ops.iter().map(Op::page).collect::<BTreeSet<_>>() {
            txn.copy(id);
        }
        wb.tick().expect("a turn");
        txn.commit(&ops);
        if turns.places_after_commit {
            wb.place_ready().expect("placed");
        }
        if turns.waits_between {
            wb.wait_for_room().expect("room");
        }

        let pages = (1..=85).chain((0..LEAVES as u32).map(|leaf| FIRST_LEAF + leaf));
        let pending: BTreeSet<[u8; 8]> = pages
            .filter(|&id| id <= 85 || shape.leaves[(id - FIRST_LEAF) as usize].0 > 1)
            .flat_map(|id| db.pending(page(id)))
            .map(|(_, pending)| pending.id())
            .collect();
        written_in.retain(|id, written| {
            let waits = pending.contains(id);
            if !waits {
                measured.waits.push(block - *written);
            }
            waits
        });
        for id in &pending {
            written_in.entry(*id).or_insert(block);
        }
        measured.log_behind.push(log_behind(&db));
        measured.log_held.push(wb.log_held());
        measured.peak_pending_bytes = measured.peak_pending_bytes.max(db.pool.attached_bytes());
    }
    let oldest = written_in.values().map(|written| blocks - written).max();
    measured.pending_at_end = (written_in.len(), oldest.unwrap_or(0));
    let logged = db.state.logged_in_all.load(Ordering::Relaxed) - logged_before;
    measured.block_bytes = logged / blocks;
    measured.checkpoints = db.stats().checkpoints;

    // Whatever the turns were, nothing is lost: once the blocks stop,
    // everything is written back and a checkpoint covers it.
    let checkpointed = wb.checkpoint().expect("a checkpoint");
    assert!(
        matches!(checkpointed, Checkpointed::Installed { covers, .. } if covers == db.latest()),
        "{checkpointed:?}"
    );
    db.check_cold();
    println!(
        "{turns:?}: {} values placed, wait mean {:.2} and longest {} blocks; {} pending at the \
         end, the oldest for {} blocks; a block logs {} bytes; log behind what is unwritten at \
         most {:.1} blocks; log held at most {:.1} blocks; {} checkpoints; pending bytes at most {}",
        measured.waits.len(),
        measured.mean_wait(),
        measured.longest_wait(),
        measured.pending_at_end.0,
        measured.pending_at_end.1,
        measured.block_bytes,
        measured.most_log_behind() as f64 / measured.block_bytes as f64,
        measured.most_log_held() as f64 / measured.block_bytes as f64,
        measured.checkpoints,
        measured.peak_pending_bytes,
    );
    measured
}

const BLOCKS: u64 = 60;
/// What one block logs, roughly: the bounds of the runs are counted in it.
const BLOCK_BYTES: u64 = 23_000;

/// A table checkpoint is due after four blocks, and the committer is made
/// to wait once eight blocks of log are held.
fn bounded() -> FlushConfig {
    FlushConfig {
        checkpoint_log_bytes: 4 * BLOCK_BYTES,
        max_log_held: 8 * BLOCK_BYTES,
        ..FlushConfig::new(Db::SIZE)
    }
}

/// The same, with no bound on the log that is held.
fn unbounded() -> FlushConfig {
    FlushConfig {
        max_log_held: u64::MAX,
        ..bounded()
    }
}

/// **Blocks of the reference workload's shape, back to back.** The turn
/// after a commit puts its values into the value log, and places those
/// whose page the next block has not copied. The others are placed when
/// that block has committed. So every value is placed one block after its
/// own, and the log behind what is unwritten is that of the last few
/// blocks and no more: of the block that was just committed, of the one
/// before it, whose last values were placed after the pass, and of a page
/// version that was superseded while it waited.
#[test]
fn a_pending_value_waits_one_block_when_the_committer_places_after_its_commit() {
    let turns = Turns {
        places_after_commit: true,
        ..Turns::default()
    };
    let measured = apply(BLOCKS, turns, unbounded());
    assert!(measured.waits.len() as u64 > BLOCKS * TRANSITIONS);
    assert_eq!(measured.longest_wait(), 1);
    assert_eq!(
        measured.pending_at_end.1, 0,
        "{:?}",
        measured.pending_at_end
    );
    assert!(
        measured.most_log_behind() <= 5 * measured.block_bytes,
        "{} of {}",
        measured.most_log_behind(),
        measured.block_bytes
    );
}

/// Without the call after the commit, a value on a page that the next
/// blocks keep copying waits for a block that leaves its page alone. On
/// this shape that is a few blocks, by chance and with no bound.
#[test]
fn without_the_call_after_the_commit_a_value_waits_for_a_block_that_leaves_its_page() {
    let measured = apply(BLOCKS, Turns::default(), unbounded());
    let with = Turns {
        places_after_commit: true,
        ..Turns::default()
    };
    let better = apply(BLOCKS, with, unbounded());
    assert!(measured.longest_wait() >= better.longest_wait());
    assert!(measured.mean_wait() >= better.mean_wait());
}

/// **A table checkpoint needs a moment at which no write transaction holds
/// back a page that is unwritten**, and blocks that come back to back
/// leave none: when write-back has its turn, the next block has copied
/// pages whose values wait for it. So no checkpoint is taken, and the log
/// that is held grows with every block, though every page is written one
/// block late.
///
/// The committer makes the moment: once more log is held than the bound,
/// it waits where it holds no copy, and the turn it waits for writes
/// everything and takes the checkpoint.
#[test]
fn back_to_back_a_checkpoint_is_taken_when_the_committer_waits_for_it() {
    let turns = Turns {
        places_after_commit: true,
        ..Turns::default()
    };
    let unbounded = apply(BLOCKS, turns, unbounded());
    assert_eq!(unbounded.checkpoints, 0);
    assert!(
        unbounded.most_log_held() >= (BLOCKS - 1) * unbounded.block_bytes,
        "{} of {}",
        unbounded.most_log_held(),
        unbounded.block_bytes
    );

    let waits = Turns {
        waits_between: true,
        ..turns
    };
    let config = bounded();
    let measured = apply(BLOCKS, waits, config);
    assert!(
        measured.checkpoints >= BLOCKS / 10,
        "{}",
        measured.checkpoints
    );
    assert!(
        measured.most_log_held() <= config.max_log_held + 2 * measured.block_bytes,
        "{} of {}",
        measured.most_log_held(),
        measured.block_bytes
    );
    assert_eq!(measured.longest_wait(), 1);
}

/// **A page that every block copies and that gets a new value out of line
/// in every block** is never without a pending value when a pass looks at
/// it, so it is never written, and the log behind it grows with every
/// block. The call after the commit places each value, and does not help
/// the page. The committer's wait does: in it the page is written.
#[test]
fn a_page_with_a_new_value_in_every_block_holds_the_log_until_the_committer_waits() {
    let hot = Turns {
        places_after_commit: true,
        hot_value: true,
        ..Turns::default()
    };
    let measured = apply(BLOCKS, hot, unbounded());
    assert_eq!(measured.longest_wait(), 1);
    assert!(
        measured.most_log_behind() > (BLOCKS - 2) * measured.block_bytes * 9 / 10,
        "{} of {}",
        measured.most_log_behind(),
        measured.block_bytes
    );

    let waits = Turns {
        waits_between: true,
        ..hot
    };
    let config = bounded();
    let bounded = apply(BLOCKS, waits, config);
    assert!(
        bounded.most_log_held() <= config.max_log_held + 2 * bounded.block_bytes,
        "{} of {}",
        bounded.most_log_held(),
        bounded.block_bytes
    );
    assert!(bounded.most_log_behind() <= bounded.most_log_held());
    assert!(
        bounded.checkpoints >= BLOCKS / 10,
        "{}",
        bounded.checkpoints
    );
    assert_eq!(bounded.longest_wait(), 1);
}
