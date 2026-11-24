//! A call that is refused after a part of it was applied to the caller's
//! transaction, and what a commit keeps of it (GROVE-04-F3).
//!
//! A dedicated insert into an indexed tree, over a child that is a tree,
//! removes the storage of that child and then puts the element
//! (`insert_into_indexed_tree_via_batch`), and the put can be refused
//! after the removal was applied. In a transaction of the caller, on an
//! engine that rolls a write scope back (`Engine::rolls_back_write_scopes`),
//! GroveDB makes the two in one write scope and rolls the scope back with
//! the refusal. LigninDB is such an engine: the child is as it was before
//! the call, and the store is sound after the commit. RocksDB is none, and
//! the call opens no scope there: the removal stays in the transaction,
//! and after the commit the element names a root that its subtree does not
//! hold, which `verify_grovedb` reports.
//!
//! The put is refused in two ways here. On grove versions 1 to 3 a delete
//! at the root path that changes the root of the root Merk leaves its
//! root-key record behind ([`made_again`](super::made_again)), and a put
//! below fails where it reaches the root Merk
//! ([`after_a_delete_at_the_root`]). On every grove version the removal
//! of a child with a negative sum makes the sum of a sum-indexed tree
//! overflow ([`by_an_overflow`]).
//!
//! Where the caller gives the view of a write scope of its own, LigninDB
//! opens no scope on it, and the two writes go into the caller's scope. A
//! scope in which a call was refused is rolled back, and the removal goes
//! with it
//! ([`a_scope_of_the_caller_in_which_the_insert_was_refused_is_rolled_back`]).
//!
//! An append to a tree that keeps flat records writes its records and
//! then the element of the tree, as two writes too ([`appended`]). Where
//! the second is refused, the first stays in the transaction. After the
//! commit the element is as before the call, the data keyspace of the tree
//! holds more records than before it, both engines answer alike, and the
//! store of LigninDB is sound. The trees of these histories hold appended
//! values only.

use grovedb_costs::{
    storage_cost::{removal::StorageRemovedBytes::BasicStorageRemoval, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_storage::{FlatStore, RawIterator};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, verified, Answer, Appended, Bulk, Commitment, Dense, History, Mmr},
    Lignin, LigninEngine,
};
use crate::{
    engine::Engine,
    operations::delete::DeleteOptions,
    tests::{faultable_reads::RefusedReads, pinned_reads::PinnedReads},
    Element, Error, GroveDb, RocksDbEngine, Transaction, TransactionArg,
};

const ROOT: &[&[u8]] = &[];
const TOP: &[u8] = b"indexed";
const INDEX: &[u8] = b"by";
const CHILD: &[u8] = b"g";
const HELD: &[u8] = b"n";
/// The tree at the root path that is the root node of the root Merk, and
/// the one that is its root once the first is deleted.
const DELETED: &[u8] = b"m";
const KEPT: &[u8] = b"z";

/// The call that is refused after a part of it was applied.
const AGAIN: &str = "again";

fn allowed() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

/// Makes two trees at the root path, the first with an item, and deletes
/// the first, which is the root node of the root Merk.
fn delete_at_the_root<'db, E: Engine>(h: &mut History<'db, E>, t: TransactionArg<'db, '_, E>) {
    let (db, v) = (h.db, h.v);
    let (tree, item) = (Element::empty_tree, Element::new_item(vec![9; 8]));
    h.write_in("tree", db.insert(ROOT, DELETED, tree(), None, t, v), t);
    h.write_in("item", db.insert(&[DELETED], HELD, item, None, t, v), t);
    h.write_in("tree", db.insert(ROOT, KEPT, tree(), None, t, v), t);
    h.write_in("delete", db.delete(ROOT, DELETED, allowed(), t, v), t);
}

/// What is read of the child under `key` that a dedicated insert writes
/// over.
fn child_under<'db, E: Engine>(h: &mut History<'db, E>, key: &[u8], t: TransactionArg<'db, '_, E>) {
    let (db, v) = (h.db, h.v);
    h.element("element", db.get(&[TOP, INDEX], key, t, v));
    h.element("held", db.get(&[TOP, INDEX, key], HELD, t, v));
    h.read("empty", db.is_empty_tree(&[TOP, INDEX, key], t, v));
}

/// What is read of the child under [`CHILD`].
fn child<'db, E: Engine>(h: &mut History<'db, E>, t: TransactionArg<'db, '_, E>) {
    child_under(h, CHILD, t);
}

/// A count tree with an item under a count-indexed tree, a delete at the
/// root path, and the dedicated insert of a count tree over the first, in
/// one transaction that is committed.
fn after_a_delete_at_the_root<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tx = db.start_transaction();
    let t = Some(&tx);
    let (index, branch) = (
        Element::empty_provable_count_indexed_tree(),
        Element::empty_count_tree,
    );
    h.write_in(
        "tree",
        db.insert(ROOT, TOP, Element::empty_tree(), None, t, v),
        t,
    );
    h.write_in("index", db.insert(&[TOP], INDEX, index, None, t, v), t);
    let made = db.insert_into_count_indexed_tree(&[TOP, INDEX], CHILD, branch(), t, v);
    h.write_in("branch", made, t);
    let item = Element::new_item(vec![7; 8]);
    h.write_in(
        "item",
        db.insert(&[TOP, INDEX, CHILD], HELD, item, None, t, v),
        t,
    );
    delete_at_the_root(&mut h, t);
    child(&mut h, t);
    let again = db.insert_into_count_indexed_tree(&[TOP, INDEX], CHILD, branch(), t, v);
    h.write_in(AGAIN, again, t);
    h.write("commit", db.commit_transaction(tx));
    child(&mut h, None);
    verified(&mut h);
    h.answers
}

/// Three sum trees under a sum-indexed tree, whose sums are in range
/// together and overflow without the negative one.
fn sum_trees<'db, E: Engine>(h: &mut History<'db, E>, t: TransactionArg<'db, '_, E>) {
    let (db, v) = (h.db, h.v);
    let index = Element::empty_provable_sum_indexed_tree();
    h.write_in(
        "tree",
        db.insert(ROOT, TOP, Element::empty_tree(), None, t, v),
        t,
    );
    h.write_in("index", db.insert(&[TOP], INDEX, index, None, t, v), t);
    for (key, sum) in [(CHILD, -10), (b"a", i64::MAX - 5), (b"z", 8)] {
        let branch = Element::empty_sum_tree();
        let made = db.insert_into_provable_sum_indexed_tree(&[TOP, INDEX], key, branch, t, v);
        h.write_in("branch", made, t);
        let item = Element::new_sum_item(sum);
        h.write_in(
            "item",
            db.insert(&[TOP, INDEX, key], HELD, item, None, t, v),
            t,
        );
    }
}

/// The dedicated insert of an empty sum tree over the one of
/// [`sum_trees`] under `key`.
fn over<'db, E: Engine>(
    db: &'db GroveDb<E>,
    key: &[u8],
    t: TransactionArg<'db, '_, E>,
    v: &GroveVersion,
) -> CostResult<(), Error> {
    let branch = Element::empty_sum_tree();
    db.insert_into_provable_sum_indexed_tree(&[TOP, INDEX], key, branch, t, v)
}

/// The dedicated insert of an empty sum tree over the one of
/// [`sum_trees`] that has the negative sum, which is refused.
fn over_the_negative_one<'db, E: Engine>(
    db: &'db GroveDb<E>,
    t: TransactionArg<'db, '_, E>,
    v: &GroveVersion,
) -> CostResult<(), Error> {
    over(db, CHILD, t, v)
}

/// The sum trees of [`sum_trees`], committed, and then the dedicated
/// insert of an empty sum tree over the one under `key`: in a transaction
/// of the caller that is committed, where GroveDB opens a write scope for
/// the call on LigninDB and none on RocksDB, or with no transaction, where
/// it opens none on either.
fn after_a_commit<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    key: &[u8],
    in_a_transaction: bool,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tx = db.start_transaction();
    sum_trees(&mut h, Some(&tx));
    h.write("commit", db.commit_transaction(tx));
    child_under(&mut h, key, None);
    if in_a_transaction {
        let tx = db.start_transaction();
        h.read(AGAIN, over(db, key, Some(&tx), v));
        h.write("commit", db.commit_transaction(tx));
    } else {
        h.read(AGAIN, over(db, key, None, v));
        h.write("commit", Ok(()).wrap_with_cost(OperationCost::default()));
    }
    child_under(&mut h, key, None);
    verified(&mut h);
    h.answers
}

/// The sum trees of [`sum_trees`] and the dedicated insert of an empty
/// sum tree over the negative one, in one transaction that is committed.
fn by_an_overflow<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tx = db.start_transaction();
    let t = Some(&tx);
    sum_trees(&mut h, t);
    child(&mut h, t);
    h.write_in(AGAIN, over_the_negative_one(db, t, v), t);
    h.write("commit", db.commit_transaction(tx));
    child(&mut h, None);
    verified(&mut h);
    h.answers
}

/// A tree of the kind `T` with one value, a delete at the root path, and
/// the append of a second value, in one transaction that is committed.
fn appended<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tx = db.start_transaction();
    let t = Some(&tx);
    h.write_in(
        "tree",
        db.insert(ROOT, TOP, Element::empty_tree(), None, t, v),
        t,
    );
    h.write_in(
        "kind",
        db.insert(&[TOP], CHILD, T::empty(None), None, t, v),
        t,
    );
    h.write_in("append", T::append(db, &[TOP], CHILD, 0, t, v), t);
    delete_at_the_root(&mut h, t);
    h.element("element", db.get(&[TOP], CHILD, t, v));
    h.plain("records", Ok(flat_records(db, &[TOP, CHILD], &tx)));
    h.write_in(AGAIN, T::append(db, &[TOP], CHILD, 1, t, v), t);
    h.write("commit", db.commit_transaction(tx));
    h.element("element", db.get(&[TOP], CHILD, None, v));
    let committed = db.start_transaction();
    h.plain("records", Ok(flat_records(db, &[TOP, CHILD], &committed)));
    drop(committed);
    T::reads(&mut h, &[TOP], CHILD, 1, None);
    verified(&mut h);
    h.answers
}

/// How many records the data keyspace of the subtree at `path` holds, as
/// `transaction` reads it.
fn flat_records<'db, E: Engine>(
    db: &'db GroveDb<E>,
    path: &[&[u8]],
    transaction: &Transaction<'db, E>,
) -> usize {
    let handle = db.engine().subtree(path.into(), None, transaction).unwrap();
    let mut cursor = FlatStore::cursor(&handle);
    let mut records = 0;
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        records += 1;
        cursor.next().unwrap();
    }
    cursor.status().expect("a cursor that walked");
    records
}

/// What the calls named `call` answered, in their order.
fn said<'a>(answers: &'a [Answer], call: &str) -> Vec<&'a str> {
    let named = answers.iter().filter(|(named, ..)| named == call);
    named.map(|(.., answer)| answer.as_str()).collect()
}

/// Whether the call that a history makes again was refused.
fn refused(answers: &[Answer]) -> bool {
    let again = said(answers, AGAIN);
    assert_eq!(again.len(), 1, "the call that is made again");
    again[0].starts_with("Err(")
}

/// Runs `$history` on LigninDB and on RocksDB on every grove version, has
/// the physical checker read the store of LigninDB, and gives the answers
/// of both to `$held` with the number of the grove version.
macro_rules! on_both {
    ($history:expr, $held:expr) => {{
        for (number, v) in GROVE_VERSIONS.iter().enumerate() {
            let number = number + 1;
            let dir = tempfile::tempdir().expect("a directory");
            let on_lignin = {
                let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                let answers = $history(&db, v);
                db.flush().expect("a flush");
                answers
            };
            check(dir.path(), v);
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            let on_rocksdb = $history(&db, v);
            $held(number, &on_lignin, &on_rocksdb);
        }
    }};
}

/// What both engines hold after a dedicated insert over a tree with an
/// item, which is refused on the grove versions `refuses` names.
fn kept(refuses: impl Fn(usize) -> bool) -> impl Fn(usize, &[Answer], &[Answer]) {
    move |number, lignin, rocksdb| {
        // Up to the commit and the root hash after it the engines answer
        // alike, the refusal and its cost among it: the removal changes
        // no node above the child.
        let commit = lignin.iter().position(|(call, ..)| call == "commit");
        let after = commit.expect("a commit") + 2;
        for (step, pair) in lignin[..after].iter().zip(&rocksdb[..after]).enumerate() {
            assert_eq!(pair.0, pair.1, "call {step} on grove version {number}");
        }
        assert_eq!(refused(lignin), refuses(number), "grove version {number}");
        let before_and_after = |answers, call| -> [String; 2] {
            let read: Vec<String> = said(answers, call).into_iter().map(String::from).collect();
            read.try_into()
                .expect("a read before the call and one after the commit")
        };
        let [element, element_after] = before_and_after(lignin, "element");
        let [held, held_after] = before_and_after(lignin, "held");
        let [empty, empty_after] = before_and_after(lignin, "empty");
        assert!(held.starts_with("Ok("), "{held}");
        assert_eq!(empty, "Ok(false)");
        if !refuses(number) {
            // The insert is made: an empty tree is where the child was.
            assert_eq!(lignin, rocksdb, "grove version {number}");
            assert_ne!(element_after, element);
            assert!(
                held_after.starts_with("Err(PathKeyNotFound("),
                "{held_after}"
            );
            assert_eq!(empty_after, "Ok(true)");
            assert_eq!(said(lignin, "verified"), ["Ok([])"]);
            return;
        }
        // LigninDB: the child is as it was before the call.
        assert_eq!(element_after, element, "grove version {number}");
        assert_eq!(held_after, held, "grove version {number}");
        assert_eq!(empty_after, empty, "grove version {number}");
        assert_eq!(
            said(lignin, "verified"),
            ["Ok([])"],
            "grove version {number}"
        );
        // RocksDB: the element is as it was, and its subtree is gone.
        let [was, is] = before_and_after(rocksdb, "element");
        assert_eq!((&was, &is), (&element, &element), "grove version {number}");
        let [_, held_after] = before_and_after(rocksdb, "held");
        assert!(
            held_after.starts_with("Err(PathKeyNotFound("),
            "{held_after}"
        );
        assert_eq!(before_and_after(rocksdb, "empty")[1], "Ok(true)");
        let child = vec![TOP.to_vec(), INDEX.to_vec(), CHILD.to_vec()];
        let named = format!("{:?}", Ok::<_, Error>(vec![child]));
        assert_eq!(said(rocksdb, "verified"), [named], "grove version {number}");
    }
}

/// The dedicated insert over a child that is a tree, refused after it
/// removed the storage of the child, in a transaction that is committed:
/// LigninDB keeps the child as it was and its store is sound, RocksDB
/// keeps the element with its subtree gone.
#[test]
fn a_refused_dedicated_insert_over_a_tree_keeps_the_tree_on_lignindb() {
    on_both!(after_a_delete_at_the_root, kept(|number| number <= 3));
    on_both!(by_an_overflow, kept(|_| true));
}

/// The refusal of an insert that leaves the sum of a sum-indexed tree out
/// of range.
const OVERFLOW: &str = "Err(CorruptedData(\"indexed secondary root hash capture after mirror: \
                        overflow error provable sum is overflowing\"))";

/// What upstream GroveDB is charged for the insert of [`after_a_commit`]
/// on the grove version `number`, where the put is made (over `z`) and
/// where it is refused (over [`CHILD`]): taken on RocksDB from this
/// branch's base, whose dedicated insert is upstream's.
fn charged_upstream(number: usize, made: bool) -> OperationCost {
    let (seek_count, replaced_bytes, storage_loaded_bytes, hash_node_calls) = match (number, made) {
        (1, false) => (18, 0, 2751, 22),
        (2 | 3, false) => (18, 0, 2767, 22),
        (4 | 5, false) => (19, 0, 3055, 23),
        (1, true) => (35, 870, 3320, 42),
        (2 | 3, true) => (35, 870, 3336, 42),
        (4 | 5, true) => (34, 870, 3476, 43),
        _ => panic!("a grove version with no cost taken: {number}"),
    };
    OperationCost {
        seek_count,
        storage_cost: StorageCost {
            added_bytes: 0,
            replaced_bytes,
            removed_bytes: BasicStorageRemoval(106),
        },
        storage_loaded_bytes,
        hash_node_calls,
        sinsemilla_hash_calls: 0,
    }
}

/// Each engine says whether it rolls a write scope back, and an engine
/// that wraps another says what that one does: LigninDB behind a wrapper
/// keeps the scope of the insert.
#[test]
fn an_engine_says_whether_it_rolls_a_write_scope_back() {
    let open = || tempfile::tempdir().expect("a directory");
    let dir = open();
    let lignin = Lignin::open_with_engine(dir.path()).expect("a database is made");
    assert!(lignin.engine().rolls_back_write_scopes());
    let dir = open();
    let rocksdb = GroveDb::<RocksDbEngine>::open_with_engine(dir.path()).expect("a database");
    assert!(!rocksdb.engine().rolls_back_write_scopes());
    let dir = open();
    let wrapped = GroveDb::<RefusedReads<LigninEngine>>::open_with_engine(dir.path());
    assert!(wrapped
        .expect("a database")
        .engine()
        .rolls_back_write_scopes());
    let dir = open();
    let wrapped = GroveDb::<RefusedReads<RocksDbEngine>>::open_with_engine(dir.path());
    assert!(!wrapped
        .expect("a database")
        .engine()
        .rolls_back_write_scopes());
    let dir = open();
    let wrapped = GroveDb::<PinnedReads>::open_with_engine(dir.path());
    assert!(!wrapped
        .expect("a database")
        .engine()
        .rolls_back_write_scopes());
}

/// The scope that GroveDB opens for the insert on LigninDB changes no
/// answer and no cost. With no transaction the call opens no scope, and in
/// a transaction of the caller it opens one on LigninDB and none on
/// RocksDB: the call answers and is charged the same either way on each
/// engine, where it is made and where it is refused, and that is what
/// upstream GroveDB answers and is charged ([`charged_upstream`]). Where
/// it is made, everything read afterwards is the same either way and on
/// both engines. Where it is refused, LigninDB holds the same either way,
/// and RocksDB holds what LigninDB does where the call had no transaction,
/// which it gave up.
#[test]
fn the_scope_of_the_insert_changes_no_answer_and_no_cost() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        for (key, made) in [(CHILD, false), (b"z".as_slice(), true)] {
            let on_lignin = [true, false].map(|in_a_transaction| {
                let dir = tempfile::tempdir().expect("a directory");
                let answers = {
                    let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                    let answers = after_a_commit(&db, v, key, in_a_transaction);
                    db.flush().expect("a flush");
                    answers
                };
                check(dir.path(), v);
                answers
            });
            let on_rocksdb = [true, false].map(|in_a_transaction| {
                let dir = tempfile::tempdir().expect("a directory");
                let db = GroveDb::<RocksDbEngine>::open_with_engine(dir.path())
                    .expect("a database on RocksDB");
                after_a_commit(&db, v, key, in_a_transaction)
            });
            let call = |answers: &[Answer]| {
                let call = answers.iter().find(|(call, ..)| call == AGAIN);
                call.cloned().expect("the call that is made again")
            };
            let told = format!("the insert over {key:?} on grove version {number}");
            let answer = if made { "Ok(())" } else { OVERFLOW };
            let upstream = (
                AGAIN.to_owned(),
                charged_upstream(number, made),
                answer.to_owned(),
            );
            for answers in on_lignin.iter().chain(&on_rocksdb) {
                assert_eq!(refused(answers), !made, "{told}");
                assert_eq!(call(answers), upstream, "{told}");
            }
            assert_eq!(on_lignin[0], on_lignin[1], "{told}");
            assert_eq!(on_lignin[1], on_rocksdb[1], "{told}");
            if made {
                assert_eq!(on_rocksdb[0], on_rocksdb[1], "{told}");
            }
        }
    }
}

/// A write scope of the caller in which a call was refused is rolled back,
/// not merged. Given the view of the caller's scope, GroveDB opens none of
/// its own, and the insert is refused by the overflow after it wrote its
/// removal into that scope: through the view the tree holds nothing. With
/// the scope the removal goes.
#[test]
fn a_scope_of_the_caller_in_which_the_insert_was_refused_is_rolled_back() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let dir = tempfile::tempdir().expect("a directory");
        let answers = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let mut h = History::new(&db, v);
            let tx = db.start_transaction();
            sum_trees(&mut h, Some(&tx));
            child(&mut h, Some(&tx));
            let scope = db.engine().open_write_scope(&tx).expect("a scope");
            let view = LigninEngine::scope_transaction(&scope);
            h.read(AGAIN, over_the_negative_one(&db, Some(view), v));
            let emptied = db.is_empty_tree(&[TOP, INDEX, CHILD], Some(view), v);
            h.read("empty in the scope", emptied);
            (db.engine().rollback_write_scope(scope)).expect("the scope is rolled back");
            h.write("commit", db.commit_transaction(tx));
            child(&mut h, None);
            verified(&mut h);
            db.flush().expect("a flush");
            h.answers
        };
        check(dir.path(), v);
        assert_eq!(said(&answers, AGAIN), [OVERFLOW], "grove version {number}");
        let emptied = said(&answers, "empty in the scope");
        assert_eq!(emptied, ["Ok(true)"], "grove version {number}");
        for call in ["element", "held", "empty"] {
            let read = said(&answers, call);
            assert_eq!(read.len(), 2, "{call} before the call and after the commit");
            assert_eq!(read[0], read[1], "{call} on grove version {number}");
        }
        assert_eq!(said(&answers, "held")[1].get(..3), Some("Ok("));
        assert_eq!(
            said(&answers, "verified"),
            ["Ok([])"],
            "grove version {number}"
        );
    }
}

/// What both engines hold after an append that is refused where it writes
/// the element, on the grove versions that leave the root-key record
/// behind: the same, with the records that the refused append wrote, and
/// the store of LigninDB is sound.
fn alike(number: usize, lignin: &[Answer], rocksdb: &[Answer]) {
    assert_eq!(lignin.len(), rocksdb.len(), "grove version {number}");
    for (step, pair) in lignin.iter().zip(rocksdb).enumerate() {
        assert_eq!(pair.0, pair.1, "call {step} on grove version {number}");
    }
    assert_eq!(said(lignin, "kind")[0], "Ok(())", "grove version {number}");
    assert_eq!(refused(lignin), number <= 3, "grove version {number}");
    let element = said(lignin, "element");
    assert_eq!(
        element.len(),
        2,
        "the element before the call and after the commit"
    );
    assert_eq!(
        element[0] == element[1],
        number <= 3,
        "grove version {number}"
    );
    // The append wrote its records before it was refused, and they are
    // committed with the transaction.
    let records = said(lignin, "records");
    let count = |at: usize| -> usize {
        let read = records[at]
            .strip_prefix("Ok(")
            .and_then(|read| read.strip_suffix(')'));
        read.and_then(|read| read.parse().ok())
            .expect("a count of records")
    };
    assert_eq!(
        records.len(),
        2,
        "the records before the call and after the commit"
    );
    assert!(count(0) < count(1), "{records:?} on grove version {number}");
}

#[test]
fn an_append_refused_after_its_records_leaves_a_sound_store() {
    on_both!(appended::<Mmr, _>, alike);
    on_both!(appended::<Bulk, _>, alike);
    on_both!(appended::<Dense, _>, alike);
    on_both!(appended::<Commitment, _>, alike);
}
