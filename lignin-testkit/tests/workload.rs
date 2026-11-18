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
//! - Each operation that reaches storage in a way of its own is produced at
//!   least once: `the_suite_produces_every_operation`, for the suite of the
//!   latest grove version and, for every other grove version, for exactly
//!   the operations that version has.
//! - The streams are ones GroveDB accepts:
//!   `every_workload_applies_on_every_grove_version`, and for more seeds
//!   `the_operation_workloads_apply_for_many_seeds`.

use std::{collections::BTreeSet, thread};

use grovedb::{
    operations::proof::{SumBudgetStop, VerifiedPathQuery},
    query_result_type::PathKeyElementTrio,
    AxisAggregateValue, Element, GroveDb, PathQuery, PathQueryRun,
};
use grovedb_commitment_tree::{ciphertext_payload_size, DashMemo};
use grovedb_merk::proofs::query::ReadMode;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::workload::{
    adversarial::{
        DeepNesting, DeleteHeavy, FlatDrops, LargeValues, LongKeys, Moves, Pagination,
        PartialBatches, ReferenceChains, TinySubtrees, MAX_NESTING_DEPTH,
    },
    apply,
    coverage::{Operation, ELEMENT_VARIANTS, OPERATION_GROUPS, REFERENCE_PATH_TYPES, TREE_TYPES},
    drive::{
        Balances, Documents, Expiration, IdentityKeys, IndexAggregates, IndexRekey, PlatformState,
        Tokens,
    },
    zoo::{ElementZoo, COMMITMENT_PAYLOAD_LEN},
    BatchOp, Coverage, Features, Op, Outcome, Query, Read, StreamDigest, Transactions, Workload,
    Write,
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
/// or a flush that does not follow a commit, on a query whose proof does not
/// verify to what the query returned, on a query marked as not proven that
/// GroveDB proves, and on a flush that does not reclaim every subtree
/// dropped since the flush before it.
fn run(
    name: &str,
    db: &GroveDb,
    ops: impl Iterator<Item = Op>,
    grove_version: &GroveVersion,
) -> u64 {
    let mut transactions = Transactions::new(db);
    let mut applied = 0u64;
    let mut committed = true;
    let mut dropped = 0u64;
    for (index, op) in ops.enumerate() {
        if op.follows_a_commit() {
            assert!(
                committed,
                "{name}: operation {index} does not follow a commit: {op:?}"
            );
        }
        dropped += flat_drops(&op);
        if std::env::var_os("WORKLOAD_TRACE").is_some() {
            eprintln!(
                "{name} v{} #{index}: {op:?}",
                grove_version.protocol_version
            );
        }
        let outcome = transactions
            .apply(&op, grove_version)
            .unwrap()
            .unwrap_or_else(|error| {
                panic!(
                    "{name} on grove v{}: op {index} refused: {error:?}\n{op:?}",
                    grove_version.protocol_version
                )
            });
        if op.ends_transaction() {
            committed = true;
        } else if !op.keeps_transaction() {
            committed = false;
        }
        match (&op, outcome) {
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
            (Op::Read(read), Outcome::Run(answer)) => {
                check_page(name, index, db, read, &answer, grove_version);
                if let Read::PathQuery { query, proven } = read {
                    prove_path_query(name, index, db, query, *proven, answer, grove_version);
                }
            }
            (Op::FlushPendingDrops, Outcome::Reclaimed(report)) => {
                assert_eq!(
                    (
                        report.reclaimed_records,
                        report.skipped_live,
                        report.unresolved_records
                    ),
                    (dropped, 0, 0),
                    "{name}: flush {index} did not reclaim the {dropped} dropped subtrees"
                );
                dropped = 0;
            }
            _ => {}
        }
        applied += 1;
    }
    assert!(committed, "{name}: the stream does not end on a commit");
    assert_eq!(dropped, 0, "{name}: the stream leaves dropped subtrees");
    applied
}

/// How many subtrees an operation drops flat.
fn flat_drops(op: &Op) -> u64 {
    let in_batch = |ops: &[BatchOp]| {
        ops.iter()
            .filter(|op| matches!(op, BatchOp::DropFlat { .. }))
            .count() as u64
    };
    match op {
        Op::Write(Write::DropFlat { .. }) => 1,
        Op::Batch(ops) => in_batch(ops),
        Op::PartialBatch(batch) => in_batch(&batch.ops) + in_batch(&batch.add_on),
        _ => 0,
    }
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

/// Proves a path query against committed state and checks that the proof
/// verifies to the database's root hash and to the answer the query gave. A
/// query the stream marks as not proven must be one GroveDB refuses to
/// prove.
fn prove_path_query(
    name: &str,
    index: usize,
    db: &GroveDb,
    query: &PathQuery,
    proven: bool,
    answer: PathQueryRun,
    grove_version: &GroveVersion,
) {
    let version = grove_version.protocol_version;
    let proof = db.prove_query(query, None, grove_version).unwrap();
    if !proven {
        assert!(
            proof.is_err(),
            "{name} on grove v{version}: query {index} is marked as not proven, but GroveDB \
             proves it\n{query:?}"
        );
        return;
    }
    let proof = proof.unwrap_or_else(|error| {
        panic!("{name} on grove v{version}: query {index} not provable: {error:?}\n{query:?}")
    });
    let verified =
        GroveDb::verify_path_query(&proof, query, grove_version).unwrap_or_else(|error| {
            panic!(
                "{name} on grove v{version}: proof of query {index} does not verify: \
                 {error:?}\n{query:?}"
            )
        });
    assert_eq!(
        *verified.root_hash(),
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash"),
        "{name} on grove v{version}: proof of query {index} verifies to another root"
    );
    let context =
        format!("{name} on grove v{version}: query {index} proves another answer\n{query:?}");
    match (answer, verified) {
        (
            PathQueryRun::Elements { elements, .. },
            VerifiedPathQuery::Elements {
                elements: proved, ..
            },
        ) => {
            let expected: Vec<_> = elements
                .to_path_key_elements()
                .into_iter()
                .map(|(path, key, element)| {
                    let element = if element.underlying().is_reference() {
                        db.get(path.as_slice(), &key, None, grove_version)
                            .unwrap()
                            .expect("a stored reference resolves")
                    } else {
                        element
                    };
                    (path, key, Some(element))
                })
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AggregateCount(count),
            VerifiedPathQuery::AggregateCount { count: proved, .. },
        ) => assert_eq!(proved, count, "{context}"),
        (PathQueryRun::AggregateSum(sum), VerifiedPathQuery::AggregateSum { sum: proved, .. }) => {
            assert_eq!(proved, sum, "{context}")
        }
        (
            PathQueryRun::AggregateCountAndSum { count, sum },
            VerifiedPathQuery::AggregateCountAndSum {
                count: proved_count,
                sum: proved_sum,
                ..
            },
        ) => assert_eq!((proved_count, proved_sum), (count, sum), "{context}"),
        (
            PathQueryRun::AggregateCountPerKey(per_key),
            VerifiedPathQuery::AggregatePerKey {
                per_key: proved, ..
            },
        ) => {
            let expected: Vec<_> = per_key
                .into_iter()
                .map(|(key, count)| (key, Some(count), None))
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AggregateSumPerKey(per_key),
            VerifiedPathQuery::AggregatePerKey {
                per_key: proved, ..
            },
        ) => {
            let expected: Vec<_> = per_key
                .into_iter()
                .map(|(key, sum)| (key, None, Some(sum)))
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AggregateCountAndSumPerKey(per_key),
            VerifiedPathQuery::AggregatePerKey {
                per_key: proved, ..
            },
        ) => {
            let expected: Vec<_> = per_key
                .into_iter()
                .map(|(key, count, sum)| (key, Some(count), Some(sum)))
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AxisEntries { entries, skipped },
            VerifiedPathQuery::AxisEntries {
                entries: proved,
                skipped: proved_skipped,
                ..
            },
        ) => assert_eq!((proved, proved_skipped), (entries, skipped), "{context}"),
        (
            PathQueryRun::AxisKeys { keys, skipped },
            VerifiedPathQuery::AxisEntries {
                entries: proved,
                skipped: proved_skipped,
                ..
            },
        ) => assert_eq!(
            (proved.to_keys(), proved_skipped),
            (keys, skipped),
            "{context}"
        ),
        (
            PathQueryRun::BranchedAxisEntries(branches),
            VerifiedPathQuery::BranchedAxisEntries {
                branches: proved, ..
            },
        ) => assert_eq!(proved, branches, "{context}"),
        (PathQueryRun::AxisRank(rank), VerifiedPathQuery::AxisRank { rank: proved, .. }) => {
            assert_eq!(proved, rank, "{context}")
        }
        (
            PathQueryRun::AxisAggregate(value),
            VerifiedPathQuery::AxisAggregate { value: proved, .. },
        ) => {
            let value = match value {
                AxisAggregateValue::Population(population) => i128::from(population),
                AxisAggregateValue::Total(total) => i128::from(total),
            };
            assert_eq!(proved, value, "{context}");
        }
        (
            PathQueryRun::SumBudget(sums),
            VerifiedPathQuery::SumBudget {
                matches,
                total,
                stop,
                ..
            },
        ) => {
            let Some(ReadMode::SumBudget(budget)) = query.query.query.read_mode.as_deref() else {
                panic!("{context}\nthe query has no sum budget");
            };
            let sum = sums
                .results
                .iter()
                .fold(0i64, |sum, (_, value)| sum.saturating_add(*value));
            let expected_stop = if sum >= budget.sum_limit as i64 {
                SumBudgetStop::BudgetReached
            } else if budget.match_limit.map(usize::from) == Some(sums.results.len()) {
                SumBudgetStop::MatchLimitReached
            } else if sums.hard_limit_reached {
                SumBudgetStop::HardScanCapReached
            } else {
                SumBudgetStop::Exhausted
            };
            assert_eq!(
                (matches, total, stop),
                (sums.results, sum, expected_stop),
                "{context}"
            );
        }
        (answer, verified) => panic!(
            "{name} on grove v{version}: query {index} was answered as {answer:?} and proved \
             as {verified:?}"
        ),
    }
}

/// Checks the page a query with an offset returned against the same query
/// run without offset or limit: the rows skipped are as many as the offset
/// asks for, or all there are, and the page is what follows them.
fn check_page(
    name: &str,
    index: usize,
    db: &GroveDb,
    read: &Read,
    answer: &PathQueryRun,
    grove_version: &GroveVersion,
) {
    let (query, raw) = match read {
        Read::Rows { query, raw } => (query, *raw),
        Read::PathQuery { query, .. } => (query, true),
        _ => return,
    };
    let (Some(offset), PathQueryRun::Elements { elements, skipped }) = (query.query.offset, answer)
    else {
        return;
    };
    let mut unpaged = (**query).clone();
    unpaged.query.offset = None;
    unpaged.query.limit = None;
    let unpaged = Op::Read(Read::Rows {
        query: Box::new(unpaged),
        raw,
    });
    let Outcome::Run(PathQueryRun::Elements { elements: all, .. }) =
        apply(db, &unpaged, None, grove_version)
            .unwrap()
            .expect("the query runs without its offset")
    else {
        panic!("{name}: a row reader returns rows");
    };
    let all = all.to_path_key_elements();
    let page = elements.clone().to_path_key_elements();
    let expected_skipped = usize::from(offset).min(all.len());
    let context = format!("{name}: query {index} pages wrongly\n{read:?}");
    assert_eq!(usize::from(*skipped), expected_skipped, "{context}");
    let rest = &all[expected_skipped..];
    assert!(page.len() <= rest.len(), "{context}");
    assert_eq!(page, rest[..page.len()], "{context}");
    let limit = query.query.limit.map_or(usize::MAX, usize::from);
    if query.query.query.has_subquery() {
        assert!(page.len() <= limit, "{context}");
    } else {
        assert_eq!(page.len(), rest.len().min(limit), "{context}");
    }
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
        Workload::IdentityKeys(IdentityKeys {
            identities: 1,
            keys: 1,
            updates: 12,
            transitions_per_block: 1,
            features,
        }),
        Workload::PlatformState(PlatformState {
            masternodes: 0,
            validator_sets: 0,
            blocks: 5,
            blocks_per_rotation: 1,
        }),
        Workload::IndexRekey(IndexRekey {
            documents: 1,
            values: 1,
            twins: 1,
            moves_per_batch: 1,
            documents_after: 0,
            features,
        }),
        Workload::IndexRekey(IndexRekey {
            documents: 0,
            twins: 0,
            ..IndexRekey::default()
        }),
        Workload::Expiration(Expiration {
            blocks: 3,
            documents_per_block: 1,
            longest_life: 1,
            features,
        }),
        Workload::Expiration(Expiration {
            blocks: 2,
            documents_per_block: 0,
            ..Expiration::default()
        }),
        Workload::IndexAggregates(IndexAggregates {
            documents: 1,
            deletes: 1,
            owners: 1,
            transitions_per_block: 1,
            features,
        }),
        Workload::FlatDrops(FlatDrops {
            rounds: 0,
            features,
        }),
        Workload::Moves(Moves {
            rounds: 0,
            features,
        }),
        Workload::PartialBatches(PartialBatches {
            transitions: 0,
            transitions_per_block: 1,
        }),
        Workload::PartialBatches(PartialBatches {
            transitions: 9,
            transitions_per_block: 1,
        }),
        Workload::Pagination(Pagination {
            rows: 8,
            rounds: 1,
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

/// The workloads added for the operations that reach storage in a way of
/// their own.
fn operation_workloads(features: Features) -> Vec<Workload> {
    Workload::suite(features)
        .into_iter()
        .skip_while(|workload| workload.name() != "identity-keys")
        .collect()
}

/// Which rows a workload reads, pages and moves depends on the seed.
/// `WORKLOAD_SEEDS` sets how many seeds to try.
#[test]
fn the_operation_workloads_apply_for_many_seeds() {
    let seeds: u64 = std::env::var("WORKLOAD_SEEDS")
        .ok()
        .map_or(4, |seeds| seeds.parse().expect("a number of seeds"));
    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        let workloads = operation_workloads(features);
        assert_eq!(workloads.len(), 9);
        for workload in workloads {
            for seed in 2..2 + seeds {
                let (_dir, db) = open();
                let name = format!("{} seed {seed}", workload.name());
                run(&name, &db, workload.stream(seed), grove_version);
            }
        }
    });
}

fn suite_coverage(features: Features) -> Coverage {
    let mut coverage = Coverage::new();
    for workload in Workload::suite(features) {
        for op in workload.stream(SEED) {
            coverage.record(&op);
        }
    }
    coverage
}

#[test]
fn the_suite_produces_every_operation() {
    assert_eq!(
        Features::for_grove_version(GroveVersion::latest()),
        Features::ALL,
        "the suite applied to the latest grove version is not the one checked here"
    );
    let coverage = suite_coverage(Features::ALL);
    assert_eq!(coverage.missing_operations(), Vec::<Operation>::new());
    for group in OPERATION_GROUPS {
        let produced: Vec<&str> = coverage
            .operations()
            .iter()
            .filter(|operation| operation.group() == *group)
            .map(|operation| operation.name())
            .collect();
        assert!(!produced.is_empty(), "no workload produces {group}");
        println!("{group}: {produced:?}");
    }

    for grove_version in GROVE_VERSIONS {
        let features = Features::for_grove_version(grove_version);
        let unsupported: Vec<Operation> = Operation::ALL
            .iter()
            .copied()
            .filter(|operation| !features.supports(*operation))
            .collect();
        assert_eq!(
            suite_coverage(features).missing_operations(),
            unsupported,
            "grove v{}",
            grove_version.protocol_version
        );
        println!(
            "grove v{}: every operation but {unsupported:?}",
            grove_version.protocol_version
        );
    }

    let first_unit: Vec<Workload> = Workload::suite(Features::ALL)
        .into_iter()
        .take_while(|workload| workload.name() != "identity-keys")
        .collect();
    assert_eq!(first_unit.len(), 10);
    for workload in first_unit {
        let ops: Vec<Op> = workload.stream(SEED).collect();
        assert!(
            Coverage::of(&ops).operations().is_empty(),
            "{} produces one of the operations",
            workload.name()
        );
    }
}

#[test]
fn coverage_reports_what_a_stream_lacks() {
    let empty = Coverage::new();
    assert_eq!(empty.missing_operations(), Operation::ALL);
    let groups: BTreeSet<&str> = Operation::ALL
        .iter()
        .map(|operation| operation.group())
        .collect();
    assert_eq!(groups, OPERATION_GROUPS.iter().copied().collect());
    assert_eq!(OPERATION_GROUPS.len(), 8);
    let names: BTreeSet<&str> = Operation::ALL
        .iter()
        .map(|operation| operation.name())
        .collect();
    assert_eq!(names.len(), Operation::ALL.len());
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
    ("identity-keys", 0x1, 262, "4fd326146efeb547d2b9154c021c3d9e8f047bfb46f19ad145db54fba5d8c187"),
    ("identity-keys", 0x4c49474e494e, 262, "e715468ac182b86764efb2e69bde64c98ea31df041baf565c8292d1bafb9075e"),
    ("platform-state", 0x1, 227, "ad7a07360f8a524353908d5adea159ceaa5d3f0ed00c6fb2a190cf649a7c5db5"),
    ("platform-state", 0x4c49474e494e, 230, "e57620e2935db7f8e6454e61c97a85c60a9adf3b317833f6b91bbe3b74acf1e3"),
    ("index-rekey", 0x1, 320, "0d21d26fae26ed367c2eb28af967f53ea055edf5098bb6548db028fcd713c1f6"),
    ("index-rekey", 0x4c49474e494e, 320, "59feab58060ff927842bcf09a9f1e294ed7f914a86d518c536c887f081734f1c"),
    ("expiration", 0x1, 559, "a1e894ffe395c4d42af1510481d97d983dffe314e1c57fa15c27ab485b1cba0a"),
    ("expiration", 0x4c49474e494e, 563, "e35fd0fbaa598145529309a6daeff4d73e3427f177480b245316e0c823665f31"),
    ("index-aggregates", 0x1, 298, "cd048c0e6e76f57f042ed8df220f620cf96399479a7f1e239fbe134b31271731"),
    ("index-aggregates", 0x4c49474e494e, 298, "b547d3657cfbc1e1f01f0fd2d2a367fdd3c9e8bdbc7eb6003329c3fd8a0d409a"),
    ("flat-drops", 0x1, 179, "b080d17288c227df6bd39f7db4eaaf76036ff8b4a3eafff029c66812c8a56d0b"),
    ("flat-drops", 0x4c49474e494e, 179, "4f5039b35499b9f764c9326fd63bd5a1304371bbc1ee961473cfdb4641f09336"),
    ("moves", 0x1, 251, "5b3657e5db9ab8462294685a88559e3e7555ddcd8223e264a0d82edbe1d5eaf8"),
    ("moves", 0x4c49474e494e, 251, "4692bf1dead57fe5cec67d5fa8ff7073552b4491ea3a7f57309a15202324eae5"),
    ("partial-batches", 0x1, 229, "d74626348e1c61fa41fffb10d61a6a6be5859373cf6ae2e6d312d8b7a911461e"),
    ("partial-batches", 0x4c49474e494e, 229, "05a4bec0f5d876e8307ab29962158cded8eb28ed95004be37ecb5dc6d6a23ba5"),
    ("pagination", 0x1, 450, "c6f32bc26af5c7c640bd4a27c7f57a50b78e34b5384658ee0a3d97e6973722b4"),
    ("pagination", 0x4c49474e494e, 450, "8312d028f4fcd6c0ffe2cd4bc6282a8316c09acf0b69802a7d6501036371b075"),
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
    (1, 18681, "9cb765cb89b3c383e89d84c84c36f9a61222db6d4344a3469ed3a26b407e3bd2"),
    (2, 18681, "9cb765cb89b3c383e89d84c84c36f9a61222db6d4344a3469ed3a26b407e3bd2"),
    (3, 18681, "3b296a8f47c4fe143f09b67acb70d8a8970998de8493dd033f526dc94f31021d"),
    (4, 19095, "c865e1840881fa86e39448c9e088c521ef9fd0ee5b31466f60f3b376e7b3e5b1"),
    (5, 19341, "66e880ef0028738781b970b71a344f0f8a1b99dfd6810e333b185a7a184c519e"),
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

/// One operation of every kind the second unit added, and one query for
/// every construct of GroveDB's query grammar.
fn one_of_each_new_kind() -> Vec<Op> {
    use grovedb::{
        AggregateSumPathQuery, AxisProjection, AxisQuery, PathQuery, Query, QueryItem, SizedQuery,
        TreeType,
    };
    use grovedb_merk::proofs::query::{AggregateFold, AggregateSumQuery, IndexAxis};
    use lignin_testkit::workload::PartialBatch;

    let path = vec![b"tree".to_vec()];
    let key = b"key".to_vec();
    let bytes = |byte: u8| vec![byte; 2];
    let mut ops = vec![
        Op::Write(Write::PutAux {
            key: key.clone(),
            value: bytes(1),
        }),
        Op::Write(Write::DeleteAux { key: key.clone() }),
        Op::Write(Write::DropFlat {
            path: path.clone(),
            key: key.clone(),
        }),
        Op::Batch(vec![
            BatchOp::Patch {
                path: path.clone(),
                key: key.clone(),
                element: Element::new_item(bytes(2)),
                change_in_bytes: -3,
            },
            BatchOp::Move {
                path: path.clone(),
                key: key.clone(),
                new_key: bytes(3),
            },
            BatchOp::DropFlat {
                path: path.clone(),
                key: key.clone(),
                tree_type: TreeType::BulkAppendTree(4),
            },
        ]),
        Op::PartialBatch(PartialBatch {
            ops: vec![BatchOp::Delete {
                path: path.clone(),
                key: key.clone(),
            }],
            add_on: vec![BatchOp::InsertOnly {
                path: path.clone(),
                key: key.clone(),
                element: Element::new_sum_item(5),
            }],
            pause_height: Some(2),
        }),
        Op::PartialBatch(PartialBatch {
            ops: Vec::new(),
            add_on: Vec::new(),
            pause_height: None,
        }),
        Op::FlushPendingDrops,
        Op::Read(Read::GetAux { key: key.clone() }),
        Op::Read(Read::AuxByPrefix {
            prefix: key.clone(),
        }),
    ];
    for (skip_non_sum_items, ignore_references) in [(false, true), (true, false)] {
        ops.push(Op::Read(Read::SumBudget {
            query: Box::new(AggregateSumPathQuery::new(
                path.clone(),
                AggregateSumQuery {
                    items: vec![QueryItem::Key(key.clone())],
                    left_to_right: skip_non_sum_items,
                    sum_limit: 600,
                    limit_of_items_to_check: ignore_references.then_some(7),
                },
            )),
            skip_non_sum_items,
            ignore_references,
        }));
    }

    let items = [
        QueryItem::Key(bytes(1)),
        QueryItem::Range(bytes(1)..bytes(2)),
        QueryItem::RangeInclusive(bytes(1)..=bytes(2)),
        QueryItem::RangeFull(..),
        QueryItem::RangeFrom(bytes(1)..),
        QueryItem::RangeTo(..bytes(2)),
        QueryItem::RangeToInclusive(..=bytes(2)),
        QueryItem::RangeAfter(bytes(1)..),
        QueryItem::RangeAfterTo(bytes(1)..bytes(2)),
        QueryItem::RangeAfterToInclusive(bytes(1)..=bytes(2)),
        QueryItem::AggregateCountOnRange(Box::new(QueryItem::RangeFrom(bytes(1)..))),
        QueryItem::AggregateSumOnRange(Box::new(QueryItem::RangeFrom(bytes(1)..))),
        QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::RangeFrom(bytes(1)..))),
    ];
    let mut queries: Vec<PathQuery> = items
        .into_iter()
        .map(|item| PathQuery::new_single_query_item(path.clone(), item))
        .collect();

    let mut nested = Query::new_single_query_item_with_direction(QueryItem::RangeFull(..), false);
    nested.limit = Some(3);
    nested.offset = Some(4);
    let mut branching = Query::new_range_full();
    branching.set_subquery_path(vec![bytes(5), bytes(6)]);
    branching.set_subquery(nested.clone());
    branching.add_conditional_subquery(QueryItem::Key(bytes(7)), Some(vec![bytes(8)]), None);
    branching.add_conditional_subquery(QueryItem::RangeFrom(bytes(9)..), None, Some(nested));
    branching.add_parent_tree_on_subquery = true;
    queries.push(PathQuery::new(
        path.clone(),
        SizedQuery::new(branching.clone(), Some(10), None),
    ));
    queries.push(PathQuery::new(
        path.clone(),
        SizedQuery::new(branching, None, Some(11)),
    ));
    queries.push(PathQuery::new(
        path.clone(),
        SizedQuery::new(Query::new_range_full(), Some(12), Some(13)),
    ));
    for aggregate in [
        Query::new_aggregate_count_on_range,
        Query::new_aggregate_sum_on_range,
        Query::new_aggregate_count_and_sum_on_range,
    ] {
        let mut carrier = Query::new_single_key(bytes(1));
        carrier.set_subquery(aggregate(QueryItem::RangeTo(..bytes(2))));
        queries.push(PathQuery::new_unsized(path.clone(), carrier));
    }

    let axis_queries = [
        AxisQuery::top_k(IndexAxis::Count, 3, 4, true),
        AxisQuery::bottom_k(IndexAxis::Sum, 3, 4),
        AxisQuery::bounded(IndexAxis::Avg, -5, 6, 7, false),
        AxisQuery::rank_of_key(IndexAxis::Count, key.clone(), true),
        AxisQuery::aggregate_over_value_range(IndexAxis::Sum, -5, 6, AggregateFold::Population),
        AxisQuery::aggregate_over_value_range(IndexAxis::Sum, -5, 6, AggregateFold::Total),
        AxisQuery::top_k(IndexAxis::Count, 3, 4, true).with_projection(AxisProjection::Keys),
    ];
    for axis_query in axis_queries {
        queries.push(PathQuery::new_axis(path.clone(), axis_query));
    }
    queries.push(PathQuery::new_branched_axis(
        path.clone(),
        vec![bytes(1), bytes(2)],
        vec![bytes(3)],
        AxisQuery::top_k(IndexAxis::Count, 3, 0, false),
    ));
    for match_limit in [None, Some(9)] {
        queries.push(PathQuery::new_sum_budget(
            path.clone(),
            vec![QueryItem::RangeFull(..)],
            true,
            700,
            match_limit,
        ));
    }

    for query in queries {
        for proven in [false, true] {
            ops.push(Op::Read(Read::PathQuery {
                query: Box::new(query.clone()),
                proven,
            }));
        }
        ops.push(Op::Read(Read::Rows {
            query: Box::new(query),
            raw: true,
        }));
    }
    ops
}

const PINNED_NEW_KINDS: &str = "97278877b581d311033c13aa0ebdfd3e0f5c1141a7cf8a45da94b262dbcd5b49";

/// The canonical encoding of the kinds the second unit added: each one
/// differs from every other, and the bytes are pinned, so a change to the
/// encoding cannot pass as a change to a workload.
#[test]
fn the_new_kinds_have_a_pinned_encoding() {
    let ops = one_of_each_new_kind();
    let mut encodings = BTreeSet::new();
    for op in &ops {
        let mut bytes = Vec::new();
        op.encode(&mut bytes);
        assert!(encodings.insert(bytes), "two operations encode as {op:?}");
        assert_eq!(*op, op.clone());
    }
    let digest = StreamDigest::of(&ops);
    assert_eq!(digest.ops(), ops.len() as u64);
    assert_eq!(digest.hex(), PINNED_NEW_KINDS);

    let coverage = Coverage::of(&ops);
    assert_eq!(coverage.missing_operations(), Vec::<Operation>::new());
    for op in &ops {
        let (query, through_rows) = match op {
            Op::Read(Read::PathQuery { query, .. }) => (query, false),
            Op::Read(Read::Rows { query, .. }) => (query, true),
            _ => continue,
        };
        if query.query.offset.is_none() || !query.query.query.has_subquery() {
            continue;
        }
        let operations = Coverage::of([op]).operations().clone();
        if through_rows {
            assert!(
                operations.contains(&Operation::QueryOffset),
                "the row readers take an offset next to a subquery"
            );
        } else {
            assert!(
                operations.is_empty(),
                "`run_path_query` refuses an offset next to a subquery"
            );
        }
    }
    let stored: Vec<Element> = ops
        .iter()
        .flat_map(|op| op.elements().into_iter().cloned())
        .collect();
    assert_eq!(
        stored,
        [Element::new_item(vec![2; 2]), Element::new_sum_item(5)]
    );
    assert!(ops
        .iter()
        .all(|op| op.follows_a_commit() == matches!(op, Op::Read(_) | Op::FlushPendingDrops)));
}

/// Reads are compared as they are encoded: the order of a query's
/// conditional branches counts, and so does having none against having an
/// empty set of them.
#[test]
fn reads_are_equal_when_they_encode_the_same() {
    use grovedb::{PathQuery, Query, QueryItem, SizedQuery};

    let read = |query: Query| {
        Op::Read(Read::PathQuery {
            query: Box::new(PathQuery::new(
                vec![b"tree".to_vec()],
                SizedQuery::new(query, None, None),
            )),
            proven: true,
        })
    };
    let branches = |first: u8, second: u8| {
        let mut query = Query::new_range_full();
        for key in [first, second] {
            query.add_conditional_subquery(
                QueryItem::Key(vec![key]),
                None,
                Some(Query::new_single_key(vec![key])),
            );
        }
        query
    };
    assert_eq!(read(branches(1, 2)), read(branches(1, 2)));
    assert_ne!(read(branches(1, 2)), read(branches(2, 1)));
    assert_eq!(branches(1, 2), branches(2, 1), "GroveDB ignores the order");

    let mut no_branches = Query::new_range_full();
    no_branches.conditional_subquery_branches = Some(Default::default());
    assert_ne!(read(no_branches), read(Query::new_range_full()));
}

/// The moves workload moves keys alone, swaps two keys and chains two
/// moves through one key, and goes on doing so for more rounds than its
/// default.
#[test]
fn the_moves_workload_swaps_and_chains() {
    let moves_of = |op: &Op| -> Vec<(Vec<u8>, Vec<u8>)> {
        let Op::Batch(ops) = op else {
            return Vec::new();
        };
        ops.iter()
            .filter_map(|op| match op {
                BatchOp::Move { key, new_key, .. } => Some((key.clone(), new_key.clone())),
                _ => None,
            })
            .collect()
    };
    let (mut alone, mut swaps, mut chains, mut longer, mut shorter) = (0, 0, 0, 0, 0);
    for op in Workload::Moves(Moves::default()).stream(SEED) {
        let moves = moves_of(&op);
        let taken = |key: &Vec<u8>| moves.iter().any(|(from, _)| from == key);
        let freed = |key: &Vec<u8>| moves.iter().any(|(_, to)| to == key);
        match moves.as_slice() {
            [] => {}
            [_] => alone += 1,
            [(a, b), (c, d)] if a == d && b == c => swaps += 1,
            _ if moves.iter().any(|(from, to)| taken(to) && !freed(from)) => chains += 1,
            _ => {}
        }
        for (from, to) in &moves {
            longer += u32::from(to.len() > from.len());
            shorter += u32::from(to.len() < from.len());
        }
    }
    println!("alone {alone}, swaps {swaps}, chains {chains}, longer {longer}, shorter {shorter}");
    assert!(alone > 0 && swaps > 0 && chains > 0 && longer > 0 && shorter > 0);

    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        for seed in [SEED, 2, 3] {
            let (_dir, db) = open();
            let workload = Moves {
                rounds: 7,
                features,
            };
            run("moves", &db, workload.stream(seed), grove_version);
        }
    });
}

/// A query GroveDB's grammar refuses counts as none of the operations.
#[test]
fn a_malformed_query_is_no_operation() {
    use grovedb::{PathQuery, Query, QueryItem, SizedQuery};

    let mut two_aggregates = Query::new();
    two_aggregates.items = vec![
        QueryItem::AggregateCountOnRange(Box::new(QueryItem::RangeFrom(vec![1]..))),
        QueryItem::AggregateSumOnRange(Box::new(QueryItem::RangeFrom(vec![1]..))),
    ];
    let malformed = PathQuery::new(
        vec![b"tree".to_vec()],
        SizedQuery::new(two_aggregates, None, None),
    );
    assert!(malformed.classify().is_err());
    let op = Op::Read(Read::PathQuery {
        query: Box::new(malformed),
        proven: false,
    });
    assert!(Coverage::of([&op]).operations().is_empty());
}
