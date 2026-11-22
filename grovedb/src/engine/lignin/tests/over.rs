//! What a group does to the subtree of a tree it writes over where a
//! commit of another writer is published beside its look, and what the
//! engine refuses of a group of several batches and tells a batch of a
//! move, at the seam.
//!
//! Where a test has another writer, the order is forced: that writer
//! runs on the test's thread, inside the call of the engine, at a place
//! the engine names (`AFTER_A_LOOK`, `AFTER_ENTRY_READS`).

use std::{cell::Cell, rc::Rc, sync::Arc};

use grovedb_merk::{
    store::lignin::hash::MerkHashRules, tree::kv::ValueDefinedCostType, Merk, Op, TreeFeatureType,
    TreeType,
};
use grovedb_storage::{prefix::build_prefix, Error as StorageError, FlatStore};
use grovedb_version::version::GroveVersion;
use lignindb::{Db, Options};

use super::{apart, batches::insert, check, version, Lignin, ROOT};
use crate::{
    engine::{
        lignin::{LigninEngine, AFTER_A_LOOK, AFTER_ENTRY_READS, BEFORE_A_WALK, REFUSED},
        Engine, SubtreeMove,
    },
    Element,
};

const NO_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;

const T: &[&[u8]] = &[b"t"];

fn item(byte: u8) -> Element {
    Element::new_item(vec![byte; 6])
}

/// A grove with the tree `t` under its root, and in it the element `x`
/// where `written` says so.
fn grove(written: bool) -> (tempfile::TempDir, Arc<Lignin>) {
    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    (db.insert(ROOT, b"t", Element::empty_tree(), None, None, v))
        .unwrap()
        .expect("a tree");
    if written {
        (db.insert(T, b"x", item(1), None, None, v))
            .unwrap()
            .expect("an element in the tree");
    }
    (dir, db)
}

/// What another writer does beside a group, with whether it did.
fn beside(
    db: &Arc<Lignin>,
    writes: impl FnOnce(&Lignin) + 'static,
) -> (Box<dyn FnOnce()>, Rc<Cell<bool>>) {
    let (db, done) = (db.clone(), Rc::new(Cell::new(false)));
    let said = done.clone();
    let other = Box::new(move || {
        writes(&db);
        said.set(true);
    });
    (other, done)
}

/// Whether the subtree of `t` is entered, and with which tree type and
/// whether it has a root.
fn entry_of_t(db: &Lignin) -> Option<(TreeType, bool)> {
    let engine = db.engine();
    let tx = engine.start_read_transaction();
    let handle = engine.subtree(T.into(), None, &tx).unwrap();
    let entry = handle.entry().expect("the directory answers")?;
    Some((
        entry.tree_type().expect("a tree type"),
        entry.root_key.is_some(),
    ))
}

/// The rule that the physical checker finds broken where a tree was
/// entered between the look of a group that writes an item over it and
/// the group's application: the item's node is an element with no Merk
/// whose cell hosts one.
const AFTER_AN_ENTER_BESIDE_THE_LOOK: [&str; 1] = ["HashUnchecked"];

/// The rules that the physical checker finds broken in the database of
/// `dir`, which nothing has open.
fn broken(dir: &std::path::Path) -> Vec<String> {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    let mut rules = MerkHashRules::for_check(version());
    let report = db.close_and_verify(Some(&mut rules)).expect("a close");
    for finding in &report.findings {
        eprintln!("{finding}");
    }
    let rules = report.findings.iter();
    rules.map(|finding| format!("{:?}", finding.rule)).collect()
}

/// A group writes an item over a tree that nothing was written into. Its
/// look finds no subtree under the tree, and before the group is applied
/// another writer's first element of the tree is committed. Nothing is
/// dropped and nothing is said of the element's node: the item is
/// written, its node keeps the Merk that the other writer hung under
/// it, and the entry of that Merk outlives the tree. The checker reads
/// the store that this leaves.
#[test]
fn a_tree_that_is_entered_after_the_look_outlives_the_item_written_over_it() {
    let v = version();
    let (dir, db) = grove(false);
    let (other, done) = beside(&db, move |db| {
        (db.insert(T, b"x", item(1), None, None, v))
            .unwrap()
            .expect("the first element of the tree");
    });
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other)].into());
    let applied = db.apply_batch(vec![insert(&[], b"t", item(2))], None, None, v);
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
    assert!(done.get());
    applied.unwrap().expect("the batch is applied");
    assert_eq!(REFUSED.take().len(), 0);
    let read = (db.get(ROOT, b"t", None, v).unwrap()).expect("an element");
    assert_eq!(read, item(2));
    assert_eq!(entry_of_t(&db), Some((TreeType::NormalTree, true)));
    db.engine().database().unchecked_at_close();
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    assert_eq!(broken(dir.path()), AFTER_AN_ENTER_BESIDE_THE_LOOK);
}

/// A group writes another tree over a tree that nothing was written
/// into, with an element in it. The look for what the group enters finds
/// no subtree, and before the look for what it drops another writer's
/// first element of the old tree is committed. The subtree is dropped
/// and entered once, as a tree of the new type: the group is applied,
/// and the other writer's element is not in the new tree.
#[test]
fn a_tree_that_is_entered_between_the_looks_is_dropped_and_entered_once() {
    let v = version();
    let (dir, db) = grove(false);
    let (other, done) = beside(&db, move |db| {
        (db.insert(T, b"x", item(1), None, None, v))
            .unwrap()
            .expect("the first element of the old tree");
    });
    let ops = vec![
        insert(&[], b"t", Element::empty_sum_tree()),
        insert(T, b"five", Element::new_sum_item(5)),
    ];
    REFUSED.take();
    // The look for what is entered reads the entry of the root Merk and
    // then that of the tree.
    AFTER_ENTRY_READS.set(Some((2, other)));
    let applied = db.apply_batch(ops, None, None, v);
    assert!(AFTER_ENTRY_READS.take().is_none());
    assert!(done.get(), "the other writer came inside the look");
    applied.unwrap().expect("the batch is applied");
    assert_eq!(REFUSED.take(), Vec::<String>::new());
    assert_eq!(entry_of_t(&db), Some((TreeType::SumTree, true)));
    let five = (db.get(T, b"five", None, v).unwrap()).expect("the element of the new tree");
    assert_eq!(five, Element::new_sum_item(5));
    assert!(db.get(T, b"x", None, v).unwrap().is_err());
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A group writes an item over a tree with an element in it. Its look
/// finds the subtree, and before the group is applied another writer
/// writes an item over the tree, which drops the subtree. The
/// transaction refuses the drop of a subtree that is not entered, the
/// engine looks again, finds nothing to drop, and the group is applied.
#[test]
fn a_tree_that_is_dropped_after_the_look_is_looked_for_again() {
    let v = version();
    let (dir, db) = grove(true);
    let (other, done) = beside(&db, move |db| {
        (db.apply_batch(vec![insert(&[], b"t", item(3))], None, None, v))
            .unwrap()
            .expect("the other writer's item over the tree");
    });
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other), apart(Box::new(|| ()))].into());
    let applied = db.apply_batch(vec![insert(&[], b"t", item(2))], None, None, v);
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked twice");
    assert!(done.get());
    applied.unwrap().expect("the group is applied");
    let refused = REFUSED.take();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("not entered"), "{refused:?}");
    assert_eq!(entry_of_t(&db), None);
    let read = (db.get(ROOT, b"t", None, v).unwrap()).expect("an element");
    assert_eq!(read, item(2));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A group writes an item over a tree and a record into the tree's
/// subtree. The subtree is dropped with the group and not entered again,
/// so the transaction refuses the record by the group's own doing. A
/// commit that another writer publishes after the look does not make the
/// engine look again: the refusal is the caller's answer, and nothing of
/// the group is applied.
#[test]
fn a_group_that_writes_into_a_tree_it_leaves_as_no_tree_is_looked_for_once() {
    let v = version();
    let (dir, db) = grove(true);
    let engine = db.engine();
    let (other, done) = beside(&db, |db| {
        let engine = db.engine();
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree_by_prefix([7; 32], Some(&batch), &tx);
        let handle = handle.unwrap();
        (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
        drop(handle);
        engine.apply_batch(batch, &tx).unwrap().expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    });
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let root = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
    let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
        .unwrap()
        .expect("the root Merk");
    let over = item(2).serialize(v).expect("the bytes of an element");
    let put = Op::Put(over, TreeFeatureType::BasicMerkNode);
    (merk.apply::<_, Vec<u8>>(&[(b"t".to_vec(), put)], &[], None, v))
        .unwrap()
        .expect("a write of the root Merk");
    drop(merk);
    let below = engine.subtree(T.into(), Some(&batch), &tx).unwrap();
    (below.aux().put(b"k", b"v", None).unwrap()).expect("staging");
    drop(below);
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other), apart(Box::new(|| ()))].into());
    let applied = engine.apply_batch(batch, &tx).unwrap();
    assert!(done.get());
    assert_eq!(AFTER_A_LOOK.take().len(), 1, "one look");
    applied.expect_err("the group is refused");
    let refused = REFUSED.take();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("not entered"), "{refused:?}");
    // The transaction holds nothing of the group: it reads the tree and
    // its element.
    let through = (db.get(ROOT, b"t", Some(&tx), v).unwrap()).expect("the element of the group");
    assert!(through.is_any_tree(), "{through:?}");
    let held = (db.get(T, b"x", Some(&tx), v).unwrap()).expect("the element of the tree");
    assert_eq!(held, item(1));
    let below = engine.subtree(T.into(), None, &tx).unwrap();
    let record = below.aux().get(b"k").unwrap().expect("a read");
    assert_eq!(record, None);
    drop(below);
    drop(tx);
    assert_eq!(entry_of_t(&db), Some((TreeType::NormalTree, true)));
    let kept = (db.get(T, b"x", None, v).unwrap()).expect("the element of the tree");
    assert_eq!(kept, item(1));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A group writes a tree that keeps its data flat over a tree whose Merk
/// has a node, and a flat record of the new tree. The Merk is dropped,
/// the subtree is entered again as a tree of the element's type, and
/// the element's node names no root: the new tree has none.
#[test]
fn a_flat_tree_written_over_a_merk_with_a_record_of_its_own_hosts_no_root() {
    let v = version();
    let (dir, db) = grove(true);
    let engine = db.engine();
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let root = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
    let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
        .unwrap()
        .expect("the root Merk");
    let leaves = (Element::empty_mmr_tree().serialize(v)).expect("the bytes of an element");
    let put = Op::Put(leaves, TreeFeatureType::BasicMerkNode);
    (merk.apply::<_, Vec<u8>>(&[(b"t".to_vec(), put)], &[], None, v))
        .unwrap()
        .expect("a write of the root Merk");
    drop(merk);
    let below = engine.subtree(T.into(), Some(&batch), &tx).unwrap();
    (FlatStore::put(&below, b"leaf", b"v", None).unwrap()).expect("staging");
    drop(below);
    (engine.apply_batch(batch, &tx).unwrap()).expect("the group is applied");
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
    assert_eq!(entry_of_t(&db), Some((TreeType::MmrTree, false)));
    let tx = engine.start_read_transaction();
    let below = engine.subtree(T.into(), None, &tx).unwrap();
    let read = FlatStore::get(&below, b"leaf").unwrap().expect("a read");
    assert_eq!(read, Some(b"v".to_vec()));
    drop(below);
    drop(tx);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// GroveDB does not ask the engine to walk a moved tree whose element has
/// no root key and whose data is a Merk: there is no record to copy. A
/// tree that was written into and emptied is entered all the same, so
/// its move, which is applied, leaves its entry under an element that is
/// gone, as the delete of such a tree does until the subtree of a removed
/// element is dropped with it (GROVE-05).
#[test]
fn the_entry_of_an_emptied_tree_stays_behind_when_the_tree_is_moved() {
    use crate::batch::QualifiedGroveDbOp;

    let v = version();
    let (dir, db) = grove(true);
    (db.delete(T, b"x", None, None, v))
        .unwrap()
        .expect("the tree is emptied");
    assert_eq!(entry_of_t(&db), Some((TreeType::NormalTree, false)));
    let moved = QualifiedGroveDbOp::move_op(Vec::new(), b"t".to_vec(), b"u".to_vec());
    (db.apply_batch(vec![moved], None, None, v))
        .unwrap()
        .expect("the element is moved");
    let read = (db.get(ROOT, b"u", None, v).unwrap()).expect("the moved tree");
    assert_eq!(read, Element::empty_tree());
    assert!(db.get(ROOT, b"t", None, v).unwrap().is_err());
    assert_eq!(entry_of_t(&db), Some((TreeType::NormalTree, false)));
    db.engine().database().unchecked_at_close();
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    assert_eq!(broken(dir.path()), ["EntryHost"]);
}

/// A group of two batches, of which one writes the element of a tree and
/// the other the nodes of the tree's Merk. In the order of a partial
/// batch, the Merk and then its element, the group is applied. The other
/// way round the last write of the Merk would leave its root for a node
/// that no batch writes after it: the group is refused, and the
/// transaction takes nothing of it.
#[test]
fn a_group_that_writes_a_merk_after_its_last_write_of_the_element_is_refused() {
    let v = version();
    let (_dir, db) = grove(false);
    let engine = db.engine();
    let put = |element: Element| {
        let bytes = element.serialize(v).expect("the bytes of an element");
        Op::Put(bytes, TreeFeatureType::BasicMerkNode)
    };
    let group_of = |element_first: bool| {
        let tx = engine.start_transaction();
        let mut group = <LigninEngine as Engine>::WriteGroup::default();
        for element in [element_first, !element_first] {
            let batch = <LigninEngine as Engine>::Batch::default();
            if element {
                let root = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
                let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
                    .unwrap()
                    .expect("the root Merk");
                let stated = Element::new_tree(Some(b"k".to_vec()));
                (merk.apply::<_, Vec<u8>>(&[(b"t".to_vec(), put(stated))], &[], None, v))
                    .unwrap()
                    .expect("a write of the root Merk");
            } else {
                let below = engine.subtree(T.into(), Some(&batch), &tx).unwrap();
                let mut merk =
                    Merk::open_layered_with_root_key(below, None, TreeType::NormalTree, NO_COST, v)
                        .unwrap()
                        .expect("the Merk of the tree");
                (merk.apply::<_, Vec<u8>>(&[(b"k".to_vec(), put(item(4)))], &[], None, v))
                    .unwrap()
                    .expect("a write of the tree's Merk");
            }
            (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
        }
        let applied = engine.apply_write_group(group, &tx).unwrap();
        let entered = {
            let below = engine.subtree(T.into(), None, &tx).unwrap();
            below.entry().expect("the directory answers").is_some()
        };
        (applied.map(|_| ()), entered)
    };
    let (refused, entered) = group_of(true);
    let said = match refused.expect_err("the Merk is written after its element") {
        StorageError::NotSupported(said) => said,
        other => panic!("{other}"),
    };
    assert!(
        said.contains("after the last one that writes the tree's element"),
        "{said}"
    );
    assert!(!entered);
    let (applied, entered) = group_of(false);
    applied.expect("the order of a partial batch");
    assert!(entered);
}

/// Walks the move of the subtree at `at` to the key `u` with `other`
/// happening after the engine read the entry of the subtree, before it
/// makes the cursor. Gives the keys the walk showed, and the moves and
/// the walks that found no subtree which the batch holds afterwards,
/// with the move that was asked for.
#[allow(clippy::type_complexity)]
fn moved_beside(
    db: &Lignin,
    at: &[&[u8]],
    other: Box<dyn FnOnce()>,
) -> (
    Vec<Vec<u8>>,
    [Vec<([u8; 32], [u8; 32])>; 2],
    ([u8; 32], [u8; 32]),
) {
    let engine = db.engine();
    let from = build_prefix(at.into()).unwrap();
    let to = build_prefix([b"u".as_slice()].as_ref().into()).unwrap();
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    AFTER_ENTRY_READS.set(Some((1, other)));
    let mut moving = engine.move_subtree(from, to, &batch, &tx).unwrap();
    assert!(AFTER_ENTRY_READS.take().is_none());
    let mut walked = Vec::new();
    loop {
        let moved = moving.move_next(|key, _| Ok(key.to_vec()), |_, _| Default::default());
        match moved.unwrap().expect("a step of the walk") {
            Some(key) => walked.push(key),
            None => break,
        }
    }
    drop(moving);
    let mut group = <LigninEngine as Engine>::WriteGroup::default();
    (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
    let part = &group.parts[0];
    let told = [part.moves().to_vec(), part.moves_of_none().to_vec()];
    (walked, told, (from, to))
}

/// The move of a subtree reads the entry of the subtree and then makes
/// the cursor that walks it. A subtree that was not entered at the read
/// and that another writer's commit entered before the cursor was made
/// is entered when the walk ends, and the batch is told of the move
/// there: no move is applied as that of a tree nothing was written into.
/// The walk shows the nodes of a Merk that was entered so.
#[test]
fn a_subtree_that_is_entered_before_the_cursor_of_its_move_is_a_move_of_the_batch() {
    let v = version();
    let (_dir, db) = grove(false);
    let (other, done) = beside(&db, move |db| {
        (db.insert(T, b"x", item(1), None, None, v))
            .unwrap()
            .expect("the first element of the tree");
    });
    let (walked, [moves, none], asked) = moved_beside(&db, T, other);
    assert!(done.get());
    assert_eq!(walked, [b"x".to_vec()]);
    assert_eq!((moves, none), (vec![asked], vec![]));
}

/// The same for a tree that keeps its data flat. The cursor was made for
/// the nodes of a Merk, since the subtree was not entered, and shows
/// none of the records that the other writer's commit put there: the
/// batch is told of the move all the same, where the walk ends.
#[test]
fn a_flat_tree_that_is_entered_before_the_cursor_of_its_move_is_a_move_of_the_batch() {
    let v = version();
    let (_dir, db) = grove(false);
    let root: &[&[u8]] = &[];
    (db.insert(root, b"m", Element::empty_mmr_tree(), None, None, v))
        .unwrap()
        .expect("a tree of leaves");
    let (other, done) = beside(&db, move |db| {
        let root: &[&[u8]] = &[];
        (db.mmr_tree_append(root, b"m", vec![7; 40], None, v))
            .unwrap()
            .expect("the first leaf");
    });
    let (walked, [moves, none], asked) = moved_beside(&db, &[b"m"], other);
    assert!(done.get());
    assert_eq!(walked, Vec::<Vec<u8>>::new());
    assert_eq!((moves, none), (vec![asked], vec![]));
    // With no writer beside it, the tree nothing was appended to is no
    // move of its batch: the batch is told that the walk found none.
    (db.insert(root, b"n", Element::empty_mmr_tree(), None, None, v))
        .unwrap()
        .expect("a tree of leaves");
    let (walked, [moves, none], asked) = moved_beside(&db, &[b"n"], Box::new(|| ()));
    assert_eq!(walked.len(), 0);
    assert_eq!((moves, none), (vec![], vec![asked]));
}

/// GroveDB works out the move of a tree from the element it read, stages
/// the put and the delete of that element, and asks the engine to walk
/// the subtree afterwards. Where a commit beside the transaction writes
/// an item over the tree in between, the subtree is dropped and the walk
/// finds none, while the batch puts an element that names the tree's
/// data under the new key. The batch is refused as a conflict, and
/// nothing of it is applied: for a tree whose data is a Merk, and for a
/// tree of leaves. A tree of leaves that nothing was appended to has no
/// subtree and names no data, and is moved. One writer that inserts an
/// element which counts a leaf that its tree does not hold is refused the
/// move of it alike.
#[test]
fn a_move_whose_subtree_a_commit_dropped_before_its_walk_is_a_conflict() {
    use crate::batch::QualifiedGroveDbOp;

    let v = version();
    let (dir, db) = grove(true);
    let root: &[&[u8]] = &[];
    (db.insert(root, b"m", Element::empty_mmr_tree(), None, None, v))
        .unwrap()
        .expect("a tree of leaves");
    (db.mmr_tree_append(root, b"m", vec![7; 40], None, v))
        .unwrap()
        .expect("a leaf");
    (db.insert(root, b"e", Element::empty_mmr_tree(), None, None, v))
        .unwrap()
        .expect("a tree of leaves that nothing is appended to");
    let rename = |key: &[u8], new_key: &[u8]| {
        vec![QualifiedGroveDbOp::move_op(
            Vec::new(),
            key.to_vec(),
            new_key.to_vec(),
        )]
    };
    for (key, new_key) in [(b"t".as_slice(), b"u".as_slice()), (b"m", b"n")] {
        let (written_over, landed) = (key.to_vec(), new_key.to_vec());
        let (other, done) = beside(&db, move |db| {
            (db.apply_batch(vec![insert(&[], &written_over, item(3))], None, None, v))
                .unwrap()
                .expect("an item over the tree, beside the mover");
        });
        BEFORE_A_WALK.set(Some(other));
        let moved = db.apply_batch(rename(key, new_key), None, None, v).unwrap();
        assert!(BEFORE_A_WALK.take().is_none(), "the subtree was walked");
        assert!(done.get());
        let refused = moved.expect_err("the subtree was dropped since the mover read its element");
        assert!(refused.is_commit_conflict(), "{refused}");
        let read = (db.get(ROOT, key, None, v).unwrap()).expect("the other writer's item");
        assert_eq!(read, item(3));
        assert!(db.get(ROOT, &landed, None, v).unwrap().is_err());
    }
    assert_eq!(entry_of_t(&db), None);
    (db.apply_batch(rename(b"e", b"f"), None, None, v))
        .unwrap()
        .expect("the move of a tree of leaves that nothing was appended to");
    let read = (db.get(ROOT, b"f", None, v).unwrap()).expect("the moved tree");
    assert_eq!(read, Element::empty_mmr_tree());
    // One writer meets the same refusal with an element that counts a
    // leaf which its tree does not hold: nothing is entered under it.
    let counted = Element::MmrTree(1, None);
    (db.apply_batch(vec![insert(&[], b"c", counted.clone())], None, None, v))
        .unwrap()
        .expect("an element that counts a leaf");
    let moved = db.apply_batch(rename(b"c", b"d"), None, None, v).unwrap();
    let refused = moved.expect_err("the element names data that the directory does not hold");
    assert!(refused.is_commit_conflict(), "{refused}");
    let read = (db.get(ROOT, b"c", None, v).unwrap()).expect("the element where it was");
    assert_eq!(read, counted);
    assert!(db.get(ROOT, b"d", None, v).unwrap().is_err());
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}
