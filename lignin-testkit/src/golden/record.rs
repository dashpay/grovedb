//! Recording a trace: running its operation streams on a fresh database and
//! laying the steps out as a [`Trace`].
//!
//! Recording, replaying and dumping are this one function. They differ only
//! in which steps are written out in full: the ones the plan samples, the
//! ones the recorded file has, or all of them.

use std::{
    collections::BTreeSet,
    mem::{discriminant, Discriminant},
    path::Path,
};

use grovedb::GroveDb;
use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, OperationCost};
use grovedb_version::version::GroveVersion;

use super::{
    run::{Observed, Runner},
    step::{Outcome, Step, TraceDigest, Value},
    trace::{one_line, shortened, Detail, Entry, Segment, Tally, Trace},
    Error,
};
use crate::workload::{Op, Read, StreamDigest, TransactionOp, Workload, Write};

/// A trace to record: the streams applied, one after another, to one
/// database that starts empty.
#[derive(Clone, Debug)]
pub struct TraceSpec {
    /// The name of the trace.
    pub name: String,
    /// The seed every stream is generated from.
    pub seed: u64,
    /// The grove version every operation is applied with.
    pub grove_version: &'static GroveVersion,
    /// The streams, in order.
    pub segments: Vec<SegmentSpec>,
}

/// One operation stream of a trace.
#[derive(Clone, Debug)]
pub struct SegmentSpec {
    /// The workload the stream belongs to.
    pub workload: String,
    /// Where the operations come from.
    pub source: Source,
    /// A checkpoint every this many steps in place of a line per step.
    pub checkpoint_every: Option<u64>,
    /// Which steps are written out in full.
    pub sample: Sample,
}

/// Where the operations of a segment come from.
#[derive(Clone, Debug)]
pub enum Source {
    /// The stream of a workload for the seed of the trace.
    Workload(Workload),
    /// Operations written by hand.
    Ops(Vec<Op>),
}

impl Source {
    /// The operations, for the seed of the trace.
    pub fn ops(&self, seed: u64) -> Box<dyn Iterator<Item = Op> + '_> {
        match self {
            Source::Workload(workload) => Box::new(workload.stream(seed)),
            Source::Ops(ops) => Box::new(ops.iter().cloned()),
        }
    }
}

/// Which steps of a segment a recording writes out in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    /// None.
    Nothing,
    /// The first step of each kind of operation in the trace whose results
    /// and proofs take at most this many bytes.
    FirstOfEachKind {
        /// The most bytes of results and proofs a sampled step has.
        max_payload: usize,
    },
    /// Every step.
    Everything,
}

/// Which steps one run writes out in full.
#[derive(Clone, Copy, Debug)]
pub enum Detailing<'a> {
    /// The ones each segment samples: a recording.
    Sampled,
    /// These steps of the trace: a replay, which follows the recorded file.
    Steps(&'a BTreeSet<u64>),
    /// Every step, with a line per step whatever the segment asks for: a
    /// dump.
    Everything,
}

/// The longest remark quoting an operation. A large-value workload stores
/// values of a megabyte.
const REMARK_CHARS: usize = 400;

/// A fresh database in a directory of its own under `scratch`. The
/// directory goes when the first of the pair is dropped, so the database is
/// the second.
fn fresh_database(scratch: &Path) -> Result<(tempfile::TempDir, GroveDb), Error> {
    let dir = tempfile::Builder::new()
        .prefix("lignin-golden-")
        .tempdir_in(scratch)
        .map_err(|error| Error::io(scratch, &error))?;
    let db =
        GroveDb::open(dir.path()).map_err(|error| Error::engine("opening a database", &error))?;
    Ok((dir, db))
}

/// Runs `spec` on a fresh database under `scratch` and hands every step to
/// `visit`: the number of its segment, the operation and what the engine
/// returned for it, as values, not text.
///
/// This is for a consumer that computes with the steps, such as a check of
/// a cost model against every `OperationCost` of the corpus. The trace runs
/// on a thread of its own, with the stack it needs.
pub fn each_step(
    spec: &TraceSpec,
    scratch: &Path,
    mut visit: impl FnMut(usize, &Op, Observed) + Send,
) -> Result<(), Error> {
    on_a_deep_stack(|| {
        let (_dir, db) = fresh_database(scratch)?;
        let mut runner = Runner::new(&db, spec.grove_version)?;
        for (index, segment) in spec.segments.iter().enumerate() {
            for op in segment.source.ops(spec.seed) {
                let observed = runner.step(&op)?;
                visit(index, &op, observed);
            }
        }
        Ok(())
    })
}

/// Runs `body` on a thread with the stack a trace needs and returns what it
/// returned.
pub(crate) fn on_a_deep_stack<T: Send>(body: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(super::run::STACK_BYTES)
            .spawn_scoped(scope, body)
            .expect("a thread can be started")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Runs `spec` on a fresh database under `scratch` and returns the trace.
///
/// Call it on a thread with a stack of
/// [`STACK_BYTES`](super::run::STACK_BYTES).
pub fn record(
    spec: &TraceSpec,
    engine: &str,
    detailing: Detailing,
    scratch: &Path,
) -> Result<Trace, Error> {
    let (_dir, db) = fresh_database(scratch)?;
    let mut runner = Runner::new(&db, spec.grove_version)?;
    let mut digest = TraceDigest::new();
    let mut tally = Tally::default();
    let mut sampled_kinds = Vec::new();
    let mut segments = Vec::with_capacity(spec.segments.len());
    for segment in &spec.segments {
        let stride = match detailing {
            Detailing::Everything => None,
            Detailing::Sampled | Detailing::Steps(_) => segment.checkpoint_every,
        };
        let mut stream = StreamDigest::new();
        let mut entries = Vec::new();
        let mut checkpointed = digest.steps();
        for op in segment.source.ops(spec.seed) {
            let index = digest.steps();
            let observed = runner.step(&op)?;
            stream.update(&op);
            digest.update(&observed.step);
            tally.failed += u64::from(observed.step.failed());
            tally.unproved += u64::from(observed.step.unproved());
            tally.unverified += u64::from(observed.step.unverified());
            match stride {
                None => entries.push(Entry::Step(observed.step.line())),
                Some(stride) if digest.steps().is_multiple_of(stride.max(1)) => {
                    entries.push(checkpoint(&digest));
                    checkpointed = digest.steps();
                }
                Some(_) => {}
            }
            let in_full = match detailing {
                Detailing::Everything => true,
                Detailing::Steps(steps) => steps.contains(&index),
                Detailing::Sampled => match segment.sample {
                    Sample::Nothing => false,
                    Sample::Everything => true,
                    Sample::FirstOfEachKind { max_payload } => {
                        let kind = Kind::of(&op, &observed.step);
                        let new = !sampled_kinds.contains(&kind)
                            && observed.step.payload_len() <= max_payload;
                        if new {
                            sampled_kinds.push(kind);
                        }
                        new
                    }
                },
            };
            if in_full {
                let mut remarks = vec![quoted(&op)];
                remarks.extend(observed.remarks.iter().map(|remark| one_line(remark)));
                entries.push(Entry::Detail(Detail {
                    step: index,
                    lines: observed.step.detail(),
                    remarks,
                }));
            }
        }
        if stride.is_some() && checkpointed != digest.steps() {
            entries.push(checkpoint(&digest));
        }
        segments.push(Segment {
            workload: segment.workload.clone(),
            entries,
            ops: stream.ops(),
            stream: stream.hex(),
        });
    }
    Ok(Trace {
        name: spec.name.clone(),
        seed: spec.seed,
        grove_version: spec.grove_version.protocol_version,
        engine: engine.to_string(),
        segments,
        tally,
        steps: digest.steps(),
        digest: digest.hex(),
    })
}

fn checkpoint(digest: &TraceDigest) -> Entry {
    let mut hex = digest.hex();
    hex.truncate(8);
    Entry::Checkpoint(digest.steps(), hex)
}

/// The operation as a remark: on one line of printable characters, cut to
/// a readable length.
fn quoted(op: &Op) -> String {
    shortened(&one_line(&format!("{op:?}")), REMARK_CHARS)
}

/// The kind of a step, as far as sampling tells kinds apart: the kind of
/// its operation and, where the operation returns a record, the kind of
/// record, so a trace shows each shape of answer a path query has. An
/// operation of a kind this module has never heard of is still a kind of
/// its own.
///
/// An operation whose cost names the bytes it freed by owner and epoch is
/// a kind of its own too, and another when it names more than one epoch of
/// one owner: a trace shows a refund, and one split across epochs, for each
/// operation that makes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kind {
    op: Discriminant<Op>,
    write: Option<Discriminant<Write>>,
    read: Option<Discriminant<Read>>,
    step: Option<Discriminant<TransactionOp>>,
    record: Option<&'static str>,
    refund: Refund,
}

/// How a cost names the bytes an operation freed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refund {
    /// Not at all, or not by owner and epoch.
    Unsplit,
    /// To one epoch of each owner it names.
    OneEpoch,
    /// To several epochs of one owner.
    SeveralEpochs,
}

impl Refund {
    fn of(cost: &OperationCost) -> Self {
        let StorageRemovedBytes::SectionedStorageRemoval(owners) = &cost.storage_cost.removed_bytes
        else {
            return Refund::Unsplit;
        };
        match owners.values().map(|epochs| epochs.len()).max() {
            None | Some(0) => Refund::Unsplit,
            Some(1) => Refund::OneEpoch,
            Some(_) => Refund::SeveralEpochs,
        }
    }
}

impl Kind {
    fn of(op: &Op, step: &Step) -> Self {
        Kind {
            record: match &step.outcome {
                Outcome::Returned(Value::Record(kind, _)) => Some(kind),
                _ => None,
            },
            op: discriminant(op),
            write: match op {
                Op::Write(write) => Some(discriminant(write)),
                _ => None,
            },
            read: match op {
                Op::Read(read)
                | Op::Transaction(
                    TransactionOp::BlockRead(read) | TransactionOp::SnapshotRead(read),
                ) => Some(discriminant(read)),
                _ => None,
            },
            step: match op {
                Op::Transaction(step) => Some(discriminant(step)),
                _ => None,
            },
            refund: Refund::of(&step.cost),
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb::Element;

    use super::*;
    use crate::workload::{drive::Documents, BatchOp, Query, QueryItem, Read, Write};

    fn small() -> Workload {
        Workload::Documents(Documents {
            documents: 30,
            owners: 3,
            document_size: 40,
            transitions_per_block: 5,
            updates: 5,
            deletes: 10,
            reads_per_block: 4,
        })
    }

    fn spec(checkpoint_every: Option<u64>, sample: Sample) -> TraceSpec {
        TraceSpec {
            name: "small".to_string(),
            seed: 7,
            grove_version: GroveVersion::latest(),
            segments: vec![
                SegmentSpec {
                    workload: "documents".to_string(),
                    source: Source::Workload(small()),
                    checkpoint_every,
                    sample,
                },
                SegmentSpec {
                    workload: "by-hand".to_string(),
                    source: Source::Ops(vec![
                        Op::Write(Write::Insert {
                            path: Vec::new(),
                            key: b"extra".to_vec(),
                            element: Element::empty_tree(),
                        }),
                        Op::Commit,
                    ]),
                    checkpoint_every,
                    sample,
                },
            ],
        }
    }

    fn run(spec: &TraceSpec, detailing: Detailing) -> Trace {
        let scratch = tempfile::tempdir().expect("temp dir");
        record(spec, "engine", detailing, scratch.path()).expect("record")
    }

    #[test]
    fn a_recording_is_reproducible_and_reads_back() {
        let spec = spec(None, Sample::FirstOfEachKind { max_payload: 4096 });
        let trace = run(&spec, Detailing::Sampled);
        assert_eq!(trace, run(&spec, Detailing::Sampled));
        assert_eq!(Trace::parse(&trace.render()), Ok(trace.clone()));

        assert_eq!(
            (trace.name.as_str(), trace.seed, trace.grove_version),
            ("small", 7, GroveVersion::latest().protocol_version)
        );
        assert_eq!(trace.engine, "engine");
        let ops: Vec<Op> = small().stream(7).collect();
        assert_eq!(trace.segments[0].workload, "documents");
        assert_eq!(trace.segments[0].ops, ops.len() as u64);
        assert_eq!(trace.segments[0].stream, StreamDigest::of(&ops).hex());
        assert_eq!(trace.segments[1].workload, "by-hand");
        assert_eq!(trace.segments[1].ops, 2);
        assert_eq!(trace.steps, ops.len() as u64 + 2);
        assert!(trace.tally.is_clean(), "{}", trace.tally);
        let lines = |segment: &Segment| {
            segment
                .entries
                .iter()
                .filter(|entry| matches!(entry, Entry::Step(_)))
                .count() as u64
        };
        assert_eq!(lines(&trace.segments[0]), trace.segments[0].ops);
        assert_eq!(lines(&trace.segments[1]), 2);

        let kinds: Vec<&str> = trace
            .details()
            .map(|detail| detail.remarks[0].split(['(', ' ']).next().unwrap())
            .collect();
        assert_eq!(
            kinds,
            ["Write", "Commit", "Batch", "Read", "Read"],
            "the first insert, commit, batch, get and query are sampled, once for the trace"
        );
        assert_eq!(trace.detail(0).expect("the first step").step, 0);
    }

    #[test]
    fn another_seed_grove_version_or_stream_is_another_trace() {
        let base = spec(None, Sample::Nothing);
        let recorded = run(&base, Detailing::Sampled);
        assert_eq!(recorded.details().count(), 0);

        let mut other_seed = base.clone();
        other_seed.seed = 8;
        let other = run(&other_seed, Detailing::Sampled);
        assert_ne!(other.digest, recorded.digest);
        assert_ne!(other.segments[0].stream, recorded.segments[0].stream);

        let mut older = base.clone();
        older.grove_version = GroveVersion::first();
        let older = run(&older, Detailing::Sampled);
        assert_eq!(older.segments[0].stream, recorded.segments[0].stream);
        assert_ne!(
            older.digest, recorded.digest,
            "grove version 1 charges or proves something differently"
        );
        assert!(!older.differences_from(&recorded).is_empty());
    }

    #[test]
    fn a_replay_writes_out_the_steps_it_is_told_to() {
        let spec = spec(None, Sample::Nothing);
        let steps = BTreeSet::from([3, 40]);
        let trace = run(&spec, Detailing::Steps(&steps));
        let detailed: Vec<u64> = trace.details().map(|detail| detail.step).collect();
        assert_eq!(detailed, [3, 40]);
        let everything = run(&spec, Detailing::Everything);
        assert_eq!(everything.details().count() as u64, everything.steps);
        assert_eq!(everything.digest, trace.digest);
        assert_eq!(
            everything.detail(40).expect("step 40").lines,
            trace.detail(40).expect("step 40").lines
        );
    }

    #[test]
    fn each_step_is_handed_over_as_values() {
        let spec = spec(None, Sample::Nothing);
        let scratch = tempfile::tempdir().expect("temp dir");
        let mut digest = TraceDigest::new();
        let mut segments = Vec::new();
        let mut seeks = 0u64;
        each_step(&spec, scratch.path(), |segment, op, observed| {
            segments.push(segment);
            digest.update(&observed.step);
            seeks += u64::from(observed.step.cost.seek_count);
            assert_eq!(observed.step.root_hash.is_none(), matches!(op, Op::Read(_)));
        })
        .expect("each step");
        let trace = run(&spec, Detailing::Sampled);
        assert_eq!((digest.steps(), digest.hex()), (trace.steps, trace.digest));
        assert!(seeks > 0);
        let in_first = segments.iter().filter(|segment| **segment == 0).count() as u64;
        assert_eq!(in_first, trace.segments[0].ops);
        assert_eq!(segments.last(), Some(&1));
        assert!(segments.is_sorted());

        let missing = Path::new("/nonexistent/lignin-golden");
        let error = each_step(&spec, missing, |_, _, _| {}).expect_err("no scratch");
        assert!(matches!(error, Error::Io { .. }), "{error}");

        let panic = std::panic::catch_unwind(|| {
            let scratch = tempfile::tempdir().expect("temp dir");
            each_step(&spec, scratch.path(), |_, _, _| panic!("inside"))
        });
        let panic = panic.expect_err("a panic of the visitor comes through");
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"inside"));
    }

    #[test]
    fn a_long_segment_is_checkpointed() {
        let spec = spec(Some(25), Sample::Everything);
        let fine = run(&spec, Detailing::Everything);
        let coarse = run(&spec, Detailing::Sampled);
        assert_eq!(coarse.digest, fine.digest);
        assert_eq!(coarse.details().count() as u64, coarse.steps);
        let checkpoints = |segment: &Segment| -> Vec<u64> {
            segment
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Checkpoint(steps, digest) => {
                        assert_eq!(digest.len(), 8);
                        Some(*steps)
                    }
                    Entry::Step(_) => panic!("a checkpointed segment has no step lines"),
                    Entry::Detail(_) => None,
                })
                .collect()
        };
        let ops = coarse.segments[0].ops;
        let mut expected: Vec<u64> = (1..=ops / 25).map(|multiple| multiple * 25).collect();
        if !ops.is_multiple_of(25) {
            expected.push(ops);
        }
        assert_eq!(checkpoints(&coarse.segments[0]), expected);
        assert_eq!(checkpoints(&coarse.segments[1]), [coarse.steps]);
        assert_eq!(Trace::parse(&coarse.render()), Ok(coarse.clone()));

        let steps = BTreeSet::new();
        let replayed = run(&spec, Detailing::Steps(&steps));
        assert_eq!(replayed.details().count(), 0);
        assert_eq!(
            checkpoints(&replayed.segments[0]),
            checkpoints(&coarse.segments[0])
        );

        let none = TraceSpec {
            segments: vec![SegmentSpec {
                workload: "empty".to_string(),
                source: Source::Ops(Vec::new()),
                checkpoint_every: Some(0),
                sample: Sample::Nothing,
            }],
            ..spec
        };
        let empty = run(&none, Detailing::Sampled);
        assert!(empty.segments[0].entries.is_empty());
        assert_eq!(empty.steps, 0);
    }

    #[test]
    fn a_step_too_large_to_sample_is_passed_over() {
        let query = |limit: Option<u16>| {
            Op::Read(Read::Query(Query {
                path: vec![b"tree".to_vec()],
                items: vec![QueryItem::RangeFull],
                subquery_path: None,
                subquery_all: false,
                limit,
                left_to_right: true,
                raw: true,
            }))
        };
        let mut ops = vec![
            Op::Write(Write::Insert {
                path: Vec::new(),
                key: b"tree".to_vec(),
                element: Element::empty_tree(),
            }),
            Op::Batch(
                (0u8..20)
                    .map(|key| BatchOp::InsertOnly {
                        path: vec![b"tree".to_vec()],
                        key: vec![key],
                        element: Element::new_item(vec![key; 100]),
                    })
                    .collect(),
            ),
            Op::Commit,
        ];
        ops.extend([query(None), query(Some(1))]);
        let spec = TraceSpec {
            name: "sampling".to_string(),
            seed: 0,
            grove_version: GroveVersion::latest(),
            segments: vec![SegmentSpec {
                workload: "by-hand".to_string(),
                source: Source::Ops(ops),
                checkpoint_every: None,
                sample: Sample::FirstOfEachKind { max_payload: 1000 },
            }],
        };
        let trace = run(&spec, Detailing::Sampled);
        let detailed: Vec<u64> = trace.details().map(|detail| detail.step).collect();
        assert_eq!(
            detailed,
            [0, 1, 2, 4],
            "the query of twenty rows is too large"
        );
    }

    /// A path query has many shapes of answer under one kind of read, so
    /// the first step of each shape is sampled.
    #[test]
    fn the_first_answer_of_each_shape_is_sampled() {
        use grovedb::{PathQuery, QueryItem as Item};

        let tree = || vec![b"tree".to_vec()];
        let read = |query: PathQuery| {
            Op::Read(Read::PathQuery {
                query: Box::new(query),
                proven: true,
            })
        };
        let rows = || {
            read(PathQuery::new_unsized(
                tree(),
                grovedb::Query::new_range_full(),
            ))
        };
        let count = || {
            read(PathQuery::new_aggregate_count_on_range(
                tree(),
                Item::RangeFrom(b"a".to_vec()..),
            ))
        };
        let ops = vec![
            Op::Write(Write::Insert {
                path: Vec::new(),
                key: b"tree".to_vec(),
                element: Element::empty_provable_count_tree(),
            }),
            Op::Write(Write::Insert {
                path: tree(),
                key: b"row".to_vec(),
                element: Element::new_item(b"value".to_vec()),
            }),
            Op::Commit,
            rows(),
            count(),
            rows(),
            count(),
        ];
        let spec = TraceSpec {
            name: "sampling".to_string(),
            seed: 0,
            grove_version: GroveVersion::latest(),
            segments: vec![SegmentSpec {
                workload: "by-hand".to_string(),
                source: Source::Ops(ops),
                checkpoint_every: None,
                sample: Sample::FirstOfEachKind { max_payload: 1000 },
            }],
        };
        let trace = run(&spec, Detailing::Sampled);
        assert!(trace.tally.is_clean(), "{}", trace.render());
        let detailed: Vec<(u64, &str)> = trace
            .details()
            .map(|detail| (detail.step, detail.lines[1].as_str()))
            .collect();
        assert_eq!(
            detailed[1..],
            [
                (2, detailed[1].1),
                (3, "result elements"),
                (4, "result aggregate-count"),
            ],
            "{}",
            trace.render()
        );
    }

    /// An update under Drive's callbacks frees bytes of no epoch, of one
    /// or of several, under one kind of operation, so the first step of
    /// each is sampled.
    #[test]
    fn the_first_refund_of_each_split_is_sampled() {
        use grovedb_epoch_based_storage_flags::StorageFlags;

        let item = |len: usize, epoch: u16| {
            let flags = StorageFlags::SingleEpochOwned(epoch, [1; 32]).to_element_flags();
            Op::FlagsBatch(vec![BatchOp::InsertOrReplace {
                path: Vec::new(),
                key: b"item".to_vec(),
                element: Element::new_item_with_flags(vec![7; len], Some(flags)),
            }])
        };
        let ops = vec![
            item(40, 0),
            item(140, 1),
            item(240, 2),
            // Within the latest epoch, twice, then across it, twice.
            item(220, 3),
            item(200, 3),
            item(90, 3),
            item(20, 3),
            Op::Commit,
        ];
        let spec = TraceSpec {
            name: "refunds".to_string(),
            seed: 0,
            grove_version: GroveVersion::latest(),
            segments: vec![SegmentSpec {
                workload: "by-hand".to_string(),
                source: Source::Ops(ops),
                checkpoint_every: None,
                sample: Sample::FirstOfEachKind { max_payload: 1000 },
            }],
        };
        let trace = run(&spec, Detailing::Sampled);
        assert!(trace.tally.is_clean(), "{}", trace.render());
        let owner = "01".repeat(32);
        let detailed: Vec<(u64, &str)> = trace
            .details()
            .map(|detail| {
                let cost = detail.lines[0].as_str();
                let removed = cost.split_once("removed=").expect("a cost line").1;
                (detail.step, removed.split(' ').next().expect("the bytes"))
            })
            .collect();
        assert_eq!(
            detailed,
            [
                (0, "-"),
                (3, format!("{{{owner}/2=20}}").as_str()),
                (5, format!("{{{owner}/1=53,{owner}/2=60}}").as_str()),
                (7, "-"),
            ],
            "{}",
            trace.render()
        );
    }

    #[test]
    fn a_long_operation_is_quoted_in_part() {
        let op = Op::Batch(
            (0u8..100)
                .map(|key| BatchOp::Delete {
                    path: vec![b"tree".to_vec()],
                    key: vec![key],
                })
                .collect(),
        );
        let remark = quoted(&op);
        assert!(remark.starts_with("Batch([Delete {"), "{remark}");
        assert!(remark.ends_with("characters)"), "{remark}");
        assert!(remark.chars().count() < REMARK_CHARS + 40);
        assert_eq!(quoted(&Op::Commit), "Commit");
    }

    /// An operation is quoted with its keys and values, which are any
    /// bytes. A trace stays a text file all the same, and reads back as it
    /// was recorded.
    #[test]
    fn a_value_of_control_bytes_is_quoted_in_printable_characters() {
        let ops = vec![
            Op::Write(Write::Insert {
                path: Vec::new(),
                key: b"key".to_vec(),
                element: Element::new_item(b"v\0\x01\n\r\x7f".to_vec()),
            }),
            Op::Commit,
            Op::Read(Read::Get {
                path: Vec::new(),
                key: b"absent".to_vec(),
            }),
        ];
        let remark = quoted(&ops[0]);
        assert!(remark.contains("\\u{0}\\u{1}"), "{remark}");
        assert!(remark.contains("\\r\\u{7f}"), "{remark}");
        assert!(!remark.chars().any(char::is_control), "{remark:?}");
        let spec = TraceSpec {
            name: "bytes".to_string(),
            seed: 0,
            grove_version: GroveVersion::latest(),
            segments: vec![SegmentSpec {
                workload: "by-hand".to_string(),
                source: Source::Ops(ops),
                checkpoint_every: None,
                sample: Sample::Everything,
            }],
        };
        let trace = run(&spec, Detailing::Sampled);
        let text = trace.render();
        assert!(
            !text
                .chars()
                .any(|character| character.is_control() && character != '\n'),
            "{text:?}"
        );
        assert_eq!(Trace::parse(&text), Ok(trace));
    }

    #[test]
    fn a_scratch_directory_that_does_not_exist_is_an_error() {
        let spec = spec(None, Sample::Nothing);
        let missing = Path::new("/nonexistent/lignin-golden");
        let error = record(&spec, "engine", Detailing::Sampled, missing).expect_err("no scratch");
        assert!(
            matches!(&error, Error::Io { path, .. } if path == missing),
            "{error}"
        );
    }
}
