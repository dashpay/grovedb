//! The write-ahead log: the files `wal/<n>.log` of a database directory.
//!
//! - [`LogStore`] is the log of a running session. It appends the group of
//!   a commit, the record of a background change and a checkpoint mark,
//!   each in one append to the log file. It turns from one file to the
//!   next, makes the log durable, reads a pending value by its address, and
//!   deletes a file once the caller no longer needs it.
//! - [`LogReader`] is the recovery iterator. It reads the log files a
//!   manifest names, item by item, and stops before the first item that is
//!   not whole.
//! - [`SyncPolicy`] says when a commit makes the log durable: never, when
//!   the last sync is an interval ago, or every time.
//! - [`LogHold`] keeps the log from the place of a page-table checkpoint's
//!   sample on.
//!
//! The bytes are `lignin-format`'s: `docs/lignindb/log-format.md` specifies
//! them (rules LG1 to LG32), and its section 11 says what this module takes
//! from the format. `docs/lignindb/transactions.md` has the rules of a
//! commit (CM3, CM5, CM7, CM9), of durability (DU1 to DU3) and of the
//! locks (LT7). This module knows no page, key or tree. It does not write a
//! group or a record: it is given their bytes.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{
//!     manifest::Manifest,
//!     wal::{GroupWriter, LogItem},
//!     PageId, PageSize,
//! };
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::{
//!     segment::ManifestStore,
//!     wal::{LogOptions, LogReader, LogStore},
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let empty = Manifest::new(PageSize::DEFAULT, Default::default(), None, Vec::new())?;
//! let mut manifest = ManifestStore::create(vfs.clone(), dir.path(), empty)?;
//!
//! // A session reads the log to its end, opens it there, and turns to a
//! // file of its own.
//! let end = LogReader::new(vfs.clone(), dir.path(), manifest.manifest()?).into_end()?;
//! let log = LogStore::open(&manifest, &end, LogOptions::default())?;
//! log.roll(&mut manifest)?;
//!
//! // A commit asks where its group goes, writes the group for that place,
//! // and appends it: one append to the log file.
//! let page = PageId::new(7).expect("not zero");
//! let at = log.next_group()?;
//! let mut group = Vec::new();
//! let mut writer = GroupWriter::new(at.version(), at.address(), &mut group)?;
//! let pending = writer.value(page, b"a value too long for its cell")?;
//! writer.finish();
//! let appended = log.append_group(&at, &group)?;
//!
//! // The wait for durability is another call. The default policy asks for
//! // no sync at a commit.
//! assert_eq!(log.sync_for_commit(&appended)?, None);
//!
//! // Until the value has a place in the value log, its cell leads to its
//! // bytes in the log.
//! let mut buf = Vec::new();
//! let value = log.read_pending(&pending, page, &mut buf)?;
//! let bytes = value.bytes().and_then(|bytes| bytes.contiguous());
//! assert_eq!(bytes, Some(&b"a value too long for its cell"[..]));
//!
//! // `flush()`: the commit is durable when this returns.
//! let durable = log.sync()?;
//! assert_eq!(durable.place, Some(appended.end));
//!
//! // A recovery reads the items back, up to the last whole one.
//! let mut reader = LogReader::new(vfs, dir.path(), manifest.manifest()?);
//! let version = reader.next_item(|read| match read.item {
//!     LogItem::Commit(group) => Some(group.version()),
//!     _ => None,
//! })?;
//! assert_eq!(version, Some(Some(at.version())));
//! assert_eq!(reader.next_item(|_| ())?, None);
//! # Ok(())
//! # }
//! ```
//!
//! # The life of a log file
//!
//! | Step | What happens | How manifests name the file from then on |
//! | --- | --- | --- |
//! | Create | [`LogStore::create`] creates `wal/<id>.log` under an id above every id used before, appends the file header, syncs the file and syncs `wal/`. The log has one such file at a time: the file it turns to next. | Open, 20 bytes valid. |
//! | Name | The manifest is replaced. From here the log can turn to the file. | |
//! | Turn | [`LogStore::rotate`] appends the file's start record, which says where the file before it ends and which commit it comes after (LG9). The file before it takes no more items, and this one takes them. | |
//! | Append | [`LogStore::append_group`], [`LogStore::append_change`] and [`LogStore::append_mark`] append one item each. | |
//! | Sync | [`LogStore::sync`] syncs the file, after every file before it. | Open, valid up to the last item synced. |
//! | Finish | The log turns to the next file, or an append to this one fails. A file whose end is synced after that is closed. | Closed, valid to the end of its last item. |
//! | Drop | [`LogStore::drop_before`] takes the file out of the log, once the caller needs nothing before a place in a later file. | Not named. |
//! | Delete | [`LogStore::manifest_replaced`] removes the file, once a manifest that does not name it is durable. | |
//!
//! The log's own record of its files is what is true now, and a manifest is
//! a durable copy of it. [`LogStore::describe`] writes that record into a
//! manifest, and [`LogStore::commit`] replaces the manifest with the
//! result. It leaves every other kind of file in the manifest as it is, so
//! it composes with the `describe` of the other stores inside one
//! [`ManifestStore::update`]. [`LogStore::roll`] is the call that keeps the
//! files going: sync, turn if a turn is due, create the next file, replace
//! the manifest.
//!
//! **A file takes items only once a durable manifest names it.** A recovery
//! reads the log files its manifest names and no others. An item in a file
//! that no manifest names would be lost in a crash however well the file
//! was synced.
//!
//! **The length of a file is not a limit.** A file is full at
//! [`LogOptions::target_file_len`], and [`LogStore::is_due`] says so. The
//! log goes on in a full file until it is turned. No append turns the log:
//! a turn is one more append, to the next file, and an append is one
//! append. An item is never split over two files.
//!
//! # An append
//!
//! **One item is one append to the log file**, and nothing else: no sync,
//! no second write, no call of the file layer besides. On the operating
//! system's files that is one `write` on a descriptor opened with
//! `O_APPEND`. A commit's group goes into the log whole, with all of its
//! value records and parts, however long it is.
//!
//! **What is appended is what recovery will take.** The bytes are read with
//! the format's own reader before they are appended, as records that follow
//! the last item of the log: they must be exactly one item of the kind the
//! call appends, a group must have the version after the last one, and a
//! mark and a freed page must not be ahead of the commits. Bytes that fail
//! are refused and do not reach the file ([`LogError::Refused`],
//! [`LogError::WrongItem`], [`LogError::NotOneItem`]). So nothing stands in
//! the log that a recovery would stop at, short of a failure of the file
//! layer, and the log agrees with recovery about every item by using its
//! test (CM9). A group is read outside the append lock. For the group of a
//! block of 20 transitions, 35 KB, that takes about as long as encoding
//! the group's bytes does: some microseconds, which the test
//! `reading_a_group_before_it_is_appended_costs_what_writing_its_bytes_costs`
//! measures.
//!
//! **A group is appended at the place it was written for, or not at all**
//! (LG22). The cells of a group hold log addresses inside it, so the commit
//! must know where its group will lie before it writes it.
//! [`LogStore::next_group`] gives the end of the log as a [`GroupPlace`]:
//! the file and offset, the log address and the version. The commit writes
//! the group for it and passes both to [`LogStore::append_group`], which
//! takes the group only if the log still ends there
//! ([`LogError::PlaceTaken`]). The log sets nothing aside for a group, so a
//! place that is asked for and not used needs no giving back. Keeping
//! others out between the two calls is the caller's: see [the locks](#locks).
//!
//! **The append and the wait for durability are two calls.** An append
//! returns once the file layer has the bytes.
//! [`LogStore::sync_for_commit`] is the fsync a [`SyncPolicy`] asks of a
//! commit, [`LogStore::sync_through`] waits for one place, and
//! [`LogStore::sync`] for everything appended so far. A page step makes
//! only the first call.
//!
//! # Durability
//!
//! [`LogStore::sync`] syncs the files in the order of the log: every file
//! the log has turned away from that is not durable to its end, then the
//! file the log ends in. It returns how far the log is durable as a
//! [`LogDurable`](crate::table::LogDurable): a place, and the version of
//! the last group before it. That is what installing a table checkpoint
//! asks for, and [`LogStore::durable`] gives it at any time. One sync runs
//! at a time, and a caller whose place another sync has covered returns
//! without one.
//!
//! **An item is durable only when every item before it is.** A log ends at
//! its first item that is not whole, and a file continues the log only
//! where the one before it stops being valid. So a place in a later file is
//! never reported durable while an earlier file is not durable to its end.
//!
//! | Policy | A commit | Besides |
//! | --- | --- | --- |
//! | [`SyncPolicy::Never`] (the default, DEC-04) | syncs nothing | `flush()` and a checkpoint call [`LogStore::sync`] |
//! | [`SyncPolicy::Interval`] | syncs when the last sync is at least the interval ago | a timer calls [`LogStore::sync_if_due`], so that the commits before a pause do not wait for the next commit |
//! | [`SyncPolicy::EveryCommit`] | syncs through its own group | |
//!
//! The log is not padded after a sync: storage writes a sector whole or not
//! at all, so an append behind bytes that are durable does not endanger
//! them (`lignin_io::vfs`).
//!
//! # When the file layer fails
//!
//! | Failed | What the call returns | Left behind |
//! | --- | --- | --- |
//! | An append of an item, and the file has the length it had | [`LogError::NotAppended`] | The log syncs the file before it returns: what the append wrote and took back could otherwise come back in a crash, all of it. The item is not in the log, and no crash brings it back. The file takes no more items. |
//! | An append, and a part of the item stays in the file | [`LogError::NotAppended`] | The same, without the sync: no crash completes the item. A recovery finds the file valid up to the item before. |
//! | An append, and all of the item is in the file, as it was given | `Ok` | The item is in the log: the commit has happened (CM9). The file takes no more items, because its handle is not trusted with another append. |
//! | An append, and the log cannot find out which of these holds: the length or the bytes cannot be read, or the sync fails | [`LogError::Unsettled`] | The item may be in the log, now or after a crash. The log takes nothing more in this session. The next open settles it. |
//! | A sync of a file this session writes | [`LogError::SyncFailed`] | Nothing behind what was durable becomes durable in this session: this and every later sync fail, without a call of the file layer. The log still takes items. |
//! | The append of a start record, in [`LogStore::rotate`] | [`LogError::NotAppended`], or `Ok` if the record is in the file | The log has not turned and ends where it did. The file it would have turned to is given up, and deleted once no manifest names it. If the record is in the file, the log has turned to a file that takes no items, and turns again. |
//! | [`LogStore::create`] | [`LogError::Io`] | No file. Its id is not used again in this session, and no manifest records it. |
//! | [`LogStore::commit`] | as [`ManifestStore::update`] | The old manifest, or the manifest in doubt. A file that was created is not turned to until a manifest names it. |
//! | The deletion of a dropped file | [`LogError::NotDeleted`] | The new manifest, in force, and the file, which the next call or the next session deletes. |
//!
//! **After a failed append the log goes on in the next file.** The file
//! ends at its last whole item. [`LogStore::rotate`] turns to the next
//! file, whose start record says where this one ends, and the next commit
//! takes the version of the one that failed. Nothing is appended behind
//! the failed bytes. If [`LogStore::roll`] has left a file ready, the turn
//! is one append and needs no manifest, so it can be made at once. The
//! next manifest names the file closed at that length. This is the cut of
//! CM9: a file that ends where a start record of the next file says, and
//! then a closed entry.
//!
//! **A failed sync cannot be made good** (`lignin_io::vfs`): the bytes that
//! were not durable are unknown, and a later sync that succeeds does not
//! cover them. They cannot be appended again somewhere else either: a group
//! is written for one place. So a session whose log fails a sync cannot
//! make another commit durable. Its commits are still in the log, and a
//! recovery takes each of them whose bytes a crash keeps. The session after
//! it writes the file again (below), and from then on they are durable.
//!
//! # Reading the log
//!
//! [`LogReader`] gives the items of the log in order: for each, where it
//! begins and ends, and the item as the format's reader gives it. A group
//! comes whole or not at all (LG13).
//!
//! | Where | What the reader does |
//! | --- | --- |
//! | A file the manifest names closed | Reads it up to its valid length. It must hold whole items to that length. |
//! | A file the manifest names open | Reads it to its end. Whole items behind the valid length are part of the log. The first failure behind it is where the file's part of the log ends. |
//! | A failure below the valid length, in a closed or an open file | [`LogError::Damaged`]. The manifest vouches for those bytes, and to end the log there would take back commits that were durable (LG32). |
//! | The end of a file | The next file the manifest names continues the log if its start record is the one the log so far asks for: the base and the version of where this file stops being valid (LG9). |
//! | A next file with no start record, with a torn one, or with one for another end of the file before it | The log ends with the file before it. This file and every later one hold nothing of the log ([`LogEnd::unread`]). It is what a crash leaves that took the end of a file and kept the file after it. |
//! | The same, in a file whose valid length covers its start record | [`LogError::Damaged`]. A manifest gives a file such a length only once the file before it is durable to its end, so the two must fit. |
//! | A file that is missing, shorter than its valid length, of another id or kind | An error. |
//!
//! When it has ended, [`LogReader::end`] gives the [`LogEnd`]: each file
//! with the length that is part of the log, the files that hold nothing of
//! it, the version of the last group, and how far the manifest vouches for
//! the log. A reader changes nothing in the directory. It holds a window of
//! one file at a time, never a whole file, and at least the longest item.
//!
//! # The next session
//!
//! A session never appends to a file of an earlier session (B11, M16).
//! [`LogStore::open`] takes the [`LogEnd`] that the session's recovery
//! read. Every file of it ends where its last whole item ends. The files
//! that hold nothing of the log are taken out. The first
//! [`LogStore::roll`] then:
//!
//! 1. makes those files durable to that end
//! 2. replaces the manifest: it names them closed at that length, names no
//!    file that holds nothing of the log, and names the session's own first
//!    file
//! 3. turns to that file, whose start record says where the log ended
//!
//! So before the first commit of a session a durable manifest says where
//! each file of the earlier sessions stops being valid (DU3), and a commit
//! that a crash took is never read again, whatever bytes of it are still in
//! a file. The versions it had are given out again.
//!
//! **A file of an earlier session whose end the manifest does not vouch for
//! is written again, not synced.** The session that wrote it may have had a
//! sync of it fail and ended with no crash. A sync that succeeds now would
//! cover nothing that was appended before that failure, and this session
//! cannot know. So the bytes of the file up to its end are written into a
//! new file `wal/<id>.log.tmp`, which is synced and renamed over the file.
//! The file is not written in place and not appended to, and whoever holds
//! the old file as a hard link keeps it. It costs one copy of the log that
//! was not synced when the last session ended, and nothing after a session
//! that synced before it ended. The next open removes a `.tmp` file that a
//! crash left.
//!
//! # What a table checkpoint asks of the log
//!
//! `docs/lignindb/page-table.md` has the rules (K14), and
//! [`TableCheckpoints`](crate::table::TableCheckpoints) checks what a
//! table can check. The log gives it three things.
//!
//! **Where the log ends.** [`LogStore::end`] is the place after the last
//! record of the log, and it never goes back: not over a turn, not over an
//! append that failed, and not from one session to the next, which begins
//! where the log of the last one ends. Every item whose append has
//! returned is before it. A page step appends its record before its change
//! reaches a block, so the record of every change that has reached a block
//! is before it too. `TableCheckpoints::write` is given `|| log.end()`,
//! and asks before it copies its first chain and after its last.
//!
//! **How far the log is durable**, as the version of the last group and as
//! a place: what [`LogStore::sync`] returns. The sync that is begun after
//! `write` has returned covers the end of its copy, and what it returns is
//! passed to `TableCheckpoints::install`.
//!
//! **The files from the sample's place on.** The mark of a sample is
//! appended with [`LogStore::append_mark`], and the end of the mark is the
//! sample's place. [`LogStore::hold_from`] keeps the log from that place
//! on for as long as the [`LogHold`] it returns lives: no file from the
//! file of the place on is dropped, also not when the mark is the last
//! item of its file and every commit in the file is covered. Whoever
//! writes checkpoints has one hold for each checkpoint that the manifest
//! names or may come to name: the one in force, one that is written and
//! not discarded, and both after `ManifestInDoubt`. With no checkpoint in
//! force it holds the whole log. The log knows no checkpoint: it keeps
//! what it is told to keep.
//!
//! # Dropping files
//!
//! [`LogStore::drop_before`] is given the earliest place of the log that
//! the caller still needs, and takes out every file before the file of
//! that place and before the file of every [`LogHold`]. What needs a place
//! is the caller's to know (FL6): the commits the table checkpoint in
//! force does not cover, and every pending value. The place of a
//! checkpoint's sample is kept by its hold. The log checks one more thing:
//! that it is durable up to the place it is given. Then the first file
//! that is left has a start record that no crash takes away, and what is
//! left never reads as a log with no file.
//!
//! # What holds after a crash
//!
//! Given the file layer's contract ([`lignin_io::vfs`]), and a caller that
//! keeps the rules below, after a crash at any point a [`LogReader`] of the
//! manifest that is in force reads without an error, and what it reads is:
//!
//! - a run of the items whose append returned `Ok`, in the order they were
//!   appended, from the first item of the first file that was not dropped,
//!   with no item missing in it and nothing else in it
//! - with every item that a sync reported durable
//! - with no item whose append returned [`LogError::NotAppended`], whenever
//!   the crash comes
//!
//! An item whose append returned [`LogError::Unsettled`] is the last item
//! of that run or is not in it. After the first manifest of the next
//! session it stays one or the other.
//!
//! With a table checkpoint in force, the log file of its sample's place is
//! among the files that are read, from its first item, and the item that
//! ends at the place is the checkpoint's mark.
//!
//! The tests check this on the simulated file system for every state a
//! crash can leave after each step of drawn sessions, with faults at every
//! operation and as they fall, across sessions that go on after a crash,
//! and with table checkpoints that are written, installed, discarded and
//! left in doubt while log files are deleted.
//!
//! # Locks
//!
//! The log has one lock that callers meet: the append lock, which an append
//! and a turn hold for one call of the file layer. **Nothing is acquired
//! under it**, so a page step can take it under its own lock (D-059, level
//! 5 of LT7). It is not held while a group is read before its append, while
//! a file is synced, created or deleted, or while a manifest is replaced.
//! The one exception is an append that the file layer fails: the log finds
//! out what became of it, with a read of the file's length, at most one
//! read of its bytes and at most one sync, before it gives the lock back,
//! so that nothing is appended behind bytes it knows nothing of.
//!
//! The other locks are the log's own and are held for no call of the file
//! layer, apart from one that keeps two syncs and one that keeps two
//! creations from running at once.
//!
//! **Who keeps other appends out of a group's place.** Between
//! [`LogStore::next_group`] and [`LogStore::append_group`] nothing else may
//! be appended, or the group is refused and written again. Other commits
//! are kept out by the commit lock. Every append that is not a commit's is
//! made under the lock on the counts of live values (level 4a of LT7): the
//! records of the page steps and the checkpoint mark. So a commit takes
//! that lock before it asks for its place and holds it until its group is
//! in, as a commit whose cells stop holding value pointers does already
//! (CM3). [`LogStore::rotate`] moves the end of the log as well: call it
//! under one of the two locks, or from the committer itself.
//!
//! # What the caller does
//!
//! - It holds the directory's lock and a [`ManifestStore`] of the
//!   directory, as for the [segment store](crate::segment). The log checks
//!   that a manifest it is given is of its directory by the path and by
//!   nothing else ([`LogError::WrongDirectory`]).
//! - It reads the log with a [`LogReader`] before it opens it, and passes
//!   what the reader found to [`LogStore::open`].
//! - It calls [`LogStore::roll`] before the session's first append, and
//!   from then on whenever [`LogStore::is_due`], and from time to time:
//!   outside a commit and outside a page step. Before the session ends it
//!   calls [`LogStore::sync`] and [`LogStore::commit`], so that the next
//!   session has no file to write again.
//! - After every [`ManifestStore::update`] that returns `Ok`, it calls
//!   [`LogStore::manifest_replaced`]. [`LogStore::commit`] does both.
//! - It gives every append the bytes of one item, written by
//!   `lignin-format`, and gives a group the version and the address of its
//!   [`GroupPlace`].
//! - It keeps other appends out between asking for a group's place and
//!   appending the group (above).
//! - A commit appends its group and then calls
//!   [`LogStore::sync_for_commit`]. It installs and publishes only when
//!   the append returned `Ok`. A sync that fails then leaves the commit
//!   published and not durable (`NotDurable`). After
//!   [`LogError::NotAppended`] the commit has failed, and the log is
//!   turned before the next one. After [`LogError::Unsettled`] the session
//!   publishes nothing more.
//! - A page step appends its record under its own lock and installs its
//!   change only when the append returned `Ok`.
//! - It takes the sample of the counts for a table checkpoint together
//!   with [`LogStore::append_mark`], under the lock on the counts: the
//!   sample's place is the end of the mark. It takes a [`LogHold`] from
//!   that place before it gives the lock back.
//! - It gives `TableCheckpoints::write` the end of the log as
//!   `|| log.end()`. It syncs the log after `write` has returned and
//!   before the checkpoint is named, and passes what the sync returned to
//!   `TableCheckpoints::install` (LG29, K14).
//! - It keeps a [`LogHold`] for the checkpoint the manifest names, from
//!   [`CheckpointInfo::log_kept_from`], taken when the session opens the
//!   log: the whole log if the manifest names none. It drops the hold of
//!   a checkpoint when another one has been installed, or when the
//!   checkpoint has been discarded, and not while the manifest is in
//!   doubt.
//! - It calls [`LogStore::drop_before`] only with a place before which it
//!   needs nothing.
//! - After [`LogError::SyncFailed`] it ends the session when it can and
//!   opens the directory again: no commit of that session becomes durable
//!   before.
//!
//! [`ManifestStore`]: crate::segment::ManifestStore
//! [`ManifestStore::update`]: crate::segment::ManifestStore::update
//! [`CheckpointInfo::log_kept_from`]: crate::table::CheckpointInfo::log_kept_from

mod error;
mod file;
mod reader;
mod store;

#[cfg(test)]
mod tests;

pub use error::LogError;
pub use reader::{FileEnd, LogEnd, LogRead, LogReader, DEFAULT_READ_WINDOW_LEN};
pub use store::{
    Appended, GroupPlace, LogHold, LogOptions, LogStore, SyncPolicy, DEFAULT_TARGET_FILE_LEN,
};
