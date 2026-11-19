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
//!   ([`MerkCursor`]), in key order.
//! - [`KeySet`] is the key set of a commit, which decides whether a commit
//!   outside a transaction has met one of its writes.
//!
//! `docs/lignindb/transactions.md` is the specification: RD3 to RD6 and
//! RD10 for what a transaction reads, section 7 for its write set, its
//! frames, the rebase and rollback, PN8 for locations that are kept, PV7
//! and ID1 for what its private copies and page ids hold back. The module
//! writes no file (WT4) and takes no version: the commit is TXN-05's, and
//! what it is handed is at the end of this page.
//!
//! No example runs here: a [`WritePath`] is made where the database is
//! opened, of the structures of one store (TXN-07). The tests make one over
//! a store on disk (`tests/store.rs`).
//!
//! # What a transaction reads
//!
//! | Read | Returns |
//! | --- | --- |
//! | [`WriteTxn::get`], [`WriteTxn::node`] | the transaction's newest write that covers the key, and with none the record of the latest committed state at the moment of the call (RD3) |
//! | [`WriteTxn::follow`], [`WriteTxn::child`] | the same record, through a node handle while that is good (PN8) |
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
//! - **A handle on the directory is of the pages it was loaded from.** The
//!   frames load theirs when they are made, so it is read again after a
//!   commit of another landed and after every rollback. The committed
//!   records beside them are read through a handle of the committed
//!   version of that read, and a cursor through one of its own version.
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
//! - **What a drop writes** is the root of the Merk named as dropped in
//!   the write of the tree pages
//!   ([`Write::dropped`](lignin_tree::fragment::Write::dropped)), the
//!   Merk's records in the directory, and a range delete of each flat
//!   keyspace under its prefix. No node of it is read. Reclaiming what it
//!   left is TREE-09's; the commit is handed what was left
//!   (`Parts::dropped`).
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
//! and it has one scope at a time: a second one, and a read outside the
//! open one, are refused at once with [`TxnError::ScopeOpen`], as a claim
//! never waits (SC3). A scope merges or is rolled back, and the rollback is
//! a rollback to the mark the scope took (SC6). Claims, scopes that run at
//! the same time and their reconciliation are TXN-09's, and savepoints,
//! which are marks kept in a stack, are TXN-04's: [`Mark`] and the
//! rollback to one are what both build on.
//!
//! Every call holds the transaction's own lock (LT7 level 2) for its
//! length, and the function a node is shown to runs under it: it must not
//! call the transaction.
//!
//! # What the commit takes
//!
//! `WriteTxn::seal` hands the transaction to its commit, which holds the
//! commit lock (CM3 steps 1 to 4): the commits published since are taken
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
//!    transaction's lock. TXN-05 owns how CF3 is kept and can change it
//!    behind the same calls.
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
//! - **The record Merk's callers expect from a cursor**, which is built
//!   from a node and its children's cells (MERK-05). A cursor over a Merk
//!   shows the node ([`MerkCursor::node`]): the transaction's own, or the
//!   one that the ordered cursor of the tree layer
//!   ([`lignin_tree::cursor`]) stands on at the cursor's version.
//! - **Reclaiming what a dropped Merk left** (CL6), and the lazy forms of
//!   the drop: TREE-09's `drop::unlink` does what the frames do for a Merk
//!   that leaves, and its `FlatDrops::unlink_range` unlinks the flat pages
//!   of a range unread where the range delete here reads every leaf it
//!   frees. Both take the place of the calls in `frames.rs` that name
//!   them.
//! - **A subtree that is entered again under the prefix of one that was
//!   below a dropped subtree** starts with the flat records of before,
//!   once it is committed and until the reclaimer has come: the drop reads
//!   nothing below the Merk it drops, and so deletes nothing there. An
//!   enter over a left-over entry has to unlink that prefix in every flat
//!   column (TREE-09's `drop::enter`). Inside the transaction that enters
//!   it, they are not read. The subtree that was dropped itself starts
//!   empty: its prefix is deleted with the drop.
//! - **The conflict rule for a write below a dropped subtree that writes
//!   nothing in the dropped subtree itself** (CF5): the key set of a drop
//!   names the dropped subtree, and those below it that the dropping
//!   transaction had written. A commit outside that writes another one
//!   below does not meet the drop. The other way is caught late: a
//!   transaction that wrote below a subtree which a commit outside dropped
//!   cannot lay its writes out any more. It serves its own records by key
//!   from then on, and its commit is refused
//!   ([`BatchError::NotEntered`]) and not failed as a conflict. On
//!   existing GroveVersions neither happens: a write below also writes the
//!   element that hosts its subtree, which is a key of the dropped one.
//! - **While the transaction serves its records by key**, a batch is not
//!   checked against the directory: what it says of subtrees is checked
//!   when the frames are made, and its commit is refused then.
//! - **A flat write among the nodes of a Merk** is refused
//!   ([`BatchError::NotFlat`]).
//! - **Pending marks and a host that is behind** (SC9): no element is
//!   marked on existing GroveVersions, and the frames answer that no host
//!   is behind. [`MerkWrite::anchored`] is passed on to the tree layer.

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
mod write_set;

/// A page store on disk with the parts of a commit that install a
/// transaction's frames and publish its version: the store of this module's
/// own tests.
///
/// It is for the tests of crates that build on a write transaction before
/// the engine can be opened (TXN-07) and before the commit exists (TXN-05):
/// [`testing::Db::new`] makes a store, [`testing::Db::path`] its write path,
/// and [`testing::Db::commit`] commits a transaction. It writes no log, and
/// it panics where a database would return an error.
#[cfg(all(feature = "testing", not(loom)))]
#[path = "tests/store.rs"]
#[expect(
    clippy::should_implement_trait,
    reason = "the generator of the tests has a `next` and is no iterator"
)]
pub mod testing;

#[cfg(all(test, not(loom)))]
mod tests;

pub use commits::Commits;
pub use cursor::{CommittedSide, Cursor, FlatSide, MerkCursor, MerkSide};
pub use error::{BatchError, TxnError};
pub use key::{Column, KeySet, Subtree, Unit};
pub use path::{FlatReader, FlatWriter, Keyspaces, WritePath};
pub use txn::{Scope, TxnStats, WriteTxn};
pub use view::{LinkView, NodeView};
pub use write::{
    Applied, Batch, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut,
    SubtreeWrite, ValuePut,
};
pub use write_set::Mark;

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
