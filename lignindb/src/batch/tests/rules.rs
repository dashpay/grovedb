//! What a batch holds after a sequence of calls, and the write of a
//! transaction it resolves to. No store is needed here.

use super::{items, leaf, subtree, tree, unit, Rec};
use crate::{
    batch::{Resolved, StagedBatch, Written},
    txn::{
        BatchError, Column, Differs, HostPut, Hosted, RecordWrite, RootPut, Subtree, SubtreeWrite,
        TxnError, ValuePut,
    },
};

const A: u8 = 1;
const B: u8 = 2;
const C: u8 = 3;
const D: u8 = 4;

fn name(subtree: Subtree) -> u8 {
    subtree.as_bytes()[0]
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Merk {
    subtree: u8,
    /// Each put: its key, its value if it states one, and the Merks it
    /// names if it names them.
    nodes: Vec<(String, Option<String>, Option<Vec<u8>>)>,
    removed: Vec<String>,
    /// The root, if the write sets it.
    root: Option<Option<String>>,
    anchored: bool,
}

/// The write of a transaction that a batch is, in owned form.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Shape {
    pub(super) subtrees: Vec<String>,
    merks: Vec<Merk>,
    records: Vec<String>,
}

pub(super) fn shape<C>(resolved: &Resolved<C>) -> Shape {
    resolved.with_batch(|batch| Shape {
        subtrees: (batch.subtrees.iter())
            .map(|write| match write {
                SubtreeWrite::Drop { subtree } => format!("drop {}", name(*subtree)),
                SubtreeWrite::Clear { subtree } => format!("clear {}", name(*subtree)),
                SubtreeWrite::Enter {
                    subtree,
                    tree_type,
                    host,
                }
                | SubtreeWrite::EnterIfAbsent {
                    subtree,
                    tree_type,
                    host,
                } => {
                    let enter = match write {
                        SubtreeWrite::EnterIfAbsent { .. } => "enter if absent",
                        _ => "enter",
                    };
                    let host = host.map(
                        |HostPut {
                             subtree,
                             key,
                             position,
                         }| {
                            format!(" at {} of {}/{}", position, name(subtree), text(key))
                        },
                    );
                    let host = host.unwrap_or_default();
                    format!("{enter} {} as {}{host}", name(*subtree), text(tree_type))
                }
                // A staged batch states no subtree anew, and none of the
                // writes that say what a look found.
                SubtreeWrite::Convert {
                    subtree, tree_type, ..
                } => format!("convert {} to {}", name(*subtree), text(tree_type)),
                SubtreeWrite::DropUnder { subtree, host, .. } => {
                    let (at, key) = (name(host.subtree), text(host.key));
                    format!("drop {} under {at}/{key}", name(*subtree))
                }
                SubtreeWrite::DropIfEntered { subtree, host } => {
                    let (at, key) = (name(host.subtree), text(host.key));
                    format!("drop {} if under {at}/{key}", name(*subtree))
                }
                SubtreeWrite::RemoveIfEntered { subtree } => {
                    format!("remove under {} if entered", name(*subtree))
                }
                SubtreeWrite::NotUnder { subtree, host } => {
                    let (at, key) = (name(host.subtree), text(host.key));
                    format!("{} not under {at}/{key}", name(*subtree))
                }
                SubtreeWrite::Move {
                    from,
                    to,
                    host,
                    below,
                    ..
                } => {
                    let below = below.iter().map(|(from, to)| (name(*from), name(*to)));
                    let below: String = below
                        .map(|(from, to)| format!(", {from} to {to}"))
                        .collect();
                    let (at, key) = (name(host.subtree), text(host.key));
                    let position = host.position;
                    let (from, to) = (name(*from), name(*to));
                    format!("move {from} to {to} at {position} of {at}/{key}{below}")
                }
            })
            .collect(),
        merks: (batch.merks.iter())
            .map(|merk| Merk {
                subtree: name(merk.subtree),
                nodes: (merk.nodes.iter())
                    .map(|node| {
                        let value = match node.value {
                            ValuePut::Keep => None,
                            ValuePut::Value(value) => Some(text(value)),
                        };
                        let hosted = match node.hosted {
                            Hosted::Keep => None,
                            Hosted::Set(hosted) => Some(hosted.iter().copied().map(name).collect()),
                        };
                        (text(node.key), value, hosted)
                    })
                    .collect(),
                removed: merk.removed.iter().map(|key| text(key)).collect(),
                root: match merk.root {
                    RootPut::Keep => None,
                    RootPut::Set(root) => Some(root.map(text)),
                },
                anchored: merk.anchored,
            })
            .collect(),
        records: (batch.records.iter())
            .map(|record| match record {
                RecordWrite::Put { unit, key, value } => {
                    format!(
                        "{} {}/{}={}",
                        unit.column,
                        name(unit.subtree),
                        text(key),
                        text(value)
                    )
                }
                RecordWrite::Delete { unit, key } => {
                    format!("{} {}/{} gone", unit.column, name(unit.subtree), text(key))
                }
                RecordWrite::DeleteRange { .. } => unreachable!("a batch stages no range"),
            })
            .collect(),
    })
}

fn resolved<C>(batch: StagedBatch<C>) -> Shape {
    shape(&batch.resolve().expect("no call is refused"))
}

fn put(batch: &StagedBatch, name: u8, node: &Rec, stated: bool) {
    batch.put_node(subtree(name), &node.put(stated, Hosted::Keep), ());
}

fn node(key: &str, value: Option<&str>) -> (String, Option<String>, Option<Vec<u8>>) {
    (key.into(), value.map(Into::into), None)
}

#[test]
fn a_node_that_keeps_its_value_keeps_the_one_the_batch_holds() {
    let batch = StagedBatch::new();
    batch.put_node(subtree(A), &leaf(b"k", b"first").put(true, Hosted::Keep), 1);
    batch.next_operation();
    // A later operation writes the node's hashes and leaves its value.
    let mut later = leaf(b"k", b"unused");
    later.kv_hash = [9; 32];
    batch.put_node(subtree(A), &later.put(false, Hosted::Keep), 7);
    let resolved = batch.resolve().expect("no call is refused");

    let outcomes: Vec<_> = resolved.outcomes().collect();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(*outcomes[0].tag, 7, "what the standing put attached");
    let Written::Node(held) = outcomes[0].write else {
        panic!("a node is held: {outcomes:?}");
    };
    assert_eq!(held.value.as_deref(), Some(&b"first"[..]));
    assert_eq!(held.kv_hash, [9; 32], "the hashes of the later put");
    assert_eq!(shape(&resolved).merks[0].nodes, [node("k", Some("first"))]);
}

#[test]
fn a_node_that_keeps_its_value_keeps_the_transactions_where_the_batch_holds_none() {
    let batch = StagedBatch::new();
    put(&batch, A, &leaf(b"k", b"unused"), false);
    batch.next_operation();
    put(&batch, A, &leaf(b"k", b"unused"), false);
    assert_eq!(resolved(batch).merks[0].nodes, [node("k", None)]);
}

#[test]
fn a_node_does_not_keep_the_value_of_a_node_an_earlier_operation_removed() {
    let refused = Err(BatchError::NoValue {
        subtree: subtree(A),
    });
    // The batch held the value, and an operation removed the node.
    let batch = StagedBatch::new();
    put(&batch, A, &leaf(b"k", b"value"), true);
    batch.next_operation();
    batch.delete_node(subtree(A), b"k", ());
    batch.next_operation();
    put(&batch, A, &leaf(b"k", b"unused"), false);
    assert_eq!(batch.resolve().map(|_| ()), refused);

    // The transaction holds the value, and an operation removed the node.
    let batch = StagedBatch::new();
    batch.delete_node(subtree(A), b"k", ());
    batch.next_operation();
    put(&batch, A, &leaf(b"k", b"unused"), false);
    assert_eq!(batch.resolve().map(|_| ()), refused);

    // A put that states its value is the node anew.
    let batch = StagedBatch::new();
    batch.delete_node(subtree(A), b"k", ());
    batch.next_operation();
    put(&batch, A, &leaf(b"k", b"anew"), true);
    assert_eq!(resolved(batch).merks[0].nodes, [node("k", Some("anew"))]);
}

#[test]
fn a_put_that_keeps_its_value_wins_over_a_delete_of_its_operation_in_both_orders() {
    for delete_first in [true, false] {
        // The node has a value in the batch before the operation.
        let batch = StagedBatch::new();
        put(&batch, A, &leaf(b"k", b"value"), true);
        batch.next_operation();
        match delete_first {
            true => {
                batch.delete_node(subtree(A), b"k", ());
                put(&batch, A, &leaf(b"k", b"unused"), false);
            }
            false => {
                put(&batch, A, &leaf(b"k", b"unused"), false);
                batch.delete_node(subtree(A), b"k", ());
            }
        }
        let merk = &resolved(batch).merks[0];
        assert_eq!(merk.nodes, [node("k", Some("value"))], "{delete_first}");
        assert_eq!(merk.removed, [""; 0]);

        // The node has its value in the transaction.
        let batch = StagedBatch::new();
        match delete_first {
            true => {
                batch.delete_node(subtree(A), b"k", ());
                put(&batch, A, &leaf(b"k", b"unused"), false);
            }
            false => {
                put(&batch, A, &leaf(b"k", b"unused"), false);
                batch.delete_node(subtree(A), b"k", ());
            }
        }
        assert_eq!(resolved(batch).merks[0].nodes, [node("k", None)]);

        // An earlier operation removed the node: the delete of this one
        // does not give it a value.
        let batch = StagedBatch::new();
        batch.delete_node(subtree(A), b"k", ());
        batch.next_operation();
        match delete_first {
            true => {
                batch.delete_node(subtree(A), b"k", ());
                put(&batch, A, &leaf(b"k", b"unused"), false);
            }
            false => {
                put(&batch, A, &leaf(b"k", b"unused"), false);
                batch.delete_node(subtree(A), b"k", ());
            }
        }
        let refused = BatchError::NoValue {
            subtree: subtree(A),
        };
        assert_eq!(batch.resolve().map(|_| ()), Err(refused), "{delete_first}");
    }
}

#[test]
fn the_root_of_a_merk_is_held_as_a_record_is() {
    let rooted = |calls: &dyn Fn(&StagedBatch)| {
        let batch = StagedBatch::new();
        put(&batch, A, &leaf(b"a", b"1"), true);
        calls(&batch);
        resolved(batch).merks[0].root.clone()
    };
    // Nothing is said of the root: it is the node it was.
    assert_eq!(rooted(&|_| ()), None);
    // Inside one operation a root key stands against "no root", in both
    // orders, as the put of the node stands against its delete.
    let some = Some(Some("a".to_owned()));
    assert_eq!(
        rooted(&|batch| {
            batch.set_root(subtree(A), Some(b"a"));
            batch.set_root(subtree(A), None);
        }),
        some
    );
    assert_eq!(
        rooted(&|batch| {
            batch.set_root(subtree(A), None);
            batch.set_root(subtree(A), Some(b"a"));
        }),
        some
    );
    // A later operation's statement stands.
    assert_eq!(
        rooted(&|batch| {
            batch.set_root(subtree(A), Some(b"a"));
            batch.next_operation();
            batch.set_root(subtree(A), None);
        }),
        Some(None)
    );
    assert_eq!(
        rooted(&|batch| {
            batch.set_root(subtree(A), Some(b"a"));
            batch.set_root(subtree(A), Some(b"b"));
        }),
        Some(Some("b".to_owned()))
    );

    // A root of a Merk of which the batch holds no node says nothing.
    let batch = StagedBatch::<()>::new();
    batch.set_root(subtree(A), Some(b"a"));
    assert!(batch.is_empty());
    assert_eq!(resolved(batch), Shape::default());
}

#[test]
fn a_host_names_the_merks_it_hosts_and_a_merk_without_its_host_is_anchored() {
    let host = leaf(b"h", b"element");
    let inner = leaf(b"n", b"value");

    // The batch holds the host and the Merk: the host names it.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, A, &host, true);
    put(&batch, C, &inner, true);
    let write = resolved(batch);
    assert_eq!(write.merks[0].nodes[0].2, Some(vec![C]));
    assert!(!write.merks[0].anchored && !write.merks[1].anchored);

    // The put itself can say what its node hosts.
    let batch = StagedBatch::new();
    let hosted = [subtree(B), subtree(C)];
    batch.put_node(subtree(A), &host.put(true, Hosted::Set(&hosted)), ());
    put(&batch, C, &inner, true);
    let write = resolved(batch);
    assert_eq!(write.merks[0].nodes[0].2, Some(vec![B, C]));
    assert!(!write.merks[1].anchored);

    // The batch holds the Merk and not its host: the Merk is anchored.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, C, &inner, true);
    let write = resolved(batch);
    assert_eq!(write.merks.len(), 1);
    assert!(write.merks[0].anchored);

    // An element that the batch deletes hosts nothing, whatever was said
    // of it.
    for delete_first in [true, false] {
        let batch = StagedBatch::new();
        if delete_first {
            batch.delete_node(subtree(A), b"h", ());
        }
        batch.host(subtree(A), b"h", &[subtree(C)]);
        if !delete_first {
            batch.delete_node(subtree(A), b"h", ());
        }
        batch.enter(subtree(C), b"t");
        put(&batch, C, &inner, true);
        let write = resolved(batch);
        assert!(!write.merks[1].anchored, "{delete_first}");
        assert_eq!(write.subtrees, ["enter 3 as t"], "{delete_first}");
    }

    // Nothing is said: the node keeps what it hosts, and the Merk stands
    // alone.
    let batch = StagedBatch::new();
    put(&batch, A, &host, true);
    put(&batch, C, &inner, true);
    let write = resolved(batch);
    assert_eq!(write.merks[0].nodes[0].2, None);
    assert!(!write.merks[1].anchored);
}

#[test]
fn the_later_statement_of_what_an_element_hosts_stands() {
    let host = leaf(b"h", b"element");
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, A, &host, true);
    batch.next_operation();
    // The element hosts nothing any more, and the put says nothing of it.
    batch.host(subtree(A), b"h", &[]);
    put(&batch, A, &host, false);
    put(&batch, C, &leaf(b"n", b"value"), true);
    let write = resolved(batch);
    assert_eq!(
        write.merks[0].nodes,
        [("h".into(), Some("element".into()), Some(vec![]))]
    );
    assert!(!write.merks[1].anchored, "nothing hosts the Merk");

    // A subtree that two elements name hangs where the later one says.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.host(subtree(B), b"g", &[subtree(D), subtree(C)]);
    batch.enter(subtree(C), b"t");
    assert_eq!(resolved(batch).subtrees, ["enter 3 as t at 1 of 2/g"]);

    // A delete of the node that stands takes the statement with it: a
    // node that is put under the key afterwards hosts what it says.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, A, &host, true);
    batch.next_operation();
    batch.delete_node(subtree(A), b"h", ());
    batch.next_operation();
    put(&batch, A, &host, true);
    assert_eq!(resolved(batch).merks[0].nodes[0].2, None);

    // A delete that does not stand takes nothing.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, A, &host, true);
    batch.delete_node(subtree(A), b"h", ());
    assert_eq!(resolved(batch).merks[0].nodes[0].2, Some(vec![C]));
}

#[test]
fn a_delete_and_a_put_of_one_operation_leave_what_the_element_hosts() {
    let host = leaf(b"h", b"element");
    for delete_first in [true, false] {
        let batch = StagedBatch::new();
        batch.host(subtree(A), b"h", &[subtree(C)]);
        batch.enter(subtree(C), b"t");
        put(&batch, A, &host, true);
        fill(&batch, C, b"old");
        batch.next_operation();
        // A rebalancing writes the element again, and says nothing of what
        // it hosts.
        if delete_first {
            batch.delete_node(subtree(A), b"h", ());
        }
        put(&batch, A, &host, false);
        if !delete_first {
            batch.delete_node(subtree(A), b"h", ());
        }
        let write = resolved(batch);
        assert_eq!(
            write.subtrees,
            ["enter 3 as t at 0 of 1/h"],
            "{delete_first}"
        );
        let element = ("h".into(), Some("element".into()), Some(vec![C]));
        assert_eq!(write.merks[0].nodes, [element], "{delete_first}");
    }

    // The subtree still hangs below: a later drop above takes it.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, A, &host, true);
    fill(&batch, C, b"old");
    batch.next_operation();
    batch.delete_node(subtree(A), b"h", ());
    put(&batch, A, &host, false);
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    assert_eq!(resolved(batch).subtrees, ["drop 1"]);

    // A node that is put after an earlier operation deleted it is new: it
    // hosts nothing of what the element of before hosted.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, A, &host, true);
    batch.next_operation();
    batch.delete_node(subtree(A), b"h", ());
    batch.next_operation();
    batch.delete_node(subtree(A), b"h", ());
    put(&batch, A, &host, true);
    fill(&batch, C, b"left");
    batch.drop_subtree(subtree(A));
    let write = resolved(batch);
    assert_eq!(write.merks[0].nodes, [node("h", Some("element"))]);
    assert_eq!(write.merks[1].subtree, C, "nothing says it is below");
}

/// A resolved batch says whether a subtree leaves with it: by a drop or
/// by a clear, also where the same operation enters the subtree again.
#[test]
fn a_resolved_batch_says_whether_a_subtree_leaves_with_it() {
    let leaves = |calls: &dyn Fn(&StagedBatch)| {
        let batch = StagedBatch::new();
        calls(&batch);
        batch.resolve().expect("no call is refused").leaves()
    };
    assert!(!leaves(&|_| ()));
    assert!(!leaves(&|batch| fill(batch, A, b"v")));
    assert!(!leaves(&|batch| batch.enter_if_absent(subtree(A), b"t")));
    assert!(leaves(&|batch| batch.drop_subtree(subtree(A))));
    assert!(leaves(&|batch| batch.clear(subtree(A))));
    assert!(leaves(&|batch| {
        batch.drop_subtree(subtree(A));
        batch.enter_if_absent(subtree(A), b"t");
    }));
}

#[test]
fn the_calls_of_one_operation_on_a_subtree_come_to_the_same_in_any_order() {
    let shapes = |calls: [&dyn Fn(&StagedBatch); 2], before: &dyn Fn(&StagedBatch)| {
        let both = [[0, 1], [1, 0]].map(|order| {
            let batch = StagedBatch::new();
            before(&batch);
            batch.next_operation();
            for at in order {
                calls[at](&batch);
            }
            batch.resolve().map(|resolved| shape(&resolved))
        });
        let [one, other] = both;
        assert_eq!(one, other);
        one
    };
    let enter = |batch: &StagedBatch| batch.enter(subtree(A), b"t");
    let drop = |batch: &StagedBatch| batch.drop_subtree(subtree(A));
    let clear = |batch: &StagedBatch| batch.clear(subtree(A));
    let keep = |batch: &StagedBatch| put(batch, A, &leaf(b"n", b"unused"), false);

    // An earlier operation entered the subtree: the drop takes that enter
    // back, and the enter is of a new one.
    let write = shapes([&enter, &drop], &|batch| {
        batch.enter(subtree(A), b"first");
        fill(batch, A, b"old");
    });
    assert_eq!(
        write.expect("no call is refused").subtrees,
        ["enter 1 as t"]
    );
    // An earlier operation cleared it: the drop is of the subtree the
    // transaction has.
    let write = shapes([&enter, &drop], &|batch| batch.clear(subtree(A)));
    assert_eq!(
        write.expect("no call is refused").subtrees,
        ["drop 1", "enter 1 as t"]
    );
    let entered = BatchError::Entered {
        subtree: subtree(A),
    };
    assert_eq!(shapes([&enter, &clear], &|_| ()).err(), Some(entered));

    // A node of the operation keeps no value that the leave comes before.
    for leave in [&drop as &dyn Fn(&StagedBatch), &clear] {
        let write = shapes([&keep, leave], &|batch| fill(batch, A, b"old"));
        let write = write.expect("no call is refused");
        assert_eq!(write.merks[0].nodes, [node("n", None)]);
        assert_eq!(write.merks[0].root, None);
    }
}

#[test]
fn what_hangs_below_is_what_the_operation_and_those_before_it_said() {
    // The operation that drops says the element hosts nothing any more,
    // before the drop or after it.
    for host_first in [true, false] {
        let batch = StagedBatch::new();
        batch.host(subtree(A), b"h", &[subtree(B)]);
        batch.host(subtree(B), b"g", &[subtree(C)]);
        fill(&batch, B, b"old");
        fill(&batch, C, b"old");
        batch.next_operation();
        if host_first {
            batch.host(subtree(A), b"h", &[]);
        }
        batch.drop_subtree(subtree(A));
        // What hung below before the operation goes, whatever the
        // operation says hangs there now.
        assert_eq!(batch.len(), 0, "{host_first}");
        if !host_first {
            batch.host(subtree(A), b"h", &[]);
        }
        assert!(!batch.is_empty());
        let write = resolved(batch);
        assert_eq!(write.subtrees, ["drop 1"], "{host_first}");
    }

    // The operation that drops is the first to say what hangs below.
    for host_first in [true, false] {
        let batch = StagedBatch::new();
        fill(&batch, B, b"old");
        fill(&batch, C, b"old");
        batch.next_operation();
        if host_first {
            batch.host(subtree(A), b"h", &[subtree(B)]);
            batch.host(subtree(B), b"g", &[subtree(C)]);
        }
        batch.drop_subtree(subtree(A));
        // Asking in between does not change what the later calls come to.
        assert_eq!(batch.len(), if host_first { 0 } else { 8 }, "{host_first}");
        if !host_first {
            batch.host(subtree(B), b"g", &[subtree(C)]);
            assert_eq!(batch.len(), 8);
            batch.host(subtree(A), b"h", &[subtree(B)]);
        }
        assert_eq!(batch.len(), 0, "{host_first}");
        assert_eq!(resolved(batch).subtrees, ["drop 1"], "{host_first}");
    }

    // The subtrees that are entered come a host before what it hosts,
    // whatever the order of the enters.
    let batch = StagedBatch::<()>::new();
    batch.enter(subtree(C), b"t");
    batch.enter(subtree(D), b"t");
    batch.enter(subtree(B), b"t");
    batch.enter(subtree(A), b"t");
    batch.host(subtree(B), b"g", &[subtree(C)]);
    batch.host(subtree(A), b"h", &[subtree(B)]);
    assert_eq!(
        resolved(batch).subtrees,
        [
            "enter 4 as t",
            "enter 1 as t",
            "enter 2 as t at 0 of 1/h",
            "enter 3 as t at 0 of 2/g"
        ]
    );
}

#[test]
fn a_subtree_that_an_earlier_operation_dropped_goes_with_a_later_leave_above_it() {
    let host = leaf(b"h", b"element");
    // What the second operation does to the element that hosted the
    // subtree it drops, and how the third takes the subtree above.
    for (element, clear) in [(0, false), (1, false), (2, false), (0, true)] {
        let batch = StagedBatch::new();
        batch.host(subtree(A), b"h", &[subtree(C)]);
        put(&batch, A, &host, true);
        batch.next_operation();
        batch.drop_subtree(subtree(C));
        match element {
            0 => batch.delete_node(subtree(A), b"h", ()),
            1 => batch.host(subtree(A), b"h", &[]),
            _ => batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[])), ()),
        }
        batch.next_operation();
        match clear {
            false => batch.drop_subtree(subtree(A)),
            true => batch.clear(subtree(A)),
        }
        // The transaction has the dropped subtree below the one that
        // leaves, and takes one name for both.
        let left = match clear {
            false => "drop 1",
            true => "clear 1",
        };
        assert_eq!(resolved(batch).subtrees, [left], "{element} {clear}");
    }

    // Where the subtree hung goes with its drop into the batch it is
    // merged into.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, A, &host, true);
    batch.next_operation();
    batch.drop_subtree(subtree(C));
    batch.delete_node(subtree(A), b"h", ());
    let group = StagedBatch::new();
    group.merge_overwriting(batch.take_pending());
    group.drop_subtree(subtree(A));
    assert_eq!(resolved(group).subtrees, ["drop 1"]);

    // One operation drops both, in either order: the one above is named.
    for below_first in [true, false] {
        let batch = StagedBatch::<()>::new();
        batch.host(subtree(A), b"h", &[subtree(C)]);
        batch.next_operation();
        if below_first {
            batch.clear(subtree(C));
        }
        batch.drop_subtree(subtree(A));
        if !below_first {
            batch.clear(subtree(C));
        }
        assert!(!batch.is_empty());
        assert_eq!(resolved(batch).subtrees, ["drop 1"], "{below_first}");
    }

    // A batch that was never told where the subtree hung names both.
    let batch = StagedBatch::<()>::new();
    batch.drop_subtree(subtree(C));
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    assert_eq!(resolved(batch).subtrees, ["drop 1", "drop 3"]);
}

#[test]
fn a_leave_above_does_not_take_a_refusal_with_it() {
    // The second enter is one the transaction would refuse, and the drop
    // above comes after it.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.enter(subtree(C), b"t");
    batch.next_operation();
    batch.enter(subtree(C), b"t");
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    assert!(!batch.is_empty());
    let entered = BatchError::Entered {
        subtree: subtree(C),
    };
    assert_eq!(batch.resolve().map(|_| ()), Err(entered.clone()));

    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.drop_subtree(subtree(C));
    batch.next_operation();
    batch.clear(subtree(C));
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    let not_entered = BatchError::NotEntered {
        subtree: subtree(C),
    };
    assert_eq!(batch.resolve().map(|_| ()), Err(not_entered.clone()));

    // Nor a refusal of a call of its own operation.
    type Call = fn(&StagedBatch);
    let again: [(Call, Call, BatchError); 3] = [
        (
            |batch| batch.drop_subtree(subtree(C)),
            |batch| batch.drop_subtree(subtree(C)),
            not_entered.clone(),
        ),
        (
            |batch| batch.drop_subtree(subtree(C)),
            |batch| batch.clear(subtree(C)),
            not_entered,
        ),
        (
            |_| (),
            |batch| {
                batch.clear(subtree(C));
                batch.enter(subtree(C), b"t");
            },
            entered,
        ),
    ];
    for (before, with, refused) in again {
        for above_first in [true, false] {
            let batch = StagedBatch::new();
            batch.host(subtree(A), b"h", &[subtree(C)]);
            before(&batch);
            batch.next_operation();
            if above_first {
                batch.drop_subtree(subtree(A));
            }
            with(&batch);
            if !above_first {
                batch.drop_subtree(subtree(A));
            }
            assert!(!batch.is_empty());
            assert_eq!(
                batch.resolve().map(|_| ()),
                Err(refused.clone()),
                "{above_first}"
            );
        }
    }

    // An enter of the operation below the drop is of a new subtree.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.next_operation();
    batch.drop_subtree(subtree(C));
    batch.enter(subtree(C), b"t");
    batch.drop_subtree(subtree(A));
    batch.enter(subtree(A), b"t");
    // The operation says what the element of the new subtree hosts.
    batch.host(subtree(A), b"h", &[subtree(C)]);
    assert_eq!(
        resolved(batch).subtrees,
        ["drop 1", "enter 1 as t", "enter 3 as t at 0 of 1/h"]
    );
}

#[test]
fn a_node_that_is_put_after_its_delete_hosts_what_is_said_from_then_on() {
    let host = leaf(b"h", b"element");
    let batch = StagedBatch::new();
    batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[subtree(B)])), ());
    batch.next_operation();
    batch.delete_node(subtree(A), b"h", ());
    batch.next_operation();
    // Said of an element that is not there.
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.next_operation();
    put(&batch, A, &host, true);
    assert_eq!(resolved(batch).merks[0].nodes[0].2, None);

    // Said by the operation that puts the node, before the put or after.
    for host_first in [true, false] {
        let batch = StagedBatch::new();
        batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[subtree(B)])), ());
        batch.next_operation();
        batch.delete_node(subtree(A), b"h", ());
        batch.next_operation();
        if host_first {
            batch.host(subtree(A), b"h", &[subtree(C)]);
        }
        put(&batch, A, &host, true);
        if !host_first {
            batch.host(subtree(A), b"h", &[subtree(C)]);
        }
        assert_eq!(resolved(batch).merks[0].nodes[0].2, Some(vec![C]));
    }
}

#[test]
fn a_kept_value_is_asked_of_the_put_that_stands() {
    let staged = || {
        let batch = StagedBatch::new();
        put(&batch, A, &leaf(b"k", b"first"), true);
        batch.next_operation();
        batch.delete_node(subtree(A), b"k", ());
        batch.next_operation();
        put(&batch, A, &leaf(b"k", b"unused"), false);
        batch
    };
    let refused = BatchError::NoValue {
        subtree: subtree(A),
    };
    assert_eq!(staged().resolve().map(|_| ()), Err(refused.clone()));
    // Another put that keeps has no more to keep.
    let batch = staged();
    batch.next_operation();
    put(&batch, A, &leaf(b"k", b"unused"), false);
    assert_eq!(batch.resolve().map(|_| ()), Err(refused.clone()));
    // A delete of the operation does not give the node a value.
    let batch = staged();
    batch.delete_node(subtree(A), b"k", ());
    assert_eq!(batch.resolve().map(|_| ()), Err(refused.clone()));
    // A later put that states its value stands in its place, and so does
    // a later delete.
    let batch = staged();
    put(&batch, A, &leaf(b"k", b"stated"), true);
    assert_eq!(resolved(batch).merks[0].nodes, [node("k", Some("stated"))]);
    let batch = staged();
    batch.next_operation();
    batch.delete_node(subtree(A), b"k", ());
    assert_eq!(resolved(batch).merks[0].removed, ["k"]);
    // A batch that is refused refuses the batch it is merged into.
    let batch = StagedBatch::new();
    batch.merge_overwriting(staged());
    put(&batch, A, &leaf(b"k", b"stated"), true);
    assert_eq!(batch.resolve().map(|_| ()), Err(refused));
}

#[test]
fn what_a_merged_batch_said_of_an_element_it_deletes_is_not_merged() {
    let host = leaf(b"h", b"element");
    let batch = StagedBatch::new();
    put(&batch, A, &host, true);
    let other = StagedBatch::new();
    other.host(subtree(A), b"h", &[subtree(C)]);
    other.next_operation();
    other.delete_node(subtree(A), b"h", ());
    // The delete does not stand against the put of this operation, and
    // the element hosts what it hosted.
    batch.merge(other);
    assert_eq!(resolved(batch).merks[0].nodes, [node("h", Some("element"))]);
}

#[test]
fn an_entered_subtree_has_the_element_that_hosts_it_as_its_host() {
    let batch = StagedBatch::<()>::new();
    batch.enter(subtree(D), b"alone");
    batch.enter(subtree(C), b"tree");
    batch.host(subtree(A), b"h", &[subtree(B), subtree(C)]);
    batch.drop_subtree(subtree(B));
    assert!(!batch.is_empty());
    assert_eq!(batch.len(), 0);
    // The leaves come first, and the enters in the order they were made.
    assert_eq!(
        resolved(batch).subtrees,
        ["drop 2", "enter 4 as alone", "enter 3 as tree at 1 of 1/h"]
    );

    let batch = StagedBatch::<()>::new();
    let many = vec![subtree(C); 256];
    batch.host(subtree(A), b"h", &many);
    let refused = BatchError::TooManyHosted {
        subtree: subtree(A),
    };
    assert_eq!(batch.resolve().map(|_| ()), Err(refused));
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &many[..255]);
    batch.enter(subtree(C), b"t");
    assert_eq!(resolved(batch).subtrees, ["enter 3 as t at 254 of 1/h"]);
}

#[test]
fn a_resolved_batch_says_what_it_enters_and_what_it_holds_for_a_key() {
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &[subtree(B)]);
    batch.enter(subtree(B), b"tree");
    batch.enter(subtree(D), b"alone");
    put(&batch, A, &leaf(b"h", b"element"), true);
    put(&batch, A, &leaf(b"gone", b"1"), true);
    batch.put(unit(Column::Data, D), b"entry", b"1", ());
    batch.delete(unit(Column::Data, D), b"none", ());
    batch.put(unit(Column::Aux, A), b"aux", b"1", ());
    batch.next_operation();
    batch.delete_node(subtree(A), b"gone", ());
    let resolved = batch.resolve().expect("no call is refused");

    // Neither is under a subtree the batch enters: the order they were
    // made in.
    assert_eq!(resolved.enters(), [subtree(B), subtree(D)]);
    let held = |name: u8, key: &[u8]| resolved.held(subtree(name), key);
    let value = |name: u8, key: &[u8]| match held(name, key) {
        Some(Written::Node(node)) => node.value.as_deref().map(<[u8]>::to_vec),
        other => panic!("no node is held: {other:?}"),
    };
    assert_eq!(value(A, b"h").as_deref(), Some(&b"element"[..]));
    assert!(matches!(held(A, b"gone"), Some(Written::NodeRemoved)));
    assert!(matches!(held(D, b"entry"), Some(Written::Record(b"1"))));
    assert!(matches!(held(D, b"none"), Some(Written::Removed)));
    // Only the data column is looked in, and only the subtree named.
    assert!(held(A, b"aux").is_none());
    assert!(held(B, b"h").is_none());
    assert!(held(C, b"h").is_none());
}

/// Stages a write in every column of `name`, and a node with a root.
fn fill(batch: &StagedBatch, name: u8, value: &[u8]) {
    for column in [Column::Aux, Column::Roots, Column::Meta] {
        batch.put(unit(column, name), b"r", value, ());
    }
    put(batch, name, &leaf(b"n", value), true);
    batch.set_root(subtree(name), Some(b"n"));
}

#[test]
fn a_drop_takes_the_place_of_what_earlier_operations_staged_in_the_subtree() {
    let batch = StagedBatch::new();
    fill(&batch, A, b"old");
    fill(&batch, B, b"other");
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    let write = resolved(batch);
    assert_eq!(write.subtrees, ["drop 1"]);
    assert_eq!(write.merks.len(), 1);
    assert_eq!(write.merks[0].subtree, B);
    assert_eq!(
        write.records,
        ["meta 2/r=other", "aux 2/r=other", "roots 2/r=other"]
    );

    // What the operation of the drop stages stands, in both orders, and so
    // does what a later one stages: they are applied after the drop.
    for drop_first in [true, false] {
        let batch = StagedBatch::new();
        fill(&batch, A, b"old");
        batch.next_operation();
        if drop_first {
            batch.drop_subtree(subtree(A));
        }
        batch.enter(subtree(A), b"t");
        batch.put(unit(Column::Aux, A), b"r", b"new", ());
        if !drop_first {
            batch.drop_subtree(subtree(A));
        }
        batch.next_operation();
        put(&batch, A, &leaf(b"later", b"node"), true);
        let write = resolved(batch);
        assert_eq!(write.subtrees, ["drop 1", "enter 1 as t"], "{drop_first}");
        assert_eq!(write.records, ["aux 1/r=new"]);
        assert_eq!(write.merks[0].nodes, [node("later", Some("node"))]);
        assert_eq!(write.merks[0].root, None, "the root of before is gone");
    }
}

#[test]
fn a_clear_takes_the_data_column_and_leaves_the_other_records() {
    let batch = StagedBatch::new();
    fill(&batch, A, b"old");
    batch.next_operation();
    batch.clear(subtree(A));
    put(&batch, A, &leaf(b"new", b"node"), true);
    let write = resolved(batch);
    assert_eq!(write.subtrees, ["clear 1"]);
    assert_eq!(
        write.records,
        ["meta 1/r=old", "aux 1/r=old", "roots 1/r=old"]
    );
    assert_eq!(write.merks[0].nodes, [node("new", Some("node"))]);
    assert_eq!(write.merks[0].root, None);

    // A second clear by a later operation is the one clear, and a drop
    // after a clear is the drop.
    let batch = StagedBatch::<()>::new();
    batch.clear(subtree(A));
    batch.next_operation();
    batch.clear(subtree(A));
    assert_eq!(resolved(batch).subtrees, ["clear 1"]);
    let batch = StagedBatch::<()>::new();
    batch.clear(subtree(A));
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    assert_eq!(resolved(batch).subtrees, ["drop 1"]);
}

#[test]
fn a_drop_and_a_clear_take_what_the_batch_was_told_is_below() {
    for clear in [false, true] {
        let batch = StagedBatch::new();
        // B hangs in A, C in B, and D nowhere the batch was told of.
        batch.host(subtree(A), b"h", &[subtree(B)]);
        batch.host(subtree(B), b"g", &[subtree(C)]);
        batch.enter(subtree(C), b"new");
        batch.drop_subtree(subtree(B));
        batch.enter(subtree(B), b"again");
        for name in [B, C, D] {
            fill(&batch, name, b"old");
        }
        batch.next_operation();
        match clear {
            false => batch.drop_subtree(subtree(A)),
            true => batch.clear(subtree(A)),
        }
        let write = resolved(batch);
        let left = match clear {
            false => "drop 1",
            true => "clear 1",
        };
        // Neither the drop of B nor an enter below is named beside it.
        assert_eq!(write.subtrees, [left], "{clear}");
        assert_eq!(
            write.records,
            ["meta 4/r=old", "aux 4/r=old", "roots 4/r=old"]
        );
        assert_eq!(write.merks.len(), 1);
        assert_eq!(write.merks[0].subtree, D);
    }

    // What the operation that drops stages below stands: it enters what
    // it writes there.
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(B)]);
    fill(&batch, B, b"old");
    batch.next_operation();
    batch.host(subtree(A), b"h", &[subtree(B)]);
    batch.enter(subtree(A), b"again");
    batch.enter(subtree(B), b"again");
    batch.put(unit(Column::Aux, B), b"r", b"new", ());
    batch.drop_subtree(subtree(A));
    let write = resolved(batch);
    assert_eq!(
        write.subtrees,
        ["drop 1", "enter 1 as again", "enter 2 as again at 0 of 1/h"]
    );
    assert_eq!(write.records, ["aux 2/r=new"]);
    assert!(write.merks.is_empty());
}

#[test]
fn a_drop_takes_back_the_enter_of_an_earlier_operation() {
    let batch = StagedBatch::new();
    batch.enter(subtree(A), b"t");
    fill(&batch, A, b"new");
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    // The transaction never sees the subtree.
    assert!(batch.is_empty());
    assert_eq!(resolved(batch), Shape::default());

    // The subtree the transaction has stays dropped.
    let batch = StagedBatch::new();
    batch.drop_subtree(subtree(A));
    batch.enter(subtree(A), b"t");
    batch.next_operation();
    fill(&batch, A, b"new");
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    assert_eq!(resolved(batch).subtrees, ["drop 1"]);

    // A clear of the batch's own subtree leaves its enter, and what the
    // subtree holds beside its nodes.
    let batch = StagedBatch::new();
    batch.enter(subtree(A), b"t");
    fill(&batch, A, b"new");
    batch.next_operation();
    batch.clear(subtree(A));
    let write = resolved(batch);
    assert_eq!(write.subtrees, ["enter 1 as t"]);
    assert_eq!(write.records.len(), 3);
    assert!(write.merks.is_empty());
}

#[test]
fn a_call_that_no_outcome_can_stand_for_refuses_the_batch() {
    type Calls = Vec<fn(&StagedBatch)>;
    let enter: fn(&StagedBatch) = |batch| batch.enter(subtree(A), b"t");
    let drop: fn(&StagedBatch) = |batch| batch.drop_subtree(subtree(A));
    let clear: fn(&StagedBatch) = |batch| batch.clear(subtree(A));
    let next: fn(&StagedBatch) = StagedBatch::next_operation;
    let twice = BatchError::SubtreeTwice {
        subtree: subtree(A),
    };
    let entered = BatchError::Entered {
        subtree: subtree(A),
    };
    let not_entered = BatchError::NotEntered {
        subtree: subtree(A),
    };
    let cases: [(Calls, BatchError); 11] = [
        (vec![enter, enter], twice.clone()),
        (vec![enter, next, enter], entered.clone()),
        (vec![clear, enter], entered.clone()),
        (vec![clear, next, enter], entered.clone()),
        (vec![enter, clear], entered),
        (vec![drop, drop], twice.clone()),
        (vec![clear, clear], twice.clone()),
        (vec![drop, clear], twice.clone()),
        (vec![clear, drop], twice.clone()),
        (vec![drop, next, drop], not_entered.clone()),
        (vec![drop, next, clear], not_entered),
    ];
    for (case, (calls, refused)) in cases.into_iter().enumerate() {
        let batch = StagedBatch::new();
        for call in calls {
            call(&batch);
        }
        // Whatever follows, the first refusal is the batch's.
        batch.next_operation();
        batch.enter(subtree(A), b"t");
        batch.put(unit(Column::Aux, B), b"k", b"v", ());
        assert!(!batch.is_empty());
        assert_eq!(batch.resolve().map(|_| ()), Err(refused), "case {case}");
    }

    // The refusal goes where the calls go.
    let batch = StagedBatch::<()>::new();
    batch.drop_subtree(subtree(A));
    batch.drop_subtree(subtree(A));
    let taken = batch.take_pending();
    assert!(batch.is_empty());
    let into = StagedBatch::new();
    into.merge_overwriting(taken);
    assert_eq!(into.resolve().map(|_| ()), Err(twice));
}

#[test]
fn a_batch_that_is_merged_is_an_operation_of_the_batch_it_is_merged_into() {
    let later = || {
        let other = StagedBatch::new();
        // The other batch keeps the value of a node, removes one, drops a
        // subtree and says what an element hosts.
        put(&other, A, &leaf(b"kept", b"unused"), false);
        other.delete_node(subtree(A), b"gone", ());
        other.set_root(subtree(A), Some(b"kept"));
        other.host(subtree(A), b"kept", &[subtree(C)]);
        other.drop_subtree(subtree(B));
        other.enter(subtree(D), b"t");
        other
    };
    let earlier = || {
        let batch = StagedBatch::new();
        put(&batch, A, &leaf(b"kept", b"value"), true);
        put(&batch, A, &leaf(b"gone", b"value"), true);
        batch.set_root(subtree(A), Some(b"gone"));
        fill(&batch, B, b"old");
        batch
    };

    // As a later operation, each of its outcomes takes the place of what
    // the batch holds.
    let batch = earlier();
    batch.merge_overwriting(later());
    // What follows is later still.
    batch.delete(unit(Column::Aux, D), b"k", ());
    batch.put(unit(Column::Aux, D), b"k", b"v", ());
    batch.delete_node(subtree(A), b"kept", ());
    let write = resolved(batch);
    assert_eq!(write.subtrees, ["drop 2", "enter 4 as t"]);
    assert_eq!(write.records, ["aux 4/k=v"]);
    assert_eq!(write.merks.len(), 1);
    assert!(write.merks[0].nodes.is_empty());
    assert_eq!(write.merks[0].removed, ["gone", "kept"]);
    assert_eq!(write.merks[0].root, Some(Some("kept".to_owned())));

    // In the same operation, its deletes and its drop leave what that
    // operation staged.
    let batch = earlier();
    batch.merge(later());
    let write = resolved(batch);
    assert_eq!(write.subtrees, ["drop 2", "enter 4 as t"]);
    assert_eq!(write.records.len(), 3, "the records of B stand");
    assert_eq!(write.merks[0].subtree, A);
    assert_eq!(
        write.merks[0].nodes,
        [
            node("gone", Some("value")),
            ("kept".into(), Some("value".into()), Some(vec![C])),
        ]
    );
    assert_eq!(write.merks[0].removed, [""; 0]);
    assert_eq!(write.merks[0].root, Some(Some("kept".to_owned())));
    assert_eq!(write.merks[1].subtree, B);
}

#[test]
fn what_is_taken_is_a_batch_of_its_own_and_merges_back_in_order() {
    let batch = StagedBatch::new();
    batch.put_node(subtree(A), &leaf(b"k", b"first").put(true, Hosted::Keep), 0);
    batch.put(unit(Column::Meta, A), b"m", b"1", 1);
    batch.next_operation();
    let first = batch.take_pending();
    assert!(batch.is_empty());
    assert_eq!((batch.len(), first.len()), (0, 2));

    // The batch goes on: its handles stage what a second part holds.
    batch.put_node(
        subtree(A),
        &leaf(b"k", b"unused").put(false, Hosted::Keep),
        2,
    );
    batch.delete(unit(Column::Meta, A), b"m", 3);
    let second = batch.take_pending();

    let group = StagedBatch::new();
    group.merge_overwriting(first);
    group.merge_overwriting(second);
    let resolved = group.resolve().expect("no call is refused");
    let tags: Vec<i32> = resolved.outcomes().map(|outcome| *outcome.tag).collect();
    assert_eq!(tags, [3, 2]);
    let write = shape(&resolved);
    assert_eq!(write.records, ["meta 1/m gone"]);
    assert_eq!(write.merks[0].nodes, [node("k", Some("first"))]);
}

#[test]
fn the_outcomes_are_in_the_order_a_storage_batch_is_charged_in() {
    let batch = StagedBatch::new();
    let mut tag = 0;
    let mut next = || {
        tag += 1;
        tag
    };
    // Staged against the order: the data column first, the later subtree
    // first, the later key first.
    for name in [B, A] {
        batch.put_node(
            subtree(name),
            &leaf(b"z", b"v").put(true, Hosted::Keep),
            next(),
        );
        batch.delete_node(subtree(name), b"y", next());
        for column in [Column::Roots, Column::Aux, Column::Meta] {
            batch.put(unit(column, name), b"b", b"v", next());
            batch.delete(unit(column, name), b"a", next());
        }
    }
    assert_eq!(batch.len(), 16);
    let mut resolved = batch.resolve().expect("no call is refused");
    assert_eq!(resolved.len(), 16);
    assert!(!resolved.is_empty());
    let order: Vec<String> = (resolved.outcomes())
        .map(|outcome| {
            let key = text(outcome.key);
            format!(
                "{} {}/{key}",
                outcome.unit.column,
                name(outcome.unit.subtree)
            )
        })
        .collect();
    let expected: Vec<String> = (["meta", "aux", "roots"].iter())
        .flat_map(|column| {
            [A, B].into_iter().flat_map(move |name| {
                ["a", "b"]
                    .into_iter()
                    .map(move |key| format!("{column} {name}/{key}"))
            })
        })
        .chain(["data 1/y", "data 1/z", "data 2/y", "data 2/z"].map(String::from))
        .collect();
    assert_eq!(order, expected);
    let kinds: Vec<_> = (resolved.outcomes())
        .map(|outcome| match outcome.write {
            Written::Record(_) => 'r',
            Written::Node(_) => 'n',
            Written::Removed => '-',
            Written::NodeRemoved => 'x',
        })
        .collect();
    assert_eq!(kinds.iter().collect::<String>(), "-r-r-r-r-r-rxnxn");

    // What each call attached is reached in the same order.
    let tags: Vec<i32> = resolved.outcomes().map(|outcome| *outcome.tag).collect();
    for tag in resolved.tags_mut() {
        *tag *= 10;
    }
    let changed: Vec<i32> = resolved.outcomes().map(|outcome| *outcome.tag).collect();
    assert_eq!(changed, tags.iter().map(|tag| tag * 10).collect::<Vec<_>>());
}

#[test]
fn a_batch_and_what_it_holds_are_shown() {
    let batch = StagedBatch::new();
    assert_eq!(
        format!("{batch:?}"),
        "StagedBatch { outcomes: 0, subtrees: 0, refused: None }"
    );
    let (_, nodes) = tree(&items(&[(b"a", b"1"), (b"b", b"2")]));
    for node in nodes.values() {
        put(&batch, A, node, true);
    }
    batch.delete_node(subtree(A), b"c", ());
    batch.delete(unit(Column::Aux, A), b"\xff", ());
    batch.enter(subtree(A), b"t");
    assert_eq!(
        format!("{batch:?}"),
        "StagedBatch { outcomes: 4, subtrees: 1, refused: None }"
    );
    let resolved = batch.resolve().expect("no call is refused");
    let shown = format!("{resolved:?}");
    assert!(
        shown.starts_with("Resolved { leaves: [], enters: [Subtree(01010101..)]"),
        "{shown}"
    );
    let outcomes: Vec<String> = (resolved.outcomes())
        .map(|outcome| format!("{outcome:?}"))
        .collect();
    assert_eq!(
        outcomes[0],
        "DeleteAux { subtree: Subtree(01010101..), key: \"[hex: ff, str: \u{fffd}]\" }"
    );
    assert!(outcomes[1].starts_with(
        "Put { subtree: Subtree(01010101..), key: \"[hex: 61, str: a]\", node: StagedNode {"
    ));
    assert!(
        outcomes[2].contains("left: Some(StagedLink { key: [97]"),
        "{}",
        outcomes[2]
    );
    assert_eq!(
        outcomes[3],
        "Delete { subtree: Subtree(01010101..), key: \"[hex: 63, str: c]\" }"
    );
    // Two outcomes are equal where the key, the call and what it attached
    // are.
    let held: Vec<_> = resolved.outcomes().collect();
    let (a, b) = (held[1], held[2]);
    assert_ne!(a, b);
    let Written::Node(b_node) = b.write else {
        panic!("a node is held: {b:?}");
    };
    let as_a = crate::batch::Outcome {
        key: a.key,
        write: Written::Node(b_node),
        ..a
    };
    assert_ne!(as_a, a, "another node under the key");
    assert_eq!(
        crate::batch::Outcome {
            write: a.write,
            ..as_a
        },
        a
    );

    let batch = StagedBatch::<()>::new();
    batch.drop_subtree(subtree(A));
    batch.drop_subtree(subtree(A));
    assert!(format!("{batch:?}")
        .ends_with("refused: Some(SubtreeTwice { subtree: Subtree(01010101..) }) }"));
    assert!(matches!(
        batch.resolve().map(|_| ()).map_err(TxnError::from),
        Err(TxnError::Batch(BatchError::SubtreeTwice { .. }))
    ));
}

#[test]
fn a_batch_is_shared_between_the_threads_of_its_handles() {
    fn shared<T: Send + Sync>() {}
    shared::<StagedBatch<Vec<u8>>>();
    shared::<Resolved<Vec<u8>>>();

    let batch = StagedBatch::new();
    std::thread::scope(|threads| {
        for name in [A, B, C, D] {
            let batch = &batch;
            threads.spawn(move || {
                for key in 0..50u8 {
                    batch.put(unit(Column::Aux, name), &[key], &[name], name);
                }
            });
        }
    });
    assert_eq!(batch.len(), 200);
}

/// What two enters of one subtree state where their tree types are `t`
/// and `u`.
fn other_type(entered: &[u8], stated: &[u8]) -> BatchError {
    BatchError::EnteredOtherwise {
        subtree: subtree(A),
        differs: Box::new(Differs::TreeType {
            entered: entered.into(),
            stated: stated.into(),
        }),
    }
}

/// An enter of a subtree that may be entered already
/// ([`StagedBatch::enter_if_absent`]): what it comes to with what the
/// batch says of the same subtree, in its operation and in those before.
#[test]
fn an_enter_if_absent_stands_with_what_the_batch_says_of_its_subtree() {
    type Call = fn(&StagedBatch);
    let enter: Call = |batch| batch.enter(subtree(A), b"t");
    let absent: Call = |batch| batch.enter_if_absent(subtree(A), b"t");
    let absent_other: Call = |batch| batch.enter_if_absent(subtree(A), b"u");
    let drop: Call = |batch| batch.drop_subtree(subtree(A));
    let clear: Call = |batch| batch.clear(subtree(A));
    let next: Call = StagedBatch::next_operation;
    let if_absent = "enter if absent 1 as t";
    let stands: [(Vec<Call>, &[&str]); 20] = [
        (vec![absent], &[if_absent]),
        // It is satisfied by an enter of its own kind, and by the enter of
        // a subtree that is new, which stands.
        (vec![absent, absent], &[if_absent]),
        (vec![absent, next, absent], &[if_absent]),
        (vec![enter, absent], &["enter 1 as t"]),
        (vec![absent, enter], &["enter 1 as t"]),
        (vec![enter, next, absent], &["enter 1 as t"]),
        // A drop comes first, in its operation and before it: the subtree
        // is absent then, and is entered.
        (vec![drop, absent], &["drop 1", if_absent]),
        (vec![absent, drop], &["drop 1", if_absent]),
        (vec![drop, next, absent], &["drop 1", if_absent]),
        // A clear leaves the transaction's subtree entered, and the
        // transaction says whether that is the subtree the enter states.
        (vec![clear, absent], &["clear 1", if_absent]),
        (vec![absent, clear], &["clear 1", if_absent]),
        (vec![clear, next, absent], &["clear 1", if_absent]),
        // The batch's own subtree: a clear leaves its enter, and a drop
        // takes it back, after which the subtree is entered if the
        // transaction does not have it.
        (vec![enter, next, clear, absent], &["enter 1 as t"]),
        (vec![enter, next, drop, absent], &[if_absent]),
        // A leave after an enter that came after a drop is of the batch's
        // own subtree: a drop takes the enter back, and a clear leaves it.
        (vec![drop, absent, next, drop], &["drop 1"]),
        (vec![drop, next, absent, next, drop], &["drop 1"]),
        (vec![drop, absent, next, clear], &["drop 1", if_absent]),
        // After a clear it is of the transaction's subtree.
        (vec![clear, absent, next, drop], &["drop 1"]),
        (vec![clear, next, absent, next, drop], &["drop 1"]),
        (vec![clear, absent, next, clear], &["clear 1", if_absent]),
    ];
    for (case, (calls, subtrees)) in stands.into_iter().enumerate() {
        let batch = StagedBatch::new();
        for call in calls {
            call(&batch);
        }
        assert!(!batch.is_empty(), "case {case}");
        let resolved = batch.resolve().expect("the calls stand");
        let entered = subtrees.iter().any(|write| write.starts_with("enter"));
        let enters = entered.then_some(subtree(A));
        assert_eq!(resolved.enters(), enters.as_slice(), "case {case}");
        assert!(!resolved.is_empty(), "case {case}");
        assert_eq!(shape(&resolved).subtrees, subtrees, "case {case}");
    }

    let entered = BatchError::Entered {
        subtree: subtree(A),
    };
    let leaves = BatchError::LeavesIfEntered {
        subtree: subtree(A),
    };
    let refused: [(Vec<Call>, BatchError); 10] = [
        // One subtree is entered as one tree type, in whichever way.
        (vec![absent, absent_other], other_type(b"t", b"u")),
        (vec![absent_other, absent], other_type(b"u", b"t")),
        (vec![enter, absent_other], other_type(b"t", b"u")),
        (vec![absent_other, enter], other_type(b"u", b"t")),
        (vec![absent, next, absent_other], other_type(b"t", b"u")),
        (vec![enter, next, absent_other], other_type(b"t", b"u")),
        // An enter of a new subtree finds the one that was entered if it
        // was absent, as it finds any other.
        (vec![absent, next, enter], entered.clone()),
        // Whether the transaction has a subtree that was entered if it
        // was absent, with no leave before, the batch does not know: it
        // has none to leave.
        (vec![absent, next, drop], leaves.clone()),
        (vec![absent, next, clear], leaves.clone()),
        (vec![absent, next, drop, absent], leaves),
    ];
    for (case, (calls, refusal)) in refused.into_iter().enumerate() {
        let batch = StagedBatch::new();
        for call in calls {
            call(&batch);
        }
        batch.next_operation();
        batch.put(unit(Column::Aux, B), b"k", b"v", ());
        assert!(!batch.is_empty(), "case {case}");
        assert_eq!(batch.resolve().map(|_| ()), Err(refusal), "case {case}");
    }
}

/// An enter of a subtree that may be entered is an enter where the batch
/// orders its enters, names their hosts, merges and drops.
#[test]
fn an_enter_if_absent_is_ordered_hosted_merged_and_dropped_as_an_enter_is() {
    // It has the host its subtree was said to hang on, and comes after
    // the enter of the subtree that host is in, of either kind.
    for outer_if_absent in [false, true] {
        let batch = StagedBatch::<()>::new();
        batch.host(subtree(A), b"h", &[subtree(B), subtree(C)]);
        batch.enter_if_absent(subtree(C), b"t");
        match outer_if_absent {
            true => batch.enter_if_absent(subtree(A), b"o"),
            false => batch.enter(subtree(A), b"o"),
        }
        let resolved = batch.resolve().expect("the calls stand");
        assert_eq!(resolved.enters(), [subtree(A), subtree(C)]);
        let outer = match outer_if_absent {
            true => "enter if absent 1 as o",
            false => "enter 1 as o",
        };
        let inner = "enter if absent 3 as t at 1 of 1/h";
        assert_eq!(shape(&resolved).subtrees, [outer, inner]);
    }

    // A batch that is merged brings its enter as the kind it is, into the
    // operation the other batch is in.
    let other = StagedBatch::<()>::new();
    other.enter_if_absent(subtree(D), b"t");
    other.enter(subtree(B), b"t");
    let into = StagedBatch::new();
    into.drop_subtree(subtree(D));
    into.merge(other);
    let resolved = into.resolve().expect("the calls stand");
    assert_eq!(
        shape(&resolved).subtrees,
        ["drop 4", "enter if absent 4 as t", "enter 2 as t"]
    );
    let other = StagedBatch::<()>::new();
    other.enter_if_absent(subtree(D), b"u");
    let into = StagedBatch::new();
    into.enter_if_absent(subtree(D), b"t");
    into.merge_overwriting(other);
    let differs = BatchError::EnteredOtherwise {
        subtree: subtree(D),
        differs: Box::new(Differs::TreeType {
            entered: (*b"t").into(),
            stated: (*b"u").into(),
        }),
    };
    assert_eq!(into.resolve().map(|_| ()), Err(differs));

    // A drop above takes the enter of an earlier operation with what that
    // operation staged in the subtree, whichever kind it is: the subtree
    // goes with the one it hangs in where the transaction has it, and is
    // never entered where it has not.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.enter_if_absent(subtree(C), b"t");
    batch.put(unit(Column::Aux, C), b"k", b"v", ());
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    let resolved = batch.resolve().expect("the calls stand");
    assert!(resolved.enters().is_empty());
    assert_eq!(resolved.len(), 0);
    assert_eq!(shape(&resolved).subtrees, ["drop 1"]);
    // One of the same operation is of the subtree after the drop.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.next_operation();
    batch.drop_subtree(subtree(A));
    batch.enter_if_absent(subtree(A), b"t");
    batch.enter_if_absent(subtree(C), b"t");
    batch.host(subtree(A), b"h", &[subtree(C)]);
    let resolved = batch.resolve().expect("the calls stand");
    assert_eq!(
        shape(&resolved).subtrees,
        [
            "drop 1",
            "enter if absent 1 as t",
            "enter if absent 3 as t at 0 of 1/h"
        ]
    );

    // The two together: an earlier operation entered the subtree, and the
    // operation of the drop above enters it again, as another tree. The
    // earlier enter went with the drop, with what its operation staged,
    // and the later one is held against nothing of it. That holds for
    // either kind of enter, in either operation.
    for (before, after) in [(true, true), (true, false), (false, true), (false, false)] {
        let enter = |batch: &StagedBatch, if_absent: bool, tree_type: &[u8]| match if_absent {
            true => batch.enter_if_absent(subtree(C), tree_type),
            false => batch.enter(subtree(C), tree_type),
        };
        let batch = StagedBatch::new();
        batch.host(subtree(A), b"h", &[subtree(C)]);
        enter(&batch, before, b"t");
        batch.put(unit(Column::Aux, C), b"k", b"v", ());
        batch.next_operation();
        batch.drop_subtree(subtree(A));
        batch.enter(subtree(A), b"t");
        batch.host(subtree(A), b"h", &[subtree(C)]);
        enter(&batch, after, b"u");
        batch.put(unit(Column::Aux, C), b"l", b"w", ());
        assert!(!batch.is_empty());
        let resolved = batch.resolve().expect("the calls stand");
        let kind = match after {
            true => "enter if absent",
            false => "enter",
        };
        assert_eq!(
            shape(&resolved).subtrees,
            [
                "drop 1".to_owned(),
                "enter 1 as t".to_owned(),
                format!("{kind} 3 as u at 0 of 1/h")
            ],
            "{before} {after}"
        );
        assert_eq!(resolved.len(), 1, "{before} {after}");
    }

    // The operation of the drop above can name a leave of the subtree
    // too. That leave goes with the drop and is not handed on. The enter
    // is held against it as in a batch that holds nothing else, and
    // against nothing of the operation before: a clear and an enter if it
    // is absent stand, as another tree as well.
    let replaced = |batch: &StagedBatch| {
        batch.next_operation();
        batch.drop_subtree(subtree(A));
        batch.enter(subtree(A), b"t");
        batch.host(subtree(A), b"h", &[subtree(C)]);
    };
    let batch = StagedBatch::new();
    batch.host(subtree(A), b"h", &[subtree(C)]);
    batch.enter(subtree(C), b"t");
    replaced(&batch);
    batch.clear(subtree(C));
    batch.enter_if_absent(subtree(C), b"u");
    let resolved = batch.resolve().expect("the calls stand");
    assert_eq!(
        shape(&resolved).subtrees,
        [
            "drop 1",
            "enter 1 as t",
            "enter if absent 3 as u at 0 of 1/h"
        ]
    );
    // A drop of a subtree that an earlier operation entered if it was
    // absent is refused where it would be handed on, and not below a
    // drop: with no enter after it, and with one of either kind.
    let batch = StagedBatch::<()>::new();
    batch.enter_if_absent(subtree(C), b"t");
    batch.next_operation();
    batch.drop_subtree(subtree(C));
    let leaves = BatchError::LeavesIfEntered {
        subtree: subtree(C),
    };
    assert_eq!(batch.resolve().map(|_| ()), Err(leaves));
    for again in [None, Some(false), Some(true)] {
        let batch = StagedBatch::new();
        batch.host(subtree(A), b"h", &[subtree(C)]);
        batch.enter_if_absent(subtree(C), b"t");
        replaced(&batch);
        batch.drop_subtree(subtree(C));
        let kind = match again {
            None => None,
            Some(false) => {
                batch.enter(subtree(C), b"u");
                Some("enter")
            }
            Some(true) => {
                batch.enter_if_absent(subtree(C), b"u");
                Some("enter if absent")
            }
        };
        let resolved = batch.resolve().expect("the calls stand");
        let mut expected = vec!["drop 1".to_owned(), "enter 1 as t".to_owned()];
        expected.extend(kind.map(|kind| format!("{kind} 3 as u at 0 of 1/h")));
        assert_eq!(shape(&resolved).subtrees, expected, "{again:?}");
    }
    // A leave of a subtree that an earlier operation dropped stays
    // refused below a drop, as with no enter after it
    // (`a_leave_above_does_not_take_a_refusal_with_it`), with an enter of
    // either kind too.
    for if_absent in [false, true] {
        let batch = StagedBatch::new();
        batch.host(subtree(A), b"h", &[subtree(C)]);
        batch.drop_subtree(subtree(C));
        replaced(&batch);
        batch.drop_subtree(subtree(C));
        match if_absent {
            true => batch.enter_if_absent(subtree(C), b"u"),
            false => batch.enter(subtree(C), b"u"),
        }
        let not_entered = BatchError::NotEntered {
            subtree: subtree(C),
        };
        assert_eq!(batch.resolve().map(|_| ()), Err(not_entered), "{if_absent}");
    }
}

/// The move of the subtree `B` to `C`, under the node `new` of `A`, which
/// takes the place of the node `old`, with the subtree `20` below it,
/// which goes to `30`.
fn move_b_to_c(batch: &StagedBatch) {
    let host = leaf(b"new", b"a tree");
    batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[subtree(C)])), ());
    batch.delete_node(subtree(A), b"old", ());
    batch.move_subtree(subtree(B), subtree(C), None, &[(subtree(20), subtree(30))]);
}

/// A staged batch states a move with the element that was said to host
/// the new prefix, holds the drop of what is entered there beside it, and
/// refuses whatever else it holds under a prefix a move names, from any
/// operation and in any order, and a move of another operation.
#[test]
fn a_staged_batch_holds_its_moves_and_nothing_else_of_what_they_name() {
    const MOVE: &str = "move 2 to 3 at 0 of 1/new, 20 to 30";
    let batch = StagedBatch::new();
    assert!(!batch.take_pending().resolve().expect("nothing").moves());
    move_b_to_c(&batch);
    assert!(!batch.is_empty());
    let moved = batch.resolve().expect("no call is refused");
    assert!(moved.moves() && !moved.leaves());
    assert_eq!(shape(&moved).subtrees, [MOVE]);
    // A move that is merged into another batch is a move of that one.
    let (batch, other) = (StagedBatch::new(), StagedBatch::new());
    move_b_to_c(&other);
    batch.merge(other);
    assert_eq!(resolved(batch).subtrees, [MOVE]);
    // What is entered where the subtree goes is dropped by the same
    // batch, before the move.
    for first in [false, true] {
        let batch = StagedBatch::new();
        if first {
            batch.drop_subtree(subtree(C));
            batch.next_operation();
        }
        move_b_to_c(&batch);
        if !first {
            batch.drop_subtree(subtree(C));
        }
        assert_eq!(resolved(batch).subtrees, ["drop 3", MOVE]);
    }

    // The batch was not told which element hosts the new prefix.
    let batch = StagedBatch::<()>::new();
    batch.move_subtree(subtree(B), subtree(C), None, &[]);
    let subtree_b = subtree(B);
    assert_eq!(
        batch.resolve().expect_err("no host"),
        BatchError::MoveHost { subtree: subtree_b }
    );

    type Call = fn(&StagedBatch);
    let beside: [(&str, u8, Call); 7] = [
        ("a record under the source", B, |batch| {
            batch.put(unit(Column::Aux, B), b"k", b"v", ())
        }),
        ("a node under the target", C, |batch| {
            put(batch, C, &leaf(b"k", b"v"), true)
        }),
        ("a root below the target", 30, |batch| {
            batch.set_root(subtree(30), Some(b"k"))
        }),
        ("an enter below the source", 20, |batch| {
            batch.enter(subtree(20), b"tree")
        }),
        ("an enter if absent of the target", C, |batch| {
            batch.enter_if_absent(subtree(C), b"tree")
        }),
        ("a clear of the target", C, |batch| batch.clear(subtree(C))),
        ("a drop of the source", B, |batch| {
            batch.drop_subtree(subtree(B))
        }),
    ];
    for (what, named, call) in beside {
        // In the operation of the move, before it and after it, and in
        // an operation before it and one after it.
        for order in 0..4 {
            let batch = StagedBatch::new();
            if order < 2 {
                call(&batch);
            }
            if order == 0 {
                batch.next_operation();
            }
            move_b_to_c(&batch);
            if order == 3 {
                batch.next_operation();
            }
            if order >= 2 {
                call(&batch);
            }
            let subtree = subtree(named);
            let refused = batch.resolve().expect_err(what);
            assert_eq!(refused, BatchError::Moved { subtree }, "{what}, {order}");
        }
    }
    // The moves of a batch are of one operation.
    let batch = StagedBatch::new();
    move_b_to_c(&batch);
    batch.next_operation();
    let host = leaf(b"newer", b"a tree");
    batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[subtree(D)])), ());
    batch.move_subtree(subtree(40), subtree(D), None, &[]);
    let moved = subtree(40);
    assert_eq!(
        batch.resolve().expect_err("two operations"),
        BatchError::Moved { subtree: moved }
    );

    // A drop of what is entered where the subtree goes that a later
    // operation states would be of the subtree that was moved there: the
    // batch applies its drops before its moves, so it holds none.
    let batch = StagedBatch::new();
    move_b_to_c(&batch);
    batch.next_operation();
    batch.drop_subtree(subtree(C));
    assert_eq!(
        batch.resolve().expect_err("a drop after the move"),
        BatchError::Moved {
            subtree: subtree(C)
        }
    );

    // The moves of one operation name a prefix once on each side: a
    // subtree is moved once, a pair of another move among them, and one
    // subtree goes to a prefix.
    let twice: [(&str, u8, Call); 3] = [
        ("one subtree is moved twice", B, |batch| {
            let host = leaf(b"newer", b"a tree");
            batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[subtree(D)])), ());
            batch.move_subtree(subtree(B), subtree(D), None, &[]);
        }),
        ("a subtree that a pair moves is moved again", 20, |batch| {
            let host = leaf(b"newer", b"a tree");
            batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[subtree(D)])), ());
            batch.move_subtree(subtree(20), subtree(D), None, &[]);
        }),
        ("two subtrees go to one prefix", C, |batch| {
            batch.move_subtree(subtree(40), subtree(C), None, &[]);
        }),
    ];
    for (what, named, again) in twice {
        let batch = StagedBatch::new();
        move_b_to_c(&batch);
        again(&batch);
        let subtree = subtree(named);
        let refused = batch.resolve().expect_err(what);
        assert_eq!(refused, BatchError::SubtreeTwice { subtree }, "{what}");
    }

    // The element that hosts the new prefix is the one the batch says
    // hosts it when it is resolved, whichever operation said so.
    let batch = StagedBatch::new();
    let host = leaf(b"new", b"a tree");
    batch.put_node(subtree(A), &host.put(true, Hosted::Set(&[subtree(C)])), ());
    batch.delete_node(subtree(A), b"old", ());
    batch.next_operation();
    batch.move_subtree(subtree(B), subtree(C), None, &[(subtree(20), subtree(30))]);
    assert_eq!(resolved(batch).subtrees, [MOVE]);

    // A batch that was told the host and does not put it holds a move
    // and nothing else. It is not empty: the transaction refuses the
    // move for the node that is not put.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(A), b"new", &[subtree(C)]);
    batch.move_subtree(subtree(B), subtree(C), None, &[]);
    let told = batch.resolve().expect("the host was told");
    assert!(told.moves() && !told.is_empty());
    assert_eq!(told.len(), 0);
    assert_eq!(shape(&told).subtrees, ["move 2 to 3 at 0 of 1/new"]);
}
