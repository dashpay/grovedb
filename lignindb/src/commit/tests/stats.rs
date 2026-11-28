//! The open write transactions in the counts of the store
//! (`lignin_store::stats`, D-091 item 1): what a commit and a rollback end.

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use lignin_store::stats::Holder;

use super::{aux, records};
use crate::{
    commit::{
        state::{CommitState, Spent},
        CommitError, CommitStats, Committed, NoStaleHashes,
    },
    txn::{
        tests::{put, store::Db},
        WriteTxn,
    },
};

/// D-091 item 1, `lignin_store::stats`: a write transaction counts among
/// the open ones of the store from its first write until its write set is
/// empty again, by its commit, a rollback, a rollback to a savepoint that
/// takes every write out, or its drop. One that only reads, or whose first
/// write was refused, holds nothing and is not counted.
#[test]
fn a_write_transaction_counts_as_open_while_it_has_a_write() {
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let stats = committer.path().stats();
    let open = || stats.open_writes().0;
    let write = |txn: &WriteTxn<'_>, key: &[u8]| {
        let scope = txn.scope().expect("a scope");
        let batch = records(vec![put(aux(1), key, b"v")]);
        batch.with(|batch| scope.apply(batch)).expect("a write");
        scope.merge();
    };

    // A transaction that reads, and one whose only batch is refused.
    let first = committer.path().begin();
    let mut out = Vec::new();
    assert!(!first.get(aux(1), b"a", &mut out).expect("a read"));
    assert_eq!(stats.open_writes(), (0, None));
    let twice = records(vec![put(aux(1), b"a", b"1"), put(aux(1), b"a", b"2")]);
    let scope = first.scope().expect("a scope");
    twice
        .with(|batch| scope.apply(batch))
        .expect_err("a key twice");
    drop(scope);
    assert_eq!(stats.open_writes(), (0, None));

    // Its first write, and that of another: the oldest is the first.
    write(&first, b"a");
    let (count, oldest) = stats.open_writes();
    assert_eq!(count, 1);
    std::thread::sleep(Duration::from_millis(20));
    let second = committer.path().begin();
    write(&second, b"b");
    write(&second, b"c");
    let (count, older) = stats.open_writes();
    assert_eq!(count, 2);
    assert!(older > oldest && older >= Some(Duration::from_millis(20)));

    // What an operator reads says so, and names them as holders of the log.
    let reading = stats.read(&db.write_back(&versions));
    assert_eq!(reading.watch.open_writes, 2);
    assert!(reading.watch.oldest_write >= Some(Duration::from_millis(20)));
    let holders = reading.watch.log_held_by();
    assert!(
        (holders.iter()).any(|holder| matches!(holder, Holder::OpenWrites { count: 2, .. })),
        "{holders:?}"
    );

    // A commit that is refused leaves the transaction open, and it counts.
    let scope = first.scope().expect("a scope");
    let refused = committer.commit(&first, &mut NoStaleHashes);
    assert!(matches!(refused, Err(CommitError::ScopesOpen)));
    drop(scope);
    assert_eq!(open(), 2);

    // The commit ends the first, and the second is the oldest now.
    committer
        .commit(&first, &mut NoStaleHashes)
        .expect("a commit");
    let (count, younger) = stats.open_writes();
    assert_eq!(count, 1);
    assert!(younger < stats.read(&db.write_back(&versions)).watch.oldest_write);
    // The transaction can be written again, and counts again.
    write(&first, b"d");
    assert_eq!(open(), 2);

    // A rollback, and a commit that fails with a conflict.
    committer.rollback(&second).expect("no scope is open");
    assert_eq!(open(), 1);
    let batch = records(vec![put(aux(1), b"d", b"outside")]);
    (batch.with(|batch| committer.write(batch, &mut NoStaleHashes))).expect("a commit outside");
    assert_eq!(
        open(),
        1,
        "a write with no transaction is over when it returns"
    );
    let conflict = committer.commit(&first, &mut NoStaleHashes);
    assert!(matches!(conflict, Err(CommitError::Conflict)));
    assert_eq!(stats.open_writes(), (0, None));

    // A rollback to a savepoint that takes every write out.
    second.set_savepoint().expect("a savepoint");
    write(&second, b"e");
    assert_eq!(open(), 1);
    second.rollback_to_savepoint().expect("the savepoint");
    assert_eq!(open(), 0);
    // One that leaves a write leaves the transaction open.
    write(&second, b"f");
    second.set_savepoint().expect("a savepoint");
    write(&second, b"g");
    second.rollback_to_savepoint().expect("the savepoint");
    assert_eq!(open(), 1);

    // A transaction that is dropped with its writes.
    drop(second);
    assert_eq!(stats.open_writes(), (0, None));
    assert_eq!(db.pool.census().private, 0);
}

/// The five times of [`CommitStats`], in the order of [`Spent`].
fn times(stats: CommitStats) -> [Duration; 5] {
    [
        stats.lock_waited,
        stats.counts_waited,
        stats.appending,
        stats.placing,
        stats.reclaiming,
    ]
}

/// Time is counted under what it was spent on, and under nothing else:
/// each of the five times of [`CommitStats`] grows by at least what its
/// thread spent, and the other four stay.
#[test]
fn time_is_counted_under_what_it_was_spent_on() {
    const SPENT: Duration = Duration::from_millis(2);
    let state = CommitState::new();
    assert_eq!(times(state.stats()), [Duration::ZERO; 5]);
    let all = [
        Spent::LockWait,
        Spent::CountsWait,
        Spent::Appending,
        Spent::Placing,
        Spent::Reclaiming,
    ];
    for (at, what) in all.into_iter().enumerate() {
        for _ in 0..2 {
            let before = times(state.stats());
            let began = Instant::now();
            thread::sleep(SPENT);
            state.spent(what, began);
            let after = times(state.stats());
            for other in 0..all.len() {
                match other == at {
                    true => assert!(after[at] >= before[at] + SPENT, "{what:?}: {after:?}"),
                    false => assert_eq!(after[other], before[other], "{what:?}"),
                }
            }
        }
    }
}

/// A commit counts the time from where it is about to take the commit
/// lock until it has it ([`CommitStats::lock_waited`]). The lock is held
/// here when the commit says that it is about to take it, and for `HELD`
/// longer, so at least that long is counted, whatever the schedule.
#[test]
fn a_commit_counts_its_wait_for_the_commit_lock() {
    const HELD: Duration = Duration::from_millis(20);
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let state = committer.state();
    assert_eq!(times(committer.stats()), [Duration::ZERO; 5]);

    let (at_the_lock, arrived) = mpsc::channel();
    state.run_before_lock(move || {
        let _ = at_the_lock.send(());
    });
    let committed = thread::scope(|threads| {
        let lock = state.lock();
        let commit = threads.spawn(|| {
            let txn = committer.path().begin();
            let scope = txn.scope().expect("a scope");
            let batch = records(vec![put(aux(1), b"a", b"v")]);
            batch.with(|batch| scope.apply(batch)).expect("a write");
            scope.merge();
            committer.commit(&txn, &mut NoStaleHashes)
        });
        // A commit that never comes to the lock fails the test here: the
        // lock is given back as this leaves, and the commit ends.
        let arrived = arrived.recv_timeout(Duration::from_secs(60));
        arrived.expect("the commit is at the lock");
        thread::sleep(HELD);
        drop(lock);
        commit.join().expect("the commit ends")
    });
    state.run_before_lock(|| {});
    assert!(matches!(committed, Ok(Committed::Published(_))));

    let stats = committer.stats();
    assert_eq!(stats.published, 1);
    assert!(stats.lock_waited >= HELD, "{stats:?}");
    // The commit appended its group, which took some time, and waited
    // for nothing else that is counted apart.
    assert!(stats.appending > Duration::ZERO, "{stats:?}");
}
