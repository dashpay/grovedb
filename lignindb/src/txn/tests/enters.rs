//! A subtree that is entered if it is absent
//! ([`SubtreeWrite::EnterIfAbsent`]): what the frames make of it when the
//! batch is applied and when the transaction's writes are laid out again,
//! and what a transaction that has no frames is shown of the subtree.

use super::{
    model::{self, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, ENTERED, TREE_TYPE},
    subtrees::{
        alone, aux, both, enter, hosted, inner_batch, model_root, planted, same, shown, subtrees,
        INNER, OUTER,
    },
    *,
};
use crate::txn::{Differs, HostName, HostPut, RecordWrite, SubtreeWrite};

/// Subtrees that no store of these tests has entered.
const NEW: u8 = ENTERED + 30;
const OTHER: u8 = ENTERED + 31;
const THIRD: u8 = ENTERED + 32;

fn if_absent(name: u8) -> OwnedSubtree {
    OwnedSubtree::EnterIfAbsent(model::subtree(name), None)
}

fn if_absent_under(name: u8, host: u8, key: &[u8]) -> OwnedSubtree {
    let host = (model::subtree(host), key.to_vec(), 0);
    OwnedSubtree::EnterIfAbsent(model::subtree(name), Some(host))
}

/// A batch that enters `name` if it is absent and puts an aux record
/// under `key` there.
fn entering(name: u8, key: &[u8]) -> OwnedBatch {
    OwnedBatch {
        subtrees: vec![if_absent(name)],
        records: vec![put(aux(name), key, key)],
        ..OwnedBatch::default()
    }
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn hex(bytes: &[u8]) -> String {
    let digits: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("0x{digits}")
}

/// Applies, through a scope of `tx` that is rolled back, a batch that
/// enters `subtree` if it is absent, as a tree of `tree_type` under
/// `host`, and puts a record there.
fn stated(
    tx: &Tx<'_>,
    subtree: Subtree,
    tree_type: &[u8],
    host: Option<HostPut<'_>>,
) -> Result<Applied, TxnError> {
    let enter = [SubtreeWrite::EnterIfAbsent {
        subtree,
        tree_type,
        host,
    }];
    let record = [RecordWrite::put(Column::Aux, subtree, b"k", b"v")];
    let scope = tx.txn.scope().expect("a scope");
    let applied = scope.apply(&Batch {
        subtrees: &enter,
        records: &record,
        ..Batch::default()
    });
    scope.rollback();
    applied
}

fn refusal(result: Result<impl std::fmt::Debug, TxnError>) -> BatchError {
    match result {
        Err(TxnError::Batch(error)) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    }
}

/// The two halves of the write: a subtree that is absent is entered with
/// what the write states, and one that is entered keeps its entry, its
/// nodes and its records, at the cost of no page.
#[test]
fn an_enter_if_absent_enters_a_subtree_that_is_absent_and_leaves_one_that_is_entered() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let empty = Some((None, TREE_TYPE.to_vec(), None));

    let tx = world.begin();
    assert_eq!(shown(&tx, NEW), None);
    (world.apply(&tx, &entering(NEW, b"a"))).expect("the subtree is entered");
    assert_eq!(shown(&tx, NEW), empty);
    // The transaction's own subtree is entered: the second enter finds it.
    (world.apply(&tx, &entering(NEW, b"b"))).expect("the subtree is found entered");
    assert_eq!(shown(&tx, NEW), empty);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));

    // A committed subtree with a Merk keeps its root, its nodes and its
    // records.
    let tx = world.begin();
    world.edit(&tx, NEW, |items| {
        for index in 0..7u8 {
            items.insert(vec![b'n', index], vec![index; 30]);
        }
    });
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    let before = shown(&tx, NEW);
    assert_eq!(before, alone(&world, &tx, NEW));
    assert!(model_root(&world, &tx, NEW).is_some());
    (world.apply(&tx, &subtrees(vec![if_absent(NEW)]))).expect("the subtree is found entered");
    let stats = tx.txn.stats();
    assert_eq!(
        (stats.tree_pages, stats.flat_pages),
        (0, 0),
        "no page is copied for a subtree that is found entered"
    );
    assert_eq!(shown(&tx, NEW), before);
    world.check(&tx);
    // A transaction that did nothing else commits, and leaves the subtree
    // as it was for every reader.
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(shown(&reader, NEW), before);

    // A hosted subtree is found by the element it hangs on.
    let tx = world.begin();
    planted(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    let before = shown(&tx, INNER);
    let found = OwnedBatch {
        subtrees: vec![if_absent_under(INNER, OUTER, b"host"), if_absent(OUTER)],
        records: vec![put(aux(INNER), b"more", b"1")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &found).expect("both are found entered");
    assert_eq!(shown(&tx, INNER), before);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A subtree that is entered with another tree type, or under another
/// element or at another place of it, is not the subtree the write
/// states: the batch is refused with both, and has changed nothing.
#[test]
fn an_enter_if_absent_of_a_subtree_that_is_entered_otherwise_is_refused_with_both() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(committed(world.commit(tx, &[])));

    let (outer, inner, one) = (
        model::subtree(OUTER),
        model::subtree(INNER),
        model::subtree(1),
    );
    let under = |subtree, key: &'static [u8], position| {
        Some(HostPut {
            subtree,
            key,
            position,
        })
    };
    let named = |subtree, key: &[u8], position| {
        Some(HostName {
            subtree,
            key: key.into(),
            position,
        })
    };
    let other_type = |stated: &[u8]| Differs::TreeType {
        entered: TREE_TYPE.into(),
        stated: stated.into(),
    };
    let held = named(outer, b"host", 0);
    let other_host = |entered: &Option<HostName>, stated: Option<HostName>| Differs::Host {
        entered: entered.clone(),
        stated,
    };
    type Case<'a> = (Subtree, &'a [u8], Option<HostPut<'a>>, Differs);
    let cases: Vec<Case<'_>> = vec![
        (
            inner,
            b"other",
            under(outer, b"host", 0),
            other_type(b"other"),
        ),
        (inner, b"", under(outer, b"host", 0), other_type(b"")),
        // The tree types are told where both differ.
        (inner, b"other", None, other_type(b"other")),
        (inner, TREE_TYPE, None, other_host(&held, None)),
        (
            inner,
            TREE_TYPE,
            under(outer, b"left", 0),
            other_host(&held, named(outer, b"left", 0)),
        ),
        (
            inner,
            TREE_TYPE,
            under(outer, b"host", 1),
            other_host(&held, named(outer, b"host", 1)),
        ),
        (
            inner,
            TREE_TYPE,
            under(one, b"host", 0),
            other_host(&held, named(one, b"host", 0)),
        ),
        (
            outer,
            TREE_TYPE,
            under(one, b"x", 0),
            other_host(&None, named(one, b"x", 0)),
        ),
    ];

    // A transaction that has frames and a write of its own keeps both.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(OUTER), b"own", b"1")]);
    // What the transaction holds, without the counts of its reads.
    let held = |tx: &Tx<'_>| TxnStats {
        keyed_reads: 0,
        handles_followed: 0,
        places_followed: 0,
        ..tx.txn.stats()
    };
    let before = held(&tx);
    assert!(before.base.is_some());
    for (case, (subtree, tree_type, host, differs)) in cases.into_iter().enumerate() {
        let error = refusal(stated(&tx, subtree, tree_type, host));
        let text = error.to_string();
        let both = match &differs {
            Differs::TreeType { entered, stated } => vec![hex(entered), hex(stated)],
            Differs::Host { entered, stated } => [entered, stated]
                .into_iter()
                .map(|host| match host {
                    Some(host) => host.to_string(),
                    None => "no element".to_owned(),
                })
                .collect(),
        };
        for side in &both {
            assert!(text.contains(side), "case {case}: {text} names {side}");
        }
        assert_ne!(both[0], both[1], "case {case}");
        let differs = Box::new(differs);
        assert_eq!(
            error,
            BatchError::EnteredOtherwise { subtree, differs },
            "case {case}"
        );
        assert_eq!(held(&tx), before, "case {case}: nothing has changed");
        world.check(&tx);
    }
    // A batch that enters two subtrees and is refused for the second has
    // entered neither.
    let two = [
        SubtreeWrite::EnterIfAbsent {
            subtree: model::subtree(NEW),
            tree_type: TREE_TYPE,
            host: None,
        },
        SubtreeWrite::EnterIfAbsent {
            subtree: inner,
            tree_type: b"other",
            host: under(outer, b"host", 0),
        },
    ];
    let scope = tx.txn.scope().expect("a scope");
    let error = refusal(scope.apply(&Batch {
        subtrees: &two,
        ..Batch::default()
    }));
    scope.rollback();
    assert!(
        matches!(&error, BatchError::EnteredOtherwise { subtree, .. } if *subtree == inner),
        "{error}"
    );
    assert_eq!(held(&tx), before, "nothing has changed");
    assert_eq!(shown(&tx, NEW), None);
    world.check(&tx);
    // What the entry has is no refusal.
    stated(&tx, inner, TREE_TYPE, under(outer, b"host", 0)).expect("the subtree it states");
    stated(&tx, outer, TREE_TYPE, None).expect("the subtree it states");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
}

/// The write is judged again where the transaction's writes are laid out
/// over a commit that landed beside it (WT7): it finds the subtree which
/// that commit entered, and the transaction commits. An enter of a subtree
/// that is new is refused there, as it was, and so is one that states
/// another tree type than the commit entered the subtree with.
#[test]
fn an_enter_if_absent_finds_the_subtree_that_a_commit_beside_it_entered() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let empty = Some((None, TREE_TYPE.to_vec(), None));

    let (first, second) = (world.begin(), world.begin());
    (world.apply(&first, &entering(NEW, b"first"))).expect("the subtree is entered");
    (world.apply(&second, &entering(NEW, b"second"))).expect("the subtree is entered");
    assert_eq!(shown(&first, NEW), empty);
    assert_eq!(shown(&second, NEW), empty);
    assert!(committed(world.commit(first, &[&second])));
    // The second is laid out over that commit, once: it reads the record
    // of the first beside its own, in the one subtree.
    world.check(&second);
    let stats = second.txn.stats();
    assert_eq!((stats.rebuilds, stats.overlapped), (1, 0));
    assert_eq!(stats.base, Some(world.latest()));
    assert_eq!(shown(&second, NEW), empty);
    assert!(committed(world.commit(second, &[])));
    world.check_committed();
    let reader = world.begin();
    for key in [&b"first"[..], b"second"] {
        let mut value = Vec::new();
        assert!(reader.txn.get(aux(NEW), key, &mut value).expect("a read"));
        assert_eq!(value, key);
    }
    drop(reader);

    // An enter of a subtree that is new cannot be laid out over a commit
    // that entered the subtree: the commit of its transaction is refused.
    let (first, second) = (world.begin(), world.begin());
    (world.apply(&first, &entering(OTHER, b"first"))).expect("the subtree is entered");
    let new = OwnedBatch {
        subtrees: vec![enter(OTHER)],
        records: vec![put(aux(OTHER), b"second", b"2")],
        ..OwnedBatch::default()
    };
    world.apply(&second, &new).expect("the subtree is entered");
    assert!(committed(world.commit(first, &[&second])));
    let entered = BatchError::Entered {
        subtree: model::subtree(OTHER),
    };
    assert_eq!(refusal(db.commit(&versions, &second.txn)), entered);
    second.txn.rollback().expect("it is rolled back");

    // A commit beside it that entered the subtree as another tree than
    // the write states: the two name one subtree as two. The transaction
    // cannot be laid out over that commit. It goes on, serving its records
    // by key, and reads the subtree as it is committed. Its commit is
    // refused with both tree types and leaves it as it was, and once it
    // has taken the enter back it commits.
    let (first, second) = (world.begin(), world.begin());
    (world.apply(&first, &entering(THIRD, b"first"))).expect("the subtree is entered");
    world.write(&second, vec![put(aux(1), b"kept", b"1")]);
    second.txn.set_savepoint().expect("no scope is open");
    let subtree = model::subtree(THIRD);
    let other = [SubtreeWrite::EnterIfAbsent {
        subtree,
        tree_type: b"other",
        host: None,
    }];
    let record = [RecordWrite::put(Column::Aux, subtree, b"second", b"2")];
    let scope = second.txn.scope().expect("a scope");
    let batch = Batch {
        subtrees: &other,
        records: &record,
        ..Batch::default()
    };
    scope.apply(&batch).expect("the subtree is entered");
    scope.merge();
    assert_eq!(shown(&second, THIRD), Some((None, b"other".to_vec(), None)));
    assert!(committed(world.commit(first, &[&second])));
    assert_eq!(shown(&second, THIRD), empty, "as it is committed");
    let stats = second.txn.stats();
    assert_eq!((stats.base, stats.overlapped), (None, 0), "no frames");
    let mut value = Vec::new();
    assert!((second.txn.get(aux(THIRD), b"first", &mut value)).expect("a read"));
    assert!((second.txn.get(aux(THIRD), b"second", &mut value)).expect("a read"));
    let differs = Box::new(Differs::TreeType {
        entered: TREE_TYPE.into(),
        stated: (*b"other").into(),
    });
    let refused = BatchError::EnteredOtherwise { subtree, differs };
    assert_eq!(refusal(db.commit(&versions, &second.txn)), refused);
    assert_eq!(refusal(db.commit(&versions, &second.txn)), refused);
    (second.txn.rollback_to_savepoint()).expect("a savepoint is set");
    assert!(!(second.txn.get(aux(THIRD), b"second", &mut value)).expect("a read"));
    world.check(&second);
    assert!(committed(world.commit(second, &[])));
    world.check_committed();
}

/// A rollback to a savepoint takes the enter back with its operation, and
/// a subtree that the same transaction dropped is absent when the write
/// is judged: it is entered as one that is new, and starts empty. A clear
/// keeps the subtree, and the write finds it.
#[test]
fn an_enter_if_absent_goes_with_a_rollback_and_comes_after_a_drop() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let empty = Some((None, TREE_TYPE.to_vec(), None));
    let data = model::unit(Column::Data, NEW);
    let meta = model::unit(Column::Meta, NEW);
    let tx = world.begin();
    let first = OwnedBatch {
        subtrees: vec![if_absent(NEW)],
        records: vec![
            put(aux(NEW), b"old", b"1"),
            put(meta, b"old", b"2"),
            put(data, b"old", b"3"),
        ],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &first).expect("the subtree is entered");
    assert!(committed(world.commit(tx, &[])));

    // The rollback takes the enter out, and what was written with it.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(NEW), b"kept", b"4")]);
    tx.txn.set_savepoint().expect("no scope is open");
    let writes = tx.model.borrow().writes.len();
    (world.apply(&tx, &entering(OTHER, b"a"))).expect("the subtree is entered");
    assert_eq!(shown(&tx, OTHER), empty);
    (tx.txn.rollback_to_savepoint()).expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(writes);
    assert_eq!(shown(&tx, OTHER), None);
    world.check(&tx);
    let into = OwnedBatch {
        records: vec![put(aux(OTHER), b"b", b"5")],
        ..OwnedBatch::default()
    };
    let not_entered = BatchError::NotEntered {
        subtree: model::subtree(OTHER),
    };
    assert_eq!(refusal(world.apply(&tx, &into)), not_entered);

    // After a drop of an earlier operation, and of its own batch, the
    // subtree is absent, and none of its records is read again.
    world
        .apply(
            &tx,
            &subtrees(vec![OwnedSubtree::Drop(model::subtree(NEW))]),
        )
        .expect("the subtree is dropped");
    assert_eq!(shown(&tx, NEW), None);
    (world.apply(&tx, &entering(NEW, b"new"))).expect("the subtree is entered");
    assert_eq!(shown(&tx, NEW), empty);
    world.check(&tx);
    let again = OwnedBatch {
        subtrees: vec![if_absent(NEW), OwnedSubtree::Drop(model::subtree(NEW))],
        records: vec![put(aux(NEW), b"newer", b"6"), put(data, b"flat", b"7")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &again).expect("the subtree is entered");
    assert_eq!(shown(&tx, NEW), empty);
    world.check(&tx);
    let mut value = Vec::new();
    for (unit, key) in [(aux(NEW), &b"old"[..]), (meta, b"old"), (aux(NEW), b"new")] {
        assert!(!tx.txn.get(unit, key, &mut value).expect("a read"));
    }

    // A clear keeps the subtree entered, with its records but the data.
    let cleared = OwnedBatch {
        subtrees: vec![OwnedSubtree::Clear(model::subtree(NEW)), if_absent(NEW)],
        records: vec![put(data, b"after", b"8")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &cleared).expect("the subtree is found");
    assert_eq!(shown(&tx, NEW), empty);
    assert!(tx.txn.get(aux(NEW), b"newer", &mut value).expect("a read"));
    assert!(!tx.txn.get(data, b"flat", &mut value).expect("a read"));
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A transaction that serves its records by key judges no write to a
/// subtree, and is shown every subtree as one with frames is: one that it
/// entered if it was absent as the committed subtree where there is one,
/// with the root its own writes gave it, and as its own otherwise.
#[test]
fn a_transaction_with_no_frames_is_shown_an_enter_if_absent_as_one_with_frames_is() {
    let names = [1, OUTER, INNER, NEW, OTHER];
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    let below_outer = Some((model::subtree(OUTER), b"host".to_vec(), 0));
    let empty = Some((None, TREE_TYPE.to_vec(), None));

    // One keeps its frames. The other has a write that a commit outside
    // meets, and answers from its write set over the committed directory.
    let framed = world.begin();
    let by_key = world.begin();
    world.write(&by_key, vec![put(aux(1), b"met", b"1")]);
    let met = OwnedBatch {
        records: vec![put(aux(1), b"met", b"2")],
        ..OwnedBatch::default()
    };
    world.write_outside(&met, &[&framed, &by_key]);
    let txs = [&framed, &by_key];

    // A subtree that is absent is the transaction's own.
    both(&world, txs, |_, _| entering(NEW, b"a"));
    assert_eq!(shown(&framed, NEW), empty);
    same(&world, &framed, &by_key, &names, "an absent subtree");

    // A committed subtree keeps its entry, and has the root that a later
    // change set gives it.
    let root = model_root(&world, &framed, INNER);
    assert!(root.is_some());
    both(&world, txs, |_, _| OwnedBatch {
        subtrees: vec![if_absent_under(INNER, OUTER, b"host")],
        records: vec![put(aux(INNER), b"x", b"1")],
        ..OwnedBatch::default()
    });
    let inner = Some((root.clone(), TREE_TYPE.to_vec(), below_outer.clone()));
    assert_eq!(shown(&framed, INNER), inner);
    same(&world, &framed, &by_key, &names, "a committed subtree");
    both(&world, txs, |world, tx| {
        let root = model_root(world, tx, INNER).expect("a root");
        inner_batch(world, tx, |items| {
            items.remove(&root);
            for index in 0..9u8 {
                items.insert(vec![b'n', index], vec![index; 30]);
            }
        })
    });
    assert_ne!(model_root(&world, &framed, INNER), root);
    same(
        &world,
        &framed,
        &by_key,
        &names,
        "a committed subtree, written",
    );

    // The transaction's own subtree, of either kind of enter, with the
    // root it gave it.
    both(&world, txs, |_, _| subtrees(vec![enter(OTHER)]));
    both(&world, txs, |world, tx| OwnedBatch {
        merks: vec![world.merk(tx, OTHER, |items| {
            items.insert(b"node".to_vec(), vec![5; 20]);
        })],
        ..OwnedBatch::default()
    });
    both(&world, txs, |_, _| entering(OTHER, b"b"));
    assert_eq!(shown(&framed, OTHER), alone(&world, &framed, OTHER));
    assert!(model_root(&world, &framed, OTHER).is_some());
    same(&world, &framed, &by_key, &names, "its own subtree");
    both(&world, txs, |world, tx| OwnedBatch {
        merks: vec![world.merk(tx, NEW, |items| {
            items.insert(b"node".to_vec(), vec![6; 20]);
        })],
        subtrees: vec![if_absent(NEW)],
        ..OwnedBatch::default()
    });
    assert_eq!(shown(&framed, NEW), alone(&world, &framed, NEW));
    assert!(model_root(&world, &framed, NEW).is_some());
    same(&world, &framed, &by_key, &names, "its own subtree, written");

    // A clear in the batch of the enter keeps the subtree, with no node.
    both(&world, txs, |_, _| {
        subtrees(vec![
            OwnedSubtree::Clear(model::subtree(NEW)),
            if_absent(NEW),
        ])
    });
    assert_eq!(shown(&framed, NEW), empty);
    same(&world, &framed, &by_key, &names, "a clear and an enter");

    // A drop above takes a committed subtree, and the enter after it, in
    // a later operation and in the batch of a drop, makes a new one.
    both(&world, txs, |_, _| {
        subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))])
    });
    assert_eq!((shown(&framed, OUTER), shown(&framed, INNER)), (None, None));
    same(&world, &framed, &by_key, &names, "a drop above");
    both(&world, txs, |_, _| entering(INNER, b"y"));
    assert_eq!(shown(&framed, INNER), empty);
    same(&world, &framed, &by_key, &names, "an enter after the drop");
    both(&world, txs, |_, _| OwnedBatch {
        subtrees: vec![OwnedSubtree::Drop(model::subtree(INNER)), if_absent(INNER)],
        records: vec![put(aux(INNER), b"z", b"2")],
        ..OwnedBatch::default()
    });
    assert_eq!(shown(&framed, INNER), empty);
    same(&world, &framed, &by_key, &names, "a drop and an enter");
    both(&world, txs, |_, _| {
        subtrees(vec![OwnedSubtree::Drop(model::subtree(INNER))])
    });
    assert_eq!(shown(&framed, INNER), None);
    same(
        &world,
        &framed,
        &by_key,
        &names,
        "a drop of its own subtree",
    );

    assert!(committed(world.commit(framed, &[&by_key])));
    assert_eq!(world.commit(by_key, &[]), Outcome::Conflict);
    world.check_committed();
}

/// A subtree that the transaction entered and wrote nothing into is
/// named by no drop. It went with a subtree above it that left since, by
/// where it hung then: also where the subtree its host was in is entered
/// again afterwards, elsewhere, by either kind of enter.
#[test]
fn a_subtree_that_went_with_a_drop_above_is_gone_however_its_host_is_entered_again() {
    const TOP: u8 = ENTERED + 35;
    const MIDDLE: u8 = ENTERED + 36;
    const LOW: u8 = ENTERED + 37;
    let names = [1, OUTER, INNER, TOP, MIDDLE, LOW];
    for anew in [false, true] {
        let (db, versions) = Db::new(128);
        let mut world = World::new(&db, &versions);
        let tx = world.begin();
        planted(&world, &tx);
        assert!(committed(world.commit(tx, &[])));
        let framed = world.begin();
        let by_key = world.begin();
        world.write(&by_key, vec![put(aux(1), b"met", b"1")]);
        let met = OwnedBatch {
            records: vec![put(aux(1), b"met", b"2")],
            ..OwnedBatch::default()
        };
        world.write_outside(&met, &[&framed, &by_key]);
        let txs = [&framed, &by_key];

        // Three subtrees, one in the other, and nothing written into the
        // innermost.
        both(&world, txs, |world, tx| {
            let mut top = world.merk(tx, TOP, |items| {
                items.insert(b"m".to_vec(), b"tree".to_vec());
            });
            (top.hosted).insert(b"m".to_vec(), vec![model::subtree(MIDDLE)]);
            let mut middle = world.merk(tx, MIDDLE, |items| {
                items.insert(b"l".to_vec(), b"tree".to_vec());
            });
            (middle.hosted).insert(b"l".to_vec(), vec![model::subtree(LOW)]);
            OwnedBatch {
                merks: vec![middle, top],
                subtrees: vec![
                    enter(TOP),
                    hosted(MIDDLE, TOP, b"m"),
                    hosted(LOW, MIDDLE, b"l"),
                ],
                ..OwnedBatch::default()
            }
        });
        let below_middle = Some((model::subtree(MIDDLE), b"l".to_vec(), 0));
        let low = Some((None, TREE_TYPE.to_vec(), below_middle));
        assert_eq!(shown(&framed, LOW), low);
        same(&world, &framed, &by_key, &names, "three subtrees");

        both(&world, txs, |_, _| {
            subtrees(vec![OwnedSubtree::Drop(model::subtree(TOP))])
        });
        assert_eq!(shown(&framed, LOW), None);
        same(&world, &framed, &by_key, &names, "the outermost is dropped");

        // The middle one is entered again, under an element of another
        // subtree.
        both(&world, txs, |_, _| {
            subtrees(vec![match anew {
                true => hosted(MIDDLE, OUTER, b"left"),
                false => if_absent_under(MIDDLE, OUTER, b"left"),
            }])
        });
        let below_outer = Some((model::subtree(OUTER), b"left".to_vec(), 0));
        let middle = Some((None, TREE_TYPE.to_vec(), below_outer));
        assert_eq!(shown(&framed, MIDDLE), middle, "{anew}");
        assert_eq!(shown(&framed, LOW), None, "{anew}");
        same(&world, &framed, &by_key, &names, "the middle one is new");
        (framed.txn.rollback()).expect("it is rolled back");
        (by_key.txn.rollback()).expect("it is rolled back");
    }
}

/// What a transaction with no frames makes of an enter if absent beside
/// drops and clears, against one with frames: a subtree that it found and
/// that went with a subtree above it afterwards, one that it enters in
/// the batch that drops the subtree above, one that it clears, and its
/// own subtree that went with a drop above and is entered again.
#[test]
fn a_transaction_with_no_frames_judges_an_enter_if_absent_by_the_drops_before_it() {
    const TOP: u8 = ENTERED + 35;
    const LOW: u8 = ENTERED + 37;
    let names = [1, OUTER, INNER, NEW, TOP, LOW];
    let below_outer = Some((model::subtree(OUTER), b"host".to_vec(), 0));
    let empty = Some((None, TREE_TYPE.to_vec(), None));
    for case in 0..4 {
        let (db, versions) = Db::new(128);
        let mut world = World::new(&db, &versions);
        let tx = world.begin();
        planted(&world, &tx);
        assert!(committed(world.commit(tx, &[])));
        let framed = world.begin();
        let by_key = world.begin();
        world.write(&by_key, vec![put(aux(1), b"met", b"1")]);
        let met = OwnedBatch {
            records: vec![put(aux(1), b"met", b"2")],
            ..OwnedBatch::default()
        };
        world.write_outside(&met, &[&framed, &by_key]);
        let txs = [&framed, &by_key];
        let drop_outer = || subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))]);
        match case {
            // A committed subtree that the enter found, with nothing
            // written into it, goes with the subtree above it, and is new
            // when it is entered after that.
            0 => {
                both(&world, txs, |_, _| {
                    subtrees(vec![if_absent_under(INNER, OUTER, b"host")])
                });
                let root = model_root(&world, &framed, INNER);
                let found = Some((root, TREE_TYPE.to_vec(), below_outer.clone()));
                assert_eq!(shown(&framed, INNER), found);
                same(&world, &framed, &by_key, &names, "found");
                both(&world, txs, |_, _| drop_outer());
                assert_eq!(shown(&framed, INNER), None);
                same(&world, &framed, &by_key, &names, "gone with the drop");
                both(&world, txs, |_, _| entering(INNER, b"new"));
                assert_eq!(shown(&framed, INNER), empty);
                same(&world, &framed, &by_key, &names, "entered after the drop");
            }
            // The enter in the batch of the drop above comes after it.
            1 => {
                both(&world, txs, |_, _| OwnedBatch {
                    subtrees: vec![if_absent(INNER), OwnedSubtree::Drop(model::subtree(OUTER))],
                    records: vec![put(aux(INNER), b"new", b"1")],
                    ..OwnedBatch::default()
                });
                assert_eq!(
                    (shown(&framed, OUTER), shown(&framed, INNER)),
                    (None, empty.clone())
                );
                same(&world, &framed, &by_key, &names, "with the drop above");
            }
            // A clear after the enter leaves the subtree it found, and its
            // own, with no node.
            2 => {
                both(&world, txs, |_, _| OwnedBatch {
                    subtrees: vec![if_absent_under(INNER, OUTER, b"host"), if_absent(NEW)],
                    ..OwnedBatch::default()
                });
                both(&world, txs, |_, _| {
                    subtrees(vec![
                        OwnedSubtree::Clear(model::subtree(INNER)),
                        OwnedSubtree::Clear(model::subtree(NEW)),
                    ])
                });
                let cleared = Some((None, TREE_TYPE.to_vec(), below_outer.clone()));
                assert_eq!(
                    (shown(&framed, INNER), shown(&framed, NEW)),
                    (cleared, empty.clone())
                );
                same(&world, &framed, &by_key, &names, "cleared");
            }
            // Its own subtree, with nothing written into it, went with the
            // drop of the one above, and is entered again.
            _ => {
                both(&world, txs, |world, tx| {
                    let mut top = world.merk(tx, TOP, |items| {
                        items.insert(b"l".to_vec(), b"tree".to_vec());
                    });
                    (top.hosted).insert(b"l".to_vec(), vec![model::subtree(LOW)]);
                    OwnedBatch {
                        merks: vec![top],
                        subtrees: vec![enter(TOP), hosted(LOW, TOP, b"l")],
                        ..OwnedBatch::default()
                    }
                });
                let below_top = Some((model::subtree(TOP), b"l".to_vec(), 0));
                let low = Some((None, TREE_TYPE.to_vec(), below_top));
                assert_eq!(shown(&framed, LOW), low);
                both(&world, txs, |_, _| {
                    subtrees(vec![OwnedSubtree::Drop(model::subtree(TOP))])
                });
                assert_eq!(shown(&framed, LOW), None);
                same(&world, &framed, &by_key, &names, "gone with its host");
                both(&world, txs, |_, _| entering(LOW, b"again"));
                assert_eq!(shown(&framed, LOW), empty);
                same(&world, &framed, &by_key, &names, "entered again");
            }
        }
        (framed.txn.rollback()).expect("it is rolled back");
        (by_key.txn.rollback()).expect("it is rolled back");
    }
}

/// What a subtree is for a transaction with no frames can rest on what
/// its host was, up a line of hosts: each subtree of this line is entered
/// as one that is new, then an unrelated subtree is cleared, and then the
/// same enter is stated if it is absent, so each of them is its own only
/// if the one above it had not left. The line is longer than any count
/// of answers held one inside another, and its last subtree is shown by
/// key as with frames.
#[test]
fn a_line_of_subtrees_one_in_the_other_is_shown_by_key_as_with_frames() {
    const DEPTH: u8 = 70;
    const FIRST: u8 = 100;
    const BESIDE: u8 = 5;
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let framed = world.begin();
    let by_key = world.begin();
    world.write(&by_key, vec![put(aux(1), b"met", b"1")]);
    let met = OwnedBatch {
        records: vec![put(aux(1), b"met", b"2")],
        ..OwnedBatch::default()
    };
    world.write_outside(&met, &[&framed, &by_key]);
    let txs = [&framed, &by_key];

    // The subtrees of the line are entered in every store of the tests,
    // with no host. They are dropped, and entered again one in the other.
    both(&world, txs, |_, _| {
        let line = (0..DEPTH).map(|level| OwnedSubtree::Drop(model::subtree(FIRST + level)));
        subtrees(line.collect())
    });
    for level in 0..DEPTH {
        let name = FIRST + level;
        both(&world, txs, |world, tx| match level {
            0 => subtrees(vec![enter(name)]),
            _ => {
                let mut above = world.merk(tx, name - 1, |items| {
                    items.insert(b"h".to_vec(), b"tree".to_vec());
                });
                (above.hosted).insert(b"h".to_vec(), vec![model::subtree(name)]);
                OwnedBatch {
                    merks: vec![above],
                    subtrees: vec![hosted(name, name - 1, b"h")],
                    ..OwnedBatch::default()
                }
            }
        });
        both(&world, txs, |_, _| {
            subtrees(vec![OwnedSubtree::Clear(model::subtree(BESIDE))])
        });
        both(&world, txs, |_, _| {
            subtrees(vec![match level {
                0 => if_absent(name),
                _ => if_absent_under(name, name - 1, b"h"),
            }])
        });
    }
    let last = FIRST + DEPTH - 1;
    let above = Some((model::subtree(last - 1), b"h".to_vec(), 0));
    assert_eq!(
        shown(&framed, last),
        Some((None, TREE_TYPE.to_vec(), above))
    );
    let names: Vec<u8> = (FIRST..FIRST + DEPTH).chain([BESIDE]).collect();
    same(&world, &framed, &by_key, &names, "a line of subtrees");
    (framed.txn.rollback()).expect("it is rolled back");
    (by_key.txn.rollback()).expect("it is rolled back");
}
