//! The move of a subtree through a database (`SubtreeWrite::Move`,
//! `transactions.md` 7.5): what a commit that moves subtrees leaves for
//! the physical checker and for the next session, and what the copy of
//! the flat records costs. What a power cut during such a commit leaves
//! is in `power`, with the grove of this file.
//!
//! `what_the_copy_of_the_flat_records_of_a_move_costs` is the measurement.
//! It is left out of a normal run and wants a release build:
//!
//! ```text
//! cargo test --release -p lignindb --lib -- --ignored --nocapture what_the_copy_of_the_flat
//! ```

use std::time::Instant;

use super::*;
use crate::txn::{TxnError, WriteTxn};

/// The Merk that hosts the others, with no host of its own.
const TOP: u8 = 30;
/// The subtrees under the nodes `a`, `b` and `c` of `TOP`, each with a
/// subtree under its node `in`; `A` has one more, with no node, under its
/// node `e`.
const A: u8 = 31;
const A_IN: u8 = 32;
const A_EMPTY: u8 = 33;
const B: u8 = 34;
const B_IN: u8 = 35;
const C: u8 = 36;
const C_IN: u8 = 37;
/// The prefixes under the node `d` of `TOP`.
const D: u8 = 38;
const D_IN: u8 = 39;

/// Every prefix of the grove, before and after its moves.
pub(super) const NAMES: [u8; 10] = [TOP, A, A_IN, A_EMPTY, B, B_IN, C, C_IN, D, D_IN];

/// The key that every subtree of the grove has a flat record under, in
/// the columns it has one in.
const FLAT: &[u8] = b"k";

/// What a subtree holds: the keys and the values of the nodes of its
/// Merk, and its flat record under `FLAT` in each column that has one.
pub(super) type Held = (Items, Vec<(Column, Vec<u8>)>);

/// What each subtree that is entered holds, by its prefix.
pub(super) type Grove = BTreeMap<u8, Held>;

/// The Merk of `TOP` whose nodes under `hosts` host those subtrees, each
/// with the value of an element, beside an item.
fn top(hosts: &[(&[u8], u8, &[u8])]) -> Merk {
    let mut items: BTreeMap<Vec<u8>, Vec<u8>> = (hosts.iter())
        .map(|(key, _, value)| (key.to_vec(), value.to_vec()))
        .collect();
    items.insert(b"x".to_vec(), vec![7; 200]);
    let merk = Merk::of(&items);
    (hosts.iter()).fold(merk, |merk, (key, hosted, _)| {
        merk.hosting(key, subtree(*hosted))
    })
}

/// The key that the subtrees under `a` and `c` have an unlinked record
/// under, which the one under `b` has not.
const LOOSE: &[u8] = b"loose";

/// A Merk with nodes of its own, whose node `in` hosts `inner` and whose
/// node `e` hosts `empty`, if it has one. But for the one that `b` leads,
/// it has one more node stated, which its tree does not reach: an
/// unlinked record.
fn outer(lead: u8, inner: u8, empty: Option<u8>) -> Merk {
    let mut items = BTreeMap::new();
    items.insert(b"in".to_vec(), vec![lead; 4]);
    items.insert(b"k1".to_vec(), vec![lead; 40]);
    items.insert(b"k2".to_vec(), vec![lead ^ 0x55; 300]);
    if empty.is_some() {
        items.insert(b"e".to_vec(), vec![lead; 2]);
    }
    let merk = Merk::of(&items).hosting(b"in", subtree(inner));
    let merk = match lead {
        b'b' => merk,
        _ => merk.with_unlinked(LOOSE, &[lead ^ 0x33; 60]),
    };
    match empty {
        Some(empty) => merk.hosting(b"e", subtree(empty)),
        None => merk,
    }
}

/// The grove, and the writes that make it in one commit: under each of
/// the three hosted subtrees flat records in two columns, and under the
/// subtree below it in two as well, one of them long; and one flat record
/// under the subtree that has no node.
pub(super) fn grove() -> (Writes, Grove) {
    let hosted = [
        (&b"a"[..], A, A_IN, Some(A_EMPTY)),
        (b"b", B, B_IN, None),
        (b"c", C, C_IN, None),
    ];
    let mut writes = Writes {
        enter: vec![(subtree(TOP), None)],
        ..Writes::default()
    };
    let mut hosts = Vec::new();
    for (key, name, inner, empty) in hosted {
        let lead = key[0];
        hosts.push((key, name, key));
        writes.enter.extend([
            (subtree(name), Some((subtree(TOP), key.to_vec()))),
            (subtree(inner), Some((subtree(name), b"in".to_vec()))),
        ]);
        let below = Merk::sized(lead, 6, 25);
        (writes.merks).extend([
            (subtree(inner), below),
            (subtree(name), outer(lead, inner, empty)),
        ]);
        writes.records.extend([
            (Column::Aux, subtree(name), FLAT.to_vec(), vec![lead; 3]),
            (Column::Roots, subtree(name), FLAT.to_vec(), vec![lead; 9]),
            (
                Column::Aux,
                subtree(inner),
                FLAT.to_vec(),
                vec![lead ^ 1; 5],
            ),
            (Column::Meta, subtree(inner), FLAT.to_vec(), vec![lead; 700]),
        ]);
        if let Some(empty) = empty {
            (writes.enter).push((subtree(empty), Some((subtree(name), b"e".to_vec()))));
            (writes.records).push((Column::Meta, subtree(empty), FLAT.to_vec(), vec![lead; 30]));
        }
    }
    (writes.merks).push((subtree(TOP), top(&hosts)));
    let held = held_by(&writes);
    (writes, held)
}

/// What the subtrees hold that `writes` enters, from the Merks and the
/// records it writes.
fn held_by(writes: &Writes) -> Grove {
    let mut grove = Grove::new();
    for (entered, _) in &writes.enter {
        let name = NAMES.into_iter().find(|name| subtree(*name) == *entered);
        let merk = writes.merks.iter().find(|(of, _)| of == entered);
        let items = merk.map_or_else(Items::new, |(_, merk)| merk.items());
        let records = Column::ALL.into_iter().filter_map(|column| {
            let mut records = writes.records.iter();
            let record = records.find(|(in_column, of, ..)| *in_column == column && of == entered);
            record.map(|(.., value)| (column, value.clone()))
        });
        grove.insert(
            name.expect("a subtree of the grove"),
            (items, records.collect()),
        );
    }
    grove
}

/// The writes that move the subtrees of the grove `before`, and what the
/// grove holds afterwards: the subtrees under `a` and `b` change places,
/// and the one under `c` goes to `d`, each with the subtree below it. The
/// subtree with no node leaves with its move, and its flat record with
/// it. `read` gives the read of each node of `TOP`.
pub(super) fn moves(before: &Grove, read: impl Fn(&[u8]) -> Option<NodeHandle>) -> (Writes, Grove) {
    let hosts: [(&[u8], u8, &[u8]); 3] = [(b"a", A, b"b"), (b"b", B, b"a"), (b"d", D, b"c")];
    let moved = |from: u8, to: u8, key: &[u8], was: &[u8], below: (u8, u8)| Moved {
        from: subtree(from),
        to: subtree(to),
        host: (subtree(TOP), key.to_vec()),
        read: read(was),
        below: vec![(subtree(below.0), subtree(below.1))],
    };
    let writes = Writes {
        moves: vec![
            moved(A, B, b"b", b"a", (A_IN, B_IN)),
            moved(B, A, b"a", b"b", (B_IN, A_IN)),
            moved(C, D, b"d", b"c", (C_IN, D_IN)),
        ],
        merks: vec![(subtree(TOP), top(&hosts).without(b"c"))],
        ..Writes::default()
    };
    let mut after = Grove::new();
    after.insert(TOP, (writes.merks[0].1.items(), before[&TOP].1.clone()));
    for (from, to) in [
        (A, B),
        (A_IN, B_IN),
        (B, A),
        (B_IN, A_IN),
        (C, D),
        (C_IN, D_IN),
    ] {
        after.insert(to, before[&from].clone());
    }
    (writes, after)
}

/// Commits the moves of the grove `before` in one write transaction,
/// which reads the nodes that host the subtrees and names those reads.
/// An error is that of the step that failed, with the transaction rolled
/// back.
pub(super) fn commit_moves(db: &Db, before: &Grove) -> Result<(Committed, Grove), String> {
    fn applied(txn: &WriteTxn<'_>, before: &Grove) -> Result<Grove, TxnError> {
        let scope = txn.scope()?;
        let handle = |view: Option<crate::txn::NodeView<'_>>| view.and_then(|view| view.handle());
        let mut reads = BTreeMap::new();
        let mut failed = None;
        for key in [&b"a"[..], b"b", b"c"] {
            match scope.node(subtree(TOP), key, handle) {
                Ok(read) => drop(reads.insert(key.to_vec(), read)),
                Err(error) => failed = Some(error),
            }
        }
        if let Some(error) = failed {
            scope.rollback();
            return Err(error);
        }
        let (writes, after) = moves(before, |key| reads[key]);
        let applied = writes.with(|batch| scope.apply(batch));
        scope.merge();
        applied.map(|_| after)
    }
    let txn = db.begin();
    let committed = match applied(&txn, before) {
        Ok(after) => match db.commit(&txn, &mut NoStaleHashes) {
            Ok(committed) => Ok((committed, after)),
            Err(error) => Err(error.to_string()),
        },
        Err(error) => Err(error.to_string()),
    };
    if committed.is_err() {
        let _ = db.rollback(&txn);
    }
    committed
}

/// What a reader of the latest version of `db` finds under every prefix
/// of `names` that has a subtree.
pub(super) fn found(db: &Db, names: &[u8]) -> Grove {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let subtrees = names.iter().filter_map(|name| {
        let (_, records) = seen(&operation, subtree(*name), FLAT)?;
        Some((*name, (items_of(&operation, subtree(*name))?, records)))
    });
    subtrees.collect()
}

/// D-212 item 3, through a database: after a commit that moves subtrees
/// a reader finds every node and every flat record under the new
/// prefixes and nothing under those no subtree went to, the physical
/// checker finds the store sound, and the next session finds the same.
#[test]
fn a_commit_that_moves_subtrees_is_sound_and_is_read_by_the_next_session() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let (writes, before) = grove();
    writes.commit(&db);
    assert_eq!(found(&db, &NAMES), before);
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");

    let (committed, after) = commit_moves(&db, &before).expect("the moves are committed");
    assert!(
        matches!(committed, Committed::Published(_)),
        "{committed:?}"
    );
    assert_eq!(found(&db, &NAMES), after);
    for gone in [C, C_IN, A_EMPTY] {
        assert!(!after.contains_key(&gone));
    }
    assert_eq!((&after[&A], &after[&B_IN]), (&before[&B], &before[&A_IN]));
    let report = db.verify(None);
    assert!(report.is_sound(), "{report}");
    let report = db.close_and_verify(None).expect("the close");
    assert!(report.is_sound(), "{report}");

    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    assert_eq!(found(&db, &NAMES), after);
    // The next session writes under a subtree that was moved.
    let more = Writes {
        records: vec![(Column::Aux, subtree(D_IN), b"more".to_vec(), b"v".to_vec())],
        ..Writes::default()
    };
    more.commit(&db);
    assert_eq!(found(&db, &NAMES), after);
    let report = db.close_and_verify(None).expect("the close");
    assert!(report.is_sound(), "{report}");
}

/// D-264 item 1, through a database: the unlinked records of a Merk go
/// with it to its new prefix. Of the two subtrees that change places one
/// holds an unlinked record and the other none, and a third that holds
/// one goes to a prefix that had no subtree. A reader of the version
/// before the moves and one of the version after them each find a record
/// under the prefixes whose Merks hold one at their version and none
/// under the others, by key and in a walk of the nodes, and each asks
/// once of a subtree whether it holds any. The next session reads the
/// same.
#[test]
fn a_reader_finds_the_unlinked_records_of_a_moved_merk_under_its_new_prefix() {
    use crate::txn::unlinked::probe;

    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let (writes, before) = grove();
    writes.commit(&db);
    let earlier = db.snapshot();
    let (_, after) = commit_moves(&db, &before).expect("the moves are committed");
    // What a reader is shown under `LOOSE` of the subtree `name`: `None`
    // where no subtree is entered, and otherwise whether a record is
    // there, and its value. It reads a key with no node first, a node of
    // the tree, and then walks the Merk.
    let shown = |snapshot: &crate::snapshot::Snapshot<'_>, name: u8| {
        let operation = snapshot.operation();
        let opened = operation.open(subtree(name)).expect("a lookup")?;
        let lookups = probe::lookups();
        let read = |key: &[u8]| {
            let read = opened.node(key, None, |view| {
                view.map(|view| {
                    let mut value = lignin_tree::value::ValueBuf::new();
                    let value = view.value(&mut value).expect("a value").to_vec();
                    (view.is_unlinked(), value)
                })
            });
            read.expect("a read")
        };
        assert_eq!(read(b"absent"), None);
        assert!(matches!(read(b"k1"), Some((false, _))));
        let record = read(LOOSE).map(|(unlinked, value)| {
            assert!(unlinked);
            value
        });
        let mut walk = opened.walk().expect("a lookup");
        walk.seek(&operation, LOOSE).expect("a move");
        assert_eq!(walk.key() == Some(LOOSE), record.is_some());
        assert_eq!(probe::lookups(), lookups + 1, "one lookup for {name}");
        Some(record)
    };
    let record = |lead: u8| Some(Some(vec![lead ^ 0x33; 60]));
    let at =
        |snapshot: &crate::snapshot::Snapshot<'_>| [A, B, C, D].map(|name| shown(snapshot, name));
    let was = [record(b'a'), Some(None), record(b'c'), None];
    let is = [Some(None), record(b'a'), None, record(b'c')];
    assert_eq!(at(&earlier), was);
    let later = db.snapshot();
    assert_eq!(at(&later), is);
    assert_eq!(
        at(&earlier),
        was,
        "the version before the moves is as it was"
    );
    assert_eq!(found(&db, &NAMES), after);
    let has = |held: &Held| held.0.iter().any(|(key, _)| key == LOOSE);
    assert!(has(&after[&B]) && has(&after[&D]) && !has(&after[&A]));
    drop((earlier, later));
    let report = db.close_and_verify(None).expect("the close");
    assert!(report.is_sound(), "{report}");

    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    assert_eq!(at(&db.snapshot()), is);
    assert_eq!(found(&db, &NAMES), after);
    let report = db.close_and_verify(None).expect("the close");
    assert!(report.is_sound(), "{report}");
}

/// How many flat records the measured subtree has.
const MEASURED: u32 = 100_000;

/// The cost of the copy that a move makes of the flat records of the
/// Merks it re-keys (`transactions.md` MV6): for a subtree with 100,000
/// flat records of a 32-byte key and a 100-byte value, the time of the
/// write that states the move, of an operation that makes the frames
/// again after a commit beside the transaction, and of the commit, with
/// the pages each of them writes. It prints, and asserts what does not
/// depend on the machine.
#[test]
#[ignore = "a measurement: it prints, and wants a release build"]
fn what_the_copy_of_the_flat_records_of_a_move_costs() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let hosts: [(&[u8], u8, &[u8]); 1] = [(b"a", A, b"a")];
    let made = Writes {
        enter: vec![
            (subtree(TOP), None),
            (subtree(A), Some((subtree(TOP), b"a".to_vec()))),
        ],
        merks: vec![(subtree(TOP), top(&hosts))],
        ..Writes::default()
    };
    made.commit(&db);
    let record = |number: u32| {
        let mut key = vec![0x33; 28];
        key.extend_from_slice(&number.to_be_bytes());
        (Column::Aux, subtree(A), key, vec![number as u8; 100])
    };
    for from in (0..MEASURED).step_by(10_000) {
        let records = (from..from + 10_000).map(record).collect();
        let loaded = Writes {
            records,
            ..Writes::default()
        };
        loaded.commit(&db);
    }

    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let handle = |view: Option<crate::txn::NodeView<'_>>| view.and_then(|view| view.handle());
    let read = scope.node(subtree(TOP), b"a", handle).expect("a read");
    let hosts: [(&[u8], u8, &[u8]); 1] = [(b"b", B, b"a")];
    let moving = Writes {
        moves: vec![Moved {
            from: subtree(A),
            to: subtree(B),
            host: (subtree(TOP), b"b".to_vec()),
            read,
            below: Vec::new(),
        }],
        merks: vec![(subtree(TOP), top(&hosts).without(b"a"))],
        ..Writes::default()
    };
    let began = Instant::now();
    (moving.with(|batch| scope.apply(batch))).expect("the subtree is moved");
    let stated = began.elapsed();
    scope.merge();
    let at_the_move = txn.stats();

    // A commit beside the transaction, of a key that no write of it
    // names: the next operation makes the frames again, and the move.
    let beside = Writes {
        records: vec![(Column::Aux, subtree(TOP), b"beside".to_vec(), b"v".to_vec())],
        ..Writes::default()
    };
    beside.commit(&db);
    let mut value = Vec::new();
    let began = Instant::now();
    let last = record(MEASURED - 1).2;
    assert!(txn
        .get(
            crate::txn::Unit::new(Column::Aux, subtree(B)),
            &last,
            &mut value
        )
        .expect("a read"));
    let again = began.elapsed();
    let rebuilt = txn.stats();
    assert_eq!(rebuilt.rebuilds, at_the_move.rebuilds + 1);
    assert_eq!(rebuilt.overlapped, 0);

    let began = Instant::now();
    let published = match db.commit(&txn, &mut NoStaleHashes) {
        Ok(Committed::Published(published)) => published,
        other => panic!("the move is committed, and not {other:?}"),
    };
    let commit = began.elapsed();
    drop(txn);
    println!(
        "a move of a subtree with {MEASURED} flat records (32-byte keys, 100-byte values):\n\
         the write that states it: {:.1} ms, {} flat pages and {} tree pages copied\n\
         the frames made again: {:.1} ms, {} flat pages and {} tree pages copied\n\
         the commit: {:.1} ms, {} pages made, {} pages changed, {} pages freed, a group of {} bytes",
        stated.as_secs_f64() * 1e3,
        at_the_move.flat_pages,
        at_the_move.tree_pages,
        again.as_secs_f64() * 1e3,
        rebuilt.flat_pages,
        rebuilt.tree_pages,
        commit.as_secs_f64() * 1e3,
        published.created.len(),
        published.changed,
        published.freed.len(),
        published.group_len,
    );
    assert!(at_the_move.flat_pages > 0 && rebuilt.flat_pages > 0);
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert!(operation.open(subtree(A)).expect("a lookup").is_none());
    let opened = operation.open(subtree(B)).expect("a lookup");
    let opened = opened.expect("the moved subtree");
    assert!(opened.get(Column::Aux, &last, &mut value).expect("a read"));
    drop(operation);
    drop(snapshot);
    let report = db.close_and_verify(None).expect("the close");
    assert!(report.is_sound(), "{report}");
}
