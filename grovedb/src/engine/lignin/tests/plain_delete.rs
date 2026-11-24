//! A tree that holds an element, removed by a batch's plain delete, and
//! made again at its key (GROVE-10): a shape recorded from real blocks.
//!
//! A plain delete of a batch checks nothing below its key. RocksDB keeps
//! the records of the removed tree under their prefix, which no element
//! leads to, and LigninDB drops a subtree with its element. So the two
//! engines differ in the reads that take the records under a prefix as
//! they are, at or below the path of such a tree: from the delete on,
//! and where a tree is made at the path again, for the keys that the new
//! tree has not written. **This is a stated difference**
//! (`docs/lignindb/test-engines.md`, "below a tree that a batch's plain
//! delete removed while it held elements"), which a later unit turns if
//! a network is found to come to such a read. The delete, the tree made
//! again, every write below it, every proof and every root hash are the
//! same on both engines.
//!
//! [`history`] is the sequence, written once for any engine. It runs on
//! LigninDB and on RocksDB on every grove version. Every call is
//! compared, and the calls that the engines answer differently are
//! written out with what each engine answers ([`differing`]): no other
//! call may differ, and none of these may come to agree unnoticed.

use grovedb_costs::OperationCost;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, taken, Answer, History},
    Lignin,
};
use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp as Op},
    engine::Engine,
    query_result_type::QueryResultType,
    Element, Error, GroveDb, PathQuery, Query,
};

/// The path of the tree that holds the three trees of the history.
const PARENT: &[&[u8]] = &[b"a", b"b", b"c", b"parent"];
/// A tree whose elements the batch that removes it deletes first.
const EMPTIED: [u8; 32] = [0x99; 32];
/// The two trees that are removed with an element in them.
const ONE: [u8; 32] = {
    let mut key = [0; 32];
    key[31] = 1;
    key
};
const TWO: [u8; 32] = {
    let mut key = [0; 32];
    key[31] = 2;
    key
};
const TREES: [(&str, &[u8; 32]); 3] = [("emptied", &EMPTIED), ("one", &ONE), ("two", &TWO)];
const BESIDE: [u8; 32] = [0; 32];
/// The key of the element that each tree holds: an empty sum tree.
const HELD: &[u8] = &[1];
const ITEM: &[u8] = &[0];
/// The key of an element in the sum tree.
const BELOW: &[u8] = &[0xee; 32];

fn at(below: &[&[u8]]) -> Vec<Vec<u8>> {
    let path = PARENT.iter().chain(below);
    path.map(|segment| segment.to_vec()).collect()
}

fn flags() -> Option<Vec<u8>> {
    Some(vec![2, 0, 4])
}

fn put(below: &[&[u8]], key: &[u8], element: Element) -> Op {
    Op::insert_or_replace_op(at(below), key.to_vec(), element)
}

fn delete(below: &[&[u8]], key: &[u8]) -> Op {
    Op::delete_op(at(below), key.to_vec())
}

fn tree() -> Element {
    Element::empty_tree_with_flags(flags())
}

fn held() -> Element {
    Element::empty_sum_tree_with_flags(flags())
}

/// The options the recorded batches were applied with.
fn options() -> Option<BatchApplyOptions> {
    Some(BatchApplyOptions {
        validate_insertion_does_not_override: false,
        validate_insertion_does_not_override_tree: false,
        disable_operation_consistency_check: true,
        base_root_storage_is_free: true,
        batch_pause_height: None,
    })
}

/// A batch in a transaction of its own, which is committed.
fn batch<E: Engine>(h: &mut History<'_, E>, name: &str, ops: Vec<Op>) {
    let (db, v) = (h.db, h.v);
    let block = db.start_transaction();
    let applied = db.apply_batch(ops, options(), Some(&block), v);
    h.write_in(name, applied, Some(&block));
    h.write(format!("{name}, committed"), db.commit_transaction(block));
}

/// The operations of the batch that makes the trees, without those
/// `left_out`: each tree, an item in the first, the sum tree that each
/// holds, and an item beside them.
fn made(left_out: &[&[u8]], n: u8) -> Vec<Op> {
    let makes = |key: &&[u8; 32]| !left_out.contains(&&key[..]);
    let mut ops = Vec::new();
    for key in [&EMPTIED, &ONE, &TWO].iter().filter(|key| makes(key)) {
        ops.push(put(&[], *key, tree()));
    }
    if makes(&&EMPTIED) {
        let item = Element::new_item_with_flags(vec![n; 40], flags());
        ops.push(put(&[&EMPTIED], ITEM, item));
    }
    for key in [&EMPTIED, &ONE, &TWO].iter().filter(|key| makes(key)) {
        ops.push(put(&[*key], HELD, held()));
    }
    ops.push(put(&[], &BESIDE, Element::new_item(vec![n; 22])));
    ops
}

/// The operations of the batch that removes the trees. The first tree
/// and each tree in `filled` are emptied before: the element in the sum
/// tree of a tree in `filled`, the sum tree, and the item of the first
/// tree. Then each tree goes by a plain delete, which leaves the sum
/// tree of every other tree where it is.
fn removed(filled: &[&[u8]], n: u8) -> Vec<Op> {
    let mut ops = Vec::new();
    for key in filled {
        ops.push(delete(&[key, HELD], BELOW));
    }
    for key in [&EMPTIED, &ONE, &TWO] {
        if key == &EMPTIED || filled.contains(&&key[..]) {
            ops.push(delete(&[key], HELD));
        }
    }
    ops.push(delete(&[&EMPTIED], ITEM));
    for key in [&EMPTIED, &ONE, &TWO] {
        ops.push(delete(&[], key));
    }
    ops.push(put(&[], &BESIDE, Element::new_item(vec![n; 22])));
    ops
}

/// A range query and its proof, with what the verifier makes of it.
fn ranged<E: Engine>(h: &mut History<'_, E>, name: String, query: &PathQuery) {
    let (db, v) = (h.db, h.v);
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    h.elements(
        format!("{name}, a range"),
        db.query_raw(query, true, true, true, pairs, None, v),
    );
    let proof = h.read(format!("{name}, a proof"), db.prove_query(query, None, v));
    if let Some(proof) = proof {
        h.plain(
            format!("{name}, the proof verified"),
            GroveDb::verify_query_raw(&proof, query, v),
        );
    }
}

/// The reads after a step: of each tree its element, whether it holds
/// an element and the one under [`HELD`], and a range and a proof of it;
/// a range and a proof of the parent, alone and with every tree below
/// it; and the grove's own check (`verify_grovedb`).
fn reads<E: Engine>(h: &mut History<'_, E>, when: &str) {
    let (db, v) = (h.db, h.v);
    let parent = at(&[]);
    let parent: Vec<&[u8]> = parent.iter().map(Vec::as_slice).collect();
    for (called, key) in TREES {
        let name = |read: &str| format!("{when}: {called}, {read}");
        h.element(name("get"), db.get(parent.as_slice(), key, None, v));
        h.read(name("has"), db.has_raw(parent.as_slice(), key, None, v));
        let tree = at(&[key]);
        let inside: Vec<&[u8]> = tree.iter().map(Vec::as_slice).collect();
        h.read(
            name("is empty"),
            db.is_empty_tree(inside.as_slice(), None, v),
        );
        h.read(
            name("has the held"),
            db.has_raw(inside.as_slice(), HELD, None, v),
        );
        h.element(
            name("get the held"),
            db.get(inside.as_slice(), HELD, None, v),
        );
        let range = PathQuery::new_unsized(tree, Query::new_range_full());
        ranged(h, format!("{when}: {called}"), &range);
    }
    let range = PathQuery::new_unsized(at(&[]), Query::new_range_full());
    ranged(h, format!("{when}: the parent"), &range);
    let mut down = Query::new_range_full();
    down.set_subquery(Query::new_range_full());
    let down = PathQuery::new_unsized(at(&[]), down);
    ranged(h, format!("{when}: the parent and below"), &down);
    let issues = db.verify_grovedb(None, true, false, v);
    h.plain(
        format!("{when}: the grove's own check"),
        issues.map(|issues| issues.into_keys().collect::<Vec<_>>()),
    );
}

/// The batches of [`history`], by name.
const BATCHES: [&str; 11] = [
    "made",
    "an element below",
    "removed",
    "one again",
    "its held again",
    "an element below one",
    "the rest again",
    "an element below two",
    "removed again",
    "made a third time",
    "removed a third time",
];

/// Three trees in one tree, each with an empty sum tree under one key,
/// made by one batch and removed by one batch as the recorded blocks do
/// it: the first tree after what it holds, the other two by a plain
/// delete alone. Then the tree `one` made again alone, its sum tree put
/// and an element put below that, each by a batch of its own; the other
/// two made again with what they hold by one batch; all three removed,
/// where two of them were emptied first; and all made and removed a
/// third time. After every batch, [`reads`].
fn history<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    for depth in 0..PARENT.len() {
        let (above, key) = (&PARENT[..depth], PARENT[depth]);
        h.write(
            "the path",
            db.insert(above, key, Element::empty_tree(), None, None, v),
        )
        .expect("a tree of the path");
    }
    reads(&mut h, "before");
    let below = |key: &[u8]| put(&[key, HELD], BELOW, Element::new_sum_item(4));
    let steps: [Vec<Op>; 11] = [
        made(&[], 1),
        vec![below(&EMPTIED)],
        removed(&[&EMPTIED], 2),
        vec![put(&[], &ONE, tree())],
        vec![put(&[&ONE], HELD, held())],
        vec![below(&ONE)],
        made(&[&ONE], 3),
        vec![below(&TWO)],
        removed(&[&ONE, &TWO], 4),
        made(&[], 5),
        removed(&[], 6),
    ];
    for (name, ops) in BATCHES.into_iter().zip(steps) {
        batch(&mut h, name, ops);
        reads(&mut h, name);
    }
    h.answers
}

/// What an engine answers to a call that the engines answer differently:
/// the seeks, the bytes loaded, the hash calls, and the answer as text.
type Said = (u32, u64, u32, String);

/// What a read of `rows` by a query that skipped none is taken in as
/// (`History::elements`).
fn rows_as(rows: &[(&[u8], Element)], v: &GroveVersion) -> String {
    let row = |(key, element): &(&[u8], Element)| {
        let bytes = element.serialize(v).expect("the bytes of an element");
        (key.to_vec(), (element.clone(), bytes))
    };
    let rows: Vec<_> = rows.iter().map(row).collect();
    format!("{:?}", Ok::<_, Error>((rows, 0u16)))
}

/// The calls of [`history`] that the engines answer differently on the
/// grove version `v`, in the order of the history, each with what
/// LigninDB and then what RocksDB answers.
///
/// **RocksDB's answers are what a later unit has to give if this stated
/// difference is turned; LigninDB's are what it gives today.**
///
/// - Under the path of a tree that was removed with its element, from
///   the delete until a tree at the path has written the key again:
///   whether the path holds the key (`has_raw`), and a range of the
///   path, in which RocksDB returns the element it kept.
/// - While a tree that was made again at the path has not written the
///   key: whether the tree is empty, a range of the tree above with
///   every tree below it, in which RocksDB returns the kept element
///   under the new tree, and the grove's own check, which fails on
///   RocksDB over the kept element.
fn differing(v: &GroveVersion) -> Vec<(String, Said, Said)> {
    let kept = rows_as(&[(HELD, held())], v);
    let has = |when: &str, tree: &str| {
        let call = format!("{when}: {tree}, has the held");
        let (lignin, rocksdb) = ("Ok(false)".to_string(), "Ok(true)".to_string());
        (call, (1, 0, 1, lignin), (1, 75, 1, rocksdb))
    };
    let range = |when: &str, tree: &str| {
        let call = format!("{when}: {tree}, a range");
        let nothing = "Ok(([], 0))".to_string();
        (call, (1, 288, 1, nothing), (3, 495, 1, kept.clone()))
    };
    let mut calls = Vec::new();
    for tree in ["one", "two"] {
        calls.extend([has("removed", tree), range("removed", tree)]);
    }
    // `one` is made again and holds nothing yet.
    calls.push((
        "one again: one, is empty".to_string(),
        (8, 1026, 6, "Ok(true)".to_string()),
        (8, 771, 6, "Ok(false)".to_string()),
    ));
    for tree in ["one", "two"] {
        calls.extend([has("one again", tree), range("one again", tree)]);
    }
    let beside = Element::new_item(vec![2; 22]);
    calls.push((
        "one again: the parent and below, a range".to_string(),
        (6, 1322, 2, rows_as(&[(&BESIDE, beside.clone())], v)),
        (8, 1529, 2, rows_as(&[(&BESIDE, beside), (HELD, held())], v)),
    ));
    let refused = "expected merk to contain value at key 0x01 for sum tree";
    calls.push((
        "one again: the grove's own check".to_string(),
        (0, 0, 0, "Ok([])".to_string()),
        (0, 0, 0, format!("Err(CorruptedData({refused:?}))")),
    ));
    // `one` has written the key again, and `two` is not made again yet.
    for when in ["its held again", "an element below one"] {
        calls.extend([has(when, "two"), range(when, "two")]);
    }
    for tree in ["one", "two"] {
        let when = "removed a third time";
        calls.extend([has(when, tree), range(when, tree)]);
    }
    calls
}

fn said((_, cost, answer): &Answer) -> Said {
    let read = OperationCost {
        seek_count: cost.seek_count,
        storage_loaded_bytes: cost.storage_loaded_bytes,
        hash_node_calls: cost.hash_node_calls,
        ..OperationCost::default()
    };
    assert_eq!(cost, &read, "a read writes nothing");
    let (seeks, loaded) = (cost.seek_count, cost.storage_loaded_bytes);
    (seeks, loaded, cost.hash_node_calls, answer.clone())
}

fn answer_of<'a>(answers: &'a [Answer], call: &str) -> &'a Answer {
    let mut named = answers.iter().filter(|(named, ..)| named == call);
    let found = named.next().unwrap_or_else(|| panic!("no call {call}"));
    assert!(named.next().is_none(), "two calls {call}");
    found
}

/// The engines answer alike where a tree that holds an element is
/// removed by a batch's plain delete and made again at its key, but for
/// the reads that are written out, which are the stated difference.
#[test]
fn a_tree_removed_with_an_element_in_it_and_made_again_answers_as_on_rocksdb_but_for_stated_reads()
{
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let answers = history(&db, v);
            db.flush().expect("a flush");
            answers
        };
        check(dir.path(), v);
        for call in BATCHES {
            let answer = &answer_of(&on_lignin, call).2;
            assert_eq!(taken(&on_lignin, call), 1, "{call}, {number}: {answer}");
            let committed = format!("{call}, committed");
            assert_eq!(taken(&on_lignin, &committed), 1, "{committed}, {number}");
        }
        // LigninDB's grove passes its own check after every batch.
        for when in ["before"].into_iter().chain(BATCHES) {
            let check = format!("{when}: the grove's own check");
            assert_eq!(answer_of(&on_lignin, &check).2, "Ok([])", "{check}");
        }
        let differing = differing(v);
        // What is: LigninDB's answers to the calls of the difference.
        for (call, lignin, _) in &differing {
            let is = said(answer_of(&on_lignin, call));
            assert_eq!(&is, lignin, "{call} on LigninDB, grove version {number}");
        }

        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            let on_rocksdb = history(&db, v);
            assert_eq!(on_lignin.len(), on_rocksdb.len(), "grove version {number}");
            let mut found = Vec::new();
            for (lignin, rocksdb) in on_lignin.iter().zip(&on_rocksdb) {
                assert_eq!(lignin.0, rocksdb.0, "grove version {number}");
                if lignin != rocksdb {
                    found.push((lignin.0.clone(), said(lignin), said(rocksdb)));
                }
            }
            // Required: RocksDB's answers. They are written out beside
            // LigninDB's, and no other call differs.
            if found != differing {
                for (call, lignin, rocksdb) in &found {
                    println!("differs on {number}: {call:?}\n  lignin  {lignin:?}\n  rocksdb {rocksdb:?}");
                }
            }
            assert!(found == differing, "grove version {number}");
        }
    }
}
