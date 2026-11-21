//! What a group does to the subtree of a tree it writes over where a
//! commit of another writer is published beside its look, and what the
//! engine refuses of a group of several batches and tells a batch of a
//! move, at the seam.
//!
//! The order is forced in each test: the other writer runs on the same
//! thread, inside the application of the group, at a place the engine
//! names (`AFTER_A_LOOK`, `AFTER_ENTRY_READS`).

use std::{cell::Cell, rc::Rc, sync::Arc};

use grovedb_merk::{
    store::lignin::hash::MerkHashRules, tree::kv::ValueDefinedCostType, Merk, Op, TreeFeatureType,
    TreeType,
};
use grovedb_storage::{prefix::build_prefix, Error as StorageError};
use grovedb_version::version::GroveVersion;
use lignindb::{Db, Options};

use super::{apart, batches::insert, check, version, Lignin, ROOT};
use crate::{
    engine::{
        lignin::{LigninEngine, AFTER_A_LOOK, AFTER_ENTRY_READS, REFUSED},
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
    assert_eq!(entry_of_t(&db), None);
    let read = (db.get(ROOT, b"t", None, v).unwrap()).expect("an element");
    assert_eq!(read, item(2));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A group writes a tree that keeps its data flat over a tree whose Merk
/// has a node, and a flat record of the new tree. The Merk is dropped,
/// the subtree is entered again as a tree of the element's type, and
/// the element's node names no root: the new tree has none.
#[test]
fn a_flat_tree_written_over_a_merk_with_a_record_of_its_own_hosts_no_root() {
    use grovedb_storage::FlatStore;

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

/// The move of a subtree reads the entry of the subtree and then makes
/// the cursor that walks it. A subtree that was not entered at the read
/// and that another writer's commit entered before the cursor was made
/// has records for the walk: the batch is told of the move with the
/// first of them, so that no record is charged for a move that the batch
/// does not hold.
#[test]
fn a_subtree_that_is_entered_before_the_cursor_of_its_move_is_a_move_of_the_batch() {
    let v = version();
    let (_dir, db) = grove(false);
    let engine = db.engine();
    let from = build_prefix(T.into()).unwrap();
    let to = build_prefix([b"u".as_slice()].as_ref().into()).unwrap();
    let (other, done) = beside(&db, move |db| {
        (db.insert(T, b"x", item(1), None, None, v))
            .unwrap()
            .expect("the first element of the tree");
    });
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    AFTER_ENTRY_READS.set(Some((1, other)));
    let mut moving = engine.move_subtree(from, to, &batch, &tx).unwrap();
    assert!(AFTER_ENTRY_READS.take().is_none());
    assert!(done.get());
    let mut walked = Vec::new();
    loop {
        let moved = moving.move_next(|key, _| Ok(key.to_vec()), |_, _| Default::default());
        match moved.unwrap().expect("a step of the walk") {
            Some(key) => walked.push(key),
            None => break,
        }
    }
    drop(moving);
    assert_eq!(walked, [b"x".to_vec()]);
    let mut group = <LigninEngine as Engine>::WriteGroup::default();
    (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
    assert_eq!(group.parts[0].moves(), [(from, to)]);
}
