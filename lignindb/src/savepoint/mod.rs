//! Savepoints: undo marks over the write set of a write transaction
//! (`transactions.md` WT8, SC8).
//!
//! A savepoint is a [`Mark`](crate::txn::Mark): the place of the write set
//! at the moment it was set. A transaction keeps its savepoints in a stack,
//! and three calls of [`WriteTxn`] work on the newest one:
//!
//! | Call | Does |
//! | --- | --- |
//! | [`WriteTxn::set_savepoint`] | puts a savepoint on the stack |
//! | [`WriteTxn::rollback_to_savepoint`] | takes out every write made after the newest savepoint, and removes that savepoint |
//! | [`WriteTxn::pop_savepoint`] | removes the newest savepoint and undoes nothing: the writes after it belong to the savepoint below it |
//!
//! [`WriteTxn::savepoints`] says how many are set.
//!
//! No example runs here, for the reason [`crate::txn`] gives: a write
//! transaction is begun where the database is opened (TXN-07). The tests
//! are in `txn/tests/savepoints.rs`, on the store of the transaction's own
//! tests.
//!
//! # What a rollback to a savepoint restores
//!
//! **The write set is cut back to the savepoint, and nothing else is kept.**
//! The private frames of a transaction are derived from its write set
//! (WT3), so the rollback gives them up and the next operation makes them
//! again from the writes that are left. Nothing is undone page by page, and
//! so nothing can be left over: the values, the shape of every Merk, the
//! page each cell is in, which hashes are stale and the entries of the
//! subtree directory are all what the remaining writes make them.
//!
//! - **The root hash of every Merk is the one it had at the savepoint**,
//!   while no commit landed outside the transaction since: the nodes, their
//!   hashes and their links are the same records. The acceptance tests read
//!   it from the transaction before the savepoint and after the rollback,
//!   and prove it against every node below the root both times.
//! - **A subtree is what it was at the savepoint.** One that was dropped
//!   after it is there again, with every Merk below it and the flat
//!   records beside them (CV9); one that was cleared has its nodes; one
//!   that was entered is entered no more, and a write to it is refused. An
//!   enter writes no record, but it is a part of its operation in the
//!   write set and is cut with it.
//! - **The hashes that were stale at the savepoint are stale, and no
//!   other.** No copy of the marks is kept at a savepoint: a node the
//!   remaining writes left stale is marked when its frame is made again,
//!   and the commit makes its hashes (CM3 step 5).
//! - **What the transaction has not written is read as it is now, not as it
//!   was at the savepoint** (WT8, case A4b). A key that was first written
//!   after the savepoint is written by the transaction no more, and a read
//!   of it gives the latest committed record, including one a commit made
//!   outside the transaction in between.
//! - **The conflict rule sees what is left** (CF4, GM6). A write that the
//!   rollback took out is in no overlap set, so a transaction whose only
//!   overlapped writes came after the savepoint commits again (cases A4,
//!   B6). A write from before the savepoint keeps its first-write version
//!   (cases A5, A16, B7).
//! - **The page ids the rolled-back writes took go back at once** (ID1).
//!   They are given up with the frames, before the call returns. The frames
//!   that are made again take ids of their own.
//! - **Every node handle read through the transaction ends** (PN8): the
//!   rollback takes a new lease, because a frame that is made again and
//!   changed again would repeat its count of departures. A handle from
//!   before is not followed, and its node is found by its key. That holds
//!   for a handle read before the savepoint as well, and for the handles a
//!   drop or a clear ended: the rollback across it gives the nodes back,
//!   not the handles.
//!
//! All of this is for a savepoint that a write was made after. With none,
//! the rollback removes the savepoint and nothing else: the frames stay,
//! and so do the handles (choice 2).
//!
//! # Scopes, commit and rollback
//!
//! - **A savepoint is set, taken or popped only while no scope is open**
//!   (SC8): each of the three calls is refused at once with
//!   [`SavepointError::ScopesOpen`] and changes nothing. A scope that is
//!   rolled back takes out its own writes only, which all came after every
//!   savepoint, so the stack is what it was when the scope was opened.
//! - **A rollback of the whole transaction removes its savepoints** (WT9),
//!   with everything else it holds.
//! - **A commit ignores them** (WT8). Savepoints that are still set do not
//!   hold a commit back, and a transaction that its commit has ended has
//!   none. Dropping a transaction is a rollback.
//!
//! # Threads
//!
//! Each call holds the transaction's own lock (LT7 level 2) for its length,
//! as every other call of the transaction does. The check for an open
//! scope, the mark and the change of the stack are one step: no scope can
//! open, and no write can land, between them.
//!
//! # Choices made here that the plan left open
//!
//! 1. **A pop is refused while a scope is open**, like the other two calls.
//!    SC8 names setting and rolling back; a pop undoes nothing and could be
//!    allowed. It is refused so that one rule covers the stack: nobody
//!    changes it beside an open scope.
//! 2. **A rollback to a savepoint that no write was made after changes
//!    nothing but the stack.** The frames stay, and so do the node handles:
//!    no frame is put back, which is what a new lease is for.
//! 3. **A commit that ends the transaction removes its savepoints**,
//!    whatever its outcome: published, a conflict, or nothing to commit. A
//!    commit that fails with an error leaves the transaction as it was,
//!    its savepoints included: a scope is open, the write set cannot be
//!    laid out, or a page was not read.
//! 4. **Savepoints have no names.** A rollback goes to the newest one, as
//!    on RocksDB, which is all Platform uses
//!    (`platform-requirements.md` P-12).
//! 5. **Their number is not bounded.** A savepoint costs one
//!    [`Mark`](crate::txn::Mark) in the transaction's memory, and Platform
//!    leaves one on the stack for every state transition of a block.
//! 6. **The errors are a type of their own** ([`SavepointError`]), not
//!    variants of [`TxnError`](crate::txn::TxnError): the three calls read
//!    no page and can fail in these two ways only.
//!
//! # What is not here
//!
//! - **Pending marks** (SC9). They are the scopes' ([`crate::scope`]), and
//!   a [`Mark`](crate::txn::Mark) remembers how many changes the
//!   transaction's marks had taken: a rollback to a savepoint takes back
//!   every change that a scope merged after it (WT8). No element is marked
//!   on existing GroveVersions: the marks are set with stop points
//!   (GROVE-11).
//! - **Memory that a rollback gives back late.** The transaction's own
//!   copies of committed values (WT3), which it makes when a commit outside
//!   it meets one of its writes, are kept apart from the writes and go when
//!   the write set is empty. A copy that was made for a write a rollback
//!   then took out stays until then. That is the write set's rule
//!   ([`crate::txn`]), and it costs memory, never a wrong read.

use std::{error::Error, fmt};

use crate::txn::{Held, WriteTxn};

/// Why a call on the savepoints of a write transaction was refused. The
/// transaction is what it was before the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavepointError {
    /// A scope of the transaction is open (`transactions.md` SC8).
    ScopesOpen,
    /// The transaction has no savepoint to roll back to or to pop (WT8,
    /// case A17).
    NoSavepoint,
}

impl fmt::Display for SavepointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ScopesOpen => "a scope of the transaction is open",
            Self::NoSavepoint => "the transaction has no savepoint",
        })
    }
}

impl Error for SavepointError {}

impl<'s> WriteTxn<'s> {
    /// Holds the transaction for a call that changes the stack of its
    /// savepoints, which none does while a scope is open (SC8).
    fn unscoped(&self) -> Result<Held<'_, 's>, SavepointError> {
        let held = self.hold();
        if held.scope_open() {
            return Err(SavepointError::ScopesOpen);
        }
        Ok(held)
    }

    /// Sets a savepoint (`transactions.md` WT8): the place after the
    /// transaction's last write goes on the stack of its savepoints.
    ///
    /// It is refused while a scope is open (SC8).
    pub fn set_savepoint(&self) -> Result<(), SavepointError> {
        let mut held = self.unscoped()?;
        let mark = held.mark();
        held.savepoints().push(mark);
        Ok(())
    }

    /// Rolls back to the newest savepoint and removes it (WT8): every write
    /// made after it goes, with its first-write version and its place in
    /// the overlap set (CF4), and the page ids those writes took return to
    /// the allocator (ID1). The frames are made again from the writes that
    /// are left, and every node handle read through the transaction ends
    /// (PN8). With no write after the savepoint, only the savepoint goes.
    ///
    /// The transaction then reads its remaining writes over the latest
    /// committed state of each read, not the state the database had when
    /// the savepoint was set.
    ///
    /// It is refused while a scope is open (SC8), and with no savepoint set
    /// ([`SavepointError::NoSavepoint`]). Nothing is changed then.
    pub fn rollback_to_savepoint(&self) -> Result<(), SavepointError> {
        let mut held = self.unscoped()?;
        let mark = (held.savepoints().pop()).ok_or(SavepointError::NoSavepoint)?;
        // Whatever takes a write from before a savepoint out removes the
        // savepoint with it: a rollback and a commit clear the stack, and a
        // scope is opened after every savepoint that is set.
        let placed = held.rewind(mark);
        assert!(placed, "a savepoint is a place of the write set");
        Ok(())
    }

    /// Removes the newest savepoint and undoes nothing (WT8): the writes
    /// made after it now belong to the savepoint below it, or to none.
    ///
    /// It is refused while a scope is open, and with no savepoint set
    /// ([`SavepointError::NoSavepoint`]).
    pub fn pop_savepoint(&self) -> Result<(), SavepointError> {
        let mut held = self.unscoped()?;
        (held.savepoints().pop()).ok_or(SavepointError::NoSavepoint)?;
        Ok(())
    }

    /// How many savepoints are set.
    pub fn savepoints(&self) -> usize {
        self.hold().savepoints().len()
    }
}
