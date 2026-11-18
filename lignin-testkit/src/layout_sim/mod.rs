//! Simulation of the Merkle host layout (TREE-01).
//!
//! `docs/lignindb/tree-layout.md` specifies how the nodes of Merk trees are
//! clustered into pages. This module checks those rules against what Merk
//! really does, before any engine code exists:
//!
//! 1. A workload from [`crate::workload`] is applied to GroveDB on RocksDB.
//! 2. After each state transition [`observe::Observer`] reads the node
//!    records Merk wrote and brings a mirror of the grove, [`tree::Grove`],
//!    up to date. The result is a [`tree::ChangeSet`]: the nodes created,
//!    changed and removed, with their links before and after.
//! 3. Each [`model::Model`] applies the layout rules to that change set and
//!    counts what an engine would have had to write.
//!
//! [`simulate`] does all three and returns a [`report::Report`] per layout.

pub mod model;
pub mod observe;
pub mod report;
pub mod tree;

use grovedb::{GroveDb, Transaction};
use grovedb_version::version::GroveVersion;

pub use self::{
    model::{CellCosts, Config, Model},
    observe::Observer,
    report::{Report, Sample, Stats},
    tree::{ChangeSet, Event, Grove, MerkId, NodeId},
};
use crate::workload::{apply, Op};

/// How a simulation is run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// A sample of every layout is taken after this many transitions.
    pub sample_every: u64,
    /// Every this many transitions the bookkeeping of every layout is
    /// recounted from the grove, and the run panics if it is off. Zero
    /// turns the check off.
    pub check_every: u64,
    /// Whether the mirror is compared with a scan of storage at the end of
    /// every block, with a panic if they differ. It is compared at every
    /// sample, after every flush of pending drops and at the end of the run
    /// in any case. A comparison reads every record, so comparing at every
    /// block is for small workloads.
    pub verify_mirror: bool,
}

/// Applies the writes of `ops` to `db`, which must be empty, and lays the
/// resulting trees out once per entry of `configs`.
///
/// A transition is one direct write or one batch, whole or in segments,
/// that changes a Merk node. A write that changes none, such as an aux
/// record, is applied and not counted. A block ends at each [`Op::Commit`],
/// where the transaction is committed. Reads are skipped: they change no
/// node. `progress` is called after every sample with the number of
/// transitions so far.
///
/// At the end of the run the mirror is compared with a scan of storage and
/// every layout is recounted from the mirror. A run that does not end with
/// a commit is committed, and its last block ends like any other.
///
/// Panics if GroveDB refuses an operation, if the mirror or a layout is
/// found wrong, or if reclaiming dropped storage changes a Merk node.
pub fn simulate(
    name: &str,
    db: &GroveDb,
    ops: impl Iterator<Item = Op>,
    grove_version: &'static GroveVersion,
    configs: &[Config],
    run: Run,
    mut progress: impl FnMut(u64, &Observer, &[Model]),
) -> Vec<Report> {
    let mut observer = Observer::new(grove_version);
    let mut models: Vec<Model> = configs.iter().copied().map(Model::new).collect();
    let mut transaction = db.start_transaction();
    let mut transitions = 0u64;
    let mut open_block = false;
    let verify = |observer: &Observer, transaction: &Transaction, transitions: u64| {
        if let Err(problem) = observer.verify(db, transaction) {
            panic!("{name} after {transitions} transitions: {problem}");
        }
    };
    for op in ops {
        match op {
            Op::Read(_) => {}
            Op::Commit => {
                if run.verify_mirror {
                    verify(&observer, &transaction, transitions);
                }
                db.commit_transaction(transaction)
                    .unwrap()
                    .expect("a block commits");
                transaction = db.start_transaction();
                for model in &mut models {
                    model.end_block();
                }
                open_block = false;
            }
            Op::FlushPendingDrops => {
                apply(db, &op, Some(&transaction), grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{name}: {op:?} refused: {error:?}"));
                assert!(
                    observer.observe(db, &transaction).is_empty(),
                    "{name} after {transitions} transitions: a flush changed the root of the grove"
                );
                verify(&observer, &transaction, transitions);
            }
            Op::Write(_) | Op::Batch(_) | Op::PartialBatch(_) => {
                apply(db, &op, Some(&transaction), grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{name}: {op:?} refused: {error:?}"));
                let changes = observer.observe(db, &transaction);
                if changes.is_empty() {
                    continue;
                }
                transitions += 1;
                open_block = true;
                for model in &mut models {
                    model.apply(observer.grove(), &changes);
                }
                if run.check_every > 0 && transitions.is_multiple_of(run.check_every) {
                    for model in &models {
                        if let Err(problem) = model.check(observer.grove()) {
                            panic!("{name} after {transitions} transitions: {problem}");
                        }
                    }
                }
                if run.sample_every > 0 && transitions.is_multiple_of(run.sample_every) {
                    verify(&observer, &transaction, transitions);
                    for model in &mut models {
                        model.sample();
                    }
                    progress(transitions, &observer, &models);
                }
            }
        }
    }
    verify(&observer, &transaction, transitions);
    db.commit_transaction(transaction)
        .unwrap()
        .expect("the last block commits");
    for model in &mut models {
        if open_block {
            model.end_block();
        }
        if let Err(problem) = model.check(observer.grove()) {
            panic!("{name} at the end: {problem}");
        }
    }
    models
        .iter()
        .map(|model| Report::new(name, model))
        .collect()
}
