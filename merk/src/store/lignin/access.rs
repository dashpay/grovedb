//! Where the store meets the write transaction of LigninDB.
//!
//! Everything this store asks of `lignindb::txn` is asked here and nowhere
//! else: the reads of a node view, the batch a change set becomes, and the
//! report its application gives back. The rest of the store speaks of nodes
//! in Merk's terms ([`StoredNode`], [`Write`]) and reaches a transaction
//! through [`Access`].

use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard, PoisonError},
};

use grovedb_storage::{
    error::{CommitError, WriteScopeError},
    Error as StorageError,
};
use lignin_format::tree_page::{HashField, Side};
use lignin_tree::{directory::DirectoryError, hash::Stale, node::NodeHandle, value::ValueBuf};
use lignindb::txn::{
    Batch, BatchError, ChildKeys, Column, Entering, HostPut, Hosted, LinkPut, LinkView, MerkCursor,
    MerkSide, MerkWrite, NodePut, NodeView, ParkedCursor, RecordWrite, Resumed, RootPut, Scope,
    StandIns, Subtree, SubtreeView, SubtreeWrite, TxnError, Unit, ValuePut, WriteTxn,
};

use super::{
    change_set::node_put,
    directory::keeps_flat_records,
    feature::{decode_aggregate, decode_feature, encode_aggregate, encode_feature, Tp12},
    refused, LigninEntry, LigninHost, LigninHosted,
};
use crate::{cost_model::Keyspace, tree::AggregateData, CryptoHash, Error, TreeFeatureType};

/// The 32 bytes that name a subtree.
pub(super) type Prefix = [u8; 32];

/// Where the cell of a node was when the store last saw it. It is a hint
/// (`tree-layout.md` S6): the transaction follows it only while it still
/// names that cell, and finds the node by its key otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Location(NodeHandle);

impl Location {
    pub(super) const fn handle(self) -> NodeHandle {
        self.0
    }
}

/// A link of a node as it is stored.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct StoredLink {
    pub(super) hash: CryptoHash,
    pub(super) heights: (u8, u8),
    pub(super) key_len: u8,
    /// The child's key. A cell does not hold it (`tree-page.md` TP10): it
    /// is in the child's cell, and [`Access::child`] reads it there.
    pub(super) key: Option<Vec<u8>>,
    pub(super) aggregate: AggregateData,
}

/// A node as a transaction shows it, copied out of its view.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct StoredNode {
    pub(super) feature: TreeFeatureType,
    pub(super) value_hash: CryptoHash,
    pub(super) kv_hash: CryptoHash,
    pub(super) value_len: u32,
    /// The value's bytes, when the read asked for them.
    pub(super) value: Option<Vec<u8>>,
    pub(super) left: Option<StoredLink>,
    pub(super) right: Option<StoredLink>,
    /// Which of the hashes above are stale: no hash yet, whatever their
    /// bytes. None is in a committed node.
    pub(super) stale: Stale,
    /// Where the node's cell is. A node the transaction serves from its
    /// write set has no cell, and neither has an unlinked record.
    pub(super) location: Option<Location>,
    /// Whether the node is an unlinked record, which no cell keeps a
    /// value for.
    pub(super) unlinked: bool,
    /// The bytes of a flat record, where the read came to one under a
    /// tree that keeps its data as flat records (D-197). It is then no
    /// node: the read answers with these bytes, as RocksDB's does, and
    /// nothing else here says anything.
    pub(super) record: Option<Vec<u8>>,
}

impl StoredNode {
    /// What a read shows of a flat record with the bytes `record`.
    fn flat(record: Vec<u8>) -> Self {
        Self {
            feature: TreeFeatureType::BasicMerkNode,
            value_hash: CryptoHash::default(),
            kv_hash: CryptoHash::default(),
            value_len: record.len() as u32,
            value: None,
            left: None,
            right: None,
            stale: Stale::NONE,
            location: None,
            unlinked: false,
            record: Some(record),
        }
    }
}

/// What following a link from a parent's cell found.
#[derive(Debug, PartialEq)]
pub(super) enum Child {
    /// The parent's location is no longer good: the parent is read again.
    Stale,
    /// The reader reads the nodes of the Merk by their keys, and is shown
    /// no child through the location of a parent, whatever the location
    /// ([`Scope::reads_nodes_by_key`]): the read is refused
    /// ([`read_by_key`]).
    ByKey,
    /// The link leads to no node.
    Missing,
    /// The child, with where its cell is.
    Found {
        key: Vec<u8>,
        location: Option<Location>,
    },
}

/// The refusal of the read of a node whose children are not shown through
/// its location, because the reader reads the Merk's nodes by key
/// ([`Child::ByKey`]). The store is as it should be: such a reader is a
/// write scope whose writes are not laid out, and a cell does not hold
/// the keys of its children. It is a read that storage does not support,
/// and an error of storage: what reads an element through the store
/// keeps such an error as a read that failed, and takes any other for a
/// record that cannot be read.
pub(super) fn read_by_key() -> Error {
    Error::StorageError(StorageError::NotSupported(
        "lignindb: a scope whose writes are not laid out reads the children of a node by key"
            .to_owned(),
    ))
}

/// A link of a written node: the child is named by its key.
#[derive(Clone, Debug)]
pub(super) struct LinkWrite {
    pub(super) key: Vec<u8>,
    /// The child's hash. Where the write leaves it stale
    /// ([`NodeWrite::stale`]) these bytes are no hash, and the bytes the
    /// transaction holds in that link are written in their place.
    pub(super) hash: CryptoHash,
    pub(super) heights: (u8, u8),
    pub(super) aggregate: Tp12,
}

/// The value of a written node.
#[derive(Clone, Debug)]
pub(super) enum ValueWrite {
    /// The node's value is the one the transaction holds.
    Kept,
    /// The node's value. It is written only where the transaction holds
    /// other bytes, or no node.
    Stated(Vec<u8>),
}

/// One written node: every part, as Merk hands the record to storage.
#[derive(Clone, Debug)]
pub(super) struct NodeWrite {
    pub(super) key: Vec<u8>,
    pub(super) feature: Tp12,
    pub(super) value_hash: CryptoHash,
    pub(super) kv_hash: CryptoHash,
    pub(super) left: Option<LinkWrite>,
    pub(super) right: Option<LinkWrite>,
    pub(super) value: ValueWrite,
    /// The hashes above that Merk held none of when it wrote the node:
    /// they are left to the engine (`store::lignin::hash`).
    pub(super) stale: Stale,
    pub(super) hint: Option<Location>,
    /// The handle the store read the node's value with: what a node that
    /// keeps its value names as the read it is kept from. A hint is where
    /// the node is, and need not be a read of it.
    pub(super) read: Option<Location>,
    /// The Merks the node's element hosts, by position, when the write
    /// names them. Without it the node keeps the ones it has.
    pub(super) hosted: Option<Vec<Prefix>>,
}

/// What a write does to the key of a Merk's root node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RootWrite {
    Keep,
    Set(Option<Vec<u8>>),
}

/// The change set of one Merk.
#[derive(Clone, Debug)]
pub(super) struct MerkChange {
    pub(super) subtree: Prefix,
    pub(super) nodes: Vec<NodeWrite>,
    pub(super) removed: Vec<Vec<u8>>,
    pub(super) root: RootWrite,
    /// Whether the Merk has a host that the same write does not name it
    /// in.
    pub(super) anchored: bool,
    /// Whether the change set is the whole Merk, which had no node before
    /// it: every node once, a child before its parent, every value stated.
    pub(super) whole: bool,
}

/// A write of a record that is no node: the root key, an aux or a meta
/// record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecordChange {
    pub(super) keyspace: Keyspace,
    pub(super) subtree: Prefix,
    pub(super) key: Vec<u8>,
    /// The record's value, or `None` to remove the record.
    pub(super) value: Option<Vec<u8>>,
    /// The subtree that the record is kept under, by the prefix of its
    /// own subtree and its key ([`kept_key`]), where it is a root-key
    /// record whose handle keeps it outside its subtree
    /// ([`LigninNodeStore::root_key_kept_in`](super::LigninNodeStore::root_key_kept_in)).
    pub(super) home: Option<Prefix>,
}

/// The key of a record of `subtree` that is kept under another subtree:
/// the prefix of `subtree`, and then the record's own key.
pub(super) fn kept_key(subtree: &Prefix, key: &[u8]) -> Vec<u8> {
    [subtree.as_slice(), key].concat()
}

/// The record's own key in a key that [`kept_key`] made.
pub(super) fn own_key(kept: &[u8]) -> &[u8] {
    kept.get(size_of::<Prefix>()..).unwrap_or(kept)
}

/// What a write does to a subtree as a whole, beside what its batch says of
/// it: to its entry in the subtree directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SubtreeChange {
    /// The subtree is entered, with no node.
    Enter {
        subtree: Prefix,
        /// The tree type, as bytes the directory keeps and gives back.
        tree_type: Vec<u8>,
        /// The element that hosts the subtree: the subtree it is in, its
        /// key there, and the position of this subtree among those it
        /// hosts.
        host: Option<(Prefix, Vec<u8>, u8)>,
        /// Whether the subtree is entered only where the transaction does
        /// not have it entered when the write is applied, and is left as
        /// it is where it has (`SubtreeWrite::EnterIfAbsent`).
        if_absent: bool,
    },
    /// The subtree is stated anew, as a conversion in place leaves it:
    /// its tree type, and its position among the subtrees its host hosts,
    /// where it has a host.
    Convert {
        subtree: Prefix,
        tree_type: Vec<u8>,
        position: Option<u8>,
    },
}

/// The writes of one operation: what it does to whole subtrees, the change
/// sets of its Merks, applied together (`tree-layout.md` P5), and its
/// other records.
#[derive(Clone, Debug, Default)]
pub(super) struct Write {
    pub(super) subtrees: Vec<SubtreeChange>,
    pub(super) merks: Vec<MerkChange>,
    pub(super) records: Vec<RecordChange>,
}

/// What the application of a [`Write`] reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Written {
    /// For each Merk of the write, where each of its written nodes is, in
    /// the order they were given: `None` for a node that the tree of its
    /// Merk does not reach, which the transaction keeps as an unlinked
    /// record with no cell. Of a write that the transaction kept in its
    /// write set by key it reports no place, and the list of each Merk is
    /// empty.
    pub(super) locations: Vec<Vec<Option<Location>>>,
    /// How many values the write stated that the transaction did not hold.
    pub(super) values_written: usize,
    /// How many values the write stated that the transaction held already,
    /// byte for byte, and that were therefore left where they are.
    pub(super) values_kept: usize,
}

/// Where a cursor goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Seek<'k> {
    /// To the first node.
    First,
    /// To the last node.
    Last,
    /// To the first node at or after a key.
    From(&'k [u8]),
    /// To the last node at or before a key.
    Back(&'k [u8]),
    /// To the node after the one it stands on.
    Next,
    /// To the node before the one it stands on.
    Prev,
}

/// The cursor of a transaction over the nodes of one Merk, in key order,
/// as the store uses it (`transactions.md` RD6): the nodes of the committed
/// state of the moment it was opened, with the transaction's own writes as
/// they are at each call.
pub(super) trait Walk: Send {
    /// Moves the cursor. `key` is emptied and takes the key the cursor then
    /// stands on, and the answer is whether it stands on one.
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error>;

    /// How many records the data of the subtrees after the cursor's own
    /// holds, in the order of their prefixes, or with `back` the data of
    /// those before it: nodes and flat records, as a cursor of each
    /// subtree shows them, counted from the nearest subtree on and up to
    /// `limit`. A step from no key asks this, and no other move
    /// ([`Place`](super::place::Place)). With `stand_ins`, as
    /// [`Cursor::beside_with`](lignindb::txn::Cursor::beside_with) counts.
    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error>;

    /// The node under the key the cursor stands on, with its value and
    /// with the key of each child in its link. `None` if the transaction
    /// has deleted the node since the cursor came to its key.
    fn node(&mut self) -> Result<Option<StoredNode>, Error>;

    /// Points the cursor at the Merk of `subtree`, in the committed state
    /// it has read from since it was opened. It stands on no node then,
    /// and is sought before it is read or stepped.
    ///
    /// A cursor of a snapshot goes on in its snapshot. A cursor of a write
    /// transaction keeps the version it pinned when it was opened, and
    /// shows the nodes that version has of `subtree` with the
    /// transaction's writes to `subtree` as they are at each call.
    fn retarget(&mut self, subtree: &Prefix) -> Result<(), Error>;
}

/// Why a cursor stands on nothing, kept so that its status gives the error
/// again, with the class it had: a refusal of a write scope and a limit of
/// the engine as what they are, and anything else by its words.
#[derive(Clone, Debug)]
pub(super) enum Failure {
    Scope(WriteScopeError),
    NotSupported(String),
    Words(String),
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        match error {
            Error::StorageError(StorageError::WriteScope(error)) => Self::Scope(error),
            Error::StorageError(StorageError::NotSupported(what)) | Error::NotSupported(what) => {
                Self::NotSupported(what)
            }
            error => Self::Words(error.to_string()),
        }
    }
}

impl Failure {
    pub(super) fn error(&self) -> StorageError {
        match self {
            Self::Scope(error) => StorageError::WriteScope(*error),
            Self::NotSupported(what) => StorageError::NotSupported(what.clone()),
            Self::Words(words) => StorageError::StorageError(words.clone()),
        }
    }
}

/// One writer's view of a LigninDB transaction, as the store uses it, or a
/// reader's view of one committed version.
pub(super) trait Access: Sync + super::flat::FlatAccess {
    /// A cursor over the nodes of `subtree`, which stands on no node yet.
    fn cursor<'a>(&'a self, subtree: &Prefix) -> Result<Box<dyn Walk + 'a>, Error>;

    /// The node under `key` in `subtree`, through `hint` while that is
    /// good. The value's bytes are copied when `with_value` asks for them.
    fn node(
        &self,
        subtree: &Prefix,
        key: &[u8],
        hint: Option<Location>,
        with_value: bool,
    ) -> Result<Option<StoredNode>, Error>;

    /// The node under `key` as the committed state has it, whatever the
    /// transaction wrote there, without its value's bytes.
    fn committed_node(&self, subtree: &Prefix, key: &[u8]) -> Result<Option<StoredNode>, Error>;

    /// The child a link of the node at `parent` leads to.
    fn child(&self, subtree: &Prefix, parent: Location, left: bool) -> Result<Child, Error>;

    /// What the subtree directory holds of `subtree`, or `None` for a
    /// subtree that is not entered.
    fn subtree(&self, subtree: &Prefix) -> Result<Option<LigninEntry>, Error>;

    /// The record under `key` in a keyspace of `subtree` that holds no
    /// nodes.
    fn record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error>;

    /// The record under `key` as the committed state has it, whatever the
    /// transaction wrote there.
    fn committed_record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error>;

    /// Applies `write` to the transaction as one operation: all of it or
    /// nothing.
    fn apply(&self, write: &Write) -> Result<Written, Error>;

    /// Applies the batches of `parts` to the transaction, one operation
    /// each and in their order, and all of them or none. `enters` are
    /// subtrees that are entered or stated anew with them, each with the
    /// part it goes with, besides those the part enters itself. Where the
    /// transaction refuses a batch with one of the two refusals that
    /// [`LigninEntryRefused`] names, `entry` is given which.
    fn apply_parts(
        &self,
        parts: &[&dyn BatchPart],
        enters: &[(usize, SubtreeChange)],
        entry: &mut Option<LigninEntryRefused>,
    ) -> Result<Vec<Written>, Error>;

    /// Names in the node of the first element of `hosts` the Merks that
    /// the element hosts, as one operation of the transaction: the node
    /// is put again as the transaction holds it, with the node above it,
    /// so that the tree pages may carry it where it grows. The elements
    /// after the first are those of the Merks above, each of which is
    /// named in the same way where the element below it is the root of
    /// its Merk. An element whose node is not found, or whose first Merk
    /// is not entered under it, is refused.
    fn name_hosted(&self, hosts: &[LigninHost]) -> Result<(), Error>;
}

/// The entry of a subtree as a transaction found it, where it refused a
/// batch with one of two refusals of `lignindb::txn::BatchError`:
/// `NotEntered` and `EnteredOtherwise`. No other refusal is told so, also
/// one that is of an entry: an enter of a subtree as one that is new,
/// where the subtree is entered (`Entered`), is told as an error alone.
///
/// The transaction judges a batch on what the batches before it in its
/// group and its own drops and clears left: a subtree that one of those
/// took out, and that none entered again, has no entry to it, whatever
/// the transaction held before. Where the transaction lays the batch out
/// in its frames, a drop takes out its subtree and those below it, and a
/// clear of a Merk that has a root takes out those below it; a clear of
/// a Merk with no node takes out none, and a drop of one that has a
/// subtree below it is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LigninEntryRefused {
    /// The subtree has no entry (`NotEntered`).
    NotEntered(Prefix),
    /// The subtree has an entry with another tree type or another host
    /// than an enter if absent of the batch states (`EnteredOtherwise`).
    EnteredOtherwise(Prefix),
}

fn entry_refused(error: &TxnError) -> Option<LigninEntryRefused> {
    match error {
        TxnError::Batch(BatchError::NotEntered { subtree }) => {
            Some(LigninEntryRefused::NotEntered(*subtree.as_bytes()))
        }
        TxnError::Batch(BatchError::EnteredOtherwise { subtree, .. }) => {
            Some(LigninEntryRefused::EnteredOtherwise(*subtree.as_bytes()))
        }
        _ => None,
    }
}

/// A batch that is handed to a transaction as it stands when it is asked
/// for: the batch of an operation, resolved.
pub(super) trait BatchPart {
    /// Calls `run` with the batch, and returns what it returns.
    fn with_batch(
        &self,
        run: &mut dyn FnMut(&Batch<'_>) -> Result<Written, Error>,
    ) -> Result<Written, Error>;

    /// The element that the batch says hosts the Merk `merk`, with every
    /// Merk it hosts, by position: what the node of that element names
    /// where the batch puts it. `None` for a Merk that nothing was said
    /// of.
    fn host_of(&self, merk: Subtree) -> Option<LigninHost>;
}

/// A read or a write the transaction did not perform. Every failure of the
/// engine is reported as a storage error and under no other
/// (`node-store.md`, "Errors"), but for one: a subtree that is entered
/// more Merks deep than the subtree directory takes. That is a limit of
/// the engine which the write met and no fault of the store, and a caller
/// tells it from damage by its error.
///
/// Among the storage errors two have a class that names no engine: a
/// claim of another open scope, and a write that the transaction refused
/// because a commit outside it took a node whose value the write keeps
/// (`TxnError::is_conflict`). The second is the conflict of the seam,
/// `CommitError::Conflict`: the write changed nothing, the transaction is
/// open, and a caller answers it as it answers a commit that lost.
pub(super) fn fault(error: TxnError) -> Error {
    match error {
        TxnError::Directory(DirectoryError::TooDeep) => {
            Error::NotSupported(format!("lignindb: {error}"))
        }
        // A tree that keeps flat records has a keyed child under the key
        // of the record that is written (grove versions 1 to 3): the
        // engine keeps one record under a key, and does not take the
        // write (`engine.md`, "What differs on grove versions 1 to 3
        // under a tree that keeps flat records").
        TxnError::Batch(BatchError::RecordUnderNode { .. }) => {
            Error::NotSupported(format!("lignindb: {error}"))
        }
        // A claim of another open scope is what stood in the way, and
        // is named so through every seam (`engine.md`, "Write scopes").
        TxnError::ScopeOverlap { .. } => {
            Error::StorageError(StorageError::WriteScope(WriteScopeError::Overlap))
        }
        error if error.is_conflict() => {
            Error::StorageError(StorageError::Commit(CommitError::Conflict))
        }
        error => refused(&error.to_string()),
    }
}

pub(super) fn column(keyspace: Keyspace) -> Column {
    match keyspace {
        Keyspace::Data => Column::Data,
        Keyspace::Aux => Column::Aux,
        Keyspace::Roots => Column::Roots,
        Keyspace::Meta => Column::Meta,
    }
}

fn side(left: bool) -> Side {
    match left {
        true => Side::Left,
        false => Side::Right,
    }
}

fn stored_link(link: LinkView<'_>) -> Result<StoredLink, Error> {
    Ok(StoredLink {
        hash: *link.hash,
        heights: link.heights,
        key_len: link.child_key_len,
        key: link.key.map(<[u8]>::to_vec),
        aggregate: decode_aggregate(link.aggregate)?,
    })
}

pub(super) fn stored_node(view: &NodeView<'_>, with_value: bool) -> Result<StoredNode, Error> {
    if let Some(record) = view.flat_record() {
        return Ok(StoredNode::flat(record.to_vec()));
    }
    let value = match with_value {
        false => None,
        true => Some(view.value(&mut ValueBuf::new()).map_err(fault)?.to_vec()),
    };
    Ok(StoredNode {
        feature: decode_feature(view.feature())?,
        value_hash: *view.value_hash(),
        kv_hash: *view.kv_hash(),
        value_len: view.value_len(),
        value,
        left: view.link(Side::Left).map(stored_link).transpose()?,
        right: view.link(Side::Right).map(stored_link).transpose()?,
        stale: view.stale(),
        location: view.handle().map(Location),
        unlinked: view.is_unlinked(),
        record: None,
    })
}

/// The key of each child of `node` in its link. A cell names a child by
/// its address, and the key was read from the child's cell with the node
/// (`children`); a record of the write set names its children by key.
pub(super) fn with_child_keys(
    mut node: StoredNode,
    children: &mut ChildKeys,
) -> Result<StoredNode, Error> {
    for (side, link) in [(Side::Left, &mut node.left), (Side::Right, &mut node.right)] {
        let Some(link) = link else {
            continue;
        };
        let key = link.key.get_or_insert_with(|| children.take(side));
        if key.len() != usize::from(link.key_len) {
            return Err(Error::CorruptedState(
                "a child's key was not read with its node",
            ));
        }
    }
    Ok(node)
}

/// What the store asks of a cursor of the transaction, for the cursor
/// itself and for a parked one that is resumed: a move, which gives the
/// key the cursor then stands on, and the node under that key, read with
/// the keys of its children in one operation (for a node of the cursor's
/// committed side at the cursor's version).
macro_rules! walks {
    ($go:ident, $node:ident, $cursor:ty) => {
        fn $go<'s>(cursor: &mut $cursor, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
            key.clear();
            match to {
                Seek::First => cursor.seek_to_first(),
                Seek::Last => cursor.seek_to_last(),
                Seek::From(key) => cursor.seek(key),
                Seek::Back(key) => cursor.seek_for_prev(key),
                Seek::Next => cursor.next(),
                Seek::Prev => cursor.prev(),
            }
            .map_err(fault)?;
            key.extend_from_slice(cursor.key().unwrap_or_default());
            Ok(cursor.valid())
        }

        fn $node<'s>(cursor: &mut $cursor) -> Result<Option<StoredNode>, Error> {
            let mut children = ChildKeys::new();
            let node = cursor
                .node_with_children(&mut children, |view| {
                    view.map(|view| stored_node(&view, true)).transpose()
                })
                .map_err(fault)??;
            node.map(|node| with_child_keys(node, &mut children))
                .transpose()
        }
    };
}

walks!(go, node_under, MerkCursor<'_, 's>);
walks!(go_resumed, node_resumed, Resumed<'_, '_, 's, MerkSide<'s>>);

/// The cursor of a handle that only reads: a cursor of its transaction.
struct TxnWalk<'a, 's>(MerkCursor<'a, 's>);

impl Walk for TxnWalk<'_, '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        go(&mut self.0, to, key)
    }

    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error> {
        (self.0.beside_with(back, limit, stand_ins)).map_err(fault)
    }

    fn node(&mut self) -> Result<Option<StoredNode>, Error> {
        node_under(&mut self.0)
    }

    fn retarget(&mut self, subtree: &Prefix) -> Result<(), Error> {
        self.0.retarget(Subtree::new(*subtree)).map_err(fault)
    }
}

/// The cursor of a writer's handle: parked between two calls
/// ([`ParkedCursor`]), and resumed under the writer's lock for the length
/// of each.
struct ScopeWalk<'a, 't, 's> {
    writer: &'a LigninWriter<'t, 's>,
    parked: ParkedCursor<'t, 's, MerkSide<'s>>,
}

impl<'s> ScopeWalk<'_, '_, 's> {
    fn resumed<T>(
        &mut self,
        call: impl FnOnce(&mut Resumed<'_, '_, 's, MerkSide<'s>>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let scope = self.writer.scope();
        let mut cursor = scope
            .resume(&mut self.parked)
            .ok_or_else(|| refused("a cursor of another scope"))?;
        call(&mut cursor)
    }
}

impl Walk for ScopeWalk<'_, '_, '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        self.resumed(|cursor| go_resumed(cursor, to, key))
    }

    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error> {
        self.resumed(|cursor| (cursor.beside_with(back, limit, stand_ins)).map_err(fault))
    }

    fn node(&mut self) -> Result<Option<StoredNode>, Error> {
        self.resumed(node_resumed)
    }

    fn retarget(&mut self, subtree: &Prefix) -> Result<(), Error> {
        self.resumed(|cursor| cursor.retarget(Subtree::new(*subtree)).map_err(fault))
    }
}

/// The reads a write transaction offers outside any scope and a scope
/// offers inside it, under one name.
pub(super) trait Reads {
    fn node<R>(
        &self,
        subtree: Subtree,
        key: &[u8],
        hint: Option<NodeHandle>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError>;

    fn committed_node<R>(
        &self,
        subtree: Subtree,
        key: &[u8],
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError>;

    fn child<R>(
        &self,
        subtree: Subtree,
        parent: NodeHandle,
        side: Side,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<Option<R>, TxnError>;

    fn get(
        &self,
        unit: Unit,
        key: &[u8],
        committed: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError>;

    fn subtree<R>(
        &self,
        subtree: Subtree,
        read: impl FnOnce(Option<SubtreeView<'_>>) -> R,
    ) -> Result<R, TxnError>;

    /// Whether the reader reads the nodes of the Merk `subtree` by their
    /// keys ([`Scope::reads_nodes_by_key`]): [`Self::child`] then answers
    /// that a parent's location is no longer good whatever the location.
    /// A write transaction that is read outside any scope never does.
    fn by_key(&self, subtree: Subtree) -> bool;
}

/// [`Reads::by_key`] of a write transaction that is read outside any
/// scope.
fn never_by_key(_: &WriteTxn<'_>, _: Subtree) -> bool {
    false
}

macro_rules! reads {
    ($reader:ty, $by_key:path) => {
        impl Reads for $reader {
            fn by_key(&self, subtree: Subtree) -> bool {
                $by_key(self, subtree)
            }

            fn node<R>(
                &self,
                subtree: Subtree,
                key: &[u8],
                hint: Option<NodeHandle>,
                read: impl FnOnce(Option<NodeView<'_>>) -> R,
            ) -> Result<R, TxnError> {
                self.node_by_kind(subtree, key, hint, keeps_flat_records, read)
            }

            fn committed_node<R>(
                &self,
                subtree: Subtree,
                key: &[u8],
                read: impl FnOnce(Option<NodeView<'_>>) -> R,
            ) -> Result<R, TxnError> {
                self.committed_node_by_kind(subtree, key, keeps_flat_records, read)
            }

            fn child<R>(
                &self,
                subtree: Subtree,
                parent: NodeHandle,
                side: Side,
                read: impl FnOnce(Option<NodeView<'_>>) -> R,
            ) -> Result<Option<R>, TxnError> {
                self.child(subtree, parent, side, read)
            }

            fn get(
                &self,
                unit: Unit,
                key: &[u8],
                committed: bool,
                out: &mut Vec<u8>,
            ) -> Result<bool, TxnError> {
                match committed {
                    true => self.committed_get(unit, key, out),
                    false => self.get(unit, key, out),
                }
            }

            fn subtree<R>(
                &self,
                subtree: Subtree,
                read: impl FnOnce(Option<SubtreeView<'_>>) -> R,
            ) -> Result<R, TxnError> {
                self.subtree(subtree, read)
            }
        }
    };
}

reads!(WriteTxn<'_>, never_by_key);
reads!(Scope<'_, '_>, Scope::reads_nodes_by_key);

fn read_node(
    reads: &impl Reads,
    subtree: &Prefix,
    key: &[u8],
    hint: Option<Location>,
    with_value: bool,
) -> Result<Option<StoredNode>, Error> {
    let hint = hint.map(|Location(handle)| handle);
    reads
        .node(Subtree::new(*subtree), key, hint, |view| {
            view.map(|view| stored_node(&view, with_value)).transpose()
        })
        .map_err(fault)?
}

fn read_committed_node(
    reads: &impl Reads,
    subtree: &Prefix,
    key: &[u8],
) -> Result<Option<StoredNode>, Error> {
    reads
        .committed_node(Subtree::new(*subtree), key, |view| {
            view.map(|view| stored_node(&view, false)).transpose()
        })
        .map_err(fault)?
}

fn read_child(
    reads: &impl Reads,
    subtree: &Prefix,
    Location(parent): Location,
    left: bool,
) -> Result<Child, Error> {
    let child = reads
        .child(Subtree::new(*subtree), parent, side(left), |view| {
            view.map(|view| Child::Found {
                key: view.key().to_vec(),
                location: view.handle().map(Location),
            })
        })
        .map_err(fault)?;
    Ok(match child {
        // The transaction gives one answer where the location has ended
        // and where the reader is shown no child through any location.
        // The question is asked after the answer: a scope beside the
        // reader can change it in between, and the read is refused if
        // the reader reads by key by now, and made again if not.
        None if reads.by_key(Subtree::new(*subtree)) => Child::ByKey,
        None => Child::Stale,
        Some(None) => Child::Missing,
        Some(Some(found)) => found,
    })
}

fn read_record(
    reads: &impl Reads,
    keyspace: Keyspace,
    subtree: &Prefix,
    key: &[u8],
    committed: bool,
) -> Result<Option<Vec<u8>>, Error> {
    let mut value = Vec::new();
    let unit = Unit::new(column(keyspace), Subtree::new(*subtree));
    let found = Reads::get(reads, unit, key, committed, &mut value).map_err(fault)?;
    Ok(found.then_some(value))
}

fn read_subtree(reads: &impl Reads, subtree: &Prefix) -> Result<Option<LigninEntry>, Error> {
    let entry = |view: SubtreeView<'_>| LigninEntry {
        root_key: view.root_key.map(<[u8]>::to_vec),
        tree_type: view.tree_type.to_vec(),
        host: (view.host).map(|host| LigninHosted {
            subtree: *host.subtree.as_bytes(),
            key: host.key.to_vec(),
            position: host.position,
        }),
    };
    Reads::subtree(reads, Subtree::new(*subtree), |view| view.map(entry)).map_err(fault)
}

/// What the transaction holds under the key of a written node, as far as
/// its write asks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Held {
    /// Whether it holds the value the write states, byte for byte.
    value: bool,
    /// The handle the node was shown with for that comparison, where the
    /// value is held: none for the transaction's own record of the node.
    /// The node then keeps its value, and names this as the read it is
    /// kept from.
    read: Option<NodeHandle>,
    /// The bytes it has for the hash of each child that the write leaves
    /// stale, where it has a link on that side.
    links: [Option<CryptoHash>; 2],
}

/// Reads what the transaction holds under the key of `node`, if the write
/// states a value or leaves a hash stale. The bytes of a value are
/// compared, never a hash: a stored value can change while no hash does
/// (`node-store.md`, "Errors").
fn held(scope: &Scope<'_, '_>, subtree: Subtree, node: &NodePut<'_>) -> Result<Held, Error> {
    let stated = match node.value {
        ValuePut::Value(stated) => Some(stated),
        ValuePut::Keep => None,
    };
    if stated.is_none() && node.stale.is_empty() {
        return Ok(Held::default());
    }
    Reads::node(scope, subtree, node.key, node.at, |view| {
        let Some(view) = view else {
            return Ok(Held::default());
        };
        let value = match stated {
            // No cell holds the value of an unlinked record: a value
            // that is stated over one is written, with no comparison.
            // Nor does one hold a value where the key has a flat record
            // of a tree that keeps flat records, whose bytes are no
            // node's value.
            Some(_) if view.is_unlinked() || view.flat_record().is_some() => false,
            // A value of another length is not read to be compared.
            Some(stated) if view.value_len() as usize == stated.len() => {
                view.value(&mut ValueBuf::new())? == stated
            }
            _ => false,
        };
        let read = view.handle().filter(|_| value);
        let links = Side::BOTH.map(|side| {
            let link = view.link(side);
            let link = link.filter(|_| node.stale.has(HashField::Child(side)));
            link.map(|link| *link.hash)
        });
        Ok(Held { value, read, links })
    })
    .and_then(|held| held)
    .map_err(fault)
}

/// A link as the transaction is handed it. For a hash the write leaves
/// stale, `held` has the bytes the cell has there, which are stated again
/// so that the write changes none of them (`NodePut::stale`).
fn link_put<'a>(link: Option<LinkPut<'a>>, held: &'a Option<CryptoHash>) -> Option<LinkPut<'a>> {
    link.map(|link| LinkPut {
        hash: held.as_ref().unwrap_or(link.hash),
        ..link
    })
}

fn subtree_write(change: &SubtreeChange) -> SubtreeWrite<'_> {
    match change {
        SubtreeChange::Enter {
            subtree,
            tree_type,
            host,
            if_absent,
        } => {
            let entering = Entering {
                subtree: Subtree::new(*subtree),
                tree_type,
                host: host.as_ref().map(|(subtree, key, position)| HostPut {
                    subtree: Subtree::new(*subtree),
                    key,
                    position: *position,
                }),
                if_absent: *if_absent,
            };
            entering.write()
        }
        SubtreeChange::Convert {
            subtree,
            tree_type,
            position,
        } => SubtreeWrite::Convert {
            subtree: Subtree::new(*subtree),
            tree_type,
            position: *position,
        },
    }
}

#[cfg(test)]
thread_local! {
    /// What a test has happen, once and on its own thread, between the
    /// reads that compare the stated values of a batch and the batch's
    /// application ([`apply_batch`]): a commit of another transaction.
    pub(super) static AFTER_COMPARING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Applies `batch` to the transaction of `scope` as one operation.
///
/// A stated value that the transaction holds already is kept where it is,
/// so that a value is written only when it changed, and a hash that is
/// left stale keeps the bytes it has. A node that keeps its value names
/// the read the value is kept from: the store's own read of the node
/// where Merk says the value is unchanged, and the comparing read here,
/// which is of exactly the bytes that are kept. A Merk whose subtree the
/// batch enters as one that is new, or drops or clears by its name, has
/// no node before the write, whatever the transaction holds under its
/// prefix until then: no node of it is read here to compare. Nor is a
/// node read here to compare for a Merk that `whole` marks, by its place
/// among the Merks of the batch: its change set says that it is the whole
/// Merk, and `apply` has checked it. One that the batch only enters if it
/// is absent may be entered, with nodes, and is read as any other.
///
/// A batch that drops or clears a subtree can take a Merk out with it
/// that it names no leave of, and enter that Merk again if it is absent:
/// the Merk it writes is then a new one, and the value the transaction
/// holds under a key is of the Merk that goes. Which subtree is below
/// which is not asked here. So of a Merk that a batch enters if it is
/// absent, where the batch drops or clears any subtree, every stated value
/// is written, also one that the transaction holds. A node of it is read
/// only where its write leaves a hash stale, for the bytes of that hash:
/// a node that is new may state any for one.
///
/// Where the transaction refuses the batch with one of the two refusals
/// that [`LigninEntryRefused`] names, `entry` is given which.
pub(super) fn apply_batch(
    scope: &Scope<'_, '_>,
    batch: &Batch<'_>,
    whole: &[bool],
    entry: &mut Option<LigninEntryRefused>,
) -> Result<Written, Error> {
    let mut written = Written::default();
    let fresh = |subtree: Subtree| {
        (batch.subtrees.iter()).any(|write| match write {
            // A subtree that is stated anew keeps its nodes.
            SubtreeWrite::EnterIfAbsent { .. } | SubtreeWrite::Convert { .. } => false,
            write => write.subtree() == subtree,
        })
    };
    let leaves = batch.subtrees.iter().any(SubtreeWrite::leaves);
    let restated = |subtree: Subtree| {
        let mut writes = batch.subtrees.iter();
        leaves
            && writes.any(|write| {
                matches!(write, SubtreeWrite::EnterIfAbsent { .. }) && write.subtree() == subtree
            })
    };
    let mut keep = Vec::with_capacity(batch.merks.len());
    for (at, merk) in batch.merks.iter().enumerate() {
        let fresh = whole.get(at) == Some(&true) || fresh(merk.subtree);
        let restated = restated(merk.subtree);
        let mut kept = Vec::with_capacity(merk.nodes.len());
        for node in merk.nodes {
            let mut held = match fresh || (restated && node.stale.is_empty()) {
                true => Held::default(),
                false => held(scope, merk.subtree, node)?,
            };
            if restated {
                (held.value, held.read) = (false, None);
            }
            if matches!(node.value, ValuePut::Value(_)) {
                match held.value {
                    true => written.values_kept += 1,
                    false => written.values_written += 1,
                }
            }
            kept.push(held);
        }
        keep.push(kept);
    }
    #[cfg(test)]
    if let Some(between) = AFTER_COMPARING.take() {
        between();
    }
    let nodes: Vec<Vec<NodePut<'_>>> = (batch.merks.iter().zip(&keep))
        .map(|(merk, keep)| {
            let puts = (merk.nodes.iter().zip(keep)).map(|(node, keep)| NodePut {
                left: link_put(node.left, &keep.links[0]),
                right: link_put(node.right, &keep.links[1]),
                value: match keep.value {
                    true => ValuePut::Keep,
                    false => node.value,
                },
                at: match (node.value, keep.value) {
                    (ValuePut::Value(_), true) => keep.read,
                    _ => node.at,
                },
                ..*node
            });
            puts.collect()
        })
        .collect();
    let merks: Vec<MerkWrite<'_>> = (batch.merks.iter().zip(&nodes))
        .map(|(merk, nodes)| MerkWrite { nodes, ..*merk })
        .collect();
    // The tree types that keep flat records go with every batch: under
    // one of them a subtree takes flat records beside the nodes of the
    // Merk of its keyed children (grove versions 1 to 3).
    let stated = Batch {
        merks: &merks,
        ..*batch
    };
    let applied = scope
        .apply_by_kind(&stated, keeps_flat_records)
        .map_err(|error| {
            *entry = entry_refused(&error);
            fault(error)
        })?;

    // The handles come in the order of the batch: the nodes of the first
    // Merk, then those of the second, without those that the transaction
    // keeps as unlinked records, which it names by their places. Of a
    // write that it keeps in its write set by key it reports neither.
    if applied.nodes.is_empty() && applied.unlinked.is_empty() {
        written.locations = vec![Vec::new(); batch.merks.len()];
        return Ok(written);
    }
    debug_assert_eq!(
        applied.nodes.len() + applied.unlinked.len(),
        batch
            .merks
            .iter()
            .map(|merk| merk.nodes.len())
            .sum::<usize>(),
        "a written node has a cell or is an unlinked record"
    );
    let mut unlinked = applied.unlinked.iter().copied().peekable();
    let mut handles = applied.nodes.into_iter().map(Location);
    let mut places = (0..).map(|place| match unlinked.next_if_eq(&place) {
        Some(_) => None,
        None => handles.next(),
    });
    written.locations = (batch.merks.iter())
        .map(|merk| places.by_ref().take(merk.nodes.len()).collect())
        .collect();
    Ok(written)
}

/// A link of a written node as the transaction is handed it.
pub(super) fn link_of(link: &Option<LinkWrite>) -> Option<LinkPut<'_>> {
    link.as_ref().map(|link| LinkPut {
        key: &link.key,
        hash: &link.hash,
        heights: link.heights,
        aggregate: link.aggregate.as_slice(),
    })
}

fn apply(scope: &Scope<'_, '_>, write: &Write) -> Result<Written, Error> {
    // No node of a change set that says it is the whole Merk is read here
    // to compare its value. So such a change set must state every value
    // and remove nothing, and it is refused for a subtree with no entry
    // and for a Merk whose entry names a root. The entry is read before the
    // write, as a read of its own: what a commit outside the transaction
    // does to the entry after that read is not checked here.
    for merk in write.merks.iter().filter(|merk| merk.whole) {
        let keeps = |node: &NodeWrite| matches!(node.value, ValueWrite::Kept);
        if merk.nodes.iter().any(keeps) || !merk.removed.is_empty() {
            return Err(refused(
                "a change set that is said to be the whole Merk keeps a value or removes a node",
            ));
        }
        let entry = read_subtree(scope, &merk.subtree)?;
        if entry.is_none() {
            return Err(refused(
                "a change set that is said to be the whole Merk, for a subtree that is not entered",
            ));
        }
        let has_root = |entry: LigninEntry| entry.root_key.is_some();
        if entry.is_some_and(has_root) {
            return Err(refused(
                "a change set that is said to be the whole Merk, for a Merk that has a root",
            ));
        }
    }
    let whole: Vec<bool> = write.merks.iter().map(|merk| merk.whole).collect();

    let hosted: Vec<Vec<Option<Vec<Subtree>>>> = (write.merks.iter())
        .map(|merk| {
            let lists = merk.nodes.iter().map(|node| {
                let hosted = node.hosted.as_ref();
                hosted.map(|hosted| hosted.iter().copied().map(Subtree::new).collect())
            });
            lists.collect()
        })
        .collect();
    let nodes: Vec<Vec<NodePut<'_>>> = (write.merks.iter().zip(&hosted))
        .map(|(merk, hosted)| {
            let puts = (merk.nodes.iter().zip(hosted)).map(|(node, hosted)| NodePut {
                hosted: match hosted {
                    None => Hosted::Keep,
                    Some(hosted) => Hosted::Set(hosted),
                },
                ..node_put(node)
            });
            puts.collect()
        })
        .collect();
    let removed: Vec<Vec<&[u8]>> = (write.merks.iter())
        .map(|merk| merk.removed.iter().map(Vec::as_slice).collect())
        .collect();
    let merks: Vec<MerkWrite<'_>> = (write.merks.iter().zip(&nodes).zip(&removed))
        .map(|((merk, nodes), removed)| MerkWrite {
            subtree: Subtree::new(merk.subtree),
            nodes,
            removed,
            root: match &merk.root {
                RootWrite::Keep => RootPut::Keep,
                RootWrite::Set(root) => RootPut::Set(root.as_deref()),
            },
            anchored: merk.anchored,
            whole: merk.whole,
        })
        .collect();
    // A record that is kept outside its subtree is written where it is
    // kept.
    let kept: Vec<Option<(Subtree, Vec<u8>)>> = (write.records.iter())
        .map(|record| {
            let home = record.home?;
            Some((Subtree::new(home), kept_key(&record.subtree, &record.key)))
        })
        .collect();
    let records: Vec<RecordWrite<'_>> = (write.records.iter().zip(&kept))
        .map(|(record, kept)| {
            let (subtree, key) = match kept {
                Some((home, key)) => (*home, key.as_slice()),
                None => (Subtree::new(record.subtree), record.key.as_slice()),
            };
            match &record.value {
                Some(value) => RecordWrite::put(column(record.keyspace), subtree, key, value),
                None => RecordWrite::delete(column(record.keyspace), subtree, key),
            }
        })
        .collect();
    let subtrees: Vec<SubtreeWrite<'_>> = write.subtrees.iter().map(subtree_write).collect();
    apply_batch(
        scope,
        &Batch {
            records: &records,
            merks: &merks,
            subtrees: &subtrees,
        },
        &whole,
        &mut None,
    )
}

/// A node that a write puts again as the transaction holds it
/// ([`held_elements`]).
struct PutAgain {
    subtree: Subtree,
    node: NodeWrite,
    /// The Merks the node's element hosts, by position, where the write
    /// names them in the node: the node is that of an element that is
    /// held.
    hosted: Option<Vec<Subtree>>,
}

/// What a write holds above the Merks that it writes below their
/// elements ([`held_elements`]).
#[derive(Default)]
struct HeldElements {
    again: Vec<PutAgain>,
    /// The Merks of the batch that a node put again names, by their
    /// place in the batch: none of them is anchored.
    named: Vec<usize>,
    /// The subtrees of nodes put again that the batch holds no change
    /// set for, each with whether its Merk is handed over as anchored:
    /// it has a host, which is not held.
    added: Vec<(Subtree, bool)>,
    /// The elements that were looked at, by subtree and key.
    looked: Vec<(Subtree, Vec<u8>)>,
}

/// The longest way from the root of a Merk to a node that is followed:
/// a Merk is an AVL tree, whose height stays under one and a half times
/// the binary logarithm of its nodes.
const LONGEST_WAY: usize = 96;

/// Whether `batch` takes `subtree` out or moves it: by a drop, a clear
/// or a move that names it, or by one that may take it with a subtree
/// above it, which is one beside an enter of `subtree`. What the
/// transaction holds under such a subtree before the batch is not what
/// the batch leaves there.
fn taken(batch: &Batch<'_>, subtree: Subtree) -> bool {
    let named = |write: &SubtreeWrite<'_>| match write.moving() {
        Some(moving) => {
            let moved = (moving.from, moving.to);
            let mut pairs = std::iter::once(&moved).chain(moving.below);
            pairs.any(|(from, to)| *from == subtree || *to == subtree)
        }
        None => write.leaves() && write.subtree() == subtree,
    };
    let takes = |write: &SubtreeWrite<'_>| write.leaves() || write.moving().is_some();
    let entered = |write: &SubtreeWrite<'_>| write.enters() && write.subtree() == subtree;
    let mut writes = batch.subtrees.iter();
    writes.clone().any(named) || (writes.clone().any(takes) && writes.any(entered))
}

/// A node of the tree of a Merk as the transaction holds it, with the
/// key of its left and of its right child, each where it was asked for
/// and the node has one.
type Linked = (StoredNode, [Option<Vec<u8>>; 2]);

/// The node under `key` of `subtree` as the transaction holds it, if it
/// is a node of the tree: no unlinked record and no flat record. With
/// the key of its child on the left and on the right, where `sides` asks
/// for it: as the link states it, or read from the child's cell.
///
/// The node and its children are read in operations of their own, the
/// children through the location of the node's cell, and that location
/// can end between two of them. The transaction then says that the
/// location is no longer good, and the node is read again, as a handle
/// reads it (`LigninNodeStore::load`): with no count of tries, and
/// refused at once where the reader reads the Merk's nodes by key
/// ([`Child::ByKey`]). A link that leads to no node is refused there too.
fn linked(
    reads: &impl Reads,
    subtree: Subtree,
    key: &[u8],
    sides: [bool; 2],
) -> Result<Option<Linked>, Error> {
    'node: loop {
        let stored = read_node(reads, subtree.as_bytes(), key, None, false)?;
        let Some(stored) = stored.filter(|stored| !stored.unlinked && stored.record.is_none())
        else {
            return Ok(None);
        };
        let mut children = [None, None];
        for (side, left) in [(0, true), (1, false)] {
            let link = match left {
                true => &stored.left,
                false => &stored.right,
            };
            let Some(link) = link.as_ref().filter(|_| sides[side]) else {
                continue;
            };
            let child = match (&link.key, stored.location) {
                (Some(key), _) => key.clone(),
                (None, Some(location)) => {
                    match read_child(reads, subtree.as_bytes(), location, left)? {
                        Child::Found { key, .. } => key,
                        Child::Stale => continue 'node,
                        Child::ByKey => return Err(read_by_key()),
                        Child::Missing => return Err(refused("a link leads to no node")),
                    }
                }
                (None, None) => {
                    return Err(Error::CorruptedState("a cell was read with no location"))
                }
            };
            children[side] = Some(child);
        }
        return Ok(Some((stored, children)));
    }
}

/// The node under `key` of `subtree` as the transaction holds it, to be
/// put again as it is: its value kept, by the read that is made here,
/// and its feature, its hashes and its links stated as they were read.
/// `None` for a key that holds no such node: no node of the tree, or a
/// node with a hash that is stale.
fn as_it_is(reads: &impl Reads, subtree: Subtree, key: &[u8]) -> Result<Option<NodeWrite>, Error> {
    let Some((stored, children)) = linked(reads, subtree, key, [true, true])? else {
        return Ok(None);
    };
    if !stored.stale.is_empty() {
        return Ok(None);
    }
    let [on_the_left, on_the_right] = children;
    let link = |link: &Option<StoredLink>, child: Option<Vec<u8>>| match (link, child) {
        (Some(link), Some(key)) => Ok(Some(LinkWrite {
            key,
            hash: link.hash,
            heights: link.heights,
            aggregate: encode_aggregate(&link.aggregate),
        })),
        (None, _) => Ok(None),
        (Some(_), None) => Err(Error::CorruptedState("a link was read with no child")),
    };
    let left = link(&stored.left, on_the_left)?;
    let right = link(&stored.right, on_the_right)?;
    Ok(Some(NodeWrite {
        key: key.to_vec(),
        feature: encode_feature(&stored.feature),
        value_hash: stored.value_hash,
        kv_hash: stored.kv_hash,
        left,
        right,
        value: ValueWrite::Kept,
        stale: Stale::NONE,
        hint: stored.location,
        read: stored.location,
        hosted: None,
    }))
}

/// What [`way_to`] finds for one element.
enum Sought {
    /// The element is held, with this put again.
    Held(Way),
    /// The element is not held.
    No,
    /// The element is the root of a Merk that has a host, and its cell
    /// could grow: it is held only where the element of that Merk is
    /// held, which it is not yet.
    Above,
}

/// What is put again for one element ([`way_to`]).
struct Way {
    /// The node above the node of the element, where neither the batch
    /// nor an element that is held already puts it, and then the node of
    /// the element, where no element that is held already puts it.
    nodes: Vec<NodeWrite>,
    /// Whether the node of the element is the root of its Merk.
    is_root: bool,
    /// Whether that Merk has a host itself.
    hosted: bool,
}

/// What is put again so that a write holds the element `host`: the node
/// of the element and the node above it in its Merk, each as the
/// transaction holds it ([`as_it_is`]). The way from the root of that
/// Merk to the element is the one `batch` leaves: through a node that
/// the batch puts, or that `again` holds, it follows the links stated
/// there, and through every other node the links the transaction holds.
/// The element is looked for on account of the Merk `below`, which it
/// hosts. `touched` is `None` for the naming of a Merk that was handed
/// over, and for a batch of a group what the group touched: the node
/// above is left out where it is one of those. See [`held_elements`] for
/// where the element is not held.
fn way_to(
    reads: &impl Reads,
    batch: &Batch<'_>,
    host: &LigninHost,
    below: Subtree,
    again: &[PutAgain],
    touched: Option<&[(Subtree, Vec<u8>)]>,
) -> Result<Sought, Error> {
    let above = Subtree::new(host.subtree);
    // Whether the cell of the node names every Merk that the element is
    // said to host already: each is entered under the element at its
    // place before the batch and has a root. A cell that is put again
    // with those Merks named keeps its length.
    let mut names_already = true;
    // Every Merk of the batch that the element hosts, and the Merk it
    // is looked for on account of, is one that the node can name: left
    // where it is, and entered under the element at its place in the
    // list, or entered so with the batch. One of the batch is anchored
    // and no load.
    for (position, hosted) in host.hosted.iter().enumerate() {
        let hosted = Subtree::new(*hosted);
        let merk = batch.merks.iter().find(|merk| merk.subtree == hosted);
        let entered = read_subtree(reads, hosted.as_bytes())?;
        names_already &= entered.as_ref().is_some_and(|entry| {
            let here = |at: &LigninHosted| {
                at.subtree == host.subtree
                    && at.key == host.key
                    && usize::from(at.position) == position
            };
            entry.root_key.is_some() && entry.host.as_ref().is_some_and(here)
        });
        if merk.is_none() && hosted != below {
            continue;
        }
        if merk.is_some_and(|merk| !merk.anchored || merk.whole) || taken(batch, hosted) {
            return Ok(Sought::No);
        }
        let entered = entered.map(|entry| {
            (entry.host).map(|at| (Subtree::new(at.subtree), at.key, usize::from(at.position)))
        });
        let stated = |write: &SubtreeWrite<'_>| {
            let enter = write.entering().filter(|enter| enter.subtree == hosted)?;
            Some((enter.host).map(|at| (at.subtree, at.key.to_vec(), usize::from(at.position))))
        };
        let under = match entered {
            Some(under) => under,
            None => batch.subtrees.iter().find_map(stated).flatten(),
        };
        if under != Some((above, host.key.clone(), position)) {
            return Ok(Sought::No);
        }
    }
    if !host.hosted.contains(below.as_bytes()) || taken(batch, above) {
        return Ok(Sought::No);
    }
    let own = batch.merks.iter().find(|merk| merk.subtree == above);
    let Some(entry) = read_subtree(reads, above.as_bytes())? else {
        return Ok(Sought::No);
    };
    let root = match own.map(|merk| merk.root) {
        Some(RootPut::Set(root)) => root.map(<[u8]>::to_vec),
        Some(RootPut::Keep) | None => entry.root_key,
    };
    let Some(mut at) = root else {
        return Ok(Sought::No);
    };
    let is_root = at == host.key;
    // The node above the one the way has come to, with whether the
    // write puts it without this.
    let mut parent: Option<(Vec<u8>, bool)> = None;
    for _ in 0..LONGEST_WAY {
        let put = own.and_then(|merk| merk.nodes.iter().find(|node| node.key == at));
        let held = (again.iter())
            .find(|held| held.subtree == above && held.node.key == at)
            .map(|held| &held.node);
        let left = match host.key.as_slice().cmp(&at) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            // A batch that puts the node of the element names the Merks
            // in it itself.
            std::cmp::Ordering::Equal if put.is_some() => return Ok(Sought::No),
            std::cmp::Ordering::Equal => {
                // The node above, where the write does not put it without
                // this. One that hosts a subtree which was touched is not
                // put again, and the element is held without it.
                let topmost = parent.is_none();
                let unheld = parent.filter(|(_, put)| !put).map(|(key, _)| key);
                let left_out = (unheld.iter()).any(|key| {
                    touched.is_some_and(|touched| touched.contains(&(above, key.clone())))
                });
                // In a batch of a group, a cell that could grow is held
                // only where the write can carry it: with the node above
                // it, or as the root of a Merk that no element hosts, or
                // whose element is held. A naming is there to grow the
                // cell of its element.
                if touched.is_some() && !names_already {
                    let named = |held: &PutAgain| {
                        let hosted = held.hosted.as_ref();
                        hosted.is_some_and(|hosted| hosted.contains(&above))
                    };
                    match topmost {
                        false if left_out => return Ok(Sought::No),
                        true if entry.host.is_some() && !again.iter().any(named) => {
                            return Ok(Sought::Above)
                        }
                        _ => {}
                    }
                }
                let unheld = unheld.filter(|_| !left_out);
                let mut nodes = Vec::with_capacity(2);
                let own = held.is_none().then_some(at);
                for key in unheld.into_iter().chain(own) {
                    let Some(node) = as_it_is(reads, above, &key)? else {
                        return Ok(Sought::No);
                    };
                    nodes.push(node);
                }
                return Ok(Sought::Held(Way {
                    nodes,
                    is_root,
                    hosted: entry.host.is_some(),
                }));
            }
        };
        let next = match (put, held) {
            (Some(put), _) => {
                let link = if left { put.left } else { put.right };
                link.map(|link| link.key.to_vec())
            }
            (None, Some(held)) => {
                let link = if left { &held.left } else { &held.right };
                link.as_ref().map(|link| link.key.clone())
            }
            (None, None) => match linked(reads, above, &at, [left, !left])? {
                Some((_, [on_the_left, on_the_right])) => match left {
                    true => on_the_left,
                    false => on_the_right,
                },
                None => None,
            },
        };
        let Some(next) = next else {
            return Ok(Sought::No);
        };
        parent = Some((at, put.is_some() || held.is_some()));
        at = next;
    }
    Ok(Sought::No)
}

/// What a write puts again so that it holds the element of each Merk of
/// `first`, as `host_of` names it, and what that takes above it.
///
/// A write that puts nodes of a Merk and not the node of the element
/// that hosts it hands the Merk over anchored: its root stays in its
/// page (`tree-layout.md` T3), and where the root's cell grows in a page
/// that has no room for it and holds nothing below the root, the tree
/// pages refuse the write
/// (`lignin_tree::fragment::FragmentError::NoRoom`). A write of the node
/// of an element alone is in the same place, where the node grows by a
/// Merk that it comes to name: the tree pages carry a cell only for a
/// write that holds the cell which points at it (F4). So for each Merk
/// of `first` the write puts again, as the transaction holds them, the
/// node of the element that hosts the Merk and the node above it in its
/// Merk, but for one that the write puts already, and names in the
/// element's node the Merks the element hosts. A node above that hosts a
/// subtree which the batch touches is not put again either, and the
/// element is held without it: see below. No value is written and
/// no hash is made: each node keeps its value and states the feature,
/// the hashes and the links that were read. The Merks of `batch` that
/// the element hosts are then anchored no more, and every cell of the
/// write that can grow is one the tree pages may carry: what tops the
/// nodes that are put again is the node above the element as it is,
/// which does not grow. An element whose node is the root of its Merk
/// has no node above it: where that Merk has a host, the element of that
/// Merk is held as well, in the same way, if `host_of` names it.
///
/// The node above an element is put again as a node that is only above:
/// it keeps the Merks it hosts as its cell has them, and the tree pages
/// refuse a cell that keeps a root which the same write takes away
/// (`lignin_tree::fragment::WriteError::Removed`). Such a node is held
/// as an element itself where a batch of a group puts or removes a node
/// of the Merk it hosts ([`held_by_a_part`]), and then names what it
/// hosts. Where it is not, and it is one of `touched`, the nodes that
/// host a subtree which the batch or an earlier batch of its group
/// touches ([`hosts_of_touched`]), it is left out. The element below it
/// is then held without it where its cell is taken to keep its length,
/// and not held where it is not (below): held so, the Merks of that
/// element are anchored no more, and the cell of the element stays in
/// its page.
///
/// A cell that stays in its page must not grow, as a cell does, by the
/// place of a pointer, where its node comes to name a Merk that it does
/// not name yet. So in a batch of a group, where `touched` is given, an
/// element whose cell could grow is held only where the write can carry
/// the cell: with the node above it, or as the root of a Merk that no
/// element hosts, or whose element is held, which is looked for first
/// then. The naming of a Merk that was handed over is there to grow the
/// cell of its element, and holds the element whatever its cell does:
/// `touched` is `None` for it. The cell is taken to keep its length
/// where every Merk that the element is said to host is entered under
/// it at its place before the batch and has a root there. A cell names
/// such a Merk in a grove that one writer's operations of GroveDB made,
/// each of which names a Merk that has a root in the node of its
/// element before it ends, a restore and a load by the naming after the
/// hand-over. A Merk that is entered with no node can be one that the
/// node of its element does not name. And two writers can leave a root
/// that no cell names: a commit that writes into a tree after the look
/// of a group which writes an empty tree over it gives the Merks of
/// that tree roots under an element that names none (`engine.md`, "An
/// element written over a tree"). The checker reports such a store, and
/// an element of it that is held without the node above it can grow.
///
/// An element is held only where all of this is so, and its Merks stay
/// anchored otherwise:
///
/// - each Merk of the batch that the element hosts is anchored and no
///   load, and it and the Merk the element is held for are entered
///   under the element at their places among the Merks the element
///   hosts, or are entered so with the batch, and the batch neither
///   drops, clears nor moves them
/// - the Merk that holds the element is entered, has a root as the
///   batch leaves it, and the batch neither drops, clears nor moves it
/// - the way from that root to the element's key, by the links the
///   batch states for the nodes it puts and by the links the
///   transaction holds for the others, comes to a node under that key
///   which the batch does not put, and that node and the node above it
///   are nodes of the tree with no stale hash. A node that the batch
///   removes is on no such way
/// - in a batch of a group, the write can carry the cell of the
///   element, or every Merk that the element is said to host is entered
///   under it at its place before the batch and has a root
fn held_elements(
    reads: &impl Reads,
    batch: &Batch<'_>,
    first: impl IntoIterator<Item = Subtree>,
    host_of: &dyn Fn(Subtree) -> Option<LigninHost>,
    touched: Option<&[(Subtree, Vec<u8>)]>,
) -> Result<HeldElements, Error> {
    let mut held = HeldElements::default();
    // The Merks whose elements are looked for: the first ones, and then
    // each Merk whose root is the node of an element that is held.
    let mut merks: VecDeque<Subtree> = first.into_iter().collect();
    // The elements that waited once for the element above them.
    let mut waited: Vec<(Subtree, Vec<u8>)> = Vec::new();
    while let Some(merk) = merks.pop_front() {
        let Some(host) = host_of(merk) else {
            continue;
        };
        let above = Subtree::new(host.subtree);
        let element = (above, host.key.clone());
        if held.looked.contains(&element) {
            continue;
        }
        let sought = way_to(reads, batch, &host, merk, &held.again, touched)?;
        // The root of a Merk that has a host, whose cell could grow: the
        // element of that Merk is looked for first, and then this one
        // once more.
        if matches!(sought, Sought::Above) && !waited.contains(&element) {
            waited.push(element);
            merks.push_front(merk);
            merks.push_front(above);
            continue;
        }
        held.looked.push(element);
        let Sought::Held(way) = sought else {
            continue;
        };
        let again = way.nodes.into_iter().map(|node| PutAgain {
            subtree: above,
            node,
            hosted: None,
        });
        held.again.extend(again);
        let hosted: Vec<Subtree> = host.hosted.iter().copied().map(Subtree::new).collect();
        let places = (0..batch.merks.len()).filter(|at| hosted.contains(&batch.merks[*at].subtree));
        held.named.extend(places);
        for (added, anchored) in &mut held.added {
            *anchored &= !hosted.contains(added);
        }
        let node = (held.again.iter_mut())
            .find(|again| again.subtree == above && again.node.key == host.key)
            .ok_or_else(|| refused("the node of a held element was not read"))?;
        node.hosted = Some(hosted);
        let written = batch.merks.iter().any(|merk| merk.subtree == above);
        if !written && !held.added.iter().any(|(added, _)| *added == above) {
            // Anchored where it has a host that is not held.
            let named = |held: &PutAgain| {
                let hosted = held.hosted.as_ref();
                hosted.is_some_and(|hosted| hosted.contains(&above))
            };
            held.added
                .push((above, way.hosted && !held.again.iter().any(named)));
        }
        if way.is_root && way.hosted {
            merks.push_back(above);
        }
    }
    Ok(held)
}

/// The nodes that host a subtree which `batch` touches: one that it has
/// a change set of, or that it enters, drops, clears, states anew or
/// moves, with the subtrees a move takes along. By the entry that the
/// transaction holds of the subtree before the batch is applied, and by
/// the host that an enter or a move of the batch states.
fn hosts_of_touched(
    reads: &impl Reads,
    batch: &Batch<'_>,
) -> Result<Vec<(Subtree, Vec<u8>)>, Error> {
    let mut subtrees: Vec<Subtree> = batch.merks.iter().map(|merk| merk.subtree).collect();
    let mut hosts = Vec::new();
    for write in batch.subtrees {
        match write.moving() {
            Some(moving) => {
                let moved = (moving.from, moving.to);
                for (from, to) in std::iter::once(&moved).chain(moving.below) {
                    subtrees.extend([*from, *to]);
                }
                hosts.push((moving.host.subtree, moving.host.key.to_vec()));
            }
            None => subtrees.push(write.subtree()),
        }
        if let Some(host) = write.entering().and_then(|enter| enter.host) {
            hosts.push((host.subtree, host.key.to_vec()));
        }
    }
    for subtree in subtrees {
        let entry = read_subtree(reads, subtree.as_bytes())?;
        if let Some(host) = entry.and_then(|entry| entry.host) {
            hosts.push((Subtree::new(host.subtree), host.key));
        }
    }
    Ok(hosts)
}

impl HeldElements {
    /// Calls `run` with `batch` and the nodes that are put again: those
    /// of a Merk that the batch writes after the nodes the batch puts
    /// there, and the Merks that the batch does not write after its
    /// Merks, each with its root kept. What `run` reports is cut back to
    /// what is of the batch alone.
    fn with_batch(
        &self,
        batch: &Batch<'_>,
        run: &mut dyn FnMut(&Batch<'_>) -> Result<Written, Error>,
    ) -> Result<Written, Error> {
        if self.again.is_empty() {
            return run(batch);
        }
        let puts: Vec<(Subtree, NodePut<'_>)> = (self.again.iter())
            .map(|again| {
                let put = NodePut {
                    hosted: match &again.hosted {
                        Some(hosted) => Hosted::Set(hosted),
                        None => Hosted::Keep,
                    },
                    ..node_put(&again.node)
                };
                (again.subtree, put)
            })
            .collect();
        let of = |subtree: Subtree| {
            let of = puts.iter().filter(move |(of, _)| *of == subtree);
            of.map(|(_, put)| *put)
        };
        let nodes: Vec<Vec<NodePut<'_>>> = (batch.merks.iter())
            .map(|merk| merk.nodes.iter().copied().chain(of(merk.subtree)).collect())
            .collect();
        let added: Vec<Vec<NodePut<'_>>> = (self.added.iter())
            .map(|(subtree, _)| of(*subtree).collect())
            .collect();
        let own =
            (batch.merks.iter().zip(&nodes).enumerate()).map(|(place, (merk, nodes))| MerkWrite {
                nodes,
                anchored: merk.anchored && !self.named.contains(&place),
                ..*merk
            });
        let above = (self.added.iter().zip(&added)).map(|((subtree, anchored), nodes)| MerkWrite {
            subtree: *subtree,
            nodes,
            removed: &[],
            root: RootPut::Keep,
            anchored: *anchored,
            whole: false,
        });
        let merks: Vec<MerkWrite<'_>> = own.chain(above).collect();
        let holding = Batch {
            merks: &merks,
            ..*batch
        };
        let mut written = run(&holding)?;
        written.locations.truncate(batch.merks.len());
        for (locations, merk) in written.locations.iter_mut().zip(batch.merks) {
            locations.truncate(merk.nodes.len());
        }
        Ok(written)
    }
}

/// What `part` holds as one batch of a group of several
/// ([`held_elements`]): the element of each Merk of `batch` that is
/// anchored and no load, and that the batch puts or removes a node of,
/// as the part names it ([`BatchPart::host_of`]). A Merk that the batch
/// only removes nodes of can get another root or none, which the node of
/// its element then names with the same write; and its element can be
/// the node above another element that is held. `touched` is what the
/// batches of the group touch up to this one, with it
/// ([`hosts_of_touched`]).
fn held_by_a_part(
    reads: &impl Reads,
    batch: &Batch<'_>,
    part: &dyn BatchPart,
    touched: &[(Subtree, Vec<u8>)],
) -> Result<HeldElements, Error> {
    let written = |merk: &&MerkWrite<'_>| {
        merk.anchored && !merk.whole && !(merk.nodes.is_empty() && merk.removed.is_empty())
    };
    let first = batch.merks.iter().filter(written).map(|merk| merk.subtree);
    held_elements(
        reads,
        batch,
        first,
        &|merk| part.host_of(merk),
        Some(touched),
    )
}

/// Names in the node of the first element of `hosts` the Merks that the
/// element hosts, through `scope`, as a write that holds the element
/// ([`held_elements`]): the node is put again as the transaction holds
/// it, with the node above it. The elements after the first are those of
/// the Merks above, each named where the element below it is the root of
/// its Merk. An element that is not held is refused, and nothing is
/// written.
fn name_hosted(scope: &Scope<'_, '_>, hosts: &[LigninHost]) -> Result<(), Error> {
    let unheld = || {
        refused("a host that is no node of its Merk, or whose first Merk is not entered under it")
    };
    let element = hosts.first().ok_or_else(unheld)?;
    let below = element.hosted.first().ok_or_else(unheld)?;
    let host_of = |merk: Subtree| {
        let mut hosts = hosts.iter();
        hosts
            .find(|host| host.hosted.contains(merk.as_bytes()))
            .cloned()
    };
    let batch = Batch::default();
    let held = held_elements(scope, &batch, [Subtree::new(*below)], &host_of, None)?;
    if held.again.is_empty() {
        return Err(unheld());
    }
    let named = held.with_batch(&batch, &mut |batch| {
        apply_batch(scope, batch, &[], &mut None)
    });
    named.map(|_| ())
}

/// Applies the batches of `parts` through `scope`, in their order, each
/// with the subtrees of `enters` that are entered or stated anew with it.
/// In a group of more than one batch, each batch holds the element of
/// each Merk whose root can grow where it is ([`held_by_a_part`]).
fn apply_parts(
    scope: &Scope<'_, '_>,
    parts: &[&dyn BatchPart],
    enters: &[(usize, SubtreeChange)],
    entry: &mut Option<LigninEntryRefused>,
) -> Result<Vec<Written>, Error> {
    if enters.iter().any(|(part, _)| *part >= parts.len()) {
        return Err(refused("subtrees were to be entered with no batch"));
    }
    let several = parts.len() > 1;
    // The nodes that host a subtree which a batch of the group touched.
    let mut touched = Vec::new();
    let mut written = Vec::with_capacity(parts.len());
    for (at, part) in parts.iter().enumerate() {
        let enters: Vec<SubtreeWrite<'_>> = (enters.iter())
            .filter(|(part, _)| *part == at)
            .map(|(_, change)| subtree_write(change))
            .collect();
        written.push(part.with_batch(&mut |batch| {
            let subtrees = match enters.is_empty() {
                true => batch.subtrees.to_vec(),
                false => with_enters(batch.subtrees, &enters),
            };
            let batch = Batch {
                subtrees: &subtrees,
                ..*batch
            };
            let held = match several {
                true => {
                    touched.extend(hosts_of_touched(scope, &batch)?);
                    held_by_a_part(scope, &batch, *part, &touched)?
                }
                false => HeldElements::default(),
            };
            held.with_batch(&batch, &mut |batch| apply_batch(scope, batch, &[], entry))
        })?);
    }
    Ok(written)
}

/// Shows `run` what `part` hands a transaction as one batch of a group of
/// several, with what it puts again ([`held_by_a_part`]) as `reader`
/// reads it, and returns what `run` reports as the part is told it.
/// Nothing is applied.
#[cfg(test)]
pub(super) fn shown_in_a_group<R: Reader>(
    reader: &R,
    part: &dyn BatchPart,
    run: &mut dyn FnMut(&Batch<'_>) -> Written,
) -> Result<Written, Error> {
    reader.reading(|reads| {
        part.with_batch(&mut |batch| {
            let touched = hosts_of_touched(reads, batch)?;
            let held = held_by_a_part(reads, batch, part, &touched)?;
            held.with_batch(batch, &mut |batch| Ok(run(batch)))
        })
    })
}

/// The subtree writes of a batch with `enters` among them: what leaves
/// and what is moved first, as the batch has it, then what is stated
/// anew, and then the enters by how many subtrees above each are entered
/// here too, so that a subtree comes after the one its host is in, and in
/// the order they came where that number is the same.
fn with_enters<'a>(own: &[SubtreeWrite<'a>], enters: &[SubtreeWrite<'a>]) -> Vec<SubtreeWrite<'a>> {
    let (mut ordered, entered): (Vec<_>, Vec<_>) =
        (own.iter().chain(enters).copied()).partition(|write| !write.enters());
    // The subtree that the host of an entered subtree is in, if that one
    // is entered here too.
    let above = |subtree: &Subtree| {
        let host = entered.iter().find_map(|write| match write.entering() {
            Some(enter) if enter.subtree == *subtree => enter.host.map(|host| host.subtree),
            _ => None,
        })?;
        let enters_it = |write: &SubtreeWrite<'_>| write.subtree() == host;
        entered.iter().any(enters_it).then_some(host)
    };
    let depth = |write: &SubtreeWrite<'_>| match write.entering() {
        Some(enter) => {
            let hosts = std::iter::successors(Some(enter.subtree), above);
            hosts.take(entered.len()).count()
        }
        None => 0,
    };
    let mut by_depth: Vec<_> = entered.iter().map(|write| (depth(write), write)).collect();
    by_depth.sort_by_key(|(depth, _)| *depth);
    ordered.extend(by_depth.into_iter().map(|(_, write)| *write));
    ordered
}

/// One writer's part of a LigninDB write transaction: the scope it writes
/// through, shared by the handles of the Merks it has open.
///
/// A scope is one thread's at a time, so it is sent and not shared
/// (`transactions.md` SC1). The handles of a writer's Merks all read and
/// write through its one scope, and a Merk on a handle has to be `Send`.
/// So the scope is kept behind a lock here: a handle holds a reference to
/// the writer, and takes the lock for the length of each read and each
/// application. One thread uses a writer at a time, so the lock is never
/// waited for.
pub struct LigninWriter<'t, 's> {
    scope: Mutex<Scope<'t, 's>>,
}

impl<'t, 's> LigninWriter<'t, 's> {
    /// A writer that reads and writes through `scope`.
    pub fn new(scope: Scope<'t, 's>) -> Self {
        Self {
            scope: Mutex::new(scope),
        }
    }

    /// The scope, to merge it into its transaction or to roll it back.
    pub fn into_scope(self) -> Scope<'t, 's> {
        self.scope
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn scope(&self) -> MutexGuard<'_, Scope<'t, 's>> {
        // A scope holds no state of its own that a panic could leave half
        // made: the transaction puts its frames right by itself (WT10).
        self.scope.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for LigninWriter<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LigninWriter").finish_non_exhaustive()
    }
}

/// What reads for an [`Access`], and how it takes a write.
pub(super) trait Reader: Sync {
    type Reads: Reads;

    fn reading<T>(&self, read: impl FnOnce(&Self::Reads) -> T) -> T;

    /// Makes the writes of `write` through a scope: all of them or none.
    fn writing<T>(
        &self,
        parts: usize,
        write: impl FnOnce(&Scope<'_, '_>) -> Result<T, Error>,
    ) -> Result<T, Error>;

    fn walk<'a>(&'a self, subtree: Subtree) -> Result<Box<dyn Walk + 'a>, TxnError>;
}

impl<'t, 's> Reader for LigninWriter<'t, 's> {
    type Reads = Scope<'t, 's>;

    fn reading<T>(&self, read: impl FnOnce(&Self::Reads) -> T) -> T {
        read(&self.scope())
    }

    fn writing<T>(
        &self,
        parts: usize,
        write: impl FnOnce(&Scope<'_, '_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // The scope is the writer's, and lives on: it is not rolled back
        // for one write that failed, so only what is one operation of the
        // transaction is all or nothing through it (WT10).
        if parts > 1 {
            return Err(Error::NotSupported(
                "lignindb: batches that were settled apart are not applied together through a \
                 write scope"
                    .to_owned(),
            ));
        }
        write(&self.scope())
    }

    fn walk<'a>(&'a self, subtree: Subtree) -> Result<Box<dyn Walk + 'a>, TxnError> {
        let scope = self.scope();
        let parked = scope.parked_merk_cursor_by_kind(subtree, keeps_flat_records)?;
        Ok(Box::new(ScopeWalk {
            writer: self,
            parked,
        }))
    }
}

/// A write transaction read outside any scope, and written through a scope
/// that is opened for one write and merged when it is made
/// (`transactions.md` SC1): what the handles of a transaction that has no
/// writer of its own are opened on. No scope of it is open between two
/// writes, so a savepoint, a rollback and the commit find the transaction
/// with every scope closed (SC8).
impl Reader for WriteTxn<'_> {
    type Reads = Self;

    fn reading<T>(&self, read: impl FnOnce(&Self::Reads) -> T) -> T {
        read(self)
    }

    fn writing<T>(
        &self,
        _parts: usize,
        write: impl FnOnce(&Scope<'_, '_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let scope = self.scope().map_err(fault)?;
        match write(&scope) {
            Ok(written) => {
                scope.merge();
                Ok(written)
            }
            Err(error) => {
                scope.rollback();
                Err(error)
            }
        }
    }

    fn walk<'a>(&'a self, subtree: Subtree) -> Result<Box<dyn Walk + 'a>, TxnError> {
        let cursor = self.merk_cursor_by_kind(subtree, keeps_flat_records)?;
        Ok(Box::new(TxnWalk(cursor)))
    }
}

impl<A: Reader + super::flat::FlatAccess> Access for A {
    fn cursor<'a>(&'a self, subtree: &Prefix) -> Result<Box<dyn Walk + 'a>, Error> {
        self.walk(Subtree::new(*subtree)).map_err(fault)
    }

    fn node(
        &self,
        subtree: &Prefix,
        key: &[u8],
        hint: Option<Location>,
        with_value: bool,
    ) -> Result<Option<StoredNode>, Error> {
        self.reading(|reads| read_node(reads, subtree, key, hint, with_value))
    }

    fn committed_node(&self, subtree: &Prefix, key: &[u8]) -> Result<Option<StoredNode>, Error> {
        self.reading(|reads| read_committed_node(reads, subtree, key))
    }

    fn child(&self, subtree: &Prefix, parent: Location, left: bool) -> Result<Child, Error> {
        self.reading(|reads| read_child(reads, subtree, parent, left))
    }

    fn subtree(&self, subtree: &Prefix) -> Result<Option<LigninEntry>, Error> {
        self.reading(|reads| read_subtree(reads, subtree))
    }

    fn record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.reading(|reads| read_record(reads, keyspace, subtree, key, false))
    }

    fn committed_record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.reading(|reads| read_record(reads, keyspace, subtree, key, true))
    }

    fn apply(&self, write: &Write) -> Result<Written, Error> {
        self.writing(1, |scope| apply(scope, write))
    }

    fn apply_parts(
        &self,
        parts: &[&dyn BatchPart],
        enters: &[(usize, SubtreeChange)],
        entry: &mut Option<LigninEntryRefused>,
    ) -> Result<Vec<Written>, Error> {
        self.writing(parts.len(), |scope| {
            apply_parts(scope, parts, enters, entry)
        })
    }

    fn name_hosted(&self, hosts: &[LigninHost]) -> Result<(), Error> {
        self.writing(1, |scope| name_hosted(scope, hosts))
    }
}
