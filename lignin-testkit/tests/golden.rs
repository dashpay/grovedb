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
//! - The corpus holds the storage fees Drive's callbacks split by owner and
//!   epoch: `the_storage_fee_splits_are_in_the_corpus`.
//! - The corpus holds the charge for a deleted tree in each of the states
//!   its transaction can find it in:
//!   `the_charge_for_a_deleted_tree_is_in_the_corpus`.
//! - The files are text: `every_trace_file_is_text`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

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
    workload::{adversarial::TreeDeletes, drive::Documents, BatchOp, Features, Op, Write},
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
        case("limit-over-subqueries", &[1, 2, 3]),
        case("capped-tree-rows", &[1, 2, 3, 4]),
        case("limit-over-empty-subtrees", &[1, 2]),
        case("branch-without-subquery", &[1, 2, 3, 4]),
        case("filled-tree-claims-a-sum", &[1, 2, 3, 4, 5]),
        case("filled-tree-axes-out-of-order", &[1, 2, 3, 4, 5]),
        case("filled-tree-wrapped", &[1, 2, 3, 4, 5]),
        case("filled-tree-entry-of-indexed-tree", &[1, 2, 3, 4, 5]),
        case("flags-of-another-length", &[1, 2, 3]),
        case("full-handover", &[1, 2, 3, 4]),
        case("ranking-drift", &[1, 2, 3, 4]),
        case("convert-twice", &[1, 2, 3, 4, 5]),
        case("convert-without-a-counterpart", &[1, 2, 3, 4, 5]),
        case("convert-what-is-not-there", &[1, 2, 3, 4, 5]),
        case("convert-before-its-parent", &[1, 2, 3, 4, 5]),
        case("convert-with-a-wide-child", &[1, 2, 3, 4, 5]),
        case("convert-claimed-sum", &[1, 2, 3, 4, 5]),
        case("convert-key-too-long-to-rank", &[1, 2, 3, 4, 5]),
        case("convert-rows-of-both-signs", &[1, 2, 3, 4]),
        case("empty-indexed-tree-row", &[1, 2]),
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

/// The traces of the two workloads that apply their batches with Drive's
/// storage-flag callbacks write out, on every grove version, a cost that
/// splits freed bytes over several epochs of one owner: by a batch, by a
/// partial batch and by each of the two deletes that take the callback. For
/// a batch they also write out one that frees nothing and one that names
/// one epoch. A batch that updates a document is written out in both, and
/// where a grove version takes a full handover, so is one that frees the
/// element it replaces across its epochs.
#[test]
fn the_storage_fee_splits_are_in_the_corpus() {
    // The epochs of each owner a cost names freed bytes by.
    let split = |detail: &Detail| -> Vec<usize> {
        let (_, removed) = field(detail, "cost")
            .split_once("removed=")
            .expect("a cost names what was freed");
        let removed = removed.split(' ').next().expect("the freed bytes");
        let Some(sections) = removed.strip_prefix('{') else {
            return Vec::new();
        };
        let mut owners: Vec<&str> = sections
            .trim_end_matches('}')
            .split(',')
            .map(|section| section.split_once('/').expect("an owner and an epoch").0)
            .collect();
        owners.sort_unstable();
        owners
            .chunk_by(|one, other| one == other)
            .map(<[&str]>::len)
            .collect()
    };
    let kinds = [
        "FlagsBatch",
        "FlagsPartialBatch",
        "Write(SectionalDelete ",
        "Write(SectionalDeleteUpTree ",
    ];
    for grove_version in GROVE_VERSIONS {
        let version = grove_version.protocol_version;
        for (workload, kinds) in [("storage-fees", &kinds[..1]), ("flag-epochs", &kinds[..])] {
            let trace = recorded(&format!("v{version}/{workload}.trace"));
            for kind in kinds {
                let splits: Vec<Vec<usize>> = trace
                    .details()
                    .filter(|detail| detail.remarks[0].starts_with(kind))
                    .map(split)
                    .collect();
                let has = |wanted: fn(&Vec<usize>) -> bool| splits.iter().any(wanted);
                let batch = *kind == "FlagsBatch";
                assert!(
                    has(|epochs| epochs.iter().any(|epochs| *epochs > 1))
                        && (!batch
                            || (has(|epochs| epochs.is_empty()) && has(|epochs| epochs == &[1]))),
                    "v{version}/{workload}: {kind} frees {splits:?}"
                );
            }
            let handovers: Vec<Vec<usize>> = trace
                .details()
                .filter(|detail| detail.remarks[0].starts_with("FlagsHandoverBatch"))
                .map(split)
                .collect();
            let frees_across_epochs = handovers
                .iter()
                .any(|epochs| epochs.iter().any(|epochs| *epochs > 1));
            let full_handovers = Features::for_grove_version(grove_version).full_handovers;
            assert!(
                !handovers.is_empty()
                    && (workload != "flag-epochs" || frees_across_epochs == full_handovers),
                "v{version}/{workload}: a handover frees {handovers:?}"
            );
        }
    }
}

/// The `tree-deletes` trace writes out every step, so the costs of deleting
/// a tree in each of its three states stand next to each other. They show
/// that what a deleted tree held is charged by the committed state: the
/// tree the transaction rewrote frees what its untouched twin frees, where
/// only values were replaced, and loads more, and the tree the transaction
/// filled frees less than either and loads less.
#[test]
fn the_charge_for_a_deleted_tree_is_in_the_corpus() {
    // The bytes a cost names as freed and as loaded.
    let cost = |detail: &Detail| -> (u64, u64) {
        let number = |text: &str| text.parse::<u64>().expect("a number of bytes");
        let cost = field(detail, "cost");
        let value = |key: &str| {
            let (_, rest) = cost.split_once(key).expect("a cost names it");
            rest.split(' ').next().expect("a value")
        };
        let removed = value("removed=");
        let freed = match removed.strip_prefix('{') {
            None => number(removed),
            Some(sections) => sections
                .trim_end_matches('}')
                .split(',')
                .map(|section| number(section.split_once('=').expect("bytes").1))
                .sum(),
        };
        (freed, number(value("loaded=")))
    };
    let workload = TreeDeletes::default();
    for grove_version in GROVE_VERSIONS {
        let version = grove_version.protocol_version;
        let trace = recorded(&format!("v{version}/tree-deletes.trace"));
        let ops: Vec<Op> = workload.stream(SEED).collect();
        let details: Vec<&Detail> = trace.details().collect();
        assert_eq!(details.len(), ops.len(), "every step is written out");
        let mut costs: BTreeMap<(String, String), [(u64, u64); 3]> = BTreeMap::new();
        for (op, detail) in ops.iter().zip(details) {
            let key = match op {
                Op::Write(Write::Delete { key, .. } | Write::SectionalDelete { key, .. }) => key,
                Op::Batch(ops) | Op::FlagsBatch(ops) => match ops.as_slice() {
                    [BatchOp::DeleteTree { key, .. }] => key,
                    _ => continue,
                },
                _ => continue,
            };
            let name = String::from_utf8(key.clone()).expect("a tree is named in text");
            let (state, rest) = name.split_once('-').expect("a state");
            let states = ["fresh", "rewritten", "untouched"];
            let state = states.iter().position(|known| *known == state);
            let state = state.expect("one of the three states");
            costs.entry(split_way(rest)).or_default()[state] = cost(detail);
        }
        assert_eq!(costs.len(), 4 * workload.rounds as usize);
        for ((way, round), [fresh, rewritten, untouched]) in costs {
            let context = format!("v{version}/tree-deletes: {way}, round {round}");
            assert!(
                fresh.0 > 0 && rewritten.0 > 0 && untouched.0 > 0,
                "{context}: a tree in each state"
            );
            // The two trees hold records of the same lengths, so a charge
            // by what the transaction sees would be the same for both, but
            // for the four bytes their own keys differ by.
            assert!(
                fresh.0 * 2 < untouched.0 && fresh.1 < untouched.1,
                "{context}"
            );
            assert!(rewritten.1 != untouched.1, "{context}");
            if round == "0" || round == "2" {
                assert_eq!(rewritten.0, untouched.0, "{context}");
            } else {
                assert!(rewritten.0 < untouched.0, "{context}");
            }
        }
    }
}

/// The way a tree of `tree-deletes` is deleted and its round, from the
/// rest of its key.
fn split_way(rest: &str) -> (String, String) {
    let (way, round) = rest.rsplit_once('-').expect("a way and a round");
    (way.to_string(), round.to_string())
}

/// The steps of `trace` that are written out in full and hold a proof, each
/// with the rows it returned.
fn proven_pages(trace: &Trace) -> Vec<(Vec<String>, Vec<u8>)> {
    trace
        .details()
        .filter(|detail| detail.lines.iter().any(|line| line.starts_with("proof ")))
        .map(|detail| {
            let rows = detail
                .lines
                .iter()
                .filter(|line| line.starts_with("row "))
                .cloned()
                .collect();
            (rows, proof(detail))
        })
        .collect()
}

/// The corpus holds the same pages of an index proven by grove version 4
/// and by grove version 5: the rows are the same and, where the limit of a
/// query node ends the layer before its last key, the proof of grove
/// version 5 is the shorter one. A limit of the whole query that is spent
/// first leaves the two proofs the same. `tests/upstream.rs` shows the same
/// for the pages of `capped-layers` on the engine.
#[test]
fn the_layer_a_limit_ends_early_is_in_the_corpus() {
    let pages =
        |version: u32, name: &str| proven_pages(&recorded(&format!("v{version}/{name}.trace")));
    let (full, cut) = (
        pages(4, "legacy-limit-over-subqueries"),
        pages(5, "legacy-limit-over-subqueries"),
    );
    let lengths = |pages: &[(Vec<String>, Vec<u8>)]| -> Vec<usize> {
        pages.iter().map(|(_, proof)| proof.len()).collect()
    };
    assert_eq!(full.len(), 5, "five pages");
    for ((rows, _), (rows_now, _)) in full.iter().zip(&cut) {
        assert_eq!(rows, rows_now);
        assert!(!rows.is_empty());
    }
    let (full, cut) = (lengths(&full), lengths(&cut));
    for page in [0, 1, 3, 4] {
        assert!(cut[page] < full[page], "page {page}: {cut:?} and {full:?}");
    }
    assert_eq!(
        cut[2], full[2],
        "the limit of the whole query ends the layer"
    );
}

/// The corpus holds the conversion of each of the seven trees as grove
/// version 5 makes it and as the versions before it refuse it. In
/// `convert-twice` grove version 5 applies the first conversion of each
/// tree and refuses every later one, step by step, and grove versions 1 to
/// 4 refuse them all for having no such operation.
#[test]
fn the_conversions_are_in_the_corpus() {
    let conversions = |version: u32| -> Vec<Option<String>> {
        recorded(&format!("v{version}/legacy-convert-twice.trace"))
            .details()
            .filter(|detail| {
                detail
                    .remarks
                    .iter()
                    .any(|remark| remark.starts_with("Write(ConvertToBigSumTree"))
            })
            .map(|detail| {
                detail
                    .lines
                    .iter()
                    .find_map(|line| line.strip_prefix("failed "))
                    .map(str::to_string)
            })
            .collect()
    };
    // The seven trees in turn, each converted and every second one asked
    // again at once, then all seven in a later block, then all seven given
    // no transaction.
    let refused = Some("InvalidInput".to_string());
    let mut expected = Vec::new();
    for tree in 0..7 {
        expected.push(None);
        if tree % 2 == 0 {
            expected.push(refused.clone());
        }
    }
    expected.extend(std::iter::repeat_n(refused, 14));
    assert_eq!(conversions(5), expected);
    for version in 1..=4 {
        let refused = conversions(version);
        assert_eq!(refused.len(), 25);
        assert!(
            refused
                .iter()
                .all(|error| { error.as_deref() == Some("VersionError::UnknownVersionMismatch") }),
            "grove version {version}: {refused:?}"
        );
    }
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
