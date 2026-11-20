//! What a reader reads of a Merk and of a column through the facade: a
//! node by its key, the nodes and the flat records in key order with a
//! cursor that is kept from one operation to the next, and the physical
//! checker on an open database.

use lignin_tree::cursor::CursorError;

use super::*;
use crate::{
    snapshot::ReadError,
    txn::ChildKeys,
    verify::{Rule, Scope},
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
        let mut walk = opened.walk();
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
    let mut walk = opened.walk();
    walk.seek_to_first(&operation).expect("a move");
    assert_eq!(walk.key(), None);
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
}

#[test]
fn the_physical_checker_reads_an_open_database() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    assert!(db.check(Scope::Reader, None).is_sound());
    let host = Merk::sized(b'h', 9, 30).hosting(&key(b'h', 4), subtree(2));
    Writes {
        enter: vec![
            (subtree(1), None),
            (subtree(2), Some((subtree(1), key(b'h', 4)))),
        ],
        merks: vec![(subtree(1), host), (subtree(2), Merk::sized(b'k', 40, 600))],
        records: vec![(Column::Aux, subtree(2), b"k".to_vec(), vec![7; 500])],
        ..Writes::default()
    }
    .commit(&db);
    let report = db.check(Scope::Reader, None);
    assert!(report.is_sound(), "{:?}", report.findings);
    assert_eq!(report.census.merks, 2);
    db.close().expect("a close");

    // A database that was just opened is still.
    let db = Db::open(dir.path(), &options()).expect("the database again");
    let report = db.check(Scope::Still, None);
    assert!(report.is_sound(), "{:?}", report.findings);
    assert_eq!(report.census.merks, 2);

    // What a store must not hold is found: a subtree whose entry names a
    // host that is no node of the Merk it is said to be in.
    Writes {
        enter: vec![(subtree(3), Some((subtree(1), b"no node".to_vec())))],
        ..Writes::default()
    }
    .commit(&db);
    let report = db.check(Scope::Reader, None);
    assert!(report.has(Rule::EntryHost), "{:?}", report.findings);
    assert!(!report.is_sound());
    // The subtree goes again, and the database is sound as before.
    Writes {
        drop: vec![subtree(3)],
        ..Writes::default()
    }
    .commit(&db);
    let report = db.check(Scope::Reader, None);
    assert!(report.is_sound(), "{:?}", report.findings);
}
