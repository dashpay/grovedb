//! Acceptance tests for the workload generators (BASE-01).
//!
//! - The same seed gives the same stream on any machine:
//!   `streams_are_pinned`, `a_suite_is_pinned_for_every_grove_version` and
//!   `the_reference_documents_stream_is_pinned_and_lazy` compare stream
//!   digests with values written down here.
//! - Every `Element` variant and `TreeType` is produced at least once:
//!   `the_suite_produces_every_element_variant_and_tree_type`. The lists it
//!   checks against come from an exhaustive `match`, so a variant added to
//!   GroveDB stops the crate compiling until it is named and then fails that
//!   test until a workload produces it.
//! - The streams are ones GroveDB accepts:
//!   `every_workload_applies_on_every_grove_version`.

use std::{collections::BTreeSet, thread};

use grovedb::{GroveDb, PathQuery};
use grovedb_commitment_tree::{ciphertext_payload_size, DashMemo};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::workload::{
    apply,
    coverage::{ELEMENT_VARIANTS, REFERENCE_PATH_TYPES, TREE_TYPES},
    drive::Documents,
    zoo::{ElementZoo, COMMITMENT_PAYLOAD_LEN},
    Coverage, Features, Op, Outcome, Read, StreamDigest, Workload,
};

const SEED: u64 = 1;
const OTHER_SEED: u64 = 0x4C49_474E_494E;

/// Deep nesting recurses once per level inside GroveDB, further than a test
/// thread's stack allows in an unoptimized build.
const STACK_BYTES: usize = 64 << 20;

fn digest(ops: impl Iterator<Item = Op>) -> (u64, String) {
    let mut digest = StreamDigest::new();
    for op in ops {
        digest.update(&op);
    }
    (digest.ops(), digest.hex())
}

/// Applies a stream to `db`, one transaction per block, and returns how many
/// operations it applied. Panics on the first refused operation, on a read
/// that does not follow a commit, and on a query whose proof does not verify
/// to the rows the query returned.
fn run(
    name: &str,
    db: &GroveDb,
    ops: impl Iterator<Item = Op>,
    grove_version: &GroveVersion,
) -> u64 {
    let mut transaction = db.start_transaction();
    let mut applied = 0u64;
    let mut committed = true;
    for (index, op) in ops.enumerate() {
        if matches!(op, Op::Read(_)) {
            assert!(
                committed,
                "{name}: read {index} does not follow a commit: {op:?}"
            );
        }
        if std::env::var_os("WORKLOAD_TRACE").is_some() {
            eprintln!(
                "{name} v{} #{index}: {op:?}",
                grove_version.protocol_version
            );
        }
        let outcome = apply(db, &op, Some(&transaction), grove_version)
            .unwrap()
            .unwrap_or_else(|error| {
                panic!(
                    "{name} on grove v{}: op {index} refused: {error:?}\n{op:?}",
                    grove_version.protocol_version
                )
            });
        match (&op, outcome) {
            (Op::Commit, Outcome::EndOfBlock) => {
                db.commit_transaction(transaction).unwrap().expect("commit");
                transaction = db.start_transaction();
                committed = true;
            }
            (Op::Read(Read::Query(query)), Outcome::Rows(rows)) => {
                prove(
                    name,
                    index,
                    db,
                    &query.to_path_query(),
                    rows.len(),
                    grove_version,
                );
            }
            (Op::Read(_), _) => {}
            _ => committed = false,
        }
        applied += 1;
    }
    assert!(committed, "{name}: the stream does not end on a commit");
    applied
}

/// Paths `verify_grovedb` reports. Grove versions 1 to 4 leave orphan rows
/// behind when an entry of a count-indexed tree is deleted, so this is
/// printed, not asserted.
fn report_verify_issues(name: &str, db: &GroveDb, grove_version: &GroveVersion) {
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify");
    if !issues.is_empty() {
        println!(
            "{name} on grove v{}: verify_grovedb reports {} paths",
            grove_version.protocol_version,
            issues.len(),
        );
    }
}

fn open() -> (tempfile::TempDir, GroveDb) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = GroveDb::open(dir.path()).expect("open");
    (dir, db)
}

/// Proves a query against committed state and checks the proof verifies to
/// the same number of rows.
fn prove(
    name: &str,
    index: usize,
    db: &GroveDb,
    path_query: &PathQuery,
    rows: usize,
    grove_version: &GroveVersion,
) {
    let proof = db
        .prove_query(path_query, None, grove_version)
        .unwrap()
        .unwrap_or_else(|error| {
            panic!(
                "{name} on grove v{}: query {index} not provable: {error:?}\n{path_query:?}",
                grove_version.protocol_version
            )
        });
    let (root_hash, proved) = GroveDb::verify_query(&proof, path_query, grove_version)
        .unwrap_or_else(|error| {
            panic!(
                "{name} on grove v{}: proof of query {index} does not verify: {error:?}\n{path_query:?}",
                grove_version.protocol_version
            )
        });
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash"),
        "{name}: proof of query {index} verifies to another root"
    );
    assert_eq!(
        proved.len(),
        rows,
        "{name}: query {index} proves another row count"
    );
}

#[test]
fn every_workload_applies_on_every_grove_version() {
    let only = std::env::var("WORKLOAD_ONLY").ok();
    let only = only.as_deref();
    thread::scope(|scope| {
        for grove_version in GROVE_VERSIONS {
            thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, move || {
                    let version = grove_version.protocol_version;
                    let suite = Workload::suite(Features::for_grove_version(grove_version));
                    let (_shared_dir, shared) = open();
                    for workload in suite {
                        let name = workload.name();
                        if only.is_some_and(|only| only != name) {
                            continue;
                        }
                        let (_dir, db) = open();
                        let applied = run(name, &db, workload.stream(SEED), grove_version);
                        report_verify_issues(name, &db, grove_version);
                        println!("grove v{version} {name}: {applied} operations applied");
                        run(name, &shared, workload.stream(OTHER_SEED), grove_version);
                    }
                    report_verify_issues("suite", &shared, grove_version);
                })
                .expect("spawn");
        }
    });
}

#[test]
fn the_suite_produces_every_element_variant_and_tree_type() {
    assert_eq!(
        Features::for_grove_version(GroveVersion::latest()),
        Features::ALL,
        "the suite applied to the latest grove version is not the one checked here"
    );
    let mut coverage = Coverage::new();
    for workload in Workload::suite(Features::ALL) {
        for op in workload.stream(SEED) {
            coverage.record(&op);
        }
    }
    assert_eq!(coverage.missing_elements(), Vec::<&str>::new());
    assert_eq!(coverage.missing_tree_types(), Vec::<&str>::new());
    assert_eq!(coverage.missing_reference_paths(), Vec::<&str>::new());
    println!(
        "{} element variants: {:?}",
        coverage.elements().len(),
        coverage.elements()
    );
    println!(
        "{} tree types: {:?}",
        coverage.tree_types().len(),
        coverage.tree_types()
    );
    println!(
        "{} reference path types: {:?}",
        coverage.reference_paths().len(),
        coverage.reference_paths()
    );

    let zoo: Vec<Op> = ElementZoo {
        features: Features::ALL,
    }
    .stream(OTHER_SEED)
    .collect();
    let zoo = Coverage::of(&zoo);
    assert_eq!(zoo.missing_elements(), Vec::<&str>::new());
    assert_eq!(zoo.missing_tree_types(), Vec::<&str>::new());
}

#[test]
fn coverage_reports_what_a_stream_lacks() {
    let empty = Coverage::new();
    assert_eq!(empty.missing_elements(), ELEMENT_VARIANTS);
    assert_eq!(empty.missing_tree_types(), TREE_TYPES);
    assert_eq!(empty.missing_reference_paths(), REFERENCE_PATH_TYPES);

    let documents: Vec<Op> = Documents::default().stream(SEED).collect();
    let documents = Coverage::of(&documents);
    assert_eq!(
        documents.elements().iter().copied().collect::<Vec<_>>(),
        ["Item", "Reference", "Tree"]
    );
    assert_eq!(
        documents.tree_types().iter().copied().collect::<Vec<_>>(),
        ["NormalTree"]
    );
    assert!(documents.missing_elements().contains(&"MmrTree"));

    for names in [ELEMENT_VARIANTS, TREE_TYPES, REFERENCE_PATH_TYPES] {
        let unique: BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
    }
}

/// The digest of every workload's stream, for two seeds, generated for
/// [`Features::ALL`]: name, seed, operations, digest.
#[rustfmt::skip]
const PINNED_STREAMS: &[(&str, u64, u64, &str)] = &[
    ("documents", 0x1, 1208, "2f0b270f7c78d34a47427df22c0146dd01b5b1998ce89c40f0cf9673d2430c46"),
    ("documents", 0x4c49474e494e, 1208, "da97d89975cde193292d60e4da87ca4b2b374b97fdb298bd25425fc6a96c7611"),
    ("balances", 0x1, 963, "2b9f7555dbb9f1c251eb2f21d5c403e4e95dd002b358cf8031d9c81a0e3a1089"),
    ("balances", 0x4c49474e494e, 963, "ba5e4bf2840006c2d4f974bce9ef371022e6f2906485b700b2d2d2cb638ed6ea"),
    ("tokens", 0x1, 1013, "58f0e7fae2cd195300ad77aa20852d4e25c2f71d8184effc69a7730b3fbd8cda"),
    ("tokens", 0x4c49474e494e, 1013, "fce0ab0f2bd09d1056d27e7c46b395ac3353e51d08d6fffbe4b4382abf81f093"),
    ("element-zoo", 0x1, 2016, "50ac5186570445b60f2ebfc7ebac0b63c1704cb7f06a50c8a9148be25dd19364"),
    ("element-zoo", 0x4c49474e494e, 2025, "b81afd3b23f6b590266b5790d0abf4a7b2a17e8b3fd31ef031c345e012e09d02"),
    ("deep-nesting", 0x1, 377, "6461d8f8cf75990e32746b1e6f2bcbb5838bf1226e33ba916c9c53372146ea8a"),
    ("deep-nesting", 0x4c49474e494e, 377, "9ff40c4595ee15f9fd0111466e67b7c08de4391489b3f2a1239bb487f2c09d21"),
    ("long-keys", 0x1, 448, "8cabc4d420d4ec8a4a4c19c3cef115d06c07013aba75b78db11089cc6eb86560"),
    ("long-keys", 0x4c49474e494e, 511, "6d8c10161534851f5ec2c60573a9cac23265ef3dd61700913bcb1c711e5576db"),
    ("large-values", 0x1, 192, "1a72c05021d10874a99ca486d54a78d6e7319289088065d2ccfbc7b4d33cf483"),
    ("large-values", 0x4c49474e494e, 190, "620f3f31df09f43d601e055c37e694149b0763398263b798e10965444e7cb0d7"),
    ("tiny-subtrees", 0x1, 3640, "155c4fb8e06d7c8510b50dce0c7d158752648d0fbf3de2e67c5fc7b1e46c14c3"),
    ("tiny-subtrees", 0x4c49474e494e, 3710, "708de79761442cf04386ef9054fcdd0ea50aa034f3eabb1fd7c8d3e2ad57c437"),
    ("delete-heavy", 0x1, 6246, "7351393ef5fd04ef9b32b0a145e858465707c0cb7529a149c0be595b7278da26"),
    ("delete-heavy", 0x4c49474e494e, 6246, "2e9de83aa09c295c2cba188785c401fb2ba19136860685595a46c594ec411531"),
    ("reference-chains", 0x1, 460, "1b4d07ee4ad1546f26e20cb507a8adacb18a371fc4f8bc6acd072a8acd4893ff"),
    ("reference-chains", 0x4c49474e494e, 460, "cea5f6f76c7d507a4ca622cc818dd28484ce702856e6e2bee4e5a9661e818f5a"),
];

#[test]
fn streams_are_pinned() {
    let mut actual = Vec::new();
    for workload in Workload::suite(Features::ALL) {
        for seed in [SEED, OTHER_SEED] {
            let (ops, hex) = digest(workload.stream(seed));
            assert_eq!(
                (ops, hex.clone()),
                digest(workload.stream(seed)),
                "{} gives two streams for seed {seed}",
                workload.name()
            );
            actual.push((workload.name(), seed, ops, hex));
        }
    }
    let actual: String = actual
        .iter()
        .map(|(name, seed, ops, hex)| format!("    ({name:?}, {seed:#x}, {ops}, {hex:?}),\n"))
        .collect();
    let expected: String = PINNED_STREAMS
        .iter()
        .map(|(name, seed, ops, hex)| format!("    ({name:?}, {seed:#x}, {ops}, {hex:?}),\n"))
        .collect();
    assert!(actual == expected, "the streams are now:\n{actual}");
}

/// The digest of the whole suite generated for each grove version's
/// features, seed [`SEED`]: protocol version, operations, digest.
#[rustfmt::skip]
const PINNED_SUITES: &[(u32, u64, &str)] = &[
    (1, 16116, "0e0bde9030970934d7585f20410a52ec40023dcddc0b185a5dd56610693e719e"),
    (2, 16116, "0e0bde9030970934d7585f20410a52ec40023dcddc0b185a5dd56610693e719e"),
    (3, 16116, "94af5e158ced5369571d9985fcc7f153a4053f97d2e59392fd6966fc56027b2d"),
    (4, 16335, "7d8f0d45679d1619574a979b9de3ab95fd651596f54ae6d255dacee9d00498e4"),
    (5, 16563, "1cd25fb7266ae89727ef5c964f9654f92ddda6ae018bef1e02b7c364341dc90d"),
];

#[test]
fn a_suite_is_pinned_for_every_grove_version() {
    let actual: String = GROVE_VERSIONS
        .iter()
        .map(|grove_version| {
            let suite = Workload::suite(Features::for_grove_version(grove_version));
            let (ops, hex) = digest(suite.iter().flat_map(|workload| workload.stream(SEED)));
            format!(
                "    ({}, {ops}, {hex:?}),\n",
                grove_version.protocol_version
            )
        })
        .collect();
    let expected: String = PINNED_SUITES
        .iter()
        .map(|(version, ops, hex)| format!("    ({version}, {ops}, {hex:?}),\n"))
        .collect();
    assert!(actual == expected, "the suites are now:\n{actual}");
}

#[test]
fn seeds_and_workloads_give_different_streams() {
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    for workload in Workload::suite(Features::ALL) {
        assert!(names.insert(workload.name()), "two workloads share a name");
        for seed in [0, SEED, 2, OTHER_SEED, u64::MAX] {
            let (_, hex) = digest(workload.stream(seed));
            assert!(
                seen.insert(hex),
                "{} repeats a stream at seed {seed}",
                workload.name()
            );
        }
    }
}

/// The first operations of the million-document stream the plan measures.
const REFERENCE_PREFIX_OPS: usize = 50_000;
const PINNED_REFERENCE_PREFIX: &str =
    "e212500dcfe1a4f8d491e8b0cfae9f19db17de9c7fcbaebed13d38037857f8e9";

#[test]
fn the_reference_documents_stream_is_pinned_and_lazy() {
    let reference = Documents::reference();
    assert_eq!(reference.documents, 1_000_000);
    assert_eq!(reference.document_size, 400);
    let (ops, hex) = digest(reference.stream(SEED).take(REFERENCE_PREFIX_OPS));
    assert_eq!(ops, REFERENCE_PREFIX_OPS as u64);
    assert_eq!(hex, PINNED_REFERENCE_PREFIX);
}

#[test]
fn the_commitment_payload_length_matches_grovedb() {
    assert_eq!(
        COMMITMENT_PAYLOAD_LEN as usize,
        ciphertext_payload_size::<DashMemo>()
    );
}
