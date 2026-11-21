//! Commit and rollback: a write transaction becomes the next version of the
//! database, or gives up what it holds.
//!
//! - [`Committer`] is the commit path of one open database: a view of its
//!   write path, its write-back, its page table and its log. It commits a
//!   [`WriteTxn`](crate::txn::WriteTxn) ([`Committer::commit`]), rolls one
//!   back ([`Committer::rollback`]) and makes a write with no transaction
//!   ([`Committer::write`]).
//! - [`CommitState`] is what the commits of one open database share: the
//!   commit lock ([`CommitLock`]), whether the session still takes commits
//!   ([`Stopped`]) and counts ([`CommitStats`]).
//! - [`Committed`] is how a commit ends that reports no error, and
//!   [`Published`] what a commit that took a version did.
//! - [`CommitError`] is every other way a commit ends, with the part of the
//!   store that refused ([`Failure`]).
//! - [`NoStaleHashes`] is the hash rules of a caller whose writers make
//!   every hash themselves, as on existing GroveVersions.
//!
//! `docs/lignindb/transactions.md` is the specification: section 8 (CM1 to
//! CM9) for the commit, section 9 (CF1 to CF9) for the conflict rule, WT9
//! for the rollback, RD7 for a write with no transaction and LT7 for the
//! order of the locks. `lignin_store::flush` says what a commit owes
//! write-back, and `log-format.md` what a group is.
//!
//! No example runs here: a [`Committer`] is made where the database is
//! opened, of the structures of one store, and a caller commits through
//! [`Db::commit`](crate::Db::commit), whose page has an example. The
//! tests make a committer over a store on disk and over the simulated
//! file system (`txn/tests/store.rs`).
//!
//! # The steps of a commit
//!
//! | Step | What is done | Locks held |
//! | --- | --- | --- |
//! | before | A transaction of another database is refused. A transaction that has written nothing commits nothing and takes no version (CM3 step 1, CV2). A scope that is open refuses the commit, and so does a pending mark that stands (CM1, SC9). If an append to the log failed since the log's files were last turned, the cut is made (CM9 item 4, below). | none |
//! | 2 to 4 | The commit lock is taken. The transaction takes in the key sets of every commit it has not looked at and fails with a conflict if one met a write of it (CF2, CF3); otherwise its frames are made over the latest version, if they are not ([`crate::txn`], "What the commit takes"). | commit lock, then the transaction's own |
//! | 5 | Every hash that is still stale is evaluated with the caller's rules. | commit lock |
//! | 6 | One group is written for the place the log ends at and appended: the values, the pages in ascending order (a run for a tree page, a run for a flat page, a record for a freed page), the value pointers that cells stopped holding. Every cell that holds a value of the commit names the value's place in the group. | commit lock, operation guard, counts lock |
//! | | Write-back is told of the group ([`WriteBack::committed`]). | the same |
//! | 7 | The private copies become the page versions that the commit's version bore, the ids of new pages are published, freed pages are freed, unused ids go back. The released pointers are counted, which gives the counts lock back. | the same, until the count |
//! | | If the sync policy asks for it, the log is made durable ([`WriteBack::sync_log`]). | commit lock |
//! | 8 | The key set is recorded for the open write transactions, the version is published, the commit lock is given back. | commit lock |
//! | after | Versions that nobody pins are reclaimed. The values that waited for the transaction's copies are placed and the thread waits while write-back is over its bounds ([`WriteBack::place_ready`], [`WriteBack::wait_for_room`]). | none |
//!
//! The order of the locks is LT7's: the commit lock (level 1), the lock of
//! the transaction (2), the latches of pages (4), the counts lock (4a). A
//! commit waits for the commit lock, for the counts lock and, after it has
//! ended, for write-back's bounds, and for nothing else (CM8) but what
//! the commit of a load waits for ("The commit of a load"): not for a
//! reader and not for another open write transaction. The latches it takes
//! under the counts lock are those of its own private copies, which nobody
//! else holds: no reader sees a private copy, and no page step changes a
//! page under one (PV7).
//!
//! Nobody is called back under a lock: an error of write-back is told to
//! whoever [`CommitState::reporting`] names when the commit has given its
//! locks back.
//!
//! **The log is the only file a commit writes, and it is appended to
//! once** (CM7). No value-log file is written, no manifest is replaced:
//! the values travel in the group (FL7) and write-back places them later.
//! The commit of a load is the exception ("The commit of a load").
//! A commit that changes no page appends a group all the same and takes a
//! version (CV3): a delete of a key that has no record is a write (ST5).
//!
//! **Nothing of a commit is visible before step 8** (CM4): no reader looks
//! at a page version born after the version it reads, and the version is
//! stored last.
//!
//! ## Where the sync of a commit is made
//!
//! CM7 puts the sync that the policy asks for "after the append and before
//! anything is installed or published". Here it is made after the install
//! and before the publication. The counts lock is held from the moment the
//! log is asked for the place of the group until the commit is counted,
//! which is after the install (LT7, CM3), and no sync is made under it
//! (CM8): [`WriteBack::sync_log`] takes that lock itself, to tell the value
//! log how far its counts are durable. What CM7 is for holds: nothing is
//! published before the sync, and whether the sync succeeds changes
//! nothing that was installed, because a group that is in the log has
//! happened either way (CM9 item 2).
//!
//! # The commit of a load
//!
//! A transaction whose loads put values into the value log
//! ([`crate::txn`], "A load") holds them there under no count. Its commit
//! takes them over (`transactions.md` LD5, `log-format.md` LG16). It is a
//! commit like any other, with this more:
//!
//! | Step | What is done | Locks held |
//! | --- | --- | --- |
//! | before | The transaction's frames are made over the latest version, and where no durable manifest covers the values its loads appended for them, the file the loads fill is synced and the manifest is replaced. An error here is returned by no one: the same is done again below. | the transaction's own, that of the loads' file, the manifest's |
//! | 2 to 4 | The same again, after the conflict is decided (CF6) and before the frames are taken. A failure refuses the commit with the transaction as it was ([`CommitError::Refused`]). Nothing is left to do unless frames were made since: a commit of another was published, or an operation of the transaction on another thread dropped them or loaded more. | commit lock, and the three above |
//! | 6 | The group lists as acquired the pointers that cells of the transaction's pages hold to values of its loads, after the released ones. A pointer of its own loads that a cell stopped holding again is not listed as released: no count had it. | as for any commit |
//! | 7 | The acquired pointers are counted with the released ones, and the hold on the files of the loads ends. | as for any commit |
//!
//! So such a commit can sync a value-log file and replace the manifest,
//! which costs what those cost on the store's disk. In the first row they
//! are made without the commit lock. In the second row a commit waits for
//! them under the commit lock, and for what the frames append (CM8), and
//! the commits behind it wait with it. What either row waits for is in
//! `transactions.md` LD8. [`Published::acquired`] counts the pointers.
//!
//! **A value-log file whose sync failed takes no more values. No
//! manifest covers what it took after its last sync that succeeded, and
//! that is read no more**
//! ([`ValueStoreError::FileFailed`](lignin_store::values::ValueStoreError::FileFailed)).
//! The commit of a transaction is refused, with the transaction as it
//! was, for as long as it has the frames that a load appended such a
//! value for, and each time it has to make its frames from a write set
//! that names such a value (`transactions.md` LD3, LD5). What the file
//! took before that sync is as it was.
//!
//! **Where the commit ends with its group possibly in the log and the
//! values not counted**, the store has the hold from then on, until the
//! session ends: a commit that cannot be settled
//! ([`CommitError::Unsettled`]) and one whose install is refused
//! ([`CommitError::Halted`]). A recovery that keeps the group finds cells
//! that point into those files. The store has the hold as well where the
//! commit panics once it holds the lock on the counts (step 6).
//!
//! **After a crash the store has all of a load or none of it.** A manifest
//! that covers every value is durable before the group that names their
//! pointers is appended. Where recovery keeps the group, each pointer
//! leads to its value. Where it does not, no cell that it keeps points at
//! a value of the load, and no count includes one: they are dead space.
//!
//! # How a commit ends
//!
//! | Outcome | The transaction | The database | The session |
//! | --- | --- | --- | --- |
//! | [`Committed::Published`] | ended | the version is the latest | goes on |
//! | [`Committed::Empty`] | ended (it held nothing) | unchanged | goes on |
//! | [`CommitError::ScopesOpen`] | open, as it was | unchanged | goes on |
//! | [`CommitError::NotFinalized`] | open, as it was | unchanged | goes on |
//! | [`CommitError::Refused`] | open, as it was | unchanged | goes on |
//! | [`CommitError::NotReady`] | open, as it was | unchanged | takes no commit until the cut can be made |
//! | [`CommitError::Conflict`] | ended, its writes discarded | unchanged | goes on |
//! | [`CommitError::Failed`] | ended, its writes discarded | unchanged; no recovery brings the commit back | goes on, unless the failure is [`Failure::VersionGap`], which stops it |
//! | [`CommitError::NotDurable`] | ended | **the version is the latest**, and is not known to be durable | goes on; see below |
//! | [`CommitError::Unsettled`] | ended | unchanged in this session | takes no further commit |
//! | [`CommitError::Halted`] | ended | unchanged in this session | takes no further commit |
//! | [`CommitError::Stopped`] | open, as it was | unchanged | has stopped |
//!
//! **A commit that ends the transaction removes its savepoints**, whatever
//! the outcome, and the transaction can be written again. A commit that
//! leaves it open leaves its writes, its marks and its savepoints
//! ([`CommitError::leaves_transaction_open`]). CM6 lets only the two
//! precondition errors leave a transaction open. [`Refused`], [`NotReady`]
//! and [`Stopped`] do too, for the same reason: the commit was refused
//! before it took anything of the transaction, and the caller can still
//! roll back to a savepoint, or try again.
//!
//! [`Refused`]: CommitError::Refused
//! [`NotReady`]: CommitError::NotReady
//! [`Stopped`]: CommitError::Stopped
//!
//! **The log decides whether a commit happened** (CM9). What the log
//! answers to the append is all a commit goes by:
//!
//! | The append | The commit |
//! | --- | --- |
//! | is taken | has happened: it is installed and published, and no step after the append is let to fail |
//! | finds the place of the group taken ([`LogError::PlaceTaken`]) | writes its group again for the new place, with every value named anew (`log-format.md` LG22), a few times; then it has failed ([`Failure::PlaceLost`]). The counts lock keeps every other append out, so this is seen only where something appends without it. |
//! | fails with the file as long as it was, or with a part of the group that is no item | has failed ([`CommitError::Failed`]) |
//! | was taken back ([`LogError::TakenBack`]) | has failed once the log has settled it ([`LogStore::settle`], with the counts lock given back); and is unsettled if that fails |
//! | cannot say ([`LogError::Unsettled`]) | is unsettled: the session stops |
//!
//! **A log that write-back left unsettled** stops the session as well. A
//! page step's record or a checkpoint's mark can be taken back and not
//! settled, and the log then takes nothing more. A commit that finds the
//! log so is refused with [`CommitError::Stopped`], its transaction whole.
//! Where the log becomes so after the commit has taken the transaction,
//! the commit ends with [`CommitError::Unsettled`].
//!
//! [`CommitError::Halted`] is what no rule lets happen: the group is in the
//! log and a part of the store refused the install. The commit is not
//! published, the session stops, and the next open recovers the commit if
//! its bytes survive. A commit that finds a group in the log that the
//! session never published, as after a commit that panicked between its
//! append and its publication, has failed ([`Failure::VersionGap`]) and
//! stops the session the same way.
//!
//! **The cut after a failed append** (CM9 items 4 and 5). After an append
//! that failed the log takes nothing until its failed bytes are cut off,
//! which [`WriteBack::roll_log`] does: it is not a commit's to do, and it
//! replaces the manifest. Where write-back has not made the cut by the time
//! the next commit comes, that commit's thread makes it before it takes the
//! commit lock. While it cannot be made, the commit is refused with
//! [`CommitError::NotReady`] and its transaction is as it was.
//!
//! The log is looked at once more under the commit lock, before the
//! transaction is taken. What that cannot see is an append of write-back
//! that fails after it and before the commit has the counts lock: the
//! commit then fails ([`CommitError::Failed`], with
//! [`LogError::NoActiveFile`]) and its transaction is ended, which is the
//! storage error of CM5.
//!
//! **A sync that failed stays failed for the session**
//! ([`LogError::SyncFailed`]). Under a policy that syncs every commit,
//! every later commit is published and reported [`NotDurable`]. A cut needs
//! a sync, so after a failed sync no cut can be made: an append that fails
//! from then on leaves every later commit [`NotReady`] until the database
//! is opened again. A caller that sees [`NotDurable`] should end the
//! session when it can.
//!
//! [`NotDurable`]: CommitError::NotDurable
//!
//! # The conflict rule
//!
//! The commit of a write transaction fails with [`CommitError::Conflict`]
//! when a commit outside it wrote a key after the transaction first wrote
//! that key (CF2), and for no other reason (CF6) but the one of a kept
//! value below. By CF2, two transactions whose
//! keys do not meet both commit. The rule is decided from the key sets of
//! the commits, which the write path keeps until every open write
//! transaction that has a write has taken them in
//! ([`Commits`](crate::txn::Commits), CF3), so it is exact however much
//! was committed in between.
//!
//! The key set of a commit is the keys of its writes, and for each Merk
//! it loaded the range of the nodes of that subtree (`transactions.md`
//! LD6). A commit that stands for more than that says so
//! ([`Committer::commit_covering`], CF5): a wipe covers every key.
//!
//! **A kept value is dated at its read** (CF2a). A node that is written
//! with its value kept ([`ValuePut::Keep`](crate::txn::ValuePut::Keep))
//! states hashes that were made for the value as its writer read it. For
//! such a write, where the value is the committed state's, the rule looks
//! from the read on and not from the write: a commit that wrote the node's
//! key after the read meets the write, whether it landed before the write
//! or after it. So a commit never publishes a node under hashes that were
//! made for a value another commit has replaced. The commit does nothing
//! for this itself. A commit that landed between the read and the write
//! is found when the write is made, from the key sets the transaction has
//! taken in since the read, and the write is marked as met then. One that
//! lands after the write meets it as it meets any write (CF2). The commit
//! finds either mark alike ([`crate::txn`], "A kept value and the read it
//! is of").
//!
//! This is the one place where a conflict may be reported that CF2 and
//! CF2a do not ask for: a write that keeps a value by a read the
//! transaction cannot date is taken to be met. That is a read more than 64
//! commits before the write, a handle the transaction did not show a node
//! with since it last ended, or any handle of the frames on which a write
//! was made that was itself met in this way. None of them happens where
//! one thread reads and writes a Merk with no commit in between.
//!
//! For a kept value the write path keeps the key sets of the newest 64
//! commits, and a transaction that has no write holds back no more than
//! those. A write that keeps a value where no node is left under the key
//! is refused where it is made, and that refusal is a conflict to its
//! caller where the transaction can tell that commits outside are the
//! cause ([`BatchError::ValueTaken`](crate::txn::BatchError::ValueTaken)).
//!
//! # Rollback
//!
//! [`Committer::rollback`] gives up the write set, the private copies, the
//! page ids the transaction took and its version pin, with its savepoints
//! (WT9). It writes to no file and reads no committed page, so it costs
//! what the transaction holds and nothing that depends on the size of the
//! database. The thread then does for write-back what it does after a
//! commit. The transaction can be written again.
//!
//! # A write with no transaction
//!
//! [`Committer::write`] applies one batch to a write transaction of its own
//! and commits it at once (RD7). It waits for no open write transaction,
//! and its commit is a commit like any other to them: one that wrote a key
//! of the batch before fails with a conflict (CF8). The transaction is the
//! write's own, so whatever the error nothing of the batch is left: a
//! batch that is refused is [`CommitError::Refused`], and none of its
//! writes is committed.
//!
//! The transaction of such a write has read nothing, so its batch has no
//! read of its own to name: a node in it that keeps a committed value and
//! names no handle is refused
//! ([`BatchError::NoRead`](crate::txn::BatchError::NoRead)), and one that
//! names a handle of another transaction is taken to be met, which is a
//! conflict. A writer that keeps values reads them through a transaction
//! of its own, writes through it and commits it.
//!
//! # A page that is freed again before it was written back
//!
//! A commit gives every page it frees a free link in the page table
//! (`page-table.md` K4), which says from which version on the page is no
//! more. Ids are used again (ID2). So a page can be made under an id that
//! named another page before and be freed again before write-back gave it
//! a link: the newest link of its id is then still the free link of the
//! earlier page, and the table takes no second one
//! ([`LinkError::AlreadyFree`]).
//!
//! **The commit goes on as for any page it frees.** No segment ever held
//! the page, so the table has nothing to say of it that the earlier link
//! does not say: the page is free. The buffer pool keeps its versions for
//! the readers that can still ask for them, and the id goes back to the
//! allocator once, at the commit's version. [`Published::freed_unlinked`]
//! counts these pages.
//!
//! The free link of a page is also what keeps write-back from linking a
//! block of it afterwards (`page-table.md` T18), and the earlier page's
//! link is too old to do that for this one. Write-back therefore links
//! a block only in a page step in which the pool still owes the page
//! version of the block, and the commit pushes the free link once more
//! after it has told the pool: a link that a pass pushed between the two
//! has the free link above it, and the page is not counted here.
//!
//! What the free link would also have done is hold a checkpoint of the page
//! table back until the log is durable through the freeing commit. That is
//! held by the place of the checkpoint's copy instead (`page-table.md` K14
//! item 2): a checkpoint is named only with the log durable to where it
//! ended when the copy was taken, and the group of the freeing commit is in
//! the log before the commit frees anything. A change that frees a page
//! before its group is appended would break this.
//!
//! # What other tasks take from here
//!
//! - **The checkpoint** (TXN-06) takes [`CommitState::lock`] for its
//!   sampling step (CK2), and holds it for that step only.
//! - **The open of the engine** (TXN-07) makes the [`CommitState`] and the
//!   [`Committer`], and passes [`CommitState::reporting`] whoever is to be
//!   told the errors of write-back in a committer's own turns. It gives
//!   the write path the store's counts
//!   ([`WritePath::stats`](crate::txn::WritePath::stats)), in which a
//!   write transaction is open from its first write until its commit or
//!   its rollback ends it. It gives the write path the value-log file
//!   that loads append to
//!   ([`LoadFiles`](crate::txn::LoadFiles)) and the manifest's lock.
//! - **Pending marks** ([`crate::scope`], SC9): a transaction on which one
//!   stands is not committed ([`CommitError::NotFinalized`], CM1). The
//!   write that takes the mark off is finalize's (GROVE-11).
//! - **The reclaimer** takes [`Published::dropped`], what each Merk that
//!   the commit dropped or cleared left behind (CL6).
//! - **GroveDB's storage** (GROVE-01) passes the hash rules of its
//!   GroveVersion to a commit, and [`NoStaleHashes`] on the existing ones.
//!
//! [`WriteBack::committed`]: lignin_store::flush::WriteBack::committed
//! [`WriteBack::sync_log`]: lignin_store::flush::WriteBack::sync_log
//! [`WriteBack::place_ready`]: lignin_store::flush::WriteBack::place_ready
//! [`WriteBack::wait_for_room`]: lignin_store::flush::WriteBack::wait_for_room
//! [`WriteBack::roll_log`]: lignin_store::flush::WriteBack::roll_log
//! [`LogStore::settle`]: lignin_store::wal::LogStore::settle
//! [`LogError::PlaceTaken`]: lignin_store::wal::LogError::PlaceTaken
//! [`LogError::TakenBack`]: lignin_store::wal::LogError::TakenBack
//! [`LogError::Unsettled`]: lignin_store::wal::LogError::Unsettled
//! [`LogError::SyncFailed`]: lignin_store::wal::LogError::SyncFailed
//! [`LogError::NoActiveFile`]: lignin_store::wal::LogError::NoActiveFile
//! [`LinkError::AlreadyFree`]: lignin_store::table::LinkError::AlreadyFree

mod committer;
mod error;
mod outcome;
mod state;

pub use committer::{Committer, NoStaleHashes};
pub use error::{CommitError, Failure, Stopped};
pub use outcome::{Committed, Published};
pub use state::{CommitLock, CommitState, CommitStats};

#[cfg(all(test, not(loom)))]
mod tests;
