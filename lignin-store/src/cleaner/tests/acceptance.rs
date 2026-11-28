//! The acceptance test of STORE-08, on the page store: **"Dead space
//! stays at or below 20% under the reference workload with no open
//! snapshot."**
//!
//! GroveDB does not run on the engine yet, so the reference workload
//! cannot be run through it. Blocks of its shape are committed to the
//! real store instead, as write-back's own measurement does
//! (`flush/tests/measure.rs`): 20 transitions to a block, each of which
//! changes a hash in the page at the root and in one page of each of
//! three levels under it, and writes one value of 460 bytes out of line
//! into a leaf. The tree grows as a tree does: a leaf that is full is
//! split, and half of its cells are carried to a new leaf, each with its
//! value appended again (`value-log-format.md` O5). That is about one
//! value carried for each one written, where the reference workload has
//! 0.8.
//!
//! After every block write-back has one turn and the cleaner one pass
//! over the segments and one over the value log, as their loops give
//! them, with the settings a store is opened with. No snapshot is open.
//! The dead space is then read from the page table and from the counts
//! of the value log.

use std::collections::BTreeSet;

use super::*;
use crate::flush::tests::Txn;

const TRANSITIONS: u64 = 20;
const VALUE_LEN: usize = 460;
const FIRST_LEAF: u32 = 1000;
const CELLS_TO_A_LEAF: usize = 20;
/// The upper pages: the root, and three levels of 4, 16 and 64 pages.
const UPPER: u32 = 85;

struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// A leaf: the slots in use, each with the seed of its value, and the
/// slots that were in use and are free again.
#[derive(Default)]
struct Leaf {
    cells: Vec<(u16, u8)>,
    free: Vec<u16>,
    next: u16,
}

impl Leaf {
    fn slot(&mut self) -> u16 {
        self.free.pop().unwrap_or_else(|| {
            self.next += 1;
            self.next
        })
    }
}

/// The pages of the store and what the cells of its leaves hold.
struct Shape {
    rng: Rng,
    leaves: Vec<Leaf>,
    keys: u64,
    carried: u64,
}

impl Shape {
    fn new() -> Self {
        Self {
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            leaves: vec![Leaf::default()],
            keys: 0,
            carried: 0,
        }
    }

    /// The commit that makes the upper pages, each with the cell whose
    /// hash the transitions change.
    fn first(&self) -> Vec<Op> {
        (1..=UPPER)
            .map(|id| insert((id, 1), b"node", inline(b"n")))
            .collect()
    }

    fn put(&mut self, leaf: usize, seed: u8, ops: &mut Vec<Op>) {
        let slot = self.leaves[leaf].slot();
        self.leaves[leaf].cells.push((slot, seed));
        self.keys += 1;
        ops.push(insert(
            (FIRST_LEAF + leaf as u32, slot),
            &self.keys.to_be_bytes(),
            long(seed, VALUE_LEN),
        ));
    }

    /// The changes of one block. `is_placed` says whether the cell in a
    /// slot of a leaf holds a pointer by now: only such a cell is carried.
    fn block(&mut self, block: u64, is_placed: impl Fn(u32, u16) -> bool) -> Vec<Op> {
        let mut ops = Vec::new();
        // A leaf takes part in one split to a block at most, so that no
        // slot is given up and taken again in one commit.
        let mut split = BTreeSet::new();
        for transition in 0..TRANSITIONS {
            let byte = (block * TRANSITIONS + transition) as u8;
            let path = [
                1,
                2 + self.rng.below(4) as u32,
                6 + self.rng.below(16) as u32,
                22 + self.rng.below(64) as u32,
            ];
            ops.extend(path.map(|id| hash((id, 1), byte)));
            let leaf = self.rng.below(self.leaves.len() as u64) as usize;
            let full = self.leaves[leaf].cells.len() >= CELLS_TO_A_LEAF;
            if full && split.insert(leaf) {
                // The leaf is split: the cells of its upper half whose
                // values have their place go to a new leaf.
                let id = FIRST_LEAF + leaf as u32;
                let (stay, go): (Vec<_>, Vec<_>) = (self.leaves[leaf].cells.iter().copied())
                    .enumerate()
                    .partition(|(at, (slot, _))| {
                        *at < CELLS_TO_A_LEAF / 2 || !is_placed(id, *slot)
                    });
                self.leaves[leaf].cells = stay.into_iter().map(|(_, cell)| cell).collect();
                let to = self.leaves.len();
                self.leaves.push(Leaf::default());
                split.insert(to);
                for (_, (slot, seed)) in go {
                    ops.push(Op::Remove {
                        page: page(id),
                        slot: super::slot(slot),
                    });
                    self.leaves[leaf].free.push(slot);
                    self.put(to, seed, &mut ops);
                    self.carried += 1;
                }
            }
            let leaf = match self.leaves[leaf].cells.len() >= CELLS_TO_A_LEAF {
                // Full, and split in this block already: the newest leaf
                // has room.
                true => self.leaves.len() - 1,
                false => leaf,
            };
            self.put(leaf, byte, &mut ops);
        }
        ops
    }
}

/// What a run came to.
#[derive(Debug, Default)]
struct Run {
    /// The most dead space among the bytes of all blocks, in a hundred,
    /// after a block, once the store held as many bytes of blocks as
    /// `from` says.
    most_dead: f64,
    /// The same in the value log.
    most_dead_values: f64,
    /// The dead space when the run ended.
    dead_at_end: f64,
    dead_values_at_end: f64,
    /// The bytes of blocks and of values the store held when the run
    /// ended, and the leaves.
    block_bytes: u64,
    value_bytes: u64,
    leaves: usize,
    /// Bytes written for each transition: by write-back to segments and
    /// to the value log, and by the cleaner.
    flushed_per_transition: f64,
    cleaned_per_transition: f64,
    /// Values carried for each one written.
    carried: f64,
    blocks_measured: u64,
}

/// Commits `blocks` blocks of the reference workload's shape to a new
/// store, with the turns of write-back and of the cleaner after each, and
/// measures the dead space after every block from the one at which the
/// store holds `from` bytes of blocks. `cleaner` says whether a cleaner
/// runs at all.
fn run(blocks: u64, from: u64, config: Option<CleanerConfig>, report_every: u64) -> Run {
    // A table checkpoint for every 30 blocks or so, as a store that has
    // run for a while has one in force: what the cleaner empties of the
    // segments it points into waits for the next one.
    let db = Db::with(FlushConfig {
        checkpoint_log_bytes: 512 << 10,
        ..FlushConfig::new(Db::SIZE)
    });
    let wb = db.wb();
    let state = CleanerState::new(config.unwrap_or_default());
    let stats = crate::stats::Stats::new();
    let cleaner = Cleaner::new(wb, &state).with_stats(&stats);
    let mut shape = Shape::new();
    db.commit(&shape.first());
    wb.pass().expect("a pass");

    let mut run = Run::default();
    let mut most_waiting = 0f64;
    for block in 1..=blocks {
        let ops = shape.block(block, |id, slot| {
            let pending = db.pending(page(id));
            !pending.iter().any(|(pending, _)| pending.get() == slot)
        });
        let txn: Txn<'_> = db.begin();
        txn.commit(&ops);
        // What a committer does once it is published, and then one turn
        // of each background loop.
        wb.place_ready().expect("placed");
        wb.wait_for_room().expect("room");
        wb.tick().expect("a turn of write-back");
        if config.is_some() {
            cleaner.pass().expect("a pass over the segments");
            (cleaner.pass_values(&Cells(&db))).expect("a pass over the value log");
        }
        // Nothing is pinned, so no reader asks for an older version of a
        // page: the model keeps the latest of each.
        for versions in db.model.lock().expect("the model").pages.values_mut() {
            versions.drain(..versions.len() - 1);
        }

        let space = cleaner.space().expect("the space");
        let values = cleaner.value_space().expect("the value log");
        if space.block_bytes() >= from {
            let waiting = space.retired_bytes() as f64 * 100.0 / space.block_bytes() as f64;
            most_waiting = most_waiting.max(waiting);
            run.most_dead = run.most_dead.max(space.dead_percent());
            run.most_dead_values = run.most_dead_values.max(values.dead_percent());
            run.blocks_measured += 1;
        }
        if block % report_every == 0 || block == blocks {
            let flushed = db.stats();
            println!(
                "block {block}: {} leaves, {} KiB of blocks in {} segments, {:.1}% dead \
                 ({:.1}% waiting to be deleted); {} KiB of values in {} files, {:.1}% dead; \
                 write-back wrote {} KiB, the cleaner {} KiB",
                shape.leaves.len(),
                space.block_bytes() >> 10,
                space.segments.len(),
                space.dead_percent(),
                space.retired_bytes() as f64 * 100.0 / space.block_bytes().max(1) as f64,
                values.bytes >> 10,
                values.files,
                values.dead_percent(),
                (flushed.stored_bytes + flushed.value_stored_bytes) >> 10,
                stats.cleaner_bytes() >> 10,
            );
        }
        if block == blocks {
            run.dead_at_end = space.dead_percent();
            run.dead_values_at_end = values.dead_percent();
            run.block_bytes = space.block_bytes();
            run.value_bytes = values.bytes;
        }
    }
    let transitions = (blocks * TRANSITIONS) as f64;
    let flushed = db.stats();
    run.flushed_per_transition =
        (flushed.stored_bytes + flushed.value_stored_bytes) as f64 / transitions;
    run.cleaned_per_transition = stats.cleaner_bytes() as f64 / transitions;
    run.carried = shape.carried as f64 / transitions;
    run.leaves = shape.leaves.len();
    let did = state.stats();
    println!(
        "{run:?}\n{did:?}\nat most {most_waiting:.1}% waited to be deleted; {} table checkpoints, \
         {} of them asked for by the cleaner",
        flushed.checkpoints, did.checkpoints
    );
    assert_eq!(did.errors, 0, "{did:?}");

    // Nothing was lost on the way: every page reads as its last commit
    // left it, from the pool and from the segments alone, with every
    // value.
    db.check_warm();
    let checkpointed = wb.checkpoint().expect("a checkpoint");
    assert!(
        matches!(checkpointed, Checkpointed::Installed { covers, .. } if covers == db.latest()),
        "{checkpointed:?}"
    );
    db.check_cold();
    check_space(&db, &cleaner.space().expect("the space"));
    run
}

/// The bytes of blocks from which a store is held to the bound: 512 KiB.
/// In a smaller store a file that is being filled can hold a large part
/// of the store before it is long enough to be closed
/// ([`CleanerConfig::min_segment_len`], 64 KiB).
const FROM: u64 = 512 << 10;

/// **The acceptance test.** 1,200 blocks of 20 transitions, with the
/// cleaner as a store is opened with it: after every block, from the one
/// at which the store holds 512 KiB of blocks, at most 20 bytes in a
/// hundred of the bytes of all blocks are dead, and at most 20 in a
/// hundred of the bytes of all values.
#[test]
fn dead_space_stays_at_or_below_20_percent_under_the_reference_workload() {
    let run = run(1200, FROM, Some(CleanerConfig::new()), 100);
    assert!(run.blocks_measured > 400, "{run:?}");
    assert!(run.block_bytes > 2 * FROM, "{run:?}");
    assert!(run.most_dead <= 20.0, "{run:?}");
    assert!(run.most_dead_values <= 20.0, "{run:?}");
    // The workload is of the shape it is meant to have.
    assert!(run.carried > 0.5 && run.carried < 1.5, "{run:?}");
}

/// The same workload with no cleaner: what the bound is kept against.
/// Most of what write-back wrote is dead, and stays.
#[test]
fn without_the_cleaner_most_of_the_store_is_dead() {
    let run = run(400, 0, None, 100);
    assert!(run.dead_at_end > 50.0, "{run:?}");
    assert!(run.dead_values_at_end > 30.0, "{run:?}");
    assert_eq!(run.cleaned_per_transition, 0.0);
}

/// The acceptance test at ten times the length, for the report: 12,000
/// blocks, 240,000 transitions. It is run by hand, in a release build:
/// `cargo test --release -p lignin-store --lib -- --ignored --nocapture
/// cleaner::tests::acceptance::the_bound_holds_over_240_000_transitions`.
#[test]
#[ignore = "minutes in a release build; run by hand for the report"]
fn the_bound_holds_over_240_000_transitions() {
    let run = run(12_000, FROM, Some(CleanerConfig::new()), 1000);
    assert!(run.most_dead <= 20.0, "{run:?}");
    assert!(run.most_dead_values <= 20.0, "{run:?}");
}

/// What the target costs, for whoever tunes the cleaner (PERF-04): the
/// same blocks under four targets, with the files of a store this small
/// (a file is 64 KiB at least, a twentieth of it) and with files that are
/// to it what the files of a large store are (8 KiB at least, so that the
/// share of the store a file may hold is what closes it). It prints, and
/// asserts nothing.
#[test]
#[ignore = "a measurement; run by hand"]
fn what_the_target_costs() {
    for (target, min_segment_len) in [10, 12, 14, 16]
        .into_iter()
        .flat_map(|target| [(target, 64 << 10), (target, 8 << 10)])
    {
        let config = CleanerConfig {
            target_dead_percent: target,
            value_dead_percent: target,
            min_segment_len,
            ..CleanerConfig::new()
        };
        let run = run(1200, FROM, Some(config), u64::MAX);
        println!(
            "TARGET {target}, files of {} KiB at least: most dead {:.1}% of blocks and {:.1}% \
             of values; the cleaner wrote {:.0} bytes a transition, write-back {:.0}",
            min_segment_len >> 10,
            run.most_dead,
            run.most_dead_values,
            run.cleaned_per_transition,
            run.flushed_per_transition
        );
    }
}
