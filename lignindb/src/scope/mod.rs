//! Write scopes: several threads writing one transaction.
//!
//! - [`Scope`] is the handle one thread writes a transaction through
//!   ([`WriteTxn::scope`](crate::txn::WriteTxn::scope)). Any number of
//!   scopes of one transaction may be open at once.
//! - A scope **claims** the units it touches, a column of a subtree each,
//!   and an operation that another open scope's claim stands against is
//!   refused at once ([`TxnError::ScopeOverlap`](crate::txn::TxnError)).
//! - A scope **ends** by merging into the transaction or by rolling back
//!   on its own, which is what a failed state transition needs.
//! - The transaction keeps the **pending marks** its scopes set, and is
//!   not committed while one stands
//!   ([`TxnError::NotFinalized`](crate::txn::TxnError)).
//!
//! `docs/lignindb/transactions.md`, section 9, is the specification: SC1
//! to SC9 for scopes, claims and marks, LT1 to LT7 for pages that the Merks
//! of two scopes share and for the locks, CM1 for what a commit needs. This page says how
//! each is kept, and at the end which choices were made here.
//!
//! No example runs here, for the reason [`crate::txn`] gives: a write
//! transaction is begun from the structures of one store. The tests make
//! one on disk (`scope/tests`).
//!
//! # Claims
//!
//! The unit of exclusion is a column of a subtree (SC2): the data column
//! of a subtree is its Merk, with the Merk's entry in the subtree
//! directory, and each of the three other columns is a unit of its own.
//!
//! | The call | Claims |
//! | --- | --- |
//! | a read of a flat record, a committed read of one, a cursor over a unit | that unit, for reading |
//! | a read of a node by key, by handle or as a child, a committed read of one, a cursor over a Merk | the Merk's data column, for reading |
//! | [`Scope::apply`] | for writing: the data column of every Merk the batch has a change set for or enters, and the unit of every flat record |
//! | a batch with a node that names a Merk it hosts, and no change set for that Merk | that Merk's data column, for reading: the node's cell is written with the Merk's root, which is read from its entry in the subtree directory |
//! | a batch that drops a subtree | for writing: every column of the subtree, and every unit of every subtree below it |
//! | a batch that clears a subtree | for writing: its data column, and every unit of every subtree below it |
//! | an evaluation of a Merk's stale hashes ([`Scope::evaluate`]) | the Merk's data column, for reading; then, for reading, that of each Merk it comes to through the pointer of a host, as it comes to it; and at the end, together or not at all, for writing: the data column of every Merk it makes a hash in ([`crate::txn`], "An evaluation before the commit") |
//!
//! - **Any number of scopes read one unit. One scope writes it**, and then
//!   no other open scope has read it or reads it (SC3). A scope that has
//!   read a unit and then writes it keeps the unit if no other scope holds
//!   it.
//! - **A claim never waits.** It is granted or refused inside the call,
//!   under the transaction's lock. A call that is refused adds no write of
//!   its batch, shows the caller no record and keeps no claim, and its
//!   scope stays open and usable. A batch, a read of a record or of a
//!   node, and a cursor's read of the node it stands on begin an operation
//!   of the transaction before their claim is checked: also where they are
//!   refused, they take in the commits that were published since the last
//!   one, which can end the node handles read before (PN8). A cursor's
//!   movement, and its read of the flat record it stands on, check the
//!   claim and begin no operation: refused, they take nothing in. Any of
//!   them reads the subtree directory where it has to ask what is below
//!   what.
//! - **A scope keeps its claims until it ends**, also the claims of a
//!   batch that was then refused for what it says.
//! - **The claims of one batch are granted together or not at all.**
//! - **A read outside any scope** is a read by a scope that ends with the
//!   call: it is refused if another scope writes the unit, and it keeps
//!   nothing.
//! - **A drop takes everything below it** (`transactions.md` CV9). Which
//!   subtree is below which is read from the hosts: of the subtrees the
//!   batch that asks enters, of those the transaction's scopes entered,
//!   from the write set, and from there up from the subtree directory, also
//!   from an entry that a drop left. So a scope that reaches below a
//!   subtree which another open scope has dropped or cleared is refused,
//!   where it would otherwise read that scope's write as "nothing is
//!   there", and so is one that enters a subtree there. A subtree is below
//!   its host from the batch that enters it.
//!
//! Read claims are what makes the result independent of timing (SC7): no
//! scope reads a unit before or after another open scope's write to it.
//!
//! # What a scope reads and writes
//!
//! A transaction has **one write set**, and the writes of its scopes are
//! in it, each operation with the scope that made it (WT2). A scope reads
//! the newest write that covers a key, and otherwise the latest committed
//! state (SC4, RD3). Nothing has to be hidden from it for that: a unit it
//! can read has no write of another open scope, because that scope would
//! hold the unit. First-write versions are those of the scope's own
//! operations (SC5), and a commit outside the transaction is compared with
//! the writes of open scopes as with every other (CF3).
//!
//! **The pages are another matter.** The transaction's frames have one
//! writer at a time (LT4), and how a scope's writes reach them depends on
//! when the scope was opened:
//!
//! | A scope that was opened | Its writes |
//! | --- | --- |
//! | while no other scope was open | are laid out on the transaction's pages as they are made, exactly as [`crate::txn`] describes: a batch is checked against the pages, and its nodes come back as handles |
//! | beside another open scope | are kept in the write set, and laid out later: when the scope merges, or when it asks ([`Scope::lay_out`]). Until then it reads its own nodes as the records it wrote, and every other node from the pages |
//!
//! Laying out is the application that LT4 asks for at the merge: every
//! operation of the scope is applied in order, through the tree layer,
//! which finds every cell by its key in the pages as the scopes before it
//! left them
//! ([`Fragments::apply`](lignin_tree::fragment::Fragments::apply)). A
//! scope that was opened beside another makes no change to a page before
//! that, so it holds no copy of a page, takes no page id and cannot move a
//! cell of another scope's Merk. A rollback of it before that leaves the
//! pages alone.
//!
//! What it gives up until its writes are laid out:
//!
//! - **It is given no handle** of a node it wrote ([`Applied`](crate::txn::Applied)
//!   is empty), and it reads the nodes of a Merk it has written by their
//!   keys.
//! - **What only the pages can refuse is refused when the writes are laid
//!   out**, not when they are made: a change set that names a child or a
//!   root with no node, a subtree that is not entered, a root that finds
//!   no room (`lignin_tree::fragment`, "A node that nothing in the write
//!   points at"). What the batch and the write set show is refused at
//!   once, as it is for a transaction that serves its records by key.
//!
//! [`Scope::lay_out`] is for the caller that wants to know before it
//! merges: if it is refused the scope is as it was, and can be rolled
//! back. [`Scope::merge`] lays out what is not, and does not fail: writes
//! that cannot be laid out become the transaction's all the same (SC6),
//! and the transaction is then one whose write set cannot be laid out,
//! which serves its records by key and is not committed until a rollback
//! takes those writes out.
//!
//! # How a scope ends
//!
//! **Merge**: the scope's operations become the transaction's, with its
//! pending marks. **Rollback**: its operations are taken out of the write
//! set, with their first-write versions and their places in the overlap
//! set, and its marks are dropped. Its claims go in both cases. Either one
//! is one step under the transaction's lock, so no other scope sees one
//! half done, and merges come one at a time (SC6).
//!
//! - A scope whose writes are laid out and that rolls back leaves frames
//!   that show them: they are given up and made again from what is left,
//!   as after a rollback to a savepoint. Every node handle read through
//!   the transaction ends (PN8).
//! - Operations of other scopes that came after a rolled-back scope's are
//!   kept in their order, with their first-write versions. They are added
//!   again to a copy of the write set, so such a rollback costs what the
//!   whole write set holds, and one whose operations are the last of the
//!   set costs what it wrote.
//! - A scope that is dropped is rolled back.
//!
//! **Every scope is closed before** a savepoint is set, taken or popped
//! ([`crate::savepoint`]), before the transaction is rolled back and
//! before it is committed (SC8, CM1). The check and the call are one step
//! under the transaction's lock.
//!
//! # Pending marks
//!
//! The engine does not know elements, so it cannot tell whether a changed
//! Merk has been carried into the element that hosts it. A scope says so
//! (SC9): [`Scope::mark_pending`] on the element, when a write stopped
//! below it, and [`Scope::clear_pending`] when the write that names the
//! element's roots again has been made.
//!
//! - **A mark is not a claim**, so scopes that write two Merks below one
//!   Merk both mark its elements.
//! - **A commit is refused while a mark stands** (CM1).
//! - **Marks go with what set them**: with their scope's rollback, and
//!   with a rollback to a savepoint that was set before the scope merged
//!   (WT8).
//! - **The changes of a scope take effect at its merge**, in the order of
//!   the merges and not in the order they were made. The two agree for an
//!   element whose mark is taken off as SC9 has it, by the scope that
//!   makes the write which names the element's roots again: that write
//!   reads the Merks the element hosts, so it is refused beside an open
//!   scope that writes one of them. [`Scope::clear_pending`] by itself
//!   takes no claim, and a caller that takes a mark off without that write
//!   is not held to the order.
//! - **Who is shown which mark.** [`Scope::is_pending`] answers from the
//!   marks the scope set and took off, and otherwise from those of the
//!   merged scopes. A mark of another open scope is not shown. A snapshot
//!   is answered "no" ([`Snapshot::is_pending`](crate::snapshot::Snapshot::is_pending)).
//! - **The tree layer is told by the cell**
//!   ([`Pages::host_is_behind`](lignin_tree::fragment::Pages::host_is_behind)):
//!   the pointers of a marked element to the roots it hosts are not
//!   followed (`tree-layout.md` T3). The frames keep the cells of the
//!   marked elements whose scope's writes they show. A cell is found by
//!   its key when the mark is set, followed when a write carries it, and
//!   let go when a write deletes its node.
//!   When the frames are made again from the write set, the moment each
//!   mark was set among the writes is not known: every host whose key is
//!   that of an element which was marked at some point is then taken to be
//!   behind, in whichever Merk it is, which keeps its pointers as they are
//!   and is what a write does for a host it cannot tell; the cells of the
//!   marks that stand are found again at the end. The same holds while a
//!   scope's writes are laid out, for the marks that scope set.
//!
//! No Merk is opened through a host's cell here: every open is by the
//! Merk's entry in the subtree directory, which the scope that holds the
//! Merk writes with the change that moves the root (SC2).
//!
//! # A drop and the conflict rule
//!
//! A drop deletes every record under every subtree below the dropped one
//! (CV9), and the key set of its commit names the dropped subtree and, of
//! the subtrees below it, those that its transaction had written under,
//! as its write-set entry does. Two cases follow that record keys do not
//! decide (CF5), and both are decided here, when the transaction takes in
//! the commits that were published since it last looked:
//!
//! - **A commit outside dropped a subtree above one the transaction had
//!   written under.** The transaction notes, for each subtree it writes
//!   under, which Merk the committed state has entered there. Where that
//!   is another Merk afterwards, or none, every write of the transaction
//!   under the subtree is met.
//! - **The transaction dropped or cleared a subtree, and a commit outside
//!   wrote below it.** The drop is met.
//!
//! Neither happens on an existing GroveVersion, where a write below a
//! subtree also writes the element that hosts it, a key of the subtree
//! above. With stop points a write does not.
//!
//! # Locks and threads
//!
//! Every call of a scope holds the transaction's own lock for its length
//! (LT7 level 2), as every call of the transaction does, and no latch of a
//! page when it returns (LT3). So the operations of scopes on several
//! threads come one after another, in an order that the threads decide. A
//! call waits for the calls that hold the lock before it: an operation, a
//! merge or a rebase each, and as many as there are threads ahead of it,
//! since the lock is not fair. What runs on several threads at once is
//! everything above the engine: the work of the Merks.
//!
//! A [`Scope`] is `Send` and not `Sync`: it is one thread's at a time.
//!
//! # Choices made here that the plan left open
//!
//! 1. **One write set, with each operation's scope**, as WT2 words it.
//!    Claims then do what a separate write set for each scope would: no
//!    reader finds a write it must not see.
//! 2. **A scope that is opened beside another makes no change to a page
//!    before it is laid out.** LT4 gives each scope a private copy of a
//!    shared page and applies the scope's writes again when it merges
//!    second. Here the application at the merge is the only one. What LT4
//!    protects holds: a frame has one writer at a time, and the result is
//!    that of the scopes one after another (SC7). What differs is in the
//!    section above: no handle and a later refusal for such a scope. The
//!    other way needs, for each scope, pages, values, hashes and a
//!    directory of its own over the transaction's, each brought up to
//!    date whenever another scope merges; PERF-10 measures whether the
//!    work of laying out is worth doing on several threads.
//! 3. **A scope that is alone writes as before this module existed.** One
//!    scope at a time is what iteration one uses, and its path is that of
//!    [`crate::txn`], unchanged.
//! 4. **The operations of all scopes wait for one lock.** A scope can wait
//!    for the operations of others, where LT3 allows one physical step. The
//!    frames are bound to one operation at a time ([`crate::txn`]), and
//!    readers of one transaction do not yet read them together.
//! 5. **Claims are kept by a batch that fails**, and released only with
//!    the scope.
//! 6. **A merge does not fail**, and [`Scope::lay_out`] is how a caller
//!    asks first.
//! 7. **A host is taken to be behind by its key while pages are made
//!    again.** Exact cells would need the place of each mark among the
//!    writes.
//! 8. **The two cases of a drop are met by the subtree directory**, not by
//!    key sets that name every subtree below a dropped one: a drop reads
//!    nothing below the subtree it names (`tree-layout.md` D1).
//!
//! # What is not here
//!
//! - **The exception of `tree-layout.md` T2**: a small Merk that travels
//!   with its host is carried whoever has claimed it. No scope holds a
//!   copy of a page here, so a carried cell is never a cell that another
//!   scope has changed elsewhere, and the scope that holds the Merk finds
//!   it by its key afterwards. The rule would only choose other pages.
//! - **Writing again while holding the host**, for a write that stopped
//!   below its host and found no room
//!   ([`FragmentError::NoRoom`](lignin_tree::fragment::FragmentError)): the
//!   error is the caller's, at the write for a scope that is alone and
//!   when the writes are laid out for one that is not (GROVE-11). A
//!   caller that knows the host before it writes holds it from the
//!   start, as the node store of Merk does for the batches of a partial
//!   batch (`node-store.md`, "A batch that stops below an element").

pub(crate) mod table;

#[cfg(test)]
mod tests;

pub use crate::txn::Scope;

impl crate::snapshot::Snapshot<'_> {
    /// Whether the element under `key` of `subtree` is marked pending
    /// (`transactions.md` SC9): never, in a committed state. A commit is
    /// refused while a mark stands, so in every state a snapshot reads the
    /// pointers of every element are exact.
    pub fn is_pending(&self, subtree: crate::txn::Subtree, key: &[u8]) -> bool {
        let _ = (subtree, key);
        false
    }
}
