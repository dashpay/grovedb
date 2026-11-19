//! The golden corpus, replayed through the node store of a Merk on LigninDB
//! (`grovedb_merk::store::lignin`, MERK-01).
//!
//! GroveDB does not run on LigninDB yet, so the corpus is replayed on
//! RocksDB, and what each operation did to the nodes of every Merk is
//! handed to a LigninDB transaction through `LigninNodeStore`:
//!
//! 1. Each trace of the standard corpus is applied to a GroveDB on RocksDB,
//!    one operation at a time, as the corpus was recorded.
//! 2. After each operation the observer of the layout simulation
//!    (`lignin_testkit::layout_sim`) reads the node records Merk wrote: the
//!    nodes that are new or changed, the nodes that are gone, and the root
//!    of each Merk. That is the write as RocksDB received it.
//! 3. The same write goes to LigninDB: one change set for each Merk, built
//!    with `prepare_node`, `put_node`, `delete_node` and `state_root` from
//!    the nodes as RocksDB holds them, all change sets of the operation in
//!    one batch, with each Merk named in the element that hosts it. The
//!    batch enters each Merk the operation made, and drops each tree it
//!    removed: a tree that is moved, or deleted and inserted again, is
//!    dropped and entered in one batch.
//! 4. Every written node is read back through `LigninNodeStore` and must
//!    be, as a record, the record RocksDB holds: the same value, feature,
//!    hashes, links, heights and aggregates. Every so often, and at the end
//!    of the trace, every node of every Merk is compared so, from each
//!    Merk's root down. Equal records from the root down are an equal tree
//!    shape, and the root hash is a function of the root's record.
//!
//! The Merk logic itself runs on RocksDB here. That it gives the same tree
//! on a `LigninNodeStore` is held by the tests of `merk::store::lignin`,
//! which run the same batches on both.

use std::{
    collections::{BTreeMap, HashMap},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
};

use grovedb::{GroveDb, Transaction};
use grovedb_costs::CostResult;
use grovedb_merk::{
    store::{
        lignin::{LigninBatch, LigninHost, LigninNodeStore, LigninSubtree, LigninWriter},
        NodeChange, NodeChangeSet, NodeStore,
    },
    tree::{kv::ValueDefinedCostType, TreeNode},
    Error,
};
use grovedb_storage::Storage;
use grovedb_version::version::GroveVersion;
use lignin_testkit::{
    golden::plan::{plan, Planned, Scale},
    layout_sim::{ChangeSet, Grove, MerkId, Observer},
    workload::apply::Transactions,
};
use lignindb::txn::testing::Db;

type ValueDefinedCost = fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>;

const NO_VALUE_DEFINED_COST: Option<ValueDefinedCost> = None;

type Prefix = [u8; 32];

/// A subtree the replay has entered.
struct Entered {
    /// The Merk of the mirror under it.
    merk: MerkId,
    /// The Merk and the key of the element that hosts it.
    host: Option<(MerkId, Vec<u8>)>,
}

/// The subtrees entered under the elements of one Merk, by the key of the
/// element.
type Hosted = HashMap<Vec<u8>, Vec<Prefix>>;

/// The stack of the thread a trace is replayed on: GroveDB recurses.
const STACK_BYTES: usize = 64 << 20;

/// After this many operations that changed a node, every node of every
/// Merk is compared.
const COMPARE_ALL_EVERY: u64 = 400;

/// What the replay of one trace handed the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    /// The operations that changed a Merk node.
    writes: u64,
    /// The nodes written, the ones handed over as `ValueWritten`, and the
    /// ones whose value's bytes are not the bytes LigninDB held under the
    /// key before.
    nodes: u64,
    stated: u64,
    values_changed: u64,
    /// The nodes removed.
    removed: u64,
    /// The values the store wrote, and the ones it was handed and kept
    /// because the transaction held the same bytes.
    values_written: u64,
    values_kept: u64,
    /// The nodes compared with RocksDB's records.
    compared: u64,
}

impl Tally {
    fn add(&mut self, other: Tally) {
        self.writes += other.writes;
        self.nodes += other.nodes;
        self.stated += other.stated;
        self.values_changed += other.values_changed;
        self.removed += other.removed;
        self.values_written += other.values_written;
        self.values_kept += other.values_kept;
        self.compared += other.compared;
    }
}

/// How the replay states the kind of a written node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kinds {
    /// The narrowest kind the records show: a value is stated only where
    /// its bytes changed.
    Narrowest,
    /// Every node as `ValueWritten`, which is what Merk reports whenever it
    /// cannot prove less. The store has to find the values that did not
    /// change by itself.
    Widest,
}

/// The tree type the replay enters every subtree with. The subtree
/// directory keeps the bytes and does not read them.
const TREE_TYPE: &[u8] = b"tree";

/// The element under `key` in the Merk `merk`, as the host of the subtrees
/// it hosts now. The node that hosted a dropped Merk may be gone, and
/// another node may stand under its key.
fn host(grove: &Grove, merk: MerkId, key: &[u8]) -> LigninHost {
    let now = grove.find(merk, key).map(|now| grove.node(now));
    LigninHost {
        subtree: grove.merk(merk).prefix,
        key: key.to_vec(),
        hosted: (now.iter().flat_map(|now| &now.hosted))
            .map(|hosted| grove.merk(*hosted).prefix)
            .collect(),
    }
}

/// The subtree of the Merk `id`, with the element that hosts it.
fn subtree(grove: &Grove, id: MerkId) -> LigninSubtree {
    let merk = grove.merk(id);
    LigninSubtree {
        prefix: merk.prefix,
        host: merk.host.map(|at| {
            let node = grove.node(at);
            host(grove, node.merk, &node.key)
        }),
    }
}

/// Every node of the tree under `root`, as `(key, record)` in key order.
fn records<'db, S: NodeStore<'db>>(store: &S, root: Option<Vec<u8>>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut found = Vec::new();
    let mut ahead: Vec<Vec<u8>> = root.into_iter().collect();
    while let Some(key) = ahead.pop() {
        let node = store
            .get_node(&key, NO_VALUE_DEFINED_COST, GroveVersion::latest())
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

/// What one operation did to the nodes under one storage prefix.
struct Change<'c> {
    /// The Merk under the prefix now. A tree that is deleted and inserted
    /// again in one operation is two Merks of the mirror under one prefix.
    merk: MerkId,
    /// The key of each written node, with whether the node is new to its
    /// Merk, whether its value's bytes changed and whether its links did.
    written: Vec<(&'c [u8], bool, bool, bool)>,
    removed: Vec<&'c [u8]>,
}

impl Change<'_> {
    fn of(merk: MerkId) -> Self {
        Self {
            merk,
            written: Vec::new(),
            removed: Vec::new(),
        }
    }
}

struct Replay<'a, 't, 's> {
    db: &'a GroveDb,
    writer: &'a LigninWriter<'t, 's>,
    kinds: Kinds,
    /// The subtrees the replay has entered.
    entered: HashMap<Prefix, Entered>,
    /// The subtrees entered under each element, by the subtree of the
    /// element.
    under: HashMap<Prefix, Hosted>,
    tally: Tally,
}

impl Replay<'_, '_, '_> {
    /// Forgets the entered subtree `prefix` and every subtree under it.
    fn forget(&mut self, prefix: [u8; 32]) {
        self.entered.remove(&prefix);
        let below = self.under.remove(&prefix).unwrap_or_default();
        for prefix in below.into_values().flatten() {
            self.forget(prefix);
        }
    }

    /// Says to `batch` which subtrees the operation dropped and which it
    /// made: a drop for the outermost of the Merks that went, and an enter
    /// for each Merk that came, a host before what it hosts.
    fn subtrees(&mut self, grove: &Grove, changes: &ChangeSet, batch: &LigninBatch) {
        // A Merk goes when the element that hosts it is deleted or stops
        // being a tree, so it is found under an element the operation
        // touched.
        let events = changes.events.iter().map(|event| &event.node);
        let touched = events.chain(&changes.removed).chain(&changes.dropped);
        let mut gone = Vec::new();
        for node in touched.map(|id| grove.node(*id)) {
            let prefix = grove.merk(node.merk).prefix;
            let hosted = self
                .under
                .get(&prefix)
                .and_then(|under| under.get(&*node.key));
            for hosted in hosted.into_iter().flatten() {
                let Entered { merk, host } = &self.entered[hosted];
                if !grove.merk(*merk).live && !gone.iter().any(|(at, _)| at == hosted) {
                    gone.push((*hosted, host.clone()));
                }
            }
        }
        for (prefix, at) in &gone {
            let (merk, key) = at.as_ref().expect("the root Merk is not dropped");
            // A Merk inside a dropped Merk goes with it, and is not named.
            if grove.merk(*merk).live {
                batch.drop_subtree(&LigninSubtree {
                    prefix: *prefix,
                    host: Some(host(grove, *merk, key)),
                });
            }
            let under = self.under.get_mut(&grove.merk(*merk).prefix);
            if let Some(hosted) = under.and_then(|under| under.get_mut(key)) {
                hosted.retain(|hosted| hosted != prefix);
            }
        }
        for (prefix, _) in gone {
            self.forget(prefix);
        }

        for id in &changes.merks_created {
            let merk = grove.merk(*id);
            let at = merk.host.map(|at| grove.node(at));
            let subtree = subtree(grove, *id);
            (batch.enter(&subtree, TREE_TYPE)).expect("a Merk is among those its host hosts");
            let at = at.map(|node| (node.merk, node.key.to_vec()));
            if let Some((host, key)) = &at {
                let under = self.under.entry(grove.merk(*host).prefix).or_default();
                under.entry(key.clone()).or_default().push(merk.prefix);
            }
            let entered = Entered {
                merk: *id,
                host: at,
            };
            self.entered.insert(merk.prefix, entered);
        }
    }

    /// What `changes` did to the nodes under each prefix that holds a Merk
    /// now. A tree that is deleted and inserted again in one operation is
    /// two Merks of the mirror under one prefix: the nodes of the first
    /// went with its drop, and the nodes of the second are written.
    fn by_prefix<'c>(
        &self,
        grove: &'c Grove,
        changes: &'c ChangeSet,
    ) -> BTreeMap<[u8; 32], Change<'c>> {
        let mut by_prefix: BTreeMap<[u8; 32], Change<'c>> = BTreeMap::new();
        for event in &changes.events {
            let node = grove.node(event.node);
            let prefix = grove.merk(node.merk).prefix;
            let change = (by_prefix.entry(prefix)).or_insert_with(|| Change::of(node.merk));
            let written = (
                &*node.key,
                event.created,
                event.value_changed,
                event.links_changed,
            );
            change.written.push(written);
        }
        // A node that went with a Merk that is still there was cleared out
        // of it: Merk removes each such node by its key.
        let cleared = changes.dropped.iter();
        let cleared = cleared.filter(|id| grove.merk(grove.node(**id).merk).live);
        for id in changes.removed.iter().chain(cleared) {
            let node = grove.node(*id);
            let prefix = grove.merk(node.merk).prefix;
            let change = (by_prefix.entry(prefix)).or_insert_with(|| Change::of(node.merk));
            change.removed.push(&node.key);
        }
        by_prefix
    }

    /// Hands LigninDB what one operation did to the nodes of every Merk, as
    /// one batch, and compares each written node.
    fn write(&mut self, transaction: &Transaction, grove: &Grove, changes: &ChangeSet) {
        let storage = self.db.raw_storage();
        let rocks = |prefix: [u8; 32]| {
            storage
                .get_transactional_storage_context_by_subtree_prefix(prefix, None, transaction)
                .unwrap()
        };
        let by_prefix = self.by_prefix(grove, changes);
        let version = GroveVersion::latest();

        let batch = LigninBatch::new();
        self.subtrees(grove, changes, &batch);
        for (prefix, change) in &by_prefix {
            let subtree = subtree(grove, change.merk);
            let store = LigninNodeStore::deferred(self.writer, subtree, Some(&batch));
            let stored = rocks(*prefix);
            // A Merk the operation made has no node before it, whatever the
            // transaction holds under its prefix until the batch is applied.
            let fresh = changes.merks_created.contains(&change.merk);
            let mut change_set = store.change_set();
            for (key, created, value_changed, links_changed) in &change.written {
                let read = |node: CostResult<Option<TreeNode>, Error>| {
                    node.unwrap().expect("the node is read")
                };
                let node = read(stored.get_node(key, NO_VALUE_DEFINED_COST, version))
                    .expect("a node the observer saw is stored");
                // A node that is new to its Merk can stand under a key that
                // held the same bytes: a tree that is deleted and inserted
                // again in one operation.
                let value_changed = match (fresh, created) {
                    (true, _) => true,
                    (false, false) => *value_changed,
                    (false, true) => read(store.get_node(key, NO_VALUE_DEFINED_COST, version))
                        .is_none_or(|held| held.value_as_slice() != node.value_as_slice()),
                };
                let kind = match (self.kinds, created, value_changed, links_changed) {
                    (Kinds::Widest, ..) | (_, true, ..) | (_, _, true, _) => {
                        NodeChange::ValueWritten
                    }
                    (_, false, false, true) => NodeChange::StructureChanged,
                    (_, false, false, false) => NodeChange::HashOnly,
                };
                let prepared = change_set.prepare_node(&node, kind);
                (change_set.put_node(key, prepared, None)).expect("the node is put");
                self.tally.nodes += 1;
                self.tally.stated += u64::from(kind == NodeChange::ValueWritten);
                self.tally.values_changed += u64::from(value_changed);
            }
            for key in &change.removed {
                change_set.delete_node(key, None);
                self.tally.removed += 1;
            }
            let root = grove.merk(change.merk).root;
            change_set.state_root(root.map(|root| &*grove.node(root).key));
            (store.commit(change_set).unwrap()).expect("the change set is staged");
        }
        let applied = (batch.apply(self.writer).unwrap()).expect("the batch is applied");
        self.tally.writes += 1;
        self.tally.values_written += applied.values_written as u64;
        self.tally.values_kept += applied.values_kept as u64;

        for (prefix, change) in &by_prefix {
            let path = &grove.merk(change.merk).path;
            let store = LigninNodeStore::deferred(self.writer, LigninSubtree::alone(*prefix), None);
            let stored = rocks(*prefix);
            for (key, ..) in &change.written {
                let read = |node: CostResult<Option<TreeNode>, Error>| {
                    let node = node.unwrap().expect("the node is read");
                    node.map(|node| node.encode())
                };
                assert_eq!(
                    read(store.get_node(key, NO_VALUE_DEFINED_COST, version)),
                    read(stored.get_node(key, NO_VALUE_DEFINED_COST, version)),
                    "the node under {key:?} of the Merk {path:?}, write {}",
                    self.tally.writes
                );
                self.tally.compared += 1;
            }
            for key in &change.removed {
                let held = store.has_node(key).unwrap().expect("the node is read");
                assert!(!held, "the node under {key:?} of the Merk {path:?} is gone");
            }
        }
    }

    /// Compares every node of every Merk with the records RocksDB holds.
    fn compare_all(&mut self, transaction: &Transaction, grove: &Grove) {
        let storage = self.db.raw_storage();
        for (_, merk) in grove.merks().filter(|(_, merk)| merk.live) {
            let root = merk.root.map(|root| grove.node(root).key.to_vec());
            let store =
                LigninNodeStore::deferred(self.writer, LigninSubtree::alone(merk.prefix), None);
            let stored = storage
                .get_transactional_storage_context_by_subtree_prefix(merk.prefix, None, transaction)
                .unwrap();
            let expected = records(&stored, root.clone());
            let found = records(&store, root);
            let path = &merk.path;
            assert_eq!(
                found.len(),
                expected.len(),
                "the nodes of the Merk {path:?}"
            );
            for ((key, found), (_, expected)) in found.iter().zip(&expected) {
                assert_eq!(
                    found, expected,
                    "the node under {key:?} of the Merk {path:?}"
                );
            }
            self.tally.compared += expected.len() as u64;
        }
    }
}

fn replay(planned: &Planned, kinds: Kinds) -> Tally {
    let spec = &planned.spec;
    let dir = tempfile::tempdir().expect("a directory");
    let db = GroveDb::open(dir.path()).expect("a GroveDB");
    let (lignin, versions) = Db::new(1024);
    let path = lignin.path(&versions);
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().expect("a scope"));
    let mut replay = Replay {
        db: &db,
        writer: &writer,
        kinds,
        // The store is made with the subtree of the root Merk entered.
        entered: HashMap::from([(
            [0; 32],
            Entered {
                merk: MerkId::ROOT,
                host: None,
            },
        )]),
        under: HashMap::new(),
        tally: Tally::default(),
    };

    let mut observer = Observer::new(spec.grove_version);
    let mut transactions = Transactions::new(&db);
    for segment in &spec.segments {
        for op in segment.source.ops(spec.seed) {
            // A hand-written case may hold an operation that GroveDB
            // refuses. What it left behind is observed all the same.
            let _ = transactions.apply(&op, spec.grove_version).unwrap();
            let changes = observer.observe(&db, transactions.block());
            if changes.is_empty() {
                continue;
            }
            replay.write(transactions.block(), observer.grove(), &changes);
            if replay.tally.writes.is_multiple_of(COMPARE_ALL_EVERY) {
                replay.compare_all(transactions.block(), observer.grove());
            }
        }
    }
    replay.compare_all(transactions.block(), observer.grove());

    // Of the values it was handed, the store wrote exactly those whose
    // bytes it did not hold, and kept the others. Handed every node as
    // written, that is every value that did not change.
    let tally = replay.tally;
    assert_eq!(tally.values_written, tally.values_changed, "{}", spec.name);
    assert_eq!(tally.values_kept, tally.stated - tally.values_changed);
    assert!(kinds == Kinds::Narrowest || tally.stated == tally.nodes);
    tally
}

/// One replay: a trace, and how its nodes are handed over.
type Planning = (Planned, Kinds);

/// Replays every trace of `traces`, `jobs` at a time, and returns the sum
/// of what was handed over and the traces that failed, each with why.
fn replay_all(traces: &[Planning], jobs: usize) -> (Tally, Vec<String>) {
    let next = AtomicUsize::new(0);
    let done = Mutex::new((Tally::default(), Vec::new()));
    thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, traces.len().max(1)) {
            thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, || loop {
                    let Some((planned, kinds)) = traces.get(next.fetch_add(1, Ordering::Relaxed))
                    else {
                        return;
                    };
                    let outcome = catch_unwind(AssertUnwindSafe(|| replay(planned, *kinds)));
                    let mut done = done.lock().expect("no replay holds the lock");
                    match outcome {
                        Ok(tally) => done.0.add(tally),
                        Err(panic) => {
                            let why = panic
                                .downcast_ref::<String>()
                                .map(String::as_str)
                                .or_else(|| panic.downcast_ref::<&str>().copied())
                                .unwrap_or("a panic");
                            let why: String = why.chars().take(300).collect();
                            done.1.push(format!("{} ({kinds:?}): {why}", planned.file));
                        }
                    }
                })
                .expect("a thread");
        }
    });
    done.into_inner().expect("every replay has ended")
}

fn jobs() -> usize {
    std::env::var("LIGNIN_MERK_STORE_JOBS").map_or_else(
        |_| thread::available_parallelism().map_or(1, |jobs| jobs.get().min(6)),
        |jobs| jobs.parse().expect("a number of jobs"),
    )
}

#[test]
fn the_corpus_gives_the_same_trees_on_the_lignindb_node_store() {
    // Every trace of the standard corpus with the narrowest kinds, and
    // every other one again with the widest.
    let mut traces: Vec<Planning> = Vec::new();
    let only = std::env::var("LIGNIN_MERK_STORE_ONLY").ok();
    let planned = plan(Scale::Standard).into_iter();
    let planned =
        planned.filter(|planned| only.as_ref().is_none_or(|only| planned.file.contains(only)));
    for (n, planned) in planned.enumerate() {
        if n.is_multiple_of(2) {
            traces.push((planned.clone(), Kinds::Widest));
        }
        traces.push((planned, Kinds::Narrowest));
    }
    let (tally, mut failed) = replay_all(&traces, jobs());
    println!("{} replays: {tally:?}", traces.len());
    failed.sort();
    assert!(
        failed.is_empty(),
        "{} of {} replays failed:\n{}",
        failed.len(),
        traces.len(),
        failed.join("\n")
    );
    if only.is_none() {
        assert!(
            tally.nodes > 1_000_000 && tally.removed > 10_000,
            "{tally:?}"
        );
        assert!(
            tally.values_kept > 10_000 && tally.compared > tally.nodes,
            "{tally:?}"
        );
    }
}
