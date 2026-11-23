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
    db.check_kinds_by(grovedb_merk::store::lignin::keeps_flat_records);
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

/// A group writes an empty indexed tree over a tree that nothing was
/// written into, or over an indexed tree that was written into and
/// emptied. Its look states no drop, and before the group is applied
/// another writer's commit writes a child into the old tree. The
/// element is written as the group states it, and the Merks of that
/// commit stay entered with their roots, under an element that names
/// none. The checker reads the store that this leaves: the value hash
/// of the element's node, which was made from no root. Where the old
/// tree had more axes than the new one, the node is written as one
/// that hosts nothing, and the checker finds each of the four roots
/// pointed at by no cell.
#[test]
fn a_tree_that_a_commit_writes_into_after_the_look_keeps_its_merks_under_an_empty_indexed_tree() {
    use super::indexed::{Count, CountSum, Indexed, Sum};

    #[derive(Clone, Copy, Debug)]
    enum Old {
        NothingEntered,
        Emptied,
        EmptiedWithMoreAxes,
    }
    let v = version();
    for old in [Old::NothingEntered, Old::Emptied, Old::EmptiedWithMoreAxes] {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
        let made = match old {
            Old::NothingEntered => Element::empty_tree(),
            Old::Emptied => Sum::empty(),
            Old::EmptiedWithMoreAxes => CountSum::empty(),
        };
        (db.insert(ROOT, b"t", made, None, None, v))
            .unwrap()
            .expect("a tree");
        match old {
            Old::NothingEntered => {}
            Old::Emptied => {
                (Sum::insert(&*db, T, b"x", Sum::leaf(1), None, v).unwrap()).expect("a child");
                (Sum::delete(&*db, T, b"x", None, v).unwrap()).expect("the child is deleted");
            }
            Old::EmptiedWithMoreAxes => {
                let child = CountSum::insert(&*db, T, b"x", CountSum::leaf(1), None, v);
                child.unwrap().expect("a child");
                (CountSum::delete(&*db, T, b"x", None, v).unwrap()).expect("deleted");
            }
        }
        let (other, done) = beside(&db, move |db| match old {
            Old::NothingEntered => (db.insert(T, b"y", item(1), None, None, v))
                .unwrap()
                .expect("the first element of the tree"),
            Old::Emptied => {
                (Sum::insert(db, T, b"y", Sum::leaf(2), None, v).unwrap()).expect("a child")
            }
            Old::EmptiedWithMoreAxes => {
                let child = CountSum::insert(db, T, b"y", CountSum::leaf(2), None, v);
                child.unwrap().expect("a child")
            }
        });
        let new = match old {
            Old::EmptiedWithMoreAxes => Count::empty(),
            Old::NothingEntered | Old::Emptied => Sum::empty(),
        };
        REFUSED.take();
        AFTER_A_LOOK.set([apart(other)].into());
        let applied = db.apply_batch(vec![insert(&[], b"t", new.clone())], None, None, v);
        assert!(AFTER_A_LOOK.take().is_empty(), "{old:?}: the group looked");
        assert!(done.get(), "{old:?}");
        applied.unwrap().expect("the batch is applied");
        assert_eq!(REFUSED.take().len(), 0, "{old:?}");
        let read = (db.get(ROOT, b"t", None, v).unwrap()).expect("an element");
        assert_eq!(read, new, "{old:?}");
        assert!(entry_of_t(&db).is_some_and(|(_, rooted)| rooted), "{old:?}");
        db.engine().database().unchecked_at_close();
        drop(Arc::into_inner(db).expect("the only handle on the grove"));
        let found: &[&str] = match old {
            Old::NothingEntered | Old::Emptied => &["Hash(Value)"],
            Old::EmptiedWithMoreAxes => &[
                "Fragments",
                "HostPointer",
                "HostPointer",
                "HostPointer",
                "HostPointer",
            ],
        };
        assert_eq!(broken(dir.path()), found, "{old:?}");
    }
}

/// The residual of that race for a partial batch (GROVE-03-F2, D-350).
/// The race of the test above leaves the primary Merk of `t` and its
/// count secondary with roots, entered at their places under an element
/// that names none. The element of `t` is the root of its Merk at the
/// race. A tree `u` and an item `v` are inserted beside `t` afterwards,
/// which turns the root Merk: the element of `u` is its root then, and
/// the node above the element of `t`. A partial batch that pauses below
/// `t` then puts a child into `t` and writes an item over `u`, whose
/// subtree leaves with its first batch. The node above is left out of
/// what that batch puts again, and the store holds the element of `t`
/// alone: it takes a Merk that is entered under the element at its
/// place with a root for one that the cell names, and here the cell
/// names none and grows by two pointers with nothing in the write to
/// carry it. In this history the page has room, and the batch is taken;
/// RocksDB takes the same calls in the order they took effect. In a
/// page with no room for the pointers and with nothing below the
/// element, the tree pages refuse that first batch and nothing is
/// written, where RocksDB takes it (`engine.md`, "An element written
/// over a tree").
///
/// `GROVE-03-F3` turns it: with a read of how many Merks a cell names,
/// the element is not held alone here.
#[test]
fn a_partial_batch_under_an_element_that_the_race_left_naming_none_of_its_merks() {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_storage::prefix::secondary_prefix_for;

    use super::indexed::{entry, Count, CountSum, Indexed};
    use crate::batch::BatchApplyOptions;

    let v = version();
    let u: &[&[u8]] = &[b"u"];
    fn trees<E: Engine>(db: &crate::GroveDb<E>, v: &GroveVersion) {
        (db.insert(ROOT, b"t", CountSum::empty(), None, None, v))
            .unwrap()
            .expect("an indexed tree");
        (CountSum::insert(db, T, b"x", CountSum::leaf(1), None, v).unwrap()).expect("a child");
        (CountSum::delete(db, T, b"x", None, v).unwrap()).expect("the child is deleted");
    }
    fn child<E: Engine>(db: &crate::GroveDb<E>, v: &GroveVersion) {
        (CountSum::insert(db, T, b"y", CountSum::leaf(2), None, v).unwrap()).expect("a child");
    }
    // The keys `t`, `u` and `v` in that order: the Merk turns with the
    // third, and the element of `u` is its root, above the one of `t`.
    fn above<E: Engine>(db: &crate::GroveDb<E>, v: &GroveVersion) {
        let u: &[&[u8]] = &[b"u"];
        (db.insert(ROOT, b"u", Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree");
        (db.insert(u, b"k", item(1), None, None, v))
            .unwrap()
            .expect("an element of it");
        (db.insert(ROOT, b"v", item(2), None, None, v))
            .unwrap()
            .expect("an item");
    }
    // The partial batch, which pauses below `t`, and what is read after
    // it: the child, and the item that is written over `u`.
    fn partial<E: Engine>(db: &crate::GroveDb<E>, v: &GroveVersion) -> (Element, Element) {
        let ops = vec![insert(T, b"z", Count::leaf(3)), insert(&[], b"u", item(7))];
        let paused = Some(BatchApplyOptions {
            batch_pause_height: Some(1),
            ..Default::default()
        });
        let written = db.apply_partial_batch(ops, paused, |_cost, _left| Ok(vec![]), None, v);
        written.unwrap().expect("the partial batch is taken");
        let z = (db.get(T, b"z", None, v).unwrap()).expect("the child");
        let over = (db.get(ROOT, b"u", None, v).unwrap()).expect("the item");
        (z, over)
    }

    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    trees(&*db, v);
    let (other, done) = beside(&db, move |db| child(db, v));
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other)].into());
    let applied = db.apply_batch(vec![insert(&[], b"t", Count::empty())], None, None, v);
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
    assert!(done.get());
    applied.unwrap().expect("the batch is applied");
    assert_eq!(REFUSED.take().len(), 0);
    // The two Merks that the new element hosts have roots, each entered
    // at its place under the element, whose node names none.
    let primary = build_prefix(T.into()).value;
    let secondary = secondary_prefix_for(&primary, IndexAxis::Count.tag()).value;
    for (place, prefix) in [primary, secondary].into_iter().enumerate() {
        let entered = entry(&db, prefix).expect("the Merk is entered");
        assert!(entered.root_key.is_some(), "{place}");
        let host = entered.host.expect("a host");
        assert_eq!((host.subtree, host.key.as_slice()), ([0; 32], &b"t"[..]));
        assert_eq!(usize::from(host.position), place);
    }
    above(&*db, v);
    assert_eq!(
        entry(&db, build_prefix(u.into()).value).map(|_| ()),
        Some(())
    );
    let on_lignin = partial(&*db, v);
    assert_eq!(on_lignin, (Count::leaf(3), item(7)));
    assert_eq!(entry(&db, build_prefix(u.into()).value), None);
    db.engine().database().unchecked_at_close();
    drop(Arc::into_inner(db).expect("the only handle on the grove"));

    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let db = crate::GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path());
        let db = db.expect("a database on RocksDB");
        trees(&db, v);
        child(&db, v);
        let over = db.apply_batch(vec![insert(&[], b"t", Count::empty())], None, None, v);
        over.unwrap().expect("the batch is applied");
        above(&db, v);
        assert_eq!(partial(&db, v), on_lignin);
    }
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

/// A group writes an item over a tree with an element in it. Its look
/// finds the subtree under the tree's node, and states the drop under
/// that node (`lignindb::txn::SubtreeWrite::DropUnder`). Before the
/// group is applied another writer writes an item over the tree, which
/// drops the subtree, and puts a record by the tree's prefix, which
/// enters the prefix under no node. The transaction refuses the drop of
/// a subtree that does not hang under the node the drop names, the
/// engine looks again, finds no subtree under the element, and the group
/// is applied: the entry under no node stays, with its record.
#[test]
fn a_tree_whose_prefix_is_entered_under_no_node_after_the_look_is_looked_for_again() {
    let v = version();
    let (dir, db) = grove(true);
    let prefix = build_prefix(T.into()).unwrap();
    let (other, done) = beside(&db, move |db| {
        (db.apply_batch(vec![insert(&[], b"t", item(3))], None, None, v))
            .unwrap()
            .expect("the other writer's item over the tree");
        let engine = db.engine();
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree_by_prefix(prefix, Some(&batch), &tx).unwrap();
        (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    });
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other), apart(Box::new(|| ()))].into());
    let applied = db.apply_batch(vec![insert(&[], b"t", item(2))], None, None, v);
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked twice");
    assert!(done.get());
    applied.unwrap().expect("the group is applied");
    let refused = REFUSED.take();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].contains("is dropped under a node"),
        "{refused:?}"
    );
    {
        let engine = db.engine();
        let tx = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
        let entry = handle.entry().expect("the directory answers");
        let entry = entry.expect("the entry under no node stays");
        assert!(entry.host.is_none(), "{entry:?}");
        let record = handle.aux().get(b"k").unwrap().expect("a read");
        assert_eq!(record.as_deref(), Some(b"v".as_slice()));
    }
    let read = (db.get(ROOT, b"t", None, v).unwrap()).expect("an element");
    assert_eq!(read, item(2));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A group writes an empty indexed tree over a tree with an element in
/// it. Its look finds the subtree under the tree's node, with records
/// that the group does not remove, and states its drop under that node.
/// Before the group is applied another writer writes an item over the
/// tree, which drops the subtree, and puts a record by the tree's prefix,
/// which enters the prefix under no node. The transaction refuses the
/// drop of a subtree that does not hang under the node, the engine looks
/// again, and the group is applied: nothing of the entry under no node is
/// dropped, and its record stays.
#[test]
fn an_empty_indexed_tree_over_a_tree_whose_prefix_hangs_under_no_node_since_the_look_takes_nothing()
{
    use super::indexed::{Indexed, Sum};

    let v = version();
    let (dir, db) = grove(true);
    let prefix = build_prefix(T.into()).unwrap();
    let (other, done) = beside(&db, move |db| {
        (db.apply_batch(vec![insert(&[], b"t", item(3))], None, None, v))
            .unwrap()
            .expect("the other writer's item over the tree");
        let engine = db.engine();
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree_by_prefix(prefix, Some(&batch), &tx).unwrap();
        (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    });
    REFUSED.take();
    AFTER_A_LOOK.set([apart(other), apart(Box::new(|| ()))].into());
    let applied = db.apply_batch(vec![insert(&[], b"t", Sum::empty())], None, None, v);
    assert!(AFTER_A_LOOK.take().is_empty(), "the group looked twice");
    assert!(done.get());
    applied.unwrap().expect("the group is applied");
    let refused = REFUSED.take();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].contains("is dropped under a node"),
        "{refused:?}"
    );
    {
        let engine = db.engine();
        let tx = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
        let entry = handle.entry().expect("the directory answers");
        let entry = entry.expect("the entry under no node stays");
        assert!(entry.host.is_none(), "{entry:?}");
        let record = handle.aux().get(b"k").unwrap().expect("a read");
        assert_eq!(record.as_deref(), Some(b"v".as_slice()));
    }
    let read = (db.get(ROOT, b"t", None, v).unwrap()).expect("an element");
    assert_eq!(read, Sum::empty());
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

/// A group writes a tree as an empty one in its first batch and removes
/// the tree's one element in its second, as the two parts of a batch do
/// that writes an empty tree over a filled one and removes the old
/// records itself. The Merk is written after the last write of its
/// element, and the group is taken all the same: the element names no
/// root, no batch writes the Merk before it and the second batch states
/// no root for it, so no root is left that a node would have to name.
/// The node of the
/// element is written as one that hosts nothing, and the Merk stays
/// entered with no node. So it is where the first batch writes the root
/// Merk through a handle by prefix, which tells the engine no path, and
/// the look for what is written over says nothing of the tree.
///
/// Where the tree holds a second element, the second batch leaves the
/// Merk with a root that no node would name: the group is refused as
/// not supported, and nothing of it is applied. So is a group whose
/// first batch writes a node of the Merk beside the element.
#[test]
fn a_group_that_empties_a_tree_after_the_batch_that_writes_its_empty_element_is_taken() {
    use grovedb_merk::tree::hash::NULL_HASH;

    let v = version();
    let cases = [
        (false, true, false),
        (true, true, false),
        (false, false, false),
        (true, false, false),
        (false, true, true),
    ];
    for (by_prefix, emptied, early) in cases {
        let (dir, db) = grove(true);
        if !emptied {
            (db.insert(T, b"y", item(2), None, None, v))
                .unwrap()
                .expect("a second element in the tree");
        }
        let root_key = match db.get(ROOT, b"t", None, v).unwrap() {
            Ok(Element::Tree(root_key, _)) => root_key,
            other => panic!("{other:?}"),
        };
        let engine = db.engine();
        let tx = engine.start_transaction();
        let mut group = <LigninEngine as Engine>::WriteGroup::default();

        let batch = <LigninEngine as Engine>::Batch::default();
        let root = match by_prefix {
            // The root Merk is at the prefix of no path.
            true => engine.subtree_by_prefix([0; 32], Some(&batch), &tx),
            false => engine.subtree(ROOT, Some(&batch), &tx),
        };
        let mut merk = Merk::open_base(root.unwrap(), TreeType::NormalTree, NO_COST, v)
            .unwrap()
            .expect("the root Merk");
        // The value hash of a tree with no node, as GroveDB makes it.
        let empty = (Element::empty_tree().serialize(v)).expect("the bytes of an element");
        let put = Op::PutCombinedReference(empty, NULL_HASH, TreeFeatureType::BasicMerkNode);
        (merk.apply::<_, Vec<u8>>(&[(b"t".to_vec(), put)], &[], None, v))
            .unwrap()
            .expect("a write of the root Merk");
        drop(merk);
        if early {
            let mut merk = {
                let below = engine.subtree(T.into(), Some(&batch), &tx).unwrap();
                let root_key = root_key.clone();
                Merk::open_layered_with_root_key(below, root_key, TreeType::NormalTree, NO_COST, v)
                    .unwrap()
                    .expect("the Merk of the tree")
            };
            let more = item(2).serialize(v).expect("the bytes of an element");
            let put = Op::Put(more, TreeFeatureType::BasicMerkNode);
            (merk.apply::<_, Vec<u8>>(&[(b"y".to_vec(), put)], &[], None, v))
                .unwrap()
                .expect("a write of the tree beside its element");
        }
        (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");

        let batch = <LigninEngine as Engine>::Batch::default();
        let mut merk = {
            let below = engine.subtree(T.into(), Some(&batch), &tx).unwrap();
            let root_key = root_key.clone();
            Merk::open_layered_with_root_key(below, root_key, TreeType::NormalTree, NO_COST, v)
                .unwrap()
                .expect("the Merk of the tree")
        };
        (merk.apply::<_, Vec<u8>>(&[(b"x".to_vec(), Op::Delete)], &[], None, v))
            .unwrap()
            .expect("the removal of the tree's element");
        drop(merk);
        (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");

        let case = format!("by prefix: {by_prefix}, emptied: {emptied}, early: {early}");
        let applied = engine.apply_write_group(group, &tx).unwrap();
        if emptied && !early {
            applied.expect("the group is applied");
            (engine.commit_transaction(tx).unwrap()).expect("a commit");
            assert_eq!(
                entry_of_t(&db),
                Some((TreeType::NormalTree, false)),
                "{case}"
            );
            assert_eq!(
                db.get(ROOT, b"t", None, v).unwrap().ok(),
                Some(Element::empty_tree())
            );
            assert!(db.get(T, b"x", None, v).unwrap().is_err());
        } else {
            let said = match applied.expect_err("a root that no node names") {
                StorageError::NotSupported(said) => said,
                other => panic!("{case}: {other}"),
            };
            assert!(
                said.contains("in a batch after the last one that writes the tree's element"),
                "{case}: {said}"
            );
            drop(tx);
            assert_eq!(
                entry_of_t(&db),
                Some((TreeType::NormalTree, true)),
                "{case}"
            );
            for (key, byte) in [(b"x", 1), (b"y", 2)] {
                let kept = db.get(T, key, None, v).unwrap().ok();
                let held = (key == b"x" || !emptied).then(|| item(byte));
                assert_eq!(kept, held, "{case}");
            }
        }
        drop(Arc::into_inner(db).expect("the only handle on the grove"));
        check(dir.path());
    }
}

/// An empty indexed tree is written over a tree with an element, whose
/// records the batch leaves: the subtree of the old tree leaves with the
/// batch. A batch that also holds a write under the key, of a record of
/// the new tree's primary or of one of its secondaries, is refused as not
/// supported, and nothing of it is applied: what it writes would go into
/// a subtree that leaves, or beside one. GroveDB makes no such batch.
#[test]
fn an_empty_indexed_tree_over_a_filled_tree_is_refused_beside_a_write_under_its_key() {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_storage::prefix::secondary_prefix_for;

    use super::indexed::{Indexed, Sum};

    let v = version();
    let (dir, db) = grove(true);
    let engine = db.engine();
    let own = build_prefix(T.into()).value;
    let secondary = secondary_prefix_for(&own, IndexAxis::Sum.tag()).value;
    for beside_the_primary in [false, true] {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let root = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
        let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
            .unwrap()
            .expect("the root Merk");
        let indexed = (Sum::empty().serialize(v)).expect("the bytes of an element");
        let put = Op::Put(indexed, TreeFeatureType::BasicMerkNode);
        (merk.apply::<_, Vec<u8>>(&[(b"t".to_vec(), put)], &[], None, v))
            .unwrap()
            .expect("a write of the root Merk");
        drop(merk);
        let named = match beside_the_primary {
            true => engine.subtree_beside(T.into(), secondary, Some(&batch), &tx),
            false => engine.subtree(T.into(), Some(&batch), &tx),
        };
        let named = named.unwrap();
        (named.aux().put(b"k", b"v", None).unwrap()).expect("staging");
        drop(named);
        let refused = engine.apply_batch(batch, &tx).unwrap();
        let said = match refused.expect_err("a write under the key of a tree that leaves") {
            StorageError::NotSupported(said) => said,
            other => panic!("{other}"),
        };
        assert!(
            said.contains("in a group that writes under that key"),
            "{said}"
        );
        drop(tx);
        assert_eq!(entry_of_t(&db), Some((TreeType::NormalTree, true)));
    }
    let kept = (db.get(T, b"x", None, v).unwrap()).expect("the element of the tree");
    assert_eq!(kept, item(1));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// An empty count-indexed tree is written over a sum tree that was
/// written into and emptied, whose subtree is entered with no node. The
/// subtree stays, and is stated anew with the batch as the primary of
/// the new tree. A record that is then written through the handle of
/// the count secondary enters that secondary under the element, and the
/// delete of the tree takes both out of the directory.
#[test]
fn an_empty_indexed_tree_over_an_emptied_tree_takes_its_subtree_as_its_primary() {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_storage::prefix::secondary_prefix_for;

    use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

    use super::indexed::{Count, Indexed};

    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    (db.insert(ROOT, b"t", Element::empty_sum_tree(), None, None, v))
        .unwrap()
        .expect("a sum tree");
    (db.insert(T, b"x", Element::new_sum_item(3), None, None, v))
        .unwrap()
        .expect("an element in the tree");
    (db.delete(T, b"x", None, None, v).unwrap()).expect("the tree is emptied");
    assert_eq!(entry_of_t(&db), Some((TreeType::SumTree, false)));
    (db.apply_batch(vec![insert(&[], b"t", Count::empty())], None, None, v))
        .unwrap()
        .expect("the indexed tree is written over the emptied tree");
    let primary = Count::empty().tree_type().expect("a tree");
    assert_eq!(entry_of_t(&db), Some((primary, false)));

    let engine = db.engine();
    let own = build_prefix(T.into()).value;
    let secondary = secondary_prefix_for(&own, IndexAxis::Count.tag()).value;
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let named = engine
        .subtree_beside(T.into(), secondary, Some(&batch), &tx)
        .unwrap();
    (named.aux().put(b"k", b"v", None).unwrap()).expect("staging");
    drop(named);
    (engine.apply_batch(batch, &tx).unwrap()).expect("the record is taken");
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
    let entered = |prefix: [u8; 32]| {
        let tx = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
        handle.entry().expect("the directory answers").is_some()
    };
    assert!(entered(own) && entered(secondary));
    (db.delete(ROOT, b"t", None, None, v).unwrap()).expect("the tree is deleted");
    assert!(!entered(own) && !entered(secondary));
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A batch at the seam writes an empty indexed tree over a tree in the
/// tree `t` and removes the node of `t`. The write of the element is
/// left out with everything in the subtree of `t`, which leaves, and
/// the look for what is written over says nothing of the indexed tree:
/// no statement anew of a subtree that leaves with the drop above, and
/// no refusal on account of a write that is not applied. The group is
/// applied, where the tree under the element was written into and
/// emptied and where it is a filled indexed tree with the axis of the
/// new one.
#[test]
fn an_indexed_tree_written_below_a_removed_node_goes_with_the_drop_above() {
    use super::indexed::{Count, Indexed};

    let v = version();
    let k: &[&[u8]] = &[b"t", b"k"];
    for filled_index in [false, true] {
        let (dir, db) = grove(true);
        let old = if filled_index {
            Count::empty()
        } else {
            Element::empty_tree()
        };
        (db.insert(T, b"k", old, None, None, v))
            .unwrap()
            .expect("a tree in the tree");
        if filled_index {
            (Count::insert(&*db, k, b"y", Count::leaf(1), None, v).unwrap()).expect("a child");
        } else {
            (db.insert(k, b"y", item(2), None, None, v))
                .unwrap()
                .expect("an element in it");
            (db.delete(k, b"y", None, None, v).unwrap()).expect("the tree is emptied");
        }
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
                let bytes = Count::empty()
                    .serialize(v)
                    .expect("the bytes of an element");
                let over = Op::Put(bytes, TreeFeatureType::BasicMerkNode);
                (merk.apply::<_, Vec<u8>>(&[(b"k".to_vec(), over)], &[], None, v))
                    .unwrap()
                    .expect("an indexed tree over the tree in it");
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
            assert!(!entered(T) && !entered(k), "a filled index: {filled_index}");
            (engine.commit_transaction(tx).unwrap()).expect("a commit");
        }
        assert!(db.get_raw(ROOT, b"t", None, v).unwrap().is_err());
        drop(Arc::into_inner(db).expect("the only handle on the grove"));
        check(dir.path());
    }
}

/// A batch at the seam writes an empty indexed tree over a tree with an
/// element, and drops the subtree of that tree itself. The look for
/// what is written over does not judge that subtree, which the batch's
/// own drop takes, and has the node of the element written as one that
/// hosts nothing: the batch is applied, and the subtree is entered no
/// more.
///
/// Where the old tree is a filled indexed tree with the axis of the new
/// one, and the batch drops its primary alone, the secondary keeps its
/// rows: the batch is refused as not supported, and nothing of it is
/// applied.
#[test]
fn an_indexed_tree_written_over_a_tree_whose_subtree_the_batch_drops_itself_is_applied() {
    use grovedb_merk::{store::lignin::LigninSubtree, tree::hash::NULL_HASH};

    use super::indexed::{Count, Indexed};

    let v = version();
    let (dir, db) = grove(true);
    assert_eq!(entry_of_t(&db), Some((TreeType::NormalTree, true)));
    let counted: &[&[u8]] = &[b"counted"];
    (db.insert(ROOT, b"counted", Count::empty(), None, None, v))
        .unwrap()
        .expect("an indexed tree");
    (Count::insert(&*db, counted, b"y", Count::leaf(1), None, v).unwrap()).expect("a child");
    let empty = Count::empty()
        .serialize(v)
        .expect("the bytes of an element");
    {
        let engine = db.engine();
        // The value hash of an indexed tree with no node in either of
        // its two Merks, as GroveDB makes it.
        fn over(
            engine: &LigninEngine,
            batch: &<LigninEngine as Engine>::Batch,
            tx: &<LigninEngine as Engine>::Transaction<'_>,
            (key, empty): (&[u8], &[u8]),
            v: &GroveVersion,
        ) {
            let root = engine.subtree(ROOT, Some(batch), tx).unwrap();
            let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
                .unwrap()
                .expect("the root Merk");
            let cost = u32::try_from(empty.len()).expect("a short element");
            let feature = TreeFeatureType::BasicMerkNode;
            let put = Op::PutLayeredCountIndexedReference(
                empty.to_vec(),
                cost,
                NULL_HASH,
                NULL_HASH,
                feature,
            );
            (merk.apply::<_, Vec<u8>>(&[(key.to_vec(), put)], &[], None, v))
                .unwrap()
                .expect("an indexed tree over the tree");
        }
        {
            let tx = engine.start_transaction();
            let batch = <LigninEngine as Engine>::Batch::default();
            over(engine, &batch, &tx, (b"counted", &empty), v);
            batch.drop_subtree(&LigninSubtree::alone(build_prefix(counted.into()).value));
            let refused = engine.apply_batch(batch, &tx).unwrap();
            let said = match refused.expect_err("the rows of the secondary stay") {
                StorageError::NotSupported(said) => said,
                other => panic!("{other}"),
            };
            assert!(
                said.contains("holds rows that the batch does not remove"),
                "{said}"
            );
        }
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        over(engine, &batch, &tx, (b"t", &empty), v);
        batch.drop_subtree(&LigninSubtree::alone(build_prefix(T.into()).value));
        (engine.apply_batch(batch, &tx).unwrap()).expect("the batch is applied");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    }
    assert_eq!(entry_of_t(&db), None);
    assert_eq!(
        db.get(ROOT, b"t", None, v).unwrap().ok(),
        Some(Count::empty())
    );
    (Count::insert(&*db, counted, b"z", Count::leaf(2), None, v).unwrap())
        .expect("the filled tree takes a child");
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// D-277 is of a subtree whose key holds an element. Where a key holds
/// none, a record that a handle by prefix wrote entered the subtree of
/// its path under no element, and a record written by that path
/// afterwards finds no element to host the subtree either: it goes into
/// the subtree as it is entered, as on RocksDB.
#[test]
fn a_record_by_the_path_of_a_key_with_no_element_goes_into_the_subtree_under_no_element() {
    let (dir, db) = grove(false);
    let engine = db.engine();
    let at: &[&[u8]] = &[b"none"];
    let prefix = build_prefix(at.into()).value;
    for by_path in [false, true] {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = match by_path {
            true => engine.subtree(at.into(), Some(&batch), &tx),
            false => engine.subtree_by_prefix(prefix, Some(&batch), &tx),
        };
        let handle = handle.unwrap();
        let key: &[u8] = if by_path { b"by path" } else { b"by prefix" };
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("the record is taken");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    }
    {
        let tx = engine.start_read_transaction();
        let read = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
        for key in [&b"by path"[..], b"by prefix"] {
            let record = read.aux().get(key).unwrap().expect("a read");
            assert_eq!(record.as_deref(), Some(b"v".as_slice()));
        }
        let entry = read.entry().expect("the directory answers");
        assert_eq!(entry.expect("entered").host, None);
    }
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

/// A tree with an element changes places with a tree of leaves that
/// nothing was appended to. After both walks, while the group is looked
/// for, a commit beside the mover puts a record that is no leaf under
/// the prefix of the tree of leaves, through its path: the look for what
/// is moved finds the subtree of that tree entered where the moved one
/// goes, with no record in its data keyspace. It makes no room: the
/// transaction refuses the batch as a conflict, since a commit wrote
/// under that prefix after the mover's read, and both trees are where
/// they were, with the record. No operation of GroveDB puts such a
/// record. One that is put before the walk of the tree of leaves ends is
/// found by that walk, and goes with the tree.
#[test]
fn leaves_with_no_leaf_that_a_commit_entered_where_a_moved_tree_goes_make_no_room() {
    use crate::batch::QualifiedGroveDbOp;

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
    let swap = vec![
        QualifiedGroveDbOp::move_op(Vec::new(), b"t".to_vec(), b"u".to_vec()),
        QualifiedGroveDbOp::move_op(Vec::new(), b"u".to_vec(), b"t".to_vec()),
    ];
    REFUSED.take();
    // The walk of each tree reads its entry where it begins, and the
    // look for what the group enters reads an entry after them.
    AFTER_ENTRY_READS.set(Some((3, other)));
    let swapped = db.apply_batch(swap, None, None, v).unwrap();
    assert!(AFTER_ENTRY_READS.take().is_none());
    assert!(done.get(), "the other writer came after the walks");
    let refused = swapped.expect_err("a subtree is entered where the moved one goes");
    assert!(refused.is_commit_conflict(), "{refused}");
    assert_eq!(REFUSED.take().len(), 1);
    let held = db.get(T, b"x", None, v).unwrap();
    assert_eq!(held.expect("the tree is where it was"), item(1));
    let leaves = db.mmr_tree_leaf_count(ROOT, b"u", None, v).unwrap();
    assert_eq!(leaves.expect("the tree of leaves is where it was"), 0);
    {
        let engine = db.engine();
        let tx = engine.start_read_transaction();
        let handle = engine.subtree(u.into(), None, &tx).unwrap();
        let entry = handle.entry().expect("the directory answers");
        let entry = entry.expect("the subtree is entered");
        assert_eq!(entry.tree_type().expect("a tree type"), TreeType::MmrTree);
        let record = handle.aux().get(b"k").unwrap().expect("a read");
        assert_eq!(record.as_deref(), Some(b"v".as_slice()));
    }
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
/// into and emptied, whose Merk makes room: the look states its drop,
/// under the node it found the subtree under and dated at the mover's
/// read of that node (`lignindb::txn::SubtreeWrite::DropUnder`). After
/// the look a commit beside the mover writes a tree of leaves over that
/// tree and appends to it, which writes the node. The batch is refused
/// as a conflict and nothing of it is applied: both trees are where they
/// were, and the other writer's tree of leaves has its leaf. RocksDB has
/// no such refusal: it takes the batch, and leaves the leaf under the
/// prefix that the moved tree's records go to.
#[test]
fn a_commit_that_writes_the_node_where_room_was_stated_after_the_look_is_a_conflict() {
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
    let refused = swapped
        .unwrap()
        .expect_err("the node was written since the mover read it");
    assert!(refused.is_commit_conflict(), "{refused}");
    assert_eq!(REFUSED.take().len(), 1);
    let held = db.get(T, b"x", None, v).unwrap();
    assert_eq!(held.expect("the tree where it was"), item(1));
    let leaves = db.mmr_tree_leaf_count(root, b"u", None, v).unwrap();
    assert_eq!(leaves.expect("the other writer's tree of leaves"), 1);
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
/// record that the other writer's commit put there, with its bytes, as
/// RocksDB's cursor over the keyspace does: the walk comes to its end and
/// the batch is told of the move. A tree that nothing was appended to,
/// with no writer beside the mover, is told to the batch as a walk that
/// found none.
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
    // The records of the tree with its one leaf.
    assert!(!walked.is_empty(), "the walk shows the flat records");
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

/// A tree with an element changes places with an item. After the mover
/// read the item, a commit beside it writes a tree over the item, puts
/// an element into that tree and deletes it again: a Merk with no node
/// is entered where the moved tree goes, and makes room. The mover's
/// handle read no tree under that key, so the drop of that Merk has no
/// read of the node to be dated at, and the look refuses the group as a
/// conflict. Nothing of it is applied and nothing is handed to the
/// transaction: the moved tree is where it was, and so is the other
/// writer's tree. The commit comes after the walk of the moved tree read
/// its entry, and inside the group's application after the look for
/// what is entered read one: the two reads of an entry that are counted
/// here, both before the look for what is moved.
#[test]
fn a_tree_made_where_a_moved_tree_goes_since_the_mover_read_an_item_there_is_a_conflict() {
    use crate::batch::QualifiedGroveDbOp;

    let v = version();
    let root: &[&[u8]] = &[];
    for reads in [1, 2] {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
        (db.insert(root, b"u", item(2), None, None, v))
            .unwrap()
            .expect("an item");
        (db.insert(root, b"t", Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree");
        (db.insert(T, b"x", item(1), None, None, v))
            .unwrap()
            .expect("an element in the tree");
        let (other, done) = beside(&db, move |db| {
            let u: &[&[u8]] = &[b"u"];
            let over = vec![insert(&[], b"u", Element::empty_tree())];
            (db.apply_batch(over, None, None, v).unwrap()).expect("a tree over the item");
            (db.insert(u, b"y", item(3), None, None, v))
                .unwrap()
                .expect("an element in it");
            (db.delete(u, b"y", None, None, v))
                .unwrap()
                .expect("the tree is emptied");
        });
        let swap = vec![
            QualifiedGroveDbOp::move_op(Vec::new(), b"t".to_vec(), b"u".to_vec()),
            QualifiedGroveDbOp::move_op(Vec::new(), b"u".to_vec(), b"t".to_vec()),
        ];
        REFUSED.take();
        AFTER_ENTRY_READS.set(Some((reads, other)));
        let swapped = db.apply_batch(swap, None, None, v);
        assert!(AFTER_ENTRY_READS.take().is_none(), "{reads}: the read came");
        assert!(done.get(), "{reads}");
        let refused = swapped
            .unwrap()
            .expect_err("the node was made a tree since the mover read it");
        assert!(refused.is_commit_conflict(), "{reads}: {refused}");
        assert_eq!(REFUSED.take(), Vec::<String>::new(), "{reads}");
        let held = db.get(T, b"x", None, v).unwrap();
        assert_eq!(held.expect("the tree where it was"), item(1), "{reads}");
        let made = db.get(root, b"u", None, v).unwrap();
        let made = made.expect("the other writer's tree");
        assert_eq!(made, Element::empty_tree(), "{reads}");
        drop(Arc::into_inner(db).expect("the only handle on the grove"));
        check(dir.path());
    }
}
