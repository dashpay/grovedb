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
//!    hashes, links, heights and aggregates.
//! 5. Every so often, and at the end of the trace, the LigninDB
//!    transaction is committed. The check of the committed tree pages
//!    walks every Merk from the root its entry in the subtree directory
//!    names, and must reach the keys RocksDB holds for that Merk and no
//!    others. The cell of the element that hosts a Merk must point, at the
//!    Merk's position, at that root, and no cell at another. Then every
//!    node of every Merk is read from the committed pages and compared, as
//!    a record, with RocksDB's. Equal records from the root down are an
//!    equal tree shape, and the root hash is a function of the root's
//!    record.
//!
//! The Merk logic itself runs on RocksDB here. That it gives the same tree
//! on a `LigninNodeStore` is held by the tests of `merk::store::lignin`,
//! which run the same batches on both.
//!
//! # What the store charges (MERK-03)
//!
//! The replay also holds the store's charges to RocksDB's, on the nodes of
//! the corpus:
//!
//! 6. Every change set that goes to LigninDB goes, call for call, to a
//!    second RocksDB storage that holds nothing else, and the two batches
//!    must cost the same when they are applied. Each written node is given
//!    one of the costs a writer gives, in turn: none, that of a new node
//!    and that of an update, whose value cost the storage layer checks
//!    against the record, and a cost that is taken as given, with bytes
//!    freed as plain numbers and by owner and epoch. Each removed node is
//!    given a writer's cost or none, and one with none is charged from the
//!    record the committed state holds. The costs are given by the change
//!    set the replay fills (`Billed`), and the replay itself hands every
//!    node over with none. The second storage is committed
//!    whenever the LigninDB transaction is, so both have the same committed
//!    state.
//! 7. Every node that is read back, after a write and after a commit, is
//!    read on both engines, and the two reads must cost the same: a seek
//!    and the length of RocksDB's record, which LigninDB works out from the
//!    cell. A lookup of a written node that reads no value costs the same
//!    again.
//!
//! The costs a writer gives are made up here. What Merk itself gives with
//! a node, on every grove version, is held by the tests of
//! `merk::store::lignin`, where a Merk runs on both engines.

#[path = "merk_store/hashes.rs"]
mod hashes;

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
use grovedb_costs::{
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{
            use_basic_sectioned_removal_addition_version, StorageRemovalPerEpochByIdentifier,
            StorageRemovedBytes,
        },
        StorageCost,
    },
    CostResult, OperationCost,
};
use grovedb_merk::{
    cost_model::HANDLE_COST_MODEL,
    store::{
        lignin::{LigninBatch, LigninHost, LigninNodeStore, LigninSubtree, LigninWriter},
        NodeChange, NodeChangeSet, NodeStore, NodeWriteCost,
    },
    tree::{kv::ValueDefinedCostType, TreeNode},
    Error,
};
use grovedb_storage::{
    rocksdb_storage::{test_utils::TempStorage, RocksDbStorage},
    FlatStore, Storage, StorageBatch,
};
use grovedb_version::version::GroveVersion;
use lignin_format::{CellAddress, CommitVersion};
use lignin_store::load::LoadBuffers;
use lignin_testkit::{
    golden::plan::{plan, Planned, Scale},
    layout_sim::{ChangeSet, Grove, MerkId, Observer},
    workload::apply::Transactions,
};
use lignin_tree::node::{Lease, NodePage};
use lignindb::{
    snapshot::Versions,
    txn::{
        testing::{Db, Outcome},
        Subtree, WriteTxn,
    },
    verify::Scope,
};

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

/// After this many operations that changed a node, the LigninDB
/// transaction is committed and every node of every Merk is compared.
const COMMIT_EVERY: u64 = 400;

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
    /// The seeks and the stored bytes the batches were charged. The cost
    /// of every batch is compared with the cost of the same batch on
    /// RocksDB.
    seeks_charged: u64,
    bytes_charged: u64,
    /// The removals handed over with no cost, and the bytes they loaded
    /// from the committed state.
    uncosted_removals: u64,
    committed_bytes_loaded: u64,
    /// The reads whose cost was compared with the cost of the same read on
    /// RocksDB, and the bytes they loaded.
    reads_charged: u64,
    bytes_loaded: u64,
    /// The cells of the Merks in use that the physical invariant checker
    /// walked in the store as the replay left it.
    checked: u64,
    /// The committed cells whose hashes Merk's hash rules made again, the
    /// hashes they made, and the value hashes only their writer can make.
    hashed: hashes::Made,
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
        self.seeks_charged += other.seeks_charged;
        self.bytes_charged += other.bytes_charged;
        self.uncosted_removals += other.uncosted_removals;
        self.committed_bytes_loaded += other.committed_bytes_loaded;
        self.reads_charged += other.reads_charged;
        self.bytes_loaded += other.bytes_loaded;
        self.checked += other.checked;
        self.hashed.cells += other.hashed.cells;
        self.hashed.hashes += other.hashed.hashes;
        self.hashed.stated += other.hashed.stated;
    }
}

/// The bytes a write frees, as plain numbers or by owner and epoch.
fn freed(sectioned: bool, bytes: u32) -> StorageRemovedBytes {
    if !sectioned {
        return StorageRemovedBytes::BasicStorageRemoval(bytes);
    }
    let mut by_owner = StorageRemovalPerEpochByIdentifier::default();
    by_owner
        .entry([(bytes % 3) as u8; 32])
        .or_default()
        .insert((bytes % 7) as u16, bytes);
    StorageRemovedBytes::SectionedStorageRemoval(by_owner)
}

/// The cost a writer gives with the `turn`th written node, by turns: none,
/// that of a new node and that of an update, both of which the storage
/// layer checks against the record, and one it takes as given.
fn write_cost(node: &TreeNode, turn: u64) -> Option<NodeWriteCost> {
    let sizes = || {
        (
            node.feature_type()
                .tree_feature_specialized_type_and_length(),
            node.child_ref_and_sum_size(true),
            node.child_ref_and_sum_size(false),
        )
    };
    // What the put is to pay for its value: what the storage layer charges
    // a put of this record that comes with no cost, less the stored key
    // (`cost-model.md` K1, K2).
    let stored_key_len = HANDLE_COST_MODEL.stored_key_len(node.key().len());
    let mut unbilled = OperationCost::default();
    unbilled
        .add_key_value_storage_costs(
            stored_key_len as u32,
            node.encoding_length() as u32,
            Some(sizes()),
            None,
        )
        .expect("a put with no cost of the writer's");
    let new_key = KeyValueStorageCost {
        new_node: true,
        ..Default::default()
    };
    let key_cost = (HANDLE_COST_MODEL.complete_new_key_cost(Some(new_key), stored_key_len))
        .expect("a cost stays a cost")
        .key_storage_cost
        .added_bytes;
    let paid = unbilled.storage_cost.added_bytes - key_cost;

    let storage_cost = match turn % 4 {
        0 => return None,
        1 => KeyValueStorageCost {
            value_storage_cost: StorageCost {
                added_bytes: paid,
                ..Default::default()
            },
            new_node: true,
            needs_value_verification: true,
            ..Default::default()
        },
        2 => KeyValueStorageCost {
            value_storage_cost: StorageCost {
                added_bytes: (turn % 5) as u32,
                replaced_bytes: paid - (turn % 5) as u32,
                removed_bytes: freed(turn % 8 == 2, (turn % 90) as u32),
            },
            needs_value_verification: true,
            ..Default::default()
        },
        _ => KeyValueStorageCost {
            value_storage_cost: StorageCost {
                added_bytes: (turn % 300) as u32,
                replaced_bytes: (turn % 11) as u32,
                removed_bytes: freed(turn % 8 == 3, (turn % 50) as u32),
            },
            ..Default::default()
        },
    };
    let (feature, left_child, right_child) = sizes();
    Some(NodeWriteCost {
        feature,
        left_child,
        right_child,
        storage_cost,
    })
}

/// The cost a writer gives with the `turn`th removed node: none for every
/// other one, and otherwise bytes freed from the key and from the value.
fn removal_cost(turn: u64) -> Option<KeyValueStorageCost> {
    if turn.is_multiple_of(2) {
        return None;
    }
    let cost = |sectioned, bytes| StorageCost {
        removed_bytes: freed(sectioned, bytes),
        ..Default::default()
    };
    Some(KeyValueStorageCost {
        key_storage_cost: cost(turn % 4 == 3, 34 + (turn % 9) as u32),
        value_storage_cost: cost(turn % 8 == 1, (turn % 400) as u32),
        ..Default::default()
    })
}

/// What [`Billed::prepare_node`] keeps of a node for each engine: what its
/// change set prepared, and the cost the node is put with.
type BilledNode<L, M> = ((L, Option<NodeWriteCost>), (M, Option<NodeWriteCost>));

/// The change set of one Merk on LigninDB with the change set of the same
/// Merk on the mirror. Each change goes to both. The replay hands a node
/// over with no cost, and each written and each removed node is given one
/// of the costs a writer gives here, the same on both.
struct Billed<L, M> {
    lignin: L,
    mirror: M,
    /// How many nodes the replay has written so far, which the cost of the
    /// next written node is chosen by.
    written: u64,
    /// The same for the nodes it has removed.
    removed: u64,
    uncosted_removals: u64,
}

impl<L: NodeChangeSet, M: NodeChangeSet> Billed<L, M> {
    fn new(lignin: L, mirror: M, tally: &Tally) -> Self {
        Self {
            lignin,
            mirror,
            written: tally.nodes,
            removed: tally.removed,
            uncosted_removals: 0,
        }
    }

    fn prepare_node(
        &mut self,
        node: &TreeNode,
        change: NodeChange,
    ) -> BilledNode<L::Prepared, M::Prepared> {
        let turn = self.written;
        self.written += 1;
        (
            (
                self.lignin.prepare_node(node, change),
                write_cost(node, turn),
            ),
            (
                self.mirror.prepare_node(node, change),
                write_cost(node, turn),
            ),
        )
    }

    fn put_node(
        &mut self,
        key: &[u8],
        (lignin, mirror): BilledNode<L::Prepared, M::Prepared>,
        unbilled: Option<NodeWriteCost>,
    ) -> Result<(), Error> {
        assert!(unbilled.is_none(), "the replay bills no node itself");
        self.lignin.put_node(key, lignin.0, lignin.1)?;
        (self.mirror.put_node(key, mirror.0, mirror.1))
            .expect("the mirror takes the put LigninDB took");
        Ok(())
    }

    fn delete_node(&mut self, key: &[u8], unbilled: Option<KeyValueStorageCost>) {
        assert!(unbilled.is_none(), "the replay bills no removal itself");
        let cost = removal_cost(self.removed);
        self.removed += 1;
        self.uncosted_removals += u64::from(cost.is_none());
        self.lignin.delete_node(key, cost.clone());
        self.mirror.delete_node(key, cost);
    }

    fn state_root(&mut self, root_key: Option<&[u8]>) {
        self.lignin.state_root(root_key);
        self.mirror.state_root(root_key);
    }

    /// The two change sets, and how many nodes were removed with no cost.
    fn into_parts(self) -> (L, M, u64) {
        (self.lignin, self.mirror, self.uncosted_removals)
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

/// The nodes of a tree, as `(key, record, cost)` in key order: each record
/// with what the read of it cost.
type Records = Vec<(Vec<u8>, Vec<u8>, OperationCost)>;

/// Every node of the tree under `root`, each with what its read cost.
fn records<'db, S: NodeStore<'db>>(store: &S, root: Option<Vec<u8>>) -> Records {
    let mut found = Vec::new();
    let mut ahead: Vec<Vec<u8>> = root.into_iter().collect();
    while let Some(key) = ahead.pop() {
        let read = store.get_node(&key, NO_VALUE_DEFINED_COST, GroveVersion::latest());
        let node = (read.value)
            .expect("the node is read")
            .expect("a node that is linked is stored");
        for left in [true, false] {
            ahead.extend(node.link(left).map(|link| link.key().to_vec()));
        }
        found.push((key, node.encode(), read.cost));
    }
    found.sort_by(|(a, ..), (b, ..)| a.cmp(b));
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

/// What `changes` did to the nodes under each prefix that holds a Merk
/// now. A tree that is deleted and inserted again in one operation is
/// two Merks of the mirror under one prefix: the nodes of the first
/// went with its drop, and the nodes of the second are written.
fn by_prefix<'c>(grove: &'c Grove, changes: &'c ChangeSet) -> BTreeMap<Prefix, Change<'c>> {
    let mut by_prefix: BTreeMap<Prefix, Change<'c>> = BTreeMap::new();
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

type MirrorTransaction<'a> = <RocksDbStorage as Storage<'a>>::Transaction;

struct Replay<'a> {
    db: &'a GroveDb,
    /// A RocksDB storage that is handed every change set LigninDB is
    /// handed and nothing else: what each batch costs is compared with it.
    mirror: &'a TempStorage,
    /// How a plain removal and a sectioned one add up on the grove version
    /// of the trace.
    arithmetic: u16,
    /// The subtrees that went with a drop and that the mirror still holds
    /// the records of.
    dropped: Vec<Prefix>,
    kinds: Kinds,
    /// The subtrees the replay has entered.
    entered: HashMap<Prefix, Entered>,
    /// The subtrees entered under each element, by the subtree of the
    /// element.
    under: HashMap<Prefix, Hosted>,
    tally: Tally,
}

impl Replay<'_> {
    /// Forgets the entered subtree `prefix` and every subtree under it.
    fn forget(&mut self, prefix: Prefix) {
        self.entered.remove(&prefix);
        self.dropped.push(prefix);
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
            let subtree = subtree(grove, *id);
            (batch.enter(&subtree, TREE_TYPE)).expect("a Merk is among those its host hosts");
            let at = merk.host.map(|at| {
                let node = grove.node(at);
                (node.merk, node.key.to_vec())
            });
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

    /// Hands LigninDB what one operation did to the nodes of every Merk, as
    /// one batch, and compares each written node. The mirror is handed the
    /// same change sets, and the two batches must cost the same.
    fn write(
        &mut self,
        writer: &LigninWriter<'_, '_>,
        mirror_transaction: &MirrorTransaction<'_>,
        transaction: &Transaction,
        grove: &Grove,
        changes: &ChangeSet,
    ) {
        let storage = self.db.raw_storage();
        let rocks = |prefix: Prefix| {
            storage
                .get_transactional_storage_context_by_subtree_prefix(prefix, None, transaction)
                .unwrap()
        };
        let by_prefix = by_prefix(grove, changes);
        let version = GroveVersion::latest();
        let _arithmetic = use_basic_sectioned_removal_addition_version(self.arithmetic);

        let batch = LigninBatch::new();
        let mirror_batch = StorageBatch::new();
        self.subtrees(grove, changes, &batch);
        // A dropped subtree loses its nodes on LigninDB with no write of
        // each. The mirror loses them here, outside the batch that is
        // compared, and like the drop inside its transaction.
        for prefix in std::mem::take(&mut self.dropped) {
            let gone = self
                .mirror
                .get_immediate_storage_context_by_subtree_prefix(prefix, mirror_transaction)
                .unwrap();
            (FlatStore::clear(&gone).unwrap()).expect("a dropped subtree is cleared");
        }
        for (prefix, change) in &by_prefix {
            let subtree = subtree(grove, change.merk);
            let store = LigninNodeStore::deferred(writer, subtree, Some(&batch));
            let mirrored = self
                .mirror
                .get_transactional_storage_context_by_subtree_prefix(
                    *prefix,
                    Some(&mirror_batch),
                    mirror_transaction,
                )
                .unwrap();
            let stored = rocks(*prefix);
            // A Merk the operation made has no node before it, whatever the
            // transaction holds under its prefix until the batch is applied.
            let fresh = changes.merks_created.contains(&change.merk);
            let mut change_set = Billed::new(
                store.change_set(),
                NodeStore::change_set(&mirrored),
                &self.tally,
            );
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
            let (change_set, mirror_set, uncosted_removals) = change_set.into_parts();
            self.tally.uncosted_removals += uncosted_removals;
            let staged = store.commit(change_set);
            let mirror_staged = NodeStore::commit(&mirrored, mirror_set);
            assert_eq!(
                staged.cost, mirror_staged.cost,
                "what staging a change set is charged"
            );
            (staged.value).expect("the change set is staged");
            (mirror_staged.value).expect("the change set is staged on the mirror");
        }
        let expected = self
            .mirror
            .commit_multi_context_batch(mirror_batch, Some(mirror_transaction));
        (expected.value).expect("the batch is applied on the mirror");
        let applied = batch.apply(writer);
        assert_eq!(
            applied.cost,
            expected.cost,
            "what the batch of write {} is charged",
            self.tally.writes + 1
        );
        let charged = &applied.cost;
        self.tally.seeks_charged += u64::from(charged.seek_count);
        self.tally.bytes_charged += u64::from(charged.storage_cost.added_bytes)
            + u64::from(charged.storage_cost.replaced_bytes);
        // A batch loads nothing but the committed records of the nodes it
        // removes with no cost.
        self.tally.committed_bytes_loaded += charged.storage_loaded_bytes;
        let applied = applied.value.expect("the batch is applied");
        self.tally.writes += 1;
        self.tally.values_written += applied.values_written as u64;
        self.tally.values_kept += applied.values_kept as u64;

        for (prefix, change) in &by_prefix {
            let path = &grove.merk(change.merk).path;
            let store = LigninNodeStore::deferred(writer, LigninSubtree::alone(*prefix), None);
            let stored = rocks(*prefix);
            for (key, ..) in &change.written {
                // The read costs what it costs on RocksDB: a seek and the
                // length of the record RocksDB holds.
                let read = |node: CostResult<Option<TreeNode>, Error>| {
                    let cost = node.cost;
                    let node = node.value.expect("the node is read");
                    (node.map(|node| node.encode()), cost)
                };
                let found = read(store.get_node(key, NO_VALUE_DEFINED_COST, version));
                assert_eq!(
                    found,
                    read(stored.get_node(key, NO_VALUE_DEFINED_COST, version)),
                    "the node under {key:?} of the Merk {path:?}, write {}",
                    self.tally.writes
                );
                // A lookup that reads no value costs what the fetch costs.
                let held = store.has_node(key);
                assert_eq!(
                    held.cost, found.1,
                    "what the lookup of the node under {key:?} is charged"
                );
                assert!(held.value.expect("the node is looked up"));
                self.tally.compared += 1;
                self.tally.reads_charged += 2;
                self.tally.bytes_loaded += 2 * found.1.storage_loaded_bytes;
            }
            // RocksDB can hold a record under the key of a node that left its
            // tree: a row that the repair of a released version unlinked
            // (GroveDB #1063). The mirror was handed the removal, as
            // LigninDB was, so the lookup is compared with the mirror's.
            let mirrored = self
                .mirror
                .get_transactional_storage_context_by_subtree_prefix(
                    *prefix,
                    None,
                    mirror_transaction,
                )
                .unwrap();
            let lookup = |held: CostResult<bool, Error>| {
                (held.value.expect("the node is looked up"), held.cost)
            };
            for key in &change.removed {
                let (held, cost) = lookup(store.has_node(key));
                assert_eq!(
                    (held, cost),
                    lookup(mirrored.has_node(key)),
                    "the lookup of the removed node under {key:?} and what it is charged"
                );
                assert!(!held, "the node under {key:?} of the Merk {path:?} is gone");
                self.tally.reads_charged += 1;
            }
        }
    }

    /// Compares what LigninDB has committed with the records RocksDB
    /// holds. `committed` says what the check of the tree pages reached
    /// from the root of each Merk in the subtree directory, and every node
    /// of every Merk is read through `reader`.
    fn compare_all(
        &mut self,
        committed: &Committed<'_>,
        reader: &WriteTxn<'_>,
        transaction: &Transaction,
        grove: &Grove,
    ) {
        let storage = self.db.raw_storage();
        let (mut with_nodes, mut hosted) = (0, 0);
        for (id, merk) in grove.merks().filter(|(_, merk)| merk.live) {
            let root = merk.root.map(|root| grove.node(root).key.to_vec());
            let store = LigninNodeStore::read_only(reader, LigninSubtree::alone(merk.prefix));
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
            for ((key, found, cost), (_, expected, expected_cost)) in found.iter().zip(&expected) {
                assert_eq!(
                    found, expected,
                    "the node under {key:?} of the Merk {path:?}"
                );
                // Each read costs what the read of the same node costs on
                // RocksDB: a sum over the tree would let two charges trade
                // bytes.
                assert_eq!(
                    cost, expected_cost,
                    "what the read of the node under {key:?} of the Merk {path:?} is charged"
                );
                self.tally.reads_charged += 1;
                self.tally.bytes_loaded += cost.storage_loaded_bytes;
            }
            let keys: Vec<&[u8]> = expected.iter().map(|(key, ..)| key.as_slice()).collect();
            let subtree = Subtree::new(merk.prefix);
            let from_root = committed.reached.get(&subtree);
            assert_eq!(
                from_root.map_or(Vec::new(), |keys| keys.iter().map(Vec::as_slice).collect()),
                keys,
                "the keys under the root of the Merk {path:?} in the directory"
            );
            with_nodes += usize::from(!keys.is_empty());
            self.tally.compared += expected.len() as u64;

            // The cell of the element that hosts the Merk points, at the
            // Merk's position, at the root its directory entry names.
            let Some(host) = merk.host.map(|host| grove.node(host)) else {
                continue;
            };
            let position = (host.hosted.iter().position(|hosted| *hosted == id))
                .expect("a Merk is among those its host hosts");
            let cell = reader
                .node(
                    Subtree::new(grove.merk(host.merk).prefix),
                    &host.key,
                    |node| node.and_then(|node| node.handle()),
                )
                .expect("the host is read")
                .expect("a committed node has a place");
            let pointers = committed.hosted_roots(cell.address());
            assert_eq!(
                pointers.get(position).copied().flatten(),
                committed.roots.get(&subtree).copied(),
                "the pointer to the Merk {path:?} in its host"
            );
            hosted += u64::from(!keys.is_empty());
        }
        let reached = committed.reached.len();
        assert_eq!(reached, with_nodes, "the Merks that have a node");
        assert_eq!(committed.pointers, hosted, "the pointers to a hosted root");
    }
}

/// What the latest commit of a LigninDB store holds, as its check of the
/// tree pages and its subtree directory have it.
struct Committed<'a> {
    db: &'a Db,
    at: CommitVersion,
    /// The keys reached from the root of each Merk that has a node.
    reached: BTreeMap<Subtree, Vec<Vec<u8>>>,
    /// The cell of the root of each Merk that has a node.
    roots: BTreeMap<Subtree, CellAddress>,
    /// How many pointers to the root of a hosted Merk the cells hold.
    pointers: u64,
}

impl<'a> Committed<'a> {
    fn check(db: &'a Db, versions: &Versions) -> Self {
        let at = versions.latest();
        Self {
            db,
            at,
            reached: db.check_trees(versions, at),
            roots: db.roots(versions, at),
            pointers: db.counts.hosted.load(Ordering::Relaxed),
        }
    }

    /// The roots the cell at `host` points at, by the position of the Merk
    /// among those its node hosts.
    fn hosted_roots(&self, host: CellAddress) -> Vec<Option<CellAddress>> {
        let (loader, mut buffers) = (self.db.loader(), LoadBuffers::new());
        let frame = (loader.get(host.page(), self.at, &mut buffers)).expect("the page of a host");
        let page = NodePage::open(&frame, Lease::take()).expect("a tree page");
        let node = page.node(host.slot()).expect("the cell of a host");
        node.hosted_roots().collect()
    }
}

fn replay(planned: &Planned, kinds: Kinds) -> Tally {
    let spec = &planned.spec;
    let dir = tempfile::tempdir().expect("a directory");
    let db = GroveDb::open(dir.path()).expect("a GroveDB");
    let (lignin, versions) = Db::new(1024);
    let path = lignin.path(&versions);
    let mirror = TempStorage::new();
    let mut replay = Replay {
        db: &db,
        mirror: &mirror,
        arithmetic: spec
            .grove_version
            .grovedb_versions
            .storage_costs
            .add_basic_storage_removal_to_sectioned_storage_removal,
        dropped: Vec::new(),
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
    let mut ops = (spec.segments.iter()).flat_map(|segment| segment.source.ops(spec.seed));
    let mut ended = false;
    while !ended {
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().expect("a scope"));
        let mirror_transaction = mirror.start_transaction();
        ended = true;
        for op in ops.by_ref() {
            // A hand-written case may hold an operation that GroveDB
            // refuses. What it left behind is observed all the same.
            let _ = transactions.apply(&op, spec.grove_version).unwrap();
            let changes = observer.observe(&db, transactions.block());
            if changes.is_empty() {
                continue;
            }
            let (block, grove) = (transactions.block(), observer.grove());
            replay.write(&writer, &mirror_transaction, block, grove, &changes);
            if replay.tally.writes.is_multiple_of(COMMIT_EVERY) {
                ended = false;
                break;
            }
        }
        writer.into_scope().merge();
        let outcome = lignin.commit(&versions, &transaction);
        assert_ne!(outcome.expect("a commit"), Outcome::Conflict);
        // The mirror has the committed state LigninDB has, which a removal
        // with no cost is charged from.
        (mirror.commit_transaction(mirror_transaction).unwrap()).expect("the mirror commits");

        let committed = Committed::check(&lignin, &versions);
        let reader = path.begin();
        let (block, grove) = (transactions.block(), observer.grove());
        replay.compare_all(&committed, &reader, block, grove);

        // The hashes of the committed pages are the ones Merk's hash
        // rules make, and the root hash they make is GroveDB's.
        let root_hash = db.root_hash(Some(block), spec.grove_version).unwrap();
        let made = hashes::check(
            &lignin,
            &versions,
            committed.at,
            &committed.roots,
            Subtree::new(grove.merk(MerkId::ROOT).prefix),
            root_hash.expect("the root hash of the grove"),
            spec.grove_version,
        );
        replay.tally.add(Tally {
            hashed: made,
            ..Tally::default()
        });
    }

    // The store as the replay leaves it passes the physical invariant
    // checker (`lignindb::verify`): the pages, the trees, the directory
    // against the cells, the values, the page ids and the page table. No
    // hash is recomputed here: the rules that make Merk's hashes for every
    // kind of tree are the hash bridge's (MERK-02).
    let report = lignin.verify(versions.latest(), Scope::Still, None);
    let unsound = lignin.unsound(&report);
    assert!(
        unsound.is_empty() && report.more_findings == 0,
        "{}: the store is not sound: {unsound:?}",
        spec.name
    );
    replay.tally.checked += report.census.live_cells;

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
        // The charges that were compared are of the size of the corpus.
        assert!(
            tally.seeks_charged > tally.nodes && tally.bytes_charged > 100_000_000,
            "{tally:?}"
        );
        assert!(
            tally.uncosted_removals > 10_000 && tally.committed_bytes_loaded > 1_000_000,
            "{tally:?}"
        );
        assert!(
            tally.reads_charged > tally.nodes && tally.bytes_loaded > 100_000_000,
            "{tally:?}"
        );
        assert!(tally.checked > 10_000, "{tally:?}");
        let hashed = tally.hashed;
        assert_eq!(hashed.cells, tally.compared, "{tally:?}");
        assert!(
            hashed.hashes > 3 * hashed.cells && hashed.stated > 10_000,
            "{tally:?}"
        );
    }
}
