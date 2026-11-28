//! Commits, reads, the flush and the counts through the facade, and a
//! session that takes no write any more (D-098).

use std::{error::Error, sync::Arc};

use lignin_format::{tree_page::HashField, CommitVersion};
use lignin_io::sim::{Fault, FaultRule, OpSet, SimConfig, SimVfs};
use lignin_store::wal::SyncPolicy;
use lignin_tree::{
    hash::{HashRules, Refusal, Stale},
    node::NodeRef,
};

use super::*;
use crate::{
    commit::CommitError,
    db::{DbError, Reopen, WriteError},
    snapshot::LatestLease,
    txn::Unit,
};

fn aux(name: u8, key: &[u8], value: &[u8]) -> (Column, Subtree, Vec<u8>, Vec<u8>) {
    (Column::Aux, subtree(name), key.to_vec(), value.to_vec())
}

/// Enters the subtrees `names`, each with an aux record `k` = its name.
fn enter(db: &Db, names: &[u8]) -> Box<Published> {
    let writes = Writes {
        enter: names.iter().map(|name| (subtree(*name), None)).collect(),
        records: names
            .iter()
            .map(|name| aux(*name, b"k", &[*name]))
            .collect(),
        ..Writes::default()
    };
    writes.commit(db)
}

fn read_aux(operation: &Operation<'_>, name: u8, key: &[u8]) -> Option<Vec<u8>> {
    let opened = operation.open(subtree(name)).expect("a lookup")?;
    let mut value = Vec::new();
    (opened.get(Column::Aux, key, &mut value))
        .expect("a read")
        .then_some(value)
}

#[test]
fn a_transaction_commits_rolls_back_and_conflicts_through_the_facade() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    enter(&db, &[1, 2]);
    let before = db.snapshot();

    // A transaction's writes are its own until it commits.
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let put = Writes {
        records: vec![aux(1, b"a", b"one")],
        ..Writes::default()
    };
    put.with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();
    let mut value = Vec::new();
    assert!(txn
        .get(Unit::new(Column::Aux, subtree(1)), b"a", &mut value)
        .expect("a read"));
    let mut lease = LatestLease::new();
    assert_eq!(read_aux(&db.read(&mut lease), 1, b"a"), None);

    // A rollback gives them up, and the transaction is written again.
    db.rollback(&txn).expect("the transaction is rolled back");
    value.clear();
    assert!(!txn
        .get(Unit::new(Column::Aux, subtree(1)), b"a", &mut value)
        .expect("a read"));
    let scope = txn.scope().expect("a scope");
    put.with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();

    // A second transaction writes the same key and commits first: the
    // first one fails with the conflict, and with nothing else.
    let other = db.begin();
    let scope = other.scope().expect("a scope");
    let theirs = Writes {
        records: vec![aux(1, b"a", b"other")],
        ..Writes::default()
    };
    theirs
        .with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();
    let committed = db.commit(&other, &mut NoStaleHashes).expect("the commit");
    let published = committed.published().expect("it took a version");
    assert_eq!(published.version, db.version());
    let error = db.commit(&txn, &mut NoStaleHashes).expect_err("a conflict");
    assert!(error.is_conflict() && !error.leaves_transaction_open());
    // A transaction that wrote nothing commits nothing.
    let empty = db
        .commit(&txn, &mut NoStaleHashes)
        .expect("an empty commit");
    assert!(matches!(empty, Committed::Empty));

    // The snapshot from before reads its version, and a new one the
    // latest.
    assert_eq!(read_aux(&before.operation(), 1, b"a"), None);
    assert_eq!(
        read_aux(&db.snapshot().operation(), 1, b"a"),
        Some(b"other".to_vec())
    );
    assert_eq!(read_aux(&db.read(&mut lease), 2, b"k"), Some(vec![2]));
    let stats = db.stats();
    assert_eq!(
        (stats.pins, stats.commits.conflicts, stats.commits.rollbacks),
        (1, 1, 1)
    );
    assert_eq!(stats.commits.published, 2);

    // A batch that the engine refuses is refused whole.
    let unentered = Writes {
        records: vec![aux(9, b"a", b"x")],
        ..Writes::default()
    };
    let refused = unentered.with(|batch| db.write(batch, &mut NoStaleHashes));
    let Err(WriteError::Commit(CommitError::Refused(_))) = &refused else {
        panic!("{refused:?}");
    };
    assert!(refused.expect_err("refused").leaves_transaction_open());
}

/// The hash rules of a caller that leaves hashes to the commit: each call
/// is counted, here and in the store's counts.
struct Rules(u64);

impl HashRules for Rules {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        _: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.0 += 1;
        Ok([value.len() as u8; 32])
    }

    fn kv_hash(&mut self, key: &[u8], _: &Hash) -> Result<Hash, Refusal> {
        self.0 += 1;
        Ok([key.len() as u8; 32])
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.0 += 1;
        Ok(*node.kv_hash())
    }
}

/// D-094: the hashes a commit evaluates and the searches by key from the
/// root of a Merk are counted in the store's counts, which the facade
/// reads with every other count.
#[test]
fn the_counts_of_the_store_are_read_through_the_facade() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let before = db.stats();
    assert_eq!((before.store.hashes, before.store.keyed_lookups), (0, 0));
    plant(&db, subtree(1), Merk::sized(b'k', 64, 30));
    let planted = db.stats();
    assert_eq!(planted.store.hashes, 0, "every hash was made by its writer");
    assert_eq!(planted.store.log_groups, 2, "the keyspaces, and the Merk");
    assert!(planted.store.log_bytes > before.store.log_bytes);
    assert!(planted.store.node_records > 0);

    // A node that is read by its key is one search from the root.
    let key = [b'k', 0, 0, 0, 5];
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let found = scope.node(subtree(1), &key, |node| node.is_some());
    assert!(found.expect("a read"));
    assert_eq!(
        db.stats().store.keyed_lookups,
        planted.store.keyed_lookups + 1
    );

    // A write that leaves a node's hashes to the commit: the rules are
    // asked, and every call is a hash the tree computed.
    let value = vec![9u8; 30];
    let node = NodePut {
        key: b"stale",
        feature: &[3, 1, 0, 0, 0, 0, 0, 0, 0],
        value_hash: &HASH,
        kv_hash: &HASH,
        left: None,
        right: None,
        hosted: Hosted::Set(&[]),
        value: ValuePut::Value(&value),
        stale: Stale::of(HashField::KeyValue),
        at: None,
    };
    let enter = [SubtreeWrite::Enter {
        subtree: subtree(2),
        tree_type: TREE_TYPE,
        host: None,
    }];
    let merk = [MerkWrite {
        subtree: subtree(2),
        nodes: &[node],
        removed: &[],
        root: RootPut::Set(Some(b"stale")),
        anchored: false,
        whole: false,
    }];
    scope
        .apply(&Batch {
            subtrees: &enter,
            merks: &merk,
            ..Batch::default()
        })
        .expect("the batch is applied");
    scope.merge();
    let mut rules = Rules(0);
    let committed = db.commit(&txn, &mut rules).expect("the commit");
    assert_eq!(committed.published().expect("a version").evaluated, 1);
    assert!(rules.0 > 0);
    let after = db.stats();
    assert_eq!(after.store.hashes, rules.0);

    let report = after.to_string();
    for part in [
        "version 3",
        "commits: 2 published",
        "readers: 0 pins",
        "reclaimed: 0",
    ] {
        assert!(report.contains(part), "{part} in:\n{report}");
    }
    assert!(format!("{after:?}").contains("DbStats"));
}

fn sim(seed: u64, sync: SyncPolicy) -> (SimVfs, Options) {
    let fs = SimVfs::new(SimConfig::new(seed));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        sync,
        ..options()
    };
    (fs, options)
}

/// DU1a, T-DU1, T-DU2: every commit that was published when `flush` was
/// called is recovered after a power cut at any time after it returned,
/// and whatever the cut leaves is one committed version.
#[test]
fn a_flush_makes_the_commits_before_it_durable() {
    for seed in 0..12 {
        let (fs, options) = sim(seed, SyncPolicy::Never);
        let db = Db::open("/db", &options).expect("a new database");
        // Each commit enters one more subtree: version v has those up to
        // v - 1.
        for name in 1..=4 {
            enter(&db, &[name]);
        }
        let flushed = db.flush().expect("the log is synced");
        assert_eq!(flushed.version, db.version());
        assert!(flushed.durable.version >= flushed.version);
        for name in 5..=8 {
            enter(&db, &[name]);
        }
        let last = db.version();
        let crashed = fs.crash(seed * 31 + 5);
        drop(db);

        let options = Options {
            vfs: Arc::new(crashed),
            ..options
        };
        let db = Db::open("/db", &options).expect("the database is recovered");
        let landed = db.version();
        assert!(
            flushed.version <= landed && landed <= last,
            "seed {seed}: {landed}"
        );
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        for name in 1..=8u8 {
            let entered = u64::from(name) < landed.get();
            let expected = entered.then(|| vec![name]);
            assert_eq!(
                read_aux(&operation, name, b"k"),
                expected,
                "seed {seed}, {name}"
            );
        }
        // The session goes on from the version it landed on (DU3).
        drop(operation);
        drop(snapshot);
        let next = enter(&db, &[20]);
        assert_eq!(Some(next.version), landed.next());
    }
}

/// D-098: after a commit that is published and whose sync failed, the
/// session takes no write: the next commit, write and wipe are refused
/// with the reason, the transaction is as it was, and reads go on. The
/// next session has the commit.
#[test]
fn a_session_whose_sync_failed_takes_no_write() {
    let (fs, options) = sim(21, SyncPolicy::EveryCommit);
    let db = Db::open("/db", &options).expect("a new database");
    enter(&db, &[1]);
    assert!(db.reopen().is_none());
    // The background work is at rest, so the sync that fails is the
    // commit's: one that write-back met would end the session as well
    // (D-151), before the commit.
    halt_background(&db);

    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
    let writes = Writes {
        records: vec![aux(1, b"a", b"published")],
        ..Writes::default()
    };
    let ended = writes.with(|batch| db.write(batch, &mut NoStaleHashes));
    let error = ended.expect_err("the sync failed");
    let version = error.published().expect("the commit is published").version;
    assert_eq!(version, db.version());
    assert!(matches!(
        error,
        WriteError::Commit(CommitError::NotDurable { .. })
    ));
    let reason = error.reopen().expect("this commit ended the session");
    assert!(matches!(&reason, Reopen::NotDurable { version: at, .. } if *at == version));
    assert_eq!(db.reopen(), Some(reason.clone()));
    assert_eq!(db.stats().reopen, Some(reason.clone()));
    fs.clear_rules();

    // Nothing more is taken, with or without a transaction.
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let more = Writes {
        records: vec![aux(1, b"b", b"refused")],
        ..Writes::default()
    };
    more.with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();
    let refused = [
        db.commit(&txn, &mut NoStaleHashes),
        more.with(|batch| db.write(batch, &mut NoStaleHashes)),
        db.wipe(),
    ];
    for ended in refused {
        let error = ended.expect_err("the session takes no write");
        assert!(
            matches!(&error, WriteError::Reopen(why) if *why == reason),
            "{error:?}"
        );
        assert!(error.leaves_transaction_open() && error.published().is_none());
        assert!(error.to_string().contains("has to be opened again"));
        assert!(error.to_string().contains("a sync of the log failed"));
        assert!(error.source().is_some() && !error.is_conflict());
        assert_eq!(error.reopen(), Some(reason.clone()));
    }
    assert_eq!(db.version(), version, "nothing was published");
    assert_eq!(txn.stats().writes, 1, "the transaction is as it was");
    db.rollback(&txn).expect("a rollback is taken");
    // Reads go on, of what was published.
    let read = read_aux(&db.snapshot().operation(), 1, b"a");
    assert_eq!(read, Some(b"published".to_vec()));
    drop(txn);
    let report = db.stats().to_string();
    assert!(report.contains("the session takes no write"), "{report}");
    drop(db);

    // The process ended and the file system kept what it was given: the
    // next session has the commit, and takes writes.
    let db = Db::open("/db", &options).expect("the database is opened again");
    assert_eq!(db.version(), version);
    assert!(db.reopen().is_none());
    enter(&db, &[2]);
}

/// A writer that was let in before the sync of another commit failed, and
/// waits for the commit lock, is refused like one that comes later: the
/// reason is recorded under the commit lock, before the lock is given up.
/// Its transaction is as it was.
#[test]
fn a_writer_that_waits_for_the_commit_lock_is_refused_after_a_failed_sync() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (fs, options) = sim(23, SyncPolicy::EveryCommit);
    let db = Db::open("/db", &options).expect("a new database");
    enter(&db, &[1, 2]);
    halt_background(&db);

    let waiting = db.begin();
    let scope = waiting.scope().expect("a scope");
    let more = Writes {
        records: vec![aux(2, b"b", b"refused")],
        ..Writes::default()
    };
    more.with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();

    // The first commit's sync has failed and its version is not published
    // yet when its hook runs, with the commit lock held: the session
    // refuses nothing yet. The second writer begins its commit then. It
    // says when it is about to take the commit lock, with every check that
    // is made without the lock passed, and only then does the first one go
    // on. So what refuses the second writer is the look under the lock.
    let flag = || Arc::new(AtomicBool::new(false));
    let (locked, at_the_lock, refusing) = (flag(), flag(), flag());
    refusing.store(true, Ordering::Release);
    let committing = &db.shared.store.committing;
    let (shared, told) = (
        db.shared.clone(),
        (locked.clone(), at_the_lock.clone(), refusing.clone()),
    );
    committing.run_before_publication(move |_| {
        let (locked, at_the_lock, refusing) = &told;
        refusing.store(shared.store.refusal().is_some(), Ordering::Release);
        locked.store(true, Ordering::Release);
        wait_for("the second writer is at the commit lock", || {
            at_the_lock.load(Ordering::Acquire)
        });
    });
    let told = (locked.clone(), at_the_lock.clone());
    committing.run_before_lock(move || {
        // The first writer comes by here too, before it has the lock.
        let (locked, at_the_lock) = &told;
        if locked.load(Ordering::Acquire) {
            at_the_lock.store(true, Ordering::Release);
        }
    });
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
    let (first, second) = std::thread::scope(|threads| {
        let second = threads.spawn(|| {
            wait_for("the first commit holds the commit lock", || {
                locked.load(Ordering::Acquire)
            });
            db.commit(&waiting, &mut NoStaleHashes)
        });
        let writes = Writes {
            records: vec![aux(1, b"a", b"published")],
            ..Writes::default()
        };
        let first = writes.with(|batch| db.write(batch, &mut NoStaleHashes));
        (first, second.join().expect("the second writer ends"))
    });
    committing.run_before_lock(|| {});
    (db.shared.store.committing).run_before_publication(|_| {});
    fs.clear_rules();
    assert!(
        !refusing.load(Ordering::Acquire),
        "the session refused before the first commit was published"
    );
    assert!(at_the_lock.load(Ordering::Acquire));

    let first = first.expect_err("the sync failed");
    let version = first.published().expect("the commit is published").version;
    let reason = first.reopen().expect("this commit ended the session");
    assert!(matches!(&reason, Reopen::NotDurable { version: at, .. } if *at == version));
    let second = second.expect_err("the session takes no write");
    assert!(
        matches!(&second, WriteError::Reopen(why) if *why == reason),
        "{second:?}"
    );
    assert!(second.leaves_transaction_open());
    assert_eq!(db.version(), version, "nothing was published behind it");
    assert_eq!(db.stats().commits.not_durable, 1);
    assert_eq!(waiting.stats().writes, 1, "the transaction is as it was");
    assert_eq!(read_aux(&db.snapshot().operation(), 2, b"b"), None);
    db.rollback(&waiting).expect("a rollback is taken");
}

/// A transaction is rolled back where it was begun: another database
/// refuses it and leaves it as it is.
#[test]
fn a_transaction_of_another_database_is_not_rolled_back() {
    let (one, other) = (temp(), temp());
    let db = Db::open(one.path(), &options()).expect("a new database");
    let elsewhere = Db::open(other.path(), &options()).expect("another database");
    enter(&db, &[1]);
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let writes = Writes {
        records: vec![aux(1, b"a", b"mine")],
        ..Writes::default()
    };
    writes
        .with(|batch| scope.apply(batch))
        .expect("the batch is applied");
    scope.merge();

    let rollbacks = elsewhere.stats().commits.rollbacks;
    let error = elsewhere.rollback(&txn).expect_err("not its transaction");
    assert!(error.to_string().contains("another database"), "{error}");
    assert_eq!(elsewhere.stats().commits.rollbacks, rollbacks);
    assert_eq!(txn.stats().writes, 1, "the transaction is as it was");
    assert!(elsewhere.commit(&txn, &mut NoStaleHashes).is_err());
    let committed = db
        .commit(&txn, &mut NoStaleHashes)
        .expect("its own database");
    assert!(matches!(committed, Committed::Published(_)));
    assert_eq!(
        read_aux(&db.snapshot().operation(), 1, b"a"),
        Some(b"mine".to_vec())
    );
}

/// A flush whose sync fails promises nothing and ends the session for
/// writes too.
#[test]
fn a_flush_that_fails_ends_the_session_for_writes() {
    let (fs, options) = sim(22, SyncPolicy::Never);
    let db = Db::open("/db", &options).expect("a new database");
    enter(&db, &[1]);
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
    let error = db.flush().expect_err("the sync failed");
    assert!(matches!(error, DbError::Flush(_)));
    assert!(error.to_string().starts_with("write-back: ") && error.source().is_some());
    fs.clear_rules();
    let latest = db.version();
    assert!(matches!(db.reopen(), Some(Reopen::NotDurable { version, .. }) if version == latest));
    let ended = db.wipe().expect_err("the session takes no write");
    assert!(matches!(
        ended,
        WriteError::Reopen(Reopen::NotDurable { .. })
    ));
    // A second flush fails as the first did: the log makes nothing
    // durable any more in this session.
    assert!(db.flush().is_err());
    assert!(db.close().is_err());
}

/// D-098: a commit of which the log cannot say whether it holds it stops
/// the commit path, and the facade says to open the database again.
#[test]
fn a_session_that_the_commit_path_stopped_takes_no_write() {
    for seed in 0..4 {
        let (fs, options) = sim(40 + seed, SyncPolicy::Never);
        let db = Db::open("/db", &options).expect("a new database");
        enter(&db, &[1]);
        // The background work is at rest, so the append that fails is the
        // commit's: it is taken back, and the sync that would settle it
        // fails too.
        halt_background(&db);
        fs.add_rule(
            FaultRule::new(Fault::Eio)
                .on(lignin_io::sim::OpKind::Append)
                .torn_append(false),
        );
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
        let before = db.version();
        let writes = Writes {
            records: vec![aux(1, b"a", &[7; 3000])],
            ..Writes::default()
        };
        let ended = writes.with(|batch| db.write(batch, &mut NoStaleHashes));
        fs.clear_rules();
        let Err(WriteError::Commit(CommitError::Unsettled { .. })) = &ended else {
            panic!("seed {seed}: {ended:?}");
        };
        let said = ended.expect_err("unsettled").reopen();
        assert!(matches!(said, Some(Reopen::Stopped(_))), "{said:?}");
        assert_eq!(db.version(), before, "nothing is published");
        let reason = db.reopen().expect("the session is stopped");
        assert!(matches!(&reason, Reopen::Stopped(_)));
        assert!(reason.to_string().contains("is not known"), "{reason}");
        let next = writes.with(|batch| db.write(batch, &mut NoStaleHashes));
        let error = next.expect_err("the session takes no write");
        assert!(matches!(&error, WriteError::Reopen(why) if *why == reason));
        assert_eq!(read_aux(&db.snapshot().operation(), 1, b"k"), Some(vec![1]));
    }
}

/// A commit that ended the session, or found it ended, says to open again
/// by itself.
#[test]
fn a_commit_that_ends_the_session_names_its_reason() {
    use crate::commit::{Failure, Stopped};
    let version = |raw| CommitVersion::new(raw).expect("a version");
    let stopped = Stopped::Halted {
        version: version(4),
        reason: "refused".into(),
    };
    let error = WriteError::from(CommitError::Stopped(stopped.clone()));
    assert_eq!(error.reopen(), Some(Reopen::Stopped(stopped)));
    assert!(error.to_string().contains("the session takes no commit"));
    let reopen = error.reopen().expect("a reason");
    assert!(reopen.to_string().contains("could not be installed"));

    let halted = WriteError::from(CommitError::Halted {
        version: version(7),
        source: Failure::PlaceLost,
    });
    let said = halted.reopen().expect("the commit halted the session");
    assert!(
        matches!(&said, Reopen::Stopped(Stopped::Halted { version: at, .. }) if *at == version(7))
    );
    assert!(!halted.leaves_transaction_open());
    let gap = WriteError::from(CommitError::Failed(Failure::VersionGap {
        log: version(9),
        latest: version(5),
    }));
    let said = gap.reopen().expect("the commit stopped the session");
    assert!(
        matches!(&said, Reopen::Stopped(Stopped::Halted { version: at, .. }) if *at == version(6))
    );
    // A commit that only failed leaves the session as it was.
    let failed = WriteError::from(CommitError::Failed(Failure::PlaceLost));
    assert!(failed.reopen().is_none() && failed.published().is_none());
}
