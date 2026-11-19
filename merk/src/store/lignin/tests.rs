use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use grovedb_costs::OperationCost;
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch};
use grovedb_version::version::GroveVersion;
use lignindb::{
    snapshot::Versions,
    txn::{
        testing::{Db, Outcome},
        WritePath, WriteTxn,
    },
};

use super::{
    access::{Access, Child, Location, Prefix, StoredNode, Write, Written},
    LigninApplied, LigninBatch, LigninHost, LigninNodeStore, LigninSubtree, LigninWriter, Mode,
};
use crate::{
    cost_model::Keyspace,
    store::{mem::MemNodeStore, NodeChange, NodeChangeSet, NodeCursor, NodeStore},
    tree::{kv::ValueDefinedCostType, AggregateData, TreeNode},
    Error, Link, Merk, Op, TreeFeatureType, TreeType,
};

const NO_VALUE_DEFINED_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> =
    None;

const ROOT: LigninSubtree = LigninSubtree::alone([0; 32]);

fn version() -> &'static GroveVersion {
    GroveVersion::latest()
}

/// A LigninDB store on disk, with the stand-in commit of the engine's own
/// transaction tests.
struct Lignin {
    db: Arc<Db>,
    versions: Versions,
}

impl Lignin {
    fn new() -> Self {
        let (db, versions) = Db::new(512);
        Self { db, versions }
    }

    fn path(&self) -> WritePath<'_> {
        self.db.path(&self.versions)
    }

    fn commit(&self, transaction: &WriteTxn<'_>) -> Outcome {
        self.db
            .commit(&self.versions, transaction)
            .expect("the transaction commits")
    }

    /// The key of the root of the Merk under `subtree`, as the subtree
    /// directory of the latest commit has it.
    fn root_in_directory(&self, subtree: Prefix) -> Option<Vec<u8>> {
        let guard = self.versions.guard();
        let pages = self.db.committed(&guard, self.versions.latest());
        let directory = self.db.directory_at(&pages);
        let mut entries = directory.entries();
        while let Some((prefix, entry)) = entries.next(&pages).expect("an entry") {
            if prefix == subtree && directory.is_live(&entry) {
                return entry.root().map(|root| root.key.to_vec());
            }
        }
        None
    }
}

fn open<'db, S: NodeStore<'db>>(store: S, tree_type: TreeType) -> Merk<S> {
    Merk::open_base(store, tree_type, NO_VALUE_DEFINED_COST, version())
        .unwrap()
        .expect("the Merk opens")
}

fn apply<'db, S: NodeStore<'db>>(merk: &mut Merk<S>, batch: &[(Vec<u8>, Op)]) -> OperationCost {
    let applied = merk.apply::<_, Vec<u8>>(batch, &[], None, version());
    applied.value.expect("the batch applies");
    applied.cost
}

/// Every node of the tree under `root`, as `(key, record)` in key order,
/// read from the store one node at a time.
fn records<'db, S: NodeStore<'db>>(store: &S, root: Option<Vec<u8>>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut found = Vec::new();
    let mut ahead: Vec<Vec<u8>> = root.into_iter().collect();
    while let Some(key) = ahead.pop() {
        let node = store
            .get_node(&key, NO_VALUE_DEFINED_COST, version())
            .unwrap()
            .expect("the node is read")
            .expect("a node that is linked is stored");
        for left in [true, false] {
            ahead.extend(node.link(left).map(|link| link.key().to_vec()));
        }
        found.push((key, node.encode()));
    }
    found.sort();
    found
}

fn assert_same_records(found: &[(Vec<u8>, Vec<u8>)], expected: &[(Vec<u8>, Vec<u8>)], at: &str) {
    use ed::Decode;

    use crate::tree::TreeNodeInner;

    let keys = |records: &[(Vec<u8>, Vec<u8>)]| -> Vec<Vec<u8>> {
        records.iter().map(|(key, _)| key.clone()).collect()
    };
    assert_eq!(keys(found), keys(expected), "the keys, {at}");
    for ((key, found), (_, expected)) in found.iter().zip(expected) {
        assert_eq!(
            TreeNodeInner::decode(found.as_slice()).unwrap(),
            TreeNodeInner::decode(expected.as_slice()).unwrap(),
            "the node under {key:?}, {at}"
        );
        assert_eq!(found, expected, "the record under {key:?}, {at}");
    }
}

struct Random(u64);

impl Random {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// The tree types a Merk has nodes of, with the feature of a node that
/// carries `n`.
const TREE_TYPES: [TreeType; 12] = [
    TreeType::NormalTree,
    TreeType::SumTree,
    TreeType::BigSumTree,
    TreeType::CountTree,
    TreeType::CountSumTree,
    TreeType::ProvableCountTree,
    TreeType::ProvableCountSumTree,
    TreeType::ProvableSumTree,
    TreeType::ProvableCountProvableSumTree,
    TreeType::ProvableCountProvableBigSumTree,
    TreeType::CountBigSumTree,
    TreeType::ProvableBigSumTree,
];

fn feature(tree_type: TreeType, n: u64) -> TreeFeatureType {
    use TreeFeatureType::*;
    let sum = (n % 4_001) as i64 - 2_000;
    let big = i128::from(sum) << 70;
    match tree_type {
        TreeType::NormalTree => BasicMerkNode,
        TreeType::SumTree => SummedMerkNode(sum),
        TreeType::BigSumTree => BigSummedMerkNode(big),
        TreeType::CountTree => CountedMerkNode(1),
        TreeType::CountSumTree => CountedSummedMerkNode(1, sum),
        TreeType::ProvableCountTree => ProvableCountedMerkNode(1),
        TreeType::ProvableCountSumTree => ProvableCountedSummedMerkNode(1, sum),
        TreeType::ProvableSumTree => ProvableSummedMerkNode(sum),
        TreeType::ProvableCountProvableSumTree => ProvableCountedAndProvableSummedMerkNode(1, sum),
        TreeType::ProvableCountProvableBigSumTree => {
            ProvableCountedAndProvableBigSummedMerkNode(1, big)
        }
        TreeType::CountBigSumTree => CountedBigSummedMerkNode(1, big),
        TreeType::ProvableBigSumTree => ProvableBigSummedMerkNode(big),
        other => panic!("no feature chosen for {other:?}"),
    }
}

fn key(n: u64) -> Vec<u8> {
    let mut key = n.to_be_bytes().to_vec();
    // Keys of several lengths, the empty key among them.
    key.truncate(8 - (n % 3) as usize);
    if n == 0 {
        key.clear();
    }
    key
}

/// A value that carries `n`: mostly short, and sometimes longer than a cell
/// holds in line, when `long` allows it.
fn value(n: u64, long: bool) -> Vec<u8> {
    let len = match n % 7 {
        0 if long => 257 + (n % 600) as usize,
        1 => 0,
        _ => 1 + (n % 120) as usize,
    };
    let mut value = n.to_be_bytes().to_vec();
    value.resize(len, n as u8);
    value
}

fn put(tree_type: TreeType, k: u64, n: u64, long: bool) -> (Vec<u8>, Op) {
    let feature = feature(tree_type, n);
    let op = match n % 11 {
        0 => Op::PutCombinedReference(value(n, long), [n as u8; 32], feature),
        1 => Op::PutWithProvidedValueHash(value(n, long), [!(n as u8); 32], None, feature),
        _ => Op::Put(value(n, long), feature),
    };
    (key(k), op)
}

/// One batch over a small key space: new keys, rewritten values, values
/// put again as they are, and removals of keys the model holds, sorted by
/// key.
fn random_batch(
    random: &mut Random,
    tree_type: TreeType,
    model: &mut BTreeMap<u64, u64>,
    long: bool,
) -> Vec<(Vec<u8>, Op)> {
    let mut batch = BTreeMap::new();
    for _ in 0..1 + random.below(24) {
        let k = random.below(300);
        let op = match (model.get(&k), random.below(10)) {
            (Some(_), 0..=2) => {
                model.remove(&k);
                (key(k), Op::Delete)
            }
            (Some(n), 3) => put(tree_type, k, *n, long),
            _ => {
                let n = random.below(1 << 40);
                model.insert(k, n);
                put(tree_type, k, n, long)
            }
        };
        batch.insert(op.0.clone(), op.1);
    }
    // Two numbers can give one key: the batch holds each key once.
    batch.into_iter().collect()
}

/// A Merk on RocksDB that is opened for each batch on a deferred context
/// and reads its nodes back from a fresh one.
struct Rocks {
    storage: TempStorage,
    tree_type: TreeType,
}

impl Rocks {
    /// Applies `batch`, and returns what the Merk and the application of
    /// the storage batch cost, with the root hash and the root key.
    fn apply(
        &self,
        batch: &[(Vec<u8>, Op)],
    ) -> (OperationCost, OperationCost, [u8; 32], Option<Vec<u8>>) {
        let transaction = self.storage.start_transaction();
        let storage_batch = StorageBatch::new();
        let context = self
            .storage
            .get_transactional_storage_context(
                SubtreePath::empty(),
                Some(&storage_batch),
                &transaction,
            )
            .unwrap();
        let mut merk = open(context, self.tree_type);
        let merk_cost = apply(&mut merk, batch);
        let (hash, root) = (merk.root_hash().unwrap(), merk.root_key());
        drop(merk);
        let applied = self
            .storage
            .commit_multi_context_batch(storage_batch, Some(&transaction));
        applied.value.expect("the storage batch is applied");
        self.storage
            .commit_transaction(transaction)
            .unwrap()
            .expect("the transaction commits");
        (merk_cost, applied.cost, hash, root)
    }

    fn records(&self, root: Option<Vec<u8>>) -> Vec<(Vec<u8>, Vec<u8>)> {
        let transaction = self.storage.start_transaction();
        let context = self
            .storage
            .get_transactional_storage_context(SubtreePath::empty(), None, &transaction)
            .unwrap();
        records(&context, root)
    }
}

/// How the Merk on LigninDB is written in a comparison.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Writing {
    /// A deferred handle and a batch for each commit, in one transaction.
    Deferred,
    /// One immediate handle and one Merk that stay open, in one
    /// transaction.
    Immediate,
    /// A deferred handle and a batch for each commit, each in a transaction
    /// of its own that is committed.
    Committed,
}

/// The same random batches for a Merk on RocksDB and for one on LigninDB.
struct Comparison {
    tree_type: TreeType,
    writing: Writing,
    seed: u64,
    random: Random,
    model: BTreeMap<u64, u64>,
    rocks: Rocks,
    round: usize,
}

/// What RocksDB gave for one batch.
struct Expected {
    merk_cost: OperationCost,
    batch_cost: OperationCost,
    root: ([u8; 32], Option<Vec<u8>>),
}

impl Comparison {
    fn new(tree_type: TreeType, writing: Writing, seed: u64) -> Self {
        Self {
            tree_type,
            writing,
            seed,
            random: Random(seed),
            model: BTreeMap::new(),
            rocks: Rocks {
                storage: TempStorage::new(),
                tree_type,
            },
            round: 0,
        }
    }

    fn at(&self) -> String {
        let Self {
            tree_type,
            writing,
            seed,
            round,
            ..
        } = self;
        format!("{tree_type:?}, {writing:?}, seed {seed}, round {round}")
    }

    /// The next batch, applied to the Merk on RocksDB.
    fn next(&mut self) -> (Vec<(Vec<u8>, Op)>, Expected) {
        self.round += 1;
        let batch = random_batch(&mut self.random, self.tree_type, &mut self.model, true);
        let (merk_cost, batch_cost, hash, root) = self.rocks.apply(&batch);
        let expected = Expected {
            merk_cost,
            batch_cost,
            root: (hash, root),
        };
        (batch, expected)
    }

    /// Both Merks have the same root hash and root key, and every node
    /// read back from LigninDB is, as a record, the record RocksDB holds:
    /// the same keys, values, features, hashes, links, heights and
    /// aggregates.
    fn check(
        &self,
        found: ([u8; 32], Option<Vec<u8>>),
        expected: &Expected,
        writer: &LigninWriter<'_, '_>,
    ) {
        let at = self.at();
        assert_eq!(found, expected.root, "the root, {at}");
        let root = found.1;
        let reader = LigninNodeStore::deferred(writer, ROOT, None);
        assert_eq!(reader.root_key().unwrap().unwrap(), root, "{at}");
        let stored = records(&reader, root.clone());
        assert_same_records(&stored, &self.rocks.records(root), &at);
    }
}

fn same_tree_as_on_rocksdb(tree_type: TreeType, writing: Writing, seed: u64, rounds: usize) {
    let mut comparison = Comparison::new(tree_type, writing, seed);
    let lignin = Lignin::new();
    let path = lignin.path();

    if writing == Writing::Immediate {
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let mut merk = open(LigninNodeStore::immediate(&writer, ROOT), tree_type);
        // What an immediate handle charges is held against a Merk that
        // stays open on an immediate context of RocksDB.
        let storage = TempStorage::new();
        let rocks_transaction = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context(SubtreePath::empty(), &rocks_transaction)
            .unwrap();
        let mut immediate = open(context, tree_type);
        for _ in 0..rounds {
            let (batch, expected) = comparison.next();
            let cost = apply(&mut merk, &batch);
            let at = comparison.at();
            assert_eq!(cost, apply(&mut immediate, &batch), "the cost, {at}");
            let found = (merk.root_hash().unwrap(), merk.root_key());
            comparison.check(found, &expected, &writer);
        }
        return;
    }

    let mut transaction = path.begin();
    let mut writer = LigninWriter::new(transaction.scope().unwrap());
    for _ in 0..rounds {
        let (batch, expected) = comparison.next();
        let at = comparison.at();
        let staged = LigninBatch::new();
        let mut merk = open(
            LigninNodeStore::deferred(&writer, ROOT, Some(&staged)),
            tree_type,
        );
        let cost = apply(&mut merk, &batch);
        assert_eq!(cost, expected.merk_cost, "what the Merk charged, {at}");
        let found = (merk.root_hash().unwrap(), merk.root_key());
        drop(merk);
        let applied = staged.apply(&writer);
        applied.value.expect("the batch is applied");
        assert_eq!(
            applied.cost, expected.batch_cost,
            "what the batch charged, {at}"
        );

        if writing == Writing::Committed {
            writer.into_scope().merge();
            assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
            let root = lignin.root_in_directory(ROOT.prefix);
            assert_eq!(root, found.1, "the root in the directory, {at}");
            transaction = path.begin();
            writer = LigninWriter::new(transaction.scope().unwrap());
        }
        comparison.check(found, &expected, &writer);
    }
}

#[test]
fn a_merk_on_lignindb_has_the_tree_it_has_on_rocksdb() {
    for (n, tree_type) in TREE_TYPES.into_iter().enumerate() {
        for writing in [Writing::Deferred, Writing::Immediate, Writing::Committed] {
            same_tree_as_on_rocksdb(tree_type, writing, 0x9E37_79B9 + n as u64, 40);
        }
    }
}

fn be(n: u64) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

fn basic(n: u64, value: &[u8]) -> (Vec<u8>, Op) {
    (
        be(n),
        Op::Put(value.to_vec(), TreeFeatureType::BasicMerkNode),
    )
}

/// Applies `batch` to a Merk that is opened on a deferred handle of
/// `subtree`, and then the batch its commit staged.
fn commit(
    writer: &LigninWriter<'_, '_>,
    subtree: &LigninSubtree,
    root: &mut Option<Vec<u8>>,
    batch: &[(Vec<u8>, Op)],
) -> LigninApplied {
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(writer, subtree.clone(), Some(&staged));
    let mut merk = open_layered(store, root.clone());
    apply(&mut merk, batch);
    *root = merk.root_key();
    drop(merk);
    staged.apply(writer).unwrap().expect("the batch is applied")
}

fn open_layered<'db, S: NodeStore<'db>>(store: S, root: Option<Vec<u8>>) -> Merk<S> {
    Merk::open_layered_with_root_key(
        store,
        root,
        TreeType::NormalTree,
        NO_VALUE_DEFINED_COST,
        version(),
    )
    .unwrap()
    .expect("the Merk opens")
}

#[test]
fn a_value_is_written_only_when_it_changed() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let (long, short) = (vec![7; 600], vec![3; 20]);
    let value = |n: u64| if n.is_multiple_of(2) { &long } else { &short };

    // New nodes: every value is written, seven of them out of line.
    let first: Vec<_> = (1..=15).map(|n| basic(n, value(n))).collect();
    let applied = commit(&writer, &ROOT, &mut root, &first);
    assert_eq!((applied.values_written, applied.values_kept), (15, 0));
    let held = transaction.stats().value_pages;
    assert!(held > 0);

    // One value changes. Its ancestors are written for their hashes, and
    // state no value at all.
    let applied = commit(&writer, &ROOT, &mut root, &[basic(1, &[9; 20])]);
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));

    // The same bytes put again, out of line and in line: Merk reports both
    // as written, and neither is.
    let handed = || transaction.stats().write_set_bytes;
    let before = handed();
    let again = [basic(2, &long), basic(3, &short)];
    let applied = commit(&writer, &ROOT, &mut root, &again);
    assert_eq!((applied.values_written, applied.values_kept), (0, 2));
    assert_eq!(transaction.stats().value_pages, held);
    let kept = handed() - before;

    // Other bytes of the same length are written: the bytes are compared,
    // and no hash.
    let mut other = long.clone();
    other[599] = 8;
    let changed = [basic(2, &other), basic(3, &[4; 20])];
    let applied = commit(&writer, &ROOT, &mut root, &changed);
    assert_eq!((applied.values_written, applied.values_kept), (2, 0));
    // Both batches write the same nodes, so the transaction was handed the
    // same bytes for them, and the bytes of the two values only now.
    let written = handed() - before - kept;
    assert_eq!(written, kept + long.len() + short.len());

    let reader = LigninNodeStore::deferred(&writer, ROOT, None);
    let read = |n: u64| {
        let node = reader.get_node(&be(n), NO_VALUE_DEFINED_COST, version());
        node.unwrap().unwrap().unwrap().value_as_slice().to_vec()
    };
    assert_eq!(
        (read(1), read(2), read(3)),
        (vec![9; 20], other, vec![4; 20])
    );
    assert_eq!(read(4), long);
}

#[test]
fn every_node_merk_writes_is_one_write_of_its_key() {
    let tree_type = TreeType::NormalTree;
    let mem = MemNodeStore::new();
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut random = Random(77);
    let mut model = BTreeMap::new();
    let mut root = None;
    let mut kinds = [0; 4];

    for round in 0..60 {
        let batch = random_batch(&mut random, tree_type, &mut model, true);
        let mut reference = open_layered(mem.clone(), root.clone());
        apply(&mut reference, &batch);
        let changes = mem.take_log();
        for (_, change) in &changes {
            kinds[match change {
                Some(NodeChange::ValueWritten) => 0,
                Some(NodeChange::StructureChanged) => 1,
                Some(NodeChange::HashOnly) => 2,
                None => 3,
            }] += 1;
        }

        // The write set of the transaction grows by one write for each
        // node Merk wrote or removed, whatever its kind, and by no other.
        let before = transaction.stats().writes;
        commit(&writer, &ROOT, &mut root, &batch);
        assert_eq!(
            transaction.stats().writes - before,
            changes.len(),
            "round {round}"
        );
        assert_eq!(root, reference.root_key());
    }
    assert!(kinds.iter().all(|seen| *seen > 20), "{kinds:?}");
}

fn reads(transaction: &WriteTxn<'_>) -> (u64, u64) {
    let stats = transaction.stats();
    (stats.keyed_reads, stats.handles_followed)
}

/// Opens the Merk under `root`, writes `value` at the bottom of it, and
/// checks which reads of that found their node by its key.
fn write_at_the_bottom(
    transaction: &WriteTxn<'_>,
    writer: &LigninWriter<'_, '_>,
    root: &Option<Vec<u8>>,
    value: &[u8],
) {
    // The root is found by its key, and each of its children through the
    // root's cell.
    let before = reads(transaction);
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(writer, ROOT, Some(&staged));
    let mut merk = open_layered(store, root.clone());
    let opened = reads(transaction);
    assert_eq!((opened.0 - before.0, opened.1 - before.1), (1, 2));

    // The write walks down through links. Each node on the way is read
    // where its parent's read saw it, and no key is looked up.
    apply(&mut merk, &[basic(0, value)]);
    let walked = reads(transaction);
    assert_eq!(
        walked.0, opened.0,
        "a node of the walk was found by its key"
    );
    assert!(walked.1 - opened.1 >= 7, "{walked:?} after {opened:?}");

    // The value the write states is compared where the node was seen.
    let applied = staged.apply(writer).unwrap().unwrap();
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));
    assert_eq!(reads(transaction), (walked.0, walked.1 + 1));

    // A handle that stays open takes in where the write put its nodes, and
    // finds a node it has never seen by its key.
    let store = &merk.storage;
    store.take_locations(&applied);
    let before = reads(transaction);
    assert!(store.has_node(&be(0)).unwrap().unwrap());
    assert_eq!(reads(transaction), (before.0, before.1 + 1));
    assert!(store.has_node(&be(199)).unwrap().unwrap());
    assert_eq!(reads(transaction), (before.0 + 1, before.1 + 1));
}

#[test]
fn a_link_is_followed_to_where_its_node_was_seen() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let nodes: Vec<_> = (0..200).map(|n| basic(n, &[n as u8; 30])).collect();
    commit(&writer, &ROOT, &mut root, &nodes);

    // In the transaction that wrote the tree, whose nodes are in its
    // private frames.
    write_at_the_bottom(&transaction, &writer, &root, b"in the frames");
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));

    // In the next one, which reads committed pages until it writes.
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    write_at_the_bottom(&transaction, &writer, &root, b"committed");
}

#[test]
fn a_committed_tree_is_read_and_written_by_the_next_transaction() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let mut root = None;
    let mut model = BTreeMap::new();
    let mut random = Random(5);
    let reference = MemNodeStore::new();

    for round in 0..12 {
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        if round % 4 == 3 {
            // A transaction that is rolled back leaves the committed tree.
            let mut lost = root.clone();
            commit(&writer, &ROOT, &mut lost, &[basic(999, b"lost")]);
            writer.into_scope().rollback();
            assert_eq!(lignin.commit(&transaction), Outcome::Empty);
            continue;
        }
        let batch = random_batch(&mut random, TreeType::NormalTree, &mut model, true);
        apply(&mut open_layered(reference.clone(), root.clone()), &batch);
        commit(&writer, &ROOT, &mut root, &batch);
        writer.into_scope().merge();
        assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));

        // A transaction that has written nothing reads the committed
        // cells, with no scope.
        let transaction = path.begin();
        let reader = LigninNodeStore::read_only(&transaction, ROOT);
        let expected = records(&reference, root.clone());
        assert_same_records(&records(&reader, root.clone()), &expected, "committed");
    }
}

#[test]
fn a_deferred_handle_reads_nothing_that_is_staged() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    assert!(staged.is_empty());
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let mut merk = open(store, TreeType::NormalTree);
    apply(&mut merk, &[basic(1, b"one"), basic(2, b"two")]);
    merk.storage
        .put_meta_record(b"m", b"meta")
        .unwrap()
        .unwrap();
    assert!(!staged.is_empty());

    let other = LigninNodeStore::deferred(&writer, ROOT, None);
    for store in [&merk.storage, &other] {
        assert!(!store.has_node(&be(1)).unwrap().unwrap());
        assert_eq!(store.root_key().unwrap().unwrap(), None);
        assert_eq!(store.get_meta_record(b"m").unwrap().unwrap(), None);
    }
    assert_eq!(transaction.stats().writes, 0);

    // A second change set of the Merk has no place in the batch.
    let again = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let mut change_set = again.change_set();
    change_set.delete_node(&be(1), None);
    let refused = again.commit(change_set).unwrap().unwrap_err();
    assert!(refused
        .to_string()
        .contains("a change set of this Merk already"));

    let root = merk.root_key();
    drop(merk);
    let shown = format!("{staged:?}");
    assert!(
        shown.contains("LigninBatch") && shown.contains("nodes: 2"),
        "{shown}"
    );
    staged.apply(&writer).unwrap().unwrap();
    assert!(staged.is_empty());
    assert!(other.has_node(&be(1)).unwrap().unwrap());
    assert_eq!(other.root_key().unwrap().unwrap(), root);
    assert_eq!(
        other.get_meta_record(b"m").unwrap().unwrap(),
        Some(b"meta".to_vec())
    );
}

#[test]
fn the_root_key_and_the_side_records_are_records_of_their_subtree() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let other = LigninSubtree::alone([9; 32]);
    let store = LigninNodeStore::immediate(&writer, ROOT);
    let elsewhere = LigninNodeStore::immediate(&writer, other.clone());

    // An immediate handle writes at once and reads what it wrote.
    store.set_root_key(Some(b"root")).unwrap().unwrap();
    store.put_meta_record(b"k", b"meta").unwrap().unwrap();
    let mut change_set = store.change_set();
    change_set.put_aux_record(b"k", b"aux", None).unwrap();
    change_set.put_aux_record(b"gone", b"x", None).unwrap();
    store.commit(change_set).unwrap().unwrap();
    let mut change_set = store.change_set();
    change_set.delete_aux_record(b"gone", None);
    store.commit(change_set).unwrap().unwrap();

    assert_eq!(store.root_key().unwrap().unwrap(), Some(b"root".to_vec()));
    assert_eq!(
        store.get_meta_record(b"k").unwrap().unwrap(),
        Some(b"meta".to_vec())
    );
    assert_eq!(
        store.get_aux_record(b"k").unwrap().unwrap(),
        Some(b"aux".to_vec())
    );
    assert_eq!(store.get_aux_record(b"gone").unwrap().unwrap(), None);
    // The keyspaces are apart, and so are the subtrees.
    assert_eq!(store.get_aux_record(b"root").unwrap().unwrap(), None);
    assert_eq!(elsewhere.root_key().unwrap().unwrap(), None);
    assert_eq!(elsewhere.get_meta_record(b"k").unwrap().unwrap(), None);
    assert_eq!(elsewhere.subtree(), &other);

    store.set_root_key(None).unwrap().unwrap();
    store.delete_meta_record(b"k").unwrap().unwrap();
    assert_eq!(store.root_key().unwrap().unwrap(), None);
    assert_eq!(store.get_meta_record(b"k").unwrap().unwrap(), None);

    // Of two writes of one record in one change set of an immediate handle
    // the later stands.
    let mut change_set = store.change_set();
    change_set.put_aux_record(b"twice", b"first", None).unwrap();
    change_set
        .put_aux_record(b"twice", b"second", None)
        .unwrap();
    change_set.put_aux_record(b"gone", b"x", None).unwrap();
    change_set.delete_aux_record(b"gone", None);
    store.commit(change_set).unwrap().unwrap();
    let aux = |key: &[u8]| store.get_aux_record(key).unwrap().unwrap();
    assert_eq!(aux(b"twice"), Some(b"second".to_vec()));
    assert_eq!(aux(b"gone"), None);

    // A deferred handle stages them, and its batch holds one outcome for a
    // record as a storage batch does: the later of two puts, and a put
    // against a delete in whichever order.
    let staged = LigninBatch::new();
    let deferred = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    deferred.set_root_key(Some(b"first")).unwrap().unwrap();
    deferred.set_root_key(Some(b"second")).unwrap().unwrap();
    deferred.put_meta_record(b"k", b"staged").unwrap().unwrap();
    deferred.delete_meta_record(b"k").unwrap().unwrap();
    deferred.delete_meta_record(b"late").unwrap().unwrap();
    deferred.put_meta_record(b"late", b"put").unwrap().unwrap();
    deferred.delete_meta_record(b"never").unwrap().unwrap();
    assert_eq!(deferred.root_key().unwrap().unwrap(), None);
    staged.apply(&writer).unwrap().unwrap();
    let meta = |key: &[u8]| deferred.get_meta_record(key).unwrap().unwrap();
    assert_eq!(meta(b"k"), Some(b"staged".to_vec()));
    assert_eq!(meta(b"late"), Some(b"put".to_vec()));
    assert_eq!(meta(b"never"), None);
    assert_eq!(
        deferred.root_key().unwrap().unwrap(),
        Some(b"second".to_vec())
    );
}

#[test]
fn a_node_written_outside_a_commit_leaves_the_root_where_it_is() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let nodes: Vec<_> = (1..=7).map(|n| basic(n, &[n as u8; 8])).collect();
    commit(&writer, &ROOT, &mut root, &nodes);
    let stored = |store: &LigninNodeStore<'_>, n: u64| {
        let node = store.get_node(&be(n), NO_VALUE_DEFINED_COST, version());
        node.unwrap().unwrap().expect("the node is stored")
    };

    // Chunk restore corrects the heights in a parent's link this way.
    let store = LigninNodeStore::immediate(&writer, ROOT);
    let mut leaf_parent = stored(&store, 2);
    let Some(Link::Reference {
        child_heights,
        hash,
        ..
    }) = leaf_parent.link(true).cloned()
    else {
        panic!("a link that was read is a reference");
    };
    assert_eq!(child_heights, (0, 0));
    leaf_parent.set_link(
        true,
        Some(Link::Reference {
            hash,
            child_heights: (3, 4),
            key: be(1),
            aggregate_data: AggregateData::NoAggregateData,
        }),
    );
    store
        .write_node(&leaf_parent, NodeChange::HashOnly)
        .unwrap()
        .unwrap();
    assert_eq!(stored(&store, 2).encode(), leaf_parent.encode());
    assert_eq!(records(&store, root.clone()).len(), 7);

    // On a deferred handle the write is staged like any other.
    let staged = LigninBatch::new();
    let deferred = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let mut corrected = stored(&deferred, 6);
    corrected.set_link(false, None);
    deferred
        .write_node(&corrected, NodeChange::StructureChanged)
        .unwrap()
        .unwrap();
    assert!(stored(&deferred, 6).link(false).is_some());
    staged.apply(&writer).unwrap().unwrap();
    assert!(stored(&deferred, 6).link(false).is_none());
    assert_eq!(records(&deferred, root.clone()).len(), 6);

    // Neither write stated a root: the directory still names the one the
    // Merk's commit stated.
    drop((store, deferred));
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    assert_eq!(root, Some(be(4)));
    assert_eq!(lignin.root_in_directory(ROOT.prefix), root);
}

#[test]
fn a_host_names_the_merk_it_hosts_when_one_batch_writes_both() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let parent = LigninSubtree::alone([1; 32]);
    // A subtree the store was not made with: the batch that writes its
    // host enters it.
    let child = |hosted: bool| LigninSubtree {
        prefix: [222; 32],
        host: hosted.then(|| LigninHost {
            subtree: parent.prefix,
            key: be(50),
            hosted: vec![[222; 32]],
        }),
    };
    let reference = MemNodeStore::new();
    let (mut parent_root, mut child_root) = (None, None);
    // Writes elements of the parent Merk and nodes of the child Merk in
    // one batch.
    let mut entered = false;
    let mut write = |child: LigninSubtree, elements: &[(Vec<u8>, Op)], nodes: &[(Vec<u8>, Op)]| {
        let staged = LigninBatch::new();
        if !std::mem::replace(&mut entered, true) {
            staged.enter(&child, b"tree")?;
        }
        let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(&staged));
        let mut upper = open_layered(upper, parent_root.clone());
        apply(&mut upper, elements);
        let lower = LigninNodeStore::deferred(&writer, child, Some(&staged));
        let mut lower = open_layered(lower, child_root.clone());
        apply(&mut lower, nodes);
        let roots = (upper.root_key(), lower.root_key());
        drop((upper, lower));
        staged.apply(&writer).unwrap()?;
        apply(
            &mut open_layered(reference.clone(), child_root.clone()),
            nodes,
        );
        (parent_root, child_root) = roots;
        Ok::<_, Error>(())
    };
    let nodes = |from: u64| -> Vec<_> { (from..from + 9).map(|n| basic(n, b"below")).collect() };

    // The element under key 50 hosts the child Merk. The first batch
    // writes it with the Merk's first nodes, and the second writes it
    // again when the Merk has another root.
    let elements: Vec<_> = (40..=60).map(|n| basic(n, b"element")).collect();
    write(child(true), &elements, &nodes(0)).unwrap();
    write(child(true), &[basic(50, b"again")], &nodes(100)).unwrap();

    // The engine holds a writer to it: a batch that writes the host and
    // the Merk it hosts, and leaves the Merk out of the host, is refused,
    // and nothing of it is written.
    let unnamed = write(child(false), &[basic(50, b"unnamed")], &nodes(200)).unwrap_err();
    assert!(matches!(unnamed, Error::StorageError(_)));
    assert!(
        unnamed.to_string().contains("keeps its pointers"),
        "{unnamed}"
    );

    // A write of the Merk that does not write its host is anchored, and is
    // taken.
    let alone = LigninNodeStore::immediate(&writer, child(true));
    let mut merk = open_layered(alone, child_root.clone());
    apply(&mut merk, &nodes(300));
    apply(
        &mut open_layered(reference.clone(), child_root.clone()),
        &nodes(300),
    );
    let child_root = merk.root_key();
    drop(merk);

    let reader = LigninNodeStore::deferred(&writer, child(true), None);
    let expected = records(&reference, child_root.clone());
    assert_eq!(expected.len(), 27);
    assert_same_records(&records(&reader, child_root), &expected, "the hosted Merk");
    let upper = LigninNodeStore::deferred(&writer, parent, None);
    let host = upper.get_node(&be(50), NO_VALUE_DEFINED_COST, version());
    assert_eq!(host.unwrap().unwrap().unwrap().value_as_slice(), b"again");
}

#[test]
fn a_merk_that_is_entered_with_no_node_is_written_by_a_later_batch() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let parent = LigninSubtree::alone([1; 32]);
    let child = LigninSubtree {
        prefix: [222; 32],
        host: Some(LigninHost {
            subtree: parent.prefix,
            key: be(50),
            hosted: vec![[222; 32]],
        }),
    };
    let reference = MemNodeStore::new();
    let (mut parent_root, mut child_root) = (None, None);
    let check = |root: &Option<Vec<u8>>| {
        let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
        let expected = records(&reference, root.clone());
        assert_same_records(
            &records(&reader, root.clone()),
            &expected,
            "the hosted Merk",
        );
    };

    // The batch that writes the element enters the Merk it hosts, which
    // has no node yet.
    let staged = LigninBatch::new();
    staged.enter(&child, b"tree").unwrap();
    let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(&staged));
    let mut upper = open_layered(upper, parent_root.clone());
    apply(&mut upper, &[basic(50, b"an empty tree")]);
    parent_root = upper.root_key();
    drop(upper);
    staged
        .apply(&writer)
        .unwrap()
        .expect("the batch is applied");
    let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
    assert!(!reader.has_node(&be(0)).unwrap().unwrap());

    // Its first nodes come in a batch that does not write the element, and
    // the next in one that does.
    let first: Vec<_> = (0..9).map(|n| basic(n, b"below")).collect();
    commit(&writer, &child, &mut child_root, &first);
    apply(&mut open_layered(reference.clone(), None), &first);
    check(&child_root);

    let next: Vec<_> = (100..109).map(|n| basic(n, b"below")).collect();
    let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(&staged));
    let mut upper = open_layered(upper, parent_root);
    apply(&mut upper, &[basic(50, b"a tree")]);
    drop(upper);
    let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(&staged));
    let mut lower = open_layered(lower, child_root.clone());
    apply(&mut lower, &next);
    apply(&mut open_layered(reference.clone(), child_root), &next);
    let child_root = lower.root_key();
    drop(lower);
    staged
        .apply(&writer)
        .unwrap()
        .expect("the batch is applied");
    check(&child_root);
    assert_eq!(records(&reference, child_root).len(), 18);
}

#[test]
fn a_subtree_that_a_batch_drops_and_enters_again_is_written_whole() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let parent = LigninSubtree::alone([1; 32]);
    let host = |hosted: &[[u8; 32]]| LigninHost {
        subtree: parent.prefix,
        key: be(50),
        hosted: hosted.to_vec(),
    };
    let child = LigninSubtree {
        prefix: [222; 32],
        host: Some(host(&[[222; 32]])),
    };
    let nodes = |keys: std::ops::Range<u64>| -> Vec<_> {
        let nodes = keys.map(|n| basic(n, b"below"));
        nodes.collect()
    };
    let mut parent_root = None;
    // Writes the element that hosts the child, after `subtrees` has said
    // what the batch does to whole subtrees.
    let mut element = |staged: &LigninBatch, value: &[u8]| {
        let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(staged));
        let mut upper = open_layered(upper, parent_root.clone());
        apply(&mut upper, &[basic(50, value)]);
        parent_root = upper.root_key();
    };
    // Writes `keys` as a Merk that is new under the child's prefix, on a
    // handle that has seen the nodes the transaction holds there.
    let below = |staged: &LigninBatch, keys: std::ops::Range<u64>| {
        let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(staged));
        for key in keys.clone() {
            let _ = lower.get_node(&be(key), NO_VALUE_DEFINED_COST, version());
        }
        let mut lower = open_layered(lower, None);
        apply(&mut lower, &nodes(keys));
        lower.root_key()
    };
    let check = |root: Option<Vec<u8>>, keys: std::ops::Range<u64>| {
        let reference = MemNodeStore::new();
        apply(&mut open_layered(reference.clone(), None), &nodes(keys));
        let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
        let expected = records(&reference, root.clone());
        assert_same_records(&records(&reader, root), &expected, "the hosted Merk");
        expected.len()
    };
    let apply_staged = |staged: &LigninBatch| {
        let applied = staged
            .apply(&writer)
            .unwrap()
            .expect("the batch is applied");
        (applied.values_written, applied.values_kept)
    };

    let staged = LigninBatch::new();
    staged.enter(&child, b"tree").unwrap();
    element(&staged, b"first");
    let root = below(&staged, 0..9);
    assert_eq!(apply_staged(&staged), (10, 0));
    assert_eq!(check(root, 0..9), 9);

    // The subtree is dropped and entered again, and the Merk under it has
    // the nodes it had. They went with the drop, so each is written whole,
    // and where the handle saw one is no place of a node any more.
    staged.drop_subtree(&child);
    staged.enter(&child, b"tree").unwrap();
    element(&staged, b"again");
    let root = below(&staged, 0..9);
    assert_eq!(apply_staged(&staged), (10, 0));
    assert_eq!(check(root, 0..9), 9);

    // A batch that writes the Merk and does not enter its subtree keeps
    // what it holds.
    let root = below(&staged, 0..9);
    assert_eq!(apply_staged(&staged), (0, 9));
    assert_eq!(check(root, 0..9), 9);

    // The batch that drops the subtree writes its host without it. From
    // then on nothing is read under the subtree, and a change set of it is
    // refused.
    staged.drop_subtree(&LigninSubtree {
        prefix: child.prefix,
        host: Some(host(&[])),
    });
    element(&staged, b"an item");
    assert_eq!(apply_staged(&staged), (1, 0));
    let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
    assert!(!reader.has_node(&be(5)).unwrap().unwrap());
    below(&staged, 0..2);
    let gone = staged.apply(&writer).unwrap().unwrap_err();
    assert!(matches!(gone, Error::StorageError(_)));
    assert!(gone.to_string().contains("is not entered"), "{gone}");
    assert!(staged.is_empty());
}

#[test]
fn a_handle_that_only_reads_refuses_every_write() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let node = TreeNode::new(be(1), b"v".to_vec(), None, TreeFeatureType::BasicMerkNode).unwrap();
    let refused = |result: grovedb_costs::CostResult<(), Error>| {
        assert_eq!(result.cost, OperationCost::default());
        let error = result.value.unwrap_err();
        assert!(matches!(error, Error::StorageError(_)), "{error:?}");
        assert!(
            error.to_string().contains("a handle that only reads"),
            "{error}"
        );
    };
    let try_writes = |store: &LigninNodeStore<'_>| {
        refused(store.commit(store.change_set()));
        refused(store.write_node(&node, NodeChange::ValueWritten));
        refused(store.set_root_key(Some(b"k")));
        refused(store.put_meta_record(b"k", b"v"));
        refused(store.delete_meta_record(b"k"));
    };

    // A handle on the transaction itself, with no scope.
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    try_writes(&reader);
    assert!(format!("{reader:?}").starts_with("LigninNodeStore"));

    // A deferred handle that was opened with no batch.
    let writer = LigninWriter::new(transaction.scope().unwrap());
    assert!(format!("{writer:?}").starts_with("LigninWriter"));
    try_writes(&LigninNodeStore::deferred(&writer, ROOT, None));

    // A read made outside every scope while one is open is the read of a
    // scope that ends with the call (`transactions.md` SC3). Where no open
    // scope holds the unit, it is made.
    let outside = LigninNodeStore::read_only(&transaction, ROOT);
    let seek = OperationCost {
        seek_count: 1,
        ..Default::default()
    };
    let link = Link::Reference {
        hash: [0; 32],
        child_heights: (0, 0),
        key: be(1),
        aggregate_data: AggregateData::NoAggregateData,
    };
    let read = outside.get_node(&be(1), NO_VALUE_DEFINED_COST, version());
    assert_eq!(read.cost, seek);
    assert!(read.value.unwrap().is_none());
    let read = outside.root_key();
    assert_eq!(read.cost, seek);
    assert_eq!(read.value.unwrap(), None);
    let read = outside.get_aux_record(b"k");
    assert_eq!(read.cost, seek);
    assert_eq!(read.value.unwrap(), None);

    // The scope writes a node, the root key and an aux record of the
    // subtree, and holds those three units from then on. A read of one of
    // them from outside is refused. A read that was not performed is a
    // storage error, which names the unit, and is charged as a lookup that
    // found nothing.
    commit(&writer, &ROOT, &mut None, &[basic(2, b"v")]);
    let holder = LigninNodeStore::immediate(&writer, ROOT);
    holder.set_root_key(Some(b"k")).unwrap().unwrap();
    let mut change_set = holder.change_set();
    change_set.put_aux_record(b"k", b"aux", None).unwrap();
    holder.commit(change_set).unwrap().unwrap();
    let not_read = |cost: OperationCost, error: Error, column: &str| {
        assert_eq!(cost, seek);
        assert!(matches!(error, Error::StorageError(_)), "{error:?}");
        let text = error.to_string();
        assert!(text.contains("another open scope"), "{text}");
        assert!(text.contains(&format!("holds {column} of 0000")), "{text}");
    };
    let read = outside.get_node(&be(1), NO_VALUE_DEFINED_COST, version());
    not_read(read.cost, read.value.unwrap_err(), "data");
    let read = outside.get_node_for_read(&be(1));
    not_read(read.cost, read.value.unwrap_err(), "data");
    let read = outside.has_node(&be(1));
    not_read(read.cost, read.value.unwrap_err(), "data");
    let read = outside.root_key();
    not_read(read.cost, read.value.unwrap_err(), "roots");
    let read = outside.get_aux_record(b"k");
    not_read(read.cost, read.value.unwrap_err(), "aux");
    let read = outside.fetch_node(&link, NO_VALUE_DEFINED_COST.as_ref(), version());
    not_read(read.cost, read.value.unwrap_err(), "data");
    // A unit that the scope does not hold is read from outside as before:
    // another column of the subtree, and another subtree.
    let read = outside.get_meta_record(b"k");
    assert_eq!(read.cost, seek);
    assert_eq!(read.value.unwrap(), None);
    let elsewhere = LigninNodeStore::read_only(&transaction, LigninSubtree::alone([9; 32]));
    let read = elsewhere.has_node(&be(1));
    assert_eq!(read.cost, seek);
    assert!(!read.value.unwrap());

    // A link whose key has no node is a key that was not found, charged as
    // the lookup.
    let inside = LigninNodeStore::deferred(&writer, ROOT, None);
    let missing = inside.fetch_node(&link, NO_VALUE_DEFINED_COST.as_ref(), version());
    assert_eq!(missing.cost, seek);
    assert!(matches!(
        missing.value,
        Err(Error::KeyNotFoundError("Key not found for fetch"))
    ));
    // An immediate handle reads for nothing, as an immediate storage
    // context does.
    let immediate = LigninNodeStore::immediate(&writer, ROOT);
    let absent = immediate.get_node(&be(1), NO_VALUE_DEFINED_COST, version());
    assert_eq!(absent.cost, OperationCost::default());
    assert!(absent.value.unwrap().is_none());
}

#[test]
fn the_cursor_is_not_built_and_says_so() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let mut cursor = LigninNodeStore::read_only(&transaction, ROOT).cursor();
    cursor.seek_to_first().unwrap();
    cursor.seek_to_last().unwrap();
    cursor.seek(b"k").unwrap();
    cursor.seek_for_prev(b"k").unwrap();
    cursor.next().unwrap();
    cursor.prev().unwrap();
    assert!(!cursor.valid().unwrap());
    assert_eq!(cursor.key().unwrap(), None);
    assert_eq!(cursor.value().unwrap(), None);
    let status = cursor.status().unwrap_err();
    assert!(status.to_string().contains("not built yet"), "{status}");
    assert_eq!(format!("{cursor:?}"), "LigninCursor");
}

#[test]
fn a_merk_on_lignindb_moves_to_another_thread_and_is_written_there() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let reference = MemNodeStore::new();
    let first: Vec<_> = (0..50).map(|n| basic(n, b"here")).collect();
    let second: Vec<_> = (25..75).map(|n| basic(n, b"there")).collect();
    let mut expected = open(reference.clone(), TreeType::NormalTree);
    apply(&mut expected, &first);
    apply(&mut expected, &second);

    let mut merk = open(
        LigninNodeStore::immediate(&writer, ROOT),
        TreeType::NormalTree,
    );
    apply(&mut merk, &first);
    let merk = std::thread::scope(|threads| {
        let there = threads.spawn(move || {
            apply(&mut merk, &second);
            merk
        });
        there.join().expect("the other thread wrote the Merk")
    });
    assert_eq!(merk.root_hash().unwrap(), expected.root_hash().unwrap());
    let root = merk.root_key();
    assert_same_records(
        &records(&merk.storage, root.clone()),
        &records(&reference, root),
        "after the move",
    );
}

/// A transaction that answers a number of reads in its own way before it
/// passes them on.
struct Unsteady<'a> {
    under: &'a dyn Access,
    stale: AtomicUsize,
    missing: AtomicUsize,
}

fn take(left: &AtomicUsize) -> bool {
    left.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
        left.checked_sub(1)
    })
    .is_ok()
}

impl Access for Unsteady<'_> {
    fn node(
        &self,
        subtree: &Prefix,
        key: &[u8],
        hint: Option<Location>,
        with_value: bool,
    ) -> Result<Option<StoredNode>, Error> {
        self.under.node(subtree, key, hint, with_value)
    }

    fn committed_node(&self, subtree: &Prefix, key: &[u8]) -> Result<Option<StoredNode>, Error> {
        self.under.committed_node(subtree, key)
    }

    fn child(&self, subtree: &Prefix, parent: Location, left: bool) -> Result<Child, Error> {
        if take(&self.stale) {
            return Ok(Child::Stale);
        }
        if take(&self.missing) {
            return Ok(Child::Missing);
        }
        self.under.child(subtree, parent, left)
    }

    fn record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.under.record(keyspace, subtree, key)
    }

    fn committed_record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.under.committed_record(keyspace, subtree, key)
    }

    fn apply(&self, write: &Write) -> Result<Written, Error> {
        self.under.apply(write)
    }
}

#[test]
fn a_node_whose_location_went_stale_between_two_reads_is_read_again() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let nodes: Vec<_> = (1..=7).map(|n| basic(n, &[n as u8; 8])).collect();
    commit(&writer, &ROOT, &mut root, &nodes);
    let expected = records(
        &LigninNodeStore::deferred(&writer, ROOT, None),
        root.clone(),
    );
    let unsteady = |stale: usize, missing: usize| Unsteady {
        under: &writer,
        stale: AtomicUsize::new(stale),
        missing: AtomicUsize::new(missing),
    };

    // A commit that lands between the read of a cell and the read of its
    // child ends the cell's location: the node is read again, and is whole.
    let transaction_view = unsteady(5, 0);
    let store = LigninNodeStore::new(&transaction_view, ROOT, Mode::Deferred(None));
    assert_same_records(&records(&store, root.clone()), &expected, "read again");

    // It is not read again for ever.
    let transaction_view = unsteady(usize::MAX, 0);
    let store = LigninNodeStore::new(&transaction_view, ROOT, Mode::Deferred(None));
    let read = store.get_node(&be(4), NO_VALUE_DEFINED_COST, version());
    assert_eq!(read.cost.seek_count, 1);
    let error = read.value.unwrap_err();
    assert!(matches!(error, Error::StorageError(_)));
    assert!(
        error.to_string().contains("changed under every read"),
        "{error}"
    );

    // A link that leads to no node is damage, and is reported.
    let transaction_view = unsteady(0, 1);
    let store = LigninNodeStore::new(&transaction_view, ROOT, Mode::Deferred(None));
    let error = store.get_node_for_read(&be(4)).unwrap().unwrap_err();
    assert!(
        error.to_string().contains("a link leads to no node"),
        "{error}"
    );
}

#[test]
fn a_commit_that_changes_nothing_writes_what_merk_writes() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;

    // An empty Merk that stays empty states its root and writes no node:
    // nothing reaches the transaction, but for an aux record of the commit.
    commit(&writer, &ROOT, &mut root, &[]);
    assert_eq!((root.clone(), transaction.stats().writes), (None, 0));
    let put = Op::Put(b"aux".to_vec(), TreeFeatureType::BasicMerkNode);
    for (immediate, key) in [(false, b"a"), (true, b"b")] {
        let staged = LigninBatch::new();
        let store = match immediate {
            true => LigninNodeStore::immediate(&writer, ROOT),
            false => LigninNodeStore::deferred(&writer, ROOT, Some(&staged)),
        };
        let mut merk = open_layered(store, None);
        let aux = [(key.to_vec(), put.clone(), None)];
        let applied = merk.apply::<Vec<u8>, _>(&[], &aux, None, version());
        applied
            .unwrap()
            .expect("the commit of an empty Merk is taken");
        staged.apply(&writer).unwrap().unwrap();
        let stored = merk.storage.get_aux_record(key).unwrap().unwrap();
        assert_eq!(stored, Some(b"aux".to_vec()));
        // The Merk still has its place in the next batch.
        let again = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
        apply(&mut open_layered(again, None), &[]);
        staged.apply(&writer).unwrap().unwrap();
    }
    let aux_writes = transaction.stats().writes;
    assert_eq!(aux_writes, 2);
    // One that keeps its own root key deletes the record of it, as it does
    // on RocksDB, and that is its one write.
    let base = LigninSubtree::alone([5; 32]);
    let mut merk = open(
        LigninNodeStore::immediate(&writer, base),
        TreeType::NormalTree,
    );
    apply(&mut merk, &[]);
    drop(merk);
    assert_eq!(transaction.stats().writes, aux_writes + 1);

    // A Merk with nodes writes its root on every commit, and that is one
    // write of the root's key although no byte changes.
    commit(&writer, &ROOT, &mut root, &[basic(1, b"one")]);
    let before = transaction.stats().writes;
    let applied = commit(&writer, &ROOT, &mut root, &[]);
    assert_eq!(transaction.stats().writes - before, 1);
    assert_eq!((applied.values_written, applied.values_kept), (0, 0));
}
