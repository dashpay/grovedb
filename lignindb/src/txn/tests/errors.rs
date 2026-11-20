//! What a write transaction refuses, and what a failed operation leaves
//! (`transactions.md` WT10): the write set is what it was before the call.
//! Also the rules of the one scope a transaction has, the Merks that
//! nodes host, and values that are out of line.

use std::error::Error;

use lignin_format::tree_page::{Hash, HashField, Side};

use super::{
    model::{self, value, OwnedBatch, OwnedMerk, OwnedSubtree},
    store::{Db, Outcome, Rng},
    *,
};
use crate::txn::txn::Sealed;

const HASH: Hash = [7; 32];

fn aux() -> Unit {
    model::unit(Column::Aux, 0)
}

fn node<'a>(key: &'a [u8], value: ValuePut<'a>) -> NodePut<'a> {
    NodePut {
        key,
        feature: &[1],
        value_hash: &HASH,
        kv_hash: &HASH,
        left: None,
        right: None,
        hosted: Hosted::Keep,
        value,
        stale: lignin_tree::hash::Stale::NONE,
        at: None,
    }
}

fn merk<'a>(name: u8, nodes: &'a [NodePut<'a>], root: RootPut<'a>) -> MerkWrite<'a> {
    MerkWrite {
        subtree: model::subtree(name),
        nodes,
        removed: &[],
        root,
        anchored: false,
    }
}

fn link(key: &[u8]) -> Option<LinkPut<'_>> {
    Some(LinkPut {
        key,
        hash: &HASH,
        heights: (0, 0),
        aggregate: &[1],
    })
}

fn records<'a>(records: &'a [RecordWrite<'a>]) -> Batch<'a> {
    Batch {
        records,
        merks: &[],
        ..Batch::default()
    }
}

fn merks<'a>(merks: &'a [MerkWrite<'a>]) -> Batch<'a> {
    Batch {
        records: &[],
        merks,
        ..Batch::default()
    }
}

fn records_of(records: Vec<Write>) -> OwnedBatch {
    OwnedBatch {
        records,
        ..OwnedBatch::default()
    }
}

/// Applies `batch` through a scope that merges, and returns the error.
fn refused(tx: &Tx<'_>, batch: &Batch<'_>) -> TxnError {
    let before = tx.txn.stats();
    let scope = tx.txn.scope().expect("no scope is open");
    let error = scope.apply(batch).expect_err("the batch is refused");
    scope.merge();
    let after = tx.txn.stats();
    assert_eq!(after.writes, before.writes, "a refused batch adds nothing");
    assert_eq!(after.write_set_bytes, before.write_set_bytes);
    error
}

fn batch_error(tx: &Tx<'_>, batch: &Batch<'_>) -> BatchError {
    match refused(tx, batch) {
        TxnError::Batch(error) => error,
        other => panic!("{other}"),
    }
}

#[test]
fn a_batch_that_names_what_is_not_there_is_refused_whole() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        1,
        |items| drop(items.insert(b"there".to_vec(), b"v".to_vec())),
        &[],
    );
    let committed = OwnedBatch {
        merks: Vec::new(),
        records: vec![put(aux(), b"k", b"v")],
        ..OwnedBatch::default()
    };
    world.write_outside(&committed, &[]);
    let (one, other) = (model::subtree(1), model::subtree(2));
    let long = [b'k'; 256];

    for overlapped in [false, true] {
        let tx = world.begin();
        world.write(&tx, vec![put(aux(), b"mine", b"1")]);
        if overlapped {
            // From here on the transaction serves its records by key.
            let outside = OwnedBatch {
                merks: Vec::new(),
                records: vec![put(aux(), b"mine", b"2")],
                ..OwnedBatch::default()
            };
            world.write_outside(&outside, &[&tx]);
            world.check(&tx);
            assert!(tx.txn.stats().overlapped > 0);
        }

        // One outcome for a key in a batch (GM0).
        let twice = [
            RecordWrite::put(Column::Aux, aux().subtree, b"a", b"1"),
            RecordWrite::delete(Column::Aux, aux().subtree, b"a"),
        ];
        assert_eq!(
            batch_error(&tx, &records(&twice)),
            BatchError::KeyTwice { unit: aux() }
        );
        let nodes = [
            node(b"a", ValuePut::Value(b"1")),
            node(b"a", ValuePut::Value(b"2")),
        ];
        let unit = Unit::data(one);
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            BatchError::KeyTwice { unit }
        );
        let nodes = [node(b"a", ValuePut::Value(b"1"))];
        let removed: [&[u8]; 2] = [b"b", b"a"];
        let write = MerkWrite {
            removed: &removed,
            ..merk(1, &nodes, RootPut::Keep)
        };
        assert_eq!(
            batch_error(&tx, &merks(&[write])),
            BatchError::KeyTwice { unit }
        );
        let removed: [&[u8]; 2] = [b"b", b"b"];
        let write = MerkWrite {
            removed: &removed,
            ..merk(1, &[], RootPut::Keep)
        };
        assert_eq!(
            batch_error(&tx, &merks(&[write])),
            BatchError::KeyTwice { unit }
        );
        let both = [merk(1, &[], RootPut::Keep), merk(1, &[], RootPut::Keep)];
        assert_eq!(
            batch_error(&tx, &merks(&both)),
            BatchError::MerkTwice { subtree: one }
        );

        // Keys that no cell holds.
        let too_long = BatchError::KeyTooLong {
            subtree: one,
            len: 256,
        };
        let nodes = [node(&long, ValuePut::Value(b"1"))];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            too_long
        );
        let nodes = [NodePut {
            right: link(&long),
            ..node(b"a", ValuePut::Value(b"1"))
        }];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            too_long
        );
        let removed: [&[u8]; 1] = [&long];
        let write = MerkWrite {
            removed: &removed,
            ..merk(1, &[], RootPut::Keep)
        };
        assert_eq!(batch_error(&tx, &merks(&[write])), too_long);
        let nodes = [node(b"a", ValuePut::Value(b"1"))];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Set(Some(&long)))])),
            too_long
        );
        // A root changes only with a node.
        for root in [RootPut::Set(None), RootPut::Set(Some(b"there"))] {
            assert_eq!(
                batch_error(&tx, &merks(&[merk(1, &[], root)])),
                BatchError::RootAlone { subtree: one }
            );
        }
        let hosted = [other; 256];
        let nodes = [NodePut {
            hosted: Hosted::Set(&hosted),
            ..node(b"a", ValuePut::Value(b"1"))
        }];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            BatchError::TooManyHosted { subtree: one }
        );

        // A root is a node the change set leaves in the Merk. Served by
        // key, nothing else would refuse this one.
        let removed: [&[u8]; 1] = [b"there"];
        let write = MerkWrite {
            removed: &removed,
            ..merk(1, &[], RootPut::Set(Some(b"there")))
        };
        assert_eq!(
            batch_error(&tx, &merks(&[write])),
            BatchError::NoRoot { subtree: one }
        );
        // A node that keeps a value has to be there.
        let nodes = [node(b"absent", ValuePut::Keep)];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            BatchError::NoValue { subtree: one }
        );
        // A range of Merk nodes is not deleted as flat records are, and
        // no flat record is written among them (ST1).
        let flat = [
            RecordWrite::DeleteRange {
                unit,
                start: b"",
                end: None,
            },
            RecordWrite::put(Column::Data, one, b"flat", b"1"),
            RecordWrite::delete(Column::Data, one, b"there"),
        ];
        for record in &flat {
            assert_eq!(
                batch_error(&tx, &records(std::slice::from_ref(record))),
                BatchError::NotFlat { unit }
            );
        }

        if !overlapped {
            // Laid out over the committed state, a link of a node of the
            // tree and a root name a node that is there. Served by key, a
            // record may name a child that is gone (WT7b), and so may a
            // node that its tree does not reach, which is kept as a record
            // (`tests/unlinked.rs`).
            let nodes = [NodePut {
                left: link(b"absent"),
                ..node(b"z", ValuePut::Value(b"1"))
            }];
            assert_eq!(
                batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Set(Some(b"z")))])),
                BatchError::NoChild { subtree: one }
            );
            let nodes = [node(b"z", ValuePut::Value(b"1"))];
            assert_eq!(
                batch_error(
                    &tx,
                    &merks(&[merk(1, &nodes, RootPut::Set(Some(b"absent")))])
                ),
                BatchError::NoRoot { subtree: one }
            );
            // A cell is smaller than half a page: the tree layer refuses
            // the write before it changes a page.
            let huge = vec![1; 6_000];
            let nodes = [NodePut {
                feature: &huge,
                ..node(b"huge", ValuePut::Value(b"1"))
            }];
            let root = RootPut::Set(Some(b"huge"));
            let error = refused(&tx, &merks(&[merk(1, &nodes, root)]));
            assert!(matches!(error, TxnError::Fragment(_)), "{error}");
            assert!(error.source().is_some());
            // The same node where its tree does not reach it is no cell,
            // and is refused as one all the same.
            let error = refused(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)]));
            assert!(matches!(error, TxnError::Format(_)), "{error}");
            // Such a node hosts no Merk.
            let hosted = [other];
            let nodes = [NodePut {
                hosted: Hosted::Set(&hosted),
                ..node(b"z", ValuePut::Value(b"1"))
            }];
            assert_eq!(
                batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
                BatchError::HostUnlinked { subtree: one }
            );
            // And leaves no hash to be made: nobody makes a hash of one.
            // The hash of a child it names is one, and so is its value
            // hash.
            let child = LinkPut {
                key: b"nowhere",
                hash: &[0; 32],
                heights: (0, 0),
                aggregate: &[],
            };
            for field in [HashField::Value, HashField::Child(Side::Left)] {
                let nodes = [NodePut {
                    left: Some(child),
                    stale: lignin_tree::hash::Stale::NONE.with(field),
                    ..node(b"z", ValuePut::Value(b"1"))
                }];
                let refused = batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)]));
                assert_eq!(refused, BatchError::StaleUnlinked { subtree: one });
                assert!(refused.to_string().contains("leaves a hash to be made"));
            }
        }
        // An empty batch adds nothing, and is not refused (GM0).
        let scope = tx.txn.scope().expect("no scope is open");
        let empty = [merk(1, &[], RootPut::Keep)];
        assert_eq!(
            scope.apply(&merks(&empty)).expect("an empty batch"),
            Applied::default()
        );
        assert_eq!(
            scope.apply(&Batch::default()).expect("an empty batch"),
            Applied::default()
        );
        scope.merge();

        // The transaction is as it was, and usable.
        world.check(&tx);
        world.edit(&tx, 1, |items| {
            drop(items.insert(b"more".to_vec(), b"v".to_vec()))
        });
        world.check(&tx);
        let outcome = world.commit(tx, &[]);
        assert_eq!(matches!(outcome, Outcome::Conflict), overlapped);
    }
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

#[test]
fn a_transaction_that_only_fails_holds_nothing() {
    let (db, versions) = Db::new(32);
    let world = World::new(&db, &versions);
    let tx = world.begin();
    let nodes = [node(b"absent", ValuePut::Keep)];
    let merks = [merk(1, &nodes, RootPut::Keep)];
    let error = batch_error(
        &tx,
        &Batch {
            records: &[],
            merks: &merks,
            ..Batch::default()
        },
    );
    assert_eq!(
        error,
        BatchError::NoValue {
            subtree: model::subtree(1)
        }
    );
    // A root that is set with no node is not a write that is half made:
    // the transaction has written nothing, and its commit takes no version.
    let merks = [merk(1, &[], RootPut::Set(None))];
    let error = batch_error(
        &tx,
        &Batch {
            records: &[],
            merks: &merks,
            ..Batch::default()
        },
    );
    assert_eq!(
        error,
        BatchError::RootAlone {
            subtree: model::subtree(1)
        }
    );
    assert!(matches!(tx.txn.seal(), Ok(Sealed::Empty)));
    assert_eq!(tx.txn.stats(), TxnStats::default());
    assert_eq!((versions.pins_held(), db.commits.kept()), (0, 1));
    assert!(format!("{:?}", tx.txn).contains("WriteTxn"));
    assert!(format!("{:?}", tx.txn.path()).contains("WritePath"));
    assert_eq!(tx.txn.path().page_size(), store::SIZE);
    assert_eq!(tx.txn.path().keyspaces(), db.keyspaces());
    assert!(std::ptr::eq(tx.txn.path().commits(), &db.commits));
}

/// An operation that fails after it has changed pages leaves the
/// transaction as it was before the call (WT10): its frames are made again
/// from its write set.
#[test]
fn an_operation_that_fails_half_way_adds_nothing() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(41);
    world.edit_outside(
        1,
        |items| {
            for index in 0..40u8 {
                items.insert(vec![b'k', index], value(&mut rng, 120));
            }
        },
        &[],
    );
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        drop(items.insert(b"first".to_vec(), b"1".to_vec()))
    });
    world.write(&tx, vec![put(aux(), b"k", b"v")]);
    // A subtree that the node `first` hosts.
    let host = (model::subtree(1), b"first".to_vec(), 0);
    let hosted = OwnedBatch {
        subtrees: vec![OwnedSubtree::Enter(model::subtree(200), Some(host.clone()))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &hosted).expect("the subtree is entered");
    let handle = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let kept = (tx.txn.node(model::subtree(1), b"first", handle)).expect("a read");
    let before = tx.txn.stats();

    // The batch makes a Merk and enters a subtree where the host has one
    // already: the directory refuses that, after the tree pages were
    // changed.
    let failing = world.merk(&tx, 2, |items| {
        for index in 0..30u8 {
            items.insert(vec![b'n', index], vec![index; 200]);
        }
    });
    let batch = OwnedBatch {
        merks: vec![failing],
        records: vec![put(aux(), b"with", b"it")],
        subtrees: vec![OwnedSubtree::Enter(model::subtree(201), Some(host.clone()))],
    };
    let error = world.apply(&tx, &batch).expect_err("the directory refuses");
    let taken = lignin_tree::directory::DirectoryError::HostTaken {
        parent: *model::subtree(1).as_bytes(),
        position: 0,
    };
    assert!(
        matches!(&error, TxnError::Directory(error) if error.to_string() == taken.to_string()),
        "{error}"
    );
    assert!(error.source().is_some());

    let after = tx.txn.stats();
    assert_eq!(
        (after.writes, after.write_set_bytes),
        (before.writes, before.write_set_bytes)
    );
    assert_eq!(
        after.rebuilds,
        before.rebuilds + 1,
        "the frames were made again"
    );
    assert_eq!(
        (after.tree_pages, after.flat_pages),
        (before.tree_pages, before.flat_pages)
    );
    // Nothing of the batch is read, and a handle from before it has ended.
    world.check(&tx);
    let followed = tx.txn.stats().handles_followed;
    let read = |view: Option<NodeView<'_>>| view.is_some();
    assert!((tx
        .txn
        .follow(model::subtree(1), kept.expect("a cell"), b"first", read))
    .expect("a read"));
    assert_eq!(
        tx.txn.stats().handles_followed,
        followed,
        "it is found by its key"
    );

    // The same batch goes through afterwards, with the subtree at a place
    // that is free.
    let batch = OwnedBatch {
        subtrees: vec![OwnedSubtree::Enter(
            model::subtree(201),
            Some((host.0, host.1, 1)),
        )],
        ..batch
    };
    world.apply(&tx, &batch).expect("the batch is applied");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
    assert_eq!(db.counts.census.lock().expect("a census").0, 202);
}

/// A subtree that the directory refuses has changed no page. A batch of
/// which nothing was written before it leaves the frames as they are, and
/// a handle from before is still followed; one that entered a subtree
/// before it has changed pages, and the frames are made again.
#[test]
fn an_enter_that_is_refused_with_nothing_written_keeps_the_frames() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        drop(items.insert(b"first".to_vec(), b"1".to_vec()))
    });
    let enter = |name: u8, position: u8| {
        let host = (model::subtree(1), b"first".to_vec(), position);
        OwnedSubtree::Enter(model::subtree(name), Some(host))
    };
    let entering = |subtrees: Vec<OwnedSubtree>| OwnedBatch {
        subtrees,
        ..OwnedBatch::default()
    };
    (world.apply(&tx, &entering(vec![enter(200, 0)]))).expect("the subtree is entered");
    let handle = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let kept = (tx.txn.node(model::subtree(1), b"first", handle)).expect("a read");
    let kept = kept.expect("a cell");
    let taken = |error: &TxnError| {
        let taken = lignin_tree::directory::DirectoryError::HostTaken {
            parent: *model::subtree(1).as_bytes(),
            position: 0,
        };
        matches!(error, TxnError::Directory(error) if error.to_string() == taken.to_string())
    };
    let pages = |stats: &TxnStats| (stats.writes, stats.tree_pages, stats.flat_pages);
    let read = |view: Option<NodeView<'_>>| view.is_some();

    // Nothing but the subtree that is refused.
    let before = tx.txn.stats();
    let error = (world.apply(&tx, &entering(vec![enter(201, 0)]))).expect_err("the host is taken");
    assert!(taken(&error), "{error}");
    let after = tx.txn.stats();
    assert_eq!(after.rebuilds, before.rebuilds, "no page was changed");
    assert_eq!(pages(&after), pages(&before));
    assert!((tx.txn.follow(model::subtree(1), kept, b"first", read)).expect("a read"));
    assert_eq!(
        tx.txn.stats().handles_followed,
        before.handles_followed + 1,
        "the handle is as good as before"
    );
    world.check(&tx);

    // A subtree that is entered, and then the one that is refused.
    let before = tx.txn.stats();
    let both = entering(vec![enter(202, 1), enter(201, 0)]);
    let error = world.apply(&tx, &both).expect_err("the host is taken");
    assert!(taken(&error), "{error}");
    let after = tx.txn.stats();
    assert_eq!(
        after.rebuilds,
        before.rebuilds + 1,
        "the frames were made again"
    );
    assert_eq!(pages(&after), pages(&before));
    world.check(&tx);

    (world.apply(&tx, &entering(vec![enter(201, 1)]))).expect("the place is free");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The data column of a subtree holds the nodes of a Merk or flat records,
/// and a write of the other kind is refused: against the batch, against
/// what the transaction has written, and against what it reads (ST1).
#[test]
fn nodes_and_flat_records_do_not_share_a_data_column() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        1,
        |items| drop(items.insert(b"there".to_vec(), b"v".to_vec())),
        &[],
    );
    let flat_in = |name: u8| -> [RecordWrite<'static>; 3] {
        let unit = Unit::data(model::subtree(name));
        [
            RecordWrite::put(Column::Data, unit.subtree, b"flat", b"1"),
            RecordWrite::delete(Column::Data, unit.subtree, b"n"),
            RecordWrite::DeleteRange {
                unit,
                start: b"a",
                end: Some(b"z"),
            },
        ]
    };
    let not_flat = |name: u8| BatchError::NotFlat {
        unit: Unit::data(model::subtree(name)),
    };

    for overlapped in [false, true] {
        let tx = world.begin();
        world.write(&tx, vec![put(aux(), b"mine", b"1")]);
        // The transaction makes the Merk 5, deletes a key of the Merk 6,
        // which has no node, and writes a flat record under the subtree 7.
        world.edit(&tx, 5, |items| {
            drop(items.insert(b"n".to_vec(), b"v".to_vec()))
        });
        let gone = OwnedMerk {
            removed: vec![b"n".to_vec()],
            ..OwnedMerk::change(model::subtree(6), &BTreeMap::new(), &BTreeMap::new())
        };
        let batch = OwnedBatch {
            merks: vec![gone],
            records: vec![put(model::unit(Column::Data, 7), b"flat", b"1")],
            ..OwnedBatch::default()
        };
        (world.apply(&tx, &batch)).expect("a delete and a flat record");
        if overlapped {
            let outside = records_of(vec![put(aux(), b"mine", b"2")]);
            world.write_outside(&outside, &[&tx]);
            world.check(&tx);
            assert!(tx.txn.stats().overlapped > 0 && tx.txn.stats().base.is_none());
        }

        for name in [1, 5, 6] {
            // The Merk 1 is committed, and 5 and 6 are in the write set.
            for record in &flat_in(name) {
                assert_eq!(
                    batch_error(&tx, &records(std::slice::from_ref(record))),
                    not_flat(name),
                    "the Merk {name}"
                );
            }
        }
        // A batch that writes both under one subtree, which had neither.
        let nodes = [node(b"n", ValuePut::Value(b"1"))];
        for record in &flat_in(8) {
            let batch = Batch {
                records: std::slice::from_ref(record),
                merks: &[merk(8, &nodes, RootPut::Set(Some(b"n")))],
                ..Batch::default()
            };
            assert_eq!(batch_error(&tx, &batch), not_flat(8));
        }
        // A change set where the transaction wrote a flat record.
        assert_eq!(
            batch_error(&tx, &merks(&[merk(7, &nodes, RootPut::Set(Some(b"n")))])),
            BatchError::NotMerk {
                subtree: model::subtree(7)
            }
        );
        // A scope that is rolled back takes its flat record along, and the
        // subtree is free for a Merk again.
        let scope = tx.txn.scope().expect("no scope is open");
        let flat = [RecordWrite::put(
            Column::Data,
            model::subtree(9),
            b"f",
            b"1",
        )];
        scope.apply(&records(&flat)).expect("a flat record");
        let refused = scope.apply(&merks(&[merk(9, &nodes, RootPut::Set(Some(b"n")))]));
        assert!(
            matches!(refused, Err(TxnError::Batch(BatchError::NotMerk { .. }))),
            "{refused:?}"
        );
        scope.rollback();
        world.edit(&tx, 9, |items| {
            drop(items.insert(b"n".to_vec(), b"v".to_vec()))
        });

        world.check(&tx);
        let outcome = world.commit(tx, &[]);
        assert_eq!(matches!(outcome, Outcome::Conflict), overlapped);
        if !overlapped {
            // The next round starts from the same state.
            world.edit_outside(5, |items| items.clear(), &[]);
            world.edit_outside(9, |items| items.clear(), &[]);
            let unit = model::unit(Column::Data, 7);
            world.write_outside(&records_of(vec![delete(unit, b"flat")]), &[]);
        }
    }
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The frames of a transaction that was handed to its commit, and whose
/// commit did not install them, are given up as a rollback gives them up
/// (CM5, PV7, ID1).
#[test]
fn sealed_frames_that_are_not_installed_are_given_up() {
    let (db, versions) = Db::new(64);
    let world = World::new(&db, &versions);
    let mut rng = Rng::new(43);
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        for index in 0..60u8 {
            items.insert(vec![b'k', index], value(&mut rng, 100));
        }
    });
    let flat: Vec<Write> = (0..200u8)
        .map(|index| put(aux(), &[b'f', index], &value(&mut rng, 60)))
        .collect();
    world.write(&tx, flat);
    let held = (db.private_ids(), db.pool.census().private);
    assert!(held.0 > 2 && held.1 > 2, "{held:?}");

    let Sealed::Ready(parts) = tx.txn.seal().expect("no scope is open") else {
        panic!("the transaction has writes that no commit met");
    };
    // The transaction is empty, and its frames are the commit's.
    assert_eq!(tx.txn.stats().writes, 0);
    assert_eq!((db.private_ids(), db.pool.census().private), held);
    assert_eq!(versions.pins_held(), 1, "the commit keeps the base pinned");
    drop(parts);
    assert_eq!((db.private_ids(), db.pool.census().private), (0, 0));
    assert_eq!(versions.pins_held(), 0);
    tx.forget();
    world.check(&tx);
    world.check_committed();
}

/// A panic inside an operation leaves frames that may hold a part of a
/// batch. The write set does not, and the frames are made again from it.
#[test]
fn an_operation_that_panics_leaves_the_transaction_usable() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        1,
        |items| drop(items.insert(b"there".to_vec(), b"v".to_vec())),
        &[],
    );
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        drop(items.insert(b"mine".to_vec(), b"1".to_vec()))
    });
    let handle = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let kept = (tx.txn.node(model::subtree(1), b"mine", handle)).expect("a read");
    let before = tx.txn.stats();

    let read = std::panic::AssertUnwindSafe(|| {
        tx.txn.node(model::subtree(1), b"mine", |_| -> () {
            panic!("a read that panics inside the operation")
        })
    });
    assert!(std::panic::catch_unwind(read).is_err());

    world.check(&tx);
    let after = tx.txn.stats();
    assert_eq!(
        after.rebuilds,
        before.rebuilds + 1,
        "the frames were made again"
    );
    assert_eq!(after.writes, before.writes);
    // A handle from before the panic has ended.
    let is_there = |view: Option<NodeView<'_>>| view.is_some();
    let kept = kept.expect("a cell");
    assert!((tx.txn.follow(model::subtree(1), kept, b"mine", is_there)).expect("a read"));
    assert_eq!(tx.txn.stats().handles_followed, after.handles_followed);
    world.edit(&tx, 1, |items| {
        drop(items.insert(b"more".to_vec(), b"2".to_vec()))
    });
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// A first write pins a version and starts to watch the commits before it
/// adds anything. If it panics, the write set is still empty, and the
/// transaction holds nothing (PN1).
#[test]
fn a_first_write_that_panics_leaves_nothing_held() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    let made = world.merk(&tx, 5, |items| {
        drop(items.insert(b"n".to_vec(), b"v".to_vec()))
    });
    let batch = OwnedBatch {
        merks: vec![made],
        records: Vec::new(),
        ..OwnedBatch::default()
    };
    // The write panics before the Merk's entry is written, after the tree
    // pages were changed.
    crate::txn::frames::probe::panic_next();
    let write = std::panic::AssertUnwindSafe(|| world.apply(&tx, &batch));
    assert!(std::panic::catch_unwind(write).is_err());

    assert_eq!(tx.txn.stats(), TxnStats::default());
    assert_eq!(versions.pins_held(), 0, "no version is pinned");
    assert_eq!((db.private_ids(), db.pool.census().private), (0, 0));
    // Nothing watches the commits: only the newest key set is kept.
    for round in 0..3u8 {
        world.write_outside(&records_of(vec![put(aux(), &[round], b"v")]), &[]);
    }
    assert_eq!(db.commits.kept(), 1);

    // The same batch goes through afterwards.
    world.apply(&tx, &batch).expect("the write goes through");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

#[test]
fn a_scope_holds_what_it_wrote_until_it_ends() {
    let (db, versions) = Db::new(32);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    let records = [RecordWrite::put(
        Column::Aux,
        aux().subtree,
        b"k",
        b"scoped",
    )];
    let batch = Batch {
        records: &records,
        merks: &[],
        ..Batch::default()
    };
    let get = |txn: &WriteTxn<'_>| {
        let mut out = Vec::new();
        txn.get(aux(), b"k", &mut out)
            .map(|found| found.then_some(out))
    };

    let scope = tx.txn.scope().expect("no scope is open");
    assert!(format!("{scope:?}").contains("Scope"));
    assert!(std::ptr::eq(scope.transaction(), &tx.txn));
    scope.apply(&batch).expect("a write");
    // The scope reads its own write. While it is open nobody else reads
    // the unit it wrote, by key or under a cursor, and the transaction is
    // not rolled back and not committed (SC3, SC8).
    let mut out = Vec::new();
    assert!(scope.get(aux(), b"k", &mut out).expect("a read"));
    assert!(!scope.committed_get(aux(), b"k", &mut out).expect("a read"));
    let held =
        |error: &TxnError| matches!(error, TxnError::ScopeOverlap { unit } if *unit == aux());
    assert!(get(&tx.txn).is_err_and(|error| held(&error)));
    let second = tx.txn.scope().expect("any number of scopes");
    assert!(second
        .get(aux(), b"k", &mut out)
        .is_err_and(|error| held(&error)));
    assert!((second.apply(&batch)).is_err_and(|error| held(&error)));
    drop(second);
    assert!(matches!(tx.txn.rollback(), Err(TxnError::ScopeOpen)));
    let commit = db.commit(&versions, &tx.txn);
    assert!(matches!(commit, Err(TxnError::ScopeOpen)), "{commit:?}");
    let mut cursor = tx.txn.cursor(aux());
    assert!(cursor.seek_to_first().is_err_and(|error| held(&error)));
    drop(cursor);
    let mut cursor = scope.cursor(aux());
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), Some(&b"k"[..]));
    drop(cursor);

    // A scope that is dropped is rolled back.
    drop(scope);
    assert_eq!(get(&tx.txn).expect("a read"), None);
    assert_eq!(tx.txn.stats().writes, 0);
    // One that is rolled back leaves what was merged before it.
    let scope = tx.txn.scope().expect("the scope is closed");
    scope.apply(&batch).expect("a write");
    scope.merge();
    let scope = tx.txn.scope().expect("the scope is closed");
    let more = [RecordWrite::put(
        Column::Aux,
        aux().subtree,
        b"k",
        b"rolled back",
    )];
    scope
        .apply(&Batch {
            records: &more,
            merks: &[],
            ..Batch::default()
        })
        .expect("a write");
    scope.rollback();
    assert_eq!(get(&tx.txn).expect("a read"), Some(b"scoped".to_vec()));
    tx.model
        .borrow_mut()
        .writes
        .push((put(aux(), b"k", b"scoped"), world.latest().get(), false));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

#[test]
fn every_error_says_what_it_is() {
    let (subtree, unit) = (model::subtree(0xab), aux());
    let batch = [
        BatchError::KeyTwice { unit },
        BatchError::MerkTwice { subtree },
        BatchError::KeyTooLong { subtree, len: 300 },
        BatchError::TooManyHosted { subtree },
        BatchError::ValueTooLong { len: 1 << 33 },
        BatchError::NoChild { subtree },
        BatchError::NoRoot { subtree },
        BatchError::NoValue { subtree },
        BatchError::NoRead { subtree },
        BatchError::RootTaken { subtree },
        BatchError::RootAlone { subtree },
        BatchError::NotFlat { unit },
        BatchError::NotMerk { subtree },
        BatchError::HostKeeps { subtree },
        BatchError::HostUnlinked { subtree },
        BatchError::StaleUnlinked { subtree },
        BatchError::NotEntered { subtree },
        BatchError::Entered { subtree },
        BatchError::SubtreeTwice { subtree },
        BatchError::TreeTypeTooLong { subtree, len: 256 },
    ];
    let mut texts: BTreeSet<String> = batch.iter().map(BatchError::to_string).collect();
    assert_eq!(texts.len(), batch.len(), "{texts:?}");
    let page = lignin_format::PageId::MIN;
    let errors = [
        TxnError::Batch(batch[0].clone()),
        TxnError::ScopeOpen,
        TxnError::Fragment(lignin_tree::fragment::FragmentError::NoRoom { page: None }),
        TxnError::Node(lignin_tree::node::NodeError::WrongPage {
            found: page,
            expected: page,
        }),
        TxnError::Flat(lignin_tree::flat::FlatError::Freed { page }),
        TxnError::Cursor(lignin_tree::cursor::CursorError::Node(
            lignin_tree::node::NodeError::WrongPage {
                found: page,
                expected: page,
            },
        )),
        TxnError::Cache(lignin_store::cache::CacheError::Latched { page }),
        TxnError::Alloc(lignin_store::alloc::AllocError::Exhausted),
        TxnError::Load(lignin_store::load::LoadError::NoPage {
            page,
            at: store::version(1),
            freed: None,
        }),
        TxnError::Format(lignin_format::FormatError::MissingId),
        TxnError::Directory(lignin_tree::directory::DirectoryError::TooDeep),
        TxnError::PageFreed { page },
        TxnError::Damaged("a page"),
        TxnError::TreeValue(lignin_tree::value::ValueError::TooLong { len: 1 << 40 }),
    ];
    let sourceless = [1usize, 11, 12];
    for (at, error) in errors.iter().enumerate() {
        assert_eq!(
            error.source().is_none(),
            sourceless.contains(&at),
            "{error}"
        );
        texts.insert(error.to_string());
    }
    assert_eq!(texts.len(), batch.len() + errors.len(), "{texts:?}");
    // The tree layer is given a failure of the pages as it takes them.
    let pages = TxnError::ScopeOpen.into_pages();
    assert!(
        matches!(pages, lignin_tree::fragment::FragmentError::Pages(_)),
        "{pages}"
    );
}
