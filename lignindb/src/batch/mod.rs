//! The batch across subtrees: the writes that the handles of one or more
//! operations stage, held as one outcome for each key until they are
//! applied to a write transaction together.
//!
//! - [`StagedBatch`] takes the calls: puts and deletes of flat records in
//!   the four columns, puts and deletes of Merk nodes, the root of a Merk,
//!   what an element hosts, and the enter, the drop and the clear of a
//!   subtree.
//! - [`Resolved`] is the batch when no more calls are made on it: the
//!   [`Outcome`] of each key, in the order a storage batch is charged in,
//!   and the one [`txn::Batch`](crate::txn::Batch) they are.
//!
//! It keeps the rules of `StorageBatch` (`storage/src/storage.rs`), which
//! `docs/lignindb/transactions.md` states as GM0, and whose unit tests run
//! against it (`tests/storage_batch.rs`). What a transaction is handed is
//! one batch with each key once (`txn::Batch`), applied as one operation:
//! one read of the latest version and one first-write version (RD4, WT5).
//!
//! ```
//! use lignindb::{
//!     batch::StagedBatch,
//!     txn::{Column, Subtree, Unit},
//! };
//!
//! let aux = Unit::new(Column::Aux, Subtree::new([7; 32]));
//! let batch = StagedBatch::new();
//! // Inside one operation a put wins over a delete of its key.
//! batch.put(aux, b"kept", b"value", ());
//! batch.delete(aux, b"kept", ());
//! // Across the boundary the later operation wins.
//! batch.put(aux, b"gone", b"value", ());
//! batch.next_operation();
//! batch.delete(aux, b"gone", ());
//!
//! let resolved = batch.resolve().expect("no call was refused");
//! let puts: Vec<bool> = resolved.outcomes().map(|outcome| outcome.is_put()).collect();
//! assert_eq!(puts, [false, true]);
//! resolved.with_batch(|batch| assert_eq!(batch.records.len(), 2));
//! ```
//!
//! # One outcome for a key
//!
//! A batch is no log. It counts **operation generations**:
//! [`StagedBatch::next_operation`] starts a new one, and a caller that
//! runs several operations against one batch calls it between them.
//!
//! | Call | Stands when | Otherwise |
//! | --- | --- | --- |
//! | put, of a record or a node | always: it takes the place of whatever the batch holds for the key | |
//! | delete | the batch holds nothing for the key, or an outcome of an earlier generation, which it takes the place of | it is dropped, and the outcome of the same generation stays |
//!
//! So inside one generation a put wins over a delete of the same key in
//! whichever order they came, and of two deletes the first stays, with
//! what it attached. Across a boundary the later operation wins. The
//! column is a part of the key, and so is the subtree.
//!
//! **The order of the calls of one operation does not matter**, where no
//! two of them say the same thing of the same key: every rule below takes
//! the calls of an operation together. Of two puts of one key, two
//! statements of what one element hosts and two elements that name one
//! subtree, the later stands.
//!
//! [`StagedBatch::merge`] makes again, in the generation this batch is in,
//! the calls that the other batch holds an outcome of: so a delete of the
//! other does not take the place of an outcome of that generation.
//! [`StagedBatch::merge_overwriting`] makes the other batch a later
//! operation, and what follows later still.
//!
//! Each call attaches a value of the batch's type parameter, which stays
//! with the outcome if the call stands and goes with it if it does not: it
//! is where the layer above keeps what the write is charged, so that only
//! the survivor is charged (`cost-model.md` B1). The engine does not look
//! at it, and drops it under the batch's lock: its `Drop` must not call
//! the batch.
//!
//! # What a node adds to a record
//!
//! A node is put as Merk hands it to storage: every part stated
//! ([`NodePut`]), and the batch holds the node as the outcome of its key.
//! Two parts of a node are stated against what is there before the put,
//! and a batch has to say what that is:
//!
//! - **A node that keeps its value**
//!   ([`ValuePut::Keep`](crate::txn::ValuePut::Keep)) keeps the value the
//!   node has in the batch before its operation: the value of the node the
//!   batch holds under the key, and with none the value the transaction
//!   has when the batch is applied. So a Merk that stays open across
//!   operations writes a value once and then only hashes, and the value is
//!   not lost with the put it came in. Where an earlier operation of the
//!   batch removed the node, there is no value to keep, and the batch is
//!   refused ([`BatchError::NoValue`]), as the transaction would refuse the
//!   second of the two operations. A delete of the same operation does not
//!   count, in either order: the put wins over it, and keeps what the node
//!   had without it. A drop or a clear of the same operation does count, in
//!   either order: it comes before the put, which then keeps the value the
//!   transaction has, and that is none.
//! - **The Merks a node hosts** are kept by the element
//!   ([`StagedBatch::host`]), and a put that states them
//!   ([`Hosted::Set`](crate::txn::Hosted::Set)) is such a statement. A put
//!   that keeps them names what its element was last said to host. What
//!   was said of an element is said of the node under its key until an
//!   operation removes that node: an element that the batch deletes hosts
//!   nothing, and a node that a later operation puts under the key is
//!   new, and hosts what is said from that operation on.
//!
//! The root of a Merk ([`StagedBatch::set_root`]) is held as a record is:
//! a root key is a put, and "no root" is a delete. So it stays in step
//! with the node it names, by the same rule. A root that is stated for a
//! Merk of which the batch holds no node says nothing, and is left out.
//!
//! # Hosts, and what is anchored
//!
//! A write of a transaction names, in each node that hosts a Merk of the
//! same write, the Merks it hosts, and says of every other Merk with a
//! host that it is anchored (`tree-layout.md` P5, T3). The batch works
//! both out when it is resolved, from what it was told of each element:
//!
//! - a node that is put names the Merks its element was last said to
//!   host, and keeps what it hosts where nothing was said;
//! - a Merk with an outcome for a node is anchored where an element was
//!   said to host it and the batch holds no put of that element's node;
//! - a subtree that is entered has that element as its host, at its place
//!   in the list, and is entered after the subtree its host is in where
//!   the batch enters both.
//!
//! So the caller says what an element hosts whenever it writes a Merk the
//! element hosts or the element itself, with the whole list, by position.
//! A Merk of which nothing is said stands alone, as the root of a grove
//! does. The pointer in a host that the batch does not write is behind
//! once an anchored Merk has another root, until a later batch of the
//! transaction writes the host (T3): never on existing GroveVersions,
//! where every operation writes the path up to the root of the grove.
//!
//! # Subtrees
//!
//! A batch holds for a subtree at most one leave, a drop or a clear of
//! the subtree as the transaction has it, and one enter, and they are
//! applied in that order (`txn::Batch`).
//!
//! - **A drop or a clear takes the place of what earlier operations
//!   staged in the subtree**: every column for a drop, the data column and
//!   the root for a clear. What the same operation stages there stands,
//!   and is applied after it, as a put wins over a delete inside one
//!   operation.
//! - **It takes what earlier operations staged below the subtree too**, in
//!   every column, with their enters, drops and clears: a drop names the
//!   outermost subtree. Below is what hung there before the operation and
//!   what the operation itself says hangs there
//!   ([`StagedBatch::host`]). A write below that the batch was not told of
//!   stays, and the transaction refuses the batch
//!   ([`BatchError::NotEntered`]).
//! - **A drop takes back the enter of an earlier operation**: the subtree
//!   was the batch's own, and the transaction never sees it. A clear of
//!   such a subtree leaves the enter.
//! - **An enter and a drop of one operation** are the drop of the subtree
//!   as it was before the operation, and then the enter of a new one.
//!
//! What a leave takes out is taken out when its operation ends, at
//! [`StagedBatch::next_operation`] or when the batch is resolved or
//! merged, when everything the operation says is known.
//! [`StagedBatch::len`] and [`StagedBatch::is_empty`] say what the batch
//! holds once it has.
//!
//! A call that no outcome can stand for is refused when the batch is
//! resolved, with the error the transaction would give the operation that
//! made it: an enter of a subtree that an earlier operation entered or
//! cleared ([`BatchError::Entered`]), a drop or a clear of a subtree that
//! an earlier operation dropped ([`BatchError::NotEntered`]), and two
//! enters or two leaves of one subtree in one operation
//! ([`BatchError::SubtreeTwice`]). A batch that is refused applies
//! nothing.
//!
//! # What is not here
//!
//! - **A range delete.** A storage batch has none: `FlatStore::clear`
//!   deletes the records its walk reads, key by key (`flat-store.md`), and
//!   a later clear does not remove a key that an earlier operation staged
//!   and the walk did not see. A subtree as a whole is dropped or cleared.
//! - **Costs.** What an outcome is charged is the layer's above
//!   (`cost-model.md` 14.6), from [`Resolved::outcomes`] and what it
//!   attached.
//! - **Which subtree is below which.** The batch knows no path and reads
//!   no directory: it is a function of the calls made on it.
//! - **Nodes and flat records in one data column.** The transaction
//!   refuses such a batch
//!   ([`BatchError::NotFlat`]); a node
//!   that keeps its value over a flat record keeps the transaction's.
//! - **Whether a stated value is the one the transaction holds.** A value
//!   that is stated is written. Leaving equal bytes where they are is the
//!   node store's (MERK-01), which reads the transaction before it puts.

mod node;
mod resolved;
mod staged;

#[cfg(all(test, not(loom)))]
mod tests;

use std::{
    fmt,
    sync::{Mutex, MutexGuard, PoisonError},
};

use self::staged::{Leave, Staged};
pub use self::{
    node::{StagedLink, StagedNode},
    resolved::{Outcome, Resolved, Written},
};
use crate::txn::{Applied, BatchError, NodePut, Scope, Subtree, TxnError, Unit};

/// The writes of one or more operations across subtrees, staged until
/// they are applied to a write transaction together.
///
/// A handle stages through a shared reference, and the batch is `Sync`
/// where what its calls attach is `Send`: the handles of one batch may be
/// on several threads, each call is whole, and the order of two calls that
/// race is not defined.
pub struct StagedBatch<C = ()> {
    staged: Mutex<Staged<C>>,
}

impl<C> Default for StagedBatch<C> {
    fn default() -> Self {
        Self::of(Staged::default())
    }
}

impl<C> fmt::Debug for StagedBatch<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let staged = self.staged();
        f.debug_struct("StagedBatch")
            .field("outcomes", &staged.len())
            .field("subtrees", &staged.subtrees.len())
            .field("refused", &staged.refused)
            .finish()
    }
}

impl<C> StagedBatch<C> {
    /// A batch with nothing staged.
    pub fn new() -> Self {
        Self::default()
    }

    const fn of(staged: Staged<C>) -> Self {
        Self {
            staged: Mutex::new(staged),
        }
    }

    fn staged(&self) -> MutexGuard<'_, Staged<C>> {
        self.staged.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many keys the batch holds an outcome for, once the drops and
    /// the clears of the operation it is in have taken out what they take
    /// the place of.
    pub fn len(&self) -> usize {
        self.staged().len()
    }

    /// Whether nothing is staged that an application would write: no
    /// outcome of a key, and no enter, drop or clear. A batch with a call
    /// that is refused is not empty.
    pub fn is_empty(&self) -> bool {
        self.staged().is_empty()
    }

    /// Marks the boundary between two operations that share the batch:
    /// what is staged so far is the outcome of earlier operations.
    pub fn next_operation(&self) {
        self.staged().next_operation();
    }

    /// Moves everything staged into a new batch and leaves this one empty,
    /// still open for the handles that stage into it.
    pub fn take_pending(&self) -> Self {
        Self::of(std::mem::take(&mut *self.staged()))
    }

    /// Makes again on this batch, in the generation it is in, every call
    /// that `other` holds an outcome of. What `other` says of the subtrees
    /// comes first, then its outcomes.
    pub fn merge(&self, other: Self) {
        self.staged().merge(other.into_staged());
    }

    /// Merges `other` as a later operation: each of its outcomes takes the
    /// place of what this batch holds for the key, and whatever is staged
    /// afterwards is later still.
    pub fn merge_overwriting(&self, other: Self) {
        self.staged().merge_overwriting(other.into_staged());
    }

    fn into_staged(self) -> Staged<C> {
        self.staged
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Stages a put of the flat record `key` of `unit`.
    pub fn put(&self, unit: Unit, key: &[u8], value: &[u8], tag: C) {
        self.staged().put(unit, key, value, tag);
    }

    /// Stages a delete of the flat record `key` of `unit`.
    pub fn delete(&self, unit: Unit, key: &[u8], tag: C) {
        self.staged().delete(unit, key, false, tag);
    }

    /// Stages a put of a node of the Merk `subtree`, under the node's key.
    ///
    /// A node that keeps its value keeps the one it has in the batch, and
    /// a node that states the Merks it hosts says so for its element
    /// ([`Self::host`]).
    pub fn put_node(&self, subtree: Subtree, node: &NodePut<'_>, tag: C) {
        self.staged().put_node(subtree, node, tag);
    }

    /// Stages a delete of the node `key` of the Merk `subtree`.
    pub fn delete_node(&self, subtree: Subtree, key: &[u8], tag: C) {
        self.staged().delete(Unit::data(subtree), key, true, tag);
    }

    /// States the root of the Merk `subtree` as its operation leaves it:
    /// the node under `root`, or none for a Merk with no node.
    pub fn set_root(&self, subtree: Subtree, root: Option<&[u8]>) {
        self.staged().set_root(subtree, root);
    }

    /// States the Merks that the element `key` of `subtree` hosts once the
    /// batch is applied, by position: every one, also those the batch does
    /// not write.
    pub fn host(&self, subtree: Subtree, key: &[u8], hosted: &[Subtree]) {
        self.staged().host(subtree, key, hosted);
    }

    /// Enters `subtree`, with no node, as a tree of `tree_type`. Its host
    /// is the element that was said to host it, or none.
    pub fn enter(&self, subtree: Subtree, tree_type: &[u8]) {
        self.staged().enter(subtree, tree_type);
    }

    /// Drops `subtree` with every subtree below it. The outermost subtree
    /// is named.
    pub fn drop_subtree(&self, subtree: Subtree) {
        self.staged().leave(subtree, Leave::Drop);
    }

    /// Clears `subtree`: it stays entered, with no node, and every subtree
    /// below it goes.
    pub fn clear(&self, subtree: Subtree) {
        self.staged().leave(subtree, Leave::Clear);
    }

    /// The batch as it stands, or the first call that no outcome can stand
    /// for.
    pub fn resolve(self) -> Result<Resolved<C>, BatchError> {
        Resolved::new(self.into_staged())
    }

    /// Resolves the batch and applies it to the transaction of `scope` as
    /// one operation ([`Resolved::apply`]).
    pub fn apply(self, scope: &Scope<'_, '_>) -> Result<Applied, TxnError> {
        self.resolve()?.apply(scope)
    }
}
