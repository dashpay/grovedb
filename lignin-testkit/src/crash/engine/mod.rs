//! Engine crash campaign (VER-04): GroveDB on the LigninDB engine is run
//! through random workloads with the three threads of each database at
//! work, crashed at every write boundary, and every state that the
//! campaign's crash plans leave is opened and held to the blocks that were
//! committed.
//!
//! It is the campaign of the page store ([`crate::crash`], VER-03) one
//! level up, and takes from it what a write boundary, a crash image, a
//! crash plan and a crash point are, the plans it picks of an image
//! ([`crate::crash::campaign::RunShape`]), of which this campaign takes
//! at most twenty-four unless it is told another number
//! ([`campaign::chosen`]), and the kinds of a violation
//! ([`crate::crash::check::Kind`]). What is its own is said here.
//!
//! ```
//! use lignin_testkit::crash::engine::campaign::{campaign, Settings, Shape};
//!
//! // One run: a lineage of a few steps, with a crash image at every write
//! // boundary and three crash plans of each.
//! let settings = Settings {
//!     seed: 1,
//!     shape: Shape {
//!         ops: 6,
//!         lineages: 1,
//!         max_plans: 3,
//!         again_one_in: 0,
//!         ..Shape::default()
//!     },
//!     ..Settings::default()
//! };
//! let report = campaign(&settings, &mut |_, _| {});
//! assert!(report.clean(), "{report}");
//! assert!(report.points > 0);
//! ```
//!
//! The commands are those of `lignin-crash engine` ([`cli::USAGE`]):
//!
//! ```text
//! cargo run --release -p lignin-testkit --bin lignin-crash -- engine run --points 1000000 --jobs 6
//! cargo run --release -p lignin-testkit --bin lignin-crash -- engine wipe --runs 8
//! cargo run --release -p lignin-testkit --bin lignin-crash -- engine planted
//! ```
//!
//! # What is run
//!
//! | Part | What it is |
//! | --- | --- |
//! | [`grove`] | A database of the campaign: `GroveDb` on `LigninEngine` on `lignindb::Db`, opened as a node opens it, with its three threads (write-back, the reclaimer, the cleaner) and thresholds under which a workload of sixty operations turns every kind of file. |
//! | [`workload`] | What a lineage does: the operations of the workload generators ([`crate::workload`]), and between them flushes, checkpoints of the database and their deletion, wipes, writes with no transaction beside an open block, closes and kills. |
//! | [`model`] | The blocks: what each commit published, and where a crash may leave the database. |
//! | [`observe::Watcher`] | The file layer of a session: it takes a crash image after every operation of any of the four threads that changed what a crash can leave, with the thread that made it. |
//! | [`check`] | One state a crash left, opened and held to the blocks. |
//! | [`campaign`] | Runs, their lineages, and what is counted. |
//!
//! **Transactions and savepoints** are the streams': a stream is blocks,
//! each a write transaction that is committed, rolled back or abandoned,
//! with savepoints that are set and rolled back to and with reads through
//! the block and through a snapshot. **Checkpoints** are made through one
//! of two calls, which a draw of the lineage's steps picks for each:
//! GroveDB's own (`GroveDb::create_checkpoint`), or the facade of the
//! engine (`Db::checkpoint`), which takes how much log write-back leaves
//! to the checkpoint's first open. They are opened as
//! `GroveDb::open_checkpoint` opens one, with an open that makes no
//! database, and deleted as `GroveDb::delete_checkpoint` deletes one, with
//! `lignindb::checkpoint::delete`: both on the campaign's file layer,
//! where GroveDB's two calls take a path of the machine's. **The cleaner**
//! is the third thread of every database of a lineage, as write-back and
//! the reclaimer are the first two, each with its own loop.
//!
//! # What a committed block is
//!
//! A **block** is what one operation that commits published
//! ([`model::Block`]): the commit of the write transaction of a block of
//! the stream, a write with no transaction, a wipe, and the flush of the
//! drops that are pending, which commits by itself and more than once. A
//! block has the version of the engine at which the database holds it, the
//! root hash of the grove, and every record that a walk from the root tree
//! reaches ([`crate::diff::state::stored`]): the nodes of every Merk as
//! Merk encodes them, with their hashes and links, the records of the
//! append-only trees, the aux and meta records, and what a subtree that is
//! no longer reached left behind.
//!
//! **What a block holds is said by a second database, which is never
//! crashed.** Each session has one beside it: when the session is opened,
//! the files it is opened on are copied as a process that ended leaves
//! them, and the copy is opened on a file system of its own, with no
//! watcher and no fault. It takes every operation of the stream that the
//! session's database takes, every write beside a block and every wipe;
//! no checkpoint is made of it and it is not flushed (`GroveDb::flush`).
//! Before the database commits a block, the other one is read through the
//! transaction that is about to commit there, so the block that a crash
//! may leave the database at is known before the database's commit
//! begins. A wipe has no transaction to read through and needs none: it
//! publishes the empty grove. A flush of pending drops has none either.
//! There the files of the other database are copied once more and the
//! copy is flushed first, under a watcher of its own, which says what the
//! database holds after each commit of the flush
//! ([`campaign::flushed_on_a_copy`]); then the other database is flushed,
//! and has to come to the same by the same versions.
//!
//! Of the database that is crashed the campaign takes whether each call
//! succeeded, the counts that a flush of the pending drops reports, the
//! version it is at, whether it still takes writes, the counts of its
//! background work, and the root hash of its open block, which has to be
//! the other one's: nothing of what a block holds. The two are the same
//! engine: what the campaign holds a crash state to is what a session with
//! no crash committed, and not what another engine would have. That an
//! engine publishes the right block is the differential fuzzer's to hold
//! ([`crate::diff`]).
//!
//! # What "the recovered state" is, and when it is a committed block
//!
//! A state that a crash left is opened as a node opens its database: with
//! the engine's open, which makes a database where the directory holds
//! none, and GroveDB on it ([`check::check_state`]). Then:
//!
//! - **It is at a block.** The version the engine opened at is that of a
//!   block of the lineage, and the root hash of the grove and every record
//!   the walk reaches are that block's. The walk is told every subtree
//!   that a walk of the lineage ever reached, so a record that a later
//!   block wrote, and that the crash took back with the block, is found
//!   where it was left.
//! - **Not before the floor.** No state is at a block before the last one
//!   that was made durable: by a flush that returned `Ok`, by the creation
//!   of a checkpoint that returned `Ok`, by a commit that returned `Ok` in
//!   a session whose commits sync the log, by a close of a session into
//!   whose file system no fault was injected, and by an open, which makes
//!   the log durable to its end. And none is below the version that the
//!   state with no unsynced change recovered to, of the same image or of
//!   an earlier one of the same file system: what the durable part of the
//!   files holds is lost by no later crash. That version can be the one of
//!   the block that is being made, or one inside a flush of the pending
//!   drops ([`model::Expect::durable`]). And inside such a flush in a
//!   session whose commits sync the log, each commit of the flush is
//!   durable when the next is appended: the state with no unsynced change
//!   of an image is one version behind the state with every change at
//!   most.
//! - **A crash that loses nothing loses no block.** The plan that keeps
//!   every unsynced change, whole and in order, leaves what a process that
//!   is killed leaves: that state is at the last block, or at the one that
//!   is being made. A session that is killed is followed by
//!   an open, also where the lineage has no step left, and that open finds
//!   the database there. The kill itself makes nothing durable: the
//!   process is stopped and its file system stays as it is
//!   ([`observe::Watcher::kill`]), so a crash between the kill and the
//!   return of that open may leave any block from the floor on.
//! - **Not after the last block**, but for the one that is being made: a
//!   state may be at the block whose commit was under way when the image
//!   was taken.
//! - **Inside a flush of the pending drops**, which commits more than
//!   once, a state is at a version between two blocks. It holds what the
//!   copy of the files of the database that is never crashed held at that
//!   version of its flush ([`model::Block::inside`]). It is after the
//!   block before it and before the block itself. No other block of the
//!   campaign is more than one commit, and a state at a version inside
//!   one is at no block.
//! - **It is sound.** The engine's physical invariant checker
//!   (`lignindb::verify`) finds no rule broken, beside the threads of the
//!   open database (`Db::verify`) and after a close and a second open
//!   (`Db::close_and_verify`), with the stored hashes of the nodes of
//!   every Merk in use held to Merk's rules for a check
//!   (`MerkHashRules::for_check`): each is made again and compared, but
//!   for the value hashes that those rules take as they are stored. The
//!   second open is at the same version.
//! - **Its checkpoints are whole.** A checkpoint whose creation returned
//!   `Ok` opens as a database of its own at the version of the block it was
//!   made at, holds that block's root hash and records, and is sound to the
//!   checker. One whose creation a crash can have cut short is whole in
//!   that way or holds no database. One whose deletion is under way, or
//!   failed, is whole, marked as being removed, or holds no database. One
//!   whose deletion returned `Ok` has no directory: the removal is durable
//!   (`lignindb::checkpoint::delete`). And where a checkpoint is whole,
//!   the database has the block it holds. A lineage that begins with a
//!   state that a crash left has the checkpoints of that state: one that
//!   is whole in it as one that was made, one whose deletion had returned
//!   as one that is gone, and of one that the crash cut short what the
//!   state has of it, no directory, one that holds no database or one
//!   that is marked, which no later state of the lineage has otherwise
//!   ([`model::Standing::Left`]).
//! - **It goes on.** One state in sixteen is opened once more with the
//!   thresholds of a session, takes a block and a flush, is closed and
//!   opened again: it is at the version after the one it recovered to, with
//!   the root hash that the block gave it, and sound. The same is done to
//!   each of its checkpoints that is whole, each with a block of its own.
//!   None changes another: each is found, when its turn comes, at the
//!   version and with the records it was recovered with.
//!
//! **A state that was recovered before** in its lineage, the same files
//! by `SimVfs::fingerprint`, is not opened again when a later crash leaves
//! it. What it held is held to what is expected then: the version has to
//! be one that is allowed then, the root hash and the records those that
//! the model has at that version then, and its checkpoints as their
//! standing asks then. Where the lineage has reached subtrees since the
//! state was walked, it is opened and walked again.
//!
//! **A wipe is a block like any other**: the empty grove, which is the
//! root hash of a grove with no element, no record that a walk lists and
//! no mark ([`campaign::not_empty`]). A database that was just made holds
//! that, or the run does not begin. So a state of a crash during a wipe
//! is at the block before the wipe or at the empty grove, or, where
//! blocks before the wipe were not durable, at one of those. The lineages
//! of `lignin-crash engine wipe` follow a script ([`campaign::WIPES`])
//! that writes an item before a wipe, so that the grove holds something
//! whatever the streams have committed, and flushes before some of its
//! wipes: of a crash during a wipe that follows a flush only the first
//! two are left. What the command counts is of the wipes of a grove that
//! holds something.
//!
//! # What a seed gives
//!
//! The streams of a run are a function of its seed and of its shape
//! ([`campaign::Shape`], of which the script of the steps is a part). In
//! the first lineage of a run that injects no fault and finds no
//! violation, the operations, their order and the blocks they commit
//! follow from the seed and the shape alone
//! ([`campaign::RunReport::digest`]). **The crash states do not**, nor
//! what follows from them. A database has three threads beside the one
//! that calls it, and where their changes to the files fall among the
//! caller's is the machine's to say. So two runs of one seed can recover
//! other states and count other numbers of them; a fault fails whichever
//! thread makes the next call of its kind, and the lineage goes on by what
//! that failed, so its operations from then on can be others; and a later
//! lineage begins with a state that a crash left, so its operations can
//! be others too. A violation need therefore not be made again by its
//! seed. For one that is found in a crash state, [`campaign::Found`]
//! holds what was expected when its image was taken, the thread that made
//! the change and the crash plan, and, where a directory is named for
//! them ([`campaign::Settings::save`]), the place of the files of that
//! state, two names of one file as two names of one file, or why they
//! could not be written. One that is found of a session, and in no crash
//! state, has no files.
//!
//! **The steps come in bursts.** During a burst the three threads work
//! beside the operations of the workload. Between two bursts the watcher is
//! paused ([`observe::Watcher::pause`]): a thread that is about to change a
//! file waits, and the campaign goes through the images of the burst. So
//! the background work of a database falls beside its commits and not into
//! the pauses in which the campaign recovers crash states.
//!
//! **What a call answered is held from the image that follows the
//! answer.** An image of a change is taken with what was expected while
//! the call ran that made it. Where the answer of a call changes what is
//! expected of a crash, the campaign takes one more image, of the files
//! as they are when it has the answer and the watcher's lock, with what
//! is expected from then on ([`observe::Watcher::acknowledge`]): after a
//! commit, a write beside a block or a wipe that published or was
//! refused, a flush, a flush of the pending drops that committed, the
//! creation or the deletion of a checkpoint, an open and a close. A
//! commit that is left in doubt changes nothing that is expected, and has
//! no such image. A later call of the workload cannot make good, before
//! that image, what the call did not do. Another thread of the database
//! can: a change that it makes between the return of the call and the
//! image is in the image. Where that change made good what the call had
//! answered for and not done, the files as the call left them are held to
//! what was expected before the answer, in the image of the change before,
//! and in no image to what is expected from the answer on. A report counts
//! the images of an answer that follow a change of another thread: the
//! others are of the files as the call left them.
//!
//! # What a violation is
//!
//! The kinds are the store campaign's ([`crate::crash::check::Kind`]):
//!
//! | Kind | What was found |
//! | --- | --- |
//! | `NotRecovered` | A state that a crash left did not open, or a checkpoint that was made holds no database in it. |
//! | `NotCommitted` | It opened, and is at no block it may be at: at a version that no block has, below the floor or the version that was durable, above the last block, or at a block whose root hash or records it does not hold. The same of a checkpoint, a database that has lost the block of a checkpoint that is whole, and a checkpoint whose deletion had returned and whose directory is there. |
//! | `Inconsistent` | It opened at a block, and its records cannot be read, or the engine's checker finds a rule broken, in the database or in a checkpoint. |
//! | `NotTheSame` | A second open is at another version, or a state that went on is not where its block left it. |
//! | `Session` | A database that was not crashed went wrong: an operation succeeded in it and failed in the database that is never crashed, or the other way round, a root hash differs from that one's before a commit, a commit was refused for a conflict in one of the two and not in the other, or was refused for a conflict and made a version in either, a commit left the two at different versions, a version was made by an operation that is no commit, a flush of the pending drops did not take the database that is never crashed and a copy of its files to the same by the same versions, or failed, or reported other counts than in the database, an open after a close or a kill is not at the last block or at a commit that was left in doubt, or, with no fault injected into the session, a commit failed otherwise than for a conflict, a flush, the making or the deletion of a checkpoint, a wipe or the read of a root hash failed, or the background work had counted an error or the session took no write any more when the campaign looked: after each burst, before a close and before a kill. What the background work counts after the last look of a session is not seen. |
//! | `Panicked` | The engine panicked, over a state a crash left or in a session, or the run did. In a debug build the engine's own check at a close is a panic. A background thread of the engine that panicked is known by the panic hook of the process ([`observe::engine_panics`]): where more than one run is under way at a time, each run whose session was open then reports it. |
//!
//! # Faults
//!
//! Two runs in four inject no fault: a failure of the engine there is a
//! violation (the row `Session` above says which). A refusal that the
//! workload asks for is none, in any run: an operation of a stream that
//! both databases refuse, and the commit of a block that both refuse for
//! a conflict with a write beside it, after which the block is made
//! again. One
//! in four fails syncs, and one in four fails every kind of operation that
//! changes a file or a name and fills the device for a few writes
//! ([`crate::crash::campaign::Faults::of_seed`]). A fault is a step of the
//! workload ([`workload::Planted`]): it fails the next call of its kind,
//! **whichever of the four threads of the database makes it**, and the
//! campaign counts which thread each fault met. So the cleaner, write-back
//! and the reclaimer meet faults through the facade, in sessions that go
//! on and are crashed.
//!
//! **A failure that is no such refusal is a fault's when a fault was
//! injected into the file system of the session since its open began**,
//! and a violation in a session with none. That is wider than the store campaign's rule, which
//! names the operation a fault went into: here another thread can have
//! made the call that the fault failed, and what its failure owes the
//! caller is the engine's to say ("A session that takes no write any
//! more" in `lignindb::db`). The model follows what the engine says became
//! of a commit:
//!
//! | The commit answered | and the database | The block |
//! | --- | --- | --- |
//! | `Ok` | is at its version | is a point, and durable if the session's commits sync the log. |
//! | `Ok` | is at the version before | is no point: the transaction wrote nothing, and its commit publishes nothing. The database that is never crashed has to be at the version before too. |
//! | an error | is at its version | is a point: it is published, and its sync failed. |
//! | an error | is at the version before, and takes writes | is no point: it is not in the log, and no state of a later crash may be at it. |
//! | an error | is at the version before, and takes no write any more, and the call changed no file | is no point: a commit is put into the log by the thread that makes it, so nothing of it is there. The session is killed, and no state of a crash from then on may be at it. |
//! | an error | is at the version before, and takes no write any more, and the call changed a file | may be in the log or not. The session is killed, and the open after it, with no crash between, finds the database at the last block or at this one: the model takes whichever it has. A crash before that may leave either. |
//!
//! An operation of a block that a fault fails leaves a block of which the
//! campaign does not know what it holds: the block is given up in both
//! databases and made again from its beginning. A session that takes no
//! write any more is killed and opened again, as its caller is told to.
//! The states that a crash leaves of these runs are opened with no fault
//! and held to the same rules.
//!
//! **A flush of the pending drops is made where no fault can meet it.**
//! It commits more than once, and what it leaves where a fault cuts it
//! short the model does not say: a look at a record that fails has the
//! flush pass over that record. So before such a flush the faults that
//! were planted and not met are taken back, and a session into which a
//! fault was injected is killed in the place of the flush. The next
//! session begins with no fault, and its stream goes on at the flush,
//! where the same holds: the flush is made in a session into which no
//! fault was injected when the lineage comes to it, and is not made
//! otherwise. A flush that fails, or that does not do in the database
//! what it did in the one that is never crashed, is a violation.
//!
//! # A database with the mark of a restore that did not finish
//!
//! A database that state sync left with its mark (`GroveDb::has_incomplete_restore`)
//! says of itself that its elements commit to Merks that were not
//! restored, and the engine's checker reports that by design. No workload
//! of the campaign restores, so no block has the mark, and the mark is
//! part of what a state is held to ([`model::State::incomplete_restore`]):
//! a state that a crash left with the mark is at no block, and is reported
//! as `NotCommitted` before the checker is asked.
//!
//! How a session of state sync leaves the mark where one of its commits
//! fails, where the process stops, and where a commit is published and
//! answered with an error is held on the databases of the campaign by the
//! tests of `tests/engine_restore_mark.rs`.
//!
//! # The campaign's own acceptance test
//!
//! `lignin-crash engine planted` plants defects and requires each to be
//! found ([`cli::DEFECTS`]): a file layer that reports a sync it did not
//! make, of the log, of a segment, of a table checkpoint, of the
//! database's manifest,
//! of the database directory and of the directory the checkpoints are made
//! in; a model that misses a write; and a wipe that is made as two commits.
//! A defect counts as found by the first violation that its campaign
//! reports and that fits it ([`cli::Defect::found_by`]): one of a kind the
//! defect makes, in a lineage in which the defect acted, and as its act
//! there asks ([`campaign::Acted`]). For a missed write that is one of
//! the differences between the two databases
//! ([`campaign::Found::differs`]) while the block is open of which the
//! write was missed; for a split wipe, a state of a crash during a wipe whose
//! database is at the version that the first of the wipe's two commits
//! made; for a sync that was not made, a crash state of an image that was
//! taken after the file layer first lied, other than the state of that
//! image in which every unsynced change is kept
//! ([`campaign::leaves_all`]). The run of that violation's seed with no
//! defect planted (the control, which one call of the command makes once
//! for a seed) has to have found no violation: where it found one, the
//! defect counts as not found.
//!
//! That a violation fits is evidence that the defect made it, and no
//! proof. The campaign does not work out what the files would hold had
//! every sync been made, and a crash that loses a change can leave what a
//! file layer leaves too that made them all. So a violation of another
//! cause that fits, in a run whose control finds nothing, is taken for
//! the defect's.
//!
//! One more defect is planted by a test and not by the command: a thread
//! with the name of the engine's cleaner that panics while a session is
//! open ([`campaign::Planted::ThreadPanic`]). The panics of the engine's
//! threads are counted for the process, so the test has a process of its
//! own (`tests/engine_thread_panic.rs`).
//!
//! # What is not in it
//!
//! - **A recovery with faults injected.** The store's campaign recovers
//!   one state in four once more with its faults injected into the
//!   recovery; here every state that a crash left is opened with none. A
//!   session that is opened after a close does meet a fault that was
//!   planted before, and that open is made again.
//! - **A close that answers.** A database is closed as a node closes it,
//!   by the drop of GroveDB's handle, which answers nothing: GroveDB has no
//!   close on this engine that returns the engine's error. A close that a
//!   fault can have failed raises no floor, a panic of the caller's thread
//!   in a close is caught, and a panic of a background thread is known by
//!   the panic hook of the process.
//! - **A flush of the pending drops that a fault fails.** It is made in
//!   sessions into which no fault was injected ("Faults", above), so no
//!   crash state is of a flush that a fault cut short.
//! - **The conversion of a tree in place**, which the engine does not have
//!   yet ([`workload::features`]). The streams are generated without it.
//! - **Appends on grove versions 1 to 3.** A run on one of them takes no
//!   stream that appends to a tree, for a known defect of the engine
//!   there ([`workload::left_out`]). On grove versions 4 and 5 the trees
//!   that keep their data flat are in the streams.
//! - **A restore by state sync.** No lineage restores a database, so none
//!   is crashed during a restore at every write boundary. One session is
//!   stopped at one moment by the tests of the mark, above.
//! - **More than one thread that commits.** The workload is one thread, as
//!   a node's is in iteration one. What several committers do to each
//!   other is the concurrency verification's (VER-05).
//! - **Every state a crash can leave.** Of an image with more than 48
//!   plans the store's campaign picks some, and of what it picks this
//!   campaign takes at most twenty-four unless it is told another number
//!   ([`campaign::chosen`]): an image here can have hundreds of unsynced
//!   changes, of four threads.
//! - **The reserve for a full device.** The databases of the campaign are
//!   opened with none (`Options::reserve` is 0): no run has a session
//!   that gives a reserve back or takes it again.
//! - **Damage.** A crash loses and tears what was not synced. A device that
//!   changes bytes that were durable is the security review's (VER-10).

use grovedb::{engine::lignin::LigninEngine, Transaction};

use crate::workload::StreamEngine;

pub mod campaign;
pub mod check;
pub mod cli;
pub mod grove;
pub mod model;
pub mod observe;
pub mod workload;

/// The LigninDB engine takes the streams of the workload generators.
impl StreamEngine for LigninEngine {
    fn is_snapshot_read(transaction: &Transaction<'_, Self>) -> bool {
        transaction.is_snapshot_read()
    }
}
