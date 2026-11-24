//! One case on two engines, compared after every step.
//!
//! [`run`] opens a fresh database of each engine, applies the operations of
//! a case to both through the corpus's own [`Runner`], and stops at the
//! first step the two answer differently. What is compared is listed in
//! the [module above](super). [`Stored`] says how often the records the two
//! databases hold are compared ([`super::state`]).
//!
//! An engine that panics, or cannot be observed, where the other answers
//! has diverged, and so have two that panic with different messages. Where
//! both panic with one message, or neither can be observed, they did the
//! same and the case ends there.

#![expect(
    clippy::result_large_err,
    reason = "a database is opened with GroveDB's own error, whatever its size"
)]

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    panic::{self, AssertUnwindSafe},
    path::Path,
    sync::Once,
};

use grovedb::GroveDb;

use super::{
    case::Case,
    close::{self, Closes},
    families::{Counts, Family, Known},
    state::{first_difference, pending_drop_paths, stored, Listing, Place, Record},
};
use crate::{
    golden::{
        run::{Observed, Runner},
        step::{Outcome, Proof, Step},
        trace::{one_line, shortened},
        Error,
    },
    workload::{Op, StreamEngine, TransactionOp},
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

    /// Opens a database in `dir`. An engine that panics here has not
    /// answered differently at any step: the run cannot start.
    fn open(&self, dir: &Path) -> Result<GroveDb<E>, Error> {
        let what = format!("opening a database of {}", self.name);
        match catching(|| (self.open)(dir)) {
            Ok(Ok(db)) => Ok(db),
            Ok(Err(error)) => Err(Error::engine(&what, &error)),
            Err(panic) => Err(Error::engine(&what, &format!("{PANICKED}{panic}"))),
        }
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
    /// last operation. A read is a read of committed state, of the block or
    /// of the snapshot.
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
    /// What the candidate's own check of its store found once the case
    /// had ended ([`super::close`]).
    Check,
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
            Part::Check => "close check",
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

/// How the line begins that stands for an engine that panicked.
const PANICKED: &str = "panicked: ";

/// How the line begins that stands for an engine that could not be
/// observed.
const NOT_OBSERVED: &str = "not observed: ";

/// How an engine ended a step it did not answer, by the line that stands
/// for it. None for an engine that answered.
fn ended(lines: &[String]) -> Option<&'static str> {
    let first = lines.first()?;
    [PANICKED, NOT_OBSERVED]
        .into_iter()
        .find(|marker| first.starts_with(marker))
}

impl Divergence {
    /// Whether `other`, found on another stream, is this divergence again:
    /// it differs in no part this one does not differ in, and where an
    /// engine panicked or could not be observed, the same engine did.
    pub fn shows_again_as(&self, other: &Divergence) -> bool {
        other.parts.iter().all(|part| self.parts.contains(part))
            && ended(&self.reference) == ended(&other.reference)
            && ended(&self.candidate) == ended(&other.candidate)
    }

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
    /// What was met of the open differences that are named
    /// ([`super::families`]).
    pub families: Counts,
    /// What the checks of the candidate's store at the ends of the cases
    /// came to ([`super::close`]).
    pub closes: Closes,
    /// The cases that met nothing of a named open difference and were
    /// compared to their end. A fuzzing run counts them, and the two
    /// below, of the cases it did not interrupt.
    pub clean: u64,
    /// The cases that ended at an answer of a named open difference.
    pub ended: u64,
    /// The cases in which something of a named open difference was
    /// passed over: stored records that differ, or an answer at which
    /// no case ends.
    pub passed: u64,
    /// What the reference's own check answered at the end of the cases
    /// that ended at a write which the reference alone took
    /// ([`Family::TakenBelowPlainDelete`]): how often it found nothing,
    /// reported places, and failed.
    pub reference_checks: [u64; 3],
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
        self.families.add(&other.families);
        self.closes.add(&other.closes);
        self.clean += other.clean;
        self.ended += other.ended;
        self.passed += other.passed;
        for (sum, more) in self.reference_checks.iter_mut().zip(other.reference_checks) {
            *sum += more;
        }
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
    /// both engines panicked, or neither could be observed.
    pub cut_short: Option<String>,
    /// Whether the caller interrupted the run before the last operation.
    pub interrupted: bool,
    /// The named open difference at an answer of which the case ended
    /// before its last operation. It is no divergence.
    pub ended_at: Option<Family>,
    /// The answers the engines gave differently where the case ended at
    /// a named open difference.
    pub ended_with: Option<Divergence>,
    /// The steps that both engines answered alike with an error.
    pub failed_steps: Vec<usize>,
    /// The answers the engines gave differently in the cost of a batch
    /// that removes or replaces a tree over records RocksDB kept, each of
    /// which was passed over ([`Family::KeptRecordsCost`]).
    pub passed_costs: Vec<Divergence>,
    /// The answers of a write below a plain delete that both engines
    /// refused, differently in the cost or the error, each of which was
    /// passed over ([`Family::RefusedBelowPlainDelete`]).
    pub passed_refusals: Vec<Divergence>,
    /// What the reference's own check (`verify_grovedb`) answered in a
    /// case that ended at a write which the reference alone took
    /// ([`Family::TakenBelowPlainDelete`]): asked in the reference's
    /// block right after that write, which the block holds, and before
    /// the block is dropped with the case. The case is not held to it.
    pub reference_check: Option<ReferenceCheck>,
}

/// What the reference's own check answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceCheck {
    /// It found nothing.
    Nothing,
    /// It reported places.
    Places,
    /// It failed.
    Failed,
}

impl ReferenceCheck {
    /// What a report says of it.
    pub fn said(self) -> &'static str {
        match self {
            ReferenceCheck::Nothing => "found nothing",
            ReferenceCheck::Places => "reported places",
            ReferenceCheck::Failed => "failed",
        }
    }
}

/// What an engine did when it was asked for something: to apply an
/// operation, or to list what it stores.
enum Seen<T> {
    Answered(T),
    Unobserved {
        /// What could not be done.
        what: String,
        /// The engine's message.
        message: String,
    },
    Panicked(String),
}

impl<T> Seen<T> {
    /// The lines that stand for this in a report.
    fn lines(self, answered: impl FnOnce(T) -> Vec<String>) -> Vec<String> {
        match self {
            Seen::Answered(answer) => answered(answer),
            Seen::Unobserved { what, message } => {
                vec![format!("{NOT_OBSERVED}{what}: {message}")]
            }
            Seen::Panicked(panic) => vec![format!("{PANICKED}{panic}")],
        }
    }
}

/// What two engines did with the same request.
enum Both<T> {
    /// Both answered.
    Answered(T, T),
    /// Neither answered, and they failed alike: why.
    Alike(String),
    /// One answered and the other did not, or they failed differently: the
    /// part they differ in and the lines that stand for each.
    Differ(Part, Vec<String>, Vec<String>),
}

/// Sets what the reference did next to what the candidate did.
///
/// Two panics are the same when their messages are, which name the place
/// that panicked. Two failures to observe are the same when the same thing
/// could not be done: the message is an engine's own text.
fn both<T>(was: Seen<T>, is: Seen<T>, answered: impl Fn(T) -> Vec<String>) -> Both<T> {
    match (was, is) {
        (Seen::Answered(was), Seen::Answered(is)) => Both::Answered(was, is),
        (Seen::Panicked(was), Seen::Panicked(is)) if was == is => {
            Both::Alike(format!("both engines panicked: {was}"))
        }
        (
            Seen::Unobserved { what, message },
            Seen::Unobserved {
                what: candidate_what,
                ..
            },
        ) if what == candidate_what => Both::Alike(format!(
            "neither engine could be observed {what}: {message}"
        )),
        (was, is) => {
            let panicked = matches!(was, Seen::Panicked(_)) || matches!(is, Seen::Panicked(_));
            let part = if panicked {
                Part::Panic
            } else {
                Part::Observation
            };
            Both::Differ(part, was.lines(&answered), is.lines(&answered))
        }
    }
}

thread_local! {
    static CATCHING: Cell<bool> = const { Cell::new(false) };
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Runs `body` and returns the message of its panic if it panics. The
/// panic is not printed: a mutated stream may ask for what an engine
/// panics on, and the report says so where it matters.
pub(super) fn catching<T>(body: impl FnOnce() -> T) -> Result<T, String> {
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
    let nested = CATCHING.replace(true);
    let result = panic::catch_unwind(AssertUnwindSafe(body));
    CATCHING.set(nested);
    result.map_err(|_| {
        PANIC
            .take()
            .map_or("a panic".to_string(), |message| one_line(&message))
    })
}

/// Applies `op` and observes the step.
fn observe<E: StreamEngine>(runner: &mut Runner<'_, E>, op: &Op) -> Seen<Box<Observed>> {
    match catching(|| runner.step(op)) {
        Ok(Ok(observed)) => Seen::Answered(Box::new(observed)),
        Ok(Err(Error::Engine { what, message })) => Seen::Unobserved { what, message },
        Ok(Err(error)) => Seen::Unobserved {
            what: "at this step".to_string(),
            message: error.to_string(),
        },
        Err(panic) => Seen::Panicked(panic),
    }
}

/// Lists the records a database stores, as the open transaction of its
/// runner sees them.
fn walk<'db, E: StreamEngine>(
    db: &'db GroveDb<E>,
    runner: &Runner<'db, E>,
    case: &Case,
    before: &BTreeSet<Place>,
) -> Seen<Listing> {
    let listed = catching(|| {
        stored(
            db,
            runner.transactions().block(),
            case.grove_version,
            before,
        )
    });
    match listed {
        Ok(Ok(listing)) => Seen::Answered(listing),
        Ok(Err(message)) => Seen::Unobserved {
            what: "listing what is stored".to_string(),
            message,
        },
        Err(panic) => Seen::Panicked(panic),
    }
}

/// The parts in which two steps differ.
pub fn differences(reference: &Step, candidate: &Step) -> Vec<Part> {
    // Every field by name, so that a field a step gains is compared or
    // does not compile.
    let Step {
        cost,
        outcome,
        root_hash,
        proof,
    } = reference;
    let mut parts = Vec::new();
    if *cost != candidate.cost {
        parts.push(Part::Cost);
    }
    match (outcome, &candidate.outcome) {
        (Outcome::Returned(was), Outcome::Returned(is)) if was == is => {}
        (Outcome::Returned(_), Outcome::Returned(_)) => parts.push(Part::Result),
        (Outcome::Failed(was), Outcome::Failed(is)) if was == is => {}
        _ => parts.push(Part::Error),
    }
    if *root_hash != candidate.root_hash {
        parts.push(Part::RootHash);
    }
    match (proof, &candidate.proof) {
        (Some(Proof { cost, proved }), Some(is)) => {
            if *cost != is.cost {
                parts.push(Part::ProofCost);
            }
            if *proved != is.proved {
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

/// Whether an operation only reads, whatever it reads through: what is
/// stored is what it was before.
fn reads(op: &Op) -> bool {
    matches!(
        op,
        Op::Read(_) | Op::Transaction(TransactionOp::SnapshotRead(_) | TransactionOp::BlockRead(_))
    )
}

/// Runs `case` on a fresh database of each engine, under `scratch`, and
/// compares them after every step. `interrupted` is asked before each
/// operation and ends the run when it says so.
///
/// Where the candidate is LigninDB's engine, a run that found no
/// divergence ends with the candidate's own check of its store
/// ([`super::close`]), wherever it ended: at the last operation of the
/// case, at a named open difference, cut short, interrupted, or with no
/// operation at all. What the check finds is a divergence at the last
/// step that was compared, or at step 0 where none was.
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
    let grove_version = case.grove_version;
    close::opened(&candidate_db, grove_version);
    let mut run = compared(
        case,
        &reference_db,
        &candidate_db,
        stored_check,
        interrupted,
    )?;
    if run.divergence.is_none() {
        let step = match &run.ended_with {
            Some(ended) => ended.step,
            None => (run.tally.steps as usize).saturating_sub(1),
        };
        let checked = catching(|| close::check(&reference_db, &candidate_db, grove_version));
        let wanting = match checked {
            Ok(Ok(closes)) => {
                run.tally.closes = closes;
                None
            }
            Ok(Err(wanting)) => Some((wanting.reference, wanting.candidate)),
            Err(panic) => {
                let expected = "no finding is expected of the candidate's store".to_string();
                Some((vec![expected], vec![format!("{PANICKED}{panic}")]))
            }
        };
        if let Some((reference, candidate)) = wanting {
            run.divergence = Some(Divergence {
                step,
                parts: vec![Part::Check],
                reference,
                candidate,
            });
        }
    }
    // The close of a debug build checks the candidate's store again, and
    // panics on what the check above has found: the divergence stands.
    let found_at_the_close = |divergence: &Divergence| divergence.parts == [Part::Check];
    if run.divergence.as_ref().is_some_and(found_at_the_close) {
        let _ = catching(move || drop(candidate_db));
    }
    Ok(run)
}

/// The steps of [`run`], on the two databases it opened.
fn compared<A: StreamEngine, B: StreamEngine>(
    case: &Case,
    reference_db: &GroveDb<A>,
    candidate_db: &GroveDb<B>,
    stored_check: Stored,
    interrupted: &dyn Fn() -> bool,
) -> Result<Run, Error> {
    let mut reference_runner = Runner::new(reference_db, case.grove_version)?;
    let mut candidate_runner = Runner::new(candidate_db, case.grove_version)?;

    let mut tally = Tally::default();
    let mut writes = 0u64;
    // The subtrees the last walk handed on: the ones it reached, and the
    // ones a pending drop kept it from looking at (`super::state`).
    let mut places: BTreeSet<Place> = BTreeSet::new();
    // The places of the named open differences that the case has made.
    let mut known = Known::default();
    let failed_steps = std::cell::RefCell::new(Vec::new());
    let passed_costs = std::cell::RefCell::new(Vec::new());
    let passed_refusals = std::cell::RefCell::new(Vec::new());
    let ended = |tally, divergence, cut_short, interrupted| {
        Ok(Run {
            tally,
            divergence,
            cut_short,
            interrupted,
            ended_at: None,
            ended_with: None,
            failed_steps: failed_steps.take(),
            passed_costs: passed_costs.take(),
            passed_refusals: passed_refusals.take(),
            reference_check: None,
        })
    };
    let diverged = |tally, step, parts, reference, candidate| {
        let divergence = Divergence {
            step,
            parts,
            reference,
            candidate,
        };
        ended(tally, Some(divergence), None, false)
    };
    let cut_short = |mut tally: Tally, step: usize, why: String| {
        tally.cut_short = 1;
        ended(tally, None, Some(format!("at step {step}, {why}")), false)
    };
    // The root hash of each engine after the last operation that has
    // one.
    let mut roots: (Option<[u8; 32]>, Option<[u8; 32]>) = (None, None);
    for (step, op) in case.ops.iter().enumerate() {
        tally.families = known.counts();
        if interrupted() {
            return ended(tally, None, None, true);
        }
        // Asked of the reference before the operation: a read of its
        // block's state, which changes nothing.
        let (
            over,
            over_indexed,
            under_an_insert_only,
            plain_deletes,
            root_delete,
            kept,
            no_element,
            below_plain_deletes,
        ) = catching(|| {
            let (db, version) = (reference_db, case.grove_version);
            let block = reference_runner.transactions().block();
            let candidate = (candidate_db, candidate_runner.transactions().block());
            let kept = known.kept_below_a_removed_tree((db, block), candidate, op, version);
            let no_element =
                known.named_by_a_kept_record_alone((db, block), candidate, op, version);
            let plain_deletes = Known::trees_under_a_plain_delete(db, block, op, version);
            let below_plain_deletes = Known::records_below(db, block, &plain_deletes, version);
            (
                Known::trees_under(db, block, op, version),
                Known::indexed_under(db, block, op, version),
                Known::indexed_under_an_insert_only(db, block, op, version),
                plain_deletes,
                Known::trees_under_a_root_delete(db, block, op, version),
                kept,
                no_element,
                below_plain_deletes,
            )
        })
        .unwrap_or_default();
        let was = observe(&mut reference_runner, op);
        // A put of an indexed tree over an indexed tree that the reference
        // took is counted, whatever the candidate did.
        if matches!(&was, Seen::Answered(observed) if !observed.step.failed()) {
            known.came_to_indexed_over_indexed(&over_indexed);
            known.came_to_insert_only_over_indexed(&under_an_insert_only);
            tally.families = known.counts();
        }
        let is = observe(&mut candidate_runner, op);
        tally.steps += 1;
        let (was, is) = match both(was, is, |observed| lines(&observed)) {
            Both::Answered(was, is) => (was, is),
            Both::Alike(why) => return cut_short(tally, step, why),
            Both::Differ(part, was, is) => return diverged(tally, step, vec![part], was, is),
        };
        let parts = differences(&was.step, &is.step);
        // Whether an answer that differs is passed over at this step.
        let mut passed_over = false;
        if !parts.is_empty() {
            // An answer that differs at an operation which names a place
            // of a named open difference, or in which the candidate
            // refuses an indexed tree over an indexed tree, ends the
            // case, and is none of the run's.
            let (reference, candidate) = (&was.step.outcome, &is.step.outcome);
            let refusal = Known::family_of_refusal(&over_indexed, reference, candidate);
            let refusal = refusal.or_else(|| {
                let over = &under_an_insert_only;
                Known::family_of_insert_only_refusal(over, reference, candidate, &is.remarks)
            });
            let taken = !was.step.failed();
            // The engines differ in the proof of a query and in nothing
            // else, and the candidate's proof holds: the open difference
            // of a dropped tree, through a proof.
            let through_a_proof = || {
                let (reference, candidate) = (&was.step, &is.step);
                let held = known.through_a_proof(op, reference, candidate, case.grove_version);
                held.then_some(Family::DroppedTreeProof)
            };
            // The candidate refuses a write of the root Merk whose
            // root-key record a delete of the case left behind.
            let root_key =
                || known.family_of_root_key_refusal(op, reference, candidate, &is.remarks);
            // The reference was charged for reading the records it kept
            // below a tree that the batch removes.
            let cost = || Known::family_of_cost(&kept, &was.step, &is.step, &parts);
            // Both engines refused a write below a plain delete, and
            // each has the root hash it had before.
            let unchanged =
                roots.0.is_some() && was.step.root_hash == roots.0 && is.step.root_hash == roots.1;
            let refused_write =
                || known.family_of_refused_write(op, reference, candidate, &parts, unchanged);
            // The reference took a write into a subtree that it opened by
            // a record it kept.
            let taken_write = || {
                let said = &is.remarks;
                Known::family_of_taken_write(op, &no_element, reference, candidate, said)
            };
            let family = refusal
                .or_else(root_key)
                .or_else(cost)
                .or_else(refused_write)
                .or_else(taken_write)
                .or_else(|| known.family_of_answer(op, taken, &parts))
                .or_else(through_a_proof);
            let Some(family) = family else {
                return diverged(tally, step, parts, lines(&was), lines(&is));
            };
            if family.ends_a_case() {
                known.ended_at(family, case.seed);
                tally.families = known.counts();
                // Where the reference alone took the write, what its own
                // check answers with that write in its block is kept,
                // and the case is not held to it.
                let reference_check = (family == Family::TakenBelowPlainDelete).then(|| {
                    let block = reference_runner.transactions().block();
                    let version = case.grove_version;
                    let checked =
                        catching(|| reference_db.verify_grovedb(Some(block), true, false, version));
                    match checked {
                        Ok(Ok(issues)) if issues.is_empty() => ReferenceCheck::Nothing,
                        Ok(Ok(_)) => ReferenceCheck::Places,
                        Ok(Err(_)) | Err(_) => ReferenceCheck::Failed,
                    }
                });
                let ended_with = Divergence {
                    step,
                    parts,
                    reference: lines(&was),
                    candidate: lines(&is),
                };
                return Ok(Run {
                    tally,
                    divergence: None,
                    cut_short: None,
                    interrupted: false,
                    ended_at: Some(family),
                    ended_with: Some(ended_with),
                    failed_steps: failed_steps.take(),
                    passed_costs: passed_costs.take(),
                    passed_refusals: passed_refusals.take(),
                    reference_check,
                });
            }
            // A read below a plain delete, or a batch that differs in
            // what it was charged for reading there: neither left the
            // engines in different states, so it is counted and the case
            // goes on, as after an operation both answered alike.
            known.passed_an_answer(family, case.seed);
            let passed = || Divergence {
                step,
                parts: parts.clone(),
                reference: lines(&was),
                candidate: lines(&is),
            };
            match family {
                Family::KeptRecordsCost => passed_costs.borrow_mut().push(passed()),
                Family::RefusedBelowPlainDelete => passed_refusals.borrow_mut().push(passed()),
                _ => {}
            }
            passed_over = true;
            tally.families = known.counts();
        }
        // The root hash each engine has now, for the next operation: a
        // read has none, and leaves it.
        let before = roots;
        roots = (
            was.step.root_hash.or(roots.0),
            is.step.root_hash.or(roots.1),
        );
        // A record that a plain delete left is no longer one where the
        // operation names its key, unless the reference refused the
        // operation and has the root hash it had.
        let root_changed = was.step.root_hash.is_some() && roots.0 != before.0;
        known.forgets(op, !was.step.failed(), root_changed);
        if !was.step.failed() {
            known.wrote(op, root_changed);
            // The trees a plain delete of the batch removed of which the
            // reference still holds a record, as its block sees it now.
            let left = catching(|| {
                let block = reference_runner.transactions().block();
                Known::left_a_record(reference_db, block, plain_deletes)
            });
            known.took(op, over, left.unwrap_or_default(), root_delete);
            known.left_records(below_plain_deletes);
        }
        // The trees whose flat drop has a pending record now, as the
        // reference's block sees it: the places of the family of a
        // dropped tree from here on. It is read once both engines
        // answered the operation alike, so the answers to an operation
        // are held against the records as they were before it. Where the
        // reference cannot be read, none is pending.
        if known.dropped_flat() {
            let pending = catching(|| {
                let block = reference_runner.transactions().block();
                pending_drop_paths(reference_db, block)
            });
            known.pending(pending.ok().and_then(Result::ok).unwrap_or_default());
            // Those of them at which the reference reads a tree again.
            let standing = catching(|| {
                let block = reference_runner.transactions().block();
                let pending = known.pending_paths();
                Known::trees_at(reference_db, block, pending, case.grove_version)
            });
            known.made_again(standing.unwrap_or_default());
        }
        tally.families = known.counts();
        // An answer that was passed over is no common failure, though
        // both engines may have failed: they differ in it.
        let failed_alike = was.step.failed() && !passed_over;
        if failed_alike {
            failed_steps.borrow_mut().push(step);
        }
        tally.failed += u64::from(failed_alike);
        tally.proofs += u64::from(was.step.proof.is_some());

        let last = step + 1 == case.ops.len();
        let write = !reads(op);
        writes += u64::from(write);
        let due = match stored_check {
            Stored::Never => false,
            Stored::AtTheEnd => last,
            Stored::Every(every) => last || (write && writes.is_multiple_of(every.max(1))),
        };
        if !due {
            continue;
        }
        let before = std::mem::take(&mut places);
        let was = walk(reference_db, &reference_runner, case, &before);
        let is = walk(candidate_db, &candidate_runner, case, &before);
        let listed = |listing: Listing| vec![format!("{} records", listing.records.len())];
        let (was, is) = match both(was, is, listed) {
            Both::Answered(was, is) => (was, is),
            Both::Alike(why) => return cut_short(tally, step, why),
            Both::Differ(part, was, is) => return diverged(tally, step, vec![part], was, is),
        };
        tally.walks += 1;
        tally.records += was.records.len() as u64;
        // The records of a subtree of a named open difference are counted
        // where they differ, and not compared.
        let (compared_was, compared_is) = known.passed_over(case.seed, &was.records, &is.records);
        tally.families = known.counts();
        if let Some((was, is)) = first_difference(&compared_was, &compared_is) {
            let text = |record: Option<&Record>| {
                vec![record.map_or("no further record".to_string(), Record::text)]
            };
            return diverged(tally, step, vec![Part::Stored], text(was), text(is));
        }
        places = was.handed_on();
    }
    tally.families = known.counts();
    ended(tally, None, None, false)
}

/// What the tests of the fuzzer share.
#[cfg(test)]
pub(crate) mod testing {
    use grovedb::{GroveDb, RocksDbEngine};
    use grovedb_version::version::GroveVersion;

    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use super::Side;
    use crate::{
        diff::{
            engines::{Fault, StandIn},
            shrink::Budget,
        },
        golden::run::Runner,
        workload::Op,
    };

    /// The reference.
    pub(crate) fn rocksdb() -> Side<RocksDbEngine> {
        Side::of("rocksdb")
    }

    /// The stand-in as it is.
    pub(crate) fn stand_in() -> Side<StandIn> {
        Side::of("stand-in")
    }

    /// A stand-in whose database holds, from the start, a record of the
    /// auxiliary keyspace that no stream writes: an engine that stores
    /// what no hash covers differently.
    pub(crate) fn stand_in_with_a_stray_record() -> Side<StandIn> {
        Side::opened_by("seeded", |dir| {
            let db = GroveDb::<StandIn>::open_with_engine(dir)?;
            db.put_aux(b"left", b"behind", None, None).unwrap()?;
            Ok(db)
        })
    }

    /// A stand-in with `fault` that opens so many databases and no more.
    pub(crate) fn stand_in_that_opens(times: u64, fault: Fault) -> Side<StandIn> {
        let opened = AtomicU64::new(0);
        Side::opened_by("worn out", move |dir| {
            if opened.fetch_add(1, Ordering::Relaxed) >= times {
                return Err(grovedb::Error::InvalidInput("no database any more"));
            }
            Ok(GroveDb::from_engine(StandIn::open_with_fault(dir, fault)?))
        })
    }

    /// The budget of a shrinking that is to end by itself, however slow the
    /// disk of the databases is while the tests run.
    pub(crate) fn unhurried() -> Budget {
        Budget {
            time: Duration::MAX,
            ..Budget::default()
        }
    }

    /// A directory for the databases of a test, in the temporary directory:
    /// whoever runs the tests says with `TMPDIR` where that is. A shrinking
    /// runs its stream dozens of times, and on a busy disk each run of them
    /// takes a minute.
    pub(crate) fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("a directory")
    }

    /// The bytes the last of `ops` adds, on a fresh database.
    pub(crate) fn added_by_the_last_of(ops: &[Op]) -> u32 {
        let dir = scratch();
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
}

#[cfg(test)]
mod tests {
    use grovedb::Element;
    use grovedb_costs::OperationCost;
    use grovedb_version::version::GroveVersion;

    use super::{testing::*, *};
    use crate::{
        diff::engines::{Fault, StandIn},
        golden::{
            record::on_a_deep_stack,
            step::{Proved, Value, Verification},
        },
        workload::{AppendEntry, Query, QueryItem, Read, Write},
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
        let scratch = scratch();
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

    /// The two inserts every test of a planted fault makes, and what the
    /// second adds.
    fn two_inserts() -> (Vec<Op>, u32) {
        let ops = vec![insert(b"a", vec![1; 8]), insert(b"b", vec![2; 60])];
        let added = added_by_the_last_of(&ops);
        assert_ne!(added, added_by_the_last_of(&ops[..1]));
        (ops, added)
    }

    #[test]
    fn two_engines_that_answer_the_same_run_to_the_end() {
        let case = case(vec![
            insert(b"a", vec![1; 8]),
            insert(b"b", vec![2; 60]),
            Op::Commit,
            all(),
            Op::Transaction(TransactionOp::BlockRead(Read::GetAux { key: vec![1] })),
            Op::Write(Write::Delete {
                path: Vec::new(),
                key: b"missing".to_vec(),
            }),
            Op::Transaction(TransactionOp::Rollback),
        ]);
        let with = |stored_check| compared(&case, &rocksdb(), &stand_in(), stored_check);
        // A walk after the second, the fourth and the last operation that
        // is not a read, of the root key and the two nodes each time.
        let run = with(Stored::Every(2));
        assert_eq!(run.divergence, None);
        assert_eq!(run.cut_short, None);
        assert!(!run.interrupted);
        assert_eq!(
            run.tally,
            Tally {
                steps: 7,
                failed: 1,
                proofs: 1,
                walks: 3,
                records: 3 + 3 + 3,
                cut_short: 0,
                families: Counts::default(),
                closes: Closes::default(),
                ..Tally::default()
            }
        );
        let once = with(Stored::AtTheEnd);
        assert_eq!((once.tally.walks, once.tally.records), (1, 3));
        assert_eq!(with(Stored::Never).tally.walks, 0);
    }

    #[test]
    fn a_byte_too_many_in_a_cost_is_a_divergence_at_its_step() {
        let (mut ops, when_added) = two_inserts();
        ops.push(Op::Commit);
        let case = case(ops);
        let reference = rocksdb();
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
        let (mut ops, when_added) = two_inserts();
        ops.push(Op::Commit);
        let case = case(ops);
        let fault = Fault::Panic { when_added };
        let run = compared(&case, &rocksdb(), &StandIn::side(fault), Stored::Never);
        let divergence = run.divergence.expect("one engine panicked");
        assert_eq!(
            (divergence.step, &divergence.parts),
            (1, &vec![Part::Panic])
        );
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
        assert!(!both.interrupted);
        let why = both.cut_short.expect("both engines panicked");
        assert!(
            why.starts_with("at step 1, both engines panicked: "),
            "{why}"
        );
        assert_eq!((both.tally.steps, both.tally.cut_short), (2, 1));
    }

    #[test]
    fn a_run_stops_when_it_is_interrupted() {
        let case = case(vec![insert(b"a", vec![1]), Op::Commit]);
        let scratch = scratch();
        let asked = Cell::new(0);
        let run = run(
            &case,
            &rocksdb(),
            &stand_in(),
            scratch.path(),
            Stored::AtTheEnd,
            &|| {
                asked.set(asked.get() + 1);
                asked.get() > 1
            },
        )
        .expect("the case runs");
        assert!(run.interrupted);
        assert_eq!((run.divergence, run.cut_short), (None, None));
        assert_eq!(
            run.tally,
            Tally {
                steps: 1,
                ..Tally::default()
            }
        );
    }

    #[test]
    fn records_one_engine_holds_differently_are_a_divergence() {
        let seeded = stand_in_with_a_stray_record();
        let case = case(vec![
            insert(b"a", vec![1]),
            insert(b"b", vec![2]),
            Op::Commit,
        ]);
        for (stored_check, step) in [(Stored::Every(1), 0), (Stored::AtTheEnd, 2)] {
            let run = compared(&case, &rocksdb(), &seeded, stored_check);
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
        let unseen = compared(&case, &rocksdb(), &seeded, Stored::Never);
        assert_eq!(unseen.divergence, None);
    }

    /// A tree that holds a leaf, committed.
    fn a_filled_log() -> Vec<Op> {
        vec![
            Op::Write(Write::Insert {
                path: Vec::new(),
                key: b"log".to_vec(),
                element: Element::empty_mmr_tree(),
            }),
            Op::Write(Write::Append {
                path: vec![b"log".to_vec()],
                entry: AppendEntry::Mmr(vec![9; 40]),
            }),
            Op::Commit,
        ]
    }

    fn drop_the_log() -> Op {
        Op::Write(Write::DropFlat {
            path: Vec::new(),
            key: b"log".to_vec(),
        })
    }

    /// A subtree that is dropped flat keeps its records until the drop is
    /// flushed, on the engines of today. They are not listed as left
    /// behind while the drop is pending: an engine may hide them at the
    /// drop. A subtree that is deleted is looked into once more, and holds
    /// nothing.
    #[test]
    fn what_a_flat_drop_leaves_until_it_is_flushed_is_not_compared() {
        let records = |ops: Vec<Op>| {
            let run = compared(&case(ops), &rocksdb(), &stand_in(), Stored::Every(1));
            assert_eq!((run.divergence, run.cut_short), (None, None));
            run.tally.records
        };
        let empty_log = records(a_filled_log()[..1].to_vec());
        assert_eq!(empty_log, 2, "the root key and the node of the tree");
        let filled = records(a_filled_log()[..2].to_vec()) - empty_log;
        assert!(filled > empty_log, "the tree holds its leaf");
        let committed = empty_log + filled + filled;
        assert_eq!(records(a_filled_log()), committed);

        let mut dropped = a_filled_log();
        dropped.extend([drop_the_log(), Op::Commit, Op::FlushPendingDrops]);
        // The root tree is empty from the drop on. The walk after the drop
        // lists the record the drop staged, the one after its commit that
        // record and the drop the engine reports, and the one after the
        // flush nothing.
        assert_eq!(records(dropped), committed + 1 + 2);

        let mut deleted = a_filled_log();
        deleted.extend([
            Op::Write(Write::Delete {
                path: Vec::new(),
                key: b"log".to_vec(),
            }),
            Op::Commit,
        ]);
        assert_eq!(records(deleted), committed);
    }

    /// An engine whose flush frees nothing answers every operation as the
    /// reference does. The walk after the flush looks at the subtree the
    /// drop was to free, and finds what the engine kept.
    #[test]
    fn what_a_flush_did_not_free_is_a_divergence() {
        let keeps = StandIn::side(Fault::Unreclaimed);
        let mut ops = a_filled_log();
        ops.extend([drop_the_log(), Op::Commit, Op::FlushPendingDrops]);
        let flush = ops.len() - 1;

        let run = compared(&case(ops.clone()), &rocksdb(), &keeps, Stored::Every(1));
        let divergence = run.divergence.expect("the records that were kept");
        assert_eq!(
            (divergence.step, &divergence.parts),
            (flush, &vec![Part::Stored])
        );
        assert_eq!(divergence.reference, ["no further record"]);
        assert!(
            divergence.candidate[0].starts_with("left behind: flat /6c6f67 key "),
            "{:?}",
            divergence.candidate
        );

        // A drop that is rolled back frees nothing and excuses nothing.
        // The tree is deleted after it, and both engines clear it.
        let mut rolled_back = a_filled_log();
        rolled_back.extend([
            drop_the_log(),
            Op::Transaction(TransactionOp::Rollback),
            Op::Write(Write::Delete {
                path: Vec::new(),
                key: b"log".to_vec(),
            }),
            Op::Commit,
            Op::FlushPendingDrops,
        ]);
        let run = compared(&case(rolled_back), &rocksdb(), &keeps, Stored::Every(1));
        assert_eq!((run.divergence, run.cut_short), (None, None));
        assert_eq!(run.tally.failed, 0);

        // Without a walk between the drop and the flush that was told of
        // the subtree, nothing is seen: the walks hand the places on.
        let unseen = compared(&case(ops), &rocksdb(), &keeps, Stored::AtTheEnd);
        assert_eq!(unseen.divergence, None);
    }

    #[test]
    fn a_database_that_does_not_open_is_an_error() {
        let closed = Side::<StandIn>::opened_by("closed", |_| {
            Err(grovedb::Error::InvalidInput("no database here"))
        });
        let scratch = scratch();
        let refused = |candidate: &Side<StandIn>| {
            run(
                &case(Vec::new()),
                &rocksdb(),
                candidate,
                scratch.path(),
                Stored::Never,
                &|| false,
            )
            .expect_err("the candidate does not open")
            .to_string()
        };
        assert_eq!(
            refused(&closed),
            "the engine failed opening a database of closed: invalid input: no database here"
        );
        let panics = Side::<StandIn>::opened_by("broken", |_| panic!("no database at all"));
        let refused = refused(&panics);
        assert!(
            refused.starts_with("the engine failed opening a database of broken: panicked: ")
                && refused.ends_with("no database at all"),
            "{refused}"
        );
        let nowhere = run(
            &case(Vec::new()),
            &rocksdb(),
            &stand_in(),
            &scratch.path().join("missing"),
            Stored::Never,
            &|| false,
        )
        .expect_err("there is no scratch directory");
        assert!(matches!(nowhere, Error::Io { .. }), "{nowhere}");
    }

    /// Two engines that did not both answer: alike, or not.
    #[test]
    fn engines_that_do_not_answer_are_compared_by_how_they_fail() {
        let answered = || Seen::Answered(7);
        let panicked = |message: &str| Seen::<i32>::Panicked(message.to_string());
        let unobserved = |what: &str, message: &str| Seen::<i32>::Unobserved {
            what: what.to_string(),
            message: message.to_string(),
        };
        let said = |answer: i32| vec![format!("answer {answer}")];
        let settled = |was, is| match both(was, is, said) {
            Both::Answered(was, is) => format!("answered {was} {is}"),
            Both::Alike(why) => format!("alike: {why}"),
            Both::Differ(part, was, is) => format!("{}: {was:?} {is:?}", part.name()),
        };
        assert_eq!(settled(answered(), answered()), "answered 7 7");
        assert_eq!(
            settled(panicked("at a"), panicked("at a")),
            "alike: both engines panicked: at a"
        );
        assert_eq!(
            settled(panicked("at a"), panicked("at b")),
            r#"panic: ["panicked: at a"] ["panicked: at b"]"#
        );
        assert_eq!(
            settled(answered(), panicked("at b")),
            r#"panic: ["answer 7"] ["panicked: at b"]"#
        );
        assert_eq!(
            settled(unobserved("reading", "x"), panicked("at b")),
            r#"panic: ["not observed: reading: x"] ["panicked: at b"]"#
        );
        // The message is an engine's own text. What could not be done is
        // the same thing on both.
        assert_eq!(
            settled(unobserved("reading", "x"), unobserved("reading", "y")),
            "alike: neither engine could be observed reading: x"
        );
        assert_eq!(
            settled(unobserved("reading", "x"), unobserved("walking", "x")),
            r#"observation: ["not observed: reading: x"] ["not observed: walking: x"]"#
        );
        assert_eq!(
            settled(unobserved("reading", "x"), answered()),
            r#"observation: ["not observed: reading: x"] ["answer 7"]"#
        );
    }

    /// A divergence shows again on a shorter stream when the engines differ
    /// there in nothing new and the same engine failed.
    #[test]
    fn a_divergence_is_told_from_another() {
        let divergence = |parts: &[Part], reference: &str, candidate: &str| Divergence {
            step: 0,
            parts: parts.to_vec(),
            reference: vec![reference.to_string()],
            candidate: vec![candidate.to_string()],
        };
        let cost = divergence(&[Part::Cost, Part::RootHash], "cost 1", "cost 2");
        assert!(cost.shows_again_as(&cost));
        assert!(cost.shows_again_as(&divergence(&[Part::Cost], "cost 3", "cost 4")));
        assert!(!cost.shows_again_as(&divergence(&[Part::Cost, Part::Error], "a", "b")));
        assert!(!cost.shows_again_as(&divergence(&[Part::Stored], "a", "b")));
        let panic = divergence(&[Part::Panic], "cost 1", "panicked: at a");
        assert!(panic.shows_again_as(&divergence(&[Part::Panic], "cost 9", "panicked: at b")));
        assert!(!panic.shows_again_as(&divergence(&[Part::Panic], "panicked: at a", "cost 1")));
        assert!(!panic.shows_again_as(&divergence(
            &[Part::Panic],
            "cost 1",
            "not observed: reading: x"
        )));
        let nothing = Divergence {
            reference: Vec::new(),
            ..panic.clone()
        };
        assert!(nothing.shows_again_as(&panic));
    }

    #[test]
    fn every_part_of_a_step_is_told_apart() {
        fn proof(step: &mut Step) -> &mut Proof {
            step.proof.as_mut().expect("a proof")
        }
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
            families: Counts::default(),
            closes: Closes::default(),
            clean: 7,
            ended: 8,
            passed: 9,
            reference_checks: [1, 2, 3],
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
                families: Counts::default(),
                closes: Closes::default(),
                clean: 14,
                ended: 16,
                passed: 18,
                reference_checks: [2, 4, 6],
            }
        );
    }

    #[test]
    fn a_panic_is_caught_where_it_is_asked_for_and_printed_elsewhere() {
        assert_eq!(catching(|| 7), Ok(7));
        let caught: Result<(), String> = catching(|| panic!("caught {}", 1));
        assert!(caught.expect_err("a panic").contains("caught 1"));
        // Inside another catch, and after it the outer one still catches.
        let outer: Result<(), String> = catching(|| {
            let inner: Result<(), String> = catching(|| panic!("inner"));
            assert!(inner.expect_err("a panic").contains("inner"));
            panic!("outer")
        });
        assert!(outer.expect_err("a panic").contains("outer"));
        // The hook hands a panic nobody catches to the hook that was there.
        let outside = std::thread::spawn(|| panic!("not caught")).join();
        assert!(outside.is_err());
    }
}
