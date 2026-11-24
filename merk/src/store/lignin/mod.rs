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
//! A handle is of one subtree ([`LigninSubtree`]), opened on a view
//! ([`LigninView`]) in one of three modes:
//!
//! | Handle | Reads | Writes |
//! | --- | --- | --- |
//! | [`LigninNodeStore::deferred`] | its view; nothing that is staged | staged in the [`LigninBatch`] it was opened with, or refused with none |
//! | [`LigninNodeStore::immediate`] | its view, its own writes included | applied at once, each commit as a write of its own |
//! | [`LigninNodeStore::read_only`] | its view | refused |
//!
//! A view is a write transaction, read from outside every scope and
//! written through a scope of one write; a [`LigninWriter`], which is the
//! write scope of one writer (`transactions.md` SC1); or a
//! [`LigninSnapshot`], which reads one version and refuses every write.
//! `view` has the table. A handle and a Merk on it are `Send`.
//!
//! A handle is also the flat keyspace of its subtree's data, for a tree
//! that keeps no Merk there, and hands out the aux and the meta keyspace
//! ([`LigninKeyspace`], `flat`).
//!
//! # Reads
//!
//! A node is read by its key, and through a link by where its cell was last
//! seen while that is still good (`tree-layout.md` S6). A cell names its
//! children by address and holds the length of each child's key but not
//! the key (`tree-page.md` TP10), so a read fills each link's key from the
//! child's cell. It keeps where that cell is: the handle follows a link to
//! the child there, and names the node there when it writes it. On a write
//! transaction the child's cell is reached through the location of its
//! parent's, in an operation after the one that read the parent, and that
//! location can have ended by then: the node is then read again (`load`).
//! Where a
//! written node is comes back from the application of its write
//! (`tree-layout.md` P5): at once on an immediate handle, and from
//! [`LigninBatch::apply`] for a handle that stays open
//! ([`LigninNodeStore::take_locations`]).
//!
//! Every read builds a `TreeNode`. A read by key and a fetch through a link
//! build it with its value. A fetch for a walk that only reads
//! ([`NodeStore::fetch_node_without_value`]), which is how a proof walks,
//! builds it without: the cell holds the hashes, the feature and the links
//! such a walk reads, and the value's length, by which the read is charged
//! as the whole record. The value is read when the walk asks for it
//! ([`NodeStore::load_node_value`]), and only if the node under the key
//! still has the value hash and the key-value hash it was read with.
//! PERF-01 replaces the `TreeNode` with views.
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
//! which Merk reports as written on every write below it, hands the
//! transaction no value. The charge for such a put is that of the whole
//! record, as on RocksDB.
//!
//! A node that keeps its value names the read the value is kept from, so
//! that the transaction can say whether a commit wrote the node since
//! (`lignindb::txn`, "A kept value and the read it is of"). Where Merk
//! says the value is unchanged, that is the handle's first load of the
//! node: no later place of the node says which value a node that is
//! handed over was made from. A first load that showed the transaction's
//! own record has no handle, and the keep names none. Where the store
//! kept a stated value, it is
//! the read that compared the bytes. A node that is written outside a
//! commit through a handle that has not loaded it has its value stated,
//! whatever change the caller names ([`NodeStore::write_node`]).
//!
//! An unlinked record has no cell that keeps a value
//! (`lignindb::txn::MerkWrite`, "Unlinked records"). A node that is
//! handed over with its value kept, under a key the handle knows to hold
//! such a record, has its value stated: the value of the node as it is
//! handed over, which is what a store of whole records is handed. The
//! handle knows it of a key from its last load of the key, and from the
//! report of a write that it took in since.
//!
//! A handle dates a node by its own loads, so a node that is handed to it
//! with its value kept has to be one it loaded, as a Merk loads what it
//! changes through the handle it writes with. Of a node that another
//! handle loaded, this one knows no read: its own first load of the key
//! may be a later one, and a commit between the two is not seen. The
//! value hash does not tell the two apart, because a node that keeps its
//! value may state another value hash for it ([`NodeChange`]). A record
//! of the handle's cursor is no load either: the cursor shows the
//! committed nodes of the moment it was opened, which a load made since
//! may no longer show, and it gives the handle no place.
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
//! # Hashes
//!
//! A cell stores the value hash, the key-value hash and the hash of each
//! child as Merk holds them, and a read hands them to Merk as they are
//! stored. A write states every hash Merk holds and leaves to the engine
//! the hash of a child whose link is modified, which Merk has not made:
//! on existing GroveVersions there is none, because a commit hashes its
//! tree first. The rules by which the engine makes such a hash are Merk's,
//! and [`hash`] has them, with what each state of a link means for the
//! store. A node with a hash that is not made yet is not read.
//!
//! # The cursor
//!
//! [`LigninCursor`] walks the nodes of the Merk in key order and shows each
//! as the record Merk encodes of it. Its documentation says what it shows
//! and what it holds.
//!
//! # Costs
//!
//! Every charge comes from the functions of [`cost_model`], with the sizes
//! Merk's own record has: a read is charged the record length of the node
//! it found, once for the read however many cells served it, a staged
//! write nothing until its batch is applied, and a batch as a storage
//! batch is. The record length of a cell is measured from its sizes
//! ([`cost_model::node_record_len_from_sizes`]), and that of a node Merk
//! writes from the node ([`cost_model::node_record_len`]), or from its
//! parts where a link of it is modified and Merk has no record of it
//! ([`hash`]): no record is encoded for a charge.
//! `docs/lignindb/cost-model.md`, section 15, has the
//! call for each event and how the store is held to it.
//!
//! # What is not here
//!
//! - **Which subtrees an operation enters and drops.** A batch passes
//!   each on to the subtree directory ([`LigninBatch::enter`],
//!   [`LigninBatch::drop_subtree`]), and names a Merk in the host a batch
//!   writes with it ([`LigninHost`]). Saying them is GroveDB's, which
//!   knows the elements (GROVE-01, GROVE-05). What the directory holds of
//!   a subtree is read through its handle ([`LigninNodeStore::entry`]),
//!   and a Merk is opened by it (`Merk::open_from_directory`). A drop
//!   charges nothing: what the removal of a tree's nodes is charged is
//!   the removals that were staged for them before
//!   ([`LigninSettled::leave_out`]).
//! - **The clear of a subtree**, which the transaction offers beside the
//!   drop. A Merk that keeps its host loses its nodes one by one, by key,
//!   as on RocksDB (`transactions.md`, CV9).

mod access;
mod change_set;
mod cursor;
mod directory;
mod feature;
mod flat;
pub mod hash;
mod left;
#[cfg(all(test, feature = "rocksdb_storage"))]
mod open_tests;
mod place;
#[cfg(all(test, feature = "rocksdb_storage"))]
mod tests;
mod view;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex, PoisonError},
};

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_element::ElementType;
use grovedb_storage::Error as StorageError;
use grovedb_version::version::GroveVersion;

use self::access::{Access, Child, Location, StoredLink, StoredNode};
pub use self::{
    access::{LigninEntryRefused, LigninWriter},
    change_set::{
        LigninApplied, LigninBatch, LigninChangeSet, LigninMove, LigninOutcome, LigninRefused,
        LigninSettled, PreparedNode,
    },
    cursor::LigninCursor,
    directory::{
        keeps_flat_records, tree_type_bytes, tree_type_from_bytes, LigninEntry, LigninHosted,
    },
    flat::{LigninFlatCursor, LigninKeyspace},
    left::left_behind_key,
    view::{LigninSnapshot, LigninView},
};
use super::{NodeChange, NodeStore};
use crate::{
    cost_model::{self, Keyspace, StorageCostModel},
    merk::defaults::ROOT_KEY_KEY,
    tree::{
        kv::{ValueDefinedCostType, KV},
        TreeNode, TreeNodeInner,
    },
    Error, Link, TreeType,
};

/// The version of the storage cost rules this store charges by: the one
/// every handle charges by, since a handle is opened with no grove version.
const MODEL: StorageCostModel = cost_model::HANDLE_COST_MODEL;

/// The element that hosts a Merk: where the Merk hangs in the grove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigninHost {
    /// The subtree of the Merk that holds the element.
    pub subtree: [u8; 32],
    /// The element's key in that Merk.
    pub key: Vec<u8>,
    /// The Merks the element hosts once the batch is applied, by position.
    /// No position is left out, so an element that loses a Merk loses
    /// those after it too.
    ///
    /// Every handle that names one element in one batch gives the same
    /// list. A handle knows one subtree: for an element that hosts one
    /// Merk the list is that Merk. Of an element that hosts more, an
    /// indexed tree, the handles give the first Merk alone, and whoever
    /// applies the batch says the whole list
    /// ([`LigninSettled::host`]), where the element is known as the batch
    /// leaves it.
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
    /// The handle each node's value was first read with, by key: that of
    /// the handle's first load of the node, and none where that load
    /// showed the transaction's own record. A node that is written with
    /// its value kept names it. A later place of the node is in `hints`
    /// and not here, whether the read of its parent said it, a second load
    /// of the node or the report of a write: the caller may still hold the
    /// node as it was first loaded.
    reads: RefCell<HashMap<Vec<u8>, Option<Location>>>,
    /// The keys of the nodes that the handle loaded with the element of a
    /// tree as their value. A write that states another value for one of
    /// them is told to the batch that takes it
    /// ([`LigninSettled::over_trees`]).
    trees: RefCell<HashSet<Vec<u8>>>,
    /// Whether the writer of the handle's Merk began with no node, once
    /// the first change set of nodes was staged through the handle
    /// ([`Self::began_empty`]).
    began: Cell<Option<bool>>,
    /// Whether the handle took in where an application put nodes of its
    /// subtree ([`Self::take_locations`]).
    placed: Cell<bool>,
    /// The path the handle was opened by, if it was told one
    /// ([`Self::at_path`]).
    path: Option<Vec<Vec<u8>>>,
    /// The keys the handle knows to hold an unlinked record.
    loose: Loose,
    /// The subtree that the handle keeps its root-key record under, where
    /// it was told one ([`Self::root_key_kept_in`]).
    root_keys: Option<[u8; 32]>,
    /// The subtree that keeps the marks of what flat drops left behind,
    /// where the handle was told one ([`Self::left_behind_in`]).
    left_behind: Option<[u8; 32]>,
    /// What the last read of the root-key record through this handle
    /// found, where the handle writes at once and the record does not
    /// name the root of the handle's Merk ([`RootLeftBehind`]).
    root_left_behind: RefCell<Option<RootLeftBehind>>,
}

/// A root-key record that does not name the root of its Merk: the record
/// as a handle read it, and the key of the root that the subtree
/// directory held for the Merk at that read.
///
/// The store finds a node from the root that the directory names. A Merk
/// that is opened with no root key given is opened from the record
/// (`Merk::open_base`): with the node the record names where the tree
/// holds one, and with no node otherwise. Such a Merk is not the tree
/// that the store holds, and a commit of it would state a root which
/// leaves cells of that tree with no link to them. So no node of the
/// Merk is written where the record was read:
///
/// - **A handle that writes at once** refuses a change set that puts or
///   removes a node, from such a read of the record through it until its
///   next read of the record that is answered, with
///   [`Error::NotSupported`].
/// - **A batch** takes no node of that Merk from the first such read by
///   a handle that stages into it, for as long as it is there
///   ([`LigninBatch::note_root_left_behind`]): it is refused where it is
///   settled ([`LigninBatch::settle`]), with [`Error::NotSupported`], if
///   it holds a put or a removal of a node of that Merk. Nothing of the
///   batch reaches the transaction.
///
/// The root is read before the record and after it, and the three are
/// read again where the two roots differ or a commit landed between
/// them ([`Access::record_with_root`]). A handle that only reads, or
/// that stages into no batch, reads the record alone and keeps nothing
/// of it. A write of a record that is no node is taken as before. A
/// record is not judged where the directory holds no root for the Merk,
/// or no entry. What a handle of one batch read says nothing of the
/// nodes of a batch that was neither taken out of that one nor merged
/// with it.
///
/// GroveDB leaves a record so on grove versions 1 to 3: the delete of a
/// tree that holds elements writes the Merk the tree was in through a
/// Merk that is opened with a root key given, whose commit writes no
/// root-key record, and at the root path that Merk is the root Merk
/// (`grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs`,
/// GroveDB #686). GroveDB #1065 is the report of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RootLeftBehind {
    record: Option<Vec<u8>>,
    root: Vec<u8>,
}

impl RootLeftBehind {
    /// The refusal of a write of nodes of the Merk of `subtree`.
    pub(super) fn refusal(&self, subtree: &[u8; 32]) -> Error {
        let record = match &self.record {
            Some(key) => format!("names the key {}", hex::encode(key)),
            None => "is absent".to_owned(),
        };
        Error::NotSupported(format!(
            "lignindb: the root-key record of Merk {} {record}, and the root of that Merk is \
             the node under the key {}: no node of a Merk is written where its root-key record \
             was read and does not name its root. On grove versions 1 to 3 the delete of a \
             tree that holds elements leaves the record so where it changes the root of the \
             Merk the tree was in \
             (grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs, GroveDB #686 and #1065)",
            hex::encode(subtree),
            hex::encode(&self.root),
        ))
    }
}

/// The keys a handle knows to hold an unlinked record: those its last
/// load of the key showed as one, and those the report of a write that it
/// took in since named as one. The handle shares the set with its change
/// sets, which prepare a node by what the handle knows then.
#[derive(Clone, Debug, Default)]
pub(super) struct Loose(Arc<Mutex<HashSet<Vec<u8>>>>);

impl Loose {
    pub(super) fn holds(&self, key: &[u8]) -> bool {
        let keys = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        keys.contains(key)
    }

    fn set(&self, key: &[u8], unlinked: bool) {
        let mut keys = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match unlinked {
            true => keys.insert(key.to_vec()),
            false => keys.remove(key),
        };
    }
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

/// The key under which a handle of `subtree` that keeps its root-key
/// record outside its subtree keeps it
/// ([`LigninNodeStore::root_key_kept_in`]), in the roots keyspace of the
/// subtree it was told: the prefix of `subtree`, and then the key of the
/// record.
pub fn kept_root_key(subtree: &[u8; 32]) -> Vec<u8> {
    access::kept_key(subtree, ROOT_KEY_KEY)
}

/// The record length of a stored node: the bytes Merk's own record of it
/// takes (`cost-model.md` N1). It is measured from the cell alone: a link
/// holds the length of its child's key.
fn record_len(node: &StoredNode) -> Result<usize, Error> {
    if let Some(record) = &node.record {
        return Ok(record.len());
    }
    let child =
        |link: &Option<StoredLink>| link.as_ref().map(|link| (link.key_len, link.aggregate));
    cost_model::node_record_len_from_sizes(
        MODEL,
        node.value_len as usize,
        &node.feature,
        child(&node.left),
        child(&node.right),
    )
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

/// A node that was read with no value, as a walk that only reads takes it.
fn node_without_value(key: Vec<u8>, node: StoredNode) -> Result<TreeNode, Error> {
    Ok(TreeNode::new_without_value(
        key,
        node.kv_hash,
        node.value_hash,
        node.feature,
        reference(node.left)?,
        reference(node.right)?,
    ))
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
            reads: RefCell::new(HashMap::new()),
            trees: RefCell::new(HashSet::new()),
            began: Cell::new(None),
            placed: Cell::new(false),
            path: None,
            loose: Loose::default(),
            root_keys: None,
            left_behind: None,
            root_left_behind: RefCell::new(None),
        }
    }

    /// The handle, with its root-key record kept outside its subtree: in
    /// the roots keyspace of the subtree `home`, under the handle's own
    /// prefix and then the record's key. [`NodeStore::root_key`] reads it
    /// there, and a write of it is handed to the transaction there. It is
    /// charged as the record of this subtree that it is for Merk: by its
    /// own key and its value. A handle that is told its own subtree keeps
    /// the record in its subtree as one that was told nothing: none of
    /// the following is of it.
    ///
    /// The record is then a record of `home`, for the transaction and for
    /// a batch, and none of this subtree: an immediate handle hands a
    /// write of it to the transaction there, and a handle that stages its
    /// writes stages it there. A batch counts it as a write into no
    /// subtree, neither into this one nor into `home`
    /// ([`LigninSettled::written`], [`LigninSettled::put_into`]), and
    /// hands it over whichever subtrees it is told to hand no write of
    /// ([`LigninSettled::leave_out`]). A change set of the handle that
    /// holds nothing else tells the batch nothing of this subtree:
    /// neither the element that the handle names for it nor the path
    /// that the handle was opened by. Where `home` is no subtree that
    /// leaves with this one: a drop of this subtree leaves the record,
    /// also a drop that the batch stages after the write
    /// ([`LigninBatch::drop_subtree`]); a move of this subtree does not
    /// carry it; and a write of it is taken whether or not this subtree
    /// is entered. What takes `home`, or the records of its roots
    /// keyspace, takes the record.
    /// The store finds no node by the record: the root of the subtree's
    /// Merk is the one its entry in the subtree directory names. `home`
    /// has to be entered when the record is written.
    ///
    /// Each handle keeps the record where it was told. Of two handles of
    /// one subtree, one that was told nothing reads and writes the record
    /// of the subtree itself, and two that were told two subtrees read
    /// and write two records.
    pub fn root_key_kept_in(mut self, home: [u8; 32]) -> Self {
        self.root_keys = (home != self.subtree.prefix).then_some(home);
        self
    }

    /// Where the handle keeps its root-key record: the subtree, and the
    /// key that the record has in the roots keyspace of that subtree
    /// ([`Self::root_key_kept_in`]).
    pub fn root_key_place(&self) -> ([u8; 32], Vec<u8>) {
        match self.root_keys {
            Some(home) => (home, access::kept_key(&self.subtree.prefix, ROOT_KEY_KEY)),
            None => (self.subtree.prefix, ROOT_KEY_KEY.to_vec()),
        }
    }

    /// The handle, whose cursors count what flat drops left behind: the
    /// marks that are kept in the aux keyspace of the subtree `home`
    /// ([`left_behind_key`]).
    ///
    /// A cursor of the handle that steps from no key asks how many
    /// records lie beside its subtree (`flat-store.md`, rule 6). A cursor
    /// of the data, the aux or the meta keyspace then counts each mark of
    /// its keyspace under a prefix on that side as one record, read at
    /// the version and with the writes that the cursor reads the
    /// subtrees beside it with, and counts no record of the aux keyspace
    /// of `home` as a record of `home`. A cursor of the roots keyspace
    /// counts as one of a handle that was told nothing does.
    ///
    /// The marks are written by whoever applies a batch
    /// ([`LigninSettled::leave_behind`]) and removed by whoever knows
    /// that the records they stand for are gone. Every record that the
    /// aux keyspace of `home` holds under a key which begins with the
    /// tag of a mark is taken for one.
    pub fn left_behind_in(mut self, home: [u8; 32]) -> Self {
        self.left_behind = Some(home);
        self
    }

    /// The handle, told the path that it was opened by: the segments that
    /// its prefix was derived from. The store reads nothing by it. A batch
    /// that takes a write of the handle into its subtree keeps it for
    /// whoever applies the batch ([`LigninSettled::path`]), who derives
    /// from it the prefix of the subtree under a key of this one.
    pub fn at_path(mut self, path: Vec<Vec<u8>>) -> Self {
        self.path = Some(path);
        self
    }

    /// A deferred handle of `subtree`: its writes are staged in `batch`
    /// until the batch is applied, and it reads none of them before. With
    /// no batch it refuses every write.
    pub fn deferred(
        view: impl Into<LigninView<'db>>,
        subtree: LigninSubtree,
        batch: Option<&'db LigninBatch>,
    ) -> Self {
        Self::new(view.into().0, subtree, Mode::Deferred(batch))
    }

    /// An immediate handle of `subtree`: it writes to the transaction at
    /// once and reads its own writes.
    pub fn immediate(view: impl Into<LigninView<'db>>, subtree: LigninSubtree) -> Self {
        Self::new(view.into().0, subtree, Mode::Immediate)
    }

    /// A handle of `subtree` that only reads. On a write transaction it
    /// reads from outside every scope, and a read of a unit that an open
    /// scope holds for writing is refused (`transactions.md` SC3).
    pub fn read_only(view: impl Into<LigninView<'db>>, subtree: LigninSubtree) -> Self {
        Self::new(view.into().0, subtree, Mode::ReadOnly)
    }

    /// The subtree the handle is of.
    pub fn subtree(&self) -> &LigninSubtree {
        &self.subtree
    }

    /// Takes in where the application of a batch put the nodes of this
    /// subtree, so that the handle finds them without a lookup by key.
    pub fn take_locations(&self, applied: &LigninApplied) {
        applied.locations(&self.subtree.prefix, |key, location| {
            self.left_at(key, location);
            self.placed.set(true);
        });
    }

    /// Takes in where a write left the node under `key`: in the cell at
    /// `location`, or with none, as an unlinked record.
    fn left_at(&self, key: &[u8], location: Option<Location>) {
        let mut hints = self.hints.borrow_mut();
        match location {
            Some(location) => hints.insert(key.to_vec(), location),
            None => hints.remove(key),
        };
        self.loose.set(key, location.is_none());
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

    /// The node under `key` with the key of each child in its link, and
    /// with its value where `with_value` asks for it.
    ///
    /// The node and its children are read in operations of their own, the
    /// children through the location of the node's cell, and that
    /// location can end between two of them: at a commit, where a cell
    /// leaves the cell's page, and by what the reader's own transaction
    /// does. The transaction then says that the location is no longer
    /// good, and the node is read again. There is no count of tries, and
    /// locations are not compared.
    ///
    /// One reader gets that answer whatever the location: a write scope
    /// that reads the Merk's nodes by their keys. So the store asks,
    /// after that answer, whether the reader reads by key, and refuses
    /// the read if it does then ([`Child::ByKey`],
    /// [`access::read_by_key`]). It reads the node again if it does not.
    /// For that, what the reader sees changed since the read of the
    /// cell, or the reader read by key at the read of the child and no
    /// longer at the question: a further try takes a further such event.
    /// A read of the cell's page that fails at the read of a child is the
    /// error of the load, and no further try.
    fn load(&self, key: &[u8], with_value: bool) -> Result<Option<StoredNode>, Error> {
        let prefix = &self.subtree.prefix;
        let mut hint = self.hint(key);
        'node: loop {
            let Some(mut node) = self.access.node(prefix, key, hint, with_value)? else {
                self.loose.set(key, false);
                return Ok(None);
            };
            if node.record.is_some() {
                // A flat record is no cell and no unlinked record: the
                // read changes nothing of what the handle remembers of
                // the key. What is decoded from the record is marked as
                // a node from a flat record where it is decoded
                // ([`TreeNode::from_flat_record`]).
                return Ok(Some(node));
            }
            hash::refuse_stale(node.stale)?;
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
                    Child::ByKey => return Err(access::read_by_key()),
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
            self.loose.set(key, node.unlinked);
            let mut reads = self.reads.borrow_mut();
            reads.entry(key.to_vec()).or_insert(node.location);
            self.note_tree(key, node.value.as_deref());
            return Ok(Some(node));
        }
    }

    /// Whether a change set of nodes that this handle stages is of a Merk
    /// whose writer began with no node. The first such change set that
    /// the handle stages decides it, for every later one of the handle
    /// too, which are of the same writer: it begins a Merk
    /// ([`LigninChangeSet::begins_a_merk`]). A handle that took in an
    /// application of the Merk writes on what that application left,
    /// whatever it began with.
    fn began_empty(&self, change_set: &LigninChangeSet) -> bool {
        if self.placed.get() {
            return false;
        }
        let begins = || change_set.begins_a_merk();
        self.began.get().unwrap_or_else(begins)
    }

    /// Keeps the key of a node that was loaded with the element of a tree
    /// as its value.
    fn note_tree(&self, key: &[u8], value: Option<&[u8]>) {
        let element = value.and_then(|value| ElementType::from_serialized_value(value).ok());
        if element.is_some_and(|element| element.is_tree()) {
            self.trees.borrow_mut().insert(key.to_vec());
        }
    }

    /// Charges a read: the record length of what it found, and a read that
    /// failed as one that found nothing.
    fn charged<T>(&self, read: Result<(T, Option<usize>), Error>) -> CostResult<T, Error> {
        match read {
            Ok((found, len)) => Ok(found).wrap_with_cost(self.read_cost(len)),
            Err(error) => Err(error).wrap_with_cost(self.read_cost(None)),
        }
    }

    /// Loads the node under `key` and charges the read: the length of the
    /// node's whole record, whether or not its value is read with it.
    fn read(&self, key: &[u8], with_value: bool) -> CostResult<Option<StoredNode>, Error> {
        self.charged(self.load(key, with_value).and_then(|node| {
            let len = node.as_ref().map(record_len).transpose()?;
            Ok((node, len))
        }))
    }

    /// Reads the value of `node`, which was read without it, if the node
    /// under its key still has the value hash and the key-value hash that
    /// were read: a value whose hashed bytes changed has another.
    fn read_value_of(&self, node: &mut TreeNode) -> Result<(), Error> {
        if !node.value_absent() {
            return Ok(());
        }
        let prefix = &self.subtree.prefix;
        let stored = self
            .access
            .node(prefix, node.key(), self.hint(node.key()), true)?
            .filter(|stored| stored.record.is_none())
            .ok_or_else(|| refused("a node was removed before its value was read"))?;
        hash::refuse_stale(stored.stale)?;
        if stored.value_hash != *node.value_hash() || stored.kv_hash != *node.kv_hash() {
            return Err(refused("a node was written before its value was read"));
        }
        let value = stored
            .value
            .ok_or(Error::CorruptedState("a node was read with no value"))?;
        self.note_tree(node.key(), Some(&value));
        node.put_read_value(value);
        Ok(())
    }

    fn read_record(&self, keyspace: Keyspace, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        let record = match (keyspace, self.root_keys) {
            (Keyspace::Roots, Some(home)) => {
                let kept = access::kept_key(&self.subtree.prefix, key);
                self.access.record(keyspace, &home, &kept)
            }
            _ => self.access.record(keyspace, &self.subtree.prefix, key),
        };
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
        let subtree = self.subtree.clone();
        let change_set =
            LigninChangeSet::of_record_of(subtree, self.root_keys, keyspace, key, value);
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
        change_set.hint(&self.hints.borrow(), &self.reads.borrow());
        change_set.note_trees(&self.trees.borrow(), &self.reads.borrow());
        match self.mode {
            Mode::ReadOnly | Mode::Deferred(None) => {
                Err(refused(&format!("{what} through a handle that only reads")))
                    .wrap_with_cost(MODEL.refused_write())
            }
            Mode::Deferred(Some(batch)) => {
                let staged = change_set.check_stale();
                let writes_nodes = change_set.writes_nodes();
                let began_empty = writes_nodes && self.began_empty(&change_set);
                let staged = staged
                    .and_then(|()| batch.stage(change_set, began_empty, self.path.as_deref()));
                // The first change set of nodes that is staged decides
                // what the handle's writer began with.
                if staged.is_ok() && writes_nodes && self.began.get().is_none() {
                    self.began.set(Some(began_empty));
                }
                staged.wrap_with_cost(MODEL.staged_write())
            }
            Mode::Immediate => {
                if let Some(found) = &*self.root_left_behind.borrow()
                    && change_set.writes_nodes()
                {
                    let refusal = found.refusal(&self.subtree.prefix);
                    return Err(refusal).wrap_with_cost(MODEL.refused_write());
                }
                let checked = change_set.check_stale();
                let (cost, write) = change_set.into_write();
                let applied = checked.and_then(|()| self.apply(&write));
                applied.wrap_with_cost(match is_commit {
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
        for (merk, locations) in write.merks.iter().zip(&written.locations) {
            for key in &merk.removed {
                self.hints.borrow_mut().remove(key);
                self.loose.set(key, false);
            }
            for (node, location) in merk.nodes.iter().zip(locations) {
                self.left_at(&node.key, *location);
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
    moved::<LigninCursor<'static>>();
    moved::<LigninBatch>();
    moved::<LigninSettled>();
    moved::<LigninWriter<'static, 'static>>();
    moved::<LigninSnapshot<'static>>();
};

impl<'db> NodeStore<'db> for LigninNodeStore<'db> {
    type ChangeSet = LigninChangeSet;
    type Cursor = LigninCursor<'db>;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error> {
        self.read(key, true)
            .map_ok(|node| {
                let Some(mut node) = node else {
                    return Ok(None);
                };
                if let Some(record) = node.record.take() {
                    let key = key.to_vec();
                    let cost_fn = value_defined_cost_fn;
                    let mut node = TreeNode::decode_raw(&record, key, cost_fn, grove_version)?;
                    node.mark_from_flat_record();
                    return Ok(Some(node));
                }
                let mut inner = node_inner(key.to_vec(), node)?;
                if let Some(value_defined_cost_fn) = &value_defined_cost_fn {
                    inner.kv.value_defined_cost =
                        value_defined_cost_fn(inner.kv.value_as_slice(), grove_version);
                }
                Ok(Some(TreeNode::new_with_tree_inner(inner)))
            })
            .flatten()
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error> {
        self.read(key, true)
            .map_ok(|node| match node {
                None => Ok(None),
                Some(StoredNode {
                    record: Some(record),
                    ..
                }) => {
                    let mut inner = TreeNode::decode_raw_for_read(&record)?;
                    inner.marks.from_flat_record = true;
                    Ok(Some(inner))
                }
                Some(node) => node_inner(Vec::new(), node).map(Some),
            })
            .flatten()
    }

    fn fetch_node_without_value(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        let key = link.key();
        self.read(key, false)
            .map_ok(|node| {
                let mut node = node.ok_or(Error::KeyNotFoundError("Key not found for fetch"))?;
                // A flat record holds its value: the node is read whole.
                if let Some(record) = node.record.take() {
                    let no_cost = None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
                    let mut node =
                        TreeNode::decode_raw(&record, key.to_vec(), no_cost, grove_version)?;
                    node.mark_from_flat_record();
                    return Ok(node);
                }
                node_without_value(key.to_vec(), node)
            })
            .flatten()
    }

    fn load_node_value(&self, node: &mut TreeNode) -> CostResult<(), Error> {
        self.read_value_of(node)
            .wrap_with_cost(OperationCost::default())
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error> {
        let prefix = &self.subtree.prefix;
        let node = self.access.node(prefix, key, self.hint(key), false);
        let len = node.and_then(|node| node.as_ref().map(record_len).transpose());
        self.charged(len.map(|len| (len.is_some(), len)))
    }

    /// A cell names a child by where the child's cell is, so the link of
    /// a cell is written only to a node the Merk holds or the same change
    /// set writes.
    fn takes_links_to_absent_nodes(&self) -> bool {
        false
    }

    fn write_node(&self, node: &TreeNode, change: NodeChange) -> CostResult<(), Error> {
        // A value is kept by the read it was kept from. Of a node this
        // handle has not loaded there is none to name, and an unlinked
        // record has no cell that keeps one, nor has a node that was
        // decoded from a flat record. The value of such a node is stated.
        let loaded = self.reads.borrow().contains_key(node.key());
        let kept_by_a_cell = loaded && !self.loose.holds(node.key());
        let change = match kept_by_a_cell && !node.from_flat_record() {
            true => change,
            false => NodeChange::ValueWritten,
        };
        match LigninChangeSet::of_node(self.subtree.clone(), node, change) {
            Ok(change_set) => self.submit(change_set, "a write", false),
            Err(error) => Err(error).wrap_with_cost(MODEL.refused_write()),
        }
    }

    /// The subtree is stated anew ([`Self::convert`]): at once on a
    /// handle that writes at once, and in the batch of a handle that
    /// stages, with the nodes that were written again.
    fn state_tree_type(&self, tree_type: TreeType) -> Result<(), Error> {
        self.convert(tree_type)
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error> {
        if matches!(self.mode, Mode::ReadOnly | Mode::Deferred(None)) {
            return self.read_record(Keyspace::Roots, ROOT_KEY_KEY);
        }
        let (home, key) = self.root_key_place();
        let prefix = &self.subtree.prefix;
        let both = (self.access).record_with_root(Keyspace::Roots, &home, &key, prefix);
        let read = self.charged(both.map(|both| {
            let len = both.0.as_ref().map(Vec::len);
            (both, len)
        }));
        read.map_ok(|(record, root)| {
            let found = root.filter(|root| record.as_ref() != Some(root));
            let found = found.map(|root| RootLeftBehind {
                record: record.clone(),
                root,
            });
            match (&self.mode, found) {
                (Mode::Deferred(Some(batch)), Some(found)) => {
                    batch.note_root_left_behind(*prefix, found);
                }
                (Mode::Immediate, found) => *self.root_left_behind.borrow_mut() = found,
                _ => {}
            }
            record
        })
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error> {
        self.write_record(Keyspace::Roots, ROOT_KEY_KEY, root_key)
    }

    fn cursor(&self) -> LigninCursor<'db> {
        LigninCursor::open(self.access, &self.subtree.prefix, self.left_behind)
    }

    fn change_set(&self) -> LigninChangeSet {
        let immediate = matches!(self.mode, Mode::Immediate);
        LigninChangeSet::new(self.subtree.clone(), immediate)
            .of_handle(self.loose.clone())
            .keeping_root_keys_in(self.root_keys)
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
