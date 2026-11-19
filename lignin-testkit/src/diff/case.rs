//! One case of the fuzzer: a random operation stream for one grove version.
//!
//! A case is a function of its seed. It takes its operations from the
//! workload generators, so it holds whatever they produce: every element
//! and tree type, batches, deletes, transactions, savepoints and rollbacks
//! today, and an operation a later workload adds from the day it is in
//! [`Workload::suite`]. What a case adds is the randomness around them:
//!
//! - **The grove version**, one of [`Shape::grove_versions`], and with it
//!   the features the workloads are generated for.
//! - **The workloads**, up to [`Shape::max_streams`] of the suite, each with
//!   a seed of its own and cut after [`Shape::max_ops_per_stream`]
//!   operations.
//! - **The arrangement.** The streams follow one another, or their blocks
//!   are interleaved: each workload writes under root keys of its own, so
//!   the blocks of several can be dealt into one database in any order.
//! - **Mutations.** Some cases are damaged on purpose: an operation is
//!   dropped, repeated later, swapped with its neighbour, a step on the
//!   transaction is put where no generator would put one, a read is moved
//!   into the block or the snapshot, a batch loses an operation or gets one
//!   twice. Such a stream asks for things that fail, and an engine has to
//!   fail them the same way: with the same error, the same cost and the
//!   same state afterwards.
//!
//! A generator steers around what its grove version refuses, so a stream
//! as generated applies without a failed operation. A mutated one does not.

use std::fmt;

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use crate::workload::{
    BatchOp, Features, Op, Path, Rng, StreamDigest, TransactionOp, Workload, Write,
};

/// The deepest subtree a case writes into or creates, as the length of its
/// path. LigninDB refuses a Merk with more than 65,535 levels above it and
/// RocksDB does not, so a stream that nested deeper would differ by design.
pub const MAX_DEPTH: usize = 65_535;

const CASE: u64 = 0x6361_7365;
const STREAM: u64 = 0x7374_7265_616d;

/// How cases are generated.
#[derive(Clone, Debug)]
pub struct Shape {
    /// The grove versions a case is generated for.
    pub grove_versions: Vec<&'static GroveVersion>,
    /// The most workloads one case takes its operations from.
    pub max_streams: u64,
    /// The most operations taken from one workload.
    pub max_ops_per_stream: usize,
    /// Out of this many cases, [`Shape::mutated`] are mutated.
    pub mutated_out_of: u64,
    /// See [`Shape::mutated_out_of`].
    pub mutated: u64,
    /// The most mutations of one case.
    pub max_mutations: u64,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            grove_versions: GROVE_VERSIONS.iter().collect(),
            max_streams: 3,
            max_ops_per_stream: 1500,
            mutated_out_of: 2,
            mutated: 1,
            max_mutations: 6,
        }
    }
}

impl Shape {
    /// The shape that generates for one grove version only, by its number.
    pub fn for_grove_version(mut self, number: u32) -> Result<Self, String> {
        self.grove_versions
            .retain(|grove_version| grove_version.protocol_version == number);
        if self.grove_versions.is_empty() {
            return Err(format!("there is no grove version {number}"));
        }
        Ok(self)
    }
}

/// How the streams of a case are put together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrangement {
    /// One stream after another.
    Sequential,
    /// The blocks of the streams dealt into one sequence, each stream's in
    /// their own order.
    Interleaved,
}

/// One stream a case takes operations from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The name of the workload.
    pub workload: &'static str,
    /// The seed its stream was generated from.
    pub seed: u64,
    /// How many operations were taken.
    pub ops: usize,
}

/// A change made to a case to damage it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// The operation at this position was removed.
    Dropped(usize),
    /// The operation at the first position was put a second time before
    /// the one at the second.
    Repeated(usize, usize),
    /// The operations at this position and the next changed places.
    Swapped(usize),
    /// This step was put before the operation at the position.
    Stepped(usize, &'static str),
    /// The read at this position was moved: from committed state into the
    /// block or the snapshot, or back.
    Retargeted(usize, &'static str),
    /// The batch at this position was edited.
    Batch(usize, &'static str),
}

impl fmt::Display for Mutation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mutation::Dropped(at) => write!(f, "operation {at} dropped"),
            Mutation::Repeated(at, to) => write!(f, "operation {at} repeated before {to}"),
            Mutation::Swapped(at) => write!(f, "operations {at} and {} swapped", at + 1),
            Mutation::Stepped(at, step) => write!(f, "{step} put before operation {at}"),
            Mutation::Retargeted(at, to) => write!(f, "the read at {at} moved to {to}"),
            Mutation::Batch(at, edit) => write!(f, "the batch at {at}: {edit}"),
        }
    }
}

/// What a seed decides before any stream is generated.
struct Plan {
    grove_version: &'static GroveVersion,
    /// The workloads, each with the seed of its stream.
    workloads: Vec<(Workload, u64)>,
    arrangement: Arrangement,
    /// How many mutations are tried. One that finds nothing to work on
    /// changes nothing.
    mutations: u64,
    /// What deals the blocks and makes the mutations.
    rng: Rng,
}

impl Plan {
    fn draw(seed: u64, shape: &Shape) -> Plan {
        let mut rng = Rng::derive(seed, CASE, 0);
        let grove_version = shape.grove_versions[rng.index(shape.grove_versions.len())];
        let mut suite = Workload::suite(Features::for_grove_version(grove_version));
        rng.shuffle(&mut suite);
        suite.truncate(rng.between(1, shape.max_streams.max(1)) as usize);
        let workloads: Vec<(Workload, u64)> = suite
            .into_iter()
            .enumerate()
            .map(|(index, workload)| {
                let stream_seed = Rng::derive(seed, STREAM, index as u64).next_u64();
                (workload, stream_seed)
            })
            .collect();
        let arrangement = if workloads.len() > 1 && rng.chance(1, 2) {
            Arrangement::Interleaved
        } else {
            Arrangement::Sequential
        };
        let mutations = if rng.chance(shape.mutated, shape.mutated_out_of.max(1)) {
            rng.between(1, shape.max_mutations.max(1))
        } else {
            0
        };
        Plan {
            grove_version,
            workloads,
            arrangement,
            mutations,
            rng,
        }
    }
}

/// A random operation stream for one grove version.
#[derive(Clone, Debug)]
pub struct Case {
    /// The seed the case is a function of.
    pub seed: u64,
    /// The grove version every operation is applied with.
    pub grove_version: &'static GroveVersion,
    /// The operations.
    pub ops: Vec<Op>,
    /// The streams the operations were taken from.
    pub sources: Vec<Source>,
    /// How the streams were put together.
    pub arrangement: Arrangement,
    /// What was done to damage the stream, in order.
    pub mutations: Vec<Mutation>,
}

impl Case {
    /// The case of `seed`.
    pub fn generate(seed: u64, shape: &Shape) -> Case {
        let Plan {
            grove_version,
            workloads,
            arrangement,
            mutations: tries,
            mut rng,
        } = Plan::draw(seed, shape);
        let mut sources = Vec::with_capacity(workloads.len());
        let mut streams = Vec::with_capacity(workloads.len());
        for (workload, stream_seed) in &workloads {
            let ops: Vec<Op> = workload
                .stream(*stream_seed)
                .take(shape.max_ops_per_stream)
                .collect();
            sources.push(Source {
                workload: workload.name(),
                seed: *stream_seed,
                ops: ops.len(),
            });
            streams.push(ops);
        }
        let mut ops = match arrangement {
            Arrangement::Sequential => streams.into_iter().flatten().collect(),
            Arrangement::Interleaved => interleave(streams, &mut rng),
        };
        ops.retain(|op| depth(op) <= MAX_DEPTH);
        let mut mutations = Vec::new();
        for _ in 0..tries {
            mutations.extend(mutate(&mut ops, &mut rng));
        }
        Case {
            seed,
            grove_version,
            ops,
            sources,
            arrangement,
            mutations,
        }
    }

    /// A case of operations written out, for a stream that was shrunk or is
    /// replayed.
    pub fn of(seed: u64, grove_version: &'static GroveVersion, ops: Vec<Op>) -> Case {
        Case {
            seed,
            grove_version,
            ops,
            sources: Vec::new(),
            arrangement: Arrangement::Sequential,
            mutations: Vec::new(),
        }
    }

    /// This case with other operations: what a shrinking step tries.
    pub fn with_ops(&self, ops: Vec<Op>) -> Case {
        Case {
            seed: self.seed,
            grove_version: self.grove_version,
            ops,
            sources: self.sources.clone(),
            arrangement: self.arrangement,
            mutations: self.mutations.clone(),
        }
    }

    /// The digest of the operations: two cases with one digest are one
    /// stream.
    pub fn digest(&self) -> String {
        StreamDigest::of(&self.ops).hex()
    }

    /// The deepest subtree the case writes into or creates.
    pub fn depth(&self) -> usize {
        self.ops.iter().map(depth).max().unwrap_or(0)
    }

    /// How the case was put together, one line for each fact.
    pub fn recipe(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "seed {:#x}, grove version {}, {} operations, stream {}",
            self.seed,
            self.grove_version.protocol_version,
            self.ops.len(),
            self.digest()
        )];
        for source in &self.sources {
            lines.push(format!(
                "workload {} with seed {:#x}: {} operations",
                source.workload, source.seed, source.ops
            ));
        }
        if self.sources.len() > 1 {
            lines.push(
                match self.arrangement {
                    Arrangement::Sequential => "the streams follow one another",
                    Arrangement::Interleaved => "the blocks of the streams are interleaved",
                }
                .to_string(),
            );
        }
        lines.extend(self.mutations.iter().map(Mutation::to_string));
        lines
    }
}

/// The length of the path of the deepest subtree `op` writes into or could
/// create, or reads from: a write at a path can put a tree there, one level
/// further down.
///
/// For a read it is an upper bound and not the length itself. A read names
/// its paths in a query of GroveDB's whole grammar, and its encoding gives
/// every segment of every path at least the four bytes of a length. So a
/// read has at most a quarter as many segments as its encoding has bytes,
/// in all its paths together.
fn depth(op: &Op) -> usize {
    let below = |path: &Path| path.len() + 1;
    match op {
        Op::Write(write) => write_path(write).map_or(0, below),
        Op::Batch(_)
        | Op::PartialBatch(_)
        | Op::FlagsBatch(_)
        | Op::FlagsPartialBatch(_)
        | Op::FlagsHandoverBatch(_) => op.batch_ops().map(|op| below(op.path())).max().unwrap_or(0),
        Op::Read(_)
        | Op::Transaction(TransactionOp::SnapshotRead(_) | TransactionOp::BlockRead(_)) => {
            let mut encoded = Vec::new();
            op.encode(&mut encoded);
            encoded.len() / 4
        }
        Op::Commit
        | Op::FlushPendingDrops
        | Op::Transaction(
            TransactionOp::Rollback
            | TransactionOp::Abandon
            | TransactionOp::SetSavepoint
            | TransactionOp::RollbackToSavepoint
            | TransactionOp::OpenSnapshot
            | TransactionOp::CloseSnapshot,
        ) => 0,
    }
}

/// The subtree a direct write works in. None for a record of the auxiliary
/// keyspace, which is in no tree.
fn write_path(write: &Write) -> Option<&Path> {
    match write {
        Write::Insert { path, .. }
        | Write::Delete { path, .. }
        | Write::ClearSubtree { path }
        | Write::Append { path, .. }
        | Write::DropFlat { path, .. }
        | Write::InsertIfNotExists { path, .. }
        | Write::InsertIfChanged { path, .. }
        | Write::DeleteIfEmptyTree { path, .. }
        | Write::DeleteUpTree { path, .. }
        | Write::DeleteBuilt { path, .. }
        | Write::IndexedInsert { path, .. }
        | Write::IndexedDelete { path, .. }
        | Write::ReconcileIndexed { path }
        | Write::CommitmentInsert { path, .. }
        | Write::SectionalDelete { path, .. }
        | Write::SectionalDeleteUpTree { path, .. } => Some(path),
        Write::PutAux { .. } | Write::DeleteAux { .. } => None,
    }
}

/// Deals the blocks of `streams` into one sequence. A block ends with the
/// operation that ends its transaction, so a stream commits, rolls back or
/// drops what it wrote itself. Only what follows the last such operation of
/// a stream is left to the transaction of whichever block is dealt next.
fn interleave(streams: Vec<Vec<Op>>, rng: &mut Rng) -> Vec<Op> {
    let mut blocks: Vec<std::collections::VecDeque<Vec<Op>>> = streams
        .into_iter()
        .map(|ops| {
            let mut blocks = std::collections::VecDeque::new();
            let mut block = Vec::new();
            for op in ops {
                let ends = op.ends_transaction();
                block.push(op);
                if ends {
                    blocks.push_back(std::mem::take(&mut block));
                }
            }
            if !block.is_empty() {
                blocks.push_back(block);
            }
            blocks
        })
        .collect();
    let mut ops = Vec::new();
    loop {
        blocks.retain(|blocks| !blocks.is_empty());
        if blocks.is_empty() {
            return ops;
        }
        let left: usize = blocks.iter().map(|blocks| blocks.len()).sum();
        let mut pick = rng.index(left);
        for stream in &mut blocks {
            if pick < stream.len() {
                ops.extend(stream.pop_front().expect("the stream has a block left"));
                break;
            }
            pick -= stream.len();
        }
    }
}

/// A step by its name in a report, and how it is made.
type Step = (&'static str, fn() -> Op);

/// The steps a mutation puts where no generator would.
const STEPS: [Step; 8] = [
    ("a commit", || Op::Commit),
    ("a rollback", || Op::Transaction(TransactionOp::Rollback)),
    ("an abandoned transaction", || {
        Op::Transaction(TransactionOp::Abandon)
    }),
    ("a savepoint", || {
        Op::Transaction(TransactionOp::SetSavepoint)
    }),
    ("a rollback to a savepoint", || {
        Op::Transaction(TransactionOp::RollbackToSavepoint)
    }),
    ("an opened snapshot", || {
        Op::Transaction(TransactionOp::OpenSnapshot)
    }),
    ("a closed snapshot", || {
        Op::Transaction(TransactionOp::CloseSnapshot)
    }),
    ("a flush of pending drops", || Op::FlushPendingDrops),
];

/// Makes one random change to `ops`. None when the change it drew has
/// nothing to work on.
fn mutate(ops: &mut Vec<Op>, rng: &mut Rng) -> Option<Mutation> {
    if ops.is_empty() {
        return None;
    }
    let at = rng.index(ops.len());
    match rng.below(6) {
        0 => {
            ops.remove(at);
            Some(Mutation::Dropped(at))
        }
        1 => {
            let to = rng.between(at as u64 + 1, ops.len() as u64) as usize;
            ops.insert(to, ops[at].clone());
            Some(Mutation::Repeated(at, to))
        }
        2 => {
            if at + 1 == ops.len() {
                return None;
            }
            ops.swap(at, at + 1);
            Some(Mutation::Swapped(at))
        }
        3 => {
            let (name, step) = STEPS[rng.index(STEPS.len())];
            ops.insert(at, step());
            Some(Mutation::Stepped(at, name))
        }
        4 => {
            let (at, read, committed) = from(at, ops.len()).find_map(|at| {
                read_of(&ops[at]).map(|(read, committed)| (at, read.clone(), committed))
            })?;
            let (op, to) = if !committed {
                (Op::Read(read), "committed state")
            } else if rng.chance(1, 2) {
                (Op::Transaction(TransactionOp::BlockRead(read)), "the block")
            } else {
                (
                    Op::Transaction(TransactionOp::SnapshotRead(read)),
                    "the snapshot",
                )
            };
            ops[at] = op;
            Some(Mutation::Retargeted(at, to))
        }
        _ => {
            let at = from(at, ops.len())
                .find(|at| batch_of(&mut ops[*at]).is_some_and(|batch| !batch.is_empty()))?;
            let batch = batch_of(&mut ops[at])?;
            let op = rng.index(batch.len());
            let edit = match rng.below(3) {
                0 => {
                    batch.remove(op);
                    "an operation dropped"
                }
                1 => {
                    let copy = batch[op].clone();
                    batch.push(copy);
                    "an operation repeated"
                }
                _ => {
                    batch.reverse();
                    "its operations reversed"
                }
            };
            Some(Mutation::Batch(at, edit))
        }
    }
}

/// Every position of a sequence of `len`, starting at `at` and wrapping.
fn from(at: usize, len: usize) -> impl Iterator<Item = usize> {
    (at..len).chain(0..at)
}

/// The read of an operation that is one, and whether it reads committed
/// state.
fn read_of(op: &Op) -> Option<(&crate::workload::Read, bool)> {
    match op {
        Op::Read(read) => Some((read, true)),
        Op::Transaction(TransactionOp::BlockRead(read) | TransactionOp::SnapshotRead(read)) => {
            Some((read, false))
        }
        _ => None,
    }
}

/// The lists of operations a batch is made of: one, or the two segments of
/// a batch applied in two. None for an operation that is no batch.
pub(super) fn batch_segments(op: &mut Op) -> Vec<&mut Vec<BatchOp>> {
    match op {
        Op::Batch(ops) | Op::FlagsBatch(ops) => vec![ops],
        Op::PartialBatch(batch) | Op::FlagsPartialBatch(batch) => {
            vec![&mut batch.ops, &mut batch.add_on]
        }
        Op::FlagsHandoverBatch(batch) => vec![&mut batch.ops],
        Op::Write(_) | Op::Read(_) | Op::Commit | Op::FlushPendingDrops | Op::Transaction(_) => {
            Vec::new()
        }
    }
}

/// The operations of an operation that is a batch: those of its first
/// segment for a batch applied in two.
fn batch_of(op: &mut Op) -> Option<&mut Vec<BatchOp>> {
    batch_segments(op).into_iter().next()
}

#[cfg(test)]
mod tests {
    use grovedb::Element;

    use super::*;
    use crate::workload::{Coverage, Operation, Read};

    fn insert(path: Path) -> Op {
        Op::Write(Write::Insert {
            path,
            key: b"key".to_vec(),
            element: Element::new_item(vec![1]),
        })
    }

    #[test]
    fn a_case_is_a_function_of_its_seed() {
        let shape = Shape::default();
        let case = Case::generate(7, &shape);
        let again = Case::generate(7, &shape);
        assert_eq!(case.ops, again.ops);
        assert_eq!(case.sources, again.sources);
        assert_eq!(case.mutations, again.mutations);
        assert_eq!(case.digest(), again.digest());
        assert!(!case.ops.is_empty());
        let other = Case::generate(8, &shape);
        assert_ne!(case.digest(), other.digest());
    }

    /// The streams of three seeds, pinned: the same seed gives the same case
    /// on another machine and after a dependency changed. The third has
    /// three streams, interleaved, and five mutations.
    #[test]
    fn the_cases_of_three_seeds_are_pinned() {
        let shape = Shape::default();
        let pinned = [
            (
                1u64,
                1u32,
                380usize,
                "aadd48e8087324b04a8badeb1ef7800174f8766f03e12ab8b6d0037dd4c5e059",
            ),
            (
                0x4c49_474e_494e,
                3,
                690,
                "6d398daf70da6f482e2e5aa172c8b84bf512d1100777530e5fb0a72eb3aa13e5",
            ),
            (
                3,
                1,
                902,
                "75941756fee765d52d7fa57c05dd080efab97c6e1b13aca0ca79b189bffa2837",
            ),
        ];
        for (seed, grove_version, ops, digest) in pinned {
            let case = Case::generate(seed, &shape);
            println!("{}", case.recipe().join("\n"));
            assert_eq!(case.grove_version.protocol_version, grove_version);
            assert_eq!(case.ops.len(), ops);
            assert_eq!(case.digest(), digest);
        }
        let mutated = Case::generate(3, &shape);
        assert_eq!(mutated.sources.len(), 3);
        assert_eq!(mutated.arrangement, Arrangement::Interleaved);
        assert_eq!(
            mutated.mutations,
            [
                Mutation::Swapped(233),
                Mutation::Dropped(718),
                Mutation::Retargeted(158, "the snapshot"),
                Mutation::Repeated(644, 656),
                Mutation::Dropped(53),
            ]
        );
    }

    /// What a seed draws, over many seeds: every workload of every grove
    /// version, alone and with others, in both arrangements, mutated and
    /// not.
    #[test]
    fn the_seeds_draw_every_workload_of_every_grove_version() {
        let shape = Shape::default();
        let mut drawn = std::collections::BTreeSet::new();
        let (mut interleaved, mut sequential, mut mutated, mut plain) = (0, 0, 0, 0);
        let mut streams = [0; 4];
        const SEEDS: u64 = 4000;
        for seed in 0..SEEDS {
            let plan = Plan::draw(seed, &shape);
            streams[plan.workloads.len()] += 1;
            for (workload, _) in &plan.workloads {
                drawn.insert((plan.grove_version.protocol_version, workload.name()));
            }
            match plan.arrangement {
                Arrangement::Interleaved => {
                    assert!(plan.workloads.len() > 1);
                    interleaved += 1;
                }
                Arrangement::Sequential => sequential += 1,
            }
            assert!(plan.mutations <= shape.max_mutations);
            if plan.mutations == 0 {
                plain += 1;
            } else {
                mutated += 1;
            }
        }
        let every: std::collections::BTreeSet<(u32, &str)> = GROVE_VERSIONS
            .iter()
            .flat_map(|grove_version| {
                Workload::suite(Features::for_grove_version(grove_version))
                    .into_iter()
                    .map(|workload| (grove_version.protocol_version, workload.name()))
            })
            .collect();
        assert!(every.len() > 150);
        assert_eq!(drawn, every);
        assert_eq!(streams[0], 0);
        for count in &streams[1..] {
            assert!((1100..1600).contains(count), "{streams:?}");
        }
        // A third of the cases has one stream and cannot be interleaved.
        assert!((1100..1600).contains(&interleaved), "{interleaved}");
        assert_eq!(interleaved + sequential, SEEDS);
        assert!((1800..2200).contains(&mutated), "{mutated}");
        assert_eq!(mutated + plain, SEEDS);
    }

    /// The cases of the latest grove version in which each workload of the
    /// suite turns up first hold, between them, everything the generators
    /// produce, and every kind of mutation.
    #[test]
    fn the_cases_hold_everything_the_generators_produce() {
        let shape = Shape::default();
        let latest = GroveVersion::latest().protocol_version;
        let mut missing: std::collections::BTreeSet<&str> =
            Workload::suite(Features::for_grove_version(GroveVersion::latest()))
                .iter()
                .map(Workload::name)
                .collect();
        let mut ops = Vec::new();
        let mut mutations = std::collections::HashSet::new();
        let mut cases = 0;
        for seed in 0.. {
            if missing.is_empty() {
                break;
            }
            let plan = Plan::draw(seed, &shape);
            let names: Vec<&str> = plan.workloads.iter().map(|(w, _)| w.name()).collect();
            if plan.grove_version.protocol_version != latest
                || !names.iter().any(|name| missing.contains(name))
            {
                continue;
            }
            let case = Case::generate(seed, &shape);
            assert_eq!(case.grove_version.protocol_version, latest);
            assert_eq!(
                case.sources.iter().map(|s| s.workload).collect::<Vec<_>>(),
                names
            );
            assert_eq!(case.arrangement, plan.arrangement);
            assert!(case.mutations.len() as u64 <= plan.mutations);
            assert!(case.depth() <= MAX_DEPTH);
            for name in names {
                missing.remove(name);
            }
            for mutation in &case.mutations {
                mutations.insert(std::mem::discriminant(mutation));
            }
            ops.extend(case.ops);
            cases += 1;
        }
        assert!(cases <= 31, "{cases} cases");
        assert_eq!(mutations.len(), 6, "every kind of mutation was made");
        let coverage = Coverage::of(&ops);
        assert_eq!(coverage.missing_elements(), Vec::<&str>::new());
        assert_eq!(coverage.missing_tree_types(), Vec::<&str>::new());
        assert_eq!(coverage.missing_operations(), Vec::<Operation>::new());
    }

    /// How deep the generators go, at their default sizes, on every grove
    /// version: the bound cuts nothing out of what they produce today, and
    /// a read that were dropped for its size would be noticed here.
    #[test]
    fn the_generators_stay_far_below_the_deepest_subtree_allowed() {
        let (mut deepest_write, mut longest_read) = (0, 0);
        for grove_version in GROVE_VERSIONS.iter() {
            for workload in Workload::suite(Features::for_grove_version(grove_version)) {
                for op in workload.stream(1) {
                    let reads = matches!(
                        op,
                        Op::Read(_)
                            | Op::Transaction(
                                TransactionOp::SnapshotRead(_) | TransactionOp::BlockRead(_)
                            )
                    );
                    if reads {
                        longest_read = longest_read.max(depth(&op));
                    } else {
                        deepest_write = deepest_write.max(depth(&op));
                    }
                }
            }
        }
        println!("deepest write {deepest_write}, longest read {longest_read}");
        assert!((8..200).contains(&deepest_write), "{deepest_write}");
        assert!(longest_read < MAX_DEPTH / 8, "{longest_read}");
    }

    #[test]
    fn a_shape_can_name_one_grove_version() {
        let shape = Shape::default().for_grove_version(2).expect("version 2");
        for seed in 0..8 {
            assert_eq!(
                Case::generate(seed, &shape).grove_version.protocol_version,
                2
            );
        }
        let refused = Shape::default().for_grove_version(99).unwrap_err();
        assert_eq!(refused, "there is no grove version 99");
    }

    #[test]
    fn interleaving_keeps_each_stream_in_order_and_its_blocks_whole() {
        // Three blocks of two writes and a commit.
        let stream = |tag: u8| -> Vec<Op> {
            (0..9u8)
                .map(|index| {
                    if index % 3 == 2 {
                        Op::Commit
                    } else {
                        insert(vec![vec![tag], vec![index]])
                    }
                })
                .collect()
        };
        let streams = vec![stream(1), stream(2), stream(3)];
        let mut orders = std::collections::BTreeSet::new();
        for seed in 0..20 {
            let dealt = interleave(streams.clone(), &mut Rng::new(seed));
            assert_eq!(dealt.len(), 27);
            for (tag, stream) in [1u8, 2, 3].into_iter().zip(&streams) {
                let own: Vec<&Op> = dealt
                    .iter()
                    .filter(|op| match op {
                        Op::Write(Write::Insert { path, .. }) => path[0] == [tag],
                        _ => false,
                    })
                    .collect();
                let writes: Vec<&Op> = stream.iter().filter(|op| **op != Op::Commit).collect();
                assert_eq!(own, writes);
            }
            for block in dealt.chunks(3) {
                let [Op::Write(Write::Insert { path: first, .. }), Op::Write(Write::Insert { path: second, .. }), Op::Commit] =
                    block
                else {
                    panic!("{block:?} is no block")
                };
                assert_eq!(first[0], second[0]);
            }
            orders.insert(format!("{dealt:?}"));
        }
        assert!(orders.len() > 10, "the deals differ");

        // What follows the last commit of a stream is a block too, and a
        // stream alone is dealt as it is.
        let open = vec![insert(vec![vec![1]]), Op::Commit, insert(vec![vec![2]])];
        assert_eq!(interleave(vec![open.clone()], &mut Rng::new(0)), open);
        let two = interleave(
            vec![open.clone(), vec![Op::FlushPendingDrops]],
            &mut Rng::new(0),
        );
        assert_eq!(two.len(), 4);
        let own: Vec<&Op> = two
            .iter()
            .filter(|op| **op != Op::FlushPendingDrops)
            .collect();
        assert_eq!(own, open.iter().collect::<Vec<_>>());
        assert_eq!(interleave(Vec::new(), &mut Rng::new(0)), Vec::new());
    }

    #[test]
    fn a_stream_nested_too_deep_is_cut_out() {
        assert_eq!(depth(&insert(vec![vec![0]; 3])), 4);
        assert_eq!(depth(&insert(vec![vec![0]; MAX_DEPTH - 1])), MAX_DEPTH);
        assert!(depth(&insert(vec![vec![0]; MAX_DEPTH])) > MAX_DEPTH);
        let batch = Op::Batch(vec![BatchOp::Delete {
            path: vec![vec![0]; MAX_DEPTH],
            key: Vec::new(),
        }]);
        assert!(depth(&batch) > MAX_DEPTH);
        assert_eq!(depth(&Op::Batch(Vec::new())), 0);
        assert_eq!(depth(&Op::Commit), 0);
        assert_eq!(depth(&Op::Transaction(TransactionOp::Rollback)), 0);
        // A read is bounded by its encoding, whatever names its paths.
        let read = |segments: usize| Read::HasRaw {
            path: vec![vec![0]; segments],
            key: Vec::new(),
        };
        for shallow in [
            Op::Read(read(3)),
            Op::Transaction(TransactionOp::BlockRead(read(3))),
            Op::Transaction(TransactionOp::SnapshotRead(read(3))),
        ] {
            assert!((3..12).contains(&depth(&shallow)), "{}", depth(&shallow));
        }
        assert!(depth(&Op::Read(read(MAX_DEPTH))) > MAX_DEPTH);
        let aux = Op::Write(Write::PutAux {
            key: vec![1],
            value: vec![2],
        });
        assert_eq!(depth(&aux), 0);
        let case = Case::of(0, GroveVersion::latest(), vec![insert(Vec::new()), aux]);
        assert_eq!(case.depth(), 1);
        assert_eq!(Case::of(0, GroveVersion::latest(), Vec::new()).depth(), 0);
    }

    #[test]
    fn every_mutation_does_what_it_says() {
        let read = Read::GetAux { key: vec![7] };
        let base = vec![
            insert(vec![vec![1]]),
            Op::Batch(vec![
                BatchOp::Delete {
                    path: Vec::new(),
                    key: vec![1],
                },
                BatchOp::Delete {
                    path: Vec::new(),
                    key: vec![2],
                },
            ]),
            Op::Read(read.clone()),
            Op::Commit,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..400 {
            let mut ops = base.clone();
            let Some(mutation) = mutate(&mut ops, &mut Rng::new(seed)) else {
                assert_eq!(ops, base, "a mutation that did nothing changed nothing");
                continue;
            };
            assert_ne!(ops, base, "{mutation}");
            seen.insert(mutation.to_string());
            match mutation {
                Mutation::Dropped(at) => {
                    let mut expected = base.clone();
                    expected.remove(at);
                    assert_eq!(ops, expected);
                }
                Mutation::Repeated(at, to) => {
                    assert_eq!(ops.len(), 5);
                    assert!(to > at);
                    assert_eq!(ops[to], base[at]);
                }
                Mutation::Swapped(at) => {
                    assert_eq!(ops[at], base[at + 1]);
                    assert_eq!(ops[at + 1], base[at]);
                }
                Mutation::Stepped(at, _) => {
                    assert_eq!(ops.len(), 5);
                    let mut without = ops.clone();
                    without.remove(at);
                    assert_eq!(without, base);
                }
                Mutation::Retargeted(at, to) => {
                    assert_eq!(at, 2);
                    let expected = match to {
                        "the block" => TransactionOp::BlockRead(read.clone()),
                        "the snapshot" => TransactionOp::SnapshotRead(read.clone()),
                        other => panic!("{other}"),
                    };
                    assert_eq!(ops[at], Op::Transaction(expected));
                    // And back again.
                    let mut rng = Rng::new(0);
                    let back = (0..200).find_map(|_| {
                        let mut again = ops.clone();
                        match mutate(&mut again, &mut rng) {
                            Some(Mutation::Retargeted(_, "committed state")) => Some(again),
                            _ => None,
                        }
                    });
                    assert_eq!(back.expect("a read is moved back"), base);
                }
                Mutation::Batch(at, edit) => {
                    assert_eq!(at, 1);
                    let Op::Batch(batch) = &ops[at] else {
                        panic!("the batch stays a batch")
                    };
                    match edit {
                        "an operation dropped" => assert_eq!(batch.len(), 1),
                        "an operation repeated" => assert_eq!(batch.len(), 3),
                        "its operations reversed" => assert_eq!(batch.len(), 2),
                        other => panic!("{other}"),
                    }
                }
            }
        }
        assert!(seen.len() > 20, "{seen:?}");
        assert_eq!(mutate(&mut Vec::new(), &mut Rng::new(0)), None);
    }

    #[test]
    fn a_mutation_that_finds_nothing_to_work_on_is_none() {
        // No read and no batch to edit.
        let base = vec![Op::Commit, Op::Batch(Vec::new())];
        for seed in 0..100 {
            let mut ops = base.clone();
            match mutate(&mut ops, &mut Rng::new(seed)) {
                Some(Mutation::Retargeted(..) | Mutation::Batch(..)) => {
                    panic!("there is no read and no operation of a batch")
                }
                Some(_) => {}
                None => assert_eq!(ops, base),
            }
        }
    }

    #[test]
    fn the_segments_of_every_kind_of_batch_are_found() {
        use crate::workload::{HandoverBatch, PartialBatch};
        let delete = || BatchOp::Delete {
            path: Vec::new(),
            key: vec![1],
        };
        let partial = || PartialBatch {
            ops: vec![delete(), delete()],
            add_on: vec![delete()],
            pause_height: None,
        };
        let lens = |mut op: Op| -> (Vec<usize>, Option<usize>) {
            let first = batch_of(&mut op).map(|ops| ops.len());
            let segments = batch_segments(&mut op);
            (segments.iter().map(|ops| ops.len()).collect(), first)
        };
        assert_eq!(lens(Op::Batch(vec![delete()])), (vec![1], Some(1)));
        assert_eq!(
            lens(Op::FlagsBatch(vec![delete(), delete()])),
            (vec![2], Some(2))
        );
        assert_eq!(lens(Op::PartialBatch(partial())), (vec![2, 1], Some(2)));
        assert_eq!(
            lens(Op::FlagsPartialBatch(partial())),
            (vec![2, 1], Some(2))
        );
        let handover = HandoverBatch {
            ops: vec![delete()],
            full_handover_owners: Default::default(),
        };
        assert_eq!(lens(Op::FlagsHandoverBatch(handover)), (vec![1], Some(1)));
        for other in [
            Op::Commit,
            Op::FlushPendingDrops,
            insert(Vec::new()),
            Op::Read(Read::GetAux { key: Vec::new() }),
            Op::Transaction(TransactionOp::Rollback),
        ] {
            assert_eq!(lens(other), (Vec::new(), None));
        }
    }

    #[test]
    fn a_recipe_says_how_the_case_was_made() {
        let shape = Shape::default();
        let case = (0..)
            .map(|seed| Case::generate(seed, &shape))
            .find(|case| case.sources.len() > 1 && !case.mutations.is_empty())
            .expect("a case of several streams that is mutated");
        let recipe = case.recipe();
        assert!(recipe[0].starts_with(&format!("seed {:#x}, grove version ", case.seed)));
        assert_eq!(
            recipe.len(),
            1 + case.sources.len() + 1 + case.mutations.len()
        );
        assert!(recipe[1].starts_with("workload "));
        let written = Case::of(3, GroveVersion::latest(), vec![Op::Commit]);
        assert_eq!(written.recipe().len(), 1);
        assert_eq!(written.with_ops(Vec::new()).ops, Vec::new());
        for (mutation, text) in [
            (Mutation::Dropped(3), "operation 3 dropped"),
            (Mutation::Repeated(3, 9), "operation 3 repeated before 9"),
            (Mutation::Swapped(3), "operations 3 and 4 swapped"),
            (
                Mutation::Stepped(3, "a commit"),
                "a commit put before operation 3",
            ),
            (
                Mutation::Retargeted(3, "the block"),
                "the read at 3 moved to the block",
            ),
            (
                Mutation::Batch(3, "an operation dropped"),
                "the batch at 3: an operation dropped",
            ),
        ] {
            assert_eq!(mutation.to_string(), text);
        }
    }
}
