//! T-CF3 (`transactions.md` 15.2): random schedules of puts, deletes,
//! range deletes, savepoints, rollbacks and commits over several write
//! transactions, with writes made with no transaction and turns of
//! write-back between them, through the commit path of a store on disk.
//!
//! The schedules of `txn::tests::random` are checked against a model that
//! marks each write of a transaction when a commit meets it. Here the
//! outcome of every commit is decided a second way, by CF2 as it is
//! written, from the history of the commits: the commit of a transaction
//! fails when a commit with a later version than the one at which the
//! transaction first wrote a key wrote that key. Every outcome agrees with
//! both, in both directions: no conflict is missed and none is made up.

use lignin_store::flush::Checkpointed;

use super::writes_of;
use crate::{
    commit::CommitError,
    savepoint::SavepointError,
    txn::{
        tests::{
            model::{self, Write},
            random::batch,
            store::{Db, Rng},
            Tx, World,
        },
        Column, KeySet, Unit,
    },
};

/// A transaction of a schedule, with the count of its writes at each
/// savepoint it has set.
struct Open<'s> {
    tx: Tx<'s>,
    savepoints: Vec<usize>,
}

/// The commits of a schedule: the version of each and the keys it wrote.
type History = Vec<(u64, Vec<Write>)>;

/// CF2 as it is written.
fn fails(history: &History, tx: &Tx<'_>) -> bool {
    let model = tx.model.borrow();
    model.writes.iter().any(|(own, first_write, _)| {
        (history.iter()).any(|(version, writes)| {
            version > first_write && writes.iter().any(|write| write.meets(own))
        })
    })
}

/// Whether a commit was made after `tx` first wrote: its commit is one
/// that the rule looks at.
fn had_company(history: &History, tx: &Tx<'_>) -> bool {
    let model = tx.model.borrow();
    let first = model
        .writes
        .iter()
        .map(|(_, first_write, _)| *first_write)
        .min();
    first.is_some_and(|first| history.iter().any(|(version, _)| *version > first))
}

/// What a schedule did.
#[derive(Debug, Default)]
struct Tally {
    committed: usize,
    /// Commits that were made although another commit was made after the
    /// transaction's first write: their keys did not meet.
    beside: usize,
    conflicts: usize,
    empty: usize,
    outside: usize,
    /// Commits whose key set had a range beside the keys they wrote (CF5).
    covering: usize,
    rollbacks: usize,
    to_savepoint: usize,
    /// Rollbacks to a savepoint that took out every write a commit had
    /// met: the transaction's commit would have failed, and no longer does
    /// (CF4).
    saved: usize,
    no_savepoint: usize,
    passes: usize,
    checkpoints: usize,
}

impl Tally {
    fn add(&mut self, other: &Self) {
        self.committed += other.committed;
        self.beside += other.beside;
        self.conflicts += other.conflicts;
        self.empty += other.empty;
        self.outside += other.outside;
        self.covering += other.covering;
        self.rollbacks += other.rollbacks;
        self.to_savepoint += other.to_savepoint;
        self.saved += other.saved;
        self.no_savepoint += other.no_savepoint;
        self.passes += other.passes;
        self.checkpoints += other.checkpoints;
    }
}

fn run(seed: u64, steps: usize, keys: usize) -> Tally {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let committer = db.committer(&versions);
    let write_back = db.write_back(&versions);
    let mut rng = Rng::new(seed);
    let mut open: Vec<Open<'_>> = Vec::new();
    let mut history = History::new();
    let mut tally = Tally::default();
    let at = |what: &str, step: usize| format!("seed {seed}, step {step}: {what}");
    let pick =
        |rng: &mut Rng, open: &Vec<Open<'_>>| (!open.is_empty()).then(|| rng.below(open.len()));
    // A commit of `writes` was published: the models and the history are
    // told.
    let published = |world: &mut World<'_>,
                     history: &mut History,
                     open: &Vec<Open<'_>>,
                     mut writes: Vec<Write>,
                     covered: Option<Write>| {
        let version = world.model.commit(&writes);
        writes.extend(covered);
        for other in open {
            other.tx.model.borrow_mut().foreign(version, &writes);
        }
        history.push((version, writes));
        version
    };

    for step in 0..steps {
        match rng.below(100) {
            0..=9 if open.len() < 4 => open.push(Open {
                tx: world.begin(),
                savepoints: Vec::new(),
            }),
            0..=39 => {
                let Some(this) = pick(&mut rng, &open).map(|at| &open[at]) else {
                    continue;
                };
                let batch = batch(&world, &this.tx, &mut rng, keys);
                (world.apply(&this.tx, &batch))
                    .unwrap_or_else(|error| panic!("{}", at(&error.to_string(), step)));
                world.check(&this.tx);
            }
            40..=47 => {
                let Some(this) = pick(&mut rng, &open).map(|at| &mut open[at]) else {
                    continue;
                };
                this.tx.txn.set_savepoint().expect("no scope is open");
                this.savepoints.push(this.tx.model.borrow().writes.len());
            }
            48..=56 => {
                let Some(this) = pick(&mut rng, &open).map(|at| &mut open[at]) else {
                    continue;
                };
                let failed = fails(&history, &this.tx);
                let went = this.tx.txn.rollback_to_savepoint();
                let Some(writes) = this.savepoints.pop() else {
                    assert_eq!(
                        went,
                        Err(SavepointError::NoSavepoint),
                        "{}",
                        at("A17", step)
                    );
                    tally.no_savepoint += 1;
                    world.check(&this.tx);
                    continue;
                };
                assert_eq!(went, Ok(()));
                this.tx.model.borrow_mut().writes.truncate(writes);
                tally.to_savepoint += 1;
                tally.saved += usize::from(failed && !fails(&history, &this.tx));
                world.check(&this.tx);
            }
            57..=59 => {
                let Some(this) = pick(&mut rng, &open).map(|at| &mut open[at]) else {
                    continue;
                };
                let went = this.tx.txn.pop_savepoint();
                assert_eq!(went.is_ok(), this.savepoints.pop().is_some());
            }
            60..=71 => {
                // A write with no transaction (RD7), as a reader of the
                // committed state would make it. Now and then its commit
                // covers a range beside what it writes, as a load does.
                let reader = world.begin();
                let batch = batch(&world, &reader, &mut rng, keys);
                drop(reader);
                let covered = rng.chance(12).then(|| {
                    let unit =
                        [model::unit(Column::Aux, 0), Unit::data(model::subtree(2))][rng.below(2)];
                    let start = format!("k{:03}", rng.below(keys)).into_bytes();
                    let end = format!("k{:03}", rng.below(keys)).into_bytes();
                    Write::Range(unit, start.clone().min(end.clone()), Some(start.max(end)))
                });
                let before = versions.latest();
                let committed = match &covered {
                    None => batch.with(|batch| db.write_as(&committer, batch)),
                    Some(Write::Range(unit, start, end)) => {
                        let tx = world.path.begin();
                        let scope = tx.scope().expect("a scope");
                        (batch.with(|batch| scope.apply(batch))).expect("a write");
                        scope.merge();
                        let mut also = KeySet::new();
                        also.range(*unit, start, end.as_deref());
                        db.commit_as(&committer, &tx, Some(&also))
                    }
                    Some(_) => unreachable!("a range"),
                };
                let made = committed
                    .unwrap_or_else(|error| panic!("{}", at(&error.to_string(), step)))
                    .unwrap_or_else(|| panic!("{}", at("a batch of the schedule writes", step)));
                assert_eq!(Some(made.version), before.next());
                tally.covering += usize::from(covered.is_some());
                let version = published(&mut world, &mut history, &open, batch.writes(), covered);
                assert_eq!(version, made.version.get(), "{}", at("the version", step));
                tally.outside += 1;
                world.check_committed();
            }
            72..=83 => {
                let Some(at_open) = pick(&mut rng, &open) else {
                    continue;
                };
                let done = open.swap_remove(at_open);
                let writes = writes_of(&done.tx);
                let expected = fails(&history, &done.tx);
                assert_eq!(
                    expected,
                    done.tx.model.borrow().conflicts(),
                    "{}",
                    at("the two models", step)
                );
                let outcome = db.commit_as(&committer, &done.tx.txn, None);
                match outcome {
                    Ok(None) => {
                        assert!(writes.is_empty(), "{}", at("an empty commit", step));
                        tally.empty += 1;
                    }
                    Ok(Some(made)) => {
                        assert!(
                            !writes.is_empty() && !expected,
                            "{}",
                            at("a missed conflict", step)
                        );
                        tally.beside += usize::from(had_company(&history, &done.tx));
                        let version = published(&mut world, &mut history, &open, writes, None);
                        assert_eq!(version, made.version.get());
                        tally.committed += 1;
                    }
                    Err(CommitError::Conflict) => {
                        assert!(
                            !writes.is_empty() && expected,
                            "{}",
                            at("a conflict too many", step)
                        );
                        tally.conflicts += 1;
                    }
                    Err(error) => panic!("{}", at(&error.to_string(), step)),
                }
                // The transaction ended, whatever the outcome: it holds
                // nothing, and its savepoints went with it.
                assert_eq!(done.tx.txn.savepoints(), 0);
                assert_eq!(done.tx.txn.stats().writes, 0);
                world.check_committed();
            }
            84..=87 => {
                let Some(this) = pick(&mut rng, &open).map(|at| &mut open[at]) else {
                    continue;
                };
                committer.rollback(&this.tx.txn).expect("no scope is open");
                this.tx.forget();
                this.savepoints.clear();
                assert_eq!(this.tx.txn.savepoints(), 0);
                tally.rollbacks += 1;
                world.check(&this.tx);
            }
            88..=94 => {
                // Write-back takes a turn: pages go to segments and values
                // to the value log, under the transactions that are open.
                if rng.chance(30) {
                    let taken = write_back.checkpoint().expect("a checkpoint");
                    tally.checkpoints +=
                        usize::from(matches!(taken, Checkpointed::Installed { .. }));
                } else {
                    write_back.pass().expect("a pass");
                    tally.passes += 1;
                }
                for this in &open {
                    world.check(&this.tx);
                }
            }
            _ => {
                for this in &open {
                    world.check(&this.tx);
                }
            }
        }
    }
    for this in &open {
        world.check(&this.tx);
    }
    drop(open);
    world.check_committed();
    assert_eq!(
        db.private_ids(),
        0,
        "seed {seed}: no id is left with a transaction"
    );
    assert_eq!(db.pool.census().private, 0, "seed {seed}: no copy is left");
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
    let stats = committer.stats();
    assert_eq!(stats.published as usize, tally.committed + tally.outside);
    assert_eq!(stats.conflicts as usize, tally.conflicts);
    assert_eq!(stats.empty as usize, tally.empty);
    assert_eq!(stats.rollbacks as usize, tally.rollbacks);
    assert_eq!((stats.failed, stats.not_durable), (0, 0));
    assert_eq!(
        versions.latest().get() as usize,
        1 + tally.committed + tally.outside
    );
    tally
}

fn check(total: &Tally) {
    assert!(total.committed > 40 && total.outside > 40, "{total:?}");
    assert!(total.conflicts > 10, "no conflict to miss: {total:?}");
    assert!(total.beside > 10, "no conflict to make up: {total:?}");
    assert!(
        total.saved > 0,
        "no rollback to a savepoint took a conflict out: {total:?}"
    );
    assert!(
        total.to_savepoint > 10 && total.no_savepoint > 0,
        "{total:?}"
    );
    assert!(total.rollbacks > 5 && total.covering > 0, "{total:?}");
    assert!(total.passes > 10 && total.checkpoints > 0, "{total:?}");
}

#[test]
fn every_commit_outcome_is_the_one_cf2_gives() {
    let mut total = Tally::default();
    for seed in 0..6 {
        total.add(&run(0xcf30 + seed, 600, 40));
    }
    check(&total);
}

#[test]
fn it_is_where_few_keys_make_most_commits_meet() {
    let mut total = Tally::default();
    for seed in 0..4 {
        total.add(&run(0xcf3f + seed, 500, 9));
    }
    check(&total);
}
