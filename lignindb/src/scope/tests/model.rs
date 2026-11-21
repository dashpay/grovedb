//! Scopes of one transaction under every interleaving of their operations
//! (`transactions.md` SC3, SC4, SC6, SC7; test T-SC1), each model written
//! once and run two ways.
//!
//! - **By enumeration**, in every normal test run: each order in which the
//!   operations of the scopes can come is made and run.
//! - **Under loom**, each scope on a thread of its own:
//!   `RUSTFLAGS="--cfg loom" cargo test -p lignindb --release --lib scope::tests::model`.
//!
//! A model is the claim table and the write set of a transaction, the two
//! structures its scopes share, under one lock, which is what a transaction
//! keeps them under: an operation of a scope is one hold of that lock. So
//! the orders of operations are all the interleavings there are. What a
//! model leaves out is the pages: laying a scope's writes out on a store,
//! under threads, is in `scope/tests/engine.rs`.
//!
//! What is compared with the scopes run one after another, in each order:
//! the records the transaction ends with, with the hashes of the nodes
//! among them, and everything each scope read, which is what its costs are
//! a function of (RD9).

#[cfg(loom)]
use ::loom as rt;
#[cfg(not(loom))]
use std as rt;
use std::{collections::BTreeMap, convert::Infallible};

use lignin_format::CommitVersion;
use lignin_tree::hash::Stale;
use rt::sync::{Arc, Mutex};

use crate::{
    scope::table::{Lineage, Refused, ScopeId, Scopes, Who},
    txn::{
        write_set::{Kind, Own, WriteSet},
        Batch, Column, Hosted, MerkWrite, NodePut, RecordWrite, RootPut, Subtree, Unit, ValuePut,
    },
};

/// No subtree of a model is below another.
struct Flat;

impl Lineage for Flat {
    type Error = Infallible;

    fn below(&mut self, _: &Subtree, _: &Subtree) -> Result<bool, Infallible> {
        Ok(false)
    }
}

fn unit(name: u8) -> Unit {
    Unit::new(Column::Aux, Subtree::new([name; 32]))
}

fn merk(name: u8) -> Subtree {
    Subtree::new([name; 32])
}

/// One operation of a scope, or of a reader outside any scope.
#[derive(Debug, Clone, Copy)]
enum Step {
    Open,
    /// Reads the record or the node under the key.
    Read(Unit, &'static [u8]),
    /// Puts a flat record.
    Put(Unit, &'static [u8], &'static [u8]),
    /// Puts a node of the Merk of that name, with a hash of that byte.
    Node(u8, &'static [u8], u8),
    /// Asks how many operations the write set has: of two scripts that
    /// ask after their writes, the one that is told more wrote later.
    Count,
    Merge,
    Rollback,
}

/// What a read returned, or that a call was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Nothing,
    Record(Vec<u8>),
    /// The key-value hash of a node.
    Node(Vec<u8>),
    Refused(Unit),
    /// The operations of the write set.
    Count(usize),
}

struct Model {
    scopes: Scopes,
    set: WriteSet,
}

/// The version every write of a model is dated with.
fn version() -> CommitVersion {
    CommitVersion::new(1).expect("a version")
}

impl Model {
    /// A transaction whose merged writes are `merged`, with no scope open.
    fn new(merged: &[(Unit, &[u8], &[u8])]) -> Self {
        let mut model = Self {
            scopes: Scopes::default(),
            set: WriteSet::new(),
        };
        for (unit, key, value) in merged {
            model.put(0, true, *unit, key, value);
        }
        model
    }

    fn put(&mut self, scope: ScopeId, laid: bool, unit: Unit, key: &[u8], value: &[u8]) {
        let records = [RecordWrite::Put { unit, key, value }];
        let batch = Batch {
            records: &records,
            ..Batch::default()
        };
        self.set.add(&batch, version(), &[], &[], &[]);
        self.set.tag_last(scope, laid);
    }

    fn read(&self, unit: Unit, key: &[u8]) -> Seen {
        match self.set.own(unit, key) {
            Own::Unwritten | Own::Deleted => Seen::Nothing,
            Own::Put(value) => Seen::Record(value.to_vec()),
            Own::Node(id) => match self.set.entry(id).kind {
                Kind::Node(node) => Seen::Node(self.set.bytes_at(node.kv_hash).to_vec()),
                _ => unreachable!("a node"),
            },
        }
    }

    /// One operation of `who`, as a transaction makes it under its lock:
    /// the claim first, and the read or the write if it is granted.
    fn step(&mut self, who: &mut Option<ScopeId>, step: Step) -> Option<Seen> {
        let reader = who.map_or(Who::OUTSIDE, Who::scope);
        let refused = |refused| match refused {
            Refused::Overlap(unit) => Seen::Refused(unit),
            Refused::Lineage(never) => match never {},
        };
        let seen = match step {
            Step::Open => {
                *who = Some(self.scopes.open());
                None
            }
            Step::Read(unit, key) => Some(match self.scopes.read(reader, unit, &mut Flat) {
                Ok(()) => self.read(unit, key),
                Err(error) => refused(error),
            }),
            Step::Put(unit, key, value) => {
                let id = who.expect("a scope writes");
                match self.scopes.write(id, &[unit], &[], &[], &mut Flat) {
                    Ok(()) => {
                        let laid = self.scopes.laid(id);
                        self.put(id, laid, unit, key, value);
                        None
                    }
                    Err(error) => Some(refused(error)),
                }
            }
            Step::Node(name, key, hash) => {
                let id = who.expect("a scope writes");
                let subtree = merk(name);
                match (self.scopes).write(id, &[Unit::data(subtree)], &[], &[], &mut Flat) {
                    Ok(()) => {
                        let hash = [hash; 32];
                        let nodes = [NodePut {
                            key,
                            feature: b"f",
                            value_hash: &hash,
                            kv_hash: &hash,
                            left: None,
                            right: None,
                            hosted: Hosted::Keep,
                            value: ValuePut::Value(b"value"),
                            stale: Stale::NONE,
                            at: None,
                        }];
                        let merks = [MerkWrite {
                            subtree,
                            nodes: &nodes,
                            removed: &[],
                            root: RootPut::Set(Some(key)),
                            anchored: false,
                        }];
                        let batch = Batch {
                            merks: &merks,
                            ..Batch::default()
                        };
                        let empty = self.set.rooted(&subtree) != Some(true);
                        self.set.add(&batch, version(), &[empty], &[], &[]);
                        self.set.tag_last(id, self.scopes.laid(id));
                        None
                    }
                    Err(error) => Some(refused(error)),
                }
            }
            Step::Count => Some(Seen::Count(self.set.ops())),
            Step::Merge => {
                let id = who.take().expect("a scope merges");
                let closed = self.scopes.close(id).expect("it is open");
                self.set.merge_scope(id);
                self.scopes.merge_marks(closed.marks);
                None
            }
            Step::Rollback => {
                let id = who.take().expect("a scope rolls back");
                self.scopes.close(id).expect("it is open");
                self.set.remove_scope(id);
                None
            }
        };
        self.scopes.check();
        seen
    }

    /// Every record the transaction has under `keys`, as a caller outside
    /// any scope reads them once every scope has ended.
    fn records(&self, keys: &[(Unit, &'static [u8])]) -> BTreeMap<(Unit, Vec<u8>), Seen> {
        assert!(!self.scopes.any_open(), "every scope has ended");
        (keys.iter())
            .map(|(unit, key)| ((*unit, key.to_vec()), self.read(*unit, key)))
            .collect()
    }
}

/// What a run left: the records, and what each script saw, step by step.
type Outcome = (BTreeMap<(Unit, Vec<u8>), Seen>, Vec<Vec<Seen>>);

/// A model: what is merged before any scope opens, the script of each
/// scope or reader, and the keys whose records are compared at the end.
#[derive(Clone)]
struct Scripts {
    merged: Vec<(Unit, &'static [u8], &'static [u8])>,
    scripts: Vec<Vec<Step>>,
    keys: Vec<(Unit, &'static [u8])>,
}

impl Scripts {
    /// Runs the steps in the order `order` gives: each element is the
    /// script whose next step comes.
    fn run(&self, order: &[usize]) -> Outcome {
        let mut model = Model::new(&self.merged);
        let mut who = vec![None; self.scripts.len()];
        let mut at = vec![0; self.scripts.len()];
        let mut seen = vec![Vec::new(); self.scripts.len()];
        for script in order {
            let step = self.scripts[*script][at[*script]];
            at[*script] += 1;
            seen[*script].extend(model.step(&mut who[*script], step));
        }
        (model.records(&self.keys), seen)
    }

    /// The scripts one after another, in the order `first` gives.
    fn serial(&self, first: &[usize]) -> Outcome {
        let order: Vec<usize> = (first.iter())
            .flat_map(|script| vec![*script; self.scripts[*script].len()])
            .collect();
        self.run(&order)
    }

    /// Every order in which the steps of the scripts can come.
    #[cfg(not(loom))]
    fn orders(&self) -> Vec<Vec<usize>> {
        fn extend(left: &mut [usize], order: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            if left.iter().all(|left| *left == 0) {
                out.push(order.clone());
                return;
            }
            for script in 0..left.len() {
                if left[script] > 0 {
                    left[script] -= 1;
                    order.push(script);
                    extend(left, order, out);
                    order.pop();
                    left[script] += 1;
                }
            }
        }
        let mut left: Vec<usize> = self.scripts.iter().map(Vec::len).collect();
        let mut out = Vec::new();
        extend(&mut left, &mut Vec::new(), &mut out);
        out
    }

    /// Runs the scripts on threads of their own, each step under the lock,
    /// and returns what the run left.
    fn threaded(&self) -> Outcome {
        let model = Arc::new(Mutex::new(Model::new(&self.merged)));
        let threads: Vec<_> = (self.scripts.iter().cloned())
            .map(|script| {
                let model = Arc::clone(&model);
                rt::thread::spawn(move || {
                    let (mut who, mut seen) = (None, Vec::new());
                    for step in script {
                        let mut model = model.lock().expect("no step panics");
                        seen.extend(model.step(&mut who, step));
                    }
                    seen
                })
            })
            .collect();
        let seen = (threads.into_iter())
            .map(|thread| thread.join().expect("a script"))
            .collect();
        let model = model.lock().expect("no step panics");
        (model.records(&self.keys), seen)
    }
}

/// Runs `check` on what every interleaving of `scripts` leaves: under loom
/// each one that loom explores, and otherwise each order of the steps.
fn every_interleaving(scripts: Scripts, check: impl Fn(&Outcome) + Send + Sync + 'static) {
    #[cfg(loom)]
    {
        let mut builder = ::loom::model::Builder::new();
        builder.max_branches = 100_000;
        builder.check(move || check(&scripts.threaded()));
    }
    #[cfg(not(loom))]
    {
        let orders = scripts.orders();
        assert!(orders.len() > 1, "more than one order");
        for order in orders {
            check(&scripts.run(&order));
        }
        // And once on real threads, as the transaction is used.
        check(&scripts.threaded());
    }
}

const SHARED: &[u8] = b"shared";

/// Two scopes that write units of their own and read one both read.
fn disjoint(second_ends: Step) -> Scripts {
    let (a, b, both) = (unit(1), unit(2), unit(3));
    Scripts {
        merged: vec![(both, SHARED, b"merged"), (a, b"x", b"before")],
        scripts: vec![
            vec![
                Step::Open,
                Step::Read(both, SHARED),
                Step::Read(a, b"x"),
                Step::Put(a, b"x", b"a1"),
                Step::Node(11, b"n", 0xa1),
                Step::Read(a, b"x"),
                Step::Merge,
            ],
            vec![
                Step::Open,
                Step::Read(both, SHARED),
                Step::Put(b, b"y", b"b1"),
                Step::Node(12, b"n", 0xb1),
                Step::Read(Unit::data(merk(12)), b"n"),
                Step::Put(b, b"y", b"b2"),
                second_ends,
            ],
        ],
        keys: vec![
            (both, SHARED),
            (a, b"x"),
            (b, b"y"),
            (Unit::data(merk(11)), b"n"),
            (Unit::data(merk(12)), b"n"),
        ],
    }
}

/// T-SC1: two scopes on disjoint units, in every interleaving, leave the
/// records and the hashes of the scopes run one after another, in either
/// order, and each has read what it reads then (SC7).
#[test]
fn scopes_on_disjoint_units_give_what_they_give_one_after_another() {
    let scripts = disjoint(Step::Merge);
    let serial = scripts.serial(&[0, 1]);
    assert_eq!(serial, scripts.serial(&[1, 0]), "the order does not matter");
    let (records, seen) = &serial;
    assert_eq!(
        records[&(unit(1), b"x".to_vec())],
        Seen::Record(b"a1".to_vec())
    );
    assert_eq!(
        records[&(unit(2), b"y".to_vec())],
        Seen::Record(b"b2".to_vec())
    );
    let node = Unit::data(merk(12));
    assert_eq!(records[&(node, b"n".to_vec())], Seen::Node(vec![0xb1; 32]));
    // A scope reads its own writes over what was merged before it (SC4).
    let before = Seen::Record(b"before".to_vec());
    let merged = Seen::Record(b"merged".to_vec());
    let own = Seen::Record(b"a1".to_vec());
    assert_eq!(seen[0], vec![merged.clone(), before, own]);
    assert_eq!(seen[1], vec![merged, Seen::Node(vec![0xb1; 32])]);
    every_interleaving(scripts, move |outcome| assert_eq!(*outcome, serial));
}

/// T-SC3, without the pages: a scope that rolls back beside one that
/// merges leaves exactly the merging scope's writes, in every interleaving
/// (SC6).
#[test]
fn a_rollback_beside_a_merge_leaves_the_merged_writes() {
    let scripts = disjoint(Step::Rollback);
    let serial = scripts.serial(&[0, 1]);
    assert_eq!(serial, scripts.serial(&[1, 0]), "the order does not matter");
    let (records, _) = &serial;
    assert_eq!(records[&(unit(2), b"y".to_vec())], Seen::Nothing);
    assert_eq!(
        records[&(Unit::data(merk(12)), b"n".to_vec())],
        Seen::Nothing
    );
    assert_eq!(
        records[&(unit(1), b"x".to_vec())],
        Seen::Record(b"a1".to_vec())
    );
    every_interleaving(scripts, move |outcome| assert_eq!(*outcome, serial));
}

/// SC3, SC4: a reader outside any scope, beside a scope that writes a key
/// twice and merges, reads the key as it was before the scope or as the
/// scope left it, or is refused. It never reads the write the scope made
/// in between, and never waits.
#[test]
fn nobody_reads_a_write_of_an_open_scope() {
    let a = unit(1);
    let scripts = Scripts {
        merged: vec![(a, b"x", b"before")],
        scripts: vec![
            vec![
                Step::Open,
                Step::Put(a, b"x", b"between"),
                Step::Put(a, b"x", b"after"),
                Step::Merge,
            ],
            vec![Step::Read(a, b"x"), Step::Read(a, b"x")],
        ],
        keys: vec![(a, b"x")],
    };
    every_interleaving(scripts, move |(records, seen)| {
        assert_eq!(
            records[&(a, b"x".to_vec())],
            Seen::Record(b"after".to_vec())
        );
        let allowed = [
            Seen::Record(b"before".to_vec()),
            Seen::Record(b"after".to_vec()),
            Seen::Refused(a),
        ];
        assert!(
            seen[1].iter().all(|seen| allowed.contains(seen)),
            "{seen:?}"
        );
        // What is merged is not unmerged: the reads do not go back.
        let after = Seen::Record(b"after".to_vec());
        assert!(seen[1][0] != after || seen[1][1] == after, "{seen:?}");
    });
}

/// T-SC2, in every interleaving: of two scopes that write one unit, each
/// write is granted or refused at once, the two never both hold the unit,
/// and what the transaction ends with is the writes of the scopes whose
/// writes were all granted, one after another.
#[test]
fn of_two_scopes_on_one_unit_one_holds_it_at_a_time() {
    let a = unit(1);
    let scripts = Scripts {
        merged: Vec::new(),
        scripts: vec![
            vec![
                Step::Open,
                Step::Put(a, b"x", b"first"),
                Step::Count,
                Step::Merge,
            ],
            vec![
                Step::Open,
                Step::Put(a, b"x", b"second"),
                Step::Count,
                Step::Merge,
            ],
        ],
        keys: vec![(a, b"x")],
    };
    every_interleaving(scripts, move |(records, seen)| {
        let refused = |scope: usize| seen[scope].contains(&Seen::Refused(a));
        let count = |scope: usize| seen[scope].last().cloned();
        let expected: &[u8] = match (refused(0), refused(1)) {
            (true, true) => unreachable!("one of the two came first"),
            (true, false) => b"second",
            (false, true) => b"first",
            // Neither was refused: one had merged before the other wrote,
            // and the later write stands. The write set had the merged
            // write when the later one was made.
            (false, false) => match (count(0), count(1)) {
                (Some(Seen::Count(1)), Some(Seen::Count(2))) => b"second",
                (Some(Seen::Count(2)), Some(Seen::Count(1))) => b"first",
                other => panic!("{other:?}"),
            },
        };
        assert_eq!(
            records[&(a, b"x".to_vec())],
            Seen::Record(expected.to_vec())
        );
    });
}
