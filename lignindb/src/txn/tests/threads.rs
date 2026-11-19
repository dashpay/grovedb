//! Write transactions under threads (`transactions.md` WT1, WT11, RD8,
//! CM4): one transaction that several threads write through its scope, and
//! transactions on several threads that commit past each other while
//! readers check that every version they pin holds whole commits only.

use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
};

use lignin_tree::flat::FlatTree;

use super::{
    model::{self, OwnedBatch, OwnedMerk},
    store::{Db, Outcome},
    *,
};

fn aux() -> Unit {
    model::unit(Column::Aux, 0)
}

#[test]
fn a_transaction_is_written_from_several_threads_through_its_scope() {
    const THREADS: u8 = 4;
    const WRITES: u8 = 40;
    let (db, versions) = Db::new(64);
    let path = db.path(&versions);
    let txn = path.begin();
    let refused = AtomicU64::new(0);
    thread::scope(|threads| {
        for thread in 0..THREADS {
            let (txn, refused) = (&txn, &refused);
            threads.spawn(move || {
                for write in 0..WRITES {
                    let (key, value) = ([thread, write], [write; 20]);
                    let records = [RecordWrite::put(Column::Aux, aux().subtree, &key, &value)];
                    let batch = Batch {
                        records: &records,
                        merks: &[],
                        ..Batch::default()
                    };
                    // Every thread writes the one unit. One scope holds it
                    // at a time, and a scope that finds it held is refused
                    // at once: it never waits (SC3).
                    loop {
                        let scope = txn.scope().expect("any number of scopes");
                        match scope.apply(&batch) {
                            Ok(_) => {
                                let mut out = Vec::new();
                                assert!(scope.get(aux(), &key, &mut out).expect("a read"));
                                scope.merge();
                                break;
                            }
                            Err(TxnError::ScopeOverlap { unit }) => {
                                assert_eq!(unit, aux());
                                refused.fetch_add(1, Ordering::Relaxed);
                                drop(scope);
                                thread::yield_now();
                            }
                            Err(error) => panic!("{error}"),
                        }
                    }
                    // A read outside a scope is refused while a scope
                    // holds the unit, and otherwise sees what was merged.
                    let mut out = Vec::new();
                    match txn.get(aux(), &key, &mut out) {
                        Ok(found) => assert!(found && out == value),
                        Err(TxnError::ScopeOverlap { .. }) => {}
                        Err(error) => panic!("{error}"),
                    }
                }
            });
        }
        // A scope is opened on one thread and used on another.
        let scope = txn.scope().expect("any number of scopes");
        threads.spawn(move || {
            let records = [RecordWrite::put(Column::Aux, aux().subtree, b"moved", b"1")];
            let batch = Batch {
                records: &records,
                merks: &[],
                ..Batch::default()
            };
            while let Err(error) = scope.apply(&batch) {
                assert!(matches!(error, TxnError::ScopeOverlap { .. }), "{error}");
                thread::yield_now();
            }
            scope.merge();
        });
    });
    assert_eq!(
        txn.stats().writes,
        usize::from(THREADS) * usize::from(WRITES) + 1
    );
    let records = scan(&mut txn.cursor(aux()), false);
    assert_eq!(records.len(), txn.stats().writes);
    assert!(records.is_sorted());
    // Nobody else saw any of it, and everybody sees all of it once it is
    // committed.
    assert!(scan(&mut path.begin().cursor(aux()), false).is_empty());
    let outcome = db.commit(&versions, &txn).expect("a commit");
    assert!(matches!(outcome, Outcome::Committed(_)));
    assert_eq!(scan(&mut path.begin().cursor(aux()), false), records);
    assert_eq!(db.private_ids(), 0);
}

/// The value of the flat record `key` of `unit` at the version `at`.
fn committed_value(
    db: &Db,
    versions: &Versions,
    tree: FlatTree,
    at: CommitVersion,
    unit: Unit,
    key: &[u8],
) -> Option<Vec<u8>> {
    let mut stored = unit.subtree.as_bytes().to_vec();
    stored.extend_from_slice(key);
    let mut out = Vec::new();
    let guard = versions.guard();
    let found = tree
        .get(&db.committed(&guard, at), &stored, &mut out)
        .expect("a read");
    found.then_some(out)
}

#[test]
fn transactions_on_several_threads_commit_past_each_other() {
    const WRITERS: u8 = 4;
    const ROUNDS: u32 = if cfg!(miri) { 4 } else { 60 };
    let (db, versions) = Db::new(96);
    let path = db.path(&versions);
    let done = AtomicBool::new(false);
    let rebuilds = AtomicU64::new(0);
    let checked = AtomicU64::new(0);
    thread::scope(|threads| {
        let (db, versions, done) = (&*db, &versions, &done);
        let (rebuilds, checked) = (&rebuilds, &checked);
        let writers: Vec<_> = (0..WRITERS)
            .map(|writer| {
                threads.spawn(move || {
                    // Each writer has a Merk and a counter of its own. A
                    // commit adds one node to the Merk and one to the
                    // counter, so the two agree at every version.
                    let name = 10 + writer;
                    let unit = Unit::data(model::subtree(name));
                    let mut view = BTreeMap::new();
                    for round in 1..=ROUNDS {
                        let txn = path.begin();
                        let scope = txn.scope().expect("its own transaction");
                        let mut items = model::Committed::items(&view);
                        items.insert(round.to_be_bytes().to_vec(), vec![writer; 100]);
                        let merk = OwnedMerk::change(model::subtree(name), &view, &items);
                        let mut records = vec![put(aux(), &[writer], &round.to_be_bytes())];
                        records.extend(World::root_record(&merk));
                        let batch = OwnedBatch {
                            merks: vec![merk],
                            records,
                            ..OwnedBatch::default()
                        };
                        // The Merk first and the counter second, in two
                        // operations, with other commits in between.
                        let (first, second) = (
                            OwnedBatch {
                                merks: batch.merks.clone(),
                                records: batch.records[1..].to_vec(),
                                ..OwnedBatch::default()
                            },
                            OwnedBatch {
                                merks: Vec::new(),
                                records: batch.records[..1].to_vec(),
                                ..OwnedBatch::default()
                            },
                        );
                        first.with(|batch| scope.apply(batch)).expect("a write");
                        thread::yield_now();
                        // It reads its own counter as committed, and the
                        // others' as they are committed now.
                        let mut out = Vec::new();
                        let found = scope.get(aux(), &[writer], &mut out).expect("a read");
                        assert_eq!(
                            found.then_some(out),
                            (round > 1).then(|| (round - 1).to_be_bytes().to_vec())
                        );
                        second.with(|batch| scope.apply(batch)).expect("a write");
                        scope.merge();
                        rebuilds.fetch_add(txn.stats().rebuilds, Ordering::Relaxed);
                        // No other writer writes these keys: no conflict.
                        let outcome = db.commit(versions, &txn).expect("a commit");
                        assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
                        for write in batch.writes() {
                            if let Write::Put(of, key, value) = write
                                && of == unit
                            {
                                view.insert(key, value);
                            }
                        }
                    }
                })
            })
            .collect();
        for _ in 0..2 {
            threads.spawn(move || {
                let reads = db.reads(versions);
                let aux_tree = db.keyspaces().aux;
                while !done.load(Ordering::Relaxed) {
                    // At a pinned version every writer's Merk has as many
                    // nodes as its counter says: no part of a commit is
                    // seen without the rest, and nothing uncommitted.
                    let snapshot = reads.snapshot();
                    let at = snapshot.version();
                    let keys = db.check_trees(versions, at);
                    for writer in 0..WRITERS {
                        let counter = committed_value(db, versions, aux_tree, at, aux(), &[writer]);
                        let rounds = counter.map_or(0, |bytes| {
                            u32::from_be_bytes(bytes.try_into().expect("four bytes"))
                        });
                        let nodes = keys.get(&model::subtree(10 + writer)).map_or(0, Vec::len);
                        assert_eq!(nodes, rounds as usize, "writer {writer} at {at}");
                    }
                    checked.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        for writer in writers {
            if writer.join().is_err() {
                done.store(true, Ordering::Relaxed);
                panic!("a writer failed");
            }
        }
        done.store(true, Ordering::Relaxed);
    });
    let commits = db.counts.commits.load(Ordering::Relaxed);
    assert_eq!(commits, u64::from(WRITERS) * u64::from(ROUNDS));
    assert_eq!(db.counts.conflicts.load(Ordering::Relaxed), 0);
    assert!(checked.load(Ordering::Relaxed) > 0);
    assert!(
        rebuilds.load(Ordering::Relaxed) > 0 || cfg!(miri),
        "no transaction was rebased"
    );
    let keys = db.check_trees(&versions, versions.latest());
    for writer in 0..WRITERS {
        assert_eq!(keys[&model::subtree(10 + writer)].len(), ROUNDS as usize);
    }
    assert_eq!((db.private_ids(), db.pool.census().private), (0, 0));
    assert_eq!(db.commits.kept(), 1, "nobody watches any more");
}
