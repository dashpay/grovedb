//! A tree that keeps flat records with a keyed child: the nodes of the
//! child's level and the tree's own records in one data keyspace (grove
//! versions 1 to 3, `engine.md`, "What differs on grove versions 1 to 3
//! under a tree that keeps flat records").
//!
//! The histories that hold every answer to RocksDB's are in
//! [`append_only`](super::append_only). Here: the store at a close while
//! both kinds are there, the child and the appends in one transaction,
//! and a child whose key is the key of one of the tree's own records.

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{
        check, taken, verified, Answer, Appended, Bulk, Commitment, Dense, History, Mmr,
    },
    Lignin,
};
use crate::{
    batch::QualifiedGroveDbOp, engine::Engine, operations::delete::DeleteOptions, Element, GroveDb,
};

const ROOT: &[&[u8]] = &[];

fn child_under(tree: &[u8], key: &[u8]) -> QualifiedGroveDbOp {
    let item = Element::new_item(b"a keyed child".to_vec());
    QualifiedGroveDbOp::insert_or_replace_op(vec![tree.to_vec()], key.to_vec(), item)
}

/// The options of a delete that takes a tree with what it holds.
fn with_what_it_holds() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

/// The store is sound at a close that finds the nodes of the keyed
/// child's level and the tree's flat records under one prefix: with the
/// child before the appends and after them, for the four kinds of tree,
/// on grove versions 1 to 3.
#[test]
fn a_store_with_nodes_and_flat_records_under_one_tree_is_sound_at_its_close() {
    fn case<T: Appended>(v: &GroveVersion, child_first: bool) {
        let tree = T::NAME.as_bytes();
        let made = QualifiedGroveDbOp::insert_or_replace_op(vec![], tree.to_vec(), T::empty(None));
        let dir = tempfile::tempdir().expect("a directory");
        {
            let db = Lignin::open_with_engine(dir.path()).expect("a database");
            let batch = |ops| {
                (db.apply_batch(ops, None, None, v).unwrap()).expect("the batch is taken");
            };
            let appends = || {
                for n in 0..2 {
                    (T::append(&db, ROOT, tree, n, None, v).unwrap()).expect("an append");
                }
            };
            match child_first {
                true => {
                    batch(vec![made, child_under(tree, b"child")]);
                    appends();
                }
                false => {
                    batch(vec![made]);
                    appends();
                    batch(vec![child_under(tree, b"child")]);
                }
            }
            db.flush().expect("a flush");
        }
        check(dir.path(), v);
    }
    for v in GROVE_VERSIONS.iter().take(3) {
        for child_first in [true, false] {
            case::<Mmr>(v, child_first);
            case::<Bulk>(v, child_first);
            case::<Dense>(v, child_first);
            case::<Commitment>(v, child_first);
        }
    }
}

/// A tree, a keyed child under it and two appends in one transaction, the
/// child before the appends or between them, with the tree's own reads
/// through the transaction; the commit, and the reads after it; then an
/// append and the delete of the tree in a second transaction; and in a
/// third the tree, the child, an append and the delete of the tree.
fn in_one_transaction<'db, T: Appended, E: Engine>(
    db: &'db GroveDb<E>,
    v: &'db GroveVersion,
    child_first: bool,
) -> Vec<Answer> {
    let tree = T::NAME.as_bytes();
    let mut h = History::new(db, v);
    let made = QualifiedGroveDbOp::insert_or_replace_op(vec![], tree.to_vec(), T::empty(None));
    let child = || child_under(tree, b"child");
    let tx = db.start_transaction();
    let through = Some(&tx);
    match child_first {
        true => {
            let batch = db.apply_batch(vec![made, child()], None, through, v);
            h.write_in("batch", batch, through);
            for n in 0..2 {
                h.write_in("append", T::append(db, ROOT, tree, n, through, v), through);
            }
        }
        false => {
            let batch = db.apply_batch(vec![made], None, through, v);
            h.write_in("batch", batch, through);
            h.write_in("append", T::append(db, ROOT, tree, 0, through, v), through);
            let batch = db.apply_batch(vec![child()], None, through, v);
            h.write_in("batch", batch, through);
            h.write_in("append", T::append(db, ROOT, tree, 1, through, v), through);
        }
    }
    T::reads(&mut h, ROOT, tree, 0, through);
    h.write("commit", db.commit_transaction(tx));
    T::reads(&mut h, ROOT, tree, 1, None);
    let tx = db.start_transaction();
    let through = Some(&tx);
    h.write_in("append", T::append(db, ROOT, tree, 2, through, v), through);
    T::reads(&mut h, ROOT, tree, 2, through);
    let deleted = db.delete(ROOT, tree, with_what_it_holds(), through, v);
    h.write_in("delete", deleted, through);
    h.write("commit", db.commit_transaction(tx));
    verified(&mut h);
    // And the whole of it in one transaction: the tree, the child, an
    // append and the delete of the tree.
    let tx = db.start_transaction();
    let through = Some(&tx);
    let made = QualifiedGroveDbOp::insert_or_replace_op(vec![], tree.to_vec(), T::empty(None));
    let batch = db.apply_batch(vec![made, child()], None, through, v);
    h.write_in("batch", batch, through);
    h.write_in("append", T::append(db, ROOT, tree, 0, through, v), through);
    let deleted = db.delete(ROOT, tree, with_what_it_holds(), through, v);
    h.write_in("delete", deleted, through);
    h.write("commit", db.commit_transaction(tx));
    verified(&mut h);
    h.answers
}

/// The nodes of the keyed child's level and the tree's records are
/// written and read inside one transaction as on RocksDB: every answer,
/// cost and root hash of [`in_one_transaction`], for the four kinds of
/// tree on grove versions 1 to 3, and the store is sound at its close.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_keyed_child_and_appends_in_one_transaction_answer_as_on_rocksdb() {
    fn case<T: Appended>(number: usize, v: &GroveVersion, child_first: bool) {
        let case = format!(
            "{}, grove version {number}, child first: {child_first}",
            T::NAME
        );
        let on_rocksdb = {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path());
            in_one_transaction::<T, _>(&db.expect("a database on RocksDB"), v, child_first)
        };
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database");
            let answers = in_one_transaction::<T, _>(&db, v, child_first);
            db.flush().expect("a flush");
            answers
        };
        check(dir.path(), v);
        assert_eq!(on_lignin.len(), on_rocksdb.len(), "{case}");
        for (at, (lignin, rocks)) in on_lignin.iter().zip(&on_rocksdb).enumerate() {
            assert_eq!(lignin, rocks, "{case}: step {at}");
        }
        // The history is the one it says: every write of it is taken.
        let batches = if child_first { 2 } else { 3 };
        let writes = [
            ("batch", batches),
            ("append", 4),
            ("delete", 2),
            ("commit", 3),
        ];
        for (call, times) in writes {
            assert_eq!(taken(&on_lignin, call), times, "{case}: {call}");
        }
    }
    for (number, v) in GROVE_VERSIONS.iter().enumerate().take(3) {
        for child_first in [true, false] {
            case::<Mmr>(number + 1, v, child_first);
            case::<Bulk>(number + 1, v, child_first);
            case::<Dense>(number + 1, v, child_first);
            case::<Commitment>(number + 1, v, child_first);
        }
    }
}

/// The key of the first node of a Merkle mountain range in the data
/// keyspace of its tree: the node's position as eight bytes, with the
/// first bit set (`mmr_node_key_sized`).
fn first_node() -> Vec<u8> {
    (1u64 << 63).to_be_bytes().to_vec()
}

/// A key of that length that a Merkle mountain range writes no record
/// under: its first bit is not set.
fn no_node() -> Vec<u8> {
    (1u64 << 62).to_be_bytes().to_vec()
}

/// What a history of [`same_key`] does, in its order.
#[derive(Clone, Copy)]
enum Order {
    ChildThenAppend,
    ChildAndNoAppend,
    AppendThenChild,
}

/// A Merkle mountain range and a keyed child under `key`, with one value
/// appended before the child, after it, or not at all; then the child
/// read by its key, the typed reads of the tree, the delete of the tree,
/// and the grove's own check.
fn same_key<E: Engine>(db: &GroveDb<E>, v: &GroveVersion, order: Order, key: &[u8]) -> Vec<Answer> {
    const TREE: &[u8] = b"mmr";
    let mut h = History::new(db, v);
    let at_tree: &[&[u8]] = &[TREE];
    let made = || {
        QualifiedGroveDbOp::insert_or_replace_op(vec![], TREE.to_vec(), Element::empty_mmr_tree())
    };
    let child = || child_under(TREE, key);
    match order {
        Order::ChildThenAppend | Order::ChildAndNoAppend => {
            h.write(
                "batch",
                db.apply_batch(vec![made(), child()], None, None, v),
            );
        }
        Order::AppendThenChild => {
            h.write("batch", db.apply_batch(vec![made()], None, None, v));
        }
    }
    if !matches!(order, Order::ChildAndNoAppend) {
        h.write(
            "append",
            db.mmr_tree_append(ROOT, TREE, vec![7; 40], None, v),
        );
    }
    if matches!(order, Order::AppendThenChild) {
        h.write("batch", db.apply_batch(vec![child()], None, None, v));
    }
    h.element("get child", db.get(at_tree, key, None, v));
    h.read("count", db.mmr_tree_leaf_count(ROOT, TREE, None, v));
    h.read("value", db.mmr_tree_get_value(ROOT, TREE, 0, None, v));
    h.read("root hash", db.mmr_tree_root_hash(ROOT, TREE, None, v));
    h.write(
        "delete",
        db.delete(ROOT, TREE, with_what_it_holds(), None, v),
    );
    verified(&mut h);
    h.answers
}

/// The answers of [`same_key`] on RocksDB.
#[cfg(feature = "rocksdb_storage")]
fn same_key_on_rocksdb(v: &GroveVersion, order: Order, key: &[u8]) -> Vec<Answer> {
    let dir = tempfile::tempdir().expect("a directory");
    let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path());
    same_key(&db.expect("a database on RocksDB"), v, order, key)
}

/// The answers of [`same_key`] on LigninDB, whose store is checked at
/// its close.
#[cfg(feature = "rocksdb_storage")]
fn same_key_on_lignin(v: &GroveVersion, order: Order, key: &[u8]) -> Vec<Answer> {
    let dir = tempfile::tempdir().expect("a directory");
    let answers = {
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        let answers = same_key(&db, v, order, key);
        db.flush().expect("a flush");
        answers
    };
    check(dir.path(), v);
    answers
}

/// What a history answered the first time it made `call`.
#[cfg(feature = "rocksdb_storage")]
fn said<'a>(answers: &'a [Answer], call: &str) -> &'a Answer {
    let made = answers.iter().find(|(name, ..)| name == call);
    made.unwrap_or_else(|| panic!("the history makes the call {call}"))
}

/// The steps of [`same_key`] that LigninDB answers otherwise than RocksDB
/// where the append goes under the key of the child: the append, the
/// root hash after it, the three typed reads of the tree, the delete of
/// the tree, and the root hash after that.
#[cfg(feature = "rocksdb_storage")]
const WHERE_AN_APPEND_GOES_UNDER_THE_KEY_OF_A_CHILD: [&str; 7] = [
    "append",
    "root",
    "count",
    "value",
    "root hash",
    "delete",
    "root",
];

/// An open difference, and no property of the engine (D-311). Grove
/// versions 1 to 3 hold the key of a child under a tree of flat records
/// to nothing, so a caller can choose a key that the tree writes a
/// record of its own under: here the key of the first node of a Merkle
/// mountain range, with the child made before the first append.
///
/// RocksDB keeps one record for a key, and the append overwrites the
/// child's node with the tree's record. Its answers are the required
/// ones and are held here on RocksDB: the append is taken, the tree
/// reads the value, and the delete of the tree removes less than where
/// the child is under another key of that length, because one record is
/// under the key.
///
/// LigninDB keeps one record for a key as well, and does not let a flat
/// record take the place of a node: the append is refused, as what the
/// engine does not support, in words that name the state. Nothing of the
/// call is kept: the root hash is the one before it, every later answer
/// and cost is the one RocksDB gives where the append is not made, and
/// the store is sound at its close.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn lignindb_does_not_yet_take_an_append_under_the_key_of_a_keyed_child() {
    use Order::{ChildAndNoAppend, ChildThenAppend};
    for (number, v) in GROVE_VERSIONS.iter().enumerate().take(3) {
        let case = format!("grove version {}", number + 1);
        let required = same_key_on_rocksdb(v, ChildThenAppend, &first_node());
        let not_made = same_key_on_rocksdb(v, ChildAndNoAppend, &first_node());
        let elsewhere = same_key_on_rocksdb(v, ChildThenAppend, &no_node());
        let on_lignin = same_key_on_lignin(v, ChildThenAppend, &first_node());

        // The required answers.
        assert!(said(&required, "append").2.starts_with("Ok("), "{case}");
        assert_eq!(said(&required, "count").2, "Ok(1)", "{case}");
        assert!(
            said(&required, "value").2.starts_with("Ok(Some([7, 7,"),
            "{case}"
        );
        assert_eq!(said(&required, "delete").2, "Ok(())", "{case}");
        let removed = |answers: &[Answer]| {
            let (_, cost, _) = said(answers, "delete");
            cost.storage_cost.removed_bytes.total_removed_bytes()
        };
        assert!(removed(&required) < removed(&elsewhere), "{case}");

        // What LigninDB answers.
        let append = on_lignin.iter().position(|(name, ..)| name == "append");
        let append = append.expect("the history makes the append");
        assert_eq!(on_lignin[..append], required[..append], "{case}");
        let refused = &on_lignin[append].2;
        let named = refused.starts_with("Err(NotSupported(\"lignindb: ")
            && refused.contains("a flat record is written under a key of")
            && refused.contains("that a Merk node is under")
            && refused.contains(
                "a tree that keeps flat records has a child under a key of one of its records",
            );
        assert!(named, "{case}: {refused}");
        assert_eq!(on_lignin[append + 1], on_lignin[append - 1], "{case}");
        let mut after = on_lignin.clone();
        after.drain(append..append + 2);
        assert_eq!(after, not_made, "{case}");
        // And the difference is the one that is written down.
        assert_eq!(on_lignin.len(), required.len(), "{case}");
        let steps = on_lignin.iter().zip(&required);
        let differ: Vec<&str> = steps
            .filter(|(lignin, rocks)| lignin != rocks)
            .map(|(lignin, _)| lignin.0.as_str())
            .collect();
        assert_eq!(
            differ, WHERE_AN_APPEND_GOES_UNDER_THE_KEY_OF_A_CHILD,
            "{case}"
        );
    }
}

/// The other order is taken by both engines: a child that is made under
/// the key of a record of its tree takes the record. The node of the
/// child is written under the key, where RocksDB had the tree's record
/// and has one record still, and where LigninDB's node takes the flat
/// record under its key as it takes an unlinked record.
///
/// So the tree has lost the record of its first node on both engines,
/// and the two typed reads that need it say so each in the engine's way:
/// RocksDB decodes the child's node as a node of the tree and fails,
/// LigninDB finds no record. Every other step is equal in its answer and
/// its cost, the delete of the tree and the grove's own check after it
/// among them.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_child_made_under_the_key_of_a_record_of_its_tree_takes_the_record_on_both_engines() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate().take(3) {
        let case = format!("grove version {}", number + 1);
        let on_rocksdb = same_key_on_rocksdb(v, Order::AppendThenChild, &first_node());
        let on_lignin = same_key_on_lignin(v, Order::AppendThenChild, &first_node());
        assert_eq!(on_rocksdb.len(), on_lignin.len(), "{case}");
        for answers in [&on_rocksdb, &on_lignin] {
            for (call, times) in [("batch", 2), ("append", 1), ("delete", 1)] {
                assert_eq!(taken(answers, call), times, "{case}: {call}");
            }
        }
        for (at, (rocks, lignin)) in on_rocksdb.iter().zip(&on_lignin).enumerate() {
            let call = rocks.0.as_str();
            if !["value", "root hash"].contains(&call) {
                assert_eq!(lignin, rocks, "{case}: step {at}");
                continue;
            }
            assert!(rocks.2.starts_with("Err("), "{case}: {}", rocks.2);
            assert!(
                rocks.2.contains("deserialize node at pos 0"),
                "{case}: {}",
                rocks.2
            );
            match call {
                "value" => assert_eq!(lignin.2, "Ok(None)", "{case}"),
                _ => {
                    let said = lignin.2.starts_with("Err(") && lignin.2.contains("Inconsistent");
                    assert!(said, "{case}: {}", lignin.2);
                }
            }
        }
    }
}
