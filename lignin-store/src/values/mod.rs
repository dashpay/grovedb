//! The value log: the files `val/<n>.val` of a database directory, which
//! hold the values that are too long to sit inside their cell.
//!
//! - [`ValueStore`] holds the value-log files a manifest names. It reads a
//!   value by its pointer, keeps each file's count of live values, cleans a
//!   file by moving its live values, and deletes a file once nothing can
//!   reach it.
//! - [`ValueWriter`] appends values to one file.
//! - [`ValueOwners`] is what the layers above implement so that the cleaner
//!   can find the cell that owns a value and give it a new pointer.
//!   [`ValueChange`] is how they report the pointers that cells start and
//!   stop holding.
//!
//! The bytes are `lignin-format`'s: `docs/lignindb/value-log-format.md`
//! specifies them, and its rules O4 to O9 are the cleaner's.
//! `docs/lignindb/transactions.md` has the rules for the page step (PV6,
//! PV7, CL5) and for values written ahead of a commit (FL1). This module
//! knows no cell, key or page layout.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{manifest::Manifest, CommitVersion, PageId};
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::{
//!     segment::ManifestStore,
//!     values::{ValueChange, ValueHold, ValueStore},
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let empty = Manifest::new(Default::default(), None, Vec::new())?;
//! let mut manifest = ManifestStore::create(vfs, dir.path(), empty)?;
//! let values = ValueStore::open(&manifest, &[])?;
//!
//! // A new file takes values once the manifest names it.
//! let mut writer = values.create()?;
//! values.commit(&mut manifest)?;
//!
//! // A transaction appends a value for a cell of page 7, ahead of its commit.
//! let page = PageId::new(7).expect("not zero");
//! let mut hold = ValueHold::new();
//! let pointer = writer.append(&mut hold, page, b"a value too long for its cell")?;
//!
//! // Before the commit stores the pointer, the manifest covers the value.
//! writer.sync()?;
//! values.commit(&mut manifest)?;
//!
//! // The commit makes the cell hold the pointer, and the value counts.
//! let mut change = ValueChange::new();
//! change.acquire(pointer);
//! values.apply(&change, CommitVersion::new(1).expect("a version"))?;
//! drop(hold);
//! assert_eq!(values.count(pointer.file()).map(|count| count.values), Some(1));
//!
//! let mut buf = Vec::new();
//! let value = values.read(pointer, page, &mut buf)?;
//! assert_eq!(value.contiguous(), Some(&b"a value too long for its cell"[..]));
//! # Ok(())
//! # }
//! ```
//!
//! # The life of a file
//!
//! | Step | What happens | How manifests name the file from then on |
//! | --- | --- | --- |
//! | Create | [`ValueStore::create`] creates `val/<id>.val` under an id above every id used before, appends the file header, syncs the file and syncs `val/`. | Open, 20 bytes valid. |
//! | Name | The manifest is replaced. From here the file takes values. | |
//! | Append | [`ValueWriter::append`] appends one value: its records, back to back, in one append. | |
//! | Sync | [`ValueWriter::sync`] or [`ValueStore::sync`] syncs the file. | Open, valid up to the last value synced. |
//! | Close | [`ValueWriter::finish`] syncs the file and ends the writer. A file whose writer was dropped is closed by the next sync that covers all of it. | Closed, valid to its end. |
//! | Empty | The file's count of live values reaches zero. | |
//! | Drop | [`ValueStore::drop_empty`] takes the file out of the store, once nothing can reach it. | Not named. |
//! | Delete | [`ValueStore::manifest_replaced`] removes the file, once a manifest that does not name it is durable. | |
//!
//! The store's own record of its files is what is true now, and a manifest
//! is a durable copy of it. [`ValueStore::describe`] writes that record into
//! a manifest, and [`ValueStore::commit`] replaces the manifest with the
//! result. It leaves every other kind of file in the manifest as it is, so
//! it composes with [`SegmentStore::describe`] inside one
//! [`ManifestStore::update`].
//!
//! When a writer's file reaches the store's target length
//! ([`ValueWriter::is_full`]), the writer is finished and another is created.
//! A value is never split over two files. Several writers can be open at
//! once, each on its own file: a transaction's, the cleaner's, the
//! flusher's.
//!
//! A file that the manifest names was written by an earlier session if this
//! session did not create it. Such a file is read and never written: its
//! file is opened read-only, and if the earlier session left it open it ends
//! at its valid length (rule M16 of the format). The first manifest this
//! session writes names it closed at that length (M15).
//!
//! # When a value is durable, and what a tail means
//!
//! A value is durable once a sync of its file that began after its append
//! has returned, and it belongs to the database across sessions once a
//! durable manifest gives its file a valid length at or past its end.
//!
//! Bytes past a file's valid length are not part of the database for a later
//! session, whatever they hold: after a crash they may be whole values, torn
//! ones or zeros, and the store does not look. Within the valid length every
//! byte belongs to a whole value, and a walk that finds otherwise fails with
//! the offset it reached.
//!
//! So a [`ValuePointer`] is stored where a later session will find it (a log
//! group, a segment block, a page-table checkpoint) only under a manifest
//! that covers the value. [`ValueStore::covers`] says whether the durable
//! manifest does. Values past a file's valid length can be read until the
//! session ends, and not after it.
//!
//! # The count of live values
//!
//! The store keeps, for each file, how many cells of the latest version hold
//! a pointer into it, and how many bytes those values take. The count comes
//! from what the layers above report, never from back pointers (O6).
//!
//! **Reporting.** Every change to a cell has one change point: the commit of
//! its transaction, or the page step of background work. For each change
//! point the layers above build one [`ValueChange`]: every pointer a cell
//! starts to hold, and every pointer a cell stops holding because the cell
//! was deleted, was given another value, had its value appended again (the
//! move rule, O5), or went with a dropped page. They apply it at the change
//! point, as one unit with the cell change. The releases are counted first,
//! against the counts as they were before the change, so that what the
//! change acquires cannot hide a release the count did not include.
//!
//! **The order of the log is the order of the counts.** A change point
//! takes the store's lock on the counts ([`ValueStore::step`]), writes its
//! record to the log, makes the cell change, and applies the
//! [`ValueChange`] with [`CountStep::apply`], which gives the lock back. So
//! no change is in the log and not yet in the counts while another reads
//! them, and the changes are in the log in the order the counts took them.
//! [`ValueStore::apply`] is the short form for a caller that orders its
//! change points by a lock of its own which a sample of the counts also
//! takes.
//!
//! **Where the count is stored.** Not in the value log's own files, and not
//! in the manifest. It is part of the page state, and is stored and
//! recovered by the same means as the pages:
//!
//! - what stores a change point for recovery (the log group of a commit, the
//!   record of a page step) stores its [`ValueChange`] with it
//! - a sample of the counts is stored with **its place in the log**: the
//!   counts include every change whose record is before that place and none
//!   whose record is after it. To take one, take [`ValueStore::step`], read
//!   [`CountStep::counts`] and the end of the log, and end the step
//! - recovery hands a sample to [`ValueStore::recover`], gives
//!   [`ValueRecovery::apply`] the change of every record after the sample's
//!   place that it keeps, in the order of the log and each once, and ends
//!   with [`ValueRecovery::finish`]
//!
//! The counts are then those of the last record recovery keeps, which is
//! the page state it lands on (O7). A count kept anywhere else could run
//! ahead of the pages, reach zero, and let a file be deleted while a
//! recovered cell still points into it.
//!
//! **A sample does not have to agree with any copy of the pages.** A
//! checkpoint of the page table copies each page at another moment, and a
//! page it copied may already hold a change whose record is after the
//! sample's place. Recovery skips that change for the page, which has it,
//! and still gives it to [`ValueRecovery::apply`]: what the counts include
//! is decided by the place alone. Three things are asked of whoever stores
//! a sample:
//!
//! - The place is at or before the record from which the pages are
//!   replayed, and the log is kept from the place on.
//! - The sample is put in force (named by a manifest, or written to the
//!   log itself) only once the log is durable through its place.
//! - The replay of a pointer rewrite is skipped for a page whose cell no
//!   longer holds the old pointer. A pointer is never given to a second
//!   cell, so this tells a page that has the rewrite from one that lacks it,
//!   which the page's commit version cannot: a rewrite takes none.
//!
//! **A count that is wrong.** If a pointer is released from a file whose
//! count is already zero, or whose counted bytes are fewer than the value
//! takes, or whose last value goes and leaves bytes counted, the count did
//! not include it: some cell held a pointer that was never counted. The
//! store cannot tell how many more there are. It reports
//! [`ValueStoreError::Miscounted`], and from then on the file is kept
//! whatever its count says ([`FileCount::miscounted`], which is stored and
//! recovered with the count). The same error reports an acquired pointer
//! that no durable manifest covers, which is still counted.
//!
//! # Values that no count includes yet
//!
//! A value is appended before the change that makes a cell hold its pointer:
//! ahead of its transaction's commit, or ahead of the page step that
//! installs a moved value. Until that change is applied, the value is held
//! by the [`ValueHold`] it was appended under, and its file is not deleted
//! (FL1). The hold is dropped after the change is applied, or when the
//! values are given up, as after a rollback. They are then dead space and go
//! with their file. No hold survives a crash, and neither does a value that
//! no recovered change acquired.
//!
//! # Cleaning
//!
//! A cleaning pass over a closed file is given another file's writer and
//! the [`ValueOwners`] of the layers above. It has two halves, with the
//! manifest between them, and [`ValueStore::clean`] is the three in one.
//!
//! [`ValueStore::copy_live`]:
//!
//! 1. It walks the file. Each value comes with its back pointer, a page id,
//!    and with the pointer its owner must hold.
//! 2. It asks [`ValueOwners::find`] whether the current version of that page
//!    holds the pointer. A hit proves the value is live, and comes with what
//!    the implementation needs to know the page version again. A miss proves
//!    nothing: the pass does nothing with the value and goes on (O4).
//! 3. It appends each value that was hit to the other file, with the same
//!    back pointer. The value's bytes, length and owner do not change.
//!
//! Then the other file is synced and the manifest is replaced, so that every
//! copy is covered before a page points at one.
//!
//! [`ValueStore::install`]:
//!
//! 4. It checks that the durable manifest covers every copy, and installs
//!    nothing if it does not.
//! 5. It asks [`ValueOwners::rewrite`] to install each new pointer in one
//!    page step (PV7). The implementation checks that it changes the current
//!    page version, that no commit has superseded the page since the pass
//!    looked at it (CL5), that a cell of it still holds the old pointer, and
//!    that no open write transaction holds a private copy of the page. If so it
//!    stores the rewrite with the pages, installs the pointer and calls
//!    [`PointerRewrite::install`], which takes one value off the old file's
//!    count and adds one to the new file's, inside that step. If not,
//!    nothing is installed and nothing is counted: the copy is dead space
//!    and the value is tried again in a later pass.
//!
//! A move takes no commit version (PV6). The pass never removes a value and
//! never decides that a file is empty: only the count does. A file whose
//! values were all moved has a count of zero. A file with a value whose
//! owner the pass could not find keeps a count above zero and stays, with
//! all its bytes.
//!
//! **An owner that cannot be found is an error.** A pass ends with
//! [`ValueStoreError::OwnersNotFound`] if the file still has live values, no
//! rewrite was refused for now, and no value of the file was counted while
//! the pass ran. Then nothing explains the values that are left but a cell
//! that was carried to another page without its value, against the move
//! rule (O5, O6). The error carries what the pass did, which stands. If a
//! rewrite was refused or a value was counted meanwhile, the pass proves
//! nothing of the kind and returns what it did: a later pass may find the
//! owners. [`ValueStore::cleanable`] does not list a file again until its
//! count has changed since the last pass over it.
//!
//! **Locks.** What makes the page step one step is the lock the
//! implementation of [`ValueOwners::rewrite`] holds on the page while it
//! checks and installs (level 5 of the lock order LT7), and nothing is
//! acquired under that lock. The store's lock on the counts is taken
//! before it: [`ValueStore::install`] takes it, then calls
//! [`ValueOwners::rewrite`], and [`PointerRewrite::install`] counts the
//! rewrite with the lock it came with. In LT7's order the lock on the
//! counts comes after the frame latches of level 4 and before level 5.
//! While the store holds it for itself, it takes no other lock, touches no
//! file and calls nothing, and no reader ever takes it. What can wait for it
//! is another change point, and the calls that read every count
//! ([`ValueStore::counts`], [`ValueStore::cleanable`],
//! [`ValueStore::retire`], [`ValueStore::drop_empty`]), never a holder of a
//! page's lock.
//!
//! Other background work that rewrites a value pointer, such as regrouping
//! a page's values when the page is consolidated (TREE-08), appends the
//! value again under a [`ValueHold`], has the manifest cover it, takes
//! [`ValueStore::step`], then its page step, and applies
//! [`ValueChange::moved`] with [`CountStep::apply`] inside that page step.
//!
//! [`ValueStore::cleanable`] lists the files worth a pass: closed, with live
//! values, and with at least a given share of dead bytes. How often passes
//! run and how they are throttled is the caller's.
//!
//! # Deleting a file
//!
//! [`ValueStore::drop_empty`] takes a file out of the store only when all of
//! this holds:
//!
//! | Condition | Why |
//! | --- | --- |
//! | The file is closed. | A writer may still append a value to it. |
//! | Its count is zero and not known to be wrong. | A cell of the latest version points into it (O6). |
//! | No [`ValueHold`] is on it. | A transaction appended a value to it that its commit will count (FL1). |
//! | The change that brought the count to zero is durable ([`ValueStore::mark_durable`]). | A crash would recover pages from before that change, which point into the file (CL5). |
//! | It became empty at a version no later than the oldest version that was pinned when the caller began to wait. | A snapshot pinned before that version reads a page version in which a cell still holds a pointer into it (O8). |
//! | It became empty before the caller began to wait, and every operation that was running then has ended. | An operation may have read a pointer into it and not yet followed it (PN6). |
//!
//! The last two are the caller's part, and they are taken together.
//! [`ValueStore::retire`] is given the oldest pinned version and returns a
//! [`Retired`]: the files that are empty for every reader from that moment
//! on. The caller waits until every operation that was running then has
//! ended, and passes the token to [`ValueStore::drop_empty`].
//!
//! Both halves are fixed before the wait. An operation that begins after
//! [`ValueStore::retire`] reads the latest version or a version that is
//! pinned then, and neither holds a pointer into a file the token covers.
//! The oldest pin as it is after the wait would not do: a snapshot of an
//! older version can end during the wait, after an operation that the wait
//! does not include read a pointer through it. A file that became empty
//! after the token was made, or that an older pin kept out of it, waits for
//! the next token.
//!
//! A checkpoint that is being created needs the files of the manifest it
//! sampled until it has linked them, whatever their counts (CK3). It takes
//! [`ValueStore::hold_files`] in its sampling step. A file under that hold
//! is neither dropped nor, if it was dropped before, deleted. The hold and
//! [`ValueStore::drop_empty`] exclude each other: a file is either held and
//! stays in the store, or was dropped first and stays on disk.
//!
//! The file itself is removed once a durable manifest no longer names it. A
//! crash between the two leaves a file that no manifest names, which the
//! next [`ValueStore::open`] removes.
//!
//! # What holds after a crash
//!
//! Given the file layer's contract ([`lignin_io::vfs`]), and a caller that
//! keeps the rules below, after a crash at any point:
//!
//! - Every value-log file the manifest names is in `val/`, and the bytes
//!   below its valid length are whole values, as they were appended.
//! - Every pointer in the recovered pages names a file the manifest names
//!   and a value below that file's valid length, which reads back unchanged.
//! - The recovered counts are the number of recovered cells that point into
//!   each file.
//! - A file in `val/` that the manifest does not name holds nothing the
//!   database refers to. [`ValueStore::open`] removes it.
//!
//! # When the file layer fails
//!
//! | Failed | Left behind |
//! | --- | --- |
//! | [`ValueStore::create`] | No file in the store. Its id is not used again in this session. If the file it began could not be removed, the next session removes it. |
//! | [`ValueWriter::append`] | The file's values, ending where they did. |
//! | A sync of a file | The file as it was last synced, which is all a manifest names of it. It takes no more values ([`ValueStoreError::FileFailed`]). |
//! | A cleaning pass, at any step | The rewrites installed so far, each counted. Every other copy is dead space. The file being cleaned is untouched. |
//! | [`ValueStore::commit`] | As [`SegmentStore::commit`](crate::segment::SegmentStore::commit) leaves the manifest. |
//! | The deletion of a dropped file | The new manifest, in force, and the file, which the next call or the next session deletes ([`ValueStoreError::NotDeleted`]). |
//!
//! # What the caller does
//!
//! - It holds the directory's lock and a [`ManifestStore`] of the directory,
//!   as for the [segment store](crate::segment). The store checks that a
//!   manifest it is given is of its directory by the path and by nothing
//!   else ([`ValueStoreError::WrongDirectory`]): two file layers that both
//!   have the path are the caller's to keep apart.
//! - After every [`ManifestStore::update`] that returns `Ok`, it calls
//!   [`ValueStore::manifest_replaced`]. [`ValueStore::commit`] does both.
//!   [`ValueStore::commit`] and [`ValueStore::clean`] change only how the
//!   manifest names value-log files, so no other store has to be told of
//!   the manifests they write.
//! - It stores a [`ValuePointer`] where a later session will find it only
//!   under a manifest that covers the value. Before it writes a change to
//!   the log, it checks [`ValueStore::covers`] for every pointer the change
//!   acquires, while the hold on those values is alive. When a manifest
//!   gives a file that holds pointers a new valid length, the value-log
//!   files are synced after that file and before [`ValueStore::describe`].
//! - It syncs a value-log file that is being written through the store and
//!   through nothing else ([`ValueStore::sync`], [`ValueWriter::sync`]). The
//!   file layer does not say, on one handle, that a sync through another
//!   failed, and a value appended before such a failure would then pass for
//!   durable. A checkpoint links a file up to the valid length its manifest
//!   gives, which is durable already.
//! - It reports every pointer a cell starts or stops holding, at its change
//!   point, with the record written while the store's lock on the counts is
//!   held ([`ValueStore::step`]). It calls [`ValueStore::apply`] and the
//!   other calls that read the counts while it holds no lock on a page.
//! - It stores each change with its change point, stores a sample of the
//!   counts with its place in the log, and recovers the counts from a
//!   sample and every change after its place.
//! - It appends every value under the [`ValueHold`] of the transaction or
//!   background run the value is for, and drops the hold only after the
//!   change that counts the value was applied, or when the value is given
//!   up.
//! - It calls [`ValueStore::mark_durable`] once the changes up to a mark
//!   will survive a crash. It calls [`ValueStore::retire`] with the oldest
//!   pinned version, and [`ValueStore::drop_empty`] with what that returned
//!   once every operation that was running then has ended. After it opened
//!   or recovered the store, it marks what it opened from durable once it
//!   is: a log that a killed process left reads back whole and can still be
//!   cut short by a loss of power.
//! - It takes [`ValueStore::hold_files`] for a checkpoint, from its sampling
//!   step until the files are linked.
//!
//! [`SegmentStore::describe`]: crate::segment::SegmentStore::describe
//! [`ManifestStore`]: crate::segment::ManifestStore
//! [`ManifestStore::update`]: crate::segment::ManifestStore::update
//! [`ValuePointer`]: lignin_format::value_log::ValuePointer

mod clean;
mod counts;
mod error;
mod file;
mod store;
mod walk;
mod writer;

#[cfg(test)]
mod tests;

pub use clean::{Cleaned, CopiedValues, Installed, PointerRewrite, Refusal, Rewrite, ValueOwners};
pub use counts::{ChangeMark, FileCount, ValueChange};
pub use error::{Miscount, OwnersError, ValueStoreError};
pub use store::{
    Cleanable, CountStep, Retired, ValueRecovery, ValueStore, DEFAULT_TARGET_FILE_LEN,
};
pub use walk::ValueWalk;
pub use writer::{ValueHold, ValueWriter};
