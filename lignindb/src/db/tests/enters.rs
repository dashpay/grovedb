//! Two transactions that each make the first write into one subtree
//! (`SubtreeWrite::EnterIfAbsent`, `transactions.md` CF2, GM1): both are
//! begun and both have written before either commits, so the second is
//! laid out over the commit of the first when it commits.

use super::*;
use crate::{
    commit::CommitError,
    db::WriteError,
    txn::{BatchError, TxnError, Unit, WriteTxn},
};

/// A subtree that is entered before the two transactions begin, and one
/// that nothing has entered.
const PARENT: u8 = 1;
const CHILD: u8 = 2;

/// The key of the element of `PARENT` that hosts `CHILD`.
const HOST: &[u8] = &[b'k', 0, 0, 0, 3];

/// Applies `writes` to `txn`, as one operation.
fn apply(txn: &WriteTxn<'_>, writes: &Writes) {
    let scope = txn.scope().expect("a scope");
    (writes.with(|batch| scope.apply(batch))).expect("the batch is applied");
    scope.merge();
}

/// Begins a transaction and applies `writes` to it, as one operation.
fn wrote<'db>(db: &'db Db, writes: &Writes) -> WriteTxn<'db> {
    let txn = db.begin();
    apply(&txn, writes);
    txn
}

/// The first write into `CHILD` of a writer that changes no tree, as
/// [`first_record`], under the element `HOST` of `PARENT`.
fn first_hosted(key: &[u8]) -> Writes {
    Writes {
        enter_if_absent: vec![(subtree(CHILD), Some((subtree(PARENT), HOST.to_vec())))],
        ..first_record(Column::Aux, key)
    }
}

fn published(committed: Result<Committed, WriteError>) -> Box<Published> {
    match committed {
        Ok(Committed::Published(published)) => published,
        other => panic!("a commit is published, and not {other:?}"),
    }
}

/// The first write into `CHILD` of a writer that changes no tree: the
/// subtree is entered if it is absent, and one record is put under `key`
/// in `column`.
fn first_record(column: Column, key: &[u8]) -> Writes {
    Writes {
        enter_if_absent: vec![(subtree(CHILD), None)],
        records: vec![(column, subtree(CHILD), key.to_vec(), key.to_vec())],
        ..Writes::default()
    }
}

/// The physical checker finds the latest version of `db` sound.
fn sound(db: &Db, at: &str) {
    halt_background(db);
    let report = checked(db);
    assert!(
        report.findings.is_empty() && report.more_findings == 0,
        "{at}:\n{report}"
    );
}

/// D-181 item 2: two transactions that each make the first write into a
/// subtree nothing has entered, with records under different keys, both
/// commit, in every column. The subtree is entered once and holds both
/// records.
#[test]
fn two_first_writers_of_one_subtree_both_commit_where_their_records_differ() {
    for column in Column::ALL {
        let dir = temp();
        let db = Db::open(dir.path(), &options()).expect("a new database");
        let first = wrote(&db, &first_record(column, b"first"));
        let second = wrote(&db, &first_record(column, b"second"));
        let before = db.version();
        let one = published(db.commit(&first, &mut NoStaleHashes));
        let two = published(db.commit(&second, &mut NoStaleHashes));
        assert_eq!(Some(one.version), before.next(), "{column}");
        assert_eq!(Some(two.version), one.version.next(), "{column}");

        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        let opened = operation.open(subtree(CHILD)).expect("a lookup");
        let opened = opened.expect("the subtree is entered");
        for key in [&b"first"[..], b"second"] {
            let mut value = Vec::new();
            let found = opened.get(column, key, &mut value).expect("a read");
            assert!(found, "{column}");
            assert_eq!(value, key, "{column}");
        }
        drop(operation);
        drop(snapshot);
        sound(&db, "two first writers");
    }
}

/// The conflict rule is what it was (CF2): two first writers of one
/// subtree that put a record under one key meet in that key, whichever
/// enter they made, and the second commit is a conflict.
#[test]
fn two_first_writers_of_one_subtree_conflict_where_they_wrote_one_key() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let first = wrote(&db, &first_record(Column::Aux, b"same"));
    let second = wrote(&db, &first_record(Column::Aux, b"same"));
    published(db.commit(&first, &mut NoStaleHashes));
    let error = (db.commit(&second, &mut NoStaleHashes)).expect_err("a conflict");
    assert!(matches!(error, WriteError::Commit(CommitError::Conflict)));
    assert!(error.is_conflict(), "{error}");
    sound(&db, "a conflict of two first writers");
}

/// An enter of a subtree that is new is what it was: of two transactions
/// that each make one, the second cannot be laid out over the commit of
/// the first, and its commit is refused by the subtree directory. That is
/// no conflict.
#[test]
fn two_enters_of_one_new_subtree_refuse_the_second_commit() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let new = |key: &[u8]| Writes {
        enter: vec![(subtree(CHILD), None)],
        records: vec![(Column::Aux, subtree(CHILD), key.to_vec(), key.to_vec())],
        ..Writes::default()
    };
    let first = wrote(&db, &new(b"first"));
    let second = wrote(&db, &new(b"second"));
    published(db.commit(&first, &mut NoStaleHashes));
    let error = (db.commit(&second, &mut NoStaleHashes)).expect_err("a refusal");
    assert!(!error.is_conflict(), "{error}");
    let entered = BatchError::Entered {
        subtree: subtree(CHILD),
    };
    assert!(
        matches!(&error, WriteError::Commit(CommitError::Refused(TxnError::Batch(refused))) if *refused == entered),
        "{error}"
    );
    sound(&db, "a refused enter");
}

/// What a writer of an existing GroveVersion writes with the first node
/// of the Merk of `CHILD`: the nodes, and the element of `PARENT` that
/// hosts it, or with no host the base root key (GM1).
fn first_nodes(lead: u8, hosted: bool) -> Writes {
    let child = Merk::sized(lead, 5, 20);
    let host = (subtree(PARENT), HOST.to_vec());
    let parent = Merk::sized(b'k', 9, 24).hosting(HOST, subtree(CHILD));
    let mut writes = Writes {
        enter_if_absent: vec![(subtree(CHILD), hosted.then_some(host))],
        merks: vec![(subtree(CHILD), child)],
        ..Writes::default()
    };
    match hosted {
        true => writes.merks.push((subtree(PARENT), parent)),
        false => (writes.records).push((Column::Roots, subtree(CHILD), b"r".to_vec(), vec![lead])),
    }
    writes
}

/// Two first writers that both changed the tree conflict by a key, as
/// GM1 says, and not by their enters: the element that hosts the subtree,
/// which both wrote, or the base root key of a Merk that no element
/// hosts. The second commit is a conflict, and no commit meets
/// `BatchError::RootTaken`.
#[test]
fn two_first_writers_that_change_the_tree_conflict_by_their_keys() {
    for hosted in [true, false] {
        let dir = temp();
        let db = Db::open(dir.path(), &options()).expect("a new database");
        plant(&db, subtree(PARENT), Merk::sized(b'k', 9, 24));
        let first = wrote(&db, &first_nodes(b'a', hosted));
        let second = wrote(&db, &first_nodes(b'b', hosted));
        published(db.commit(&first, &mut NoStaleHashes));
        let error = (db.commit(&second, &mut NoStaleHashes)).expect_err("a conflict");
        assert!(
            matches!(error, WriteError::Commit(CommitError::Conflict)),
            "{hosted}: {error}"
        );
        assert!(error.is_conflict(), "{hosted}: {error}");
        // The Merk is the one the first made: it has five nodes, which
        // are the five of the first, and none of the second.
        {
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            let found = seen(&operation, subtree(CHILD), b"no record");
            assert_eq!(found.map(|(nodes, _)| nodes), Some(5), "{hosted}");
            let opened = operation.open(subtree(CHILD)).expect("a lookup");
            let opened = opened.expect("the subtree is entered");
            let has = |lead: u8, number: u32| {
                let mut key = vec![lead];
                key.extend_from_slice(&number.to_be_bytes());
                let node = opened.node(&key, None, |node| node.is_some());
                node.expect("a read")
            };
            for number in 0..5 {
                let found = (has(b'a', number), has(b'b', number));
                assert_eq!(found, (true, false), "{hosted}, node {number}");
            }
        }
        sound(&db, "a conflict of two tree changes");
    }
}

/// What the engine itself does with two first writers of a Merk's first
/// node that wrote no key in common, which no writer of an existing
/// GroveVersion is: the second commit is refused, because the Merk it
/// found empty has a root that is not its own
/// (`BatchError::RootTaken`), and that is no conflict in iteration one.
#[test]
fn two_first_nodes_of_one_merk_with_no_key_in_common_refuse_the_second_commit() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let nodes = |lead: u8| Writes {
        enter_if_absent: vec![(subtree(CHILD), None)],
        merks: vec![(subtree(CHILD), Merk::sized(lead, 5, 20))],
        ..Writes::default()
    };
    let first = wrote(&db, &nodes(b'a'));
    let second = wrote(&db, &nodes(b'b'));
    published(db.commit(&first, &mut NoStaleHashes));
    let error = (db.commit(&second, &mut NoStaleHashes)).expect_err("a refusal");
    assert!(!error.is_conflict(), "{error}");
    let taken = BatchError::RootTaken {
        subtree: subtree(CHILD),
    };
    assert!(
        matches!(&error, WriteError::Commit(CommitError::Refused(TxnError::Batch(refused))) if *refused == taken),
        "{error}"
    );
    sound(&db, "a refused first node");
}

/// What a commit beside the transaction does above a subtree that the
/// transaction entered if it was absent (CF5). The subtree is below the
/// element its enter states from that enter on, whoever enters it. A
/// commit that drops the subtree which that element is in takes every
/// record under the subtree, also where another commit entered the
/// subtree in between and the commit that drops enters both again under
/// the names they had. The writes that the transaction made before that
/// commit are met, whether or not it looked between the two commits.
#[test]
fn a_drop_above_a_subtree_entered_if_absent_meets_the_writes_under_it() {
    let parent = || Merk::sized(b'k', 9, 24);
    // The parent is dropped and entered again with the child, each under
    // the name and the element it had, and the child holds nothing.
    let again = Writes {
        drop: vec![subtree(PARENT)],
        enter: vec![
            (subtree(PARENT), None),
            (subtree(CHILD), Some((subtree(PARENT), HOST.to_vec()))),
        ],
        merks: vec![(subtree(PARENT), parent().hosting(HOST, subtree(CHILD)))],
        ..Writes::default()
    };
    let unit = Unit::new(Column::Aux, subtree(CHILD));
    for looks in [false, true] {
        let dir = temp();
        let db = Db::open(dir.path(), &options()).expect("a new database");
        plant(&db, subtree(PARENT), parent());
        let loser = wrote(&db, &first_hosted(b"x"));
        let other = wrote(&db, &first_hosted(b"y"));
        published(db.commit(&other, &mut NoStaleHashes));
        if looks {
            let read = loser.get(unit, b"y", &mut Vec::new());
            assert!(read.expect("a read"), "the commit beside it is read");
        }
        again.commit(&db);
        let error = (db.commit(&loser, &mut NoStaleHashes)).expect_err("a conflict");
        assert!(
            matches!(error, WriteError::Commit(CommitError::Conflict)),
            "{looks}: {error}"
        );
        // The child is the one the last commit entered, with no record.
        {
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            for key in [&b"x"[..], b"y"] {
                let found = seen(&operation, subtree(CHILD), key);
                assert_eq!(found, Some((0, Vec::new())), "{looks}");
            }
        }
        sound(&db, "a drop above two first writers");
    }
}

/// A transaction whose enter if absent states another host than a commit
/// beside it gave the subtree is refused at its commit. Until then it
/// serves its records by key, over the subtree the committed version has,
/// and what it reads agrees with itself: a drop of the subtree that the
/// committed host is in takes the subtree with the records under it, the
/// transaction's own as well.
#[test]
fn a_transaction_whose_enter_is_refused_reads_one_subtree_by_key() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(PARENT), Merk::sized(b'k', 9, 24));
    let unit = Unit::new(Column::Aux, subtree(CHILD));
    // The transaction enters the subtree with no host, and then a commit
    // enters it under an element.
    let txn = wrote(&db, &first_record(Column::Aux, b"x"));
    first_hosted(b"y").commit(&db);
    let host = |txn: &WriteTxn<'_>| {
        let host = |view: Option<crate::txn::SubtreeView<'_>>| {
            view.map(|view| view.host.map(|host| host.subtree))
        };
        txn.subtree(subtree(CHILD), host).expect("a read")
    };
    let has = |txn: &WriteTxn<'_>, key: &[u8]| {
        let read = txn.get(unit, key, &mut Vec::new());
        read.expect("a read")
    };
    assert_eq!(host(&txn), Some(Some(subtree(PARENT))));
    assert_eq!((has(&txn, b"x"), has(&txn, b"y")), (true, true));
    let dropped = Writes {
        drop: vec![subtree(PARENT)],
        ..Writes::default()
    };
    apply(&txn, &dropped);
    assert_eq!(host(&txn), None);
    assert_eq!((has(&txn, b"x"), has(&txn, b"y")), (false, false));

    let error = (db.commit(&txn, &mut NoStaleHashes)).expect_err("a refusal");
    assert!(!error.is_conflict(), "{error}");
    assert!(
        matches!(
            &error,
            WriteError::Commit(CommitError::Refused(TxnError::Batch(
                BatchError::EnteredOtherwise { subtree: refused, .. }
            ))) if *refused == subtree(CHILD)
        ),
        "{error}"
    );
    txn.rollback().expect("a rollback");
    sound(&db, "a refused host");
}

/// The scopes of such a transaction are held apart by where the committed
/// version has the subtree (SC2): while one has a write under it, another
/// cannot drop the subtree that its committed host is in.
#[test]
fn a_scope_cannot_drop_above_a_subtree_that_another_wrote_under() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(PARENT), Merk::sized(b'k', 9, 24));
    let txn = db.begin();
    let writer = txn.scope().expect("a scope");
    let first = first_record(Column::Aux, b"x");
    (first.with(|batch| writer.apply(batch))).expect("the batch is applied");
    first_hosted(b"y").commit(&db);
    let dropper = txn.scope().expect("a scope beside the first");
    let dropped = Writes {
        drop: vec![subtree(PARENT)],
        ..Writes::default()
    };
    let refused = dropped.with(|batch| dropper.apply(batch));
    let refused = refused.expect_err("the drop is above a write of the other scope");
    assert!(
        matches!(refused, TxnError::ScopeOverlap { unit } if unit.subtree == subtree(CHILD)),
        "{refused}"
    );
    dropper.rollback();
    writer.rollback();
    sound(&db, "two scopes");
}

/// Two first writers of a subtree under an element, with records under
/// different keys, both commit, also where a commit between theirs drops
/// a subtree that the element is not in or below: nothing above the
/// subtree left.
#[test]
fn two_first_writers_under_an_element_both_commit_beside_a_drop_elsewhere() {
    const ELSEWHERE: u8 = 3;
    for drops in [false, true] {
        let dir = temp();
        let db = Db::open(dir.path(), &options()).expect("a new database");
        plant(&db, subtree(PARENT), Merk::sized(b'k', 9, 24));
        plant(&db, subtree(ELSEWHERE), Merk::sized(b'e', 3, 24));
        let first = wrote(&db, &first_hosted(b"x"));
        let second = wrote(&db, &first_hosted(b"y"));
        published(db.commit(&second, &mut NoStaleHashes));
        if drops {
            let elsewhere = Writes {
                drop: vec![subtree(ELSEWHERE)],
                ..Writes::default()
            };
            elsewhere.commit(&db);
        }
        published(db.commit(&first, &mut NoStaleHashes));
        {
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            for key in [&b"x"[..], b"y"] {
                let found = seen(&operation, subtree(CHILD), key);
                let record = vec![(Column::Aux, key.to_vec())];
                assert_eq!(found, Some((0, record)), "{drops}");
            }
        }
        sound(&db, "two first writers under an element");
    }
}

/// What an enter if absent recorded of where its subtree hangs is of that
/// enter. A transaction that dropped the subtree and entered it again
/// under an element of another subtree, and rolled both back, has its
/// first enter and the writes under it: a commit that drops the other
/// subtree meets none of them, whether the transaction saw that commit
/// before the rollback or not.
#[test]
fn a_drop_above_an_enter_that_was_rolled_back_meets_nothing() {
    const ELSEWHERE: u8 = 3;
    let unit = Unit::new(Column::Aux, subtree(CHILD));
    for seen_first in [false, true] {
        let dir = temp();
        let db = Db::open(dir.path(), &options()).expect("a new database");
        plant(&db, subtree(PARENT), Merk::sized(b'k', 9, 24));
        plant(&db, subtree(ELSEWHERE), Merk::sized(b'k', 9, 24));
        let txn = wrote(&db, &first_hosted(b"x"));
        txn.set_savepoint().expect("a savepoint");
        let dropped = Writes {
            drop: vec![subtree(CHILD)],
            ..Writes::default()
        };
        apply(&txn, &dropped);
        let host = (subtree(ELSEWHERE), HOST.to_vec());
        let moved = Writes {
            enter_if_absent: vec![(subtree(CHILD), Some(host))],
            ..first_record(Column::Aux, b"z")
        };
        apply(&txn, &moved);
        let elsewhere = Writes {
            drop: vec![subtree(ELSEWHERE)],
            ..Writes::default()
        };
        if seen_first {
            elsewhere.commit(&db);
            let read = txn.get(unit, b"z", &mut Vec::new());
            assert!(read.expect("a read"), "its own record");
        }
        txn.rollback_to_savepoint().expect("a rollback");
        if !seen_first {
            elsewhere.commit(&db);
        }
        published(db.commit(&txn, &mut NoStaleHashes));
        {
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            let found = seen(&operation, subtree(CHILD), b"x");
            let record = vec![(Column::Aux, b"x".to_vec())];
            assert_eq!(found, Some((0, record)), "{seen_first}");
        }
        sound(&db, "an enter that was rolled back");
    }
}

/// A delete of every flat record of a subtree's data column is no clear
/// of a Merk, and takes no subtree with it: a commit that makes one in
/// the subtree which hosts a first writer's subtree meets no write of
/// that writer.
#[test]
fn a_delete_of_every_flat_record_above_a_first_writer_is_no_conflict() {
    const FLAT: u8 = 4;
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let record = (Column::Data, subtree(FLAT), b"h".to_vec(), b"flat".to_vec());
    let flat = Writes {
        enter: vec![(subtree(FLAT), None)],
        records: vec![record.clone()],
        ..Writes::default()
    };
    flat.commit(&db);
    let hosted = Writes {
        enter_if_absent: vec![(subtree(CHILD), Some((subtree(FLAT), b"h".to_vec())))],
        ..first_record(Column::Aux, b"x")
    };
    let txn = wrote(&db, &hosted);
    let range = [RecordWrite::DeleteRange {
        unit: Unit::new(Column::Data, subtree(FLAT)),
        start: b"",
        end: None,
    }];
    let deleted = Batch {
        records: &range,
        ..Batch::default()
    };
    published(db.write(&deleted, &mut NoStaleHashes));
    let again = Writes {
        records: vec![record],
        ..Writes::default()
    };
    again.commit(&db);
    published(db.commit(&txn, &mut NoStaleHashes));
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let found = seen(&operation, subtree(CHILD), b"x");
    assert_eq!(found, Some((0, vec![(Column::Aux, b"x".to_vec())])));
}

/// A scope's claim on a record is on the subtree as it hangs when the
/// claim is taken (SC3). A batch of another scope that drops the subtree
/// above it and enters the subtree again, if it is absent, under an
/// element of another subtree would take that record: it is refused.
#[test]
fn a_scope_cannot_take_a_subtree_that_another_read_and_enter_it_elsewhere() {
    const ELSEWHERE: u8 = 3;
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(PARENT), Merk::sized(b'k', 9, 24));
    plant(&db, subtree(ELSEWHERE), Merk::sized(b'k', 9, 24));
    first_hosted(b"x").commit(&db);
    let unit = Unit::new(Column::Aux, subtree(CHILD));
    let txn = db.begin();
    let reader = txn.scope().expect("a scope");
    let read = reader.get(unit, b"x", &mut Vec::new());
    assert!(read.expect("a read"));
    let other = txn.scope().expect("a scope beside the first");
    let host = (subtree(ELSEWHERE), HOST.to_vec());
    let replaced = Writes {
        drop: vec![subtree(PARENT)],
        enter_if_absent: vec![(subtree(CHILD), Some(host))],
        ..Writes::default()
    };
    let refused = replaced.with(|batch| other.apply(batch));
    let refused = refused.expect_err("the drop takes a record the other scope read");
    assert!(
        matches!(refused, TxnError::ScopeOverlap { unit: held } if held == unit),
        "{refused}"
    );
    other.rollback();
    reader.rollback();
    sound(&db, "two scopes and a replacement");
}

/// A scope that dropped a subtree holds every subtree that went with it,
/// as each hung when it went. One that the transaction had entered if it
/// was absent with no host, and that a commit beside it entered under an
/// element of the dropped subtree, went with the drop: another scope
/// reads no record of it.
#[test]
fn a_scope_cannot_read_under_a_subtree_that_another_took_with_one_above() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(PARENT), Merk::sized(b'k', 9, 24));
    let txn = wrote(&db, &first_record(Column::Aux, b"x"));
    first_hosted(b"y").commit(&db);
    let dropper = txn.scope().expect("a scope");
    let dropped = Writes {
        drop: vec![subtree(PARENT)],
        ..Writes::default()
    };
    (dropped.with(|batch| dropper.apply(batch))).expect("the drop is applied");
    let reader = txn.scope().expect("a scope beside the first");
    let meta = Unit::new(Column::Meta, subtree(CHILD));
    let refused = reader.get(meta, b"m", &mut Vec::new());
    let refused = refused.expect_err("the subtree went with the drop of the other scope");
    let held = Unit::new(Column::Data, subtree(PARENT));
    assert!(
        matches!(refused, TxnError::ScopeOverlap { unit } if unit == held),
        "{refused}"
    );
    reader.rollback();
    dropper.rollback();
    sound(&db, "a scope beside a drop above");
}

/// Where a subtree hung before the transaction itself moved it holds no
/// scope: an operation of the transaction drops the subtree above, enters
/// that one again and enters the subtree under an element of another.
/// A scope that reads a record of the subtree afterwards has a claim on
/// it where it hangs now, and another scope clears the subtree it hung
/// in before.
#[test]
fn a_scope_is_not_held_by_where_a_subtree_hung_before_the_scopes() {
    const ELSEWHERE: u8 = 3;
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let parent = || Merk::sized(b'k', 9, 24);
    plant(&db, subtree(PARENT), parent());
    plant(&db, subtree(ELSEWHERE), parent());
    first_hosted(b"x").commit(&db);
    let host = (subtree(ELSEWHERE), HOST.to_vec());
    let moved = Writes {
        drop: vec![subtree(PARENT)],
        enter: vec![(subtree(PARENT), None), (subtree(CHILD), Some(host))],
        merks: vec![
            (subtree(PARENT), parent()),
            (subtree(ELSEWHERE), parent().hosting(HOST, subtree(CHILD))),
        ],
        ..Writes::default()
    };
    let txn = wrote(&db, &moved);
    let reader = txn.scope().expect("a scope");
    let unit = Unit::new(Column::Aux, subtree(CHILD));
    let read = reader.get(unit, b"x", &mut Vec::new());
    assert!(!read.expect("a read"), "the subtree is a new one");
    let other = txn.scope().expect("a scope beside the first");
    let clear = [SubtreeWrite::Clear {
        subtree: subtree(PARENT),
    }];
    let cleared = other.apply(&Batch {
        subtrees: &clear,
        ..Batch::default()
    });
    cleared.expect("the subtree hangs elsewhere");
    other.merge();
    reader.merge();
    published(db.commit(&txn, &mut NoStaleHashes));
    sound(&db, "a subtree that was moved before the scopes");
}
