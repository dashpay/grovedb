//! Shrinking a stream two engines answered differently.
//!
//! A case that diverges has hundreds of operations and the divergence
//! needs a few of them. [`shrink`] looks for those few: it runs shorter
//! streams on fresh databases and keeps a shorter one whenever the engines
//! still differ in a part they differed in before.
//!
//! 1. Everything after the diverging step goes.
//! 2. Runs of operations are taken out, first half the stream at a time,
//!    then a quarter, down to single operations, until no single operation
//!    can be taken out.
//! 3. Single operations are taken out of the batches that are left.
//! 4. Steps 2 and 3 repeat until neither shortens the stream.
//!
//! What is left is minimal in that sense: every operation, and every
//! operation of every batch, is needed for the engines to differ. It is
//! not the shortest stream there is. A [`Budget`] bounds the work, and a
//! stream shrunk on a spent budget says so.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use super::{
    case::Case,
    pair::{run, Divergence, Part, Side, Stored},
};
use crate::{
    golden::Error,
    workload::{BatchOp, Op, StreamEngine},
};

/// How much work a shrinking may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// The most streams it runs.
    pub runs: u64,
    /// The longest it takes.
    pub time: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            runs: 4000,
            time: Duration::from_secs(300),
        }
    }
}

/// What shrinking a diverging case came to.
#[derive(Clone, Debug)]
pub struct Shrunk {
    /// The shortest stream found on which the engines still differ.
    pub case: Case,
    /// How they differ on it.
    pub divergence: Divergence,
    /// How many streams were run.
    pub runs: u64,
    /// Whether the divergence showed again when the case was run a second
    /// time. If not, the engines do not answer the same stream the same
    /// way twice, and nothing was shrunk.
    pub reproduced: bool,
    /// Whether no operation can be taken out any more. False when the
    /// budget ran out first.
    pub minimal: bool,
}

struct Shrinker<'a, A: StreamEngine, B: StreamEngine> {
    template: &'a Case,
    target: &'a [Part],
    reference: &'a Side<A>,
    candidate: &'a Side<B>,
    scratch: &'a Path,
    stored: Stored,
    budget: Budget,
    started: Instant,
    runs: u64,
}

impl<A: StreamEngine, B: StreamEngine> Shrinker<'_, A, B> {
    fn spent(&self) -> bool {
        self.runs >= self.budget.runs || self.started.elapsed() >= self.budget.time
    }

    /// Runs `ops` and returns how the engines differ on them, if they
    /// differ in a part they are meant to.
    fn diverges(&mut self, ops: &[Op]) -> Result<Option<Divergence>, Error> {
        self.runs += 1;
        let case = self.template.with_ops(ops.to_vec());
        let run = run(
            &case,
            self.reference,
            self.candidate,
            self.scratch,
            self.stored,
            &|| false,
        )?;
        Ok(run.divergence.filter(|divergence| {
            divergence
                .parts
                .iter()
                .any(|part| self.target.contains(part))
        }))
    }

    /// Tries `shorter` in place of `ops`. If the engines still differ, it
    /// takes its place, cut after the step they differ at.
    fn keep_if_it_diverges(
        &mut self,
        shorter: Vec<Op>,
        ops: &mut Vec<Op>,
        divergence: &mut Divergence,
    ) -> Result<bool, Error> {
        let Some(found) = self.diverges(&shorter)? else {
            return Ok(false);
        };
        *ops = shorter;
        ops.truncate(found.step + 1);
        *divergence = found;
        Ok(true)
    }

    /// Takes runs of operations out, from half the stream down to single
    /// operations. Returns whether the stream got shorter.
    fn take_out_operations(
        &mut self,
        ops: &mut Vec<Op>,
        divergence: &mut Divergence,
    ) -> Result<bool, Error> {
        let mut shortened = false;
        let mut run_len = (ops.len() / 2).max(1);
        loop {
            let mut taken_out = false;
            let mut start = 0;
            while start < ops.len() && !self.spent() {
                let end = (start + run_len).min(ops.len());
                let mut shorter = ops[..start].to_vec();
                shorter.extend_from_slice(&ops[end..]);
                if self.keep_if_it_diverges(shorter, ops, divergence)? {
                    taken_out = true;
                } else {
                    start = end;
                }
            }
            shortened |= taken_out;
            if self.spent() || (run_len == 1 && !taken_out) {
                return Ok(shortened);
            }
            run_len = (run_len / 2).max(1);
        }
    }

    /// Takes single operations out of the batches. Returns whether a batch
    /// got shorter.
    fn take_out_of_batches(
        &mut self,
        ops: &mut Vec<Op>,
        divergence: &mut Divergence,
    ) -> Result<bool, Error> {
        let mut shortened = false;
        let mut at = 0;
        while at < ops.len() {
            for segment in 0..segments(&mut ops[at]).len() {
                let mut op = segments(&mut ops[at])[segment].len();
                while op > 0 && !self.spent() {
                    op -= 1;
                    let mut shorter = ops.clone();
                    segments(&mut shorter[at])[segment].remove(op);
                    shortened |= self.keep_if_it_diverges(shorter, ops, divergence)?;
                    if at >= ops.len() {
                        return Ok(shortened);
                    }
                    op = op.min(segments(&mut ops[at])[segment].len());
                }
            }
            at += 1;
        }
        Ok(shortened)
    }
}

/// The lists of operations a batch is made of: one, or the two segments of
/// a batch applied in two. None for an operation that is no batch.
fn segments(op: &mut Op) -> Vec<&mut Vec<BatchOp>> {
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

/// Shrinks `case`, on which the two engines differ as `divergence` says.
///
/// Call it on a thread with a stack of
/// [`STACK_BYTES`](crate::golden::run::STACK_BYTES).
pub fn shrink<A: StreamEngine, B: StreamEngine>(
    case: &Case,
    divergence: &Divergence,
    reference: &Side<A>,
    candidate: &Side<B>,
    scratch: &Path,
    budget: Budget,
) -> Result<Shrunk, Error> {
    let mut shrinker = Shrinker {
        template: case,
        target: &divergence.parts,
        reference,
        candidate,
        scratch,
        // The step at which stored records differ is found by comparing
        // them after every step. A divergence of a step shows without.
        stored: if divergence.parts.contains(&Part::Stored) {
            Stored::Every(1)
        } else {
            Stored::Never
        },
        budget,
        started: Instant::now(),
        runs: 0,
    };
    let mut ops = case.ops.clone();
    ops.truncate(divergence.step + 1);
    let mut found = divergence.clone();
    if !shrinker.keep_if_it_diverges(ops.clone(), &mut ops, &mut found)? {
        return Ok(Shrunk {
            case: case.clone(),
            divergence: divergence.clone(),
            runs: shrinker.runs,
            reproduced: false,
            minimal: false,
        });
    }
    loop {
        shrinker.take_out_operations(&mut ops, &mut found)?;
        let shorter_batches = shrinker.take_out_of_batches(&mut ops, &mut found)?;
        // A pass over the batches that took nothing out leaves the stream
        // the pass over the operations left, from which no operation can
        // be taken.
        if shrinker.spent() || !shorter_batches {
            break;
        }
    }
    Ok(Shrunk {
        case: case.with_ops(ops),
        divergence: found,
        runs: shrinker.runs,
        reproduced: true,
        minimal: !shrinker.spent(),
    })
}

#[cfg(test)]
mod tests {
    use grovedb::{Element, GroveDb, RocksDbEngine};
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        diff::{
            engines::{Fault, StandIn},
            pair::stand_in_with_a_stray_record,
        },
        golden::{record::on_a_deep_stack, run::Runner},
        workload::{HandoverBatch, PartialBatch, Read, TransactionOp, Write},
    };

    fn item(len: usize) -> Element {
        Element::new_item(vec![5; len])
    }

    fn insert(key: u8, element: Element) -> Op {
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: vec![key],
            element,
        })
    }

    fn batch_insert(path: &[u8], key: u8, element: Element) -> BatchOp {
        BatchOp::InsertOrReplace {
            path: path.iter().map(|segment| vec![*segment]).collect(),
            key: vec![key],
            element,
        }
    }

    /// What the last of `ops` adds, on a fresh database.
    fn added_by_the_last_of(ops: &[Op]) -> u32 {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::open(dir.path()).expect("a database");
        let mut runner = Runner::new(&db, GroveVersion::latest()).expect("a runner");
        let mut added = 0;
        for op in ops {
            let observed = runner.step(op).expect("a step");
            assert!(!observed.step.failed(), "{op:?}: {:?}", observed.remarks);
            added = observed.step.cost.storage_cost.added_bytes;
        }
        added
    }

    fn shrunk(case: &Case, fault: Fault, budget: Budget) -> (Divergence, Shrunk) {
        let (reference, candidate) = (Side::<RocksDbEngine>::of("rocksdb"), StandIn::side(fault));
        let scratch = tempfile::tempdir().expect("a directory");
        on_a_deep_stack(|| {
            let divergence = run(
                case,
                &reference,
                &candidate,
                scratch.path(),
                Stored::AtTheEnd,
                &|| false,
            )
            .expect("the case runs")
            .divergence
            .expect("the engines differ");
            let shrunk = shrink(
                case,
                &divergence,
                &reference,
                &candidate,
                scratch.path(),
                budget,
            )
            .expect("the case shrinks");
            (divergence, shrunk)
        })
    }

    /// A stream of sixty operations of which two are needed, the tree and
    /// the batch that writes into it, and of that batch one operation of
    /// three. The other two write what is stored already, so they add no
    /// byte and the batch is charged wrongly with them and without.
    #[test]
    fn a_stream_shrinks_to_the_operations_the_divergence_needs() {
        let needed = [
            insert(200, Element::empty_tree()),
            Op::Batch(vec![batch_insert(&[200], 1, item(300))]),
        ];
        let when_added = added_by_the_last_of(&needed);
        let mut ops: Vec<Op> = (0..30).map(|key| insert(key, item(key as usize))).collect();
        ops.push(insert(200, Element::empty_tree()));
        ops.extend((30..45).map(|key| insert(key, item(key as usize))));
        ops.push(Op::Commit);
        ops.push(Op::Batch(vec![
            batch_insert(&[], 3, item(3)),
            batch_insert(&[200], 1, item(300)),
            batch_insert(&[], 4, item(4)),
        ]));
        ops.extend((45..55).map(|key| insert(key, item(key as usize))));
        let case = Case::of(9, GroveVersion::latest(), ops);

        let (divergence, shrunk) =
            shrunk(&case, Fault::AddedByte { when_added }, Budget::default());
        assert_eq!(
            (divergence.step, &divergence.parts),
            (47, &vec![Part::Cost])
        );
        assert!(shrunk.reproduced && shrunk.minimal);
        assert_eq!(shrunk.case.ops, needed);
        assert_eq!(shrunk.divergence.step, 1);
        assert_eq!(shrunk.divergence.parts, [Part::Cost]);
        assert_eq!(shrunk.case.seed, 9);
        assert!(shrunk.runs < 100, "{} runs", shrunk.runs);
    }

    #[test]
    fn a_spent_budget_leaves_a_stream_that_still_diverges() {
        let needed = [insert(200, item(300))];
        let when_added = added_by_the_last_of(&needed);
        let mut ops: Vec<Op> = (0..40).map(|key| insert(key, item(key as usize))).collect();
        ops.push(insert(200, item(300)));
        let case = Case::of(0, GroveVersion::latest(), ops);
        let fault = Fault::AddedByte { when_added };

        // One run shows the divergence again and one takes half the
        // stream out.
        let few_runs = Budget {
            runs: 2,
            ..Budget::default()
        };
        let (_, cut) = shrunk(&case, fault, few_runs);
        assert!(cut.reproduced && !cut.minimal);
        assert_eq!((cut.runs, cut.case.ops.len()), (2, 21));
        assert_eq!(cut.divergence.step + 1, cut.case.ops.len());

        let no_time = Budget {
            time: Duration::ZERO,
            ..Budget::default()
        };
        let (_, cut) = shrunk(&case, fault, no_time);
        assert!(cut.reproduced && !cut.minimal);
        assert_eq!((cut.runs, cut.case.ops.len()), (1, 41));

        let (_, whole) = shrunk(&case, fault, Budget::default());
        assert!(whole.minimal);
        assert_eq!(whole.case.ops, needed);
    }

    /// Only the tail of the stream goes when what differs is stored and
    /// not returned: the records are compared after every step.
    #[test]
    fn a_divergence_in_what_is_stored_shrinks_to_its_first_step() {
        let seeded = stand_in_with_a_stray_record();
        let reference = Side::<RocksDbEngine>::of("rocksdb");
        let ops: Vec<Op> = (0..12).map(|key| insert(key, item(4))).collect();
        let case = Case::of(0, GroveVersion::latest(), ops);
        let scratch = tempfile::tempdir().expect("a directory");
        let (divergence, shrunk) = on_a_deep_stack(|| {
            let divergence = run(
                &case,
                &reference,
                &seeded,
                scratch.path(),
                Stored::AtTheEnd,
                &|| false,
            )
            .expect("the case runs")
            .divergence
            .expect("the engines differ");
            let shrunk = shrink(
                &case,
                &divergence,
                &reference,
                &seeded,
                scratch.path(),
                Budget::default(),
            )
            .expect("the case shrinks");
            (divergence, shrunk)
        });
        assert_eq!(
            (divergence.step, &divergence.parts),
            (11, &vec![Part::Stored])
        );
        assert!(shrunk.reproduced && shrunk.minimal);
        assert_eq!(shrunk.case.ops.len(), 1, "any one operation shows it");
        assert_eq!(shrunk.divergence.parts, [Part::Stored]);
    }

    /// A divergence that does not show a second time is reported as that.
    #[test]
    fn a_divergence_that_does_not_show_again_is_not_shrunk() {
        let case = Case::of(0, GroveVersion::latest(), vec![insert(1, item(4))]);
        let (reference, candidate) = (
            Side::<RocksDbEngine>::of("rocksdb"),
            Side::<StandIn>::of("stand-in"),
        );
        let claimed = Divergence {
            step: 0,
            parts: vec![Part::Cost],
            reference: Vec::new(),
            candidate: Vec::new(),
        };
        let scratch = tempfile::tempdir().expect("a directory");
        let shrunk = on_a_deep_stack(|| {
            shrink(
                &case,
                &claimed,
                &reference,
                &candidate,
                scratch.path(),
                Budget::default(),
            )
        })
        .expect("the case runs");
        assert!(!shrunk.reproduced && !shrunk.minimal);
        assert_eq!((shrunk.runs, shrunk.case.ops.len()), (1, 1));
        assert_eq!(shrunk.divergence, claimed);
    }

    #[test]
    fn the_segments_of_every_kind_of_batch_are_shrunk() {
        let delete = || BatchOp::Delete {
            path: Vec::new(),
            key: vec![1],
        };
        let partial = || PartialBatch {
            ops: vec![delete(), delete()],
            add_on: vec![delete()],
            pause_height: None,
        };
        let lens =
            |mut op: Op| -> Vec<usize> { segments(&mut op).iter().map(|ops| ops.len()).collect() };
        assert_eq!(lens(Op::Batch(vec![delete()])), [1]);
        assert_eq!(lens(Op::FlagsBatch(vec![delete(), delete()])), [2]);
        assert_eq!(lens(Op::PartialBatch(partial())), [2, 1]);
        assert_eq!(lens(Op::FlagsPartialBatch(partial())), [2, 1]);
        let handover = HandoverBatch {
            ops: vec![delete()],
            full_handover_owners: Default::default(),
        };
        assert_eq!(lens(Op::FlagsHandoverBatch(handover)), [1]);
        for other in [
            Op::Commit,
            Op::FlushPendingDrops,
            insert(1, item(1)),
            Op::Read(Read::GetAux { key: Vec::new() }),
            Op::Transaction(TransactionOp::Abandon),
        ] {
            assert_eq!(lens(other), Vec::<usize>::new());
        }
    }

    /// A batch applied in two segments loses what is not needed from both.
    #[test]
    fn a_batch_in_two_segments_is_shrunk_in_both() {
        let batch = |ops, add_on| {
            Op::PartialBatch(PartialBatch {
                ops,
                add_on,
                pause_height: None,
            })
        };
        let needed = [
            insert(200, Element::empty_tree()),
            batch(vec![batch_insert(&[200], 9, item(300))], Vec::new()),
        ];
        let when_added = added_by_the_last_of(&needed);
        let case = Case::of(
            0,
            GroveVersion::latest(),
            vec![
                insert(1, item(1)),
                insert(2, item(2)),
                insert(200, Element::empty_tree()),
                batch(
                    vec![
                        batch_insert(&[], 1, item(1)),
                        batch_insert(&[200], 9, item(300)),
                    ],
                    vec![batch_insert(&[], 2, item(2))],
                ),
            ],
        );
        let (divergence, shrunk) =
            shrunk(&case, Fault::AddedByte { when_added }, Budget::default());
        assert_eq!(divergence.step, 3);
        assert!(shrunk.minimal);
        assert_eq!(shrunk.case.ops, needed);
    }
}
