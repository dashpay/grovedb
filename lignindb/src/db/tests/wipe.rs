//! Wipe (P-23, CV3, CF5, T-WP1): one commit that leaves no record in any
//! column, for every reader from that commit on.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
};

use lignin_io::sim::{SimConfig, SimVfs};

use super::*;
use crate::{
    commit::CommitError,
    db::WriteError,
    txn::{BatchError, TxnError, Unit},
};

/// A grove of five subtrees: Merks with values in their cells and out of
/// line, one Merk nested in another, one subtree whose data column is
/// flat, and a record in every other column of each.
fn grove(db: &Db) {
    let record = |column, name: u8| (column, subtree(name), b"k".to_vec(), vec![name; 40]);
    let outer = Merk::sized(b'k', 300, 24).hosting(&[b'k', 0, 0, 0, 7], subtree(3));
    let writes = Writes {
        enter: vec![
            (subtree(1), None),
            (subtree(2), None),
            (subtree(3), Some((subtree(2), vec![b'k', 0, 0, 0, 7]))),
            (subtree(4), None),
            (subtree(5), None),
        ],
        merks: vec![
            (subtree(1), Merk::sized(b'k', 200, 600)),
            (subtree(3), Merk::sized(b'k', 120, 24)),
            (subtree(2), outer),
        ],
        records: (1..=5)
            .flat_map(|name| [Column::Aux, Column::Roots, Column::Meta].map(|c| record(c, name)))
            .chain((0..50u8).map(|n| (Column::Data, subtree(4), vec![b'f', n], vec![n; 700])))
            .chain([record(Column::Data, 4)])
            .collect(),
        ..Writes::default()
    };
    writes.commit(db);
}

/// What a reader finds of the grove: for each subtree, its nodes and its
/// records under `k`.
fn read(operation: &Operation<'_>) -> Vec<Option<(usize, usize)>> {
    (1..=5)
        .map(|name| {
            seen(operation, subtree(name), b"k").map(|(nodes, records)| (nodes, records.len()))
        })
        .collect()
}

const FULL: [Option<(usize, usize)>; 5] = [
    Some((200, 3)),
    Some((300, 3)),
    Some((120, 3)),
    Some((0, 4)),
    Some((0, 3)),
];

#[test]
fn a_wipe_empties_every_column_in_one_commit() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    grove(&db);
    std::fs::write(dir.path().join(".fsck"), b"kept").expect("a file");
    let before = db.snapshot();
    assert_eq!(read(&before.operation()), FULL);

    // Three write transactions are open: one wrote into a subtree, one
    // entered a subtree of its own, one wrote nothing.
    let wrote = db.begin();
    let scope = wrote.scope().expect("a scope");
    let put = Writes {
        records: vec![(Column::Aux, subtree(5), b"t".to_vec(), b"mine".to_vec())],
        ..Writes::default()
    };
    put.with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();
    let entered = db.begin();
    let scope = entered.scope().expect("a scope");
    let enter = Writes {
        enter: vec![(subtree(8), None)],
        ..Writes::default()
    };
    enter
        .with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();
    let idle = db.begin();

    let at = db.version();
    let Ok(Committed::Published(wiped)) = db.wipe() else {
        panic!("the wipe is a commit");
    };
    assert_eq!(Some(wiped.version), at.next(), "one commit");
    assert_eq!(wiped.dropped.len(), 4, "every subtree that no other hosts");

    // Nothing is left for a reader of the wipe's version: no node and no
    // record, by key or under a cursor, in any column.
    let after = db.snapshot();
    let operation = after.operation();
    assert_eq!(read(&operation), [None; 5]);
    // The pages still hold what was there, and nobody is shown it: the
    // open transactions read nothing of it either.
    let mut value = Vec::new();
    for name in 1..=5 {
        for column in Column::ALL {
            let found = idle.get(Unit::new(column, subtree(name)), b"k", &mut value);
            assert!(!found.expect("a read"), "{column} of {name}");
        }
        let mut cursor = idle.cursor(Unit::new(Column::Data, subtree(name)));
        cursor.seek_to_first().expect("the cursor is positioned");
        assert_eq!(cursor.key(), None, "the data column of {name}");
    }
    // A snapshot from before keeps reading its version (RD1).
    assert_eq!(read(&before.operation()), FULL);

    // CF5: the wipe's key set is every key. Whoever wrote a record
    // before it fails with the conflict, and whoever wrote nothing commits
    // nothing. Entering a subtree writes no record, so the transaction
    // that only did that is after the wipe: its subtree is in use, and
    // empty.
    let error = db
        .commit(&wrote, &mut NoStaleHashes)
        .expect_err("a conflict");
    assert!(matches!(error, WriteError::Commit(CommitError::Conflict)));
    assert!(matches!(
        db.commit(&idle, &mut NoStaleHashes),
        Ok(Committed::Empty)
    ));
    assert!(matches!(
        db.commit(&entered, &mut NoStaleHashes),
        Ok(Committed::Published(_))
    ));
    assert_eq!(
        seen(&db.snapshot().operation(), subtree(8), b"k"),
        Some((0, vec![]))
    );
    drop((wrote, entered, idle));

    // What the wipe left is storage, and it is reclaimed once the readers
    // from before it are gone. The pages that its range deletes unlinked
    // wait for every reader that began before the reclaimer read their
    // record, which the reader of the wipe's own version is.
    drop(operation);
    drop(before);
    drop(after);
    reclaimed(&db, "the wipe is reclaimed", |reclaimed| {
        reclaimed.merks == 4 && reclaimed.waiting == 0
    });
    let reclaimed = db.stats().reclaimed;
    assert_eq!(reclaimed.nested, 1, "the Merk that was hosted");
    assert_eq!(reclaimed.cells, 620);
    assert!(
        reclaimed.pages_freed > 0 && reclaimed.range_deletes > 0,
        "{reclaimed:?}"
    );
    assert_eq!(read(&db.snapshot().operation()), [None; 5]);

    // A second wipe drops the subtree that was entered after the first,
    // and the database is used again: a subtree that is entered again
    // starts empty.
    assert!(matches!(db.wipe(), Ok(Committed::Published(_))));
    let again = Writes {
        enter: vec![(subtree(4), None), (subtree(2), None)],
        ..Writes::default()
    };
    again.commit(&db);
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(seen(&operation, subtree(4), b"k"), Some((0, vec![])));
    assert_eq!(seen(&operation, subtree(2), b"k"), Some((0, vec![])));
    let opened = operation
        .open(subtree(4))
        .expect("a lookup")
        .expect("entered");
    let mut cursor = opened.cursor(Column::Data);
    cursor.seek_to_first().expect("the cursor is positioned");
    assert_eq!(cursor.key(), None);
    drop(opened);
    drop(operation);
    drop(snapshot);

    // The marker beside the database survived all of it (P-03).
    assert_eq!(
        db.foreign_entries().expect("the directory is listed"),
        [".fsck"]
    );
    db.close().expect("the session ends");
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    let snapshot = db.snapshot();
    assert_eq!(seen(&snapshot.operation(), subtree(1), b"k"), None);
    assert_eq!(
        seen(&snapshot.operation(), subtree(4), b"k"),
        Some((0, vec![]))
    );
}

/// CV3 and CF5 for a database with nothing in it: its wipe is a commit
/// with a version and a group, and a transaction that entered a subtree
/// and wrote a record before it fails with the conflict.
#[test]
fn a_wipe_of_a_database_with_no_subtree_is_a_commit() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let wrote = db.begin();
    let scope = wrote.scope().expect("a scope");
    let staged = Writes {
        enter: vec![(subtree(3), None)],
        records: vec![(Column::Aux, subtree(3), b"t".to_vec(), b"mine".to_vec())],
        ..Writes::default()
    };
    staged
        .with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();

    let mut wrote = Some(wrote);
    for wipe in 1..=2u64 {
        let (at, logged) = (db.version(), db.stats().store.log_bytes);
        let Ok(Committed::Published(wiped)) = db.wipe() else {
            panic!("the wipe is a commit");
        };
        assert_eq!(Some(wiped.version), at.next(), "one commit");
        assert!(db.stats().store.log_bytes > logged, "one group");
        // Nothing is entered at the wipe's version: not the subtree the
        // wipe made its commit of, and not the open transaction's.
        let snapshot = db.snapshot();
        assert_eq!(snapshot.version(), wiped.version);
        let operation = snapshot.operation();
        for subtree in [Subtree::new([0; 32]), subtree(3)] {
            assert!(operation.open(subtree).expect("a lookup").is_none());
        }
        drop(operation);
        drop(snapshot);
        // The transaction that wrote before the first wipe meets it.
        if let Some(wrote) = wrote.take() {
            let error = db
                .commit(&wrote, &mut NoStaleHashes)
                .expect_err("a conflict");
            assert!(error.is_conflict(), "{error}");
        }
        // What the wipe wrote to be a commit is taken out again.
        reclaimed(&db, "the wipe's own subtree is reclaimed", |reclaimed| {
            reclaimed.merks == wipe && reclaimed.waiting == 0
        });
    }
    drop(wrote);
    let wiped = db.version();
    db.close().expect("the session ends");
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    assert_eq!(db.version(), wiped);
    assert!(db
        .snapshot()
        .operation()
        .open(subtree(3))
        .expect("a lookup")
        .is_none());
}

/// T-WP1: a power cut around a wipe leaves the database as it was or
/// empty in every column, and it opens.
#[test]
fn a_power_cut_around_a_wipe_leaves_all_or_nothing() {
    let (mut whole, mut empty) = (0, 0);
    for seed in 0..16u64 {
        let fs = SimVfs::new(SimConfig::new(seed));
        let options = Options {
            vfs: Arc::new(fs.clone()),
            ..options()
        };
        let db = Db::open("/db", &options).expect("a new database");
        grove(&db);
        db.flush().expect("the log is synced");
        let grown = db.version();
        assert!(matches!(db.wipe(), Ok(Committed::Published(_))));
        // Half of the seeds make the wipe durable, and let the reclaimer
        // work on what it left before the power goes.
        if seed % 2 == 0 {
            db.flush().expect("the log is synced");
            db.reclaim();
        }
        let crashed = fs.crash(seed * 7 + 1);
        drop(db);

        let options = Options {
            vfs: Arc::new(crashed),
            ..options
        };
        let db = Db::open("/db", &options).expect("the database opens");
        let snapshot = db.snapshot();
        let found = read(&snapshot.operation());
        match db.version() == grown {
            true => {
                // A wipe that a flush made durable is never lost.
                assert!(seed % 2 == 1, "seed {seed}: the wipe was durable");
                assert_eq!(found, FULL, "seed {seed}: as it was");
                whole += 1;
            }
            false => {
                assert_eq!(Some(db.version()), grown.next(), "seed {seed}");
                assert_eq!(found, [None; 5], "seed {seed}: empty");
                empty += 1;
            }
        }
        drop(snapshot);
        // Either way the session goes on, and reclaims what is left.
        let _ = db.wipe().expect("a wipe");
        reclaimed(&db, "what the drops left is reclaimed", |reclaimed| {
            reclaimed.waiting == 0
        });
        assert_eq!(read(&db.snapshot().operation()), [None; 5]);
    }
    assert!(empty >= 8, "{empty} wipes survived, {whole} did not");
}

/// T-WP1: a wipe beside commits and readers. Each reader sees a subtree
/// whole or not at all, and each writer's commit either lands with the
/// subtree it entered or fails with the conflict.
///
/// The threads meet by the states they wait for, and nothing that is
/// asserted is a count the schedule gives. A wipe is made when a subtree
/// was entered since the last one and the reader has looked at a version
/// that has it: so each wipe takes a Merk that a writer planted and that
/// the reader saw whole. In every second round a writer waits for the
/// wipe that takes its subtree before it writes the record, which is then
/// refused. Every other write is made when the schedule has it, beside
/// the wipes.
#[test]
fn a_wipe_beside_writers_and_readers_is_one_commit_to_each() {
    const WRITERS: u32 = 3;
    const ROUNDS: u32 = 40;
    struct Failing<'a>(&'a AtomicBool);
    impl Drop for Failing<'_> {
        fn drop(&mut self) {
            if thread::panicking() {
                self.0.store(true, Ordering::Release);
            }
        }
    }
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let (done, failed) = (AtomicBool::new(false), AtomicBool::new(false));
    // A thread that waits for a state another one is to reach fails when
    // that one has failed, and does not hang.
    let until = |what: &str, reached: &dyn Fn() -> bool| {
        while !reached() {
            let failed = failed.load(Ordering::Acquire);
            assert!(!failed, "a thread failed before {what}");
            thread::sleep(Duration::from_millis(1));
        }
    };
    let subtrees = || (0..WRITERS).flat_map(|writer| (0..ROUNDS).map(move |round| (writer, round)));
    let index = |writer: u32, round: u32| (writer * ROUNDS + round) as usize;
    // The version each subtree was entered at, once its writer knows it,
    // and whether the reader has seen it whole.
    let entered: Vec<AtomicU64> = subtrees().map(|_| AtomicU64::new(0)).collect();
    let whole: Vec<AtomicBool> = subtrees().map(|_| AtomicBool::new(false)).collect();
    // The version of the latest wipe, once what it left was looked at, and
    // the latest version the reader has looked at all subtrees of.
    let (wiped_at, looked_at) = (AtomicU64::new(0), AtomicU64::new(0));
    thread::scope(|threads| {
        let _ending = Ending(&done);
        let _failing = Failing(&failed);
        let mut writers = Vec::new();
        for writer in 0..WRITERS {
            let (db, entered, wiped_at, failed) = (&db, &entered, &wiped_at, &failed);
            writers.push(threads.spawn(move || {
                let _failing = Failing(failed);
                for round in 0..ROUNDS {
                    let subtree = numbered(writer * 1000 + round);
                    // Two commits for each subtree: it is entered with a
                    // Merk, and then gets its record.
                    let planted = Writes {
                        enter: vec![(subtree, None)],
                        merks: vec![(subtree, Merk::sized(b'k', 16, 300))],
                        ..Writes::default()
                    };
                    let at = match planted.with(|batch| db.write(batch, &mut NoStaleHashes)) {
                        Ok(Committed::Published(planted)) => planted.version.get(),
                        // A wipe was published while the write was made:
                        // its key set is every key.
                        Err(WriteError::Commit(CommitError::Conflict)) => continue,
                        other => panic!("a commit or a conflict, and not {other:?}"),
                    };
                    entered[index(writer, round)].store(at, Ordering::Release);
                    // In every second round the record comes after the
                    // wipe that takes the subtree, which is entered once:
                    // it finds none. In the others a wipe may come between
                    // the two commits or while the second is made.
                    let after_its_wipe = round % 2 == 1;
                    if after_its_wipe {
                        until("the wipe of a subtree that was entered", &|| {
                            wiped_at.load(Ordering::Acquire) > at
                        });
                    }
                    let record = Writes {
                        records: vec![(Column::Aux, subtree, b"k".to_vec(), b"whole".to_vec())],
                        ..Writes::default()
                    };
                    match record.with(|batch| db.write(batch, &mut NoStaleHashes)) {
                        Ok(Committed::Published(_)) if !after_its_wipe => {}
                        Err(WriteError::Commit(
                            CommitError::Refused(_) | CommitError::Conflict,
                        )) => {}
                        other => panic!(
                            "a commit or a refusal, after its wipe a refusal, and not {other:?}"
                        ),
                    }
                }
            }));
        }
        let (db, done, whole, looked_at) = (&db, &done, &whole, &looked_at);
        let reader_failed = &failed;
        let reader = threads.spawn(move || {
            let _failing = Failing(reader_failed);
            while !done.load(Ordering::Acquire) {
                let snapshot = db.snapshot();
                let operation = snapshot.operation();
                for (writer, round) in subtrees() {
                    match seen(&operation, numbered(writer * 1000 + round), b"k") {
                        None => {}
                        Some((16, records)) => {
                            assert!(records.len() <= 1);
                            whole[index(writer, round)].store(true, Ordering::Release);
                        }
                        Some(other) => panic!("a subtree in part: {other:?}"),
                    }
                }
                drop(operation);
                looked_at.fetch_max(snapshot.version().get(), Ordering::AcqRel);
            }
        });
        // The version of the last wipe.
        let mut last = 0;
        loop {
            // The writers are asked first: when all have ended, every
            // subtree that was entered is known here.
            let ended = writers.iter().all(|writer| writer.is_finished());
            let planted = subtrees()
                .map(|(writer, round)| (writer, round, &entered[index(writer, round)]))
                .map(|(writer, round, at)| (writer, round, at.load(Ordering::Acquire)))
                .find(|(_, _, at)| *at > last);
            let Some((writer, round, at)) = planted else {
                if ended {
                    break;
                }
                let failed = failed.load(Ordering::Acquire);
                assert!(!failed, "a thread failed before the writers ended");
                thread::sleep(Duration::from_millis(1));
                continue;
            };
            // The subtree was entered after the last wipe, and only this
            // thread wipes: it is there, at every version from the one it
            // was entered at, until the wipe below. So a look of the
            // reader at such a version found it whole.
            let subtree = numbered(writer * 1000 + round);
            until("a look of the reader at the subtree", &|| {
                looked_at.load(Ordering::Acquire) >= at
            });
            let seen_whole = whole[index(writer, round)].load(Ordering::Acquire);
            assert!(seen_whole, "{writer}/{round} was not seen whole");
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            let found = operation.open(subtree).expect("a lookup");
            let id = found.expect("it is entered until the wipe").entry().id();
            drop(operation);
            drop(snapshot);
            let before = db.version();
            let Committed::Published(wiped) = db.wipe().expect("a wipe") else {
                panic!("a wipe is a commit");
            };
            assert!(wiped.version > before);
            let mut dropped = wiped.dropped.iter();
            let took = dropped.any(|dropped| dropped.id == id && dropped.root.is_some());
            assert!(took, "{writer}/{round} was not wiped");
            last = wiped.version.get();
            // No subtree that was entered before the wipe's version is
            // there at a later one: each is entered once, so one that is
            // found outlived the wipe, whatever was committed while the
            // wipe ran and since. The one the wipe was made for is among
            // them.
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            for (writer, round) in subtrees() {
                let at = entered[index(writer, round)].load(Ordering::Acquire);
                if at == 0 || at >= last {
                    continue;
                }
                let found = operation.open(numbered(writer * 1000 + round));
                let found = found.expect("a lookup");
                assert!(found.is_none(), "{writer}/{round} outlived a wipe");
            }
            drop(operation);
            drop(snapshot);
            wiped_at.store(last, Ordering::Release);
        }
        for writer in writers {
            writer.join().expect("a writer ends");
        }
        // The first subtree of all is entered with no wipe before it, and
        // nothing else refuses it: a wipe was made for it.
        assert!(last > 0, "no subtree was entered");
        done.store(true, Ordering::Release);
        reader.join().expect("the reader ends");
    });
    // With the writers gone, one wipe leaves nothing, and all of it is
    // reclaimed.
    let _ = db.wipe().expect("a wipe");
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    for writer in 0..WRITERS {
        for round in 0..ROUNDS {
            assert_eq!(
                seen(&operation, numbered(writer * 1000 + round), b"k"),
                None
            );
        }
    }
    drop(operation);
    drop(snapshot);
    reclaimed(&db, "everything is reclaimed", |reclaimed| {
        reclaimed.waiting == 0
    });
    let stats = db.stats();
    assert_eq!(stats.background.reclaim_errors, 0, "{:?}", stats.background);
    assert_eq!(stats.reclaimed.given_up, 0);
}

/// What a reader finds of the subtree `name`: the tree type it is entered
/// with, its nodes and its records under `k`.
fn entered(operation: &Operation<'_>, name: u8) -> Option<(Vec<u8>, usize, usize)> {
    let opened = operation.open(subtree(name)).expect("a lookup")?;
    let tree_type = opened.entry().tree_type().to_vec();
    let (nodes, records) = seen(operation, subtree(name), b"k").expect("it is entered");
    Some((tree_type, nodes, records.len()))
}

/// D-181 item 5: a wipe enters the subtrees its caller names in its own
/// commit. At the wipe's version they are entered, new and empty, and no
/// other subtree is: one of them was entered before with all it held, one
/// was hosted by a subtree that the wipe drops, and one was never there.
#[test]
fn a_wipe_enters_the_subtrees_its_caller_names_in_its_own_commit() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    grove(&db);
    let before = db.snapshot();
    let open = db.begin();
    let scope = open.scope().expect("a scope");
    let staged = Writes {
        records: vec![(Column::Aux, subtree(5), b"t".to_vec(), b"mine".to_vec())],
        ..Writes::default()
    };
    (staged.with(|batch| scope.apply(batch))).expect("the batch is applied");
    scope.merge();

    let (at, logged) = (db.version(), db.stats().store.log_bytes);
    let enters: [(Subtree, &[u8]); 3] = [
        (subtree(1), b"one"),
        (subtree(3), b""),
        (subtree(9), b"nine"),
    ];
    let Ok(Committed::Published(wiped)) = db.wipe_entering(&enters) else {
        panic!("the wipe is a commit");
    };
    assert_eq!(Some(wiped.version), at.next(), "one commit");
    assert_eq!(db.version(), wiped.version);
    assert!(db.stats().store.log_bytes > logged, "one group");

    let read_all = |operation: &Operation<'_>| -> Vec<Option<(Vec<u8>, usize, usize)>> {
        (1..=9).map(|name| entered(operation, name)).collect()
    };
    let new = |tree_type: &[u8]| Some((tree_type.to_vec(), 0, 0));
    let expected = vec![
        new(b"one"),
        None,
        new(b""),
        None,
        None,
        None,
        None,
        None,
        new(b"nine"),
    ];
    let snapshot = db.snapshot();
    assert_eq!(snapshot.version(), wiped.version);
    assert_eq!(read_all(&snapshot.operation()), expected);
    // The subtree that was hosted has no host now.
    let operation = snapshot.operation();
    let third = operation.open(subtree(3)).expect("a lookup");
    assert!(third.expect("it is entered").entry().host().is_none());
    drop(operation);
    drop(snapshot);
    // A reader from before keeps its version, and a transaction that
    // wrote before the wipe meets it.
    assert_eq!(read(&before.operation()), FULL);
    drop(before);
    let error = (db.commit(&open, &mut NoStaleHashes)).expect_err("a conflict");
    assert!(error.is_conflict(), "{error}");
    drop(open);

    // The subtrees are written as any other, and what the wipe left of
    // the old ones is reclaimed beside them.
    let again = Writes {
        merks: vec![(subtree(1), Merk::sized(b'n', 30, 24))],
        records: vec![(Column::Aux, subtree(9), b"k".to_vec(), vec![9; 40])],
        ..Writes::default()
    };
    again.commit(&db);
    reclaimed(&db, "what the wipe left is reclaimed", |reclaimed| {
        reclaimed.merks == 4 && reclaimed.waiting == 0
    });
    let written = vec![
        Some((b"one".to_vec(), 30, 0)),
        None,
        new(b""),
        None,
        None,
        None,
        None,
        None,
        Some((b"nine".to_vec(), 0, 1)),
    ];
    assert_eq!(read_all(&db.snapshot().operation()), written);
    db.flush().expect("the log is synced");
    let report = {
        halt_background(&db);
        checked(&db)
    };
    assert!(
        report.findings.is_empty() && report.more_findings == 0,
        "{report}"
    );
    drop(db);
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    assert_eq!(read_all(&db.snapshot().operation()), written);
}

/// CV3 and CF5 for a database with nothing in it: with subtrees to enter
/// its wipe is one commit with the key set of a wipe, in which they are
/// entered, and nothing else is made to be dropped.
#[test]
fn a_wipe_of_a_database_with_no_subtree_enters_what_its_caller_names() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    // A transaction that entered a subtree and wrote a record before the
    // wipe: the wipe drops nothing of it, and its key set meets it.
    let wrote = db.begin();
    let scope = wrote.scope().expect("a scope");
    let staged = Writes {
        enter_if_absent: vec![(subtree(3), None)],
        records: vec![(Column::Aux, subtree(3), b"t".to_vec(), b"mine".to_vec())],
        ..Writes::default()
    };
    (staged.with(|batch| scope.apply(batch))).expect("the batch is applied");
    scope.merge();

    let at = db.version();
    let enters: [(Subtree, &[u8]); 2] = [(subtree(0), b"root"), (subtree(3), b"")];
    let Ok(Committed::Published(wiped)) = db.wipe_entering(&enters) else {
        panic!("the wipe is a commit");
    };
    assert_eq!(Some(wiped.version), at.next(), "one commit");
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(entered(&operation, 0), Some((b"root".to_vec(), 0, 0)));
    assert_eq!(entered(&operation, 3), Some((Vec::new(), 0, 0)));
    drop(operation);
    drop(snapshot);
    let error = (db.commit(&wrote, &mut NoStaleHashes)).expect_err("a conflict");
    assert!(error.is_conflict(), "{error}");
    // Nothing was entered to be dropped: there is nothing to reclaim.
    assert_eq!(db.stats().reclaimed.merks, 0);
    drop(wrote);

    // The same wipe again drops the two and enters them anew.
    let Ok(Committed::Published(again)) = db.wipe_entering(&enters) else {
        panic!("the wipe is a commit");
    };
    assert_eq!(Some(again.version), wiped.version.next());
    reclaimed(&db, "the two subtrees are reclaimed", |reclaimed| {
        reclaimed.merks == 2 && reclaimed.waiting == 0
    });
    assert_eq!(
        entered(&db.snapshot().operation(), 0),
        Some((b"root".to_vec(), 0, 0))
    );
}

/// A wipe that is refused has wiped nothing and entered nothing: a list
/// that names a subtree twice, and one with a tree type the directory
/// does not keep. The session goes on.
#[test]
fn a_wipe_whose_enters_are_refused_has_wiped_nothing() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    grove(&db);
    let at = db.version();
    let long = vec![7u8; 256];
    let twice: [(Subtree, &[u8]); 3] = [(subtree(9), b"a"), (subtree(1), b"b"), (subtree(9), b"a")];
    let too_long: [(Subtree, &[u8]); 2] = [(subtree(9), b"a"), (subtree(8), &long)];
    for (case, enters) in [&twice[..], &too_long[..]].into_iter().enumerate() {
        let error = db.wipe_entering(enters).expect_err("the list is refused");
        let refused = matches!(
            &error,
            WriteError::Commit(CommitError::Refused(TxnError::Batch(
                BatchError::SubtreeTwice { .. } | BatchError::TreeTypeTooLong { .. }
            )))
        );
        assert!(refused, "case {case}: {error}");
        assert!(!error.is_conflict() && error.reopen().is_none(), "{error}");
        assert_eq!(db.version(), at, "case {case}: no commit");
        let snapshot = db.snapshot();
        assert_eq!(read(&snapshot.operation()), FULL, "case {case}");
        let operation = snapshot.operation();
        for name in [8, 9] {
            assert!(operation.open(subtree(name)).expect("a lookup").is_none());
        }
    }
    assert!(db.reopen().is_none());
    // The next wipe is made, with its enters.
    let enters: [(Subtree, &[u8]); 1] = [(subtree(9), b"a")];
    let Ok(Committed::Published(wiped)) = db.wipe_entering(&enters) else {
        panic!("the wipe is a commit");
    };
    assert_eq!(Some(wiped.version), at.next());
    assert_eq!(read(&db.snapshot().operation()), [None; 5]);
}

/// T-WP1 for a wipe with enters: a power cut around it leaves the
/// database as it was, with none of them entered, or empty with all of
/// them entered.
#[test]
fn a_power_cut_around_a_wipe_with_enters_leaves_all_or_nothing() {
    let (mut whole, mut empty) = (0, 0);
    for seed in 0..16u64 {
        let fs = SimVfs::new(SimConfig::new(seed));
        let options = Options {
            vfs: Arc::new(fs.clone()),
            ..options()
        };
        let db = Db::open("/db", &options).expect("a new database");
        grove(&db);
        db.flush().expect("the log is synced");
        let grown = db.version();
        let enters: [(Subtree, &[u8]); 2] = [(subtree(2), b"two"), (subtree(9), b"nine")];
        assert!(matches!(
            db.wipe_entering(&enters),
            Ok(Committed::Published(_))
        ));
        if seed % 2 == 0 {
            db.flush().expect("the log is synced");
            db.reclaim();
        }
        let crashed = fs.crash(seed * 7 + 1);
        drop(db);

        let options = Options {
            vfs: Arc::new(crashed),
            ..options
        };
        let db = Db::open("/db", &options).expect("the database opens");
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        let found = read(&operation);
        let (two, nine) = (entered(&operation, 2), entered(&operation, 9));
        match db.version() == grown {
            true => {
                assert!(seed % 2 == 1, "seed {seed}: the wipe was durable");
                assert_eq!(found, FULL, "seed {seed}: as it was");
                assert_eq!(nine, None, "seed {seed}: nothing is entered");
                assert_eq!(two, Some((TREE_TYPE.to_vec(), 300, 3)), "seed {seed}");
                whole += 1;
            }
            false => {
                assert_eq!(Some(db.version()), grown.next(), "seed {seed}");
                let wiped = [None, Some((0, 0)), None, None, None];
                assert_eq!(found, wiped, "seed {seed}: empty");
                assert_eq!(two, Some((b"two".to_vec(), 0, 0)), "seed {seed}");
                assert_eq!(nine, Some((b"nine".to_vec(), 0, 0)), "seed {seed}");
                empty += 1;
            }
        }
    }
    assert!(empty >= 8, "{empty} wipes survived, {whole} did not");
}

/// A wipe whose commit fails at the log: every append of the file layer
/// fails from the wipe on. An append that is undone leaves the log as it
/// was, and the wipe has failed: nothing is wiped and nothing is entered,
/// in this session and in the next. An append that is torn leaves a part
/// of its bytes in the file, or all of them, and the wipe is then made or
/// not. Whichever it is and whatever the wipe returned, a reader finds
/// the database as it was with nothing entered, or empty with what the
/// wipe was given: never one without the other.
#[test]
fn a_wipe_that_fails_at_the_log_leaves_the_database_as_it_was() {
    use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};

    for seed in 0..12u64 {
        let torn = seed % 2 == 1;
        let fs = SimVfs::new(SimConfig::new(seed));
        let options = Options {
            vfs: Arc::new(fs.clone()),
            ..options()
        };
        let db = Db::open("/db", &options).expect("a new database");
        grove(&db);
        db.flush().expect("the log is synced");
        let grown = db.version();
        let appends = OpSet::of(&[OpKind::Append]);
        let rule = FaultRule::new(Fault::Eio).on(appends);
        fs.add_rule(rule.torn_append(torn).forever());
        let enters: [(Subtree, &[u8]); 1] = [(subtree(9), b"nine")];
        let wiped = db.wipe_entering(&enters);
        fs.clear_rules();

        // Whether a reader finds the database as it was.
        let whole = |db: &Db, at: &str| {
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            let found = read(&operation);
            match entered(&operation, 9) {
                None => {
                    assert_eq!(found, FULL, "seed {seed}, {at}: as it was");
                    true
                }
                Some(nine) => {
                    assert_eq!(found, [None; 5], "seed {seed}, {at}: empty");
                    assert_eq!(nine, (b"nine".to_vec(), 0, 0), "seed {seed}, {at}");
                    false
                }
            }
        };
        let published = db.version() != grown;
        assert_eq!(whole(&db, "in this session"), !published, "seed {seed}");
        match &wiped {
            Ok(_) => assert!(published, "seed {seed}"),
            Err(error) => assert_eq!(
                error.published().is_some(),
                published,
                "seed {seed}: {error}"
            ),
        }
        if !torn {
            assert!(wiped.is_err() && !published, "seed {seed}: {wiped:?}");
        }
        drop(db);

        let db = Db::open("/db", &options).expect("the database opens");
        let kept = whole(&db, "in the next session");
        assert!(
            !(published && kept),
            "seed {seed}: a published wipe is lost"
        );
        assert!(
            torn || kept,
            "seed {seed}: a wipe that was taken back is made"
        );
    }
}

/// A wipe is one commit, and ends as a commit does. One of its outcomes
/// is an error after it is published: the log took the group whole, and
/// the sync the commit asked for failed (`CommitError::NotDurable`). The
/// error says that the wipe is published, and it is published whole: a
/// reader of the latest version finds the database empty with what the
/// wipe was given, and a snapshot from before reads what it read.
#[test]
fn a_wipe_whose_sync_fails_is_published_whole_and_says_so() {
    use lignin_io::sim::{Fault, FaultRule, OpSet};
    use lignin_store::wal::SyncPolicy;

    let fs = SimVfs::new(SimConfig::new(31));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        sync: SyncPolicy::EveryCommit,
        ..options()
    };
    let db = Db::open("/db", &options).expect("a new database");
    grove(&db);
    // The background work is at rest, so the sync that fails is the
    // commit's.
    halt_background(&db);
    let grown = db.version();
    let before = db.snapshot();
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
    let enters: [(Subtree, &[u8]); 1] = [(subtree(9), b"nine")];
    let error = db.wipe_entering(&enters).expect_err("the sync failed");
    fs.clear_rules();
    assert!(
        matches!(error, WriteError::Commit(CommitError::NotDurable { .. })),
        "{error}"
    );
    let version = error.published().expect("the wipe is published").version;
    assert_eq!((Some(version), db.version()), (grown.next(), version));
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(read(&operation), [None; 5]);
    assert_eq!(entered(&operation, 9), Some((b"nine".to_vec(), 0, 0)));
    let operation = before.operation();
    assert_eq!(read(&operation), FULL);
    assert_eq!(entered(&operation, 9), None);
}

/// A transaction that only enters a subtree has written no record, so a
/// wipe meets no key of it and is no conflict for it (CF5). Its enter is
/// judged by the directory the wipe leaves, as by that of any commit: a
/// subtree that the wipe entered refuses the enter of a subtree that is
/// new, and an enter if it is absent that states another tree type. An
/// enter if it is absent that states the type the wipe gave is satisfied.
#[test]
fn an_enter_beside_a_wipe_is_judged_by_what_the_wipe_entered() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    grove(&db);
    let entering = |name: u8, new: bool| {
        let (mut writes, enter) = (Writes::default(), (subtree(name), None));
        match new {
            true => writes.enter.push(enter),
            false => writes.enter_if_absent.push(enter),
        }
        let txn = db.begin();
        let scope = txn.scope().expect("a scope");
        (writes.with(|batch| scope.apply(batch))).expect("the batch is applied");
        scope.merge();
        txn
    };
    let refused = |txn: &crate::txn::WriteTxn<'_>| {
        let error = (db.commit(txn, &mut NoStaleHashes)).expect_err("a refusal");
        assert!(!error.is_conflict(), "{error}");
        match error {
            WriteError::Commit(CommitError::Refused(TxnError::Batch(refused))) => refused,
            other => panic!("the directory refuses the enter, and not {other}"),
        }
    };

    // All three are begun, and have entered, before the wipe.
    let (new, other, same) = (entering(9, true), entering(9, false), entering(10, false));
    let enters: [(Subtree, &[u8]); 2] = [(subtree(9), b"nine"), (subtree(10), TREE_TYPE)];
    db.wipe_entering(&enters).expect("a wipe");
    let entered_already = BatchError::Entered {
        subtree: subtree(9),
    };
    assert_eq!(refused(&new), entered_already);
    assert!(matches!(
        refused(&other),
        BatchError::EnteredOtherwise { subtree: found, .. } if found == subtree(9)
    ));
    (db.commit(&same, &mut NoStaleHashes)).expect("the enter finds the subtree of the wipe");
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(entered(&operation, 9), Some((b"nine".to_vec(), 0, 0)));
    assert_eq!(entered(&operation, 10), Some((TREE_TYPE.to_vec(), 0, 0)));
}
