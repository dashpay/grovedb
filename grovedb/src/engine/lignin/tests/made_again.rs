//! A root-key record that does not name the root of its Merk, and what
//! the LigninDB engine does with it (GROVE-05-F1, D-357).
//!
//! On `GROVE_V1` to `GROVE_V3` the delete of a tree that holds elements
//! writes the Merk the tree is in through a Merk that is opened with a
//! root key given (`delete_internal_on_transaction` v0, GroveDB #686),
//! and the commit of such a Merk writes no root-key record (GroveDB
//! #1065). At the root path that Merk is the root Merk, which every
//! other call opens from its record. So where the delete changes the root of the root Merk, the
//! record names the old root from there on, on both engines: the root
//! Merk is opened with no node, or with the old root and what is below
//! it. RocksDB takes the writes of such a Merk. LigninDB refuses them
//! with `NotSupported`, changes nothing, and answers every read as
//! RocksDB does on the state the delete left.
//!
//! Each history is written once and runs on both engines on every grove
//! version. On grove versions 4 and 5 the two engines answer alike to
//! the end. On grove versions 1 to 3 they answer alike up to the first
//! write that LigninDB refuses, and from there LigninDB's answers are
//! RocksDB's where RocksDB is not asked for the refused writes
//! ([`class`]). The two ways into the state are the delete of the root
//! node of the root Merk ([`four_steps`]) and a delete that changes the
//! root by a rotation ([`rotation`]).

use std::panic::{catch_unwind, AssertUnwindSafe};

use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::proofs::Query;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, verified, Answer, History},
    whole::Whole,
    Lignin,
};
use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp as Op},
    engine::Engine,
    operations::delete::DeleteOptions,
    query_result_type::QueryResultType,
    Element, Error, GroveDb, PathQuery, RocksDbEngine, TransactionArg,
};

const ROOT: &[&[u8]] = &[];
const MADE: &[u8] = b"made";
const OVER: &[u8] = b"over";
const ONE: &[u8] = b"one";

/// What a history has in the place of a write that it does not make.
const NOT_MADE: &str = "not made";

fn allowed() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

fn owned(path: &[&[u8]]) -> Vec<Vec<u8>> {
    path.iter().map(|segment| segment.to_vec()).collect()
}

fn item(n: u8) -> Element {
    Element::new_item(vec![n])
}

/// The refusal of a write of the root Merk, whose root-key record is
/// `record` while its root is the node under `root`.
fn refusal_of(record: Option<&[u8]>, root: &[u8]) -> String {
    let record = match record {
        Some(key) => format!("names the key {}", hex::encode(key)),
        None => "is absent".to_owned(),
    };
    format!(
        "Err(NotSupported(\"lignindb: the root-key record of Merk {} {record}, and the root of \
         that Merk is the node under the key {}: no node of a Merk is written where its \
         root-key record was read and does not name its root. On grove versions 1 to 3 the \
         delete of a tree that holds elements leaves the record so where it changes the root \
         of the Merk the tree was in \
         (grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs, GroveDB #686 and #1065)\"))",
        hex::encode([0; 32]),
        hex::encode(root),
    )
}

fn refusal(record: &[u8], root: &[u8]) -> String {
    refusal_of(Some(record), root)
}

/// Whether `answer` is the refusal of a write of a Merk whose root-key
/// record was left behind.
fn is_refusal(answer: &str) -> bool {
    answer.starts_with("Err(NotSupported(\"lignindb: the root-key record of Merk ")
        && answer.contains("delete_internal_on_transaction/v0.rs, GroveDB #686")
}

/// The calls of a history, with which of them write elements.
struct Run<'db, E: Engine> {
    h: History<'db, E>,
    /// The places in the answers of the writes of elements, in order.
    writes: Vec<usize>,
    /// The writes of elements that are not made.
    not_made: NotMade<'db>,
    /// A write panicked: the history ends there.
    panicked: bool,
}

/// The writes of elements that a history does not make, by their place
/// among its writes of elements.
type NotMade<'a> = &'a [usize];

const ALL: NotMade<'static> = &[];

/// What a history came to.
#[derive(Debug, PartialEq)]
struct Ran {
    answers: Vec<Answer>,
    writes: Vec<usize>,
    panicked: bool,
}

impl<'db, E: Engine> Run<'db, E> {
    fn new(db: &'db GroveDb<E>, v: &'db GroveVersion, not_made: NotMade<'db>) -> Self {
        Self {
            h: History::new(db, v),
            writes: Vec::new(),
            not_made,
            panicked: false,
        }
    }

    /// Takes in a write of elements and the root hash that `through` reads
    /// after it. A write that the history does not make leaves
    /// [`NOT_MADE`] in its place, and the root hash is read all the same.
    fn write<T: Whole>(
        &mut self,
        name: &str,
        through: TransactionArg<'db, '_, E>,
        call: impl FnOnce() -> CostResult<T, Error>,
    ) {
        if self.panicked {
            return;
        }
        let ordinal = self.writes.len();
        self.writes.push(self.h.answers.len());
        let blank = |answer: &str| (name.to_owned(), OperationCost::default(), answer.to_owned());
        if self.not_made.contains(&ordinal) {
            self.h.answers.push(blank(NOT_MADE));
        } else {
            match catch_unwind(AssertUnwindSafe(call)) {
                Ok(answer) => drop(self.h.read(name, answer)),
                Err(_) => {
                    self.h.answers.push(blank("panic"));
                    self.panicked = true;
                    return;
                }
            }
        }
        let root = self.h.db.root_hash(through, self.h.v);
        self.h.read("root", root);
    }

    /// Takes in a call that is no write of an element, and the root hash
    /// after it.
    fn other<T: Whole>(&mut self, name: &str, call: impl FnOnce() -> CostResult<T, Error>) {
        if self.panicked {
            return;
        }
        self.h.read(name, call());
        let root = self.h.db.root_hash(None, self.h.v);
        self.h.read("root", root);
    }

    /// Reads what the histories of this file write: the two trees at
    /// `keys` under `above` and an element in each, a query and a proof of
    /// everything under `above`, and the grove's own check.
    fn reads(&mut self, above: &[&[u8]], keys: &[&[u8]], through: TransactionArg<'db, '_, E>) {
        if self.panicked {
            return;
        }
        let (db, v) = (self.h.db, self.h.v);
        let h = &mut self.h;
        for key in keys {
            let name = String::from_utf8_lossy(key).into_owned();
            let below: Vec<&[u8]> = above.iter().copied().chain([*key]).collect();
            h.element(format!("get {name}"), db.get(above, key, through, v));
            for element in [ONE, b"x", b"y"] {
                let read = db.get(below.as_slice(), element, through, v);
                h.element(format!("get {name}/{}", element[0] as char), read);
            }
            h.read(format!("has {name}"), db.has_raw(above, key, through, v));
            let empty = db.is_empty_tree(below.as_slice(), through, v);
            h.read(format!("empty {name}"), empty);
        }
        let mut all = Query::new();
        all.insert_all();
        let query = PathQuery::new_unsized(owned(above), all);
        let pairs = QueryResultType::QueryKeyElementPairResultType;
        let read = db.query_raw(&query, true, true, true, pairs, through, v);
        h.elements("query", read);
        if through.is_none() {
            // A debug build of GroveDB asserts in a proof that a tree which
            // keeps flat records has the state its element commits to.
            match catch_unwind(AssertUnwindSafe(|| db.prove_query(&query, None, v))) {
                Ok(proof) => drop(h.read("proof", proof)),
                Err(_) => (h.answers).push((
                    "proof".to_owned(),
                    OperationCost::default(),
                    "panic".to_owned(),
                )),
            }
            verified(h);
        }
    }

    fn done(self) -> Ran {
        Ran {
            answers: self.h.answers,
            writes: self.writes,
            panicked: self.panicked,
        }
    }
}

/// Runs `history` on LigninDB, closes the database and has the physical
/// checker read it.
fn on_lignindb(v: &GroveVersion, history: impl FnOnce(&Lignin) -> Ran) -> Ran {
    let dir = tempfile::tempdir().expect("a directory");
    let ran = {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let ran = history(&db);
        db.flush().expect("a flush");
        ran
    };
    check(dir.path(), v);
    ran
}

fn on_rocksdb(history: impl FnOnce(&GroveDb<RocksDbEngine>) -> Ran) -> Ran {
    let dir = tempfile::tempdir().expect("a directory");
    let db = GroveDb::<RocksDbEngine>::open_with_engine(dir.path()).expect("a database");
    history(&db)
}

/// The writes of `ran` that LigninDB refused because a root-key record
/// does not name the root of its Merk: the place of each among the writes
/// of elements, its call and its answer.
fn refusals(ran: &Ran) -> Vec<(usize, &str, &str)> {
    (ran.writes.iter().enumerate())
        .map(|(write, at)| (write, &ran.answers[*at]))
        .filter(|(_, (_, _, answer))| is_refusal(answer))
        .map(|(write, (call, _, answer))| (write, call.as_str(), answer.as_str()))
        .collect()
}

/// Holds what LigninDB answered against RocksDB, and says which of three
/// things the history was:
///
/// - `equal`: every answer, cost and root hash is RocksDB's.
/// - `panic`: a write panicked on both engines, after the same answers.
/// - `refused`: LigninDB refused writes because a root-key record does
///   not name the root of its Merk, each of them a write that RocksDB
///   took, and every other answer, cost and root hash of the history is
///   what RocksDB gives where it is not asked for the refused writes
///   (`without`, which is handed the writes to leave out).
///
/// `expected` is what the caller knows of the history by itself: the
/// calls that are refused, in order, each with the words of its refusal.
/// The writes that LigninDB refused are held to that list before
/// anything is run without them, so a refusal of another call, or with
/// other words, or one that is missing, fails here.
///
/// Anything else fails.
fn class(
    told: &str,
    lignin: &Ran,
    rocksdb: &Ran,
    expected: &[(&str, String)],
    without: impl Fn(&[usize]) -> Ran,
) -> &'static str {
    let refused = refusals(lignin);
    let seen: Vec<(&str, &str)> = (refused.iter())
        .map(|(_, call, answer)| (*call, *answer))
        .collect();
    let expected: Vec<(&str, &str)> = (expected.iter())
        .map(|(call, answer)| (*call, answer.as_str()))
        .collect();
    assert_eq!(seen, expected, "{told}: the refused writes");
    if lignin == rocksdb {
        return match lignin.panicked {
            true => "panic",
            false => "equal",
        };
    }
    assert!(
        !refused.is_empty(),
        "{told}: the engines differ with no write refused"
    );
    assert!(!lignin.panicked && !rocksdb.panicked, "{told}");
    let refused: Vec<usize> = refused.iter().map(|(write, ..)| *write).collect();
    // Each refused write is one that RocksDB takes on the state it is
    // made on: the state of the history without the writes that were
    // refused before it.
    for (nth, write) in refused.iter().enumerate() {
        let at = lignin.writes[*write];
        let taken = match nth {
            0 => rocksdb.answers[at].clone(),
            _ => without(&refused[..nth]).answers[at].clone(),
        };
        assert!(
            taken.2.starts_with("Ok("),
            "{told}: RocksDB answered {taken:?}"
        );
    }

    let without = without(&refused);
    assert_eq!(lignin.answers.len(), without.answers.len(), "{told}");
    for (at, (seen, expected)) in lignin.answers.iter().zip(&without.answers).enumerate() {
        match lignin.writes.iter().position(|write| *write == at) {
            Some(write) if refused.contains(&write) => assert_eq!(expected.2, NOT_MADE, "{told}"),
            _ => assert_eq!(seen, expected, "{told}: call {at}"),
        }
    }
    "refused"
}

/// The trees `made` and `over` at the root, each with an item, `made`
/// first: the root node of the root Merk.
fn two_trees<E: Engine>(run: &mut Run<'_, E>) {
    let (db, v) = (run.h.db, run.h.v);
    let tree = Element::empty_tree;
    run.write("made", None, || {
        db.insert(ROOT, MADE, tree(), None, None, v)
    });
    run.write("made/one", None, || {
        db.insert(&[MADE], ONE, item(1), None, None, v)
    });
    run.write("over", None, || {
        db.insert(ROOT, OVER, tree(), None, None, v)
    });
    run.write("over/x", None, || {
        db.insert(&[OVER], b"x", item(2), None, None, v)
    });
}

const TWO: &[&[u8]] = &[MADE, OVER];

/// The four steps of GROVE-05-F1 with no transaction, and what follows
/// them: `made` and `over`, the delete of `made` with what it holds,
/// `made` again, an item under `over`, a tree `zed`, an item over `over`,
/// the delete of `over`, a batch that deletes a key which holds nothing,
/// `zed` once more, and an aux record, which is no write of an element.
/// Everything is read after each.
fn four_steps<'db, E: Engine>(
    db: &'db GroveDb<E>,
    v: &'db GroveVersion,
    not_made: NotMade<'db>,
) -> Ran {
    let mut run = Run::new(db, v, not_made);
    let tree = Element::empty_tree;
    two_trees(&mut run);
    run.reads(ROOT, TWO, None);
    run.write("delete", None, || db.delete(ROOT, MADE, allowed(), None, v));
    run.reads(ROOT, TWO, None);
    run.write("made again", None, || {
        db.insert(ROOT, MADE, tree(), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.write("over/y", None, || {
        db.insert(&[OVER], b"y", item(3), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.write("zed", None, || {
        db.insert(ROOT, b"zed", tree(), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.write("over again", None, || {
        db.insert(ROOT, OVER, item(9), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.write("over deleted", None, || {
        db.delete(ROOT, OVER, allowed(), None, v)
    });
    run.reads(ROOT, TWO, None);
    // A batch that deletes a key which the root Merk as it is opened does
    // not hold commits that Merk with no node: it removes the record.
    run.write("absent deleted", None, || {
        let op = Op::delete_op(Vec::new(), b"absent".to_vec());
        db.apply_batch(vec![op], None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.write("zed again", None, || {
        db.insert(ROOT, b"zed", tree(), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.other("aux", || db.put_aux(b"aux", b"value", None, None));
    run.h.read("aux read", db.get_aux(b"aux", None));
    run.done()
}

/// The calls of `ran` that LigninDB refused because a root-key record
/// was left behind.
fn refused(ran: &Ran) -> Vec<&str> {
    (ran.writes.iter())
        .map(|at| &ran.answers[*at])
        .filter(|(_, _, answer)| is_refusal(answer))
        .map(|(call, ..)| call.as_str())
        .collect()
}

/// What `ran` answered to its call `call`.
fn answered<'r>(ran: &'r Ran, call: &str) -> &'r str {
    let mut answers = ran.answers.iter().filter(|(named, ..)| named == call);
    let (_, _, answer) = answers.next().expect("the call was made");
    assert!(answers.next().is_none(), "{call} was made once");
    answer
}

/// On grove versions 1 to 3 the write after the delete is refused with
/// the words of the refusal, which name the record, the root and where
/// the record was left. No write of an element is taken from there on,
/// and every read after a refused write answers as before it, with
/// RocksDB's answer and cost on the state the delete left (`class`); an
/// aux record is written; and the store is sound when it is closed. On
/// grove versions 4 and 5 nothing is refused.
#[test]
fn a_write_after_the_delete_that_left_the_root_key_behind_is_refused_and_changes_nothing() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let told = format!("grove version {number}");
        let lignin = on_lignindb(v, |db| four_steps(db, v, ALL));
        let rocksdb = on_rocksdb(|db| four_steps(db, v, ALL));
        let without = |refused: &[usize]| on_rocksdb(|db| four_steps(db, v, refused));
        // The record names `made` and the root is `over`, until the batch
        // that deletes nothing removes the record.
        let expected = match number {
            1..=3 => vec![
                ("made again", refusal(MADE, OVER)),
                ("zed", refusal(MADE, OVER)),
                ("over again", refusal(MADE, OVER)),
                ("zed again", refusal_of(None, OVER)),
            ],
            _ => Vec::new(),
        };
        let class = class(&told, &lignin, &rocksdb, &expected, without);
        println!("FOUR STEPS {told}: {class}, refused {:?}", refused(&lignin));
        assert_eq!(answered(&lignin, "delete"), "Ok(())", "{told}");
        assert_eq!(answered(&lignin, "aux"), "Ok(())", "{told}");
        let value = "Ok(Some([118, 97, 108, 117, 101]))";
        assert_eq!(answered(&lignin, "aux read"), value, "{told}");
        match number {
            1..=3 => {
                assert_eq!(class, "refused", "{told}");
                // The batch that removes the record is taken, as on
                // RocksDB.
                assert_eq!(answered(&lignin, "absent deleted"), "Ok(())", "{told}");
            }
            _ => assert_eq!(class, "equal", "{told}"),
        }
    }
}

/// How the transaction of [`ended`] ends.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ending {
    RolledBack,
    Dropped,
    Committed,
}

/// The delete and the write after it in one transaction, which is then
/// rolled back, dropped with no commit, or committed; and three writes
/// after it.
fn ended<'db, E: Engine>(
    db: &'db GroveDb<E>,
    v: &'db GroveVersion,
    ending: Ending,
    not_made: NotMade<'db>,
) -> Ran {
    let mut run = Run::new(db, v, not_made);
    let tree = Element::empty_tree;
    two_trees(&mut run);
    run.reads(ROOT, TWO, None);
    let transaction = db.start_transaction();
    let through = Some(&transaction);
    run.write("delete", through, || {
        db.delete(ROOT, MADE, allowed(), through, v)
    });
    run.reads(ROOT, TWO, through);
    run.write("made again", through, || {
        db.insert(ROOT, MADE, tree(), None, through, v)
    });
    run.reads(ROOT, TWO, through);
    run.reads(ROOT, TWO, None);
    match ending {
        Ending::Dropped => drop(transaction),
        Ending::RolledBack => {
            run.h
                .plain("rollback", db.rollback_transaction(&transaction));
            run.reads(ROOT, TWO, Some(&transaction));
            drop(transaction);
        }
        Ending::Committed => run.other("commit", || db.commit_transaction(transaction)),
    }
    run.reads(ROOT, TWO, None);
    run.write("zed", None, || {
        db.insert(ROOT, b"zed", tree(), None, None, v)
    });
    run.write("made/y", None, || {
        db.insert(&[MADE], b"y", item(4), None, None, v)
    });
    run.write("over/y", None, || {
        db.insert(&[OVER], b"y", item(3), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.done()
}

/// On grove versions 1 to 3 the write after the delete is refused in the
/// transaction, which still reads what its delete left. Rolled back or
/// dropped, it leaves the grove as it was before the delete, and the
/// grove takes the three writes. Committed, it commits the delete alone:
/// the next write of the root Merk is refused, and the two writes into
/// the trees fail as they do on RocksDB on that state.
#[test]
fn a_transaction_with_the_delete_and_a_refused_write_is_rolled_back_or_committed() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        for ending in [Ending::RolledBack, Ending::Dropped, Ending::Committed] {
            let told = format!("grove version {number}, {ending:?}");
            let lignin = on_lignindb(v, |db| ended(db, v, ending, ALL));
            let rocksdb = on_rocksdb(|db| ended(db, v, ending, ALL));
            let without = |refused: &[usize]| on_rocksdb(|db| ended(db, v, ending, refused));
            let mut expected = Vec::new();
            if number <= 3 {
                expected.push(("made again", refusal(MADE, OVER)));
                if ending == Ending::Committed {
                    expected.push(("zed", refusal(MADE, OVER)));
                }
            }
            let class = class(&told, &lignin, &rocksdb, &expected, without);
            assert_eq!(answered(&lignin, "delete"), "Ok(())", "{told}");
            match number {
                1..=3 => assert_eq!(class, "refused", "{told}"),
                _ => assert_eq!(class, "equal", "{told}"),
            }
            if ending == Ending::Committed {
                assert_eq!(answered(&lignin, "commit"), "Ok(())", "{told}");
            }
            if number > 3 || ending != Ending::Committed {
                for call in ["zed", "made/y", "over/y"] {
                    assert_eq!(answered(&lignin, call), "Ok(())", "{told}: {call}");
                }
            }
        }
    }
}

const FOUR: &[&[u8]] = &[b"b", b"a", b"c", b"d"];

/// Four trees at the root, each with an item, `b` the root node, and the
/// delete of `a` with what it holds, which makes `c` the root by a
/// rotation.
fn rotated<E: Engine>(run: &mut Run<'_, E>) {
    let (db, v) = (run.h.db, run.h.v);
    for key in FOUR {
        let name = String::from_utf8_lossy(key).into_owned();
        run.write(&name, None, || {
            db.insert(ROOT, key, Element::empty_tree(), None, None, v)
        });
        run.write(&format!("{name}/x"), None, || {
            db.insert(&[*key], b"x", item(key[0]), None, None, v)
        });
    }
    run.reads(ROOT, FOUR, None);
    run.write("delete a", None, || {
        db.delete(ROOT, b"a", allowed(), None, v)
    });
    run.reads(ROOT, FOUR, None);
}

/// The second way to a root-key record that is left behind: four trees
/// at the root, `b` the root node; the delete of `a` with what it holds
/// makes `c` the root by a rotation, and `b` stays in the tree. The
/// record names `b`, so the root Merk is opened as `b` alone. Then an
/// item under `b`, which is carried up to the root Merk; the delete of
/// `b` with what it holds, which writes the root Merk through a second
/// handle that is given the key of the root; a tree `e`; a delete of `c`,
/// which the root Merk as it is read does not hold; and a batch that
/// deletes a key which holds nothing: Merk writes the root node of a
/// Merk that has one again with every commit, so that batch holds a node
/// here, where the same batch on a root Merk that is opened with no node
/// holds none ([`four_steps`]).
fn rotation<'db, E: Engine>(
    db: &'db GroveDb<E>,
    v: &'db GroveVersion,
    not_made: NotMade<'db>,
) -> Ran {
    let mut run = Run::new(db, v, not_made);
    let tree = Element::empty_tree;
    rotated(&mut run);
    run.write("b/y", None, || {
        db.insert(&[b"b"], b"y", item(7), None, None, v)
    });
    run.reads(ROOT, FOUR, None);
    run.write("delete b", None, || {
        db.delete(ROOT, b"b", allowed(), None, v)
    });
    run.reads(ROOT, FOUR, None);
    run.write("e", None, || db.insert(ROOT, b"e", tree(), None, None, v));
    run.reads(ROOT, FOUR, None);
    run.write("delete c", None, || {
        db.delete(ROOT, b"c", allowed(), None, v)
    });
    run.reads(ROOT, FOUR, None);
    run.write("absent deleted", None, || {
        let op = Op::delete_op(Vec::new(), b"absent".to_vec());
        db.apply_batch(vec![op], None, None, v)
    });
    run.reads(ROOT, FOUR, None);
    run.done()
}

#[test]
fn a_delete_that_changes_the_root_by_a_rotation_leaves_the_same_refusal() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let told = format!("grove version {number}");
        let lignin = on_lignindb(v, |db| rotation(db, v, ALL));
        let rocksdb = on_rocksdb(|db| rotation(db, v, ALL));
        let without = |refused: &[usize]| on_rocksdb(|db| rotation(db, v, refused));
        // The record names `b` and the root is `c`.
        let all = ["b/y", "delete b", "e", "absent deleted"];
        let expected: Vec<_> = match number {
            1..=3 => all
                .iter()
                .map(|call| (*call, refusal(b"b", b"c")))
                .collect(),
            _ => Vec::new(),
        };
        let class = class(&told, &lignin, &rocksdb, &expected, without);
        println!("ROTATION {told}: {class}, refused {:?}", refused(&lignin));
        assert_eq!(answered(&lignin, "delete a"), "Ok(())", "{told}");
        match number {
            1..=3 => assert_eq!(class, "refused", "{told}"),
            _ => assert_eq!(class, "equal", "{told}"),
        }
    }
}

/// `made` alone at the root, deleted with what it holds and made again.
/// On grove versions 1 to 3 the record names `made` after the delete as
/// well, and the root Merk has no node: the directory holds no root for
/// it, the record is not judged, and every write is taken as on
/// RocksDB.
fn alone<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Ran {
    let mut run = Run::new(db, v, ALL);
    let tree = Element::empty_tree;
    run.write("made", None, || {
        db.insert(ROOT, MADE, tree(), None, None, v)
    });
    run.write("made/one", None, || {
        db.insert(&[MADE], ONE, item(1), None, None, v)
    });
    run.write("delete", None, || db.delete(ROOT, MADE, allowed(), None, v));
    run.reads(ROOT, TWO, None);
    run.write("over", None, || {
        db.insert(ROOT, OVER, tree(), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.write("made again", None, || {
        db.insert(ROOT, MADE, tree(), None, None, v)
    });
    run.write("made/y", None, || {
        db.insert(&[MADE], b"y", item(4), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.done()
}

#[test]
fn a_root_merk_that_the_delete_left_with_no_node_takes_writes() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let told = format!("grove version {}", number + 1);
        let lignin = on_lignindb(v, |db| alone(db, v));
        let rocksdb = on_rocksdb(|db| alone(db, v));
        assert_eq!(lignin, rocksdb, "{told}");
        assert!(!lignin.panicked, "{told}");
        for call in ["delete", "over", "made again", "made/y"] {
            assert_eq!(answered(&lignin, call), "Ok(())", "{told}: {call}");
        }
    }
}

/// The record is as it was left when the database is opened again: the
/// write is refused with the same words, and the store is sound after
/// each close.
#[test]
fn the_refusal_holds_when_the_database_is_opened_again() {
    for v in &GROVE_VERSIONS[..3] {
        let dir = tempfile::tempdir().expect("a directory");
        let before = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let mut run = Run::new(&db, v, ALL);
            two_trees(&mut run);
            run.write("delete", None, || db.delete(ROOT, MADE, allowed(), None, v));
            run.reads(ROOT, TWO, None);
            let ran = run.done();
            db.flush().expect("a flush");
            ran
        };
        check(dir.path(), v);
        assert_eq!(answered(&before, "delete"), "Ok(())");
        let again = {
            let db = Lignin::open_with_engine(dir.path()).expect("the database opens again");
            let mut run = Run::new(&db, v, ALL);
            let tree = Element::empty_tree;
            run.reads(ROOT, TWO, None);
            run.write("made again", None, || {
                db.insert(ROOT, MADE, tree(), None, None, v)
            });
            run.reads(ROOT, TWO, None);
            let ran = run.done();
            db.flush().expect("a flush");
            ran
        };
        check(dir.path(), v);
        assert_eq!(answered(&again, "made again"), refusal(MADE, OVER));
        // The reads before the delete's close, after the open, and after
        // the refused write are the same, call for call: 15 answers each
        // time, after the root hash that follows the write.
        let reads = |ran: &'_ Ran, from: usize| ran.answers[from..from + 15].to_vec();
        let left = reads(&before, before.answers.len() - 15);
        assert_eq!(reads(&again, 0), left);
        assert_eq!(reads(&again, 17), left);
        assert_eq!(again.answers.len(), 32);
    }
}

/// A wipe takes the record with everything else: the grove takes writes
/// after it, and every answer is RocksDB's.
fn wiped<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Ran {
    let mut run = Run::new(db, v, ALL);
    let tree = Element::empty_tree;
    two_trees(&mut run);
    run.write("delete", None, || db.delete(ROOT, MADE, allowed(), None, v));
    run.reads(ROOT, TWO, None);
    run.h.plain("wipe", db.wipe());
    run.reads(ROOT, TWO, None);
    run.write("made again", None, || {
        db.insert(ROOT, MADE, tree(), None, None, v)
    });
    run.write("made/y", None, || {
        db.insert(&[MADE], b"y", item(4), None, None, v)
    });
    run.write("over", None, || {
        db.insert(ROOT, OVER, tree(), None, None, v)
    });
    run.reads(ROOT, TWO, None);
    run.done()
}

#[test]
fn a_wipe_takes_the_record_that_was_left_behind() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let told = format!("grove version {}", number + 1);
        let lignin = on_lignindb(v, |db| wiped(db, v));
        let rocksdb = on_rocksdb(|db| wiped(db, v));
        assert_eq!(lignin, rocksdb, "{told}");
        assert_eq!(refused(&lignin), Vec::<&str>::new(), "{told}");
        for call in ["made again", "made/y"] {
            assert_eq!(answered(&lignin, call), "Ok(())", "{told}: {call}");
        }
    }
}

/// The heights at which the partial batches pause. With none given a
/// partial batch pauses at height 1.
const PAUSES: [Option<u8>; 4] = [None, Some(0), Some(2), Some(3)];

/// Partial batches after the delete of [`rotated`], paused at each of
/// [`PAUSES`]: an item under `b` in the first segment, and a
/// tree at the root that the caller adds for the second. The Merks of
/// the first segment go on writing into its batch after the segment's
/// writes were taken out of it, and the root Merk is among them.
fn partial<'db, E: Engine>(
    db: &'db GroveDb<E>,
    v: &'db GroveVersion,
    not_made: NotMade<'db>,
) -> Ran {
    let mut run = Run::new(db, v, not_made);
    rotated(&mut run);
    for (n, height) in PAUSES.into_iter().enumerate() {
        let options = BatchApplyOptions {
            batch_pause_height: height,
            ..BatchApplyOptions::default()
        };
        let n = n as u8;
        run.write(&format!("paused at {height:?}"), None, || {
            let below = Op::insert_or_replace_op(vec![b"b".to_vec()], vec![b'y', n], item(n));
            let added = Op::insert_or_replace_op(Vec::new(), vec![b'z', n], Element::empty_tree());
            let add = |_: &OperationCost, _: &_| Ok(vec![added.clone()]);
            db.apply_partial_batch(vec![below], Some(options), add, None, v)
        });
        run.reads(ROOT, FOUR, None);
    }
    run.done()
}

#[test]
fn a_partial_batch_after_the_delete_is_refused_at_every_pause_height() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let told = format!("grove version {number}");
        let lignin = on_lignindb(v, |db| partial(db, v, ALL));
        let rocksdb = on_rocksdb(|db| partial(db, v, ALL));
        let without = |refused: &[usize]| on_rocksdb(|db| partial(db, v, refused));
        let calls = PAUSES.map(|height| format!("paused at {height:?}"));
        let expected: Vec<_> = match number {
            1..=3 => (calls.iter())
                .map(|call| (call.as_str(), refusal(b"b", b"c")))
                .collect(),
            _ => Vec::new(),
        };
        let class = class(&told, &lignin, &rocksdb, &expected, without);
        println!("PARTIAL {told}: {class}, refused {:?}", refused(&lignin));
        assert_eq!(answered(&lignin, "delete a"), "Ok(())", "{told}");
        match number {
            1..=3 => assert_eq!(class, "refused", "{told}"),
            _ => assert_eq!(class, "equal", "{told}"),
        }
    }
}

/// A savepoint before the delete, the refused write, and a rollback to
/// the savepoint, all in one transaction, which then takes the write and
/// is committed: nothing of the refusal is kept past the rollback.
fn back_to_a_savepoint<'db, E: Engine>(
    db: &'db GroveDb<E>,
    v: &'db GroveVersion,
    not_made: NotMade<'db>,
) -> Ran {
    let mut run = Run::new(db, v, not_made);
    let tree = Element::empty_tree;
    two_trees(&mut run);
    let transaction = db.start_transaction();
    let through = Some(&transaction);
    run.h.plain("savepoint", db.set_savepoint(&transaction));
    run.write("delete", through, || {
        db.delete(ROOT, MADE, allowed(), through, v)
    });
    run.write("made again", through, || {
        db.insert(ROOT, MADE, tree(), None, through, v)
    });
    run.reads(ROOT, TWO, through);
    run.h
        .plain("rollback", db.rollback_to_savepoint(&transaction));
    run.reads(ROOT, TWO, through);
    run.write("zed", through, || {
        db.insert(ROOT, b"zed", tree(), None, through, v)
    });
    run.write("made/y", through, || {
        db.insert(&[MADE], b"y", item(4), None, through, v)
    });
    run.reads(ROOT, TWO, through);
    run.other("commit", || db.commit_transaction(transaction));
    run.reads(ROOT, TWO, None);
    run.done()
}

#[test]
fn a_rollback_to_a_savepoint_before_the_delete_ends_the_refusal_in_its_transaction() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let told = format!("grove version {number}");
        let lignin = on_lignindb(v, |db| back_to_a_savepoint(db, v, ALL));
        let rocksdb = on_rocksdb(|db| back_to_a_savepoint(db, v, ALL));
        let without = |refused: &[usize]| on_rocksdb(|db| back_to_a_savepoint(db, v, refused));
        let expected = match number {
            1..=3 => vec![("made again", refusal(MADE, OVER))],
            _ => Vec::new(),
        };
        let class = class(&told, &lignin, &rocksdb, &expected, without);
        for call in ["delete", "zed", "made/y", "commit"] {
            assert_eq!(answered(&lignin, call), "Ok(())", "{told}: {call}");
        }
        match number {
            1..=3 => assert_eq!(class, "refused", "{told}"),
            _ => assert_eq!(class, "equal", "{told}"),
        }
    }
}
