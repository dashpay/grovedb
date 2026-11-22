//! The inspection of a grove on the engine (GROVE-08): `verify_grovedb`,
//! `visualize_verify_grovedb` and the visualization as text.
//!
//! Each is code that names no engine, and is run here on the grove of the
//! state-sync tests ([`Made`]) and on a checkpoint of it, on LigninDB and,
//! where the RocksDB engine is compiled in, on RocksDB with the answers
//! compared. `verify_grovedb` is also run where it has something to
//! report: on what a restore that stopped left.

use std::collections::BTreeMap;

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use grovedb_visualize::{Drawer, Visualize};

use super::{
    sync::{assert_shape, begin, open, Made, Requests, Side, PARTS},
    LigninEngine,
};
use crate::{engine::Engine, GroveDb};

/// What [`verified`] gives for a setting that reports no issue.
const CLEAN: &str = "{} {}";

fn drawn<E: Engine>(db: &GroveDb<E>) -> String {
    let mut text = Vec::new();
    db.visualize(Drawer::new(&mut text)).expect("a drawing");
    String::from_utf8(text).expect("text")
}

/// What `verify_grovedb` reports of `db`, in order, with every setting of
/// its two switches, and what `visualize_verify_grovedb` makes of it.
fn verified<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<String> {
    let mut reports = Vec::new();
    for (references, cache) in [(false, false), (false, true), (true, false), (true, true)] {
        let issues = db.verify_grovedb(None, references, cache, v);
        let issues: BTreeMap<_, _> = issues.expect("a verification").into_iter().collect();
        let texts = db.visualize_verify_grovedb(None, references, cache, v);
        let texts: BTreeMap<_, _> = texts.expect("a verification").into_iter().collect();
        assert_eq!(issues.len(), texts.len());
        reports.push(format!("{issues:?} {texts:?}"));
    }
    reports
}

/// The inspection of the grove on `E` and of a checkpoint of it: no issue
/// in either, by every setting, and one drawing for both, which names
/// every tree under the root.
fn inspected<E: Side>(v: &GroveVersion, number: usize) -> String {
    let made = Made::<E>::new(v);
    assert_shape(&made.shape, number);
    let clean = vec![CLEAN.to_string(); 4];
    assert_eq!(verified(&made.db, v), clean, "{}", E::NAME);
    let text = drawn(&made.db);
    for (at, _) in made.shape.merks.iter().filter(|(at, _)| at.len() == 1) {
        let name = String::from_utf8_lossy(&at[0]).into_owned();
        assert!(text.contains(&name), "{name} is drawn");
    }

    let above = tempfile::tempdir().expect("a directory");
    let target = above.path().join("checkpoint");
    made.db.create_checkpoint(&target).expect("a checkpoint");
    let checkpoint = GroveDb::<E>::open_checkpoint_with_engine(&target).expect("it opens");
    assert_eq!(verified(&checkpoint, v), clean, "{}", E::NAME);
    assert_eq!(drawn(&checkpoint), text, "{}", E::NAME);
    text
}

#[test]
fn a_grove_and_its_checkpoint_are_inspected_on_lignindb() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let text = inspected::<LigninEngine>(v, number + 1);
        #[cfg(feature = "rocksdb_storage")]
        assert_eq!(
            text,
            inspected::<crate::RocksDbEngine>(v, number + 1),
            "grove version {}",
            number + 1
        );
        assert!(text.starts_with("root"), "{}", &text[..text.len().min(80)]);
    }
}

/// What `verify_grovedb` reports of a database on `T` in which a session
/// in parts, from a grove on `S`, was dropped after its third commit: it
/// holds trees whose element commits to a Merk the session had not come
/// to.
fn stopped<S: Side, T: Side>(v: &GroveVersion) -> Vec<String> {
    let source = Made::<S>::new(v);
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    {
        let mut session = begin(&target, source.root, PARTS, v).expect("a session starts");
        let mut requests = Requests::new(&source.db, source.root, v);
        while session.intermediate_commits() < 3 {
            let (request, payload) = requests.fetch().expect("a request");
            let applied = requests.apply(&mut session, &request, &payload);
            applied.expect("the chunk is taken");
        }
        drop(session);
    }
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    let reports = verified(&target, v);
    for report in &reports {
        assert_ne!(report, CLEAN, "{}: something is reported", T::NAME);
    }
    reports
}

#[test]
fn verify_grovedb_reports_what_a_restore_that_stopped_left_on_lignindb() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let reports = stopped::<LigninEngine, LigninEngine>(v);
        #[cfg(feature = "rocksdb_storage")]
        {
            type Rocks = crate::RocksDbEngine;
            let told = format!("grove version {}", number + 1);
            assert_eq!(reports, stopped::<Rocks, Rocks>(v), "{told}");
            assert_eq!(reports, stopped::<Rocks, LigninEngine>(v), "{told}");
            assert_eq!(reports, stopped::<LigninEngine, Rocks>(v), "{told}");
        }
        assert_eq!(reports.len(), 4, "grove version {}", number + 1);
    }
}
