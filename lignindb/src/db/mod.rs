//! The engine facade: one open database ([`Db`]).
//!
//! A [`Db`] is a database directory, opened: the store that
//! [recovery](lignin_store::recovery) brought to one committed version, its
//! write-back, its cleaner and its reclaimers running on three threads,
//! and the versions its readers pin. Everything a caller does to a
//! database goes through it:
//!
//! | Call | What it does | Rule |
//! | --- | --- | --- |
//! | [`Db::open`] | Opens the database in a directory, and makes it if the directory has none ([`Options`]). It takes the directory's lock, recovers, makes turns of write-back for what the recovery replayed, and starts the background work. | P-01 to P-04, DU2 to DU4 |
//! | [`Db::close`], or dropping the handle | Ends the session: the background work stops, what can be written back is, a table checkpoint is taken, the log is made durable, and the lock is released. | P-04, P-35 |
//! | [`Db::begin`] | Begins a write transaction ([`WriteTxn`]). | WT1 |
//! | [`Db::commit`], [`Db::rollback`] | Commits a write transaction as the next version, or gives up what it holds. | CM1 to CM9, WT9 |
//! | [`Db::write`] | A write with no transaction: one batch, committed at once. | RD7 |
//! | [`Db::snapshot`] | A snapshot of the latest version, read-only ([`Snapshot`]). | RD1, RD2 |
//! | [`Db::read`] | One operation that pins no version and reads the latest one ([`Operation`]). | RD4 |
//! | [`Db::flush`] | Makes every commit that was published when it was called durable. | DU1a, P-05 |
//! | [`Db::wipe`], [`Db::wipe_entering`] | Removes every record of every keyspace, in one commit, in which the second enters the subtrees its caller names. | CV3, CF5, P-23 |
//! | [`Db::checkpoint`] | Makes a checkpoint of the database in a new directory, which is opened as a database of its own ([`crate::checkpoint`]). | CK1 to CK10 |
//! | [`Db::stats`] | Every count the engine keeps ([`DbStats`]). | |
//! | [`Db::keyspaces`] | The flat keyspace of each column. | |
//!
//! The rules are those of `docs/lignindb/transactions.md`. The modules
//! this one is made of say how each part keeps them: [`crate::txn`] for
//! the write transaction, [`crate::commit`] for the commit and its
//! outcomes, [`crate::snapshot`] for readers, [`crate::scope`],
//! [`crate::savepoint`] and [`crate::batch`] for what is built on a
//! transaction.
//!
//! ```
//! use lignindb::{
//!     commit::{Committed, NoStaleHashes},
//!     txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite},
//!     Db, Options,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let db = Db::open(dir.path(), &Options::default())?;
//!
//! // A write transaction enters a subtree and puts a record into its aux
//! // column. Nobody else sees either before the commit.
//! let subtree = Subtree::new([7; 32]);
//! let txn = db.begin();
//! let scope = txn.scope()?;
//! scope.apply(&Batch {
//!     subtrees: &[SubtreeWrite::Enter {
//!         subtree,
//!         tree_type: b"tree",
//!         host: None,
//!     }],
//!     records: &[RecordWrite::put(Column::Aux, subtree, b"key", b"value")],
//!     ..Batch::default()
//! })?;
//! scope.merge();
//! let before = db.snapshot();
//! let Committed::Published(published) = db.commit(&txn, &mut NoStaleHashes)? else {
//!     unreachable!("the transaction wrote");
//! };
//! assert_eq!(db.version(), published.version);
//!
//! // A snapshot reads the version it was taken at, for as long as it is
//! // held.
//! let mut value = Vec::new();
//! assert!(before.operation().open(subtree)?.is_none());
//! let after = db.snapshot();
//! let operation = after.operation();
//! let opened = operation.open(subtree)?.expect("the subtree is entered");
//! assert!(opened.get(Column::Aux, b"key", &mut value)?);
//! assert_eq!(value, b"value");
//! drop(opened);
//! drop(operation);
//! drop((before, after));
//!
//! // Durable from here: a crash after `flush` returned recovers the
//! // commit.
//! let flushed = db.flush()?;
//! assert_eq!(flushed.version, published.version);
//!
//! // A transaction borrows the database, so it ends before the session.
//! drop(txn);
//! db.close()?;
//!
//! // The next session opens the same state, and a wipe empties it.
//! let db = Db::open(dir.path(), &Options::default())?;
//! assert_eq!(db.version(), published.version);
//! assert!(db.snapshot().operation().open(subtree)?.is_some());
//! db.wipe()?;
//! assert!(db.snapshot().operation().open(subtree)?.is_none());
//! # Ok(())
//! # }
//! ```
//!
//! # The directory
//!
//! ```text
//! <db>/
//!   LOCK       locked while a handle has the database open
//!   MANIFEST   the live files, the table checkpoint, the format version,
//!              the page size
//!   wal/       the log
//!   seg/       segments
//!   val/       the value log
//!   table/     page-table checkpoints
//!   RESERVE    room that is given back when the device is full, in a
//!              session that keeps a reserve; RESERVE.new while it is
//!              written
//! ```
//!
//! **The engine touches these and nothing else** (P-03). Whatever else a
//! directory holds is not the engine's: an open, a recovery, a wipe and a
//! close see the names of the other entries when they list the directory,
//! and open, write, remove and rename none of them. A node keeps
//! `checkpoints/`, marker files and its own state beside the database, and
//! they are as they were after each of those.
//!
//! **One handle at a time** (P-04). The lock is taken before anything of
//! a database is written, and released when the handle is dropped. Only a
//! directory that is not there is made before it, and the lock file
//! itself, so the same path can
//! be opened again in the same process once the handle is gone. A second
//! open while one is alive fails with [`OpenError::Locked`].
//!
//! **A directory that is not this engine's is refused, and left byte for
//! byte as it was** (P-02). An open looks before it writes:
//!
//! | The directory | The open |
//! | --- | --- |
//! | is given by a path whose last name ends in `.creating`, in any case of its ASCII letters, whatever is there | [`OpenError::Creating`] |
//! | is not there, or holds nothing of any engine | makes the database, or fails with [`OpenError::NoDatabase`] if [`Options::create_if_missing`] is off |
//! | has an entry named `MANIFEST.deleting`, whatever else it holds | [`OpenError::Deleting`] |
//! | has an entry that RocksDB makes (`CURRENT`, `IDENTITY`, `MANIFEST-…`, `OPTIONS-…`, an `.sst` file), or a `MANIFEST` that is no LigninDB file | [`OpenError::OtherEngine`] |
//! | has a manifest of a format version this build does not read | [`OpenError::FormatVersion`] |
//! | has the engine's directories and no manifest | [`OpenError::NoManifest`] |
//! | lists `seg`, `wal`, `val` or `table` as anything but a directory of its own, such as a symbolic link, which is not followed | [`OpenError::StoreDirNotADirectory`] |
//! | lists `LOCK`, `MANIFEST`, `RESERVE` or `RESERVE.new`, or a file its manifest names, as anything but a regular file, such as a named pipe or a symbolic link | [`OpenError::NotARegularFile`] |
//! | has pages of another size than [`Options::page_size`] names | [`OpenError::PageSize`] |
//! | holds a database of this engine and no `LOCK` | [`OpenError::NoLockFile`] |
//! | held a database at one look of the open and none at a later one; or had a lock file that the open took, and has another, or none, after the look under it | [`OpenError::Replaced`] |
//!
//! For each refusal in the table, the open makes no file
//! ([`OpenError::left_untouched`]): it checks the supplied name, reads
//! before taking the lock, or checks under a lock on an existing lock
//! file. A `MANIFEST` that is too short to have a header is reported as
//! a manifest that cannot be read ([`OpenError::Manifest`]), and nothing
//! is written for it either.
//!
//! **Whom the guarantee is for** (D-129). The byte-for-byte guarantee is
//! for a directory that nobody else changes while it is being opened, and
//! for one that is put in the place of a database during the open: the
//! lock of a database is taken through the lock file that is there, a
//! lock file is made in no directory in which the open found a database,
//! no database is made once a look of the open found one, and the open
//! writes only after it found, under the lock, that the lock file at the
//! path is the one it holds. A directory
//! with no lock file into which another process writes during the open is
//! refused as [`OpenError::Changed`] and may have gained an empty `LOCK`.
//! No protocol closes that window against a process that does not take
//! the lock, because the lock is the only thing two openers share and it
//! has to be made before it can be held. The file is left where it is
//! because another opener may hold it. (`Changed` is what such a
//! directory is refused as, if under the lock it is one that is refused.
//! Two opens of an empty directory are not refused for each other: the
//! second is locked out, or opens the database that the first made. And
//! a database of this engine that someone puts into the directory in
//! that window, without its lock file, is opened under the lock file
//! that this open made: the open cannot tell it from one that another
//! open made there. A directory whose lock file this open asked to be
//! made, and whose lock file is another, or none, after the look under
//! it, is refused as `Changed` as well.)
//!
//! **Where the guarantee ends.** The file layer names files by path. The
//! last thing an open does before its first write is to ask, under the
//! lock and after its look, whether the lock file at the path is the one
//! it holds. A directory that is put at the path after that answer is
//! written into as the database would have been: by the recovery of this
//! open, and by the session after it. So nobody puts another directory at
//! the path of a database that is being opened or is open, as with the
//! close below.
//!
//! **Where a database is made** ([`Options::create_if_missing`]): at a
//! path whose last name does not end in `.creating` (below), in a
//! directory that is not there, and in one that holds none of these: an
//! entry named `MANIFEST.deleting`, an entry that RocksDB makes, a
//! `MANIFEST`, an entry with the name of a directory of a store's files
//! (`seg`, `val`, `wal`, `table`), and an entry named `LOCK`, `RESERVE`
//! or `RESERVE.new` that is anything but a regular file (the table
//! above). So an empty directory is such a place, one that holds only
//! entries that are not the engine's is, and so is what an open leaves
//! that failed before it made its first manifest: a `LOCK` that is a
//! regular file, a `MANIFEST.tmp`, or both.
//!
//! A directory that holds the mark of a removal gets no database while
//! the mark is there ([`OpenError::Deleting`] says what
//! `lignindb::checkpoint::delete` does with it). Once the mark is gone
//! the directory is looked at as any other, by the table above: what a
//! removal leaves that was cut short after its mark went and before the
//! directory itself did is a place.
//!
//! A path whose last name ends in `.creating`, in any case of its ASCII
//! letters, is not opened and gets no database
//! ([`OpenError::Creating`]). A checkpoint is built beside its target by
//! appending this suffix to the target's name (`lignindb::checkpoint`,
//! D-128 B): a directory that a creation built in can hold nothing, a
//! lock file alone, a part of a checkpoint or a whole one, and the first
//! two the open could not tell from a place. The open goes by the name
//! it is given, before it makes a call to the file layer, so this row of
//! the table comes before every other. What a creation leaves at its
//! target, and what becomes of the directory it built in, is said there
//! ("Durability, and a creation that fails").
//!
//! # A new database
//!
//! A new database is its first manifest, recovered like any other
//! (`lignin_store::recovery`), and one commit that the open makes: the
//! catalog, the flat keyspace of each column (data, aux, roots, meta), the
//! subtree directory and the keyspace of the pages that drops unlink, all
//! empty. That commit has version 1, so the first commit of a caller is
//! version 2. A crash before it is in the log leaves a store with no
//! commit, and the next open makes it again.
//!
//! **A power cut during a creation leaves a directory that the next open
//! takes, or makes a database in anew.** The lock file's name is synced
//! with the directory before the first manifest is installed, so a cut
//! leaves no manifest without its lock file
//! ([`OpenError::NoLockFile`] is for a database whose lock file someone
//! left out or removed, and no crash makes one).
//!
//! **The catalog says where the keyspaces are.** It is a flat keyspace in
//! the first page a store makes, page 1, with one record for each of the
//! six: the id of its root page, which is the same for as long as the
//! database lives. An open reads it at the version it landed on.
//!
//! # A session that takes no write any more
//!
//! Four outcomes of a commit end the session for writes (D-098): a commit
//! that is published and whose sync failed
//! ([`CommitError::NotDurable`]), a commit that could not be installed
//! ([`CommitError::Halted`]), one of which the log cannot say whether it
//! holds it ([`CommitError::Unsettled`]), and a commit that found the
//! session stopped ([`CommitError::Stopped`]). A [`flush`](Db::flush)
//! whose sync failed is a fifth. After the first of them every
//! [`commit`](Db::commit), [`write`](Db::write) and [`wipe`](Db::wipe) is
//! refused with [`WriteError::Reopen`], with the transaction as it was,
//! and [`Db::reopen`] says why. The log makes nothing durable after a
//! failed sync, and what it holds after the other three is settled only by
//! a recovery, so the caller closes the database and opens it again. Reads
//! go on until then, and what was published stays readable.
//!
//! **The store ends a session as well, whichever step met the failure**
//! (D-151, D-157). Two things stay failed in a store until it is opened
//! again, whatever the device does later: a sync of the log that failed
//! ([`Reopen::NotDurable`]) and a sync of a segment that failed
//! ([`Reopen::SegmentsNotDurable`]). The step that meets one need not be a
//! commit: a turn of write-back does, a table checkpoint, the creation of
//! a checkpoint of the database. A session in one of these states would
//! take commits that can never become durable, keep a log that only
//! grows, or refuse every commit for a failure that is over. A log that
//! has used the last of its file ids ends the session in the same way
//! ([`Reopen::LogFileIdsUsedUp`], DF12).
//!
//! So every commit looks at the store before it begins and after it has
//! ended, and so does [`Db::reopen`]: the session has ended from the first
//! look that finds one of the two and records it, under every sync policy.
//! The look reads what the store holds and waits for nothing. What it
//! finds is recorded under the commit lock. Before a commit that lock is
//! tried, and a look that comes while another commit is being made records
//! nothing. After a commit it is waited for. So a thread that writes
//! commit after commit makes at most one more after the store failed, and
//! a commit that was waiting for the lock when a failed sync of a segment
//! was recorded is still taken.
//!
//! **A session gives one reason, the first.** The commit path can stop a
//! session by itself while the store's failed sync is being recorded, and
//! both are true then. Whichever is seen first where the session is
//! answered for is kept, once: [`Db::reopen`], [`DbStats::reopen`] and
//! every commit that is refused with [`WriteError::Reopen`] give that one
//! from then on. (A commit whose own sync failed, or whose own group the
//! log could not settle, says that in its own error:
//! [`CommitError::NotDurable`], [`CommitError::Unsettled`].)
//!
//! **A session ends for what stays failed, and for nothing else.** A
//! replacement of the manifest that failed in its rename, or in the sync
//! of the directory after it, leaves the manifest in doubt: which of two a
//! crash would leave is not known. That does not last. Write-back opens
//! the manifest again where it next needs it, and so do a commit before it
//! begins, a close, and the creation of a checkpoint of the database
//! before it makes a directory, by the same call
//! (`WriteBack::settle_manifest`). Where the manifest cannot be opened
//! again now, the commit that found it in doubt is refused with that error
//! and its transaction whole ([`CommitError::NotReady`]), a close answers
//! it, and the creation fails with it and can be made again.
//!
//! A commit looks at the manifest without waiting for it. While
//! write-back holds the manifest to replace it, or another thread holds it
//! to settle it, a commit that comes finds nothing in doubt and is made,
//! as it is beside any turn of write-back: the commit itself writes the
//! log, and no manifest. Only a commit that found the manifest in doubt
//! waits for the manifest's lock, to have it settled, behind whoever holds
//! it then. [`Db::reopen`] and [`Db::stats`] settle nothing: they are
//! looks, and can be made from anywhere.
//!
//! So a device that was full has one of two outcomes for a session, and
//! the session says which. A refusal that involved no sync leaves the
//! session as it was: once there is room, the next commit is taken. A sync
//! of the log or of a segment that was refused ends it: [`Db::reopen`]
//! says so, and a close and an open resume with everything that was
//! promised (`docs/lignindb/disk-full-and-backup.md`).
//!
//! # Background work
//!
//! Three threads belong to an open database, and all end with its handle.
//!
//! - **Write-back** ([`lignin_store::flush`]): page versions go from the
//!   buffer pool to segments, values that commits carried in their groups
//!   get their place in the value log, table checkpoints are taken, the
//!   log's files are kept going, and the log is synced where the sync
//!   policy is an interval. A committer is slowed while it is behind
//!   (FL5). The values of a tree page are regrouped before the page is
//!   written as a base image (`lignin_tree::value::Regrouping`).
//! - **Reclaiming**: the page id allocator, the buffer pool and the kept
//!   handles on the subtree directory give up what no reader can reach any
//!   more, when a reader that held it back has gone (`page-alloc.md` N8);
//!   and what dropped and cleared subtrees left is taken out of the pages
//!   (`lignin_tree::drop`, CL6). A commit hands it what it dropped
//!   ([`Published::dropped`]), an open what earlier sessions left, and it
//!   reads the records of the lazy range deletes itself. It begins each
//!   once no reader from before the drop is left, takes it out in page
//!   steps whose records go to the log through write-back, and tries
//!   again later what a write transaction's copy of a page holds back.
//!   When a reclaiming is done, the handle on the directory of the latest
//!   version is loaded again, under the commit lock, so that the id of the
//!   Merk leaves the handles.
//! - **The cleaner** ([`lignin_store::cleaner`]): live blocks are moved
//!   out of segments that are mostly dead, and live values out of such
//!   files of the value log, and the files are given back, by the policy
//!   in [`Options::cleaner`]. The pointers to moved values are rewritten
//!   in page steps of the tree layer's owners of values. A close ends its
//!   loop and has it close the files that took its copies, before
//!   write-back's last checkpoint names them.
//!
//! **Until a session is asked for its first commit, two of the three
//! make no turn, and the open has made the first turns of the third.** A
//! caller may remove the directory of a database under its handle (P-18),
//! and a file that a thread makes there meanwhile can make that removal
//! fail. So:
//!
//! - The open makes turns of write-back on its own thread before it
//!   starts the three ([`Db::open`]), for what its recovery replayed.
//!   What those turns did not write, because one of them failed or
//!   because they were as many as an open makes, is left to write-back's
//!   thread.
//! - The reclaiming thread and the cleaner's wait for the first commit
//!   that a caller asks of this handle, by [`Db::commit`], [`Db::write`]
//!   or a wipe, whatever that call is answered: a commit that is refused
//!   for room has asked too, and the cleaner is what gives room back.
//!   What earlier sessions left to reclaim and to clean is space and no
//!   state. It waits with the two threads, for as long as the session
//!   lasts if no commit is asked of it, unless it is asked for:
//!   [`Db::reclaim`] makes a round of the reclaiming and [`Db::clean`] a
//!   turn of the cleaner, each on the calling thread and in any session.
//! - **A release of the reserve does not end that wait.** A session that
//!   keeps a reserve gives it back where it is given an error for room,
//!   and tells the three loops (DF13). Where no commit was asked of it,
//!   by a flush that is refused, say, or the creation of a checkpoint,
//!   the two threads go on waiting: no pass of the cleaner and no round
//!   of the reclaimer follows the release. The fill of a reserve that
//!   was released is made by the cleaner's thread at the end of a turn
//!   and waits for a version that is published after the refusal, so
//!   none is made before a commit is asked. Such a session can therefore
//!   remove the two files of its reserve from its directory, and makes
//!   neither once its open has returned.
//!
//! An error of any of them changes nothing that is committed. It is counted
//! and kept as text ([`DbStats::background`]). After a turn that failed,
//! each of the three waits before its next turn, whoever wakes it, unless
//! the session releases its reserve meanwhile (`disk-full-and-backup.md`,
//! DF12).
//!
//! **Closing waits for a reclaiming that holds cells.** From the moment
//! the record of a dropped Merk is out of the directory, the cells that
//! are left of it are known to the session and to nothing on disk
//! (`lignin_tree::drop`, "Reclaiming"). A close goes on with each such
//! reclaiming until it is done or does not move: a page that the latest
//! commit wrote or that a write transaction has copied holds it back, or
//! a step failed. What a close leaves of it then, and what a process
//! that is killed leaves, is space and no state.
//!
//! **A close writes only into a directory that is this database's
//! still** (P-18). Platform removes the directory of a checkpoint while
//! its handle is open. A close ends the three threads first and then looks:
//! the lock file in the directory has to be the file this handle's guard
//! is on (`VfsLock::is_at`), and the manifest has to be the one this
//! session wrote last. Where one of the two fails, or cannot be found
//! out, the close does nothing more:
//! no reclaiming is finished, no checkpoint is taken, no file is written
//! and nothing is made at the path ([`DbError::Removed`], which a drop of
//! the handle keeps to itself). So a directory that was removed, and one
//! that holds another database by then, are left as they are: also a copy
//! of this one, whoever holds its lock.
//!
//! A session whose own replacement of the manifest did not finish cannot
//! say which of two manifests it wrote last. Its close opens the manifest
//! again first, as write-back would, in a directory that has the handle's
//! lock file still, and then looks as any close does. Where the manifest
//! cannot be opened again, the close does nothing more, and answers that
//! error and not that the directory was removed ([`DbError::Flush`]).
//! Such a close has the lock file alone to go by before it opens the
//! manifest: it takes the manifest it finds, as write-back does during
//! the session. So a directory whose files were written over in place
//! while the handle was open and its manifest in doubt is not told apart.
//!
//! This is about the close, and not about a session that goes on. Until
//! the handle is closed its commits and its background work write as
//! before: to the files they have open, which nobody sees any more once
//! the directory is removed, and to the path, where a manifest that is
//! installed or a file that is begun fails with the directory gone and
//! lands in a directory that was put in its place. So whoever puts
//! another directory at the path closes the handle first. A checkpoint
//! that is deleted while its handle is open is TXN-06's to test in full
//! (`platform-requirements.md`, item 3).
//!
//! # The reserve
//!
//! A session that is opened with [`Options::reserve`] above 0 keeps a
//! reserve of that many bytes in its directory ([`crate::space`]), and
//! does with it what `disk-full-and-backup.md` says in DF13:
//!
//! - **The open fills it**, as its last step before the background work
//!   is started, where the directory has none of that length. An open
//!   that cannot fill it is made all the same.
//! - **The session releases it when a full device refuses it something**:
//!   a commit, a write, a wipe, a flush, the creation of a checkpoint or
//!   a close that returns an error which a full device caused
//!   ([`crate::space::out_of_space`]), and a turn of write-back, a pass of
//!   the cleaner or a call of the reclaimer that failed with such an
//!   error, in their threads or on the thread of a caller. The thread
//!   that took a file of the reserve off the device has each of the
//!   three loops take its next turn at once. Once both files of the
//!   reserve are away with no removal that failed, and until the next
//!   fill begins, a refusal makes no operation on a file for the
//!   reserve.
//! - **The session fills it again from the cleaner's thread**, at the end
//!   of a turn of its loop in which the cleaner was at rest, once both
//!   of its marks have moved on since it was last told of a refusal for
//!   room: the latest version that it published, and its count of the
//!   table checkpoints that it installed (DF13 says when that count
//!   moves). A fill that fails is tried again by the same rule, with the
//!   marks read after the fill, and the next refusal for room looks for
//!   what the fill left on the device.
//!
//! [`DbStats::reserve`] says what the session holds and what it did. An
//! open that is refused for room does not release a reserve: whoever
//! opens the database removes the file `RESERVE` then
//! (`disk-full-and-backup.md`, section 7.1).
//!
//! # Wipe
//!
//! [`Db::wipe`] is one commit (CV3): a write transaction that drops every
//! subtree that no other hosts, with a key set that covers every key
//! (CF5). In every column the records of a subtree are those of its entry
//! in the directory (`tree-layout.md` D6), so with the entries gone no
//! record is left to read: no node, and no record of the data, roots, aux
//! and meta columns.
//!
//! **No commit comes between the wipe's look at the database and its
//! publication.** The wipe holds the commit lock from the walk that finds
//! the subtrees to drop until its version is published
//! ([`Committer::commit_alone`](crate::commit::Committer)), so every
//! other commit has a version before the wipe's, and is wiped, or after
//! it. At the wipe's own version no subtree is entered, but those that
//! [`Db::wipe_entering`] was given: they are entered by the same write as
//! the drops, new and with nothing in them, so a caller that has subtrees
//! in every database, as GroveDB has the root of its grove, is never
//! left with a database that is wiped and does not have them. A snapshot from
//! before the wipe keeps reading its version, a crash leaves the database
//! as it was or empty, and an open write transaction that wrote a record
//! before the wipe fails its commit with a conflict. Entering a subtree
//! writes no record: a transaction that did nothing else has no key for
//! the wipe to meet, and its commit after the wipe is no conflict. Its
//! enter is judged where it is laid out again over the wipe, as over any
//! commit: by the directory the wipe left, with what the transaction's
//! own writes before the enter made of it. It is refused where the
//! subtree its host is in is not entered then. And it is refused where
//! the subtree itself is entered then, as one that the wipe entered is:
//! an enter of a subtree that is new, and an enter if it is absent that
//! states another tree type or another host than the subtree has.
//!
//! **The wipe ends as a commit does**, and an error it returns does not
//! always say that nothing was wiped ([`Db::wipe_entering`]):
//! [`CommitError::NotDurable`] is of a wipe that is published and whose
//! sync failed. Every read of the latest version then finds the database
//! wiped, and a snapshot from before keeps reading its version.
//!
//! What the wipe removed is storage until the reclaimer has taken it out,
//! like what any drop leaves. No file is deleted by a wipe, and no entry
//! of the directory that is not the engine's is touched.
//!
//! # The check at a close
//!
//! **In a debug build a close checks the store with the physical
//! invariant checker** ([`crate::verify`], D-126): at its latest version,
//! with what a store keeps beside its pages
//! ([`Scope::Still`](verify::Scope::Still)), after the background work
//! has ended and what could be written back was. A finding is a panic
//! with the report's text, so a test that closes a database, or drops its
//! handle, ends with the check. A release build makes no check at a
//! close. What the checker counts and does not call a finding is no
//! panic: cells that nothing reaches, and a value hash that the caller's
//! rules name as another writer's
//! ([`OtherHashes`](verify::OtherHashes)).
//!
//! The check is left out where there is nothing to say or nobody to say
//! it to: on a thread that is panicking already; for a session that ended
//! for writes ([`Db::reopen`]), whose store the next recovery makes
//! whole; for a close that did not do all of its work (a directory that
//! was removed, a failed checkpoint); and for a handle that was taken out
//! ([`Db::unchecked_at_close`]). No stored hash is recomputed unless the
//! caller gave the rules that make them ([`Db::check_hashes_by`]): the
//! engine knows no Merk rule. The data column of a subtree is held to
//! the tree type of its entry only where the caller said which tree
//! types keep flat records ([`Db::check_kinds_by`]), for the same reason.
//!
//! In any build, [`Db::verify`] checks the latest version as a reader
//! sees it, beside writers, at a version it pins under the commit lock
//! with the reclaiming held back for the run, and
//! [`Db::close_and_verify`] closes and returns the report of
//! the full check with no panic: a tool that checks a database calls
//! these.
//!
//! # Choices made here that the plan left open
//!
//! 1. **The roots of the keyspaces are in a catalog in page 1**, written
//!    by the first commit. The manifest names files and no page, and a
//!    table checkpoint is the store's.
//! 2. **The layout of tree pages and the inline threshold are the
//!    defaults** ([`Layout::proposed`], [`InlinePolicy::DEFAULT`]) and are
//!    not in [`Options`]: a database that was written with one cannot be
//!    read with another, and nothing records which it was written with.
//!    They become options when they are recorded (PERF-08).
//! 3. **A wipe finds the subtrees to drop by a walk of the directory's
//!    entries**, which reads every entry, under the commit lock. A wipe of
//!    a database in which no subtree is entered is a commit all the same,
//!    with a version and the key set of a wipe (CV3): where its caller
//!    names no subtree to enter, it enters one and drops it
//!    ([`Db::wipe`]).
//! 4. **A wipe is made under one hold of the commit lock**, and not as a
//!    transaction that is made again when a commit lands beside it: a
//!    commit that entered a subtree between the walk and the wipe's commit
//!    would meet none of the wipe's writes, and its records would be in
//!    the wipe's own version. Commits wait for as long as the walk and the
//!    drops take.
//! 5. **Every error of `flush` ends the session for writes**, also one
//!    that is not the sync's own: the caller asked for its commits to be
//!    durable and was not given that.
//! 6. **The handles on the directory are behind a read-write lock**
//!    ([`Directories`](crate::snapshot::Directories)). A reader takes it
//!    once for each version it reads at, when it first opens a subtree,
//!    and a snapshot that opens none takes it never.
//!
//! [`Layout::proposed`]: lignin_tree::fragment::Layout::proposed
//! [`InlinePolicy::DEFAULT`]: lignin_tree::value::InlinePolicy::DEFAULT
//! [`Published::dropped`]: crate::commit::Published::dropped
//! [`CommitError::NotDurable`]: crate::commit::CommitError::NotDurable
//! [`CommitError::Halted`]: crate::commit::CommitError::Halted
//! [`CommitError::Unsettled`]: crate::commit::CommitError::Unsettled
//! [`CommitError::Stopped`]: crate::commit::CommitError::Stopped

mod error;
mod open;
mod reclaim;
mod reserve;
mod store;

#[cfg(all(test, not(loom)))]
mod tests;

use std::{
    ffi::OsString,
    fmt,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use lignin_format::{tree_page::Hash, CommitVersion, PageSize};
use lignin_io::VfsLock;
use lignin_store::{
    alloc::Census,
    cleaner::{BothOwners, Cleaner, CleanerError, CleanerStats, Pass, ValuePass},
    flush::{FlushError, Stores, WriteBack},
    recovery::RecoveryReport,
    stats::StoreStats,
    table::LogDurable,
};
use lignin_tree::{
    flat::{FlatOwners, FlatTree},
    hash::{HashRules, Refusal},
    node::NodeRef,
    value::{RegroupPolicy, Regrouping, TreeOwners},
};

pub use self::{
    error::{DbError, OpenError, Reopen, WriteError},
    reclaim::{ReclaimStats, Round},
    reserve::ReserveStats,
};
use self::{
    reserve::CleanerTurns,
    store::{lock, Shared},
};
use crate::{
    checkpoint::{CheckpointError, CheckpointOptions, Checkpointer, Created},
    commit::{CommitError, CommitStats, Committed, Committer, NoStaleHashes, Published},
    options::Options,
    snapshot::{LatestLease, Operation, Snapshot},
    txn::{Batch, Column, FlatKinds, KeySet, Keyspaces, Subtree, SubtreeWrite, TxnError, WriteTxn},
    verify,
};

/// What a [`Db::flush`] made durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flushed {
    /// The target of the flush (DU1a): the latest version when the flush
    /// was called. A crash at any later time recovers this version or a
    /// later one.
    pub version: CommitVersion,
    /// How far the log is durable, which can be further.
    pub durable: LogDurable,
}

/// What the background work of a session could not do
/// ([`DbStats::background`]). None of it changes what is committed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Background {
    /// Turns of write-back that failed: in its loop, in the turns that
    /// committers made or waited for, and among those that the open made.
    pub write_back_errors: u64,
    /// Calls of the reclaimer that failed.
    pub reclaim_errors: u64,
    /// Passes of the cleaner that failed. Its loop goes on after one.
    pub cleaner_errors: u64,
    /// The last of these, as text.
    pub last_error: Option<String>,
}

/// What the open of a database took, by its parts ([`DbStats::opening`]).
///
/// The recovery is the store's (`lignin_store::recovery`): the manifest,
/// the table checkpoint, the log from the checkpoint on, and the syncs
/// that make the first log file of the session, which are about a dozen
/// whatever the size of the database and are most of the time on a disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Opening {
    /// Looking at the directory, taking its lock and reading its
    /// manifest, or making the directory and the first manifest.
    pub directory: Duration,
    /// The recovery of the store.
    pub recovery: Duration,
    /// Reading the catalog and loading the handle on the subtree
    /// directory, or the first commit of a new database, and reading what
    /// earlier drops left.
    pub keyspaces: Duration,
    /// The turns of write-back that the open makes for what the recovery
    /// replayed and left owed.
    pub write_back: Duration,
    /// Filling the reserve, in a session that keeps one: the writing of
    /// its bytes where the directory had none of its length, and the sync
    /// of its name.
    pub reserve: Duration,
    /// Starting the threads of the background work.
    pub threads: Duration,
}

impl Opening {
    /// The whole open.
    pub fn total(&self) -> Duration {
        self.directory
            + self.recovery
            + self.keyspaces
            + self.write_back
            + self.reserve
            + self.threads
    }
}

impl fmt::Display for Opening {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |part: Duration| part.as_secs_f64() * 1e3;
        write!(
            f,
            "opened in {:.2} ms: the directory {:.2}, the recovery {:.2}, the keyspaces {:.2}, \
             write-back {:.2}, the reserve {:.2}, the threads {:.2}",
            ms(self.total()),
            ms(self.directory),
            ms(self.recovery),
            ms(self.keyspaces),
            ms(self.write_back),
            ms(self.reserve),
            ms(self.threads)
        )
    }
}

/// The memory of the buffer pool ([`DbStats::memory`]), in bytes.
///
/// **What the process holds is not the pool's size, and grows to it.** A
/// pool of a megabyte of frames or more is a mapping of its own on Linux
/// and macOS ([`Self::mapped`]): the system gives it zeroed and backs a
/// frame with memory when the frame is first used. So [`Self::fixed`]
/// counts the whole of the frames from the open on, the resident size of
/// the process after an open is far below it, and it grows towards it as
/// pages are read. The allocator's own statistics do not see a mapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory {
    /// The size the pool was given ([`Options::cache_size`]).
    pub capacity: usize,
    /// What the pool asked the allocator and the system for when it was
    /// made, a mapping included whole (`BufferPool::fixed_memory`).
    pub fixed: usize,
    /// The part of [`Self::fixed`] that is a mapping of its own, backed as
    /// frames are first used: all of the frames, or none
    /// (`BufferPool::mapped_memory`).
    pub mapped: usize,
    /// What the pool holds now: the fixed part, the frames it could not
    /// evict beside it and the values attached to frames
    /// (`BufferPool::memory`).
    pub held: usize,
}

impl fmt::Display for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pool memory: {} bytes asked for ({} of them a mapping, backed as frames are first \
             used), {} held now, of {} configured",
            self.fixed, self.mapped, self.held, self.capacity
        )
    }
}

/// Every count of an open database ([`Db::stats`]).
#[derive(Debug, Clone)]
pub struct DbStats {
    /// The latest published version.
    pub version: CommitVersion,
    /// The counts of the store (`lignin_store::stats`): the rows of the
    /// plan's prediction table that count what the engine did, and what an
    /// operator watches.
    pub store: StoreStats,
    /// What the commits of the session did.
    pub commits: CommitStats,
    /// What the recovery that opened the session did.
    pub recovery: RecoveryReport,
    /// How long the open took, by its parts.
    pub opening: Opening,
    /// What the reclaiming of dropped subtrees has done, and what waits.
    pub reclaimed: ReclaimStats,
    /// What the cleaner has moved and given back in this session
    /// (`lignin_store::cleaner`).
    pub cleaner: CleanerStats,
    /// Page ids that reclaims gave back to the allocator.
    pub ids_released: u64,
    /// How many page ids are in each state, and the highest one that has
    /// been in use: every page of the latest version has an id at or
    /// below it.
    pub ids: Census,
    /// The memory of the buffer pool.
    pub memory: Memory,
    /// Version pins and operation guards that are held.
    pub pins: usize,
    /// See [`Self::pins`].
    pub guards: usize,
    /// Handles on the subtree directory that are kept, one for each
    /// version a reader can still read at.
    pub directories: usize,
    /// What the background work could not do.
    pub background: Background,
    /// What the session holds of its reserve, and what it did with it.
    pub reserve: ReserveStats,
    /// Why the session takes no write any more, if it does not.
    pub reopen: Option<Reopen>,
}

impl fmt::Display for DbStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "version {}", self.version)?;
        writeln!(f, "{}", self.opening)?;
        writeln!(f, "{}", self.store)?;
        writeln!(f, "{}", self.memory)?;
        writeln!(
            f,
            "page ids: {} live, {} held by transactions, {} retired, the highest {}",
            self.ids.live,
            self.ids.private,
            self.ids.retired,
            self.ids.high_water.map_or(0, |page| page.get())
        )?;
        writeln!(
            f,
            "commits: {} published, {} conflicts, {} failed, {} rollbacks",
            self.commits.published,
            self.commits.conflicts,
            self.commits.failed,
            self.commits.rollbacks
        )?;
        writeln!(
            f,
            "reclaimed: {} dropped subtrees ({} waiting), {} cells, {} pages freed",
            self.reclaimed.merks,
            self.reclaimed.waiting,
            self.reclaimed.cells,
            self.reclaimed.pages_freed
        )?;
        writeln!(
            f,
            "cleaner: {} blocks moved and {} segments dropped, {} values moved and {} value-log \
             files dropped, {} segments not durable",
            self.cleaner.blocks_moved,
            self.cleaner.segments_dropped,
            self.cleaner.values_moved,
            self.cleaner.value_files_dropped,
            self.cleaner.segments_not_durable
        )?;
        writeln!(
            f,
            "readers: {} pins, {} guards, {} directory handles kept",
            self.pins, self.guards, self.directories
        )?;
        if let Some(error) = &self.background.last_error {
            writeln!(f, "background: {error}")?;
        }
        if self.reserve.len != 0 {
            let reserve = &self.reserve;
            writeln!(
                f,
                "reserve: {} bytes, {}; refused for room {} times, released {} times ({} bytes), \
                 filled {} times, {} fills failed",
                reserve.len,
                match reserve.held {
                    true => "held",
                    false => "not held",
                },
                reserve.refusals,
                reserve.releases,
                reserve.bytes_released,
                reserve.fills,
                reserve.fills_failed
            )?;
            if let Some(error) = &reserve.last_error {
                writeln!(f, "reserve: {error}")?;
            }
        }
        if let Some(reopen) = &self.reopen {
            writeln!(f, "{reopen}")?;
        }
        Ok(())
    }
}

/// The three threads of an open database.
struct Threads {
    write_back: JoinHandle<()>,
    reclaim: JoinHandle<()>,
    cleaner: JoinHandle<()>,
}

/// An open database. See the [module documentation](self).
///
/// It is `Send` and `Sync`: every thread of a process shares one handle,
/// by reference or in an `Arc`. Transactions, snapshots and operations
/// borrow it, so the database stays open while one of them lives.
pub struct Db {
    shared: Arc<Shared>,
    threads: Mutex<Option<Threads>>,
    created: bool,
    opening: Opening,
    /// Whether the check that a close makes in a debug build is left out.
    unchecked: AtomicBool,
    /// The rules that the check of a close recomputes the hashes by, if
    /// the caller gave some.
    hash_rules: Mutex<Option<Box<dyn HashRules + Send>>>,
    /// The tree types that keep flat records, by which a check holds the
    /// data column to the entries, if the caller gave them.
    flat_kinds: Mutex<Option<FlatKinds>>,
    /// The lock of the directory. It is the last field, so that it is
    /// released when everything else of the handle is gone.
    guard: Box<dyn VfsLock>,
}

const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Db>();
};

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db")
            .field("dir", &self.shared.store.dir)
            .field("version", &self.version())
            .field("page_size", &self.page_size())
            .finish_non_exhaustive()
    }
}

/// The hash rules of a commit, with a count of the calls: the hashes the
/// tree layer computes are those a commit evaluates (CM3 step 5).
struct Counted<'r, R> {
    rules: &'r mut R,
    calls: u64,
}

impl<R: HashRules> HashRules for Counted<'_, R> {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.value_hash(node, value, hosted)
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.node_hash(node)
    }
}

impl Db {
    /// Opens the database in the directory `dir`, and makes one there if
    /// the directory has none and [`Options::create_if_missing`] is on.
    /// The directory is created with every directory above it that is
    /// missing. A path whose last name ends in `.creating` is refused
    /// whatever is there, with nothing made ([`OpenError::Creating`]).
    ///
    /// The directory's lock is held from here until the handle is dropped.
    /// The store is recovered to the last commit whose group its log holds
    /// whole (DU2), which is every commit of a session that was closed,
    /// turns of write-back are made on the calling thread, and the
    /// background work is started.
    ///
    /// The turns are for what the recovery replayed and left owed. The
    /// segment, the value-log file and the manifests that take it are
    /// made here, and what the turns did not write of it is left to
    /// write-back's thread. A table checkpoint that is due is taken in
    /// these turns, and where a turn filled the log's file the next of
    /// them, if one is left, turns the log to its next file. What
    /// write-back's thread does by itself afterwards, a table checkpoint
    /// or a turn of the log, it does by its policy
    /// (`lignin_store::flush::FlushConfig`), as in any session, and a
    /// close takes its own checkpoint. The turns are made until the pass
    /// of one moves nothing on, the turn takes the log to no other file
    /// and leaves no turn of the log due, and [`OPEN_TURNS`] at most. A
    /// turn that fails is the last. It does not fail the open: it is
    /// counted in [`Background::write_back_errors`] and kept as the last
    /// error there, as a turn of the loop that failed is, and what it did
    /// not write stays owed to the loop. The store's own count of the
    /// loop's errors does not take it, and the loop's first turn follows
    /// with no wait.
    ///
    /// The turns come before the fill of the reserve
    /// ([`Options::reserve`]), which is the open's last step before the
    /// threads: what the open itself needs of the device comes first
    /// (DF9). A turn that is refused for room is told to the session as a
    /// refusal for room, and finds no reserve that this session has
    /// written to release.
    ///
    /// The cleaner's thread and the reclaimer's are started with
    /// write-back's, and wait for the first commit that is asked of the
    /// session (the module documentation, "Background work").
    ///
    /// A directory that this build does not open is refused before
    /// anything is written to it ([`OpenError::left_untouched`]), and
    /// entries that are not the engine's are never read, removed or
    /// renamed.
    pub fn open(dir: impl AsRef<Path>, options: &Options) -> Result<Self, OpenError> {
        let mut db = Self::open_at_rest(dir.as_ref(), options, OPEN_TURNS)?;
        db.start(options)?;
        Ok(db)
    }

    /// The open up to the start of the background work: the directory's
    /// lock, the recovery, the keyspaces, what earlier sessions dropped
    /// and did not reclaim, at most `turns` turns of write-back for what
    /// the recovery replayed, and the fill of the reserve, in this order.
    /// The handle it returns has none of the three threads. Dropped as it
    /// is, it does nothing of what a close does: the session ends as a
    /// process that is killed ends.
    fn open_at_rest(dir: &Path, options: &Options, turns: usize) -> Result<Self, OpenError> {
        let mut opening = Opening::default();
        let began = Instant::now();
        let opened = open::open_dir(dir, options)?;
        opening.directory = began.elapsed();
        let (shared, directory, recovery) = open::open_store(dir, options, opened.manifest)?;
        // What earlier sessions dropped and did not reclaim.
        shared
            .adopt_drops(&directory)
            .map_err(OpenError::catalog("what earlier drops left"))?;
        opening.recovery = recovery;
        opening.keyspaces = began.elapsed() - opening.directory - recovery;
        let shared = Arc::new(shared);
        let store = &shared.store;
        store.troubles.of(&shared);
        // The turns of write-back before the reserve: what the open
        // itself needs of the device comes first (DF9), and a turn that
        // is refused for room finds no reserve of this session to
        // release.
        let writing = Instant::now();
        turns_at_the_open(&shared, turns);
        opening.write_back = writing.elapsed();
        let filling = Instant::now();
        store.troubles.reserve.fill_at_open(|| shared.marks());
        opening.reserve = filling.elapsed();
        Ok(Self {
            shared,
            threads: Mutex::new(None),
            created: opened.created,
            opening,
            unchecked: AtomicBool::new(false),
            hash_rules: Mutex::new(None),
            flat_kinds: Mutex::new(None),
            guard: opened.lock,
        })
    }

    /// Starts the three threads of the background work, for a handle that
    /// has none ([`Self::open_at_rest`]). Where one of them cannot be
    /// started, those that were are ended again and the handle is left
    /// with none.
    ///
    /// It returns once the loop of write-back has said that it runs. A
    /// committer that finds write-back over a bound and no loop running
    /// makes a turn of write-back itself, syncs and all
    /// ([`WriteBack::wait_for_room`]), and the loop says that it runs at
    /// its first instruction, on its thread: so nobody commits through
    /// this handle before that. The wait ends with the thread, too: a
    /// thread that has ended with no loop running is an error of the
    /// start.
    fn start(&mut self, options: &Options) -> Result<(), OpenError> {
        let started = Instant::now();
        let shared = &self.shared;
        let writer = shared.clone();
        #[cfg(test)]
        let before_the_loop = BEFORE_THE_LOOP.with(|hook| hook.borrow_mut().take());
        let write_back = spawn("lignindb-write-back", move || {
            #[cfg(test)]
            if let Some(before_the_loop) = before_the_loop {
                before_the_loop();
            }
            write_back(&writer);
        })?;
        while !shared.write_back().is_running() {
            if write_back.is_finished() {
                let _ = write_back.join();
                let ended = "the thread of write-back ended before its loop was seen to run";
                return Err(OpenError::Thread(std::io::Error::other(ended)));
            }
            thread::yield_now();
        }
        // A thread that is never to sleep would spin.
        let idle = options.idle.max(Duration::from_millis(1));
        let reclaimer = shared.clone();
        let reclaim = spawn("lignindb-reclaim", move || reclaimer.reclaim_loop(idle));
        let reclaim = match reclaim {
            Ok(reclaim) => reclaim,
            Err(error) => {
                shared.write_back().stop();
                let _ = write_back.join();
                return Err(error);
            }
        };
        let cleaning = shared.clone();
        let cleaner = match spawn("lignindb-cleaner", move || clean(&cleaning)) {
            Ok(cleaner) => cleaner,
            Err(error) => {
                shared.store.signal.stop();
                shared.write_back().stop();
                let _ = reclaim.join();
                let _ = write_back.join();
                return Err(error);
            }
        };
        self.opening.threads = started.elapsed();
        *lock(&self.threads) = Some(Threads {
            write_back,
            reclaim,
            cleaner,
        });
        Ok(())
    }

    /// Ends the session and releases the directory's lock: the background
    /// work stops, what can be written back is, a table checkpoint is
    /// taken, the session's files are closed and the log is made durable,
    /// so that the next open has little to replay.
    ///
    /// Dropping the handle does the same and returns no error of it; in a
    /// debug build both run the check at a close, which panics on a
    /// finding (see the module documentation). Either way
    /// every commit that was published is in the log, and the next open
    /// recovers what was durable or more (DU2). An error here says that
    /// something of the above was not done: the database is closed all the
    /// same, and the next open replays what the checkpoint does not cover.
    ///
    /// # What a caller still holds
    ///
    /// A close waits for no reader, refuses for none and ends none: it
    /// cannot be called while one lives. Everything a caller reads or
    /// writes through borrows the handle, and a close takes the handle:
    /// a write transaction, its scopes and cursors, a snapshot and its
    /// operations, and a parked cursor
    /// ([`ParkedCursor`](crate::txn::ParkedCursor)), which borrows its
    /// transaction and holds the pin of its version after its scope has
    /// ended. So a parked cursor is dropped before its transaction, and
    /// the transaction before the close:
    ///
    /// ```
    /// use lignindb::{
    ///     txn::{Batch, Subtree, SubtreeWrite},
    ///     Db, Options,
    /// };
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempfile::tempdir()?;
    /// let db = Db::open(dir.path(), &Options::default())?;
    /// let subtree = Subtree::new([1; 32]);
    /// let txn = db.begin();
    /// let scope = txn.scope()?;
    /// scope.apply(&Batch {
    ///     subtrees: &[SubtreeWrite::Enter {
    ///         subtree,
    ///         tree_type: b"tree",
    ///         host: None,
    ///     }],
    ///     ..Batch::default()
    /// })?;
    /// let parked = scope.parked_merk_cursor(subtree)?;
    /// scope.merge();
    /// drop(parked);
    /// drop(txn);
    /// db.close()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The same with the cursor dropped after the transaction and the
    /// close does not compile: the cursor borrows the transaction, which
    /// borrows the handle, so neither is given up while the cursor lives.
    ///
    /// ```compile_fail,E0505
    /// use lignindb::{
    ///     txn::{Batch, Subtree, SubtreeWrite},
    ///     Db, Options,
    /// };
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempfile::tempdir()?;
    /// let db = Db::open(dir.path(), &Options::default())?;
    /// let subtree = Subtree::new([1; 32]);
    /// let txn = db.begin();
    /// let scope = txn.scope()?;
    /// scope.apply(&Batch {
    ///     subtrees: &[SubtreeWrite::Enter {
    ///         subtree,
    ///         tree_type: b"tree",
    ///         host: None,
    ///     }],
    ///     ..Batch::default()
    /// })?;
    /// let parked = scope.parked_merk_cursor(subtree)?;
    /// scope.merge();
    /// drop(txn);
    /// db.close()?;
    /// drop(parked);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// A pin whose holder leaked it (`std::mem::forget`) borrows nothing
    /// any more. A close does not wait for it: what it holds back from
    /// being reclaimed is left to the next session.
    pub fn close(self) -> Result<(), DbError> {
        self.shut_and_check()
    }

    /// Ends the session and, where this call ended it, makes the check of
    /// a debug build.
    fn shut_and_check(&self) -> Result<(), DbError> {
        if self.shut()? {
            self.check_at_close();
        }
        Ok(())
    }

    /// Ends the session as [`Self::close`] does and then checks the store
    /// with the physical invariant checker ([`crate::verify`]), in every
    /// build: at its latest version, with what a store keeps beside its
    /// pages ([`Scope::Still`](verify::Scope::Still)), which can be
    /// compared because nothing of the session writes any more. `rules`
    /// are the functions that make the hashes of the nodes; with `None` no
    /// stored hash is recomputed.
    ///
    /// The report is returned and nothing panics: a finding is the
    /// caller's to act on. An error says that the close did not do all of
    /// its work, and then nothing is checked.
    ///
    /// A session that ended for writes ([`WriteError::Reopen`]) is
    /// checked here like any other, and its findings can be what it left
    /// half done: the next open's recovery makes that whole, and the check
    /// of the database it opens is the one that counts.
    pub fn close_and_verify(
        self,
        rules: Option<&mut dyn HashRules>,
    ) -> Result<verify::Report, DbError> {
        self.shut()?;
        let at = self.shared.versions.latest();
        Ok(self.check(at, verify::Scope::Still, rules))
    }

    /// Checks the latest version of the database as a reader sees it
    /// ([`Scope::Reader`](verify::Scope::Reader)): its pages, its trees,
    /// its directory and its values. It runs beside writers, write-back
    /// and the cleaner, pins its version for the run, and changes nothing.
    /// `rules` are the functions that make the hashes of the nodes; with
    /// `None` no stored hash is recomputed.
    ///
    /// **The version is pinned under the commit lock, with the reclaiming
    /// of what drops left held back, and the reclaiming stays held back
    /// until the check has ended.** A step of the reclaimer takes cells
    /// and records out of the newest version of a page, and a Merk goes
    /// in several steps on several pages. A version reads such a step on
    /// the pages of which it is the newest version and not on the
    /// others, so a check of a sound store beside a reclaiming can find a
    /// flat record whose entry is gone, which is also what damage looks
    /// like. Under the commit lock no commit is on its way from its
    /// installs to its publication, so every step that was made is in
    /// the pinned version. (A commit that stopped between the two ended
    /// the session, and its pages are of no version: no step is made on
    /// a page it installed, and a step on another page is in the pinned
    /// version, which is the newest of that page.) No step is made
    /// during the run, so what this check reads of the version is what
    /// the version holds from the pin to the end of the check.
    ///
    /// Before it pins, the check waits for a round of the reclaiming that
    /// is running, and for whoever holds the commit lock: a commit that
    /// is being made, a wipe, the sample of a checkpoint. The commit lock
    /// is given back once the version is pinned: commits and readers go
    /// on beside the check, and so do write-back and the cleaner, for
    /// whom the check is one reader more. So what waits for the end of a
    /// check is what waits for any reader's pin and operation guard (the
    /// reuse of page ids, the links of a chain, a file that was emptied),
    /// and two things more: a round of the reclaiming that is due, with
    /// any call of [`Self::reclaim`], so that the pages and values of
    /// dropped subtrees are given back later; and a second check on
    /// demand.
    ///
    /// `rules` are called with the reclaiming held back, so they call
    /// neither this function nor [`Self::reclaim`] on this handle. What
    /// the rules of a commit must not call is said at [`Self::commit`].
    ///
    /// In a session that has ended ([`Self::reopen`]) it checks what the
    /// readers of that session read on: the latest version that was
    /// published.
    ///
    /// What a store keeps beside its pages (the page ids, the counts of
    /// live values, the page table against its segments) can be compared
    /// only in a store that is still: [`Self::close_and_verify`].
    pub fn verify(&self, rules: Option<&mut dyn HashRules>) -> verify::Report {
        let shared = &*self.shared;
        // Every round of the reclaiming is made under this lock. It is
        // taken before the pin and given back after the check, so no step
        // is made from the pin to the end of the check.
        let _held_back = lock(&shared.store.reclaiming);
        // The pin is taken under the commit lock: no commit is on its
        // way from its installs to its publication, so every step the
        // reclaimer has made is in the version (also after a commit that
        // stopped between the two: see the documentation above). The
        // order is the one a round of the
        // reclaiming has when it loads the directory again: its lock,
        // then the commit lock. The other order would wait for such a
        // round for ever.
        let pin = {
            let _commit = shared.store.committing.lock();
            #[cfg(test)]
            store::at_verify(store::VerifyStage::Locked);
            let pin = shared.versions.pin();
            #[cfg(test)]
            store::at_verify(store::VerifyStage::Pinned);
            pin
        };
        #[cfg(test)]
        store::at_verify(store::VerifyStage::Reading);
        let report = self.check(pin.version(), verify::Scope::Reader, rules);
        #[cfg(test)]
        store::at_verify(store::VerifyStage::Checked);
        report
    }

    /// Leaves this handle out of the check that a close makes in a debug
    /// build ("The check at a close" in the [module documentation](self)):
    /// for a test that damages its store on purpose.
    pub fn unchecked_at_close(&self) {
        self.unchecked.store(true, Ordering::Relaxed);
    }

    /// Gives the check that a close makes in a debug build the rules by
    /// which it makes stored hashes again. It makes those that the checker
    /// does ([`crate::verify`]): of the cells of the Merks in use. The
    /// cells that a dropped Merk left, and those that nothing reaches, are
    /// no state, and their hashes are not made again. With no rules given
    /// it recomputes none.
    pub fn check_hashes_by(&self, rules: Box<dyn HashRules + Send>) {
        *lock(&self.hash_rules) = Some(rules);
    }

    /// Gives the checks of this handle the tree types that keep flat
    /// records, as the entries of the subtree directory state them: what
    /// the writers of the database give their batches
    /// ([`Scope::apply_by_kind`](crate::txn::Scope::apply_by_kind)). The
    /// check that a close makes in a debug build, [`Self::verify`] and
    /// [`Self::close_and_verify`] then hold the data column of every
    /// subtree to the tree type of its entry ([`verify::DataColumn`]).
    /// With none given they do not: the engine knows no tree type.
    pub fn check_kinds_by(&self, keeps_flat_records: FlatKinds) {
        *lock(&self.flat_kinds) = Some(keeps_flat_records);
    }

    /// The structures of this store as the checker reads them, checked at
    /// the version `at`.
    fn check(
        &self,
        at: CommitVersion,
        scope: verify::Scope,
        rules: Option<&mut dyn HashRules>,
    ) -> verify::Report {
        let shared = &*self.shared;
        let store = &*shared.store;
        let catalog = store.catalog();
        let columns = catalog.keyspaces.columns();
        // The catalog is the one flat keyspace that holds no record of a
        // subtree.
        let beside = [FlatTree::open(open::CATALOG_PAGE, store.config())];
        let data = Column::ALL
            .iter()
            .position(|column| *column == Column::Data);
        let kinds = *lock(&self.flat_kinds);
        let view = verify::Store {
            loader: store.loader(),
            table: &store.table,
            segments: &store.segments,
            values: &store.values,
            page_size: store.page_size,
            ids: Some(&store.ids),
            grove: verify::Grove::Directory {
                tree: catalog.directory,
                columns: &columns,
                data: data
                    .zip(kinds)
                    .map(|(column, keeps_flat_records)| verify::DataColumn {
                        column,
                        keeps_flat_records,
                    }),
            },
            flat_drops: Some(catalog.flat_drops),
            flat: &beside,
            values_counted: true,
        };
        let guard = shared.versions.guard();
        verify::check(&view, &guard, verify::Options::new(at, scope), rules)
    }

    /// The check of a close, in a debug build: the store is still, and
    /// every physical rule of it is checked at its latest version. A
    /// finding is a panic with the report's text.
    ///
    /// It is left out where the test is failing already, where the handle
    /// was taken out ([`Self::unchecked_at_close`]), and for a session
    /// that ended for writes: what such a session leaves is the next
    /// recovery's to make whole.
    fn check_at_close(&self) {
        if !cfg!(debug_assertions) || thread::panicking() {
            return;
        }
        let store = &*self.shared.store;
        if self.unchecked.load(Ordering::Relaxed) || store.refusal().is_some() {
            return;
        }
        let at = self.shared.versions.latest();
        let mut given = lock(&self.hash_rules);
        let rules = given
            .as_mut()
            .map(|rules| &mut **rules as &mut dyn HashRules);
        let report = self.check(at, verify::Scope::Still, rules);
        assert!(
            report.findings.is_empty() && report.more_findings == 0,
            "the database in {} is left unsound at version {at}:\n{report}",
            store.dir.display()
        );
    }

    /// Ends the session. `false` if it had ended before.
    fn shut(&self) -> Result<bool, DbError> {
        let Some(threads) = lock(&self.threads).take() else {
            return Ok(false);
        };
        let shared = &*self.shared;
        // Each thread ends after the turn it is in, which is still made
        // as any turn of the session was.
        shared.stop_background();
        let reclaimed = threads.reclaim.join();
        let cleaned = threads.cleaner.join();
        let written = threads.write_back.join();
        let finished = self.answered(self.finish());
        reclaimed.map_err(|_| DbError::Panicked("reclaim"))?;
        cleaned.map_err(|_| DbError::Panicked("cleaner"))?;
        written.map_err(|_| DbError::Panicked("write-back"))?;
        finished.map(|()| true)
    }

    /// What a close does for the next session, with the background
    /// threads ended: only in a directory that is found to be this
    /// database's still. One that was removed under the handle, or that
    /// holds another database now, is written to no more (P-18).
    fn finish(&self) -> Result<(), DbError> {
        let shared = &*self.shared;
        let ours = match shared.store.ours(&*self.guard) {
            // The directory has this handle's lock file, and the session
            // cannot say which manifest it wrote last: the manifest is
            // opened again, as write-back would at its next use, and the
            // directory is looked at once more. Where the manifest cannot
            // be opened, the close finishes nothing more.
            Err(_in_doubt) => {
                shared.write_back().settle_manifest()?;
                matches!(shared.store.ours(&*self.guard), Ok(true))
            }
            Ok(ours) => ours,
        };
        if !ours {
            return Err(DbError::Removed {
                dir: shared.store.dir.clone(),
            });
        }
        // The reclaimings that hold cells first: their steps go to the log
        // that the finish makes durable. Then the cleaner closes the files
        // that took its copies, so that the manifest of write-back's
        // finish names them as they are.
        shared.finish_reclaiming();
        let cleaned = shared.cleaner().finish();
        let written = shared.write_back().finish();
        // The session was given both, whichever of them the close
        // answers with: a refusal for room releases the reserve (DF13).
        let troubles = &shared.store.troubles;
        if let Err(error) = &cleaned {
            troubles.refused(error);
        }
        if let Err(error) = &written {
            troubles.refused(error);
        }
        match (cleaned, written) {
            // The cleaner could not make its segment durable, and
            // write-back answers with what follows from that: it installs
            // no table checkpoint. The cleaner's error is the one that
            // names the segment, and has what the file layer said where
            // the cleaner's own call met the refusal.
            (
                Err(cause @ CleanerError::SegmentNotDurable { .. }),
                Err(FlushError::SegmentsNotDurable),
            ) => Err(DbError::Clean(cause)),
            // The same where a pass of the cleaner's loop met the
            // refusal, in a turn that nobody was answered for: the
            // cleaner has no segment left to close then, and the error
            // of that pass is the one that names the segment.
            (_, Err(FlushError::SegmentsNotDurable)) => {
                let lost = troubles.segment_lost();
                Err(lost.map_or(
                    DbError::Flush(FlushError::SegmentsNotDurable),
                    DbError::Clean,
                ))
            }
            (cleaned, written) => {
                written?;
                cleaned.map_err(DbError::Clean)
            }
        }
    }

    /// The directory of the database.
    pub fn dir(&self) -> &Path {
        &self.shared.store.dir
    }

    /// Whether this open made the database: the directory held none.
    pub fn created(&self) -> bool {
        self.created
    }

    /// The size of the store's pages.
    pub fn page_size(&self) -> PageSize {
        self.shared.store.page_size
    }

    /// The latest published version. The first commit of a caller is
    /// version 2: version 1 made the keyspaces.
    pub fn version(&self) -> CommitVersion {
        self.shared.versions.latest()
    }

    /// The flat keyspace of each column of records: data, aux, roots and
    /// meta. A record of a subtree is in its column's keyspace under the
    /// subtree's 32 bytes and its key, and is read and written through a
    /// transaction or a snapshot by its [`Unit`](crate::txn::Unit).
    pub fn keyspaces(&self) -> Keyspaces {
        self.shared.store.catalog().keyspaces
    }

    /// Begins a write transaction (WT1). Any number may be open, and one
    /// commits at a time. It holds nothing until it writes, or reads a
    /// node of a Merk or opens a cursor over one, which has it watch the
    /// commits beside it ([`crate::txn`], "What a transaction holds").
    ///
    /// It is committed with [`Self::commit`] and rolled back with
    /// [`Self::rollback`], through this handle. A transaction that is
    /// dropped gives up what it holds.
    pub fn begin(&self) -> WriteTxn<'_> {
        self.shared.path().begin()
    }

    /// Commits `txn` as the next version of the database
    /// (`transactions.md` section 8, [`crate::commit`]): its stale hashes
    /// are evaluated with `rules`, one group is appended to the log, and
    /// the version is published. A caller whose writers make every hash,
    /// as on existing GroveVersions, passes [`NoStaleHashes`].
    ///
    /// How a commit ends, and what each outcome leaves of the transaction
    /// and of the database, is the table of [`CommitError`]. A transaction
    /// that wrote nothing commits nothing ([`Committed::Empty`]).
    ///
    /// A session that takes no write any more refuses with
    /// [`WriteError::Reopen`] and leaves the transaction as it was.
    ///
    /// `rules` are called under the commit lock. They call nothing of
    /// this database that waits for a commit or for the reclaiming, on
    /// their own thread or by waiting for another: no commit and no
    /// write, and neither [`Self::verify`] nor [`Self::reclaim`]. A check
    /// on demand that holds the reclaiming back waits for this commit,
    /// and a rule that waited for the reclaiming would wait for that
    /// check for ever.
    pub fn commit<R: HashRules>(
        &self,
        txn: &WriteTxn<'_>,
        rules: &mut R,
    ) -> Result<Committed, WriteError> {
        self.counted(rules, |committer, rules| committer.commit(txn, rules))
    }

    /// Rolls `txn` back (WT9): its writes, its private copies, its page
    /// ids and its savepoints go, and it can be written again. It is
    /// refused while a scope of the transaction is open.
    ///
    /// A transaction of another database is refused and left as it is,
    /// as [`Self::commit`] leaves it.
    pub fn rollback(&self, txn: &WriteTxn<'_>) -> Result<(), TxnError> {
        if !std::ptr::eq(txn.path().versions, &self.shared.versions) {
            return Err(TxnError::Damaged(
                "the transaction was begun from the write path of another database",
            ));
        }
        self.shared.committer().rollback(txn)
    }

    /// A write with no transaction (RD7): `batch` is applied to a write
    /// transaction of its own, which is committed at once. It waits for no
    /// open write transaction, and its commit is one like any other to
    /// them (CF8).
    ///
    /// That transaction has read nothing, so a node of the batch cannot
    /// keep a committed value: with no handle named the batch is refused
    /// ([`BatchError::NoRead`](crate::txn::BatchError::NoRead)), and with
    /// a handle of another transaction the write fails with a conflict.
    ///
    /// `rules` are called under the commit lock, as those of
    /// [`Self::commit`] are, and what is said of them there holds here.
    pub fn write<R: HashRules>(
        &self,
        batch: &Batch<'_>,
        rules: &mut R,
    ) -> Result<Committed, WriteError> {
        self.counted(rules, |committer, rules| committer.write(batch, rules))
    }

    /// A snapshot of the latest version (RD1): every read through it is of
    /// that version, until it is dropped. It takes no lock and reads no
    /// page here. A read with no transaction is one of these, held for one
    /// operation of the caller (RD2).
    pub fn snapshot(&self) -> Snapshot<'_> {
        self.shared.reads().snapshot()
    }

    /// One operation that pins no version (RD4): it takes its guard and
    /// reads the latest version. `lease` is the caller's, kept from one
    /// operation to the next ([`LatestLease`]).
    pub fn read(&self, lease: &mut LatestLease) -> Operation<'_> {
        self.shared.reads().operation(lease)
    }

    /// Makes every commit that was published when this was called durable
    /// (DU1a, P-05): the log is synced through the group of the latest
    /// version, which is the flush's target. A crash at any time after it
    /// returned recovers that version or a later one. A commit that is
    /// published while it runs may or may not be covered.
    ///
    /// It holds an operation guard and not the commit lock: commits are
    /// made and published beside it. The sync itself is the log's, which
    /// makes one sync at a time, and the counts of the value files are
    /// read under their lock for a moment before and after it.
    ///
    /// If it fails nothing is promised, and the session takes no write any
    /// more ([`Reopen::NotDurable`]): after a sync that failed the log
    /// makes nothing durable in this session. The failure is recorded
    /// under the commit lock, so every commit is published before it or
    /// refused.
    pub fn flush(&self) -> Result<Flushed, DbError> {
        let shared = &*self.shared;
        // The guard first, then the target (PN5): every commit up to the
        // target has its group in the log (CM3 steps 6 and 8), and no file
        // of the log goes under the guard.
        let guard = shared.versions.guard();
        let version = guard.latest();
        let flushed = match shared.write_back().sync_log() {
            Ok(durable) => Ok(Flushed { version, durable }),
            Err(error) => {
                drop(guard);
                // Under the commit lock, so that no commit is published
                // behind the record of the sync that failed.
                let committing = &shared.store.committing;
                let _commit = committing.lock();
                committing.sync_failed(shared.versions.latest(), &error);
                Err(DbError::Flush(error))
            }
        };
        self.answered(flushed)
    }

    /// Makes a checkpoint of the database in the new directory `target`,
    /// which is opened as a database of its own ([`Db::open`]). This is
    /// [`Checkpointer::create`] with `options`, on the commit path of this
    /// database: the documentation of that call is the contract of this
    /// one, and [`crate::checkpoint`] says what a checkpoint is, what its
    /// creation holds of the live database and for how long, and what a
    /// creation leaves that fails or that the power cuts.
    ///
    /// The call borrows the handle: it is made from any thread, beside
    /// commits, readers, the background work and other calls of it. A
    /// close takes the handle, so no call of this is made beside one.
    ///
    /// **The call asks the commit path whether the session takes
    /// commits, and not [`Self::reopen`].**
    ///
    /// - A call that finds the commit path stopped when it begins is
    ///   refused with [`CheckpointError::Stopped`], which has the commit
    ///   path's reason, and has made no call to the file layer. The
    ///   commit path has stopped in every session of which
    ///   [`Self::reopen`] says [`Reopen::NotDurable`] or
    ///   [`Reopen::Stopped`].
    /// - A call that finds it not stopped goes on as in any session. A
    ///   session of which [`Self::reopen`] says
    ///   [`Reopen::SegmentsNotDurable`] can be one whose commit path has
    ///   not stopped.
    /// - A call whose own sync of the log fails returns
    ///   [`CheckpointError::Sync`]. A failed WAL file sync stays failed
    ///   for this session. An otherwise active session ends for that
    ///   failure; if the session already reported a reason for ending, it
    ///   keeps that reason ("A session that takes no write any more" in
    ///   the [module documentation](self)).
    ///
    /// [`DbStats`] has no count of the checkpoints that this call makes.
    /// An error of this call is returned to its caller, and the call does
    /// not count it among the errors of the background work
    /// ([`DbStats::background`]).
    pub fn checkpoint(
        &self,
        target: &Path,
        options: CheckpointOptions,
    ) -> Result<Created, CheckpointError> {
        let committer = self.shared.committer();
        let created = Checkpointer::new(&committer)
            .with_options(options)
            .create(target);
        self.answered(created)
    }

    /// Removes every record of every keyspace: the nodes of every Merk,
    /// and the records of the data, roots, aux and meta columns (P-23).
    /// It is one commit (CV3), so no reader sees a database that is wiped
    /// in part, and a crash leaves it as it was or empty.
    ///
    /// The commit lock is held from the walk that finds what there is to
    /// drop until the wipe is published, so no commit comes between the
    /// two: every commit is before the wipe, and wiped, or after it. At the
    /// wipe's own version no subtree is entered.
    ///
    /// Its key set is every key (CF5): an open write transaction that
    /// wrote a record before the wipe was published fails its commit with
    /// a conflict. A snapshot from before keeps reading its version.
    ///
    /// A database in which no subtree is entered has no record. Its wipe
    /// is a commit all the same, with a version and the key set of a wipe:
    /// it enters one subtree and drops it.
    pub fn wipe(&self) -> Result<Committed, WriteError> {
        self.wipe_entering(&[])
    }

    /// As [`Self::wipe`], and enters each subtree of `enters` in the same
    /// commit, as a tree of the type given with it and with no host: after
    /// a wipe no element is left to host one. At the wipe's version these
    /// subtrees are entered and hold nothing, and no other subtree is
    /// entered. A subtree of the list that was entered before is dropped
    /// with the others and is new.
    ///
    /// The drops and the enters are one write of one transaction and one
    /// group of the log, so whatever the wipe returns is of both. An error
    /// of a commit that was not made has removed nothing and entered
    /// nothing: a list that is refused, as one that names a subtree twice
    /// is, and every [`CommitError`] that says the commit is not in the
    /// log. The errors of a commit that happened, or that the next open
    /// may find ([`CommitError::NotDurable`], [`CommitError::Halted`],
    /// [`CommitError::Unsettled`]), are of the wipe with its enters, and
    /// never of the wipe alone.
    pub fn wipe_entering(&self, enters: &[(Subtree, &[u8])]) -> Result<Committed, WriteError> {
        self.shared.store.signal.commit_asked();
        self.answered(self.wipe_and_enter(enters))
    }

    fn wipe_and_enter(&self, enters: &[(Subtree, &[u8])]) -> Result<Committed, WriteError> {
        self.refused()?;
        let mut everything = KeySet::new();
        everything.everything();
        let committer = self.shared.committer();
        let committed = committer.commit_alone(&mut NoStaleHashes, Some(&everything), |txn| {
            let outermost = self.outermost()?;
            let scope = txn.scope()?;
            let apply = |subtrees: &[SubtreeWrite<'_>]| {
                scope.apply(&Batch {
                    subtrees,
                    ..Batch::default()
                })
            };
            if outermost.is_empty() && enters.is_empty() {
                // A wipe is a commit also where there is nothing to drop
                // (CV3): a subtree is entered and dropped, so that the
                // wipe has a version and its key set meets what open
                // transactions wrote before it (CF5).
                let subtree = Subtree::new([0; 32]);
                apply(&[SubtreeWrite::Enter {
                    subtree,
                    tree_type: &[],
                    host: None,
                }])?;
                apply(&[SubtreeWrite::Drop { subtree }])?;
            } else {
                // The drops come before the enters of their batch: each
                // subtree of the list is absent when it is entered.
                let drops =
                    (outermost.iter()).map(|subtree| SubtreeWrite::Drop { subtree: *subtree });
                let enters = enters
                    .iter()
                    .map(|(subtree, tree_type)| SubtreeWrite::Enter {
                        subtree: *subtree,
                        tree_type,
                        host: None,
                    });
                let writes: Vec<SubtreeWrite<'_>> = drops.chain(enters).collect();
                apply(&writes)?;
            }
            scope.merge();
            Ok(())
        });
        self.settle(committed)
    }

    /// The subtrees in use that no other subtree hosts, as the latest
    /// version has them: dropping them drops every subtree. The caller
    /// holds the commit lock, so the latest version stays the latest.
    fn outermost(&self) -> Result<Vec<Subtree>, TxnError> {
        let shared = &*self.shared;
        let guard = shared.versions.guard();
        let at = guard.latest();
        let directory = shared.path().directory_at(at)?;
        let pages = shared.store.committed(&guard, at);
        let mut outermost = Vec::new();
        let mut entries = directory.entries();
        while let Some((prefix, entry)) = entries.next(&pages)? {
            if entry.host().is_none() && directory.is_live(&entry) {
                outermost.push(Subtree::new(prefix));
            }
        }
        Ok(outermost)
    }

    /// Every count the engine keeps.
    pub fn stats(&self) -> DbStats {
        let shared = &*self.shared;
        let store = &*shared.store;
        let troubles = &store.troubles;
        DbStats {
            version: shared.versions.latest(),
            store: store.stats.read(&shared.write_back()),
            commits: store.committing.stats(),
            recovery: store.recovery,
            opening: self.opening,
            reclaimed: *lock(&store.reclaimed),
            cleaner: store.cleaner.stats(),
            ids_released: store.ids_released.load(Ordering::Relaxed),
            ids: store.ids.census(),
            memory: Memory {
                capacity: store.pool.capacity(),
                fixed: store.pool.fixed_memory(),
                mapped: store.pool.mapped_memory(),
                held: store.pool.memory(),
            },
            pins: shared.versions.pins_held(),
            guards: shared.versions.guards_held(),
            directories: store.directories.len(),
            background: Background {
                write_back_errors: troubles.write_back.load(Ordering::Relaxed),
                reclaim_errors: troubles.reclaim.load(Ordering::Relaxed),
                cleaner_errors: troubles.cleaner.load(Ordering::Relaxed),
                last_error: lock(&troubles.last).clone(),
            },
            reserve: troubles.reserve.stats(),
            reopen: store.refusal(),
        }
    }

    /// Why the session takes no write any more, if it does not: the
    /// database has to be closed and opened again. The reason is the
    /// first the session had, and it is the same from then on, here, in
    /// [`DbStats::reopen`] and in every commit that is refused for it.
    ///
    /// This is a look at what the store holds. It makes no operation on a
    /// file and settles nothing: a replacement of the manifest that was
    /// left in doubt is no reason, and is not settled here.
    pub fn reopen(&self) -> Option<Reopen> {
        self.shared.store.refusal()
    }

    /// One round of the reclaiming that the background thread makes, on
    /// the calling thread: the reclaimers of the store take their step,
    /// and what dropped subtrees left is taken out as far as it can be
    /// now. A tool that has to see the space back, and a test, call it;
    /// nothing else has to.
    pub fn reclaim(&self) -> Round {
        self.shared.reclaim_round(false)
    }

    /// One turn of the cleaning that the background thread makes, on the
    /// calling thread: a pass over the segments and then one over the
    /// value log, each made whatever became of the other. A pass that
    /// fails is counted and told as a pass of the thread that failed
    /// ([`Background::cleaner_errors`]) when it returns, before the next
    /// pass begins, and the first such error is returned. A tool that has to see the space back in a session that
    /// it asks for no commit, and a test, call it; nothing else has to.
    ///
    /// It may be called while the cleaner's thread makes its turns: one
    /// pass of the cleaner runs at a time, over the segments or over the
    /// value log, and a pass of this call waits for the one that is under
    /// way, as one of the thread waits for a pass of this call.
    pub fn clean(&self) -> Result<(Pass, ValuePass), CleanerError> {
        let troubles = &self.shared.store.troubles;
        with_cleaner(&self.shared, |cleaner, owners| {
            // Each pass is told when it returns, before the next begins.
            let told = |error: &CleanerError| troubles.cleaner_failed(error);
            let segments = cleaner.pass().inspect_err(told);
            let values = cleaner.pass_values(owners).inspect_err(told);
            Ok((segments?, values?))
        })
    }

    /// The entries of the database directory that are not the engine's,
    /// by name, in order: what an open, a recovery, a wipe and a close
    /// leave alone (P-03).
    pub fn foreign_entries(&self) -> std::io::Result<Vec<OsString>> {
        open::foreign_entries(&*self.shared.store.vfs, &self.shared.store.dir)
    }

    /// A commit through the facade: refused in a session that takes no
    /// write, made with the calls of `rules` counted as hashes of the
    /// store, and settled.
    fn counted<R: HashRules>(
        &self,
        rules: &mut R,
        commit: impl FnOnce(&Committer<'_>, &mut Counted<'_, R>) -> Result<Committed, CommitError>,
    ) -> Result<Committed, WriteError> {
        self.shared.store.signal.commit_asked();
        let written = self.refused().and_then(|()| {
            let mut rules = Counted { rules, calls: 0 };
            let committed = commit(&self.shared.committer(), &mut rules);
            self.shared.store.stats.count_hashes(rules.calls);
            self.settle(committed)
        });
        self.answered(written)
    }

    /// Passes on what a call of the facade answers. Where that is an
    /// error which a full device caused, the session releases its reserve
    /// first, if it has one on the device (DF13). The caller holds no
    /// lock.
    fn answered<T, E: std::error::Error + 'static>(&self, answer: Result<T, E>) -> Result<T, E> {
        if let Err(error) = &answer {
            self.shared.store.troubles.refused(error);
        }
        answer
    }

    /// What a commit looks at before it begins, with no lock held: a
    /// manifest that it finds in doubt is settled, and a session that has
    /// ended refuses. A manifest that cannot be settled now refuses the
    /// commit with the error of that, its transaction whole: the commit
    /// can be tried again, and the session has not ended for it.
    ///
    /// The look does not wait for the manifest. While write-back replaces
    /// it, or somebody else settles it, the commit finds nothing in doubt
    /// and is made. A commit that found the manifest in doubt waits for
    /// the manifest's lock, to have it settled.
    fn refused(&self) -> Result<(), WriteError> {
        let settled = self.shared.settle_manifest();
        match self.shared.store.refusal() {
            Some(reopen) => Err(WriteError::Reopen(reopen)),
            None => settled.map_err(|error| WriteError::Commit(CommitError::NotReady(error))),
        }
    }

    /// What the facade does with the end of a commit: the reclaimer is
    /// handed what the commit dropped, and write-back is woken. An outcome
    /// that ends the session for writes was recorded by the commit path,
    /// under the commit lock.
    fn settle(&self, committed: Result<Committed, CommitError>) -> Result<Committed, WriteError> {
        let shared = &*self.shared;
        let published = |published: &Published| {
            shared.dropped(published.version, &published.dropped);
            shared.write_back().wake();
        };
        match &committed {
            Ok(Committed::Published(commit)) => published(commit),
            Err(CommitError::NotDurable {
                published: commit, ..
            }) => published(commit),
            _ => {}
        }
        // A store that failed beside this commit ends the session before
        // this thread's next one (D-151).
        let said = shared.store.look_after_a_commit();
        // A commit that waited for the commit lock while the session
        // stopped is refused as one that came later is: the transaction is
        // as it was, and the reason is the one the session gives.
        committed.map_err(|error| match error {
            CommitError::Stopped(stopped) => WriteError::Reopen(said.unwrap_or(stopped.into())),
            error => WriteError::Commit(error),
        })
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // The error a close returns is dropped here: a handle whose
        // directory was removed under it closes without a word (P-18).
        // The check of a debug build is made as at a close.
        let _ = self.shut_and_check();
    }
}

#[cfg(test)]
thread_local! {
    /// What a test has the thread of write-back run before its loop, in
    /// the next database whose background work this thread starts.
    static BEFORE_THE_LOOP: std::cell::RefCell<Option<Box<dyn FnOnce() + Send>>> =
        const { std::cell::RefCell::new(None) };
}

/// Starts a thread of a database's background work.
fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, OpenError> {
    let builder = thread::Builder::new().name(name.to_owned());
    builder.spawn(work).map_err(OpenError::Thread)
}

/// Runs `work` with the view of write-back that the background threads
/// use. It has the tree layer's word on the values of a page: their scan
/// order, and the regrouping before a base image. The page steps of a
/// regrouping append their records through a view that has no such word,
/// since the two refer to each other.
fn with_page_values<R>(shared: &Shared, work: impl FnOnce(WriteBack<'_>) -> R) -> R {
    let store = &*shared.store;
    let plain = shared.write_back();
    let regrouping = Regrouping::new(
        store.loader(),
        &store.pool,
        &store.values,
        store.page_size,
        shared.steps(),
        RegroupPolicy::default(),
    );
    let stores = Stores {
        page_values: Some(&regrouping),
        ..plain.stores()
    };
    work(WriteBack::new(stores, &store.flush, store.page_size).unwrap_or(plain))
}

/// How many turns of write-back an open makes at most ([`Db::open`]).
pub const OPEN_TURNS: usize = 8;

/// The turns of write-back that an open makes on its own thread, before
/// the background threads are started ([`Db::open`]): turns as the loop
/// makes them, with the loop's view of the store, until the pass of one
/// moves nothing on, the turn takes the log to no other file and leaves
/// no turn of the log due, and `at_most` turns. A table checkpoint that
/// such a last turn takes asks for no further turn by itself: the record
/// it appends can, where it fills the log's file. The error of a turn
/// that fails is told to the session where the loop tells its own
/// (`Troubles::write_back_failed`), and no turn is made after it.
fn turns_at_the_open(shared: &Shared, at_most: usize) {
    let store = &*shared.store;
    let troubles = &store.troubles;
    with_page_values(shared, |write_back| {
        for _ in 0..at_most {
            match write_back.tick() {
                // The mark of a table checkpoint that the turn took can
                // have filled the log's file: the next turn turns the log.
                Ok(tick) if tick.rolled || tick.pass.progressed() || store.log.is_due() => {}
                Ok(_) => break,
                Err(error) => {
                    troubles.write_back_failed(&error);
                    break;
                }
            }
        }
    });
}

/// The background loop of write-back, for as long as the session lives
/// (`lignin_store::flush`).
fn write_back(shared: &Shared) {
    let troubles = &shared.store.troubles;
    with_page_values(shared, |write_back| {
        write_back.run(|error| troubles.write_back_failed(&error));
    });
}

/// The owners of values that the engine gives its cleaner: the cells of
/// tree pages, and the entries of flat leaves.
type Owners<'a> = BothOwners<TreeOwners<'a, store::Steps<'a>>, FlatOwners<'a, store::Steps<'a>>>;

/// Runs `work` with the cleaner of the session as its thread has it, and
/// with the owners of values. The owners are the tree layer's, made on
/// this thread: the cells of tree pages and the entries of flat leaves,
/// whose pointers a pass over the value log rewrites in page steps. The
/// table checkpoints the cleaner asks for make passes of write-back, so
/// it has the view that write-back's own thread has.
fn with_cleaner<R>(shared: &Shared, work: impl FnOnce(&Cleaner<'_>, &Owners<'_>) -> R) -> R {
    let store = &*shared.store;
    let owners = BothOwners(
        TreeOwners::new(store.loader(), &store.pool, store.page_size, shared.steps()),
        FlatOwners::new(store.loader(), &store.pool, store.config(), shared.steps()),
    );
    with_page_values(shared, |write_back| {
        work(&shared.cleaner_on(write_back), &owners)
    })
}

/// The background loop of the cleaner, for as long as the session lives
/// (`lignin_store::cleaner`): a pass over the segments and one over the
/// value log, again and again. At the end of each turn a reserve that the
/// session released is filled again, if it is time for that (DF13).
///
/// The loop begins when the session is asked for its first commit
/// (`store::Signal::wait_for_a_commit`), and not at all in a session that
/// is asked for none.
fn clean(shared: &Shared) {
    let store = &*shared.store;
    if !store.signal.wait_for_a_commit() {
        return;
    }
    with_cleaner(shared, |cleaner, owners| {
        let mut turns = CleanerTurns::begun(store);
        cleaner.run(
            |cleaner| end_turn(shared, cleaner, owners, &mut turns),
            |error| store.troubles.cleaner_loop_failed(error),
        );
    });
}

/// What ends a turn of the cleaner's loop: the pass over the value log,
/// and the look at the turn for the reserve of the session.
fn end_turn(
    session: &Shared,
    cleaner: &Cleaner<'_>,
    owners: &Owners<'_>,
    turns: &mut CleanerTurns,
) -> Result<ValuePass, CleanerError> {
    let passed = cleaner.pass_values(owners);
    turns.ended(session);
    passed
}
