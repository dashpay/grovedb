//! The engine crash campaign (VER-04): a background thread of the engine
//! that panics is reported.
//!
//! A database that GroveDB owns is closed by the drop of its handle, which
//! joins the three threads of the engine and answers nothing. The campaign
//! knows a panic of one of them by the panic hook of the process
//! (`engine::observe::engine_panics`). The count is the process's, so
//! this test has a process of its own, and is the only test in it.

// The engine the golden corpus is pinned to has no LigninDB under GroveDB
// (`build.rs`).
#![cfg(grovedb_engine_seam)]

use lignin_testkit::crash::{
    campaign::Faults,
    check::Kind,
    engine::{
        campaign::{campaign, Planted, Settings, Shape, ACTED},
        observe::engine_panics,
    },
};

fn panic_in(name: &str) {
    let thread = std::thread::Builder::new().name(name.to_string());
    let panicked = thread.spawn(|| panic!("planted")).expect("a thread");
    assert!(panicked.join().is_err());
}

/// The panics of the threads that have a name of the engine's are counted,
/// and no other. A campaign in which a thread with the name of the
/// cleaner's panics while a session is open reports a violation of that
/// session, at or after the step in which it panicked; the same campaign
/// with no such panic is clean.
#[test]
fn a_panic_of_a_thread_of_the_engine_is_a_violation_of_the_session() {
    let before = engine_panics();
    panic_in("somebody");
    assert_eq!(engine_panics(), before);
    let names = [
        "lignindb-write-back",
        "lignindb-reclaim",
        "lignindb-cleaner",
    ];
    for (counted, name) in names.into_iter().enumerate() {
        panic_in(name);
        assert_eq!(engine_panics(), before + counted as u64 + 1);
    }

    let shape = Shape {
        ops: 8,
        stream_ops: 40,
        lineages: 1,
        max_plans: 3,
        again_one_in: 0,
        faults: Some(Faults::None),
        ..Shape::default()
    };
    let run = |shape: Shape| {
        let settings = Settings {
            seed: 1,
            runs: Some(1),
            shape,
            ..Settings::default()
        };
        campaign(&settings, &mut |_, _| {})
    };
    let control = run(shape);
    assert!(control.clean(), "{control}");
    let planted = Shape {
        planted: Some(Planted::ThreadPanic),
        ..shape
    };
    let report = run(planted);
    assert_eq!(report.tally.get(ACTED), 1, "{report}");
    assert_eq!(report.violations.len(), 1, "{report}");
    let found = &report.violations[0];
    assert_eq!(found.violation.kind, Kind::Panicked, "{report}");
    assert!(found.boundary.is_none(), "{report}");
    assert!(report.acts[0].shows_in(found), "{report}");
    let said = found.violation.to_string();
    assert!(said.contains("1 panics of a background thread"), "{said}");
    assert_eq!(report.tally.get("panics of the threads of the engine"), 1);
}
