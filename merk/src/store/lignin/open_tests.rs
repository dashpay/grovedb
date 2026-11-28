//! The opens of a Merk on LigninDB (MERK-04): the base open, the open with
//! the root key of a parent element, and the open by what the subtree
//! directory holds, each beside the same open on RocksDB.

use std::cell::{Cell, RefCell};

use grovedb_costs::{CostResult, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch};
use lignin_format::{tree_page::Side, CommitVersion};
use lignin_tree::{directory::DirectoryError, node::NodeHandle};
use lignindb::txn::{Column, NodeView, Subtree, SubtreeView, TxnError, Unit, WriteTxn};

use super::{
    access::{fault, read_record_with_root, Reads},
    tests::{apply, version, Lignin, Outcome, NO_VALUE_DEFINED_COST},
    tree_type_bytes, tree_type_from_bytes, LigninBatch, LigninEntry, LigninHost, LigninHosted,
    LigninNodeStore, LigninSnapshot, LigninSubtree, LigninView, LigninWriter,
};
use crate::{
    cost_model::Keyspace, merk::defaults::ROOT_KEY_KEY, store::NodeStore, tree::kv::KV, CryptoHash,
    Error, Merk, MerkType, Op, TreeFeatureType, TreeType,
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

/// What `view` answers for the subtree `prefix` when it is asked itself
/// ([`LigninView::entry`]), after it was held to what a handle of each of
/// the three modes on that view answers for the same subtree, an error
/// by its words.
fn asked_of<'a>(view: impl Into<LigninView<'a>>, prefix: [u8; 32]) -> Result<LigninEntry, String> {
    let view = view.into();
    let words = |answer: Result<Option<LigninEntry>, Error>| format!("{answer:?}");
    let asked = view.entry(&prefix);
    let subtree = LigninSubtree::alone(prefix);
    let handles = [
        LigninNodeStore::read_only(view, subtree.clone()),
        LigninNodeStore::deferred(view, subtree.clone(), None),
        LigninNodeStore::immediate(view, subtree),
    ];
    for handle in handles {
        assert_eq!(words(view.entry(&prefix)), words(handle.entry()));
    }
    match asked {
        Ok(Some(entry)) => Ok(entry),
        Ok(None) => Err("not entered".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// A view that is asked for the entry of a subtree answers what a handle
/// of that subtree on it answers. Each view is asked for a subtree that a
/// writer enters and writes and for one that is never entered: the
/// writer; the write transaction while the writer's scope is open, after
/// the scope is merged, and a new one after the commit; a snapshot from
/// before the transaction, before and after the commit; and a snapshot
/// from after it.
#[test]
fn a_view_answers_the_entry_that_a_handle_of_the_subtree_answers() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let before = LigninSnapshot::new(path.snapshot());
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let fresh = LigninNodeStore::immediate(&writer, LigninSubtree::alone(FRESH));
    fresh
        .enter(TreeType::SumTree)
        .expect("the subtree is entered");
    let (mut merk, _) = open(Kind::Base, fresh, TreeType::SumTree);
    apply(&mut merk, &batches(TreeType::SumTree)[0]);
    let root = merk.root_key();
    assert!(root.is_some(), "the batch left a root");
    drop(merk);

    // The writer reads its own enter and the root it stated.
    let entry = asked_of(&writer, FRESH).expect("the writer entered the subtree");
    assert_eq!(entry.tree_type().unwrap(), TreeType::SumTree);
    assert_eq!(
        (entry.root_key.clone(), entry.host.clone()),
        (root.clone(), None)
    );
    assert_eq!(asked_of(&writer, OTHER), Err("not entered".to_owned()));
    // The transaction reads from outside the writer's scope, which has
    // written the subtree's Merk: the read is refused, to the view as to
    // a handle. A subtree that no scope has written is read.
    let refused = asked_of(&transaction, FRESH).expect_err("a scope is open");
    assert_ne!(refused, "not entered");
    assert_eq!(asked_of(&transaction, OTHER), Err("not entered".to_owned()));
    // The snapshot from before the transaction has neither.
    assert_eq!(asked_of(&before, FRESH), Err("not entered".to_owned()));
    assert_eq!(asked_of(&before, OTHER), Err("not entered".to_owned()));

    writer.into_scope().merge();
    assert_eq!(asked_of(&transaction, FRESH), Ok(entry.clone()));
    assert_eq!(asked_of(&transaction, OTHER), Err("not entered".to_owned()));
    commit(&lignin, &transaction);
    assert_eq!(asked_of(&before, FRESH), Err("not entered".to_owned()));
    assert_eq!(asked_of(&before, OTHER), Err("not entered".to_owned()));
    let after = LigninSnapshot::new(path.snapshot());
    assert_eq!(asked_of(&after, FRESH), Ok(entry.clone()));
    assert_eq!(asked_of(&after, OTHER), Err("not entered".to_owned()));
    let transaction = path.begin();
    assert_eq!(asked_of(&transaction, FRESH), Ok(entry));
    assert_eq!(asked_of(&transaction, OTHER), Err("not entered".to_owned()));
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

/// Writes the element of the Merk under `CHILD` in the Merk of its parent,
/// through `staged`, and returns the root key of that Merk.
fn write_element(
    writer: &LigninWriter<'_, '_>,
    staged: &LigninBatch,
    parent_root: Option<Vec<u8>>,
    element: &[u8],
) -> Option<Vec<u8>> {
    let upper = LigninNodeStore::deferred(writer, LigninSubtree::alone(MADE), Some(staged));
    let (mut upper, _) = open_layered(upper, parent_root, TreeType::NormalTree);
    let element = Op::Put(element.to_vec(), TreeFeatureType::BasicMerkNode);
    apply(&mut upper, &[(be(50), element)]);
    upper.root_key()
}

/// A conversion in place (GroveDB #1063) keeps a Merk and gives it another
/// tree type. `Merk::convert_to_big_sum_tree` states it anew through the
/// handle it writes with (`NodeStore::state_tree_type`), in the batch that
/// writes its nodes again, and from that batch on the directory gives the
/// open by prefix the wide tree, with the root, the host and the place
/// the Merk had.
#[test]
fn a_converted_merk_opens_from_the_directory_as_the_wide_tree() {
    let narrow = TreeType::SumTree;
    let wide = narrow.big_sum_counterpart().expect("a sum tree has one");
    let lignin = Lignin::new();
    let path = lignin.path();
    let child = LigninSubtree {
        prefix: CHILD,
        host: Some(LigninHost {
            subtree: MADE,
            key: be(50),
            hosted: vec![CHILD],
        }),
    };

    // The Merk is entered as a sum tree and filled, by the batch that
    // writes its element, and committed.
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(&staged));
    lower.enter(narrow).unwrap();
    let (mut lower, _) = open_layered(lower, None, narrow);
    apply(&mut lower, &puts(narrow, 0..40, 30));
    let root = lower.root_key();
    drop(lower);
    let parent_root = write_element(&writer, &staged, None, b"a sum tree");
    (staged.apply(&writer).unwrap()).expect("the batch is applied");
    writer.into_scope().merge();
    commit(&lignin, &transaction);

    // The conversion: every node is written again as a node of the wide
    // tree, by a batch that also writes the element and in which the
    // conversion states the Merk anew.
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(&staged));
    let opened = Merk::open_from_directory(lower, NO_VALUE_DEFINED_COST, version());
    let merk = opened.value.unwrap().expect("the Merk is entered");
    assert_eq!(merk.tree_type, narrow);
    let node_type = narrow.inner_node_type();
    let converted = merk.convert_to_big_sum_tree(
        &|_key, _value, _widened| Ok(()),
        &|key, value| {
            let (key_len, value_len) = (key.len() as u32, value.len() as u32);
            Ok(KV::node_value_byte_cost_size(key_len, value_len, node_type))
        },
        NO_VALUE_DEFINED_COST.as_ref(),
        version(),
    );
    let merk = converted.value.expect("the Merk is converted");
    assert_eq!((merk.tree_type, merk.root_key()), (wide, root.clone()));
    let root_hash = merk.root_hash().unwrap();
    drop(merk);
    write_element(&writer, &staged, parent_root, b"a big sum tree");
    // A handle that only reads states nothing.
    let reading = LigninNodeStore::deferred(&writer, child.clone(), None);
    assert!(reading.convert(wide).is_err());
    // Until the batch is applied the directory has the narrow tree.
    let entry = entry_of(&writer, CHILD).expect("the Merk is entered");
    assert_eq!(entry.tree_type().unwrap(), narrow);
    (staged.apply(&writer).unwrap()).expect("the batch is applied");

    let stated = LigninEntry {
        root_key: root.clone(),
        tree_type: tree_type_bytes(wide),
        host: Some(LigninHosted {
            subtree: MADE,
            key: be(50),
            position: 0,
        }),
    };
    assert_eq!(entry_of(&writer, CHILD), Some(stated.clone()));
    // Another transaction has the narrow tree until the commit, and the
    // wide one from then on.
    let other = path.begin();
    let outside = LigninNodeStore::read_only(&other, LigninSubtree::alone(CHILD));
    let before = outside.entry().unwrap().expect("the Merk is entered");
    assert_eq!(before.tree_type().unwrap(), narrow);
    writer.into_scope().merge();
    commit(&lignin, &transaction);
    assert_eq!(outside.entry().unwrap(), Some(stated));
    let opened = Merk::open_from_directory(outside, NO_VALUE_DEFINED_COST, version());
    let opened = opened.value.unwrap().expect("the Merk is entered");
    assert_eq!(
        (
            opened.tree_type,
            opened.root_key(),
            opened.root_hash().unwrap()
        ),
        (wide, root, root_hash)
    );
}

/// Converts the Merk with no node that is opened on `store`, and returns
/// what the conversion cost.
fn convert_an_empty_sum_tree<'db, S: NodeStore<'db>>(store: S) -> OperationCost {
    let narrow = TreeType::SumTree;
    let node_type = narrow.inner_node_type();
    let (merk, _) = open_layered(store, None, narrow);
    let converted = merk.convert_to_big_sum_tree(
        &|_key, _value, _widened| Ok(()),
        &|key, value| {
            let (key_len, value_len) = (key.len() as u32, value.len() as u32);
            Ok(KV::node_value_byte_cost_size(key_len, value_len, node_type))
        },
        NO_VALUE_DEFINED_COST.as_ref(),
        version(),
    );
    let merk = converted.value.expect("the Merk is converted");
    assert_eq!(
        (merk.tree_type, merk.root_key()),
        (TreeType::BigSumTree, None)
    );
    converted.cost
}

/// The conversion of a Merk with no node states nothing
/// (`Merk::convert_to_big_sum_tree`). Each Merk converted here is one
/// under an element, opened with no root key on a handle that has staged
/// and written nothing. In a subtree that is not entered it is converted at
/// the cost it has on RocksDB, with nothing staged and nothing written,
/// and the subtree is entered later with the tree type that the one who
/// enters it names. An entered subtree keeps the bytes of its entry
/// through the conversion of such a Merk.
#[test]
fn the_conversion_of_a_merk_with_no_node_states_nothing() {
    let wide = TreeType::BigSumTree;
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let fresh = LigninSubtree::alone(FRESH);
    let rocks = TempStorage::new();
    let rocks_transaction = rocks.start_transaction();
    let storage_batch = StorageBatch::new();

    let staged = LigninBatch::new();
    let writes = transaction.stats().writes;
    let store = LigninNodeStore::deferred(&writer, fresh.clone(), Some(&staged));
    let context = rocks
        .get_transactional_storage_context(
            SubtreePath::empty(),
            Some(&storage_batch),
            &rocks_transaction,
        )
        .unwrap();
    assert_eq!(
        convert_an_empty_sum_tree(store),
        convert_an_empty_sum_tree(context)
    );
    assert!(staged.is_empty(), "nothing is staged");
    let store = LigninNodeStore::immediate(&writer, fresh.clone());
    let context = rocks
        .get_immediate_storage_context(SubtreePath::empty(), &rocks_transaction)
        .unwrap();
    assert_eq!(
        convert_an_empty_sum_tree(store),
        convert_an_empty_sum_tree(context)
    );
    assert_eq!(transaction.stats().writes, writes, "nothing is written");
    assert_eq!(entry_of(&writer, FRESH), None);

    // The first writer enters the subtree, as the wide tree it now is.
    let store = LigninNodeStore::deferred(&writer, fresh, Some(&staged));
    store.enter(wide).unwrap();
    let (mut merk, _) = open_layered(store, None, wide);
    let nodes: Vec<_> = (0..5u64)
        .map(|n| {
            let feature = TreeFeatureType::BigSummedMerkNode(n.into());
            (be(n), Op::Put(vec![n as u8; 10], feature))
        })
        .collect();
    apply(&mut merk, &nodes);
    drop(merk);
    (staged.apply(&writer).unwrap()).expect("the batch is applied");
    let entry = entry_of(&writer, FRESH).expect("the subtree is entered");
    assert_eq!(entry.tree_type().unwrap(), wide);

    // A subtree that is entered as a sum tree, whose Merk has no node:
    // the conversion states nothing, on either kind of handle, and the
    // entry is as it was.
    let other = LigninSubtree::alone(OTHER);
    LigninNodeStore::immediate(&writer, other.clone())
        .enter(TreeType::SumTree)
        .unwrap();
    let entered = entry_of(&writer, OTHER).expect("the subtree is entered");
    assert_eq!(entered.tree_type().unwrap(), TreeType::SumTree);
    let writes = transaction.stats().writes;
    convert_an_empty_sum_tree(LigninNodeStore::deferred(
        &writer,
        other.clone(),
        Some(&staged),
    ));
    assert!(staged.is_empty(), "nothing is staged");
    convert_an_empty_sum_tree(LigninNodeStore::immediate(&writer, other));
    assert_eq!(transaction.stats().writes, writes, "nothing is written");
    assert_eq!(entry_of(&writer, OTHER), Some(entered));
}

/// A batch states a subtree anew that is entered before it. Where one
/// batch enters a subtree, fills its Merk and converts it, the batch
/// holds an enter and a statement anew of one subtree, and the
/// transaction refuses it whole: the Merk is not left entered as the
/// narrow tree with the nodes of the wide one.
#[test]
fn a_batch_that_enters_a_subtree_and_converts_its_merk_is_refused_whole() {
    let narrow = TreeType::SumTree;
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, LigninSubtree::alone(FRESH), Some(&staged));
    store.enter(narrow).unwrap();
    let (mut merk, _) = open_layered(store, None, narrow);
    apply(&mut merk, &puts(narrow, 0..9, 10));
    let node_type = narrow.inner_node_type();
    let converted = merk.convert_to_big_sum_tree(
        &|_key, _value, _widened| Ok(()),
        &|key, value| {
            let (key_len, value_len) = (key.len() as u32, value.len() as u32);
            Ok(KV::node_value_byte_cost_size(key_len, value_len, node_type))
        },
        NO_VALUE_DEFINED_COST.as_ref(),
        version(),
    );
    let merk = converted.value.expect("the Merk is converted");
    assert_eq!(merk.tree_type, TreeType::BigSumTree);
    drop(merk);

    let writes = transaction.stats().writes;
    let refused = staged
        .apply(&writer)
        .value
        .expect_err("an enter and a statement anew");
    assert!(matches!(refused, Error::StorageError(_)), "{refused}");
    let words = refused.to_string();
    assert!(
        words.contains("is stated anew") && words.contains("twice"),
        "{refused}"
    );
    assert_eq!(transaction.stats().writes, writes, "nothing is written");
    assert_eq!(entry_of(&writer, FRESH), None);
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
        store.enter(TreeType::ProvableCountTree).unwrap();
        let entry = store.entry().unwrap().expect("the subtree is entered");
        assert_eq!(entry.tree_type().unwrap(), TreeType::ProvableCountTree);
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
    // The element that hosts the two is a node of its Merk when the
    // transaction commits: an entry names a node as its host.
    let host = LigninNodeStore::immediate(&writer, fresh.clone());
    let (mut host, _) = open(Kind::Base, host, TreeType::CountTree);
    let element = Op::Put(b"two".to_vec(), TreeFeatureType::CountedMerkNode(1));
    apply(&mut host, &[(b"element".to_vec(), element)]);
    drop(host);
    let with_element = plain(TreeType::CountTree).map(|entry| LigninEntry {
        root_key: Some(b"element".to_vec()),
        ..entry
    });

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
    assert_eq!(entered.entry().unwrap(), with_element);
    let other = path.begin();
    let unseen = LigninNodeStore::read_only(&other, fresh.clone());
    assert_eq!(unseen.entry().unwrap(), None);
    commit(&lignin, &transaction);
    assert_eq!(unseen.entry().unwrap(), with_element);
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

/// A Merk that is opened from a root-key record which does not name its
/// root writes no node (GROVE-05-F1). The record of a Merk with no parent
/// element is left naming a node that is gone, as GroveDB leaves the
/// record of the root Merk on grove versions 1 to 3: the root node is
/// deleted through a Merk that is opened with the root key given, whose
/// commit writes no record. From there on:
///
/// - a batch is refused where it is settled, if a handle that stages into
///   it read the record and it holds a node of the Merk, whichever of
///   its handles staged the node;
/// - a batch whose handle read the record and that holds no node of the
///   Merk is taken, also where it holds nodes of another Merk;
/// - a handle that writes at once and read the record is refused its
///   commit;
/// - nothing is written by a refusal: the directory, the record and the
///   root hash are what they were;
/// - a batch that is merged with one whose handle read the record is
///   refused as that one is, and so is a batch whose handle read the
///   record before its staged writes were taken out of it, where the
///   nodes are staged afterwards or merged in afterwards;
/// - the refusal names the record as the latest such read of the batch
///   found it;
/// - a batch in which the record was read so is refused the nodes of
///   the Merk for good, also once the record names the root and a new
///   handle reads it, and a new batch takes them;
/// - a write of the record is taken, and a handle that reads the record
///   again where it names the root writes;
/// - a record of a Merk with no node is not judged;
/// - a Merk that has nodes and no record is refused as one whose record
///   names another node.
#[test]
fn a_merk_opened_from_a_root_key_record_that_does_not_name_its_root_writes_no_node() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let subtree = LigninSubtree::alone(FRESH);
    let tree_type = TreeType::NormalTree;
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let more = puts(tree_type, 100..103, 25);

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    store.enter(tree_type).unwrap();
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    apply(&mut merk, &puts(tree_type, 0..20, 25));
    let first = merk.root_key().expect("a root");
    drop(merk);
    staged.apply(&writer).unwrap().unwrap();

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    let (mut merk, _) = open_layered(store, Some(first.clone()), tree_type);
    apply(&mut merk, &[(first.clone(), Op::Delete)]);
    let (root, root_hash) = (merk.root_key().expect("a root"), merk.root_hash().unwrap());
    drop(merk);
    staged.apply(&writer).unwrap().unwrap();
    assert_ne!(root, first);

    let held = |told: &str| {
        let entry = entry_of(&writer, FRESH).expect("the subtree is entered");
        assert_eq!(entry.root_key, Some(root.clone()), "{told}");
        let reader = LigninNodeStore::deferred(&writer, subtree.clone(), None);
        let record = reader.root_key().unwrap().unwrap();
        assert_eq!(record, Some(first.clone()), "{told}");
        let reader = LigninNodeStore::deferred(&writer, subtree.clone(), None);
        let (merk, _) = open_layered(reader, Some(root.clone()), tree_type);
        assert_eq!(merk.root_hash().unwrap(), root_hash, "{told}");
        // The record names no node, so the Merk is opened from it with
        // none, as it is on every engine.
        let reader = LigninNodeStore::deferred(&writer, subtree.clone(), None);
        let (merk, _) = open(Kind::Base, reader, tree_type);
        assert_eq!(merk.root_key(), None, "{told}");
    };
    held("after the delete");
    let words = format!(
        "lignindb: the root-key record of Merk {} names the key {}, and the root of that Merk is \
         the node under the key {}: no node of a Merk is written where its root-key record was \
         read and does not name its root. On grove versions 1 to 3 the delete of a tree that \
         holds elements leaves the record so where it changes the root of the Merk the tree was \
         in (grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs, GroveDB #686 and #1065)",
        hex::encode(FRESH),
        hex::encode(&first),
        hex::encode(&root),
    );
    let refused = |error: Error, told: &str| match error {
        Error::NotSupported(said) => assert_eq!(said, words, "{told}"),
        other => panic!("{told}: {other}"),
    };

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    assert_eq!(merk.root_key(), None);
    apply(&mut merk, &more);
    drop(merk);
    let applied = staged.apply(&writer).value;
    refused(applied.expect_err("a batch"), "a batch");
    held("after a batch");

    let staged = LigninBatch::new();
    let reader = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    assert_eq!(reader.root_key().unwrap().unwrap(), Some(first.clone()));
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    let (mut merk, _) = open_layered(store, Some(root.clone()), tree_type);
    apply(&mut merk, &more);
    drop(merk);
    let applied = staged.apply(&writer).value;
    refused(
        applied.expect_err("the node of another handle"),
        "the node of another handle",
    );
    held("after the node of another handle");

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    assert_eq!(store.root_key().unwrap().unwrap(), Some(first.clone()));
    store.put_meta_record(b"meta", b"1").unwrap().unwrap();
    (staged.apply(&writer).value).expect("a batch with no node");
    held("after a batch with no node");

    let beside = LigninSubtree::alone(OTHER);
    let staged = LigninBatch::new();
    let reader = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    assert_eq!(reader.root_key().unwrap().unwrap(), Some(first.clone()));
    let store = LigninNodeStore::deferred(&writer, beside.clone(), Some(&staged));
    store.enter(tree_type).unwrap();
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    apply(&mut merk, &more);
    let beside_root = merk.root_key();
    drop(merk);
    (staged.apply(&writer).value).expect("the nodes of another Merk");
    assert!(beside_root.is_some());
    assert_eq!(entry_of(&writer, OTHER).unwrap().root_key, beside_root);
    held("after a batch with the nodes of another Merk");

    let store = LigninNodeStore::immediate(&writer, subtree.clone());
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    assert_eq!(merk.root_key(), None);
    let applied = merk.apply::<_, Vec<u8>>(&more, &[], None, version());
    refused(
        applied.value.expect_err("a handle that writes at once"),
        "a handle that writes at once",
    );
    drop(merk);
    held("after a handle that writes at once");

    for read_by_the_first in [true, false] {
        let (one, other) = (LigninBatch::new(), LigninBatch::new());
        let (reads, writes) = match read_by_the_first {
            true => (&one, &other),
            false => (&other, &one),
        };
        let reader = LigninNodeStore::deferred(&writer, subtree.clone(), Some(reads));
        assert_eq!(reader.root_key().unwrap().unwrap(), Some(first.clone()));
        let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(writes));
        let (mut merk, _) = open_layered(store, Some(root.clone()), tree_type);
        apply(&mut merk, &more);
        drop(merk);
        one.merge_overwriting(other);
        let applied = one.apply(&writer).value;
        refused(
            applied.expect_err("two batches merged"),
            "two batches merged",
        );
        held("after two batches merged");
    }

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    staged.take_pending().apply(&writer).unwrap().unwrap();
    apply(&mut merk, &more);
    drop(merk);
    let applied = staged.apply(&writer).value;
    refused(
        applied.expect_err("a batch whose first writes were taken out"),
        "a batch whose first writes were taken out",
    );
    held("after a batch whose first writes were taken out");

    let staged = LigninBatch::new();
    let reader = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    assert_eq!(reader.root_key().unwrap().unwrap(), Some(first.clone()));
    staged.take_pending().apply(&writer).unwrap().unwrap();
    let other = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&other));
    let (mut merk, _) = open_layered(store, Some(root.clone()), tree_type);
    apply(&mut merk, &more);
    drop(merk);
    drop(reader);
    staged.merge_overwriting(other);
    let applied = staged.apply(&writer).value;
    refused(
        applied.expect_err("nodes merged in after the writes were taken out"),
        "nodes merged in after the writes were taken out",
    );
    held("after nodes merged in after the writes were taken out");

    // The record is removed, and a second handle of a batch whose first
    // read found it naming another node finds none: the refusal names
    // what the second found.
    let staged = LigninBatch::new();
    let reader = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    assert_eq!(reader.root_key().unwrap().unwrap(), Some(first.clone()));
    drop(reader);
    let record = LigninNodeStore::immediate(&writer, subtree.clone());
    record.set_root_key(None).unwrap().unwrap();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    assert_eq!(merk.root_key(), None);
    apply(&mut merk, &more);
    drop(merk);
    match staged.apply(&writer).value.expect_err("the latest read") {
        Error::NotSupported(said) => assert_eq!(
            said,
            words.replace(
                &format!("names the key {}", hex::encode(&first)),
                "is absent"
            ),
            "the latest read"
        ),
        other => panic!("the latest read: {other}"),
    }
    record.set_root_key(Some(&first)).unwrap().unwrap();
    drop(record);
    held("after the latest read");

    let used = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&used));
    assert_eq!(store.root_key().unwrap().unwrap(), Some(first.clone()));
    store.put_meta_record(b"meta", b"2").unwrap().unwrap();
    (used.apply(&writer).value).expect("a batch with no node");
    drop(store);

    let store = LigninNodeStore::immediate(&writer, subtree.clone());
    assert_eq!(store.root_key().unwrap().unwrap(), Some(first.clone()));
    store.set_root_key(Some(&root)).unwrap().unwrap();
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    assert_eq!(merk.root_key(), Some(root.clone()));
    assert_eq!(merk.root_hash().unwrap(), root_hash);
    let applied = merk.apply::<_, Vec<u8>>(&more, &[], None, version());
    applied.value.expect("a record that names the root again");
    let root = merk.root_key();
    drop(merk);
    assert_eq!(entry_of(&writer, FRESH).unwrap().root_key, root);

    // The batch that the record was read in while it named another node
    // takes no node of the Merk, whatever its new handle reads. A new
    // batch does.
    let later = puts(tree_type, 200..203, 25);
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&used));
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    assert_eq!(merk.root_key(), root);
    apply(&mut merk, &later);
    drop(merk);
    let applied = used.apply(&writer).value;
    refused(
        applied.expect_err("a batch that is used again"),
        "a batch that is used again",
    );
    assert_eq!(entry_of(&writer, FRESH).unwrap().root_key, root);
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    apply(&mut merk, &later);
    let root = merk.root_key();
    drop(merk);
    (staged.apply(&writer).value).expect("a new batch");
    assert_eq!(entry_of(&writer, FRESH).unwrap().root_key, root);

    const EMPTY: [u8; 32] = [233; 32];
    let other = LigninSubtree::alone(EMPTY);
    let store = LigninNodeStore::immediate(&writer, other.clone());
    store.enter(tree_type).unwrap();
    store.set_root_key(Some(b"absent")).unwrap().unwrap();
    let store = LigninNodeStore::immediate(&writer, other);
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    assert_eq!(merk.root_key(), None);
    let applied = merk.apply::<_, Vec<u8>>(&more, &[], None, version());
    applied.value.expect("a record under a Merk with no node");
    let root = merk.root_key();
    drop(merk);
    assert!(root.is_some());
    assert_eq!(entry_of(&writer, EMPTY).unwrap().root_key, root);

    let third = LigninSubtree::alone(CHILD);
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, third.clone(), Some(&staged));
    store.enter(tree_type).unwrap();
    let (mut merk, _) = open_layered(store, None, tree_type);
    apply(&mut merk, &puts(tree_type, 0..5, 25));
    let root = merk.root_key().expect("a root");
    drop(merk);
    staged.apply(&writer).unwrap().unwrap();
    let store = LigninNodeStore::immediate(&writer, third);
    assert_eq!(store.root_key().unwrap().unwrap(), None);
    let (mut merk, _) = open(Kind::Base, store, tree_type);
    assert_eq!(merk.root_key(), None);
    let applied = merk.apply::<_, Vec<u8>>(&more, &[], None, version());
    match applied.value.expect_err("a Merk with nodes and no record") {
        Error::NotSupported(said) => {
            let begins = format!(
                "lignindb: the root-key record of Merk {} is absent, and the root of that Merk \
                 is the node under the key {}: ",
                hex::encode(CHILD),
                hex::encode(&root),
            );
            assert!(said.starts_with(&begins), "{said}");
        }
        other => panic!("{other}"),
    }
    drop(merk);
    assert_eq!(entry_of(&writer, CHILD).unwrap().root_key, Some(root));

    writer.into_scope().merge();
    commit(&lignin, &transaction);
}

/// What [`read_record_with_root`] is given in the place of a transaction:
/// the transaction, with something done once after its first read of the
/// directory and once after its first read of a record.
struct Met<'a, R: Reads> {
    under: &'a R,
    after_the_root: Hook<'a>,
    after_the_record: Hook<'a>,
    records: Cell<usize>,
}

type Hook<'a> = RefCell<Option<Box<dyn FnOnce() + 'a>>>;

fn hook<'a>(run: impl FnOnce() + 'a) -> Hook<'a> {
    RefCell::new(Some(Box::new(run)))
}

fn run(hook: &Hook<'_>) {
    let run = hook.borrow_mut().take();
    if let Some(run) = run {
        run();
    }
}

impl<R: Reads> Reads for Met<'_, R> {
    fn node<T>(
        &self,
        subtree: Subtree,
        key: &[u8],
        hint: Option<NodeHandle>,
        read: impl FnOnce(Option<NodeView<'_>>) -> T,
    ) -> Result<T, TxnError> {
        self.under.node(subtree, key, hint, read)
    }

    fn committed_node<T>(
        &self,
        subtree: Subtree,
        key: &[u8],
        read: impl FnOnce(Option<NodeView<'_>>) -> T,
    ) -> Result<T, TxnError> {
        self.under.committed_node(subtree, key, read)
    }

    fn child<T>(
        &self,
        subtree: Subtree,
        parent: NodeHandle,
        side: Side,
        read: impl FnOnce(Option<NodeView<'_>>) -> T,
    ) -> Result<Option<T>, TxnError> {
        self.under.child(subtree, parent, side, read)
    }

    fn get(
        &self,
        unit: Unit,
        key: &[u8],
        committed: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        let found = self.under.get(unit, key, committed, out);
        self.records.set(self.records.get() + 1);
        run(&self.after_the_record);
        found
    }

    fn subtree<T>(
        &self,
        subtree: Subtree,
        read: impl FnOnce(Option<SubtreeView<'_>>) -> T,
    ) -> Result<T, TxnError> {
        let shown = self.under.subtree(subtree, read);
        run(&self.after_the_root);
        shown
    }

    fn by_key(&self, subtree: Subtree) -> bool {
        self.under.by_key(subtree)
    }

    fn version(&self, unit: Unit) -> CommitVersion {
        self.under.version(unit)
    }
}

/// The root is read on both sides of the record, and the reads are made
/// again where something came between them:
///
/// - a commit of another transaction that changes the root and the
///   record, between the read of the record and the second read of the
///   root;
/// - a write of the same transaction through another handle that does
///   the same;
/// - two commits of another transaction, one on each side of the read of
///   the record, that leave the root under the key it had: the roots are
///   equal, and the committed version is not.
///
/// Each time the record that is answered names the root that is
/// answered. With nothing between, the record is read once.
#[test]
fn a_change_between_the_reads_of_the_record_and_of_the_root_has_them_read_again() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let subtree = LigninSubtree::alone(FRESH);
    let tree_type = TreeType::NormalTree;
    // A commit that makes the Merk, or writes it.
    let written = |batch: &[(Vec<u8>, Op)]| {
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
        if store.entry().unwrap().is_none() {
            store.enter(tree_type).unwrap();
        }
        let (mut merk, _) = open(Kind::Base, store, tree_type);
        apply(&mut merk, batch);
        let root = merk.root_key();
        drop(merk);
        staged.apply(&writer).unwrap().unwrap();
        writer.into_scope().merge();
        commit(&lignin, &transaction);
        root
    };
    let read = |met: &Met<'_, WriteTxn<'_>>| {
        let both = read_record_with_root(met, Keyspace::Roots, &FRESH, ROOT_KEY_KEY, &FRESH);
        (both.expect("the two are read"), met.records.get())
    };
    let nothing = || RefCell::new(None);
    let first = written(&puts(tree_type, 0..20, 25)).expect("a root");

    // A commit of another transaction.
    let transaction = path.begin();
    let second = Cell::new(None);
    let met = Met {
        under: &transaction,
        after_the_root: nothing(),
        after_the_record: hook(|| second.set(written(&[(first.clone(), Op::Delete)]))),
        records: Cell::new(0),
    };
    let (both, records) = read(&met);
    drop(met);
    let second = second.take().expect("the commit between the reads");
    assert_ne!(second, first);
    assert_eq!(both, (Some(second.clone()), Some(second.clone())));
    assert_eq!(records, 2);

    // Nothing between.
    let met = Met {
        under: &transaction,
        after_the_root: nothing(),
        after_the_record: nothing(),
        records: Cell::new(0),
    };
    assert_eq!(
        read(&met),
        ((Some(second.clone()), Some(second.clone())), 1)
    );
    drop(met);

    // A write of the same transaction through another handle.
    let third = Cell::new(None);
    let met = Met {
        under: &transaction,
        after_the_root: nothing(),
        after_the_record: hook(|| {
            let store = LigninNodeStore::immediate(&transaction, subtree.clone());
            let (mut merk, _) = open(Kind::Base, store, tree_type);
            apply(&mut merk, &[(second.clone(), Op::Delete)]);
            third.set(merk.root_key());
        }),
        records: Cell::new(0),
    };
    let (both, records) = read(&met);
    drop(met);
    let third = third.take().expect("the write between the reads");
    assert_ne!(third, second);
    assert_eq!(both, (Some(third.clone()), Some(third)));
    assert_eq!(records, 2);
    transaction.rollback().unwrap();
    drop(transaction);

    // Two commits that leave the root under the key it had: the Merk of
    // one node is emptied before the read of the record, and its node is
    // put again after it.
    let all: Vec<_> = (0..20).filter(|n| be(*n) != second).collect();
    let only = written(&deletes(all.into_iter())).expect("a root");
    let one = [(only.clone(), Op::Put(vec![7; 9], feature(tree_type, 0)))];
    let transaction = path.begin();
    let met = Met {
        under: &transaction,
        after_the_root: hook(|| assert_eq!(written(&[(only.clone(), Op::Delete)]), None)),
        after_the_record: hook(|| assert_eq!(written(&one), Some(only.clone()))),
        records: Cell::new(0),
    };
    assert_eq!(read(&met), ((Some(only.clone()), Some(only.clone())), 2));
}

/// A handle that only reads, and one that stages into no batch, read the
/// root-key record and nothing else: the read is answered where a scope
/// of the transaction holds the Merk of the subtree for writing, which
/// refuses a read of the directory's entry from outside it.
#[test]
fn a_handle_that_writes_nothing_reads_the_root_key_record_alone() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let subtree = LigninSubtree::alone(FRESH);
    let tree_type = TreeType::NormalTree;
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let store = LigninNodeStore::immediate(&writer, subtree.clone());
    store.enter(tree_type).unwrap();
    let (mut merk, _) = open_layered(store, None, tree_type);
    apply(&mut merk, &puts(tree_type, 0..5, 25));
    assert!(merk.root_key().is_some());

    for reader in [
        LigninNodeStore::read_only(&transaction, subtree.clone()),
        LigninNodeStore::deferred(&transaction, subtree.clone(), None),
    ] {
        let entry = reader.entry();
        assert!(entry.is_err(), "the scope holds the Merk: {entry:?}");
        assert_eq!(reader.root_key().unwrap().unwrap(), None);
    }
    drop(merk);
    writer.into_scope().merge();
    commit(&lignin, &transaction);
}
