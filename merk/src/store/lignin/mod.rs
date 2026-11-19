//! [`NodeStore`] on a write transaction of LigninDB.
//!
//! A Merk on a [`LigninNodeStore`] keeps each node as one cell of a tree
//! page: its key, its feature, its two hashes, a link to each child and its
//! value. A commit hands the transaction the nodes Merk wrote as cell
//! operations, so that a value is written only when it changed and a hash
//! update writes hashes.
//!
//! `docs/lignindb/node-store.md` has the contract of the seam and a section
//! on this store.
//!
//! # Handles
//!
//! A handle is of one subtree ([`LigninSubtree`]) inside one transaction,
//! in one of three modes:
//!
//! | Handle | Reads | Writes |
//! | --- | --- | --- |
//! | [`LigninNodeStore::deferred`] | the transaction's view; nothing that is staged | staged in the [`LigninBatch`] it was opened with, or refused with none |
//! | [`LigninNodeStore::immediate`] | the transaction's view, its own writes included | applied to the transaction at once, each commit as a write of its own |
//! | [`LigninNodeStore::read_only`] | a transaction that has no scope open | refused |
//!
//! The first two read and write through a [`LigninWriter`], which is the
//! write scope of one writer (`transactions.md` SC1). A handle and a Merk on
//! it are `Send`.
//!
//! # Reads
//!
//! A node is read by its key, and through a link by where its cell was last
//! seen while that is still good (`tree-layout.md` S6). A cell names its
//! children by address and holds the length of each child's key but not
//! the key (`tree-page.md` TP10), so a read fills each link's key from the
//! child's cell. It keeps where that cell is: the handle follows a link to
//! the child there, and names the node there when it writes it. Where a
//! written node is comes back from the application of its write
//! (`tree-layout.md` P5): at once on an immediate handle, and from
//! [`LigninBatch::apply`] for a handle that stays open
//! ([`LigninNodeStore::take_locations`]).
//!
//! Every read builds a `TreeNode`, value included. PERF-01 replaces that
//! with views.
//!
//! # Writes
//!
//! Every node Merk writes is one node of the change set, under its key,
//! with every part stated but the value (`transactions.md` ST6): a node
//! that is written with `HashOnly` or `StructureChanged` keeps the value
//! the transaction holds. A node written with `ValueWritten` states its
//! value, and when the write is applied the value is compared, byte for
//! byte, with what the transaction holds under that key: equal bytes are
//! kept where they are. So the tree element of every Merk on the way up,
//! which Merk reports as written on every write below it, costs no value.
//!
//! A Merk whose subtree the same batch enters has no node before the
//! write, whatever the transaction holds under its prefix until then: a
//! drop is applied before any change set. Every value stated for it is
//! written, with no comparison. A deferred handle does not see a staged
//! drop, as it sees no staged write, so a Merk that is new under such a
//! prefix is opened with no root.
//!
//! The root of a Merk is stated by its commit
//! ([`NodeChangeSet::state_root`](super::NodeChangeSet::state_root)): the
//! engine keeps the Merk's directory entry by it.
//!
//! # Costs
//!
//! Every charge comes from the functions of
//! [`cost_model`](crate::cost_model), with the sizes Merk's own record has:
//! a read is charged the record length of the node it found, a staged write
//! nothing until its batch is applied, and a batch as a storage batch is.
//! MERK-03 holds the store to the corpus on this.
//!
//! # What is not here
//!
//! - **The cursor** (MERK-05). [`LigninNodeStore::cursor`] gives a cursor
//!   that stands on no record and whose `status` is an error.
//! - **Which subtrees an operation enters and drops.** A batch passes
//!   each on to the subtree directory ([`LigninBatch::enter`],
//!   [`LigninBatch::drop_subtree`]), and names a Merk in the host a batch
//!   writes with it ([`LigninHost`]). Saying them is GroveDB's, which
//!   knows the elements (MERK-04, GROVE-01, GROVE-05). A dropped subtree's
//!   nodes are not charged here.
//! - **The clear of a subtree**, which the transaction offers beside the
//!   drop. What a clear takes is GROVE-05's to state. Until then a Merk
//!   loses its nodes one by one, by key, as on RocksDB.
//! - **Reads through a snapshot** (GROVE-01).

mod access;
mod change_set;
mod feature;
#[cfg(all(test, feature = "rocksdb_storage"))]
mod tests;

use std::{cell::RefCell, collections::HashMap, fmt};

use ed::Encode;
use grovedb_costs::{CostContext, CostResult, CostsExt, OperationCost};
use grovedb_storage::Error as StorageError;
use grovedb_version::version::GroveVersion;
use lignindb::txn::WriteTxn;

use self::access::{Access, Child, Location, StoredLink, StoredNode};
pub use self::{
    access::LigninWriter,
    change_set::{LigninApplied, LigninBatch, LigninChangeSet, PreparedNode},
};
use super::{NodeChange, NodeCursor, NodeStore};
use crate::{
    cost_model::{Keyspace, StorageCostModel},
    merk::defaults::ROOT_KEY_KEY,
    tree::{
        kv::{ValueDefinedCostType, KV},
        TreeNode, TreeNodeInner,
    },
    Error, Link,
};

/// The version of the storage cost rules this store charges by. A handle is
/// opened with no grove version, as a storage context is, so a grove
/// version that charges by another has to reach here too.
const MODEL: StorageCostModel = StorageCostModel::V0;

/// How many times a node is read again because a commit landed between the
/// read of its cell and the read of a child's.
const READ_ATTEMPTS: usize = 8;

/// The element that hosts a Merk: where the Merk hangs in the grove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigninHost {
    /// The subtree of the Merk that holds the element.
    pub subtree: [u8; 32],
    /// The element's key in that Merk.
    pub key: Vec<u8>,
    /// Every Merk the element hosts once the batch is applied, by
    /// position: one for a tree, more for an indexed tree. Every mention
    /// of one element in one batch gives the same list. No position is
    /// left out, so an element that loses a Merk loses those after it too.
    pub hosted: Vec<[u8; 32]>,
}

/// The subtree a handle is of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigninSubtree {
    /// The 32 bytes that name the subtree.
    pub prefix: [u8; 32],
    /// The element that hosts the subtree's Merk, or `None` for the root
    /// Merk of a grove and for a Merk that stands alone.
    ///
    /// A batch that writes a Merk and the node of its host names the Merk
    /// in the host's cell, so that the cell points at the Merk's root
    /// (`tree-layout.md` C3, P2). A write of a Merk whose host it does not
    /// write is anchored (T3).
    pub host: Option<LigninHost>,
}

impl LigninSubtree {
    /// The subtree `prefix` with no host.
    pub const fn alone(prefix: [u8; 32]) -> Self {
        Self { prefix, host: None }
    }
}

enum Mode<'db> {
    ReadOnly,
    Deferred(Option<&'db LigninBatch>),
    Immediate,
}

/// The handle through which a Merk reads and writes the nodes of one
/// subtree in a LigninDB write transaction.
pub struct LigninNodeStore<'db> {
    access: &'db (dyn Access + 'db),
    subtree: LigninSubtree,
    mode: Mode<'db>,
    /// Where the cells of this subtree's nodes were last seen, by key.
    hints: RefCell<HashMap<Vec<u8>, Location>>,
}

impl fmt::Debug for LigninNodeStore<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LigninNodeStore")
            .field("subtree", &self.subtree)
            .finish_non_exhaustive()
    }
}

fn refused(what: &str) -> Error {
    Error::StorageError(StorageError::StorageError(format!("lignindb: {what}")))
}

fn link_len(link: &Option<StoredLink>) -> Result<Option<usize>, Error> {
    let reference = link.as_ref().map(|link| Link::Reference {
        hash: link.hash,
        child_heights: link.heights,
        key: vec![0; usize::from(link.key_len)],
        aggregate_data: link.aggregate,
    });
    reference
        .map(|link| link.encoding_length())
        .transpose()
        .map_err(Error::EdError)
}

/// The record length of a stored node: the bytes Merk's own record of it
/// takes (`cost-model.md` N1). It is measured from the cell alone: a link
/// holds the length of its child's key.
fn record_len(node: &StoredNode) -> Result<usize, Error> {
    let feature_len = node.feature.encoding_length().map_err(Error::EdError)?;
    Ok(MODEL.node_record_len(
        node.value_len as usize,
        feature_len,
        link_len(&node.left)?,
        link_len(&node.right)?,
    ))
}

fn reference(link: Option<StoredLink>) -> Result<Option<Link>, Error> {
    link.map(|link| {
        Ok(Link::Reference {
            hash: link.hash,
            child_heights: link.heights,
            key: link
                .key
                .ok_or(Error::CorruptedState("a link was read with no child key"))?,
            aggregate_data: link.aggregate,
        })
    })
    .transpose()
}

fn node_inner(key: Vec<u8>, node: StoredNode) -> Result<TreeNodeInner, Error> {
    let value = node
        .value
        .ok_or(Error::CorruptedState("a node was read with no value"))?;
    Ok(TreeNodeInner {
        marks: Default::default(),
        left: reference(node.left)?,
        right: reference(node.right)?,
        kv: KV::from_fields(key, value, node.kv_hash, node.value_hash, node.feature),
    })
}

impl<'db> LigninNodeStore<'db> {
    fn new(access: &'db (dyn Access + 'db), subtree: LigninSubtree, mode: Mode<'db>) -> Self {
        Self {
            access,
            subtree,
            mode,
            hints: RefCell::new(HashMap::new()),
        }
    }

    /// A deferred handle of `subtree`: its writes are staged in `batch`
    /// until the batch is applied, and it reads none of them before. With
    /// no batch it refuses every write.
    pub fn deferred(
        writer: &'db LigninWriter<'_, '_>,
        subtree: LigninSubtree,
        batch: Option<&'db LigninBatch>,
    ) -> Self {
        Self::new(writer, subtree, Mode::Deferred(batch))
    }

    /// An immediate handle of `subtree`: it writes to the transaction at
    /// once and reads its own writes.
    pub fn immediate(writer: &'db LigninWriter<'_, '_>, subtree: LigninSubtree) -> Self {
        Self::new(writer, subtree, Mode::Immediate)
    }

    /// A handle of `subtree` that only reads, on a transaction that has no
    /// scope open.
    pub fn read_only(transaction: &'db WriteTxn<'_>, subtree: LigninSubtree) -> Self {
        Self::new(transaction, subtree, Mode::ReadOnly)
    }

    /// The subtree the handle is of.
    pub fn subtree(&self) -> &LigninSubtree {
        &self.subtree
    }

    /// Takes in where the application of a batch put the nodes of this
    /// subtree, so that the handle finds them without a lookup by key.
    pub fn take_locations(&self, applied: &LigninApplied) {
        let mut hints = self.hints.borrow_mut();
        for (key, location) in applied.locations(&self.subtree.prefix) {
            hints.insert(key.clone(), *location);
        }
    }

    fn read_cost(&self, found: Option<usize>) -> OperationCost {
        match self.mode {
            Mode::Immediate => MODEL.read_immediate(),
            Mode::Deferred(_) | Mode::ReadOnly => MODEL.read(found),
        }
    }

    fn hint(&self, key: &[u8]) -> Option<Location> {
        self.hints.borrow().get(key).copied()
    }

    /// The node under `key` with the key of each child in its link.
    ///
    /// The node and its children are read in operations of their own, and
    /// a commit can land between two of them. The transaction then says
    /// that the node's location is no longer good, and the node is read
    /// again.
    fn load(&self, key: &[u8]) -> Result<Option<StoredNode>, Error> {
        let prefix = &self.subtree.prefix;
        let mut hint = self.hint(key);
        'node: for _ in 0..READ_ATTEMPTS {
            let Some(mut node) = self.access.node(prefix, key, hint, true)? else {
                return Ok(None);
            };
            hint = None;
            let mut found = Vec::new();
            for (left, link) in [(true, &mut node.left), (false, &mut node.right)] {
                let Some(link) = link.as_mut().filter(|link| link.key.is_none()) else {
                    continue;
                };
                let parent = node
                    .location
                    .ok_or(Error::CorruptedState("a cell was read with no location"))?;
                match self.access.child(prefix, parent, left)? {
                    Child::Stale => continue 'node,
                    Child::Missing => return Err(refused("a link leads to no node")),
                    Child::Found { key, location } => {
                        found.extend(location.map(|location| (key.clone(), location)));
                        link.key = Some(key);
                    }
                }
            }
            let mut hints = self.hints.borrow_mut();
            hints.extend(found);
            match node.location {
                Some(location) => hints.insert(key.to_vec(), location),
                None => hints.remove(key),
            };
            return Ok(Some(node));
        }
        Err(refused("a node changed under every read of it"))
    }

    /// Charges a read: the record length of what it found, and a read that
    /// failed as one that found nothing.
    fn charged<T>(&self, read: Result<(T, Option<usize>), Error>) -> CostResult<T, Error> {
        match read {
            Ok((found, len)) => Ok(found).wrap_with_cost(self.read_cost(len)),
            Err(error) => Err(error).wrap_with_cost(self.read_cost(None)),
        }
    }

    /// Loads the node under `key` and charges the read.
    fn read(&self, key: &[u8]) -> CostResult<Option<StoredNode>, Error> {
        self.charged(self.load(key).and_then(|node| {
            let len = node.as_ref().map(record_len).transpose()?;
            Ok((node, len))
        }))
    }

    fn read_record(&self, keyspace: Keyspace, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        let record = self.access.record(keyspace, &self.subtree.prefix, key);
        self.charged(record.map(|record| {
            let len = record.as_ref().map(Vec::len);
            (record, len)
        }))
    }

    /// Writes one record that is no node, outside a change set.
    fn write_record(
        &self,
        keyspace: Keyspace,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> CostResult<(), Error> {
        let change_set = LigninChangeSet::of_record(self.subtree.clone(), keyspace, key, value);
        self.submit(change_set, "a write", false)
    }

    /// Hands a change set on: staged on a deferred handle, and applied at
    /// once on an immediate one, where a commit is charged what its change
    /// set was charged as it was filled (`cost-model.md` M2, owed whether
    /// or not the transaction takes the writes) and any other write as one
    /// made directly (M1).
    fn submit(
        &self,
        mut change_set: LigninChangeSet,
        what: &str,
        is_commit: bool,
    ) -> CostResult<(), Error> {
        change_set.hint(&self.hints.borrow());
        match self.mode {
            Mode::ReadOnly | Mode::Deferred(None) => {
                Err(refused(&format!("{what} through a handle that only reads")))
                    .wrap_with_cost(MODEL.refused_write())
            }
            Mode::Deferred(Some(batch)) => {
                batch.stage(change_set).wrap_with_cost(MODEL.staged_write())
            }
            Mode::Immediate => {
                let (cost, write) = change_set.into_write();
                self.apply(&write).wrap_with_cost(match is_commit {
                    true => cost,
                    false => MODEL.write_immediate(),
                })
            }
        }
    }

    /// Applies a write of this subtree to the transaction and takes in
    /// where its nodes are now.
    fn apply(&self, write: &access::Write) -> Result<(), Error> {
        let written = self.access.apply(write)?;
        let mut hints = self.hints.borrow_mut();
        for (merk, locations) in write.merks.iter().zip(&written.locations) {
            for key in &merk.removed {
                hints.remove(key);
            }
            for (node, location) in merk.nodes.iter().zip(locations) {
                hints.insert(node.key.clone(), *location);
            }
        }
        Ok(())
    }
}

/// A Merk on the store moves between threads with it.
const _: fn() = || {
    fn moved<T: Send>() {}
    moved::<LigninNodeStore<'static>>();
    moved::<crate::Merk<LigninNodeStore<'static>>>();
    moved::<LigninChangeSet>();
    moved::<LigninBatch>();
    moved::<LigninWriter<'static, 'static>>();
};

impl<'db> NodeStore<'db> for LigninNodeStore<'db> {
    type ChangeSet = LigninChangeSet;
    type Cursor = LigninCursor;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error> {
        self.read(key)
            .map_ok(|node| {
                node.map(|node| {
                    let mut inner = node_inner(key.to_vec(), node)?;
                    if let Some(value_defined_cost_fn) = &value_defined_cost_fn {
                        inner.kv.value_defined_cost =
                            value_defined_cost_fn(inner.kv.value_as_slice(), grove_version);
                    }
                    Ok(TreeNode::new_with_tree_inner(inner))
                })
                .transpose()
            })
            .flatten()
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error> {
        self.read(key)
            .map_ok(|node| node.map(|node| node_inner(Vec::new(), node)).transpose())
            .flatten()
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error> {
        let prefix = &self.subtree.prefix;
        let node = self.access.node(prefix, key, self.hint(key), false);
        let len = node.and_then(|node| node.as_ref().map(record_len).transpose());
        self.charged(len.map(|len| (len.is_some(), len)))
    }

    fn write_node(&self, node: &TreeNode, change: NodeChange) -> CostResult<(), Error> {
        let change_set = LigninChangeSet::of_node(self.subtree.clone(), node, change);
        self.submit(change_set, "a write", false)
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error> {
        self.read_record(Keyspace::Roots, ROOT_KEY_KEY)
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error> {
        self.write_record(Keyspace::Roots, ROOT_KEY_KEY, root_key)
    }

    fn cursor(&self) -> LigninCursor {
        LigninCursor
    }

    fn change_set(&self) -> LigninChangeSet {
        LigninChangeSet::new(self.subtree.clone(), matches!(self.mode, Mode::Immediate))
    }

    fn commit(&self, change_set: LigninChangeSet) -> CostResult<(), Error> {
        self.submit(change_set, "a commit", true)
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.read_record(Keyspace::Aux, key)
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.read_record(Keyspace::Meta, key)
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), Error> {
        self.write_record(Keyspace::Meta, key, Some(value))
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), Error> {
        self.write_record(Keyspace::Meta, key, None)
    }
}

/// The cursor of a [`LigninNodeStore`], which MERK-05 builds. Until then
/// it stands on no record, and [`NodeCursor::status`] says that it stopped
/// on an error and not at the end of the records.
#[derive(Debug)]
pub struct LigninCursor;

impl LigninCursor {
    fn nowhere<T: Default>() -> CostContext<T> {
        T::default().wrap_with_cost(OperationCost::default())
    }
}

impl NodeCursor for LigninCursor {
    fn seek_to_first(&mut self) -> CostContext<()> {
        Self::nowhere()
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        Self::nowhere()
    }

    fn seek<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        Self::nowhere()
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        Self::nowhere()
    }

    fn next(&mut self) -> CostContext<()> {
        Self::nowhere()
    }

    fn prev(&mut self) -> CostContext<()> {
        Self::nowhere()
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        Self::nowhere()
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        Self::nowhere()
    }

    fn valid(&self) -> CostContext<bool> {
        Self::nowhere()
    }

    fn status(&self) -> Result<(), StorageError> {
        Err(StorageError::StorageError(
            "lignindb: the cursor over the nodes of a Merk is not built yet".to_owned(),
        ))
    }
}
