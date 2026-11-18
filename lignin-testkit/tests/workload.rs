//! Acceptance tests for the workload generators (BASE-01).

use grovedb::{GroveDb, PathQuery};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::workload::{
    apply,
    drive::{Balances, Documents, Tokens},
    zoo::ElementZoo,
    Coverage, Features, Op, Outcome, Read,
};

/// Applies a stream to a fresh database, one transaction per block, and
/// returns how many operations it applied. Panics on the first refused
/// operation.
fn run(name: &str, ops: impl Iterator<Item = Op>, grove_version: &GroveVersion) -> u64 {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = GroveDb::open(dir.path()).expect("open");
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
        let outcome = apply(&db, &op, Some(&transaction), grove_version)
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
                    &db,
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
    drop(transaction);
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify");
    if !issues.is_empty() {
        println!(
            "{name} on grove v{}: verify_grovedb found {} issues: {:?}",
            grove_version.protocol_version,
            issues.len(),
            issues
                .keys()
                .take(8)
                .map(|path| path
                    .iter()
                    .map(|segment| String::from_utf8_lossy(segment).into_owned())
                    .collect::<Vec<_>>()
                    .join("/"))
                .collect::<Vec<_>>()
        );
    }
    applied
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
fn zoo_applies_on_every_grove_version() {
    for grove_version in GROVE_VERSIONS.iter().rev() {
        let features = Features::for_grove_version(grove_version);
        let applied = run("zoo", ElementZoo { features }.stream(1), grove_version);
        println!("grove v{}: {applied} ops", grove_version.protocol_version);
    }
}

#[test]
fn zoo_covers_everything() {
    let ops: Vec<Op> = ElementZoo {
        features: Features::ALL,
    }
    .stream(1)
    .collect();
    let coverage = Coverage::of(&ops);
    println!("{:?}", coverage.missing_elements());
    println!("{:?}", coverage.missing_tree_types());
}

#[test]
fn drive_applies_on_every_grove_version() {
    for grove_version in GROVE_VERSIONS.iter().rev() {
        let v = grove_version.protocol_version;
        println!(
            "v{v} documents: {}",
            run("documents", Documents::default().stream(1), grove_version)
        );
        println!(
            "v{v} balances: {}",
            run("balances", Balances::default().stream(1), grove_version)
        );
        println!(
            "v{v} tokens: {}",
            run("tokens", Tokens::default().stream(1), grove_version)
        );
    }
}
