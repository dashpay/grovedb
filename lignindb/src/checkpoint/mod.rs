//! Checkpoints: one committed version of an open database, as a database
//! directory of its own that shares the files of the live one.
//!
//! - [`Checkpointer`] makes checkpoints of one open database
//!   ([`Checkpointer::create`]). It is a view of the database's commit
//!   path, made from its [`Committer`](crate::commit::Committer).
//! - [`Created`] says what a creation made, and [`CheckpointOptions`] how
//!   much log it leaves for the checkpoint's first open to replay.
//! - [`delete`] removes a checkpoint's directory.
//! - [`CheckpointError`] is every way a creation or a deletion fails.
//!
//! `docs/lignindb/transactions.md` section 11 is the specification (CK1 to
//! CK10), with DU1 for durability, LT7 for the order of the locks and row
//! 15 and rows 18 to 21 of section 13 for a creation beside the other
//! actors. `segment-manifest.md` says how a manifest names a file that two
//! databases share (S3, M15, M16, M18), and `platform-requirements.md` what
//! Platform does with checkpoints (P-14 to P-20).
//!
//! No example runs here: a [`Checkpointer`] is made of a
//! [`Committer`](crate::commit::Committer), which is made where the
//! database is opened (TXN-07). The tests make both over a store on disk
//! and over the simulated file system (`txn/tests/store.rs`).
//!
//! # What a checkpoint is
//!
//! A directory with a manifest, and with the numbered files that manifest
//! names, each under the name it has in the live database:
//!
//! ```text
//! <target>/
//!   MANIFEST             the page size, the last file ids and the table
//!                        checkpoint of the live manifest; every file closed
//!                        or sealed, with its valid length
//!   wal/00000045.log     hard links to the live files
//!   seg/00000310.seg
//!   val/00000012.val
//!   table/00000122.pt
//! ```
//!
//! **It is exactly one committed version** (CK1): the state of the version
//! *vc* that was the latest at the sampling step, with every commit up to
//! it, nothing of a later one and nothing uncommitted (CK8). The store's
//! [recovery](lignin_store::recovery) lands on *vc* when it opens the
//! directory, so **opening a checkpoint is the normal open**. Nothing in
//! the directory says that it is a checkpoint, and from its first open on
//! it is a database like any other: with a lock, versions and pins of its
//! own, taking commits, written back and cleaned by itself (CK7, P-15).
//!
//! **No data is copied.** The two databases share files as hard links, and
//! neither changes a shared byte: a file is only ever appended to, a
//! session appends to no file of an earlier session (M16, B11), and the
//! store opens every file that a manifest names read-only. So the live
//! database goes on appending to the files it was writing, behind the
//! lengths the checkpoint's manifest gives them, and the checkpoint starts
//! files of its own under ids above the last ids it took over. Its
//! manifest names every file closed, or sealed if it was: what lies behind
//! the valid length is the live database's alone, a later footer included
//! (S3).
//!
//! # The steps of a creation
//!
//! | Step | What is done | Held |
//! | --- | --- | --- |
//! | 1 | The target is made, a new directory, and its name is made durable. One that exists is refused with nothing touched (P-14). | nothing |
//! | 2 | If write-back holds more log than the checkpoint is to leave for its first open, it catches up: a pass and a table checkpoint, as its own loop takes them (CK6). | nothing of the checkpoint's |
//! | 3 | **The sample** (CK2): the latest version *vc*, where the log ends, and the durable manifest. A session that takes no commit any more, or whose log holds a commit it has not published, is refused. The version is pinned and the whole log is held (CK3). | the commit lock and the lock on the manifest, which is the file-set lock |
//! | 4 | The table checkpoint, the segments and the value-log files that the sampled manifest names are linked into the target, and so are the files the engine has the checkpoint carry. | the lock on the manifest; the pin; the hold on the log |
//! | 5 | The log is made durable, by the log itself, through the sampled end. *vc* is durable in the live database from here (CK4, DU1). | the pin; the hold on the log |
//! | 6 | The log's files up to the one the sample ended in are linked. | the same |
//! | 7 | What could not be linked is copied, and each copy is synced. | nothing: each such file is open |
//! | 8 | Every name in the target is made durable, and then the manifest is written, as the first manifest of a database directory is: to a temporary file that is synced, renamed, and made durable with the directory. | nothing |
//!
//! **Commits wait for step 3 and for nothing else** (CK5, CM8). The commit
//! lock is given back as soon as the three things are read, before the
//! first link is made. The lock on the manifest is held through step 4,
//! which is one `link` for each file the manifest names: write-back
//! replaces no manifest and deletes no file for that long (row 18 of
//! section 13), and a committer meets it only where it waits for
//! write-back. The sample waits for that lock while it holds the commit
//! lock, so a commit can wait for one replacement of the manifest that
//! write-back was in when the sample came.
//!
//! **The manifest is written last, and it is what makes the directory a
//! checkpoint.** A directory without one is not a database: a creation
//! that was cut short, by an error or by the power, leaves nothing that
//! opens.
//!
//! # Why that is the state of *vc*
//!
//! The checkpoint is what a power cut at the sampling step would have left
//! of the live database, had the log survived it exactly to its end: the
//! durable manifest, the files it names up to the lengths it vouches for,
//! and the log. The store recovers from every such state to the last
//! commit whose group the log holds whole, which is *vc*. Each part is
//! there for a reason:
//!
//! - **The manifest and the end of the log are read under both locks.**
//!   Under the commit lock no group is appended and no version published,
//!   so the log ends at the end of a record at or after the group of *vc*
//!   and holds no later group. It can hold records of page steps that were
//!   installed after that group: they change no record of the state. Each
//!   of them names only what a manifest covered before it was appended
//!   (FL7), and under the lock on the manifest that is the sampled one.
//!   A manifest that was sampled later than the log could name a table
//!   checkpoint that covers a commit after *vc*.
//! - **Segments, value-log files and the table checkpoint are taken at the
//!   lengths the sampled manifest gives, and are not synced.** A manifest
//!   vouches only for bytes that were synced before it was written, the
//!   table checkpoint it names points to no block behind those lengths,
//!   and a block that write-back has appended since is reached by no link
//!   that a recovery would load. So nothing is gained by taking more, and
//!   nobody but a store syncs a file it is writing.
//! - **Only the log is synced, and the log does it.** A sync that fails
//!   through another handle is invisible to the log (`lignin_store::wal`),
//!   so the creation asks write-back for the sync and links the log's
//!   files afterwards. It never syncs a log file, under its name or under
//!   a link.
//! - **The file the sample ended in is named closed at the sampled end.**
//!   The live database appends to it at once, and the sync of step 5
//!   covers what it appended meanwhile. The checkpoint's manifest gives the
//!   length of the sample, so none of it is read (CK5).
//!
//! # What the live database is held to
//!
//! **While a checkpoint is being made** it holds, from the sample until
//! every file is in the target or open to be copied (CK3):
//!
//! - a [version pin](crate::snapshot::VersionPin) on *vc*, which holds
//!   back what any pin on that version does: the reuse of page ids freed
//!   since, the page versions the pool keeps for readers, the reclaiming
//!   of what a later drop left;
//! - the whole log ([`LogHold`](lignin_store::wal::LogHold)): a table
//!   checkpoint that write-back installs meanwhile drops no log file;
//! - for step 4, the lock on the manifest. **Every file that a durable
//!   manifest names is on disk, and its name is removed only under that
//!   lock**, by the `manifest_replaced` of its store. The creation relies
//!   on that and on nothing else for segments, value-log files and the
//!   table checkpoint: once a file has a name in the target, or is open,
//!   what the live database does with its own name is its business. A
//!   store that deletes files (STORE-08) keeps that rule, and needs to
//!   know of no checkpoint.
//!
//! All of it is given back when the creation ends, whether it made a
//! checkpoint or not. Where files are copied, it is given back before the
//! copying begins: a creation that copies for minutes holds back nothing
//! but the space of the files it has open.
//!
//! **A checkpoint that was made holds nothing in the live database**
//! (CK7): no pin, no hold, no entry anywhere. The live database commits,
//! writes back, reclaims and cleans as if it were not there, and deletes
//! its own names of the files the two share whenever it has no more use
//! for them. What a checkpoint that is kept for long costs is disk space
//! and nothing else:
//!
//! - A shared file is on disk once. Its bytes are given back when both
//!   names are gone.
//! - So the checkpoint keeps the bytes of every file the live database
//!   has deleted since: at most everything the live database had when the
//!   checkpoint was made, reached when the live database has rewritten or
//!   let go of all of it. A cleaner that moves live blocks out of a
//!   shared segment and deletes it frees nothing until the checkpoint
//!   goes, and writes the blocks a second time.
//! - A checkpoint that is open pins and cleans within its own directory,
//!   as any database does.
//!
//! # Durability, and a creation that fails
//!
//! **When [`Checkpointer::create`] returns `Ok`, the checkpoint is durable
//! and so is its version in the live database** (CK4). After a power cut
//! at any later moment the target opens at *vc*, and the live database
//! opens at *vc* or later.
//!
//! A cut during the creation leaves the target without a manifest, or not
//! at all, or whole: every name is durable before the manifest is written,
//! and the manifest's name appears in one rename. Removing a directory
//! that is left without one is the caller's (CK10, P-18).
//!
//! When the creation fails, the live database is as it was (CK10), except
//! that its log may be durable further. The creation removes what it made
//! in the target, the manifest first, and the target itself, so that the
//! same call can be made again. It removes nothing it did not make. Where
//! the file layer fails that too, what is left is for the caller to
//! remove. An error of the catch-up of step 2 does not fail the creation:
//! it is reported in [`Created::catch_up`], and the checkpoint's first
//! open replays more log.
//!
//! # Links and copies
//!
//! A file is linked where the file system links it, and copied where it
//! reports that it cannot: the two directories are on different file
//! systems, the file system has no hard links, or the file has as many
//! names as it can have. Any other failure of a link fails the creation.
//! The choice is made for each file, so a target on the file system of
//! some of the live directories shares those.
//!
//! A copy is of the first bytes of the file, as many as the checkpoint's
//! manifest gives it, read through a handle that was opened in step 4 or
//! step 6, and it is synced before the manifest names it. It costs the
//! time and the space of the whole database, and the checkpoint is then
//! the consistent copy of the database that a backup wants (OPS-08).
//!
//! # What the engine keeps beside the store's files
//!
//! The roots of the four flat keyspaces, of the subtree directory and of
//! the keyspace of unlinked pages are page ids that are the same for good
//! (`lignin_tree::flat`), and the pages are the checkpoint's as they are
//! the live database's. Where the engine notes which pages they are is its
//! own to choose (TXN-07). If that is a file in the database directory,
//! or if the engine keeps any other file there that an open needs, the
//! checkpoint has to have it: [`Checkpointer::carrying`] names such files,
//! and each is linked or copied in step 4, before the manifest is written.
//! A checkpoint takes nothing else from the database directory: not the
//! lock file, and not what somebody else keeps there, such as the
//! directory of the checkpoints themselves (P-03, P-14).
//!
//! # Deleting a checkpoint, and removing one that is open
//!
//! **Deleting a checkpoint is removing its directory.** Nothing in the
//! live database knows of it. [`delete`] does it with two checks, and a
//! caller that removes the directory itself does as well (P-18).
//!
//! Platform removes the directory of a checkpoint while a handle has it
//! open. That handle goes on reading: its pages are in its pool, and the
//! files it has open stay until it closes them. What it writes afterwards
//! goes to files that have no name, or fails where a file would have to be
//! created, since no part of the store creates a directory after its open.
//! So the close of such a handle reports an error and brings nothing back,
//! and the live database, which shared only links with it, is as it was.
//! The test
//! `a_checkpoint_whose_directory_is_removed_while_it_is_open_closes_cleanly`
//! shows it for the parts of a close that exist here: write-back's last
//! turn and the drop of every store.
//!
//! # What other tasks take from here
//!
//! - **The open of the engine** (TXN-07) opens a checkpoint as it opens
//!   any directory, and is told nothing by the directory. If it keeps a
//!   file beside the store's, whoever makes checkpoints names it
//!   ([`Checkpointer::carrying`]). Its close of a database whose directory
//!   is gone ends with the errors of write-back and no more: it creates no
//!   directory.
//! - **GroveDB's engine** (GROVE-01, GROVE-08) makes a [`Checkpointer`]
//!   from the committer of the open database for `create_checkpoint`,
//!   which refuses an existing path as the trait asks, and opens a
//!   checkpoint with the open of the engine. GroveDB's own
//!   `delete_checkpoint` needs nothing of the engine.
//! - **The segment cleaner** (STORE-08) deletes a file as the other stores
//!   do: only once a durable manifest no longer names it, in its
//!   `manifest_replaced`, under the lock on the manifest. "Once no
//!   checkpoint needs them" is then kept by the creation alone. Space that
//!   a checkpoint holds is not the cleaner's to find.
//! - **The crash campaign** (VER-04) and **the stress test** (VER-05) have
//!   the model to hold a checkpoint to: it opens at the one version its
//!   creation reported, whatever ran beside it. The loom model of the
//!   commit lock against the sample and a manifest replacement is VER-05's
//!   (`transactions.md` 15.2).
//! - **Backup** (OPS-08): a checkpoint on another file system is a
//!   consistent copy. A full disk fails the creation with the error of the
//!   file layer and leaves the live database as it was.
//!
//! # Tests
//!
//! - `tests/acceptance.rs`: the sentences of the task's "Done when" but
//!   the timings, and T-CK2 and the second half of T-CK3.
//! - `tests/races.rs`: T-CK1, T-CK4 and the first half of T-CK3: a
//!   creation beside commits, page steps and table checkpoints, on threads
//!   and with the live database let in at a chosen step.
//! - `tests/crash.rs`: the power cut and the file layer failed at every
//!   operation of a creation, on the simulated file system.
//! - `tests/files.rs`: copies, carried files, deletion, and the errors.
//! - `tests/reference.rs`: the timings on a store of the reference
//!   database's size, left out of a normal run.

mod create;
mod error;
mod files;

pub use create::{CheckpointOptions, Checkpointer, Created, DEFAULT_CATCH_UP_ABOVE};
pub use error::CheckpointError;
pub use files::delete;

#[cfg(all(test, not(loom)))]
mod tests;
