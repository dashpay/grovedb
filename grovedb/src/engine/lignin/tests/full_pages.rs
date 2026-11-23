//! A first segment of a partial batch that grows the root of a Merk in a
//! page with no room, on both engines (D-335).
//!
//! `apply_partial_batch` applies its batch in two segments, which the
//! LigninDB engine applies as two batches of one group. The first
//! segment stops at its pause height: it writes the Merks at that level
//! and not the elements that host them. On LigninDB the root of a Merk
//! stays in its page in a write that does not hold the element
//! (`tree-layout.md` T3), and the tree pages refuse such a write where
//! the root's cell grows in a page with no room. So a batch of a group
//! holds the way to the element (`node-store.md`, "A batch that stops
//! below an element").
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

use std::cell::RefCell;

use grovedb_merk::{element::insert::ElementInsertToStorageExtensions, proofs::Query};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, sound, verified, Answer, History},
    nested::{made_by, number, segments, KINDS},
    LigninEngine,
};
use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    engine::Engine,
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
}

/// The history of `shape`, whose roots grow by their values, with
/// `fill` beside its trees: each grown tree in turn is grown by a
/// partial batch that pauses at the level of the grown trees and does
/// `others` to every other tree under their parent. The element of one
/// of those is the node above the element of the grown tree in the
/// Merk of the parent, unless the grown tree's is the root of that
/// Merk. After each batch the other trees are made again as they were
/// made. With the answers, how many of the growths the tree pages
/// refused as a write of the Merk alone.
fn grown_beside<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    shape: Shape,
    fill: Fill,
    others: Others,
) -> (Vec<Answer>, usize) {
    let mut h = History::new(db, v);
    let trees = made(&mut h, shape);
    filled(&mut h, &trees, fill);
    let child = &KINDS[shape.child % made_by(v)];
    let mut full = 0;
    for n in 0..TREES {
        let step = step(v, shape, &trees, 1, n);
        full += usize::from(step.refused_alone(db, v));
        // The other trees: the key of each under the parent, how it was
        // made, and the item it was made with.
        let grown = (0..TREES).filter(|m| *m != n);
        let grown = grown.map(|m| (vec![b'o', m], (child.make)(), Element::new_item(vec![m; 8])));
        let beside = (
            b"x".to_vec(),
            Element::empty_tree(),
            Element::new_item(vec![0; 8]),
        );
        let beside: Vec<_> = grown.chain([beside]).collect();
        let under = |key: &[u8]| [trees.docs.clone(), vec![key.to_vec()]].concat();
        let to_others = beside.iter().map(|(key, ..)| match others {
            Others::Emptied => Op::delete_op(under(key), FIRST.to_vec()),
            Others::WrittenOver => {
                let item = Element::new_item(b"no tree".to_vec());
                Op::insert_or_replace_op(trees.docs.clone(), key.clone(), item)
            }
        });
        let ops = [step.op].into_iter().chain(to_others).collect();
        let name = format!("tree {n}");
        partial(&mut h, &name, ops, step.at.len(), None, |_| Vec::new());
        for (key, tree, first) in beside {
            if matches!(others, Others::WrittenOver) {
                let at = segments(&trees.docs);
                let inserted = db.insert(at.as_slice(), &key, tree, None, None, v);
                h.write("a tree again", inserted);
            }
            let at = under(&key);
            let inserted = db.insert(segments(&at).as_slice(), &FIRST, first, None, None, v);
            h.write("its item again", inserted);
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
                on::<LigninEngine, _>(v, true, |db| grown_beside(db, v, shape, fill, others));
            held(&on_lignin, full, &what);
            #[cfg(feature = "rocksdb_storage")]
            {
                let (on_rocksdb, _) = on::<crate::RocksDbEngine, _>(v, false, |db| {
                    grown_beside(db, v, shape, fill, others)
                });
                same(&on_lignin, &on_rocksdb, &what);
            }
        }
    }
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
            let transaction = transaction
                .write_transaction()
                .expect("a write transaction");
            rebuilds.borrow_mut().push(transaction.stats().rebuilds);
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
            let transaction = transaction
                .write_transaction()
                .expect("a write transaction");
            met.borrow_mut().push(transaction.stats().overlapped);
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
