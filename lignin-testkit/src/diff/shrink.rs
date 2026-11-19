//! Shrinking a stream two engines answered differently.
//!
//! A case that diverges has hundreds of operations and the divergence
//! needs a few of them. [`shrink`] looks for those few: it runs shorter
//! streams on fresh databases and keeps a shorter one whenever it shows
//! the divergence again ([`Divergence::shows_again_as`]): the engines
//! differ in no part they did not differ in before.
//!
//! 1. Everything after the diverging step goes.
//! 2. Runs of operations are taken out, first half the stream at a time,
//!    then a quarter, down to single operations, until no single operation
//!    can be taken out.
//! 3. Single operations are taken out of the batches that are left.
//! 4. Steps 2 and 3 repeat until step 3 takes nothing out.
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
    case::{batch_segments, Case},
    pair::{run, Divergence, Part, Side, Stored},
};
use crate::{
    golden::Error,
    workload::{Op, StreamEngine},
};

/// How much work a shrinking may do, beyond the one run that shows the
/// divergence again, which is always made.
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
    /// How long it took.
    pub took: Duration,
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
    target: &'a Divergence,
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

    /// Runs `shorter` in place of `ops`. If it shows the divergence again,
    /// it takes their place, cut after the step the engines differ at.
    fn keep_if_it_diverges(
        &mut self,
        shorter: Vec<Op>,
        ops: &mut Vec<Op>,
        divergence: &mut Divergence,
    ) -> Result<bool, Error> {
        self.runs += 1;
        let case = self.template.with_ops(shorter);
        let ran = run(
            &case,
            self.reference,
            self.candidate,
            self.scratch,
            self.stored,
            &|| false,
        )?;
        let Some(found) = ran
            .divergence
            .filter(|found| self.target.shows_again_as(found))
        else {
            return Ok(false);
        };
        *ops = case.ops;
        ops.truncate(found.step + 1);
        *divergence = found;
        Ok(true)
    }

    /// Takes runs of operations out, from half the stream down to single
    /// operations, until no single operation can be taken out or the
    /// budget is spent.
    fn take_out_operations(
        &mut self,
        ops: &mut Vec<Op>,
        divergence: &mut Divergence,
    ) -> Result<(), Error> {
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
            if self.spent() || (run_len == 1 && !taken_out) {
                return Ok(());
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
            for segment in 0..batch_segments(&mut ops[at]).len() {
                for op in (0..batch_segments(&mut ops[at])[segment].len()).rev() {
                    if self.spent() {
                        return Ok(shortened);
                    }
                    let mut shorter = ops.clone();
                    batch_segments(&mut shorter[at])[segment].remove(op);
                    shortened |= self.keep_if_it_diverges(shorter, ops, divergence)?;
                    // The stream is cut after the step the engines differ
                    // at, which may now be before this batch.
                    if at >= ops.len() {
                        return Ok(shortened);
                    }
                }
            }
            at += 1;
        }
        Ok(shortened)
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
        target: divergence,
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
    let reproduced = shrinker.keep_if_it_diverges(ops.clone(), &mut ops, &mut found)?;
    if reproduced {
        loop {
            shrinker.take_out_operations(&mut ops, &mut found)?;
            // A pass over the batches that takes nothing out leaves the
            // stream the pass over the operations left, from which no
            // operation can be taken.
            if !shrinker.take_out_of_batches(&mut ops, &mut found)? || shrinker.spent() {
                break;
            }
        }
    }
    Ok(Shrunk {
        case: if reproduced {
            case.with_ops(ops)
        } else {
            case.clone()
        },
        divergence: found,
        runs: shrinker.runs,
        took: shrinker.started.elapsed(),
        reproduced,
        minimal: reproduced && !shrinker.spent(),
    })
}

#[cfg(test)]
mod tests {
    use grovedb::Element;
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        diff::{
            engines::{Fault, StandIn},
            pair::testing::{
                added_by_the_last_of, rocksdb, stand_in, stand_in_with_a_stray_record,
            },
        },
        golden::record::on_a_deep_stack,
        workload::{BatchOp, PartialBatch, Write},
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

    /// The divergence of `case` on RocksDB against `candidate`, and what
    /// shrinking it comes to.
    fn shrunk<B: StreamEngine>(
        case: &Case,
        candidate: &Side<B>,
        budget: Budget,
    ) -> (Divergence, Shrunk) {
        let reference = rocksdb();
        let scratch = tempfile::tempdir().expect("a directory");
        on_a_deep_stack(|| {
            let divergence = run(
                case,
                &reference,
                candidate,
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
                candidate,
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

        let candidate = StandIn::side(Fault::AddedByte { when_added });
        let (divergence, shrunk) = shrunk(&case, &candidate, Budget::default());
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
        assert!(shrunk.took > Duration::ZERO);
    }

    #[test]
    fn a_spent_budget_leaves_a_stream_that_still_diverges() {
        let needed = [insert(200, item(300))];
        let when_added = added_by_the_last_of(&needed);
        let mut ops: Vec<Op> = (0..40).map(|key| insert(key, item(key as usize))).collect();
        ops.push(insert(200, item(300)));
        let case = Case::of(0, GroveVersion::latest(), ops);
        let candidate = StandIn::side(Fault::AddedByte { when_added });

        // One run shows the divergence again and one takes half the
        // stream out.
        let few_runs = Budget {
            runs: 2,
            ..Budget::default()
        };
        let (_, cut) = shrunk(&case, &candidate, few_runs);
        assert!(cut.reproduced && !cut.minimal);
        assert_eq!((cut.runs, cut.case.ops.len()), (2, 21));
        assert_eq!(cut.divergence.step + 1, cut.case.ops.len());

        // The run that shows it again is made whatever the budget.
        for nothing in [
            Budget {
                time: Duration::ZERO,
                ..Budget::default()
            },
            Budget {
                runs: 0,
                ..Budget::default()
            },
        ] {
            let (_, cut) = shrunk(&case, &candidate, nothing);
            assert!(cut.reproduced && !cut.minimal);
            assert_eq!((cut.runs, cut.case.ops.len()), (1, 41));
        }

        let (_, whole) = shrunk(&case, &candidate, Budget::default());
        assert!(whole.minimal);
        assert_eq!(whole.case.ops, needed);
    }

    /// Only the tail of the stream goes when what differs is stored and
    /// not returned: the records are compared after every step.
    #[test]
    fn a_divergence_in_what_is_stored_shrinks_to_its_first_step() {
        let ops: Vec<Op> = (0..12).map(|key| insert(key, item(4))).collect();
        let case = Case::of(0, GroveVersion::latest(), ops);
        let (divergence, shrunk) =
            shrunk(&case, &stand_in_with_a_stray_record(), Budget::default());
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
                &rocksdb(),
                &stand_in(),
                scratch.path(),
                Budget::default(),
            )
        })
        .expect("the case runs");
        assert!(!shrunk.reproduced && !shrunk.minimal);
        assert_eq!((shrunk.runs, shrunk.case.ops.len()), (1, 1));
        assert_eq!(shrunk.divergence, claimed);
    }

    /// A shorter stream on which the engines differ in another way is not
    /// taken for the divergence: here the candidate panics on one size of
    /// write and is charged wrongly for none, so a stream that only shows
    /// the panic does not show a divergence in a cost.
    #[test]
    fn a_stream_that_differs_in_another_part_is_not_kept() {
        let ops = vec![insert(1, item(4)), insert(200, item(300))];
        let when_added = added_by_the_last_of(&ops);
        let case = Case::of(0, GroveVersion::latest(), ops);
        let claimed = Divergence {
            step: 1,
            parts: vec![Part::Cost],
            reference: Vec::new(),
            candidate: Vec::new(),
        };
        let scratch = tempfile::tempdir().expect("a directory");
        let shrunk = on_a_deep_stack(|| {
            shrink(
                &case,
                &claimed,
                &rocksdb(),
                &StandIn::side(Fault::Panic { when_added }),
                scratch.path(),
                Budget::default(),
            )
        })
        .expect("the case runs");
        assert!(!shrunk.reproduced);
        assert_eq!(shrunk.case.ops.len(), 2);
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
        let candidate = StandIn::side(Fault::AddedByte { when_added });
        let (divergence, shrunk) = shrunk(&case, &candidate, Budget::default());
        assert_eq!(divergence.step, 3);
        assert!(shrunk.minimal);
        assert_eq!(shrunk.case.ops, needed);
    }
}
