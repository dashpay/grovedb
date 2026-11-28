//! How a commit ends other than by publishing (`transactions.md` CM1, CM5,
//! CM6), what each ending leaves of the transaction and of the session,
//! and what the commit lock keeps out.

use std::{
    error::Error,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use lignin_format::{tree_page::HashField, wal::GroupWriter, FileId, PageId};
use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};
use lignin_store::{
    alloc::PageState,
    flush::FlushConfig,
    segment::BlockLocation,
    table::Link,
    wal::{LogError, LogOptions, SyncPolicy},
};
use lignin_tree::hash::Stale;

use super::{aux, groups, records, sim, store_on, store_with, version};
use crate::{
    commit::{CommitError, Committed, Failure, NoStaleHashes, Stopped},
    txn::{
        tests::{
            model::{self, OwnedBatch, Write},
            put,
            store::{Db, Outcome, SIZE},
            Tx, World,
        },
        BatchError, TxnError,
    },
};

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

/// CM1, T-SC4: a commit and a rollback with a scope open are refused, and
/// the scope and the transaction are as they were.
#[test]
fn a_commit_with_a_scope_open_is_refused_and_changes_nothing() {
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let txn = committer.path().begin();
    let scope = txn.scope().expect("a scope");
    let batch = records(vec![put(aux(1), b"k", b"written through the scope")]);
    batch.with(|batch| scope.apply(batch)).expect("a write");

    let refused = committer
        .commit(&txn, &mut NoStaleHashes)
        .expect_err("a scope is open");
    assert!(matches!(refused, CommitError::ScopesOpen), "{refused}");
    assert!(refused.leaves_transaction_open());
    assert!(matches!(committer.rollback(&txn), Err(TxnError::ScopeOpen)));
    assert_eq!(committer.stats().rollbacks, 0);

    // The scope goes on, and what it wrote is committed once it has merged.
    let more = records(vec![put(aux(1), b"k2", b"and more")]);
    more.with(|batch| scope.apply(batch)).expect("a write");
    scope.merge();
    let published = committer
        .commit(&txn, &mut NoStaleHashes)
        .expect("no scope is open");
    assert_eq!(published.version(), Some(versions.latest()));
    let mut out = Vec::new();
    assert!(txn.get(aux(1), b"k2", &mut out).expect("a read"));
    assert_eq!(out, b"and more");

    // An empty transaction with a scope open is refused as well: the
    // precondition comes first (CM1, then CM3 step 1).
    let scope = txn.scope().expect("a scope");
    let refused = committer.commit(&txn, &mut NoStaleHashes);
    assert!(matches!(refused, Err(CommitError::ScopesOpen)));
    scope.rollback();
    assert!(matches!(
        committer.commit(&txn, &mut NoStaleHashes),
        Ok(Committed::Empty)
    ));
    assert_eq!(committer.stats().empty, 1);
}

/// CM1, SC9: a transaction is not committed while a pending mark stands.
/// It is as it was, the mark and its savepoints included, and it commits
/// once the mark is taken off.
#[test]
fn a_commit_with_a_pending_mark_is_refused_and_changes_nothing() {
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let txn = committer.path().begin();
    let subtree = model::subtree(1);
    let scope = txn.scope().expect("a scope");
    let batch = records(vec![put(aux(1), b"k", b"v")]);
    batch.with(|batch| scope.apply(batch)).expect("a write");
    scope.mark_pending(subtree, b"host").expect("a mark");
    scope.merge();
    txn.set_savepoint().expect("a savepoint");
    let before = (versions.latest(), db.log.end());

    let refused = committer
        .commit(&txn, &mut NoStaleHashes)
        .expect_err("a mark stands");
    assert!(matches!(refused, CommitError::NotFinalized), "{refused}");
    assert!(refused.leaves_transaction_open() && refused.source().is_none());
    assert_eq!(
        refused.to_string(),
        "the transaction has a pending mark left"
    );
    assert_eq!((txn.stats().writes, txn.savepoints()), (1, 1), "as it was");
    assert!(txn.is_pending(subtree, b"host"));
    assert_eq!((versions.latest(), db.log.end()), before);

    // The mark is taken off, and the commit is made.
    let scope = txn.scope().expect("a scope");
    scope.clear_pending(subtree, b"host").expect("the mark");
    scope.merge();
    let published = committer
        .commit(&txn, &mut NoStaleHashes)
        .expect("no mark is left");
    assert_eq!(published.version(), before.0.next());
    assert_eq!(txn.savepoints(), 0);

    // A mark with no write beside it keeps the transaction from
    // committing as well, and a rollback takes it along.
    let scope = txn.scope().expect("a scope");
    scope.mark_pending(subtree, b"host").expect("a mark");
    scope.merge();
    let refused = committer.commit(&txn, &mut NoStaleHashes);
    assert!(
        matches!(refused, Err(CommitError::NotFinalized)),
        "{refused:?}"
    );
    committer.rollback(&txn).expect("no scope is open");
    assert!(!txn.is_pending(subtree, b"host"));
    assert!(matches!(
        committer.commit(&txn, &mut NoStaleHashes),
        Ok(Committed::Empty)
    ));
    let stats = committer.stats();
    assert_eq!((stats.failed, stats.conflicts, stats.published), (0, 0, 1));
}

/// A commit that cannot take the transaction leaves it as it was, its
/// savepoints included, and a commit that takes it removes them whatever
/// comes of it (TXN-04's rule 3).
#[test]
fn a_refused_commit_leaves_the_transaction_with_its_savepoints() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let committer = db.committer(&versions);
    // The first node of a Merk that has none, with no record of its root
    // beside it: two transactions that each make one write no key in
    // common.
    fn first_node<'s>(world: &World<'s>, tx: &Tx<'s>, name: &[u8]) {
        let merks = vec![world.merk(tx, 5, |items| {
            items.insert(name.to_vec(), b"a first node".to_vec());
        })];
        let batch = OwnedBatch {
            merks,
            ..OwnedBatch::default()
        };
        world.apply(tx, &batch).expect("a first node");
    }

    // The transaction writes, sets a savepoint, and makes its node.
    let second = world.begin();
    world.write(&second, vec![put(aux(1), b"mine", b"1")]);
    second.txn.set_savepoint().expect("no scope is open");
    first_node(&world, &second, b"two");
    // Another one makes the root of the Merk and commits: nothing of the
    // transaction is met, and its writes cannot be laid out any more.
    let first = world.begin();
    first_node(&world, &first, b"one");
    assert!(committed(world.commit(first, &[&second])));

    let before = versions.latest();
    let held = second.txn.stats().writes;
    let refused = committer
        .commit(&second.txn, &mut NoStaleHashes)
        .expect_err("two roots");
    let CommitError::Refused(TxnError::Batch(BatchError::RootTaken { subtree })) = &refused else {
        panic!("{refused}");
    };
    assert_eq!(*subtree, model::subtree(5));
    assert!(refused.leaves_transaction_open() && refused.source().is_some());
    assert_eq!(versions.latest(), before);
    assert_eq!(
        (second.txn.stats().writes, second.txn.savepoints()),
        (held, 1),
        "as it was"
    );
    // It can still go back to its savepoint, and be rolled back.
    second
        .txn
        .rollback_to_savepoint()
        .expect("the savepoint is there");
    assert_eq!(second.txn.stats().writes, 1);
    committer.rollback(&second.txn).expect("no scope is open");
    assert_eq!((second.txn.stats().writes, second.txn.savepoints()), (0, 0));

    // A commit that takes the transaction removes its savepoints: one
    // that publishes, one that conflicts, and one that has nothing to
    // commit.
    let tx = world.begin();
    tx.txn.set_savepoint().expect("a savepoint");
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(2), b"k", b"1")]);
    tx.txn.set_savepoint().expect("a savepoint");
    let again = Tx {
        txn: committer.path().begin(),
        model: Default::default(),
    };
    world.write(&again, vec![put(aux(2), b"k", b"2")]);
    again.txn.set_savepoint().expect("a savepoint");
    assert!(matches!(
        committer.commit(&tx.txn, &mut NoStaleHashes),
        Ok(Committed::Published(_))
    ));
    assert_eq!(tx.txn.savepoints(), 0);
    let conflict = committer.commit(&again.txn, &mut NoStaleHashes);
    assert!(matches!(conflict, Err(CommitError::Conflict)));
    assert_eq!(again.txn.savepoints(), 0);
    assert_eq!(db.private_ids(), 0);
}

/// A transaction is committed where it was begun.
#[test]
fn a_transaction_of_another_database_is_refused_and_left_alone() {
    let (db, versions) = Db::new(64);
    let (other, other_versions) = Db::new(64);
    let txn = other.path(&other_versions).begin();
    let scope = txn.scope().expect("a scope");
    let batch = records(vec![put(aux(1), b"k", b"of the other store")]);
    batch.with(|batch| scope.apply(batch)).expect("a write");
    scope.merge();

    let refused = db.committer(&versions).commit(&txn, &mut NoStaleHashes);
    assert!(matches!(
        refused,
        Err(CommitError::Refused(TxnError::Damaged(_)))
    ));
    assert_eq!(txn.stats().writes, 1);
    assert_eq!(versions.latest(), version(1));
    let published = other
        .committer(&other_versions)
        .commit(&txn, &mut NoStaleHashes);
    assert_eq!(
        published.expect("its own store takes it").version(),
        Some(version(2))
    );
}

/// CM3 step 5: a stale hash that no rule makes fails the commit before
/// anything is appended. The transaction is ended and its copies are gone.
#[test]
fn a_hash_that_cannot_be_made_fails_the_commit_and_appends_nothing() {
    let (db, versions) = Db::new(64);
    let world = World::new(&db, &versions);
    let committer = db.committer(&versions);
    let tx = world.begin();
    let mut merk = world.merk(&tx, 3, |items| {
        items.insert(b"node".to_vec(), b"a value".to_vec());
    });
    let stale = Stale::of(HashField::Value).with(HashField::KeyValue);
    merk.stale = [(b"node".to_vec(), stale)].into();
    let batch = OwnedBatch {
        records: World::root_record(&merk).into_iter().collect(),
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("a node with stale hashes");
    assert_eq!(tx.txn.stats().stale_nodes, 1);
    let (logged, end) = (groups(&db).len(), db.log.end());

    let failed = committer
        .commit(&tx.txn, &mut NoStaleHashes)
        .expect_err("no rule");
    let CommitError::Failed(failure @ Failure::Hash(_)) = &failed else {
        panic!("{failed}");
    };
    assert!(failure
        .to_string()
        .starts_with("a stale hash could not be made"));
    assert!(failure.source().is_some());
    assert!(!failed.leaves_transaction_open() && failed.source().is_some());
    assert_eq!(tx.txn.stats().writes, 0, "the commit ended the transaction");
    assert_eq!((groups(&db).len(), db.log.end()), (logged, end));
    assert_eq!(versions.latest(), version(1));
    assert_eq!((db.pool.census().private, db.private_ids()), (0, 0));
    assert_eq!(committer.stats().failed, 1);
    world.check_committed();
    // The session goes on.
    world.write(&tx, vec![put(aux(3), b"k", b"v")]);
    tx.forget();
    let published = committer
        .commit(&tx.txn, &mut NoStaleHashes)
        .expect("a commit");
    assert_eq!(published.version(), Some(version(2)));
}

/// CM7, CM5: the sync that the sync policy asks of a commit fails. The
/// group is in the log, so the commit has happened: it is published, and
/// reported as not known to be durable.
#[test]
fn a_commit_whose_sync_fails_is_published_and_not_durable() {
    let fs = sim(21);
    let options = LogOptions {
        sync: SyncPolicy::EveryCommit,
        ..LogOptions::default()
    };
    let (db, versions) = store_on(&fs, options);
    let mut world = World::new(&db, &versions);
    let committer = db.committer(&versions);
    // A commit under the policy is durable when it returns.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"first", b"durable")]);
    let published = (db.commit_as(&committer, &tx.txn, None))
        .expect("the sync succeeds")
        .expect("a version");
    world.model.commit(&[put(aux(1), b"first", b"durable")]);
    let durable = published.durable.expect("the policy asked for a sync");
    assert_eq!(durable.version, published.version);
    assert_eq!(durable.place, Some(published.end));
    assert_eq!(db.log.durable().version, published.version);

    // From here on every sync of a log file fails.
    let log_file = format!("{}", published.at.file());
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"second", b"published all the same")]);
    let ended = db
        .commit_as(&committer, &tx.txn, None)
        .expect_err("the sync fails");
    let CommitError::NotDurable { published, .. } = &ended else {
        panic!("{ended} (log file {log_file})");
    };
    world
        .model
        .commit(&[put(aux(1), b"second", b"published all the same")]);
    assert_eq!(published.version, versions.latest());
    assert_eq!(published.durable, None);
    assert_eq!(
        ended.published().map(|commit| commit.version),
        Some(versions.latest())
    );
    assert!(!ended.leaves_transaction_open() && ended.source().is_some());
    assert!(ended.to_string().contains("not known to be durable"));
    assert_eq!(tx.txn.stats().writes, 0);
    // Every handle reads it, and a recovery finds its group in the log.
    world.check_committed();
    assert_eq!(
        groups(&db).last().map(|group| group.version),
        Some(versions.latest().get())
    );
    assert!(db.log.durable().version < versions.latest());
    let stats = committer.stats();
    assert_eq!((stats.published, stats.not_durable), (2, 1));
    fs.clear_rules();
}

/// What no rule lets happen: a part of the store refuses the install of a
/// commit whose group is in the log. Nothing is published, the session
/// takes no further commit, and its readers read what they read.
#[test]
fn a_commit_that_cannot_be_installed_halts_the_session() {
    let (db, versions) = Db::new(64);
    // The page table of this store is damaged on purpose.
    db.unchecked();
    let mut world = World::new(&db, &versions);
    let committer = db.committer(&versions);
    // Enough flat records for several leaves, so that a range delete
    // frees pages.
    let unit = aux(4);
    let many: Vec<Write> = (0..400u16)
        .map(|index| put(unit, &index.to_be_bytes(), &[index as u8; 60]))
        .collect();
    assert!(committed(world.write_outside(&records(many), &[])));
    let before = versions.latest();

    // The page table is given a link that no commit and no write-back
    // made, for every page: a version far ahead, so that the free link of
    // the commit is out of order.
    let nowhere = BlockLocation {
        file: FileId::MIN,
        offset: 64,
        len: 64,
    };
    let (mut from, mut ids) = (Some(PageId::MIN), Vec::new());
    while let Some(start) = from {
        ids.clear();
        from = db.ids.scan(start, &mut ids);
        for (page, state) in &ids {
            if *state == PageState::Live {
                (db.table
                    .push(*page, Link::base(version(1_000_000), nowhere)))
                .expect("a link");
            }
        }
    }

    // The transaction frees those pages, and makes pages that it empties
    // again: it holds copies of pages to free and ids it has no use for.
    let tx = world.begin();
    let passing: Vec<Write> = (0..400u16)
        .map(|index| put(aux(6), &index.to_be_bytes(), &[7; 60]))
        .collect();
    world.write(&tx, passing);
    world.write(&tx, vec![Write::Range(aux(6), Vec::new(), None)]);
    world.write(&tx, vec![Write::Range(unit, Vec::new(), None)]);
    assert!(tx.txn.stats().flat_pages > 2);
    assert!(db.private_ids() > 0 && db.pool.census().private > 2);
    let halted = committer
        .commit(&tx.txn, &mut NoStaleHashes)
        .expect_err("a refused link");
    let CommitError::Halted {
        version: at,
        source,
    } = &halted
    else {
        panic!("{halted}");
    };
    assert_eq!(*at, before.next().expect("a version"));
    assert!(matches!(source, Failure::Table(_)), "{source}");
    assert!(!halted.leaves_transaction_open() && halted.source().is_some());
    // What the install had not come to is given up, as by a rollback: no
    // copy stays private and no id stays with the transaction.
    assert_eq!((db.pool.census().private, db.private_ids()), (0, 0));
    // Nothing is published, and the group is in the log: the next open
    // has the commit.
    assert_eq!(versions.latest(), before);
    assert_eq!(
        groups(&db).last().map(|group| group.version),
        Some(at.get())
    );
    tx.forget();
    world.check_committed();
    let stopped = committer.state().stopped().expect("the session is stopped");
    assert!(matches!(&stopped, Stopped::Halted { version, .. } if version == at));
    assert_eq!(stopped.version(), *at);

    // No further commit is taken. A transaction is left as it was, and
    // one with nothing to commit still ends.
    let next = world.begin();
    world.write(&next, vec![put(aux(5), b"k", b"v")]);
    let refused = committer
        .commit(&next.txn, &mut NoStaleHashes)
        .expect_err("stopped");
    assert!(
        matches!(&refused, CommitError::Stopped(Stopped::Halted { .. })),
        "{refused}"
    );
    assert!(refused.leaves_transaction_open());
    assert_eq!(next.txn.stats().writes, 1);
    committer.rollback(&next.txn).expect("a rollback");
    let empty = committer.commit(&world.begin().txn, &mut NoStaleHashes);
    assert!(matches!(empty, Ok(Committed::Empty)));
    assert_eq!(versions.latest(), before);
}

/// RD7: a write with no transaction whose batch is refused writes nothing.
/// Its transaction goes with whatever it held, the log is not appended to,
/// no commit is counted as tried, and the next write is made.
#[test]
fn a_write_with_no_transaction_whose_batch_is_refused_writes_nothing() {
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let (before, logged) = (versions.latest(), db.log.end());

    // One batch names a key twice.
    let twice = records(vec![
        put(aux(1), b"k", b"first"),
        put(aux(1), b"k", b"second"),
    ]);
    let refused = twice.with(|batch| committer.write(batch, &mut NoStaleHashes));
    let Err(CommitError::Refused(TxnError::Batch(error))) = refused else {
        panic!("{refused:?}");
    };
    assert!(matches!(error, BatchError::KeyTwice { .. }), "{error}");
    assert_eq!((versions.latest(), db.log.end()), (before, logged));
    assert_eq!((db.pool.census().private, db.private_ids()), (0, 0));
    let stats = committer.stats();
    assert_eq!((stats.failed, stats.published, stats.empty), (0, 0, 0));

    let batch = records(vec![put(aux(1), b"k", b"written")]);
    let made = batch.with(|batch| committer.write(batch, &mut NoStaleHashes));
    assert_eq!(made.expect("a commit").version(), before.next());
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
}

/// A group is in the log that the session never published, as after a
/// commit that panicked between its append and its publication. No commit
/// can be made over it: the one that finds it has failed, with nothing
/// appended, and the session takes no further commit.
#[test]
fn a_group_that_was_never_published_stops_the_session() {
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let before = versions.latest();
    let begin = |key: &[u8]| {
        let txn = committer.path().begin();
        let scope = txn.scope().expect("a scope");
        let batch = records(vec![put(aux(1), key, b"v")]);
        batch.with(|batch| scope.apply(batch)).expect("a write");
        scope.merge();
        txn
    };

    // A group of the next version, appended and not published.
    let at = db.log.next_group().expect("a place");
    let mut bytes = Vec::new();
    let group = GroupWriter::new(at.version(), at.address(), &mut bytes).expect("a group");
    group.finish();
    db.log.append_group(&at, &bytes).expect("an append");
    let end = db.log.end();

    let first = begin(b"a");
    let failed = committer.commit(&first, &mut NoStaleHashes);
    let Err(CommitError::Failed(Failure::VersionGap { log, latest })) = &failed else {
        panic!("{failed:?}");
    };
    assert_eq!((*latest, Some(*log)), (before, at.version().next()));
    assert_eq!(first.stats().writes, 0, "the commit ended the transaction");
    assert_eq!((versions.latest(), db.log.end()), (before, end));
    assert_eq!((db.pool.census().private, db.private_ids()), (0, 0));
    assert_eq!(committer.stats().failed, 1);
    let stopped = committer.state().stopped().expect("the session is stopped");
    assert!(
        matches!(&stopped, Stopped::Halted { version, reason }
            if Some(*version) == before.next() && reason.contains("the log would give")),
        "{stopped}"
    );

    // The next commit is refused with its transaction whole.
    let second = begin(b"b");
    let refused = committer.commit(&second, &mut NoStaleHashes);
    assert!(
        matches!(refused, Err(CommitError::Stopped(_))),
        "{refused:?}"
    );
    assert_eq!(second.stats().writes, 1);
    assert_eq!((versions.latest(), db.log.end()), (before, end));
    assert_eq!(committer.stats().failed, 1);
}

/// CM9 (d): the log cannot say whether an item that write-back appended is
/// in it, and takes nothing more. A commit that finds it so stops the
/// session, and its transaction is as it was.
#[test]
fn a_log_that_write_back_left_unsettled_stops_the_session() {
    let fs = sim(23);
    let (db, versions) = store_on(&fs, LogOptions::default());
    let committer = db.committer(&versions);
    let write = |key: &[u8]| {
        let batch = records(vec![put(aux(1), key, b"v")]);
        batch.with(|batch| committer.write(batch, &mut NoStaleHashes))
    };
    write(b"k").expect("a commit");
    let before = versions.latest();

    // A mark, as a checkpoint appends one, is taken back, and the sync
    // that would settle it fails.
    let taken_back = FaultRule::new(Fault::Eio)
        .on(OpKind::Append)
        .torn_append(false);
    fs.add_rule(taken_back);
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
    let mark = db.log.append_mark(before);
    assert!(matches!(mark, Err(LogError::TakenBack { .. })), "{mark:?}");
    let settled = db.log.settle();
    assert!(
        matches!(settled, Err(LogError::Unsettled { .. })),
        "{settled:?}"
    );
    fs.clear_rules();
    let end = db.log.end();

    let txn = committer.path().begin();
    let scope = txn.scope().expect("a scope");
    let batch = records(vec![put(aux(1), b"a", b"v")]);
    batch.with(|batch| scope.apply(batch)).expect("a write");
    scope.merge();
    let refused = committer.commit(&txn, &mut NoStaleHashes);
    let Err(CommitError::Stopped(stopped @ Stopped::Unsettled { version: at, .. })) = &refused
    else {
        panic!("{refused:?}");
    };
    assert_eq!(Some(*at), before.next());
    assert_eq!(txn.stats().writes, 1, "the transaction is as it was");
    assert_eq!((versions.latest(), db.log.end()), (before, end));
    assert_eq!(committer.state().stopped().as_ref(), Some(stopped));
    let stats = committer.stats();
    assert_eq!((stats.failed, stats.published), (0, 1));

    // No further commit is taken, and what was committed is read.
    assert!(matches!(write(b"b"), Err(CommitError::Stopped(_))));
    let mut out = Vec::new();
    assert!(txn.committed_get(aux(1), b"k", &mut out).expect("a read"));
    assert_eq!(versions.latest(), before);
}

/// An error of write-back in a turn that a committer makes after its
/// commit changes nothing of the commit: it is counted and told to
/// whoever the commits of the store report to.
#[test]
fn an_error_of_write_back_after_a_commit_is_told_and_changes_no_commit() {
    let fs = sim(21);
    let options = LogOptions {
        sync: SyncPolicy::Never,
        ..LogOptions::default()
    };
    // Bounds that a few commits pass, and no loop that makes the turns:
    // the committer that finds write-back over them makes one itself.
    let flush = FlushConfig {
        max_unwritten: 4,
        max_log_held: 8 << 10,
        checkpoint_log_bytes: 4 << 10,
        idle: Duration::from_millis(1),
        ..FlushConfig::new(SIZE)
    };
    let (db, versions) = store_with(&fs, 64, options, flush);
    let committer = db.committer(&versions);
    let write = |round: u16| {
        let writes = (0..40u16)
            .map(|index| {
                put(
                    aux(3),
                    &(round * 100 + index).to_be_bytes(),
                    &[round as u8; 300],
                )
            })
            .collect();
        let made = records(writes).with(|batch| committer.write(batch, &mut NoStaleHashes));
        made.expect("the commit is made").version()
    };
    let told = || db.flush_errors.lock().expect("the errors").len();
    assert_eq!(write(0), Some(version(2)));
    assert_eq!((told(), committer.stats().write_back_errors), (0, 0));

    // Every sync fails from here on. The commit makes none, since the
    // policy asks for none. The turn of write-back does.
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
    let mut round = 1;
    while told() == 0 && round < 40 {
        let before = versions.latest();
        assert_eq!(write(round), before.next(), "round {round}");
        round += 1;
    }
    assert!(told() > 0, "write-back made no turn in {round} commits");
    let stats = committer.stats();
    assert_eq!(stats.write_back_errors as usize, told());
    assert_eq!((stats.failed, stats.not_durable), (0, 0));
    assert_eq!(stats.published, u64::from(round));
    assert!(committer.state().stopped().is_none());
    fs.clear_rules();

    // Every commit is there to read.
    let reader = committer.path().begin();
    let mut out = Vec::new();
    for round in 0..round {
        let key = (round * 100 + 39).to_be_bytes();
        out.clear();
        assert!(reader.get(aux(3), &key, &mut out).expect("a read"));
        assert_eq!(out, [round as u8; 300]);
    }
}

/// CM7, DEC-04: under a policy that syncs at an interval, a commit syncs
/// the log when a sync is due and says how far the log is durable, and
/// makes none when none is due.
#[test]
fn a_commit_syncs_when_the_interval_policy_has_a_sync_due() {
    for (interval, due) in [(Duration::ZERO, true), (Duration::from_secs(3600), false)] {
        let fs = sim(22);
        let options = LogOptions {
            sync: SyncPolicy::Interval(interval),
            ..LogOptions::default()
        };
        let (db, versions) = store_on(&fs, options);
        let committer = db.committer(&versions);
        let batch = records(vec![put(aux(1), b"k", b"v")]);
        let committed = batch.with(|batch| committer.write(batch, &mut NoStaleHashes));
        let committed = committed.expect("a commit");
        let published = committed.published().expect("a version");
        match &published.durable {
            Some(durable) => {
                assert!(due && durable.version == published.version, "{durable:?}");
                assert!(db.log.durable().version >= published.version);
            }
            None => assert!(!due),
        }
        assert_eq!(committer.stats().not_durable, 0);
    }
}

/// CM2, CK2: while the commit lock is held, no commit is published. The
/// sampling step of a checkpoint takes it for that.
#[test]
fn nothing_is_published_while_the_commit_lock_is_held() {
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let done = AtomicBool::new(false);
    let before = versions.latest();
    std::thread::scope(|scope| {
        let lock = committer.state().lock();
        assert!(format!("{lock:?}").contains("CommitLock"));
        scope.spawn(|| {
            let batch = records(vec![put(aux(1), b"k", b"v")]);
            let published = batch.with(|batch| committer.write(batch, &mut NoStaleHashes));
            done.store(true, Ordering::SeqCst);
            assert_eq!(published.expect("a commit").version(), before.next());
        });
        // A reader and a writer in another transaction do not wait for it
        // (CM2).
        let reader = committer.path().begin();
        let mut out = Vec::new();
        assert!(!reader.get(aux(1), b"k", &mut out).expect("a read"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !done.load(Ordering::SeqCst),
            "a commit was made under the lock"
        );
        assert_eq!(versions.latest(), before);
        drop(lock);
    });
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(versions.latest(), before.next().expect("a version"));
    assert!(format!("{committer:?}").contains("Committer"));
    assert!(format!("{:?}", committer.state()).contains("published: 1"));
}

/// Every error says what it is, and what is behind it.
#[test]
fn the_errors_name_what_happened() {
    let log = || lignin_store::wal::LogError::NoActiveFile;
    let unsettled = Stopped::Unsettled {
        version: version(7),
        reason: "the read-back failed".into(),
    };
    let halted = Stopped::Halted {
        version: version(8),
        reason: "the pool refused".into(),
    };
    assert_eq!(unsettled.version(), version(7));
    assert!(unsettled
        .to_string()
        .contains("whether commit 7 is in the log is not known"));
    assert!(halted
        .to_string()
        .contains("commit 8 is in the log and could not be installed"));

    let failures = [
        (Failure::from(TxnError::ScopeOpen), "scope", true),
        (Failure::from(log()), "the log:", true),
        (
            Failure::from(lignin_store::table::LinkError::AlreadyFree),
            "the page table:",
            true,
        ),
        (
            Failure::VersionGap {
                log: version(5),
                latest: version(3),
            },
            "version 5",
            false,
        ),
        (Failure::PlaceLost, "taken again and again", false),
    ];
    for (failure, says, has_source) in failures {
        assert!(failure.to_string().contains(says), "{failure}");
        assert_eq!(failure.source().is_some(), has_source, "{failure}");
    }
    let publish = crate::snapshot::Versions::new(version(3))
        .publish(version(9))
        .expect_err("not the next version");
    let failure = Failure::from(publish);
    assert!(failure.source().is_some() && !failure.to_string().is_empty());

    let errors = [
        (
            CommitError::ScopesOpen,
            "a scope of the transaction is open",
            true,
        ),
        (CommitError::Conflict, "wrote a key after", false),
        (
            CommitError::Refused(TxnError::Damaged("pages")),
            "the commit was refused",
            true,
        ),
        (
            CommitError::NotReady(lignin_store::flush::FlushError::Log(log())),
            "takes no commit yet",
            true,
        ),
        (
            CommitError::Failed(log().into()),
            "the commit was not made",
            false,
        ),
        (
            CommitError::Unsettled {
                version: version(4),
                source: log(),
            },
            "whether commit 4 is in the log is not known",
            false,
        ),
        (
            CommitError::Halted {
                version: version(4),
                source: Failure::PlaceLost,
            },
            "commit 4 is in the log and could not be installed",
            false,
        ),
        (
            CommitError::Stopped(halted),
            "the session takes no commit",
            true,
        ),
    ];
    for (error, says, open) in errors {
        assert!(error.to_string().contains(says), "{error}");
        assert_eq!(error.leaves_transaction_open(), open, "{error}");
        assert!(error.published().is_none());
        let sourced = !matches!(
            error,
            CommitError::ScopesOpen | CommitError::Conflict | CommitError::Stopped(_)
        );
        assert_eq!(error.source().is_some(), sourced, "{error}");
        assert!(!format!("{error:?}").is_empty());
    }
    assert!(Committed::Empty.version().is_none() && Committed::Empty.published().is_none());
}
