//! The write transaction: private copy-on-write frames over the latest
//! committed state, derived from a write set that is the truth.
//!
//! - [`WritePath`] is what a write transaction is made of: the versions of
//!   a database, its pages, the key sets of its commits ([`Commits`]), the
//!   flat keyspaces of its records ([`Keyspaces`]) and the keyspace of
//!   its subtree directory ([`lignin_tree::directory`]). A transaction is
//!   begun from it.
//! - [`WriteTxn`] is one write transaction, and [`Scope`] the handle a
//!   thread writes it through.
//! - [`Batch`] is the writes of one operation: flat records
//!   ([`RecordWrite`]) and the change sets of Merks ([`MerkWrite`]), whose
//!   nodes are named by their keys ([`NodePut`]).
//! - [`NodeView`] is a node as a transaction shows it, and [`Cursor`] walks
//!   the flat records of one unit, or the nodes of one Merk
//!   ([`MerkCursor`]), in key order. A [`ParkedCursor`] is a cursor of a
//!   scope that is kept apart from the scope between two calls.
//! - [`KeySet`] is the key set of a commit, which decides whether a commit
//!   outside a transaction has met one of its writes.
//!
//! `docs/lignindb/transactions.md` is the specification: RD3 to RD6 and
//! RD10 for what a transaction reads, section 7 for its write set, its
//! frames, the rebase and rollback, PN8 for locations that are kept, PV7
//! and ID1 for what its private copies and page ids hold back. The module
//! writes no file (WT4) and takes no version: the commit is
//! [`crate::commit`], and what it is handed is at the end of this page.
//!
//! No example runs here: a [`WritePath`] is made where the database is
//! opened, of the structures of one store, and a caller begins a
//! transaction with [`Db::begin`](crate::Db::begin), whose page has one.
//! The tests of this module make a write path over a store on disk
//! (`tests/store.rs`).
//!
//! # What a transaction reads
//!
//! | Read | Returns |
//! | --- | --- |
//! | [`WriteTxn::get`], [`WriteTxn::node`] | the transaction's newest write that covers the key, and with none the record of the latest committed state at the moment of the call (RD3) |
//! | [`WriteTxn::follow`], [`WriteTxn::child`] | the same record, through a node handle while that is good (PN8) |
//! | [`WriteTxn::subtree`] | what the subtree directory holds of a subtree ([`SubtreeView`]): the transaction's own enters, drops, clears and roots, and for a subtree it has not written the entry of the latest committed state |
//! | [`WriteTxn::committed_get`], [`WriteTxn::committed_node`] | the record of the latest committed state, whatever the transaction wrote there (RD10) |
//! | [`Cursor`], from [`WriteTxn::cursor`] for flat records and from [`WriteTxn::merk_cursor`] for the nodes of a Merk | the committed records of the version the cursor was opened on, merged with the transaction's writes as they are at each step (RD6) |
//!
//! Each call is one operation (RD4): it takes an operation guard, reads the
//! latest version once, and reads every committed record at that version.
//! The flat pages it reads borrow that guard (PN5,
//! [`FlatReader`], [`FlatWriter`]). What outlives the operation is bound to
//! no guard between two of them and reads nothing then: the frames of the
//! transaction, whose base pin keeps their version, and the committed side
//! of a cursor, which the cursor's pin keeps.
//! Between two calls a commit can land, and the next call sees it for every
//! key the transaction has not written. Nobody else reads the
//! transaction's writes: they are in its own memory and in private copies
//! of the buffer pool until a commit installs them (RD8).
//!
//! # The write set, and the two states of a transaction
//!
//! The write set is the ordered list of the transaction's writes (WT2): a
//! put or a delete of a flat record, a put or a delete of a Merk node, a
//! delete of a range. Each has the first-write version of its operation
//! (WT5) and a mark that says whether a commit outside the transaction has
//! met it since (CF2). All bytes of the list are in one arena, and an index
//! beside it gives the newest write that covers a key and the written keys
//! of a unit in order. The index is derived from the list, and holds one
//! boxed key for each key that was written.
//!
//! **The frames are derived from the write set** (WT3). They are private
//! copies of tree pages and of flat pages in the buffer pool, over the
//! committed pages of a base version that the transaction keeps pinned
//! (PN1). Applying the write set's operations, in order, to the committed
//! state of the base gives them, so they can be given up and made again at
//! any time. A transaction is in one of these states:
//!
//! | State | When | Reads | Writes |
//! | --- | --- | --- | --- |
//! | idle | the write set is empty | committed pages at the latest version; no pin, no copy, no page id (PN1) | make the frames |
//! | framed | no write is met, and the frames are over the latest version | the frames: a node is where its handle says, or where its key leads from the root of its Merk | change the frames through the tree layer and the flat keyspaces, then add to the write set |
//! | by key | a write is met (WT7b), or the write set cannot be laid out | each record by its key: from the write set if the transaction wrote the key, and otherwise from the committed tree of its subtree, from the committed root | add to the write set, and nothing else |
//!
//! **An operation brings the transaction to the latest version** (WT7).
//! It takes in the key sets of the commits that were published since the
//! transaction last looked, and marks the writes they meet. Then:
//!
//! - *No write is met* (WT7a). Frames over another version are given up
//!   and made again over the latest one: every operation of the write set
//!   is applied again, with every cell found by its key. The records are
//!   what they were; the pages need not be.
//! - *A write is met* (WT7b). The frames are given up and not made again.
//!   Before the base pin goes, the transaction takes its own copy of every
//!   committed value that a write of it kept (WT3). It then serves its
//!   records exactly as they are, pins nothing, and its commit fails with
//!   a conflict unless a rollback to a mark takes the met writes out.
//!
//! No speed target applies to either (WT7c): the first costs a keyed
//! descent for each node of the write set, and the second makes every read
//! a keyed lookup, which is what the owner ruled for a transaction that a
//! commit has overlapped.
//!
//! **An operation that fails adds nothing** (WT10). A batch is checked
//! before a page is changed, and the tree layer plans a write before it
//! makes it. Where an operation fails after it has changed a page, the
//! frames are made again from the write set, which the batch was never
//! added to.
//!
//! # A batch of Merk nodes
//!
//! A change set names its nodes, their children and its root by key, as
//! Merk hands records to storage (ST6), and the transaction finds what
//! each key names: a node of the same batch, or a cell that is found by a
//! hint that is still good ([`NodePut::at`]) or by its key. All change sets
//! of one batch are one write of the tree layer
//! ([`Fragments::apply`](lignin_tree::fragment::Fragments::apply)), so a
//! host points at the root of the Merk it hosts as the whole batch leaves
//! it (`tree-layout.md` P5). [`Applied`] gives the caller the handles of
//! the nodes it wrote.
//!
//! # Subtrees and the directory
//!
//! Where the root of a Merk is, the subtree directory says
//! ([`lignin_tree::directory`]): the transaction opens every Merk through
//! it, and tells it what its writes do.
//!
//! - **A batch says which subtrees it enters, drops and clears**
//!   ([`SubtreeWrite`]), as the caller that writes the hosting elements
//!   knows them. A change set and a flat record, in any column, are of a
//!   subtree that is entered ([`BatchError::NotEntered`]).
//! - **No record of a subtree is read without its entry.** In every
//!   column the committed records under a prefix belong to the entry the
//!   directory has under that prefix, so where the version that is read
//!   has no live entry, nothing is read: not a node, not a flat record,
//!   and not under a cursor. An entry that was found is kept for its
//!   version, and what the transaction itself dropped since is asked of
//!   its write set at every read.
//! - **Every root that one write changed is told in one statement**
//!   ([`Directory::set_roots`](lignin_tree::directory::Directory::set_roots)),
//!   from the report of the write: the Merks of the batch whose change set
//!   sets a root by their name, and every root that was carried by the
//!   address it had, also of a Merk the batch did not open
//!   (`tree-layout.md` S3). The cell one root left can be the cell another
//!   is in afterwards, so they are resolved together, against the entries
//!   as they were before the write.
//! - **A handle on the directory is of one version, and none is loaded
//!   here.** The engine keeps the handle of every version that is read
//!   ([`Directories`](crate::snapshot::Directories)), and a transaction
//!   takes copies ([`WritePath::directory_at`]): the frames one of their
//!   base when they are made, so again after a commit of another landed
//!   and after every rollback; the committed records beside them one of
//!   the committed version of that read; and a cursor one of its own
//!   version. A handle that was loaded late, after a later commit landed
//!   and what a drop left was reclaimed, would take a subtree that was
//!   below the dropped one for a subtree in use.
//! - **A drop is one write** (`transactions.md` CV9, CF5): one entry of
//!   the write set, a delete of every key of every column under the
//!   subtree, which is in the key set of the commit as the subtree. A
//!   clear is one entry as well, a delete of every key of the data column;
//!   what else a clear takes is GROVE-05's to state. The Merks below go
//!   with either, and neither names them:
//!   - what is committed below is not read from the drop on, because no
//!     open returns it: the frames' directory has no live entry for it,
//!     and what is read beside the frames is not read under a Merk the
//!     transaction dropped or cleared;
//!   - what the transaction itself had written below is covered by the
//!     entry of the drop, which names those subtrees, so that it is gone
//!     also where the same transaction enters the subtree again.
//! - **A drop ends every handle that was read before it** (PN8): the
//!   cells of the Merk stay in their pages, and a handle into them would
//!   still be followed.
//! - **A drop, a clear and an enter are the tree layer's**
//!   ([`lignin_tree::drop`]). A batch that drops or clears is one
//!   [`drop::unlink`](lignin_tree::drop::unlink): the write to the tree
//!   pages with the root of each Merk named as dropped, the Merk's records
//!   in the directory, and the range of each dropped Merk in every flat
//!   column, by the lazy range delete. No node of the Merk is read, and no
//!   flat page between the two ends of a range: those pages are named in a
//!   record of a keyspace of their own, which the write path names as it
//!   names the directory's, and the reclaimer frees them. A subtree is
//!   entered through [`drop::enter`](lignin_tree::drop::enter), which
//!   takes out what an entry that a drop left under the prefix has, so a
//!   subtree starts with no flat record whatever was under its prefix
//!   before. A clear keeps the cleared Merk's own aux, roots and meta
//!   records, and takes its data.
//! - **A drop is given every column** ([`Keyspaces::columns`]) and the
//!   version the frames are built over. A record of unlinked pages says
//!   which version its writer read at, so frames that are made again over
//!   another base make the drop and the enter again with that base: they
//!   are made from the write set, by the same calls.
//! - **What was left is handed to the commit** (`Parts::dropped`).
//!   Reclaiming it is background work, which the engine starts (TXN-07).
//!
//! Three things a batch may not say, beside what it names that is not
//! there:
//!
//! - **Nodes and flat records in one data column.** The data column of a
//!   subtree holds the nodes of a Merk or flat records (ST1). A flat write
//!   where the batch, the write set or the state the transaction reads
//!   has nodes is refused ([`BatchError::NotFlat`]), and so is a change
//!   set where the write set has flat records ([`BatchError::NotMerk`]).
//! - **A root with no node.** A change set that sets its root writes or
//!   deletes a node ([`BatchError::RootAlone`]).
//! - **A host that keeps its pointers beside the Merk it hosts.** The
//!   node that hosts a Merk of the same batch names it ([`Hosted::Set`],
//!   [`BatchError::HostKeeps`]).
//!
//! A value is kept ([`ValuePut::Keep`]) or stated ([`ValuePut::Value`]).
//! Whether a stated value is stored in its cell or out of line is the
//! store's rule, by the value's length alone
//! ([`InlinePolicy`](lignin_tree::value::InlinePolicy),
//! [`WritePath::inline_policy`]). The bytes of a value out of line stay
//! with the transaction's frames until its commit carries them in the log
//! group and attaches them to the frame of their page (WT4, FL7). Every
//! rebuild of the frames states the write set to values that have not
//! seen it.
//!
//! **Stale hashes** (`lignin_tree::hash`). A node is written with the
//! hashes its writer made, and says which it did not make
//! ([`NodePut::stale`]). The frames keep those marks: they follow every
//! write of the tree pages, a later write of the node with the hash made
//! takes its mark off, and [`NodeView::stale`] shows them to a reader. The
//! marks are of the frames' pages, so they are made again from the write
//! set whenever the frames are, which is also what a rollback to a mark
//! does. The commit is handed them and evaluates them all (CM3 step 5).
//! Where every hash is made by its writer, as on existing GroveVersions,
//! nothing is marked, a write does nothing for them, and no rule runs.
//!
//! # Locations that are kept
//!
//! A [`NodeHandle`](lignin_tree::node::NodeHandle) is good under the lease
//! it was read under, while no cell has left its page. The transaction
//! takes a new lease whenever a page id could name another page, or a frame
//! could be another frame: when the latest version moves while it has no
//! frames, when its frames are made again (a rebase, a rollback to a mark,
//! a failed operation), and when it is rolled back. A handle of another
//! lease is not followed, and the node is found by its key. No address is
//! handed out that is not inside a handle.
//!
//! # Scopes and threads
//!
//! A transaction is `Send` and `Sync`. Its writes go through a [`Scope`],
//! and any number of scopes may be open, each on a thread of its own:
//! [`crate::scope`] says what a scope claims, how it merges or is rolled
//! back, and what the transaction keeps for its scopes. This module is
//! what a scope that is alone writes through, and what every scope reads
//! through. Savepoints are marks kept in a stack ([`crate::savepoint`]):
//! [`Mark`] and the rollback to one are what they build on.
//!
//! Every call holds the transaction's own lock (LT7 level 2) for its
//! length, and the function a node is shown to runs under it: it must not
//! call the transaction.
//!
//! # What the commit takes
//!
//! `WriteTxn::seal` hands the transaction to its commit
//! ([`crate::commit`]), which holds the commit lock (CM3 steps 1 to 4):
//! the commits published since are taken
//! in, a met write is the conflict (CF2), and otherwise the frames are over
//! the latest version and are handed over with the key set of the commit.
//! The commit installs the tree copies, each with the run of delta records
//! that its changes made, and the flat writer's copies, publishes the page
//! ids, frees the freed pages, records the key set ([`Commits::record`])
//! and then publishes the version. The key set is recorded first, so that
//! a transaction that reads a version finds the key set of every commit up
//! to it.
//!
//! # What a transaction holds
//!
//! - **A version pin** on its base while it has frames, and none while it
//!   is idle or serves its records by key. Each cursor has a pin of its
//!   own, and a cursor over a Merk a lease of its own beside it.
//! - **Private copies** of the pages it changed, made or freed. The pool
//!   cannot evict them, and no page step changes a page under one (PV7).
//!   A copy of a tree page is asked for with
//!   [`make_private`](lignin_store::cache::BufferPool::make_private), and
//!   the flat keyspaces' writer asks for its own with
//!   [`make_private_once`](lignin_store::cache::BufferPool::make_private_once):
//!   one form for each kind of page.
//!   Their number is reported ([`TxnStats`]) and not bounded: nothing is
//!   spilled to a file (WT4 allows it, and does not ask for it).
//! - **Page ids** of the pages it made, until its commit publishes them or
//!   its frames are given up (ID1).
//! - **A watch on the key sets of commits** while it has a write
//!   ([`Commits`]).
//! - **A place among the open write transactions of the store's counts**
//!   ([`WritePath::stats`]), from its first write until its write set is
//!   empty again: by its commit, a rollback, a rollback to a savepoint
//!   that takes every write out, or its drop. That is the time in which it
//!   can hold private copies, which is what holds the log
//!   (`lignin_store::stats`, "What an operator watches"). A transaction
//!   that only reads is not counted.
//!
//! # Choices made here that the plan left open
//!
//! 1. **A transaction that is overlapped has no frames.** It serves every
//!    record by key from its write set, as RocksDB serves a transaction's
//!    batch, and is laid out again only if a rollback empties the overlap
//!    set.
//! 2. **The key sets of commits are kept until every transaction with
//!    writes has taken them** ([`Commits`]). The other way, marking the
//!    met writes at the commit, makes the committer take each
//!    transaction's lock. The commit keeps CF3 this way
//!    ([`crate::commit`], "The conflict rule").
//! 3. **The subtree directory is written through the transaction's own
//!    flat pages**, in the operation that changes it, so its records
//!    commit, roll back and are made again with the frames. A batch is
//!    checked against it before a page is changed, so that a batch which
//!    names what is not entered is refused with every page as it was; what
//!    the directory itself refuses afterwards costs the frames, which are
//!    made again.
//! 4. **The flat records are in one tree for each column** ([`Keyspaces`]),
//!    under the subtree's 32 bytes and the key. Where the roots of those
//!    trees are kept is TXN-07's.
//! 5. **A Merk's nodes are written by key, with handles as hints.** A node
//!    of a batch is found by descending from the root of its Merk unless
//!    the caller passes a handle that is still good.
//! 6. **A node is shown to a function**, not returned: the view borrows a
//!    latched frame or the write set, and no latch is held when a call
//!    returns (LT3).
//! 7. **The cursor stands on a key.** A write under an open cursor is
//!    defined: the cursor reads what is under its key now, and steps on
//!    from the key. This tightens RD6, which left it unspecified. A scope
//!    takes a write through a shared reference, so that its own cursor
//!    can be open across it, and is one thread's at a time.
//! 8. **A transaction that gave a root to a Merk that had none is not
//!    laid out over a root that a commit outside made since**
//!    ([`BatchError::RootTaken`]). No writer of an existing GroveVersion
//!    gets there, because both would have written the element that hosts
//!    the Merk (GM3). The rule for iteration two is GROVE-11's.
//! 9. **A node's value is read where it is, by the tree layer's read
//!    path** ([`NodeView::value`], `lignin_tree::value`): in its cell, in
//!    the value log, with the frame of its page while it is pending, or
//!    with the transaction that wrote it. The frames keep the values of
//!    their cells in a [`TreeValues`](lignin_tree::value::TreeValues),
//!    which is what the commit logs, attaches and counts from.
//!
//! # What is not here
//!
//! - **The record Merk's callers expect from a cursor**, which Merk's
//!   store builds (`merk::store::lignin`). A cursor over a Merk shows the
//!   node ([`MerkCursor::node`]): the transaction's own, or the one that
//!   the ordered cursor of the tree layer ([`lignin_tree::cursor`]) stands
//!   on at the cursor's version. A record names each child by its key,
//!   which a cell does not hold, so the cursor also reads the keys of the
//!   node's children with it ([`MerkCursor::node_with_children`],
//!   [`ChildKeys`]): for a node of its committed side from the children's
//!   cells at its own version, which no read of the scope reaches.
//! - **Starting a reclaimer** (CL6): what a drop left is reclaimed by
//!   `lignin_tree::drop::Reclaimer`, which the engine runs on a thread of
//!   its own, with the log of its page steps ([`crate::db`], "Background
//!   work").
//! - **While the transaction serves its records by key**, a batch is not
//!   checked against the directory: what it says of subtrees is checked
//!   when the frames are made, and its commit is refused then.
//! - **A flat write among the nodes of a Merk** is refused
//!   ([`BatchError::NotFlat`]).
//! - **Scopes, claims and pending marks**, and the conflict rule for what
//!   a drop covers below the subtree it names (CF5): [`crate::scope`].
//!   [`MerkWrite::anchored`] is passed on to the tree layer, and the frames
//!   answer it which hosts are behind from the marks.

mod arena;
mod check;
mod commits;
mod cursor;
mod error;
mod frames;
mod key;
mod opened;
mod pages;
mod path;
#[allow(clippy::module_inception)]
mod txn;
mod view;
mod write;
pub(crate) mod write_set;

/// A page store on disk with its log and its write-back, as the open of the
/// engine will make them: the store of this crate's own tests.
///
/// It is for the tests of crates that build on a write transaction before
/// the engine can be opened (TXN-07): [`testing::Db::new`] makes a store,
/// [`testing::Db::path`] its write path, and [`testing::Db::commit`]
/// commits a transaction through the commit of this crate
/// ([`crate::commit`]), which appends a group to the store's log. It runs
/// no write-back loop, and it panics where a database would return an
/// error other than a conflict or a refusal that leaves the transaction
/// open.
#[cfg(all(feature = "testing", not(loom)))]
#[path = "tests/store.rs"]
#[expect(
    clippy::should_implement_trait,
    reason = "the generator of the tests has a `next` and is no iterator"
)]
pub mod testing;

#[cfg(all(test, not(loom)))]
pub(crate) mod tests;

pub use commits::Commits;
pub use cursor::{CommittedSide, Cursor, FlatSide, MerkCursor, MerkSide, ParkedCursor, Resumed};
pub use error::{BatchError, TxnError};
pub use key::{Column, KeySet, Subtree, Unit};
pub(crate) use path::Store;
pub use path::{FlatReader, FlatWriter, Keyspaces, WritePath};
pub(crate) use txn::Held;
pub use txn::{Scope, TxnStats, WriteTxn};
pub use view::{ChildKeys, LinkView, NodeView, SubtreeView};
pub use write::{
    Applied, Batch, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut,
    SubtreeWrite, ValuePut,
};
pub use write_set::Mark;
pub(crate) use {
    frames::FlatFrames,
    pages::{TreeCopy, TreePages},
    txn::{Parts, Sealed},
};

/// The write half of a snapshot being read-only (`transactions.md` RD1,
/// T-RD5): the entry point that writes through a transaction answers
/// [`SnapshotError::ReadOnly`](crate::snapshot::SnapshotError::ReadOnly)
/// for a snapshot, and changes nothing.
impl crate::snapshot::Snapshot<'_> {
    /// Refused: a snapshot is read-only.
    pub fn apply(&self, batch: &Batch<'_>) -> Result<Applied, crate::snapshot::SnapshotError> {
        let _ = batch;
        Err(crate::snapshot::SnapshotError::ReadOnly)
    }
}
