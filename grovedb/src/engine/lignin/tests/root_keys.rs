//! The root-key record of a secondary of an indexed tree, on both
//! engines (D-289).
//!
//! GroveDB opens a secondary with the root that the tree's element names
//! for its axis, or, where the element names none, as a base Merk, which
//! reads the root-key record of the roots keyspace under the secondary's
//! prefix. The commit of a base Merk writes the record, and so does a
//! restore; the commit of a Merk that is opened with a root key writes
//! none. So on RocksDB an insert into a secondary writes the record while
//! the element names no root for it, and no insert or delete does while
//! the element names one; a delete of the tree and a move of the tree
//! leave the record under the prefix, a move brings none to the new
//! prefix, and every later open of a secondary under the prefix whose
//! element names no root reads it: a seek, the bytes of the key in it,
//! and in these histories a seek for the node it names, which is gone.
//!
//! The LigninDB engine keeps that record by the prefix, outside the
//! subtree (`engine.md`, "The root-key record of a subtree"), so that
//! the calls of these histories cost on it what they cost on RocksDB.
//!
//! A history is a function that returns its calls with their answers,
//! and the macro `alike!` runs it on both engines and compares the two
//! call for call. A read and a write of GroveDB are taken in with the
//! answer and the `OperationCost`, a write also with the root hash after
//! it. Other calls are taken in with what they return alone, with no
//! cost and no root hash: the grove's own check at the end of each
//! history; in the history of a flat drop the commit of its transaction
//! and the flush of the pending drops; in the history of the namespace
//! the drain of its prefix, which is a call of the seam; and the reads
//! of the store at the seam, which are what the store holds under the
//! four prefixes of a key ([`left`]) and, in the history of the
//! namespace, what is under its prefix. The four records that the
//! history of the namespace writes under that prefix at the seam before
//! the drain are none of its calls. A
//! history that moves a tree or drops one flat runs on the grove
//! versions that do ([`from_the_fourth`]), and the others on every grove
//! version. The tests of a synced grove, of a checkpoint and of a wipe
//! compare groves on the two engines by the same reads, on every grove
//! version. Two tests are of the LigninDB engine alone: one reads the
//! store with no handle of the engine, where a handle of each kind
//! keeps the record, and one applies a group in which a handle stages
//! its record and nothing else.

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::store::NodeStore;
use grovedb_storage::prefix::{build_prefix, secondary_prefix_for};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{self, verified, Answer, History},
    indexed::{axis_reads, Count, CountSum, Indexed, Sum, SumAndAverage},
    Lignin, LigninEngine,
};
use crate::{
    batch::QualifiedGroveDbOp, engine::Engine, operations::delete::DeleteOptions,
    BackwardsReferences, Element, Error, GroveDb,
};

/// The numbers of the grove versions that move a tree and that drop one
/// flat: the fourth and those after it.
fn from_the_fourth() -> std::ops::RangeInclusive<usize> {
    4..=GROVE_VERSIONS.len()
}

/// The number of every grove version.
fn every() -> std::ops::RangeInclusive<usize> {
    1..=GROVE_VERSIONS.len()
}

const P: &[&[u8]] = &[b"p"];
const IDX: &[&[u8]] = &[b"p", b"idx"];
const AWAY: &[&[u8]] = &[b"p", b"away"];

fn leaf_key(n: usize) -> Vec<u8> {
    format!("leaf{n:02}").into_bytes()
}

/// The move of the element under `key` of the tree `p` to the key `to`.
fn op_move(key: &[u8], to: &[u8]) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::move_op(vec![b"p".to_vec()], key.to_vec(), to.to_vec())
}

/// A delete of a tree with what it holds.
fn all() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

/// What [`left`] says of a root-key record that names a node which the
/// store does not hold.
const GONE: &str = "whose node is gone";

/// What the store holds under the prefix of the path `at` and of the
/// three secondaries beside it, read at the seam through handles by
/// prefix: for each, the root-key record, and whether the node it names
/// is there.
fn left<E: Engine>(db: &GroveDb<E>, at: &[&[u8]]) -> String {
    let engine = db.engine();
    let tx = engine.start_read_transaction();
    let own = build_prefix(at.into()).value;
    let mut prefixes = vec![("primary", own)];
    for (name, axis) in [
        ("count", IndexAxis::Count),
        ("sum", IndexAxis::Sum),
        ("avg", IndexAxis::Avg),
    ] {
        prefixes.push((name, secondary_prefix_for(&own, axis.tag()).value));
    }
    let mut said = Vec::new();
    for (name, prefix) in prefixes {
        let handle = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
        let record = match handle.root_key().unwrap().expect("a read of the record") {
            Some(key) => {
                let node = handle.has_node(&key).unwrap().expect("a read of the store");
                match node {
                    true => format!("a record of {} bytes", key.len()),
                    false => format!("a record of {} bytes, {GONE}", key.len()),
                }
            }
            None => "no record".to_owned(),
        };
        said.push(format!("{name}: {record}"));
    }
    said.join(" | ")
}

/// Takes in what the store holds under the prefixes of `at`.
fn records<E: Engine>(h: &mut History<'_, E>, when: &str, at: &[&[u8]]) {
    let said = left(h.db, at);
    h.plain(format!("records {when}"), Ok::<_, Error>(said));
}

/// The tree `p`, and in it the indexed tree `idx` of the kind `M` with
/// the rows `rows`.
fn with_rows<M: Indexed, E: Engine>(h: &mut History<'_, E>, rows: std::ops::Range<usize>) {
    let (db, v) = (h.db, h.v);
    let root: &[&[u8]] = &[];
    (h.write(
        "made",
        db.insert(root, b"p", Element::empty_tree(), None, None, v),
    ))
    .expect("a tree");
    h.write("made", db.insert(P, b"idx", M::empty(), None, None, v))
        .expect("an indexed tree");
    for n in rows {
        let inserted = M::insert(db, IDX, &leaf_key(n), M::leaf(n), None, v);
        h.write("insert", inserted).expect("a row");
    }
}

/// Every read of the axes of the tree at `at`, an insert of the row `n`
/// into it, and what the store holds under its prefixes afterwards.
fn read_and_written<N: Indexed, E: Engine>(
    h: &mut History<'_, E>,
    at: &[&[u8]],
    n: usize,
    when: &str,
) {
    let (db, v) = (h.db, h.v);
    axis_reads::<N, E>(h, at);
    let inserted = N::insert(db, at, &leaf_key(n), N::leaf(n), None, v);
    h.write("insert", inserted).expect("a row");
    records(h, when, at);
}

/// An indexed tree of the kind `M` with rows is moved away from its key,
/// or changes places with a tree of the kind `N` that nothing was
/// written into, and a tree of the kind `N` is under the old key: read
/// while it is empty, written, read, emptied and written again. Then the
/// new tree is deleted and the first is moved back, or the two change
/// places again, and the first is read and written under its old key.
fn under_the_old_key<M: Indexed, N: Indexed, E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    swap: bool,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    with_rows::<M, E>(&mut h, 0..4);
    if swap {
        let made = db.insert(P, b"away", N::empty(), None, None, v);
        h.write("made", made)
            .expect("a tree nothing is written into");
    }
    records(&mut h, "before the move", IDX);
    let there_and_back = || match swap {
        false => vec![op_move(b"idx", b"away")],
        true => vec![op_move(b"idx", b"away"), op_move(b"away", b"idx")],
    };
    (h.write("moved", db.apply_batch(there_and_back(), None, None, v))).expect("the move");
    records(&mut h, "after the move away", IDX);
    if !swap {
        h.element("get", db.get(P, b"idx", None, v));
        (h.write("made", db.insert(P, b"idx", N::empty(), None, None, v))).expect("a new tree");
    }
    read_and_written::<N, E>(&mut h, IDX, 1, "after the first insert");
    read_and_written::<N, E>(&mut h, IDX, 2, "after the second insert");
    for n in [1, 2] {
        let deleted = N::delete(db, IDX, &leaf_key(n), None, v);
        h.write("delete", deleted).expect("a row less");
    }
    read_and_written::<N, E>(&mut h, IDX, 3, "after the tree was emptied and written");
    if swap {
        (h.write("moved", db.apply_batch(there_and_back(), None, None, v))).expect("the swap");
        read_and_written::<N, E>(&mut h, AWAY, 8, "of the other tree after the swap back");
    } else {
        (h.write("deleted", db.delete(P, b"idx", all(), None, v))).expect("the delete");
        records(&mut h, "after the delete of the new tree", IDX);
        let back = vec![op_move(b"away", b"idx")];
        (h.write("moved", db.apply_batch(back, None, None, v))).expect("the move back");
    }
    read_and_written::<M, E>(&mut h, IDX, 7, "after the first tree is back");
    verified(&mut h);
    h.answers
}

/// No move: an indexed tree with rows is deleted with them, and made
/// again under its key.
fn deleted_and_made_again<M: Indexed, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    with_rows::<M, E>(&mut h, 0..3);
    (h.write("deleted", db.delete(P, b"idx", all(), None, v))).expect("the delete");
    records(&mut h, "after the delete", IDX);
    (h.write("made", db.insert(P, b"idx", M::empty(), None, None, v))).expect("the tree again");
    read_and_written::<M, E>(&mut h, IDX, 5, "after the first insert");
    read_and_written::<M, E>(&mut h, IDX, 6, "after the second insert");
    verified(&mut h);
    h.answers
}

/// The moved tree is emptied and written again under its new key, where
/// RocksDB has no record of it, moved back, and emptied and written
/// again under its old key, where RocksDB has the record that its first
/// insert left. The rows it is given under the two keys have keys of
/// other lengths, so that the two records are of other lengths.
fn emptied_where_it_was_moved<M: Indexed, E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let root: &[&[u8]] = &[];
    (h.write(
        "made",
        db.insert(root, b"p", Element::empty_tree(), None, None, v),
    ))
    .expect("a tree");
    (h.write("made", db.insert(P, b"idx", M::empty(), None, None, v))).expect("an indexed tree");
    let long: &[u8] = b"the key of a row that is longer than the others";
    (h.write("insert", M::insert(db, IDX, long, M::leaf(0), None, v))).expect("a row");
    let moved = db.apply_batch(vec![op_move(b"idx", b"away")], None, None, v);
    h.write("moved", moved).expect("the move");
    records(&mut h, "under the old key after the move away", IDX);
    records(&mut h, "under the new key after the move away", AWAY);
    read_and_written::<M, E>(&mut h, AWAY, 3, "under the new key after an insert");
    for key in [long, leaf_key(3).as_slice()] {
        (h.write("delete", M::delete(db, AWAY, key, None, v))).expect("a row less");
    }
    records(&mut h, "under the new key, emptied", AWAY);
    read_and_written::<M, E>(&mut h, AWAY, 7, "under the new key, written again");
    let back = db.apply_batch(vec![op_move(b"away", b"idx")], None, None, v);
    h.write("moved", back).expect("the move back");
    records(&mut h, "under the old key after the move back", IDX);
    records(&mut h, "under the new key after the move back", AWAY);
    (h.write("delete", M::delete(db, IDX, &leaf_key(7), None, v))).expect("no row");
    read_and_written::<M, E>(&mut h, IDX, 8, "under the old key, written again");
    verified(&mut h);
    h.answers
}

/// An indexed tree in a tree that is moved: emptied and written again
/// under the new path, and a new one made under the old path.
fn in_a_moved_tree<M: Indexed, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let root: &[&[u8]] = &[];
    let moved: &[&[u8]] = &[b"q", b"idx"];
    with_rows::<M, E>(&mut h, 0..3);
    let above = QualifiedGroveDbOp::move_op(Vec::new(), b"p".to_vec(), b"q".to_vec());
    (h.write("moved", db.apply_batch(vec![above], None, None, v))).expect("the move");
    records(&mut h, "under the old path after the move", IDX);
    records(&mut h, "under the new path after the move", moved);
    for n in 0..3 {
        (h.write("delete", M::delete(db, moved, &leaf_key(n), None, v))).expect("a row less");
    }
    read_and_written::<M, E>(&mut h, moved, 7, "under the new path, written again");
    (h.write(
        "made",
        db.insert(root, b"p", Element::empty_tree(), None, None, v),
    ))
    .expect("a tree");
    (h.write("made", db.insert(P, b"idx", M::empty(), None, None, v))).expect("an indexed tree");
    read_and_written::<M, E>(&mut h, IDX, 9, "under the old path, a new tree written");
    verified(&mut h);
    h.answers
}

/// An indexed tree with rows is dropped flat in a transaction, and its
/// pending-drop record is drained after the commit, which removes what is
/// under its prefixes in every keyspace, the root-key records among it.
/// A tree that is made under the key afterwards reads none.
fn dropped_flat_and_drained<M: Indexed, E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    with_rows::<M, E>(&mut h, 0..3);
    records(&mut h, "before the drop", IDX);
    assert!(left(db, IDX).contains("a record of"));
    let unchecked = BackwardsReferences::DontCheck;
    let tx = db.start_transaction();
    let dropped = db.drop_flat_subtree(P, b"idx", unchecked, Some(&tx), v);
    (h.write_in("dropped", dropped, Some(&tx))).expect("the flat drop");
    h.plain("commit", db.commit_transaction(tx).unwrap());
    // Between the drop and its drain RocksDB still holds the nodes of
    // the dropped tree (`transactions.md` 14.5, D3): nothing is read of
    // the store there.
    let report = db.flush_pending_prefix_drops(v);
    h.plain("drained", report.map(|report| report.reclaimed_records));
    records(&mut h, "after the drain", IDX);
    assert!(
        !left(db, IDX).contains("a record of"),
        "the drain removed the records"
    );
    (h.write("made", db.insert(P, b"idx", M::empty(), None, None, v))).expect("the tree again");
    read_and_written::<M, E>(&mut h, IDX, 5, "after the first insert");
    verified(&mut h);
    h.answers
}

/// How many of `answers` are reads of the store that found a root-key
/// record whose node is gone.
fn stale(answers: &[Answer]) -> usize {
    let stale = |(call, _, said): &&Answer| call.starts_with("records") && said.contains(GONE);
    answers.iter().filter(stale).count()
}

/// Runs a history on both engines on the grove versions whose numbers
/// are `$versions` and compares the two, call for call. `$stale` is how
/// many of the history's reads of the store found a root-key record whose
/// node is gone: the state that the history is for.
macro_rules! alike {
    ($history:ident::<$($kind:ty),+>($($arg:expr),*), on: $versions:expr, stale: $stale:expr) => {{
        for number in $versions {
            let v = &GROVE_VERSIONS[number - 1];
            let dir = tempfile::tempdir().expect("a directory");
            let on_lignin = {
                let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                let answers = $history::<$($kind,)+ _>(&db, v $(, $arg)*);
                db.flush().expect("a flush");
                answers
            };
            append_only::check(dir.path(), v);
            assert_eq!(append_only::taken(&on_lignin, "verified"), 1);
            assert!(append_only::sound(&on_lignin), "grove version {number}");
            let stale = stale(&on_lignin);
            assert_eq!(stale, $stale, "records whose node is gone, grove version {number}");
            #[cfg(feature = "rocksdb_storage")]
            {
                let dir = tempfile::tempdir().expect("a directory");
                let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                    .expect("a database on RocksDB");
                let on_rocksdb = $history::<$($kind,)+ _>(&db, v $(, $arg)*);
                assert_eq!(
                    on_lignin.len(),
                    on_rocksdb.len(),
                    "the calls on grove version {number}"
                );
                for (step, (lignin, rocksdb)) in on_lignin.iter().zip(&on_rocksdb).enumerate() {
                    assert_eq!(lignin, rocksdb, "call {step} on grove version {number}");
                }
            }
        }
    }};
}

#[test]
fn a_tree_under_the_key_an_indexed_tree_was_moved_from_reads_the_records_it_left() {
    alike!(under_the_old_key::<CountSum, Count>(false), on: from_the_fourth(), stale: 5);
    alike!(under_the_old_key::<CountSum, CountSum>(false), on: from_the_fourth(), stale: 2);
    alike!(under_the_old_key::<CountSum, SumAndAverage>(false), on: from_the_fourth(), stale: 5);
    alike!(under_the_old_key::<Count, Sum>(false), on: from_the_fourth(), stale: 6);
    alike!(under_the_old_key::<SumAndAverage, CountSum>(false), on: from_the_fourth(), stale: 3);
}

#[test]
fn a_tree_that_changes_places_with_an_indexed_tree_reads_the_records_it_left() {
    alike!(under_the_old_key::<CountSum, Count>(true), on: from_the_fourth(), stale: 4);
    alike!(under_the_old_key::<CountSum, CountSum>(true), on: from_the_fourth(), stale: 1);
    alike!(under_the_old_key::<CountSum, SumAndAverage>(true), on: from_the_fourth(), stale: 4);
    alike!(under_the_old_key::<Count, Sum>(true), on: from_the_fourth(), stale: 5);
    alike!(under_the_old_key::<SumAndAverage, CountSum>(true), on: from_the_fourth(), stale: 2);
}

#[test]
fn an_indexed_tree_made_again_under_its_key_reads_the_records_its_delete_left() {
    alike!(deleted_and_made_again::<Count>(), on: every(), stale: 1);
    alike!(deleted_and_made_again::<CountSum>(), on: every(), stale: 1);
}

#[test]
fn a_moved_indexed_tree_brings_no_record_and_finds_the_one_it_left_when_it_is_back() {
    alike!(emptied_where_it_was_moved::<Count>(), on: from_the_fourth(), stale: 3);
    alike!(emptied_where_it_was_moved::<CountSum>(), on: from_the_fourth(), stale: 3);
}

#[test]
fn an_indexed_tree_in_a_moved_tree_leaves_its_records_under_the_old_path() {
    alike!(in_a_moved_tree::<Count>(), on: from_the_fourth(), stale: 1);
    alike!(in_a_moved_tree::<SumAndAverage>(), on: from_the_fourth(), stale: 1);
}

#[test]
fn the_drain_of_a_flat_drop_removes_the_records_under_its_prefixes() {
    alike!(dropped_flat_and_drained::<Count>(), on: from_the_fourth(), stale: 0);
    alike!(dropped_flat_and_drained::<CountSum>(), on: from_the_fourth(), stale: 0);
}

/// Where the store holds the root-key records, read with no handle of
/// the engine. A handle of each kind that the engine gives writes the
/// record of its subtree in the roots keyspace of the namespace of the
/// pending-drop records, under the prefix of the subtree and then the
/// record's key, and reads it there, and nothing is in the roots keyspace
/// of the subtree. The record of the root Merk is in the roots keyspace
/// of the root Merk's subtree, and a handle of the namespace's own prefix
/// keeps its record in the namespace under the record's key alone.
#[test]
fn every_handle_keeps_its_root_key_record_under_the_namespace_but_the_root_merks() {
    use grovedb_merk::store::lignin::kept_root_key;
    use grovedb_storage::flat::pending_prefix_drops_namespace;
    use lignindb::txn::{Column, Subtree};

    type Batch = <LigninEngine as Engine>::Batch;
    type Group = <LigninEngine as Engine>::WriteGroup;
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
    let root: &[&[u8]] = &[];
    (db.insert(root, b"p", Element::empty_tree(), None, None, v)
        .unwrap())
    .expect("a tree");
    (db.insert(P, b"idx", Count::empty(), None, None, v).unwrap()).expect("an indexed tree");
    (Count::insert(&db, IDX, &leaf_key(0), Count::leaf(0), None, v).unwrap()).expect("a row");
    let engine = db.engine();
    let primary = build_prefix(IDX.into()).value;
    let secondary = secondary_prefix_for(&primary, IndexAxis::Count.tag()).value;
    let namespace = *pending_prefix_drops_namespace();
    // The key of the record in the subtree it is a record of.
    let own_key = |prefix: &[u8; 32]| kept_root_key(prefix)[32..].to_vec();
    // What the store holds in the roots keyspace of `subtree` under `key`.
    let held = |subtree: [u8; 32], key: &[u8]| {
        let snapshot = engine.database().snapshot();
        let operation = snapshot.operation();
        let opened = operation.open(Subtree::new(subtree)).expect("a read")?;
        let mut value = Vec::new();
        (opened.get(Column::Roots, key, &mut value).expect("a read")).then_some(value)
    };
    let kept = |prefix: &[u8; 32]| held(namespace, &kept_root_key(prefix));

    // The first insert into the tree put the record of its secondary, and
    // none of its primary, which was opened with the root its element
    // names.
    let first = kept(&secondary).expect("the record that the first insert put");
    assert_eq!(kept(&primary), None);

    // A handle that writes at once, by path and by prefix.
    let tx = engine.start_transaction();
    let by_path = engine.immediate_subtree(IDX.into(), &tx).unwrap();
    (by_path.set_root_key(Some(b"by path")).unwrap()).expect("a write");
    let read = by_path.root_key().unwrap().expect("a read");
    assert_eq!(read, Some(b"by path".to_vec()));
    let by_prefix = engine.immediate_subtree_by_prefix(secondary, &tx).unwrap();
    assert_eq!(by_prefix.root_key().unwrap().expect("a read"), Some(first));
    (by_prefix.set_root_key(Some(b"by prefix")).unwrap()).expect("a write");
    drop((by_path, by_prefix));
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
    assert_eq!(kept(&primary), Some(b"by path".to_vec()));
    assert_eq!(kept(&secondary), Some(b"by prefix".to_vec()));

    // A handle that stages into a batch: by path, by prefix, and beside a
    // path.
    macro_rules! staged {
        ($value:expr, $prefix:expr, |$batch:ident, $tx:ident| $handle:expr) => {{
            let tx = engine.start_transaction();
            let batch = Batch::default();
            {
                let ($batch, $tx) = (&batch, &tx);
                let handle = $handle;
                assert!(handle.root_key().unwrap().expect("a read").is_some());
                (handle.set_root_key(Some($value)).unwrap()).expect("a staged write");
            }
            batch.next_operation();
            let mut group = Group::default();
            (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
            (engine.apply_write_group(group, &tx).unwrap()).expect("the group is applied");
            (engine.commit_transaction(tx).unwrap()).expect("a commit");
            assert_eq!(kept($prefix), Some($value.to_vec()));
        }};
    }
    staged!(b"staged by path", &primary, |batch, tx| {
        engine.subtree(IDX.into(), Some(batch), tx).unwrap()
    });
    staged!(b"staged by prefix", &secondary, |batch, tx| {
        engine
            .subtree_by_prefix(secondary, Some(batch), tx)
            .unwrap()
    });
    staged!(b"staged beside", &secondary, |batch, tx| {
        engine
            .subtree_beside(IDX.into(), secondary, Some(batch), tx)
            .unwrap()
    });

    // Nothing is in the roots keyspace of either subtree. The record of
    // the root Merk is in the root Merk's subtree, and nowhere else.
    for prefix in [primary, secondary] {
        assert_eq!(held(prefix, &own_key(&prefix)), None);
    }
    let root_merk = [0; 32];
    assert!(held(root_merk, &own_key(&root_merk)).is_some());
    assert_eq!(kept(&root_merk), None);

    // A handle of the namespace's own prefix keeps its record in the
    // namespace, as the record of its own subtree.
    let tx = engine.start_transaction();
    let own = engine.immediate_subtree_by_prefix(namespace, &tx).unwrap();
    (own.set_root_key(Some(b"of the namespace")).unwrap()).expect("a write");
    drop(own);
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
    let read = held(namespace, &own_key(&namespace));
    assert_eq!(read, Some(b"of the namespace".to_vec()));
    assert_eq!(kept(&namespace), None);
    db.flush().expect("a flush");
    drop(db);
    append_only::check(dir.path(), v);
}

/// A drain that is given the prefix of the namespace of the pending-drop
/// records itself, which the record of no flat drop names, after a
/// handle by that prefix wrote its root-key record and a record in each
/// of its keyspaces. The drain takes those and leaves the record of the
/// tree's secondary, which is read when the tree is emptied and written
/// again. The handle's four writes are made at the seam and are none of
/// the history's calls. The drain is a call of the seam and is taken in
/// with what it returns, and so are the reads at the seam of what is
/// under the namespace's prefix.
fn drained_at_the_namespace<M: Indexed, E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
) -> Vec<Answer> {
    use grovedb_storage::{flat::pending_prefix_drops_namespace, FlatStore};

    use crate::engine::SubtreeHandle;

    let mut h = History::new(db, v);
    with_rows::<M, E>(&mut h, 0..1);
    let engine = db.engine();
    let namespace = *pending_prefix_drops_namespace();
    // What is under the namespace's own prefix: its root-key record, and
    // a record of its data, of its aux and of its meta keyspace.
    let own = |h: &mut History<'_, E>, when: &str| {
        let tx = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(namespace, None, &tx).unwrap();
        let held = [
            handle.root_key().unwrap().expect("a read of the record"),
            handle.get(b"data").unwrap().expect("a read"),
            handle.aux().get(b"aux").unwrap().expect("a read"),
            handle.meta().get(b"meta").unwrap().expect("a read"),
        ];
        let name = format!("under the namespace's prefix {when}");
        h.plain(name, Ok::<_, Error>(held.clone()));
        held.iter().flatten().count()
    };
    let tx = engine.start_transaction();
    {
        let handle = engine.immediate_subtree_by_prefix(namespace, &tx).unwrap();
        (handle.set_root_key(Some(b"of the namespace")).unwrap()).expect("a write");
        (handle.put(b"data", b"1", None).unwrap()).expect("a write");
        (handle.aux().put(b"aux", b"2", None).unwrap()).expect("a write");
        (handle.meta().put(b"meta", b"3", None).unwrap()).expect("a write");
    }
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
    assert_eq!(own(&mut h, "before the drain"), 4);
    let drained = engine.delete_prefix_ranges(&[namespace]);
    h.plain("drained", drained.map_err(Error::from));
    assert_eq!(own(&mut h, "after the drain"), 0);
    records(&mut h, "after the drain", IDX);
    assert!(left(db, IDX).contains("a record of"), "the drain left it");
    (h.write("delete", M::delete(db, IDX, &leaf_key(0), None, v))).expect("a row less");
    records(&mut h, "after the tree was emptied", IDX);
    read_and_written::<M, E>(&mut h, IDX, 1, "after an insert");
    verified(&mut h);
    h.answers
}

#[test]
fn a_drain_of_the_namespace_itself_takes_no_record_of_another_prefix() {
    alike!(drained_at_the_namespace::<Count>(), on: every(), stale: 1);
    alike!(drained_at_the_namespace::<CountSum>(), on: every(), stale: 1);
}

/// A handle that stages its root-key record and nothing else names no
/// element for its subtree to the group. Under a key of the root Merk
/// that holds no element, a handle by prefix stages an aux record and a
/// handle by path stages its root-key record: the group is applied where
/// the two are staged in one batch as where they are staged in two, the
/// aux record is read back, and the root-key record is kept under the
/// namespace.
#[test]
fn a_root_key_record_staged_alone_names_no_element_for_its_subtree() {
    use grovedb_merk::store::lignin::kept_root_key;
    use grovedb_storage::{flat::pending_prefix_drops_namespace, FlatStore};
    use lignindb::txn::{Column, Subtree};

    type Batch = <LigninEngine as Engine>::Batch;
    type Group = <LigninEngine as Engine>::WriteGroup;
    let v = GroveVersion::latest();
    let missing: &[&[u8]] = &[b"missing"];
    let absent = build_prefix(missing.into()).value;
    let namespace = *pending_prefix_drops_namespace();
    for batches in [1, 2] {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let engine = db.engine();
        let tx = engine.start_transaction();
        let (first, second) = (Batch::default(), Batch::default());
        {
            let by_prefix = engine.subtree_by_prefix(absent, Some(&first), &tx).unwrap();
            (by_prefix.aux().put(b"k", b"aux", None).unwrap()).expect("a staged write");
            let beside = match batches {
                1 => &first,
                _ => &second,
            };
            let by_path = engine.subtree(missing.into(), Some(beside), &tx).unwrap();
            (by_path.set_root_key(Some(b"of no tree")).unwrap()).expect("a staged write");
        }
        let mut group = Group::default();
        for batch in [first, second].into_iter().take(batches) {
            batch.next_operation();
            (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
        }
        (engine.apply_write_group(group, &tx).unwrap()).expect("the group is applied");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
        {
            let read = engine.start_read_transaction();
            let by_prefix = engine.subtree_by_prefix(absent, None, &read).unwrap();
            let aux = by_prefix.aux().get(b"k").unwrap().expect("a read");
            assert_eq!(aux, Some(b"aux".to_vec()), "{batches} batches");
        }
        let kept = {
            let snapshot = engine.database().snapshot();
            let operation = snapshot.operation();
            let opened = operation.open(Subtree::new(namespace)).expect("a read");
            let opened = opened.expect("the namespace is entered");
            let mut value = Vec::new();
            let found = opened.get(Column::Roots, &kept_root_key(&absent), &mut value);
            found.expect("a read").then_some(value)
        };
        assert_eq!(kept, Some(b"of no tree".to_vec()), "{batches} batches");
        db.flush().expect("a flush");
        drop(db);
        append_only::check(dir.path(), v);
    }
}

/// A grove whose indexed trees have each had a history that leaves a
/// root-key record behind on a grove that lived through it: `again` was
/// deleted with its rows and made again, `emptied` was written and
/// emptied, `full` holds rows, and `moved` was moved from the key `from`,
/// under which a new tree was made.
///
/// So it is on a grove version from the fourth on (`late`). Before it a
/// move is refused, and the delete of `again` with its rows, beside the
/// three other trees of this grove, panics in Merk on both engines
/// (`TreeNode::hash_for_link`, reached from the delete's propagation).
/// There `again` and `from` hold the rows they were given, no tree is
/// moved, and `emptied` is the tree that a record is left of.
#[cfg(feature = "rocksdb_storage")]
fn with_histories<M: Indexed, E: Engine>(db: &GroveDb<E>, v: &GroveVersion, late: bool) {
    let root: &[&[u8]] = &[];
    let rows = |at: &[&[u8]], rows: std::ops::Range<usize>| {
        for n in rows {
            (M::insert(db, at, &leaf_key(n), M::leaf(n), None, v).unwrap()).expect("a row");
        }
    };
    let made = |key: &[u8]| {
        (db.insert(P, key, M::empty(), None, None, v).unwrap()).expect("an indexed tree");
    };
    (db.insert(root, b"p", Element::empty_tree(), None, None, v)
        .unwrap())
    .expect("a tree");
    for key in [b"again".as_slice(), b"emptied", b"from", b"full"] {
        made(key);
    }
    rows(&[b"p", b"again"], 0..3);
    if late {
        (db.delete(P, b"again", all(), None, v).unwrap()).expect("the delete");
        made(b"again");
    }
    rows(&[b"p", b"emptied"], 0..2);
    for n in 0..2 {
        let deleted = M::delete(db, &[b"p", b"emptied"], &leaf_key(n), None, v);
        deleted.unwrap().expect("a row less");
    }
    rows(&[b"p", b"from"], 0..3);
    if late {
        let moved = db.apply_batch(vec![op_move(b"from", b"moved")], None, None, v);
        moved.unwrap().expect("the move");
        made(b"from");
    }
    rows(&[b"p", b"full"], 0..4);
}

/// The indexed trees of a grove of [`with_histories`].
#[cfg(feature = "rocksdb_storage")]
fn histories(late: bool) -> &'static [&'static [&'static [u8]]] {
    const TREES: [&[&[u8]]; 5] = [
        &[b"p", b"again"],
        &[b"p", b"emptied"],
        &[b"p", b"from"],
        &[b"p", b"full"],
        &[b"p", b"moved"],
    ];
    match late {
        true => &TREES,
        false => &TREES[..4],
    }
}

/// What a grove of [`with_histories`] holds under the prefixes of each
/// tree, and what it answers and is charged: the root hash, every read of
/// the axes of each tree, and an insert into each.
#[cfg(feature = "rocksdb_storage")]
fn after_histories<M: Indexed, E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    late: bool,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    h.read("root hash", db.root_hash(None, v));
    for at in histories(late).iter().copied() {
        records(&mut h, "before", at);
        read_and_written::<M, E>(&mut h, at, 9, "after an insert");
    }
    verified(&mut h);
    h.answers
}

/// A grove that is synced from `source`, in a directory of its own.
#[cfg(feature = "rocksdb_storage")]
fn synced<S: Engine, T: Engine>(
    source: &GroveDb<S>,
    v: &GroveVersion,
) -> (tempfile::TempDir, GroveDb<T>) {
    use super::sync::{begin, Requests};
    let dir = tempfile::tempdir().expect("a directory");
    let target = GroveDb::<T>::open_with_engine(dir.path()).expect("a database");
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    {
        let atomic = crate::replication::RestoreCommitMode::Atomic;
        let mut session = begin(&target, root, atomic, v).expect("a session starts");
        let mut requests = Requests::new(source, root, v);
        while let Some((request, payload)) = requests.fetch() {
            (requests.apply(&mut session, &request, &payload)).expect("the chunk is taken");
        }
        assert!(session.is_sync_completed());
        (target.commit_session(session, v)).expect("the session commits");
    }
    (dir, target)
}

/// A state sync carries no root-key record that a history left: the
/// synced copy has the records that its own restore writes. Each engine
/// is synced from each, and the four copies answer and are charged the
/// same, which is not what the two sources are charged, whose records
/// their histories left; the two sources answer and are charged the same
/// as each other.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_synced_grove_is_charged_alike_on_both_engines_whatever_engine_it_was_synced_from() {
    use crate::RocksDbEngine as Rocks;
    for number in every() {
        let v = &GROVE_VERSIONS[number - 1];
        let late = from_the_fourth().contains(&number);
        let (on_lignin, on_rocksdb) = (
            tempfile::tempdir().expect("a directory"),
            tempfile::tempdir().expect("a directory"),
        );
        let lignin = Lignin::open_with_engine(on_lignin.path()).expect("a database is made");
        let rocksdb = GroveDb::<Rocks>::open_with_engine(on_rocksdb.path()).expect("RocksDB");
        with_histories::<CountSum, _>(&lignin, v, late);
        with_histories::<CountSum, _>(&rocksdb, v, late);

        let mut copies = Vec::new();
        let mut checked = Vec::new();
        {
            let (dir, copy) = synced::<LigninEngine, LigninEngine>(&lignin, v);
            copies.push((
                "LigninDB from LigninDB",
                after_histories::<CountSum, _>(&copy, v, late),
            ));
            copy.flush().expect("a flush");
            checked.push(dir);
        }
        {
            let (dir, copy) = synced::<Rocks, LigninEngine>(&rocksdb, v);
            copies.push((
                "LigninDB from RocksDB",
                after_histories::<CountSum, _>(&copy, v, late),
            ));
            copy.flush().expect("a flush");
            checked.push(dir);
        }
        {
            let (_dir, copy) = synced::<LigninEngine, Rocks>(&lignin, v);
            copies.push((
                "RocksDB from LigninDB",
                after_histories::<CountSum, _>(&copy, v, late),
            ));
        }
        {
            let (_dir, copy) = synced::<Rocks, Rocks>(&rocksdb, v);
            copies.push((
                "RocksDB from RocksDB",
                after_histories::<CountSum, _>(&copy, v, late),
            ));
        }
        for dir in &checked {
            append_only::check(dir.path(), v);
        }
        let (_, first) = &copies[0];
        assert!(append_only::sound(first));
        assert_eq!(
            stale(first),
            0,
            "a copy holds no record that a history left"
        );
        for (name, copy) in &copies[1..] {
            assert_eq!(copy.len(), first.len(), "{name} on grove version {number}");
            for (step, (one, other)) in first.iter().zip(copy).enumerate() {
                assert_eq!(one, other, "call {step}, {name}, grove version {number}");
            }
        }

        let source_on_lignin = after_histories::<CountSum, _>(&lignin, v, late);
        let source_on_rocksdb = after_histories::<CountSum, _>(&rocksdb, v, late);
        assert_eq!(
            source_on_lignin, source_on_rocksdb,
            "grove version {number}"
        );
        assert!(
            stale(&source_on_lignin) > 0,
            "the sources hold what their histories left"
        );
        assert_ne!(
            &source_on_lignin, first,
            "a source is charged for what its history left"
        );
        // The two make the same calls, and an insert among them is charged
        // otherwise on the source.
        let insert = |(of_source, of_copy): &(&Answer, &Answer)| {
            assert_eq!(of_source.0, of_copy.0);
            of_source.0 == "insert" && of_source.1 != of_copy.1
        };
        assert!(
            source_on_lignin.iter().zip(first).any(|pair| insert(&pair)),
            "no insert is charged otherwise on a source than on its copy"
        );
        lignin.flush().expect("a flush");
        drop(lignin);
        append_only::check(on_lignin.path(), v);
    }
}

/// A checkpoint is the store of its moment, the root-key records that
/// histories left among it: on each engine it holds, answers and is
/// charged what its source is, and the two engines' are the same.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_checkpoint_keeps_the_root_key_records_of_its_source_on_both_engines() {
    use crate::RocksDbEngine as Rocks;
    fn checkpointed<E: Engine>(
        v: &GroveVersion,
        late: bool,
        check: bool,
    ) -> (Vec<Answer>, Vec<Answer>) {
        let dir = tempfile::tempdir().expect("a directory");
        let above = tempfile::tempdir().expect("a directory");
        let target = above.path().join("checkpoint");
        let db = GroveDb::<E>::open_with_engine(dir.path()).expect("a database");
        with_histories::<CountSum, E>(&db, v, late);
        db.create_checkpoint(&target).expect("a checkpoint");
        let of_the_source = after_histories::<CountSum, E>(&db, v, late);
        let checkpoint =
            GroveDb::<E>::open_checkpoint_with_engine(&target).expect("the checkpoint opens");
        let of_the_checkpoint = after_histories::<CountSum, E>(&checkpoint, v, late);
        for db in [&db, &checkpoint] {
            db.flush().expect("a flush");
        }
        drop((db, checkpoint));
        if check {
            append_only::check(dir.path(), v);
            append_only::check(&target, v);
        }
        (of_the_source, of_the_checkpoint)
    }
    for number in every() {
        let v = &GROVE_VERSIONS[number - 1];
        let late = from_the_fourth().contains(&number);
        let (source, checkpoint) = checkpointed::<LigninEngine>(v, late, true);
        assert!(
            stale(&source) > 0,
            "the source holds what its histories left"
        );
        assert_eq!(source, checkpoint, "LigninDB, grove version {number}");
        let (source_on_rocksdb, checkpoint_on_rocksdb) = checkpointed::<Rocks>(v, late, false);
        assert_eq!(
            source_on_rocksdb, checkpoint_on_rocksdb,
            "RocksDB, grove version {number}"
        );
        assert_eq!(source, source_on_rocksdb, "grove version {number}");
    }
}

/// A wipe takes the root-key records with everything else. A grove that
/// is wiped holds none, and when it is written again it holds, answers
/// and is charged what a grove that was not written before is, on both
/// engines.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_wipe_removes_the_root_key_records_on_both_engines() {
    use crate::RocksDbEngine as Rocks;
    fn written<E: Engine>(v: &GroveVersion, late: bool, wiped: bool, check: bool) -> Vec<Answer> {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<E>::open_with_engine(dir.path()).expect("a database");
        if wiped {
            with_histories::<CountSum, E>(&db, v, late);
            let holds = |at: &&[&[u8]]| left(&db, at).contains("a record of");
            assert!(
                histories(late).iter().any(holds),
                "the histories left records"
            );
            db.wipe().expect("a wipe");
            assert!(!histories(late).iter().any(holds), "the wipe left a record");
        }
        with_histories::<CountSum, E>(&db, v, late);
        let answers = after_histories::<CountSum, E>(&db, v, late);
        db.flush().expect("a flush");
        drop(db);
        if check {
            append_only::check(dir.path(), v);
        }
        answers
    }
    for number in every() {
        let v = &GROVE_VERSIONS[number - 1];
        let late = from_the_fourth().contains(&number);
        let new = written::<LigninEngine>(v, late, false, true);
        assert!(append_only::sound(&new));
        let wiped = written::<LigninEngine>(v, late, true, true);
        assert_eq!(wiped, new, "LigninDB, grove version {number}");
        let on_rocksdb = written::<Rocks>(v, late, true, false);
        assert_eq!(on_rocksdb, new, "RocksDB, grove version {number}");
    }
}
