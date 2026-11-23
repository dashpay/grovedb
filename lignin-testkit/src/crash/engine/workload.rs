//! What a lineage does to its database: the operations of GroveDB, from
//! the workload generators, and between them the steps of the campaign.
//!
//! **The streams** are those of [`crate::workload`]: every element and tree
//! type, batches, deletes, transactions that are committed, rolled back and
//! abandoned, savepoints, and reads through the open block and through a
//! snapshot. A run has a grove version and a list of streams, both by its
//! seed. A stream is applied to an empty grove: a lineage that comes to the
//! end of a stream, and one whose database was wiped, goes on with the next
//! one.
//!
//! A stream leaves out an operation that GroveDB commits in a
//! transaction of its own ([`Plan::stream`]), and a run on one of the
//! first three grove versions takes no stream that appends to a tree
//! ([`left_out`]).
//!
//! **The steps** are what a node does to a database beside the blocks it
//! applies ([`Step`]). A lineage takes them from a list, or draws them:
//! [`Steps::next`] draws the step that comes next from a generator that
//! the seed gives, by the situation of the lineage ([`Situation`]), with
//! nothing that depends on a thread. The situation follows from what the
//! database answered, and a draw can take more from the generator in one
//! situation than in another. So the steps that are drawn for a lineage
//! follow from the seed and the shape of the run alone where nothing else
//! decides an answer: in the first lineage of a run that injects no fault
//! and finds no violation.

use std::sync::Arc;

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::model::Position;
use crate::{
    crash::campaign::Faults,
    workload::{BatchOp, Features, Op, Rng, Workload, Write},
};

const PLAN: u64 = 0x7665_7230_3420_706c;
const STREAM: u64 = 0x7665_7230_3420_7374;
const STEPS: u64 = 0x7665_7230_3420_7370;

/// What the streams of the campaign are generated for: everything the
/// grove version has, a move in a batch and the conversion of a tree in
/// place among it. Of the conversions a stream keeps those that are made
/// in the transaction of a block ([`Plan::stream`]).
pub fn features(grove_version: &GroveVersion) -> Features {
    Features::for_grove_version(grove_version)
}

/// Whether the stream of `ops` is left out of a run on `grove_version`:
/// on grove versions 1 to 3, a stream that appends to a tree is.
///
/// The engine has a known defect there (D-282, D-283, to be fixed by
/// `TXN-03-F5`): under a tree that keeps its data flat (MMR, bulk-append,
/// dense, commitment) and has a keyed child, a typed append is refused
/// that RocksDB takes, and an append followed by a keyed child leaves a
/// store that the physical checker finds unsound. Both need an append, so
/// a stream with none meets neither, and the campaign would report the
/// second in every state of a database that has it.
pub fn left_out(grove_version: &GroveVersion, ops: &[Op]) -> bool {
    let appends = |op: &Op| match op {
        Op::Write(Write::Append { .. } | Write::CommitmentInsert { .. }) => true,
        other => (other.batch_ops()).any(|op| matches!(op, BatchOp::Append { .. })),
    };
    grove_version.protocol_version <= 3 && ops.iter().any(appends)
}

/// One stream of a plan.
#[derive(Clone, Debug)]
pub struct Stream {
    /// The name of its workload.
    pub workload: &'static str,
    /// The seed it was generated from.
    pub seed: u64,
    /// Its operations.
    pub ops: Arc<Vec<Op>>,
}

/// What a run applies: a grove version, and streams without end.
#[derive(Clone, Debug)]
pub struct Plan {
    seed: u64,
    /// The grove version every operation is applied with.
    pub grove_version: &'static GroveVersion,
    /// The most operations taken of one stream.
    pub max_stream_ops: usize,
    order: Vec<usize>,
    /// How many streams were generated, those that were left out among
    /// them.
    tried: usize,
    streams: Vec<Stream>,
}

impl Plan {
    /// The plan of the run of `seed`.
    pub fn of_seed(seed: u64, max_stream_ops: usize) -> Self {
        let mut rng = Rng::derive(seed, PLAN, 0);
        let grove_version = &GROVE_VERSIONS[rng.index(GROVE_VERSIONS.len())];
        let mut order: Vec<usize> = (0..Workload::suite(features(grove_version)).len()).collect();
        rng.shuffle(&mut order);
        Self {
            seed,
            grove_version,
            max_stream_ops,
            order,
            tried: 0,
            streams: Vec::new(),
        }
    }

    /// The stream `index` of the plan. The workloads of the suite come in
    /// an order drawn from the seed, again and again, each time with
    /// another seed of its own, without the streams that are left out of a
    /// run on the plan's grove version ([`left_out`]).
    pub fn stream(&mut self, index: usize) -> &Stream {
        while self.streams.len() <= index {
            let suite = Workload::suite(features(self.grove_version));
            let workload = &suite[self.order[self.tried % self.order.len()]];
            let seed = Rng::derive(self.seed, STREAM, self.tried as u64).next_u64();
            self.tried += 1;
            // The campaign makes every commit itself: an operation that
            // GroveDB commits in a transaction of its own is left out.
            let ops: Vec<Op> = (workload.stream(seed))
                .filter(|op| !op.commits_by_itself())
                .take(self.max_stream_ops)
                .collect();
            if !left_out(self.grove_version, &ops) {
                self.streams.push(Stream {
                    workload: workload.name(),
                    seed,
                    ops: Arc::new(ops),
                });
            }
        }
        &self.streams[index]
    }

    /// The operation at `position`, or `None` at the end of its stream.
    pub fn op(&mut self, position: Position) -> Option<Op> {
        self.stream(position.stream).ops.get(position.op).cloned()
    }
}

/// A fault that a step plants in the file layer under the database: the
/// next operation of its kind fails, whichever of the four threads of the
/// database makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Planted {
    /// The next sync, of a file or of a directory, fails with `EIO`.
    Sync,
    /// The next write to a file fails with `EIO`: a write, an append or a
    /// change of its length. An append that fails can leave its bytes.
    Write,
    /// The next operation on a name fails with `EIO`: an open, a rename, a
    /// link, a removal, the making or the removal of a directory, a lock.
    Name,
    /// The device is full for the next three writes to a file.
    Full,
}

/// One step of a lineage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The next operation of the stream.
    Stream,
    /// `GroveDb::flush`: every block that is committed is durable when it
    /// returns.
    Flush,
    /// A checkpoint of the database is made (`Db::checkpoint`).
    Checkpoint,
    /// The oldest checkpoint is removed (`lignindb::checkpoint::delete`).
    DeleteCheckpoint,
    /// `GroveDb::wipe`. The lineage goes on with the next stream.
    Wipe,
    /// A write with no transaction, beside the block that is open: it
    /// commits at once, and the block's own commit is refused for a
    /// conflict where the block had written what this write meets.
    Beside,
    /// The workload waits a moment, and the three threads of the database
    /// work.
    Settle,
    /// The database is closed and opened again.
    Close,
    /// The process is killed: the database is not closed, nothing that was
    /// written is lost, and the database is opened again.
    Kill,
    /// A fault is planted in the file layer, in a run that injects faults.
    Fault(Planted),
}

impl Step {
    /// The name the counts know it by.
    pub fn name(self) -> &'static str {
        match self {
            Self::Stream => "operations of the streams",
            Self::Flush => "flushes",
            Self::Checkpoint => "checkpoints asked for",
            Self::DeleteCheckpoint => "checkpoints whose deletion was asked for",
            Self::Wipe => "wipes",
            Self::Beside => "writes with no transaction beside a block",
            Self::Settle => "pauses of the workload",
            Self::Close => "closes",
            Self::Kill => "kills",
            Self::Fault(_) => "faults planted",
        }
    }
}

/// What a draw of a step goes by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Situation {
    /// Whether the open block has written.
    pub dirty: bool,
    /// How many checkpoints of the database there are.
    pub checkpoints: usize,
    /// Whether only operations of the stream are to come: a block that was
    /// refused more than once is made without anything beside it.
    pub calm: bool,
    /// The faults the run injects.
    pub faults: Faults,
}

/// The most checkpoints a lineage keeps.
pub const MAX_CHECKPOINTS: usize = 2;

/// The steps of one lineage, drawn from the seed.
#[derive(Clone, Debug)]
pub struct Steps {
    rng: Rng,
}

impl Steps {
    /// The steps of lineage `lineage` of the run of `seed`.
    pub fn new(seed: u64, lineage: u64) -> Self {
        Self {
            rng: Rng::derive(seed, STEPS, lineage),
        }
    }

    /// The next step. Between two blocks a lineage flushes, closes, is
    /// killed, makes and deletes checkpoints, wipes and pauses; inside a
    /// block it does so more rarely, and what is open then is lost or
    /// refused.
    pub fn next(&mut self, situation: Situation) -> Step {
        let draw = self.rng.below(1000);
        if situation.calm {
            return Step::Stream;
        }
        let made = situation.checkpoints;
        // The fault of a step that plants one is drawn in every run, so
        // that the steps of a seed are the same whatever its faults.
        let fault = match (situation.faults, self.rng.below(4)) {
            (Faults::None, _) => None,
            (Faults::Syncs, _) | (Faults::All, 0) => Some(Planted::Sync),
            (Faults::All, 1) => Some(Planted::Write),
            (Faults::All, 2) => Some(Planted::Name),
            (Faults::All, _) => Some(Planted::Full),
        };
        let table: &[(u64, Step)] = match situation.dirty {
            false => &[
                (45, Step::Flush),
                (30, Step::Checkpoint),
                (15, Step::DeleteCheckpoint),
                (12, Step::Wipe),
                (25, Step::Close),
                (20, Step::Kill),
                (30, Step::Settle),
                (15, Step::Beside),
                (30, Step::Fault(Planted::Sync)),
            ],
            true => &[
                (6, Step::Flush),
                (5, Step::Checkpoint),
                (3, Step::DeleteCheckpoint),
                (4, Step::Wipe),
                (6, Step::Kill),
                (8, Step::Settle),
                (10, Step::Beside),
                (12, Step::Fault(Planted::Sync)),
            ],
        };
        let mut below = 0;
        for (share, step) in table {
            below += share;
            if draw < below {
                return match (step, fault) {
                    (Step::Checkpoint, _) if made >= MAX_CHECKPOINTS => Step::DeleteCheckpoint,
                    (Step::DeleteCheckpoint, _) if made == 0 => Step::Checkpoint,
                    (Step::Fault(_), Some(fault)) => Step::Fault(fault),
                    // A run with no faults takes the stream in the place
                    // of a fault.
                    (Step::Fault(_), None) => Step::Stream,
                    (step, _) => *step,
                };
            }
        }
        Step::Stream
    }

    /// A number below `bound`, for what the campaign decides by chance
    /// beside the steps.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.rng.below(bound)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn a_plan_is_a_function_of_its_seed() {
        let mut plan = Plan::of_seed(7, 40);
        let mut again = Plan::of_seed(7, 40);
        assert_eq!(
            plan.grove_version.protocol_version,
            again.grove_version.protocol_version
        );
        for index in [2, 0, 1] {
            let (one, other) = (plan.stream(index).clone(), again.stream(index).clone());
            assert_eq!((one.workload, one.seed), (other.workload, other.seed));
            assert_eq!(one.ops, other.ops);
            assert!(!one.ops.is_empty() && one.ops.len() <= 40);
        }
        let first = plan.stream(0).clone();
        assert_eq!(
            plan.op(Position { stream: 0, op: 0 }),
            Some(first.ops[0].clone())
        );
        let end = Position {
            stream: 0,
            op: first.ops.len(),
        };
        assert_eq!(plan.op(end), None);
        // The workloads come round again with other seeds.
        let suite = Workload::suite(features(plan.grove_version)).len();
        let round = (1..=suite)
            .map(|index| plan.stream(index).clone())
            .find(|stream| stream.workload == first.workload)
            .expect("the first workload comes again");
        assert_ne!(round.seed, first.seed);
        // Other seeds give other grove versions and other workloads.
        let versions: std::collections::BTreeSet<u32> = (0..40)
            .map(|seed| Plan::of_seed(seed, 4).grove_version.protocol_version)
            .collect();
        assert_eq!(versions.len(), GROVE_VERSIONS.len());
    }

    /// The streams of the grove version that has the conversion of a
    /// tree in place convert trees, in the transaction of a block and
    /// never in one of their own, and those of the grove versions that
    /// have a move in a batch move subtrees, within the operations a
    /// lineage takes of a stream. No stream of another grove version
    /// does either.
    #[test]
    fn streams_convert_trees_and_move_subtrees_where_the_grove_version_has_them() {
        let (mut converted, mut moved) = (0, 0);
        for seed in 0..12 {
            let mut plan = Plan::of_seed(seed, 60);
            let has_moves = features(plan.grove_version).move_element;
            assert_eq!(has_moves, plan.grove_version.protocol_version >= 4);
            let has_conversions = features(plan.grove_version).convert_to_big_sum_tree;
            assert_eq!(has_conversions, plan.grove_version.protocol_version >= 5);
            let suite = Workload::suite(features(plan.grove_version)).len();
            for index in 0..suite {
                for op in plan.stream(index).ops.iter() {
                    let converts = matches!(op, Op::Write(Write::ConvertToBigSumTree { .. }));
                    let moves = (op.batch_ops()).any(|op| matches!(op, BatchOp::Move { .. }));
                    assert!(
                        has_conversions || !converts,
                        "seed {seed}, stream {index}: {op:?}"
                    );
                    converted += usize::from(converts);
                    assert!(has_moves || !moves, "seed {seed}, stream {index}: {op:?}");
                    assert!(
                        !op.commits_by_itself(),
                        "seed {seed}, stream {index}: {op:?}"
                    );
                    moved += usize::from(moves);
                }
            }
        }
        assert!(moved > 0, "no stream of twelve plans moves a subtree");
        assert!(converted > 0, "no stream of twelve plans converts a tree");
    }

    /// On the first three grove versions no stream of a plan appends to a
    /// tree, and on the later ones some do: the trees that keep their data
    /// flat are in the campaign.
    #[test]
    fn a_run_on_an_early_grove_version_takes_no_stream_that_appends() {
        // How many of the first thirty streams of a plan append: each is
        // asked as the rule asks it on a grove version it holds for.
        let appends = |plan: &mut Plan| {
            (0..30)
                .filter(|index| {
                    let ops = plan.stream(*index).ops.clone();
                    left_out(&GROVE_VERSIONS[0], &ops)
                })
                .count()
        };
        let (mut early, mut late) = (0, 0);
        for seed in 0..40 {
            let mut plan = Plan::of_seed(seed, 60);
            let with_appends = appends(&mut plan);
            match plan.grove_version.protocol_version {
                1..=3 => {
                    assert_eq!(with_appends, 0, "seed {seed}");
                    early += 1;
                }
                _ => late += with_appends,
            }
        }
        assert!(early > 0 && late > 0, "{early} early runs, {late} streams");
        assert!(!left_out(&GROVE_VERSIONS[3], &[]));
        assert!(!left_out(&GROVE_VERSIONS[0], &[Op::Commit]));
    }

    #[test]
    fn the_steps_of_a_lineage_are_a_function_of_its_seed_and_hold_every_kind() {
        let quiet = Situation {
            dirty: false,
            checkpoints: 1,
            calm: false,
            faults: Faults::None,
        };
        let drawn = |seed: u64, lineage: u64, situation: Situation| -> Vec<Step> {
            let mut steps = Steps::new(seed, lineage);
            (0..4000).map(|_| steps.next(situation)).collect()
        };
        assert_eq!(drawn(3, 1, quiet), drawn(3, 1, quiet));
        assert_ne!(drawn(3, 1, quiet), drawn(3, 2, quiet));
        let count = |steps: &[Step]| {
            let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
            for step in steps {
                *counts.entry(step.name()).or_default() += 1;
            }
            counts
        };
        let between = count(&drawn(3, 1, quiet));
        assert_eq!(between.len(), 9, "{between:?}");
        assert!(between[Step::Stream.name()] > 3000);
        let dirty = Situation {
            dirty: true,
            ..quiet
        };
        let inside = count(&drawn(3, 1, dirty));
        // Inside a block nothing is closed.
        assert_eq!(inside.len(), 8, "{inside:?}");
        assert!(!inside.contains_key(Step::Close.name()));
        assert!(inside[Step::Stream.name()] > between[Step::Stream.name()]);
        // No checkpoint is deleted where there is none, and none is made
        // beyond the most a lineage keeps.
        let none = count(&drawn(
            3,
            1,
            Situation {
                checkpoints: 0,
                ..quiet
            },
        ));
        assert!(!none.contains_key(Step::DeleteCheckpoint.name()));
        let full = Situation {
            checkpoints: MAX_CHECKPOINTS,
            ..quiet
        };
        assert!(!count(&drawn(3, 1, full)).contains_key(Step::Checkpoint.name()));
        // A calm lineage takes the stream alone.
        let calm = Situation {
            calm: true,
            ..quiet
        };
        assert!(drawn(3, 1, calm).iter().all(|step| *step == Step::Stream));
        assert!(Steps::new(1, 1).below(10) < 10);

        // A run that injects faults plants one where a run with none takes
        // the stream, and every other step is the same: failing syncs
        // alone, or a fault of every kind.
        let with = |faults| drawn(3, 1, Situation { faults, ..quiet });
        let (none, syncs, all) = (with(Faults::None), with(Faults::Syncs), with(Faults::All));
        let mut kinds = std::collections::BTreeSet::new();
        for ((none, syncs), all) in none.iter().zip(&syncs).zip(&all) {
            match all {
                Step::Fault(planted) => {
                    assert_eq!((none, syncs), (&Step::Stream, &Step::Fault(Planted::Sync)));
                    kinds.insert(format!("{planted:?}"));
                }
                other => assert_eq!((none, syncs), (other, other)),
            }
        }
        assert_eq!(kinds.len(), 4, "{kinds:?}");
        assert_eq!(Step::Fault(Planted::Full).name(), "faults planted");
    }
}
