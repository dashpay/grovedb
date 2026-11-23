//! T-CM1: the one append of a commit, and the sync that a sync policy asks
//! of it, are made to fail or to tear, and what the commit reports is
//! checked against recovery's own test of the log (`transactions.md` CM9):
//! after a crash at that moment, after the cut, and after later commits.
//!
//! The file layer is the simulated one (`lignin_io::sim`). A failed append
//! stops at a sector boundary that is drawn from the seed, and stays in
//! the file or is taken back. A crash keeps, loses or tears each change
//! that was not synced, as a plan says: every plan where they are few, and
//! prefixes, holes, reorders and drawn samples where they are many.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    path::Path,
    sync::Arc,
};

use lignin_io::{
    sim::{Effect, Event, Fault, FaultRule, OpKind, OpSet, Outcome as OpOutcome, SimVfs},
    Vfs,
};
use lignin_store::{
    segment::ManifestStore,
    wal::{LogError, LogOptions, SyncPolicy},
};

use super::{aux, read_groups, records, sim, store_on, DIR};
use crate::{
    commit::{CommitError, Committed, Committer, Failure, NoStaleHashes, Stopped},
    snapshot::Versions,
    txn::{
        tests::{put, store::Db},
        TxnError,
    },
};

/// One commit that was tried: the number of the attempt, which the value
/// it writes begins with, and the version it had or would have had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Attempt {
    number: u8,
    version: u64,
}

/// What the commits of one session came to.
#[derive(Debug, Default)]
struct Told {
    /// The commits that were reported as made, in order: published, and
    /// published without being durable.
    happened: Vec<Attempt>,
    /// How many of them a successful sync of the log has covered.
    durable: usize,
    /// The attempts that were reported as failed.
    failed: Vec<u8>,
    /// The attempt whose outcome the engine could not find out.
    unsettled: Option<Attempt>,
}

struct Session {
    fs: SimVfs,
    db: Arc<Db>,
    versions: Versions,
    told: RefCell<Told>,
    attempts: Cell<u8>,
    /// In how many states of a crash the unsettled attempt was read as the
    /// last group of the log.
    came_back: Cell<usize>,
}

fn key(number: u8) -> [u8; 2] {
    [b'c', number]
}

/// The value of an attempt: too long for its entry, so that the group
/// carries it as a value record and spans several sectors.
fn value(number: u8) -> Vec<u8> {
    let mut value = vec![number; 3000];
    value[1] = !number;
    value
}

impl Session {
    fn new(seed: u64, sync: SyncPolicy) -> Self {
        let fs = sim(seed);
        let options = LogOptions {
            sync,
            ..LogOptions::default()
        };
        let (db, versions) = store_on(&fs, options);
        Self {
            fs,
            db,
            versions,
            told: RefCell::default(),
            attempts: Cell::new(0),
            came_back: Cell::new(0),
        }
    }

    fn committer(&self) -> Committer<'_> {
        self.db.committer(&self.versions)
    }

    /// Tries one commit, of one flat record that no other attempt writes.
    fn attempt(&self) -> (Attempt, Result<Committed, CommitError>) {
        self.attempts.set(self.attempts.get() + 1);
        let number = self.attempts.get();
        let attempt = Attempt {
            number,
            version: self.versions.latest().get() + 1,
        };
        let batch = records(vec![put(aux(1), &key(number), &value(number))]);
        let ended = batch.with(|batch| self.committer().write(batch, &mut NoStaleHashes));
        (attempt, ended)
    }

    /// A commit that no fault meets.
    fn commit(&self) -> Attempt {
        let (attempt, ended) = self.attempt();
        let published = ended.expect("no fault is armed");
        assert_eq!(
            published.version().map(|version| version.get()),
            Some(attempt.version)
        );
        let mut told = self.told.borrow_mut();
        told.happened.push(attempt);
        if self.db.log.options().sync == SyncPolicy::EveryCommit {
            told.durable = told.happened.len();
        }
        attempt
    }

    /// `flush()`: every commit so far is durable when it returns.
    fn flush(&self) {
        let durable = (self.db.write_back(&self.versions).sync_log()).expect("the log is synced");
        assert_eq!(durable.version, self.versions.latest());
        let mut told = self.told.borrow_mut();
        told.durable = told.happened.len();
    }

    /// What every handle reads: the record of each commit that happened,
    /// and of no other attempt.
    fn check_reads(&self) {
        let reader = self.committer().path().begin();
        let told = self.told.borrow();
        for number in 1..=self.attempts.get() {
            let mut out = Vec::new();
            let found = reader.get(aux(1), &key(number), &mut out).expect("a read");
            let happened = told.happened.iter().any(|made| made.number == number);
            assert_eq!(found, happened, "attempt {number}");
            if found {
                assert_eq!(out, value(number), "attempt {number}");
            }
        }
        let latest = told.happened.last().map_or(1, |made| made.version);
        assert_eq!(self.versions.latest().get(), latest);
        assert_eq!(self.db.pool.census().private, 0, "a copy is left");
        assert_eq!(self.db.private_ids(), 0, "an id is left with a transaction");
    }

    /// Checks what a recovery reads of the log against what the commits
    /// were told: now, as the files are, and after a crash at this moment,
    /// for each state a crash can leave of them.
    fn check_recovery(&self, context: &str) {
        // As the files are now, under the manifest that is in force.
        self.check_groups(super::groups(&self.db), context, true);
        let image = self.fs.crash_image();
        let mut plans = vec![image.lose_all(), image.keep_all()];
        match image.every_plan(400) {
            Some(every) => plans.extend(every),
            None => {
                plans.extend(image.prefixes());
                plans.extend(image.holes());
                plans.extend(image.reorders());
                plans.extend((0..48).map(|draw| image.sample(draw)));
            }
        }
        for (index, plan) in plans.iter().enumerate() {
            let Ok(left) = image.recover(plan) else {
                continue;
            };
            self.check_log(&left, &format!("{context}, crash plan {index}"), index == 1);
        }
    }

    /// Reads the log of `fs` as the next open would (DU2), and checks it:
    /// - the groups are whole, and their versions are dense from 1;
    /// - they are the commits that were reported as made, in order, each
    ///   with the value its attempt wrote, and at least those that a sync
    ///   has covered;
    /// - no attempt that was reported as failed is among them;
    /// - an attempt whose outcome is unsettled is the last of them or is
    ///   not there;
    /// - with `whole`, where no unsynced change was lost, all of the
    ///   commits that were made are there.
    fn check_log(&self, fs: &SimVfs, context: &str, whole: bool) {
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let manifest = ManifestStore::open(vfs.clone(), Path::new(DIR));
        let manifest = manifest.unwrap_or_else(|error| panic!("{context}: the manifest: {error}"));
        let named = manifest.manifest().expect("a manifest").clone();
        self.check_groups(read_groups(vfs, Path::new(DIR), &named), context, whole);
    }

    fn check_groups(&self, groups: Vec<super::Group>, context: &str, whole: bool) {
        let told = &*self.told.borrow();
        // The first group is the one that made the keyspaces.
        let commits = groups.iter().skip(1);
        let read: Vec<Attempt> = commits
            .map(|group| {
                assert_eq!(group.values.len(), 1, "{context}: {group:?}");
                let bytes = &group.values[0].1;
                assert_eq!(bytes, &value(bytes[0]), "{context}: a value is not whole");
                Attempt {
                    number: bytes[0],
                    version: group.version,
                }
            })
            .collect();
        for (index, group) in groups.iter().enumerate() {
            assert_eq!(group.version, index as u64 + 1, "{context}: {groups:?}");
        }
        for made in &read {
            assert!(
                !told.failed.contains(&made.number),
                "{context}: attempt {} was reported as failed and is recovered",
                made.number
            );
        }
        let (in_order, last) = match told.unsettled {
            Some(unsettled) if read.last() == Some(&unsettled) => {
                (&read[..read.len() - 1], Some(unsettled))
            }
            _ => (&read[..], None),
        };
        assert!(
            told.happened.starts_with(in_order),
            "{context}: read {read:?}, told {told:?}"
        );
        assert!(
            in_order.len() >= told.durable,
            "{context}: {} commits were durable, {} are read",
            told.durable,
            in_order.len()
        );
        if whole {
            assert_eq!(in_order, &told.happened[..], "{context}");
        }
        if last.is_some() {
            assert_eq!(in_order.len(), told.happened.len(), "{context}");
            self.came_back.set(self.came_back.get() + 1);
        }
    }

    /// What the last injected fault left of its operation.
    fn last_fault(&self) -> (OpKind, Effect) {
        let injected = self
            .fs
            .trace()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                Event::Op(op) => match op.outcome {
                    OpOutcome::Injected { effect, .. } => Some((op.kind, effect)),
                    _ => None,
                },
                _ => None,
            });
        injected.expect("a fault was injected")
    }
}

/// How the append of a commit's group is made to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Broken {
    fault: Fault,
    /// Whether what the append wrote stays in the file.
    torn: bool,
}

/// One session in which the append of the third commit fails: what the
/// commit reports, what a recovery reads at each point from there, and
/// that the session goes on under the version that was not taken.
/// Returns how many bytes of the group the failed append wrote.
fn a_failed_append(seed: u64, sync: SyncPolicy, broken: Broken, flushed: bool) -> (u64, bool) {
    let context = format!("seed {seed}, {sync:?}, {broken:?}");
    let session = Session::new(seed, sync);
    session.commit();
    session.commit();
    if flushed {
        session.flush();
    }
    let reader = session.committer().path().begin();
    let before = session.versions.latest();
    let logged = session.committer().stats().group_bytes;

    let rule = FaultRule::new(broken.fault)
        .on(OpKind::Append)
        .torn_append(broken.torn);
    session.fs.add_rule(rule);
    let (attempt, ended) = session.attempt();
    let (kind, effect) = session.last_fault();
    assert_eq!(kind, OpKind::Append, "{context}");
    let wrote = match effect {
        Effect::AppendTorn(wrote) | Effect::AppendUndone(wrote) => wrote,
        other => panic!("{context}: {other:?}"),
    };

    let made = match ended {
        // All of the group stayed in the file: the commit has happened,
        // whatever the file layer said of its call (CM9 item 2).
        Ok(published) => {
            assert!(
                broken.torn,
                "{context}: an append that was taken back is no commit"
            );
            let group_len = session.committer().stats().group_bytes - logged;
            assert_eq!(
                wrote, group_len,
                "{context}: a part of a group was taken for it"
            );
            assert_eq!(
                published.version().map(|version| version.get()),
                Some(attempt.version)
            );
            session.told.borrow_mut().happened.push(attempt);
            true
        }
        Err(CommitError::Failed(Failure::Log(error))) => {
            // What stays in the file is no item, and nothing completes it.
            // With the file as long as it was, the log has made that
            // durable before the commit was reported as failed.
            let stays = broken.torn && wrote > 0;
            match &error {
                LogError::NotAppended { .. } if stays => {}
                LogError::TakenBack { .. } if !stays => {}
                other => panic!("{context}: {other:?}"),
            }
            session.told.borrow_mut().failed.push(attempt.number);
            assert_eq!(session.versions.latest(), before, "{context}");
            assert_eq!(session.committer().stats().failed, 1);
            false
        }
        Err(other) => panic!("{context}: {other}"),
    };
    // Every handle reads what it read before a failed commit, and a
    // recovery never has it: at once,
    session.check_reads();
    let mut out = Vec::new();
    assert!(reader.get(aux(1), &key(1), &mut out).expect("a read"));
    session.check_recovery(&format!("{context}, at once"));

    // nothing else is appended behind the failed bytes,
    let cut_owed = session.db.log.next_group().is_err();
    assert!(
        cut_owed,
        "{context}: the log file takes items after a failed append"
    );
    let mark = session.db.log.append_mark(before);
    assert!(
        matches!(mark, Err(LogError::NoActiveFile)),
        "{context}: {mark:?}"
    );

    // after the cut, which the next commit has made before it begins, and
    // that commit takes the version that was not taken,
    let next = session.commit();
    let expected = attempt.version + u64::from(made);
    assert_eq!(next.version, expected, "{context}");
    session.check_reads();
    session.check_recovery(&format!("{context}, after the cut and a commit"));

    // and after later commits and a flush, which every state of a crash
    // then has.
    session.commit();
    session.flush();
    session.commit();
    session.check_reads();
    session.check_recovery(&format!("{context}, after a flush"));
    (wrote, made)
}

/// The append of a commit fails or tears at every boundary, under the
/// default policy and under one that syncs each commit.
#[test]
fn a_commit_whose_append_fails_is_never_recovered_and_one_that_is_whole_always() {
    let mut boundaries = BTreeSet::new();
    let (mut whole, mut failed) = (0, 0);
    for seed in 0..24 {
        for sync in [SyncPolicy::Never, SyncPolicy::EveryCommit] {
            for (fault, torn) in [
                (Fault::Enospc, true),
                (Fault::Enospc, false),
                (Fault::Eio, true),
                (Fault::Eio, false),
            ] {
                let broken = Broken { fault, torn };
                let (wrote, made) = a_failed_append(seed, sync, broken, seed % 2 == 0);
                boundaries.insert((torn, wrote));
                match made {
                    true => whole += 1,
                    false => failed += 1,
                }
            }
        }
    }
    // The group is about 3 KiB, in sectors of 512 bytes: the faults stopped
    // at each sector boundary inside it, at none of its bytes, and at all
    // of them, both ways.
    for torn in [true, false] {
        let stopped: Vec<u64> = (boundaries.iter())
            .filter(|(way, _)| *way == torn)
            .map(|(_, wrote)| *wrote)
            .collect();
        assert!(stopped.contains(&0), "torn {torn}: {stopped:?}");
        assert!(stopped.len() >= 6, "torn {torn}: {stopped:?}");
    }
    assert!(whole > 0, "no torn append left the whole group");
    assert!(failed > 100, "{failed} commits failed");
}

/// The sync that the policy asks of a commit fails: the commit is
/// published as `NotDurable`, a recovery has it whenever its bytes
/// survive, and no later commit of the session is durable either.
#[test]
fn a_commit_whose_sync_fails_is_recovered_whenever_its_bytes_survive() {
    for seed in 0..12 {
        let context = format!("seed {seed}");
        let session = Session::new(100 + seed, SyncPolicy::EveryCommit);
        session.commit();
        session.commit();
        session.check_recovery(&format!("{context}, before the fault"));

        session
            .fs
            .add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
        let (attempt, ended) = session.attempt();
        let Err(CommitError::NotDurable { published, .. }) = ended else {
            panic!("{context}: {ended:?}");
        };
        assert_eq!(published.version.get(), attempt.version);
        assert_eq!(session.versions.latest().get(), attempt.version);
        session.told.borrow_mut().happened.push(attempt);
        session.check_reads();
        session.check_recovery(&format!("{context}, not durable"));

        // A sync that failed cannot be made good in this session: the next
        // commit is published and not durable as well.
        let (attempt, ended) = session.attempt();
        assert!(
            matches!(ended, Err(CommitError::NotDurable { .. })),
            "{context}: {ended:?}"
        );
        session.told.borrow_mut().happened.push(attempt);
        session.check_reads();
        session.check_recovery(&format!("{context}, a second commit"));
        assert_eq!(session.told.borrow().durable, 2);
        assert_eq!(session.committer().stats().not_durable, 2);
    }
}

/// An append that was taken back, and a log that cannot be made durable
/// to say so: the engine cannot tell whether a crash brings the group
/// back. The commit is unsettled, nothing is published, and the session
/// takes no further commit (CM9 item 6).
#[test]
fn a_commit_that_cannot_be_settled_stops_the_session() {
    let mut came_back = 0;
    for seed in 0..40 {
        let context = format!("seed {seed}");
        let session = Session::new(200 + seed, SyncPolicy::Never);
        session.commit();
        session.commit();
        let before = session.versions.latest();

        let taken_back = FaultRule::new(Fault::Eio)
            .on(OpKind::Append)
            .torn_append(false);
        session.fs.add_rule(taken_back);
        session
            .fs
            .add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
        let (attempt, ended) = session.attempt();
        let Err(CommitError::Unsettled { version, .. }) = &ended else {
            panic!("{context}: {ended:?}");
        };
        assert_eq!(version.get(), attempt.version);
        session.told.borrow_mut().unsettled = Some(attempt);
        assert_eq!(
            session.versions.latest(),
            before,
            "{context}: nothing is published"
        );
        session.fs.clear_rules();
        session.check_reads();
        let stopped = session.committer().state().stopped();
        assert!(
            matches!(stopped, Some(Stopped::Unsettled { .. })),
            "{context}"
        );

        // A crash can bring the group back, as the last of the log, or
        // not: the next open settles it.
        session.check_recovery(&format!("{context}, unsettled"));
        came_back += session.came_back.get();

        // The session takes no further commit.
        let (_, ended) = session.attempt();
        assert!(
            matches!(ended, Err(CommitError::Stopped(_))),
            "{context}: {ended:?}"
        );
        session.attempts.set(session.attempts.get() - 1);
        let txn = session.committer().path().begin();
        let scope = txn.scope().expect("a scope");
        let batch = records(vec![put(aux(2), b"k", b"v")]);
        batch.with(|batch| scope.apply(batch)).expect("a write");
        scope.merge();
        let refused = session.committer().commit(&txn, &mut NoStaleHashes);
        assert!(
            matches!(refused, Err(CommitError::Stopped(_))),
            "{context}: {refused:?}"
        );
        assert_eq!(
            txn.stats().writes,
            1,
            "{context}: the transaction is as it was"
        );
        assert_eq!(session.versions.latest(), before);
    }
    assert!(
        came_back > 0,
        "in no state of a crash did the group come back"
    );
}

/// While the cut cannot be made, no commit is taken, and the transaction
/// is left as it was (CM9 item 5). It commits once the cut can be made.
#[test]
fn no_commit_is_taken_while_the_cut_cannot_be_made() {
    let session = Session::new(300, SyncPolicy::Never);
    session.commit();
    session.fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .on(OpKind::Append)
            .torn_append(true),
    );
    let (attempt, ended) = session.attempt();
    assert!(matches!(ended, Err(CommitError::Failed(_))), "{ended:?}");
    session.told.borrow_mut().failed.push(attempt.number);

    // The disk is still full: the manifest that would name the file closed
    // cannot be written. (A sync that failed would be another matter: the
    // log makes nothing durable after one, so its session makes no cut.)
    session
        .fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpSet::WRITES).forever());
    let committer = session.committer();
    let txn = committer.path().begin();
    let scope = txn.scope().expect("a scope");
    let number = session.attempts.get() + 1;
    let batch = records(vec![put(aux(1), &key(number), &value(number))]);
    batch.with(|batch| scope.apply(batch)).expect("a write");
    scope.merge();
    txn.set_savepoint().expect("a savepoint");
    let refused = committer
        .commit(&txn, &mut NoStaleHashes)
        .expect_err("no cut");
    assert!(matches!(refused, CommitError::NotReady(_)), "{refused}");
    assert!(refused.leaves_transaction_open());
    assert_eq!((txn.stats().writes, txn.savepoints()), (1, 1));
    assert_eq!(session.versions.latest().get(), 2);

    // There is room again: the same transaction commits, under the
    // version that the failed commit did not take.
    session.fs.clear_rules();
    let published = committer
        .commit(&txn, &mut NoStaleHashes)
        .expect("the cut is made");
    assert_eq!(
        published.version().map(|version| version.get()),
        Some(attempt.version)
    );
    drop(txn);
    session.attempts.set(number);
    session.told.borrow_mut().happened.push(Attempt {
        number,
        version: attempt.version,
    });
    session.check_reads();
    session.check_recovery("after the cut could be made");
}

/// A load whose commit is unsettled keeps the files of its values for as
/// long as the session lasts: its group names them, and a crash can bring
/// the group back (CM9). The store has the hold from then on, and it ends
/// with the session: nothing of the process outlives that. A load that is
/// rolled back holds none, and the file its values went to is taken out
/// of the store once it is closed.
#[test]
fn a_load_whose_commit_is_unsettled_keeps_its_values_for_the_session() {
    use crate::txn::tests::{
        load::{batch_of, items, whole},
        model,
        store::Rng,
    };

    // Whether the file that the values of a load went to is taken out of
    // the store once it is closed and nothing of the latest version points
    // into it, how many holds the store has for the session then, and
    // whether the file goes once those holds have ended, as they do with
    // the session.
    let goes = |unsettled: bool| {
        let session = Session::new(700, SyncPolicy::Never);
        session.commit();
        let committer = session.committer();
        let txn = committer.path().begin();
        let scope = txn.scope().expect("a scope");
        let merk = items(&mut Rng::new(3), b'u', 60, 2);
        let batch = batch_of(vec![whole(1, &merk)]);
        let loaded = batch.loading(&[model::subtree(1)], |batch| scope.apply(batch));
        assert!(loaded.expect("a load").load.is_some());
        scope.merge();
        let files = session.db.values.files_named().expect("the files");
        let [file] = &files[..] else {
            panic!("the values of the load are in one file: {files:?}");
        };
        let file = file.id();
        if unsettled {
            // A manifest covers the values while the files can be synced.
            txn.prepare_loads();
            let taken_back = FaultRule::new(Fault::Eio)
                .on(OpKind::Append)
                .torn_append(false);
            session.fs.add_rule(taken_back);
            session
                .fs
                .add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).forever());
            let ended = committer.commit(&txn, &mut NoStaleHashes);
            assert!(
                matches!(ended, Err(CommitError::Unsettled { .. })),
                "{ended:?}"
            );
            session.fs.clear_rules();
        } else {
            committer.rollback(&txn).expect("no scope is open");
        }
        let closed = session.db.load_files.close(file);
        assert!(closed.expect("the file is closed"), "the loads fill it");
        assert!(!session.db.load_files.close(file).expect("no file"));
        let gone = |session: &Session| {
            let values = &session.db.values;
            let retired = values.retire(session.versions.latest());
            let dropped = values.drop_empty(retired).expect("what is empty goes");
            dropped.contains(&file)
        };
        let went = gone(&session);
        let kept = session.db.load_files.end_kept();
        (went, kept, went || gone(&session))
    };
    assert_eq!(
        goes(false),
        (true, 0, true),
        "the file of a load that was rolled back"
    );
    assert_eq!(
        goes(true),
        (false, 1, true),
        "the file of a load whose commit is unsettled"
    );
}

/// A replacement of the manifest that fails in its rename leaves the
/// manifest in doubt (`lignin_store::segment`). The commit of a load that
/// meets this is refused with the transaction as it was. The next try
/// settles the manifest as write-back does, with no pass of write-back
/// between, and commits.
#[test]
fn the_commit_of_a_load_settles_a_manifest_that_was_left_in_doubt() {
    use crate::txn::tests::{
        load::{batch_of, items, whole},
        model,
        store::Rng,
    };

    let session = Session::new(701, SyncPolicy::Never);
    session.commit();
    let committer = session.committer();
    let txn = committer.path().begin();
    let scope = txn.scope().expect("a scope");
    let merk = items(&mut Rng::new(4), b'd', 60, 2);
    let long = merk.values().filter(|value| value.len() > 256).count();
    let batch = batch_of(vec![whole(1, &merk)]);
    let loaded = batch.loading(&[model::subtree(1)], |batch| scope.apply(batch));
    assert!(loaded.expect("a load").load.is_some());
    scope.merge();
    let in_doubt = || {
        let manifest = session.db.manifest.lock().expect("the manifest");
        manifest.manifest().is_err()
    };

    let renames = FaultRule::new(Fault::Eio).on(OpKind::Rename).forever();
    session.fs.add_rule(renames);
    for _ in 0..2 {
        let refused = committer.commit(&txn, &mut NoStaleHashes);
        assert!(
            matches!(&refused, Err(CommitError::Refused(TxnError::Values(_)))),
            "{refused:?}"
        );
        assert!(in_doubt(), "the replacement was left in doubt");
    }
    session.fs.clear_rules();
    let published = match committer.commit(&txn, &mut NoStaleHashes) {
        Ok(Committed::Published(published)) => published,
        other => panic!("the load is committed, and not {other:?}"),
    };
    assert_eq!(published.acquired, long);
    assert!(!in_doubt());
}

/// The first value of the loads of a store goes to a file that is created
/// for it and that a manifest names before it takes the value. Where the
/// manifest cannot be replaced, the load is refused, and every later try
/// finds the same file: no file is left behind for each of them.
#[test]
fn a_load_whose_file_cannot_be_named_creates_it_once() {
    use crate::txn::tests::{
        load::{batch_of, items, whole},
        model,
        store::Rng,
    };

    let session = Session::new(702, SyncPolicy::Never);
    session.commit();
    let committer = session.committer();
    let txn = committer.path().begin();
    let merk = items(&mut Rng::new(5), b'f', 60, 2);
    let long = merk.values().filter(|value| value.len() > 256).count();
    let batch = batch_of(vec![whole(1, &merk)]);
    let load = || {
        let scope = txn.scope().expect("a scope");
        let loaded = batch.loading(&[model::subtree(1)], |batch| scope.apply(batch));
        scope.merge();
        loaded
    };
    let files = || session.db.values.files_named().expect("the files").len();
    assert_eq!(files(), 0);

    let renames = FaultRule::new(Fault::Eio).on(OpKind::Rename).forever();
    session.fs.add_rule(renames);
    for _ in 0..3 {
        let refused = load().expect_err("no manifest names the file");
        assert!(matches!(refused, TxnError::Values(_)), "{refused}");
        assert_eq!(files(), 1, "the file of the first try");
    }
    session.fs.clear_rules();
    assert!(load().expect("a load").load.is_some());
    assert_eq!(files(), 1);
    let published = match committer.commit(&txn, &mut NoStaleHashes) {
        Ok(Committed::Published(published)) => published,
        other => panic!("the load is committed, and not {other:?}"),
    };
    assert_eq!(published.acquired, long);
}
