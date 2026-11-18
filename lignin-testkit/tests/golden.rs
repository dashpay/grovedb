//! Acceptance tests for the golden trace corpus (BASE-02).
//!
//! - The corpus replays green: `the_corpus_replays_green` runs every
//!   standard-scale trace again and compares it with its file. This is the
//!   test a pull request has to pass. `the_reference_scale_replays_green`
//!   does the same for the million-document traces and is run on request:
//!   `cargo test --release -p lignin-testkit --test golden -- --ignored`.
//! - Every record names its workload, seed and grove version:
//!   `every_trace_names_its_workload_seed_and_grove_version`.
//! - The corpus holds the two named proofs of the reference workload:
//!   `the_named_proofs_are_in_the_corpus`.
//! - The corpus holds the hand-written cases for older grove versions, as
//!   the engine returns them: `only_the_hand_written_cases_record_a_failure`.
//! - Every read GroveDB proves is recorded with its proof:
//!   `every_proven_read_is_recorded_with_its_proof`.
//! - The files are text: `every_trace_file_is_text`.

use std::{collections::BTreeSet, fs, path::PathBuf};

use grovedb::{Element, GroveDb};
use grovedb_version::version::GROVE_VERSIONS;
use lignin_testkit::{
    golden::{
        corpus::{corpus_dir, replay, Selection, Settings},
        legacy,
        plan::{
            self, Scale, HAND_WRITTEN_SEED, NAMED_PROOFS, REFERENCE_DOCUMENT, REFERENCE_OWNER,
            REFERENCE_OWNER_LIMIT, SEED, SUITE_SEED,
        },
        run::provable,
        trace::{Detail, Entry, Tally, Trace},
    },
    workload::{drive::Documents, Op},
};

fn recorded(file: &str) -> Trace {
    let path = corpus_dir().join(file);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}. Record the corpus.", path.display()));
    Trace::parse(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn the_corpus_replays_green() {
    let report = replay(
        &corpus_dir(),
        &Selection::all(Scale::Standard),
        &Settings::default(),
    )
    .expect("the corpus can be replayed");
    assert!(report.is_green(), "{report}");
    assert_eq!(report.traces.len(), plan::plan(Scale::Standard).len());
    print!("{report}");
}

/// `LIGNIN_GOLDEN_SCRATCH` names the directory the databases are created in
/// (a memory-backed one makes this several times faster) and
/// `LIGNIN_GOLDEN_JOBS` how many grove versions run at once.
#[test]
#[ignore = "a million documents per grove version: minutes, and a gigabyte of scratch each"]
fn the_reference_scale_replays_green() {
    let settings = Settings {
        scratch: std::env::var_os("LIGNIN_GOLDEN_SCRATCH")
            .map_or_else(std::env::temp_dir, PathBuf::from),
        jobs: std::env::var("LIGNIN_GOLDEN_JOBS")
            .ok()
            .map_or(1, |jobs| jobs.parse().expect("a number of jobs")),
    };
    let report = replay(&corpus_dir(), &Selection::all(Scale::Reference), &settings)
        .expect("the corpus can be replayed");
    assert!(report.is_green(), "{report}");
    assert_eq!(report.traces.len(), GROVE_VERSIONS.len());
    print!("{report}");
}

#[test]
fn every_trace_names_its_workload_seed_and_grove_version() {
    let mut engines = BTreeSet::new();
    let mut steps = 0;
    for scale in [Scale::Standard, Scale::Reference] {
        for planned in plan::plan(scale) {
            let trace = recorded(&planned.file);
            let spec = &planned.spec;
            assert_eq!(trace.name, spec.name, "{}", planned.file);
            assert_eq!(trace.seed, spec.seed, "{}", planned.file);
            assert_eq!(
                trace.grove_version, spec.grove_version.protocol_version,
                "{}",
                planned.file
            );
            assert!(
                planned.file.contains(&format!("v{}", trace.grove_version)),
                "{} is not filed under its grove version",
                planned.file
            );
            let expected_seed = match trace.name.as_str() {
                "suite" => SUITE_SEED,
                name if name.starts_with("legacy-") => HAND_WRITTEN_SEED,
                _ => SEED,
            };
            assert_eq!(trace.seed, expected_seed, "{}", planned.file);
            let workloads: Vec<&str> = trace
                .segments
                .iter()
                .map(|segment| segment.workload.as_str())
                .collect();
            let planned_workloads: Vec<&str> = spec
                .segments
                .iter()
                .map(|segment| segment.workload.as_str())
                .collect();
            assert_eq!(workloads, planned_workloads, "{}", planned.file);
            assert!(!workloads.is_empty(), "{}", planned.file);
            for segment in &trace.segments {
                assert!(segment.ops > 0, "{}: an empty stream", planned.file);
            }
            assert_eq!(
                trace.steps,
                trace
                    .segments
                    .iter()
                    .map(|segment| segment.ops)
                    .sum::<u64>(),
                "{}: a step for every operation",
                planned.file
            );
            steps += trace.steps;
            engines.insert(trace.engine);
        }
    }
    assert_eq!(engines.len(), 1, "one engine commit: {engines:?}");
    let engine = engines.first().expect("the corpus has traces");
    assert!(
        engine.len() == 40 && engine.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{engine} is not a commit"
    );
    println!("{steps} steps recorded from engine commit {engine}");
}

/// A trace quotes operations and error messages, which hold keys and values
/// of any bytes. The file holds them escaped: a control byte in it would
/// make it a binary file to the tools that show a difference.
#[test]
fn every_trace_file_is_text() {
    for scale in [Scale::Standard, Scale::Reference] {
        for planned in plan::plan(scale) {
            let path = corpus_dir().join(&planned.file);
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let control = text
                .lines()
                .enumerate()
                .find(|(_, line)| line.chars().any(char::is_control));
            assert!(
                control.is_none(),
                "{}: line {} holds a control character",
                planned.file,
                control.map_or(0, |(line, _)| line + 1)
            );
        }
    }
}

/// A generated workload applies and proves on every grove version, so its
/// traces record no failure. The hand-written cases are the ones that do,
/// on the grove versions that have the behaviour.
#[test]
fn only_the_hand_written_cases_record_a_failure() {
    let cases: BTreeSet<String> = legacy::cases()
        .into_iter()
        .map(|case| format!("legacy-{}", case.name))
        .collect();
    let mut unclean = BTreeSet::new();
    for scale in [Scale::Standard, Scale::Reference] {
        for planned in plan::plan(scale) {
            let trace = recorded(&planned.file);
            if cases.contains(&trace.name) {
                assert_eq!(
                    trace.details().count() as u64,
                    trace.steps,
                    "{}: a hand-written case is recorded in full",
                    planned.file
                );
                if !trace.tally.is_clean() {
                    unclean.insert((trace.name, trace.grove_version));
                }
            } else {
                assert_eq!(trace.tally, Tally::default(), "{}", planned.file);
            }
        }
    }
    let case = |name: &str, versions: &[u32]| -> Vec<(String, u32)> {
        versions
            .iter()
            .map(|version| (format!("legacy-{name}"), *version))
            .collect()
    };
    let expected: BTreeSet<(String, u32)> = [
        case("delete-tree-under-provable-count-tree", &[1, 2, 3]),
        case("delete-sum-tree-under-provable-count-sum-tree", &[1, 2, 3]),
        case("delete-count-tree-under-provable-sum-tree", &[1, 2, 3]),
        case(
            "delete-tree-under-provable-count-provable-sum-tree",
            &[1, 2, 3],
        ),
        case("delete-mmr-tree-under-provable-count-tree", &[1, 2, 3]),
        case(
            "delete-provable-count-provable-sum-tree-under-provable-count-provable-sum-tree",
            &[1, 2],
        ),
        case(
            "delete-provable-count-provable-sum-indexed-tree-under-provable-count-provable-sum-tree",
            &[1, 2],
        ),
        case("query-in-provable-count-provable-sum-tree", &[1, 2]),
        case("query-in-provable-sum-indexed-tree", &[1, 2]),
        case("query-in-provable-count-indexed-tree", &[1, 2]),
        case("query-in-provable-count-provable-sum-indexed-tree", &[1, 2]),
        case("reference-in-provable-count-sum-tree", &[1, 2]),
        case("move-in-batch", &[1, 2, 3]),
        case("drop-flat", &[1, 2, 3]),
        case("drop-flat-in-batch", &[1, 2, 3]),
        case("aggregate-on-range", &[1, 2]),
        case("aggregate-per-key", &[1, 2]),
        case("count-offset-page", &[1, 2]),
        case("axis-read", &[1, 2, 3]),
        case("sum-budget-window", &[1, 2, 3]),
        case("subquery-limit", &[1, 2, 3]),
        case("subquery-offset", &[1, 2, 3, 4]),
        case("offset-without-count", &[1, 2, 3, 4, 5]),
        case("aggregate-per-key-over-every-key", &[1, 2, 3, 4, 5]),
    ]
    .into_iter()
    .flatten()
    .collect();
    assert_eq!(unclean, expected);
}

/// A step holds a proof exactly when its operation is a read GroveDB
/// proves, and a root hash exactly when it is not a read. The letters of a
/// step line that stand for a proof and for a root hash are no hex digits,
/// so they are found by looking for them.
#[test]
fn every_proven_read_is_recorded_with_its_proof() {
    let (mut proofs, mut lines) = (0u64, 0u64);
    for planned in plan::plan(Scale::Standard) {
        let trace = recorded(&planned.file);
        assert_eq!(trace.segments.len(), planned.spec.segments.len());
        for (segment, spec) in trace.segments.iter().zip(&planned.spec.segments) {
            let mut ops = spec.source.ops(planned.spec.seed);
            for entry in &segment.entries {
                let Entry::Step(line) = entry else { continue };
                let op = ops.next().expect("a step for every operation");
                let proven = matches!(&op, Op::Read(read) if provable(read).is_some());
                let context = || format!("{}: `{line}` for {op:?}", planned.file);
                assert_eq!(line.contains('p'), proven, "{}", context());
                assert_eq!(line.contains('q'), proven, "{}", context());
                assert_eq!(
                    line.contains('r'),
                    !matches!(op, Op::Read(_)),
                    "{}",
                    context()
                );
                proofs += u64::from(proven);
                lines += 1;
            }
            assert!(
                ops.next().is_none(),
                "{}: an operation without a step",
                planned.file
            );
        }
    }
    assert!(proofs > 0 && lines > proofs);
    println!("{proofs} of {lines} steps hold a proof");
}

/// The value of the line of `detail` that starts with `key`.
fn field<'a>(detail: &'a Detail, key: &str) -> &'a str {
    detail
        .lines
        .iter()
        .find_map(|line| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(' '))
        })
        .unwrap_or_else(|| panic!("step {} has no `{key}` line", detail.step))
}

/// The proof of the step `detail` writes out, checked against its recorded
/// length.
fn proof(detail: &Detail) -> Vec<u8> {
    let (len, hex) = field(detail, "proof")
        .split_once(' ')
        .expect("a proof line has a length and the bytes");
    let bytes = hex::decode(hex).expect("a proof is written in hex");
    assert_eq!(len.parse::<usize>().expect("a length"), bytes.len());
    assert_eq!(field(detail, "verification"), "verified");
    bytes
}

/// The sizes of the reference workload's two named proofs, by the protocol
/// version of the grove version: the point proof of one document and the
/// proof of one hundred documents read through index references. Grove
/// versions 3 and later wrap a proof in the envelope that names its
/// version, a few bytes more.
///
/// The plan measured 2,848 and 49,835 bytes on a workload this one was
/// modelled on. They are not these: see `corpus/README.md`.
const NAMED_PROOF_SIZES: &[(u32, usize, usize)] = &[
    (1, 2222, 52545),
    (2, 2222, 52545),
    (3, 2227, 52552),
    (4, 2227, 52552),
    (5, 2227, 52552),
];

#[test]
fn the_named_proofs_are_in_the_corpus() {
    let documents = Documents::reference();
    let point_query = documents
        .point_query(SEED, REFERENCE_DOCUMENT)
        .to_path_query();
    let owner_query = documents
        .owner_query(SEED, REFERENCE_OWNER, REFERENCE_OWNER_LIMIT)
        .to_path_query();
    let mut roots = BTreeSet::new();
    let mut sizes = Vec::new();
    for grove_version in GROVE_VERSIONS {
        let version = grove_version.protocol_version;
        let trace = recorded(&format!("reference/v{version}.trace"));
        let [stream, named] = trace.segments.as_slice() else {
            panic!("the reference trace has two segments");
        };
        assert_eq!(stream.workload, "documents-reference");
        assert_eq!(
            stream.ops, 1_050_008,
            "a million documents, twenty to a block"
        );
        assert_eq!(named.workload, NAMED_PROOFS);
        assert_eq!(trace.steps, stream.ops + 3);
        let details: Vec<&Detail> = trace.details().collect();
        let [commit, point, owner] = details.as_slice() else {
            panic!("the named proofs are a commit and two queries, in full");
        };
        let root: [u8; 32] = hex::decode(field(commit, "root"))
            .expect("a root hash is written in hex")
            .try_into()
            .expect("a root hash has 32 bytes");
        roots.insert(root);

        let point_proof = proof(point);
        let (proved_root, rows) = GroveDb::verify_query(&point_proof, &point_query, grove_version)
            .expect("the point proof verifies");
        assert_eq!(proved_root, root, "grove version {version}");
        let [(path, key, Some(Element::Item(body, _)))] = rows.as_slice() else {
            panic!("the point proof proves one document: {rows:?}");
        };
        assert_eq!(*path, documents.primary_path(SEED));
        assert_eq!(*key, documents.document_id(SEED, REFERENCE_DOCUMENT));
        assert_eq!(body.len(), 400);
        assert_eq!(field(point, "rows"), "1");

        let owner_proof = proof(owner);
        let (proved_root, rows) = GroveDb::verify_query(&owner_proof, &owner_query, grove_version)
            .expect("the owner proof verifies");
        assert_eq!(proved_root, root, "grove version {version}");
        assert_eq!(rows.len(), 100);
        for (path, key, element) in &rows {
            assert_eq!(
                *path,
                documents.owner_references_path(SEED, REFERENCE_OWNER)
            );
            assert_eq!(key.len(), 32);
            assert!(
                matches!(element, Some(Element::Item(body, _)) if body.len() == 400),
                "a reference is proved as the document it points at"
            );
        }
        assert_eq!(field(owner, "rows"), "100");
        sizes.push((version, point_proof.len(), owner_proof.len()));
    }
    assert_eq!(roots.len(), 1, "every grove version builds the same state");
    assert_eq!(
        sizes, NAMED_PROOF_SIZES,
        "the named proofs are now {sizes:?}"
    );
}
