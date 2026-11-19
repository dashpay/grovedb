//! Where the store meets the write transaction of LigninDB.
//!
//! Everything this store asks of `lignindb::txn` is asked here and nowhere
//! else: the reads of a node view, the batch a change set becomes, and the
//! report its application gives back. The rest of the store speaks of nodes
//! in Merk's terms ([`StoredNode`], [`Write`]) and reaches a transaction
//! through [`Access`].

use std::sync::{Mutex, MutexGuard, PoisonError};

use lignin_format::tree_page::{HashField, Side};
use lignin_tree::{directory::DirectoryError, hash::Stale, node::NodeHandle, value::ValueBuf};
use lignindb::txn::{
    Batch, ChildKeys, Column, HostPut, Hosted, LinkPut, LinkView, MerkCursor, MerkSide, MerkWrite,
    NodePut, NodeView, ParkedCursor, RecordWrite, RootPut, Scope, Subtree, SubtreeView,
    SubtreeWrite, TxnError, Unit, ValuePut, WriteTxn,
};

use super::{
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

/// What a write does to a subtree as a whole: to its entry in the subtree
/// directory.
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
    /// The subtree is dropped, with every subtree below it.
    Drop(Prefix),
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

impl Write {
    /// Whether the Merk under `subtree` has no node before this write: the
    /// write enters its subtree. What the transaction holds under the
    /// prefix until then goes with a drop, which is applied before any
    /// change set, so no value of it is kept.
    fn is_fresh(&self, subtree: &Prefix) -> bool {
        self.subtrees.iter().any(|change| match change {
            SubtreeChange::Enter { subtree: at, .. } => at == subtree,
            SubtreeChange::Drop(_) => false,
        })
    }
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
}

/// One writer's view of a LigninDB transaction, as the store uses it.
pub(super) trait Access: Sync {
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
        error => refused(&error.to_string()),
    }
}

fn column(keyspace: Keyspace) -> Column {
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

fn stored_node(view: &NodeView<'_>, with_value: bool) -> Result<StoredNode, Error> {
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

fn go(cursor: &mut MerkCursor<'_, '_>, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
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

/// The node a cursor stands on. A cell names a child by its address, so
/// the key of each child is read from the child's cell with the node, in
/// one operation of the transaction: for a node of the cursor's committed
/// side that is the cell of the cursor's version.
fn node_under(cursor: &mut MerkCursor<'_, '_>) -> Result<Option<StoredNode>, Error> {
    let mut children = ChildKeys::new();
    let node = cursor
        .node_with_children(&mut children, |view| {
            view.map(|view| stored_node(&view, true)).transpose()
        })
        .map_err(fault)??;
    Ok(node.map(|mut node| {
        for (side, link) in [(Side::Left, &mut node.left), (Side::Right, &mut node.right)] {
            if let Some(link) = link {
                link.key = Some(children.key(side).to_vec());
            }
        }
        node
    }))
}

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

/// The cursor of a writer's handle. A cursor of a scope borrows the scope,
/// which the writer keeps behind its lock, so the cursor is parked between
/// two calls: it owns where it stands, the pin of its version and its
/// lease, and takes the lock for the length of each call.
struct ScopeWalk<'a, 't, 's> {
    writer: &'a LigninWriter<'t, 's>,
    parked: ParkedCursor<'t, 's, MerkSide<'s>>,
}

impl ScopeWalk<'_, '_, '_> {
    fn resumed<T>(
        &mut self,
        call: impl FnOnce(&mut MerkCursor<'_, '_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let scope = self.writer.scope();
        let cursor = scope
            .resume(&mut self.parked)
            .ok_or_else(|| refused("a cursor of another transaction"))?;
        call(cursor)
    }
}

impl Walk for ScopeWalk<'_, '_, '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        self.resumed(|cursor| go(cursor, to, key))
    }

    fn node(&mut self) -> Result<Option<StoredNode>, Error> {
        self.resumed(node_under)
    }
}

/// The reads a write transaction offers outside any scope and a scope
/// offers inside it, under one name.
trait Reads {
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
    /// The bytes it has for the hash of each child that the write leaves
    /// stale, where it has a link on that side.
    links: [Option<CryptoHash>; 2],
}

/// Reads what the transaction holds under the key of `node`, if the write
/// states a value or leaves a hash stale. The bytes of a value are
/// compared, never a hash: a stored value can change while no hash does
/// (`node-store.md`, "Errors").
fn held(scope: &Scope<'_, '_>, subtree: Subtree, node: &NodeWrite) -> Result<Held, Error> {
    let stated = match &node.value {
        ValueWrite::Stated(stated) => Some(stated.as_slice()),
        ValueWrite::Kept => None,
    };
    if stated.is_none() && node.stale.is_empty() {
        return Ok(Held::default());
    }
    let hint = node.hint.map(|Location(handle)| handle);
    Reads::node(scope, subtree, &node.key, hint, |view| {
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
        let links = Side::BOTH.map(|side| {
            let link = view.link(side);
            let link = link.filter(|_| node.stale.has(HashField::Child(side)));
            link.map(|link| *link.hash)
        });
        Ok(Held { value, links })
    })
    .and_then(|held| held)
    .map_err(fault)
}

/// A link as the transaction is handed it. For a hash the write leaves
/// stale, `held` has the bytes the cell has there, which are stated again
/// so that the write changes none of them (`NodePut::stale`).
fn link_put<'a>(link: &'a Option<LinkWrite>, held: &'a Option<CryptoHash>) -> Option<LinkPut<'a>> {
    link.as_ref().map(|link| LinkPut {
        key: &link.key,
        hash: held.as_ref().unwrap_or(&link.hash),
        heights: link.heights,
        aggregate: link.aggregate.as_slice(),
    })
}

fn apply(scope: &Scope<'_, '_>, write: &Write) -> Result<Written, Error> {
    let mut written = Written::default();

    // A stated value that the transaction holds already is kept where it
    // is, so that a value is written only when it changed. A hash that is
    // left stale keeps the bytes it has.
    let mut keep = Vec::with_capacity(write.merks.len());
    for merk in &write.merks {
        let subtree = Subtree::new(merk.subtree);
        let fresh = write.is_fresh(&merk.subtree);
        let mut kept = Vec::with_capacity(merk.nodes.len());
        for node in &merk.nodes {
            let held = match fresh {
                true => Held::default(),
                false => held(scope, subtree, node)?,
            };
            if matches!(node.value, ValueWrite::Stated(_)) {
                match held.value {
                    true => written.values_kept += 1,
                    false => written.values_written += 1,
                }
            }
            kept.push(held);
        }
        keep.push(kept);
    }

    let hosted: Vec<Vec<Option<Vec<Subtree>>>> = (write.merks.iter())
        .map(|merk| {
            let lists = merk.nodes.iter().map(|node| {
                let hosted = node.hosted.as_ref();
                hosted.map(|hosted| hosted.iter().copied().map(Subtree::new).collect())
            });
            lists.collect()
        })
        .collect();
    let nodes: Vec<Vec<NodePut<'_>>> = (write.merks.iter().zip(&keep).zip(&hosted))
        .map(|((merk, keep), hosted)| {
            let puts =
                (merk.nodes.iter().zip(keep).zip(hosted)).map(|((node, keep), hosted)| NodePut {
                    key: &node.key,
                    feature: node.feature.as_slice(),
                    value_hash: &node.value_hash,
                    kv_hash: &node.kv_hash,
                    left: link_put(&node.left, &keep.links[0]),
                    right: link_put(&node.right, &keep.links[1]),
                    hosted: match hosted {
                        None => Hosted::Keep,
                        Some(hosted) => Hosted::Set(hosted),
                    },
                    value: match (&node.value, keep.value) {
                        (ValueWrite::Stated(stated), false) => ValuePut::Value(stated),
                        _ => ValuePut::Keep,
                    },
                    at: node.hint.map(|Location(handle)| handle),
                    stale: node.stale,
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

    let subtrees: Vec<SubtreeWrite<'_>> = (write.subtrees.iter())
        .map(|change| match change {
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
            SubtreeChange::Drop(subtree) => SubtreeWrite::Drop {
                subtree: Subtree::new(*subtree),
            },
        })
        .collect();

    let applied = scope
        .apply(&Batch {
            records: &records,
            merks: &merks,
            subtrees: &subtrees,
        })
        .map_err(fault)?;

    // The handles come in the order of the batch: the nodes of the first
    // Merk, then those of the second. A transaction that serves its records
    // by key reports none.
    let mut handles = applied.nodes.into_iter().map(Location);
    written.locations = (write.merks.iter())
        .map(|merk| handles.by_ref().take(merk.nodes.len()).collect())
        .collect();
    Ok(written)
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

    fn scope(&self) -> MutexGuard<'_, Scope<'t, 's>> {
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
trait Reader: Sync {
    type Reads: Reads;

    fn reading<T>(&self, read: impl FnOnce(&Self::Reads) -> T) -> T;

    fn write(&self, write: &Write) -> Result<Written, Error>;

    fn walk<'a>(&'a self, subtree: Subtree) -> Result<Box<dyn Walk + 'a>, TxnError>;
}

impl<'t, 's> Reader for LigninWriter<'t, 's> {
    type Reads = Scope<'t, 's>;

    fn reading<T>(&self, read: impl FnOnce(&Self::Reads) -> T) -> T {
        read(&self.scope())
    }

    fn write(&self, write: &Write) -> Result<Written, Error> {
        apply(&self.scope(), write)
    }

    fn walk<'a>(&'a self, subtree: Subtree) -> Result<Box<dyn Walk + 'a>, TxnError> {
        let parked = self.scope().parked_merk_cursor(subtree)?;
        Ok(Box::new(ScopeWalk {
            writer: self,
            parked,
        }))
    }
}

/// A write transaction read outside any scope: what a handle that only
/// reads is opened on.
impl Reader for WriteTxn<'_> {
    type Reads = Self;

    fn reading<T>(&self, read: impl FnOnce(&Self::Reads) -> T) -> T {
        read(self)
    }

    fn write(&self, _write: &Write) -> Result<Written, Error> {
        Err(refused("a write through a handle that only reads"))
    }

    fn walk<'a>(&'a self, subtree: Subtree) -> Result<Box<dyn Walk + 'a>, TxnError> {
        Ok(Box::new(TxnWalk(self.merk_cursor(subtree)?)))
    }
}

impl<A: Reader> Access for A {
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
        self.write(write)
    }
}
