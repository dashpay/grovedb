//! The opens of a Merk on LigninDB (MERK-04): the base open, the open with
//! the root key of a parent element, and the open by what the subtree
//! directory holds, each beside the same open on RocksDB.

use grovedb_costs::{CostResult, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch};
use lignin_tree::directory::DirectoryError;
use lignindb::txn::{testing::Outcome, Column, Subtree, TxnError, Unit, WriteTxn};

use super::{
    access::fault,
    tests::{apply, version, Lignin, NO_VALUE_DEFINED_COST},
    tree_type_bytes, tree_type_from_bytes, LigninBatch, LigninEntry, LigninHost, LigninHosted,
    LigninNodeStore, LigninSubtree, LigninWriter,
};
use crate::{
    merk::defaults::ROOT_KEY_KEY, store::NodeStore, CryptoHash, Error, Merk, MerkType, Op,
    TreeFeatureType, TreeType,
};

/// A subtree that the store of the tests is made with, and three that it
/// is not: a batch has to enter them.
const MADE: [u8; 32] = [1; 32];
const FRESH: [u8; 32] = [230; 32];
const CHILD: [u8; 32] = [231; 32];
const OTHER: [u8; 32] = [232; 32];

fn commit(lignin: &Lignin, transaction: &WriteTxn<'_>) {
    let outcome = lignin.commit(transaction);
    assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
}

/// The Merk an open gave, and what the open cost.
fn opened<T>(opened: CostResult<T, Error>) -> (T, OperationCost) {
    (opened.value.expect("the Merk opens"), opened.cost)
}

/// How a Merk with no parent element is opened.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Base,
    Standalone,
}

fn open<'db, S: NodeStore<'db>>(
    kind: Kind,
    store: S,
    tree_type: TreeType,
) -> (Merk<S>, OperationCost) {
    opened(match kind {
        Kind::Base => Merk::open_base(store, tree_type, NO_VALUE_DEFINED_COST, version()),
        Kind::Standalone => {
            Merk::open_standalone(store, tree_type, NO_VALUE_DEFINED_COST, version())
        }
    })
}

fn open_layered<'db, S: NodeStore<'db>>(
    store: S,
    root: Option<Vec<u8>>,
    tree_type: TreeType,
) -> (Merk<S>, OperationCost) {
    opened(Merk::open_layered_with_root_key(
        store,
        root,
        tree_type,
        NO_VALUE_DEFINED_COST,
        version(),
    ))
}

fn be(n: u64) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

fn feature(tree_type: TreeType, n: u64) -> TreeFeatureType {
    match tree_type {
        TreeType::NormalTree => TreeFeatureType::BasicMerkNode,
        TreeType::SumTree => TreeFeatureType::SummedMerkNode(n as i64),
        other => panic!("the tests write no {other}"),
    }
}

fn puts(tree_type: TreeType, keys: impl Iterator<Item = u64>, len: usize) -> Vec<(Vec<u8>, Op)> {
    keys.map(|n| (be(n), Op::Put(vec![n as u8; len], feature(tree_type, n))))
        .collect()
}

fn deletes(keys: impl Iterator<Item = u64>) -> Vec<(Vec<u8>, Op)> {
    keys.map(|n| (be(n), Op::Delete)).collect()
}

/// The batches a Merk is taken through: it grows from nothing, its root
/// becomes another node, it shrinks, and it ends with no node.
fn batches(tree_type: TreeType) -> Vec<Vec<(Vec<u8>, Op)>> {
    let mut mixed = deletes(0..6);
    mixed.extend(puts(tree_type, 6..9, 70));
    mixed.extend(puts(tree_type, 500..540, 20));
    vec![
        puts(tree_type, 0..9, 30),
        puts(tree_type, 100..160, 40),
        mixed,
        deletes((6..9).chain(100..160).chain(500..540)),
        Vec::new(),
        puts(tree_type, 7..8, 10),
    ]
}

/// What one step of a run did: the cost of opening the Merk, of applying
/// the batch to it and of applying the batch of its storage, and the root
/// it has afterwards.
#[derive(Debug, PartialEq)]
struct Step {
    open: OperationCost,
    merk: OperationCost,
    storage: OperationCost,
    root_hash: CryptoHash,
    root_key: Option<Vec<u8>>,
}

/// The run on RocksDB: the Merk is opened for each batch on a deferred
/// context, and once more at the end.
fn on_rocksdb(kind: Kind, tree_type: TreeType) -> (Vec<Step>, OperationCost) {
    let storage = TempStorage::new();
    let path = [b"a merk".as_slice()];
    let mut steps = Vec::new();
    for batch in batches(tree_type) {
        let transaction = storage.start_transaction();
        let storage_batch = StorageBatch::new();
        let context = storage
            .get_transactional_storage_context(
                SubtreePath::from(path.as_ref()),
                Some(&storage_batch),
                &transaction,
            )
            .unwrap();
        let (mut merk, open) = open(kind, context, tree_type);
        let merk_cost = apply(&mut merk, &batch);
        let (root_hash, root_key) = (merk.root_hash().unwrap(), merk.root_key());
        drop(merk);
        let applied = storage.commit_multi_context_batch(storage_batch, Some(&transaction));
        applied.value.expect("the storage batch is applied");
        (storage.commit_transaction(transaction).unwrap()).expect("the transaction commits");
        steps.push(Step {
            open,
            merk: merk_cost,
            storage: applied.cost,
            root_hash,
            root_key,
        });
    }
    let transaction = storage.start_transaction();
    let context = storage
        .get_transactional_storage_context(SubtreePath::from(path.as_ref()), None, &transaction)
        .unwrap();
    let (_, last) = open(kind, context, tree_type);
    (steps, last)
}

/// What the directory holds of `subtree`, read through a handle of
/// `writer`.
fn entry_of(writer: &LigninWriter<'_, '_>, subtree: [u8; 32]) -> Option<LigninEntry> {
    let reader = LigninNodeStore::deferred(writer, LigninSubtree::alone(subtree), None);
    reader.entry().expect("the directory is read")
}

/// The same run on LigninDB, in one transaction or with a commit after
/// every batch. The subtree is entered with the first batch.
///
/// After each batch the directory, the record of the root key and the
/// open by the directory are held to the root the Merk has.
fn on_lignindb(kind: Kind, tree_type: TreeType, committing: bool) -> (Vec<Step>, OperationCost) {
    let lignin = Lignin::new();
    let path = lignin.path();
    let subtree = LigninSubtree::alone(FRESH);
    let batches = batches(tree_type);
    let mut steps = Vec::new();
    while steps.len() < batches.len() {
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        loop {
            let batch = &batches[steps.len()];
            let staged = LigninBatch::new();
            let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
            if steps.is_empty() {
                assert_eq!(
                    store.entry().unwrap(),
                    None,
                    "the subtree is not entered yet"
                );
                store.enter(tree_type).expect("the enter is staged");
                assert_eq!(store.entry().unwrap(), None, "a staged enter is not read");
            }
            let (mut merk, open_cost) = open(kind, store, tree_type);
            let merk_cost = apply(&mut merk, batch);
            let (root_hash, root_key) = (merk.root_hash().unwrap(), merk.root_key());
            drop(merk);
            let applied = staged.apply(&writer);
            applied.value.expect("the batch is applied");
            steps.push(Step {
                open: open_cost,
                merk: merk_cost,
                storage: applied.cost,
                root_hash,
                root_key: root_key.clone(),
            });

            // The directory names the root the commit stated, and the tree
            // type the subtree was entered with.
            let entry = entry_of(&writer, FRESH).expect("the subtree is entered");
            assert_eq!(entry.root_key, root_key, "the root in the directory");
            assert_eq!(entry.tree_type().unwrap(), tree_type);
            assert_eq!(entry.host, None);
            // The record of the root key holds the same key.
            let reader = LigninNodeStore::deferred(&writer, subtree.clone(), None);
            assert_eq!(reader.root_key().unwrap().unwrap(), root_key);
            // The open by the directory is the open a caller makes that
            // knows the tree type, and costs what that costs.
            let (known, known_cost) = open(Kind::Base, reader, tree_type);
            let reader = LigninNodeStore::deferred(&writer, subtree.clone(), None);
            let by_directory = Merk::open_from_directory(reader, NO_VALUE_DEFINED_COST, version());
            assert_eq!(by_directory.cost, known_cost);
            let by_directory = by_directory.value.unwrap().expect("the subtree is entered");
            assert!(by_directory.merk_type == MerkType::BaseMerk);
            assert_eq!(by_directory.tree_type, tree_type);
            assert_eq!(by_directory.root_key(), root_key);
            assert_eq!(by_directory.root_hash().unwrap(), root_hash);
            assert_eq!(known.root_hash().unwrap(), root_hash);

            if committing || steps.len() == batches.len() {
                break;
            }
        }
        writer.into_scope().merge();
        commit(&lignin, &transaction);
    }
    let transaction = path.begin();
    let reader = LigninNodeStore::read_only(&transaction, subtree);
    let (_, last) = open(kind, reader, tree_type);
    (steps, last)
}

/// "`open_layered_with_root_key` and the base open work with no `roots`
/// column", for the base open: a Merk with no parent element is opened on
/// LigninDB, written and opened again, with the root, the root key and
/// every cost it has on RocksDB.
#[test]
fn a_merk_with_no_parent_element_opens_as_it_does_on_rocksdb() {
    for kind in [Kind::Base, Kind::Standalone] {
        for tree_type in [TreeType::NormalTree, TreeType::SumTree] {
            let expected = on_rocksdb(kind, tree_type);
            // An open that finds no root key is one seek, and an open
            // that finds one reads the key and the root node.
            assert_eq!(expected.0[0].open.seek_count, 1);
            assert_eq!(expected.0[1].open.seek_count, 2);
            assert!(expected.0[1].root_key.is_some() && expected.0[3].root_key.is_none());
            assert_ne!(expected.0[0].root_key, expected.0[1].root_key);
            for committing in [false, true] {
                let found = on_lignindb(kind, tree_type, committing);
                let at = format!("{kind:?}, {tree_type}, committing {committing}");
                assert_eq!(found.0.len(), expected.0.len(), "{at}");
                for (step, (found, expected)) in found.0.iter().zip(&expected.0).enumerate() {
                    assert_eq!(found, expected, "step {step}, {at}");
                }
                assert_eq!(found.1, expected.1, "the last open, {at}");
            }
        }
    }
}

/// The base root lives in the flat keyspace: the root key of a Merk with
/// no parent element is a record of its subtree in the transaction's flat
/// keyspace `roots`, beside the directory's entry, and both go with a
/// rollback.
#[test]
fn the_root_key_of_a_base_merk_is_a_flat_record_beside_the_directory() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let subtree = LigninSubtree::alone(FRESH);
    let unit = Unit::new(Column::Roots, Subtree::new(FRESH));
    let record = |transaction: &WriteTxn<'_>| {
        let mut key = Vec::new();
        let found = transaction.get(unit, ROOT_KEY_KEY, &mut key).unwrap();
        found.then_some(key)
    };

    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    store.enter(TreeType::NormalTree).unwrap();
    let (mut merk, _) = open(Kind::Base, store, TreeType::NormalTree);
    apply(&mut merk, &puts(TreeType::NormalTree, 0..20, 25));
    let root_key = merk.root_key();
    assert!(root_key.is_some());
    drop(merk);
    staged.apply(&writer).unwrap().unwrap();
    let entry = entry_of(&writer, FRESH).expect("the subtree is entered");
    assert_eq!(entry.root_key, root_key);
    writer.into_scope().merge();
    assert_eq!(record(&transaction), root_key);

    // A Merk that was written with no record of its root key, as a Merk
    // under a parent element is, has its root in the directory alone. The
    // open by the directory takes a Merk with no host for a base Merk and
    // finds that the two do not agree.
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let other = LigninSubtree::alone(OTHER);
    let store = LigninNodeStore::deferred(&writer, other.clone(), Some(&staged));
    store.enter(TreeType::NormalTree).unwrap();
    let (mut merk, _) = open_layered(store, None, TreeType::NormalTree);
    apply(&mut merk, &puts(TreeType::NormalTree, 0..5, 25));
    drop(merk);
    staged.apply(&writer).unwrap().unwrap();
    assert!(entry_of(&writer, OTHER).unwrap().root_key.is_some());
    let reader = LigninNodeStore::deferred(&writer, other, None);
    assert_eq!(reader.root_key().unwrap().unwrap(), None);
    let opened = Merk::open_from_directory(reader, NO_VALUE_DEFINED_COST, version());
    let error = opened
        .value
        .expect_err("the record and the directory differ");
    assert!(matches!(error, Error::CorruptedState(_)), "{error}");
    writer.into_scope().merge();

    // A rollback takes the enter, the directory's root and the record.
    transaction.rollback().unwrap();
    assert_eq!(record(&transaction), None);
    let reader = LigninNodeStore::read_only(&transaction, subtree);
    assert_eq!(reader.entry().unwrap(), None);
    let (merk, cost) = open(Kind::Base, reader, TreeType::NormalTree);
    assert_eq!((merk.root_key(), cost.seek_count), (None, 1));
}

/// "`open_layered_with_root_key` and the base open work with no `roots`
/// column", for a Merk under a parent element: it is opened with the root
/// key that element holds, at the cost it has on RocksDB, and the
/// directory gives the same root key, its tree type and its host, by which
/// the same Merk opens with nothing but the prefix known.
#[test]
fn a_merk_under_a_parent_element_opens_with_its_root_key_and_from_the_directory() {
    let tree_type = TreeType::SumTree;
    let nodes = [
        puts(tree_type, 0..40, 30),
        puts(tree_type, 100..130, 30),
        deletes((0..40).chain(100..130)),
    ];

    // On RocksDB the root key is all the parent element gives the open.
    let storage = TempStorage::new();
    let child_path = [b"parent".as_slice(), b"child".as_slice()];
    let mut expected = Vec::new();
    let mut root = None;
    for batch in &nodes {
        let transaction = storage.start_transaction();
        let storage_batch = StorageBatch::new();
        let context = storage
            .get_transactional_storage_context(
                SubtreePath::from(child_path.as_ref()),
                Some(&storage_batch),
                &transaction,
            )
            .unwrap();
        let (mut merk, open) = open_layered(context, root.clone(), tree_type);
        apply(&mut merk, batch);
        root = merk.root_key();
        expected.push((open, merk.root_hash().unwrap(), root.clone()));
        drop(merk);
        (storage.commit_multi_context_batch(storage_batch, Some(&transaction)))
            .value
            .expect("the storage batch is applied");
        (storage.commit_transaction(transaction).unwrap()).expect("the transaction commits");
    }

    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let parent = LigninSubtree::alone(MADE);
    let child = LigninSubtree {
        prefix: CHILD,
        host: Some(LigninHost {
            subtree: MADE,
            key: be(50),
            hosted: vec![CHILD],
        }),
    };
    let hosted = Some(LigninHosted {
        subtree: MADE,
        key: be(50),
        position: 0,
    });
    let (mut parent_root, mut root) = (None, None);
    for (step, batch) in nodes.iter().enumerate() {
        // One batch writes the element, which holds the root key of the
        // Merk it hosts, and the Merk. The first enters the Merk.
        let staged = LigninBatch::new();
        let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(&staged));
        if step == 0 {
            lower.enter(tree_type).unwrap();
        }
        let (mut lower, open) = open_layered(lower, root.clone(), tree_type);
        apply(&mut lower, batch);
        root = lower.root_key();
        let root_hash = lower.root_hash().unwrap();
        drop(lower);
        let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(&staged));
        let (mut upper, _) = open_layered(upper, parent_root.clone(), TreeType::NormalTree);
        let element = root.clone().unwrap_or_default();
        let element = vec![(be(50), Op::Put(element, TreeFeatureType::BasicMerkNode))];
        apply(&mut upper, &element);
        parent_root = upper.root_key();
        drop(upper);
        staged
            .apply(&writer)
            .unwrap()
            .expect("the batch is applied");
        assert_eq!(
            (open, root_hash, root.clone()),
            expected[step],
            "step {step}"
        );

        // The directory names the root key the parent element holds.
        let entry = entry_of(&writer, CHILD).expect("the Merk is entered");
        assert_eq!(entry.root_key, root, "step {step}");
        assert_eq!(entry.tree_type().unwrap(), tree_type);
        assert_eq!(entry.host, hosted);
        let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
        let (known, known_cost) = open_layered(reader, root.clone(), tree_type);
        // And the Merk opens by it as it opens with the key.
        let reader = LigninNodeStore::deferred(&writer, LigninSubtree::alone(CHILD), None);
        let opened = Merk::open_from_directory(reader, NO_VALUE_DEFINED_COST, version());
        assert_eq!(opened.cost, known_cost, "step {step}");
        let opened = opened.value.unwrap().expect("the Merk is entered");
        assert!(opened.merk_type == MerkType::LayeredMerk);
        assert_eq!(opened.tree_type, tree_type);
        assert_eq!(opened.root_key(), root);
        assert_eq!(opened.root_hash().unwrap(), known.root_hash().unwrap());
        assert_eq!(opened.root_hash().unwrap(), root_hash);
    }
    assert_eq!(root, None, "the Merk ends with no node");
    // A Merk under a parent element keeps no record of its root key.
    let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
    assert_eq!(reader.root_key().unwrap().unwrap(), None);

    // A subtree that is not entered opens as no Merk, and one that was
    // entered with other bytes than a tree type's is refused. Neither is
    // charged.
    let reader = LigninNodeStore::deferred(&writer, LigninSubtree::alone(OTHER), None);
    let opened = Merk::open_from_directory(reader, NO_VALUE_DEFINED_COST, version());
    assert_eq!(opened.cost, OperationCost::default());
    assert!(opened.value.unwrap().is_none());
    let reader = LigninNodeStore::deferred(&writer, parent, None);
    assert_eq!(reader.entry().unwrap().unwrap().tree_type, b"merk".to_vec());
    let opened = Merk::open_from_directory(reader, NO_VALUE_DEFINED_COST, version());
    assert_eq!(opened.cost, OperationCost::default());
    let error = opened.value.expect_err("the bytes of no tree type");
    assert!(matches!(error, Error::UnknownTreeType(_)), "{error}");

    // An entry that cannot be read is the error of the open, uncharged: a
    // handle that reads, outside every scope, a Merk that an open scope
    // has written.
    let outside = LigninNodeStore::read_only(&transaction, child.clone());
    let opened = Merk::open_from_directory(outside, NO_VALUE_DEFINED_COST, version());
    assert_eq!(opened.cost, OperationCost::default());
    let error = opened.value.expect_err("a scope is open");
    assert!(matches!(error, Error::StorageError(_)), "{error}");

    // The same from the committed state, through a handle that only reads.
    writer.into_scope().merge();
    commit(&lignin, &transaction);
    let transaction = path.begin();
    let reader = LigninNodeStore::read_only(&transaction, child);
    let entry = reader.entry().unwrap().expect("the Merk is entered");
    assert_eq!((entry.root_key, entry.host), (None, hosted));
    let opened = Merk::open_from_directory(reader, NO_VALUE_DEFINED_COST, version());
    assert_eq!(opened.value.unwrap().unwrap().root_key(), None);
}

/// A Merk that is opened where no subtree is entered is empty, as on
/// RocksDB a Merk is where nothing was written, and a write to it is
/// refused: nothing is written without an entry in the directory.
#[test]
fn a_merk_in_a_subtree_that_is_not_entered_is_empty_and_takes_no_write() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, LigninSubtree::alone(FRESH), Some(&staged));
    let (mut merk, cost) = open(Kind::Base, store, TreeType::NormalTree);
    assert_eq!((merk.root_key(), cost.seek_count), (None, 1));
    apply(&mut merk, &puts(TreeType::NormalTree, 0..3, 10));
    drop(merk);
    let error = staged.apply(&writer).value.expect_err("no entry");
    assert!(matches!(error, Error::StorageError(_)), "{error}");
    assert!(error.to_string().contains("is not entered"), "{error}");
    assert_eq!(entry_of(&writer, FRESH), None);
}

/// A subtree is entered through its handle: staged on a deferred handle
/// and made at once on an immediate one, alone or with its host, once.
#[test]
fn a_subtree_is_entered_through_its_handle() {
    let lignin = Lignin::new();
    // The subtrees below are entered as hosted by an element that no
    // commit writes: entries whose host is no node are not a state the
    // physical invariant checker passes (`lignindb::verify`), and the
    // store is taken out of its check.
    lignin.unchecked();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let plain = |tree_type: TreeType| {
        Some(LigninEntry {
            root_key: None,
            tree_type: tree_type_bytes(tree_type),
            host: None,
        })
    };

    // A deferred handle stages the enter in its batch, which charges
    // nothing for it.
    let staged = LigninBatch::new();
    let fresh = LigninSubtree::alone(FRESH);
    let deferred = LigninNodeStore::deferred(&writer, fresh.clone(), Some(&staged));
    assert!(staged.is_empty());
    deferred.enter(TreeType::CountTree).unwrap();
    assert!(!staged.is_empty());
    assert_eq!(deferred.entry().unwrap(), None);
    let applied = staged.apply(&writer);
    assert_eq!(applied.cost, OperationCost::default());
    applied.value.expect("the subtree is entered");
    assert_eq!(deferred.entry().unwrap(), plain(TreeType::CountTree));

    // A subtree is entered once.
    deferred.enter(TreeType::CountTree).unwrap();
    let error = staged.apply(&writer).value.expect_err("entered already");
    assert!(error.to_string().contains("entered already"), "{error}");
    let immediate = LigninNodeStore::immediate(&writer, fresh.clone());
    let error = immediate
        .enter(TreeType::SumTree)
        .expect_err("entered already");
    assert!(error.to_string().contains("entered already"), "{error}");
    assert_eq!(immediate.entry().unwrap(), plain(TreeType::CountTree));

    // An immediate handle enters at once, and each subtree an element
    // hosts at its place among them.
    let hosts = |prefix: [u8; 32], hosted: &[[u8; 32]]| LigninSubtree {
        prefix,
        host: Some(LigninHost {
            subtree: FRESH,
            key: b"element".to_vec(),
            hosted: hosted.to_vec(),
        }),
    };
    for (position, prefix) in [CHILD, OTHER].into_iter().enumerate() {
        let store = LigninNodeStore::immediate(&writer, hosts(prefix, &[CHILD, OTHER]));
        store.enter(TreeType::BulkAppendTree(9)).unwrap();
        let entry = store.entry().unwrap().expect("the subtree is entered");
        assert_eq!(entry.tree_type().unwrap(), TreeType::BulkAppendTree(9));
        let hosted = LigninHosted {
            subtree: FRESH,
            key: b"element".to_vec(),
            position: position as u8,
        };
        assert_eq!((entry.root_key, entry.host), (None, Some(hosted)));
    }
    // A subtree its host does not name is not entered.
    let unnamed = LigninNodeStore::immediate(&writer, hosts([233; 32], &[CHILD, OTHER]));
    let error = unnamed.enter(TreeType::NormalTree).expect_err("not hosted");
    assert!(error.to_string().contains("not among those"), "{error}");
    assert_eq!(unnamed.entry().unwrap(), None);

    // A handle that only reads enters nothing.
    let unstaged = LigninNodeStore::deferred(&writer, LigninSubtree::alone([234; 32]), None);
    let error = unstaged.enter(TreeType::NormalTree).expect_err("no batch");
    assert!(error.to_string().contains("only reads"), "{error}");
    writer.into_scope().merge();
    let read_only = LigninNodeStore::read_only(&transaction, LigninSubtree::alone([234; 32]));
    let error = read_only.enter(TreeType::NormalTree).expect_err("no scope");
    assert!(error.to_string().contains("only reads"), "{error}");
    assert_eq!(read_only.entry().unwrap(), None);
    // What the transaction entered is read through it, and by no other
    // before it commits.
    let entered = LigninNodeStore::read_only(&transaction, fresh.clone());
    assert_eq!(entered.entry().unwrap(), plain(TreeType::CountTree));
    let other = path.begin();
    let unseen = LigninNodeStore::read_only(&other, fresh.clone());
    assert_eq!(unseen.entry().unwrap(), None);
    commit(&lignin, &transaction);
    assert_eq!(unseen.entry().unwrap(), plain(TreeType::CountTree));
}

/// Every tree type has bytes of its own in the directory, which give the
/// tree type back, its parameter with it.
#[test]
fn every_tree_type_is_kept_in_the_directory_as_bytes_that_give_it_back() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut seen = Vec::new();
    for discriminant in 0..=u8::MAX {
        let Ok(kind) = TreeType::try_from(discriminant) else {
            continue;
        };
        assert_eq!(kind.discriminant(), discriminant);
        // A tree type with a parameter shows it in its name: a new one
        // that this module does not know would lose its parameter here.
        let parameterized = format!("{kind:?}").contains('(');
        let with = |parameter: u8| match kind {
            TreeType::CommitmentTree(_) => TreeType::CommitmentTree(parameter),
            TreeType::BulkAppendTree(_) => TreeType::BulkAppendTree(parameter),
            TreeType::DenseAppendOnlyFixedSizeTree(_) => {
                TreeType::DenseAppendOnlyFixedSizeTree(parameter)
            }
            TreeType::PrivateDocumentStore(_) => TreeType::PrivateDocumentStore(parameter),
            other => other,
        };
        let parameters: &[u8] = if parameterized { &[0, 7, 255] } else { &[0] };
        for parameter in parameters {
            let tree_type = with(*parameter);
            let bytes = tree_type_bytes(tree_type);
            assert_eq!(bytes.len(), 1 + usize::from(parameterized), "{tree_type}");
            assert_eq!(bytes[0], discriminant);
            assert_eq!(tree_type_from_bytes(&bytes).unwrap(), tree_type);
            assert!(!seen.contains(&bytes), "{tree_type} has bytes of its own");
            seen.push(bytes.clone());
            // One byte more or less is no tree type.
            let mut longer = bytes.clone();
            longer.push(0);
            let shorter = &bytes[..bytes.len() - 1];
            for other in [longer.as_slice(), shorter] {
                let error = tree_type_from_bytes(other).expect_err("no tree type");
                assert!(matches!(error, Error::UnknownTreeType(_)), "{error}");
            }
        }
        // Through the directory.
        let tree_type = with(3);
        let prefix = [discriminant, 250].repeat(16).try_into().unwrap();
        let store = LigninNodeStore::immediate(&writer, LigninSubtree::alone(prefix));
        store.enter(tree_type).unwrap();
        let entry = store.entry().unwrap().expect("the subtree is entered");
        assert_eq!(entry.tree_type().unwrap(), tree_type);
    }
    assert!(seen.len() >= 22 + 4 * 2, "every tree type was seen");
    let unknown = (0..=u8::MAX).find(|discriminant| TreeType::try_from(*discriminant).is_err());
    let error = tree_type_from_bytes(&[unknown.expect("a free discriminant")]);
    assert!(matches!(error, Err(Error::UnknownTreeType(_))));
    assert!(matches!(
        tree_type_from_bytes(b"merk"),
        Err(Error::UnknownTreeType(_))
    ));
}

/// A subtree that is entered deeper than the directory takes is refused
/// with an error of its own, which a caller tells from a fault of the
/// store; every other failure of the engine is a storage error.
#[test]
fn a_merk_that_is_too_deep_is_not_supported_and_no_storage_error() {
    let too_deep = fault(TxnError::Directory(DirectoryError::TooDeep));
    assert!(matches!(too_deep, Error::NotSupported(_)), "{too_deep}");
    assert!(
        too_deep.to_string().contains("65535 Merks deep"),
        "{too_deep}"
    );
    let damaged = fault(TxnError::Damaged("a page"));
    assert!(matches!(damaged, Error::StorageError(_)), "{damaged}");
    let scope_open = fault(TxnError::ScopeOpen);
    assert!(matches!(scope_open, Error::StorageError(_)), "{scope_open}");
}
