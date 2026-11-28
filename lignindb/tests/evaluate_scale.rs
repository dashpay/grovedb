//! An evaluation before the commit through the engine as its users open
//! it ([`Db`], with its background threads), and what many calls in one
//! transaction cost (`lignindb::txn`, "An evaluation before the commit").
//!
//! This crate's own tests hold what a transaction keeps to walks over
//! all of it, at every call, so a call there costs what the transaction
//! holds. These tests are built against the crate as a user has it.
//!
//! Two shapes of one transaction that changes a Merk and asks for its
//! root hash after every change:
//!
//! - **one Merk**: a Merk of 1,023 nodes, ten levels deep, of which each
//!   change gives a leaf another value and leaves the hash of the child
//!   on the way to it stale in each of its nine ancestors;
//! - **many Merks**: each change enters a subtree and gives it three
//!   nodes, with the hashes of both children of the root left stale.
//!
//! Each is run twice: with the call after every change, and with no call,
//! so that the commit makes every hash. The two publish the same root
//! hashes, which are the ones a model makes with the same rules, and the
//! check of the database at its close makes every stored hash again with
//! those rules, in a debug build.

use std::time::{Duration, Instant};

use lignin_format::tree_page::{Hash, HashField, Side};
use lignin_tree::{
    hash::{HashRules, Refusal, Stale},
    node::NodeRef,
};
use lignindb::{
    commit::{Committed, NoStaleHashes},
    txn::{
        Batch, Hosted, LinkPut, MerkWrite, NodePut, RootPut, Scope, Subtree, SubtreeWrite, ValuePut,
    },
    Db, Options,
};

const NONE: Hash = [0; 32];

/// Bytes that are no hash: what a writer states for a hash it leaves.
const UNMADE: Hash = [0xee; 32];

/// The feature of every node of these tests, and the aggregate of every
/// link: bytes that the engine keeps as they are.
const FEATURE: [u8; 2] = [3, 1];

fn hash_of(parts: &[&[u8]]) -> Hash {
    let mut state = 0xcbf2_9ce4_8422_2325u64;
    for part in parts {
        for byte in part.iter().chain(&(part.len() as u64).to_le_bytes()) {
            state = (state ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let mut hash = [0; 32];
    for (index, chunk) in hash.chunks_mut(8).enumerate() {
        state = (state ^ (state >> 29) ^ index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    hash
}

/// The rules of these tests, with a count of their calls.
#[derive(Default)]
struct Rules {
    calls: u64,
}

impl HashRules for Rules {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        _: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.calls += 1;
        Ok(hash_of(&[value]))
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.calls += 1;
        Ok(hash_of(&[key, value_hash]))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.calls += 1;
        let child = |side| node.link(side).map_or(&NONE, |link| link.hash);
        Ok(hash_of(&[
            node.kv_hash(),
            child(Side::Left),
            child(Side::Right),
        ]))
    }
}

fn key(index: usize) -> [u8; 4] {
    (index as u32).to_be_bytes()
}

/// A link as a write states it: the key of the child, its hash, and the
/// heights below it.
type Link = ([u8; 4], Hash, (u8, u8));

/// A Merk of `2^levels - 1` nodes in the shape of a full tree, whose keys
/// are their places in key order, with the hashes the rules make.
struct Model {
    values: Vec<Vec<u8>>,
    /// The hash of each node, by its place.
    hashes: Vec<Hash>,
}

impl Model {
    fn new(levels: u32, salt: u8) -> Self {
        let nodes = (1usize << levels) - 1;
        let values = (0..nodes).map(|at| vec![salt, at as u8, 7]).collect();
        let mut model = Self {
            values,
            hashes: vec![NONE; nodes],
        };
        for at in model.bottom_up() {
            model.hashes[at] = model.node_hash(at);
        }
        model
    }

    fn root(&self) -> usize {
        self.values.len() / 2
    }

    /// The level of the node at `at`: one for a leaf.
    fn level(at: usize) -> u32 {
        (at + 1).trailing_zeros() + 1
    }

    fn child(at: usize, side: Side) -> Option<usize> {
        let below = 1usize << (Self::level(at) - 1) >> 1;
        (below > 0).then(|| match side {
            Side::Left => at - below,
            Side::Right => at + below,
        })
    }

    /// Every node, each after its children.
    fn bottom_up(&self) -> Vec<usize> {
        let mut nodes: Vec<usize> = (0..self.values.len()).collect();
        nodes.sort_by_key(|at| Self::level(*at));
        nodes
    }

    fn kv_hash(&self, at: usize) -> Hash {
        hash_of(&[&key(at), &hash_of(&[&self.values[at]])])
    }

    fn node_hash(&self, at: usize) -> Hash {
        let child = |side| Self::child(at, side).map_or(NONE, |child| self.hashes[child]);
        hash_of(&[&self.kv_hash(at), &child(Side::Left), &child(Side::Right)])
    }

    fn root_hash(&self) -> Hash {
        self.hashes[self.root()]
    }

    /// The nodes from the root down to the node at `to`.
    fn path(&self, to: usize) -> Vec<usize> {
        let mut path = vec![self.root()];
        while let Some(at) = path.last().copied().filter(|at| *at != to) {
            let side = match to < at {
                true => Side::Left,
                false => Side::Right,
            };
            path.push(Self::child(at, side).expect("a node above a leaf has children"));
        }
        path
    }

    /// Gives the node at `at` the value `value`, and makes the hashes
    /// above it again.
    fn set(&mut self, at: usize, value: Vec<u8>) {
        self.values[at] = value;
        for above in self.path(at).into_iter().rev() {
            self.hashes[above] = self.node_hash(above);
        }
    }

    /// Writes the nodes `written` through `scope` as one change set of
    /// `subtree`, each with its value stated. With `lazy`, a node leaves
    /// the hash of each child that is in `written` to the engine.
    fn write(&self, scope: &Scope<'_, '_>, subtree: Subtree, written: &[usize], lazy: bool) {
        struct Put {
            key: [u8; 4],
            value_hash: Hash,
            kv_hash: Hash,
            links: [Option<Link>; 2],
            stale: Stale,
        }
        let puts: Vec<Put> = written
            .iter()
            .map(|at| {
                let mut stale = Stale::NONE;
                let links = Side::BOTH.map(|side| {
                    let child = Self::child(*at, side)?;
                    let left = lazy && written.contains(&child);
                    if left {
                        stale = stale.with(HashField::Child(side));
                    }
                    let hash = match left {
                        true => UNMADE,
                        false => self.hashes[child],
                    };
                    let below = (Self::level(child) - 1) as u8;
                    Some((key(child), hash, (below, below)))
                });
                Put {
                    key: key(*at),
                    value_hash: hash_of(&[&self.values[*at]]),
                    kv_hash: self.kv_hash(*at),
                    links,
                    stale,
                }
            })
            .collect();
        fn link(link: &Option<Link>) -> Option<LinkPut<'_>> {
            link.as_ref().map(|(key, hash, heights)| LinkPut {
                key,
                hash,
                heights: *heights,
                aggregate: &FEATURE,
            })
        }
        let nodes: Vec<NodePut<'_>> = (puts.iter().zip(written))
            .map(|(put, at)| NodePut {
                key: &put.key,
                feature: &FEATURE,
                value_hash: &put.value_hash,
                kv_hash: &put.kv_hash,
                left: link(&put.links[0]),
                right: link(&put.links[1]),
                hosted: Hosted::Keep,
                value: ValuePut::Value(&self.values[*at]),
                stale: put.stale,
                at: None,
            })
            .collect();
        let root = key(self.root());
        let whole = written.len() == self.values.len();
        let merks = [MerkWrite {
            subtree,
            nodes: &nodes,
            removed: &[],
            root: match whole {
                true => RootPut::Set(Some(&root)),
                false => RootPut::Keep,
            },
            anchored: false,
            whole: false,
        }];
        let batch = Batch {
            merks: &merks,
            ..Batch::default()
        };
        scope.apply(&batch).expect("the change set is taken");
    }
}

fn open() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("a directory");
    let db = Db::open(dir.path(), &Options::default()).expect("a new database");
    db.check_hashes_by(Box::new(Rules::default()));
    (dir, db)
}

fn enter(scope: &Scope<'_, '_>, subtree: Subtree) {
    let entered = [SubtreeWrite::Enter {
        subtree,
        tree_type: b"merk",
        host: None,
    }];
    let batch = Batch {
        subtrees: &entered,
        ..Batch::default()
    };
    scope.apply(&batch).expect("the subtree is entered");
}

/// What one transaction of many changes came to.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    /// The entries of the write set before the commit.
    writes: usize,
    /// The calls of the rules that the calls for a root hash made.
    called: u64,
    /// The nodes whose hashes the commit made, and the calls of its rules.
    evaluated: usize,
    at_commit: u64,
    /// The root hashes that were published, read from the store after
    /// the commit: each is the model's.
    roots: Vec<Hash>,
}

/// The root hash of `subtree` as a transaction that has written nothing
/// reads it.
fn published(db: &Db, subtree: Subtree) -> Option<Hash> {
    let reader = db.begin();
    let hash = reader.evaluate(subtree, &mut Rules::default());
    hash.expect("the root hash is read")
}

fn commit(db: &Db, txn: &lignindb::txn::WriteTxn<'_>) -> (usize, u64) {
    let mut rules = Rules::default();
    match db.commit(txn, &mut rules).expect("the commit") {
        Committed::Published(published) => (published.evaluated, rules.calls),
        Committed::Empty => panic!("the transaction wrote"),
    }
}

/// One Merk of 1,023 nodes that is committed with every hash made, and
/// then `changes` changes of a leaf in one transaction, each with a call
/// for the root hash after it where `calls` says so.
fn one_merk(changes: usize, calls: bool) -> (Run, Duration) {
    let (_dir, db) = open();
    let subtree = Subtree::new([1; 32]);
    let mut model = Model::new(10, 1);
    {
        let txn = db.begin();
        let scope = txn.scope().expect("a scope");
        enter(&scope, subtree);
        model.write(&scope, subtree, &model.bottom_up(), false);
        scope.merge();
        let made = db.commit(&txn, &mut NoStaleHashes);
        assert!(matches!(made, Ok(Committed::Published(_))), "{made:?}");
    }
    let leaves = model.values.len().div_ceil(2);
    let hashes = db.stats().store.hashes;
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let mut rules = Rules::default();
    // The hash that the changes leave stale in each node on the way to a
    // leaf: of the child on that way, by the last change that passed. A
    // write states the hash of the other child, which takes its mark off.
    let mut stale = std::collections::BTreeMap::new();
    let began = Instant::now();
    for change in 0..changes {
        let leaf = 2 * ((change * 7919) % leaves);
        model.set(leaf, (change as u64).to_le_bytes().to_vec());
        let path = model.path(leaf);
        stale.extend(path.windows(2).map(|pair| (pair[0], pair[1])));
        model.write(&scope, subtree, &path, true);
        if calls {
            let hash = scope.evaluate(subtree, &mut rules).expect("the hashes");
            assert_eq!(hash, Some(model.root_hash()), "after {change} changes");
        }
    }
    let spent = began.elapsed();
    scope.merge();
    let writes = txn.stats().writes;
    assert_eq!(txn.stats().stale_nodes == 0, calls || changes == 0);
    let (evaluated, at_commit) = commit(&db, &txn);
    drop(txn);
    if !calls {
        // The commit makes each of them once, however often it went
        // stale.
        assert_eq!((evaluated, at_commit), (stale.len(), stale.len() as u64));
    }
    // The store counts the hashes of the calls with those of the commit.
    let counted = db.stats().store.hashes - hashes;
    assert_eq!(counted, rules.calls + at_commit);
    let root = published(&db, subtree);
    assert_eq!(root, Some(model.root_hash()));
    let run = Run {
        writes,
        called: rules.calls,
        evaluated,
        at_commit,
        roots: root.into_iter().collect(),
    };
    db.close().expect("the database closes");
    (run, spent)
}

/// `changes` Merks of three nodes, each entered and written in one
/// transaction, with a call for its root hash after it where `calls`
/// says so.
fn many_merks(changes: usize, calls: bool) -> (Run, Duration) {
    let (_dir, db) = open();
    let name = |change: usize| {
        let mut prefix = [9; 32];
        prefix[..8].copy_from_slice(&(change as u64).to_be_bytes());
        Subtree::new(prefix)
    };
    let hashes = db.stats().store.hashes;
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let mut rules = Rules::default();
    let mut roots = Vec::with_capacity(changes);
    let began = Instant::now();
    for change in 0..changes {
        let subtree = name(change);
        let model = Model::new(2, change as u8);
        enter(&scope, subtree);
        model.write(&scope, subtree, &model.bottom_up(), true);
        if calls {
            let hash = scope.evaluate(subtree, &mut rules).expect("the hashes");
            assert_eq!(hash, Some(model.root_hash()), "the Merk {change}");
        }
        roots.push(model.root_hash());
    }
    let spent = began.elapsed();
    scope.merge();
    let writes = txn.stats().writes;
    let (evaluated, at_commit) = commit(&db, &txn);
    drop(txn);
    let counted = db.stats().store.hashes - hashes;
    assert_eq!(counted, rules.calls + at_commit);
    // The root hash of every Merk, as the commit published it.
    let read: Vec<Hash> = (0..changes)
        .filter_map(|change| published(&db, name(change)))
        .collect();
    assert_eq!(read, roots, "the root hashes that were published");
    let run = Run {
        writes,
        called: rules.calls,
        evaluated,
        at_commit,
        roots: read,
    };
    db.close().expect("the database closes");
    (run, spent)
}

/// What a run of `changes` changes comes to, by its shape, with a call
/// after each and with none: `on_the_way` nodes of each change are
/// written, and `left` of them have a stale hash, of `stale` hashes. With
/// no call it is so where no two changes leave a hash of one node.
fn expected(changes: usize, on_the_way: usize, left: usize, stale: u64, calls: bool) -> Run {
    // A call makes each stale hash, and the root's own; and a put of each
    // node that had one is kept.
    let made = changes as u64 * stale;
    match calls {
        true => Run {
            writes: changes * (on_the_way + left),
            called: made + changes as u64,
            evaluated: 0,
            at_commit: 0,
            roots: Vec::new(),
        },
        false => Run {
            writes: changes * on_the_way,
            called: 0,
            evaluated: changes * left,
            at_commit: made,
            roots: Vec::new(),
        },
    }
}

/// The call after every change of a transaction, through the engine: the
/// root hashes are the model's at every call and at the commit, the
/// commit that follows makes no hash, and a transaction that makes no
/// call publishes the same root hashes.
#[test]
fn a_transaction_that_calls_after_every_change_publishes_what_one_with_no_call_does() {
    for changes in [1, 60] {
        let (with, _) = one_merk(changes, true);
        let (without, _) = one_merk(changes, false);
        assert_eq!(with.roots, without.roots, "{changes} changes of one Merk");
        let on_the_way = Run {
            roots: with.roots.clone(),
            ..expected(changes, 10, 9, 9, true)
        };
        assert_eq!(with, on_the_way, "{changes} changes of one Merk");
        assert_eq!(
            (without.writes, without.called),
            (changes * 10, 0),
            "{changes} changes of one Merk"
        );

        let (with, _) = many_merks(changes, true);
        let (without, _) = many_merks(changes, false);
        assert_eq!(with.roots, without.roots, "{changes} Merks");
        let each = Run {
            roots: with.roots.clone(),
            ..expected(changes, 3, 1, 2, true)
        };
        assert_eq!(with, each, "{changes} Merks");
        // The commit makes the two hashes of each root.
        let each = Run {
            roots: without.roots.clone(),
            ..expected(changes, 3, 1, 2, false)
        };
        assert_eq!(without, each, "{changes} Merks");
    }
}

/// A measurement: the time of 2,000 and of 8,000 changes in one
/// transaction, each with a call after it, in both shapes, and how the
/// time grows between the two. What the calls did is held to the same
/// counts at both sizes: every call makes the hashes of its change and no
/// other, and keeps a put of each node it made a hash in.
///
/// Run it by hand, in a release build:
/// `cargo test --release -p lignindb --test evaluate_scale -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, which takes a minute"]
fn many_calls_in_one_transaction() {
    type Shape = (
        &'static str,
        fn(usize, bool) -> (Run, Duration),
        [usize; 2],
        u64,
    );
    let shapes: [Shape; 2] = [
        ("one Merk", one_merk, [10, 9], 9),
        ("many Merks", many_merks, [3, 1], 2),
    ];
    for (shape, run, [on_the_way, left], stale) in shapes {
        let mut times = Vec::new();
        for changes in [2_000, 8_000] {
            let (with, spent) = run(changes, true);
            let (_, bare) = run(changes, false);
            let counts = Run {
                roots: with.roots.clone(),
                ..expected(changes, on_the_way, left, stale, true)
            };
            assert_eq!(with, counts, "{shape}, {changes} changes");
            println!(
                "{shape}: {changes} changes with a call after each in {spent:?} ({:?} a change), \
                 with no call in {bare:?}; {} entries, {} calls of the rules",
                spent / changes as u32,
                with.writes,
                with.called,
            );
            times.push((spent.as_secs_f64(), bare.as_secs_f64()));
        }
        let exponent = |small: f64, large: f64| (large / small).ln() / 4f64.ln();
        println!(
            "{shape}: between 2,000 and 8,000 changes the time grows with the exponent {:.2} \
             with the calls, and {:.2} without",
            exponent(times[0].0, times[1].0),
            exponent(times[0].1, times[1].1),
        );
    }
}
