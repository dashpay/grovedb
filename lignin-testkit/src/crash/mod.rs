//! Store crash tests (VER-03): the page store is crashed at every write
//! boundary of random workloads, the states that the campaign's crash
//! plans leave of each crash are recovered, and what a recovery lands on
//! is held to a model.
//!
//! The store is `lignin-store` behind its public API: the log, write-back,
//! table checkpoints, recovery, the page ids and the value log. The file
//! layer is the simulated one of `lignin_io::sim`, so a run needs no disk
//! and is a function of its seed. The engine above the store, with
//! transactions, savepoints, checkpoints of a database and GroveDB's root
//! hashes, has a campaign of its own (VER-04), which this one is built to
//! be the base of.
//!
//! ```
//! use lignin_testkit::crash::campaign::{campaign, RunShape, Settings};
//!
//! // Two runs, each a workload of a few operations with a crash image at
//! // every write boundary.
//! let settings = Settings {
//!     seed: 1,
//!     runs: Some(2),
//!     shape: RunShape {
//!         ops: 8,
//!         lineages: 2,
//!         ..RunShape::default()
//!     },
//!     ..Settings::default()
//! };
//! let report = campaign(&settings, &mut |_, _| {});
//! assert!(report.clean(), "{report}");
//! assert!(report.points > 100);
//! ```
//!
//! The command is `lignin-crash` ([`cli::USAGE`]):
//!
//! ```text
//! cargo run --release -p lignin-testkit --bin lignin-crash -- run --points 1000000 --jobs 6
//! cargo run --release -p lignin-testkit --bin lignin-crash -- seed --seed 4711
//! cargo run --release -p lignin-testkit --bin lignin-crash -- planted
//! ```
//!
//! # What is run
//!
//! | Part | What it is |
//! | --- | --- |
//! | [`store::Session`] | An open store, and what `lignin-store` asks of its callers: the committer of `transactions.md` CM3, in the order of the engine's (`lignindb::commit`), with the sync of a commit before its version is published; the keeper of the pins, a writer's on the version it was begun at among them; the owners of values for the value log's cleaner; the regrouping of a page's values; and the page steps that take cells out of a page or free one. |
//! | [`workload`] | The operations of a session, drawn from the seed: commits to tree pages and flat leaves with values in and out of line, write transactions that hold copies, reclaiming steps, passes, turns, table checkpoints, syncs and rolls of the log, passes of the cleaner over the segments and over the value log, pins, kills and closes. |
//! | [`model`] | What the pages say after each item of the log that changes them. It knows no frame, block or file. |
//! | [`observe::Observer`] | The file layer of the sessions of a lineage: it takes a crash image after every operation that changed what a crash can leave, and knows which handles wrote to a file since its last sync. |
//! | [`check`] | One state a crash left, recovered and held to the model. |
//! | [`campaign`] | Runs, their lineages, and what is counted. |
//!
//! A **write boundary** is an operation of the file layer after which a
//! crash can leave something new: one that made an unsynced change, or a
//! sync that made a change durable (`CrashPoint::AfterChange` of
//! `lignin_io::sim`). The workload is run once, and the observer freezes
//! the file system at each boundary. A **crash state** is what one crash
//! plan leaves of such an image: some of the unsynced changes, whole, torn
//! at a sector or applied out of turn, on the durable state. For each
//! image the campaign takes every plan when there are at most 48. Of an
//! image with more it takes a choice, and not every state that crash can
//! leave: the two ends; the plans of a file system that writes in order,
//! with the first lost write cut short after each number of its leading
//! sectors (`CrashImage::prefixes`); each change lost alone; each change
//! applied late; and a few drawn from the seed ([`campaign::RunShape`]). A
//! tear that keeps sectors of a write and loses one before them is in a
//! drawn plan or in none. States are told apart by `SimVfs::fingerprint`,
//! and **the crash points a campaign counts are the distinct states it
//! recovered**.
//!
//! A store that a crash left does not only recover: a few states of each
//! lineage go on. The store is opened, a workload is run on it, and it is
//! crashed again at every boundary, to the depth the shape says. So the
//! files of an earlier session that a recovery closes, the log file it
//! writes again, the versions a crash gave back and the pages a replay
//! rebuilt are all crashed in their turn, and so is the recovery itself:
//! the boundaries of an open are boundaries like any other.
//!
//! # What "the recovered state" is
//!
//! What [`store::Session::read`] finds in the store that
//! `lignin_store::recovery::recover` returned, read through the pool, the
//! page table, the segments, the value log and the page ids:
//!
//! - **The pages in use, and what each says.** Every page id that the
//!   allocator has in use, every page that the page table has a chain of,
//!   and every page of the points the store may be at, is asked for at the
//!   version the store landed on. A tree page is its cells by slot: key,
//!   feature bytes, value hash, key-value hash and value. A flat leaf is
//!   its entries by key. The store has no roots: which pages a tree
//!   reaches is the tree layer's to say, so every page in use is compared.
//! - **When each page was born, and how many cells have left it.** A page
//!   version is born at the commit that last changed the page
//!   (`transactions.md` PV1), and a tree page counts the cells that were
//!   taken out of it (`tree-page.md` TP7). Both are the workload's doing
//!   and are compared: a handle on a cell goes by the count, and a page
//!   step by the birth.
//! - **The page ids.** An id is in use exactly when its page is read, and
//!   the chain of a page that is not in use does not end in a block.
//! - **The files.** The directories of the log, the segments, the value
//!   log and the table checkpoints hold no file that the manifest in force
//!   does not name ([`store::Session::strays`]): a file that an earlier
//!   session dropped and could not remove, or made and never named, is
//!   gone once a recovery with no fault has returned.
//! - **The values.** A value is its bytes, wherever they lie: in the cell,
//!   with the frame of its page while it is pending, or in the value log.
//!   Each is read, and has the length its cell says. Whether a value is in
//!   line is compared. Where a value out of line lies is not: write-back
//!   and the cleaner move it without a commit.
//! - **The counts of live values** are the pointers that the pages hold,
//!   file by file, and no count was found wrong.
//!
//! Not compared with the model: the chains of the page table, which blocks
//! hold a page, how many files there are and what the dead space is. They
//! are what the store is free to change.
//!
//! **A second look, by the engine's own checker.** Every recovered store
//! is also given to the physical invariant checker of the engine
//! (`lignindb::verify`, VER-07) in its scope for a store that is still
//! ([`store::Session::verify`]). It knows nothing of the model, and reads
//! what the model does not: every link of every chain against its block,
//! a block that two links name, a page version in a segment that holds a
//! pending value, a value whose record names another owner, a pointer
//! that two cells hold. It is told what the workload writes: every cell
//! is a Merk of one node and every flat leaf a keyspace of one page, and
//! no hash is recomputed. A finding of it is a violation like one of the
//! model's.
//!
//! # What "committed" is
//!
//! The model keeps a **point** for every item of the log that changes what
//! the pages say, in the order of the log: the group of a commit, and the
//! record of a reclaiming step, which takes cells out of a page or frees
//! it with no commit (`lignin_store::recovery`: a recovery lands on a
//! commit "with every background change whose record the log holds"). A
//! point is added when the append of its item has returned `Ok`: from then
//! the commit has happened (`transactions.md` CM9). The session tells the
//! model at that moment, and not when its call returns
//! ([`store::Item`]): what it writes back after the append is crashed
//! with the item a point already.
//!
//! **A recovered state is committed when it is a point**: the version the
//! recovery landed on is the point's, and every page says what the point
//! says ([`model::History::find`]). Which points a state may be is bounded
//! on both sides ([`model::Expect`]):
//!
//! - **Not before the floor.** The floor is the latest of these points:
//!   - *What the store was asked for.* The last point, once a sync of the
//!     log has returned `Ok`, a commit's or one on its own, and once a
//!     recovery has returned a store, which makes the log durable to its
//!     end (`lignin_store::recovery`, step 9). The place the log reports
//!     is not asked: a sync that returned `Ok` and left the last commit
//!     to be lost is found whatever the log says of itself.
//!   - *What the log reported.* The last point whose item ends at or
//!     before the place the log had reported durable
//!     (`LogStore::durable`) when the operation ended: write-back syncs
//!     the log on its own.
//!   - *What the files hold.* The point that the state with no unsynced
//!     change recovered to, of the same image or of an earlier one of the
//!     same file system (`Run::check_image`). What the durable part of
//!     the files recovers to is lost by no crash from then on: a store
//!     that takes back a point that was durable is found even where
//!     nothing had said the point was.
//!
//!   An image carries the floor of the moment it was taken. After every
//!   operation the states that a crash then can leave are gone through
//!   once more, and held to what the operation made of the floor and of
//!   its item.
//! - **A crash that loses nothing loses no point.** The plan that keeps
//!   every unsynced change, whole and in order, leaves the files as the
//!   session sees them: what a process that is killed leaves. Of every
//!   image, that state is at the last point, or at the item that is being
//!   appended, and at no point before, as the store is when it is opened
//!   again with no crash. An image that was taken after a write-back
//!   failed can be the exception, until a recovery with no write-back
//!   failing under it has returned the store: see "A write-back that
//!   fails" under [Faults](self#faults).
//! - **Not after the last point**, but for one case: an item whose append
//!   has begun and has not been reported as in the log or as out of it for
//!   good. The model is told what that item makes of the pages before it
//!   is appended, and a state may be at it.
//!
//! **When the file layer fails**, the store says what became of an item,
//! and the model follows it (`lignin_store::wal`, "When the file layer
//! fails"):
//!
//! | The append returned | The item |
//! | --- | --- |
//! | `Ok` | is a point. |
//! | `NotAppended`, or any error that is not one of the two below | is no point. A state of a crash before the append returned may be at it: the model does not know then how the append ends. No state of a later crash may. |
//! | `TakenBack`, and `LogStore::settle` then returned `Ok` | the same, from the moment the settling returned. |
//! | `TakenBack` with a settling that failed, or `Unsettled` | may be in the log or not. The session publishes nothing more and the store is opened again, with no crash: it is then at the last point or at the item, and the model takes whichever the store has. A crash before that may leave either. Where no fault was injected, an item in doubt is a violation. |
//!
//! **When a sync fails**, nothing that was not durable becomes durable in
//! that session, so the floor that the store was asked for and the one
//! that the log reports stay where they were, and the commits after them
//! are points that a crash may or may not keep. What the files hold can
//! have moved all the same: a sync of the log's files that fails at the
//! second has made the first durable, and the state with no unsynced
//! change shows it. The changes that the
//! failed sync left unknown are kept, lost or drawn by the file layer once
//! a later sync of the same file succeeds (`SimConfig::unknown`). A store
//! that keeps its rules makes no such sync, so the three settings recover
//! the same states; a store that broke the rule would report a commit
//! durable that the setting `Lost` then takes away, and land below the
//! floor. The runs take the three settings by turns
//! ([`campaign::unknown_of_seed`]).
//!
//! A store that is opened again with no crash has lost nothing: it is at
//! the last point, or at the item in doubt.
//!
//! # What a violation is
//!
//! | [`check::Kind`] | What was found |
//! | --- | --- |
//! | `NotRecovered` | A state that a crash left did not open, with no fault injected into the recovery. |
//! | `Inconsistent` | It opened, and is not at one with itself: an id without a page, a value that is not read, counts that are not the pointers, a file that the manifest does not name, or a finding of the engine's invariant checker. |
//! | `NotCommitted` | It is at no point it may be at: below the floor, above the last, or with pages of no point. |
//! | `NotTheSame` | One state in a few is recovered a second time with another tuning, goes on until everything is written back and a table checkpoint covers it, is closed and is opened again; and in a run with faults, one in four is recovered once more with the faults injected. Each time it is at the same point, with the same pages. |
//! | `Session` | A store that was not crashed says other than the model or is not sound to the checker, a call failed that no fault excuses, the log left an item in doubt with no fault, or a sync was asked through a handle of a file to which another handle had written since the file was made or since its last sync that was answered. |
//! | `Panicked` | The store panicked over a state a crash left, or the run did: it is reported with its seed like the others. |
//!
//! # Faults
//!
//! Two runs in four inject no fault into a call: a call that fails there
//! is a violation, but for what a write-back that failed leaves (below).
//! One in four fails syncs, and one in four fails every kind of
//! operation, leaves appends torn, fills the device for a while, and has
//! the workload fail the next write or the next removal of a file, so that
//! a file the store has dropped stays behind wherever the store removes
//! one: at a replacement of the manifest, at the settling of a manifest in
//! doubt, at a close ([`campaign::Faults`], [`workload::Op::FailRemoval`]). In those an error is counted and the session
//! goes on, as the callers of the store do: a commit that failed is not a
//! point, write-back is asked again, and a log that cannot say what it
//! holds ends the session.
//!
//! **A write-back that fails** is no call that fails (`lignin_io::sim`, "A
//! write-back that fails"). By default in the runs of the odd seeds, which are one of
//! the two that inject no fault into a call and the one that injects
//! every kind ([`campaign::write_backs_of_seed`]), the system's own write
//! of a file fails now and then, after a write to the file. What storage has of the file's unsynced
//! changes is settled then, as the run's setting for unknown changes says
//! ([`campaign::unknown_of_seed`]): no later sync covers them, and no
//! crash plan chooses among them. Reads go on returning what was written.
//! The file layer tells the handles that made those changes, each by the
//! next sync through it, and no other handle: the least its contract
//! promises (`lignin_io::sim::Told::Writers`). So:
//!
//! - A store that syncs a file through the handle that wrote it meets a
//!   sync that failed, which is a fault like any other of this section.
//! - A store that syncs the file through another handle is told nothing
//!   there, and can be answered `Ok` over bytes that storage does not
//!   have. It then names them, in a manifest or in what a table
//!   checkpoint points to, and a crash can leave a state that does not
//!   recover, is not at one with itself or is at no point it may be at.
//!   Two planted defects are that store, for what is appended to a
//!   segment and to a value-log file.
//! - From the failure on, storage can lack bytes that the session still
//!   reads. The plan that keeps every unsynced change then need not leave
//!   the files as the session sees them. The observer keeps what the
//!   session saw at each boundary ([`observe::Taken::seen`]), and that
//!   plan's state is held to the last point or the item that is being
//!   appended, as what a killed process leaves, only where it is the
//!   same. Elsewhere it is held to what every other state is. That lasts
//!   until a recovery has returned the store with no write-back failing
//!   under it, which has made durable what the store reads of its files.
//!   A store that is opened again with no crash is at the last point or
//!   at that item all the same: it reads what was written.
//!
//! **In the sessions of a lineage, every sync of a file is held to the
//! handle that wrote**, whether or not a write-back fails in the run. The store's rule is that
//! a file's changes are synced through the handle that made them
//! (`docs/lignindb/sync-handles.md`), because the file layer's contract
//! has a sync through any other handle prove nothing of them once a
//! write-back has failed. The observer knows which of its handles wrote
//! to a file since the file was made or since its last sync that was
//! answered, and a sync that is asked through a handle while another one
//! has written so is a violation
//! ([`observe::Observer::take_foreign_sync`], which says what counts as
//! writing). The recoveries of crash
//! states ([`check`]) are made on the simulator with no observer between,
//! and are not held to this. A state that goes on as a lineage of its own
//! is, from its first session.
//!
//! **An error is a fault's** in three cases, and a violation in every
//! other ([`observe::Observer::lasting_faults`]):
//!
//! - A fault was injected into the operation that failed: a sync that is
//!   told of a write-back that failed counts as one.
//! - Since the store was opened, a sync failed of a file of the log or of
//!   a segment. A failed sync cannot be made good: the log makes nothing
//!   more durable in the session, and a segment keeps every table
//!   checkpoint from being installed (`lignin_store::wal`, "When the file
//!   layer fails", and `lignin_store::flush`, "When something fails").
//! - An append to the log failed earlier in the session, and the log had
//!   not been rolled when the operation began: until it is, it takes no
//!   item (`transactions.md` CM9 item 5).
//!
//! What else a fault leaves, the store gets over in the session: a file
//! whose append or creation failed is given up for another, and a
//! deletion is made again. A manifest whose replacement failed before its
//! rename is still in force. One whose replacement failed in the rename
//! or in the sync of the directory is in doubt: the next call that needs
//! the manifest opens it again and finds the old one or the new one,
//! whole (`lignin_store::segment`, "When the file layer fails").
//! The store that a recovery returns owes nothing to the faults before
//! it. A store that says it is damaged where only faults were injected is
//! a violation as well.
//!
//! The states a crash leaves of those runs are recovered with no fault and
//! held to the same rule. One in four is then recovered once more, on a
//! copy of its own, with the faults injected (`check::check_with_faults`).
//! A recovery is a function of the files it finds: one that the faults do
//! not fail returns the store that the recovery with no fault returned,
//! at the same version with the same pages; and one that they fail says
//! that the file layer failed, and is followed by a recovery with no
//! fault that returns that store (`lignin_store::recovery`, "Twice").
//!
//! # The campaign's own acceptance test
//!
//! `lignin-crash planted` plants defects and requires each to be found
//! ([`cli::DEFECTS`]): a file layer that reports a sync it did not make,
//! of the log, of a segment, of a value-log file, of a table checkpoint,
//! of the manifest and of a directory, a model that is wrong by one
//! hash, and a file layer that makes the syncs of what is appended to a
//! segment, or to a value-log file, through a second handle beside the
//! one that appends, in the sessions of the lineages of runs whose
//! write-backs fail. A defect counts as found by a violation of the kind it makes
//! ([`cli::Defect`]), in a run in which it did what it is planted to do
//! ([`campaign::ACTED`]), of a seed that gives no violation with no defect
//! planted.
//!
//! # What is not in it
//!
//! - **The owners of values that the tree layer has.** The cleaner
//!   (`lignin_store::cleaner`) is run, over the segments and over the
//!   value log, as operations of the workload
//!   ([`store::Session::clean_segments`],
//!   [`store::Session::clean_values`]). Its pass over the value log asks
//!   the owners of values for each value it moves: here those are the
//!   session's own, which know the cells and entries the workload wrote.
//!   The owners of `lignin-tree`, which find a value's cell through a
//!   tree, are the engine's campaign's (VER-04).
//! - **Threads.** A run is one thread, so that a seed gives one run. The
//!   workload interleaves commits with passes, turns and checkpoints of
//!   write-back, never inside one. What write-back does beside a committer
//!   is `lignin-store`'s own tests on threads and VER-05's.
//! - **A write-back whose lost bytes the system forgets at once.** The
//!   simulator can have reads return what storage has from the moment a
//!   write-back fails (`lignin_io::sim::WriteBackOdds::forgotten`). A
//!   session can then read other bytes than it wrote before any call has
//!   failed, which the campaign would take for a store that says other
//!   than the model. Its write-backs fail with nothing forgotten.
//!   `lignin-store`'s own tests of a failed write-back run both ways
//!   (`segment::tests::write_back`, `values::tests::write_back`).
//! - **Damage.** A crash loses and tears what was not synced. A device
//!   that changes bytes that were durable is the security review's
//!   (VER-10) and the decoders' fuzzing (FMT-08).

pub mod campaign;
pub mod check;
pub mod cli;
#[cfg(grovedb_engine_seam)]
pub mod engine;
pub mod model;
pub mod observe;
pub mod store;
pub mod workload;
