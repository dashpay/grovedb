//! Where the store meets the write transaction of LigninDB.
//!
//! Everything this store asks of `lignindb::txn` is asked here and nowhere
//! else: the reads of a node view, the batch a change set becomes, and the
//! report its application gives back. The rest of the store speaks of nodes
//! in Merk's terms ([`StoredNode`], [`Write`]) and reaches a transaction
//! through [`Access`].

use std::sync::{Mutex, MutexGuard, PoisonError};

use grovedb_storage::{error::WriteScopeError, Error as StorageError};
use lignin_format::tree_page::{HashField, Side};
use lignin_tree::{directory::DirectoryError, hash::Stale, node::NodeHandle, value::ValueBuf};
use lignindb::txn::{
    Batch, ChildKeys, Column, HostPut, Hosted, LinkPut, LinkView, MerkCursor, MerkSide, MerkWrite,
    NodePut, NodeView, ParkedCursor, RecordWrite, Resumed, RootPut, Scope, Subtree, SubtreeView,
    SubtreeWrite, TxnError, Unit, ValuePut, WriteTxn,
};

use super::{
    change_set::node_put,
    feature::{decode_aggregate, decode_feature, Tp12},
    refused, LigninEntry, LigninHosted,
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
    /// write set has no cell.
    pub(super) location: Option<Location>,
}

/// What following a link from a parent's cell found.
#[derive(Debug, PartialEq)]
pub(super) enum Child {
    /// The parent's location is no longer good: the parent is read again.
    Stale,
    /// The link leads to no node.
    Missing,
    /// The child, with where its cell is.
    Found {
        key: Vec<u8>,
        location: Option<Location>,
    },
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
    /// the order they were given. Empty for a transaction that serves its
    /// records by key and has no cells.
    pub(super) locations: Vec<Vec<Location>>,
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

    /// The node under the key the cursor stands on, with its value and
    /// with the key of each child in its link. `None` if the transaction
    /// has deleted the node since the cursor came to its key.
    fn node(&mut self) -> Result<Option<StoredNode>, Error>;

    /// Points the cursor at the Merk of `subtree`, in the committed state
    /// it has read from since it was opened. It stands on no node then.
    ///
    /// A cursor of a snapshot does this. A cursor of a write transaction
    /// is refused: the transaction has no way to point the committed side
    /// of a cursor at another Merk under the pin it holds.
    fn retarget(&mut self, _subtree: &Prefix) -> Result<(), Error> {
        Err(Error::NotSupported(
            "lignindb: a cursor of a write transaction is not pointed at another subtree"
                .to_owned(),
        ))
    }
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
    /// subtrees that are entered with them, each with the part it is
    /// entered by, besides those the part enters itself.
    fn apply_parts(
        &self,
        parts: &[&dyn BatchPart],
        enters: &[(usize, SubtreeChange)],
    ) -> Result<Vec<Written>, Error>;
}

/// A batch that is handed to a transaction as it stands when it is asked
/// for: the batch of an operation, resolved.
pub(super) trait BatchPart {
    /// Calls `run` with the batch, and returns what it returns.
    fn with_batch(
        &self,
        run: &mut dyn FnMut(&Batch<'_>) -> Result<Written, Error>,
    ) -> Result<Written, Error>;
}

/// A read or a write the transaction did not perform. Every failure of the
/// engine is reported as a storage error and under no other
/// (`node-store.md`, "Errors"), but for one: a subtree that is entered
/// more Merks deep than the subtree directory takes. That is a limit of
/// the engine which the write met and no fault of the store, and a caller
/// tells it from damage by its error.
pub(super) fn fault(error: TxnError) -> Error {
    match error {
        TxnError::Directory(DirectoryError::TooDeep) => {
            Error::NotSupported(format!("lignindb: {error}"))
        }
        // A claim of another open scope is what stood in the way, and
        // is named so through every seam (`engine.md`, "Write scopes").
        TxnError::ScopeOverlap { .. } => {
            Error::StorageError(StorageError::WriteScope(WriteScopeError::Overlap))
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

    fn node(&mut self) -> Result<Option<StoredNode>, Error> {
        node_under(&mut self.0)
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

    fn node(&mut self) -> Result<Option<StoredNode>, Error> {
        self.resumed(node_resumed)
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
}

macro_rules! reads {
    ($reader:ty) => {
        impl Reads for $reader {
            fn node<R>(
                &self,
                subtree: Subtree,
                key: &[u8],
                hint: Option<NodeHandle>,
                read: impl FnOnce(Option<NodeView<'_>>) -> R,
            ) -> Result<R, TxnError> {
                match hint {
                    Some(handle) => self.follow(subtree, handle, key, read),
                    None => self.node(subtree, key, read),
                }
            }

            fn committed_node<R>(
                &self,
                subtree: Subtree,
                key: &[u8],
                read: impl FnOnce(Option<NodeView<'_>>) -> R,
            ) -> Result<R, TxnError> {
                self.committed_node(subtree, key, read)
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

reads!(WriteTxn<'_>);
reads!(Scope<'_, '_>);

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
        } => SubtreeWrite::Enter {
            subtree: Subtree::new(*subtree),
            tree_type,
            host: host.as_ref().map(|(subtree, key, position)| HostPut {
                subtree: Subtree::new(*subtree),
                key,
                position: *position,
            }),
        },
    }
}

/// Applies `batch` to the transaction of `scope` as one operation.
///
/// A stated value that the transaction holds already is kept where it is,
/// so that a value is written only when it changed, and a hash that is
/// left stale keeps the bytes it has. A node that keeps its value names
/// the read the value is kept from: the store's own read of the node
/// where Merk says the value is unchanged, and the comparing read here,
/// which is of exactly the bytes that are kept. A Merk whose subtree the
/// batch enters has no node before the write, whatever the transaction
/// holds under its prefix until then: nothing of it is read.
pub(super) fn apply_batch(scope: &Scope<'_, '_>, batch: &Batch<'_>) -> Result<Written, Error> {
    let mut written = Written::default();
    let fresh = |subtree: Subtree| {
        let enters = |write: &SubtreeWrite<'_>| write.enters() && write.subtree() == subtree;
        batch.subtrees.iter().any(enters)
    };
    let mut keep = Vec::with_capacity(batch.merks.len());
    for merk in batch.merks {
        let fresh = fresh(merk.subtree);
        let mut kept = Vec::with_capacity(merk.nodes.len());
        for node in merk.nodes {
            let held = match fresh {
                true => Held::default(),
                false => held(scope, merk.subtree, node)?,
            };
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
    let applied = scope
        .apply(&Batch {
            merks: &merks,
            ..*batch
        })
        .map_err(fault)?;

    // The handles come in the order of the batch: the nodes of the first
    // Merk, then those of the second. A transaction that serves its records
    // by key reports none.
    let mut handles = applied.nodes.into_iter().map(Location);
    written.locations = (batch.merks.iter())
        .map(|merk| handles.by_ref().take(merk.nodes.len()).collect())
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
        })
        .collect();
    let records: Vec<RecordWrite<'_>> = (write.records.iter())
        .map(|record| {
            let subtree = Subtree::new(record.subtree);
            match &record.value {
                Some(value) => {
                    RecordWrite::put(column(record.keyspace), subtree, &record.key, value)
                }
                None => RecordWrite::delete(column(record.keyspace), subtree, &record.key),
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
    )
}

/// Applies the batches of `parts` through `scope`, in their order, each
/// with the subtrees of `enters` that are entered by it.
fn apply_parts(
    scope: &Scope<'_, '_>,
    parts: &[&dyn BatchPart],
    enters: &[(usize, SubtreeChange)],
) -> Result<Vec<Written>, Error> {
    if enters.iter().any(|(part, _)| *part >= parts.len()) {
        return Err(refused("subtrees were to be entered with no batch"));
    }
    let mut written = Vec::with_capacity(parts.len());
    for (at, part) in parts.iter().enumerate() {
        let enters: Vec<SubtreeWrite<'_>> = (enters.iter())
            .filter(|(part, _)| *part == at)
            .map(|(_, change)| subtree_write(change))
            .collect();
        written.push(part.with_batch(&mut |batch| {
            if enters.is_empty() {
                return apply_batch(scope, batch);
            }
            let subtrees = with_enters(batch.subtrees, &enters);
            apply_batch(
                scope,
                &Batch {
                    subtrees: &subtrees,
                    ..*batch
                },
            )
        })?);
    }
    Ok(written)
}

/// The subtree writes of a batch with `enters` among them: what leaves
/// first, as the batch has it, and then every enter after the enter of the
/// subtree its host is in, and otherwise in the order they came.
fn with_enters<'a>(own: &[SubtreeWrite<'a>], enters: &[SubtreeWrite<'a>]) -> Vec<SubtreeWrite<'a>> {
    let (mut ordered, entered): (Vec<_>, Vec<_>) =
        (own.iter().chain(enters).copied()).partition(|write| !write.enters());
    // The subtree that the host of an entered subtree is in, if that one
    // is entered here too.
    let above = |subtree: &Subtree| {
        let host = entered.iter().find_map(|write| match write {
            SubtreeWrite::Enter {
                subtree: entered,
                host,
                ..
            } if entered == subtree => host.as_ref().map(|host| host.subtree),
            _ => None,
        })?;
        let enters_it = |write: &SubtreeWrite<'_>| matches!(write, SubtreeWrite::Enter { subtree, .. } if *subtree == host);
        entered.iter().any(enters_it).then_some(host)
    };
    let depth = |write: &SubtreeWrite<'_>| match write {
        SubtreeWrite::Enter { subtree, .. } => {
            let hosts = std::iter::successors(Some(*subtree), above);
            hosts.take(entered.len()).count()
        }
        SubtreeWrite::Drop { .. } | SubtreeWrite::Clear { .. } => 0,
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
        let parked = self.scope().parked_merk_cursor(subtree)?;
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
        Ok(Box::new(TxnWalk(self.merk_cursor(subtree)?)))
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
    ) -> Result<Vec<Written>, Error> {
        self.writing(parts.len(), |scope| apply_parts(scope, parts, enters))
    }
}
