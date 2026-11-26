//! What a reader reads of a Merk and of a column through the facade: a
//! node by its key, and the nodes and the flat records in key order with
//! a cursor that is kept from one operation to the next.

use lignin_tree::{cursor::CursorError, node::NodeHandle};

use super::*;
use crate::{
    snapshot::ReadError,
    txn::{ChildKeys, FlatKinds, NodeView},
};

fn key(lead: u8, number: u32) -> Vec<u8> {
    let mut key = vec![lead];
    key.extend_from_slice(&number.to_be_bytes());
    key
}

/// A node as a reader is shown it: its key, its value, and the key of
/// each child.
type Shown = (Vec<u8>, Vec<u8>, [Vec<u8>; 2]);

/// The node under `at` as `opened` shows it.
fn shown(opened: &crate::snapshot::Opened<'_, '_>, at: &[u8]) -> Option<Shown> {
    let mut children = ChildKeys::new();
    let node = opened
        .node(at, Some(&mut children), |view| {
            view.map(|view| {
                let mut value = lignin_tree::value::ValueBuf::new();
                let value = view.value(&mut value).expect("a value").to_vec();
                (view.key().to_vec(), value)
            })
        })
        .expect("a read");
    node.map(|(key, value)| {
        let children = [Side::Left, Side::Right].map(|side| children.key(side).to_vec());
        (key, value, children)
    })
}

#[test]
fn a_reader_reads_the_nodes_of_a_merk_by_key_and_in_key_order() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    // Values in their cells, and values out of line.
    for (name, value_len) in [(1, 40), (2, 700)] {
        plant(&db, subtree(name), Merk::sized(b'k', 23, value_len));
    }
    let before = db.snapshot();
    // A later version has other Merks under the same subtrees.
    Writes {
        merks: vec![
            (subtree(1), Merk::sized(b'k', 31, 41)),
            (subtree(2), Merk::sized(b'k', 29, 701)),
        ],
        ..Writes::default()
    }
    .commit(&db);

    for (name, value_len, count) in [(1, 40, 23u32), (2, 700, 23)] {
        let operation = before.operation();
        let opened = operation
            .open(subtree(name))
            .expect("a lookup")
            .expect("entered");
        // By key: the node, its value, and the key of each child.
        let mut links = 0;
        for number in 0..count {
            let (found, value, children) = shown(&opened, &key(b'k', number)).expect("a node");
            assert_eq!(found, key(b'k', number));
            assert_eq!(value, vec![b'k' ^ number as u8; value_len]);
            for (child, smaller) in children.iter().zip([true, false]) {
                if !child.is_empty() {
                    links += 1;
                    assert_eq!(*child < found, smaller);
                }
            }
        }
        assert_eq!(links, count - 1, "every node but the root is a child");
        // A key that is no node, between two nodes and beyond either end.
        for absent in [key(b'k', count), key(b'j', 0), vec![b'k', 0, 0, 0, 3, 1]] {
            assert_eq!(shown(&opened, &absent), None);
        }
        // With no word of the children, none is read.
        let found = opened.node(&key(b'k', 4), None, |view| view.is_some());
        assert!(found.expect("a read"));

        // In key order, each move in an operation of its own.
        let mut walk = opened.walk().expect("a lookup");
        assert!(format!("{walk:?}").contains("NodeWalk"));
        drop(opened);
        drop(operation);
        assert_eq!(walk.key(), None);
        let mut keys = Vec::new();
        walk.seek_to_first(&before.operation()).expect("a move");
        while let Some(at) = walk.key() {
            keys.push(at.to_vec());
            let mut children = ChildKeys::new();
            let value_len_read = walk
                .node_with_children(&before.operation(), &mut children, |view| {
                    view.expect("the node the cursor stands on").value_len()
                })
                .expect("a read");
            assert_eq!(value_len_read as usize, value_len);
            walk.next(&before.operation()).expect("a move");
        }
        let all: Vec<Vec<u8>> = (0..count).map(|number| key(b'k', number)).collect();
        assert_eq!(keys, all);
        let mut back = Vec::new();
        walk.seek_to_last(&before.operation()).expect("a move");
        while let Some(at) = walk.key() {
            back.push(at.to_vec());
            walk.prev(&before.operation()).expect("a move");
        }
        back.reverse();
        assert_eq!(back, all);
        walk.seek(&before.operation(), &[b'k', 0, 0, 0, 3, 1])
            .expect("a move");
        assert_eq!(walk.key(), Some(key(b'k', 4).as_slice()));
        walk.seek_for_prev(&before.operation(), &[b'k', 0, 0, 0, 3, 1])
            .expect("a move");
        assert_eq!(walk.key(), Some(key(b'k', 3).as_slice()));
        // On no node it shows none.
        walk.seek(&before.operation(), b"l").expect("a move");
        let none = walk.node_with_children(&before.operation(), &mut ChildKeys::new(), |view| {
            view.is_none()
        });
        assert!(none.expect("a read"));

        // The cursor is of its reader: an operation of another is refused,
        // and the cursor goes on with its own.
        walk.seek_to_first(&before.operation()).expect("a move");
        let other = db.snapshot();
        let refused = walk.next(&other.operation()).expect_err("another reader");
        assert!(
            matches!(refused, ReadError::Cursor(CursorError::OtherReader { .. })),
            "{refused}"
        );
        assert!(refused.to_string().starts_with("cursor: "));
        assert!(std::error::Error::source(&refused).is_some());
        walk.next(&before.operation()).expect("a move");
        assert_eq!(walk.key(), Some(key(b'k', 1).as_slice()));
    }

    // The latest version has the later Merks.
    let latest = db.snapshot();
    let operation = latest.operation();
    let opened = operation
        .open(subtree(2))
        .expect("a lookup")
        .expect("entered");
    assert_eq!(shown(&opened, &key(b'k', 4)).expect("a node").1.len(), 701);
    assert!(shown(&opened, &key(b'k', 28)).is_some());
    assert_eq!(shown(&opened, &key(b'k', 29)), None);
    // A Merk with no node has none to show.
    Writes {
        enter: vec![(subtree(3), None)],
        ..Writes::default()
    }
    .commit(&db);
    let latest = db.snapshot();
    let operation = latest.operation();
    let opened = operation
        .open(subtree(3))
        .expect("a lookup")
        .expect("entered");
    assert_eq!(shown(&opened, b"k"), None);
    let mut walk = opened.walk().expect("a lookup");
    walk.seek_to_first(&operation).expect("a move");
    assert_eq!(walk.key(), None);
}

/// A reader is shown the unlinked records of a Merk as a write transaction
/// shows them: by key where the tree has no node, and among the nodes in
/// key order, whichever way the walk goes and turns. Whether a subtree
/// holds any is looked up once for what the reader keeps of it.
#[test]
fn a_reader_reads_the_unlinked_records_of_a_merk() {
    use crate::txn::unlinked::probe;

    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    // Records before the first node, between two nodes, and after the
    // last, one of them with a value that is kept out of line.
    let after = |number: u32| [key(b'k', number), vec![1]].concat();
    let loose = [vec![b'j'], after(2), after(4), after(5), vec![b'l']];
    let value = |number: usize| vec![0xa0 + number as u8; [50, 700][number % 2]];
    let mut merk = Merk::sized(b'k', 7, 40);
    for (number, at) in loose.iter().enumerate() {
        merk = merk.with_unlinked(at, &value(number));
    }
    plant(&db, subtree(1), merk);
    plant(&db, subtree(2), Merk::sized(b'k', 7, 40));
    let snapshot = db.snapshot();
    // A later version has none of the records.
    Writes {
        drop: vec![subtree(1)],
        ..Writes::default()
    }
    .commit(&db);

    let operation = snapshot.operation();
    let opened = operation
        .open(subtree(1))
        .expect("a lookup")
        .expect("entered");
    // By key: the record, as the node its writer stated, with no child
    // read for it.
    let lookups = probe::lookups();
    for (number, at) in loose.iter().enumerate() {
        let mut children = ChildKeys::new();
        let (unlinked, found) = opened
            .node(at, Some(&mut children), |view| {
                let view = view.expect("a record");
                let mut buffer = lignin_tree::value::ValueBuf::new();
                let found = view.value(&mut buffer).expect("a value").to_vec();
                assert_eq!(view.key(), at.as_slice());
                assert!(view.link(Side::Left).is_none() && view.link(Side::Right).is_none());
                (view.is_unlinked(), found)
            })
            .expect("a read");
        assert!(unlinked);
        assert_eq!(found, value(number));
        assert!(children.key(Side::Left).is_empty() && children.key(Side::Right).is_empty());
    }
    let linked = opened.node(&key(b'k', 3), None, |view| {
        view.expect("a node").is_unlinked()
    });
    assert!(!linked.expect("a read"));
    assert_eq!(shown(&opened, &after(3)), None);
    assert_eq!(probe::lookups(), lookups + 1, "one lookup for the subtree");

    // In key order, each move in an operation of its own.
    let mut all: Vec<(Vec<u8>, bool)> = (0..7).map(|number| (key(b'k', number), false)).collect();
    all.extend(loose.iter().map(|at| (at.clone(), true)));
    all.sort();
    let mut walk = opened.walk().expect("a lookup");
    let kept = opened.keep();
    drop(operation);
    let on = |walk: &mut crate::snapshot::NodeWalk| {
        let at = walk.key()?.to_vec();
        let mut children = ChildKeys::new();
        let unlinked = walk
            .node_with_children(&snapshot.operation(), &mut children, |view| {
                let view = view.expect("what the walk stands on");
                assert_eq!(view.key(), at.as_slice());
                view.is_unlinked()
            })
            .expect("a read");
        Some((at, unlinked))
    };
    assert_eq!(on(&mut walk), None);
    let mut forward = Vec::new();
    walk.seek_to_first(&snapshot.operation()).expect("a move");
    while let Some(at) = on(&mut walk) {
        forward.push(at);
        walk.next(&snapshot.operation()).expect("a move");
    }
    assert_eq!(forward, all);
    let mut back = Vec::new();
    walk.seek_to_last(&snapshot.operation()).expect("a move");
    while let Some(at) = on(&mut walk) {
        back.push(at);
        walk.prev(&snapshot.operation()).expect("a move");
    }
    back.reverse();
    assert_eq!(back, all);
    // A seek at each key and between any two, both ways.
    for (place, (at, _)) in all.iter().enumerate() {
        let beyond = [at.as_slice(), &[0]].concat();
        walk.seek(&snapshot.operation(), at).expect("a move");
        assert_eq!(on(&mut walk).as_ref(), Some(&all[place]));
        walk.seek_for_prev(&snapshot.operation(), at)
            .expect("a move");
        assert_eq!(on(&mut walk).as_ref(), Some(&all[place]));
        walk.seek(&snapshot.operation(), &beyond).expect("a move");
        assert_eq!(on(&mut walk).as_ref(), all.get(place + 1));
        walk.seek_for_prev(&snapshot.operation(), &beyond)
            .expect("a move");
        assert_eq!(on(&mut walk).as_ref(), Some(&all[place]));
    }
    // Steps that turn, from every place and in every order of three.
    for start in 0..all.len() {
        for steps in 0..8u8 {
            walk.seek(&snapshot.operation(), &all[start].0)
                .expect("a move");
            let mut place = Some(start);
            for step in 0..3 {
                let forward = steps >> step & 1 == 1;
                match forward {
                    true => walk.next(&snapshot.operation()).expect("a move"),
                    false => walk.prev(&snapshot.operation()).expect("a move"),
                }
                place = place.and_then(|place| match forward {
                    true => Some(place + 1).filter(|place| *place < all.len()),
                    false => place.checked_sub(1),
                });
                assert_eq!(on(&mut walk).as_ref(), place.map(|place| &all[place]));
            }
        }
    }
    assert_eq!(probe::lookups(), lookups + 1, "the walk asked nothing more");

    // A move that fails in one of the two cursors leaves the walk on no
    // key, wherever the other one stands, until it is sought again: the
    // cursor over the tree has moved when the one over the records fails.
    walk.seek(&snapshot.operation(), &all[3].0).expect("a move");
    walk.fail_next_record_move();
    let failed = walk.seek(&snapshot.operation(), &all[5].0);
    assert!(matches!(failed, Err(ReadError::NoRecords)), "{failed:?}");
    assert_eq!(walk.key(), None);
    let none = walk.node_with_children(&snapshot.operation(), &mut ChildKeys::new(), |view| {
        view.is_none()
    });
    assert!(none.expect("a read"));
    walk.next(&snapshot.operation()).expect("no move");
    walk.prev(&snapshot.operation()).expect("no move");
    assert_eq!(walk.key(), None);
    walk.seek(&snapshot.operation(), &all[3].0).expect("a move");
    assert_eq!(on(&mut walk).as_ref(), Some(&all[3]));
    walk.next(&snapshot.operation()).expect("a move");
    assert_eq!(on(&mut walk).as_ref(), Some(&all[4]));
    // The same for a step, from a record, and for a step that turns.
    assert!(all[0].1 && !all[1].1);
    for (from, back) in [(0, false), (1, true)] {
        walk.seek(&snapshot.operation(), &all[from].0)
            .expect("a move");
        walk.fail_next_record_move();
        let failed = match back {
            false => walk.next(&snapshot.operation()),
            true => walk.prev(&snapshot.operation()),
        };
        assert!(matches!(failed, Err(ReadError::NoRecords)), "{failed:?}");
        assert_eq!(walk.key(), None);
        walk.next(&snapshot.operation()).expect("no move");
        assert_eq!(walk.key(), None);
    }
    walk.seek_to_first(&snapshot.operation()).expect("a move");
    assert_eq!(on(&mut walk).as_ref(), Some(&all[0]));

    // The walk is of its reader: an operation of another is refused before
    // either of its cursors moves, also where only the one over the
    // records would move.
    for at in [&all[0].0, &all[1].0] {
        walk.seek(&snapshot.operation(), at).expect("a move");
        let other = db.snapshot();
        for refused in [
            walk.next(&other.operation()),
            walk.prev(&other.operation()),
            walk.seek_to_last(&other.operation()),
            walk.node_with_children(&other.operation(), &mut ChildKeys::new(), |_| ()),
        ] {
            let refused = refused.expect_err("another reader");
            assert!(
                matches!(refused, ReadError::Cursor(CursorError::OtherReader { .. })),
                "{refused}"
            );
        }
        assert_eq!(walk.key(), Some(at.as_slice()));
    }

    // What the reader keeps of the subtree keeps the answer: a later
    // operation that resumes from it asks nothing, for a read by key that
    // finds no node and for a walk.
    let operation = snapshot.operation();
    let resumed = operation.resume(&kept).expect("a lookup").expect("entered");
    assert_eq!(shown(&resumed, &after(3)), None);
    assert!(resumed.walk().is_ok());
    assert_eq!(probe::lookups(), lookups + 1);

    // A Merk with none: one lookup, and then nothing more is read for a
    // key that is no node or for a walk, which is the tree's cursor.
    let opened = operation
        .open(subtree(2))
        .expect("a lookup")
        .expect("entered");
    let lookups = probe::lookups();
    for absent in [after(3), vec![b'j'], vec![b'l']] {
        assert_eq!(shown(&opened, &absent), None);
    }
    let mut walk = opened.walk().expect("a lookup");
    let kept = opened.keep();
    let mut keys = 0;
    walk.seek_to_first(&operation).expect("a move");
    while walk.key().is_some() {
        keys += 1;
        walk.next(&operation).expect("a move");
    }
    assert_eq!(keys, 7);
    let resumed = operation.resume(&kept).expect("a lookup").expect("entered");
    assert_eq!(shown(&resumed, &after(3)), None);
    assert_eq!(probe::lookups(), lookups + 1);

    // A flat record under a subtree's data that is no unlinked record is
    // not shown as a node: not by its bytes, and not by a key that is
    // longer than a node's, whatever its bytes.
    let long = vec![b'x'; 256];
    let stated = NodePut {
        key: b"k",
        feature: &[3],
        value_hash: &HASH,
        kv_hash: &HASH,
        left: None,
        right: None,
        hosted: Hosted::Keep,
        value: ValuePut::Keep,
        stale: Stale::NONE,
        at: None,
    };
    let mut as_unlinked = Vec::new();
    crate::txn::unlinked::encode(&mut as_unlinked, &stated, b"a value").expect("a record");
    assert!(crate::txn::unlinked::decode(&long[..255], &as_unlinked).is_ok());
    let stored_long = as_unlinked.clone();
    Writes {
        enter: vec![(subtree(3), None)],
        records: vec![
            (Column::Data, subtree(3), b"flat".to_vec(), vec![9; 12]),
            (Column::Data, subtree(3), long.clone(), as_unlinked),
        ],
        ..Writes::default()
    }
    .commit(&db);
    let latest = db.snapshot();
    let operation = latest.operation();
    let opened = operation
        .open(subtree(3))
        .expect("a lookup")
        .expect("entered");
    let refused = opened.node(b"flat", None, |_| ()).expect_err("no node");
    assert!(matches!(refused, ReadError::NotUnlinked), "{refused}");
    assert!(refused.to_string().contains("no unlinked record"));
    // A read that is told that the subtree's tree type keeps flat records
    // shows each record as it is stored, by key and under a walk, and one
    // that is told it does not refuses as above.
    let bytes =
        |view: Option<NodeView<'_>>| view.and_then(|view| view.flat_record().map(<[u8]>::to_vec));
    let (every, none): (FlatKinds, FlatKinds) = (|_| true, |_| false);
    let shown = opened.node_by_kind(b"flat", None, every, bytes);
    assert_eq!(shown.expect("a read"), Some(vec![9; 12]));
    let shown = opened.node_by_kind(&long, None, every, bytes);
    assert_eq!(shown.expect("a read"), Some(stored_long.clone()));
    let shown = opened.node_by_kind(b"absent", None, every, bytes);
    assert_eq!(shown.expect("a read"), None);
    let told_none = opened.node_by_kind(b"flat", None, none, bytes);
    assert!(matches!(told_none, Err(ReadError::NotUnlinked)));
    let mut by_kind = opened.walk_by_kind(every).expect("a lookup");
    by_kind.seek_to_first(&operation).expect("a move");
    for stored in [vec![9; 12], stored_long.clone()] {
        let shown = by_kind.node_with_children(&operation, &mut ChildKeys::new(), bytes);
        assert_eq!(shown.expect("a read"), Some(stored));
        by_kind.next(&operation).expect("a move");
    }
    assert_eq!(by_kind.key(), None);
    assert!(std::error::Error::source(&refused).is_none());
    let mut walk = opened.walk().expect("a lookup");
    walk.seek_to_first(&operation).expect("a move");
    assert_eq!(walk.key(), Some(b"flat".as_slice()));
    let refused = walk.node_with_children(&operation, &mut ChildKeys::new(), |_| ());
    assert!(matches!(refused, Err(ReadError::NotUnlinked)));
    let refused = opened.node(&long, None, |_| ()).expect_err("no node");
    assert!(matches!(refused, ReadError::NotUnlinked), "{refused}");
    walk.next(&operation).expect("a move");
    assert_eq!(walk.key(), Some(long.as_slice()));
    let refused = walk.node_with_children(&operation, &mut ChildKeys::new(), |_| ());
    assert!(matches!(refused, Err(ReadError::NotUnlinked)));
    // The later version of the first subtree has nothing.
    assert!(operation.open(subtree(1)).expect("a lookup").is_none());
}

#[test]
fn a_reader_walks_the_records_of_a_column_across_its_operations() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let record = |name: u8, key: &[u8], value: &[u8]| {
        (Column::Meta, subtree(name), key.to_vec(), value.to_vec())
    };
    Writes {
        enter: vec![(subtree(1), None), (subtree(2), None)],
        records: vec![
            record(1, b"a", b"1"),
            record(1, b"c", b"3"),
            record(1, b"e", &[5; 900]),
            record(2, b"b", b"other subtree"),
        ],
        ..Writes::default()
    }
    .commit(&db);
    let before = db.snapshot();
    Writes {
        records: vec![record(1, b"d", b"4")],
        ..Writes::default()
    }
    .commit(&db);

    let operation = before.operation();
    let opened = operation
        .open(subtree(1))
        .expect("a lookup")
        .expect("entered");
    let mut walk = opened.record_walk(Column::Meta);
    assert!(format!("{walk:?}").contains("RecordWalk"));
    drop(opened);
    drop(operation);
    let value = |walk: &crate::snapshot::RecordWalk| {
        let mut value = Vec::new();
        let found = walk.value(&before.operation(), &mut value).expect("a read");
        found.then_some(value)
    };
    assert_eq!((walk.key(), value(&walk)), (None, None));
    let mut records = Vec::new();
    walk.seek_to_first(&before.operation()).expect("a move");
    while let Some(key) = walk.key() {
        records.push((key.to_vec(), value(&walk).expect("a value")));
        walk.next(&before.operation()).expect("a move");
    }
    // The records of its subtree and its version, and no other.
    assert_eq!(
        records,
        [
            (b"a".to_vec(), b"1".to_vec()),
            (b"c".to_vec(), b"3".to_vec()),
            (b"e".to_vec(), vec![5; 900]),
        ]
    );
    walk.seek_to_last(&before.operation()).expect("a move");
    assert_eq!(walk.key(), Some(b"e".as_slice()));
    walk.prev(&before.operation()).expect("a move");
    assert_eq!(walk.key(), Some(b"c".as_slice()));
    walk.seek(&before.operation(), b"b").expect("a move");
    assert_eq!(walk.key(), Some(b"c".as_slice()));
    walk.seek_for_prev(&before.operation(), b"b")
        .expect("a move");
    assert_eq!(walk.key(), Some(b"a".as_slice()));
    walk.prev(&before.operation()).expect("a move");
    assert_eq!(walk.key(), None);

    // The cursor is of its version: an operation of a later one is
    // refused, and the cursor stays where it was.
    walk.seek_to_first(&before.operation()).expect("a move");
    let later = db.snapshot();
    let refused = walk.next(&later.operation()).expect_err("a later version");
    assert!(
        matches!(refused, ReadError::OtherVersion { .. }),
        "{refused}"
    );
    assert!(refused.to_string().contains("flat records"));
    assert!(std::error::Error::source(&refused).is_none());
    let refused = walk.value(&later.operation(), &mut Vec::new());
    assert!(matches!(refused, Err(ReadError::OtherVersion { .. })));
    assert_eq!(walk.key(), Some(b"a".as_slice()));
    assert_eq!(value(&walk), Some(b"1".to_vec()));
    // And of its database: an operation of another one is refused at the
    // same version.
    let other_dir = temp();
    let other = Db::open(other_dir.path(), &options()).expect("a new database");
    Writes {
        enter: vec![(subtree(1), None)],
        records: vec![record(1, b"a", b"elsewhere")],
        ..Writes::default()
    }
    .commit(&other);
    let elsewhere = other.snapshot();
    assert_eq!(elsewhere.version(), before.version());
    let refused = walk.next(&elsewhere.operation());
    assert!(matches!(refused, Err(ReadError::OtherVersion { .. })));
    assert_eq!(value(&walk), Some(b"1".to_vec()));
}

/// A lookup of a write transaction that has frames can show it a cell of
/// a committed page that it has never copied. The value of such a node
/// can be pending: an earlier commit logged it, and write-back has not
/// placed it. Write-back places it with a page step, which the
/// transaction does not itself hold back, since it holds no private copy
/// of that page, and which puts a frame with the pointer and without the
/// value's bytes in the place of the one the transaction was shown.
///
/// Here the step falls where a thread of write-back can make it: after
/// the transaction was shown the node and before it reads the node's
/// value. The value is read all the same, from the frame the node was
/// shown in (D-262).
#[test]
fn a_write_transaction_reads_a_value_that_write_back_places_while_it_is_shown_the_node() {
    let dir = temp();
    let db = open_at_rest(dir.path(), &options()).expect("a new database");
    let merk = Merk::sized(b'k', 300, 700);
    let items = merk.items();
    plant(&db, subtree(1), merk);
    let flush = &db.shared.store.flush;
    assert_eq!(flush.stats().values_placed, 0, "write-back is at rest");

    // The transaction writes another Merk: it has frames from here on.
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let other = Writes {
        enter: vec![(subtree(2), None)],
        merks: vec![(subtree(2), Merk::sized(b'j', 3, 24))],
        ..Writes::default()
    };
    (other.with(|batch| scope.apply(batch))).expect("the batch is applied");

    let (key, value) = items.last().expect("a node").clone();
    let read = scope.node(subtree(1), &key, |view| {
        let view = view.expect("the node is there");
        // What write-back's thread does beside the transaction.
        db.shared.write_back().pass().expect("a pass");
        let mut buf = lignin_tree::value::ValueBuf::new();
        view.value(&mut buf).map(<[u8]>::to_vec)
    });
    let stats = flush.stats();
    println!(
        "{} values placed and {} steps put off inside the read: {:?}",
        stats.values_placed,
        stats.steps_put_off,
        read.as_ref().map(|read| read.as_ref().map(Vec::len))
    );
    assert_eq!(
        (stats.values_placed, stats.steps_put_off),
        (300, 0),
        "the value of the node that was shown was placed"
    );
    let read = read.expect("a read").expect("the value of the node");
    assert_eq!(read, value);
    scope.rollback();
    txn.rollback().expect("a rollback");
}

/// What a read of the node under a key came to, through a hint or with
/// none: the length of its value, its handle, and the key and the handle
/// of each child.
type Came = (u32, NodeHandle, [(Vec<u8>, Option<NodeHandle>); 2]);

fn came(
    opened: &crate::snapshot::Opened<'_, '_>,
    at: &[u8],
    hint: Option<NodeHandle>,
) -> Option<Came> {
    let none: FlatKinds = |_| false;
    let mut children = ChildKeys::new();
    let shown = opened
        .follow_by_kind(at, hint, Some(&mut children), none, |view| {
            view.map(|view| {
                assert_eq!(view.key(), at);
                (view.value_len(), view.handle().expect("a cell"))
            })
        })
        .expect("a read");
    shown.map(|(value_len, handle)| {
        let children = [Side::Left, Side::Right]
            .map(|side| (children.key(side).to_vec(), children.handle(side)));
        (value_len, handle, children)
    })
}

/// A reader goes to a node through the handle that an earlier read of it
/// gave, and searches by key where the handle is not good. A read that
/// follows a handle is held to the search here, and counted
/// (`snapshot::nodes::held`): the count says which reads followed one.
#[test]
fn a_reader_follows_a_handle_to_a_node_and_searches_by_key_where_it_is_not_good() {
    use crate::snapshot::held::compared;

    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let count = 300;
    plant(&db, subtree(1), Merk::sized(b'k', count, 40));
    plant(&db, subtree(2), Merk::sized(b'k', count, 41));
    Writes {
        enter: vec![(subtree(3), None)],
        ..Writes::default()
    }
    .commit(&db);
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let open = |name| {
        let opened = operation.open(subtree(name)).expect("a lookup");
        opened.expect("entered")
    };
    let (own, beside, empty) = (open(1), open(2), open(3));
    // A read with a hint, and whether it followed the hint.
    let hinted = |opened: &crate::snapshot::Opened<'_, '_>, at: &[u8], hint| {
        let before = compared();
        let node = came(opened, at, Some(hint));
        (node, compared() - before == 1)
    };

    let mut links = 0;
    for number in 0..count {
        let at = key(b'k', number);
        let before = compared();
        let (len, handle, children) = came(&own, &at, None).expect("a node");
        assert_eq!(compared(), before, "a read with no hint follows none");
        assert_eq!(len, 40);
        // Through its own handle the read comes to the same node, with the
        // same children.
        let (followed, by_handle) = hinted(&own, &at, handle);
        assert!(by_handle);
        assert_eq!(followed, Some((len, handle, children.clone())));
        // The handle of each child is the one the child's own read gives,
        // and is followed.
        for (child, told) in children {
            match told {
                Some(told) => {
                    links += 1;
                    let (_, of_child, _) = came(&own, &child, None).expect("a child");
                    assert_eq!(told, of_child);
                    let (followed, by_handle) = hinted(&own, &child, told);
                    assert!(by_handle);
                    assert_eq!(followed.map(|(len, at, _)| (len, at)), Some((40, told)));
                }
                None => assert!(child.is_empty(), "a child with a key has a handle"),
            }
        }
    }
    assert_eq!(links, count - 1, "every node but the root is a child");

    let (first, second, absent) = (key(b'k', 7), key(b'k', 8), key(b'k', count));
    let (_, of_first, _) = came(&own, &first, None).expect("a node");
    let (_, beside_first, _) = came(&beside, &first, None).expect("a node");
    assert_ne!(of_first, beside_first);
    // A handle whose cell has another key is not followed.
    let (node, by_handle) = hinted(&own, &second, of_first);
    assert!(!by_handle);
    assert_eq!(node.map(|(len, ..)| len), Some(40));
    // Under a key with no node, a handle shows none.
    assert_eq!(hinted(&own, &absent, of_first), (None, false));
    // A Merk with no node follows no handle, whatever key its cell has.
    assert_eq!(hinted(&empty, &first, of_first), (None, false));
    // A handle of another reader is of another lease, and is not followed:
    // also one of the same version.
    let other = db.snapshot();
    assert_eq!(other.version(), snapshot.version());
    let other_operation = other.operation();
    let other_own = other_operation
        .open(subtree(1))
        .expect("a lookup")
        .expect("entered");
    let (_, of_other, _) = came(&other_own, &first, None).expect("a node");
    assert_ne!(of_other.lease(), of_first.lease());
    let (node, by_handle) = hinted(&own, &first, of_other);
    assert!(!by_handle);
    assert_eq!(node.map(|(len, at, _)| (len, at)), Some((40, of_first)));
    // A later operation of the same reader follows what an earlier one
    // was given.
    let later = snapshot.operation();
    let again = later.open(subtree(1)).expect("a lookup").expect("entered");
    let (node, by_handle) = hinted(&again, &first, of_first);
    assert!(by_handle);
    assert_eq!(node.map(|(len, at, _)| (len, at)), Some((40, of_first)));
}

/// A good handle that was read for another Merk, whose node has the same
/// key, is followed: no check of a handle says which Merk its cell is in.
/// The hold of a test build finds it, since the search by key from the
/// root of the reader's own Merk comes to another cell.
#[test]
#[should_panic(expected = "through a kept location came to the cell at")]
fn a_handle_that_was_read_for_another_merk_fails_the_hold() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 30, 40));
    plant(&db, subtree(2), Merk::sized(b'k', 30, 41));
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let open = |name| {
        let opened = operation.open(subtree(name)).expect("a lookup");
        opened.expect("entered")
    };
    let at = key(b'k', 7);
    let (_, of_the_other, _) = came(&open(2), &at, None).expect("a node");
    came(&open(1), &at, Some(of_the_other));
}

/// What a reader keeps of a subtree is of one version of one database and
/// of no reader: an operation of another snapshot of that version resumes
/// from it with the entry the directory has, and an operation at another
/// version, or of another database, looks the subtree up.
#[test]
fn a_kept_subtree_is_resumed_by_any_reader_of_its_version() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 23, 40));
    let first = db.snapshot();
    let (kept, entry) = {
        let operation = first.operation();
        let opened = operation.open(subtree(1)).expect("a lookup");
        let opened = opened.expect("entered");
        let entry = opened.entry().clone();
        (opened.keep(), entry)
    };
    assert_eq!(kept.subtree(), subtree(1));
    let value_len = |opened: &crate::snapshot::Opened<'_, '_>, number: u32| {
        shown(opened, &key(b'k', number)).map(|(_, value, _)| value.len())
    };

    // Another snapshot of the same version.
    let second = db.snapshot();
    assert_eq!(second.version(), first.version());
    {
        let operation = second.operation();
        let resumed = operation.resume(&kept).expect("a lookup");
        let resumed = resumed.expect("entered");
        assert_eq!(*resumed.entry(), entry);
        assert_eq!(
            (value_len(&resumed, 3), value_len(&resumed, 30)),
            (Some(40), None)
        );
    }

    // A later version has another Merk under the subtree. An operation of
    // it looks the subtree up, and one of the earlier version resumes.
    Writes {
        merks: vec![(subtree(1), Merk::sized(b'k', 31, 41))],
        ..Writes::default()
    }
    .commit(&db);
    let later = db.snapshot();
    assert!(later.version() > first.version());
    {
        let operation = later.operation();
        let resumed = operation.resume(&kept).expect("a lookup");
        let resumed = resumed.expect("entered");
        assert_ne!(*resumed.entry(), entry);
        assert_eq!(
            (value_len(&resumed, 3), value_len(&resumed, 30)),
            (Some(41), Some(41))
        );
        let operation = first.operation();
        let resumed = operation.resume(&kept).expect("a lookup");
        let resumed = resumed.expect("entered");
        assert_eq!(*resumed.entry(), entry);
        assert_eq!(
            (value_len(&resumed, 3), value_len(&resumed, 30)),
            (Some(40), None)
        );
    }

    // Another database, at the same version, with no subtree under the
    // prefix: what was kept in this one opens nothing there.
    let other_dir = temp();
    let other = Db::open(other_dir.path(), &options()).expect("a new database");
    plant(&other, subtree(2), Merk::sized(b'k', 5, 9));
    let elsewhere = other.snapshot();
    assert_eq!(elsewhere.version(), first.version());
    let operation = elsewhere.operation();
    assert!(operation.resume(&kept).expect("a lookup").is_none());
}
