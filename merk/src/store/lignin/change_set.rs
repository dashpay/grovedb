//! The change set of a Merk on LigninDB, and the batch of one operation.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    sync::{Mutex, MutexGuard, PoisonError},
};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostResult,
    CostsExt, OperationCost,
};
use grovedb_storage::Error as StorageError;
use lignin_format::tree_page::{HashField, Side};
use lignindb::{
    batch::{Resolved, StagedBatch, Written as Staged},
    txn::{Batch, Column, Hosted, MerkWrite, NodePut, Subtree, TxnError, Unit, ValuePut},
};

use super::{
    access::{
        column, fault, link_of, Access, BatchPart, LinkWrite, Location, MerkChange, NodeWrite,
        Prefix, RecordChange, RootWrite, SubtreeChange, ValueWrite, Write, Written,
    },
    feature::{encode_aggregate, encode_feature},
    hash::{held_link, stale_hashes, HeldLink},
    record_len, refused, LigninHost, LigninSubtree, LigninView, MODEL,
};
use crate::{
    cost_model::{self, BatchCharge, ImmediateBatchCharge, Keyspace},
    merk::defaults::ROOT_KEY_KEY,
    store::{NodeChange, NodeChangeSet, NodeWriteCost},
    tree::{TreeNode, NULL_HASH},
    Error, Link,
};

/// What the store keeps of a node between `prepare_node` and `put_node`:
/// its feature, its hashes and its links as they are written, which of the
/// hashes are left to the engine, the value only where it was written, a
/// copy of its key that `put_node` replaces with the key of the record,
/// and the record length its put is charged by (`cost-model.md` N1). Of a
/// node that cannot be written, what is kept is why: it has no record
/// length, or its aggregates do not add up.
#[derive(Debug)]
pub struct PreparedNode(Result<(NodeWrite, usize), Error>);

/// A link as it is written, with what Merk holds of it. A hash that Merk
/// does not hold is left to the engine: the bytes written for it are no
/// hash (`store::lignin::hash`).
fn link_write(link: &Link) -> Result<(LinkWrite, HeldLink), Error> {
    let held = held_link(link)?;
    let write = LinkWrite {
        key: link.key().to_vec(),
        hash: held.hash.unwrap_or(NULL_HASH),
        heights: held.heights,
        aggregate: encode_aggregate(&held.aggregate),
    };
    Ok((write, held))
}

fn prepare(node: &TreeNode, change: NodeChange) -> Result<(NodeWrite, usize), Error> {
    use ed::Encode;

    let stale = stale_hashes(node);
    // A node with no modified link is measured first, so one that has no
    // record length is not taken apart.
    let encoded_len = match stale.is_empty() {
        true => Some(cost_model::node_record_len(MODEL, node)?),
        false => None,
    };
    let left = node.link(true).map(link_write).transpose()?;
    let right = node.link(false).map(link_write).transpose()?;
    let record_len = match encoded_len {
        Some(record_len) => record_len,
        // Merk has no record of a node with a modified link. Its length is
        // that of the record it has once the hash of the link is made,
        // which is made of its parts (`cost-model.md` N1).
        None => {
            let link_len = |link: &Option<(LinkWrite, HeldLink)>| {
                let len = (link.as_ref()).map(|(link, held)| held.record_len(link.key.len()));
                len.transpose()
            };
            let feature = node.feature_type().encoding_length();
            MODEL.node_record_len(
                node.value_as_slice().len(),
                feature.map_err(Error::EdError)?,
                link_len(&left)?,
                link_len(&right)?,
            )
        }
    };
    Ok((
        NodeWrite {
            key: node.key().to_vec(),
            feature: encode_feature(&node.feature_type()),
            value_hash: *node.value_hash(),
            kv_hash: *node.kv_hash(),
            left: left.map(|(link, _)| link),
            right: right.map(|(link, _)| link),
            value: match change {
                NodeChange::ValueWritten => ValueWrite::Stated(node.value_as_slice().to_vec()),
                NodeChange::StructureChanged | NodeChange::HashOnly => ValueWrite::Kept,
            },
            stale,
            hint: None,
            read: None,
            hosted: None,
        },
        record_len,
    ))
}

/// The sizes the cost rules take with a put of a node that is charged when
/// its batch is applied.
struct PutSizes {
    record_len: usize,
    children: ChildrenSizesWithIsSumTree,
    cost: Option<KeyValueStorageCost>,
}

/// A write of a record that is no node, with the writer's cost.
#[derive(Debug)]
struct RecordOp {
    change: RecordChange,
    cost: Option<KeyValueStorageCost>,
}

/// The changes of one commit to one Merk on LigninDB.
///
/// It holds each written node as the parts a cell has, and the value of a
/// node only where Merk wrote it (`NodeChange::ValueWritten`). Nothing is
/// placed and nothing is charged for a staged write before the change set
/// is applied: on an immediate handle when it is committed, and otherwise
/// with the batch of its operation ([`LigninBatch`]).
pub struct LigninChangeSet {
    subtree: LigninSubtree,
    nodes: Vec<(NodeWrite, PutSizes)>,
    removed: Vec<(Vec<u8>, Option<KeyValueStorageCost>)>,
    root: RootWrite,
    /// Whether the change set is the whole Merk
    /// ([`NodeChangeSet::state_whole_merk`]).
    whole: bool,
    records: Vec<RecordOp>,
    /// The charge of an immediate handle's change set, to which each change
    /// is added as it is made.
    immediate: Option<ImmediateBatchCharge>,
}

impl fmt::Debug for LigninChangeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LigninChangeSet")
            .field("subtree", &self.subtree)
            .field("nodes", &self.nodes.len())
            .field("removed", &self.removed.len())
            .field("root", &self.root)
            .field("whole", &self.whole)
            .field("records", &self.records)
            .finish_non_exhaustive()
    }
}

fn stored_key_len(key: &[u8]) -> usize {
    MODEL.stored_key_len(key.len())
}

impl LigninChangeSet {
    pub(super) fn new(subtree: LigninSubtree, immediate: bool) -> Self {
        Self {
            subtree,
            nodes: Vec::new(),
            removed: Vec::new(),
            root: RootWrite::Keep,
            whole: false,
            records: Vec::new(),
            immediate: immediate.then(|| MODEL.immediate_batch()),
        }
    }

    /// A change set of one node, written outside a commit of its Merk. The
    /// root of the Merk is the node it was.
    pub(super) fn of_node(
        subtree: LigninSubtree,
        node: &TreeNode,
        change: NodeChange,
    ) -> Result<Self, Error> {
        let mut change_set = Self::new(subtree, false);
        let (node, record_len) = prepare(node, change)?;
        change_set.nodes.push((
            node,
            PutSizes {
                record_len,
                children: None,
                cost: None,
            },
        ));
        Ok(change_set)
    }

    /// A change set of one record that is no node, written or removed
    /// with the writer's cost, if it gave one.
    pub(super) fn of_record(
        subtree: LigninSubtree,
        keyspace: Keyspace,
        key: &[u8],
        value: Option<&[u8]>,
        cost: Option<KeyValueStorageCost>,
    ) -> Self {
        let mut change_set = Self::new(subtree, false);
        change_set.record(keyspace, key, value, cost);
        change_set
    }

    /// Takes the hint of each written node from `hints`, and the handle its
    /// value was read with from `reads`.
    pub(super) fn hint(
        &mut self,
        hints: &HashMap<Vec<u8>, Location>,
        reads: &HashMap<Vec<u8>, Option<Location>>,
    ) {
        for (node, _) in &mut self.nodes {
            node.hint = hints.get(&node.key).copied();
            node.read = reads.get(&node.key).copied().flatten();
        }
    }

    /// Refuses a change set that leaves the hash of a child to the engine
    /// and does not write the child: the engine makes that hash from the
    /// child's cell, which would be the child as it was before.
    pub(super) fn check_stale(&self) -> Result<(), Error> {
        let mut written: Option<HashSet<&[u8]>> = None;
        for (node, _) in self.nodes.iter().filter(|(node, _)| !node.stale.is_empty()) {
            let written = written.get_or_insert_with(|| {
                let keys = self.nodes.iter().map(|(node, _)| node.key.as_slice());
                keys.collect()
            });
            for (side, link) in [(Side::Left, &node.left), (Side::Right, &node.right)] {
                let stale = node.stale.has(HashField::Child(side));
                let child = link.as_ref().filter(|_| stale);
                if child.is_some_and(|child| !written.contains(child.key.as_slice())) {
                    return Err(refused(
                        "the hash of a child is left to the engine, and the child is not \
                         written with its parent",
                    ));
                }
            }
        }
        Ok(())
    }

    fn record(
        &mut self,
        keyspace: Keyspace,
        key: &[u8],
        value: Option<&[u8]>,
        cost: Option<KeyValueStorageCost>,
    ) {
        let change = RecordChange {
            keyspace,
            subtree: self.subtree.prefix,
            key: key.to_vec(),
            value: value.map(<[u8]>::to_vec),
        };
        self.records.push(RecordOp { change, cost });
    }

    fn put_record(
        &mut self,
        keyspace: Keyspace,
        key: &[u8],
        value: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        if let Some(charge) = &mut self.immediate {
            charge
                .put(
                    keyspace,
                    stored_key_len(key),
                    value.len(),
                    None,
                    cost.clone(),
                )
                .map_err(Error::CostsError)?;
        }
        self.record(keyspace, key, Some(value), cost);
        Ok(())
    }

    fn delete_record(&mut self, keyspace: Keyspace, key: &[u8], cost: Option<KeyValueStorageCost>) {
        if let Some(charge) = &mut self.immediate {
            charge.delete(cost.clone());
        }
        self.record(keyspace, key, None, cost);
    }

    /// What the change set of an immediate handle is charged, and the
    /// change set as a write of its own: its Merk has no host in it, so a
    /// Merk that has a host is anchored.
    pub(super) fn into_write(mut self) -> (OperationCost, Write) {
        let cost = self.immediate.take().map(ImmediateBatchCharge::finish);
        // An immediate handle's writes are made in the order they were
        // given: of two writes of one record the later stands.
        let mut records: Vec<RecordChange> = Vec::with_capacity(self.records.len());
        for RecordOp { change, .. } in std::mem::take(&mut self.records) {
            records.retain(|at| (at.keyspace, &at.key) != (change.keyspace, &change.key));
            records.push(change);
        }
        let anchored = self.subtree.host.is_some();
        let write = Write {
            subtrees: Vec::new(),
            merks: match self.writes_nodes() {
                true => vec![self.into_merk(anchored)],
                false => Vec::new(),
            },
            records,
        };
        (cost.unwrap_or_default(), write)
    }

    /// Whether the change set writes or removes a node. The root of a Merk
    /// changes only with a node, so a root that is stated with none says
    /// nothing new: a commit that leaves an empty Merk empty.
    fn writes_nodes(&self) -> bool {
        !self.nodes.is_empty() || !self.removed.is_empty()
    }

    fn into_merk(self, anchored: bool) -> MerkChange {
        MerkChange {
            subtree: self.subtree.prefix,
            nodes: self.nodes.into_iter().map(|(node, _)| node).collect(),
            removed: self.removed.into_iter().map(|(key, _)| key).collect(),
            root: self.root,
            anchored,
            whole: self.whole,
        }
    }
}

impl NodeChangeSet for LigninChangeSet {
    type Prepared = PreparedNode;

    fn prepare_node(&mut self, node: &TreeNode, change: NodeChange) -> PreparedNode {
        PreparedNode(prepare(node, change))
    }

    fn put_node(
        &mut self,
        key: &[u8],
        prepared: PreparedNode,
        cost: Option<NodeWriteCost>,
    ) -> Result<(), Error> {
        let (mut node, record_len) = prepared.0?;
        // The record key of the write is the key Merk puts the node under.
        node.key = key.to_vec();
        let (children, cost) = match cost {
            Some(cost) => (
                Some((cost.feature, cost.left_child, cost.right_child)),
                Some(cost.storage_cost),
            ),
            None => (None, None),
        };
        let cost = MODEL.complete_new_key_cost(cost, stored_key_len(key));
        let sizes = match &mut self.immediate {
            Some(charge) => {
                charge
                    .put(
                        Keyspace::Data,
                        stored_key_len(key),
                        record_len,
                        children,
                        cost,
                    )
                    .map_err(Error::CostsError)?;
                PutSizes {
                    record_len,
                    children: None,
                    cost: None,
                }
            }
            None => PutSizes {
                record_len,
                children,
                cost,
            },
        };
        self.nodes.push((node, sizes));
        Ok(())
    }

    fn delete_node(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>) {
        if let Some(charge) = &mut self.immediate {
            charge.delete(cost.clone());
        }
        self.removed.push((key.to_vec(), cost));
    }

    fn state_root(&mut self, root_key: Option<&[u8]>) {
        self.root = RootWrite::Set(root_key.map(<[u8]>::to_vec));
    }

    fn state_whole_merk(&mut self) {
        self.whole = true;
    }

    fn put_root_key(
        &mut self,
        root_key: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.put_record(Keyspace::Roots, ROOT_KEY_KEY, root_key, cost)
    }

    fn delete_root_key(&mut self, cost: Option<KeyValueStorageCost>) {
        self.delete_record(Keyspace::Roots, ROOT_KEY_KEY, cost);
    }

    fn put_aux_record(
        &mut self,
        key: &[u8],
        value: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.put_record(Keyspace::Aux, key, value, cost)
    }

    fn delete_aux_record(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>) {
        self.delete_record(Keyspace::Aux, key, cost);
    }
}

/// What a staged write is charged by when its batch is applied: the sizes
/// and the writer's cost that came with the call which stands for its key
/// (`cost-model.md` B1).
enum Charge {
    Put {
        record_len: usize,
        children: ChildrenSizesWithIsSumTree,
        cost: Option<KeyValueStorageCost>,
    },
    Delete(Option<KeyValueStorageCost>),
}

/// What the application of a [`LigninBatch`] reports.
#[derive(Debug, Default)]
pub struct LigninApplied {
    /// Each batch that was applied, with where its written nodes are, for
    /// each of its Merks in the order of the batch.
    parts: Vec<(Resolved<Charge>, Vec<Vec<Location>>)>,
    /// How many node values the batch stated that the transaction did not
    /// hold: the values that were written.
    pub values_written: usize,
    /// How many node values the batch stated that the transaction held
    /// already, byte for byte. They were left where they are.
    pub values_kept: usize,
}

impl LigninApplied {
    fn take_in(&mut self, part: Resolved<Charge>, written: Written) {
        self.values_written += written.values_written;
        self.values_kept += written.values_kept;
        self.parts.push((part, written.locations));
    }

    /// Shows `seen` where each written node of `subtree` is now, by the key
    /// it was written under: what a handle that stays open takes in
    /// ([`LigninNodeStore::take_locations`](super::LigninNodeStore::take_locations))
    /// so that it follows no link to such a node by key (`tree-layout.md`
    /// P5). A transaction that serves its records by key has none to show.
    pub(super) fn locations(&self, subtree: &Prefix, mut seen: impl FnMut(&[u8], Location)) {
        let subtree = Subtree::new(*subtree);
        for (part, locations) in &self.parts {
            part.with_batch(|batch| {
                let merks = batch.merks.iter().zip(locations);
                for (merk, locations) in merks.filter(|(merk, _)| merk.subtree == subtree) {
                    for (node, location) in merk.nodes.iter().zip(locations) {
                        seen(node.key, *location);
                    }
                }
            });
        }
    }
}

/// What enters `subtree` in the subtree directory as a tree of
/// `tree_type`: its place among the subtrees of its host is the place of
/// its prefix in [`LigninHost::hosted`](super::LigninHost::hosted).
pub(super) fn entering(subtree: &LigninSubtree, tree_type: &[u8]) -> Result<SubtreeChange, Error> {
    let host = match &subtree.host {
        None => None,
        Some(host) => {
            let position = (host.hosted.iter())
                .position(|hosted| *hosted == subtree.prefix)
                .and_then(|position| u8::try_from(position).ok())
                .ok_or_else(|| refused("a subtree is not among those its host hosts"))?;
            Some((host.subtree, host.key.clone(), position))
        }
    };
    Ok(SubtreeChange::Enter {
        subtree: subtree.prefix,
        tree_type: tree_type.to_vec(),
        host,
    })
}

const fn keyspace(column: Column) -> Keyspace {
    match column {
        Column::Data => Keyspace::Data,
        Column::Aux => Keyspace::Aux,
        Column::Roots => Keyspace::Roots,
        Column::Meta => Keyspace::Meta,
    }
}

pub(super) fn node_put(node: &NodeWrite) -> NodePut<'_> {
    NodePut {
        key: &node.key,
        feature: node.feature.as_slice(),
        value_hash: &node.value_hash,
        kv_hash: &node.kv_hash,
        left: link_of(&node.left),
        right: link_of(&node.right),
        // What an element hosts is said of the element
        // ([`StagedBatch::host`]), with every Merk it hosts.
        hosted: Hosted::Keep,
        value: match &node.value {
            ValueWrite::Stated(stated) => ValuePut::Value(stated),
            ValueWrite::Kept => ValuePut::Keep,
        },
        // A node that keeps its value names the read the value is kept
        // from, and one that states it where the node is.
        at: match &node.value {
            ValueWrite::Kept => node.read.map(Location::handle),
            ValueWrite::Stated(_) => node.hint.map(Location::handle),
        },
        stale: node.stale,
    }
}

/// The writes of one or more operations on LigninDB: the change sets
/// their deferred handles committed and the records they wrote, staged
/// until the batch is applied.
///
/// It is a [`StagedBatch`] of the engine (`lignindb::batch`), which holds
/// one outcome for each key by the rule of a storage batch
/// (`transactions.md` GM0): inside one operation a put stands against a
/// delete of its key, and across the boundary that
/// [`Self::next_operation`] marks the later operation stands. So a Merk
/// that is written by two operations of one batch has, for each of its
/// nodes, the write that a storage batch would hold, and is charged for
/// that write alone.
///
/// The change sets of all its Merks are applied together, as one write of
/// the tree layer: a node is placed then, a host names the root of the Merk
/// it hosts as the whole batch leaves it, and where each written node is
/// comes back from there (`tree-layout.md` P5). A handle says which
/// element hosts its Merk ([`LigninSubtree::host`]), and the batch says it
/// of that element to the engine's batch, which names each hosted Merk in
/// the node of its host where it holds both, and anchors a Merk whose host
/// it does not put.
#[derive(Default)]
pub struct LigninBatch {
    staged: StagedBatch<Charge>,
    marks: Mutex<Marks>,
}

/// What a batch keeps beside the outcomes: the subtrees that a staged
/// write is in, in any column, each as its handle named it.
#[derive(Default)]
struct Marks {
    written: BTreeMap<Prefix, LigninSubtree>,
    /// The subtrees that two handles named different hosts for.
    two_hosts: BTreeSet<Prefix>,
}

impl Marks {
    /// Takes in that a write was staged in the subtree `named`. Of two
    /// handles of one subtree, the one that names its host is kept: a
    /// handle by prefix names none where the directory has none yet.
    fn write(&mut self, named: LigninSubtree) {
        match self.written.get_mut(&named.prefix) {
            Some(known) if known.host.is_some() && named.host.is_none() => {}
            Some(known) => {
                if known.host.is_some() && known.host != named.host {
                    self.two_hosts.insert(named.prefix);
                }
                *known = named;
            }
            None => {
                self.written.insert(named.prefix, named);
            }
        }
    }

    fn take_in(&mut self, other: Self) {
        self.two_hosts.extend(other.two_hosts);
        for named in other.written.into_values() {
            self.write(named);
        }
    }
}

impl fmt::Debug for LigninBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let marks = self.marks();
        f.debug_struct("LigninBatch")
            .field("staged", &self.staged)
            .field("written", &marks.written.len())
            .finish()
    }
}

impl LigninBatch {
    /// A batch with nothing staged.
    pub fn new() -> Self {
        Self::default()
    }

    fn marks(&self) -> MutexGuard<'_, Marks> {
        self.marks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether nothing is staged.
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty()
    }

    /// Marks the boundary between two operations that share the batch:
    /// what is staged so far is the outcome of earlier operations, and a
    /// delete that follows takes the place of a put among them.
    pub fn next_operation(&self) {
        self.staged.next_operation();
    }

    /// Moves everything staged into a new batch and leaves this one empty,
    /// still open for the handles that stage into it.
    pub fn take_pending(&self) -> Self {
        // The marks are taken with what they are of: a call that stages
        // holds them from its first write to its mark.
        let mut marks = self.marks();
        Self {
            staged: self.staged.take_pending(),
            marks: Mutex::new(std::mem::take(&mut *marks)),
        }
    }

    /// Merges `other` as a later operation: each of its outcomes takes the
    /// place of what this batch holds for the key.
    pub fn merge_overwriting(&self, other: Self) {
        let marks = other.marks.into_inner();
        let mut own = self.marks();
        own.take_in(marks.unwrap_or_else(PoisonError::into_inner));
        self.staged.merge_overwriting(other.staged);
    }

    /// Says to the engine's batch which Merks the element that hosts
    /// `subtree` hosts once the batch is applied.
    fn host(&self, subtree: &LigninSubtree) {
        if let Some(host) = &subtree.host {
            let hosted: Vec<Subtree> = host.hosted.iter().copied().map(Subtree::new).collect();
            (self.staged).host(Subtree::new(host.subtree), &host.key, &hosted);
        }
    }

    /// Enters `subtree` in the subtree directory, with no node, as a tree
    /// of `tree_type`: bytes the directory keeps and gives back.
    ///
    /// A subtree is entered before a change set names it, by the batch that
    /// writes the element which hosts it, and every subtree an element
    /// hosts is entered: a Merk with no node, and a tree that keeps its
    /// data in a flat keyspace. Its place among the subtrees of its host
    /// is the place of its prefix in
    /// [`LigninHost::hosted`](super::LigninHost::hosted).
    pub fn enter(&self, subtree: &LigninSubtree, tree_type: &[u8]) -> Result<(), Error> {
        entering(subtree, tree_type)?;
        // What the element hosts and the enter are staged as one: no
        // batch is taken between the two.
        let _held = self.marks();
        self.host(subtree);
        (self.staged).enter(Subtree::new(subtree.prefix), tree_type);
        Ok(())
    }

    /// Drops `subtree` with every subtree below it. The outermost subtree
    /// is named, and none below it.
    ///
    /// The batch that drops a hosted subtree deletes the element that
    /// hosted it, or writes it without the subtree: [`LigninSubtree::host`]
    /// is that element with the subtrees it hosts afterwards, and the batch
    /// names them in its node if it writes one.
    pub fn drop_subtree(&self, subtree: &LigninSubtree) {
        let _held = self.marks();
        self.host(subtree);
        self.staged.drop_subtree(Subtree::new(subtree.prefix));
    }

    pub(super) fn stage(&self, change_set: LigninChangeSet) -> Result<(), Error> {
        // A batch keeps one outcome for each key and not the change set, so
        // nothing would tell the transaction that the Merk is whole.
        if change_set.whole {
            return Err(refused(
                "a change set that is said to be the whole Merk, through a handle that stages its \
                 writes",
            ));
        }
        let writes_nodes = change_set.writes_nodes();
        if !writes_nodes && change_set.records.is_empty() {
            return Ok(());
        }
        let LigninChangeSet {
            subtree: named,
            nodes,
            removed,
            root,
            records,
            ..
        } = change_set;
        let subtree = Subtree::new(named.prefix);
        let mut marks = self.marks();
        if writes_nodes {
            self.host(&named);
        }
        for (node, sizes) in nodes {
            let charge = Charge::Put {
                record_len: sizes.record_len,
                children: sizes.children,
                cost: sizes.cost,
            };
            self.staged.put_node(subtree, &node_put(&node), charge);
        }
        for (key, cost) in removed {
            (self.staged).delete_node(subtree, &key, Charge::Delete(cost));
        }
        // The root of a Merk changes only with a node.
        if let (true, RootWrite::Set(root)) = (writes_nodes, &root) {
            self.staged.set_root(subtree, root.as_deref());
        }
        for RecordOp { change, cost } in records {
            let unit = Unit::new(column(change.keyspace), subtree);
            match &change.value {
                Some(value) => {
                    let charge = Charge::Put {
                        record_len: value.len(),
                        children: None,
                        cost,
                    };
                    self.staged.put(unit, &change.key, value, charge);
                }
                None => (self.staged).delete(unit, &change.key, Charge::Delete(cost)),
            }
        }
        marks.write(named);
        Ok(())
    }

    /// Takes everything staged out of the batch and works out what it is
    /// charged: the batch as it stands, with one outcome for each key
    /// ([`LigninSettled`]). Nothing is applied. The batch is empty
    /// afterwards, and a handle that was opened with it stages its next
    /// writes in it.
    ///
    /// The cost returned is what a storage batch is charged at once
    /// (`cost-model.md`, B1): each write in the order of the keyspaces and
    /// by stored key, a delete with no cost from the record that the
    /// committed state holds, which is read through `committed`. The
    /// storage cost is held back ([`LigninSettled::pending_cost`]) and
    /// owed only once the transaction took the writes.
    pub fn settle<'v>(
        &self,
        committed: impl Into<LigninView<'v>>,
    ) -> CostResult<LigninSettled, Error> {
        let LigninBatch { staged, marks } = self.take_pending();
        let marks = marks.into_inner().unwrap_or_else(PoisonError::into_inner);
        let mut charge = MODEL.batch();
        let settled = (staged.resolve())
            .map_err(|error| fault(TxnError::Batch(error)))
            .and_then(|mut resolved| {
                charge_outcomes(&mut resolved, committed.into().0, &mut charge)?;
                Ok(resolved)
            });
        match settled {
            Ok(resolved) => {
                let (at_once, pending) = charge.into_parts();
                Ok(LigninSettled::new(resolved, marks, pending)).wrap_with_cost(at_once)
            }
            Err(error) => Err(error).wrap_with_cost(charge.finish(false)),
        }
    }

    /// Applies everything staged to the transaction that `view` writes, as
    /// one operation: all of it or nothing. The batch is empty afterwards,
    /// whether or not the transaction took the writes, and a handle that was
    /// opened with it stages its next writes in it.
    ///
    /// The cost is that of a storage batch which is applied
    /// (`cost-model.md`, B1): each write is charged in the order of the
    /// keyspaces and by stored key, a delete with no cost from the
    /// committed record, and the storage cost only if the transaction took
    /// the writes.
    pub fn apply<'v>(&self, view: impl Into<LigninView<'v>>) -> CostResult<LigninApplied, Error> {
        let view = view.into();
        let mut cost = OperationCost::default();
        match self.settle(view).unwrap_add_cost(&mut cost) {
            Ok(settled) => LigninSettled::apply(vec![settled], view, &[]).add_cost(cost),
            Err(error) => Err(error).wrap_with_cost(cost),
        }
    }
}

/// A batch whose cost is worked out and which waits to be applied
/// ([`LigninBatch::settle`]): one outcome for each key it writes.
#[derive(Debug)]
pub struct LigninSettled {
    resolved: Resolved<Charge>,
    written: BTreeMap<Prefix, LigninSubtree>,
    pending: OperationCost,
    /// Whether two handles named different hosts for a subtree the batch
    /// holds a write for.
    two_hosts: bool,
    /// What whoever applies the batch said of an element after the batch
    /// was settled ([`Self::host`]): the element's subtree and key, and
    /// the Merks it hosts.
    hosts: Vec<(Subtree, Vec<u8>, Vec<Subtree>)>,
}

/// A settled batch is handed to the transaction as it was resolved, with
/// what was said of an element since ([`LigninSettled::host`]) in the
/// place of what its handles said.
impl BatchPart for LigninSettled {
    fn with_batch(
        &self,
        run: &mut dyn FnMut(&Batch<'_>) -> Result<Written, Error>,
    ) -> Result<Written, Error> {
        if self.hosts.is_empty() {
            return self.resolved.with_batch(run);
        }
        self.resolved.with_batch(|batch| {
            let hosted = |subtree: Subtree, key: &[u8]| {
                (self.hosts.iter())
                    .find(|(of, at, _)| *of == subtree && at == key)
                    .map(|(.., hosted)| hosted.as_slice())
            };
            let puts = |subtree: Subtree, key: &[u8]| {
                batch.merks.iter().any(|merk| {
                    merk.subtree == subtree && merk.nodes.iter().any(|node| node.key == key)
                })
            };
            let nodes: Vec<Vec<NodePut<'_>>> = (batch.merks.iter())
                .map(|merk| {
                    let puts = merk.nodes.iter().map(|node| NodePut {
                        hosted: match hosted(merk.subtree, node.key) {
                            Some(hosted) => Hosted::Set(hosted),
                            None => node.hosted,
                        },
                        ..*node
                    });
                    puts.collect()
                })
                .collect();
            let merks: Vec<MerkWrite<'_>> = (batch.merks.iter().zip(&nodes))
                .map(|(merk, nodes)| MerkWrite {
                    nodes,
                    // A Merk is anchored where the batch does not put the
                    // node of the element that hosts it.
                    anchored: match (self.hosts.iter())
                        .find(|(.., hosted)| hosted.contains(&merk.subtree))
                    {
                        Some((subtree, key, _)) => !puts(*subtree, key),
                        None => merk.anchored,
                    },
                    ..*merk
                })
                .collect();
            run(&Batch {
                merks: &merks,
                ..*batch
            })
        })
    }
}

/// What a settled batch holds for the node under a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LigninOutcome<'a> {
    /// It puts the node, with this value where the put states one.
    Put(Option<&'a [u8]>),
    /// It removes the node.
    Removed,
}

impl LigninSettled {
    fn new(resolved: Resolved<Charge>, marks: Marks, pending: OperationCost) -> Self {
        // A subtree whose every write a later operation of the batch took
        // back is written into no more.
        let live: BTreeSet<Prefix> = (resolved.outcomes())
            .map(|outcome| *outcome.unit.subtree.as_bytes())
            .collect();
        let mut written = marks.written;
        written.retain(|prefix, _| live.contains(prefix));
        Self {
            resolved,
            written,
            pending,
            two_hosts: marks.two_hosts.iter().any(|prefix| live.contains(prefix)),
            hosts: Vec::new(),
        }
    }

    /// Whether handles that staged into the batch named two different
    /// elements as the host of one subtree the batch holds a write for.
    /// [`Self::written`] has the later of the two; the store does not
    /// choose between them, and whoever applies the batch may refuse it.
    pub fn names_two_hosts(&self) -> bool {
        self.two_hosts
    }

    /// Says which Merks the element `host` hosts once the batch is
    /// applied, by position, in the place of what the handles that staged
    /// into the batch said of it ([`LigninSubtree::host`]): the node of
    /// the element names them if the batch puts it, and each of them that
    /// the batch writes is anchored if it does not.
    ///
    /// A handle knows the one subtree it is of. Whoever applies the batch
    /// knows the element as the batch leaves it, and says here what an
    /// element hosts that hosts more than the subtree of its own path. Of
    /// two statements of one element the later stands.
    pub fn host(&mut self, host: &LigninHost) {
        let subtree = Subtree::new(host.subtree);
        self.hosts
            .retain(|(of, key, _)| !(*of == subtree && *key == host.key));
        let hosted = host.hosted.iter().copied().map(Subtree::new).collect();
        self.hosts.push((subtree, host.key.clone(), hosted));
    }

    /// The storage cost of the batch, which is owed once the transaction
    /// took its writes.
    pub fn pending_cost(&self) -> &OperationCost {
        &self.pending
    }

    /// Whether the batch writes nothing.
    pub fn is_empty(&self) -> bool {
        self.resolved.is_empty()
    }

    /// The subtrees the batch holds a write for, in any column, each as
    /// the handle that staged the write named it: with the element that
    /// hosts it.
    pub fn written(&self) -> impl Iterator<Item = &LigninSubtree> {
        self.written.values()
    }

    /// Whether the batch itself enters the subtree `prefix`.
    pub fn enters(&self, prefix: &[u8; 32]) -> bool {
        self.resolved.enters().contains(&Subtree::new(*prefix))
    }

    /// What the batch holds for the node `key` of the Merk under `prefix`,
    /// or `None` if it holds nothing for it.
    pub fn node(&self, prefix: &[u8; 32], key: &[u8]) -> Option<LigninOutcome<'_>> {
        match self.resolved.held(Subtree::new(*prefix), key)? {
            Staged::Node(node) => Some(LigninOutcome::Put(node.value.as_deref())),
            Staged::NodeRemoved => Some(LigninOutcome::Removed),
            Staged::Record(_) | Staged::Removed => None,
        }
    }

    /// Applies `parts` to the transaction that `view` writes, in their
    /// order: all of them or nothing. `enters` are subtrees that are
    /// entered with them, each with its tree type as the bytes the
    /// directory keeps: those that a part writes into and that neither the
    /// transaction nor a part has entered, a subtree after the one its
    /// host is in. Each is entered with the first part, or with the part
    /// that enters the subtree its host is in.
    ///
    /// The cost is the storage cost of every part, which is owed only if
    /// the transaction took the writes (`cost-model.md`, B1).
    pub fn apply<'v>(
        parts: Vec<Self>,
        view: impl Into<LigninView<'v>>,
        enters: &[(LigninSubtree, Vec<u8>)],
    ) -> CostResult<LigninApplied, Error> {
        let mut pending = OperationCost::default();
        for part in &parts {
            pending += part.pending.clone();
        }
        // A subtree is entered by the part that enters the subtree its
        // host is in, or that the enter of that one goes with, and by the
        // first part otherwise.
        let mut entered_by: Vec<usize> = Vec::with_capacity(enters.len());
        for (subtree, _) in enters {
            let above = subtree.host.as_ref().map(|host| host.subtree);
            let part = above.and_then(|above| {
                let own = parts.iter().position(|part| part.enters(&above));
                let given = (enters.iter().zip(&entered_by))
                    .find_map(|((given, _), part)| (given.prefix == above).then_some(*part));
                own.or(given)
            });
            entered_by.push(part.unwrap_or(0));
        }
        let applied = (enters.iter().zip(entered_by))
            .map(|((subtree, tree_type), part)| Ok((part, entering(subtree, tree_type)?)))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|enters| {
                let batches: Vec<&dyn BatchPart> =
                    parts.iter().map(|part| part as &dyn BatchPart).collect();
                view.into().0.apply_parts(&batches, &enters)
            });
        match applied {
            Ok(written) => {
                let mut applied = LigninApplied::default();
                for (part, written) in parts.into_iter().zip(written) {
                    applied.take_in(part.resolved, written);
                }
                Ok(applied).wrap_with_cost(MODEL.batch_written(pending, true))
            }
            Err(error) => Err(error).wrap_with_cost(MODEL.batch_written(pending, false)),
        }
    }
}

/// Feeds `charge` every outcome of the batch: the keyspaces in the order
/// of the cost rules, and within each the writes by stored key. It takes
/// what each call attached out of the batch, which is charged once.
fn charge_outcomes(
    resolved: &mut Resolved<Charge>,
    access: &dyn Access,
    charge: &mut BatchCharge,
) -> Result<(), Error> {
    let charges: Vec<Charge> = (resolved.tags_mut())
        .map(|charge| std::mem::replace(charge, Charge::Delete(None)))
        .collect();
    if charges.len() != resolved.len() {
        return Err(Error::CorruptedState(
            "a batch does not have one charge for each of its outcomes",
        ));
    }
    for (outcome, attached) in resolved.outcomes().zip(charges) {
        let keyspace = keyspace(outcome.unit.column);
        let prefix = outcome.unit.subtree.as_bytes();
        let stored_key_len = stored_key_len(outcome.key);
        match (outcome.write, attached) {
            (
                Staged::Record(_) | Staged::Node(_),
                Charge::Put {
                    record_len,
                    children,
                    cost,
                },
            ) => charge
                .put(keyspace, stored_key_len, record_len, children, cost)
                .map_err(cost_refused)?,
            (Staged::Removed | Staged::NodeRemoved, Charge::Delete(Some(cost))) => {
                charge.delete(cost);
            }
            (Staged::NodeRemoved, Charge::Delete(None)) => {
                let committed = access
                    .committed_node(prefix, outcome.key)
                    .and_then(|node| node.as_ref().map(record_len).transpose());
                charge.delete_uncosted(stored_key_len, committed)?;
            }
            (Staged::Removed, Charge::Delete(None)) => {
                let committed = access
                    .committed_record(keyspace, prefix, outcome.key)
                    .map(|record| record.map(|record| record.len()));
                charge.delete_uncosted(stored_key_len, committed)?;
            }
            (Staged::Record(_) | Staged::Node(_), Charge::Delete(_))
            | (Staged::Removed | Staged::NodeRemoved, Charge::Put { .. }) => {
                return Err(Error::CorruptedState(
                    "an outcome of a batch does not have the charge of its call",
                ));
            }
        }
    }
    Ok(())
}

fn cost_refused(error: grovedb_costs::error::Error) -> Error {
    Error::StorageError(StorageError::CostError(error))
}
