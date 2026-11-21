//! The writes of one operation, as the caller states them, and what it is
//! told when they are applied.

use lignin_format::{tree_page::Hash, CellAddress};
use lignin_tree::{hash::Stale, node::NodeHandle};

use super::key::{Column, Subtree, Unit};

/// A write of a record that is not a Merk node: an aux, roots or meta
/// record, or a flat entry of an append-only tree (`transactions.md` ST1,
/// ST5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordWrite<'a> {
    /// Stores `value` under `key`, in place of any value that is there.
    Put {
        /// The unit of the record.
        unit: Unit,
        /// The record's key.
        key: &'a [u8],
        /// The record's value.
        value: &'a [u8],
    },
    /// Removes the record under `key`. A delete of a key that has no record
    /// is a write all the same (ST5).
    Delete {
        /// The unit of the record.
        unit: Unit,
        /// The record's key.
        key: &'a [u8],
    },
    /// Removes every record of `unit` whose key is at or after `start` and
    /// before `end`, or to the end of the unit with no `end` (ST4).
    DeleteRange {
        /// The unit of the records.
        unit: Unit,
        /// The first key of the range.
        start: &'a [u8],
        /// The key the range ends before.
        end: Option<&'a [u8]>,
    },
}

impl<'a> RecordWrite<'a> {
    /// A put of `value` under `key` in `column` of `subtree`.
    pub const fn put(column: Column, subtree: Subtree, key: &'a [u8], value: &'a [u8]) -> Self {
        Self::Put {
            unit: Unit::new(column, subtree),
            key,
            value,
        }
    }

    /// A delete of `key` in `column` of `subtree`.
    pub const fn delete(column: Column, subtree: Subtree, key: &'a [u8]) -> Self {
        Self::Delete {
            unit: Unit::new(column, subtree),
            key,
        }
    }

    /// The unit the write is in.
    pub const fn unit(&self) -> Unit {
        match *self {
            Self::Put { unit, .. } | Self::Delete { unit, .. } | Self::DeleteRange { unit, .. } => {
                unit
            }
        }
    }
}

/// A link of a written node, as Merk holds it: the child is named by its
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkPut<'a> {
    /// The child's key. The child is the node under this key in the same
    /// Merk (RD5).
    pub key: &'a [u8],
    /// The child's hash.
    pub hash: &'a Hash,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The child's aggregate data (`tree-page.md` TP12).
    pub aggregate: &'a [u8],
}

/// The value of a written node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValuePut<'a> {
    /// The value the node has in the transaction's view when the write is
    /// applied: the write changes a hash or a link and leaves the value
    /// (`node-store.md`, the kinds `StructureChanged` and `HashOnly`).
    ///
    /// The hashes that are stated beside it were made for the value as the
    /// writer read it. Where that is a value of the committed state, the
    /// write says which read it was, by the node's handle
    /// ([`NodePut::at`]): a commit outside the transaction that wrote the
    /// node's key since that read meets the write, and a keep that names
    /// no read is refused
    /// ([`BatchError::NoRead`](super::BatchError::NoRead)). Where no node
    /// is under the key, the batch is refused: as a conflict where the
    /// transaction can tell that commits outside are the cause
    /// ([`BatchError::ValueTaken`](super::BatchError::ValueTaken)).
    /// A value the
    /// transaction stated itself is kept with no handle. A node the
    /// transaction has only written with its value kept has the committed
    /// value still, and a keep of it by a handle is dated as any other
    /// (`crate::txn`, "A kept value and the read it is of").
    Keep,
    /// The value. Its length alone says whether it is stored in the node's
    /// cell or out of line: that is the store's rule
    /// ([`InlinePolicy`](lignin_tree::value::InlinePolicy)), and not the
    /// writer's to choose. A value that goes out of line stays with the
    /// transaction until its commit carries it in the log group (WT4, FL7).
    Value(&'a [u8]),
}

/// The Merks a written node hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hosted<'a> {
    /// The Merks the node hosts already. A new node hosts none.
    ///
    /// It is for a write that leaves every hosted Merk as it is. A batch
    /// that holds the change set of a Merk and writes the node that hosts
    /// it names the Merk in the node ([`Self::Set`]): the pointer in the
    /// host's cell is written with the root it names. Where the cell shows
    /// that this was not done, the batch is refused
    /// ([`BatchError::HostKeeps`](super::BatchError::HostKeeps)). It does
    /// not show it for a Merk that had no root.
    Keep,
    /// The hosted Merks by position: the node's cell points at the root of
    /// each, as that Merk has it once the whole write is applied
    /// (`tree-layout.md` C3, P5).
    Set(&'a [Subtree]),
}

/// One written node of a Merk: the whole record, as Merk hands it to
/// storage (ST6). Every part is stated, and the cell is changed where it
/// differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodePut<'a> {
    /// The node's key: the record key of the write.
    pub key: &'a [u8],
    /// The node's feature type with its aggregate data.
    pub feature: &'a [u8],
    /// The node's value hash.
    pub value_hash: &'a Hash,
    /// The node's key-value hash.
    pub kv_hash: &'a Hash,
    /// The link to the left child.
    pub left: Option<LinkPut<'a>>,
    /// The link to the right child.
    pub right: Option<LinkPut<'a>>,
    /// The Merks the node hosts.
    pub hosted: Hosted<'a>,
    /// The node's value.
    pub value: ValuePut<'a>,
    /// The hashes of the node that its writer did not make: they are stale
    /// until they are evaluated, at the latest by the commit
    /// (`lignin_tree::hash`, `transactions.md` CM3 step 5). For a stale
    /// hash the write states the bytes the cell has, or any bytes in a
    /// node or a link that is new. A hash of a child is stale only where
    /// the node has a link on that side.
    ///
    /// [`Stale::NONE`] where the writer makes every hash itself, as Merk
    /// does on existing GroveVersions: nothing is marked there, and no
    /// rule runs at the commit.
    pub stale: Stale,
    /// Where the caller last saw the node, if it kept that. It is a hint:
    /// it is followed while it is good, and the node is found by its key
    /// otherwise (PN8).
    ///
    /// A handle is good for the cell it names, and a cell does not say
    /// which Merk it is in. So the handle is one that was read for this
    /// Merk: a good handle of another Merk whose node has the same key
    /// would be followed.
    ///
    /// For a node that keeps its value ([`ValuePut::Keep`]) the handle is
    /// more than a hint: it names the read the value is kept from, good or
    /// not. It is the handle this transaction showed the node with when
    /// the writer read the value it made its hashes for, or the one a
    /// write of this node reported since ([`Applied::nodes`]), and not a
    /// later place of the node's cell that is no read of it, such as a
    /// write reports for a cell it moved ([`Applied::carried`]).
    /// Where the value is the committed state's, a handle the transaction
    /// cannot date, such as one of another transaction, has the write
    /// taken to be met by a commit, and its commit fail.
    pub at: Option<NodeHandle>,
}

/// What a write does to the key of a Merk's root node.
///
/// The root key is a part of a record the engine does not read: of the
/// element that hosts the Merk, or of the base root record. So the writer
/// states it, and the engine keeps the Merk's directory entry by it
/// (`transactions.md` ST3, SC2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootPut<'a> {
    /// The root is the node it was.
    Keep,
    /// The node under this key is the root, or the Merk is empty. A root
    /// changes only with a node: a change set that sets it writes or
    /// deletes one
    /// ([`BatchError::RootAlone`](super::BatchError::RootAlone)).
    Set(Option<&'a [u8]>),
}

/// The change set of one Merk in one operation.
///
/// The data column of the Merk's subtree holds its nodes and no flat
/// record (ST1): a batch that writes both there is refused
/// ([`BatchError::NotFlat`](super::BatchError::NotFlat),
/// [`BatchError::NotMerk`](super::BatchError::NotMerk)). Whether the
/// committed state holds flat records under a subtree that had no Merk is
/// not looked up: which of the two a subtree holds is its writer's to know.
///
/// # Unlinked records
///
/// A change set states a record for each key, as a storage batch holds
/// one outcome for a key (GM0), and the tree is what its root reaches. The
/// two can differ: a batch that keeps a put of a node over a later delete
/// of it inside one operation can write a node that no link of its tree
/// leads to. An engine that keeps one record for a key holds that record,
/// shows it to a read by key and to a cursor, and has it in no hash. So
/// does this one:
///
/// - **A written node that the tree does not reach once the batch is
///   applied is kept as an unlinked record**: no cell, but a flat record
///   in the data column of the subtree, under the node's key, which
///   follows the subtree's entry in the directory as every flat record
///   does. A cell the node had leaves its tree. The node names no Merk
///   that it hosts
///   ([`BatchError::HostUnlinked`](super::BatchError::HostUnlinked)): what
///   its cell hosted is its batch's to drop, as for a node that is deleted.
///   It leaves no hash to be made
///   ([`BatchError::StaleUnlinked`](super::BatchError::StaleUnlinked)):
///   nobody makes a hash of an unlinked record. And with its value it is
///   no longer than the value of a flat record may be
///   ([`BatchError::ValueTooLong`](super::BatchError::ValueTooLong)).
/// - **Which written nodes the tree reaches** is found from the root the
///   change set states, through the links it states for the nodes it
///   writes and the links the cells have for all others. Under a root
///   that is kept, a written node that has a cell is taken to stay in its
///   tree, and a new node is in the tree where a written node of the tree
///   links it.
/// - **Only written nodes are looked at.** A cell that the batch neither
///   writes nor deletes, and that no node links any more, stays in its
///   page with no link to it, and no record is kept for it. So does a
///   written node with a cell that its parent stops linking under a root
///   that is kept. A commit of a Merk leaves neither: it states its root,
///   and puts or deletes every node that leaves its tree.
/// - **A key has one record.** A node that is linked under a key takes the
///   place of an unlinked record there. A delete of the key takes
///   whichever is there, and so does a drop or a clear of the subtree.
/// - **A node the transaction wrote keeps its value, in its tree or out
///   of it** ([`ValuePut::Keep`]). A node that leaves its tree and keeps
///   its value takes the value of the cell it had, by the read its write
///   names, as a node that stays does. A later write of that node that
///   keeps its value keeps the value of the record the transaction left,
///   whether the write links the node again or leaves it out of its tree:
///   the record is the node as the transaction last wrote it.
/// - **A committed unlinked record has no value for a write to keep**: it
///   is no node, and no read of it is dated. A node that keeps its value
///   under a key that has neither a node of the tree nor a record the
///   transaction left is refused
///   ([`BatchError::NoValue`](super::BatchError::NoValue), or
///   [`BatchError::ValueTaken`](super::BatchError::ValueTaken) where a
///   commit outside the transaction took the node away after the read the
///   write names), whatever record the committed state holds under the
///   key.
/// - **It is read as a node**: by its key where the tree has none, and
///   under a cursor over the Merk, in key order among the nodes. A read
///   that finds it in the pages, the transaction's or the committed ones,
///   shows it as an unlinked record
///   ([`NodeView::is_unlinked`](super::NodeView::is_unlinked)).
/// - **Whether the tree reaches a node is found when its batch is laid
///   out on the transaction's pages.** A batch that the transaction keeps
///   in its write set by key (one of a scope beside another scope, and
///   one of a transaction that serves its records by key) is laid out
///   later or not at all. Until then its nodes are the write set's
///   records of their keys, whether or not their tree reaches them, and
///   [`Applied`] names none of them.
///   [`BatchError::HostUnlinked`](super::BatchError::HostUnlinked),
///   [`BatchError::StaleUnlinked`](super::BatchError::StaleUnlinked) and
///   the [`BatchError::ValueTooLong`](super::BatchError::ValueTooLong) of
///   a record come when the batch is laid out.
///
/// A Merk that holds no such record pays a lookup of whether its subtree
/// holds any. When the lookup is made, and for how long its answer is
/// kept, is said where the records are kept (`txn/unlinked.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MerkWrite<'a> {
    /// The Merk's subtree.
    pub subtree: Subtree,
    /// The nodes the operation writes, each key once.
    pub nodes: &'a [NodePut<'a>],
    /// The keys of the nodes it deletes. A delete of a key with no node is
    /// a write all the same (ST5).
    pub removed: &'a [&'a [u8]],
    /// The Merk's root after the operation.
    pub root: RootPut<'a>,
    /// Whether the Merk has a host that this operation does not write, so
    /// that the root stays in its page until a later write names it in the
    /// host (`tree-layout.md` T3). Never on existing GroveVersions, where
    /// every write holds the path up to the root of the grove.
    pub anchored: bool,
}

/// The element that hosts an entered subtree: the node it is, and which of
/// that node's subtrees this one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPut<'a> {
    /// The subtree of the Merk the host is a node of. It is entered.
    pub subtree: Subtree,
    /// The host's key in that Merk.
    pub key: &'a [u8],
    /// The position of the entered subtree among those the host hosts:
    /// its place in the host's [`Hosted::Set`].
    pub position: u8,
}

/// What an operation does to a subtree as a whole: to its entry in the
/// subtree directory (`lignin_tree::directory`).
///
/// - **A subtree is entered before anything is written in it**, in the
///   same batch or in an earlier one: a change set or a flat record, in
///   any column, for a subtree that is not entered is refused
///   ([`BatchError::NotEntered`](super::BatchError::NotEntered)).
///   While the transaction serves its records by key (WT7b) a batch is
///   not checked against the directory: this is checked when its frames
///   are made, and at the latest its commit is refused.
/// - **Every subtree that a node hosts is entered**, a Merk with no node
///   and a tree that keeps its data in a flat keyspace among them: what is
///   not entered is not found below a dropped Merk.
/// - **A drop takes every subtree below with it**, and names none of them:
///   a batch drops the outermost. So does a clear.
/// - **A drop or a clear ends every node handle** that was read through
///   the transaction before it (PN8), and no hint of the batch that makes
///   it is followed ([`NodePut::at`]).
///
/// # The two enters
///
/// [`Self::Enter`] makes a subtree that is new: it is refused where the
/// subtree is entered in the transaction's view
/// ([`BatchError::Entered`](super::BatchError::Entered)), and a
/// transaction that entered a subtree which a commit beside it has entered
/// since cannot be laid out over that commit, so its own commit is
/// refused. It is the enter of a writer that makes the subtree with its
/// element, and of one that enters a subtree again after it dropped it: a
/// subtree that is entered so starts empty.
///
/// [`Self::EnterIfAbsent`] says that the subtree is in use from here on,
/// by whoever entered it. It is judged where the write is laid out, each
/// time it is: when the batch is applied, and again whenever the
/// transaction's writes are applied anew over another committed version
/// or after a rollback to a mark (WT7, WT8).
///
/// - Where the subtree is not entered in the view the batch is applied
///   on, it is entered, as [`Self::Enter`] enters it.
/// - Where it is entered there, with the tree type and the host the write
///   states, nothing is done: the subtree keeps its entry and all it
///   holds, and no page is changed for it. It is entered there by a
///   commit, by an earlier write of the transaction, or kept by a clear of
///   the same batch. A drop of the same batch comes first, as before any
///   enter: after it the subtree is absent, and is entered as one that is
///   new.
/// - Where it is entered there with another tree type, or under another
///   element or at another position of it, the batch is refused
///   ([`BatchError::EnteredOtherwise`](super::BatchError::EnteredOtherwise),
///   which has both). The tree types are the same where their bytes are,
///   and the hosts where the subtree, the key and the position are.
///
/// So of two transactions that each state it for one subtree, with one
/// tree type and one host, the second to commit finds the subtree that
/// the first entered when it is laid out over that commit, and its enter
/// refuses nothing. An enter is no key of the conflict rule (CF2), of
/// either kind. Whether the second commits is decided by the keys the two
/// wrote, and by what else its writes ask of the committed state: a Merk
/// that it gave its first node has to be empty still
/// ([`BatchError::RootTaken`](super::BatchError::RootTaken)).
///
/// For the rule of what a drop covers (CF5), the subtree is below the
/// host the write states from the write on. A commit beside the
/// transaction that drops or clears a subtree which that host was in or
/// below, as the transaction had the subtrees when it made the write,
/// meets the writes that the transaction made under the subtree from this
/// write on, until it enters the subtree again, and before that commit:
/// whether the subtree was entered then or not, and also where that
/// commit enters it again. A commit that takes the subtree with a subtree
/// which is none of those is not met by this: for one, where a commit
/// beside the transaction entered the subtree under an element of a Merk
/// beside the stated host's, and a later commit drops that Merk.
///
/// Between the scopes of one transaction the write is a write of the
/// subtree's entry, whether it finds the subtree or enters it (SC3): two
/// scopes that are open together do not both state it for one subtree.
///
/// While the transaction serves its records by key the write is not
/// judged, as no write to a subtree is (above). Where the transaction
/// had the subtree before the write, by its own earlier writes and the
/// committed state, it has that subtree still, under the host that one
/// has, whatever the write states: its reads go by that host, and so do
/// what its own drops and clears take and what its scopes hold against
/// each other. Where it had none, it has a subtree with what the write
/// states.
///
/// # The move
///
/// [`Self::Move`] gives a subtree another prefix, under another key of
/// the Merk its host is in, and takes the subtrees below it along. Its
/// rules are `transactions.md` MV1 to MV8 (section 7.5) and CF10. What a
/// caller states and is answered:
///
/// - **No node is copied.** The directory's records of the moved Merk,
///   and of each Merk below it that `below` names, are written under the
///   new prefix, and so is each of their flat records, in every column
///   ([`lignin_tree::drop::rekey`]). A Merk below that `below` does not
///   name leaves with the move if it has no node, as a drop takes it, and
///   refuses the batch if it has one; so does a Merk that `below` names
///   below one that leaves
///   ([`BatchError::MoveUnnamed`](super::BatchError::MoveUnnamed)).
/// - **The batch puts the node that hosts the subtree afterwards**, in
///   the Merk the host is in before and at the position the subtree had.
///   Whether the cell of that node points at the moved Merk is as it was
///   for the old host: the node names the new prefix among the Merks it
///   hosts ([`Hosted::Set`]) where the cell of the old host hosts a Merk
///   at that position, and hosts no Merk at the position where that
///   cell hosts none there, as the element of a tree that keeps its data
///   flat does
///   ([`BatchError::MoveHost`](super::BatchError::MoveHost) for each
///   mismatch). It states
///   nothing else of a prefix that one of its moves names, but the drop
///   of a subtree that is entered where one goes
///   ([`BatchError::Moved`](super::BatchError::Moved)). Its moves are
///   made together, after its drops and clears and before its enters.
/// - **The move is dated at `read`**, as a kept value is dated at its
///   read: a commit beside the transaction that wrote the node which
///   hosts the subtree, since that read, refuses the batch as a conflict
///   ([`BatchError::ValueTaken`](super::BatchError::ValueTaken)), and a
///   move of a subtree whose host is a node of the committed state names
///   a read ([`BatchError::NoRead`](super::BatchError::NoRead)).
/// - **Afterwards the transaction reads under the new prefix** what it
///   read under the old one, and no subtree under a prefix that a
///   subtree was taken from and that no move of the batch brought one
///   to. Frames that are made again make the move again, and write the
///   flat records again. A transaction that serves its records by key,
///   and a scope that keeps its writes in the write set, do not answer
///   for a move: each refuses a move, every read and write that a move
///   decides, and every drop and clear. MV7 and MV8 say with which
///   refusal: [`BatchError::MovedByKey`](super::BatchError::MovedByKey),
///   the one that stops the layout of the transaction's writes, or
///   [`BatchError::MoveBeside`](super::BatchError::MoveBeside).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtreeWrite<'a> {
    /// Enters the subtree, with no node: the operation writes the element
    /// that hosts it.
    Enter {
        /// The subtree.
        subtree: Subtree,
        /// The tree type, as bytes that are kept and given back: up to 255.
        tree_type: &'a [u8],
        /// The element that hosts the subtree, or `None` for the root of
        /// the grove and for a Merk that stands alone.
        host: Option<HostPut<'a>>,
    },
    /// Enters the subtree, with no node, if it is not entered, and leaves
    /// it as it is if it is entered with the same tree type under the same
    /// host: the operation writes into a subtree that it, or a commit
    /// beside it, may be the first to write into.
    EnterIfAbsent {
        /// The subtree.
        subtree: Subtree,
        /// The tree type, as for [`Self::Enter`].
        tree_type: &'a [u8],
        /// The element that hosts the subtree, as for [`Self::Enter`].
        host: Option<HostPut<'a>>,
    },
    /// Drops the subtree: the operation deletes the element that hosts it,
    /// or writes it without the subtree (`tree-layout.md` D1). No node of
    /// it is read. From here on the transaction reads no record under it,
    /// in any column, and none of a subtree below it.
    Drop {
        /// The subtree.
        subtree: Subtree,
    },
    /// Clears the subtree: a drop that keeps the host (D4). The subtree
    /// stays entered, with no node, and the same batch may write it.
    Clear {
        /// The subtree.
        subtree: Subtree,
    },
    /// States the subtree anew in the subtree directory. The subtree
    /// stays entered and keeps its nodes, its host and the subtrees below
    /// it, and is from here on a tree of `tree_type`, at `position` among
    /// the subtrees of its host. The statement changes the subtree
    /// directory and nothing else: no node, and no pointer in the cell of
    /// its host.
    ///
    /// It is what a conversion in place states beside its writes
    /// (GroveDB's `convert_to_big_sum_tree`): a tree whose element takes
    /// another type is neither dropped nor entered again, and an element
    /// that hosts several subtrees can name them in another order
    /// afterwards. The subtrees of one element that change places are
    /// stated by one batch: a place that one of them takes is free, or
    /// left by another of the batch. The cell of the host points at its
    /// subtrees in the new order where a batch puts the host's node and
    /// states what it hosts in that order ([`Hosted::Set`]), which is its
    /// caller's to do, in the batch that states the places.
    ///
    /// The subtree is entered before the batch, which neither enters nor
    /// drops nor clears it.
    Convert {
        /// The subtree.
        subtree: Subtree,
        /// The tree type it has from here on, as bytes that are kept and
        /// given back: up to 255.
        tree_type: &'a [u8],
        /// The position it has among the subtrees of its host from here
        /// on: its place in the host's [`Hosted::Set`]. `None` keeps the
        /// position it has, and is what a subtree with no host states.
        position: Option<u8>,
    },
    /// Moves the subtree to another prefix, under another key of the Merk
    /// its host is in, with the subtrees below it (`tree-layout.md` D5,
    /// D6): see [The move](#the-move).
    Move {
        /// The prefix the subtree is entered under before the batch.
        from: Subtree,
        /// The prefix it has afterwards.
        to: Subtree,
        /// The element that hosts it afterwards: a node that the same
        /// batch puts, in the Merk its host is in before, at the
        /// position the subtree had. The node names `to` among the Merks
        /// it hosts ([`Hosted::Set`]) where the cell of the node that
        /// hosts the subtree before hosts a Merk at that position, and
        /// hosts no Merk at the position where that cell hosts none
        /// there.
        host: HostPut<'a>,
        /// The handle the mover read the node with that hosts the
        /// subtree before the batch, or `None` where that node is a
        /// record the transaction wrote itself.
        read: Option<NodeHandle>,
        /// The prefix that each subtree below `from` has afterwards, by
        /// the prefix it has before the batch.
        below: &'a [(Subtree, Subtree)],
    },
}

/// A move of a subtree ([`SubtreeWrite::Move`], [`SubtreeWrite::moving`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moving<'a> {
    /// The prefix the subtree is entered under before the batch.
    pub from: Subtree,
    /// The prefix it has afterwards.
    pub to: Subtree,
    /// The element that hosts it afterwards.
    pub host: HostPut<'a>,
    /// The read of the node that hosts it before.
    pub read: Option<NodeHandle>,
    /// The prefixes of the subtrees below it, before and afterwards.
    pub below: &'a [(Subtree, Subtree)],
}

impl<'a> Moving<'a> {
    /// The move as the write it is.
    pub const fn write(&self) -> SubtreeWrite<'a> {
        let Self {
            from,
            to,
            host,
            read,
            below,
        } = *self;
        SubtreeWrite::Move {
            from,
            to,
            host,
            read,
            below,
        }
    }

    /// The prefixes the move takes a subtree from, if one is entered
    /// there: `from`, and the first of each pair.
    pub fn sources(&self) -> impl Iterator<Item = Subtree> + 'a {
        let below = self.below.iter().map(|(from, _)| *from);
        std::iter::once(self.from).chain(below)
    }

    /// The prefixes the move brings a subtree to: `to`, and the second
    /// of each pair.
    pub fn targets(&self) -> impl Iterator<Item = Subtree> + 'a {
        let below = self.below.iter().map(|(_, to)| *to);
        std::iter::once(self.to).chain(below)
    }

    /// Every prefix the move names, on either side.
    pub fn named(&self) -> impl Iterator<Item = Subtree> + 'a {
        self.sources().chain(self.targets())
    }
}

/// An enter of a subtree, of either kind ([`SubtreeWrite::entering`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entering<'a> {
    /// The subtree.
    pub subtree: Subtree,
    /// The tree type.
    pub tree_type: &'a [u8],
    /// The element that hosts the subtree.
    pub host: Option<HostPut<'a>>,
    /// Whether the subtree is entered only where it is absent
    /// ([`SubtreeWrite::EnterIfAbsent`]).
    pub if_absent: bool,
}

impl<'a> Entering<'a> {
    /// The enter as the write it is.
    pub const fn write(&self) -> SubtreeWrite<'a> {
        let Self {
            subtree,
            tree_type,
            host,
            if_absent,
        } = *self;
        match if_absent {
            false => SubtreeWrite::Enter {
                subtree,
                tree_type,
                host,
            },
            true => SubtreeWrite::EnterIfAbsent {
                subtree,
                tree_type,
                host,
            },
        }
    }
}

impl<'a> SubtreeWrite<'a> {
    /// Whether the write is an enter of its subtree, of either kind.
    pub const fn enters(&self) -> bool {
        matches!(self, Self::Enter { .. } | Self::EnterIfAbsent { .. })
    }

    /// Whether the write is a drop or a clear of its subtree: it takes
    /// the subtree out, or what its Merk holds.
    pub const fn leaves(&self) -> bool {
        matches!(self, Self::Drop { .. } | Self::Clear { .. })
    }

    /// The move the write is, or `None` for any other.
    pub const fn moving(&self) -> Option<Moving<'a>> {
        match *self {
            Self::Move {
                from,
                to,
                host,
                read,
                below,
            } => Some(Moving {
                from,
                to,
                host,
                read,
                below,
            }),
            _ => None,
        }
    }

    /// The enter the write is, of either kind, or `None` for any other.
    pub const fn entering(&self) -> Option<Entering<'a>> {
        match *self {
            Self::Enter {
                subtree,
                tree_type,
                host,
            }
            | Self::EnterIfAbsent {
                subtree,
                tree_type,
                host,
            } => Some(Entering {
                subtree,
                tree_type,
                host,
                if_absent: matches!(self, Self::EnterIfAbsent { .. }),
            }),
            Self::Drop { .. } | Self::Clear { .. } | Self::Convert { .. } | Self::Move { .. } => {
                None
            }
        }
    }

    /// The subtree the write names: for a move, the one it moves, by the
    /// prefix it has before the batch.
    pub const fn subtree(&self) -> Subtree {
        match *self {
            Self::Enter { subtree, .. }
            | Self::EnterIfAbsent { subtree, .. }
            | Self::Drop { subtree }
            | Self::Clear { subtree }
            | Self::Convert { subtree, .. }
            | Self::Move { from: subtree, .. } => subtree,
        }
    }
}

/// The writes of one operation (`transactions.md` section 1): one batch,
/// applied to the transaction as a whole or not at all (WT10), with one
/// first-write version (WT5).
///
/// The Merk change sets are those of every Merk the operation wrote
/// (`tree-layout.md` P5). The records are applied in the order they are
/// given, so a put after a range delete that covers its key stays. A record
/// key is written at most once in a batch, as a storage batch holds one
/// outcome for a key (GM0); which call survives for a key is the rule of
/// the batch above this one (TXN-08).
///
/// The parts of a batch are applied in this order, wherever they stand in
/// it: the drops and the clears, the moves, the subtrees that are stated
/// anew, the enters of both kinds in the order they are given, the change
/// sets, and the records. The moves of a batch are made together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Batch<'a> {
    /// The writes of flat records.
    pub records: &'a [RecordWrite<'a>],
    /// The change sets of the Merks.
    pub merks: &'a [MerkWrite<'a>],
    /// The subtrees the operation enters, drops, clears and moves.
    pub subtrees: &'a [SubtreeWrite<'a>],
}

impl Batch<'_> {
    /// Whether the batch writes nothing. Such a batch adds nothing to the
    /// write set (GM0).
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
            && self.subtrees.is_empty()
            && self.merks.iter().all(|merk| {
                merk.nodes.is_empty() && merk.removed.is_empty() && merk.root == RootPut::Keep
            })
    }

    /// Whether the batch drops or clears `subtree`.
    pub(crate) fn leaves(&self, subtree: Subtree) -> bool {
        (self.subtrees.iter()).any(|write| write.leaves() && write.subtree() == subtree)
    }

    /// The subtrees the batch drops and clears, in its order.
    pub(crate) fn leaving(&self) -> Vec<Subtree> {
        let leaves = (self.subtrees.iter()).filter(|write| write.leaves());
        leaves.map(SubtreeWrite::subtree).collect()
    }

    /// Whether the batch drops or clears a subtree.
    pub(crate) fn has_leaves(&self) -> bool {
        self.subtrees.iter().any(SubtreeWrite::leaves)
    }

    /// The moves of the batch, in its order.
    pub(crate) fn moves(&self) -> impl Iterator<Item = Moving<'_>> + '_ {
        self.subtrees.iter().filter_map(SubtreeWrite::moving)
    }
}

/// What the application of a batch reports: where the nodes it wrote are
/// (`tree-layout.md` P5, `engine.md` "The batch across subtrees").
///
/// A holder of node handles that takes this in never has to find a node of
/// its own transaction by key (S6). The handles say where a node is: one
/// the write wrote, or one whose cell it moved without writing its key.
/// The second kind is no read of the node: a write that keeps the
/// committed value of a node names the handle the node was read with
/// ([`NodePut::at`]). The first kind is the transaction's own node as the
/// write left it, and a later keep of that node can name it. The report
/// is empty while the transaction serves its records by key (WT7b): they
/// have no cells then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// The handles of the written nodes, in the order of the batch: the
    /// nodes of the first Merk, then those of the second, and so on. A
    /// node that is kept as an unlinked record has no cell and no handle,
    /// and is left out here ([`Self::unlinked`]).
    pub nodes: Vec<NodeHandle>,
    /// The nodes of the batch that are kept as unlinked records
    /// ([`MerkWrite`], "Unlinked records"), each by its place among the
    /// nodes of the batch, in ascending order. Empty for a batch of whose
    /// nodes every tree reaches each, and for a batch that is kept in the
    /// write set by key: what its trees reach is found when it is laid
    /// out.
    pub unlinked: Vec<usize>,
    /// Every cell the write carried to another page: where it was, and the
    /// node where it is now.
    pub carried: Vec<(CellAddress, NodeHandle)>,
}
