//! What the corpus holds: the traces recorded for each grove version.
//!
//! At the standard scale, the one a pull request replays, a grove version
//! has:
//!
//! - one trace per workload of [`Workload::suite`], each on a database of
//!   its own, generated from [`SEED`];
//! - `suite`: every workload of the suite, one after another on one
//!   database, generated from [`SUITE_SEED`], so the root tree holds many
//!   subtrees and every stream is recorded for a second seed;
//! - one trace per hand-written [`legacy`] case.
//!
//! The reference scale is one trace per grove version: the million-document
//! workload the plan's measurements were taken on, followed by its two
//! named proofs. It takes minutes and a gigabyte of scratch per grove
//! version, so it is recorded and replayed only when asked for.
//!
//! The plan follows whatever [`Workload::suite`] returns: a workload added
//! there is recorded the next time the corpus is.

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    legacy,
    record::{Sample, SegmentSpec, Source, TraceSpec},
};
use crate::workload::{drive::Documents, Features, Op, Read, Workload};

/// The seed of the traces that give each workload a database of its own.
pub const SEED: u64 = 1;

/// The seed of the `suite` trace: the bytes of "LIGNIN".
pub const SUITE_SEED: u64 = 0x4C49_474E_494E;

/// The seed written into a hand-written trace, whose operations draw on no
/// generator.
pub const HAND_WRITTEN_SEED: u64 = 0;

/// The most bytes of results and proofs a sampled step has.
pub const SAMPLE_MAX_PAYLOAD: usize = 2048;

/// How many steps of the reference trace lie between two checkpoints.
pub const REFERENCE_CHECKPOINT_EVERY: u64 = 1000;

/// The document the reference trace proves by id: the one created halfway
/// through the stream.
pub const REFERENCE_DOCUMENT: u64 = 500_000;

/// The owner whose first [`REFERENCE_OWNER_LIMIT`] documents the reference
/// trace proves through the index references.
pub const REFERENCE_OWNER: u64 = 0;

/// How many documents the reference trace proves through references.
pub const REFERENCE_OWNER_LIMIT: u16 = 100;

/// The workload name of the reference trace's last segment, which holds the
/// two named proofs.
pub const NAMED_PROOFS: &str = "named-proofs";

/// How much is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// Every workload at its default size and the hand-written cases.
    Standard,
    /// The million-document reference workload.
    Reference,
}

/// A trace of the corpus and the file it is stored in.
#[derive(Clone, Debug)]
pub struct Planned {
    /// The file, relative to the corpus directory.
    pub file: String,
    /// The trace.
    pub spec: TraceSpec,
    /// Whether the trace is a hand-written case, which may record an
    /// operation that fails or a proof that does not verify. A generated
    /// workload may not.
    pub hand_written: bool,
}

/// Every trace of `scale`, for every grove version.
pub fn plan(scale: Scale) -> Vec<Planned> {
    GROVE_VERSIONS
        .iter()
        .flat_map(|grove_version| match scale {
            Scale::Standard => standard(grove_version),
            Scale::Reference => vec![reference(grove_version)],
        })
        .collect()
}

/// The standard-scale traces of one grove version.
pub fn standard(grove_version: &'static GroveVersion) -> Vec<Planned> {
    let version = grove_version.protocol_version;
    let suite = Workload::suite(Features::for_grove_version(grove_version));
    let sample = Sample::FirstOfEachKind {
        max_payload: SAMPLE_MAX_PAYLOAD,
    };
    let segment = |workload: &Workload| SegmentSpec {
        workload: workload.name().to_string(),
        source: Source::Workload(*workload),
        checkpoint_every: None,
        sample,
    };
    let trace = |name: &str, seed: u64, segments: Vec<SegmentSpec>| Planned {
        file: format!("v{version}/{name}.trace"),
        spec: TraceSpec {
            name: name.to_string(),
            seed,
            grove_version,
            segments,
        },
        hand_written: seed == HAND_WRITTEN_SEED,
    };
    let mut planned: Vec<Planned> = suite
        .iter()
        .map(|workload| trace(workload.name(), SEED, vec![segment(workload)]))
        .collect();
    planned.push(trace(
        "suite",
        SUITE_SEED,
        suite.iter().map(segment).collect(),
    ));
    for case in legacy::cases() {
        let name = format!("legacy-{}", case.name);
        planned.push(trace(
            &name,
            HAND_WRITTEN_SEED,
            vec![SegmentSpec {
                workload: name.clone(),
                source: Source::Ops(case.ops),
                checkpoint_every: None,
                sample: Sample::Everything,
            }],
        ));
    }
    planned
}

/// The reference-scale trace of one grove version: the million-document
/// stream, checkpointed, then a commit and the two named proofs in full.
pub fn reference(grove_version: &'static GroveVersion) -> Planned {
    let documents = Documents::reference();
    Planned {
        file: format!("reference/v{}.trace", grove_version.protocol_version),
        spec: TraceSpec {
            name: "reference".to_string(),
            seed: SEED,
            grove_version,
            segments: vec![
                SegmentSpec {
                    workload: "documents-reference".to_string(),
                    source: Source::Workload(Workload::Documents(documents)),
                    checkpoint_every: Some(REFERENCE_CHECKPOINT_EVERY),
                    sample: Sample::Nothing,
                },
                SegmentSpec {
                    workload: NAMED_PROOFS.to_string(),
                    source: Source::Ops(named_proofs(&documents, SEED)),
                    checkpoint_every: None,
                    sample: Sample::Everything,
                },
            ],
        },
        hand_written: false,
    }
}

/// The operations of the named-proofs segment: a commit, which writes the
/// root hash out in full, the point query and the query of one owner's
/// documents.
pub fn named_proofs(documents: &Documents, seed: u64) -> Vec<Op> {
    vec![
        Op::Commit,
        Op::Read(Read::Query(documents.point_query(
            seed,
            REFERENCE_DOCUMENT.min(documents.documents - 1),
        ))),
        Op::Read(Read::Query(documents.owner_query(
            seed,
            REFERENCE_OWNER,
            REFERENCE_OWNER_LIMIT,
        ))),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn the_standard_plan_follows_the_suite_for_every_grove_version() {
        let planned = plan(Scale::Standard);
        let files: BTreeSet<&str> = planned
            .iter()
            .map(|planned| planned.file.as_str())
            .collect();
        assert_eq!(files.len(), planned.len(), "two traces share a file");
        for grove_version in GROVE_VERSIONS {
            let version = grove_version.protocol_version;
            let suite = Workload::suite(Features::for_grove_version(grove_version));
            let traces = standard(grove_version);
            assert_eq!(traces.len(), suite.len() + 1 + legacy::cases().len());
            for (workload, planned) in suite.iter().zip(&traces) {
                assert_eq!(
                    planned.file,
                    format!("v{version}/{}.trace", workload.name())
                );
                assert_eq!(planned.spec.name, workload.name());
                assert_eq!(planned.spec.seed, SEED);
                assert_eq!(planned.spec.grove_version.protocol_version, version);
                assert_eq!(planned.spec.segments.len(), 1);
                assert_eq!(planned.spec.segments[0].workload, workload.name());
                assert!(!planned.hand_written);
            }
            let whole = &traces[suite.len()];
            assert_eq!(whole.file, format!("v{version}/suite.trace"));
            assert_eq!(whole.spec.seed, SUITE_SEED);
            assert!(!whole.hand_written);
            let workloads: Vec<&str> = whole
                .spec
                .segments
                .iter()
                .map(|segment| segment.workload.as_str())
                .collect();
            let names: Vec<&str> = suite.iter().map(Workload::name).collect();
            assert_eq!(workloads, names);
            for planned in &traces[suite.len() + 1..] {
                assert!(planned.spec.name.starts_with("legacy-"));
                assert!(planned.hand_written);
                assert_eq!(planned.spec.seed, HAND_WRITTEN_SEED);
                assert_eq!(planned.spec.segments[0].sample, Sample::Everything);
                assert_eq!(planned.spec.segments[0].workload, planned.spec.name);
            }
        }
    }

    #[test]
    fn the_reference_plan_is_the_measured_workload_and_its_two_proofs() {
        let planned = plan(Scale::Reference);
        assert_eq!(planned.len(), GROVE_VERSIONS.len());
        let first = &planned[0];
        assert_eq!(first.file, "reference/v1.trace");
        assert!(!first.hand_written);
        assert_eq!(first.spec.seed, SEED);
        let [stream, proofs] = first.spec.segments.as_slice() else {
            panic!("the reference trace has two segments");
        };
        assert!(matches!(
            stream.source,
            Source::Workload(Workload::Documents(documents)) if documents == Documents::reference()
        ));
        assert_eq!(stream.checkpoint_every, Some(REFERENCE_CHECKPOINT_EVERY));
        assert_eq!(stream.sample, Sample::Nothing);
        assert_eq!(proofs.workload, NAMED_PROOFS);
        assert_eq!(proofs.sample, Sample::Everything);
        let Source::Ops(ops) = &proofs.source else {
            panic!("the named proofs are written by hand");
        };
        let reference = Documents::reference();
        assert_eq!(
            ops,
            &[
                Op::Commit,
                Op::Read(Read::Query(reference.point_query(SEED, 500_000))),
                Op::Read(Read::Query(reference.owner_query(SEED, 0, 100))),
            ]
        );
        let tiny = Documents {
            documents: 3,
            ..Documents::default()
        };
        assert_eq!(
            named_proofs(&tiny, 9)[1],
            Op::Read(Read::Query(tiny.point_query(9, 2))),
            "a smaller workload proves its last document"
        );
    }
}
