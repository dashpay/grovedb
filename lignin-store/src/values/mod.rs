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
//! move rule, O5), or went with a dropped page. They call
//! [`ValueStore::apply`] at the change point, as one unit with the cell
//! change.
//!
//! **Where the count is stored.** Not in the value log's own files, and not
//! in the manifest. It is part of the page state, and is stored and
//! recovered by the same means as the pages:
//!
//! - what stores a change point for recovery (the log group of a commit, the
//!   record of a page step) stores its [`ValueChange`] with it
//! - what stores a checkpoint of the pages stores [`ValueStore::counts`]
//!   with it, taken at the same point as the pages
//! - recovery hands the checkpoint's counts to [`ValueStore::recover`],
//!   replays each later change point's change through
//!   [`ValueRecovery::apply`] together with its pages, and ends with
//!   [`ValueRecovery::finish`]
//!
//! So after any crash the counts are those of the page state recovery lands
//! on (O7). A count kept anywhere else could run ahead of the pages, reach
//! zero, and let a file be deleted while a recovered cell still points into
//! it.
//!
//! **A count that is wrong.** If a pointer is released from a file whose
//! count is already zero, the count was too low: some cell held a pointer it
//! did not include. The store cannot tell how many more there are. It
//! reports [`ValueStoreError::Miscounted`], and from then on the file is
//! kept whatever its count says ([`FileCount::miscounted`], which is stored
//! and recovered with the count).
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
//! 2. It asks [`ValueOwners::holds`] whether the current version of that page
//!    holds the pointer. A hit proves the value is live. A miss proves
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
//!    page version, that a cell of it still holds the old pointer, and that
//!    no open write transaction holds a private copy of the page. If so it
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
//! all its bytes ([`Cleaned::owners_not_found`]).
//!
//! What makes the page step one step is the lock the implementation of
//! [`ValueOwners::rewrite`] holds on the page while it checks and installs
//! (level 5 of the lock order LT7). The store's own lock on the counts is
//! taken inside it, for the length of [`PointerRewrite::install`], and the
//! store calls nothing while it holds that lock. A commit applies its
//! change the same way, where it installs its pages.
//!
//! Other background work that rewrites a value pointer, such as regrouping
//! a page's values when the page is consolidated (TREE-08), appends the
//! value again under a [`ValueHold`], has the manifest cover it, and applies
//! [`ValueChange::moved`] inside its own page step.
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
//! | It became empty at a version no later than the oldest pinned version. | A snapshot pinned before that version reads a page version in which a cell still holds a pointer into it (O8). |
//!
//! The caller adds one more, which only it can see: every operation that was
//! running when the file became empty has ended (PN6), since one of them may
//! have read a pointer and not yet followed it.
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
//! | [`ValueStore::create`] | No file. Its id is not used again in this session. |
//! | [`ValueWriter::append`] | The file's values, ending where they did. |
//! | A sync of a file | The file as it was last synced, which is all a manifest names of it. It takes no more values ([`ValueStoreError::FileFailed`]). |
//! | A cleaning pass, at any step | The rewrites installed so far, each counted. Every other copy is dead space. The file being cleaned is untouched. |
//! | [`ValueStore::commit`] | As [`SegmentStore::commit`](crate::segment::SegmentStore::commit) leaves the manifest. |
//! | The deletion of a dropped file | The new manifest, in force, and the file, which the next call or the next session deletes ([`ValueStoreError::NotDeleted`]). |
//!
//! # What the caller does
//!
//! - It holds the directory's lock and a [`ManifestStore`] of the directory,
//!   as for the [segment store](crate::segment).
//! - After every [`ManifestStore::update`] that returns `Ok`, it calls
//!   [`ValueStore::manifest_replaced`]. [`ValueStore::commit`] does both.
//!   [`ValueStore::commit`] and [`ValueStore::clean`] change only how the
//!   manifest names value-log files, so no other store has to be told of
//!   the manifests they write.
//! - It stores a [`ValuePointer`] where a later session will find it only
//!   under a manifest that covers the value. When a manifest gives a file
//!   that holds pointers a new valid length, the value-log files are synced
//!   after that file and before [`ValueStore::describe`].
//! - It reports every pointer a cell starts or stops holding, at its change
//!   point ([`ValueStore::apply`]), stores that change and the counts with
//!   the pages, and recovers them with the pages.
//! - It appends every value under the [`ValueHold`] of the transaction or
//!   background run the value is for, and drops the hold only after the
//!   change that counts the value was applied, or when the value is given
//!   up.
//! - It calls [`ValueStore::mark_durable`] once the changes up to a mark
//!   will survive a crash, and [`ValueStore::drop_empty`] with the oldest
//!   pinned version, once the operations that were running have ended.
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
pub use store::{Cleanable, ValueRecovery, ValueStore, DEFAULT_TARGET_FILE_LEN};
pub use walk::ValueWalk;
pub use writer::{ValueHold, ValueWriter};
