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

    // The same of the database that the next open recovers: as a reader
    // sees it, and still, once that session is closed.
    let db = Db::open(dir.path(), &options()).expect("the database again");
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.merks, 2);
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

/// A check on demand pins its version under the commit lock with the
/// reclaiming held back (D-193), and holds the reclaiming back until the
/// checker has returned (D-183). Four looks at the check show it: with
/// both locks held no version is pinned yet; then one is, and neither
/// lock is free; before the first read the reclaiming is held; and when
/// the checker has returned it is held still, with nothing reclaimed. The reclaiming that came due is asked for after
/// the last look, and is made when the check has returned.
///
/// A Merk that hosts another is dropped, and once the check has pinned
/// that version a commit writes the leaf of the column that holds a
/// record of the hosted Merk. A reclaiming from then on takes that record
/// out of the leaf's newest version, which the pinned version does not
/// read, and the hosted Merk's entry out of the directory's page, which
/// it does read: the check would find a flat record with no entry in a
/// sound store. The check finds nothing, and the reclaiming is made when
/// the check has ended.
///
/// The background threads are ended, so the lock of the reclaiming is
/// held by the check or by nobody when it is asked for, and the
/// reclaiming is made on a thread of the test's.
#[test]
fn a_check_on_demand_holds_the_reclaiming_back_until_it_has_ended() {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::{mpsc, TryLockError},
    };

    use crate::db::store::{VerifyStage, AT_VERIFY};

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
    // Whether the reclaiming is held: by the check, since nothing else
    // of this test holds it when it is asked.
    fn held_back(db: &Db) -> bool {
        let asked = db.shared.store.reclaiming.try_lock();
        matches!(asked, Err(TryLockError::WouldBlock))
    }
    // What each look found: the commit lock, the reclaiming and the
    // pins that are held, before the pin and after it; the reclaiming before the first read; the reclaiming, and how
    // many Merks were reclaimed, when the checker had returned.
    let looks = Rc::new(RefCell::new(Vec::new()));
    let due = Rc::new(Cell::new(None));
    // The reclaiming that comes due is asked for by a thread that begins
    // when the last look has been made: at each look the check holds the
    // reclaiming back, or nobody does.
    let (begin, begun) = mpsc::channel::<()>();
    let begun = Cell::new(Some(begun));
    let (store, looked, waits) = (db.clone(), looks.clone(), due.clone());
    AT_VERIFY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |stage| {
            let held = held_back(&store);
            let found = match stage {
                VerifyStage::Locked | VerifyStage::Pinned => {
                    let committing = store.shared.store.committing.try_lock().is_none();
                    let pins = store.shared.versions.pins_held();
                    let stage = match stage {
                        VerifyStage::Locked => "locked",
                        _ => "pinned",
                    };
                    format!("{stage}: commit lock {committing}, reclaiming {held}, {pins} pins")
                }
                VerifyStage::Reading => {
                    Writes {
                        records: vec![(Column::Aux, beside, b"more".to_vec(), vec![9; 40])],
                        ..Writes::default()
                    }
                    .commit(&store);
                    assert!(store.version() > dropped_at);
                    let reclaiming = store.clone();
                    let begun = begun.take().expect("one look before the first read");
                    match held {
                        // Nothing holds the reclaiming back: it is made
                        // here, behind the pin and before the check
                        // reads a page.
                        false => reclaim(&store),
                        true => waits.set(Some(thread::spawn(move || {
                            let _ = begun.recv();
                            reclaim(&reclaiming);
                        }))),
                    }
                    format!("reading: reclaiming {held}")
                }
                VerifyStage::Checked => {
                    let merks = store.stats().reclaimed.merks;
                    // From here the reclaiming is asked for, and waits
                    // for the check to return.
                    let _ = begin.send(());
                    format!("checked: reclaiming {held}, {merks} reclaimed")
                }
            };
            looked.borrow_mut().push(found);
        }));
    });
    let report = db.verify(None);
    AT_VERIFY.with(|hook| hook.borrow_mut().take());
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.merks, 1, "{report}");
    assert_eq!(
        *looks.borrow(),
        [
            "locked: commit lock true, reclaiming true, 0 pins",
            "pinned: commit lock true, reclaiming true, 1 pins",
            "reading: reclaiming true",
            "checked: reclaiming true, 0 reclaimed",
        ]
    );

    // The reclaiming goes on when the check has ended, and leaves
    // nothing: the next check reads a version with it made.
    let waited = due.take().expect("a reclaiming that waited");
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

/// A session that the store has ended (OPS-08): a sync of the log was
/// refused, and the session takes no commit any more. Its readers read
/// on, and a check on demand reads as they do: the latest version that
/// was published, which is sound. Its close cannot make the log durable
/// and returns that error, and a close that did not do all of its work
/// makes no check, in any build: the store is spoiled here as only the
/// check of a still store finds it, and the close returns its error with
/// no panic. What the session left is the next recovery's to make whole,
/// and the next session is checked like any other.
///
/// A session that ended for writes and whose close does its work is
/// `a_session_that_ended_for_writes_is_not_checked_at_its_close`.
#[test]
fn a_session_that_the_store_ended_is_checked_on_demand_as_its_readers_read_it() {
    use lignin_io::sim::{Event, Fault, FaultRule, OpKind, OpSet};
    use lignin_store::wal::SyncPolicy;

    use super::full::Session;
    use crate::backup::tests::world::{first, next};

    let mut session = Session::new(31, SyncPolicy::Never, true);
    assert!(session.open());
    session.write("w1", first, false);
    assert!(!session.ticks("t1"));
    session.write("w2", next, false);
    let db = session.db.take().expect("the session");
    let before = db.verify(None);
    assert!(before.is_sound(), "{before}");

    let mut appends = session.fs.trace().into_iter().rev();
    let log = appends.find_map(|event| match event {
        Event::Op(op) if op.kind == OpKind::Append && op.path.contains("/wal/") => Some(op.path),
        _ => None,
    });
    let log = log.expect("the log was appended to");
    let refusing = FaultRule::new(Fault::Enospc).on(OpSet::SYNCS).at(&log);
    session.fs.add_rule(refusing);
    let refused = db.shared.write_back().sync_log();
    refused.expect_err("the sync is refused");
    session.fs.clear_rules();
    assert!(db.reopen().is_some(), "the session has ended");

    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census, before.census, "{report}");

    // The session's background work was at rest, with its threads
    // ended. The handle is given threads to end, so that its close does
    // what the close of a session does.
    let ended = || thread::spawn(|| {});
    *crate::db::store::lock(&db.threads) = Some(crate::db::Threads {
        write_back: ended(),
        reclaim: ended(),
        cleaner: ended(),
    });
    spoil(&db);
    let version = db.version();
    let closed = db.close();
    assert!(
        matches!(&closed, Err(crate::db::DbError::Flush(_))),
        "{closed:?}"
    );

    session.options.create_if_missing = false;
    let db = Db::open("/db", &session.options).expect("the next session");
    assert_eq!(db.reopen(), None);
    assert_eq!(db.version(), version);
    let report = db.close_and_verify(None).expect("the close");
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.live_cells, before.census.live_cells);
}

/// The checks of a database hold the data column of a subtree to the tree
/// type of its entry once they are given the tree types that keep flat
/// records, and not before: the check on demand and the check of a close
/// that is asked for its report. A handle of the next session was given
/// none, and holds the column to nothing.
#[test]
fn the_checks_hold_the_data_column_to_the_tree_types_they_are_given() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    // A Merk with a node that no node links, and a subtree with no node
    // and a flat record that is no such node.
    let merk = Merk::sized(b'k', 9, 30).with_unlinked(b"unlinked", b"a value");
    Writes {
        enter: vec![(subtree(1), None), (subtree(2), None)],
        merks: vec![(subtree(1), merk)],
        records: vec![(
            Column::Data,
            subtree(2),
            b"a record".to_vec(),
            b"any bytes".to_vec(),
        )],
        ..Writes::default()
    }
    .commit(&db);
    let rules = |report: &crate::verify::Report| -> Vec<Rule> {
        report.findings.iter().map(|found| found.rule).collect()
    };
    assert!(db.verify(None).is_sound());
    // Where every tree type keeps flat records, no record has a form to
    // keep.
    db.check_kinds_by(|_| true);
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    // Where the tree type of the two subtrees keeps a Merk, the flat
    // record is the one finding.
    db.check_kinds_by(|tree_type| tree_type != TREE_TYPE);
    let report = db.verify(None);
    assert_eq!(rules(&report), [Rule::RecordKind], "{report}");
    let report = db.close_and_verify(None).expect("a close");
    assert_eq!(rules(&report), [Rule::RecordKind], "{report}");

    let db = Db::open(dir.path(), &options()).expect("the database again");
    assert!(db.verify(None).is_sound());
    let report = db.close_and_verify(None).expect("a close");
    assert!(report.is_sound(), "{report}");
}
