//! The seam between Merk and the engine that stores its nodes.
//!
//! A [`NodeStore`] is the handle of one subtree. Merk reaches storage only
//! through it: it fetches nodes, reads the root key, walks the nodes in key
//! order, and commits a [`NodeChangeSet`]. Which engine is underneath is known
//! only below this module: [`kv`] implements the seam over the storage contexts
//! of `grovedb-storage`, one whole node record per key, and `lignin` (with
//! the feature `lignin_storage`) over a write transaction of LigninDB, one
//! cell per node.
//!
//! The contract is written out in `docs/lignindb/node-store.md`.

pub mod kv;
#[cfg(feature = "lignin_storage")]
pub mod lignin;
#[cfg(test)]
pub(crate) mod mem;
#[cfg(all(test, feature = "rocksdb_storage"))]
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
#[cfg(all(test, merk_test_engine))]
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

    /// States the key of the Merk's root node as this change set leaves it,
    /// or `None` for a Merk it leaves empty. `Merk::commit` states it in
    /// every change set, whether or not the root is another node than
    /// before, and `Merk::clear` states that it leaves no node.
    ///
    /// It is not a write and is charged nothing. A store that finds every
    /// node by its key has no use for it, which is the default. A store
    /// that keeps the links of the tree has to know where a walk begins.
    /// The record of the root key that a Merk with no parent element keeps
    /// is [`Self::put_root_key`].
    fn state_root(&mut self, _root_key: Option<&[u8]>) {}

    /// States that this change set is the whole Merk: the Merk has no node
    /// before it, every node of the Merk is put in it once, a child before
    /// its parent, each with its value written, and [`Self::state_root`]
    /// names the root. A restore states it of the change set that hands a
    /// staged Merk to its store ([`NodeStore::takes_links_to_absent_nodes`]),
    /// and no other code of Merk outside its tests does.
    ///
    /// It is not a write and is charged nothing. A store may lay such a
    /// change set out in one pass, and need read no node of the Merk to
    /// apply it. The default does nothing: a store that keeps one record
    /// per node puts each as it puts any other.
    fn state_whole_merk(&mut self) {}

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

    /// Whether the store holds a node under `key`, without reading the node
    /// itself. It is charged as the lookup of that node. A store that keeps
    /// one record per node answers from the record's presence alone, so a
    /// record that does not decode counts as held, where
    /// [`Self::get_node_for_read`] reports an error.
    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error>;

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

    /// The node a parent's `link` points to, for a walk that only reads the
    /// tree and asks for the value of a node where it needs it, as the walk
    /// of a proof does.
    ///
    /// A store may leave the value out, and says so in the node
    /// ([`TreeNode::value_absent`]); [`Self::load_node_value`] reads it. In
    /// everything else the node is the one [`Self::fetch_node`] returns at
    /// that moment, with no value-defined cost, and the read is charged as
    /// that fetch is: the whole record, whether or not the value comes
    /// with it. A node without its value has no mutation baseline. It must
    /// never reach a commit or be kept in the tree of a Merk.
    ///
    /// The default is [`Self::fetch_node`]: a store that keeps one record
    /// per node has read the value with it.
    fn fetch_node_without_value(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        self.fetch_node(
            link,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
    }

    /// Reads the value of `node`, which [`Self::fetch_node_without_value`]
    /// returned without it, and puts it in the node. A node that has its
    /// value is left as it is.
    ///
    /// It is charged nothing: the fetch of the node was charged its whole
    /// record. The value that is read is one the node's hashes are the
    /// hashes of, or the read fails with `Error::StorageError`: a store
    /// does not put a value under a value hash and a key-value hash that
    /// are not its own. What a value holds outside its hash, as an item
    /// holds the list of its referrers, is what the store holds when the
    /// value is read.
    ///
    /// The default is for a store that returns every node whole.
    fn load_node_value(&self, node: &mut TreeNode) -> CostResult<(), Error> {
        match node.value_absent() {
            false => Ok(()),
            true => Err(Error::CorruptedCodeExecution(
                "a store of whole nodes was handed a node without its value",
            )),
        }
        .wrap_with_cost(Default::default())
    }

    /// Whether a node may be written here with a link that names a node
    /// the store does not hold.
    ///
    /// The RocksDB adapter writes a link as the bytes of its child's key,
    /// whatever is under that key: it answers `true`, which is the
    /// default. A store that keeps a link as the place of the child has
    /// nothing to write for a child that is not there, and answers
    /// `false`: every link of a node it is handed names a node it holds,
    /// or a node of the same change set.
    ///
    /// Chunk restore asks. The chunks of a Merk arrive from the root down,
    /// and the links of a chunk to the chunks below it name no node until
    /// those are written. On a store that answers `false` the restore
    /// keeps the nodes in memory until the Merk is whole, and writes it in
    /// one change set ([`NodeChangeSet::state_whole_merk`]).
    fn takes_links_to_absent_nodes(&self) -> bool {
        true
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
