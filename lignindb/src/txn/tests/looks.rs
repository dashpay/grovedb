//! What a writer found of a subtree, judged where its batch is laid out
//! ([`SubtreeWrite::DropUnder`], [`SubtreeWrite::DropIfEntered`],
//! [`SubtreeWrite::RemoveIfEntered`]): a commit beside the transaction
//! can be published between the writer's look at the directory and the
//! operation that takes its batch.

use lignin_tree::node::NodeHandle;

use super::{
    model::{self, OwnedBatch, OwnedMerk, OwnedSubtree},
    store::{Db, Outcome, ENTERED},
    subtrees::{aux, enter, hosted, inner_batch, planted, shown, subtrees, INNER, OUTER},
    *,
};

/// Subtrees that no store of these tests has entered.
const NEW: u8 = ENTERED + 40;
const DEEP: u8 = ENTERED + 41;

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn refusal(result: Result<Applied, TxnError>) -> BatchError {
    match result {
        Err(TxnError::Batch(error)) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    }
}

/// A drop of `name` if it is entered under the node `key` of `host`.
/// `named` says whether that is the node `name` hangs under where a node
/// of `host` hosts it.
fn drop_if(name: u8, host: u8, key: &[u8], named: bool) -> OwnedSubtree {
    OwnedSubtree::DropIfEntered {
        subtree: model::subtree(name),
        host: (model::subtree(host), key.to_vec()),
        named,
    }
}

fn drop_under(name: u8, host: u8, key: &[u8], read: Option<NodeHandle>) -> OwnedSubtree {
    OwnedSubtree::DropUnder {
        subtree: model::subtree(name),
        host: (model::subtree(host), key.to_vec()),
        read,
    }
}

/// The change set that takes the node `host` out of `OUTER`, as `tx`
/// reads the Merk.
fn unhosted<'s>(world: &World<'s>, tx: &Tx<'s>) -> OwnedMerk {
    world.merk(tx, OUTER, |items| drop(items.remove(&b"host"[..])))
}

/// A batch that takes the node `host` out of `OUTER` and states `drop`.
fn with_its_node<'s>(world: &World<'s>, tx: &Tx<'s>, drop: OwnedSubtree) -> OwnedBatch {
    OwnedBatch {
        merks: vec![unhosted(world, tx)],
        subtrees: vec![drop],
        ..OwnedBatch::default()
    }
}

/// A store with `OUTER` and, under its node `host`, `INNER`.
fn grown<'s>(db: &'s Db, versions: &'s Versions) -> World<'s> {
    let mut world = World::new(db, versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    world
}

/// A drop of a subtree if it is entered takes the subtree where it hangs
/// under the node the drop names, and does nothing and refuses nothing
/// everywhere else: under another node, under a node of another subtree,
/// and for a subtree that is not entered.
#[test]
fn a_drop_if_entered_takes_what_hangs_under_the_node_it_names_and_refuses_nothing() {
    let (db, versions) = Db::new(128);
    let mut world = grown(&db, &versions);
    let tx = world.begin();
    let entered = shown(&tx, INNER);
    assert!(entered.is_some());
    for nothing in [
        drop_if(INNER, OUTER, b"left", false),
        drop_if(INNER, 1, b"host", true),
        drop_if(NEW, OUTER, b"host", true),
        // A subtree that is entered with no host hangs under no node.
        drop_if(1, OUTER, b"host", true),
    ] {
        (world.apply(&tx, &subtrees(vec![nothing]))).expect("nothing is refused");
        assert_eq!(shown(&tx, INNER), entered);
        world.check(&tx);
    }
    assert!(shown(&tx, 1).is_some());
    let stats = tx.txn.stats();
    assert_eq!(
        (stats.tree_pages, stats.flat_pages),
        (0, 0),
        "no page is copied for a drop that takes nothing"
    );
    let batch = with_its_node(&world, &tx, drop_if(INNER, OUTER, b"host", true));
    world.apply(&tx, &batch).expect("the subtree leaves");
    assert_eq!(shown(&tx, INNER), None);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(shown(&reader, INNER), None);
}

/// A transaction whose only writes are drops of subtrees if they are
/// entered that took nothing has written nothing: its commit takes no
/// version (CV2).
#[test]
fn a_transaction_whose_drops_if_entered_took_nothing_commits_nothing() {
    let (db, versions) = Db::new(128);
    let mut world = grown(&db, &versions);
    let before = world.latest();
    let tx = world.begin();
    for nothing in [
        drop_if(NEW, OUTER, b"host", true),
        drop_if(INNER, OUTER, b"left", false),
    ] {
        (world.apply(&tx, &subtrees(vec![nothing]))).expect("nothing is refused");
    }
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    assert_eq!(world.latest(), before);
    world.check_committed();
    let reader = world.begin();
    assert!(shown(&reader, INNER).is_some());
}

/// A removal under a subtree if it is entered says nothing of what the
/// data column of the subtree holds: after a flat record and such a
/// delete of its key, a change set of the subtree is refused as it is
/// after a plain delete.
#[test]
fn a_removal_if_entered_leaves_what_the_data_column_holds_as_it_was() {
    const BARE: u8 = 9;
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let subtree = model::subtree(BARE);
    let data = model::unit(Column::Data, BARE);
    for if_entered in [true, false] {
        let tx = world.begin();
        world.write(&tx, vec![put(data, b"x", b"flat")]);
        let removing = OwnedBatch {
            records: vec![delete(data, b"x")],
            subtrees: match if_entered {
                true => vec![OwnedSubtree::RemoveIfEntered(subtree)],
                false => Vec::new(),
            },
            ..OwnedBatch::default()
        };
        world.apply(&tx, &removing).expect("the record is removed");
        let node = OwnedBatch {
            merks: vec![world.merk(&tx, BARE, |items| {
                items.insert(b"n".to_vec(), vec![3; 20]);
            })],
            ..OwnedBatch::default()
        };
        let refused = refusal(world.apply(&tx, &node));
        assert_eq!(refused, BatchError::NotMerk { subtree }, "{if_entered}");
        tx.txn.rollback().expect("no scope is open");
        tx.forget();
    }
    // By key, where no pages judge the batch, the write set holds it.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"x", b"mine")]);
    let other = OwnedBatch {
        records: vec![put(aux(1), b"x", b"theirs")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&other, &[&tx])));
    world.check(&tx);
    assert_eq!(tx.txn.stats().base, None, "it serves its records by key");
    world.write(&tx, vec![put(data, b"x", b"flat")]);
    let removing = OwnedBatch {
        records: vec![delete(data, b"x")],
        subtrees: vec![OwnedSubtree::RemoveIfEntered(subtree)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &removing).expect("the record is removed");
    let node = OwnedBatch {
        merks: vec![world.merk(&tx, BARE, |items| {
            items.insert(b"n".to_vec(), vec![3; 20]);
        })],
        ..OwnedBatch::default()
    };
    let refused = refusal(world.apply(&tx, &node));
    assert_eq!(refused, BatchError::NotMerk { subtree }, "by key");
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    // With nothing written under the key before, such a delete holds
    // nothing of the subtree, and a plain delete holds a flat record. So
    // it is with a range delete.
    for (if_entered, range) in [(true, false), (false, false), (true, true), (false, true)] {
        let tx = world.begin();
        let removal = match range {
            false => delete(data, b"x"),
            true => Write::Range(data, b"a".to_vec(), Some(b"z".to_vec())),
        };
        let removing = OwnedBatch {
            records: vec![removal],
            subtrees: match if_entered {
                true => vec![OwnedSubtree::RemoveIfEntered(subtree)],
                false => Vec::new(),
            },
            ..OwnedBatch::default()
        };
        world.apply(&tx, &removing).expect("the delete is taken");
        let node = OwnedBatch {
            merks: vec![world.merk(&tx, BARE, |items| {
                items.insert(b"n".to_vec(), vec![3; 20]);
            })],
            ..OwnedBatch::default()
        };
        let applied = world.apply(&tx, &node);
        match if_entered {
            true => drop(applied.unwrap_or_else(|refused| panic!("range: {range}: {refused}"))),
            false => assert_eq!(
                refusal(applied),
                BatchError::NotMerk { subtree },
                "range: {range}"
            ),
        }
        tx.txn.rollback().expect("no scope is open");
        tx.forget();
    }
}

/// A drop of a subtree if it is entered that is below a Merk which its
/// batch takes with what is below it goes with that one, and is no
/// second drop of it: below another such drop that finds its subtree,
/// and below a plain drop, where a second plain drop is refused.
#[test]
fn a_drop_if_entered_below_a_merk_that_its_batch_takes_goes_with_that_one() {
    let (db, versions) = Db::new(128);
    let mut world = grown(&db, &versions);
    // `DEEP` under the node `one` of `INNER`, with a record.
    let tx = world.begin();
    let mut deepen = inner_batch(&world, &tx, |items| {
        items.insert(b"one".to_vec(), vec![9; 20]);
    });
    (deepen.merks[0].hosted).insert(b"one".to_vec(), vec![model::subtree(DEEP)]);
    deepen.subtrees.push(hosted(DEEP, INNER, b"one"));
    deepen.records.push(put(aux(DEEP), b"d", b"1"));
    (world.apply(&tx, &deepen)).expect("the subtree below is entered");
    assert!(committed(world.commit(tx, &[])));

    // Both hang under a node: the one above takes the one below.
    let tx = world.begin();
    let mut batch = with_its_node(&world, &tx, drop_if(DEEP, INNER, b"one", true));
    (batch.subtrees).push(drop_if(INNER, OUTER, b"host", true));
    world.apply(&tx, &batch).expect("both leave");
    assert_eq!((shown(&tx, INNER), shown(&tx, DEEP)), (None, None));
    world.check(&tx);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();

    // Below a plain drop: two plain drops are refused, and the drop of
    // the lower one if it is entered is not.
    let plain = OwnedSubtree::Drop(model::subtree(OUTER));
    let scope = tx.txn.scope().expect("a scope");
    let twice = subtrees(vec![
        plain.clone(),
        OwnedSubtree::Drop(model::subtree(INNER)),
    ]);
    assert_eq!(
        refusal(twice.with(|batch| scope.apply(batch))),
        BatchError::NotEntered {
            subtree: model::subtree(INNER)
        }
    );
    scope.rollback();
    let batch = subtrees(vec![drop_if(INNER, OUTER, b"host", true), plain]);
    world.apply(&tx, &batch).expect("the drop above takes both");
    assert_eq!((shown(&tx, OUTER), shown(&tx, INNER)), (None, None));
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// The drop is judged in the operation that takes its batch. A writer
/// removes a node and states the drop of what hangs under it, and
/// nothing does. A commit beside it then enters a subtree under the
/// node, with a record and no node of the host's Merk: the two wrote no
/// key in common. The writer's writes are not laid out over that commit,
/// which would leave the subtree under a node that is none: the layout
/// is refused as a conflict, and so is the writer's commit, which leaves
/// the transaction as it was. It reads what the commit left meanwhile.
/// Once another commit took the subtree again, its writes are laid out
/// and it commits.
#[test]
fn a_subtree_that_a_commit_enters_under_a_removed_node_is_a_conflict_for_the_remover() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    let outer = world.merk(&tx, OUTER, |items| {
        items.insert(b"host".to_vec(), b"tree".to_vec());
        items.insert(b"left".to_vec(), vec![1; 40]);
    });
    let batch = OwnedBatch {
        merks: vec![outer],
        subtrees: vec![enter(OUTER)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the Merk is made");
    assert!(committed(world.commit(tx, &[])));

    let remover = world.begin();
    let batch = with_its_node(&world, &remover, drop_if(INNER, OUTER, b"host", true));
    (world.apply(&remover, &batch)).expect("nothing hangs under the node");
    assert_eq!(remover.txn.stats().overlapped, 0);

    let enterer = world.begin();
    let entering = OwnedBatch {
        subtrees: vec![hosted(INNER, OUTER, b"host")],
        records: vec![put(aux(INNER), b"k", b"v")],
        ..OwnedBatch::default()
    };
    (world.apply(&enterer, &entering)).expect("the subtree is entered under the node");
    assert!(committed(world.commit(enterer, &[&remover])));
    // No write of the remover was met, and its writes cannot be laid out
    // over the commit: it serves them by key, over what the commit left.
    assert!(shown(&remover, INNER).is_some());
    world.check(&remover);
    assert_eq!(remover.txn.stats().overlapped, 0);
    assert_eq!(remover.txn.stats().base, None, "it has no frames");
    assert!(remover.model.borrow().stopped());
    let refused = world.db.commit(world.versions, &remover.txn);
    assert!(matches!(&refused, Err(error) if error.is_conflict()));
    let subtree = model::subtree(INNER);
    assert_eq!(
        refusal(refused.map(|_| Applied::default())),
        BatchError::EnteredSince { subtree }
    );
    world.check(&remover);

    // The subtree leaves again: the remover's writes are laid out.
    let gone = subtrees(vec![OwnedSubtree::Drop(subtree)]);
    assert!(committed(world.write_outside(&gone, &[&remover])));
    world.check(&remover);
    assert!(!remover.model.borrow().stopped());
    assert!(remover.txn.stats().base.is_some(), "it has frames");
    assert!(committed(world.commit(remover, &[])));
    world.check_committed();
}

/// A subtree that hangs under no node of `OUTER`, stated as not entered
/// under the node `key` of `OUTER`.
fn not_under(name: u8, key: &[u8]) -> OwnedSubtree {
    OwnedSubtree::NotUnder {
        subtree: model::subtree(name),
        host: (model::subtree(OUTER), key.to_vec()),
    }
}

/// A batch that says a subtree is not entered under a node writes
/// nothing by that. Where the subtree hangs under another node, under
/// none, or is not entered, the batch is taken as it is without it, and a
/// transaction of such statements alone commits nothing. Where the
/// subtree is entered under the node the batch is refused as a conflict,
/// whatever else it states: alone, with a record of the subtree, and
/// with a drop of the Merk above it. A batch says it of a subtree once.
#[test]
fn a_subtree_that_a_batch_states_as_not_under_a_node_refuses_the_batch_where_it_is() {
    let (db, versions) = Db::new(128);
    let mut world = grown(&db, &versions);
    let before = world.latest();
    let inner = model::subtree(INNER);
    let tx = world.begin();
    for holds in [
        not_under(INNER, b"left"),
        not_under(1, b"host"),
        not_under(NEW, b"host"),
    ] {
        (world.apply(&tx, &subtrees(vec![holds]))).expect("nothing is refused");
        world.check(&tx);
    }
    let stats = tx.txn.stats();
    assert_eq!((stats.tree_pages, stats.flat_pages), (0, 0));
    let scope = tx.txn.scope().expect("a scope");
    let apply = |batch: &OwnedBatch| batch.with(|batch| scope.apply(batch));
    let refused = BatchError::EnteredSince { subtree: inner };
    assert!(refused.is_conflict());
    let alone = subtrees(vec![not_under(INNER, b"host")]);
    assert_eq!(refusal(apply(&alone)), refused);
    let with_a_record = OwnedBatch {
        records: vec![put(aux(INNER), b"k", b"v")],
        subtrees: vec![not_under(INNER, b"host")],
        ..OwnedBatch::default()
    };
    assert_eq!(refusal(apply(&with_a_record)), refused);
    let below_a_drop = subtrees(vec![
        OwnedSubtree::Drop(model::subtree(OUTER)),
        not_under(INNER, b"host"),
    ]);
    assert_eq!(refusal(apply(&below_a_drop)), refused);
    let twice = subtrees(vec![not_under(INNER, b"left"), not_under(INNER, b"right")]);
    let subtree = inner;
    assert_eq!(refusal(apply(&twice)), BatchError::SubtreeTwice { subtree });
    scope.rollback();
    world.check(&tx);
    // A second scope keeps its writes beside the pages: it is refused
    // where it applies the batch as well.
    let first = tx.txn.scope().expect("a scope");
    let second = tx.txn.scope().expect("a second scope");
    assert_eq!(refusal(alone.with(|batch| second.apply(batch))), refused);
    assert_eq!(
        refusal(with_a_record.with(|batch| second.apply(batch))),
        refused
    );
    second.rollback();
    first.rollback();
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    assert_eq!(world.latest(), before);

    let tx = world.begin();
    let elsewhere = OwnedBatch {
        records: vec![put(aux(INNER), b"k", b"v")],
        subtrees: vec![not_under(INNER, b"left")],
        ..OwnedBatch::default()
    };
    (world.apply(&tx, &elsewhere)).expect("the subtree is not under that node");
    let mut out = Vec::new();
    assert!(tx.txn.get(aux(INNER), b"k", &mut out).expect("a read"));
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A batch that says a subtree is not under a node reads the entry of
/// the subtree. A scope that says so while another open scope of the
/// transaction has entered the subtree is refused by the claim (SC2):
/// what it says is not judged on an enter that the other scope can still
/// take back, or that is laid out after it.
#[test]
fn a_subtree_that_is_stated_as_not_under_a_node_is_claimed_as_read() {
    let (db, versions) = Db::new(128);
    let world = grown(&db, &versions);
    let tx = world.begin();
    let stating = tx.txn.scope().expect("a scope");
    let entering = tx.txn.scope().expect("a second scope");
    let entered = OwnedBatch {
        subtrees: vec![hosted(NEW, OUTER, b"left")],
        records: vec![put(aux(NEW), b"k", b"v")],
        ..OwnedBatch::default()
    };
    (entered.with(|batch| entering.apply(batch))).expect("the subtree is entered");
    let stated = subtrees(vec![not_under(NEW, b"left")]);
    let refused = stated.with(|batch| stating.apply(batch));
    let claimed = Unit::data(model::subtree(NEW));
    assert!(
        matches!(&refused, Err(TxnError::ScopeOverlap { unit }) if *unit == claimed),
        "{refused:?}"
    );
    entering.rollback();
    stating.rollback();
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// What a batch said a subtree is not under is held again at every
/// layout of the transaction's writes. A writer puts a record into a
/// subtree that is entered under no node, and says that the subtree is
/// not under a node. A commit beside it drops the subtree and enters it
/// under that node, with no key in common: the writer's writes are not
/// laid out over that commit, and its commit is refused as a conflict.
#[test]
fn a_subtree_that_a_commit_enters_under_a_node_it_was_stated_as_not_under_is_a_conflict() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    let outer = world.merk(&tx, OUTER, |items| {
        items.insert(b"host".to_vec(), b"tree".to_vec());
        items.insert(b"left".to_vec(), vec![1; 40]);
    });
    let batch = OwnedBatch {
        merks: vec![outer],
        subtrees: vec![enter(OUTER)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the Merk is made");
    assert!(committed(world.commit(tx, &[])));

    let writer = world.begin();
    let batch = OwnedBatch {
        records: vec![put(aux(1), b"mine", b"v")],
        subtrees: vec![not_under(INNER, b"host")],
        ..OwnedBatch::default()
    };
    (world.apply(&writer, &batch)).expect("nothing hangs under the node");

    let enterer = world.begin();
    let entering = OwnedBatch {
        subtrees: vec![hosted(INNER, OUTER, b"host")],
        records: vec![put(aux(INNER), b"k", b"v")],
        ..OwnedBatch::default()
    };
    (world.apply(&enterer, &entering)).expect("the subtree is entered under the node");
    assert!(committed(world.commit(enterer, &[&writer])));
    world.check(&writer);
    assert_eq!(writer.txn.stats().overlapped, 0);
    assert_eq!(writer.txn.stats().base, None, "it has no frames");
    let refused = world.db.commit(world.versions, &writer.txn);
    assert!(matches!(&refused, Err(error) if error.is_conflict()));
    let subtree = model::subtree(INNER);
    assert_eq!(
        refusal(refused.map(|_| Applied::default())),
        BatchError::EnteredSince { subtree }
    );
    world.check(&writer);
}

/// What a batch says a subtree is not under is held before what the
/// batch writes is held against what the transaction wrote. A subtree is
/// entered under a node with no root, and the transaction wrote a flat
/// record into its data column. A change set of the subtree is refused
/// for that record where the batch says the subtree is not under another
/// node, which holds; where the batch says it is not under its own node,
/// the batch is refused as a conflict.
#[test]
fn what_a_batch_says_a_subtree_is_not_under_is_held_before_what_it_writes() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    let outer = world.merk(&tx, OUTER, |items| {
        items.insert(b"host".to_vec(), b"tree".to_vec());
        items.insert(b"left".to_vec(), vec![1; 40]);
    });
    let batch = OwnedBatch {
        merks: vec![outer],
        subtrees: vec![enter(OUTER)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the Merk is made");
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    let entering = OwnedBatch {
        subtrees: vec![hosted(INNER, OUTER, b"host")],
        records: vec![put(aux(INNER), b"k", b"v")],
        ..OwnedBatch::default()
    };
    (world.apply(&tx, &entering)).expect("the subtree is entered under the node");
    assert!(committed(world.commit(tx, &[])));

    let subtree = model::subtree(INNER);
    let data = model::unit(Column::Data, INNER);
    let tx = world.begin();
    world.write(&tx, vec![put(data, b"x", b"flat")]);
    let node = |key: &[u8]| OwnedBatch {
        merks: vec![world.merk(&tx, INNER, |items| {
            items.insert(b"n".to_vec(), vec![3; 20]);
        })],
        subtrees: vec![not_under(INNER, key)],
        ..OwnedBatch::default()
    };
    let refused = refusal(world.apply(&tx, &node(b"left")));
    assert_eq!(refused, BatchError::NotMerk { subtree });
    let refused = refusal(world.apply(&tx, &node(b"host")));
    assert_eq!(refused, BatchError::EnteredSince { subtree });
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// A transaction that a commit has met lays nothing out. A drop of a
/// subtree if it is entered is taken by key there: it takes the subtree
/// where the transaction has it entered under the node, and is nothing
/// otherwise.
#[test]
fn a_drop_if_entered_is_taken_by_key_as_the_transaction_has_the_subtree() {
    let (db, versions) = Db::new(128);
    let mut world = grown(&db, &versions);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"x", b"mine")]);
    let other = OwnedBatch {
        records: vec![put(aux(1), b"x", b"theirs")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&other, &[&tx])));
    world.check(&tx);
    assert_eq!(tx.txn.stats().base, None, "it serves its records by key");

    let entered = shown(&tx, INNER);
    let idle = subtrees(vec![drop_if(INNER, OUTER, b"left", false)]);
    world.apply(&tx, &idle).expect("taken by key");
    assert_eq!(shown(&tx, INNER), entered);
    world.check(&tx);
    // What a batch says the subtree is not under is held by key as well.
    let stating = |key: &[u8]| OwnedBatch {
        records: vec![put(aux(INNER), b"k", b"v")],
        subtrees: vec![not_under(INNER, key)],
        ..OwnedBatch::default()
    };
    let refused = refusal(world.apply(&tx, &stating(b"host")));
    let subtree = model::subtree(INNER);
    assert_eq!(refused, BatchError::EnteredSince { subtree });
    (world.apply(&tx, &stating(b"left"))).expect("taken by key");
    assert_eq!(shown(&tx, INNER), entered);
    world.check(&tx);
    let batch = with_its_node(&world, &tx, drop_if(INNER, OUTER, b"host", true));
    world.apply(&tx, &batch).expect("taken by key");
    assert_eq!(shown(&tx, INNER), None);
    world.check(&tx);
    assert_eq!(tx.txn.stats().base, None);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
}

/// A drop that names the node its subtree hangs under is refused where
/// the subtree hangs under another node, under a node of another
/// subtree, or under none, and where it is not entered. Under the node
/// it names, it is the drop.
#[test]
fn a_drop_under_a_node_is_refused_where_the_subtree_hangs_elsewhere() {
    let (db, versions) = Db::new(128);
    let mut world = grown(&db, &versions);
    let tx = world.begin();
    let scope = tx.txn.scope().expect("a scope");
    let apply = |batch: &OwnedBatch| batch.with(|batch| scope.apply(batch));
    let absent = subtrees(vec![drop_under(NEW, OUTER, b"host", None)]);
    assert_eq!(
        refusal(apply(&absent)),
        BatchError::NotEntered {
            subtree: model::subtree(NEW)
        }
    );
    for (name, host, key) in [
        (INNER, OUTER, &b"left"[..]),
        (INNER, 1, b"host"),
        (1, OUTER, b"host"),
    ] {
        let elsewhere = subtrees(vec![drop_under(name, host, key, None)]);
        let subtree = model::subtree(name);
        assert_eq!(
            refusal(apply(&elsewhere)),
            BatchError::HungOtherwise { subtree }
        );
    }
    scope.rollback();
    assert!(shown(&tx, INNER).is_some());
    let batch = with_its_node(&world, &tx, drop_under(INNER, OUTER, b"host", None));
    world
        .apply(&tx, &batch)
        .expect("the subtree hangs under the node");
    assert_eq!(shown(&tx, INNER), None);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A drop under a node that names the read of the node is dated at it:
/// where a commit beside the transaction wrote the node since, the batch
/// is refused as a conflict and changes nothing. From a read of the node
/// as it is then, the drop is made. A drop that names no read is not
/// dated.
#[test]
fn a_drop_under_a_node_is_dated_at_the_read_of_the_node_it_names() {
    let (db, versions) = Db::new(128);
    let mut world = grown(&db, &versions);
    let tx = world.begin();
    let read = |tx: &Tx<'_>| {
        let node = (tx.txn).node(model::subtree(OUTER), b"host", model::handle_of);
        node.expect("the node is read").expect("a handle")
    };
    let before = read(&tx);
    // The other writer changes the hosted Merk, and with it its host.
    let other = world.begin();
    let batch = inner_batch(&world, &other, |items| {
        items.insert(b"three".to_vec(), vec![5; 20]);
    });
    world.apply(&other, &batch).expect("the other write");
    assert!(committed(world.commit(other, &[&tx])));

    let dated = with_its_node(&world, &tx, drop_under(INNER, OUTER, b"host", Some(before)));
    let refused = world.apply(&tx, &dated);
    assert!(matches!(&refused, Err(error) if error.is_conflict()));
    let subtree = model::subtree(OUTER);
    assert_eq!(refusal(refused), BatchError::ValueTaken { subtree });
    assert!(shown(&tx, INNER).is_some());
    world.check(&tx);

    let undated = subtrees(vec![drop_under(INNER, OUTER, b"host", None)]);
    let scope = tx.txn.scope().expect("a scope");
    (undated.with(|batch| scope.apply(batch))).expect("a drop that names no read");
    scope.rollback();

    let after = read(&tx);
    let dated = with_its_node(&world, &tx, drop_under(INNER, OUTER, b"host", Some(after)));
    world
        .apply(&tx, &dated)
        .expect("the node is as it was read");
    assert_eq!(shown(&tx, INNER), None);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A dated drop reads the node it names. A scope that states one while
/// another open scope of the transaction has written the Merk of that
/// node is refused by the claim (SC2): the date is not taken from a
/// write that the other scope can still take back.
#[test]
fn a_dated_drop_claims_a_read_of_the_merk_that_its_node_is_in() {
    let (db, versions) = Db::new(128);
    let world = grown(&db, &versions);
    let tx = world.begin();
    let node = (tx.txn).node(model::subtree(OUTER), b"host", model::handle_of);
    let read = node.expect("the node is read").expect("a handle");
    let writer = tx.txn.scope().expect("a scope");
    let again = OwnedBatch {
        merks: vec![world.merk(&tx, OUTER, |items| {
            items.insert(b"host".to_vec(), b"written again".to_vec());
        })],
        ..OwnedBatch::default()
    };
    let again = again.through(&writer);
    (again.with(|batch| writer.apply(batch))).expect("the node is written through a scope");
    let beside = tx.txn.scope().expect("a second scope");
    let dated = subtrees(vec![drop_under(INNER, OUTER, b"host", Some(read))]);
    let refused = dated.with(|batch| beside.apply(batch));
    let claimed = Unit::data(model::subtree(OUTER));
    assert!(
        matches!(&refused, Err(TxnError::ScopeOverlap { unit }) if *unit == claimed),
        "{refused:?}"
    );
    beside.rollback();
    writer.rollback();
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// A change set that removes the node `key` of the Merk `name`, and
/// nothing else.
fn removal(name: u8, key: &[u8]) -> OwnedMerk {
    OwnedMerk {
        subtree: model::subtree(name),
        nodes: Vec::new(),
        removed: vec![key.to_vec()],
        root: None,
        hosted: BTreeMap::new(),
        stale: BTreeMap::new(),
        anchored: false,
        hints: BTreeMap::new(),
    }
}

/// What a batch removes under a subtree if it is entered is no write of
/// a page where the subtree is not entered, and no refusal. It is a
/// write of each key all the same: the transaction reads the key as
/// deleted, and a commit beside it that writes the key afterwards meets
/// the removal.
#[test]
fn what_is_removed_if_entered_is_a_write_of_its_keys_where_the_subtree_is_not_entered() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let subtree = model::subtree(NEW);
    let removing = |stated: bool| OwnedBatch {
        merks: vec![removal(NEW, b"n")],
        records: vec![delete(aux(NEW), b"k")],
        subtrees: match stated {
            true => vec![OwnedSubtree::RemoveIfEntered(subtree)],
            false => Vec::new(),
        },
    };
    let tx = world.begin();
    let scope = tx.txn.scope().expect("a scope");
    assert_eq!(
        refusal(removing(false).with(|batch| scope.apply(batch))),
        BatchError::NotEntered { subtree }
    );
    scope.rollback();
    world
        .apply(&tx, &removing(true))
        .expect("nothing is refused");
    let stats = tx.txn.stats();
    assert_eq!((stats.tree_pages, stats.flat_pages), (0, 0));
    assert_eq!(shown(&tx, NEW), None);
    world.check(&tx);

    // A commit beside it enters the subtree and puts a record under the
    // key, after the removal: the two wrote the key.
    let other = OwnedBatch {
        subtrees: vec![enter(NEW)],
        records: vec![put(aux(NEW), b"k", b"v"), put(aux(NEW), b"other", b"w")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&other, &[&tx])));
    world.check(&tx);
    assert_eq!(tx.txn.stats().overlapped, 1);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
}

/// Where the subtree is entered when the batch is laid out, what the
/// batch removes under it is removed: a record that a commit beside the
/// transaction put there before the batch was taken, and, where the
/// writes are laid out again over a later commit that entered the
/// subtree, what that commit left under the keys.
#[test]
fn what_is_removed_if_entered_is_removed_where_the_subtree_is_entered() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let subtree = model::subtree(NEW);
    let removing = OwnedBatch {
        records: vec![delete(aux(NEW), b"k")],
        subtrees: vec![OwnedSubtree::RemoveIfEntered(subtree)],
        ..OwnedBatch::default()
    };
    // Entered before the batch is taken.
    let entered = OwnedBatch {
        subtrees: vec![enter(NEW)],
        records: vec![put(aux(NEW), b"k", b"v"), put(aux(NEW), b"kept", b"w")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&entered, &[])));
    let tx = world.begin();
    world.apply(&tx, &removing).expect("the record is removed");
    let mut out = Vec::new();
    assert!(!tx.txn.get(aux(NEW), b"k", &mut out).expect("a read"));
    assert!(tx.txn.get(aux(NEW), b"kept", &mut out).expect("a read"));
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // Not entered when the batch is taken, and entered by a commit that
    // writes another key: the removal is laid out over it, and meets
    // nothing.
    let gone = subtrees(vec![OwnedSubtree::Drop(subtree)]);
    assert!(committed(world.write_outside(&gone, &[])));
    let tx = world.begin();
    world.apply(&tx, &removing).expect("nothing is refused");
    let again = OwnedBatch {
        subtrees: vec![enter(NEW)],
        records: vec![put(aux(NEW), b"kept", b"w")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&again, &[&tx])));
    assert_eq!(tx.txn.stats().overlapped, 0);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert!(reader.txn.get(aux(NEW), b"kept", &mut out).expect("a read"));
}

/// What a batch states of a subtree if the subtree is entered is all it
/// states of it: a batch that drops a subtree if it is entered writes
/// nothing under it and neither enters it nor states it anew, and one
/// that removes under a subtree if it is entered puts nothing there.
#[test]
fn a_batch_states_nothing_else_of_a_subtree_that_it_writes_if_it_is_entered() {
    let (db, versions) = Db::new(128);
    let world = grown(&db, &versions);
    let tx = world.begin();
    let scope = tx.txn.scope().expect("a scope");
    let apply = |batch: &OwnedBatch| batch.with(|batch| scope.apply(batch));
    let inner = model::subtree(INNER);
    let refused = BatchError::IfEntered { subtree: inner };
    let dropping = || vec![drop_if(INNER, OUTER, b"host", true)];
    let written = OwnedBatch {
        records: vec![put(aux(INNER), b"k", b"v")],
        subtrees: dropping(),
        ..OwnedBatch::default()
    };
    assert_eq!(refusal(apply(&written)), refused);
    let changed = OwnedBatch {
        merks: vec![removal(INNER, b"one")],
        subtrees: dropping(),
        ..OwnedBatch::default()
    };
    assert_eq!(refusal(apply(&changed)), refused);
    for beside in [enter(INNER), OwnedSubtree::EnterIfAbsent(inner, None)] {
        let mut stated = subtrees(dropping());
        stated.subtrees.push(beside);
        assert_eq!(refusal(apply(&stated)), refused);
    }
    for beside in [
        OwnedSubtree::Drop(inner),
        OwnedSubtree::Clear(inner),
        OwnedSubtree::Convert(inner, b"other".to_vec(), None),
        drop_under(INNER, OUTER, b"host", None),
    ] {
        let mut stated = subtrees(dropping());
        stated.subtrees.push(beside);
        let subtree = inner;
        assert_eq!(
            refusal(apply(&stated)),
            BatchError::SubtreeTwice { subtree }
        );
    }

    let removing = || vec![OwnedSubtree::RemoveIfEntered(inner)];
    let put_under = OwnedBatch {
        records: vec![put(aux(INNER), b"k", b"v")],
        subtrees: removing(),
        ..OwnedBatch::default()
    };
    assert_eq!(refusal(apply(&put_under)), refused);
    let node_under = inner_batch(&world, &tx, |items| {
        items.insert(b"three".to_vec(), vec![5; 20]);
    });
    let node_under = OwnedBatch {
        subtrees: removing(),
        ..node_under
    };
    assert_eq!(refusal(apply(&node_under)), refused);
    let twice = subtrees(vec![
        OwnedSubtree::RemoveIfEntered(inner),
        OwnedSubtree::RemoveIfEntered(inner),
    ]);
    let subtree = inner;
    assert_eq!(refusal(apply(&twice)), BatchError::SubtreeTwice { subtree });
    scope.rollback();
    world.check(&tx);
}
