//! The reclaimer of what drops left, started by the engine (CL6, D-103):
//! from what each commit dropped, in page steps whose records go to the
//! log through write-back, beside write transactions on other threads.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    thread,
};

use lignin_io::sim::{SimConfig, SimVfs};

use super::*;
use crate::{commit::CommitError, db::WriteError};

fn drop_of(subtrees: &[Subtree]) -> Writes {
    Writes {
        drop: subtrees.to_vec(),
        ..Writes::default()
    }
}

#[test]
fn what_a_drop_left_is_reclaimed_once_no_reader_from_before_is_left() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 400, 500));
    plant(&db, subtree(2), Merk::sized(b'k', 50, 20));
    let before = db.snapshot();
    let dropped = drop_of(&[subtree(1)]).commit(&db);
    assert_eq!(dropped.dropped.len(), 1);

    // A reader from before the drop still reads all of it, so nothing is
    // taken out: the reclaiming waits for it.
    let round = db.reclaim();
    assert_eq!((round.finished, round.waiting), (0, 1));
    assert!(!round.holds_cells && !round.more);
    assert_eq!(
        seen(&before.operation(), subtree(1), b"k"),
        Some((400, vec![]))
    );
    let waiting = db.stats().reclaimed;
    assert_eq!((waiting.merks, waiting.waiting, waiting.cells), (0, 1, 0));

    // With the reader gone the background thread takes it out: the drop
    // of the pin woke it.
    drop(before);
    wait_for("the dropped Merk is reclaimed", || {
        db.stats().reclaimed.merks == 1
    });
    let stats = db.stats();
    let reclaimed = stats.reclaimed;
    assert_eq!(
        (reclaimed.cells, reclaimed.waiting, reclaimed.given_up),
        (400, 0, 0)
    );
    // The record that the drop left of the Merk, and the pages that held
    // nothing but its cells.
    assert!(
        reclaimed.records >= 1 && reclaimed.pages_freed > 0,
        "{reclaimed:?}"
    );
    assert_eq!(stats.background.reclaim_errors, 0, "{:?}", stats.background);
    // The page steps went to the log through write-back.
    assert!(stats.store.log_bytes > 0);
    // The ids of the freed pages come back after the next commits, and
    // one handle on the directory is kept.
    plant(&db, subtree(3), Merk::sized(b'k', 10, 20));
    plant(&db, subtree(4), Merk::sized(b'k', 10, 20));
    wait_for("the page ids are released", || {
        db.reclaim();
        db.stats().ids_released > 0
    });
    wait_for("the handles of older versions go", || {
        db.reclaim();
        db.stats().directories == 1
    });
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(seen(&operation, subtree(1), b"k"), None);
    assert_eq!(seen(&operation, subtree(2), b"k"), Some((50, vec![])));
    drop(operation);
    drop(snapshot);
    db.close().expect("the session ends");

    // The next session recovers the steps, and has nothing to reclaim.
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    assert_eq!(db.reclaim().waiting, 0);
    let snapshot = db.snapshot();
    assert_eq!(seen(&snapshot.operation(), subtree(1), b"k"), None);
    assert_eq!(
        seen(&snapshot.operation(), subtree(2), b"k"),
        Some((50, vec![]))
    );
}

/// What a session dropped and did not reclaim, the next one reclaims: it
/// reads the records of the drops when it opens.
#[test]
fn the_drops_of_an_earlier_session_are_reclaimed_by_the_next() {
    let fs = SimVfs::new(SimConfig::new(11));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    let db = Db::open("/db", &options).expect("a new database");
    let outer = Merk::sized(b'k', 200, 300).hosting(&[b'k', 0, 0, 0, 3], subtree(2));
    let writes = Writes {
        enter: vec![
            (subtree(1), None),
            (subtree(2), Some((subtree(1), vec![b'k', 0, 0, 0, 3]))),
            (subtree(3), None),
        ],
        merks: vec![(subtree(2), Merk::sized(b'k', 80, 20)), (subtree(1), outer)],
        records: vec![
            (Column::Aux, subtree(2), b"k".to_vec(), vec![1; 30]),
            (Column::Aux, subtree(3), b"k".to_vec(), vec![3; 30]),
        ],
        ..Writes::default()
    };
    writes.commit(&db);
    // A reader keeps the reclaimer from beginning, and the power goes.
    let before = db.snapshot();
    drop_of(&[subtree(1)]).commit(&db);
    db.flush().expect("the log is synced");
    assert_eq!(db.reclaim().waiting, 1);
    let crashed = fs.crash(3);
    drop(before);
    drop(db);

    let options = Options {
        vfs: Arc::new(crashed),
        ..options
    };
    let db = Db::open("/db", &options).expect("the database is recovered");
    // The drop that was left is the session's to reclaim, and its
    // background thread may have done so by now.
    let adopted = db.stats().reclaimed;
    assert_eq!(adopted.merks + adopted.waiting, 1, "{adopted:?}");
    wait_for("the drop of the session before is reclaimed", || {
        db.reclaim();
        let reclaimed = db.stats().reclaimed;
        reclaimed.merks == 1 && reclaimed.waiting == 0
    });
    let reclaimed = db.stats().reclaimed;
    assert_eq!((reclaimed.nested, reclaimed.cells), (1, 280));
    assert_eq!(
        reclaimed.flat_records, 1,
        "the record of the nested subtree"
    );
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(seen(&operation, subtree(1), b"k"), None);
    assert_eq!(seen(&operation, subtree(2), b"k"), None);
    let kept = seen(&operation, subtree(3), b"k");
    assert_eq!(kept, Some((0, vec![(Column::Aux, vec![3; 30])])));
}

/// What the writer of the test below has in use after its commit of
/// `round`: the two newest of its subtrees, each with a nested one.
fn live(round: u32) -> [u32; 2] {
    [round.saturating_sub(1), round]
}

const NODES: u32 = 150;
const NESTED: u32 = 40;

/// D-103 item 4: the reclaimer beside a writer of `lignindb::txn`, each on
/// a thread of its own, with readers beside both. The writer keeps its
/// transaction open over the pages the reclaimer works on, so steps are
/// refused and made again. Every reader sees each subtree that is in use
/// at its version whole and nothing of one that was dropped, every step
/// is in the log, and the next session has the same.
#[test]
fn the_reclaimer_runs_beside_a_writer_and_readers_on_threads() {
    const ROUNDS: u32 = 60;
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let index = subtree(0xee);
    let enter = Writes {
        enter: vec![(index, None)],
        ..Writes::default()
    };
    enter.commit(&db);
    let committed = AtomicU32::new(0);
    let done = AtomicBool::new(false);
    let outer_of = |round: u32| numbered(round * 2);
    let inner_of = |round: u32| numbered(round * 2 + 1);
    let host_key = vec![b'k', 0, 0, 0, 1];

    thread::scope(|threads| {
        let _ending = Ending(&done);
        let (db, done, committed, host_key) = (&db, &done, &committed, &host_key);
        let check = move |operation: &Operation<'_>| -> u32 {
            // The index says which round the version is of.
            let opened = operation.open(index).expect("a lookup").expect("entered");
            let mut value = Vec::new();
            let Ok(true) = opened.get(Column::Aux, b"round", &mut value) else {
                return 0;
            };
            let round = u32::from_be_bytes(value[..].try_into().expect("four bytes"));
            for in_use in live(round) {
                if in_use == 0 {
                    continue;
                }
                let outer = seen(operation, outer_of(in_use), b"k");
                let records = vec![(Column::Aux, vec![in_use as u8; 20])];
                assert_eq!(outer, Some((NODES as usize, records)), "round {round}");
                let inner = seen(operation, inner_of(in_use), b"k");
                assert_eq!(inner, Some((NESTED as usize, vec![])), "round {round}");
            }
            // What was dropped two rounds ago and before is gone, whether
            // or not it is reclaimed.
            for gone in round.saturating_sub(4)..round.saturating_sub(1) {
                if gone > 0 {
                    assert_eq!(seen(operation, outer_of(gone), b"k"), None, "round {round}");
                    assert_eq!(seen(operation, inner_of(gone), b"k"), None, "round {round}");
                }
            }
            round
        };
        let readers: Vec<_> = (0..3)
            .map(|reader| {
                threads.spawn(move || {
                    let mut lease = crate::snapshot::LatestLease::new();
                    let mut rounds = 0u64;
                    while !done.load(Ordering::Acquire) {
                        // A snapshot that is held over several operations,
                        // and an operation that pins nothing.
                        if reader == 0 {
                            check(&db.read(&mut lease));
                        } else {
                            let snapshot = db.snapshot();
                            let first = check(&snapshot.operation());
                            thread::yield_now();
                            assert_eq!(check(&snapshot.operation()), first);
                        }
                        rounds += 1;
                    }
                    rounds
                })
            })
            .collect();

        let writer = threads.spawn(move || {
            let mut conflicts = 0;
            for round in 1..=ROUNDS {
                let outer = Merk::sized(b'k', NODES, 300).hosting(host_key, inner_of(round));
                let writes = Writes {
                    enter: vec![
                        (outer_of(round), None),
                        (inner_of(round), Some((outer_of(round), host_key.clone()))),
                    ],
                    drop: match round > 2 {
                        true => vec![outer_of(round - 2)],
                        false => Vec::new(),
                    },
                    merks: vec![
                        (inner_of(round), Merk::sized(b'k', NESTED, 20)),
                        (outer_of(round), outer),
                    ],
                    records: vec![
                        (
                            Column::Aux,
                            outer_of(round),
                            b"k".to_vec(),
                            vec![round as u8; 20],
                        ),
                        (
                            Column::Aux,
                            index,
                            b"round".to_vec(),
                            round.to_be_bytes().to_vec(),
                        ),
                    ],
                };
                loop {
                    let txn = db.begin();
                    let scope = txn.scope().expect("a scope");
                    writes
                        .with(|batch| scope.apply(batch))
                        .expect("the batch is applied");
                    scope.merge();
                    // The transaction holds its copies while the reclaimer
                    // works: on the pages of the directory, which both
                    // write, a step waits for the commit.
                    if round % 3 == 0 {
                        thread::sleep(Duration::from_millis(2));
                    }
                    match db.commit(&txn, &mut NoStaleHashes) {
                        Ok(Committed::Published(_)) => break,
                        Err(WriteError::Commit(CommitError::Conflict)) => conflicts += 1,
                        other => panic!("a commit, and not {other:?}"),
                    }
                }
                committed.store(round, Ordering::Release);
            }
            conflicts
        });
        assert_eq!(
            writer.join().expect("the writer ends"),
            0,
            "nobody else writes"
        );
        done.store(true, Ordering::Release);
        for reader in readers {
            assert!(reader.join().expect("a reader ends") > 0);
        }
    });
    assert_eq!(committed.load(Ordering::Acquire), ROUNDS);

    // Everything that was dropped is reclaimed, with every step made
    // through the log.
    reclaimed(&db, "every drop is reclaimed", |reclaimed| {
        reclaimed.merks == u64::from(ROUNDS - 2) && reclaimed.waiting == 0
    });
    let stats = db.stats();
    let reclaimed = stats.reclaimed;
    println!("{reclaimed:?}");
    assert_eq!(reclaimed.nested, u64::from(ROUNDS - 2));
    assert_eq!(reclaimed.cells, u64::from((ROUNDS - 2) * (NODES + NESTED)));
    assert_eq!(reclaimed.given_up, 0);
    assert_eq!(stats.background.reclaim_errors, 0, "{:?}", stats.background);
    assert_eq!(
        stats.background.write_back_errors, 0,
        "{:?}",
        stats.background
    );
    assert_eq!((stats.pins, stats.guards), (0, 0));
    let last = |db: &Db| {
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        for round in 1..=ROUNDS {
            let found = seen(&operation, outer_of(round), b"k").map(|(nodes, _)| nodes);
            let expected = (round + 1 >= ROUNDS).then_some(NODES as usize);
            assert_eq!(found, expected, "round {round}");
            let nested = seen(&operation, inner_of(round), b"k").map(|(nodes, _)| nodes);
            assert_eq!(nested, (round + 1 >= ROUNDS).then_some(NESTED as usize));
        }
    };
    last(&db);
    db.close().expect("the session ends");
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    last(&db);
    assert_eq!(db.reclaim().waiting, 0);
}

/// A page that is freed again before write-back has written it
/// (`lignin_tree::drop`, choice 3; D-098 item 4 for a commit): its id
/// named another page before, the newest link of the id is that page's
/// free link, and the step that frees it is given no second one. The
/// reclaiming goes on, and a recovery of the log that holds such steps
/// lands on what the session had.
///
/// Write-back is held still here, so no page of the session has a block
/// and every id that is used again is in that state.
#[test]
fn a_page_that_is_freed_before_it_was_written_back_is_reclaimed_and_recovered() {
    let fs = SimVfs::new(SimConfig::new(17));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    let db = Db::open("/db", &options).expect("a new database");
    halt_background(&db);

    let settle = |merks: u64| {
        for _ in 0..1_000 {
            db.reclaim();
            let reclaimed = db.stats().reclaimed;
            if reclaimed.merks == merks && reclaimed.waiting == 0 {
                return reclaimed;
            }
        }
        panic!("the drops are not reclaimed: {:?}", db.stats());
    };
    plant(&db, subtree(9), Merk::sized(b'k', 40, 20));
    // The first life of the ids: a Merk that is dropped and reclaimed. Its
    // pages never had a link, and are given their free links.
    plant(&db, subtree(1), Merk::sized(b'a', 300, 30));
    drop_of(&[subtree(1)]).commit(&db);
    let first = settle(1);
    assert!(first.pages_freed > 2, "{first:?}");
    assert_eq!((first.pages_never_linked, first.given_up), (0, 0));
    // Two commits and their reclaims bring the ids back.
    for name in [20, 21] {
        enter_alone(&db, subtree(name));
        db.reclaim();
    }
    assert!(db.stats().ids_released >= first.pages_freed);

    // The second life: a Merk in pages under those ids, dropped before a
    // block holds one of them.
    plant(&db, subtree(2), Merk::sized(b'b', 300, 30));
    drop_of(&[subtree(2)]).commit(&db);
    let second = settle(2);
    assert!(second.pages_never_linked > 2, "{second:?}");
    assert_eq!(second.given_up, 0);
    assert_eq!(second.pages_freed, 2 * first.pages_freed);
    assert_eq!(second.cells, 600);
    let background = db.stats().background;
    assert_eq!(background.reclaim_errors, 0, "{background:?}");
    // A third life, which stays.
    for name in [22, 23] {
        enter_alone(&db, subtree(name));
        db.reclaim();
    }
    plant(&db, subtree(3), Merk::sized(b'c', 300, 30));
    let check = |db: &Db| {
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        assert_eq!(seen(&operation, subtree(9), b"k"), Some((40, vec![])));
        assert_eq!(seen(&operation, subtree(3), b"k"), Some((300, vec![])));
        for gone in [1, 2] {
            assert_eq!(seen(&operation, subtree(gone), b"k"), None);
        }
    };
    check(&db);
    let version = db.version();
    let live = db.shared.store.ids.census().live;
    db.flush().expect("the log is synced");

    // The power goes. No page was written back, so the recovery replays
    // every commit and every step, those that freed a page with no link
    // among them, and has the pages and the ids the session had.
    for draw in 0..6 {
        let crashed = fs.crash(draw);
        let options = Options {
            vfs: Arc::new(crashed),
            ..options.clone()
        };
        let recovered = Db::open("/db", &options)
            .unwrap_or_else(|error| panic!("draw {draw}: the recovery: {error}"));
        assert_eq!(recovered.version(), version, "draw {draw}");
        let recovery = recovered.stats().recovery;
        assert!(
            recovery.steps > 0 && recovery.pages_freed > 0,
            "{recovery:?}"
        );
        check(&recovered);
        assert_eq!(
            recovered.shared.store.ids.census().live,
            live,
            "draw {draw}"
        );
        // The session goes on: nothing waits, and new pages are made,
        // written back and read after a close.
        assert_eq!(recovered.reclaim().waiting, 0);
        plant(&recovered, subtree(4), Merk::sized(b'd', 300, 30));
        recovered.close().expect("the session ends");
        let again = Db::open("/db", &options).expect("the database is opened again");
        check(&again);
        assert_eq!(
            seen(&again.snapshot().operation(), subtree(4), b"k"),
            Some((300, vec![]))
        );
    }
    drop(db);
}

/// Enters `subtree` with no host and nothing in it, in a commit of its
/// own.
fn enter_alone(db: &Db, subtree: Subtree) {
    let writes = Writes {
        enter: vec![(subtree, None)],
        ..Writes::default()
    };
    writes.commit(db);
}

/// A step whose record the log does not take changes nothing, is counted
/// and told, and is made again once the log takes records: the reclaiming
/// ends, and nothing is given up.
#[test]
fn a_step_that_the_log_refuses_is_made_again() {
    use lignin_io::sim::{Fault, FaultRule, OpKind};
    let fs = SimVfs::new(SimConfig::new(29));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    let db = Db::open("/db", &options).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 300, 30));
    plant(&db, subtree(2), Merk::sized(b'k', 30, 30));
    // A reader holds the reclaiming back until every append fails.
    let before = db.snapshot();
    drop_of(&[subtree(1)]).commit(&db);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Append)
            .torn_append(false)
            .forever(),
    );
    drop(before);
    wait_for("a step is refused", || {
        db.reclaim();
        db.stats().background.reclaim_errors > 0
    });
    let stats = db.stats();
    assert_eq!((stats.reclaimed.merks, stats.reclaimed.given_up), (0, 0));
    assert_eq!(stats.reclaimed.waiting, 1);
    let told = stats.background.last_error.expect("the error is kept");
    assert!(
        told.contains("reclaiming: ") || told.contains("write-back: "),
        "{told}"
    );
    // Nothing of the subtree is shown meanwhile, and the other one is
    // whole.
    let snapshot = db.snapshot();
    assert_eq!(seen(&snapshot.operation(), subtree(1), b"k"), None);
    assert_eq!(
        seen(&snapshot.operation(), subtree(2), b"k"),
        Some((30, vec![]))
    );
    drop(snapshot);

    // The file layer is whole again. The log is cut where its last whole
    // item ends, by write-back or by the next commit, and the step is
    // made.
    fs.clear_rules();
    wait_for("the log takes a commit again", || {
        let writes = Writes {
            enter: vec![(subtree(7), None)],
            ..Writes::default()
        };
        let committed = writes.with(|batch| db.write(batch, &mut NoStaleHashes));
        matches!(committed, Ok(Committed::Published(_))) || db.reopen().is_some()
    });
    assert!(db.reopen().is_none(), "{:?}", db.reopen());
    reclaimed(&db, "the drop is reclaimed", |reclaimed| {
        reclaimed.merks == 1 && reclaimed.waiting == 0
    });
    let stats = db.stats();
    assert_eq!((stats.reclaimed.cells, stats.reclaimed.given_up), (300, 0));
    assert_eq!(
        seen(&db.snapshot().operation(), subtree(2), b"k"),
        Some((30, vec![]))
    );
    db.close().expect("the session ends");
    let db = Db::open("/db", &options).expect("the database is opened again");
    let snapshot = db.snapshot();
    assert_eq!(seen(&snapshot.operation(), subtree(1), b"k"), None);
    assert_eq!(
        seen(&snapshot.operation(), subtree(2), b"k"),
        Some((30, vec![]))
    );
}
