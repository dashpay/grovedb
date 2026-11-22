//! What a group does to the subtree of a tree it writes over where a
//! commit of another writer is published beside its look or the tree
//! above is removed with it, and what the engine refuses of a group of
//! several batches and tells a batch of a move, at the seam.
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
/// tree that was written into and emptied is entered all the same. Its
/// move, which is applied, removes its element under the old key, and
/// the entry leaves with that node, as the entry of a deleted tree does
/// ([`super::super::gone`]).
#[test]
fn the_entry_of_an_emptied_tree_leaves_with_its_element_when_the_tree_is_moved() {
    let v = version();
    let (dir, db) = grove(true);
    (db.delete(T, b"x", None, None, v))
        .unwrap()
        .expect("the tree is emptied");
    assert_eq!(entry_of_t(&db), Some((TreeType::NormalTree, false)));
    (db.apply_batch(rename_t_to_u(), None, None, v))
        .unwrap()
        .expect("the element is moved");
    let read = (db.get(ROOT, b"u", None, v).unwrap()).expect("the moved tree");
    assert_eq!(read, Element::empty_tree());
    assert!(db.get(ROOT, b"t", None, v).unwrap().is_err());
    assert_eq!(entry_of_t(&db), None);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// No operation of GroveDB makes a batch that moves a subtree and states
/// the same subtree anew: a tree type is stated anew in the storage batch
/// of `convert_to_big_sum_tree`, which is told no move, and a move is told
/// to the storage batch of a batch of operations, which states none. At
/// the seam one batch can hold both. Here it is told the move of the
/// subtree of `t`, removes the element of `t`, puts it under `u`, and
/// states the subtree of `t` anew as a tree that sums. The engine states
/// the move, the transaction refuses the batch whole
/// (`BatchError::ConvertMoved`), and holds nothing of it afterwards.
#[test]
fn a_batch_that_moves_a_subtree_and_states_it_anew_is_refused_by_the_transaction() {
    let v = version();
    let (dir, db) = grove(true);
    {
        let engine = db.engine();
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let u: &[&[u8]] = &[b"u"];
        let (from, to) = (build_prefix(T.into()).value, build_prefix(u.into()).value);
        {
            let root = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
            let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
                .unwrap()
                .expect("the root Merk");
            let moved = Element::new_tree(Some(b"x".to_vec()));
            let bytes = moved.serialize(v).expect("the bytes of an element");
            let ops = [
                (b"t".to_vec(), Op::Delete),
                (
                    b"u".to_vec(),
                    Op::Put(bytes, TreeFeatureType::BasicMerkNode),
                ),
            ];
            (merk.apply::<_, Vec<u8>>(&ops, &[], None, v))
                .unwrap()
                .expect("a write of the root Merk");
        }
        batch.move_subtree(&from, &to);
        {
            let moved = engine.subtree(T.into(), Some(&batch), &tx).unwrap();
            (moved.convert(TreeType::SumTree)).expect("the statement is staged");
        }
        let mut group = <LigninEngine as Engine>::WriteGroup::default();
        (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
        let applied = engine.apply_write_group(group, &tx).unwrap();
        let refused = applied
            .map(|_| ())
            .expect_err("a subtree is moved and stated anew");
        let said = refused.to_string();
        assert!(
            said.contains("is stated anew in a batch whose move names it"),
            "{said}"
        );
        // The transaction holds nothing of the group.
        let entered = |at: &[&[u8]]| {
            let handle = engine.subtree(at.into(), None, &tx).unwrap();
            let entry = handle.entry().expect("the directory answers");
            entry.map(|entry| entry.tree_type().expect("a tree type"))
        };
        assert_eq!(entered(T), Some(TreeType::NormalTree));
        assert_eq!(entered(u), None);
        (engine.commit_transaction(tx).unwrap()).expect("a commit of nothing");
    }
    let read = (db.get(T, b"x", None, v).unwrap()).expect("the element under the tree");
    assert_eq!(read, item(1));
    assert!(db.get(ROOT, b"u", None, v).unwrap().is_err());
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
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

/// The look for what is written over meets the look for what leaves by
/// a removed node ([`super::super::gone`]), in a group of one batch that
/// is made at the seam. The tree `t` holds the element `x` and the tree
/// `k`, which holds an element. The batch removes the node of `t` in the
/// root Merk and, through a handle by the path of `t`, writes another
/// element over `k`.
///
/// - **An item over `k`**: the look for what is written over states the
///   drop of the subtree of `k`. It is below the drop of the subtree of
///   `t`, which the removed node states, and goes with that drop.
/// - **A tree of another type over `k`, with a node put into its Merk
///   by a writer that began with no node**: the look for what is written
///   over states the drop of the subtree of `k` and enters it again, and
///   the drop of the subtree of `t` takes both.
///
/// Each group is applied. The transaction has neither subtree entered
/// afterwards, and after its commit the grove has no element `t` and the
/// checker finds the store sound.
#[test]
fn what_is_written_over_below_a_removed_node_goes_with_the_drop_above() {
    let v = version();
    let k: &[&[u8]] = &[b"t", b"k"];
    let put = |element: Element, feature: TreeFeatureType| {
        let bytes = element.serialize(v).expect("the bytes of an element");
        Op::Put(bytes, feature)
    };
    for entered_again in [false, true] {
        let (dir, db) = grove(true);
        (db.insert(T, b"k", Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree in the tree");
        (db.insert(k, b"y", item(2), None, None, v))
            .unwrap()
            .expect("an element in it");
        let root_of_t = match db.get_raw(ROOT, b"t", None, v).unwrap() {
            Ok(Element::Tree(root, _)) => root,
            other => panic!("{other:?}"),
        };
        {
            let engine = db.engine();
            let tx = engine.start_transaction();
            let entered = |at: &[&[u8]]| {
                let handle = engine.subtree(at.into(), None, &tx).unwrap();
                handle.entry().expect("the directory answers").is_some()
            };
            assert!(entered(T) && entered(k));
            let batch = <LigninEngine as Engine>::Batch::default();
            let over = match entered_again {
                false => item(3),
                true => {
                    let below = engine.subtree(k.into(), Some(&batch), &tx).unwrap();
                    let mut merk = Merk::open_layered_with_root_key(
                        below,
                        None,
                        TreeType::SumTree,
                        NO_COST,
                        v,
                    )
                    .unwrap()
                    .expect("the Merk of the new tree");
                    let node = put(Element::new_sum_item(5), TreeFeatureType::SummedMerkNode(5));
                    (merk.apply::<_, Vec<u8>>(&[(b"z".to_vec(), node)], &[], None, v))
                        .unwrap()
                        .expect("a write of the new tree's Merk");
                    Element::new_sum_tree(Some(b"z".to_vec()))
                }
            };
            {
                let within = engine.subtree(T.into(), Some(&batch), &tx).unwrap();
                let mut merk = Merk::open_layered_with_root_key(
                    within,
                    root_of_t,
                    TreeType::NormalTree,
                    NO_COST,
                    v,
                )
                .unwrap()
                .expect("the Merk of the tree");
                let over = put(over, TreeFeatureType::BasicMerkNode);
                (merk.apply::<_, Vec<u8>>(&[(b"k".to_vec(), over)], &[], None, v))
                    .unwrap()
                    .expect("an element over the tree in it");
            }
            {
                let root = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
                let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
                    .unwrap()
                    .expect("the root Merk");
                (merk.apply::<_, Vec<u8>>(&[(b"t".to_vec(), Op::Delete)], &[], None, v))
                    .unwrap()
                    .expect("the node of the tree is removed");
            }
            let mut group = <LigninEngine as Engine>::WriteGroup::default();
            (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
            let applied = engine.apply_write_group(group, &tx).unwrap();
            applied.map(|_| ()).expect("the group is applied");
            assert!(!entered(T) && !entered(k), "entered again: {entered_again}");
            (engine.commit_transaction(tx).unwrap()).expect("a commit");
        }
        let gone = db.get_raw(ROOT, b"t", None, v).unwrap();
        assert!(
            matches!(gone, Err(crate::Error::PathKeyNotFound(_))),
            "{gone:?}"
        );
        drop(Arc::into_inner(db).expect("the only handle on the grove"));
        check(dir.path());
    }
}

/// A tree of leaves that nothing was appended to is deleted, and after
/// the look of its delete a commit beside the delete puts a record that
/// is no leaf under the tree's prefix: the entry outlives its element
/// (`engine.md`, "A subtree leaves with its element", the open point),
/// as the subtree of a tree that keeps its data flat, with no record in
/// its data keyspace. A tree with an element is then moved to that key.
/// The subtree that is entered there makes room: it is dropped with the
/// move, the record that was put under it goes with the drop, and the
/// moved tree is read under the new key.
#[test]
fn leaves_that_outlived_their_tree_with_no_leaf_make_room_where_a_moved_tree_goes() {
    let v = version();
    let (dir, db) = grove(true);
    (db.insert(ROOT, b"u", Element::empty_mmr_tree(), None, None, v))
        .unwrap()
        .expect("a tree of leaves");
    let u: &[&[u8]] = &[b"u"];
    let (other, done) = beside(&db, move |db| {
        let engine = db.engine();
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree(u.into(), Some(&batch), &tx).unwrap();
        (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    });
    let tx = db.start_transaction();
    AFTER_A_LOOK.set([apart(other)].into_iter().collect());
    let deleted = db.delete(ROOT, b"u", None, Some(&tx), v).unwrap();
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
    assert!(done.get(), "the other writer came after the look");
    deleted.expect("the delete of a tree of leaves that holds none");
    (db.commit_transaction(tx).unwrap()).expect("no key was written by both");
    let gone = db.get_raw(ROOT, b"u", None, v).unwrap();
    assert!(
        matches!(gone, Err(crate::Error::PathKeyNotFound(_))),
        "{gone:?}"
    );
    // The entry of the subtree under the key `u`, as its tree type and
    // whether it has a root, and the record that was put under it.
    let under_u = || {
        let engine = db.engine();
        let tx = engine.start_read_transaction();
        let handle = engine.subtree(u.into(), None, &tx).unwrap();
        let entry = handle.entry().expect("the directory answers");
        let entry = entry.map(|entry| {
            (
                entry.tree_type().expect("a tree type"),
                entry.root_key.is_some(),
            )
        });
        let record = handle.aux().get(b"k").unwrap().expect("a read");
        (entry, record.is_some())
    };
    assert_eq!(
        under_u(),
        (Some((TreeType::MmrTree, false)), true),
        "the entry outlives its element"
    );
    (db.apply_batch(rename_t_to_u(), None, None, v).unwrap())
        .expect("the tree is moved to the key that the leaves had");
    assert_eq!(under_u(), (Some((TreeType::NormalTree, true)), false));
    let moved = (db.get(u, b"x", None, v).unwrap()).expect("the element under the new key");
    assert_eq!(moved, item(1));
    let gone = db.get_raw(ROOT, b"t", None, v).unwrap();
    assert!(
        matches!(gone, Err(crate::Error::PathKeyNotFound(_))),
        "{gone:?}"
    );
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
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
    let (walked, told, asked) = walked_beside(db, at, other);
    (walked.expect("a step of the walk"), told, asked)
}

/// As [`moved_beside`], with the keys the walk showed to its end, or the
/// failure of the step that ended it.
#[allow(clippy::type_complexity)]
fn walked_beside(
    db: &Lignin,
    at: &[&[u8]],
    other: Box<dyn FnOnce()>,
) -> (
    Result<Vec<Vec<u8>>, crate::Error>,
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
    let mut keys = Vec::new();
    let walked = loop {
        let moved = moving.move_next(|key, _| Ok(key.to_vec()), |_, _| Default::default());
        match moved.unwrap() {
            Ok(Some(key)) => keys.push(key),
            Ok(None) => break Ok(keys),
            Err(error) => break Err(error),
        }
    };
    drop(moving);
    let mut group = <LigninEngine as Engine>::WriteGroup::default();
    (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
    let part = &group.parts[0];
    let told = [part.moves().to_vec(), part.moves_of_none().to_vec()];
    (walked, told, (from, to))
}

fn rename_t_to_u() -> Vec<crate::batch::QualifiedGroveDbOp> {
    use crate::batch::QualifiedGroveDbOp;

    vec![QualifiedGroveDbOp::move_op(
        Vec::new(),
        b"t".to_vec(),
        b"u".to_vec(),
    )]
}

/// A commit beside the mover writes into a tree between the mover's read
/// of the tree's element and the application of its batch. The move is
/// dated at that read: the batch is refused as a conflict and nothing of
/// it is applied, the transaction stays open and reads the tree where it
/// is, and the same batch made again through it is applied.
#[test]
fn a_move_is_dated_at_the_read_of_the_element_that_hosts_the_tree() {
    let v = version();
    let (dir, db) = grove(true);
    let (other, done) = beside(&db, move |db| {
        (db.insert(T, b"late", item(5), None, None, v))
            .unwrap()
            .expect("a write into the tree");
    });
    let tx = db.start_transaction();
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other)].into());
    let moved = db.apply_batch(rename_t_to_u(), None, Some(&tx), v).unwrap();
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
    assert!(done.get());
    let refused = moved.expect_err("the tree was written into since the mover read its element");
    assert!(refused.is_commit_conflict(), "{refused}");
    assert_eq!(REFUSED.take().len(), 1);
    let late = db.get(T, b"late", Some(&tx), v).unwrap();
    assert_eq!(late.expect("the tree is where it was"), item(5));
    (db.apply_batch(rename_t_to_u(), None, Some(&tx), v).unwrap()).expect("the move made again");
    (db.commit_transaction(tx).unwrap()).expect("a commit");
    let u: &[&[u8]] = &[b"u"];
    for (key, held) in [(b"late".as_slice(), item(5)), (b"x", item(1))] {
        let read = db.get(u, key, None, v).unwrap();
        assert_eq!(read.expect("under the new key"), held);
    }
    assert_eq!(entry_of_t(&db), None);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A group moves a tree and writes an item over another tree, whose
/// subtree a commit beside the group drops after the look. The
/// transaction refuses the drop of a subtree that is not entered, the
/// engine looks again, and the move is stated once, as the second look
/// has it: the group is applied, and the tree is under its new key.
#[test]
fn a_group_that_is_looked_for_again_states_its_move_once() {
    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    // The root of the root Merk is `b`, with `t` under it. The other
    // writer's commit writes the node `b`, whose value the group states,
    // and no node that the group keeps the value of or that hosts the
    // moved tree: its commit is no conflict for the group.
    for tree in [b"b", b"t"] {
        (db.insert(ROOT, tree, Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree");
        let at: &[&[u8]] = &[tree];
        (db.insert(at, b"x", item(1), None, None, v))
            .unwrap()
            .expect("an element in the tree");
    }
    let (other, done) = beside(&db, move |db| {
        (db.apply_batch(vec![insert(&[], b"b", item(3))], None, None, v))
            .unwrap()
            .expect("the other writer's item over the tree");
    });
    let mut ops = rename_t_to_u();
    ops.push(insert(&[], b"b", item(2)));
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other), apart(Box::new(|| ()))].into());
    let applied = db.apply_batch(ops, None, None, v);
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked twice");
    assert!(done.get());
    let refused = REFUSED.take();
    applied.unwrap().expect("the group is applied");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("not entered"), "{refused:?}");
    let u: &[&[u8]] = &[b"u"];
    let moved = db.get(u, b"x", None, v).unwrap();
    assert_eq!(moved.expect("under the new key"), item(1));
    assert_eq!(entry_of_t(&db), None);
    let over = (db.get(ROOT, b"b", None, v).unwrap()).expect("an element");
    assert_eq!(over, item(2));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A batch works out a write into a tree in a tree, and before it is
/// applied a commit beside it moves the tree above. The subtree the
/// batch writes is under another prefix by then: the batch is refused as
/// a conflict, and nothing of it is applied (`transactions.md`, 14.5,
/// D9).
#[test]
fn a_write_under_a_tree_that_a_commit_moved_since_is_a_conflict() {
    let v = version();
    let (dir, db) = grove(true);
    let nested: &[&[u8]] = &[b"t", b"n"];
    (db.insert(T, b"n", Element::empty_tree(), None, None, v))
        .unwrap()
        .expect("a tree in the tree");
    (db.insert(nested, b"y", item(3), None, None, v))
        .unwrap()
        .expect("an element in it");
    let (other, done) = beside(&db, move |db| {
        (db.apply_batch(rename_t_to_u(), None, None, v))
            .unwrap()
            .expect("the move of the tree above");
    });
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other)].into());
    let written = db.apply_batch(vec![insert(nested, b"late", item(5))], None, None, v);
    AFTER_A_LOOK.take();
    assert!(done.get());
    let refused = written
        .unwrap()
        .expect_err("the tree above was moved since");
    assert!(refused.is_commit_conflict(), "{refused}");
    // The moved tree holds what it held, under its new key, and nothing
    // is under the old one.
    let moved: &[&[u8]] = &[b"u", b"n"];
    let held = db.get(moved, b"y", None, v).unwrap();
    assert_eq!(held.expect("under the new key"), item(3));
    assert!(db.get(moved, b"late", None, v).unwrap().is_err());
    assert!(db.get(ROOT, b"t", None, v).unwrap().is_err());
    assert_eq!(entry_of_t(&db), None);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A commit beside the mover writes a tree of leaves over the moved tree
/// and appends to it, after the walk of the move and before the batch is
/// applied. The subtree under the tree's path is another one by then,
/// entered as a tree that keeps its data flat under the node the mover
/// read. The engine states the move as the directory has the subtree,
/// the transaction dates it at the mover's read of that node, and the
/// batch is refused as a conflict with nothing of it applied.
#[test]
fn a_tree_that_a_commit_wrote_leaves_over_since_the_walk_of_its_move_is_a_conflict() {
    let v = version();
    let (dir, db) = grove(true);
    let root: &[&[u8]] = &[];
    let (other, done) = beside(&db, move |db| {
        let root: &[&[u8]] = &[];
        let leaves = vec![insert(&[], b"t", Element::empty_mmr_tree())];
        (db.apply_batch(leaves, None, None, v).unwrap()).expect("a tree of leaves over the tree");
        (db.mmr_tree_append(root, b"t", vec![7; 40], None, v))
            .unwrap()
            .expect("a leaf");
    });
    REFUSED.take();
    // The walk reads the entry of the tree, and the look for what the
    // group enters reads an entry after it.
    AFTER_ENTRY_READS.set(Some((2, other)));
    let moved = db.apply_batch(rename_t_to_u(), None, None, v).unwrap();
    assert!(AFTER_ENTRY_READS.take().is_none());
    assert!(done.get(), "the other writer came after the walk");
    let refused = moved.expect_err("the tree was written over since the mover read its element");
    assert!(refused.is_commit_conflict(), "{refused}");
    assert_eq!(REFUSED.take().len(), 1);
    let leaves = db.mmr_tree_leaf_count(root, b"t", None, v).unwrap();
    assert_eq!(leaves.expect("the other writer's tree"), 1);
    assert!(db.get(ROOT, b"u", None, v).unwrap().is_err());
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A tree with an element changes places with a tree that nothing was
/// written into, and after the walk a commit beside the mover writes a
/// tree of leaves over that tree and appends to it. The subtree that is
/// entered where the moved one goes holds the other writer's leaves: it
/// makes no room, the transaction refuses the batch as a conflict, since
/// a commit wrote under that prefix after the mover's read, and the
/// leaves are where they were.
#[test]
fn leaves_that_a_commit_entered_where_a_moved_tree_goes_are_not_dropped() {
    use crate::batch::QualifiedGroveDbOp;

    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    let root: &[&[u8]] = &[];
    // `u` is the root of the root Merk and `t` its child: a write of the
    // node `u` writes no node that hosts the moved tree.
    for tree in [b"u", b"t"] {
        (db.insert(root, tree, Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree");
    }
    (db.insert(T, b"x", item(1), None, None, v))
        .unwrap()
        .expect("an element in the tree");
    let (other, done) = beside(&db, move |db| {
        let root: &[&[u8]] = &[];
        let leaves = vec![insert(&[], b"u", Element::empty_mmr_tree())];
        (db.apply_batch(leaves, None, None, v).unwrap()).expect("a tree of leaves over the other");
        (db.mmr_tree_append(root, b"u", vec![7; 40], None, v))
            .unwrap()
            .expect("a leaf");
    });
    let swap = vec![
        QualifiedGroveDbOp::move_op(Vec::new(), b"t".to_vec(), b"u".to_vec()),
        QualifiedGroveDbOp::move_op(Vec::new(), b"u".to_vec(), b"t".to_vec()),
    ];
    REFUSED.take();
    AFTER_ENTRY_READS.set(Some((2, other)));
    let swapped = db.apply_batch(swap, None, None, v).unwrap();
    assert!(AFTER_ENTRY_READS.take().is_none());
    assert!(done.get(), "the other writer came after the walk");
    let refused = swapped.expect_err("a subtree is entered where the moved one goes");
    assert!(refused.is_commit_conflict(), "{refused}");
    assert_eq!(REFUSED.take().len(), 1);
    let leaves = db.mmr_tree_leaf_count(root, b"u", None, v).unwrap();
    assert_eq!(leaves.expect("the other writer's tree"), 1);
    let held = db.get(T, b"x", None, v).unwrap();
    assert_eq!(held.expect("the tree is where it was"), item(1));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A tree of leaves changes places with a tree that nothing was written
/// into, and after the walk a commit beside the mover puts an element
/// into that tree. The batch puts the element of the tree of leaves over
/// the element of a tree whose Merk is entered by then, with a node. The
/// look for the trees that are written over says nothing of a prefix
/// that a moved subtree goes to, the Merk makes no room, the transaction
/// refuses the batch as a conflict, and the element is where it was.
#[test]
fn an_element_that_a_commit_put_where_moved_leaves_go_is_not_dropped() {
    use crate::batch::QualifiedGroveDbOp;

    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    let root: &[&[u8]] = &[];
    let u: &[&[u8]] = &[b"u"];
    // `u` is the root of the root Merk and `m` its child: a write of the
    // node `u` writes no node that hosts the moved tree.
    (db.insert(root, b"u", Element::empty_tree(), None, None, v))
        .unwrap()
        .expect("a tree");
    (db.insert(root, b"m", Element::empty_mmr_tree(), None, None, v))
        .unwrap()
        .expect("a tree of leaves");
    (db.mmr_tree_append(root, b"m", vec![7; 40], None, v))
        .unwrap()
        .expect("a leaf");
    let (other, done) = beside(&db, move |db| {
        let u: &[&[u8]] = &[b"u"];
        (db.insert(u, b"y", item(2), None, None, v))
            .unwrap()
            .expect("the first element of the tree");
    });
    let swap = vec![
        QualifiedGroveDbOp::move_op(Vec::new(), b"m".to_vec(), b"u".to_vec()),
        QualifiedGroveDbOp::move_op(Vec::new(), b"u".to_vec(), b"m".to_vec()),
    ];
    REFUSED.take();
    // The walk reads the entry of the tree of leaves, and the look for
    // what the group enters reads an entry after it.
    AFTER_ENTRY_READS.set(Some((2, other)));
    let swapped = db.apply_batch(swap, None, None, v).unwrap();
    assert!(AFTER_ENTRY_READS.take().is_none());
    assert!(done.get(), "the other writer came after the walk");
    let refused = swapped.expect_err("a Merk with a node is entered where the leaves go");
    assert!(refused.is_commit_conflict(), "{refused}");
    assert_eq!(REFUSED.take().len(), 1);
    let held = db.get(u, b"y", None, v).unwrap();
    assert_eq!(held.expect("the other writer's element"), item(2));
    let leaves = db.mmr_tree_leaf_count(root, b"m", None, v).unwrap();
    assert_eq!(leaves.expect("the tree of leaves where it was"), 1);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A tree with an element changes places with a tree that was written
/// into and emptied, whose Merk makes room: the look states its drop.
/// After the look a commit beside the mover writes a tree of leaves over
/// that tree and appends to it. The drop takes what is entered under the
/// prefix when the batch is applied, and the transaction is asked for no
/// date of it: the batch is applied, the moved tree is under its new
/// key, and the other writer's tree of leaves is gone with its leaf.
#[test]
fn what_a_commit_entered_after_the_look_where_room_was_stated_goes_with_the_drop() {
    use crate::batch::QualifiedGroveDbOp;

    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    let root: &[&[u8]] = &[];
    let u: &[&[u8]] = &[b"u"];
    // `u` is the root of the root Merk and `t` its child: a write of the
    // node `u` writes no node that hosts the moved tree.
    for tree in [b"u", b"t"] {
        (db.insert(root, tree, Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree");
    }
    (db.insert(T, b"x", item(1), None, None, v))
        .unwrap()
        .expect("an element in the tree");
    (db.insert(u, b"y", item(2), None, None, v))
        .unwrap()
        .expect("an element in the other tree");
    (db.delete(u, b"y", None, None, v))
        .unwrap()
        .expect("the other tree is emptied");
    let (other, done) = beside(&db, move |db| {
        let root: &[&[u8]] = &[];
        let leaves = vec![insert(&[], b"u", Element::empty_mmr_tree())];
        (db.apply_batch(leaves, None, None, v).unwrap()).expect("a tree of leaves over the other");
        (db.mmr_tree_append(root, b"u", vec![7; 40], None, v))
            .unwrap()
            .expect("a leaf");
    });
    let swap = vec![
        QualifiedGroveDbOp::move_op(Vec::new(), b"t".to_vec(), b"u".to_vec()),
        QualifiedGroveDbOp::move_op(Vec::new(), b"u".to_vec(), b"t".to_vec()),
    ];
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other)].into());
    let swapped = db.apply_batch(swap, None, None, v);
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
    assert!(done.get());
    swapped.unwrap().expect("the batch is applied");
    assert_eq!(REFUSED.take(), Vec::<String>::new());
    let held = db.get(u, b"x", None, v).unwrap();
    assert_eq!(held.expect("the moved tree under its new key"), item(1));
    let read = (db.get(ROOT, b"t", None, v).unwrap()).expect("an element");
    assert_eq!(read, Element::empty_tree());
    assert!(db
        .mmr_tree_leaf_count(root, b"u", None, v)
        .unwrap()
        .is_err());
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// The walk of a move tells the batch the pair of a subtree that is
/// entered. A batch that holds the pair and wrote nothing through a
/// handle by path is refused, since the engine cannot state its move. A
/// commit beside the transaction that drops the subtree before the batch
/// is applied wrote the node that hosted it: the batch is refused as a
/// conflict by the engine's look, and nothing of it is applied.
#[test]
fn a_moved_subtree_that_a_commit_dropped_since_its_walk_is_a_conflict() {
    use grovedb_storage::error::CommitError;

    let v = version();
    let (dir, db) = grove(true);
    let engine = db.engine();
    let from = build_prefix(T.into()).unwrap();
    let to = build_prefix([b"u".as_slice()].as_ref().into()).unwrap();
    let tx = engine.start_transaction();
    let walked = || {
        let batch = <LigninEngine as Engine>::Batch::default();
        let mut moving = engine.move_subtree(from, to, &batch, &tx).unwrap();
        while (moving.move_next(|_, _| Ok(()), |_, _| Default::default()))
            .unwrap()
            .expect("a step of the walk")
            .is_some()
        {}
        drop(moving);
        batch
    };
    // A batch that holds the pair and no write of the Merk that the
    // tree's element is in: no handle by path told the engine where that
    // Merk is, and the move is not stated.
    let unstated = engine.apply_batch(walked(), &tx).unwrap();
    match unstated.expect_err("no handle by path wrote the Merk of the host") {
        StorageError::NotSupported(said) => assert!(said.contains("handle by path"), "{said}"),
        other => panic!("{other}"),
    }
    let batch = walked();
    (db.apply_batch(vec![insert(&[], b"t", item(3))], None, None, v))
        .unwrap()
        .expect("an item over the tree, beside the mover");
    let refused = engine.apply_batch(batch, &tx).unwrap();
    let refused = refused.expect_err("the subtree is gone");
    assert_eq!(refused.commit_error(), Some(CommitError::Conflict));
    drop(tx);
    assert_eq!(entry_of_t(&db), None);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
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

/// A tree that keeps its data flat, entered so. The cursor was made for
/// the nodes of a Merk, since the subtree was not entered. It shows the
/// key of the record that the other writer's commit put there and refuses
/// its value, until it shows the record's bytes (`MERK-05-F1` part 2): the
/// walk fails at that record, and the batch is told nothing of the move.
/// A tree that nothing was appended to, with no writer beside the mover,
/// is told to the batch as a walk that found none.
#[test]
fn a_flat_tree_that_is_entered_before_the_cursor_of_its_move_fails_the_walk() {
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
    let (walked, [moves, none], _) = walked_beside(&db, &[b"m"], other);
    assert!(done.get());
    let failed = walked.expect_err("the walk comes to a flat record");
    let no_value = "a raw iterator at a record returned no key or value";
    assert!(
        matches!(&failed, crate::Error::CorruptedData(said) if said == no_value),
        "{failed:?}"
    );
    assert_eq!((moves, none), (vec![], vec![]));
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
