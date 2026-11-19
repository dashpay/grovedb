//! What a batch holds after a sequence of calls, and the write of a
//! transaction it resolves to. No store is needed here.

use super::{items, leaf, subtree, tree, unit, Rec};
use crate::{
    batch::{Resolved, StagedBatch, Written},
    txn::{
        BatchError, Column, HostPut, Hosted, RecordWrite, RootPut, Subtree, SubtreeWrite, TxnError,
        ValuePut,
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
struct Merk {
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
struct Shape {
    subtrees: Vec<String>,
    merks: Vec<Merk>,
    records: Vec<String>,
}

fn shape<C>(resolved: &Resolved<C>) -> Shape {
    resolved.with_batch(|batch| Shape {
        subtrees: (batch.subtrees.iter())
            .map(|write| match write {
                SubtreeWrite::Drop { subtree } => format!("drop {}", name(*subtree)),
                SubtreeWrite::Clear { subtree } => format!("clear {}", name(*subtree)),
                SubtreeWrite::Enter {
                    subtree,
                    tree_type,
                    host,
                } => {
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
                    format!("enter {} as {}{host}", name(*subtree), text(tree_type))
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

    // A delete of the host is no put of it.
    let batch = StagedBatch::new();
    batch.delete_node(subtree(A), b"h", ());
    batch.host(subtree(A), b"h", &[subtree(C)]);
    put(&batch, C, &inner, true);
    assert!(resolved(batch).merks[1].anchored);

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
        (vec![enter, enter], twice),
        (vec![enter, next, enter], entered),
        (vec![clear, enter], entered),
        (vec![clear, next, enter], entered),
        (vec![enter, clear], entered),
        (vec![drop, drop], twice),
        (vec![clear, clear], twice),
        (vec![drop, clear], twice),
        (vec![clear, drop], twice),
        (vec![drop, next, drop], not_entered),
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
    assert!(outcomes[1] != outcomes[2] && resolved.outcomes().next() == resolved.outcomes().next());

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
