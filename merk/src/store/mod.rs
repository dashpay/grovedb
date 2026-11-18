//! The seam between Merk and the engine that stores its nodes.
//!
//! A [`NodeStore`] is the handle of one subtree. Merk reaches storage only
//! through it: it fetches nodes, reads the root key, walks the nodes in key
//! order, and commits a [`NodeChangeSet`]. Which engine is underneath is known
//! only below this module: [`kv`] implements the seam over the storage contexts
//! of `grovedb-storage`, one whole node record per key.
//!
//! The contract is written out in `docs/lignindb/node-store.md`.

pub mod kv;
#[cfg(test)]
pub(crate) mod mem;
#[cfg(test)]
pub(crate) mod recording;
#[cfg(test)]
mod tests;

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildKeyLength, ChildSumLength, CostResult,
    CostsExt, FeatureSumLength, TreeCostType,
};
use grovedb_version::version::GroveVersion;

use crate::{
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Error, Link,
};

/// The ordered cursor of a node store: seek, step in either direction, and
/// read the key and the value under the cursor.
///
/// It is the cursor trait of `grovedb-storage`, which names no engine. The
/// query code of `grovedb-query` positions cursors through the same trait.
pub use grovedb_storage::RawIterator as NodeCursor;

/// The record a store's cursor shows under `key`, for tests that measure or
/// decode stored nodes.
#[cfg(test)]
pub(crate) fn stored_record<'db, S: NodeStore<'db>>(store: &S, key: &[u8]) -> Option<Vec<u8>> {
    let mut cursor = store.cursor();
    cursor.seek(key).unwrap();
    if cursor.key().unwrap() != Some(key) {
        return None;
    }
    cursor.value().unwrap().map(<[u8]>::to_vec)
}

/// How a node that Merk writes differs from the node the store holds under
/// the same key.
///
/// A node has three parts:
///
/// - the value part: its key, its value bytes and its feature type;
/// - the structure part: which child, named by key, hangs on each side;
/// - the hash part: its value hash, its key-value hash, and for each child
///   the child's hash, heights and aggregate data.
///
/// A kind names the parts that may differ. The store may keep what it holds
/// of every other part. A kind is never weaker than the truth: Merk reports
/// [`NodeChange::ValueWritten`] whenever it cannot prove a narrower one.
///
/// A kind says how a node may be stored. It never makes the write optional:
/// a node written with any kind is a write of the record under its key.
///
/// The fourth kind of change, a removed node, has no node to describe and is
/// [`NodeChangeSet::delete_node`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeChange {
    /// The node is new to the store, or its value was written. Every part
    /// may differ.
    ValueWritten,
    /// The value part is what the store holds. The structure part and the
    /// hash part may differ.
    StructureChanged,
    /// The value part and the structure part are what the store holds. Only
    /// the hash part may differ.
    HashOnly,
}

/// The sizes Merk's cost rules worked out for one node write. They are
/// logical sizes, the same for every engine.
pub struct NodeWriteCost {
    /// The cost type and length of the node's feature data, when its tree
    /// type carries any.
    pub feature: Option<(TreeCostType, FeatureSumLength)>,
    /// Key length and aggregate length of the left child link.
    pub left_child: Option<(ChildKeyLength, ChildSumLength)>,
    /// Key length and aggregate length of the right child link.
    pub right_child: Option<(ChildKeyLength, ChildSumLength)>,
    /// The storage cost of the node's key and value.
    pub storage_cost: KeyValueStorageCost,
}

/// The changes of one commit to one subtree. A store takes them together in
/// [`NodeStore::commit`], or not at all.
///
/// A change set names every node Merk writes and every node it removes, each
/// under its key: no node is left out because only its hash changed, and
/// none is added. Merk writes each key at most once in a change set.
/// `Merk::commit` issues the root key first, then the nodes in ascending key
/// order, then the aux records.
pub trait NodeChangeSet {
    /// What the store keeps of a node between [`Self::prepare_node`] and
    /// [`Self::put_node`].
    type Prepared;

    /// Takes from `node` what the store needs to write it. Merk calls this
    /// while it holds the node in memory, children before parents, and
    /// [`Self::put_node`] later.
    fn prepare_node(&mut self, node: &TreeNode, change: NodeChange) -> Self::Prepared;

    /// Writes a prepared node under `key`. `cost` is absent for a write Merk
    /// does not charge, as in chunk restore.
    fn put_node(
        &mut self,
        key: &[u8],
        node: Self::Prepared,
        cost: Option<NodeWriteCost>,
    ) -> Result<(), Error>;

    /// Removes the node under `key`. Without `cost` the store works out what
    /// the removal frees.
    fn delete_node(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>);

    /// Records `root_key` as the key of the subtree's root node.
    fn put_root_key(
        &mut self,
        root_key: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error>;

    /// Removes the record of the root key: the subtree is empty.
    fn delete_root_key(&mut self, cost: Option<KeyValueStorageCost>);

    /// Writes an aux record.
    fn put_aux_record(
        &mut self,
        key: &[u8],
        value: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error>;

    /// Removes an aux record.
    fn delete_aux_record(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>);
}

/// The handle through which a Merk reads and writes the nodes of one subtree.
///
/// `'db` is the lifetime of the engine state the handle borrows. A cursor or
/// a change set may borrow that state too, but never the handle itself.
///
/// A read returns what the engine shows the handle under that key at the
/// moment of the call. A store answers no read from a node it kept from an
/// earlier one.
///
/// Every operation returns the cost the store charges for it.
pub trait NodeStore<'db> {
    /// An ordered cursor over the subtree's nodes. Its keys are node keys
    /// and its values are node records in Merk's encoding.
    type Cursor: NodeCursor;

    /// The change set this store commits.
    type ChangeSet: NodeChangeSet;

    /// The node stored under `key`, ready to be modified.
    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error>;

    /// The fields of the node stored under `key`, for a caller that only
    /// reads them: without the key, a value-defined cost, or the copy of the
    /// value a node keeps as its mutation baseline.
    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error>;

    /// The node a parent's `link` points to: the node [`Self::get_node`]
    /// returns for the link's key at that moment, or an error when there is
    /// none. A store that keeps a location in its links may follow it to that
    /// node, never to another answer. The default looks the link's key up.
    fn fetch_node(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        self.get_node(link.key(), value_defined_cost_fn, grove_version)
            .map_ok(|node| node.ok_or(Error::KeyNotFoundError("Key not found for fetch")))
            .flatten()
    }

    /// Writes one node at once, outside a change set, uncharged. The default
    /// commits a change set of that one node.
    fn write_node(&self, node: &TreeNode, change: NodeChange) -> CostResult<(), Error> {
        let mut change_set = self.change_set();
        let prepared = change_set.prepare_node(node, change);
        match change_set.put_node(node.key(), prepared, None) {
            Ok(()) => self.commit(change_set),
            Err(e) => Err(e).wrap_with_cost(Default::default()),
        }
    }

    /// The recorded key of the root node, for a Merk that keeps its own. A
    /// layered Merk takes its root key from its parent element instead.
    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error>;

    /// Records the key of the root node at once, outside a change set, or
    /// removes the record when `root_key` is `None`.
    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error>;

    /// A cursor over the subtree's nodes, not yet positioned.
    fn cursor(&self) -> Self::Cursor;

    /// An empty change set.
    fn change_set(&self) -> Self::ChangeSet;

    /// Applies `change_set`.
    fn commit(&self, change_set: Self::ChangeSet) -> CostResult<(), Error>;

    /// The aux record under `key`.
    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error>;

    /// The meta record under `key`.
    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error>;

    /// Writes a meta record.
    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), Error>;

    /// Removes a meta record.
    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), Error>;
}
