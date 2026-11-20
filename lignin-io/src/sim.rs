//! [`SimVfs`]: a file system in memory that crashes and fails on purpose.
//!
//! Storage code is tested on it. It keeps the [contract](crate::vfs) of the
//! real file system and passes the same conformance suite, and it adds what
//! an operating system cannot be asked for: a crash after a chosen operation,
//! every state the contract lets that crash leave, and `ENOSPC` and `EIO` at
//! chosen operations. Everything it decides comes from one seed.
//!
//! ```
//! use lignin_io::{
//!     sim::{CrashPoint, SimConfig, SimError, SimVfs},
//!     Create, OpenMode, Vfs,
//! };
//! use std::path::Path;
//!
//! let fs = SimVfs::new(SimConfig::new(42));
//! let log = fs.open(Path::new("/log"), OpenMode::Append(Create::New))?;
//! fs.sync_dir(Path::new("/"))?;
//! log.append(b"durable")?;
//! log.sync_data()?;
//!
//! // The power fails after one more operation.
//! fs.crash_at(CrashPoint::AfterOp(fs.stats().ops + 1));
//! log.append(b" and not")?;
//! assert!(SimError::is_power_loss(&log.sync_data().unwrap_err()));
//!
//! // What is left is up to the crash: here, only what was synced.
//! let image = fs.crash_image();
//! let after = image.recover(&image.lose_all()).unwrap();
//! let log = after.open(Path::new("/log"), OpenMode::Read)?;
//! assert_eq!(log.len()?, 7);
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # What a crash leaves
//!
//! The simulator keeps two versions of everything: what operations see, and
//! what is durable. Between them lie the unsynced changes, each recorded as
//! the operation made it. A crash state is the durable version with some of
//! those changes put in. The rules are the ones under
//! [Durability](crate::vfs#durability), and each has its place here:
//!
//! | The contract says | The simulator does |
//! | --- | --- |
//! | A process that exits loses nothing. | Dropping every handle and lock changes nothing in the file system: open it again and all of it is there. |
//! | A lock is not durable. | No crash state has a guard: every lock file of a [`CrashImage::recover`]ed file system is free. Locking a file that is there records no change, with [`Vfs::lock`] or with [`Vfs::lock_existing`], and [`Vfs::lock`] records one [`NameChange`] when it makes the file. [`VfsLock::is_at`] is no operation: it is not counted, no fault falls on it, and with the power off it fails as every call does. |
//! | Synced content is durable. | A sync that returns `Ok` moves the file's unsynced changes into its durable version. No crash plan can take them out. |
//! | Any subset of the unsynced changes of a file survives. | Every `write_at`, `append` and `set_len` is one [`FileChange`], and a [`CrashPlan`] keeps any of them. |
//! | In any order. | A plan lists the surviving changes in the order they are applied, which is free. |
//! | A write survives possibly in part. | A surviving write can carry a [`Tear`]: any of the sectors it touches survive, each whole. The sector size is [`SimConfig::sector_size`], 512 bytes unless set; with a sector size of 1 a write tears at any byte. |
//! | Each change lands where it was made. | A change is recorded with its offset. An append is recorded as a write at the offset it returned. |
//! | A length can survive without the bytes below it, which then read as zeros. | A [`Tear`] keeps the length a write gave the file apart from its bytes. |
//! | Nothing else is damaged. | A crash state is built from the durable bytes and the surviving sectors of the surviving writes, and nothing else. |
//! | A change of names is durable once its directory is synced. | Creating, linking, removing and renaming a file, and creating, renaming and removing a directory, is one [`NameChange`]. `sync_dir` makes the changes of its directory durable, and a plan keeps any of the others, each with the changes it builds on (see below). They are always applied in the order they were made. |
//! | A removed or renamed name can come back. | A plan that loses a removal or a rename leaves the old name in place, with the file's durable content. |
//! | A crash keeps a change of names only with the earlier changes it builds on. | A plan keeps the changes its own build on, listed or not: [`CrashImage::names_kept`] says which. A sync makes them durable together with the change that builds on them. |
//! | Syncing a file does not sync its name, nor the other way round. | Names and content are kept or lost separately. A file whose name is lost is gone with all its content, and a name can survive for a file that is empty or stale. A directory whose own name is lost is gone with everything in it, synced or not. |
//! | A rename is atomic, synced or not. | A rename is a single [`NameChange`], of a file and of a directory: a plan keeps it or loses it, and no state has half of it. It is durable once the directories of both names are synced. A directory that is renamed takes what is in it along as it is: its names and the content of its files stay as durable or as unsynced as they were. |
//! | A set synced together is the syncs of its list, and a crash may find any of them made. | [`Vfs::sync_together`] is the method the trait provides: `sync_data`, `sync_all` and `sync_dir`, one after another in the order of the list, each an operation with its own number. A crash point can fall after each, a fault on each, and what a crash image, a plan or a fingerprint sees between two of them is the first ones made and the others not. The states in which a later one was made without an earlier one are the plans of the image before the first. |
//! | A failed sync cannot be made good. | When a sync fails, the file's unsynced changes are marked [unknown](FileChange::unknown). A crash keeps any of them, like other unsynced changes. A later sync that succeeds covers the changes made after the failure, and settles the unknown ones as [`SimConfig::unknown`] says, by default drawn from the seed: kept, torn or lost, as an [`Event::Settled`]. Reads keep seeing them either way. |
//!
//! ## Changes of names that build on each other
//!
//! The contract lets a crash keep a subset of the unsynced changes of names
//! only if each change in it comes with the earlier changes it builds on.
//! Without that, a rename could be left half done. Rename `a` to `b`, then
//! create a new `a`: a crash that kept the creation and lost the rename would
//! leave the new file at `a` and nothing at `b`, and the renamed file under
//! neither name. So a plan keeps a change of names together with what the
//! contract says it builds on:
//!
//! - the earlier unsynced changes of the names it touches,
//! - for the removal of a directory, and for a rename that replaces an empty
//!   one, the changes that emptied it,
//! - for a rename, what gave the directories its two names are in, and every
//!   directory above them, the names they had when it was made: the creation
//!   of each, or its last rename.
//!
//! The last keeps a directory from being lost to its own renames. Move `a`
//! into `b`, move it out again, then move `b` into `a`: a crash that kept the
//! first and the third rename and lost the second would leave each directory
//! inside the other, and neither under the root.
//!
//! [`CrashImage::names_kept`] gives what a plan keeps in all. A sync follows
//! the same rule: a change that becomes durable makes the changes it builds
//! on durable with it. That can be sooner than their own directories were
//! synced, which the contract allows, and it is never later. Everything else
//! is as loose as the contract: a file created and synced in a new directory
//! is gone if the directory's own name was not synced, and a file is lost if
//! a crash keeps the removal of one of its names and loses the hard link
//! that gave it another.
//!
//! Two more things follow from the model that are easy to miss. A write
//! that overwrites the end of a file and grows it can survive as its bytes
//! without its growth, so it ends where the file ended, not at a sector
//! boundary: the sector was written whole, and the length is kept elsewhere.
//! And once every directory and every file has been synced, with no sync
//! failing, nothing is unsynced: [`lose_all`](CrashImage::lose_all) then
//! gives the file system as it is.
//!
//! # Crashing
//!
//! A crash is taken in three steps.
//!
//! 1. **Stop.** [`SimVfs::crash_at`] sets the point at which the power goes
//!    off: after the n-th operation, the n-th sync, or the n-th operation
//!    that changed what a crash can leave. That operation still takes place.
//!    Every later one, on any handle, fails with [`SimError::PowerLoss`] and
//!    changes nothing, so the code under test unwinds on errors. A crash
//!    inside an operation is not a separate case: an unsynced write may be
//!    lost or torn whether or not its call had returned.
//! 2. **Freeze.** [`SimVfs::crash_image`] gives the [`CrashImage`]: the
//!    durable state and the list of unsynced changes.
//! 3. **Choose.** A [`CrashPlan`] says which changes survive.
//!    [`CrashImage::recover`] gives that state as a new [`SimVfs`], on which
//!    recovery is run and checked.
//!
//! Steps 2 and 3 do not disturb the file system they are taken from, and
//! they work with the power on as well. A harness can therefore run a
//! workload once and take crash states after every operation, by wrapping
//! the [`SimVfs`] in a [`Vfs`] of its own that calls
//! [`crash_image`](SimVfs::crash_image) between calls. A file the plan does
//! not touch is shared with the image, not copied.
//!
//! Plans come from the image:
//!
//! - [`lose_all`](CrashImage::lose_all) and
//!   [`keep_all`](CrashImage::keep_all) are the two ends, and
//!   [`keep_through`](CrashImage::keep_through) cuts the sequence at an
//!   operation.
//! - [`prefixes`](CrashImage::prefixes) lists what a file system that writes
//!   in order leaves, with the first lost write torn at each sector boundary.
//!   [`holes`](CrashImage::holes) loses one change at a time, with the
//!   changes of names that build on it, and
//!   [`reorders`](CrashImage::reorders) applies one change late.
//! - [`every_plan`](CrashImage::every_plan) lists them all, for the small
//!   cases where that is possible.
//! - [`sample`](CrashImage::sample) draws a plan from the seed: each change
//!   lost, torn or moved with the [`CrashOdds`] of the configuration.
//!   [`SimVfs::crash`] does steps 2 and 3 with a sampled plan.
//! - Any other state the contract allows is a plan written by hand from
//!   [`CrashImage::changes`], [`CrashImage::names`] and
//!   [`CrashImage::tears`].
//!
//! Different plans often give the same state.
//! [`SimVfs::fingerprint`] tells two states apart without reading them.
//!
//! # Faults
//!
//! A [`Fault`] is injected only into an operation that would otherwise
//! succeed, so an injected error always stands for a call that could have
//! worked. A read, a write or an append of no bytes never fails: it asks
//! nothing of the device. The error says what it is: [`SimError::of`] finds
//! the simulator's part of an [`io::Error`].
//!
//! - **At a chosen operation.** A [`FaultRule`] fails the operations it
//!   names: one operation by its number, the n-th sync of one file, or
//!   every operation that needs space from some point on, which is a full
//!   disk until [`SimVfs::clear_rules`] gives the space back.
//! - **By chance.** [`FaultOdds`] gives each operation a chance of `EIO` and
//!   of `ENOSPC`.
//!
//! What a failed operation leaves is what the contract allows it to leave:
//!
//! | Operation | After an injected fault |
//! | --- | --- |
//! | `write_at` | Some of the write's sectors were written, each whole: its first few, or any of them, from none to all. |
//! | `append` | Some sectors were written and taken back: the file has the length it had, and a crash can still bring the bytes back. Or, with the chance [`FaultOdds::torn_append`], they stay and the handle appends no more. |
//! | `sync_data`, `sync_all` | The file's unsynced changes are unknown from then on, and the handle syncs no more. |
//! | `sync_dir` | Nothing became durable. A later `sync_dir` can still make it so. |
//! | `sync_together` | What the fault leaves of the sync it fell on, as in the two rows above. The syncs before it in the list were made, the ones after it were not, and the call returns the error. |
//! | Every other one | Nothing changed. |
//!
//! # The same seed, the same schedule
//!
//! The schedule is every fault the simulator injects, with what it left, and
//! every crash state it samples. It is a function of the
//! [configuration](SimConfig), seed included, and of the sequence of calls
//! made to the simulator, and of nothing else: not the clock, not an address,
//! not the order of a hash map, not the width of `usize`, and no dependency.
//! The random numbers are [`SimRng`], which is SplitMix64 written out in this
//! crate.
//!
//! Each decision is drawn from a generator of its own, derived from the seed
//! and the number of the operation it belongs to. Whether operation 57 fails
//! does not depend on what was drawn for operation 56.
//!
//! Operations are numbered from 1 in the order the simulator takes them, and
//! it takes one at a time: every call runs from start to end under one lock.
//! With one thread calling, the program fixes that order, and the seed alone
//! reproduces the schedule. With several threads, the scheduler decides
//! which call comes first, and so which number each gets. The schedule is
//! then the same whenever the calls arrive in the same order, which is all
//! that can be said: a test that wants the same schedule on every run has to
//! make its threads take turns. A [`FaultRule`] with a path counts the
//! operations on that path alone, so it keeps hitting the same call for as
//! long as one thread works on the file.
//!
//! # The trace
//!
//! [`SimVfs::trace`] is the record of what was injected: every operation
//! that was failed, with what the failure left, the settling of changes a
//! failed sync left unknown, the power going off, and, as the first
//! [`Event`] of a recovered file system, the [`CrashRecord`] of the plan
//! that made it. With [`SimConfig::trace_ops`] it records every operation.
//! Events print as one line each. [`SimVfs::trace_digest`] is a number that
//! is the same for the same trace on every machine, so two runs whose
//! digests differ did not make the same schedule.
//!
//! # Limits
//!
//! Exclusion between writers and the lock work within one [`SimVfs`]: there
//! is no second process. Paths are taken from the root of the simulated file
//! system, with or without a leading `/`, and must not contain `..`. There
//! are no symbolic links. The trace and the fault rules know a path as text,
//! so two names that differ only in bytes that are not UTF-8 look the same
//! to them, and they know a handle by the path it was opened by, also after
//! a directory above the file was renamed. The space on the device is not
//! counted: it is full when a rule says so.

mod crash;
mod fault;
mod rng;
mod state;
mod trace;

#[cfg(test)]
mod tests;

use std::{
    fmt, io,
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use self::state::{ArmedRule, FileId, State};
pub use self::{
    crash::{
        CrashImage, CrashOdds, CrashPlan, FileChange, FileChangeKind, InvalidPlan, NameChange,
        NameChangeKind, Survivor, Tear,
    },
    fault::{Fault, FaultOdds, FaultRule, OpKind, OpSet, SimError},
    rng::{Chance, SimRng},
    trace::{digest, ChangeFate, CrashRecord, Effect, Event, Fate, NameFate, OpRecord, Outcome},
};
use crate::vfs::{DirEntry, OpenMode, Vfs, VfsFile, VfsLock};

/// How a [`SimVfs`] behaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimConfig {
    /// The seed every fault and every sampled crash state is drawn from.
    pub seed: u64,
    /// The sector size in bytes. A sector starts where the offset in the file
    /// is a multiple of it, and a write tears only between the sectors it
    /// touches: of each, the part the write covers survives whole or not at
    /// all.
    pub sector_size: u64,
    /// The longest a file may get, at most 2 GiB less one byte, which is the
    /// most a 32-bit machine holds. An operation that would grow a file past
    /// it fails with [`FileTooLarge`](io::ErrorKind::FileTooLarge), before
    /// the simulator runs out of memory.
    pub max_file_len: u64,
    /// The faults injected by chance.
    pub faults: FaultOdds,
    /// How [`CrashImage::sample`] draws a crash state.
    pub crash: CrashOdds,
    /// What becomes of the changes a failed sync left unknown, once a later
    /// sync of the file succeeds.
    pub unknown: UnknownFate,
    /// Whether the trace records every operation, not only the failed ones.
    pub trace_ops: bool,
}

impl SimConfig {
    /// The configuration with `seed`, sectors of 512 bytes, files of up to
    /// 1 GiB, no faults, the default [`CrashOdds`], unknown changes settled
    /// by the first draw from the seed, and a trace of injected events only.
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            sector_size: 512,
            max_file_len: 1 << 30,
            faults: FaultOdds::NONE,
            crash: CrashOdds::default(),
            unknown: UnknownFate::Drawn(0),
            trace_ops: false,
        }
    }
}

/// What becomes of the changes a failed sync left unknown, once a later sync
/// of the file succeeds.
///
/// Until then a crash keeps or loses them like any unsynced change, and a
/// [`CrashPlan`] chooses. The later sync covers only what came after the
/// failure, so from then on the unknown changes are settled one way, and the
/// crash plans of that file system no longer choose. To see the other ways,
/// run the same calls again with another value here: nothing else in the
/// schedule depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownFate {
    /// Each one is lost, torn, kept or applied out of turn, as drawn from
    /// the seed with the [`CrashOdds`]. The number picks one of the draws
    /// the seed gives.
    Drawn(u64),
    /// All of them are lost.
    Lost,
    /// All of them are kept, whole and in order.
    Kept,
}

/// When the power goes off. Counts start with the file system and include
/// the operations that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    /// After this many operations.
    AfterOp(u64),
    /// After this many calls of `sync_data`, `sync_all` and `sync_dir`.
    AfterSync(u64),
    /// After this many operations that changed what a crash can leave: an
    /// operation that made an unsynced change, or a sync that made at least
    /// one durable. These are the points at which a crash is worth taking.
    AfterChange(u64),
}

/// Counts of what a [`SimVfs`] has done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimStats {
    /// Operations made: the number of the last one.
    pub ops: u64,
    /// Calls of `sync_data`, `sync_all` and `sync_dir` among them.
    pub syncs: u64,
    /// Operations that changed what a crash can leave.
    pub changes: u64,
    /// Operations the simulator failed.
    pub faults: u64,
}

/// The fault-injecting [`Vfs`] in memory.
///
/// A clone is another handle on the same file system. See the
/// [module documentation](self).
#[derive(Clone)]
pub struct SimVfs {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SimVfs {
    /// An empty file system: a root directory with nothing in it.
    ///
    /// # Panics
    ///
    /// If the sector size is 0, or the longest file is longer than 2 GiB
    /// less one byte.
    pub fn new(config: SimConfig) -> Self {
        Self::from_state(State::new(config))
    }

    fn from_state(state: State) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
            }),
        }
    }

    /// The configuration, with the fault odds as they are now.
    pub fn config(&self) -> SimConfig {
        self.shared.state().config.clone()
    }

    /// Replaces the faults injected by chance. [`FaultOdds::NONE`] switches
    /// them off.
    pub fn set_faults(&self, odds: FaultOdds) {
        self.shared.state().config.faults = odds;
    }

    /// Adds a fault at chosen operations. Rules are asked in the order they
    /// were added and the first whose turn it is decides; the operation
    /// still counts for the others. Faults by chance are drawn only for an
    /// operation no rule failed.
    pub fn add_rule(&self, rule: FaultRule) {
        self.shared.state().rules.push(ArmedRule::new(rule));
    }

    /// Removes every rule: the space is back, the device works again.
    pub fn clear_rules(&self) {
        self.shared.state().rules.clear();
    }

    /// Turns the power off now.
    pub fn power_off(&self) {
        self.shared.state().power_off();
    }

    /// Sets the point at which the power goes off, in place of an earlier
    /// one. If the point is already reached, it goes off now.
    pub fn crash_at(&self, point: CrashPoint) {
        let mut state = self.shared.state();
        if !state.powered_off {
            state.crash_point = Some(point);
            state.check_crash_point();
        }
    }

    /// Whether the power is off: every operation fails with
    /// [`SimError::PowerLoss`].
    pub fn is_powered_off(&self) -> bool {
        self.shared.state().powered_off
    }

    /// Counts of what the file system has done.
    pub fn stats(&self) -> SimStats {
        self.shared.state().stats
    }

    /// The record of what was injected so far.
    pub fn trace(&self) -> Vec<Event> {
        self.shared.state().trace.clone()
    }

    /// The [`digest`] of the trace.
    pub fn trace_digest(&self) -> u64 {
        digest(&self.shared.state().trace)
    }

    /// A number that is equal for two file systems in which operations see
    /// the same thing: the same names, the same content, and the same names
    /// sharing a file. It is the same on every machine.
    pub fn fingerprint(&self) -> u64 {
        self.shared.state().fingerprint()
    }

    /// Freezes what a crash at this moment would have to work with. The file
    /// system itself goes on unchanged.
    pub fn crash_image(&self) -> CrashImage {
        self.shared.state().image()
    }

    /// What a crash at this moment leaves under the plan the seed gives for
    /// `draw`: [`crash_image`](Self::crash_image),
    /// [`sample`](CrashImage::sample) and [`recover`](CrashImage::recover) in
    /// one.
    pub fn crash(&self, draw: u64) -> Self {
        let image = self.crash_image();
        image
            .recover(&image.sample(draw))
            .expect("a sampled plan fits its image")
    }
}

impl fmt::Debug for SimVfs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.shared.state();
        f.debug_struct("SimVfs")
            .field("seed", &state.config.seed)
            .field("ops", &state.stats.ops)
            .field("powered_off", &state.powered_off)
            .finish_non_exhaustive()
    }
}

impl Vfs for SimVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let handle = self.shared.state().open(path, mode)?;
        Ok(Box::new(SimFile {
            shared: self.shared.clone(),
            handle,
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.shared.state().rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.shared.state().hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.shared.state().remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.shared.state().create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.shared.state().remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.shared.state().read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.shared.state().sync_dir(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        let file = self.shared.state().lock(path, true)?;
        Ok(Box::new(SimLock {
            shared: self.shared.clone(),
            file,
        }))
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        let file = self.shared.state().lock(path, false)?;
        Ok(Box::new(SimLock {
            shared: self.shared.clone(),
            file,
        }))
    }
}

struct SimFile {
    shared: Arc<Shared>,
    handle: u64,
}

impl fmt::Debug for SimFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SimFile({})", self.handle)
    }
}

impl VfsFile for SimFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.shared.state().read_at(self.handle, buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.shared.state().write_at(self.handle, buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.shared.state().append(self.handle, buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.shared.state().sync(self.handle, OpKind::SyncData)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.shared.state().sync(self.handle, OpKind::SyncAll)
    }

    fn len(&self) -> io::Result<u64> {
        self.shared.state().len(self.handle)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.shared.state().set_len(self.handle, len)
    }
}

impl Drop for SimFile {
    fn drop(&mut self) {
        self.shared.state().close(self.handle);
    }
}

struct SimLock {
    shared: Arc<Shared>,
    file: FileId,
}

impl fmt::Debug for SimLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SimLock({})", self.file.0)
    }
}

impl VfsLock for SimLock {
    fn is_at(&self, path: &Path) -> io::Result<bool> {
        self.shared.state().is_at(path, self.file)
    }
}

impl Drop for SimLock {
    fn drop(&mut self) {
        self.shared.state().unlock(self.file);
    }
}
