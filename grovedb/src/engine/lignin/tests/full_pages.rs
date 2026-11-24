//! A first segment of a partial batch that grows the root of a Merk in a
//! page with no room, on both engines (D-335).
//!
//! `apply_partial_batch` applies its batch in two segments, which the
//! LigninDB engine applies as two batches of one group. The first
//! segment stops at its pause height: it writes the Merks at that level
//! and not the elements that host them. On LigninDB the root of a Merk
//! stays in its page in a write that does not hold the element
//! (`tree-layout.md` T3), and the tree pages refuse such a write where
//! the root's cell grows in a page with no room for it and with nothing
//! below the root. So a batch of a group holds the element
//! (`node-store.md`, "A batch that stops below an element").
//!
//! A history is a function that returns its calls with their answers,
//! on either engine. It makes trees whose Merks have one node under one
//! tree, puts items into a tree beside them ([`filled`]), and then
//! grows the roots of those Merks by partial batches whose first
//! segment stops right below the trees ([`Grows`]). What is put beside
//! the trees is found on LigninDB first ([`Fill::found`]): items until
//! a write of the Merk of a tree beside the grown ones, alone, that
//! grows its root by [`ROOM`] bytes is refused, and then one item that
//! is made longer by [`PAD`] bytes at a time until the first growth of
//! the history, made as a write of its Merk alone, is refused. Such a
//! write is made through the seam, with nothing above the Merk in its
//! batch, and its transaction is rolled back ([`alone`]).
//!
//! Before each partial batch a history tries the growth of its first
//! segment as a write of the Merk alone, where the segment grows the
//! root, and counts the ones that the tree pages refuse. [`alike`]
//! asserts that a history on LigninDB met at least one, that no call of
//! it but a proof failed and that the grove's own check found nothing,
//! has the physical checker read the database after its close, and
//! compares the answers with RocksDB's call for call: of a partial
//! batch the cost and the leftover levels that the caller is shown
//! after the first segment, the cost and the answer of the whole batch,
//! and the root hash after it.
//!
//! A restore names a Merk in its element after it has handed the Merk
//! over, which grows the element's cell by a pointer in a page that the
//! bulk loader filled, and the write that names the Merk holds the
//! element in the same way. [`restored_alike`] restores a tree that
//! holds many trees between the two engines.

use std::{cell::RefCell, path::Path};

use grovedb_merk::{
    element::insert::ElementInsertToStorageExtensions, proofs::Query,
    store::lignin::hash::MerkHashRules,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignindb::{Db, Options};

use super::{
    append_only::{check, sound, verified, Answer, History},
    nested::{made_by, number, segments, KINDS},
    sync::{begin, open, Requests, Side, PARTS},
    LigninEngine,
};
use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    engine::Engine,
    replication::RestoreCommitMode,
    Element, GroveDb, PathQuery, Transaction, TransactionArg,
};

type Op = QualifiedGroveDbOp;

/// What the tree pages say of a write that they refuse for a page with
/// no room (`lignin_tree::fragment::FragmentError::NoRoom`).
const NO_ROOM: &str = "is full of cells the write does not hold";

/// How many trees a history grows.
const TREES: u8 = 8;

/// How many times it grows each.
const ROUNDS: u8 = 6;

/// The bytes that the root of the tree beside the grown ones is grown
/// by where the fillers are counted. The value it is grown to stays in
/// its cell.
const ROOM: usize = 240;

/// The length of the item of a filler: with its cell and the link to it,
/// a filler takes fewer bytes than [`ROOM`], so the filler after a write
/// that was not refused fits where it goes.
const FILLER: usize = 80;

/// The bytes that the last item beside the trees is made longer by at a
/// time: fewer than any growth of a history.
const PAD: usize = 4;

/// The key of the element that each grown tree is made with.
const FIRST: [u8; 8] = [0; 8];

/// How many times at most the last item beside the trees is made longer
/// before one partial batch of a history that keeps its page full
/// ([`Pads`]). A growth that is still taken alone then is left so, and
/// the history makes the item longer for no later batch.
const MOST_PADS: usize = 128;

/// How the first segments grow the roots.
#[derive(Clone, Copy, Debug)]
enum Grows {
    /// A grown tree is made with one item, and an element is inserted
    /// into it: the root comes to have a link.
    Links,
    /// A grown tree is made with one item, which is written with a
    /// longer one.
    Values,
    /// A grown tree is made with one tree in it, and an element is
    /// inserted into that tree, which is one level below the pause: the
    /// first segment writes the element of that tree, the root of the
    /// grown tree, and the first insert gives it a root key.
    Below,
}

/// What the caller adds between the two segments.
#[derive(Clone, Copy, Debug)]
enum Adds {
    Nothing,
    /// A replacement of a sum item in another tree.
    Elsewhere,
    /// An insert into the tree that the first segment wrote into.
    Same,
}

/// What a history is made of.
#[derive(Clone, Copy, Debug)]
struct Shape {
    /// The kind of the tree that holds the grown trees ([`KINDS`]).
    parent: usize,
    /// The kind of the grown trees.
    child: usize,
    /// How many trees are between the tree `p` and that tree.
    above: usize,
    grows: Grows,
    adds: Adds,
}

impl Shape {
    const fn of(parent: usize, child: usize) -> Self {
        Self {
            parent,
            child,
            above: 0,
            grows: Grows::Links,
            adds: Adds::Nothing,
        }
    }
}

/// The paths of the trees of a history.
struct Trees {
    /// The tree that holds the grown trees.
    docs: Vec<Vec<u8>>,
    /// The tree of the fillers.
    fill: Vec<Vec<u8>>,
    /// A sum tree with the sum item `fees`.
    paid: Vec<Vec<u8>>,
}

impl Trees {
    /// The path of the grown tree `n`.
    fn tree(&self, n: u8) -> Vec<Vec<u8>> {
        [self.docs.clone(), vec![vec![b'o', n]]].concat()
    }

    /// The path of the tree beside the grown ones, which holds one item.
    fn beside(&self) -> Vec<Vec<u8>> {
        [self.docs.clone(), vec![b"x".to_vec()]].concat()
    }
}

/// The trees of `shape`, each grown tree with its first element, a tree
/// with one item beside them, and beside the trees the last item with
/// no byte and an item of four bytes.
fn made<E: Engine>(h: &mut History<'_, E>, shape: Shape) -> Trees {
    let (db, v) = (h.db, h.v);
    let kinds = made_by(v);
    let (parent, child) = (&KINDS[shape.parent % kinds], &KINDS[shape.child % kinds]);
    let put = |h: &mut History<'_, E>, at: &[Vec<u8>], key: &[u8], element: Element| {
        let inserted = db.insert(segments(at).as_slice(), key, element, None, None, v);
        h.write("made", inserted).expect("an element");
    };
    put(h, &[], b"p", Element::empty_tree());
    let p = vec![b"p".to_vec()];
    put(h, &p, b"fill", Element::empty_tree());
    put(h, &p, b"paid", Element::empty_sum_tree());
    let trees = |last: &[u8]| [p.clone(), vec![last.to_vec()]].concat();
    let (fill, paid) = (trees(b"fill"), trees(b"paid"));
    put(h, &paid, b"fees", Element::new_sum_item(0));
    put(h, &fill, b"pad", Element::new_item(Vec::new()));
    put(h, &fill, b"same", Element::new_item(vec![0; 4]));
    let mut docs = p;
    for level in 0..shape.above {
        let key = [b'a', level as u8];
        put(h, &docs, &key, Element::empty_tree());
        docs.push(key.to_vec());
    }
    put(h, &docs, b"docs", (parent.make)());
    docs.push(b"docs".to_vec());
    let trees = Trees { docs, fill, paid };
    put(h, &trees.docs, b"x", Element::empty_tree());
    put(h, &trees.beside(), &FIRST, Element::new_item(vec![0; 8]));
    for n in 0..TREES {
        put(h, &trees.docs, &[b'o', n], (child.make)());
        let (key, first): (&[u8], _) = match shape.grows {
            Grows::Links => (&FIRST, number(child.sums, i64::from(n))),
            Grows::Values => (&FIRST, Element::new_item(vec![n; 8])),
            Grows::Below => (b"in", Element::empty_tree()),
        };
        put(h, &trees.tree(n), key, first);
    }
    trees
}

/// What a history puts beside its trees ([`Fill::found`]).
#[derive(Clone, Copy, Debug)]
struct Fill {
    /// How many items of [`FILLER`] bytes.
    fillers: u16,
    /// How many times the last item is made longer by [`PAD`] bytes.
    pads: usize,
}

/// Puts the filler `n` into its tree.
fn filler<E: Engine>(h: &mut History<'_, E>, trees: &Trees, n: u16) {
    let (db, v) = (h.db, h.v);
    let item = Element::new_item(vec![7; FILLER]);
    let at = segments(&trees.fill);
    let inserted = db.insert(at.as_slice(), &n.to_be_bytes(), item, None, None, v);
    h.write("filler", inserted).expect("a filler");
}

/// Writes the last item beside the trees with `pads` times [`PAD`]
/// bytes.
fn pad<E: Engine>(h: &mut History<'_, E>, trees: &Trees, pads: usize) {
    let (db, v) = (h.db, h.v);
    let item = Element::new_item(vec![5; pads * PAD]);
    let at = segments(&trees.fill);
    let inserted = db.insert(at.as_slice(), b"pad", item, None, None, v);
    h.write("pad", inserted).expect("the last item");
}

/// Puts `fill` beside the trees, by the writes that found it.
fn filled<E: Engine>(h: &mut History<'_, E>, trees: &Trees, fill: Fill) {
    for n in 0..fill.fillers {
        filler(h, trees, n);
    }
    for pads in 1..=fill.pads {
        pad(h, trees, pads);
    }
}

/// Whether the tree pages refuse, for a page with no room, a write of
/// the Merk at `at` alone that puts `element` under `key`: the batch
/// holds the nodes of that Merk and nothing above it, and is applied to
/// a transaction that is then rolled back. RocksDB takes every such
/// write. Any other refusal fails the test.
fn alone<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    at: &[Vec<u8>],
    key: &[u8],
    element: Element,
) -> bool {
    let transaction = db.start_transaction();
    let batch = E::Batch::default();
    {
        let at = segments(at);
        let opened =
            db.open_transactional_merk_at_path(at.as_slice().into(), &transaction, Some(&batch), v);
        let mut merk = opened.unwrap().expect("the Merk of a tree");
        (element.insert(&mut merk, key, None, v).unwrap()).expect("an element in it");
    }
    let applied = db.db.apply_batch(batch, &transaction).unwrap();
    db.rollback_transaction(&transaction).expect("a rollback");
    match applied {
        Ok(_) => false,
        Err(refused) => {
            let refused = refused.to_string();
            assert!(refused.contains(NO_ROOM), "{refused}");
            true
        }
    }
}

impl Fill {
    /// What it takes beside the trees of `shape` on LigninDB, on the
    /// grove version `v`, until the first growth of the history is
    /// refused as a write of its Merk alone: items of [`FILLER`] bytes
    /// while a write of the Merk of the tree beside the grown ones,
    /// alone, that grows its root by [`ROOM`] bytes is taken, and then
    /// an item that is made longer by [`PAD`] bytes while the first
    /// growth is taken so.
    fn found(v: &GroveVersion, shape: Shape) -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<LigninEngine>::open_with_engine(dir.path()).expect("a database is made");
        let mut h = History::new(&db, v);
        let trees = made(&mut h, shape);
        let roomy = Element::new_item(vec![0; 8 + ROOM]);
        let mut fillers = 0;
        while !alone(&db, v, &trees.beside(), &FIRST, roomy.clone()) {
            assert!(
                fillers < 2048,
                "no write of the Merk alone was refused: {shape:?}"
            );
            filler(&mut h, &trees, fillers);
            fillers += 1;
        }
        let first = step(v, shape, &trees, 1, 0);
        let (key, element) = first.root.expect("the first step grows a root");
        let mut pads = 0;
        while !alone(&db, v, &first.at, &key, element.clone()) {
            assert!(
                pads * PAD < ROOM,
                "the first growth was not refused: {shape:?}"
            );
            pads += 1;
            pad(&mut h, &trees, pads);
        }
        Self { fillers, pads }
    }
}

/// `apply_partial_batch` with `ops`, paused at `pause`, through
/// `through`. `between` is called between the two segments with a place
/// for the answers of what it does there, and gives what the caller
/// adds. What the caller is shown, the cost so far and how many paths
/// are left over at each level, is an answer of its own.
fn partial<'db, E: Engine>(
    h: &mut History<'db, E>,
    name: &str,
    ops: Vec<Op>,
    pause: usize,
    through: TransactionArg<'db, '_, E>,
    mut between: impl FnMut(&mut Vec<Answer>) -> Vec<Op>,
) {
    let (db, v) = (h.db, h.v);
    let seen = RefCell::new(Vec::new());
    let options = BatchApplyOptions {
        batch_pause_height: Some(pause as u8),
        ..BatchApplyOptions::default()
    };
    let applied = db.apply_partial_batch(
        ops,
        Some(options),
        |cost, left_over| {
            let mut levels: Vec<(u32, usize)> = (left_over.iter())
                .flat_map(|levels| levels.iter())
                .map(|(level, paths)| (level, paths.len()))
                .collect();
            levels.sort_unstable();
            let mut seen = seen.borrow_mut();
            seen.push((
                format!("{name}, shown"),
                cost.clone(),
                format!("{levels:?}"),
            ));
            Ok(between(&mut seen))
        },
        through,
        v,
    );
    h.answers.extend(seen.into_inner());
    h.write_in(name, applied, through);
}

/// The step of `round` for the grown tree `n`: the operation of its
/// partial batch, what the caller adds, and the node that its first
/// segment grows the root of the tree by.
fn step(v: &GroveVersion, shape: Shape, trees: &Trees, round: u8, n: u8) -> Step {
    let child = &KINDS[shape.child % made_by(v)];
    let at = trees.tree(n);
    let new = vec![round; 8];
    let (op, got, root) = match shape.grows {
        Grows::Links => {
            let leaf = number(child.sums, i64::from(round) * 100 + i64::from(n));
            let op = Op::insert_only_known_to_not_already_exist_op(
                at.clone(),
                new.clone(),
                leaf.clone(),
            );
            (op, (at.clone(), new.clone()), Some((new, leaf)))
        }
        Grows::Values => {
            let longer = Element::new_item(vec![n; 8 + 16 * usize::from(round)]);
            let op = Op::insert_or_replace_op(at.clone(), FIRST.to_vec(), longer.clone());
            (
                op,
                (at.clone(), FIRST.to_vec()),
                Some((FIRST.to_vec(), longer)),
            )
        }
        Grows::Below => {
            let inner = [at.clone(), vec![b"in".to_vec()]].concat();
            let item = Element::new_item(vec![round; 4]);
            let op =
                Op::insert_only_known_to_not_already_exist_op(inner.clone(), new.clone(), item);
            // The element of the inner tree grows where it comes to name
            // a root key, with the first element of that tree.
            let named = Element::new_tree(Some(new.clone()));
            let root = (round == 1).then(|| (b"in".to_vec(), named));
            (op, (inner, new), root)
        }
    };
    let added = match shape.adds {
        Adds::Nothing => Vec::new(),
        Adds::Elsewhere => {
            let fees = Element::new_sum_item(i64::from(round) * 10 + i64::from(n));
            vec![Op::replace_op(trees.paid.clone(), b"fees".to_vec(), fees)]
        }
        Adds::Same => {
            let more = number(child.sums, i64::from(round));
            vec![Op::insert_only_known_to_not_already_exist_op(
                at.clone(),
                vec![b'z', round],
                more,
            )]
        }
    };
    Step {
        at,
        op,
        added,
        got,
        root,
    }
}

/// One step of a history ([`step`]).
struct Step {
    /// The path of the grown tree, at whose level the batch pauses.
    at: Vec<Vec<u8>>,
    op: Op,
    added: Vec<Op>,
    /// Where the element is that the operation writes.
    got: (Vec<Vec<u8>>, Vec<u8>),
    /// The node that the first segment puts in the Merk of the grown
    /// tree and that grows its root, if it grows it: its key and its
    /// element.
    root: Option<(Vec<u8>, Element)>,
}

impl Step {
    /// Whether the tree pages refuse the growth of the root as a write
    /// of the Merk alone ([`alone`]).
    fn refused_alone<E: Engine>(&self, db: &GroveDb<E>, v: &GroveVersion) -> bool {
        let grown = self.root.as_ref();
        grown.is_some_and(|(key, element)| alone(db, v, &self.at, key, element.clone()))
    }
}

/// What a history reads at its end: the element that the last step of
/// two of the grown trees wrote, a proof of all of the first tree, and
/// the grove's own check.
fn read<E: Engine>(h: &mut History<'_, E>, shape: Shape, trees: &Trees) {
    let (db, v) = (h.db, h.v);
    for n in [0, TREES - 1] {
        let (at, key) = step(v, shape, trees, ROUNDS, n).got;
        h.element("got", db.get(segments(&at).as_slice(), &key, None, v));
    }
    let mut all = Query::new();
    all.insert_all();
    let query = PathQuery::new_unsized(trees.tree(0), all);
    h.read("proof", db.prove_query(&query, None, v));
    verified(h);
}

/// The history of `shape` with `fill` beside its trees: the trees, what
/// is beside them, and each tree grown [`ROUNDS`] times by a partial
/// batch that pauses at the level of the grown trees. With the answers,
/// how many of the growths the tree pages refused as a write of the
/// Merk alone.
fn grown<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    shape: Shape,
    fill: Fill,
) -> (Vec<Answer>, usize) {
    let mut h = History::new(db, v);
    let trees = made(&mut h, shape);
    filled(&mut h, &trees, fill);
    let mut full = 0;
    for round in 1..=ROUNDS {
        for n in 0..TREES {
            let step = step(v, shape, &trees, round, n);
            full += usize::from(step.refused_alone(db, v));
            let name = format!("round {round} of tree {n}");
            let pause = step.at.len();
            partial(&mut h, &name, vec![step.op], pause, None, |_| {
                step.added.clone()
            });
        }
    }
    read(&mut h, shape, &trees);
    (h.answers, full)
}

/// The history `run` in a database of its own on the engine `E`. With
/// `checked`, the physical checker reads the database after its close.
fn on<E: Engine, T>(v: &GroveVersion, checked: bool, run: impl FnOnce(&GroveDb<E>) -> T) -> T {
    let dir = tempfile::tempdir().expect("a directory");
    let db = GroveDb::<E>::open_with_engine(dir.path()).expect("a database is made");
    let answers = run(&db);
    db.flush().expect("a flush");
    drop(db);
    if checked {
        check(dir.path(), v);
    }
    answers
}

/// What every history on LigninDB is held to: it met a page with no
/// room `full` times and at least once, no call of it failed but a
/// proof, which a grove version refuses of some kinds of tree on both
/// engines, every commit of a transaction at the seam was taken, and
/// the grove's own check found nothing.
fn held(answers: &[Answer], full: usize, what: &str) {
    assert!(full > 0, "{what}: no page was full");
    let failed: Vec<_> = (answers.iter())
        .filter(|(call, _, answer)| match call.as_str() {
            "proof" => false,
            "commit" => answer != "committed",
            _ => answer.starts_with("Err("),
        })
        .collect();
    assert!(failed.is_empty(), "{what}: {failed:#?}");
    assert!(sound(answers), "{what}");
}

/// Compares what a history answered on LigninDB with what it answered
/// on RocksDB, call for call.
#[cfg(feature = "rocksdb_storage")]
fn same(on_lignin: &[Answer], on_rocksdb: &[Answer], what: &str) {
    assert_eq!(on_lignin.len(), on_rocksdb.len(), "the calls of {what}");
    for (call, (lignin, rocksdb)) in on_lignin.iter().zip(on_rocksdb).enumerate() {
        assert_eq!(lignin, rocksdb, "call {call} of {what}");
    }
}

/// Runs the history of each of `shapes` on LigninDB and on RocksDB on
/// every grove version that makes the kinds of its trees.
fn alike(shapes: &[Shape]) {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        for shape in shapes {
            if shape.parent.max(shape.child) >= made_by(v) {
                continue;
            }
            let what = format!("{shape:?} on grove version {number}");
            let fill = Fill::found(v, *shape);
            let (on_lignin, full) = on::<LigninEngine, _>(v, true, |db| grown(db, v, *shape, fill));
            held(&on_lignin, full, &what);
            #[cfg(feature = "rocksdb_storage")]
            {
                let (on_rocksdb, _) =
                    on::<crate::RocksDbEngine, _>(v, false, |db| grown(db, v, *shape, fill));
                same(&on_lignin, &on_rocksdb, &what);
            }
        }
    }
}

/// What a partial batch does to the trees beside the one it grows.
#[derive(Clone, Copy, Debug)]
enum Others {
    /// It deletes the one item of each: the first segment removes a
    /// node of each of their Merks and puts none, and leaves them with
    /// no root.
    Emptied,
    /// It writes an item over each tree: what the trees held leaves
    /// with the first segment, and the second writes their elements.
    WrittenOver,
    /// It deletes each tree with what it holds: what the trees held
    /// leaves with the second segment, which removes their elements.
    Deleted,
    /// It writes an empty sum tree over each tree, which is a tree of
    /// another kind than any of them.
    OtherKind,
    /// It writes an empty tree that keeps flat records and no Merk over
    /// each tree.
    FlatRecords,
    /// It writes an empty tree of the same kind over each tree and puts
    /// an item into the new tree, another one than the tree held: the
    /// first segment writes the Merk of the new tree, which is entered
    /// again there in the place of the one that leaves, and the second
    /// writes its element.
    WrittenInto,
}

/// How many times the last item beside the trees is made longer, by
/// [`PAD`] bytes each time, before each partial batch of a history that
/// keeps its page full: no time where the growth is refused as it is.
enum Pads<'a> {
    /// Until the growth of the batch is refused as a write of its Merk
    /// alone ([`alone`]), and [`MOST_PADS`] times at most, after which
    /// the item is made longer for no later batch: found on LigninDB,
    /// and kept here for each batch.
    Found(&'a mut Vec<usize>),
    /// As many times as another run found.
    Given(&'a [usize]),
}

/// Where a history that keeps its page full calls what it is given
/// ([`Kept::between`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    /// Before the last item is made longer for a partial batch, and after
    /// each tree is made again.
    Settled,
    /// Right before a partial batch.
    Before,
    /// Right after it.
    After,
}

/// What makes a history keep the page of its trees full.
struct Kept<'a, E: Engine> {
    pads: Pads<'a>,
    between: &'a dyn Fn(&GroveDb<E>, Point),
}

/// The history of `shape`, whose roots grow by their values, with
/// `fill` beside its trees: each grown tree in turn is grown by a
/// partial batch that pauses at the level of the grown trees and does
/// `others` to every other tree under their parent. The element of one
/// of those is the node above the element of the grown tree in the
/// Merk of the parent, unless the grown tree's is the root of that
/// Merk, and as the trees are made the elements of two of them hang
/// below the element of the second tree that is grown. After each batch
/// the other trees are made again as they were made, where the batch
/// did not make them again itself. With the answers, the numbers of the
/// trees whose growth the tree pages refused as a write of the Merk
/// alone.
///
/// With `kept`, the page is kept full as far as its [`Pads`] go: before
/// each batch the last item beside the trees is made longer by them, and
/// `kept.between` is called at each [`Point`].
fn grown_beside<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    shape: Shape,
    fill: Fill,
    others: Others,
    kept: Option<Kept<'_, E>>,
) -> (Vec<Answer>, Vec<u8>) {
    use crate::{batch::SubelementsDeletionBehavior::DeleteChildren, TreeType};

    let mut h = History::new(db, v);
    let trees = made(&mut h, shape);
    filled(&mut h, &trees, fill);
    let kind = shape.child % made_by(v);
    let child = &KINDS[kind];
    let child_type = match kind {
        3 => TreeType::CountTree,
        _ => TreeType::NormalTree,
    };
    let (mut pads, between) = match kept {
        Some(kept) => (Some(kept.pads), Some(kept.between)),
        None => (None, None),
    };
    let at = |point: Point| {
        if let Some(between) = between {
            between(db, point);
        }
    };
    let mut full = Vec::new();
    let mut padded = fill.pads;
    let mut in_reach = true;
    for n in 0..TREES {
        let step = step(v, shape, &trees, 1, n);
        at(Point::Settled);
        let given = match pads.as_mut() {
            None => 0,
            Some(Pads::Given(given)) => given[usize::from(n)],
            Some(Pads::Found(found)) => {
                let mut more = 0;
                while in_reach && !step.refused_alone(db, v) {
                    more += 1;
                    pad(&mut h, &trees, padded + more);
                    in_reach = more < MOST_PADS;
                }
                found.push(more);
                padded += more;
                0
            }
        };
        for _ in 0..given {
            padded += 1;
            pad(&mut h, &trees, padded);
        }
        if step.refused_alone(db, v) {
            full.push(n);
        }
        // The other trees: the key of each under the parent, how it was
        // made, its tree type, and the item it was made with.
        let grown = (0..TREES).filter(|m| *m != n);
        let grown = grown.map(|m| {
            let first = Element::new_item(vec![m; 8]);
            (vec![b'o', m], (child.make)(), child_type, first)
        });
        let beside = (
            b"x".to_vec(),
            Element::empty_tree(),
            TreeType::NormalTree,
            Element::new_item(vec![0; 8]),
        );
        let beside: Vec<_> = grown.chain([beside]).collect();
        let under = |key: &[u8]| [trees.docs.clone(), vec![key.to_vec()]].concat();
        let over = |key: &[u8], element: Element| {
            Op::insert_or_replace_op(trees.docs.clone(), key.to_vec(), element)
        };
        let to_others = beside.iter().flat_map(|(key, tree, tree_type, _)| {
            let one = match others {
                Others::Emptied => Op::delete_op(under(key), FIRST.to_vec()),
                Others::WrittenOver => over(key, Element::new_item(b"no tree".to_vec())),
                Others::Deleted => {
                    Op::delete_tree_op(trees.docs.clone(), key.clone(), *tree_type, DeleteChildren)
                }
                Others::OtherKind => over(key, Element::empty_sum_tree()),
                Others::FlatRecords => over(key, Element::empty_mmr_tree()),
                Others::WrittenInto => over(key, tree.clone()),
            };
            // The item of a tree that the batch makes again names the
            // batch: the tree that left held another.
            let again = Element::new_item(vec![key[key.len() - 1], n]);
            let item = matches!(others, Others::WrittenInto)
                .then(|| Op::insert_or_replace_op(under(key), FIRST.to_vec(), again));
            [one].into_iter().chain(item)
        });
        let ops = [step.op].into_iter().chain(to_others).collect();
        let name = format!("tree {n}");
        at(Point::Before);
        partial(&mut h, &name, ops, step.at.len(), None, |_| Vec::new());
        at(Point::After);
        for (key, tree, _, first) in beside {
            match others {
                Others::Emptied => {}
                Others::WrittenOver | Others::Deleted => {
                    let at = segments(&trees.docs);
                    let inserted = db.insert(at.as_slice(), &key, tree, None, None, v);
                    h.write("a tree again", inserted);
                }
                // A tree is there, which an insert does not write over.
                Others::OtherKind | Others::FlatRecords => {
                    let again = vec![over(&key, tree)];
                    h.write("a tree again", db.apply_batch(again, None, None, v));
                }
                // The batch left the tree there with its item.
                Others::WrittenInto => continue,
            }
            let under = under(&key);
            let inserted = db.insert(segments(&under).as_slice(), &FIRST, first, None, None, v);
            h.write("its item again", inserted);
            at(Point::Settled);
        }
    }
    read(&mut h, shape, &trees);
    (h.answers, full)
}

/// Runs [`grown_beside`] with `others` on LigninDB and on RocksDB on
/// every grove version, for trees and for count trees under a tree.
fn alike_beside(others: Others) {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        for child in [0, 3] {
            let shape = Shape {
                grows: Grows::Values,
                ..Shape::of(0, child)
            };
            if child >= made_by(v) {
                continue;
            }
            let what = format!(
                "{others:?} beside {shape:?} on grove version {}",
                number + 1
            );
            let fill = Fill::found(v, shape);
            let (on_lignin, full) =
                on::<LigninEngine, _>(v, true, |db| grown_beside(db, v, shape, fill, others, None));
            held(&on_lignin, full.len(), &what);
            #[cfg(feature = "rocksdb_storage")]
            {
                let (on_rocksdb, _) = on::<crate::RocksDbEngine, _>(v, false, |db| {
                    grown_beside(db, v, shape, fill, others, None)
                });
                same(&on_lignin, &on_rocksdb, &what);
            }
        }
    }
}

/// The physical checker reads the open database of `db`, which it has
/// to find sound.
fn sound_now(db: &GroveDb<LigninEngine>, v: &GroveVersion, when: &str) {
    let mut rules = MerkHashRules::for_check(v);
    let report = db.engine().database().verify(Some(&mut rules));
    assert!(report.is_sound(), "{when}: {:#?}", report.findings);
}

/// Makes rounds of the reclaiming of `db` until nothing waits, whichever
/// thread took it out: the engine's own reclaimer runs beside these
/// calls. What is left when a round can do no more waits for something,
/// as a page does that would be freed with the version it was born
/// with, which waits for the next commit: a commit that writes a record
/// of no tree is made, and the rounds are made again. No call of the
/// reclaimer has failed by then, and no reclaiming was given up.
fn reclaimed(db: &GroveDb<LigninEngine>) {
    let database = db.engine().database();
    for _ in 0..64 {
        let mut round = database.reclaim();
        for _ in 0..10_000 {
            if !round.more {
                break;
            }
            round = database.reclaim();
        }
        if round.waiting == 0 && !round.more {
            let stats = database.stats();
            let failed = (stats.background.reclaim_errors, stats.reclaimed.given_up);
            assert_eq!(failed, (0, 0), "{:?}", stats.background.last_error);
            return;
        }
        (db.put_aux(b"reclaimed", b"", None, None).unwrap()).expect("a commit");
    }
    panic!("the reclaiming did not come to an end");
}

/// Runs [`grown_beside`] with `others` in a page that is kept full, on
/// LigninDB and on RocksDB on every grove version, for trees and for
/// count trees under a tree.
///
/// On LigninDB the pace of the reclaimer's thread does not decide how
/// full the page is at a batch: nothing waits to be reclaimed where the
/// room is measured ([`reclaimed`]), and the last item beside the trees
/// is then made longer until the growth of the batch is refused as a
/// write of its Merk alone ([`Pads`]). Nothing is dropped between that
/// and the batch. A growth that is still taken alone after
/// [`MOST_PADS`] times goes to its batch so, and the item is made
/// longer for no later batch: a later growth that is taken alone goes
/// to its batch as it is. The history is held to have met a refusal for
/// each tree of `full_first`. A snapshot holds what each batch dropped
/// in its pages until the physical checker has read the open database
/// after the batch, and the checker reads it again once nothing waits.
/// Where the batch writes over the trees, as many Merks as it wrote
/// over have left and are reclaimed by then, at least.
///
/// What the tree pages send away in such a batch, and that the root of
/// a Merk which the batch drops stays behind, is not seen from here:
/// the tests of `lignin_tree::drop` hold the tree pages to it. These
/// histories hold the engine to its answers where it is so.
///
/// RocksDB is given the calls of the history, the writes that make the
/// item longer among them, and the one probe of each growth before its
/// batch, which it takes and rolls back. The search for how long the
/// item has to be, the snapshot, the checker, the rounds of the
/// reclaiming and their commits of a record of no tree are made on
/// LigninDB alone.
fn alike_beside_full(others: Others, full_first: &[u8]) {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        for child in [0, 3] {
            let shape = Shape {
                grows: Grows::Values,
                ..Shape::of(0, child)
            };
            if child >= made_by(v) {
                continue;
            }
            let what = format!(
                "{others:?} in a page kept full beside {shape:?} on grove version {}",
                number + 1
            );
            let fill = Fill::found(v, shape);
            let mut pads = Vec::new();
            let writes_over = !matches!(others, Others::Deleted);
            let (on_lignin, full) = on::<LigninEngine, _>(v, true, |db| {
                let held = RefCell::new(None);
                let gone = std::cell::Cell::new(0);
                // How many dropped and cleared Merks are reclaimed in full.
                let left =
                    |db: &GroveDb<LigninEngine>| db.engine().database().stats().reclaimed.merks;
                let between = |_: &GroveDb<LigninEngine>, point: Point| match point {
                    Point::Settled => reclaimed(db),
                    Point::Before => {
                        gone.set(left(db));
                        *held.borrow_mut() = Some(db.engine().database().snapshot());
                    }
                    Point::After => {
                        sound_now(db, v, "with what the batch dropped in its pages");
                        held.borrow_mut().take();
                        reclaimed(db);
                        // A batch that writes over the trees beside the
                        // grown one drops the Merk of each, and of the
                        // tree with one item beside them.
                        let gone = left(db) - gone.get();
                        assert!(
                            !writes_over || gone >= u64::from(TREES),
                            "{what}: {gone} Merks left with a batch"
                        );
                        sound_now(db, v, "with what the batch dropped reclaimed");
                    }
                };
                let kept = Kept {
                    pads: Pads::Found(&mut pads),
                    between: &between,
                };
                grown_beside(db, v, shape, fill, others, Some(kept))
            });
            held(&on_lignin, full.len(), &what);
            assert!(
                full_first.iter().all(|n| full.contains(n)),
                "{what}: the growths of {full:?} met a page with no room: {pads:?}"
            );
            #[cfg(feature = "rocksdb_storage")]
            {
                let (on_rocksdb, _) = on::<crate::RocksDbEngine, _>(v, false, |db| {
                    let kept = Kept {
                        pads: Pads::Given(&pads),
                        between: &|_, _| (),
                    };
                    grown_beside(db, v, shape, fill, others, Some(kept))
                });
                same(&on_lignin, &on_rocksdb, &what);
            }
        }
    }
}

/// The trees whose growth meets a page with no room in a history that
/// keeps its page full and leaves the elements of the trees beside the
/// grown one under their keys, as nodes that are written over and not
/// removed: the first, and the second, whose element has the elements
/// of two other trees below it.
const BOTH_FIRST: &[u8] = &[0, 1];

/// The tree whose growth meets a page with no room in a history whose
/// batches remove the nodes of the elements beside the grown one, or
/// give the trees beside it new roots.
const THE_FIRST: &[u8] = &[0];

/// [`grown_beside`] in a page that is kept full, with an item written
/// over every tree beside the grown one. The first segment drops the
/// Merks of those trees and does not write their elements. The second
/// tree that is grown has the elements of two of them below its own, and
/// its growth is refused as a write of its Merk alone.
#[test]
fn a_first_segment_grows_a_root_in_a_page_kept_full_beside_trees_that_are_written_over() {
    alike_beside_full(Others::WrittenOver, BOTH_FIRST);
}

/// The same with every tree beside the grown one deleted with what it
/// holds: the subtrees leave with the second segment, which removes the
/// nodes of their elements. The first growth alone is held to meet a
/// page with no room.
#[test]
fn a_first_segment_grows_a_root_in_a_page_kept_full_beside_trees_that_are_deleted() {
    alike_beside_full(Others::Deleted, THE_FIRST);
}

/// The same with a tree of another kind written over every tree beside
/// the grown one.
#[test]
fn a_first_segment_grows_a_root_in_a_page_kept_full_beside_trees_of_another_kind_written_over_them()
{
    alike_beside_full(Others::OtherKind, BOTH_FIRST);
}

/// The same with a tree that keeps flat records written over every tree
/// beside the grown one.
#[test]
fn a_first_segment_grows_a_root_in_a_page_kept_full_beside_trees_of_flat_records_written_over_them()
{
    alike_beside_full(Others::FlatRecords, BOTH_FIRST);
}

/// The same with a tree of the same kind written over every tree beside
/// the grown one and written into by the same batch: the first segment
/// writes the Merk of each new tree, which takes the place of the one
/// that leaves there, and the element still points at the root that
/// left until the second segment writes it. The first growth alone is
/// held to meet a page with no room.
#[test]
fn a_first_segment_grows_a_root_in_a_page_kept_full_beside_trees_that_are_made_again() {
    alike_beside_full(Others::WrittenInto, THE_FIRST);
}

/// A first segment that grows the root of one tree and removes the one
/// node of every tree beside it. A batch of a group holds the element
/// of a Merk that it only removes nodes of as well: the node of such an
/// element, put again only as the node above the grown tree's, would
/// keep the root that the same write removes, which the tree pages
/// refuse.
#[test]
fn a_first_segment_grows_a_root_in_a_page_with_no_room_and_empties_the_trees_beside_it() {
    alike_beside(Others::Emptied);
}

/// A first segment that grows the root of one tree while the batch
/// writes an item over every tree beside it. What those trees held
/// leaves with the first segment, which does not write their elements:
/// the node of such an element is not put again as the node above the
/// grown tree's, whose element is held without it.
#[test]
fn a_first_segment_grows_a_root_in_a_page_with_no_room_beside_trees_that_are_written_over() {
    alike_beside(Others::WrittenOver);
}

/// A tree whose element names no Merk although its Merks are entered,
/// and the first write into it. A tree `b` and a count-indexed tree `a`
/// under a tree: the element of `b` is the root of the Merk that holds
/// them and the element of `a` hangs under it. `a` is given children,
/// and `b` items that are deleted again, so that its Merk is entered
/// with no node. An empty count-indexed tree is written over `a` by a
/// partial batch that pauses where the element is put: the first batch
/// of its group puts the element, as one that hosts nothing, and the
/// second removes what the old tree held and leaves the two Merks of
/// the tree entered with no node (`engine.md`, "An empty tree written
/// before the Merks under its key are emptied"). Then a partial batch
/// that pauses below `a` puts the first child into `a` and writes an
/// item over the tree `b`, whose subtree leaves with the first batch:
/// the element of `a` would come to name its Merks there, with the node
/// above it left out. `between` is called after each of the two partial
/// batches. The pages of this history have room.
fn named_again<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    mut between: impl FnMut(&GroveDb<E>, bool),
) -> Vec<Answer> {
    use super::indexed::{Count, Indexed};

    let mut h = History::new(db, v);
    let put = |h: &mut History<'_, E>, at: &[Vec<u8>], key: &[u8], element: Element| {
        let inserted = db.insert(segments(at).as_slice(), key, element, None, None, v);
        h.write("made", inserted).expect("an element");
    };
    put(&mut h, &[], b"p", Element::empty_tree());
    let p = vec![b"p".to_vec()];
    let under = |key: &[u8]| [p.clone(), vec![key.to_vec()]].concat();
    let (a, b) = (under(b"a"), under(b"b"));
    put(&mut h, &p, b"b", Element::empty_tree());
    put(&mut h, &p, b"a", Count::empty());
    for n in 0..3u8 {
        let child = Count::leaf(usize::from(n));
        let inserted = Count::insert(db, segments(&a).as_slice(), &[n], child, None, v);
        h.write("a child", inserted).expect("a child");
        put(&mut h, &b, &[n], Element::new_item(vec![n; 8]));
    }
    for n in 0..3u8 {
        let deleted = db.delete(segments(&b).as_slice(), &[n], None, None, v);
        h.write("deleted", deleted).expect("an item is deleted");
    }
    let over = Op::insert_or_replace_op(p.clone(), b"a".to_vec(), Count::empty());
    partial(
        &mut h,
        "an empty tree over a tree",
        vec![over],
        p.len(),
        None,
        |_| Vec::new(),
    );
    h.read(
        "emptied",
        db.is_empty_tree(segments(&a).as_slice(), None, v),
    );
    between(db, false);
    let first = Op::insert_or_replace_op(a.clone(), vec![9], Count::leaf(1));
    let item = Element::new_item(b"no tree".to_vec());
    let over = Op::insert_or_replace_op(p.clone(), b"b".to_vec(), item);
    let ops = vec![first, over];
    partial(&mut h, "the first child", ops, a.len(), None, |_| {
        Vec::new()
    });
    between(db, true);
    h.element("got", db.get(segments(&a).as_slice(), &[9], None, v));
    h.element("got", db.get(segments(&p).as_slice(), b"b", None, v));
    h.read("root", db.root_hash(None, v));
    verified(&mut h);
    h.answers
}

/// [`named_again`] on the grove versions that remove what an indexed
/// tree held where an element is written over it (4 and 5), on both
/// engines. On LigninDB the directory is asked between the calls: after
/// the first partial batch the primary Merk of the tree and its
/// secondary are entered under the element, each at its place, with no
/// root, and after the second each has a root. No call fails, the tree
/// is empty after the first batch, the grove's own check finds nothing,
/// the physical checker reads the database after its close, and the
/// answers are RocksDB's.
#[test]
fn the_first_write_into_a_tree_whose_element_names_no_merk_is_taken_as_on_rocksdb() {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_storage::prefix::{build_prefix, secondary_prefix_for};

    use super::indexed::entry;

    for (number, v) in GROVE_VERSIONS.iter().enumerate().skip(3) {
        let what = format!("grove version {}", number + 1);
        let parent: &[&[u8]] = &[b"p"];
        let tree: &[&[u8]] = &[b"p", b"a"];
        let primary = build_prefix(tree.into()).value;
        let secondary = secondary_prefix_for(&primary, IndexAxis::Count.tag()).value;
        let under = build_prefix(parent.into()).value;
        let asked = |db: &GroveDb<LigninEngine>, written: bool| {
            for (place, prefix) in [primary, secondary].into_iter().enumerate() {
                let entered = entry(db, prefix).expect("the Merk is entered");
                assert_eq!(entered.root_key.is_some(), written, "{what}: {place}");
                let host = entered.host.expect("a host");
                assert_eq!((host.subtree, host.key.as_slice()), (under, &b"a"[..]));
                assert_eq!(usize::from(host.position), place, "{what}");
            }
        };
        let on_lignin = on::<LigninEngine, _>(v, true, |db| named_again(db, v, asked));
        let failed: Vec<_> = (on_lignin.iter())
            .filter(|(.., answer)| answer.starts_with("Err("))
            .collect();
        assert!(failed.is_empty(), "{what}: {failed:#?}");
        assert!(sound(&on_lignin), "{what}");
        let emptied = on_lignin.iter().find(|(call, ..)| call == "emptied");
        let emptied = emptied.expect("the tree was asked for");
        assert_eq!(emptied.2, "Ok(true)", "{what}");
        #[cfg(feature = "rocksdb_storage")]
        {
            let on_rocksdb =
                on::<crate::RocksDbEngine, _>(v, false, |db| named_again(db, v, |_, _| ()));
            same(&on_lignin, &on_rocksdb, &what);
        }
    }
}

/// The case of D-335: count trees under a tree, each with one item, and
/// a partial batch that inserts a second item into one of them and
/// pauses right below it.
#[test]
fn a_first_segment_grows_a_root_in_a_page_with_no_room() {
    alike(&[Shape::of(0, 3)]);
}

/// Trees of every kind under a tree of the next kind.
#[test]
fn a_first_segment_grows_a_root_of_every_kind_of_tree_in_a_page_with_no_room() {
    // The parent is of a kind that every grove version makes.
    let narrow = made_by(&GROVE_VERSIONS[0]);
    let shapes: Vec<Shape> = (0..KINDS.len())
        .map(|child| Shape::of((child + 1) % narrow, child))
        .collect();
    alike(&shapes);
}

/// The grown trees one and three trees further down, and a first
/// segment of two levels: it writes into a tree inside each grown tree,
/// and grows the root of the grown tree by the element of that tree.
#[test]
fn a_first_segment_grows_a_root_at_a_depth_in_a_page_with_no_room() {
    let deeper = |above: usize, grows: Grows| Shape {
        above,
        grows,
        ..Shape::of(1, 0)
    };
    alike(&[
        deeper(1, Grows::Links),
        deeper(3, Grows::Links),
        deeper(0, Grows::Below),
        deeper(2, Grows::Below),
        Shape {
            grows: Grows::Below,
            ..Shape::of(0, 3)
        },
    ]);
}

/// The root grows by its value, in a tree and in a count tree.
#[test]
fn a_first_segment_grows_a_root_by_its_value_in_a_page_with_no_room() {
    let by_value = |parent: usize, child: usize| Shape {
        grows: Grows::Values,
        ..Shape::of(parent, child)
    };
    alike(&[by_value(0, 0), by_value(3, 3), by_value(0, 1)]);
}

/// The caller adds a write into another tree, and one into the tree
/// that the first segment wrote into.
#[test]
fn a_first_segment_grows_a_root_in_a_page_with_no_room_before_the_caller_adds() {
    let adding = |adds: Adds, grows: Grows| Shape {
        adds,
        grows,
        ..Shape::of(0, 3)
    };
    alike(&[
        adding(Adds::Elsewhere, Grows::Links),
        adding(Adds::Same, Grows::Links),
        adding(Adds::Same, Grows::Values),
    ]);
}

/// What a history does with the transaction of each of its steps.
#[derive(Clone, Copy, Debug)]
enum Beside {
    /// A write with no transaction between the two segments, which
    /// writes an item beside the trees again with other bytes of its
    /// length, and the commit of the transaction.
    Between,
    /// A savepoint after the partial batch, a write through the
    /// transaction, a rollback to the savepoint, a read through the
    /// transaction, and its commit.
    Again,
    /// A write with no transaction after the partial batch, a read
    /// through the transaction, and its commit.
    After,
}

/// The answer of a commit at the seam, in which the engines name a
/// conflict each in its own words.
fn committed<'db, E: Engine>(
    h: &mut History<'db, E>,
    name: &str,
    transaction: Transaction<'db, E>,
) {
    let committed = h.db.engine().commit_transaction(transaction);
    let answer = match committed.value {
        Ok(()) => "committed".to_owned(),
        Err(error) if error.is_commit_conflict() => "a conflict".to_owned(),
        Err(error) => format!("refused: {error}"),
    };
    h.answers.push((name.to_owned(), committed.cost, answer));
    h.read("root", h.db.root_hash(None, h.v));
}

/// The history of [`grown`] with each partial batch through a
/// transaction of its own, and with what `beside` says around it.
/// `seen` is called with the transaction of each step before its
/// commit.
fn through<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    shape: Shape,
    fill: Fill,
    beside: Beside,
    mut seen: impl FnMut(&Transaction<'_, E>),
) -> (Vec<Answer>, usize) {
    let mut h = History::new(db, v);
    let trees = made(&mut h, shape);
    filled(&mut h, &trees, fill);
    let beside_at = segments(&trees.fill);
    // A small item beside the fillers, under a key of its own.
    let small = |n: u16| {
        (
            [b's', (n >> 8) as u8, n as u8],
            Element::new_item(vec![9; 4]),
        )
    };
    let mut full = 0;
    let mut count = 0u16;
    for round in 1..=ROUNDS {
        for n in 0..TREES {
            let step = step(v, shape, &trees, round, n);
            full += usize::from(step.refused_alone(db, v));
            let Step {
                at,
                op,
                added,
                got: (got_at, key),
                ..
            } = step;
            let ops = vec![op];
            count += 1;
            let (beside_key, item) = small(count);
            let name = format!("round {round} of tree {n}");
            let transaction = db.start_transaction();
            let tx = Some(&transaction);
            match beside {
                Beside::Between => {
                    partial(&mut h, &name, ops, at.len(), tx, |answers| {
                        let same = Element::new_item(vec![count as u8; 4]);
                        let inserted =
                            db.insert(beside_at.as_slice(), b"same", same, None, None, v);
                        let cost = inserted.cost.clone();
                        let answer = format!("{:?}", inserted.unwrap());
                        answers.push(("beside".to_owned(), cost, answer));
                        added.clone()
                    });
                }
                Beside::Again => {
                    partial(&mut h, &name, ops, at.len(), tx, |_| added.clone());
                    h.plain("savepoint", db.set_savepoint(&transaction));
                    let inserted = db.insert(beside_at.as_slice(), &beside_key, item, None, tx, v);
                    h.write_in("in the transaction", inserted, tx);
                    h.plain("rolled back", db.rollback_to_savepoint(&transaction));
                    h.read("root", db.root_hash(tx, v));
                    h.element("got", db.get(segments(&got_at).as_slice(), &key, tx, v));
                }
                Beside::After => {
                    partial(&mut h, &name, ops, at.len(), tx, |_| added.clone());
                    let inserted =
                        db.insert(beside_at.as_slice(), &beside_key, item, None, None, v);
                    h.write("beside", inserted);
                    h.element("got", db.get(segments(&got_at).as_slice(), &key, tx, v));
                }
            }
            seen(&transaction);
            committed(&mut h, "commit", transaction);
        }
    }
    read(&mut h, shape, &trees);
    (h.answers, full)
}

/// Runs [`through`] with `beside` on both engines on every grove
/// version, and gives `told` the transaction of each step on LigninDB
/// and, at the end of each history there, its answers.
fn alike_through(
    beside: Beside,
    mut seen: impl FnMut(&Transaction<'_, LigninEngine>),
    told: impl Fn(&[Answer], usize, &str),
) {
    let shape = Shape::of(0, 3);
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let what = format!("{beside:?} on grove version {}", number + 1);
        let fill = Fill::found(v, shape);
        let (on_lignin, full) =
            on::<LigninEngine, _>(v, true, |db| through(db, v, shape, fill, beside, &mut seen));
        told(&on_lignin, full, &what);
        #[cfg(feature = "rocksdb_storage")]
        {
            let (on_rocksdb, _) = on::<crate::RocksDbEngine, _>(v, false, |db| {
                through(db, v, shape, fill, beside, |_| ())
            });
            same(&on_lignin, &on_rocksdb, &what);
        }
    }
}

/// A commit beside the transaction between the two segments: the group
/// is applied to pages that the commit wrote, and the transaction
/// commits, on both engines.
#[test]
fn a_commit_between_the_segments_leaves_the_partial_batch_as_on_rocksdb() {
    alike_through(Beside::Between, |_| (), held);
}

/// The frames of the transaction are made again from its write set
/// after the partial batch, by a rollback to a savepoint: the batch is
/// laid out a second time where its page has no room.
#[test]
fn a_partial_batch_is_laid_out_again_in_a_page_with_no_room() {
    let rebuilds = RefCell::new(Vec::new());
    alike_through(
        Beside::Again,
        |transaction| {
            let stats = transaction
                .with_write_transaction(|transaction| transaction.stats())
                .expect("a write transaction");
            rebuilds.borrow_mut().push(stats.rebuilds);
        },
        |answers, full, what| {
            held(answers, full, what);
            let rebuilds = rebuilds.take();
            assert_eq!(rebuilds.len(), usize::from(TREES) * usize::from(ROUNDS));
            assert!(
                rebuilds.iter().all(|made| *made > 0),
                "{what}: {rebuilds:?}"
            );
        },
    );
}

/// A commit beside the transaction after the partial batch meets it:
/// the transaction reads on, and its commit is a conflict, on both
/// engines.
#[test]
fn a_commit_after_the_partial_batch_meets_the_transaction_as_on_rocksdb() {
    let met = RefCell::new(Vec::new());
    alike_through(
        Beside::After,
        |transaction| {
            let stats = transaction
                .with_write_transaction(|transaction| transaction.stats())
                .expect("a write transaction");
            met.borrow_mut().push(stats.overlapped);
        },
        |answers, full, what| {
            assert!(full > 0, "{what}: no page was full");
            let met = met.take();
            assert!(met.iter().all(|met| *met > 0), "{what}: {met:?}");
            let commits = answers.iter().filter(|(call, ..)| call == "commit");
            let conflicts = commits
                .filter(|(.., answer)| answer == "a conflict")
                .count();
            assert_eq!(
                conflicts,
                usize::from(TREES) * usize::from(ROUNDS),
                "{what}"
            );
            assert!(sound(answers), "{what}");
        },
    );
}

/// How the trees of a grove that is restored lie under the tree `p`.
#[derive(Clone, Copy, Debug)]
enum Lies {
    /// Each tree holds one item.
    Under,
    /// Each tree holds one tree, which holds the item: the element of
    /// that tree is the root of its Merk.
    TwoDown,
}

/// The tree `p` with `trees` trees in it, each with what `lies` says.
fn many_trees<E: Engine>(db: &GroveDb<E>, v: &GroveVersion, trees: u16, lies: Lies) {
    let put = |at: &[&[u8]], key: &[u8], element: Element| {
        (db.insert(at, key, element, None, None, v).unwrap()).expect("an element");
    };
    put(&[], b"p", Element::empty_tree());
    for n in 0..trees {
        let key = n.to_be_bytes();
        put(&[b"p"], &key, Element::empty_tree());
        match lies {
            Lies::Under => put(&[b"p", &key], b"k", Element::new_item(vec![1; 8])),
            Lies::TwoDown => {
                put(&[b"p", &key], b"in", Element::empty_tree());
                put(&[b"p", &key, b"in"], b"k", Element::new_item(vec![1; 8]));
            }
        }
    }
}

/// What a restore left: how many requests the session made, the root
/// hash of the restored grove, how many issues its own check finds, and
/// the item of its last tree.
#[derive(Debug, PartialEq)]
struct Restored {
    requests: usize,
    root: [u8; 32],
    issues: usize,
    item: Element,
}

/// Syncs a new database on `T` from `source`, whose trees lie as `lies`
/// says, and commits the session. `counted` is shown the restored
/// database before it is closed. With what the restore left, how many
/// times the session committed before its end, and the directory of the
/// database, which is closed.
fn restored<S: Engine, T: Side>(
    source: &GroveDb<S>,
    v: &GroveVersion,
    mode: RestoreCommitMode,
    (trees, lies): (u16, Lies),
    counted: impl FnOnce(&GroveDb<T>),
) -> (Restored, usize, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    let mut session = begin(&target, root, mode, v).expect("a session starts");
    let mut requests = Requests::new(source, root, v);
    let mut served = 0;
    while let Some((request, payload)) = requests.fetch() {
        let applied = requests.apply(&mut session, &request, &payload);
        applied.expect("the chunk is taken");
        served += 1;
    }
    assert!(session.is_sync_completed());
    let commits = session.intermediate_commits();
    (target.commit_session(session, v)).expect("the session commits");
    let last = (trees - 1).to_be_bytes();
    let at: Vec<&[u8]> = match lies {
        Lies::Under => vec![b"p", &last],
        Lies::TwoDown => vec![b"p", &last, b"in"],
    };
    let issues = target.verify_grovedb(None, true, false, v);
    let left = Restored {
        requests: served,
        root: target.root_hash(None, v).unwrap().expect("a root hash"),
        issues: issues.expect("the verification ends").len(),
        item: (target.get(at.as_slice(), b"k", None, v).unwrap()).expect("the item"),
    };
    counted(&target);
    target.flush().expect("a flush");
    drop(target);
    (left, commits, dir)
}

/// The physical checker reads the LigninDB database in `dir`, which
/// nothing has open, and finds it sound. Returns how many tree pages it
/// has and how many cells are in them.
fn census(dir: &Path, v: &GroveVersion) -> (u64, u64) {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    let mut rules = MerkHashRules::for_check(v);
    let report = db.close_and_verify(Some(&mut rules)).expect("a close");
    assert!(report.is_sound(), "{:#?}", report.findings);
    (report.census.tree_pages, report.census.cells)
}

/// What the store of a LigninDB database has written to its log since
/// the database was opened.
fn written(db: &GroveDb<LigninEngine>) -> String {
    let store = db.engine().database().stats().store;
    format!(
        "{} log bytes in {} groups, {} node records of {} bytes",
        store.log_bytes, store.log_groups, store.node_records, store.node_record_bytes
    )
}

/// A restore of `shape` on the grove version `v` from LigninDB into
/// LigninDB, which ends on the root hash of its source with no issue
/// and leaves a store that the physical checker finds sound. With
/// `both`, also from LigninDB into RocksDB and from RocksDB into
/// LigninDB: the three are left alike, and the two LigninDB targets
/// with as many tree pages and cells.
fn restored_alike(v: &GroveVersion, shape: (u16, Lies), both: bool) {
    let what = format!("{shape:?}");
    let once = RestoreCommitMode::default();
    let source_dir = tempfile::tempdir().expect("a directory");
    let source = open::<LigninEngine>(source_dir.path());
    many_trees(&source, v, shape.0, shape.1);
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    let mut log = String::new();
    let (on_lignin, _, dir) =
        restored::<_, LigninEngine>(&source, v, once, shape, |db| log = written(db));
    assert_eq!((on_lignin.root, on_lignin.issues), (root, 0), "{what}");
    let (pages, cells) = census(dir.path(), v);
    eprintln!("RESTORED {what}: {log}; {pages} tree pages, {cells} cells");
    #[cfg(feature = "rocksdb_storage")]
    if both {
        type Rocks = crate::RocksDbEngine;
        let (on_rocksdb, ..) = restored::<_, Rocks>(&source, v, once, shape, |_| ());
        assert_eq!(on_rocksdb, on_lignin, "{what}, into RocksDB");
        let other_dir = tempfile::tempdir().expect("a directory");
        let other = open::<Rocks>(other_dir.path());
        many_trees(&other, v, shape.0, shape.1);
        let (from_rocksdb, _, dir) = restored::<_, LigninEngine>(&other, v, once, shape, |_| ());
        assert_eq!(from_rocksdb, on_lignin, "{what}, from RocksDB");
        assert_eq!(
            census(dir.path(), v),
            (pages, cells),
            "{what}, from RocksDB"
        );
    }
    let _ = both;
}

/// A restore of a tree that holds many trees, in the three directions
/// between the engines: 300, 1,500 and 4,000 trees with one item each,
/// and 1,500 trees with a tree in each, whose element is the root of
/// its Merk. Each tree is named in its element after its Merk was
/// handed over, in pages that the bulk loader filled.
#[test]
fn a_restore_names_many_trees_in_pages_with_no_room() {
    let v = GROVE_VERSIONS.last().expect("a grove version");
    for shape in [
        (300, Lies::Under),
        (1500, Lies::Under),
        (4000, Lies::Under),
        (1500, Lies::TwoDown),
    ] {
        restored_alike(v, shape, true);
    }
}

/// The restore of 1,500 trees under one tree from LigninDB into
/// LigninDB, on the grove versions before the last.
#[test]
fn a_restore_names_many_trees_on_every_grove_version() {
    let before_the_last = &GROVE_VERSIONS[..GROVE_VERSIONS.len() - 1];
    for v in before_the_last {
        restored_alike(v, (1500, Lies::Under), false);
    }
}

/// A session that commits in parts, with a budget of one byte and one
/// subtree in flight, names its trees across its commits: it commits
/// more than a thousand times before its end, in more requests than a
/// session that commits once, and leaves what that one leaves, in a
/// store that is sound and holds as many cells. A session of the same
/// kind that is dropped after its third commit leaves a database that
/// says it holds an incomplete restore, in which the physical checker
/// finds what such a database has and nothing else.
#[test]
fn a_restore_that_commits_in_parts_names_many_trees_as_one_that_commits_once() {
    let v = GROVE_VERSIONS.last().expect("a grove version");
    let shape = (1500, Lies::Under);
    let in_parts = PARTS;
    let source_dir = tempfile::tempdir().expect("a directory");
    let source = open::<LigninEngine>(source_dir.path());
    many_trees(&source, v, shape.0, shape.1);
    let once = RestoreCommitMode::default();
    let (at_once, none, dir) = restored::<_, LigninEngine>(&source, v, once, shape, |_| ());
    assert_eq!(none, 0);
    let whole = census(dir.path(), v);
    let (parts, commits, dir) = restored::<_, LigninEngine>(&source, v, in_parts, shape, |_| ());
    assert!(commits > 1000, "{commits} commits");
    let requests = at_once.requests;
    assert!(parts.requests > requests, "{} requests", parts.requests);
    assert_eq!(Restored { requests, ..parts }, at_once);
    assert_eq!(census(dir.path(), v).1, whole.1, "the cells");

    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<LigninEngine>(dir.path());
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    let mut session = begin(&target, root, in_parts, v).expect("a session starts");
    let mut requests = Requests::new(&source, root, v);
    while session.intermediate_commits() < 3 {
        let (request, payload) = requests.fetch().expect("a request");
        let applied = requests.apply(&mut session, &request, &payload);
        applied.expect("the chunk is taken");
    }
    drop(session);
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    drop(target);
    LigninEngine::check_incomplete(dir.path(), v);
}
