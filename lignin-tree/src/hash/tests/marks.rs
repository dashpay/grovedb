//! The marks themselves: what a set of stored hashes is, and how marks are
//! made, taken off, merged and taken through a write.

use lignin_format::tree_page::CellContent;

use super::*;
use crate::{
    fragment::HostedWrite,
    fragment::{Carried, Cause, LinkWrite, NodeWrite, Target, ValueWrite},
    node::NodeHandle,
};

const LEFT: HashField = HashField::Child(Side::Left);
const RIGHT: HashField = HashField::Child(Side::Right);

fn marked(hashes: &Hashes) -> Vec<(CellAddress, Stale)> {
    hashes.marked().collect()
}

fn set(fields: &[HashField]) -> Stale {
    fields.iter().copied().collect()
}

/// The handle of a node in the slot `slot_raw` of a page with the id
/// `page_raw`.
fn handle(page_raw: u32, slot_raw: u16) -> NodeHandle {
    let mut frame = vec![0; SIZE.get()];
    let mut nodes =
        NodePageMut::create(&mut frame, SIZE, page(page_raw), Lease::take(), ()).expect("a page");
    let mut last = None;
    for number in 0..slot_raw {
        let key = number.to_le_bytes();
        last = Some(nodes.insert(&CellContent {
            key: &key,
            feature: &[],
            value_hash: &[0; 32],
            kv_hash: &[0; 32],
            left: None,
            right: None,
            hosted_roots: &[],
            value: CellValue::Inline(b""),
            fragment_root: true,
        }));
    }
    let handle = last.expect("a slot").expect("the cell fits");
    assert_eq!(handle.address(), address(page_raw, slot_raw));
    handle
}

fn carried(from: CellAddress, to: NodeHandle) -> Carried {
    Carried {
        from,
        to,
        cause: Cause::Split,
        node: None,
        hosted_by: None,
    }
}

fn written<'a>(at: CellAddress, left: bool, right: bool) -> NodeWrite<'a> {
    let link = |there: bool| {
        there.then_some(LinkWrite {
            child: Target::Kept,
            hash: &[0; 32],
            heights: (0, 0),
            child_key_len: 1,
            aggregate: &[],
        })
    };
    NodeWrite {
        at: Some(at),
        key: b"key",
        feature: &[],
        value_hash: &[0; 32],
        kv_hash: &[0; 32],
        left: link(left),
        right: link(right),
        hosted: HostedWrite::Keep,
        value: ValueWrite::Keep,
    }
}

#[test]
fn a_set_of_stored_hashes() {
    assert_eq!(Stale::default(), Stale::NONE);
    assert!(Stale::NONE.is_empty());
    assert_eq!((Stale::NONE.len(), Stale::ALL.len()), (0, 4));
    assert_eq!(Stale::ALL, set(&HashField::ALL));
    assert_eq!(Stale::ALL.fields().collect::<Vec<_>>(), HashField::ALL);

    for field in HashField::ALL {
        let one = Stale::of(field);
        assert_eq!(one, Stale::from(field));
        assert_eq!((one.len(), one.is_empty()), (1, false));
        assert_eq!(one.fields().collect::<Vec<_>>(), [field]);
        for other in HashField::ALL {
            assert_eq!(one.has(other), other == field);
        }
        assert_eq!(one.without(field), Stale::NONE);
        assert_eq!(Stale::ALL.without(field).with(field), Stale::ALL);
        assert!(!Stale::ALL.without(field).has(field));
    }

    let children = set(&[RIGHT, LEFT]);
    assert_eq!(children.fields().collect::<Vec<_>>(), [LEFT, RIGHT]);
    assert_eq!(children.with(LEFT), children);
    let own = Stale::of(HashField::Value).with(HashField::KeyValue);
    assert_eq!(children.union(own), Stale::ALL);
    assert!(Stale::ALL.contains(children) && children.contains(Stale::NONE));
    assert!(children.contains(Stale::of(LEFT)) && !children.contains(own));
    assert!(!Stale::of(LEFT).contains(children));

    assert_eq!(format!("{:?}", Stale::NONE), "{}");
    assert_eq!(
        format!("{:?}", Stale::ALL),
        "{\"value hash\", \"key-value hash\", \"hash of the left child\", \
         \"hash of the right child\"}"
    );
}

#[test]
fn a_mark_is_one_flag_for_one_stored_hash_of_one_node() {
    let (first, second) = (address(3, 1), address(2, 9));
    let mut hashes = Hashes::new();
    assert!(hashes.is_empty());
    assert_eq!(hashes.stale(first), Stale::NONE);

    hashes.mark(first, LEFT);
    assert_eq!(hashes.stale(first), Stale::of(LEFT));
    assert_eq!(hashes.stale(second), Stale::NONE);
    hashes.mark(first, LEFT);
    hashes.mark(first, RIGHT);
    assert_eq!(hashes.stale(first), set(&[LEFT, RIGHT]));

    // A key-value hash is made from the value hash, and goes with it.
    hashes.mark(second, HashField::Value);
    assert_eq!(
        hashes.stale(second),
        set(&[HashField::Value, HashField::KeyValue])
    );
    assert_eq!((hashes.len(), hashes.is_empty()), (2, false));
    // In the order of the addresses.
    assert_eq!(
        marked(&hashes),
        [
            (second, set(&[HashField::Value, HashField::KeyValue])),
            (first, set(&[LEFT, RIGHT]))
        ]
    );

    // A writer that made a hash takes that mark off, and no other.
    hashes.unmark(second, HashField::Value);
    assert_eq!(hashes.stale(second), Stale::of(HashField::KeyValue));
    hashes.unmark(second, LEFT);
    assert_eq!(hashes.stale(second), Stale::of(HashField::KeyValue));
    hashes.unmark(second, HashField::KeyValue);
    assert_eq!(marked(&hashes), [(first, set(&[LEFT, RIGHT]))]);
    hashes.unmark(second, HashField::KeyValue);
    hashes.unmark(address(9, 9), LEFT);
    assert_eq!(hashes.len(), 1);

    hashes.mark(second, HashField::KeyValue);
    assert_eq!(hashes.stale(second), Stale::of(HashField::KeyValue));
    assert_eq!(hashes.forget(second), Stale::of(HashField::KeyValue));
    assert_eq!(hashes.forget(second), Stale::NONE);
    assert_eq!(hashes.len(), 1);
    hashes.clear();
    assert!(hashes.is_empty());
    assert_eq!(hashes, Hashes::new());
    assert_eq!(*hashes.counters(), Counters::default());
}

#[test]
fn the_marks_follow_a_write() {
    let all = set(&HashField::ALL);
    let (gone, parent, stays) = (address(1, 1), address(1, 2), address(1, 3));
    let (first, second, third) = (address(2, 1), address(2, 2), address(3, 1));
    let unmarked = address(4, 4);
    let mut hashes = Hashes::new();
    for at in [gone, parent, stays, first, second] {
        for field in HashField::ALL {
            hashes.mark(at, field);
        }
    }
    let before = hashes.clone();

    // A write that names none of them changes nothing.
    hashes.follow(&Write::default(), &Report::default());
    assert_eq!(hashes, before);

    let nodes = [
        written(parent, false, true),
        written(unmarked, false, false),
    ];
    let write = Write {
        nodes: &nodes,
        removed: &[gone, unmarked],
        ..Write::default()
    };
    // The cell at `first` goes to where the cell at `second` was, which
    // goes to another page, and a cell with no mark takes the slot of the
    // removed one.
    let report = Report {
        carried: vec![
            carried(first, handle(2, 2)),
            carried(second, handle(3, 1)),
            carried(address(5, 5), handle(1, 1)),
        ],
        ..Report::default()
    };
    hashes.follow(&write, &report);
    assert_eq!(
        marked(&hashes),
        [
            // The mark of the left child went with the link.
            (parent, all.without(LEFT)),
            (stays, all),
            (second, all),
            (third, all),
        ]
    );
    assert_eq!(hashes.stale(gone), Stale::NONE, "the removed cell's marks");
    assert_eq!(hashes.stale(first), Stale::NONE);

    // With no mark there is nothing to take anywhere.
    let mut clean = Hashes::new();
    clean.follow(&write, &report);
    assert!(clean.is_empty());
}

#[test]
fn a_scope_that_merges_gives_its_marks_to_the_transaction() {
    let (shared, own, other) = (address(1, 1), address(1, 2), address(2, 1));
    let mut transaction = Hashes::new();
    transaction.mark(shared, LEFT);
    transaction.mark(own, HashField::KeyValue);
    transaction.counters = Counters {
        nodes: 1,
        value_hashes: 2,
        kv_hashes: 3,
        node_hashes: 4,
        written: 5,
        unchanged: 6,
    };
    let mut scope = Hashes::new();
    scope.mark(shared, RIGHT);
    scope.mark(other, HashField::Value);
    scope.counters = Counters {
        nodes: 10,
        value_hashes: 20,
        kv_hashes: 30,
        node_hashes: 40,
        written: 50,
        unchanged: 60,
    };

    transaction.merge(scope);
    assert_eq!(
        marked(&transaction),
        [
            (shared, set(&[LEFT, RIGHT])),
            (own, Stale::of(HashField::KeyValue)),
            (other, set(&[HashField::Value, HashField::KeyValue])),
        ]
    );
    assert_eq!(
        *transaction.counters(),
        Counters {
            nodes: 11,
            value_hashes: 22,
            kv_hashes: 33,
            node_hashes: 44,
            written: 55,
            unchanged: 66,
        }
    );
}
