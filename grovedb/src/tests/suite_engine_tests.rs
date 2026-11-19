//! The suite is built for the engine that was selected, and the fixtures
//! open that engine.

use std::any::type_name;

use grovedb_version::version::GroveVersion;

use crate::{
    engine::DefaultEngine,
    tests::{make_empty_grovedb, make_test_grovedb, SuiteEngine, TempGroveDb, TEST_LEAF},
    Element, GroveDb,
};

const REGISTRY: &str = include_str!("../../test-engines.txt");

fn registered() -> Vec<&'static str> {
    REGISTRY
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// The engine type of a registered name. An engine that is registered and
/// has no line here fails the first test below.
fn engine_type(name: &str) -> Option<&'static str> {
    match name {
        "rocksdb" => Some("RocksDbEngine"),
        "pinned-reads" => Some("PinnedReads"),
        _ => None,
    }
}

/// The name the build script selected.
fn selected() -> &'static str {
    env!("GROVEDB_SUITE_ENGINE")
}

#[test]
fn every_registered_engine_has_a_type() {
    let registered = registered();
    assert!(!registered.is_empty());
    for name in &registered {
        assert!(engine_type(name).is_some(), "{name} has no engine type");
    }
    let mut unique = registered.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), registered.len(), "a name is registered twice");
}

#[test]
fn the_suite_is_built_for_the_selected_engine() {
    let name = selected();
    assert!(registered().contains(&name), "{name} is not registered");
    let named = std::env::var("GROVEDB_TEST_ENGINE").unwrap_or_default();
    if named.is_empty() {
        assert_eq!(name, registered()[0], "the default is the first engine");
    } else {
        assert_eq!(name, named, "the suite was built for another engine");
    }
    let expected = engine_type(name).expect("a registered engine");
    for engine in [type_name::<SuiteEngine>(), type_name::<DefaultEngine>()] {
        assert!(
            engine.ends_with(expected),
            "{name} is {expected}, and the suite runs on {engine}"
        );
    }
    println!("grovedb suite engine: {name} ({})", type_name::<SuiteEngine>());
}

#[test]
fn the_fixtures_and_the_constructors_open_the_suite_engine() {
    let grove_version = GroveVersion::latest();
    let fixture: TempGroveDb = make_test_grovedb(grove_version);
    let by_fixture: &GroveDb<SuiteEngine> = &fixture;
    let _: &SuiteEngine = by_fixture.engine();

    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let opened: GroveDb<SuiteEngine> = GroveDb::open(directory.path()).expect("the database opens");
    opened
        .insert(
            [].as_ref() as &[&[u8]],
            TEST_LEAF,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("an insert");
    assert_eq!(
        opened.root_hash(None, grove_version).unwrap().expect("a root hash"),
        {
            let one_leaf = make_empty_grovedb();
            one_leaf
                .insert(
                    [].as_ref() as &[&[u8]],
                    TEST_LEAF,
                    Element::empty_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("an insert");
            one_leaf.root_hash(None, grove_version).unwrap().expect("a root hash")
        }
    );
}

/// On this engine the transaction of a read that was given none is a
/// snapshot, so a database of the fixtures counts it as a read.
#[cfg(grovedb_test_engine = "pinned-reads")]
#[test]
fn a_database_of_the_fixtures_pins_its_reads() {
    use std::sync::atomic::Ordering;

    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let engine = db.engine();
    let reads = engine.reads_started.load(Ordering::Relaxed);
    let writes = engine.writes_started.load(Ordering::Relaxed);
    db.get([].as_ref() as &[&[u8]], TEST_LEAF, None, grove_version)
        .unwrap()
        .expect("the leaf");
    assert_eq!(engine.reads_started.load(Ordering::Relaxed), reads + 1);
    assert_eq!(engine.writes_started.load(Ordering::Relaxed), writes);
}
