//! The change set of a Merk on LigninDB, and the batch of one operation.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::{Mutex, MutexGuard, PoisonError},
};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostResult,
    CostsExt, OperationCost,
};
use grovedb_storage::Error as StorageError;

use super::{
    access::{
        Access, LinkWrite, Location, MerkChange, NodeWrite, Prefix, RecordChange, RootWrite,
        SubtreeChange, ValueWrite, Write, Written,
    },
    feature::{encode_aggregate, encode_feature},
    record_len, refused, LigninHost, LigninSubtree, LigninWriter, MODEL,
};
use crate::{
    cost_model::{BatchCharge, ImmediateBatchCharge, Keyspace},
    merk::defaults::ROOT_KEY_KEY,
    store::{NodeChange, NodeChangeSet, NodeWriteCost},
    tree::TreeNode,
    Error, Link,
};

/// What the store keeps of a node between `prepare_node` and `put_node`:
/// every part of it but the key, and the value only where it was written.
#[derive(Debug)]
pub struct PreparedNode {
    node: NodeWrite,
    record_len: usize,
}

fn link_write(link: &Link) -> LinkWrite {
    let heights = match link {
        Link::Reference { child_heights, .. }
        | Link::Modified { child_heights, .. }
        | Link::Uncommitted { child_heights, .. }
        | Link::Loaded { child_heights, .. } => *child_heights,
    };
    LinkWrite {
        key: link.key().to_vec(),
        hash: *link.hash(),
        heights,
        aggregate: encode_aggregate(&link.aggregate_data()),
    }
}

fn prepare(node: &TreeNode, change: NodeChange) -> PreparedNode {
    PreparedNode {
        node: NodeWrite {
            key: node.key().to_vec(),
            feature: encode_feature(&node.feature_type()),
            value_hash: *node.value_hash(),
            kv_hash: *node.kv_hash(),
            left: node.link(true).map(link_write),
            right: node.link(false).map(link_write),
            value: match change {
                NodeChange::ValueWritten => ValueWrite::Stated(node.value_as_slice().to_vec()),
                NodeChange::StructureChanged | NodeChange::HashOnly => ValueWrite::Kept,
            },
            hint: None,
            hosted: None,
        },
        record_len: node.encoding_length(),
    }
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
            records: Vec::new(),
            immediate: immediate.then(|| MODEL.immediate_batch()),
        }
    }

    /// A change set of one node, written outside a commit of its Merk. The
    /// root of the Merk is the node it was.
    pub(super) fn of_node(subtree: LigninSubtree, node: &TreeNode, change: NodeChange) -> Self {
        let mut change_set = Self::new(subtree, false);
        let PreparedNode { node, record_len } = prepare(node, change);
        change_set.nodes.push((
            node,
            PutSizes {
                record_len,
                children: None,
                cost: None,
            },
        ));
        change_set
    }

    /// A change set of one record that is no node, written or removed with
    /// no cost of the writer's.
    pub(super) fn of_record(
        subtree: LigninSubtree,
        keyspace: Keyspace,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> Self {
        let mut change_set = Self::new(subtree, false);
        change_set.record(keyspace, key, value, None);
        change_set
    }

    /// Takes the hint of each written node from `hints`.
    pub(super) fn hint(&mut self, hints: &HashMap<Vec<u8>, Location>) {
        for (node, _) in &mut self.nodes {
            node.hint = hints.get(&node.key).copied();
        }
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
        }
    }
}

impl NodeChangeSet for LigninChangeSet {
    type Prepared = PreparedNode;

    fn prepare_node(&mut self, node: &TreeNode, change: NodeChange) -> PreparedNode {
        prepare(node, change)
    }

    fn put_node(
        &mut self,
        key: &[u8],
        prepared: PreparedNode,
        cost: Option<NodeWriteCost>,
    ) -> Result<(), Error> {
        let PreparedNode {
            mut node,
            record_len,
        } = prepared;
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

/// What the application of a [`LigninBatch`] reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LigninApplied {
    /// Where each written node is now, by the subtree and the key it was
    /// written under: what a handle that stays open takes in
    /// ([`LigninNodeStore::take_locations`](super::LigninNodeStore::take_locations))
    /// so that it follows no link to such a node by key (`tree-layout.md`
    /// P5).
    locations: HashMap<Prefix, Vec<(Vec<u8>, Location)>>,
    /// How many node values the batch stated that the transaction did not
    /// hold: the values that were written.
    pub values_written: usize,
    /// How many node values the batch stated that the transaction held
    /// already, byte for byte. They were left where they are.
    pub values_kept: usize,
}

impl LigninApplied {
    fn new(write: &Write, written: Written) -> Self {
        let locations = (write.merks.iter().zip(written.locations))
            .map(|(merk, locations)| {
                let keys = merk.nodes.iter().map(|node| node.key.clone());
                (merk.subtree, keys.zip(locations).collect())
            })
            .collect();
        Self {
            locations,
            values_written: written.values_written,
            values_kept: written.values_kept,
        }
    }

    pub(super) fn locations(&self, subtree: &Prefix) -> &[(Vec<u8>, Location)] {
        self.locations.get(subtree).map_or(&[], Vec::as_slice)
    }
}

#[derive(Default)]
struct Staged {
    /// What the operation does to whole subtrees.
    subtrees: Vec<SubtreeChange>,
    /// The element that hosted each dropped subtree, as it is after the
    /// drop.
    dropped_hosts: Vec<LigninHost>,
    merks: Vec<LigninChangeSet>,
    /// The writes of records that are no nodes, by keyspace and stored key:
    /// one outcome for a key.
    records: BTreeMap<(u8, Prefix, Vec<u8>), RecordOp>,
}

/// The writes of one operation on LigninDB: the change sets its deferred
/// handles committed and the records they wrote, staged until the batch is
/// applied.
///
/// The change sets of all its Merks are applied together, as one write of
/// the tree layer: a node is placed then, a host names the root of the Merk
/// it hosts as the whole batch leaves it, and where each written node is
/// comes back from there (`tree-layout.md` P5).
///
/// A batch holds one change set for a Merk. Which of two change sets of one
/// Merk stands is the rule of the batch across subtrees (TXN-08), and until
/// that exists a second one is refused. For a record that is no node the
/// batch holds one outcome, by the rule of a storage batch inside one
/// operation: a put stands against a delete of the same key, and otherwise
/// the later write stands.
#[derive(Default)]
pub struct LigninBatch {
    staged: Mutex<Staged>,
}

impl fmt::Debug for LigninBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let staged = self.staged();
        f.debug_struct("LigninBatch")
            .field("subtrees", &staged.subtrees)
            .field("dropped_hosts", &staged.dropped_hosts)
            .field("merks", &staged.merks)
            .field("records", &staged.records.len())
            .finish()
    }
}

/// What enters `subtree` in the subtree directory as a tree of
/// `tree_type`: its place among the subtrees of its host is the place of
/// its prefix in [`LigninHost::hosted`].
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

fn keyspace_order(keyspace: Keyspace) -> u8 {
    let order = MODEL.batch_order();
    order
        .iter()
        .position(|at| *at == keyspace)
        .expect("the batch order names every keyspace") as u8
}

impl LigninBatch {
    /// A batch with nothing staged.
    pub fn new() -> Self {
        Self::default()
    }

    fn staged(&self) -> MutexGuard<'_, Staged> {
        self.staged.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether nothing is staged.
    pub fn is_empty(&self) -> bool {
        let staged = self.staged();
        staged.subtrees.is_empty() && staged.merks.is_empty() && staged.records.is_empty()
    }

    /// Enters `subtree` in the subtree directory, with no node, as a tree
    /// of `tree_type`: bytes the directory keeps and gives back.
    ///
    /// A subtree is entered before a change set names it, by the batch that
    /// writes the element which hosts it, and every subtree an element
    /// hosts is entered: a Merk with no node, and a tree that keeps its
    /// data in a flat keyspace. Its place among the subtrees of its host
    /// is the place of its prefix in [`LigninHost::hosted`].
    pub fn enter(&self, subtree: &LigninSubtree, tree_type: &[u8]) -> Result<(), Error> {
        let enter = entering(subtree, tree_type)?;
        self.staged().subtrees.push(enter);
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
        let mut staged = self.staged();
        staged.subtrees.push(SubtreeChange::Drop(subtree.prefix));
        staged.dropped_hosts.extend(subtree.host.clone());
    }

    pub(super) fn stage(&self, mut change_set: LigninChangeSet) -> Result<(), Error> {
        let mut staged = self.staged();
        let prefix = change_set.subtree.prefix;
        if change_set.writes_nodes() && staged.merks.iter().any(|at| at.subtree.prefix == prefix) {
            return Err(refused("the batch holds a change set of this Merk already"));
        }
        for record in std::mem::take(&mut change_set.records) {
            let change = &record.change;
            let at = (
                keyspace_order(change.keyspace),
                change.subtree,
                change.key.clone(),
            );
            // As in a storage batch, a put stands against a delete of the
            // same key, in whichever order they came, and of two puts or
            // two deletes the later stands.
            let put_stands = change.value.is_none()
                && (staged.records.get(&at)).is_some_and(|staged| staged.change.value.is_some());
            if !put_stands {
                staged.records.insert(at, record);
            }
        }
        if change_set.writes_nodes() {
            staged.merks.push(change_set);
        }
        Ok(())
    }

    /// Applies everything staged to the transaction `writer` writes, as one
    /// operation: all of it or nothing. The batch is empty afterwards,
    /// whether or not the transaction took the writes, and a handle that was
    /// opened with it stages its next writes in it.
    ///
    /// The cost is that of a storage batch which is applied
    /// (`cost-model.md`, B1): each write is charged in the order of the
    /// keyspaces and by stored key, a delete with no cost from the
    /// committed record, and the storage cost only if the transaction took
    /// the writes.
    pub fn apply(&self, writer: &LigninWriter<'_, '_>) -> CostResult<LigninApplied, Error> {
        let mut staged = std::mem::take(&mut *self.staged());
        let mut charge = MODEL.batch();
        if let Err(error) = charge_staged(&mut staged, writer, &mut charge) {
            return Err(error).wrap_with_cost(charge.finish(false));
        }
        let write = into_write(staged);
        match writer.apply(&write) {
            Ok(written) => {
                Ok(LigninApplied::new(&write, written)).wrap_with_cost(charge.finish(true))
            }
            Err(error) => Err(error).wrap_with_cost(charge.finish(false)),
        }
    }
}

/// Feeds `charge` every staged write: the keyspaces in the order of the
/// cost rules, and within each the writes by stored key.
fn charge_staged(
    staged: &mut Staged,
    access: &dyn Access,
    charge: &mut BatchCharge,
) -> Result<(), Error> {
    enum Node<'a> {
        Put(&'a mut PutSizes),
        Delete(&'a Option<KeyValueStorageCost>),
    }
    let mut nodes: Vec<(&Prefix, &[u8], Node<'_>)> = Vec::new();
    for merk in &mut staged.merks {
        let prefix = &merk.subtree.prefix;
        nodes.extend(
            (merk.nodes.iter_mut())
                .map(|(node, sizes)| (prefix, node.key.as_slice(), Node::Put(sizes))),
        );
        nodes.extend(
            (merk.removed.iter()).map(|(key, cost)| (prefix, key.as_slice(), Node::Delete(cost))),
        );
    }
    nodes.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let data = keyspace_order(Keyspace::Data);
    let records = |before_data: bool| {
        let all = staged.records.iter();
        all.filter(move |((order, ..), _)| (*order < data) == before_data)
            .map(|(_, record)| record)
    };
    for record in records(true) {
        charge_record(charge, access, record)?;
    }
    for (prefix, key, node) in nodes {
        match node {
            Node::Put(sizes) => charge
                .put(
                    Keyspace::Data,
                    stored_key_len(key),
                    sizes.record_len,
                    sizes.children.take(),
                    sizes.cost.take(),
                )
                .map_err(cost_refused)?,
            Node::Delete(Some(cost)) => charge.delete(cost.clone()),
            Node::Delete(None) => {
                let committed = access
                    .committed_node(prefix, key)
                    .and_then(|node| node.as_ref().map(record_len).transpose());
                charge.delete_uncosted(stored_key_len(key), committed)?;
            }
        }
    }
    for record in records(false) {
        charge_record(charge, access, record)?;
    }
    Ok(())
}

fn cost_refused(error: grovedb_costs::error::Error) -> Error {
    Error::StorageError(StorageError::CostError(error))
}

fn charge_record(
    charge: &mut BatchCharge,
    access: &dyn Access,
    record: &RecordOp,
) -> Result<(), Error> {
    let change = &record.change;
    let stored_key_len = stored_key_len(&change.key);
    match (&change.value, &record.cost) {
        (Some(value), cost) => charge
            .put(
                change.keyspace,
                stored_key_len,
                value.len(),
                None,
                cost.clone(),
            )
            .map_err(cost_refused),
        (None, Some(cost)) => {
            charge.delete(cost.clone());
            Ok(())
        }
        (None, None) => {
            let committed = access
                .committed_record(change.keyspace, &change.subtree, &change.key)
                .map(|record| record.map(|record| record.len()));
            charge.delete_uncosted(stored_key_len, committed)
        }
    }
}

/// The staged writes as one write: each host names the Merks it hosts
/// where the batch holds both, and a Merk whose host the batch does not
/// write is anchored. The element that hosted a dropped subtree names the
/// subtrees it hosts now.
fn into_write(staged: Staged) -> Write {
    let (hosts, mut merks): (Vec<Option<LigninHost>>, Vec<MerkChange>) = (staged.merks.into_iter())
        .map(|mut merk| (merk.subtree.host.take(), merk.into_merk(false)))
        .unzip();
    for (merk, host) in hosts.into_iter().enumerate() {
        if host.is_some_and(|host| !name_hosted(&mut merks, host)) {
            merks[merk].anchored = true;
        }
    }
    for host in staged.dropped_hosts {
        name_hosted(&mut merks, host);
    }
    let records = (staged.records.into_values())
        .map(|record| record.change)
        .collect();
    Write {
        subtrees: staged.subtrees,
        merks,
        records,
    }
}

/// Names the subtrees `host` hosts in its node, where `merks` write that
/// node, and returns whether they do.
fn name_hosted(merks: &mut [MerkChange], host: LigninHost) -> bool {
    let node = (merks.iter_mut())
        .find(|merk| merk.subtree == host.subtree)
        .and_then(|merk| merk.nodes.iter_mut().find(|node| node.key == host.key));
    match node {
        Some(node) => {
            node.hosted = Some(host.hosted);
            true
        }
        None => false,
    }
}
