//! Histories that the registration of the engine for GroveDB's suite owed
//! (GROVE-10), each written once for any engine and run on LigninDB and on
//! RocksDB on every grove version, with every answer, every
//! `OperationCost` and the root hash after every write compared call for
//! call:
//!
//! - a range that is read right to left and ends past the last key of its
//!   subtree, in every subtree of a grove, the one that is last in prefix
//!   order among them;
//! - a conversion to a big sum tree that is given no transaction, beside a
//!   transaction that has written;
//! - a clear and a delete of a tree in a transaction that a commit of
//!   another writer met;
//! - the move of a tree after a tree inside it was deleted.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_storage::prefix::build_prefix;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, on_both_engines, sound, taken, verified, Answer, History},
    Lignin,
};
use crate::{
    batch::QualifiedGroveDbOp,
    engine::Engine,
    operations::delete::{ClearOptions, DeleteOptions},
    query_result_type::QueryResultType,
    BackwardsReferences, Element, Error, GroveDb, PathQuery, Query, QueryItem,
};

const ROOT: &[&[u8]] = &[];

/// The keys of the trees at the root, and of the items in each.
const KEYS: [&[u8]; 3] = [b"k1", b"k2", b"k3"];

fn item(n: u8) -> Element {
    Element::new_item(vec![n; 8])
}

fn owned(path: &[&[u8]]) -> Vec<Vec<u8>> {
    path.iter().map(|segment| segment.to_vec()).collect()
}

/// The answer of the call named `call`.
fn answer_of<'a>(answers: &'a [Answer], call: &str) -> &'a str {
    let mut found = answers.iter().filter(|(named, ..)| named == call);
    let (.., answer) = found.next().unwrap_or_else(|| panic!("no call {call}"));
    assert!(found.next().is_none(), "two calls {call}");
    answer
}

/// A delete of a tree with what it holds.
fn all() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

/// The ranges of [`stepped`]: the first three exclude their end, which is
/// past the last key, so that a read right to left seeks to the end and
/// steps back from no key of the subtree. The others end on a key or
/// include their end, and are read from a key.
fn ranges() -> Vec<(&'static str, QueryItem)> {
    let (below, above) = (b"k0".to_vec(), b"k9".to_vec());
    vec![
        ("a range", QueryItem::Range(below.clone()..above.clone())),
        ("to", QueryItem::RangeTo(..above.clone())),
        (
            "after to",
            QueryItem::RangeAfterTo(below.clone()..above.clone()),
        ),
        (
            "a closed range",
            QueryItem::RangeInclusive(below.clone()..=above.clone()),
        ),
        ("up to", QueryItem::RangeToInclusive(..=above)),
        ("to a key", QueryItem::RangeTo(..KEYS[1].to_vec())),
        ("from", QueryItem::RangeFrom(below..)),
    ]
}

fn stepped_call(range: &str, left_to_right: bool, at: &[&[u8]]) -> String {
    let direction = if left_to_right {
        "ascending"
    } else {
        "descending"
    };
    format!("{range}, {direction}, of {:?}", owned(at))
}

/// Three trees at the root with the same items, and every range of
/// [`ranges`] in both directions in the root tree and in each of them.
fn stepped<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    for tree in KEYS {
        h.write(
            "a tree",
            db.insert(ROOT, tree, Element::empty_tree(), None, None, v),
        )
        .expect("a tree");
        let at: &[&[u8]] = &[tree];
        for (n, key) in KEYS.iter().enumerate() {
            h.write("an item", db.insert(at, key, item(n as u8), None, None, v))
                .expect("an item");
        }
    }
    let mut places: Vec<Vec<&[u8]>> = vec![Vec::new()];
    places.extend(KEYS.iter().map(|tree| vec![*tree]));
    for at in &places {
        for (range, query_item) in ranges() {
            for left_to_right in [true, false] {
                let query =
                    Query::new_single_query_item_with_direction(query_item.clone(), left_to_right);
                let query = PathQuery::new_unsized(owned(at), query);
                let pairs = QueryResultType::QueryKeyElementPairResultType;
                h.elements(
                    stepped_call(range, left_to_right, at),
                    db.query_raw(&query, true, true, true, pairs, None, v),
                );
            }
        }
    }
    verified(&mut h);
    h.answers
}

/// A range that is read right to left and ends past the last key of its
/// subtree is answered on LigninDB as on RocksDB in every subtree of a
/// grove, the last in prefix order among them (D-189): there RocksDB's
/// cursor steps back from the end of its column and finds no key, and the
/// read answers no row, where in every other subtree it answers the rows.
/// The fixture holds both: the trees have the same items, so their
/// answers are the same text where they are the same rows.
#[test]
fn a_range_read_from_past_the_last_key_is_answered_as_on_rocksdb_in_the_last_subtree_too() {
    let prefix = |tree: &[u8]| {
        let at: &[&[u8]] = &[tree];
        build_prefix(SubtreePath::from(at)).unwrap()
    };
    let last = *(KEYS.iter())
        .max_by_key(|tree| prefix(tree))
        .expect("three trees");
    on_both_engines!(stepped, |answers: &[Answer], number: usize| {
        assert!(sound(answers), "grove version {number}");
        let of = |range: &str, left_to_right: bool, tree: &[u8]| {
            answer_of(answers, &stepped_call(range, left_to_right, &[tree])).to_owned()
        };
        for range in ["a range", "to", "after to"] {
            let ascending = of(range, true, last);
            assert!(ascending.starts_with("Ok(([(["), "{range}: {ascending}");
            for tree in KEYS {
                assert_eq!(of(range, true, tree), ascending, "{range}, ascending");
                let descending = of(range, false, tree);
                if tree == last {
                    assert_eq!(descending, "Ok(([], 0))", "{range} of the last subtree");
                } else {
                    assert!(
                        descending.starts_with("Ok(([(["),
                        "{range} of a subtree that another follows: {descending}"
                    );
                }
            }
        }
        // Read from a key, the last subtree answers its rows.
        for range in ["a closed range", "up to", "to a key", "from"] {
            let descending = of(range, false, last);
            assert!(descending.starts_with("Ok(([(["), "{range}: {descending}");
        }
    });
}

/// How a transaction's commit ended, in words that name no engine.
fn ended(committed: Result<(), Error>) -> Result<&'static str, Error> {
    match committed {
        Ok(()) => Ok("committed"),
        Err(error) if error.is_commit_conflict() => Ok("a conflict"),
        Err(error) => Err(error),
    }
}

/// A sum tree with sum items, a transaction that writes into another
/// tree, the conversion of the sum tree with no transaction, and the
/// commit of the transaction.
fn converted_beside_a_block<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let sums: &[&[u8]] = &[b"sums"];
    let other: &[&[u8]] = &[b"other"];
    h.write(
        "a sum tree",
        db.insert(ROOT, b"sums", Element::empty_sum_tree(), None, None, v),
    )
    .expect("a sum tree");
    for n in 0..3u8 {
        let sum_item = Element::new_sum_item(i64::from(n) + 1);
        h.write(
            "a sum item",
            db.insert(sums, &[b'k', n], sum_item, None, None, v),
        )
        .expect("a sum item");
    }
    h.write(
        "a tree",
        db.insert(ROOT, b"other", Element::empty_tree(), None, None, v),
    )
    .expect("a tree");

    let block = db.start_transaction();
    h.write_in(
        "the block writes",
        db.insert(other, b"k", item(1), None, Some(&block), v),
        Some(&block),
    )
    .expect("a write of the block");
    h.write(
        "the conversion beside the block",
        db.convert_to_big_sum_tree(ROOT, b"sums", None, v),
    );
    h.plain(
        "the block commits",
        ended(db.commit_transaction(block).unwrap()),
    );

    h.element("the tree", db.get_raw(ROOT.into(), b"sums", None, v));
    h.element(
        "the write of the block",
        db.get_raw(other.into(), b"k", None, v),
    );
    h.read("root", db.root_hash(None, v));
    verified(&mut h);
    h.answers
}

/// A conversion that is given no transaction runs in one that GroveDB
/// opens and commits. Beside a transaction that holds a write it is
/// applied at once, and the commit of that transaction then fails as a
/// conflict, on both engines (RocksDB's `Busy`): every write reaches the
/// root tree. The grove versions before the fifth refuse the conversion,
/// and the transaction commits.
#[test]
fn a_conversion_beside_a_transaction_that_wrote_fails_its_commit_as_on_rocksdb() {
    on_both_engines!(
        converted_beside_a_block,
        |answers: &[Answer], number: usize| {
            assert!(sound(answers), "grove version {number}");
            let converted = taken(answers, "the conversion beside the block") == 1;
            let block = answer_of(answers, "the block commits");
            let written = answer_of(answers, "the write of the block");
            if number >= 5 {
                assert!(converted);
                assert_eq!(block, "Ok(\"a conflict\")");
                assert!(written.starts_with("Err("), "{written}");
                assert!(answer_of(answers, "the tree").contains("big_sum_tree: "));
            } else {
                assert!(!converted);
                assert_eq!(block, "Ok(\"committed\")");
                assert!(written.starts_with("Ok("), "{written}");
            }
        }
    );
}

/// Takes in a call of a transaction that a commit of another writer may
/// have met: a refusal as a conflict is taken in by that word, with no
/// cost, since an engine may refuse at the write what another refuses at
/// the commit. Gives whether the transaction goes on. A clear is taken in
/// with no cost: it returns none.
fn in_a_met_transaction<T: std::fmt::Debug, E: Engine>(
    h: &mut History<'_, E>,
    name: String,
    answer: CostResult<T, Error>,
) -> bool {
    if answer.value.as_ref().is_err_and(Error::is_commit_conflict) {
        h.plain(name, Ok::<_, Error>("a conflict"));
        return false;
    }
    h.read(name, answer).is_some()
}

/// The commits of [`cleared_and_deleted`], by where the commit of the
/// other writer lands.
const MET: [&str; 3] = [
    "before the clear",
    "between the clear and the delete",
    "after the delete",
];

/// For each case of [`MET`], a tree under the root with a tree `t` of
/// items and of a tree with items, and a tree `o` beside it. A
/// transaction clears `t` and deletes it, and another writer commits an
/// item into `o` where the case says. Then the transaction commits.
fn cleared_and_deleted<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let clear = ClearOptions {
        check_for_subtrees: true,
        allow_deleting_subtrees: true,
        trying_to_clear_with_subtrees_returns_error: false,
        backwards_references: BackwardsReferences::DontCheck,
    };
    for (case, met) in MET.iter().enumerate() {
        let grove = [b'g', b'0' + case as u8];
        let top: &[&[u8]] = &[&grove];
        let tree: &[&[u8]] = &[&grove, b"t"];
        let nested: &[&[u8]] = &[&grove, b"t", b"n"];
        let beside: &[&[u8]] = &[&grove, b"o"];
        let made = [
            (ROOT, &grove[..], Element::empty_tree()),
            (top, b"t", Element::empty_tree()),
            (top, b"o", Element::empty_tree()),
            (tree, b"k1", item(1)),
            (tree, b"n", Element::empty_tree()),
            (nested, b"k1", item(2)),
            (nested, b"k2", item(3)),
        ];
        for (at, key, element) in made {
            h.write("made", db.insert(at, key, element, None, None, v))
                .expect("the grove of the case");
        }

        let transaction = db.start_transaction();
        let tx = Some(&transaction);
        let commit_beside = |h: &mut History<'_, E>, when: usize| {
            if when == case {
                h.write(
                    format!("the commit beside, {met}"),
                    db.insert(beside, b"late", item(9), None, None, v),
                )
                .expect("the other writer commits");
            }
        };
        commit_beside(&mut h, 0);
        let mut open = in_a_met_transaction(
            &mut h,
            format!("the clear, met {met}"),
            db.clear_subtree(tree, Some(clear.clone()), tx, v)
                .wrap_with_cost(OperationCost::default()),
        );
        commit_beside(&mut h, 1);
        if open {
            open = in_a_met_transaction(
                &mut h,
                format!("the delete, met {met}"),
                db.delete(top, b"t", all(), tx, v),
            );
        }
        commit_beside(&mut h, 2);
        let committed = match open {
            true => ended(db.commit_transaction(transaction).unwrap()),
            false => {
                drop(transaction);
                Ok("a conflict")
            }
        };
        h.plain(format!("the transaction, met {met}"), committed);

        h.read(
            format!("the tree, met {met}"),
            db.has_raw(top, b"t", None, v),
        );
        h.element(
            format!("the item beside, met {met}"),
            db.get_raw(beside.into(), b"late", None, v),
        );
        h.read("root", db.root_hash(None, v));
    }
    verified(&mut h);
    h.answers
}

/// A transaction that clears a tree and deletes it, and that a commit of
/// another writer met (D-253): where the commit landed before the
/// transaction's first write, the transaction commits and the tree is
/// gone; where it landed after a write of the transaction, the
/// transaction ends in a conflict and the tree stays, on both engines.
/// RocksDB refuses at the commit. LigninDB may refuse at the write, with
/// the transaction as it was: the history takes both in as the conflict
/// of the transaction.
#[test]
fn a_clear_and_a_delete_in_a_transaction_that_a_commit_met_end_as_on_rocksdb() {
    on_both_engines!(cleared_and_deleted, |answers: &[Answer], number: usize| {
        assert!(sound(answers), "grove version {number}");
        for (case, met) in MET.iter().enumerate() {
            let ended = answer_of(answers, &format!("the transaction, met {met}"));
            let tree = answer_of(answers, &format!("the tree, met {met}"));
            let late = answer_of(answers, &format!("the item beside, met {met}"));
            assert!(late.starts_with("Ok("), "{met}: {late}");
            if case == 0 {
                assert_eq!(ended, "Ok(\"committed\")", "{met}, grove version {number}");
                assert_eq!(tree, "Ok(false)", "{met}");
            } else {
                assert_eq!(ended, "Ok(\"a conflict\")", "{met}, grove version {number}");
                assert_eq!(tree, "Ok(true)", "{met}");
            }
        }
    });
}

fn move_op(at: &[u8], key: &[u8], to: &[u8]) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::move_op(vec![at.to_vec()], key.to_vec(), to.to_vec())
}

/// Under each of two trees at the root a tree `t` with an item and a tree
/// `n`, which holds items and a tree of its own with an item. `n` is
/// deleted with what it holds, and `t` is moved to the key `u`: under the
/// first tree by two calls, under the second in one transaction. The
/// moved tree is read, a tree is made again under the key of the deleted
/// one and written into, and a tree is made again under the old key.
fn moved_after_a_nested_delete<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    for (top_key, in_one) in [(b"p1", false), (b"p2", true)] {
        let top: &[&[u8]] = &[top_key];
        let tree: &[&[u8]] = &[top_key, b"t"];
        let nested: &[&[u8]] = &[top_key, b"t", b"n"];
        let deepest: &[&[u8]] = &[top_key, b"t", b"n", b"m"];
        let made = [
            (ROOT, &top_key[..], Element::empty_tree()),
            (top, b"t", Element::empty_tree()),
            (tree, b"k1", item(1)),
            (tree, b"n", Element::empty_tree()),
            (nested, b"k1", item(2)),
            (nested, b"k2", item(3)),
            (nested, b"m", Element::empty_tree()),
            (deepest, b"k1", item(4)),
        ];
        for (at, key, element) in made {
            h.write("made", db.insert(at, key, element, None, None, v))
                .expect("the trees of the history");
        }

        let transaction = in_one.then(|| db.start_transaction());
        let tx = transaction.as_ref();
        h.write_in("the nested delete", db.delete(tree, b"n", all(), tx, v), tx);
        h.write_in(
            "the move",
            db.apply_batch(vec![move_op(top_key, b"t", b"u")], None, tx, v),
            tx,
        );
        if let Some(transaction) = transaction {
            h.plain(
                "the transaction",
                ended(db.commit_transaction(transaction).unwrap()),
            );
        }

        let moved: &[&[u8]] = &[top_key, b"u"];
        let again: &[&[u8]] = &[top_key, b"u", b"n"];
        h.element(
            "the item of the moved tree",
            db.get_raw(moved.into(), b"k1", None, v),
        );
        h.read(
            "the deleted tree under the new key",
            db.has_raw(moved, b"n", None, v),
        );
        h.read("the old key", db.has_raw(top, b"t", None, v));
        h.write(
            "a tree under the key of the deleted one",
            db.insert(moved, b"n", Element::empty_tree(), None, None, v),
        );
        h.write(
            "an item in it",
            db.insert(again, b"k1", item(5), None, None, v),
        );
        h.write(
            "a tree under the old key",
            db.insert(top, b"t", Element::empty_tree(), None, None, v),
        );
        let old: &[&[u8]] = &[top_key, b"t"];
        h.write(
            "an item in it",
            db.insert(old, b"k1", item(6), None, None, v),
        );
        let everything = PathQuery::new_unsized(owned(top), Query::new_range_full());
        let pairs = QueryResultType::QueryKeyElementPairResultType;
        h.elements(
            "the trees",
            db.query_raw(&everything, true, true, true, pairs, None, v),
        );
    }
    verified(&mut h);
    h.answers
}

/// A tree is moved after a tree inside it was deleted with what it held
/// (D-213 item 2): the delete leaves nothing of the deleted tree for the
/// move to carry, and the moved tree, the key of the deleted one under it
/// and the old key are written again, on LigninDB as on RocksDB. The
/// grove versions before the fourth refuse a move alike.
#[test]
fn a_tree_is_moved_after_a_tree_inside_it_was_deleted_as_on_rocksdb() {
    on_both_engines!(
        moved_after_a_nested_delete,
        |answers: &[Answer], number: usize| {
            assert!(sound(answers), "grove version {number}");
            assert_eq!(taken(answers, "the nested delete"), 2);
            let moves = taken(answers, "the move");
            assert_eq!(
                moves,
                if number >= 4 { 2 } else { 0 },
                "grove version {number}"
            );
        }
    );
}

const FLAT_AT: &[&[u8]] = &[b"p", b"s", b"e"];
const DROPPED: &[&[u8]] = &[b"p", b"s", b"e", b"t"];
const BELOW: &[&[u8]] = &[b"p", b"s", b"e", b"t", b"00"];

/// Where the history of [`dropped_flat_with_a_tree_in_it`] makes the
/// dropped tree again.
const MADE_AGAIN: &str = "the dropped tree is made again";

/// Trees `p`, `s` and `e`, one inside the other. One batch makes a tree
/// `t` in `e`, a tree `00` in `t` and an item `old` in `00`, and `t` is
/// dropped flat, against the caller's word that it holds no tree. Then `t`
/// and `00` are made again at their paths, and `00` is read as it stands:
/// whether it is empty, what a range of a query finds in it, and an insert
/// of the key `old` only where it holds no element.
fn dropped_flat_with_a_tree_in_it<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let made = [(ROOT, &b"p"[..]), (&[b"p"], b"s"), (&[b"p", b"s"], b"e")];
    for (at, key) in made {
        h.write(
            "made",
            db.insert(at, key, Element::empty_tree(), None, None, v),
        )
        .expect("the trees above");
    }
    let insert_only = |at: &[&[u8]], key: &[u8], element: Element| {
        QualifiedGroveDbOp::insert_only_op(owned(at), key.to_vec(), element)
    };
    let batch = vec![
        insert_only(FLAT_AT, b"t", Element::empty_tree()),
        insert_only(DROPPED, b"00", Element::empty_tree()),
        insert_only(BELOW, b"old", item(1)),
    ];
    h.write("the batch", db.apply_batch(batch, None, None, v))
        .expect("the tree with a tree in it");
    h.write(
        "the flat drop",
        db.drop_flat_subtree(FLAT_AT, b"t", BackwardsReferences::DontCheck, None, v),
    )
    .expect("the flat drop is taken");
    h.read("the dropped tree", db.has_raw(FLAT_AT, b"t", None, v));
    verified(&mut h);

    h.write(
        MADE_AGAIN,
        db.insert(FLAT_AT, b"t", Element::empty_tree(), None, None, v),
    );
    h.write(
        "the tree in it is made again",
        db.insert(DROPPED, b"00", Element::empty_tree(), None, None, v),
    );
    h.read("is it empty", db.is_empty_tree(BELOW, None, v));
    let everything = PathQuery::new_unsized(owned(BELOW), Query::new_range_full());
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    h.elements(
        "a range of a query",
        db.query_raw(&everything, true, true, true, pairs, None, v),
    );
    h.read("the old key", db.has_raw(BELOW, b"old", None, v));
    h.write(
        "an insert only where the key holds none",
        db.insert_if_not_exists(BELOW, b"old", item(2), None, v),
    );
    h.element(
        "the element under the old key",
        db.get_raw(BELOW.into(), b"old", None, v),
    );
    h.answers
}

/// A flat drop of a tree that holds a tree, and the dropped path made
/// again before anything else (D-324; `transactions.md` 14.5, row D3).
///
/// A flat drop is its caller's word that the tree holds no tree. Where it
/// holds one, RocksDB keeps the records of that tree under their prefix,
/// which the drain of the drop does not name, and LigninDB drops them with
/// the drop. Up to the drop, and for the read of the dropped tree after
/// it, the two engines answer and are charged alike. A caller that makes
/// the trees again at the dropped path then reads the old records on
/// RocksDB and none on LigninDB. **RocksDB's answers are the required
/// ones; LigninDB's are what it gives today**, an open difference that is
/// put to the maintainer of GroveDB. The differential fuzzer counts the
/// cases that come to it (`lignin-testkit::diff::families`).
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_tree_made_again_below_a_flat_drop_reads_what_rocksdb_kept_and_lignindb_dropped() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate().skip(3) {
        let number = number + 1;
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let answers = dropped_flat_with_a_tree_in_it(&db, v);
            db.flush().expect("a flush");
            answers
        };
        check(dir.path(), v);
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
            .expect("a database on RocksDB");
        let on_rocksdb = dropped_flat_with_a_tree_in_it(&db, v);

        // Alike up to the drop and the read after it.
        let again = (on_rocksdb.iter())
            .position(|(call, ..)| call == MADE_AGAIN)
            .expect("the history makes the tree again");
        assert_eq!(
            on_lignin[..again],
            on_rocksdb[..again],
            "grove version {number}"
        );
        assert_eq!(answer_of(&on_rocksdb, "the dropped tree"), "Ok(false)");

        // Required: RocksDB reads the records it kept.
        assert_eq!(answer_of(&on_rocksdb, "is it empty"), "Ok(false)");
        assert_eq!(answer_of(&on_rocksdb, "the old key"), "Ok(true)");
        assert!(answer_of(&on_rocksdb, "a range of a query").starts_with("Ok(([(["));
        assert_eq!(
            answer_of(&on_rocksdb, "an insert only where the key holds none"),
            "Ok(false)"
        );
        // What is: LigninDB reads a tree with nothing in it.
        assert_eq!(answer_of(&on_lignin, "is it empty"), "Ok(true)");
        assert_eq!(answer_of(&on_lignin, "the old key"), "Ok(false)");
        assert_eq!(answer_of(&on_lignin, "a range of a query"), "Ok(([], 0))");
        assert_eq!(
            answer_of(&on_lignin, "an insert only where the key holds none"),
            "Ok(true)"
        );
    }
}
