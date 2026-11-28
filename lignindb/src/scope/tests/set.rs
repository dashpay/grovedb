//! The write set under scopes (`transactions.md` WT2, SC5, SC6): each
//! operation has the scope that made it, and a scope's operations are
//! taken out from between those of others.

use lignin_format::CommitVersion;
use lignin_tree::hash::Stale;

use crate::txn::{
    write_set::{NodeValue, Own, WriteSet},
    Batch, Column, Hosted, KeySet, MerkWrite, NodePut, RecordWrite, RootPut, Subtree, SubtreeWrite,
    Unit, ValuePut,
};

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn subtree(name: u8) -> Subtree {
    Subtree::new([name; 32])
}

fn aux(name: u8) -> Unit {
    Unit::new(Column::Aux, subtree(name))
}

/// One operation of a scope: flat records, a node that keeps its value, or
/// a drop of a subtree that covers one below it.
#[derive(Clone, Copy)]
enum Op {
    Put(u8, &'static [u8], &'static [u8]),
    Range(u8, &'static [u8]),
    /// A node of the Merk of that name, which keeps the value it had.
    Node(u8, &'static [u8]),
    /// A drop of the first subtree, which covers what was written under
    /// the second.
    Drop(u8, u8),
    Enter(u8),
}

fn add(set: &mut WriteSet, (scope, laid, first_write, op): (u32, bool, u64, Op)) {
    let hash = [first_write as u8; 32];
    let nodes;
    let merks;
    let records;
    let subtrees;
    let (mut empty, mut cut) = (Vec::new(), Vec::new());
    let mut batch = Batch::default();
    match op {
        Op::Put(name, key, value) => {
            records = [RecordWrite::Put {
                unit: aux(name),
                key,
                value,
            }];
            batch.records = &records;
        }
        Op::Range(name, start) => {
            records = [RecordWrite::DeleteRange {
                unit: aux(name),
                start,
                end: None,
            }];
            batch.records = &records;
        }
        Op::Node(name, key) => {
            nodes = [NodePut {
                key,
                feature: b"f",
                value_hash: &hash,
                kv_hash: &hash,
                left: None,
                right: None,
                hosted: Hosted::Keep,
                value: ValuePut::Keep,
                stale: Stale::NONE,
                at: None,
            }];
            merks = [MerkWrite {
                subtree: subtree(name),
                nodes: &nodes,
                removed: &[],
                root: RootPut::Set(Some(key)),
                anchored: false,
                whole: false,
            }];
            batch.merks = &merks;
            empty.push(set.rooted(&subtree(name)) != Some(true));
        }
        Op::Drop(name, below) => {
            subtrees = [SubtreeWrite::Drop {
                subtree: subtree(name),
            }];
            batch.subtrees = &subtrees;
            cut.push(subtree(below));
        }
        Op::Enter(name) => {
            subtrees = [SubtreeWrite::Enter {
                subtree: subtree(name),
                tree_type: b"merk",
                host: None,
            }];
            batch.subtrees = &subtrees;
        }
    }
    set.add(&batch, version(first_write), &empty, &cut, &[], &[], &[]);
    set.tag_last(scope, laid);
}

/// What a set says of every key the tests write, and of the set as a
/// whole.
fn shown(set: &WriteSet) -> Vec<String> {
    let mut shown = Vec::new();
    for name in 1..=6 {
        for key in [&b"a"[..], b"b", b"n"] {
            for unit in [aux(name), Unit::data(subtree(name))] {
                let own = match set.own(unit, key) {
                    Own::Node(id) => {
                        let entry = set.entry(id);
                        let value = set.node_value(id).map(|value| match value {
                            NodeValue::Bytes(bytes) => bytes.to_vec(),
                            NodeValue::Stored(_) => unreachable!("no load is in these sets"),
                        });
                        format!("node {:?} {} {value:?}", entry.first_write, entry.met)
                    }
                    Own::Put(value) => {
                        let entry = set.entry(set.newest(unit, key).expect("an entry"));
                        format!("put {value:?} {:?} {}", entry.first_write, entry.met)
                    }
                    other => format!("{other:?}"),
                };
                shown.push(format!("{unit} {key:?}: {own}"));
            }
        }
        shown.push(format!("rooted {name}: {:?}", set.rooted(&subtree(name))));
        shown.push(format!("host {name}: {:?}", set.host_of(&subtree(name))));
    }
    shown.push(format!("leaving: {:?}", set.leaving().collect::<Vec<_>>()));
    shown.push(format!("written: {:?}", set.written().collect::<Vec<_>>()));
    shown.push(format!("writes: {}, met: {}", set.len(), set.met()));
    shown
}

#[test]
fn a_scopes_operations_are_taken_out_from_between_those_of_others() {
    // The transaction's own operation, then those of three scopes, each
    // dated with the version its operation read (SC5).
    let ops = [
        (0, true, 1, Op::Put(1, b"a", b"merged")),
        (0, true, 1, Op::Node(4, b"n")),
        (1, true, 2, Op::Put(2, b"a", b"one")),
        (2, false, 2, Op::Put(3, b"a", b"two")),
        (1, true, 3, Op::Node(4, b"n")),
        (2, false, 3, Op::Enter(6)),
        (2, false, 3, Op::Put(6, b"b", b"below")),
        (3, false, 3, Op::Put(5, b"a", b"three")),
        (2, false, 4, Op::Drop(3, 6)),
        (1, true, 4, Op::Range(2, b"a")),
        (2, false, 5, Op::Put(3, b"b", b"again")),
        (3, false, 5, Op::Put(5, b"b", b"more")),
    ];
    let build = |without: &[u32]| {
        let mut set = WriteSet::new();
        for op in ops.iter().filter(|(scope, ..)| !without.contains(scope)) {
            add(&mut set, *op);
        }
        // A commit outside met what each of the scopes wrote first.
        let mut keys = KeySet::new();
        keys.key(aux(2), b"a").key(aux(3), b"a").key(aux(5), b"a");
        keys.key(Unit::data(subtree(4)), b"n");
        set.meet(version(3), &keys);
        // The transaction took its own copy of the value a node kept.
        if let Own::Node(id) = set.own(Unit::data(subtree(4)), b"n") {
            let first = std::iter::successors(Some(id), |id| set.entry(*id).prev).last();
            set.keep(first.expect("an entry"), b"a kept value");
        }
        set
    };

    let mut set = build(&[]);
    assert_eq!(set.ops_of(2), [3, 5, 6, 8, 10]);
    assert!(set.laid(2) && !set.laid(3));
    assert_eq!(set.unlaid_leaving(), [subtree(3)]);
    let dropped = set.newest(aux(6), b"b").expect("the drop covers it");
    assert!(set.unlaid(dropped) && !set.unlaid(set.newest(aux(1), b"a").expect("a put")));
    assert_eq!(set.newest(aux(1), b"zz"), None);
    assert_eq!(set.newest(aux(9), b"a"), None);
    assert!(!set.remove_scope(9), "a scope that wrote nothing");

    // The mark of a savepoint that was set before any scope opened is a
    // place of the set through all of it.
    let mut marked = WriteSet::new();
    add(&mut marked, ops[0]);
    add(&mut marked, ops[1]);
    let savepoint = marked.mark();
    assert!(set.holds(savepoint));

    // The second scope is rolled back: the set is the one the others
    // made, with their first-write versions, what met them, and the copy
    // of the kept value.
    assert!(set.remove_scope(2));
    assert_eq!(shown(&set), shown(&build(&[2])));
    assert_eq!(set.ops_of(2), [0usize; 0]);
    assert_eq!(set.ops_of(3), [4, 6]);
    assert!(set.unlaid_leaving().is_empty());
    assert!(set.holds(savepoint));
    // The third is laid out and merges, the first is rolled back: its
    // operations are not the last of the set either.
    set.lay_scope(3);
    assert!(set.laid(4) && set.ops_of(3) == [4, 6]);
    set.merge_scope(3);
    assert!(set.ops_of(3).is_empty() && set.ops_of(0).len() == 4);
    assert!(set.remove_scope(1));
    assert_eq!(shown(&set), shown(&build(&[1, 2])));
    // A scope whose operations are the last is taken out as a rollback
    // to the place before them.
    add(&mut set, (4, true, 6, Op::Put(1, b"b", b"last")));
    add(&mut set, (4, true, 6, Op::Node(4, b"n")));
    assert!(set.remove_scope(4));
    assert_eq!(shown(&set), shown(&build(&[1, 2])));
    // The savepoint's mark still takes out what came after it.
    assert!(set.rewind(savepoint));
    assert_eq!(shown(&set), shown(&marked_met(&ops)));
}

/// The transaction's own two operations, met as the test's commit meets
/// them.
fn marked_met(ops: &[(u32, bool, u64, Op)]) -> WriteSet {
    let mut set = WriteSet::new();
    add(&mut set, ops[0]);
    add(&mut set, ops[1]);
    let mut keys = KeySet::new();
    keys.key(Unit::data(subtree(4)), b"n");
    set.meet(version(3), &keys);
    if let Own::Node(id) = set.own(Unit::data(subtree(4)), b"n") {
        set.keep(id, b"a kept value");
    }
    set
}

#[test]
fn a_drop_outside_meets_what_is_under_a_subtree() {
    let mut set = WriteSet::new();
    add(&mut set, (0, true, 1, Op::Put(1, b"a", b"under")));
    add(&mut set, (0, true, 1, Op::Node(1, b"n")));
    add(&mut set, (0, true, 1, Op::Put(2, b"a", b"beside")));
    add(&mut set, (0, true, 1, Op::Drop(3, 1)));
    add(&mut set, (0, true, 5, Op::Put(1, b"b", b"later")));
    // Every write under the subtree that was made before the version,
    // in every column, and a drop that covers it.
    set.meet_subtree(subtree(1), version(4));
    assert_eq!(set.met(), 3);
    assert!(!set.entry(set.newest(aux(2), b"a").expect("a put")).met);
    assert!(!set.entry(set.newest(aux(1), b"b").expect("a put")).met);
    set.meet_subtree(subtree(1), version(4));
    assert_eq!(set.met(), 3, "an entry is met once");
    // A drop of the subtree, and of no other.
    let mut set = WriteSet::new();
    add(&mut set, (0, true, 1, Op::Drop(3, 1)));
    add(&mut set, (0, true, 1, Op::Drop(4, 2)));
    set.meet_leaving(subtree(4), version(1));
    assert_eq!(set.met(), 0, "a commit before the drop does not meet it");
    set.meet_leaving(subtree(4), version(2));
    assert_eq!(set.met(), 1);
    set.meet_leaving(subtree(9), version(2));
    assert_eq!(set.met(), 1);
}
