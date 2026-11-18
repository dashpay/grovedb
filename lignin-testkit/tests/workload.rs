//! Acceptance tests for the workload generators (BASE-01).
//!
//! - The same seed gives the same stream on any machine:
//!   `streams_are_pinned`, `a_suite_is_pinned_for_every_grove_version` and
//!   `the_reference_documents_stream_is_pinned` compare stream
//!   digests with values written down here.
//! - Every `Element` variant and `TreeType` is produced at least once:
//!   `the_suite_produces_every_element_variant_and_tree_type`. The lists it
//!   checks against come from an exhaustive `match`, so a variant added to
//!   GroveDB stops the crate compiling until it is named and then fails that
//!   test until a workload produces it.
//! - The streams are ones GroveDB accepts:
//!   `every_workload_applies_on_every_grove_version`.

use std::{collections::BTreeSet, thread};

use grovedb::{query_result_type::PathKeyElementTrio, Element, GroveDb};
use grovedb_commitment_tree::{ciphertext_payload_size, DashMemo};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::workload::{
    adversarial::{
        DeepNesting, DeleteHeavy, LargeValues, LongKeys, ReferenceChains, TinySubtrees,
        MAX_NESTING_DEPTH,
    },
    apply,
    coverage::{ELEMENT_VARIANTS, REFERENCE_PATH_TYPES, TREE_TYPES},
    drive::{Balances, Documents, Tokens},
    zoo::{ElementZoo, COMMITMENT_PAYLOAD_LEN},
    Coverage, Features, Op, Outcome, Query, Read, StreamDigest, Workload,
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
                    query,
                    rows.to_path_key_elements(),
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

/// Proves a query against committed state and checks that the proof
/// verifies to the database's root hash and to the rows the query returned:
/// the same paths, keys and elements in the same order.
///
/// A proof carries the value a reference points at, never the reference, so
/// a raw query's reference rows are compared with what `get` resolves them
/// to.
fn prove(
    name: &str,
    index: usize,
    db: &GroveDb,
    query: &Query,
    rows: Vec<PathKeyElementTrio>,
    grove_version: &GroveVersion,
) {
    let version = grove_version.protocol_version;
    let path_query = query.to_path_query();
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap_or_else(|error| {
            panic!("{name} on grove v{version}: query {index} not provable: {error:?}\n{query:?}")
        });
    let refused = |error| -> ! {
        panic!(
            "{name} on grove v{version}: proof of query {index} does not verify: {error:?}\n{query:?}"
        )
    };
    let (root_hash, proved): (_, Vec<PathKeyElementTrio>) = if query.raw {
        let (root_hash, proved) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .unwrap_or_else(|error| refused(error));
        let proved = proved
            .into_iter()
            .map(|row| {
                let element = Element::deserialize(&row.value, grove_version)
                    .expect("a proved value is an element");
                (row.path, row.key, element)
            })
            .collect();
        (root_hash, proved)
    } else {
        let (root_hash, proved) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .unwrap_or_else(|error| refused(error));
        let proved = proved
            .into_iter()
            .map(|(path, key, element)| (path, key, element.expect("a proved row has a value")))
            .collect();
        (root_hash, proved)
    };
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash"),
        "{name} on grove v{version}: proof of query {index} verifies to another root"
    );
    let expected: Vec<PathKeyElementTrio> = rows
        .into_iter()
        .map(|(path, key, element)| {
            let element = if element.underlying().is_reference() {
                db.get(path.as_slice(), &key, None, grove_version)
                    .unwrap()
                    .expect("a stored reference resolves")
            } else {
                element
            };
            (path, key, element)
        })
        .collect();
    assert!(
        proved == expected,
        "{name} on grove v{version}: query {index} proves other rows\n{query:?}\nproved: \
         {proved:?}\nexpected: {expected:?}"
    );
}

/// Runs `body` once per grove version, each on its own thread.
fn on_every_grove_version(body: impl Fn(&'static GroveVersion) + Sync) {
    let body = &body;
    thread::scope(|scope| {
        for grove_version in GROVE_VERSIONS {
            thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, move || body(grove_version))
                .expect("spawn");
        }
    });
}

#[test]
fn every_workload_applies_on_every_grove_version() {
    let only = std::env::var("WORKLOAD_ONLY").ok();
    let only = only.as_deref();
    on_every_grove_version(|grove_version| {
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
    });
}

/// The zoo's second pass picks what to replace, refresh and delete by seed,
/// so each seed meets other pairs of parent and child. `WORKLOAD_SEEDS` sets
/// how many seeds to try.
#[test]
fn the_zoo_applies_for_many_seeds() {
    let seeds: u64 = std::env::var("WORKLOAD_SEEDS")
        .ok()
        .map_or(12, |seeds| seeds.parse().expect("a number of seeds"));
    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        for seed in 2..2 + seeds {
            let (_dir, db) = open();
            let name = format!("element-zoo seed {seed}");
            run(
                &name,
                &db,
                ElementZoo { features }.stream(seed),
                grove_version,
            );
        }
    });
}

#[test]
fn the_smallest_workloads_apply() {
    let features = Features::ALL;
    let workloads = [
        Workload::Documents(Documents {
            documents: 1,
            owners: 1,
            deletes: 1,
            updates: 1,
            ..Documents::default()
        }),
        Workload::Documents(Documents {
            documents: 7,
            owners: u64::MAX,
            deletes: 7,
            transitions_per_block: 1,
            ..Documents::default()
        }),
        Workload::Balances(Balances {
            identities: 1,
            transfers: 5,
            ..Balances::default()
        }),
        Workload::Tokens(Tokens {
            tokens: 1,
            holders: 1,
            actions: 30,
            ..Tokens::default()
        }),
        Workload::DeepNesting(DeepNesting {
            depth: 1,
            chains: 4,
            leaves: 1,
        }),
        Workload::DeepNesting(DeepNesting {
            depth: 2,
            chains: 0,
            leaves: 1,
        }),
        Workload::LongKeys(LongKeys {
            keys: 0,
            nesting: 0,
        }),
        Workload::LongKeys(LongKeys {
            keys: 1,
            nesting: 1,
        }),
        Workload::LargeValues(LargeValues {
            max_value_size: 0,
            random_values: 0,
        }),
        Workload::TinySubtrees(TinySubtrees {
            subtrees: 0,
            features,
        }),
        Workload::TinySubtrees(TinySubtrees {
            subtrees: 1,
            features,
        }),
        Workload::DeleteHeavy(DeleteHeavy {
            keys: 1,
            rounds: 0,
            features,
        }),
        Workload::ReferenceChains(ReferenceChains {
            chains: 0,
            features,
        }),
    ];
    let grove_version = GroveVersion::latest();
    for workload in workloads {
        for seed in [0, u64::MAX] {
            let (_dir, db) = open();
            run(workload.name(), &db, workload.stream(seed), grove_version);
        }
    }
}

#[test]
fn the_deepest_nesting_applies_on_every_grove_version() {
    on_every_grove_version(|grove_version| {
        let deepest = DeepNesting {
            depth: MAX_NESTING_DEPTH,
            chains: 4,
            leaves: 2,
        };
        let (_dir, db) = open();
        run("deepest-nesting", &db, deepest.stream(SEED), grove_version);
    });
}

#[test]
fn the_largest_parameters_do_not_overflow() {
    let tokens = Tokens {
        tokens: 1,
        holders: u64::MAX,
        actions: 200,
        ..Tokens::default()
    };
    let (_dir, db) = open();
    run("tokens", &db, tokens.stream(SEED), GroveVersion::latest());
}

/// A benchmark can compare a workload with and without its reads: the
/// writes are the same.
#[test]
fn reads_do_not_change_the_writes() {
    fn writes(workload: Workload) -> (u64, String) {
        digest(
            workload
                .stream(SEED)
                .filter(|op| !matches!(op, Op::Read(_))),
        )
    }
    let pairs = [
        (
            Workload::Documents(Documents::default()),
            Workload::Documents(Documents {
                reads_per_block: 0,
                ..Documents::default()
            }),
        ),
        (
            Workload::Balances(Balances::default()),
            Workload::Balances(Balances {
                reads_per_block: 0,
                ..Balances::default()
            }),
        ),
        (
            Workload::Tokens(Tokens::default()),
            Workload::Tokens(Tokens {
                reads_per_block: 0,
                ..Tokens::default()
            }),
        ),
    ];
    for (with_reads, without_reads) in pairs {
        assert!(with_reads.stream(SEED).any(|op| matches!(op, Op::Read(_))));
        assert!(!without_reads
            .stream(SEED)
            .any(|op| matches!(op, Op::Read(_))));
        assert_eq!(
            writes(with_reads),
            writes(without_reads),
            "{}",
            with_reads.name()
        );
    }
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
    ("documents", 0x1, 1208, "fbb70cc59ad376efbda8b7ce167df16b78191c23e91c92c6bd9fdbbb7190bf80"),
    ("documents", 0x4c49474e494e, 1208, "659032e87a47cf5a8bbf2f827276816645f1eba0ecdd51b03c87f806fd426d76"),
    ("balances", 0x1, 963, "5e9b74a10a9b77c52c6e3fbde4730dbcf5230cfeee582a82ebc54423dc1bd6b1"),
    ("balances", 0x4c49474e494e, 963, "bd2093107784baea22f85168ebc5fac77e5d36070a7c1f2431e7e7e2dcc7f508"),
    ("tokens", 0x1, 1013, "9c854f5c4192d2caff3a11e28a5c86191d8efd5b8adb6f3c8b750eeac081e92c"),
    ("tokens", 0x4c49474e494e, 1013, "42317281250ae58cdb2793ef11663af995a571e41096cf7eb64e403c330a461c"),
    ("element-zoo", 0x1, 2019, "52e9aa3fb0721dfbdd81865f0aab125dbf252db20b3b8d0d1ce942486d40ed0d"),
    ("element-zoo", 0x4c49474e494e, 2026, "3636474016c3c914098e1061a6acc4e40ad0771b8bf05d15faf132744a67bf05"),
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
    ("reference-chains", 0x1, 460, "76d941fb245d7cb0be8005bbbc8aaf78bf04cf82fab6c233d0071865b95421b5"),
    ("reference-chains", 0x4c49474e494e, 460, "776d86bf9f6a7d87b8f7a5da2d14d27047dc6a9ab3439b7eec97092b75308e6b"),
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
    (1, 16118, "fea4d535565fe51e32179da75b73ce01b1ac655836e7b285f2d2b0397fd2d932"),
    (2, 16118, "fea4d535565fe51e32179da75b73ce01b1ac655836e7b285f2d2b0397fd2d932"),
    (3, 16118, "84301809e2ceca9c01cccb2f3a9928024cb2ff8c8be89f28690e5525f2916fce"),
    (4, 16337, "88f545c9f7dec6a47b85746e3939a55f43d873f0d8ebab3854505e44a6d50870"),
    (5, 16566, "35d4619423f3763603141161426242f3d5789a400fcb9857f012cf5251af88dd"),
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
fn the_reference_documents_stream_is_pinned() {
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
