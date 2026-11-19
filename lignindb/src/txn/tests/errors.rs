//! What a write transaction refuses, and what a failed operation leaves
//! (`transactions.md` WT10): the write set is what it was before the call.
//! Also the rules of the one scope a transaction has, the Merks that
//! nodes host, and values that are out of line.

use std::{error::Error, sync::atomic::Ordering};

use lignin_format::tree_page::Hash;

use super::{
    model::{self, value, OwnedBatch},
    store::{Db, Outcome, Rng},
    *,
};

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
    }
}

fn merks<'a>(merks: &'a [MerkWrite<'a>]) -> Batch<'a> {
    Batch {
        records: &[],
        merks,
    }
}

/// Applies `batch` through a scope that merges, and returns the error.
fn refused(tx: &Tx<'_>, batch: &Batch<'_>) -> TxnError {
    let before = tx.txn.stats();
    let mut scope = tx.txn.scope().expect("no scope is open");
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
            node(b"a", ValuePut::Inline(b"1")),
            node(b"a", ValuePut::Inline(b"2")),
        ];
        let unit = Unit::data(one);
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            BatchError::KeyTwice { unit }
        );
        let nodes = [node(b"a", ValuePut::Inline(b"1"))];
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
        let both = [
            merk(1, &[], RootPut::Set(None)),
            merk(1, &[], RootPut::Set(None)),
        ];
        assert_eq!(
            batch_error(&tx, &merks(&both)),
            BatchError::MerkTwice { subtree: one }
        );

        // Keys that no cell holds.
        let too_long = BatchError::KeyTooLong {
            subtree: one,
            len: 256,
        };
        let nodes = [node(&long, ValuePut::Inline(b"1"))];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            too_long
        );
        let nodes = [NodePut {
            right: link(&long),
            ..node(b"a", ValuePut::Inline(b"1"))
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
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &[], RootPut::Set(Some(&long)))])),
            too_long
        );
        let hosted = [other; 256];
        let nodes = [NodePut {
            hosted: Hosted::Set(&hosted),
            ..node(b"a", ValuePut::Inline(b"1"))
        }];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            BatchError::TooManyHosted { subtree: one }
        );

        // A node that keeps a value has to be there.
        let nodes = [node(b"absent", ValuePut::Keep)];
        assert_eq!(
            batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
            BatchError::NoValue { subtree: one }
        );
        // A range of Merk nodes is not deleted as flat records are.
        let range = [RecordWrite::DeleteRange {
            unit,
            start: b"",
            end: None,
        }];
        assert_eq!(
            batch_error(&tx, &records(&range)),
            BatchError::MerkRange { unit }
        );

        if !overlapped {
            // Laid out over the committed state, a link and a root name a
            // node that is there. Served by key, a record may name a child
            // that is gone (WT7b).
            let nodes = [NodePut {
                left: link(b"absent"),
                ..node(b"z", ValuePut::Inline(b"1"))
            }];
            assert_eq!(
                batch_error(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)])),
                BatchError::NoChild { subtree: one }
            );
            assert_eq!(
                batch_error(&tx, &merks(&[merk(1, &[], RootPut::Set(Some(b"absent")))])),
                BatchError::NoRoot { subtree: one }
            );
            // A cell is smaller than half a page: the tree layer refuses
            // the write before it changes a page.
            let huge = vec![1; 6_000];
            let nodes = [node(b"huge", ValuePut::Inline(&huge))];
            let error = refused(&tx, &merks(&[merk(1, &nodes, RootPut::Keep)]));
            assert!(matches!(error, TxnError::Fragment(_)), "{error}");
            assert!(error.source().is_some());
        }
        // An empty batch adds nothing, and is not refused (GM0).
        let mut scope = tx.txn.scope().expect("no scope is open");
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
        },
    );
    assert_eq!(
        error,
        BatchError::NoValue {
            subtree: model::subtree(1)
        }
    );
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
    let handle = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let kept = (tx.txn.node(model::subtree(1), b"first", handle)).expect("a read");
    let before = tx.txn.stats();

    // The batch makes a Merk, which writes a directory entry: that write
    // fails, after the tree pages were changed.
    let failing = world.merk(&tx, 2, |items| {
        for index in 0..30u8 {
            items.insert(vec![b'n', index], vec![index; 200]);
        }
    });
    let batch = OwnedBatch {
        merks: vec![failing],
        records: vec![put(aux(), b"with", b"it")],
    };
    db.roots.writes_left.store(0, Ordering::Relaxed);
    let error = world.apply(&tx, &batch).expect_err("the directory fails");
    assert!(matches!(error, TxnError::Directory(_)), "{error}");
    assert!(error.to_string().contains("told to fail") && error.source().is_some());

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

    // The same batch goes through afterwards.
    world.apply(&tx, &batch).expect("the directory works again");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

#[test]
fn a_transaction_has_one_scope_at_a_time() {
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
    };
    let get = |txn: &WriteTxn<'_>| {
        let mut out = Vec::new();
        txn.get(aux(), b"k", &mut out)
            .map(|found| found.then_some(out))
    };

    let mut scope = tx.txn.scope().expect("no scope is open");
    assert!(format!("{scope:?}").contains("Scope"));
    assert!(std::ptr::eq(scope.transaction(), &tx.txn));
    scope.apply(&batch).expect("a write");
    // The scope reads its own write. Nothing else of the transaction is
    // used while it is open: no second scope, no read outside it, no
    // rollback and no commit (SC3, SC8).
    let mut out = Vec::new();
    assert!(scope.get(aux(), b"k", &mut out).expect("a read"));
    assert!(!scope.committed_get(aux(), b"k", &mut out).expect("a read"));
    assert!(matches!(tx.txn.scope(), Err(TxnError::ScopeOpen)));
    assert!(matches!(get(&tx.txn), Err(TxnError::ScopeOpen)));
    assert!(matches!(tx.txn.rollback(), Err(TxnError::ScopeOpen)));
    let commit = db.commit(&versions, &tx.txn);
    assert!(matches!(commit, Err(TxnError::ScopeOpen)), "{commit:?}");
    let mut cursor = tx.txn.cursor(aux());
    assert!(matches!(cursor.seek_to_first(), Err(TxnError::ScopeOpen)));
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
    let mut scope = tx.txn.scope().expect("the scope is closed");
    scope.apply(&batch).expect("a write");
    scope.merge();
    let mut scope = tx.txn.scope().expect("the scope is closed");
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
        BatchError::RootTaken { subtree },
        BatchError::MerkRange { unit },
    ];
    let mut texts: BTreeSet<String> = batch.iter().map(BatchError::to_string).collect();
    assert_eq!(texts.len(), batch.len(), "{texts:?}");
    let page = lignin_format::PageId::MIN;
    let errors = [
        TxnError::Batch(batch[0]),
        TxnError::ScopeOpen,
        TxnError::Fragment(lignin_tree::fragment::FragmentError::NoRoom { page: None }),
        TxnError::Node(lignin_tree::node::NodeError::WrongPage {
            found: page,
            expected: page,
        }),
        TxnError::Flat(lignin_tree::flat::FlatError::Freed { page }),
        TxnError::Cache(lignin_store::cache::CacheError::Latched { page }),
        TxnError::Alloc(lignin_store::alloc::AllocError::Exhausted),
        TxnError::Load(lignin_store::load::LoadError::NoPage {
            page,
            at: store::version(1),
            freed: None,
        }),
        TxnError::Format(lignin_format::FormatError::MissingId),
        TxnError::Directory("no entry".into()),
        TxnError::PageFreed { page },
        TxnError::Damaged("a page"),
        TxnError::PendingValue { subtree },
    ];
    let sourceless = [1usize, 10, 11, 12];
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
