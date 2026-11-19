//! One case on two engines, compared after every step.
//!
//! [`run`] opens a fresh database of each engine, applies the operations of
//! a case to both through the corpus's own [`Runner`], and stops at the
//! first step the two answer differently. What a step is, and so what is
//! compared, is what the golden corpus records: the `OperationCost`, the
//! result or the name of the error, the root hash inside the open
//! transaction, and for a read GroveDB proves the proof bytes, the cost of
//! proving and what the verifier makes of the proof. Besides that,
//! [`Stored`] says how often the records the two databases hold are
//! compared ([`super::state`]).
//!
//! An engine that panics where the other answers has diverged. Where both
//! panic, or neither can be observed, they did the same and the case ends
//! there.

#![expect(
    clippy::result_large_err,
    reason = "a database is opened with GroveDB's own error, whatever its size"
)]

use std::{
    cell::{Cell, RefCell},
    panic::{self, AssertUnwindSafe},
    path::Path,
    sync::Once,
};

use grovedb::GroveDb;

use super::{
    case::Case,
    state::{first_difference, stored, Record},
};
use crate::{
    golden::{
        run::{Observed, Runner},
        step::{Outcome, Step},
        trace::{one_line, shortened},
        Error,
    },
    workload::{Op, StreamEngine},
};

/// The longest line of a report. A proof of a hundred documents is a
/// hundred thousand hex digits.
const LINE_CHARS: usize = 400;

/// How a database of one engine is opened.
type Open<E> = dyn Fn(&Path) -> Result<GroveDb<E>, grovedb::Error> + Send + Sync;

/// One of the two engines of a comparison: its name in a report, and how a
/// fresh database of it is opened.
pub struct Side<E: StreamEngine> {
    name: String,
    open: Box<Open<E>>,
}

impl<E: StreamEngine> Side<E> {
    /// The engine as it opens itself.
    pub fn of(name: &str) -> Self {
        Self::opened_by(name, |dir| GroveDb::open_with_engine(dir))
    }

    /// An engine opened in a way of the caller's.
    pub fn opened_by(
        name: &str,
        open: impl Fn(&Path) -> Result<GroveDb<E>, grovedb::Error> + Send + Sync + 'static,
    ) -> Self {
        Side {
            name: name.to_string(),
            open: Box::new(open),
        }
    }

    /// The name of the engine in a report.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn open(&self, dir: &Path) -> Result<GroveDb<E>, Error> {
        (self.open)(dir)
            .map_err(|error| Error::engine(&format!("opening a database of {}", self.name), &error))
    }
}

/// How often the records the two databases store are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    /// Never.
    Never,
    /// Once, after the last operation.
    AtTheEnd,
    /// After every this many operations that are not reads, and after the
    /// last operation.
    Every(u64),
}

/// What two engines can differ in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    /// The `OperationCost` of the operation.
    Cost,
    /// The value the operation returned.
    Result,
    /// The error the operation failed with, or that only one of them failed.
    Error,
    /// The root hash after the operation.
    RootHash,
    /// The `OperationCost` of proving the query.
    ProofCost,
    /// The proof bytes, the error proving failed with, or what the verifier
    /// made of the proof.
    Proof,
    /// The records the databases store.
    Stored,
    /// One engine panicked and the other did not.
    Panic,
    /// One engine could not be observed after the operation and the other
    /// could.
    Observation,
}

impl Part {
    /// The name of the part in a report.
    pub fn name(self) -> &'static str {
        match self {
            Part::Cost => "cost",
            Part::Result => "result",
            Part::Error => "error",
            Part::RootHash => "root hash",
            Part::ProofCost => "proof cost",
            Part::Proof => "proof",
            Part::Stored => "stored records",
            Part::Panic => "panic",
            Part::Observation => "observation",
        }
    }
}

/// The first step of a case two engines answered differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// The position of the operation in the case.
    pub step: usize,
    /// What differs.
    pub parts: Vec<Part>,
    /// What the reference returned, a line for each value.
    pub reference: Vec<String>,
    /// What the candidate returned.
    pub candidate: Vec<String>,
}

impl Divergence {
    /// The divergence for a reader: the operation, what differs, and the
    /// lines of the two engines that are not the same.
    pub fn report(&self, case: &Case, reference: &str, candidate: &str) -> Vec<String> {
        let parts: Vec<&str> = self.parts.iter().map(|part| part.name()).collect();
        let mut lines = vec![format!(
            "step {} of {}: the engines differ in {}",
            self.step,
            case.ops.len(),
            parts.join(", ")
        )];
        if let Some(op) = case.ops.get(self.step) {
            lines.push(format!("  operation: {}", quoted(op)));
        }
        let pairs = self.reference.len().max(self.candidate.len());
        for index in 0..pairs {
            let (was, is) = (self.reference.get(index), self.candidate.get(index));
            if was == is {
                continue;
            }
            let shared = match (was, is) {
                (Some(was), Some(is)) => was
                    .chars()
                    .zip(is.chars())
                    .take_while(|(was, is)| was == is)
                    .count(),
                _ => 0,
            };
            for (engine, line) in [(reference, was), (candidate, is)] {
                lines.push(match line {
                    Some(line) => format!("  {engine}: {}", around(line, shared)),
                    None => format!("  {engine}: (nothing)"),
                });
            }
        }
        lines
    }
}

/// An operation on one line of printable characters, cut to a readable
/// length.
pub fn quoted(op: &Op) -> String {
    shortened(&one_line(&format!("{op:?}")), LINE_CHARS)
}

/// A line cut to a readable length around the character at `shared`, where
/// it starts to differ from the line it is compared with. What a line
/// begins with says what it holds, so its first word is kept.
fn around(line: &str, shared: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= LINE_CHARS {
        return line.to_string();
    }
    let word = chars
        .iter()
        .position(|character| *character == ' ')
        .map_or(0, |end| end + 1);
    let from = shared.saturating_sub(LINE_CHARS / 4).max(word);
    let to = (from + LINE_CHARS).min(chars.len());
    let mut cut: String = chars[..word].iter().collect();
    if from > word {
        cut.push_str(&format!("…(from character {from}) "));
    }
    cut.extend(&chars[from..to]);
    if to < chars.len() {
        cut.push('…');
    }
    cut
}

/// What a run compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// The operations applied to both engines.
    pub steps: u64,
    /// The operations that failed, the same way on both.
    pub failed: u64,
    /// The proofs compared.
    pub proofs: u64,
    /// The times the stored records were compared.
    pub walks: u64,
    /// The records those comparisons read from each engine.
    pub records: u64,
    /// The cases that ended early because both engines panicked or neither
    /// could be observed.
    pub cut_short: u64,
}

impl Tally {
    /// Adds what another run compared.
    pub fn add(&mut self, other: &Tally) {
        self.steps += other.steps;
        self.failed += other.failed;
        self.proofs += other.proofs;
        self.walks += other.walks;
        self.records += other.records;
        self.cut_short += other.cut_short;
    }
}

/// How the run of a case ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// What was compared.
    pub tally: Tally,
    /// The first step the engines answered differently, if there is one.
    pub divergence: Option<Divergence>,
    /// Why the case ended before its last operation without a divergence:
    /// both engines panicked, neither could be observed, or the caller
    /// interrupted the run.
    pub cut_short: Option<String>,
}

/// What an engine did with one operation.
enum Seen {
    Step(Box<Observed>),
    Unobserved(String),
    Panicked(String),
}

thread_local! {
    static CATCHING: Cell<bool> = const { Cell::new(false) };
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Runs `body` and returns the message of its panic if it panics. The
/// panic is not printed: a mutated stream may ask for what an engine
/// panics on, and the report says so where it matters.
fn catching<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let printing = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if CATCHING.get() {
                PANIC.set(Some(info.to_string()));
            } else {
                printing(info);
            }
        }));
    });
    CATCHING.set(true);
    let result = panic::catch_unwind(AssertUnwindSafe(body));
    CATCHING.set(false);
    result.map_err(|_| {
        PANIC
            .take()
            .map_or("a panic".to_string(), |message| one_line(&message))
    })
}

fn observe<E: StreamEngine>(runner: &mut Runner<'_, E>, op: &Op) -> Seen {
    match catching(|| runner.step(op)) {
        Ok(Ok(observed)) => Seen::Step(Box::new(observed)),
        Ok(Err(error)) => Seen::Unobserved(error.to_string()),
        Err(panic) => Seen::Panicked(panic),
    }
}

/// The parts in which two steps differ.
pub fn differences(reference: &Step, candidate: &Step) -> Vec<Part> {
    let mut parts = Vec::new();
    if reference.cost != candidate.cost {
        parts.push(Part::Cost);
    }
    match (&reference.outcome, &candidate.outcome) {
        (Outcome::Returned(was), Outcome::Returned(is)) if was == is => {}
        (Outcome::Returned(_), Outcome::Returned(_)) => parts.push(Part::Result),
        (Outcome::Failed(was), Outcome::Failed(is)) if was == is => {}
        _ => parts.push(Part::Error),
    }
    if reference.root_hash != candidate.root_hash {
        parts.push(Part::RootHash);
    }
    match (&reference.proof, &candidate.proof) {
        (Some(was), Some(is)) => {
            if was.cost != is.cost {
                parts.push(Part::ProofCost);
            }
            if was.proved != is.proved {
                parts.push(Part::Proof);
            }
        }
        (None, None) => {}
        _ => parts.push(Part::Proof),
    }
    parts
}

/// The values of a step with the messages of its errors, a line each.
fn lines(observed: &Observed) -> Vec<String> {
    let mut lines = observed.step.detail();
    lines.extend(observed.remarks.iter().map(|remark| format!("# {remark}")));
    lines
}

/// The records a database stores, as the open transaction of its runner
/// sees them.
fn listing<'db, E: StreamEngine>(
    db: &'db GroveDb<E>,
    runner: &Runner<'db, E>,
    side: &Side<E>,
    case: &Case,
) -> Result<Vec<Record>, Error> {
    stored(db, runner.transactions().block(), case.grove_version)
        .map_err(|error| Error::engine(&format!("walking {}", side.name()), &error))
}

/// Runs `case` on a fresh database of each engine, under `scratch`, and
/// compares them after every step. `interrupted` is asked before each
/// operation and ends the run when it says so.
///
/// Call it on a thread with a stack of
/// [`STACK_BYTES`](crate::golden::run::STACK_BYTES).
pub fn run<A: StreamEngine, B: StreamEngine>(
    case: &Case,
    reference: &Side<A>,
    candidate: &Side<B>,
    scratch: &Path,
    stored_check: Stored,
    interrupted: &dyn Fn() -> bool,
) -> Result<Run, Error> {
    let dir = tempfile::Builder::new()
        .prefix("lignin-diff-")
        .tempdir_in(scratch)
        .map_err(|error| Error::io(scratch, &error))?;
    let reference_db = reference.open(&dir.path().join("reference"))?;
    let candidate_db = candidate.open(&dir.path().join("candidate"))?;
    let mut reference_runner = Runner::new(&reference_db, case.grove_version)?;
    let mut candidate_runner = Runner::new(&candidate_db, case.grove_version)?;

    let mut tally = Tally::default();
    let mut writes = 0u64;
    let diverged = |tally, divergence| Run {
        tally,
        divergence: Some(divergence),
        cut_short: None,
    };
    let cut_short = |mut tally: Tally, why: String| {
        tally.cut_short = 1;
        Run {
            tally,
            divergence: None,
            cut_short: Some(why),
        }
    };
    for (step, op) in case.ops.iter().enumerate() {
        if interrupted() {
            return Ok(cut_short(tally, "interrupted".to_string()));
        }
        let was = observe(&mut reference_runner, op);
        let is = observe(&mut candidate_runner, op);
        tally.steps += 1;
        let (was, is) = match (was, is) {
            (Seen::Step(was), Seen::Step(is)) => (was, is),
            (Seen::Panicked(was), Seen::Panicked(_)) => {
                return Ok(cut_short(
                    tally,
                    format!("both engines panicked at step {step}: {was}"),
                ));
            }
            (Seen::Unobserved(was), Seen::Unobserved(_)) => {
                return Ok(cut_short(
                    tally,
                    format!("neither engine could be observed at step {step}: {was}"),
                ));
            }
            (was, is) => {
                let panicked = matches!(was, Seen::Panicked(_)) || matches!(is, Seen::Panicked(_));
                let said = |seen: Seen| match seen {
                    Seen::Step(observed) => lines(&observed),
                    Seen::Unobserved(error) => vec![format!("not observed: {error}")],
                    Seen::Panicked(panic) => vec![format!("panicked: {panic}")],
                };
                return Ok(diverged(
                    tally,
                    Divergence {
                        step,
                        parts: vec![if panicked {
                            Part::Panic
                        } else {
                            Part::Observation
                        }],
                        reference: said(was),
                        candidate: said(is),
                    },
                ));
            }
        };
        let parts = differences(&was.step, &is.step);
        if !parts.is_empty() {
            return Ok(diverged(
                tally,
                Divergence {
                    step,
                    parts,
                    reference: lines(&was),
                    candidate: lines(&is),
                },
            ));
        }
        tally.failed += u64::from(was.step.failed());
        tally.proofs += u64::from(was.step.proof.is_some());

        let last = step + 1 == case.ops.len();
        if !matches!(op, Op::Read(_)) {
            writes += 1;
        }
        let due = match stored_check {
            Stored::Never => false,
            Stored::AtTheEnd => last,
            Stored::Every(every) => {
                last || (!matches!(op, Op::Read(_)) && writes.is_multiple_of(every.max(1)))
            }
        };
        if due {
            let was = listing(&reference_db, &reference_runner, reference, case)?;
            let is = listing(&candidate_db, &candidate_runner, candidate, case)?;
            tally.walks += 1;
            tally.records += was.len() as u64;
            if let Some((was, is)) = first_difference(&was, &is) {
                let text = |record: Option<&Record>| {
                    vec![record.map_or("no further record".to_string(), Record::text)]
                };
                return Ok(diverged(
                    tally,
                    Divergence {
                        step,
                        parts: vec![Part::Stored],
                        reference: text(was),
                        candidate: text(is),
                    },
                ));
            }
        }
    }
    Ok(Run {
        tally,
        divergence: None,
        cut_short: None,
    })
}

/// A stand-in whose database holds, from the start, a record of the
/// auxiliary keyspace that no stream writes: an engine that stores what no
/// hash covers differently.
#[cfg(test)]
pub(crate) fn stand_in_with_a_stray_record() -> Side<super::engines::StandIn> {
    Side::opened_by("seeded", |dir| {
        let db = GroveDb::<super::engines::StandIn>::open_with_engine(dir)?;
        db.put_aux(b"left", b"behind", None, None).unwrap()?;
        Ok(db)
    })
}

#[cfg(test)]
mod tests {
    use grovedb::{Element, RocksDbEngine};
    use grovedb_costs::OperationCost;
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        diff::engines::{Fault, StandIn},
        golden::{
            record::on_a_deep_stack,
            step::{Proof, Proved, Value, Verification},
        },
        workload::{Query, QueryItem, Read, TransactionOp, Write},
    };

    fn insert(key: &[u8], value: Vec<u8>) -> Op {
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: key.to_vec(),
            element: Element::new_item(value),
        })
    }

    fn all() -> Op {
        Op::Read(Read::Query(Query {
            path: Vec::new(),
            items: vec![QueryItem::RangeFull],
            subquery_path: None,
            subquery_all: false,
            limit: None,
            left_to_right: true,
            raw: true,
        }))
    }

    fn case(ops: Vec<Op>) -> Case {
        Case::of(0, GroveVersion::latest(), ops)
    }

    fn compared<A: StreamEngine, B: StreamEngine>(
        case: &Case,
        reference: &Side<A>,
        candidate: &Side<B>,
        stored_check: Stored,
    ) -> Run {
        let scratch = tempfile::tempdir().expect("a directory");
        on_a_deep_stack(|| {
            run(
                case,
                reference,
                candidate,
                scratch.path(),
                stored_check,
                &|| false,
            )
        })
        .expect("the case runs")
    }

    /// The size of the write of an item of `len` bytes into the root tree,
    /// under a key of one byte, on a database that holds one item already.
    fn added_by_an_item_of(len: usize) -> u32 {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::open(dir.path()).expect("a database");
        let grove_version = GroveVersion::latest();
        let mut runner = Runner::new(&db, grove_version).expect("a runner");
        runner.step(&insert(b"a", vec![0; 8])).expect("an insert");
        let observed = runner.step(&insert(b"b", vec![0; len])).expect("an insert");
        observed.step.cost.storage_cost.added_bytes
    }

    #[test]
    fn two_engines_that_answer_the_same_run_to_the_end() {
        let case = case(vec![
            insert(b"a", vec![1; 8]),
            insert(b"b", vec![2; 60]),
            Op::Commit,
            all(),
            Op::Write(Write::Delete {
                path: Vec::new(),
                key: b"missing".to_vec(),
            }),
            Op::Transaction(TransactionOp::Rollback),
        ]);
        let run = compared(
            &case,
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            Stored::Every(2),
        );
        assert_eq!(run.divergence, None);
        assert_eq!(run.cut_short, None);
        assert_eq!(
            run.tally,
            Tally {
                steps: 6,
                failed: 1,
                proofs: 1,
                walks: 3,
                records: 3 + 3 + 3,
                cut_short: 0,
            }
        );
        let once = compared(
            &case,
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            Stored::AtTheEnd,
        );
        assert_eq!((once.tally.walks, once.tally.records), (1, 3));
        let never = compared(
            &case,
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            Stored::Never,
        );
        assert_eq!(never.tally.walks, 0);
    }

    #[test]
    fn a_byte_too_many_in_a_cost_is_a_divergence_at_its_step() {
        let when_added = added_by_an_item_of(60);
        assert_ne!(when_added, added_by_an_item_of(8));
        let case = case(vec![
            insert(b"a", vec![1; 8]),
            insert(b"b", vec![2; 60]),
            Op::Commit,
        ]);
        let reference = Side::<RocksDbEngine>::of("rocksdb");
        let candidate = StandIn::side(Fault::AddedByte { when_added });
        let run = compared(&case, &reference, &candidate, Stored::AtTheEnd);
        let divergence = run.divergence.expect("the planted byte");
        assert_eq!(divergence.step, 1);
        assert_eq!(divergence.parts, [Part::Cost]);
        assert_eq!(run.tally.steps, 2);
        let report = divergence.report(&case, reference.name(), candidate.name());
        println!("{}", report.join("\n"));
        assert_eq!(report[0], "step 1 of 3: the engines differ in cost");
        assert!(report[1].starts_with("  operation: Write(Insert {"));
        assert_eq!(report.len(), 4, "the one line that differs, of each engine");
        assert!(
            report[2].starts_with("  rocksdb: cost ")
                && report[2].contains(&format!("added={when_added} "))
        );
        assert!(
            report[3].starts_with("  StandIn with added-byte:")
                && report[3].contains(&format!("added={} ", when_added + 1))
        );
    }

    #[test]
    fn a_panic_of_one_engine_is_a_divergence_and_of_both_ends_the_case() {
        let when_added = added_by_an_item_of(60);
        let case = case(vec![
            insert(b"a", vec![1; 8]),
            insert(b"b", vec![2; 60]),
            Op::Commit,
        ]);
        let fault = Fault::Panic { when_added };
        let run = compared(
            &case,
            &Side::<RocksDbEngine>::of("rocksdb"),
            &StandIn::side(fault),
            Stored::Never,
        );
        let divergence = run.divergence.expect("one engine panicked");
        assert_eq!((divergence.step, divergence.parts), (1, vec![Part::Panic]));
        assert!(divergence.reference[0].starts_with("cost "));
        assert!(
            divergence.candidate[0].starts_with("panicked: ")
                && divergence.candidate[0].contains("the planted fault"),
            "{:?}",
            divergence.candidate
        );

        let both = compared(
            &case,
            &StandIn::side(fault),
            &StandIn::side(fault),
            Stored::Never,
        );
        assert_eq!(both.divergence, None);
        let why = both.cut_short.expect("both engines panicked");
        assert!(
            why.starts_with("both engines panicked at step 1: "),
            "{why}"
        );
        assert_eq!((both.tally.steps, both.tally.cut_short), (2, 1));
    }

    #[test]
    fn a_run_stops_when_it_is_interrupted() {
        let case = case(vec![insert(b"a", vec![1]), Op::Commit]);
        let scratch = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        let run = run(
            &case,
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            scratch.path(),
            Stored::AtTheEnd,
            &|| {
                asked.set(asked.get() + 1);
                asked.get() > 1
            },
        )
        .expect("the case runs");
        assert_eq!(run.cut_short.as_deref(), Some("interrupted"));
        assert_eq!((run.tally.steps, run.tally.walks), (1, 0));
    }

    #[test]
    fn records_one_engine_holds_differently_are_a_divergence() {
        // The same engine opened twice, the second with a record of the
        // auxiliary keyspace the stream never wrote: no hash holds it.
        let seeded = stand_in_with_a_stray_record();
        let case = case(vec![
            insert(b"a", vec![1]),
            insert(b"b", vec![2]),
            Op::Commit,
        ]);
        let reference = Side::<RocksDbEngine>::of("rocksdb");
        for (stored_check, step) in [(Stored::Every(1), 0), (Stored::AtTheEnd, 2)] {
            let run = compared(&case, &reference, &seeded, stored_check);
            let divergence = run.divergence.expect("the record");
            assert_eq!(
                (divergence.step, &divergence.parts),
                (step, &vec![Part::Stored])
            );
            assert_eq!(divergence.reference, ["no further record"]);
            assert_eq!(
                divergence.candidate,
                ["aux / key 6c656674 = 6 bytes 626568696e64"]
            );
        }
        let unseen = compared(&case, &reference, &seeded, Stored::Never);
        assert_eq!(unseen.divergence, None);
    }

    #[test]
    fn a_database_that_does_not_open_is_an_error() {
        let closed = Side::<StandIn>::opened_by("closed", |_| {
            Err(grovedb::Error::InvalidInput("no database here"))
        });
        let scratch = tempfile::tempdir().expect("a directory");
        let refused = run(
            &case(Vec::new()),
            &Side::<RocksDbEngine>::of("rocksdb"),
            &closed,
            scratch.path(),
            Stored::Never,
            &|| false,
        )
        .expect_err("the candidate does not open");
        assert_eq!(
            refused.to_string(),
            "the engine failed opening a database of closed: invalid input: no database here"
        );
        let nowhere = run(
            &case(Vec::new()),
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            &scratch.path().join("missing"),
            Stored::Never,
            &|| false,
        )
        .expect_err("there is no scratch directory");
        assert!(matches!(nowhere, Error::Io { .. }), "{nowhere}");
    }

    #[test]
    fn every_part_of_a_step_is_told_apart() {
        let step = || Step {
            cost: OperationCost::default(),
            outcome: Outcome::Returned(Value::Element(vec![1])),
            root_hash: Some([1; 32]),
            proof: Some(Proof {
                cost: OperationCost::default(),
                proved: Proved::Bytes(vec![1, 2], Verification::Verified),
            }),
        };
        let differ = |change: fn(&mut Step)| {
            let mut changed = step();
            change(&mut changed);
            differences(&step(), &changed)
        };
        assert_eq!(differ(|_| {}), []);
        assert_eq!(
            differ(|step| step.cost.storage_loaded_bytes = 1),
            [Part::Cost]
        );
        assert_eq!(
            differ(|step| step.outcome = Outcome::Returned(Value::Nothing)),
            [Part::Result]
        );
        assert_eq!(
            differ(|step| step.outcome = Outcome::Failed("InvalidInput".to_string())),
            [Part::Error]
        );
        assert_eq!(
            differ(|step| step.root_hash = Some([2; 32])),
            [Part::RootHash]
        );
        assert_eq!(differ(|step| step.root_hash = None), [Part::RootHash]);
        assert_eq!(differ(|step| step.proof = None), [Part::Proof]);
        fn proof(step: &mut Step) -> &mut Proof {
            step.proof.as_mut().expect("a proof")
        }
        assert_eq!(
            differ(|step| proof(step).cost.seek_count = 1),
            [Part::ProofCost]
        );
        assert_eq!(
            differ(|step| proof(step).proved = Proved::Bytes(vec![1, 3], Verification::Verified)),
            [Part::Proof]
        );
        assert_eq!(
            differ(|step| proof(step).proved = Proved::Bytes(vec![1, 2], Verification::OtherRoot)),
            [Part::Proof]
        );
        assert_eq!(
            differ(|step| {
                step.cost.seek_count = 9;
                step.root_hash = Some([3; 32]);
                proof(step).proved = Proved::Failed("NotSupported".to_string());
            }),
            [Part::Cost, Part::RootHash, Part::Proof]
        );
        let failed = |name: &str| {
            let mut failed = step();
            failed.outcome = Outcome::Failed(name.to_string());
            failed
        };
        assert_eq!(differences(&failed("A"), &failed("A")), []);
        assert_eq!(differences(&failed("A"), &failed("B")), [Part::Error]);
        for (part, name) in [
            (Part::Cost, "cost"),
            (Part::Result, "result"),
            (Part::Error, "error"),
            (Part::RootHash, "root hash"),
            (Part::ProofCost, "proof cost"),
            (Part::Proof, "proof"),
            (Part::Stored, "stored records"),
            (Part::Panic, "panic"),
            (Part::Observation, "observation"),
        ] {
            assert_eq!(part.name(), name);
        }
    }

    #[test]
    fn a_report_shows_a_long_line_where_it_starts_to_differ() {
        let long = |tail: &str| format!("proof 4096 {}{tail}", "ab".repeat(2000));
        let divergence = Divergence {
            step: 0,
            parts: vec![Part::Proof, Part::Observation],
            reference: vec!["cost same".to_string(), long("0123")],
            candidate: vec![
                "cost same".to_string(),
                long("4567"),
                "# only here".to_string(),
            ],
        };
        let report = divergence.report(&case(vec![Op::Commit]), "one", "two");
        assert_eq!(
            report[..2],
            [
                "step 0 of 1: the engines differ in proof, observation",
                "  operation: Commit"
            ]
        );
        assert_eq!(report.len(), 6);
        assert!(report[2].starts_with("  one: proof …(from character 3911) "));
        assert!(report[2].ends_with("ab0123") && report[3].ends_with("ab4567"));
        assert!(report[2].chars().count() < LINE_CHARS + 60);
        assert_eq!(report[4..], ["  one: (nothing)", "  two: # only here"]);
        // A line cut before its end says so, and a short one is whole.
        let cut = around(&format!("root {}", "0".repeat(1000)), 0);
        assert!(cut.starts_with("root 000") && cut.ends_with('…'));
        assert_eq!(around("short", 2), "short");
        // A step past the case, which a report of another case would name.
        let stray = Divergence {
            step: 5,
            ..divergence
        };
        assert!(!stray.report(&case(Vec::new()), "one", "two")[1].contains("operation"));
    }

    #[test]
    fn tallies_add_up() {
        let mut tally = Tally {
            steps: 1,
            failed: 2,
            proofs: 3,
            walks: 4,
            records: 5,
            cut_short: 6,
        };
        tally.add(&tally.clone());
        assert_eq!(
            tally,
            Tally {
                steps: 2,
                failed: 4,
                proofs: 6,
                walks: 8,
                records: 10,
                cut_short: 12,
            }
        );
    }

    #[test]
    fn a_panic_outside_a_step_is_printed_as_ever() {
        assert_eq!(catching(|| 7), Ok(7));
        let caught: Result<(), String> = catching(|| panic!("caught {}", 1));
        assert!(caught.expect_err("a panic").contains("caught 1"));
        // The hook hands a panic nobody catches to the hook that was there.
        let outside = std::thread::spawn(|| panic!("not caught")).join();
        assert!(outside.is_err());
    }
}
