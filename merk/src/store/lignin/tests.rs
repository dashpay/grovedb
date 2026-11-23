mod cost;

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use grovedb_costs::OperationCost;
use grovedb_path::SubtreePath;
use grovedb_storage::{
    error::WriteScopeError, rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch,
};
use grovedb_version::version::GroveVersion;
use lignin_format::{CommitVersion, PageSize};
use lignin_io::{StdVfs, Vfs};
use lignin_tree::{
    hash::{HashRules, Refusal},
    node::NodeRef,
};
use lignindb::{
    commit::{Committed, NoStaleHashes},
    txn::{Batch, Subtree, SubtreeWrite, WriteTxn},
    Db, Options,
};

use super::{
    access::{
        Access, BatchPart, Child, LigninEntryRefused, Location, Prefix, StoredNode, SubtreeChange,
        Walk, Write, Written, AFTER_COMPARING,
    },
    flat::{FlatAccess, FlatWalk},
    hash::MerkHashRules,
    tree_type_bytes, LigninApplied, LigninBatch, LigninEntry, LigninHost, LigninNodeStore,
    LigninOutcome, LigninSettled, LigninSubtree, LigninView, LigninWriter, Mode,
};
use crate::{
    cost_model::Keyspace,
    store::{mem::MemNodeStore, NodeChange, NodeChangeSet, NodeCursor, NodeStore},
    tree::{kv::ValueDefinedCostType, value_hash, AggregateData, TreeNode},
    CryptoHash, Error, Link, Merk, Op, TreeFeatureType, TreeType,
};

mod chunk;
mod cursor;
mod differences;
mod edges;
mod large;
mod proof;
mod unlinked;
mod views;

pub(super) const NO_VALUE_DEFINED_COST: Option<
    fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
> = None;

const ROOT: LigninSubtree = LigninSubtree::alone([0; 32]);

pub(super) fn version() -> &'static GroveVersion {
    GroveVersion::latest()
}

/// The rules by which the store of a test has its hashes recomputed when
/// it is dropped (`lignindb::verify`): Merk's for the key-value hash and
/// the hash of a node.
///
/// The values of these tests are no elements of GroveDB, so Merk's rule
/// for the value hash of an element does not apply to them. A store has
/// one rule for every value hash in it, chosen when it is made:
///
/// - [`Lignin::new`]: a value hash is the hash of the value's bytes, which
///   is what `Op::Put` and `Op::PutWithSpecializedCost` give a value.
/// - [`Lignin::with_stated_value_hashes`]: a value hash is taken as
///   stored, for a test that writes with an operation which states it.
///   That holds for every value of the store, also for one that was put
///   plainly.
struct RawValues {
    rules: MerkHashRules<'static>,
    stated: bool,
}

impl HashRules for RawValues {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        _: &[Option<CryptoHash>],
    ) -> Result<CryptoHash, Refusal> {
        Ok(match self.stated {
            true => *node.value_hash(),
            false => value_hash(value).unwrap(),
        })
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &CryptoHash) -> Result<CryptoHash, Refusal> {
        self.rules.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<CryptoHash, Refusal> {
        self.rules.node_hash(node)
    }
}

/// The size of the pages of the databases of these tests.
pub(super) const SIZE: PageSize = PageSize::MIN;

/// The tree type the Merks of a database of these tests are entered with
/// when it is made.
const TREE_TYPE: &[u8] = b"merk";

/// The subtrees that every database of these tests has entered when it is
/// made, as Merks that stand alone: those whose 32 bytes are all one byte
/// below this. A test that enters a subtree itself names another.
const ENTERED: u8 = 200;

/// How a commit of a write transaction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The transaction wrote nothing: no version was taken.
    Empty,
    /// A commit outside the transaction wrote a key after the transaction
    /// first wrote it.
    Conflict,
    /// The commit was published as this version.
    Committed(CommitVersion),
}

/// A LigninDB database in a directory of its own, opened as a user of the
/// engine opens one (`lignindb::Db`), with the subtrees of these tests
/// entered.
///
/// It is closed when it is dropped, and the engine checks it then, in a
/// debug build, with the rules of [`RawValues`]. Then it is opened once
/// more, as the next session would open it: the open lands on the version
/// the session ended at, and what it opens is sound by the same rules.
pub(super) struct Lignin {
    /// The database, until the wrapper is dropped.
    db: Option<Db>,
    options: Options,
    stated: bool,
    dir: tempfile::TempDir,
}

impl Drop for Lignin {
    fn drop(&mut self) {
        let Some(db) = self.db.take() else { return };
        // A test that is failing says why itself, and a session that ended
        // for writes can have left a commit in the log that it did not
        // publish.
        if std::thread::panicking() || db.reopen().is_some() {
            return;
        }
        let ended_at = db.version();
        db.close().expect("the database is closed");
        let again = Db::open(self.dir.path(), &self.options);
        let again = again.expect("the database is opened again");
        assert_eq!(
            again.version(),
            ended_at,
            "the version the next open lands on"
        );
        let rules = MerkHashRules::for_check(version());
        let mut rules = RawValues {
            rules,
            stated: self.stated,
        };
        let report = again.close_and_verify(Some(&mut rules));
        let report = report.expect("the database is closed again");
        assert!(
            report.is_sound(),
            "the database is not sound when it is opened again:\n{report}"
        );
    }
}

impl Lignin {
    pub(super) fn new() -> Self {
        Self::checked(false)
    }

    /// A store for a test that writes with operations which state a value
    /// hash: the check at its drop takes every value hash as stored.
    pub(super) fn with_stated_value_hashes() -> Self {
        Self::checked(true)
    }

    fn checked(stated: bool) -> Self {
        Self::on(Arc::new(StdVfs), stated)
    }

    /// A database whose files are `vfs`'s, in a new directory of the file
    /// system.
    pub(super) fn on(vfs: Arc<dyn Vfs>, stated: bool) -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let options = Options {
            page_size: Some(SIZE),
            cache_size: 512 * (SIZE.get() + 512),
            vfs,
            ..Options::default()
        };
        let db = Db::open(dir.path(), &options).expect("a new database");
        let rules = MerkHashRules::for_check(version());
        db.check_hashes_by(Box::new(RawValues { rules, stated }));
        let entered: Vec<SubtreeWrite<'_>> = (0..ENTERED)
            .map(|name| SubtreeWrite::Enter {
                subtree: Subtree::new([name; 32]),
                tree_type: TREE_TYPE,
                host: None,
            })
            .collect();
        let batch = Batch {
            subtrees: &entered,
            ..Batch::default()
        };
        let made = db.write(&batch, &mut NoStaleHashes);
        assert!(matches!(made, Ok(Committed::Published(_))), "{made:?}");
        Self {
            db: Some(db),
            options,
            stated,
            dir,
        }
    }

    /// The database: a write transaction is begun from it.
    pub(super) fn path(&self) -> &Db {
        self.db.as_ref().expect("the database is open")
    }

    /// Closes the database and opens it again. A close writes back what
    /// the session had to write, so the next session begins with every
    /// value that was out of line in the value log, whatever the
    /// background threads of the first had got to.
    pub(super) fn reopened(mut self) -> Self {
        let db = self.db.take().expect("the database is open");
        let ended_at = db.version();
        db.close().expect("the database is closed");
        let db = Db::open(self.dir.path(), &self.options);
        let db = db.expect("the database is opened again");
        assert_eq!(db.version(), ended_at, "the version the open lands on");
        let rules = MerkHashRules::for_check(version());
        let stated = self.stated;
        db.check_hashes_by(Box::new(RawValues { rules, stated }));
        self.db = Some(db);
        self
    }

    /// Commits `transaction` as GroveDB does on existing GroveVersions:
    /// with Merk's rules for the hashes a writer left out.
    pub(super) fn commit(&self, transaction: &WriteTxn<'_>) -> Outcome {
        let stale = transaction.stats().stale_nodes;
        let before = self.path().version();
        match (self.path()).commit(transaction, &mut MerkHashRules::new(version())) {
            Ok(Committed::Empty) => Outcome::Empty,
            Ok(Committed::Published(published)) => {
                assert_eq!(published.evaluated, stale);
                assert!(published.version > before, "a commit takes a later version");
                assert!(published.miscounted.is_none(), "a value is miscounted");
                Outcome::Committed(published.version)
            }
            Err(error) if error.is_conflict() => Outcome::Conflict,
            Err(error) => panic!("the transaction commits: {error}"),
        }
    }

    /// The key of the root of the Merk under `subtree`, as a reader of
    /// the latest version finds it in the subtree's entry.
    fn root_in_directory(&self, subtree: Prefix) -> Option<Vec<u8>> {
        let snapshot = self.path().snapshot();
        let operation = snapshot.operation();
        let opened = operation.open(Subtree::new(subtree)).expect("a lookup")?;
        let root = opened.entry().root();
        root.map(|root| root.key.to_vec())
    }
}

fn open<'db, S: NodeStore<'db>>(store: S, tree_type: TreeType) -> Merk<S> {
    Merk::open_base(store, tree_type, NO_VALUE_DEFINED_COST, version())
        .unwrap()
        .expect("the Merk opens")
}

pub(super) fn apply<'db, S: NodeStore<'db>>(
    merk: &mut Merk<S>,
    batch: &[(Vec<u8>, Op)],
) -> OperationCost {
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
    let lignin = Lignin::with_stated_value_hashes();
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
    let lignin = Lignin::with_stated_value_hashes();
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
    let lignin = Lignin::with_stated_value_hashes();
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

    // A second change set of the Merk joins the batch, which holds one
    // outcome for each key: inside one operation the put of a node stands
    // against a delete of it.
    let again = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let mut change_set = again.change_set();
    change_set.delete_node(&be(1), None);
    again.commit(change_set).unwrap().unwrap();

    let root = merk.root_key();
    drop(merk);
    let shown = format!("{staged:?}");
    assert!(
        shown.contains("LigninBatch") && shown.contains("written: 1"),
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

    // Chunk restore corrects the heights in a parent's link this way: the
    // link is written with heights that are not the child's, and then with
    // the child's.
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
    for child_heights in [(3, 4), child_heights] {
        leaf_parent.set_link(
            true,
            Some(Link::Reference {
                hash,
                child_heights,
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
    }
    drop(store);
    writer.into_scope().merge();

    // On a deferred handle the write is staged like any other. A node that
    // is written alone cannot take a child out of a tree: the cell of the
    // child stays, and the node above states the hash and the heights of
    // what was there. So this write is made in a scope of its own, which
    // is rolled back.
    let writer = LigninWriter::new(transaction.scope().unwrap());
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
    let entry = deferred.entry().unwrap().expect("the subtree is entered");
    assert_eq!(entry.root_key, root);
    drop(deferred);
    writer.into_scope().rollback();
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    assert!(stored(&reader, 6).link(false).is_some());

    // No write stated a root: the directory still names the one the Merk's
    // commit stated.
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    assert_eq!(root, Some(be(4)));
    assert_eq!(lignin.root_in_directory(ROOT.prefix), root);
}

/// What a settled batch hands the transaction once it is told what an
/// element hosts ([`LigninSettled::host`]): the node of the element names
/// every Merk of the list where the batch puts it, and a Merk of the list
/// is anchored exactly where the batch does not. Handles that name two
/// elements for one subtree are told apart from handles that agree.
#[test]
fn a_settled_batch_is_told_what_an_element_hosts() {
    use lignindb::txn::Hosted;

    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let parent = LigninSubtree::alone([1; 32]);
    let (first, second) = ([221; 32], [222; 32]);
    let host = |key: u64, hosted: &[Prefix]| LigninHost {
        subtree: parent.prefix,
        key: be(key),
        hosted: hosted.to_vec(),
    };
    // A handle names the first Merk of its element alone.
    let named = |prefix: Prefix, key: u64| LigninSubtree {
        prefix,
        host: Some(host(key, &[first])),
    };
    // A batch that writes a node of the second Merk, with `element` the
    // node of its host, and with `other` the second Merk again through a
    // handle that names the element under that key.
    let settled = |element: bool, other: Option<u64>| {
        let staged = LigninBatch::new();
        if element {
            let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(&staged));
            apply(&mut open_layered(upper, None), &[basic(50, b"element")]);
        }
        let lower = LigninNodeStore::deferred(&writer, named(second, 50), Some(&staged));
        apply(&mut open_layered(lower, None), &[basic(1, b"below")]);
        if let Some(key) = other {
            let again = LigninNodeStore::deferred(&writer, named(second, key), Some(&staged));
            (again.put_meta_record(b"k", b"v").unwrap()).expect("a staged record");
        }
        staged.settle(&writer).unwrap().expect("a settled batch")
    };
    // What the transaction is handed: the Merks the node of the element
    // names, if the batch puts it, and whether the second Merk is anchored.
    let handed = |settled: &LigninSettled| {
        let mut seen = None;
        let run = &mut |batch: &lignindb::txn::Batch<'_>| {
            let of = |prefix: Prefix| {
                let mut merks = batch.merks.iter();
                merks.find(|merk| merk.subtree == Subtree::new(prefix))
            };
            let element = of(parent.prefix)
                .and_then(|merk| merk.nodes.iter().find(|node| node.key == be(50)));
            let names = element.map(|node| match node.hosted {
                Hosted::Set(hosted) => Some(hosted.to_vec()),
                Hosted::Keep => None,
            });
            seen = Some((names, of(second).expect("the second Merk").anchored));
            Ok(Written::default())
        };
        BatchPart::with_batch(settled, run).expect("the batch is shown");
        seen.expect("the batch was shown")
    };
    let both = Some(vec![Subtree::new(first), Subtree::new(second)]);

    // As the handles named it, the element hosts the first Merk alone.
    let mut with_element = settled(true, None);
    assert!(!with_element.names_two_hosts());
    assert_eq!(
        handed(&with_element),
        (Some(Some(vec![Subtree::new(first)])), false)
    );
    // Told the whole list, its node names both, and the second Merk hangs
    // on a node the batch puts.
    with_element.host(&host(50, &[first, second]));
    assert_eq!(handed(&with_element), (Some(both.clone()), false));
    // The later statement of one element stands.
    with_element.host(&host(50, &[second, first]));
    let turned = Some(vec![Subtree::new(second), Subtree::new(first)]);
    assert_eq!(handed(&with_element), (Some(turned), false));
    // Taken back, the element hosts what the handles named.
    with_element.forget_hosts();
    assert_eq!(
        handed(&with_element),
        (Some(Some(vec![Subtree::new(first)])), false)
    );

    // A batch that does not put the element's node writes the Merk
    // anchored once it is told whose it is.
    let mut alone = settled(false, None);
    assert_eq!(handed(&alone), (None, false));
    alone.host(&host(50, &[first, second]));
    assert_eq!(handed(&alone), (None, true));
    // What is said of another element changes nothing of this Merk.
    let mut elsewhere = settled(false, None);
    elsewhere.host(&host(60, &[[7; 32], [8; 32]]));
    assert_eq!(handed(&elsewhere), (None, false));

    // Two handles of one subtree that name one element agree, and two
    // that name different elements do not.
    assert!(!settled(true, Some(50)).names_two_hosts());
    assert!(settled(true, Some(60)).names_two_hosts());
    // So do two that name one element with two different first subtrees:
    // what the element hosts is worked out from the first.
    let staged = LigninBatch::new();
    for (own, key) in [(first, b"k"), (second, b"l")] {
        let subtree = LigninSubtree {
            prefix: second,
            host: Some(host(50, &[own])),
        };
        let handle = LigninNodeStore::deferred(&writer, subtree, Some(&staged));
        (handle.put_meta_record(key, b"v").unwrap()).expect("a staged record");
    }
    let settled = staged.settle(&writer).unwrap().expect("a settled batch");
    assert!(settled.names_two_hosts());
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

/// A group can take a subtree out with one above it, and give that subtree
/// to be entered if it is absent (`SubtreeWrite::EnterIfAbsent`). The Merk
/// it writes under the prefix is then a new one, whatever the transaction
/// holds there until the group is applied: every value the group states of
/// it is written, one that is byte for byte the value of the Merk that
/// goes as well as another. A group that takes no subtree out keeps what
/// the Merk it is given holds.
#[test]
fn a_subtree_given_with_a_group_that_takes_one_above_it_is_written_whole() {
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
    let enters = [(child.clone(), b"tree".to_vec())];
    let nodes = |value: &[u8]| -> Vec<_> { (0..9).map(|n| basic(n, value)).collect() };
    // Stages the element that hosts the child, in a Merk of the parent
    // that is new.
    let element = |staged: &LigninBatch| {
        let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(staged));
        apply(&mut open_layered(upper, None), &[basic(50, b"an element")]);
    };
    // Stages nine nodes with `value` as a Merk that is new under the
    // child's prefix, on a handle that has seen the nodes the transaction
    // holds there.
    let below = |staged: &LigninBatch, value: &[u8]| {
        let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(staged));
        for key in 0..9 {
            let _ = lower.get_node(&be(key), NO_VALUE_DEFINED_COST, version());
        }
        let mut lower = open_layered(lower, None);
        apply(&mut lower, &nodes(value));
        lower.root_key()
    };
    let apply_group = |staged: &LigninBatch| {
        let part = staged.settle(&writer).unwrap().expect("a settled batch");
        let applied = LigninSettled::apply(vec![part], &writer, &enters);
        let applied = applied.unwrap().expect("the group is applied");
        (applied.values_written, applied.values_kept)
    };
    let check = |root: Option<Vec<u8>>, value: &[u8]| {
        let reference = MemNodeStore::new();
        apply(&mut open_layered(reference.clone(), None), &nodes(value));
        let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
        let expected = records(&reference, root.clone());
        assert_same_records(&records(&reader, root), &expected, "the hosted Merk");
        assert_eq!(expected.len(), 9);
    };

    // The subtree is absent: the group enters it under its element.
    let staged = LigninBatch::new();
    element(&staged);
    let root = below(&staged, b"below");
    assert_eq!(apply_group(&staged), (10, 0));
    check(root, b"below");

    // The parent is dropped and entered again, which takes the subtree
    // with it, and the group gives the subtree once more: with the values
    // its Merk had, and then with others.
    for value in [&b"below"[..], b"other"] {
        let staged = LigninBatch::new();
        staged.drop_subtree(&parent);
        staged.enter(&parent, b"tree").unwrap();
        element(&staged);
        let root = below(&staged, value);
        assert_eq!(apply_group(&staged), (10, 0), "{value:?}");
        check(root, value);
    }

    // A group that takes nothing out finds the subtree it is given, and
    // keeps the values its Merk holds.
    let staged = LigninBatch::new();
    let root = below(&staged, b"other");
    assert_eq!(apply_group(&staged), (0, 9));
    check(root, b"other");
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
    // storage error, the overlap of two write scopes, and is charged as a
    // lookup that found nothing.
    commit(&writer, &ROOT, &mut None, &[basic(2, b"v")]);
    let holder = LigninNodeStore::immediate(&writer, ROOT);
    holder.set_root_key(Some(b"k")).unwrap().unwrap();
    let mut change_set = holder.change_set();
    change_set.put_aux_record(b"k", b"aux", None).unwrap();
    holder.commit(change_set).unwrap().unwrap();
    let not_read = |cost: OperationCost, error: Error| {
        assert_eq!(cost, seek);
        let overlap = grovedb_storage::Error::WriteScope(WriteScopeError::Overlap);
        assert!(
            matches!(&error, Error::StorageError(error) if error.to_string() == overlap.to_string()),
            "{error:?}"
        );
    };
    let read = outside.get_node(&be(1), NO_VALUE_DEFINED_COST, version());
    not_read(read.cost, read.value.unwrap_err());
    let read = outside.get_node_for_read(&be(1));
    not_read(read.cost, read.value.unwrap_err());
    let read = outside.has_node(&be(1));
    not_read(read.cost, read.value.unwrap_err());
    let read = outside.root_key();
    not_read(read.cost, read.value.unwrap_err());
    let read = outside.get_aux_record(b"k");
    not_read(read.cost, read.value.unwrap_err());
    let read = outside.fetch_node(&link, NO_VALUE_DEFINED_COST.as_ref(), version());
    not_read(read.cost, read.value.unwrap_err());
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
    fn cursor<'a>(&'a self, subtree: &Prefix) -> Result<Box<dyn Walk + 'a>, Error> {
        self.under.cursor(subtree)
    }

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

    fn subtree(&self, subtree: &Prefix) -> Result<Option<LigninEntry>, Error> {
        self.under.subtree(subtree)
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

    fn apply_parts(
        &self,
        parts: &[&dyn BatchPart],
        enters: &[(usize, SubtreeChange)],
        entry: &mut Option<LigninEntryRefused>,
    ) -> Result<Vec<Written>, Error> {
        self.under.apply_parts(parts, enters, entry)
    }

    fn name_hosted(&self, hosts: &[LigninHost]) -> Result<(), Error> {
        self.under.name_hosted(hosts)
    }
}

impl FlatAccess for Unsteady<'_> {
    fn records<'a>(
        &'a self,
        keyspace: Keyspace,
        subtree: &Prefix,
    ) -> Result<Box<dyn FlatWalk + 'a>, Error> {
        self.under.records(keyspace, subtree)
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
    assert_eq!(store.entry().unwrap().unwrap().root_key, root);

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

/// The node under `n`, loaded through `store`.
fn node_of(store: &LigninNodeStore<'_>, n: u64) -> TreeNode {
    let node = store.get_node(&be(n), NO_VALUE_DEFINED_COST, version());
    node.unwrap().unwrap().expect("the node is stored")
}

/// Applies `batch` to the root Merk in a transaction of its own, and
/// commits it.
fn commit_outside(lignin: &Lignin, root: &mut Option<Vec<u8>>, batch: &[(Vec<u8>, Op)]) {
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    commit(&writer, &ROOT, root, batch);
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}

/// A store whose root Merk holds the nodes 1 to 7, committed: 4 is the
/// root, 2 and 6 are under it, and the others are leaves.
fn seven_nodes() -> (Lignin, Option<Vec<u8>>) {
    let lignin = Lignin::new();
    let mut root = None;
    let nodes: Vec<_> = (1..=7).map(|n| basic(n, &[n as u8; 8])).collect();
    commit_outside(&lignin, &mut root, &nodes);
    assert_eq!(root, Some(be(4)));
    (lignin, root)
}

/// Gives the node under `n` the value `value` in a transaction of its own,
/// and commits it.
fn rewrite_outside(lignin: &Lignin, root: &Option<Vec<u8>>, n: u64, value: &[u8]) {
    commit_outside(lignin, &mut root.clone(), &[basic(n, value)]);
}

/// Writes `node` outside a commit through a handle that has loaded
/// nothing, and applies the batch the write is staged in.
fn write_staged(
    writer: &LigninWriter<'_, '_>,
    node: &TreeNode,
    change: NodeChange,
) -> LigninApplied {
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(writer, ROOT, Some(&staged));
    (store.write_node(node, change).unwrap()).expect("the write is staged");
    drop(store);
    (staged.apply(writer).unwrap()).expect("the batch is applied")
}

/// The node under `n` as it is committed.
fn committed_node(lignin: &Lignin, n: u64) -> TreeNode {
    let path = lignin.path();
    let transaction = path.begin();
    node_of(&LigninNodeStore::read_only(&transaction, ROOT), n)
}

/// A node that keeps its value names the read the value is kept from, and
/// that is the handle's first load of the node: the caller may hold the
/// node as that load showed it. A later place of the node is no such read,
/// whether the read of its parent says it, a second load of the node, as a
/// read past Merk's own tree makes, or the report of a write of the node
/// as it is now, which a handle that writes into a batch takes in from the
/// batch. A handle loads a leaf, another transaction commits another value
/// for the leaf, the handle does one of these, and then writes the leaf as
/// it first loaded it, with its value kept. The transaction's commit is a
/// conflict, and the leaf stays as the other commit wrote it.
#[test]
fn a_kept_value_names_the_first_load_of_its_node() {
    let cases = [
        "its parent",
        "the leaf",
        "the leaf, written as it is now",
        "the leaf, written as it is now through a batch",
    ];
    for later in cases {
        let (lignin, root) = seven_nodes();
        let path = lignin.path();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let staged = LigninBatch::new();
        let store = match later.ends_with("batch") {
            true => LigninNodeStore::deferred(&writer, ROOT, Some(&staged)),
            false => LigninNodeStore::immediate(&writer, ROOT),
        };
        // Applies what the handle staged, and has it take the places in.
        let applied = || {
            if later.ends_with("batch") {
                let applied = (staged.apply(&writer).unwrap()).expect("the batch is applied");
                store.take_locations(&applied);
            }
        };

        let leaf = node_of(&store, 1);
        assert_eq!(leaf.value_as_slice(), [1; 8]);
        rewrite_outside(&lignin, &root, 1, &[9; 8]);
        let theirs = committed_node(&lignin, 1);
        assert_eq!(theirs.value_as_slice(), [9; 8]);
        assert_ne!(theirs.value_hash(), leaf.value_hash());

        // What is loaded after the other commit shows the leaf as it is
        // now: its parent says where it is, and the leaf has its value.
        let shown = node_of(&store, if later == "its parent" { 2 } else { 1 });
        match later {
            "its parent" => assert_eq!(shown.link(true).map(Link::key), Some(&be(1)[..])),
            _ => assert_eq!(shown.encode(), theirs.encode()),
        }
        if later.starts_with("the leaf, written") {
            // The store finds the stated bytes held and keeps them by the
            // read that compared them. Where the write put the leaf is no
            // read of the leaf as it was first loaded.
            let written = store.write_node(&shown, NodeChange::ValueWritten);
            written.unwrap().expect("the write is taken");
            applied();
            assert_eq!(transaction.stats().overlapped, 0, "{later}");
        }
        (store.write_node(&leaf, NodeChange::HashOnly).unwrap()).expect("the write is taken");
        applied();

        drop(store);
        writer.into_scope().merge();
        assert_eq!(lignin.commit(&transaction), Outcome::Conflict, "{later}");
        assert_eq!(committed_node(&lignin, 1).encode(), theirs.encode());
    }
}

/// Where the store finds the stated bytes held, it keeps them, and the
/// keep names the read that compared them: it is a read of exactly the
/// bytes that are kept. Another transaction commits a value for a node,
/// and a handle that has not loaded the node then puts the node with that
/// same value. Nothing is written for the value, nothing is refused, and
/// the commit is made.
#[test]
fn a_value_that_is_kept_by_comparison_names_the_comparing_read() {
    let (lignin, root) = seven_nodes();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    // The transaction has read the node before the other commit, through
    // another handle: that is no read of the handle that writes.
    drop(node_of(&LigninNodeStore::deferred(&writer, ROOT, None), 1));

    rewrite_outside(&lignin, &root, 1, &[9; 8]);
    let theirs = committed_node(&lignin, 1);

    let applied = write_staged(&writer, &theirs, NodeChange::ValueWritten);
    assert_eq!((applied.values_written, applied.values_kept), (0, 1));

    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    assert_eq!(committed_node(&lignin, 1).encode(), theirs.encode());
}

/// The store compares a stated value with the bytes the transaction holds
/// in one operation of the transaction, and writes the node in another,
/// with its value kept where the bytes were the same. The keep names the
/// comparing read, so a commit of another transaction between the two is a
/// commit since that read (`transactions.md` CF2a):
///
/// - where it gave the node another value, the transaction's commit is a
///   conflict, and the node stays as the other commit wrote it. Its bytes
///   are not published under the hashes of the first writer;
/// - where it deleted the node, there is no value to keep, and the write is
///   refused as one whose node a commit outside took away;
/// - where it wrote another node, nothing is met, and the commit is made.
///
/// That is so where the transaction lays its writes out and where it takes
/// them by key, in a scope that is opened beside another, and through a
/// handle that writes at once and one that stages its writes.
#[test]
fn a_commit_between_the_comparing_read_and_the_write_is_a_commit_since_the_read() {
    #[derive(Debug, Clone, Copy)]
    enum Between {
        Rewrites,
        Deletes,
        WritesAnother,
    }
    for between in [Between::Rewrites, Between::Deletes, Between::WritesAnother] {
        for (by_key, staged) in [(false, false), (false, true), (true, false), (true, true)] {
            let at = format!("{between:?}, by key: {by_key}, staged: {staged}");
            let (lignin, root) = seven_nodes();
            let lignin = Arc::new(lignin);
            let mine = committed_node(&lignin, 1);
            let other = committed_node(&lignin, 7);
            let path = lignin.path();
            let transaction = path.begin();
            let first = transaction.scope().unwrap();
            let (scope, beside) = match by_key {
                true => (transaction.scope().unwrap(), Some(first)),
                false => (first, None),
            };
            assert_eq!(scope.is_laid_out(), !by_key, "{at}");
            let writer = LigninWriter::new(scope);

            // The other transaction's commit, made when the store has
            // compared the bytes and before it writes.
            let theirs = Arc::clone(&lignin);
            AFTER_COMPARING.set(Some(Box::new(move || match between {
                Between::Rewrites => rewrite_outside(&theirs, &root, 1, &[9; 8]),
                Between::Deletes => {
                    commit_outside(&theirs, &mut root.clone(), &[(be(1), Op::Delete)]);
                }
                Between::WritesAnother => rewrite_outside(&theirs, &root, 7, &[9; 8]),
            })));

            // The node is put with the value it has, through a handle
            // that has not loaded it: the value is stated, and compared.
            let batch = LigninBatch::new();
            let store = match staged {
                true => LigninNodeStore::deferred(&writer, ROOT, Some(&batch)),
                false => LigninNodeStore::immediate(&writer, ROOT),
            };
            let written = store.write_node(&mine, NodeChange::ValueWritten).unwrap();
            drop(store);
            let written = match staged {
                true => written.and_then(|()| batch.apply(&writer).unwrap().map(drop)),
                false => written,
            };
            assert!(
                AFTER_COMPARING.take().is_none(),
                "{at}: no commit came between"
            );

            match between {
                Between::Deletes => {
                    // The refusal is the conflict of the seam, and no
                    // storage error by its words.
                    let refused = written.expect_err("there is no value to keep");
                    let Error::StorageError(refused) = refused else {
                        panic!("{at}: {refused}");
                    };
                    assert!(refused.is_commit_conflict(), "{at}: {refused}");
                    let conflict = grovedb_storage::error::CommitError::Conflict;
                    assert_eq!(refused.commit_error(), Some(conflict), "{at}");
                }
                _ => written.unwrap_or_else(|error| panic!("{at}: {error}")),
            }
            writer.into_scope().merge();
            if let Some(first) = beside {
                first.merge();
            }
            let outcome = lignin.commit(&transaction);
            let now = |n: u64| {
                let transaction = path.begin();
                let store = LigninNodeStore::read_only(&transaction, ROOT);
                let node = store.get_node(&be(n), NO_VALUE_DEFINED_COST, version());
                node.unwrap().unwrap().map(|node| node.encode())
            };
            match between {
                Between::Rewrites => {
                    assert_eq!(outcome, Outcome::Conflict, "{at}");
                    let theirs = committed_node(&lignin, 1);
                    assert_eq!(theirs.value_as_slice(), [9; 8], "{at}");
                    assert_ne!(theirs.value_hash(), mine.value_hash(), "{at}");
                }
                Between::Deletes => {
                    assert_eq!(outcome, Outcome::Empty, "{at}");
                    assert_eq!(now(1), None, "{at}");
                }
                Between::WritesAnother => {
                    assert!(
                        matches!(outcome, Outcome::Committed(_)),
                        "{at}: {outcome:?}"
                    );
                    assert_eq!(now(1), Some(mine.encode()), "{at}");
                    assert_ne!(now(7), Some(other.encode()), "{at}");
                }
            }
        }
    }
}

/// A change set that says it is the whole Merk is handed to the
/// transaction after the store has looked at the Merk's entry, and a
/// commit of another transaction can come between the two. That the Merk
/// has no root is a precondition of the engine's write
/// (`transactions.md` LD6), so no Merk is written over another's:
///
/// - where the other commit gives the Merk a root before the write is
///   taken, the write is refused there as a conflict
///   (`BatchError::MerkTaken`, which the seam gives as the conflict of a
///   commit): the store's look found the Merk with no root, and the
///   transaction dates it. Nothing of the write is taken;
/// - where it does so after the write, the transaction's commit is a
///   conflict, although the two wrote no key in common.
///
/// Either way the Merk is the other commit's.
#[test]
fn a_whole_merk_is_not_written_over_the_merk_of_a_commit_between() {
    for before_the_write in [true, false] {
        let lignin = Arc::new(Lignin::new());
        let path = lignin.path();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let theirs = Arc::clone(&lignin);
        let mut other_commit =
            Some(move || commit_outside(&theirs, &mut None, &[basic(9, &[9; 8])]));
        if before_the_write {
            // Made when the store has looked at the entry, and before it
            // hands the change set to the transaction.
            AFTER_COMPARING.set(other_commit.take().map(|commit| Box::new(commit) as _));
        }

        let store = LigninNodeStore::immediate(&writer, ROOT);
        let node = TreeNode::new(be(1), vec![0xf7; 5], None, TreeFeatureType::BasicMerkNode);
        let node = node.unwrap();
        let mut change_set = store.change_set();
        change_set.state_root(Some(node.key()));
        change_set.state_whole_merk();
        let prepared = change_set.prepare_node(&node, NodeChange::ValueWritten);
        change_set.put_node(node.key(), prepared, None).unwrap();
        let handed = store.commit(change_set).unwrap();
        drop(store);
        assert!(
            AFTER_COMPARING.take().is_none(),
            "no commit came between the look and the write"
        );
        match other_commit.take() {
            None => {
                let refused = handed.expect_err("the Merk has a root");
                let Error::StorageError(refused) = refused else {
                    panic!("{refused}");
                };
                assert!(refused.is_commit_conflict(), "{refused}");
                let conflict = grovedb_storage::error::CommitError::Conflict;
                assert_eq!(refused.commit_error(), Some(conflict));
            }
            Some(commit) => {
                handed.expect("a Merk with no root takes a whole Merk");
                commit();
            }
        }
        writer.into_scope().merge();
        let ended = lignin.commit(&transaction);
        let expected = match before_the_write {
            true => Outcome::Empty,
            false => Outcome::Conflict,
        };
        assert_eq!(ended, expected, "before the write: {before_the_write}");

        assert_eq!(lignin.root_in_directory(ROOT.prefix), Some(be(9)));
        assert_eq!(committed_node(&lignin, 9).value_as_slice(), [9; 8]);
        let reader = path.begin();
        let store = LigninNodeStore::read_only(&reader, ROOT);
        let mine = store.get_node(&be(1), NO_VALUE_DEFINED_COST, version());
        assert!(mine.unwrap().unwrap().is_none(), "a node of the whole Merk");
    }
}

/// A node that is written outside a commit through a handle that has not
/// loaded it has no read to name. So its value is stated, whatever kind of
/// change the caller says it is: it is kept where the same bytes are held,
/// by the read that compared them, and written where they are not. No
/// commit has met either write.
///
/// A node that is written alone leaves the node above it with the hash of
/// what was there, so the writes are made in a scope that is rolled back.
#[test]
fn a_node_the_handle_has_not_loaded_is_written_with_its_value_stated() {
    let (lignin, _) = seven_nodes();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let other = LigninNodeStore::deferred(&writer, ROOT, None);

    // The same bytes under another value hash, as the element of a tree
    // has when the Merk below it changes: the value hash is of the hash
    // part, and a node that keeps its value may state another.
    let held = node_of(&other, 2);
    let value = held.value_as_slice().to_vec();
    let mut corrected =
        TreeNode::new_with_combined_value_hash(be(2), value, [7; 32], held.feature_type()).unwrap();
    corrected.set_link(true, held.link(true).cloned());
    corrected.set_link(false, held.link(false).cloned());
    assert_ne!(corrected.value_hash(), held.value_hash());
    let applied = write_staged(&writer, &corrected, NodeChange::HashOnly);
    assert_eq!((applied.values_written, applied.values_kept), (0, 1));
    assert_eq!(node_of(&other, 2).encode(), corrected.encode());

    // Other bytes than are held: the caller says the value is unchanged,
    // the handle cannot know, and the node is written as it is handed
    // over.
    let held = node_of(&other, 6);
    let mut replaced = TreeNode::new(be(6), vec![0xee; 8], None, held.feature_type()).unwrap();
    replaced.set_link(true, held.link(true).cloned());
    replaced.set_link(false, held.link(false).cloned());
    let applied = write_staged(&writer, &replaced, NodeChange::HashOnly);
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));
    assert_eq!(node_of(&other, 6).encode(), replaced.encode());
    assert_eq!(transaction.stats().overlapped, 0);

    drop(other);
    writer.into_scope().rollback();
}

/// A node the handle has loaded as the transaction's own record has no
/// handle to name and needs none: a write of it that keeps its value is
/// taken as it is, and no value is compared for it. A scope beside
/// another keeps its writes by key, so the node it wrote is shown as its
/// record.
///
/// The leaf is written alone, which leaves the node above it with the
/// hash of what was there, so its scope is rolled back.
#[test]
fn a_node_that_was_loaded_as_the_transactions_record_keeps_its_value_uncompared() {
    let (lignin, _) = seven_nodes();
    let path = lignin.path();
    let transaction = path.begin();
    let first = transaction.scope().unwrap();
    let second = transaction.scope().unwrap();
    assert!(!second.is_laid_out());
    let writer = LigninWriter::new(second);

    let held = committed_node(&lignin, 1);
    let own = TreeNode::new(be(1), vec![0xee; 8], None, held.feature_type()).unwrap();
    let applied = write_staged(&writer, &own, NodeChange::ValueWritten);
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let loaded = node_of(&store, 1);
    assert_eq!(loaded.value_as_slice(), [0xee; 8]);
    // The write that keeps the value states another value hash for it, so
    // that it is seen to be made.
    let value = loaded.value_as_slice().to_vec();
    let rehashed =
        TreeNode::new_with_combined_value_hash(be(1), value, [7; 32], loaded.feature_type());
    let rehashed = rehashed.unwrap();
    assert_ne!(rehashed.encode(), loaded.encode());
    let reads = || {
        let stats = transaction.stats();
        stats.handles_followed + stats.keyed_reads
    };
    let before = reads();
    (store.write_node(&rehashed, NodeChange::HashOnly).unwrap()).expect("the write is staged");
    drop(store);
    let applied = (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!((applied.values_written, applied.values_kept), (0, 0));
    assert_eq!(reads(), before, "no node is read to compare its value");
    let written = node_of(&LigninNodeStore::deferred(&writer, ROOT, None), 1);
    assert_eq!(written.encode(), rehashed.encode());
    assert_eq!(transaction.stats().overlapped, 0);

    writer.into_scope().rollback();
    first.merge();
}

/// The first load of a node stays the read of it, also where a later load
/// shows the same value: the node may have other links by then. A handle
/// loads a node, another transaction commits a change below
/// it, which rewrites the node with the value it had, the handle loads the
/// node again, and then writes the node as it first loaded it. The
/// transaction's commit is a conflict, and the node stays as the other
/// commit wrote it.
#[test]
fn a_later_load_that_shows_the_same_value_does_not_replace_the_first() {
    let (lignin, root) = seven_nodes();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let store = LigninNodeStore::immediate(&writer, ROOT);

    let parent = node_of(&store, 2);
    rewrite_outside(&lignin, &root, 1, &[9; 8]);
    let theirs = committed_node(&lignin, 2);
    assert_eq!(theirs.value_hash(), parent.value_hash());
    assert_ne!(theirs.encode(), parent.encode());
    assert_eq!(node_of(&store, 2).encode(), theirs.encode());
    (store.write_node(&parent, NodeChange::HashOnly).unwrap()).expect("the write is taken");

    drop(store);
    writer.into_scope().merge();
    assert_eq!(lignin.commit(&transaction), Outcome::Conflict);
    assert_eq!(committed_node(&lignin, 2).encode(), theirs.encode());
}

/// A node that keeps its value while a link of it is modified is read
/// again when the write is applied, for the bytes the link holds for the
/// hash that is not made yet. That read is no read the value is kept from,
/// and neither is a later load: the keep names the handle's first load of
/// the node. A handle loads the root, another transaction commits another
/// value for the root, and the handle writes a child of the root and the
/// root as it first loaded it, with the link to that child modified and
/// its value kept. The transaction's commit is a conflict, and the root
/// stays as the other commit wrote it. With no commit between, the commit
/// is made.
#[test]
fn a_kept_value_beside_a_modified_link_names_the_first_load_of_its_node() {
    let cases = [
        "no commit between",
        "a commit between",
        "a commit between, and the root loaded again",
    ];
    for between in cases {
        let (lignin, root) = seven_nodes();
        let path = lignin.path();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let store = LigninNodeStore::immediate(&writer, ROOT);

        let loaded = node_of(&store, 4);
        let held = node_of(&store, 2);
        if between != "no commit between" {
            rewrite_outside(&lignin, &root, 4, &[9; 8]);
        }
        let theirs = committed_node(&lignin, 4);
        if between.ends_with("again") {
            assert_eq!(node_of(&store, 4).encode(), theirs.encode());
        }

        // The child with another value, under a link of the root that no
        // commit of Merk has hashed.
        let mut child = TreeNode::new(be(2), vec![0xee; 8], None, held.feature_type()).unwrap();
        child.set_link(true, held.link(true).cloned());
        child.set_link(false, held.link(false).cloned());
        let mut parent = loaded.clone();
        parent.inner.left = None;
        let parent = parent.attach(true, Some(child)).unwrap();
        assert!(matches!(parent.link(true), Some(Link::Modified { .. })));

        let mut change_set = store.change_set();
        change_set.state_root(Some(parent.key()));
        let child = parent.child(true).expect("the child is attached");
        let written = [
            (child, NodeChange::ValueWritten),
            (&parent, NodeChange::StructureChanged),
        ];
        for (node, change) in written {
            let prepared = change_set.prepare_node(node, change);
            change_set.put_node(node.key(), prepared, None).unwrap();
        }
        (store.commit(change_set).unwrap()).expect("the write is taken");
        assert_eq!(transaction.stats().stale_nodes, 1, "{between}");

        drop(store);
        writer.into_scope().merge();
        // The commit makes the hash that the root's link was left without
        // by Merk's rules, which the check of the store recomputes it by.
        let outcome = (lignin.path()).commit(&transaction, &mut MerkHashRules::new(version()));
        match between {
            "no commit between" => {
                let published = match outcome {
                    Ok(Committed::Published(published)) => published,
                    other => panic!("the commit is made: {other:?}"),
                };
                assert_eq!(published.evaluated, 1);
                let committed = committed_node(&lignin, 4);
                assert_eq!(committed.value_as_slice(), loaded.value_as_slice());
                assert_eq!(committed_node(&lignin, 2).value_as_slice(), [0xee; 8]);
            }
            _ => {
                assert!(
                    matches!(&outcome, Err(error) if error.is_conflict()),
                    "{between}: {outcome:?}"
                );
                assert_eq!(committed_node(&lignin, 4).encode(), theirs.encode());
            }
        }
    }
}

/// A record that the handle's cursor shows is no load of its node. The
/// cursor shows the committed nodes of the moment it was opened, which a
/// load made since may no longer show, and it gives the handle no place.
/// A handle opens a cursor, another transaction commits another value for
/// a leaf, and the cursor still shows the leaf as it was. The handle has
/// not loaded the leaf, so it has no read of it to name: a write of the
/// leaf as the cursor showed it, with its value kept, states the value.
/// The leaf is written as it is handed over, with the hashes that are of
/// its value, and the other commit has not met the write: for a value
/// that is stated the rule looks from the write on (`transactions.md`
/// CF2).
///
/// The leaf is written alone, which leaves the nodes above it with the
/// hash of what the other commit wrote, so its scope is rolled back.
#[test]
fn a_record_of_the_cursor_is_no_load_of_its_node() {
    let (lignin, root) = seven_nodes();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));

    let mut cursor = store.cursor();
    cursor.seek(be(1)).unwrap();
    rewrite_outside(&lignin, &root, 1, &[9; 8]);
    let theirs = committed_node(&lignin, 1);
    assert_eq!(theirs.value_as_slice(), [9; 8]);

    let record = cursor.value().unwrap().map(<[u8]>::to_vec);
    let record = record.expect("the cursor stands on the leaf");
    cursor.status().expect("the cursor read the record");
    drop(cursor);
    assert!(store.hint(&be(1)).is_none(), "a record gives no place");
    let leaf = TreeNode::decode_raw(&record, be(1), NO_VALUE_DEFINED_COST, version()).unwrap();
    assert_eq!(leaf.value_as_slice(), [1; 8]);
    assert_ne!(leaf.value_hash(), theirs.value_hash());

    (store.write_node(&leaf, NodeChange::HashOnly).unwrap()).expect("the write is staged");
    drop(store);
    let applied = (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));
    assert_eq!(transaction.stats().overlapped, 0);
    let written = node_of(&LigninNodeStore::deferred(&writer, ROOT, None), 1);
    assert_eq!(written.encode(), leaf.encode());

    writer.into_scope().rollback();
}

/// A batch that holds a write of its own under the stored key that a
/// record of a moved subtree is charged at is refused where it is
/// settled: a storage batch keeps one of the two, and the batch does not
/// say which. A write under another key of the subtree is settled.
#[test]
fn a_write_under_the_key_of_a_moved_record_is_refused_at_the_settle() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let to = LigninSubtree::alone([9; 32]);
    let settled = |written: u64| {
        let staged = LigninBatch::new();
        staged.enter(&to, b"tree").unwrap();
        let handle = LigninNodeStore::deferred(&writer, to.clone(), Some(&staged));
        apply(&mut open_layered(handle, None), &[basic(written, b"own")]);
        staged.charge_landed(&to.prefix, &be(7), 40, Default::default());
        staged.settle(&writer).unwrap().map(|_| ())
    };
    settled(8).expect("a write beside the moved record");
    let refused = settled(7).expect_err("a write under the moved record's key");
    assert!(
        refused.to_string().contains("a record of a moved subtree"),
        "{refused}"
    );
}

/// A batch that was told of the move of a subtree is applied where
/// whoever applies it states the move ([`LigninSettled::move_as`]). A
/// pair that nobody stated is refused, and the transaction takes nothing
/// of the batch.
#[test]
fn a_move_that_nobody_stated_is_refused_where_its_batch_is_applied() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let root = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    root.put_meta_record(b"m", b"meta").unwrap().unwrap();
    drop(root);
    staged.move_subtree(&[7; 32], &[9; 32]);
    let Err(refused) = staged.apply(&writer).unwrap() else {
        panic!("nobody stated the move");
    };
    assert!(refused.to_string().contains("was not stated"), "{refused}");
    let read = LigninNodeStore::deferred(&writer, ROOT, None);
    assert_eq!(read.get_meta_record(b"m").unwrap().unwrap(), None);
    assert_eq!(transaction.stats().writes, 0);
}

/// What a settled batch says for whoever applies it to drop the subtree
/// of a tree that another element is written over, and the drop it takes.
#[test]
fn a_settled_batch_says_which_puts_are_over_a_tree_and_takes_a_drop() {
    use grovedb_element::Element;
    use lignindb::txn::Hosted;

    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let parent = LigninSubtree::alone([1; 32]);
    let host = |hosted: &[Prefix]| LigninHost {
        subtree: parent.prefix,
        key: be(50),
        hosted: hosted.to_vec(),
    };
    let child = LigninSubtree {
        prefix: [222; 32],
        host: Some(host(&[[222; 32]])),
    };
    let tree = Element::empty_tree().serialize(version()).unwrap();
    let item = Element::new_item(b"an item".to_vec())
        .serialize(version())
        .unwrap();

    // The element of a tree under one key and an item under another, and
    // the tree's Merk with a node. The handle of the Merk loads none of
    // its nodes before it writes, and the batch says so.
    let staged = LigninBatch::new();
    staged.enter(&child, b"tree").unwrap();
    let upper = LigninNodeStore::deferred(&writer, parent.clone(), Some(&staged));
    let mut upper = open_layered(upper, None);
    apply(&mut upper, &[basic(50, &tree), basic(60, &item)]);
    let root = upper.root_key();
    drop(upper);
    let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(&staged));
    apply(&mut open_layered(lower, None), &[basic(1, b"below")]);
    let settled = staged.settle(&writer).unwrap().expect("a settled batch");
    assert!(settled.began_empty(&child.prefix) && settled.began_empty(&parent.prefix));
    // Nothing was loaded, so no put is over a tree, and no handle was
    // told a path.
    assert_eq!(settled.over_trees().count(), 0);
    assert_eq!(settled.path(&parent.prefix), None);
    (LigninSettled::apply(vec![settled], &writer, &[]).unwrap()).expect("the batch is applied");

    // The writer of a Merk that begins with no node can ask for a key
    // which the transaction holds under the prefix, as an insert of what
    // is not there does: it began with no node all the same, since every
    // node it puts is new to its Merk. One that is opened with the root
    // and writes on did not: the root it puts again is none.
    let began = |root: Option<Vec<u8>>, asked: Option<u64>, put: u64| {
        let staged = LigninBatch::new();
        let lower = LigninNodeStore::deferred(&writer, child.clone(), Some(&staged));
        let mut lower = open_layered(lower, root);
        if let Some(asked) = asked {
            let there = lower.exists(&be(asked), NO_VALUE_DEFINED_COST, version());
            assert!(there.unwrap().expect("a read"));
        }
        apply(&mut lower, &[basic(put, b"beside")]);
        drop(lower);
        let settled = staged.settle(&writer).unwrap().expect("a settled batch");
        (
            settled.began_empty(&child.prefix),
            settled.writes_merk(&child.prefix),
        )
    };
    assert_eq!(began(None, None, 2), (true, true));
    assert_eq!(began(None, Some(1), 2), (true, true));
    assert_eq!(began(Some(be(1)), None, 2), (false, true));
    assert_eq!(began(Some(be(1)), Some(1), 2), (false, true));
    // So did a writer that began with no node and then puts the key it
    // asked for: the node it puts is new to its Merk.
    assert_eq!(began(None, Some(1), 1), (true, true));
    // A handle that took in where an application put the nodes it wrote
    // writes that Merk on, whatever it fetched.
    let own = LigninSubtree::alone([223; 32]);
    let staged = LigninBatch::new();
    staged.enter(&own, b"tree").unwrap();
    let handle = LigninNodeStore::deferred(&writer, own.clone(), Some(&staged));
    let mut merk = open_layered(handle, None);
    apply(&mut merk, &[basic(1, b"one")]);
    let first = staged.settle(&writer).unwrap().expect("a settled batch");
    assert!(first.began_empty(&own.prefix));
    let applied = LigninSettled::apply(vec![first], &writer, &[]).unwrap();
    merk.storage
        .take_locations(&applied.expect("the batch is applied"));
    apply(&mut merk, &[basic(2, b"two")]);
    drop(merk);
    let second = staged.settle(&writer).unwrap().expect("a settled batch");
    assert!(!second.began_empty(&own.prefix) && second.writes_merk(&own.prefix));
    (LigninSettled::apply(vec![second], &writer, &[]).unwrap()).expect("the batch is applied");
    // A writer that removes the root it was opened with, the one node of
    // its Merk, and writes the Merk on through the same handle began with
    // that root: what its first change set says holds for the one after
    // it, which by itself puts one new node and removes none.
    let sole = LigninSubtree::alone([224; 32]);
    let staged = LigninBatch::new();
    staged.enter(&sole, b"tree").unwrap();
    let handle = LigninNodeStore::deferred(&writer, sole.clone(), Some(&staged));
    let mut merk = open_layered(handle, None);
    apply(&mut merk, &[basic(1, b"one")]);
    let opened_with = merk.root_key();
    drop(merk);
    (staged.apply(&writer).unwrap()).expect("the batch is applied");
    let staged = LigninBatch::new();
    let handle = LigninNodeStore::deferred(&writer, sole.clone(), Some(&staged));
    let mut merk = open_layered(handle, opened_with.clone());
    apply(&mut merk, &[(opened_with.expect("a root"), Op::Delete)]);
    staged.next_operation();
    apply(&mut merk, &[basic(3, b"three")]);
    drop(merk);
    let on = staged.settle(&writer).unwrap().expect("a settled batch");
    assert!(!on.began_empty(&sole.prefix) && on.writes_merk(&sole.prefix));
    // So did a writer whose first change set removes that root and puts a
    // node that is new to the Merk, both at once: a change set that
    // removes a node begins no Merk, whatever it puts.
    let staged = LigninBatch::new();
    let handle = LigninNodeStore::deferred(&writer, sole.clone(), Some(&staged));
    let mut merk = open_layered(handle, Some(be(1)));
    apply(&mut merk, &[(be(1), Op::Delete), basic(3, b"three")]);
    drop(merk);
    let at_once = staged.settle(&writer).unwrap().expect("a settled batch");
    assert!(!at_once.began_empty(&sole.prefix) && at_once.writes_merk(&sole.prefix));
    let none = LigninBatch::new()
        .settle(&writer)
        .unwrap()
        .expect("a settled batch");
    assert_eq!(
        (
            none.began_empty(&child.prefix),
            none.writes_merk(&child.prefix)
        ),
        (false, false)
    );

    // A handle that was told its path loads both nodes and states another
    // value for each: the put where it loaded a tree's element is over a
    // tree, and the put over the item is not. A node whose value is kept,
    // and one that a later operation of the batch removes, are not.
    fn over<'db>(
        writer: &'db LigninWriter<'_, '_>,
        root: &Option<Vec<u8>>,
        staged: &'db LigninBatch,
        puts: &[(Vec<u8>, Op)],
    ) -> Merk<LigninNodeStore<'db>> {
        let parent = LigninSubtree::alone([1; 32]);
        let upper = LigninNodeStore::deferred(writer, parent, Some(staged));
        let upper = upper.at_path(vec![b"up".to_vec(), b"per".to_vec()]);
        let mut upper = open_layered(upper, root.clone());
        apply(&mut upper, puts);
        upper
    }
    let staged = LigninBatch::new();
    drop(over(
        &writer,
        &root,
        &staged,
        &[basic(50, &item), basic(60, b"other")],
    ));
    let mut settled = staged.settle(&writer).unwrap().expect("a settled batch");
    let key = be(50);
    assert_eq!(
        settled.over_trees().collect::<Vec<_>>(),
        [(&parent.prefix, key.as_slice())]
    );
    assert!(!settled.began_empty(&parent.prefix));
    let told = [b"up".to_vec(), b"per".to_vec()];
    assert_eq!(settled.path(&parent.prefix), Some(told.as_slice()));
    let staged = LigninBatch::new();
    drop(over(&writer, &root, &staged, &[basic(61, b"beside")]));
    let kept = staged.settle(&writer).unwrap().expect("a settled batch");
    assert_eq!(kept.over_trees().count(), 0);
    let staged = LigninBatch::new();
    let mut upper = over(&writer, &root, &staged, &[basic(50, &item)]);
    staged.next_operation();
    apply(&mut upper, &[(be(50), Op::Delete)]);
    drop(upper);
    let removed = staged.settle(&writer).unwrap().expect("a settled batch");
    assert_eq!(removed.over_trees().count(), 0);

    // The batch takes the drop of the tree's subtree: the transaction is
    // handed it before anything else, the element's node names no Merk,
    // and nothing is read under the subtree afterwards.
    settled.drop_subtree(&child.prefix);
    settled.drop_subtree(&child.prefix);
    settled.host(&host(&[]));
    let mut shown = None;
    let run = &mut |batch: &Batch<'_>| {
        let drops = |write: &&SubtreeWrite<'_>| matches!(write, SubtreeWrite::Drop { .. });
        let element = batch.merks.iter().flat_map(|merk| merk.nodes.iter());
        let names = element
            .filter(|node| node.key == be(50))
            .map(|node| matches!(node.hosted, Hosted::Set(hosted) if hosted.is_empty()))
            .collect::<Vec<_>>();
        let first = batch.subtrees.first().map(SubtreeWrite::subtree);
        shown = Some((batch.subtrees.iter().filter(drops).count(), first, names));
        Ok(Written::default())
    };
    BatchPart::with_batch(&settled, run).expect("the batch is shown");
    let dropped = Subtree::new(child.prefix);
    assert_eq!(shown, Some((1, Some(dropped), vec![true])));
    (LigninSettled::apply(vec![settled], &writer, &[]).unwrap()).expect("the batch is applied");
    let reader = LigninNodeStore::deferred(&writer, child.clone(), None);
    assert!(!reader.has_node(&be(1)).unwrap().unwrap());
    assert_eq!(reader.entry().unwrap(), None);
}

/// A handle that keeps its root-key record outside its subtree
/// ([`LigninNodeStore::root_key_kept_in`]) reads and writes it in the
/// roots keyspace of the subtree it was told, under its own prefix and
/// then the record's key, through an immediate handle and through a
/// deferred one, and is charged what a handle that keeps the record in
/// its subtree is charged: for the write, for the read, for the batch,
/// and for a removal that came with no cost, which is charged from the
/// committed record. Two subtrees keep two records there. The record is
/// none of its subtree's for the transaction, and for a batch a write
/// into no subtree. The subtree it is kept under here is none that
/// leaves with the handle's: a handle of the subtree that was told
/// nothing reads none; a drop of the subtree leaves the record where it
/// is, where it takes the record of a subtree that keeps its own; a
/// batch that is told to hand over none of the writes of the subtree,
/// nor of the one the record is kept under, hands the record over; and
/// a batch that itself drops the subtree after it staged a put of the
/// record, or a removal of the committed one, makes that write all the
/// same, at the charge it has with no drop. A handle that is told its
/// own subtree keeps the record there as one that was told nothing.
#[test]
fn a_root_key_record_that_is_kept_outside_its_subtree_is_none_of_the_subtrees_records() {
    use lignindb::txn::{Column, Unit};

    use super::kept_root_key;
    use crate::merk::defaults::ROOT_KEY_KEY;

    let lignin = Lignin::new();
    let path = lignin.path();
    let (home, one, other, plain) = ([8; 32], [9; 32], [10; 32], [11; 32]);
    let (third, fourth, fifth) = ([12; 32], [13; 32], [14; 32]);
    let held = |transaction: &WriteTxn<'_>, subtree: Prefix, key: &[u8]| {
        let mut value = Vec::new();
        let unit = Unit::new(Column::Roots, Subtree::new(subtree));
        (transaction.get(unit, key, &mut value).unwrap()).then_some(value)
    };
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    {
        let kept = |prefix: Prefix| {
            LigninNodeStore::immediate(&writer, LigninSubtree::alone(prefix)).root_key_kept_in(home)
        };
        let own =
            |prefix: Prefix| LigninNodeStore::immediate(&writer, LigninSubtree::alone(prefix));

        // Written and read where it is kept, at the charge of a record of
        // the subtree.
        let written = kept(one).set_root_key(Some(b"root of one"));
        let in_its_subtree = own(plain).set_root_key(Some(b"root of one"));
        assert_eq!(written.cost, in_its_subtree.cost);
        written.unwrap().unwrap();
        in_its_subtree.unwrap().unwrap();
        let (read, in_its_subtree) = (kept(one).root_key(), own(plain).root_key());
        assert_eq!(read.cost, in_its_subtree.cost);
        assert_eq!(read.unwrap().unwrap(), Some(b"root of one".to_vec()));
        assert_eq!(
            in_its_subtree.unwrap().unwrap(),
            Some(b"root of one".to_vec())
        );
        // Each subtree has its own, and the subtree itself holds none.
        (kept(other).set_root_key(Some(b"of the other")).unwrap()).unwrap();
        let read = |prefix: Prefix| kept(prefix).root_key().unwrap().unwrap();
        assert_eq!(read(other), Some(b"of the other".to_vec()));
        assert_eq!(read(one), Some(b"root of one".to_vec()));
        assert_eq!(own(one).root_key().unwrap().unwrap(), None);

        // Two records for the next transaction to remove.
        for prefix in [fourth, fifth] {
            (kept(prefix).set_root_key(Some(b"to be removed")).unwrap()).unwrap();
        }

        // A deferred handle stages the record in its batch where it is
        // kept, and the batch hands it to the transaction there.
        let (staged, staged_plain) = (LigninBatch::new(), LigninBatch::new());
        let deferred = |prefix: Prefix, batch| {
            LigninNodeStore::deferred(&writer, LigninSubtree::alone(prefix), Some(batch))
        };
        let outside = deferred(one, &staged).root_key_kept_in(home);
        (outside.set_root_key(Some(b"a later root")).unwrap()).unwrap();
        (deferred(plain, &staged_plain).set_root_key(Some(b"a later root")))
            .unwrap()
            .unwrap();
        assert_eq!(read(one), Some(b"root of one".to_vec()));
        let (applied, plain_applied) = (staged.apply(&writer), staged_plain.apply(&writer));
        assert_eq!(applied.cost, plain_applied.cost);
        applied.unwrap().unwrap();
        plain_applied.unwrap().unwrap();
        assert_eq!(read(one), Some(b"a later root".to_vec()));
        assert_eq!(
            own(plain).root_key().unwrap().unwrap(),
            Some(b"a later root".to_vec())
        );
    }
    writer.into_scope().merge();
    assert_eq!(
        held(&transaction, home, &kept_root_key(&one)),
        Some(b"a later root".to_vec())
    );
    assert_eq!(
        held(&transaction, home, &kept_root_key(&other)),
        Some(b"of the other".to_vec())
    );
    assert_eq!(held(&transaction, one, ROOT_KEY_KEY), None);
    assert_eq!(held(&transaction, home, ROOT_KEY_KEY), None);
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));

    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    {
        // A removal with no cost of its writer's is charged from the
        // committed record, which is read where it is kept.
        let (staged, staged_plain) = (LigninBatch::new(), LigninBatch::new());
        let deferred = |prefix: Prefix, batch| {
            LigninNodeStore::deferred(&writer, LigninSubtree::alone(prefix), Some(batch))
        };
        let outside = deferred(other, &staged).root_key_kept_in(home);
        outside.set_root_key(None).unwrap().unwrap();
        deferred(plain, &staged_plain)
            .set_root_key(None)
            .unwrap()
            .unwrap();
        let (applied, plain_applied) = (staged.apply(&writer), staged_plain.apply(&writer));
        assert_eq!(applied.cost, plain_applied.cost);
        applied.unwrap().unwrap();
        plain_applied.unwrap().unwrap();
        assert_eq!(outside.root_key().unwrap().unwrap(), None);

        // A drop takes the records of a subtree, and this one is none of
        // them.
        (deferred(plain, &staged_plain).set_root_key(Some(b"its own")))
            .unwrap()
            .unwrap();
        staged_plain.apply(&writer).unwrap().unwrap();
        let dropping = LigninBatch::new();
        for prefix in [one, plain] {
            dropping.drop_subtree(&LigninSubtree::alone(prefix));
        }
        dropping.apply(&writer).unwrap().unwrap();
        let kept = LigninNodeStore::immediate(&writer, LigninSubtree::alone(one));
        let kept = kept.root_key_kept_in(home);
        assert_eq!(
            kept.root_key().unwrap().unwrap(),
            Some(b"a later root".to_vec())
        );

        // A batch that is told to hand the transaction none of the
        // writes it holds for the subtree, nor for the one the record is
        // kept under, hands it this record, which is written where it is
        // kept, for a subtree that is not entered.
        let staged = LigninBatch::new();
        let outside = deferred(one, &staged).root_key_kept_in(home);
        (outside.set_root_key(Some(b"all the same")).unwrap()).unwrap();
        let mut settled = staged.settle(&writer).unwrap().expect("a settled batch");
        assert_eq!(settled.written().count(), 0, "no write of the subtree");
        settled.leave_out(&one);
        settled.leave_out(&home);
        (LigninSettled::apply(vec![settled], &writer, &[]).unwrap()).expect("the batch is applied");
        assert_eq!(
            kept.root_key().unwrap().unwrap(),
            Some(b"all the same".to_vec())
        );

        // A handle that is told its own subtree keeps the record there
        // as one that was told nothing: the batch writes into the
        // subtree.
        let staged = LigninBatch::new();
        let within = deferred(third, &staged).root_key_kept_in(third);
        assert_eq!(within.root_key_place(), (third, ROOT_KEY_KEY.to_vec()));
        (within.set_root_key(Some(b"in its own")).unwrap()).unwrap();
        let settled = staged.settle(&writer).unwrap().expect("a settled batch");
        assert_eq!(settled.written().count(), 1, "a write into the subtree");
        (LigninSettled::apply(vec![settled], &writer, &[]).unwrap()).expect("the batch is applied");
        assert_eq!(
            within.root_key().unwrap().unwrap(),
            Some(b"in its own".to_vec())
        );

        // A batch that stages a put of the record and then drops the
        // subtree itself, which cuts what it staged in the subtree before,
        // puts the record, and is charged what the put alone is.
        let kept_of = |prefix: Prefix| {
            let handle = LigninNodeStore::immediate(&writer, LigninSubtree::alone(prefix));
            handle.root_key_kept_in(home).root_key().unwrap().unwrap()
        };
        let (staged, alone) = (LigninBatch::new(), LigninBatch::new());
        let outside = deferred(other, &staged).root_key_kept_in(home);
        (outside.set_root_key(Some(b"before its drop")).unwrap()).unwrap();
        staged.next_operation();
        staged.drop_subtree(&LigninSubtree::alone(other));
        let outside = deferred(third, &alone).root_key_kept_in(home);
        (outside.set_root_key(Some(b"before its drop")).unwrap()).unwrap();
        let (applied, put_alone) = (staged.apply(&writer), alone.apply(&writer));
        assert_eq!(applied.cost, put_alone.cost);
        applied.unwrap().unwrap();
        put_alone.unwrap().unwrap();
        assert_eq!(kept_of(other), Some(b"before its drop".to_vec()));
        assert_eq!(kept_of(third), Some(b"before its drop".to_vec()));
        let entry = LigninNodeStore::deferred(&writer, LigninSubtree::alone(other), None).entry();
        assert_eq!(entry.unwrap(), None);

        // And so with a removal of a record that is committed: it is
        // charged from the committed record, as the removal alone is, and
        // the record goes.
        let (staged, alone) = (LigninBatch::new(), LigninBatch::new());
        let outside = deferred(fourth, &staged).root_key_kept_in(home);
        outside.set_root_key(None).unwrap().unwrap();
        staged.next_operation();
        staged.drop_subtree(&LigninSubtree::alone(fourth));
        let outside = deferred(fifth, &alone).root_key_kept_in(home);
        outside.set_root_key(None).unwrap().unwrap();
        let (applied, removed_alone) = (staged.apply(&writer), alone.apply(&writer));
        assert_eq!(applied.cost, removed_alone.cost);
        assert!(applied.cost.storage_loaded_bytes > 0, "a committed record");
        applied.unwrap().unwrap();
        removed_alone.unwrap().unwrap();
        assert_eq!(kept_of(fourth), None);
        assert_eq!(kept_of(fifth), None);
    }
    writer.into_scope().merge();
    assert_eq!(
        held(&transaction, home, &kept_root_key(&one)),
        Some(b"all the same".to_vec())
    );
    assert_eq!(
        held(&transaction, home, &kept_root_key(&other)),
        Some(b"before its drop".to_vec())
    );
    assert_eq!(
        held(&transaction, home, &kept_root_key(&third)),
        Some(b"before its drop".to_vec())
    );
    assert_eq!(
        held(&transaction, third, ROOT_KEY_KEY),
        Some(b"in its own".to_vec())
    );
    for prefix in [fourth, fifth] {
        assert_eq!(held(&transaction, home, &kept_root_key(&prefix)), None);
    }
    assert_eq!(held(&transaction, plain, ROOT_KEY_KEY), None);
}

/// A batch names the subtrees it writes into by the writes that are left
/// of it, and a record that its handle keeps outside its subtree is a
/// write into none: neither into the handle's subtree nor into the one
/// it is kept under, also where an earlier operation of the batch wrote
/// into that one and a later operation took the write back. A subtree
/// that is not entered is entered by the batch, written into and dropped
/// again, and then a handle of another subtree puts its root-key record
/// there: the batch names no subtree and no put into one, and the
/// transaction refuses the record, whose home is not entered.
#[test]
fn a_kept_record_is_a_write_into_no_subtree_where_the_batch_took_its_other_writes_back() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let (absent, other) = ([230; 32], [20; 32]);
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let handle = |prefix: Prefix| {
        LigninNodeStore::deferred(&writer, LigninSubtree::alone(prefix), Some(&staged))
    };
    let of_the_home = handle(absent);
    of_the_home.enter(TreeType::NormalTree).unwrap();
    (of_the_home.set_root_key(Some(b"its own")).unwrap()).unwrap();
    staged.next_operation();
    staged.drop_subtree(&LigninSubtree::alone(absent));
    staged.next_operation();
    let kept = handle(other).root_key_kept_in(absent);
    (kept.set_root_key(Some(b"of another")).unwrap()).unwrap();
    let settled = staged.settle(&writer).unwrap().expect("a settled batch");
    assert_eq!(settled.written().count(), 0, "no write into a subtree");
    assert_eq!(settled.put_into().count(), 0, "no put into a subtree");
    let refused = (LigninSettled::apply(vec![settled], &writer, &[]).unwrap())
        .expect_err("the home of the record is not entered");
    assert!(refused.to_string().contains("is not entered"), "{refused}");
}

/// A change set that holds nothing but a record which its handle keeps
/// outside its subtree leaves no mark of the handle's subtree in the
/// batch. A handle that names no host puts an aux record into a subtree,
/// and a handle of the same subtree that names a host and was told a
/// path then puts its root-key record outside: the batch names the
/// subtree as the first handle did, with no host, says of no subtree
/// that two hosts were named for it, and holds no path of the subtree.
#[test]
fn a_kept_record_alone_leaves_no_mark_of_its_handles_subtree() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let (subtree, home, above) = ([231; 32], [22; 32], [1; 32]);
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let alone = LigninNodeStore::deferred(&writer, LigninSubtree::alone(subtree), Some(&staged));
    let mut change_set = alone.change_set();
    change_set.put_aux_record(b"k", b"aux", None).unwrap();
    alone.commit(change_set).unwrap().unwrap();
    let hosted = LigninSubtree {
        prefix: subtree,
        host: Some(LigninHost {
            subtree: above,
            key: b"missing".to_vec(),
            hosted: vec![subtree],
        }),
    };
    let named = LigninNodeStore::deferred(&writer, hosted, Some(&staged))
        .at_path(vec![b"missing".to_vec()])
        .root_key_kept_in(home);
    (named.set_root_key(Some(b"kept")).unwrap()).unwrap();
    let settled = staged.settle(&writer).unwrap().expect("a settled batch");
    let written: Vec<_> = settled.written().collect();
    assert_eq!(written.len(), 1, "the subtree of the aux record");
    assert_eq!(written[0].prefix, subtree);
    assert!(written[0].host.is_none(), "as the first handle named it");
    assert!(!settled.names_two_hosts());
    assert_eq!(settled.path(&subtree), None);
}

/// A batch charges its records in the order of their own stored keys, as
/// a storage batch does (`cost-model.md` B1), wherever each is kept: a
/// record that is kept outside its subtree has the place of its subtree's
/// prefix, and not the place of the subtree it is kept under. Of the
/// root-key records of two subtrees, the first of which is kept under a
/// subtree after both, the put of the second is refused for its cost:
/// the batch is charged the put of the first before the refusal, as a
/// batch is whose two records are kept in their subtrees.
#[test]
fn a_record_that_is_kept_outside_its_subtree_is_charged_in_the_place_of_its_own_key() {
    use grovedb_costs::storage_cost::{key_value_cost::KeyValueStorageCost, StorageCost};

    let lignin = Lignin::new();
    let path = lignin.path();
    let (first, second, home) = ([20; 32], [21; 32], [22; 32]);
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let wrong = || KeyValueStorageCost {
        value_storage_cost: StorageCost {
            added_bytes: 1,
            ..Default::default()
        },
        needs_value_verification: true,
        ..Default::default()
    };
    let refused = |kept: bool| {
        let staged = LigninBatch::new();
        let handle = |prefix: Prefix| {
            LigninNodeStore::deferred(&writer, LigninSubtree::alone(prefix), Some(&staged))
        };
        let of_the_first = match kept {
            true => handle(first).root_key_kept_in(home),
            false => handle(first),
        };
        (of_the_first.set_root_key(Some(b"taken")).unwrap()).unwrap();
        let of_the_second = handle(second);
        let mut change_set = of_the_second.change_set();
        (change_set.put_root_key(b"refused", Some(wrong()))).unwrap();
        of_the_second.commit(change_set).unwrap().unwrap();
        let settled = staged.settle(&writer);
        assert!(settled.value.is_err(), "the put of the second is refused");
        settled.cost
    };
    let (kept_outside, in_its_subtree) = (refused(true), refused(false));
    assert_eq!(kept_outside, in_its_subtree);
    assert_eq!(kept_outside.seek_count, 2, "the first was charged before");
}
