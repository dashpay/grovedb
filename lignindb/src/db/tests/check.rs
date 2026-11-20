//! The check of a store by the physical invariant checker, at a close and
//! on demand (D-126 item 3).

use lignin_store::{cache::CacheError, flush::FlushError};

use super::*;
use crate::verify::Rule;

/// A database with two Merks, one of them dropped and reclaimed.
fn used(dir: &tempfile::TempDir) -> Db {
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 60, 300));
    plant(&db, subtree(2), Merk::sized(b'k', 20, 20));
    let dropped = Writes {
        drop: vec![subtree(1)],
        ..Writes::default()
    };
    dropped.commit(&db);
    reclaimed(&db, "the drop is reclaimed", |reclaimed| {
        reclaimed.merks == 1
    });
    db
}

/// Makes the store of `db` unsound in a way that only a check of a still
/// store finds: a page id that is in use and names no page.
fn spoil(db: &Db) -> lignin_format::PageId {
    let ids = &db.shared.store.ids;
    let id = ids.allocate().expect("an id");
    ids.publish(id).expect("it is private");
    id
}

/// A close that is asked for the report gives it, of the full check, and
/// panics for nothing: a sound store has no finding, and the report has
/// counted what the store holds.
#[test]
fn a_close_that_is_asked_gives_the_report_of_a_still_store() {
    let dir = temp();
    let report = used(&dir).close_and_verify(None).expect("the close");
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.live_cells, 20, "{report}");

    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    let id = spoil(&db);
    let report = db.close_and_verify(None).expect("the close");
    let found = |finding: &crate::verify::Finding| (finding.rule, finding.place);
    let found: Vec<_> = report.findings.iter().map(found).collect();
    let spoiled = (Rule::IdWithoutPage, crate::verify::Place::Page(id));
    assert_eq!(found, [spoiled], "{report}");
    assert_eq!(report.more_findings, 0, "{report}");
}

/// A check on demand reads the latest version as a reader does, beside
/// the background work and a writer, and finds a sound store sound. What
/// a store keeps beside its pages is not looked at: a page id in use that
/// names no page is found only by the check of a still store.
#[test]
fn a_check_on_demand_reads_what_a_reader_sees() {
    let dir = temp();
    let db = used(&dir);
    // A writer with a Merk of its own that is not committed: the check
    // reads the latest version, which has none of it.
    let txn = db.begin();
    {
        let scope = txn.scope().expect("a scope");
        let writes = Writes {
            enter: vec![(subtree(7), None)],
            merks: vec![(subtree(7), Merk::sized(b'k', 5, 20))],
            ..Writes::default()
        };
        let applied = writes.with(|batch| scope.apply(batch));
        applied.expect("the batch is applied");
        scope.merge();
    }
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.live_cells, 20, "{report}");
    db.rollback(&txn).expect("the transaction is rolled back");
    drop(txn);

    spoil(&db);
    assert!(db.verify(None).is_sound());
    db.unchecked_at_close();
    db.close().expect("the session ends");
}

/// In a debug build a close checks the store, and a store that is left
/// unsound is a panic with the report's text: from `close` and from the
/// drop of the handle. A handle that was taken out of the check closes
/// without a word.
#[cfg(debug_assertions)]
#[test]
fn a_close_of_an_unsound_store_panics_in_a_debug_build() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    for dropped in [false, true] {
        let dir = temp();
        let db = used(&dir);
        spoil(&db);
        let closed = catch_unwind(AssertUnwindSafe(move || match dropped {
            true => drop(db),
            false => db.close().expect("the session ends"),
        }));
        let panic = closed.expect_err("the store is unsound");
        let said = panic.downcast_ref::<String>().expect("a text");
        assert!(
            said.contains("is left unsound at version") && said.contains("NOT SOUND"),
            "dropped {dropped}: {said}"
        );
        // The session ended all the same, and the lock is free.
        let db = Db::open(dir.path(), &options()).expect("the database is opened again");
        assert!(db.verify(None).is_sound());
    }

    let dir = temp();
    let db = used(&dir);
    spoil(&db);
    db.unchecked_at_close();
    db.close().expect("the session ends");
}

/// A session that ended for writes is not checked at its close: what it
/// leaves is the next recovery's to make whole. The close itself does
/// its work, so it is the session's end for writes that leaves the check
/// out. A release build checks no close, so only a debug build tells the
/// two apart.
#[cfg(debug_assertions)]
#[test]
fn a_session_that_ended_for_writes_is_not_checked_at_its_close() {
    let dir = temp();
    let db = used(&dir);
    let store = &*db.shared.store;
    let commit = store.committing.lock();
    let failed = FlushError::from(CacheError::WrongPool);
    store.committing.sync_failed(db.version(), &failed);
    drop(commit);
    assert!(db.reopen().is_some());
    spoil(&db);
    db.close().expect("the session ends");
}

/// The checker reads an open database through the handle: a new one, one
/// with a Merk that hosts another and a record of a column, the same
/// closed and opened again, and one that holds what a store must not.
#[test]
fn the_physical_checker_reads_an_open_database() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    assert!(db.verify(None).is_sound());
    let hosting = [b'h', 0, 0, 0, 4];
    let host = Merk::sized(b'h', 9, 30).hosting(&hosting, subtree(2));
    Writes {
        enter: vec![
            (subtree(1), None),
            (subtree(2), Some((subtree(1), hosting.to_vec()))),
        ],
        merks: vec![(subtree(1), host), (subtree(2), Merk::sized(b'k', 40, 600))],
        records: vec![(Column::Aux, subtree(2), b"k".to_vec(), vec![7; 500])],
        ..Writes::default()
    }
    .commit(&db);
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.merks, 2);
    // A database that is closed is still.
    let report = db.close_and_verify(None).expect("a close");
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.merks, 2);

    // What a store must not hold is found: a subtree whose entry names a
    // host that is no node of the Merk it is said to be in.
    let db = Db::open(dir.path(), &options()).expect("the database again");
    Writes {
        enter: vec![(subtree(3), Some((subtree(1), b"no node".to_vec())))],
        ..Writes::default()
    }
    .commit(&db);
    let report = db.verify(None);
    assert!(report.has(Rule::EntryHost), "{report}");
    assert!(!report.is_sound());
    // The subtree goes again, and the database is sound as before.
    Writes {
        drop: vec![subtree(3)],
        ..Writes::default()
    }
    .commit(&db);
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
}

/// A check on demand holds the reclaiming back from before it pins its
/// version until it has ended (D-183). A Merk that hosts another is
/// dropped, and once the check has pinned that version a commit writes
/// the leaf of the column that holds a record of the hosted Merk. A
/// reclaiming from then on takes that record out of the leaf's newest
/// version, which the pinned version does not read, and the hosted
/// Merk's entry out of the directory's page, which it does read: the
/// check would find a flat record with no entry in a sound store. The
/// reclaiming that comes due here waits, the check finds nothing, and the
/// reclaiming is made when the check has ended.
///
/// The background threads are ended, so the lock of the reclaiming is
/// held by the check or by nobody when it is asked for, and the
/// reclaiming is made on a thread of the test's.
#[test]
fn a_check_on_demand_holds_the_reclaiming_back_until_it_has_ended() {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::TryLockError,
    };

    use crate::db::store::ONCE_PINNED;

    let dir = temp();
    let db = Arc::new(Db::open(dir.path(), &options()).expect("a new database"));
    let (outer, inner, beside) = (subtree(1), subtree(2), subtree(9));
    let hosting = [b'h', 0, 0, 0, 4];
    Writes {
        enter: vec![
            (outer, None),
            (inner, Some((outer, hosting.to_vec()))),
            (beside, None),
        ],
        merks: vec![
            (outer, Merk::sized(b'h', 9, 30).hosting(&hosting, inner)),
            (inner, Merk::sized(b'k', 12, 40)),
            (beside, Merk::sized(b'k', 5, 20)),
        ],
        records: vec![
            (Column::Aux, inner, b"k".to_vec(), vec![7; 40]),
            (Column::Aux, beside, b"k".to_vec(), vec![8; 40]),
        ],
        ..Writes::default()
    }
    .commit(&db);
    halt_background(&db);
    Writes {
        drop: vec![outer],
        ..Writes::default()
    }
    .commit(&db);
    let dropped_at = db.version();

    // Every round until nothing waits: the dropped Merk and the one it
    // hosted.
    fn reclaim(db: &Db) {
        for _ in 0..1_000 {
            if db.reclaim().waiting == 0 {
                return;
            }
        }
        panic!("the drop is not reclaimed: {}", db.stats());
    }
    let held_back = Rc::new(Cell::new(None));
    let due = Rc::new(RefCell::new(None));
    let (store, held, waits) = (db.clone(), held_back.clone(), due.clone());
    ONCE_PINNED.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            Writes {
                records: vec![(Column::Aux, beside, b"more".to_vec(), vec![9; 40])],
                ..Writes::default()
            }
            .commit(&store);
            assert!(store.version() > dropped_at);
            let asked = store.shared.store.reclaiming.try_lock();
            let free = !matches!(asked, Err(TryLockError::WouldBlock));
            drop(asked);
            held.set(Some(!free));
            match free {
                // Nothing holds the reclaiming back: it is made here,
                // behind the pin and before the check reads a page.
                true => reclaim(&store),
                false => *waits.borrow_mut() = Some(thread::spawn(move || reclaim(&store))),
            }
        }));
    });
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.merks, 1, "{report}");
    assert_eq!(
        held_back.get(),
        Some(true),
        "the reclaiming was free while the check ran"
    );

    // The reclaiming goes on when the check has ended, and leaves
    // nothing: the next check reads a version with it made.
    let waited = due.borrow_mut().take().expect("a reclaiming that waited");
    waited.join().expect("the reclaiming is made");
    let stats = db.stats();
    let reclaimed = &stats.reclaimed;
    assert_eq!(
        (reclaimed.merks, reclaimed.nested, reclaimed.waiting),
        (1, 1, 0),
        "{stats}"
    );
    assert!(reclaimed.flat_records > 0, "{stats}");
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.unreached.cells, 0, "{report}");
}
