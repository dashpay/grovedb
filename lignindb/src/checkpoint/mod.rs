//! Checkpoints: one committed version of an open database, as a database
//! directory of its own that shares the files of the live one.
//!
//! - [`Checkpointer`] makes checkpoints of one open database
//!   ([`Checkpointer::create`]). It is a view of the database's commit
//!   path, made from its [`Committer`](crate::commit::Committer).
//! - [`Created`] says what a creation made, and [`CheckpointOptions`] how
//!   much log it leaves for the checkpoint's first open to replay.
//! - [`delete`] removes a checkpoint's directory, and
//!   [`DELETING_FILE_NAME`] marks one that it is removing.
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
//! | 1 | A session that takes no commit any more is refused, and so is a target that is there, with nothing touched (P-14). | nothing |
//! | 2 | If write-back holds more log than the checkpoint is to leave for its first open, it catches up: a pass and a table checkpoint, as its own loop takes them (CK6). | nothing of the checkpoint's |
//! | 3 | The target is made, a new directory, and its lock is taken, as for any database directory that is written. A target that holds anything but its lock file by then is somebody else's, and is refused as one that was there. | the lock of the target, to the end |
//! | 4 | **The sample** (CK2): the latest version *vc*, where the checkpoint's log ends, and the durable manifest. The log ends where the live log ends, or, if the live log is durable through *vc* already, where it is durable to. A session whose log holds a commit it has not published is refused. The version is pinned and the whole log is held, and the value-log files are held as soon as the commit lock is given back (CK3). | the commit lock and the lock on the manifest, which is the file-set lock |
//! | 5 | The table checkpoint, the segments and the value-log files that the sampled manifest names are linked into the target, and so are the files the engine has the checkpoint carry. | the lock on the manifest; the pin; the holds |
//! | 6 | The log is made durable, by the log itself, through the sampled end, unless it is. *vc* is durable in the live database from here (CK4, DU1). | the pin; the holds |
//! | 7 | The log's files up to the one the sample ended in are linked. | the same |
//! | 8 | What could not be linked is copied, and each copy is synced. | nothing: each such file is open |
//! | 9 | Every name in the target is made durable, and then the target's own name. | nothing |
//! | 10 | The manifest is written, as the first manifest of a database directory is: to a temporary file that is synced, renamed, and made durable with the directory. | nothing |
//!
//! **Commits wait for the reads of step 4 and for nothing else** (CK5,
//! CM8). The commit lock is given back as soon as the three things are
//! read, before the first link is made. And no commit waits for the lock
//! on the manifest, which write-back holds over a sync when it replaces a
//! manifest and which another creation holds while it links: the sample
//! only tries that lock while it has the commit lock. Where the manifest
//! is taken, it gives the commit lock back, waits, and takes both anew.
//!
//! **A commit that syncs the log itself can wait for a sync of the log,
//! and a creation makes as few as it can.** The log makes itself durable
//! one sync at a time (`lignin_store::wal`), and such a commit syncs
//! under the commit lock (CM7), where it waits for a sync that is under
//! way, whoever makes it: write-back in its own loop, a flush, or a
//! creation. CK5 and CM8 word "never waits for the fsync" for commits
//! that sync nothing, which is the default, and where no commit syncs the
//! log no commit waits for a sync. For the other policies:
//!
//! - **Step 6 syncs nothing where the log is durable through *vc* at the
//!   sample.** The checkpoint's log then ends where the log is durable
//!   to. That is so after a flush and after the catch-up of step 2, and,
//!   where every commit syncs the log
//!   ([`SyncPolicy::EveryCommit`](lignin_store::wal::SyncPolicy::EveryCommit)),
//!   after every commit whose sync did not fail. After one that failed,
//!   the sync of step 6 fails as well, at once.
//! - **The catch-up of step 2 is a table checkpoint of write-back's, and
//!   syncs the log as every one of them does**: a commit meets it as it
//!   meets write-back's loop. It is due only above
//!   [`CheckpointOptions::catch_up_above`], and `None` there takes it
//!   away.
//! - Where commits sync the log at an interval
//!   ([`SyncPolicy::Interval`](lignin_store::wal::SyncPolicy::Interval)),
//!   a commit whose sync falls due while the one of step 6 is under way
//!   waits for it, and then makes its own.
//!
//! So with a policy that syncs, a commit can wait for one sync of the log
//! that a creation caused, in the last two cases. Taking that away is the
//! log's to do, as it is for write-back's own syncs, and the wording of
//! CK5 and CM8 is the owner's to decide on.
//!
//! **The lock on the manifest is held through step 5**, which is one
//! `link` for each file the manifest names: write-back replaces no
//! manifest and deletes no file for that long (row 18 of section 13). A
//! committer meets it only where its thread takes a turn for write-back
//! after its commit is published.
//!
//! **The manifest is written last, and it is what makes the directory a
//! checkpoint.** A directory without one is not a database. So a creation
//! that was cut short before its manifest was written, by an error or by
//! the power, leaves nothing that opens as the checkpoint, and one that
//! was cut short after it leaves the whole checkpoint or nothing that
//! opens ("Durability, and a creation that fails").
//!
//! # Why that is the state of *vc*
//!
//! The checkpoint is what a power cut at the sampling step would have left
//! of the live database, had the log survived it to its end, or to the
//! place it was durable to if that is behind the group of *vc*: the
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
//! - **Where the log is durable through *vc*, the checkpoint's log ends
//!   where it is durable to.** That place is the end of the log as a sync
//!   found it, at or after the group of *vc*, so it is the end of a record
//!   and has every group up to *vc* before it. The mark of the table
//!   checkpoint that the sampled manifest names is before it too, since a
//!   manifest names a table checkpoint only once the log is durable
//!   through its mark. What the log holds behind that place are records of
//!   page steps, which the checkpoint does without: the values they placed
//!   are read from the log, where a power cut would have left them.
//! - **Segments, value-log files and the table checkpoint are taken at the
//!   lengths the sampled manifest gives, and are not synced.** A manifest
//!   vouches only for bytes that were synced before it was written, the
//!   table checkpoint it names points to no block behind those lengths,
//!   and a block that write-back has appended since is reached by no link
//!   that a recovery would load. So nothing is gained by taking more, and
//!   nobody but a store syncs a file it is writing.
//! - **Only the log is synced, and the log does it.** A sync that fails
//!   through another handle is invisible to the log (`lignin_store::wal`),
//!   so the creation asks write-back for the sync, where one is needed,
//!   and links the log's files afterwards. It never syncs a log file,
//!   under its name or under a link.
//! - **The file the sample ended in is named closed at the sampled end.**
//!   The live database appends to it at once, and a sync of step 6 covers
//!   what it appended meanwhile. The checkpoint's manifest gives the
//!   length of the sample, so none of it is read (CK5).
//!
//! `transactions.md` words three of its rules for a creation that records
//! the lengths of the files, syncs them and holds a file set while it
//! links (CK2, CK3, CK4). What is built keeps what they are for by other
//! means: the lengths are the manifest's, which are synced already; the
//! file set is kept by the lock on the manifest until each file has a
//! second name, and the log by a hold; and the one file that is synced is
//! the log.
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
//! - the value-log files
//!   ([`ValueStore::hold_files`](lignin_store::values::ValueStore::hold_files)),
//!   as the value log asks of a checkpoint. The hold is taken under the
//!   lock on the manifest and not under the commit lock: it looks at
//!   every file the value log has;
//! - for step 5, the lock on the manifest. **Every file that a durable
//!   manifest names is on disk, and its name is removed only under that
//!   lock**, by the `manifest_replaced` of its store. The creation relies
//!   on that for the segments and the table checkpoint, which no hold
//!   keeps: once a file has a name in the target, or is open, what the
//!   live database does with its own name is its business. A store that
//!   deletes files (STORE-08) keeps that rule, and needs to know of no
//!   checkpoint.
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
//! at all, or whole. Every name in it is durable before its own name is,
//! and that before the manifest is written, whose name appears in one
//! rename.
//!
//! **A target that is left without a manifest is not a database, and it
//! must not become one.** A power cut can keep the making of the target
//! and lose everything after it, so the directory can be left empty, and
//! the file layer has no rename of a directory by which the target could
//! appear whole, as a RocksDB checkpoint does. Removing such a directory
//! is the caller's (CK10, P-18). Until it is removed, **an open that makes
//! a database where it finds none must not take it for a place where there
//! is none**: Platform opens every directory beside its checkpoints with
//! the open that creates (P-19), and would have an empty database for a
//! checkpoint. See "What other tasks take from here".
//!
//! When the creation fails, the live database is as it was (CK10), except
//! that its log may be durable further. One failure is the live
//! database's as much as the creation's: if the sync of the log fails
//! ([`CheckpointError::Sync`]), the log makes nothing more durable until
//! the database is opened again (`lignin_store::wal`). The creation
//! removes what it made in the target, the manifest first, and the target
//! itself, so that the same call can be made again. It removes nothing it
//! did not make: a file that somebody else put into the target while it
//! was made stays, and the target with it. Where the creation does not
//! get the lock of the target it has just made, somebody else may have
//! it, and where it gets the lock and finds more than the lock file,
//! somebody else has been there: nothing in the target is touched in
//! either case. A manifest in the target is the creation's to remove only
//! once it has asked the store to write one, and not where the store
//! answers that one is there. The name a manifest is written under first
//! is the store's to use in a database directory, and a file of that name
//! is replaced. Where the file layer fails the removal too, or fails the
//! making of a file without saying whether it made it, what is left is
//! for the caller to remove, and so is a target that could not be listed
//! once its lock was taken: it holds its lock file, and [`delete`]
//! removes it. **No file goes while a
//! manifest could come back**: where a manifest was written and its
//! removal cannot be made durable, every file stays, so that a power cut
//! leaves the whole checkpoint or a directory without a manifest, and not
//! a manifest without one of its files. An error of
//! the catch-up of step 2 does not fail
//! the creation: it is reported in [`Created::catch_up`], and the
//! checkpoint's first open replays more log.
//!
//! # Links and copies
//!
//! A file is linked where the system links it, and copied where it
//! reports that it does not: the two directories are on different file
//! systems, the file has as many names as it can have, or the file system
//! has no hard links, which is reported as "not supported" or, on Linux,
//! as "not permitted". Linux says "not permitted" also where its policy
//! keeps one file from being linked (a file of another owner where links
//! are protected, a file marked append-only), and the two cannot be told
//! apart. **So a link that is not permitted becomes a copy wherever the
//! file can be read and the copy created**, and the checkpoint is as good
//! a one. It then costs what a copy costs, and nothing says so but
//! [`Created::copied`] and [`Created::copied_bytes`]: an engine that
//! expects links looks there. Any other failure of a link fails the
//! creation. The choice is made for each file, so a target on the file
//! system of some of the live directories shares those.
//!
//! A copy is of the first bytes of the file, as many as the checkpoint's
//! manifest gives it, read through a handle that was opened in step 5 or
//! step 7, and it is synced before the manifest names it. It costs the
//! time and the space of the whole database, and the checkpoint is then
//! the consistent copy of the database that a backup wants (OPS-08).
//!
//! # What the first open of a checkpoint costs
//!
//! The store's recovery reads every log file the manifest names from its
//! first item (`lignin_store::recovery`), and replays the commits that the
//! table checkpoint does not cover. The catch-up of step 2 is what leaves
//! few commits to replay. It does not shorten what is read: the mark of a
//! table checkpoint is appended where the log ends, and the file it is in
//! is kept whole, so an open reads up to one log file's length
//! ([`LogOptions::target_file_len`](lignin_store::wal::LogOptions::target_file_len))
//! before it reaches the mark. How long the log's files are is therefore
//! what bounds the open of a checkpoint, and of any database, from below.
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
//! and each is linked or copied in step 5, before the manifest is written.
//! Each is a file, and not a link to one, which would lead nowhere from
//! the target.
//! A checkpoint takes nothing else from the database directory: not what
//! somebody else keeps there, such as the directory of the checkpoints
//! themselves (P-03, P-14), and not the lock file. Its own lock file is
//! made with it.
//!
//! # Deleting a checkpoint, and removing one that is open
//!
//! **Deleting a checkpoint is removing its directory.** Nothing in the
//! live database knows of it. A caller that removes the directory itself
//! does as well as [`delete`] (P-18), which is the careful way:
//!
//! - It removes nothing of a path that is no directory of its own, of a
//!   directory that is no database of this engine, or of one that a handle
//!   has open. A database is known by a manifest of this engine in a file
//!   of the directory's own, under the manifest's name or the renamed
//!   one's, and not by a name. What the other of the two names holds then
//!   goes with the rest. An empty directory, and
//!   the empty lock file of one that holds nothing else, it removes
//!   whatever they were: nothing is lost with them, and a deletion that
//!   was cut short can leave both.
//! - **It makes the directory stop being a database before it removes a
//!   file.** The manifest is renamed to [`DELETING_FILE_NAME`], nothing
//!   is left under the manifest's own name, durably, and then everything
//!   goes, the renamed manifest last. A deletion that
//!   is cut short therefore never leaves a directory that opens with
//!   something missing: not a file of the store, which an open would miss,
//!   and not a file that the engine or Platform keeps beside them (P-17),
//!   which it would not. A second call finishes what it leaves.
//!
//! Platform removes the directory of a checkpoint while a handle has it
//! open. That handle goes on reading: its pages are in its pool, and the
//! files it has open stay until it closes them. What it writes afterwards
//! goes to files that have no name, or fails where a file would have to be
//! created, since no part of the store creates a directory after its open.
//! So the close of such a handle reports an error and brings nothing back,
//! and the live database, which shared only links with it, is as it was.
//! This holds for as long as nothing is made under the same name while
//! the handle is open: a handle writes by its directory's name, and would
//! write into a directory that was made anew there. The test
//! `a_checkpoint_whose_directory_is_removed_while_it_is_open_closes_cleanly`
//! shows it for the parts of a close that exist here: write-back's last
//! turn and the drop of every store. It does not show that such a close
//! writes nothing, which P-18 asks as well: write-back's last turn can
//! append to a file it has open before it meets the first one it cannot
//! create. Those bytes go to a file without a name and are lost with it.
//! A close that writes nothing into a directory that is gone is the
//! engine's to make (TXN-07).
//!
//! # What other tasks take from here
//!
//! - **The open of the engine** (TXN-07, GROVE-01) opens a checkpoint as
//!   it opens any directory, and is told nothing by the directory.
//!   - **Where it makes a database because it finds none, it must not
//!     make one in a directory that a creation left** (above). That is
//!     any directory that is there, where checkpoints are made, and has
//!     no manifest: one with a directory of the store in it, one with the
//!     lock file alone, and, since a creation can be cut before it has
//!     its lock, one that is empty. The store's own opens refuse the
//!     first kind where a segment, a value-log file or a log file with
//!     data is left there, since no manifest names it. They do not refuse
//!     one that is left with a table checkpoint alone or with empty
//!     directories, and nothing refuses the other two kinds. A rename of a directory in the
//!     file layer would let a creation build under another name and
//!     appear whole, and is the owner's to decide on.
//!   - **It makes no database in a directory that holds
//!     [`DELETING_FILE_NAME`]**: that is a checkpoint, or a database,
//!     whose deletion was cut short, and [`delete`] finishes it.
//!   - If it keeps a file beside the store's, whoever makes checkpoints
//!     names it ([`Checkpointer::carrying`]).
//!   - Its close of a database whose directory is gone ends with the
//!     errors of write-back and no more: it creates no directory, and it
//!     is the one to see that nothing is written then (P-18). It shares
//!     the name of the lock file with [`delete`], which is `LOCK`.
//! - **GroveDB's engine** (GROVE-01, GROVE-08) makes a [`Checkpointer`]
//!   from the committer of the open database for `create_checkpoint`,
//!   which refuses an existing path as the trait asks. Its
//!   `open_checkpoint` never makes a database. GroveDB's own
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
pub use files::{delete, DELETING_FILE_NAME};

#[cfg(all(test, not(loom)))]
mod tests;
