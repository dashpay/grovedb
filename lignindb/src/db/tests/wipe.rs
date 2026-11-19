//! Wipe (P-23, CV3, CF5, T-WP1): one commit that leaves no record in any
//! column, for every reader from that commit on.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};

use lignin_io::sim::{SimConfig, SimVfs};

use super::*;
use crate::{commit::CommitError, db::WriteError, txn::Unit};

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
    // A wipe of a database with no subtree writes nothing.
    assert!(matches!(db.wipe(), Ok(Committed::Empty)));
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
    // a third writes nothing, and the database is used again: a subtree
    // that is entered again starts empty.
    assert!(matches!(db.wipe(), Ok(Committed::Published(_))));
    assert!(matches!(db.wipe(), Ok(Committed::Empty)));
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
#[test]
fn a_wipe_beside_writers_and_readers_is_one_commit_to_each() {
    const WRITERS: u32 = 3;
    const ROUNDS: u32 = 40;
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let done = AtomicBool::new(false);
    thread::scope(|threads| {
        let _ending = Ending(&done);
        let mut writers = Vec::new();
        for writer in 0..WRITERS {
            let db = &db;
            writers.push(threads.spawn(move || {
                for round in 0..ROUNDS {
                    let subtree = numbered(writer * 1000 + round);
                    // Two commits for each subtree: it is entered with a
                    // Merk, and then gets its record. The second one meets
                    // a wipe that came between them.
                    let planted = Writes {
                        enter: vec![(subtree, None)],
                        merks: vec![(subtree, Merk::sized(b'k', 16, 300))],
                        ..Writes::default()
                    };
                    match planted.with(|batch| db.write(batch, &mut NoStaleHashes)) {
                        Ok(Committed::Published(_)) => {}
                        // A wipe was published while the write was made:
                        // its key set is every key.
                        Err(WriteError::Commit(CommitError::Conflict)) => continue,
                        other => panic!("a commit or a conflict, and not {other:?}"),
                    }
                    let record = Writes {
                        records: vec![(Column::Aux, subtree, b"k".to_vec(), b"whole".to_vec())],
                        ..Writes::default()
                    };
                    match record.with(|batch| db.write(batch, &mut NoStaleHashes)) {
                        Ok(Committed::Published(_)) => {}
                        // The subtree was wiped before the record came, or
                        // a wipe was published while the write was made.
                        Err(WriteError::Commit(
                            CommitError::Refused(_) | CommitError::Conflict,
                        )) => {}
                        other => panic!("a commit or a refusal, and not {other:?}"),
                    }
                }
            }));
        }
        let (db, done) = (&db, &done);
        let reader = threads.spawn(move || {
            let mut looks = 0u64;
            while !done.load(Ordering::Acquire) {
                let snapshot = db.snapshot();
                let operation = snapshot.operation();
                for writer in 0..WRITERS {
                    for round in (0..ROUNDS).step_by(7) {
                        match seen(&operation, numbered(writer * 1000 + round), b"k") {
                            None => {}
                            Some((16, records)) => assert!(records.len() <= 1),
                            Some(other) => panic!("a subtree in part: {other:?}"),
                        }
                    }
                }
                looks += 1;
            }
            looks
        });
        // A wipe every few milliseconds for as long as a writer writes,
        // and one when they are done.
        let mut wipes = 0;
        loop {
            let writing = writers.iter().any(|writer| !writer.is_finished());
            let at = db.version();
            match db.wipe().expect("a wipe") {
                Committed::Empty => {}
                Committed::Published(wiped) => {
                    assert!(wiped.version > at);
                    wipes += 1;
                }
            }
            if !writing {
                break;
            }
            thread::sleep(Duration::from_millis(3));
        }
        for writer in writers {
            writer.join().expect("a writer ends");
        }
        assert!(wipes > 0, "no wipe found a subtree");
        done.store(true, Ordering::Release);
        assert!(reader.join().expect("the reader ends") > 0);
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
