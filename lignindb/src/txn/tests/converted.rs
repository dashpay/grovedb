//! A Merk that a batch states anew ([`SubtreeWrite::Convert`]): the tree
//! type of its entry in the subtree directory, and its place among the
//! Merks of its host (`lignin_tree::directory`, "A Merk is stated anew").

use lignin_tree::directory::DirectoryError;

use super::{
    model::{self, records, Committed, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, ENTERED, TREE_TYPE},
    *,
};

const HOST: u8 = ENTERED + 30;
const COUNT: u8 = ENTERED + 31;
const SUM: u8 = ENTERED + 32;
const AVERAGE: u8 = ENTERED + 33;
const HOSTED: [u8; 3] = [COUNT, SUM, AVERAGE];
const ELEMENT: &[u8] = b"element";
const WIDE: &[u8] = b"a wide merk";
const WIDER: &[u8] = b"a wider merk";

/// What a transaction is shown of a subtree: the key of its root, its
/// tree type and its host.
type Shown = Option<(Option<Vec<u8>>, Vec<u8>, Option<(Subtree, Vec<u8>, u8)>)>;

fn shown(tx: &Tx<'_>, name: u8) -> Shown {
    let read = tx.txn.subtree(model::subtree(name), |view| {
        view.map(|view| {
            let host = (view.host).map(|host| (host.subtree, host.key.to_vec(), host.position));
            (
                view.root_key.map(<[u8]>::to_vec),
                view.tree_type.to_vec(),
                host,
            )
        })
    });
    read.expect("the directory is read")
}

fn all(tx: &Tx<'_>) -> [Shown; 3] {
    HOSTED.map(|name| shown(tx, name))
}

/// What `tx` is to be shown of the Merk `name` of the element, with
/// `tree_type` at `position`.
fn at(world: &World<'_>, tx: &Tx<'_>, name: u8, tree_type: &[u8], position: u8) -> Shown {
    let unit = Unit::data(model::subtree(name));
    let view = tx.model.borrow().view(&world.model.records, unit);
    let root = records(&Committed::items(&view)).0;
    let host = (model::subtree(HOST), ELEMENT.to_vec(), position);
    Some((root, tree_type.to_vec(), Some(host)))
}

fn before(world: &World<'_>, tx: &Tx<'_>) -> [Shown; 3] {
    [0, 1, 2].map(|place| at(world, tx, HOSTED[place], TREE_TYPE, place as u8))
}

/// The Merk of the sums is `wide` at the last place, and the Merk of the
/// averages has the place it left.
fn after(world: &World<'_>, tx: &Tx<'_>, wide: &[u8]) -> [Shown; 3] {
    [
        at(world, tx, COUNT, TREE_TYPE, 0),
        at(world, tx, SUM, wide, 2),
        at(world, tx, AVERAGE, TREE_TYPE, 1),
    ]
}

fn subtrees(subtrees: Vec<OwnedSubtree>) -> OwnedBatch {
    OwnedBatch {
        subtrees,
        ..OwnedBatch::default()
    }
}

fn stated(name: u8, tree_type: &[u8], position: Option<u8>) -> OwnedSubtree {
    OwnedSubtree::Convert(model::subtree(name), tree_type.to_vec(), position)
}

/// Enters a Merk and three that one of its nodes hosts, each with nodes.
fn planted<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let mut merks: Vec<_> = (HOSTED.iter())
        .map(|name| {
            world.merk(tx, *name, |items| {
                for index in 0..6u8 {
                    items.insert(vec![b'n', *name, index], vec![index; 30]);
                }
            })
        })
        .collect();
    let mut host = world.merk(tx, HOST, |items| {
        items.insert(ELEMENT.to_vec(), b"three trees".to_vec());
        items.insert(b"beside".to_vec(), vec![1; 40]);
    });
    (host.hosted).insert(ELEMENT.to_vec(), HOSTED.map(model::subtree).to_vec());
    merks.push(host);
    let below = |place: usize| {
        let host = (model::subtree(HOST), ELEMENT.to_vec(), place as u8);
        OwnedSubtree::Enter(model::subtree(HOSTED[place]), Some(host))
    };
    let batch = OwnedBatch {
        merks,
        records: Vec::new(),
        subtrees: vec![
            OwnedSubtree::Enter(model::subtree(HOST), None),
            below(0),
            below(1),
            below(2),
        ],
    };
    world.apply(tx, &batch).expect("the subtrees are made");
}

/// The batch of a conversion in place: the element is written with its
/// Merks in their new order, every node of the Merk of the sums is
/// written again, and the two Merks that change places are stated anew.
fn conversion<'s>(world: &World<'s>, tx: &Tx<'s>) -> OwnedBatch {
    let sum = world.merk(tx, SUM, |items| {
        for value in items.values_mut() {
            value.push(0);
        }
    });
    let mut host = world.merk(tx, HOST, |items| {
        let mut value = items[ELEMENT].clone();
        value.extend_from_slice(b", one wide");
        items.insert(ELEMENT.to_vec(), value);
    });
    let order = [COUNT, AVERAGE, SUM].map(model::subtree);
    (host.hosted).insert(ELEMENT.to_vec(), order.to_vec());
    OwnedBatch {
        merks: vec![sum, host],
        records: Vec::new(),
        subtrees: vec![
            stated(SUM, WIDE, Some(2)),
            stated(AVERAGE, TREE_TYPE, Some(1)),
        ],
    }
}

/// The Merks that the directory of the latest committed version has under
/// the element, by position.
fn children(db: &Db, versions: &Versions) -> Vec<(u8, Subtree)> {
    let guard = versions.guard();
    let pages = db.committed(&guard, versions.latest());
    let directory = db.directory_at(&pages);
    let host = directory.open(&pages, model::subtree(HOST).as_bytes());
    let host = host.expect("the directory is read").expect("the host");
    let mut children = directory.children(host.id());
    let mut found = Vec::new();
    while let Some(child) = children.next(&pages).expect("a child") {
        assert_eq!(child.key, ELEMENT);
        found.push((child.position, Subtree::new(child.prefix)));
    }
    found
}

/// A conversion in place: the Merk has its new tree type and its new
/// place from the batch on for its transaction, with frames and with
/// none, and for every other handle from the commit. Nothing else of the
/// entry changes, and a drop of the host finds every Merk where the
/// conversion put it.
#[test]
fn a_merk_that_is_stated_anew_has_its_new_type_and_place() {
    let (db, versions) = Db::new(160);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(
        children(&db, &versions),
        [(0, COUNT), (1, SUM), (2, AVERAGE)].map(|(place, name)| (place, model::subtree(name)))
    );

    let tx = world.begin();
    let reader = world.begin();
    assert_eq!(all(&tx), before(&world, &tx));
    let mark = tx.txn.mark();
    (world.apply(&tx, &conversion(&world, &tx))).expect("the Merks are stated anew");
    world.check(&tx);
    assert_eq!(all(&tx), after(&world, &tx, WIDE));
    assert_eq!(all(&reader), before(&world, &reader));

    // A rollback to a mark takes the statement out with the writes.
    assert!(tx.txn.rewind(mark));
    tx.forget();
    world.check(&tx);
    assert_eq!(all(&tx), before(&world, &tx));
    (world.apply(&tx, &conversion(&world, &tx))).expect("the Merks are stated anew");

    // A transaction that a commit has overlapped answers from its write
    // set (WT7b), and one that the commit met no key of has its frames
    // made again with the statement in them (WT7a).
    let aux = model::unit(Column::Aux, 1);
    let by_key = world.begin();
    world.write(&by_key, vec![put(aux, b"mine", b"1")]);
    let rebuilds = tx.txn.stats().rebuilds;
    let outside = OwnedBatch {
        records: vec![put(aux, b"mine", b"2")],
        ..OwnedBatch::default()
    };
    world.write_outside(&outside, &[&tx, &by_key]);
    world.check(&by_key);
    assert!(by_key.txn.stats().overlapped > 0);
    (world.apply(&by_key, &conversion(&world, &by_key))).expect("the batch is taken by key");
    world.check(&by_key);
    assert_eq!(by_key.txn.stats().base, None);
    assert_eq!(all(&by_key), after(&world, &by_key, WIDE));
    world.check(&tx);
    assert!(tx.txn.stats().rebuilds > rebuilds);
    assert_eq!(all(&tx), after(&world, &tx, WIDE));

    // A later statement stands over an earlier one, and one that gives no
    // position keeps the place.
    for each in [&tx, &by_key] {
        let wider = subtrees(vec![stated(SUM, WIDER, None)]);
        world.apply(each, &wider).expect("the Merk is stated again");
        world.check(each);
        assert_eq!(all(each), after(&world, each, WIDER));
    }
    assert_eq!(all(&reader), before(&world, &reader));

    assert!(matches!(
        world.commit(tx, &[&by_key]),
        Outcome::Committed(_)
    ));
    world.check_committed();
    assert_eq!(all(&reader), after(&world, &reader, WIDER));
    assert_eq!(
        children(&db, &versions),
        [(0, COUNT), (1, AVERAGE), (2, SUM)].map(|(place, name)| (place, model::subtree(name)))
    );
    assert!(matches!(world.commit(by_key, &[]), Outcome::Conflict));

    // A change of a Merk that changed places is written through its host
    // as before.
    let tx = world.begin();
    let average = world.merk(&tx, AVERAGE, |items| {
        drop(items.insert(b"more".to_vec(), vec![7; 500]))
    });
    let mut host = world.merk(&tx, HOST, |items| {
        drop(items.insert(ELEMENT.to_vec(), b"written again".to_vec()))
    });
    let order = [COUNT, AVERAGE, SUM].map(model::subtree);
    (host.hosted).insert(ELEMENT.to_vec(), order.to_vec());
    let batch = OwnedBatch {
        merks: vec![average, host],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the Merk is written");
    assert_eq!(all(&tx), after(&world, &tx, WIDER));
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();

    // A drop of the host takes the three with it, each found at its place.
    let tx = world.begin();
    let dropped = subtrees(vec![OwnedSubtree::Drop(model::subtree(HOST))]);
    world.apply(&tx, &dropped).expect("the host is dropped");
    world.check(&tx);
    assert_eq!(all(&tx), [None, None, None]);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let swept = db.sweep(&versions);
    assert_eq!(swept.blocked, None, "{swept:?}");
    world.check_committed();
    assert_eq!(
        *db.counts.census.lock().expect("a census"),
        (u64::from(ENTERED), 0, 0)
    );
    assert_eq!(db.private_ids(), 0);
}

/// A statement that cannot stand is refused whole, and the transaction is
/// as it was: of a subtree that is not entered, of one that the batch also
/// enters, drops or clears, of one twice, with a tree type that is too
/// long, at a place that a Merk holds, and at a place for a Merk with no
/// host.
#[test]
fn a_statement_that_cannot_stand_is_refused_whole() {
    let (db, versions) = Db::new(160);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let tx = world.begin();
    world.edit(&tx, 2, |items| {
        drop(items.insert(b"beside".to_vec(), b"v".to_vec()))
    });
    let held = |tx: &Tx<'_>| {
        let stats = tx.txn.stats();
        (stats.writes, stats.write_set_bytes, stats.overlapped)
    };
    let was = (held(&tx), all(&tx));
    let refused = |batch: OwnedBatch| match world.apply(&tx, &batch) {
        Err(error) => error,
        Ok(_) => panic!("the batch is refused"),
    };
    let batch_error = |batch: OwnedBatch| match refused(batch) {
        TxnError::Batch(error) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    };
    let directory_error = |batch: OwnedBatch| match refused(batch) {
        TxnError::Directory(error) => error,
        other => panic!("a refusal of the directory is expected, and not {other:?}"),
    };
    let sum = model::subtree(SUM);
    let missing = model::subtree(ENTERED + 40);
    let host = *model::subtree(HOST).as_bytes();

    let error = batch_error(subtrees(vec![stated(ENTERED + 40, WIDE, None)]));
    assert_eq!(error, BatchError::NotEntered { subtree: missing });
    for beside in [
        stated(SUM, WIDER, None),
        OwnedSubtree::Drop(sum),
        OwnedSubtree::Clear(sum),
    ] {
        let error = batch_error(subtrees(vec![stated(SUM, WIDE, None), beside]));
        assert_eq!(error, BatchError::SubtreeTwice { subtree: sum });
    }
    let error = batch_error(subtrees(vec![
        OwnedSubtree::Enter(missing, None),
        stated(ENTERED + 40, WIDE, None),
    ]));
    assert_eq!(error, BatchError::SubtreeTwice { subtree: missing });
    let error = batch_error(subtrees(vec![stated(SUM, &[7; 256], None)]));
    let long = BatchError::TreeTypeTooLong {
        subtree: sum,
        len: 256,
    };
    assert_eq!(error, long);
    // The place of the Merk of the counts is taken, and that of the
    // averages is too unless that Merk leaves it in the same batch.
    for place in [0, 2] {
        let error = directory_error(subtrees(vec![stated(SUM, WIDE, Some(place))]));
        assert!(
            matches!(error, DirectoryError::HostTaken { parent, position } if parent == host && position == place)
        );
    }
    let error = directory_error(subtrees(vec![
        stated(SUM, WIDE, Some(2)),
        stated(AVERAGE, TREE_TYPE, Some(2)),
    ]));
    assert!(matches!(
        error,
        DirectoryError::HostTaken { position: 2, .. }
    ));
    let error = directory_error(subtrees(vec![stated(HOST, WIDE, Some(1))]));
    assert!(matches!(error, DirectoryError::NoParent { parent } if parent == host));

    world.check(&tx);
    assert_eq!((held(&tx), all(&tx)), was);

    // A Merk that stands alone takes a tree type, and a Merk that the
    // transaction entered is stated anew by a later batch of it.
    let made = subtrees(vec![
        stated(HOST, WIDE, None),
        OwnedSubtree::Enter(missing, None),
    ]);
    world.apply(&tx, &made).expect("a Merk with no host");
    assert_eq!(shown(&tx, HOST).expect("entered").1, WIDE);
    assert_eq!(
        shown(&tx, ENTERED + 40),
        Some((None, TREE_TYPE.to_vec(), None))
    );
    let again = subtrees(vec![stated(ENTERED + 40, WIDER, None)]);
    world.apply(&tx, &again).expect("stated anew");
    assert_eq!(shown(&tx, ENTERED + 40), Some((None, WIDER.to_vec(), None)));
    // A Merk that the transaction then drops has no entry, and none to
    // state anew.
    let dropped = subtrees(vec![OwnedSubtree::Drop(missing)]);
    world.apply(&tx, &dropped).expect("the Merk is dropped");
    assert_eq!(shown(&tx, ENTERED + 40), None);
    let error = batch_error(subtrees(vec![stated(ENTERED + 40, WIDE, None)]));
    assert_eq!(error, BatchError::NotEntered { subtree: missing });
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(shown(&reader, HOST).expect("entered").1, WIDE);
    assert_eq!(shown(&reader, ENTERED + 40), None);
}

/// A statement made through a scope is the scope's: it claims the Merk's
/// unit, a read outside the scope is refused while it is open, and a
/// rollback of the scope takes the statement out (SC2, SC6).
#[test]
fn a_statement_of_a_scope_goes_with_the_scope() {
    let (db, versions) = Db::new(160);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let tx = world.begin();
    let read = |view: Option<SubtreeView<'_>>| view.map(|view| view.tree_type.to_vec());
    let sum = model::subtree(SUM);
    for kept in [false, true] {
        let scope = tx.txn.scope().expect("a scope");
        let mark = tx.model.borrow().writes.len();
        let batch = subtrees(vec![stated(SUM, WIDE, None)]);
        (world.apply_in(&tx, &scope, &batch)).expect("the Merk is stated anew");
        let through = scope.subtree(sum, read).expect("its own write");
        assert_eq!(through, Some(WIDE.to_vec()));
        let outside = tx.txn.subtree(sum, read);
        assert!(matches!(outside, Err(TxnError::ScopeOverlap { unit }) if unit == Unit::data(sum)));
        match kept {
            false => {
                scope.rollback();
                tx.model.borrow_mut().writes.truncate(mark);
                assert_eq!(all(&tx), before(&world, &tx));
            }
            true => scope.merge(),
        }
    }
    assert_eq!(shown(&tx, SUM), at(&world, &tx, SUM, WIDE, 1));
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}
