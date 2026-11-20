//! The check of a store by the physical invariant checker, at a close and
//! on demand (D-126 item 3).

use lignin_store::{cache::CacheError, flush::FlushError};

use super::*;

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
fn spoil(db: &Db) {
    let ids = &db.shared.store.ids;
    let id = ids.allocate().expect("an id");
    ids.publish(id).expect("it is private");
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
    spoil(&db);
    let report = db.close_and_verify(None).expect("the close");
    assert_eq!(report.findings.len(), 1, "{report}");
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
/// out.
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
