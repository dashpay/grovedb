//! The reserve of an open database: when the session gives its room back,
//! and when it takes it again (`docs/lignindb/disk-full-and-backup.md`,
//! DF13; [`crate::space`]).

use std::{
    error::Error,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use lignin_io::Vfs;
use lignin_store::cleaner::CleanerStats;

use super::store::{lock, Store};
use crate::space::{out_of_space, Reserve};

/// What a session has done with its reserve
/// ([`DbStats::reserve`](super::DbStats::reserve)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReserveStats {
    /// The bytes the session keeps as its reserve
    /// ([`Options::reserve`](crate::Options::reserve)). With 0 it keeps
    /// none, and every other count here is 0.
    pub len: u64,
    /// Whether the session holds its reserve: a fill of it returned with
    /// the reserve whole, and the session has not released it since.
    pub held: bool,
    /// How often the session released its reserve: one that it held, or
    /// what a fill that failed can have left of one.
    pub releases: u64,
    /// The bytes those releases gave back.
    pub bytes_released: u64,
    /// Fills that returned with the reserve whole and durable: the open's,
    /// and those after a release.
    pub fills: u64,
    /// Fills that failed. The session does not hold its reserve after
    /// one, whatever the fill left on the device. That can be the whole
    /// reserve under its name, where the device refused no more than to
    /// make the name durable: the next refusal for room releases it as it
    /// releases a reserve that is held.
    pub fills_failed: u64,
    /// What the last fill or release that failed said.
    pub last_error: Option<String>,
}

/// How far a session has come, by two of its counts: what a fill of the
/// reserve after a release waits for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Marks {
    /// Table checkpoints that were installed.
    pub(super) checkpoints: u64,
    /// Commits that were published.
    pub(super) published: u64,
}

/// What the session knows of its reserve.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Hold {
    /// Whether a fill returned with the reserve whole, and it was not
    /// released since.
    held: bool,
    /// Whether a file of the reserve can be on the device by this
    /// session's doing: it began a fill, and no release of its has
    /// returned since. That is so while the reserve is held, and after a
    /// fill or a release that failed.
    on_device: bool,
    /// How far the session had come when it was last told of a refusal
    /// for room, or when a fill of its last failed if that was later. A
    /// fill is tried once both counts have moved on from here.
    since: Marks,
}

#[derive(Debug, Default)]
struct Counts {
    /// Whether the reserve is held, for a reader that waits for no fill.
    held: AtomicBool,
    releases: AtomicU64,
    bytes_released: AtomicU64,
    fills: AtomicU64,
    fills_failed: AtomicU64,
    last_error: Mutex<Option<String>>,
}

/// The reserve a session keeps.
#[derive(Debug)]
struct Kept {
    reserve: Reserve,
    len: u64,
    /// Every call of the reserve is made under this lock: its calls are
    /// for one caller at a time ([`Reserve`]). The marks of the session
    /// are read under it as well, so a refusal and the end of a turn of
    /// the cleaner's loop come one after the other.
    hold: Mutex<Hold>,
    counts: Counts,
}

/// The reserve of a session, if it keeps one, with what the session knows
/// of it.
#[derive(Debug, Default)]
pub(super) struct SessionReserve(Option<Kept>);

impl SessionReserve {
    /// The reserve of `len` bytes of the database in `dir`, which this
    /// session has not filled yet, or none with a `len` of 0. Nothing is
    /// read or made.
    pub(super) fn new(vfs: Arc<dyn Vfs>, dir: &Path, len: u64) -> Self {
        Self((len > 0).then(|| Kept {
            reserve: Reserve::of(vfs, dir),
            len,
            hold: Mutex::default(),
            counts: Counts::default(),
        }))
    }

    /// Whether the session keeps a reserve.
    pub(super) fn kept(&self) -> bool {
        self.0.is_some()
    }

    /// The fill of an open. One that fails leaves the reserve to be
    /// filled as after a release. `marks` says how far the session has
    /// come.
    pub(super) fn fill_at_open(&self, marks: impl FnOnce() -> Marks) {
        if let Some(kept) = &self.0 {
            kept.fill(&mut lock(&kept.hold), marks);
        }
    }

    /// What the session does with an error it was given: if a device that
    /// is full caused it, the session gives back what it can have of its
    /// reserve on the device, and a fill waits for what follows this
    /// refusal. `marks` says how far the session has come, and is called
    /// under the lock of the reserve.
    ///
    /// Returns whether files of the reserve were taken off the device:
    /// the loops of the background work are told of that
    /// ([`Troubles::refused`](super::store::Troubles::refused)). With
    /// nothing of the reserve on the device the call makes no operation
    /// on a file.
    pub(super) fn refused(
        &self,
        error: &(dyn Error + 'static),
        marks: impl FnOnce() -> Marks,
    ) -> bool {
        let Some(kept) = &self.0 else { return false };
        if !out_of_space(error) {
            return false;
        }
        let mut hold = lock(&kept.hold);
        hold.since = marks();
        if !hold.on_device {
            return false;
        }
        match kept.reserve.release() {
            Ok(bytes) => {
                kept.counts.releases.fetch_add(1, Ordering::Relaxed);
                (kept.counts.bytes_released).fetch_add(bytes, Ordering::Relaxed);
                hold.on_device = false;
                kept.set(&mut hold, false);
                true
            }
            // What the session knows of its reserve is as it was, and
            // the next refusal for room tries the release again.
            Err(error) => {
                kept.failed("the release", &error);
                false
            }
        }
    }

    /// What follows a turn of the cleaner's loop, for a reserve that is
    /// to be filled. `at_rest` says whether the cleaner was at rest in the
    /// turn, and `marks` how far the session has come: it is called under
    /// the lock of the reserve.
    ///
    /// A fill is tried at the end of a turn in which the cleaner was at
    /// rest, once a commit was published and a table checkpoint was
    /// installed since the session was last told of a refusal for room,
    /// and since its last fill that failed.
    pub(super) fn turn_ended(&self, at_rest: bool, marks: impl Fn() -> Marks) {
        let Some(kept) = &self.0 else { return };
        let mut hold = lock(&kept.hold);
        if hold.held || !at_rest {
            return;
        }
        let now = marks();
        if now.checkpoints > hold.since.checkpoints && now.published > hold.since.published {
            kept.fill(&mut hold, marks);
        }
    }

    pub(super) fn stats(&self) -> ReserveStats {
        let Some(kept) = &self.0 else {
            return ReserveStats::default();
        };
        let counts = &kept.counts;
        let read = |count: &AtomicU64| count.load(Ordering::Relaxed);
        ReserveStats {
            len: kept.len,
            held: counts.held.load(Ordering::Relaxed),
            releases: read(&counts.releases),
            bytes_released: read(&counts.bytes_released),
            fills: read(&counts.fills),
            fills_failed: read(&counts.fills_failed),
            last_error: lock(&counts.last_error).clone(),
        }
    }
}

impl Kept {
    /// Fills the reserve, under the lock on what the session knows of it.
    /// After a fill that failed the reserve is to be filled from the
    /// marks on that are read then, and what the fill left on the device
    /// goes at the next refusal for room.
    fn fill(&self, hold: &mut MutexGuard<'_, Hold>, marks: impl FnOnce() -> Marks) {
        hold.on_device = true;
        match self.reserve.fill(self.len) {
            Ok(_) => {
                self.counts.fills.fetch_add(1, Ordering::Relaxed);
                self.set(hold, true);
            }
            Err(error) => {
                self.counts.fills_failed.fetch_add(1, Ordering::Relaxed);
                self.failed("the fill", &error);
                self.set(hold, false);
                hold.since = marks();
            }
        }
    }

    fn set(&self, hold: &mut MutexGuard<'_, Hold>, held: bool) {
        hold.held = held;
        self.counts.held.store(held, Ordering::Relaxed);
    }

    fn failed(&self, what: &str, error: &std::io::Error) {
        *lock(&self.counts.last_error) = Some(format!("{what} of the reserve: {error}"));
    }
}

/// What the cleaner's thread keeps from one turn of its loop to the next:
/// by it a turn in which the cleaner was at rest is told.
///
/// **The cleaner was at rest in a turn** if none of the cleaner's counts
/// moved in it but those of its passes, of its reads of the page table
/// and of the links it trimmed ([`CleanerStats`]): no pass of the turn
/// failed, and it copied no block and no value, sealed and closed no
/// file, retired and dropped none, and asked for no table checkpoint.
#[derive(Debug)]
pub(super) struct CleanerTurns {
    /// The cleaner's counts when the turn before ended.
    counted: CleanerStats,
}

/// The counts of the cleaner that a turn at rest leaves as they were.
pub(super) fn work(counted: &CleanerStats) -> CleanerStats {
    CleanerStats {
        passes: 0,
        scans: 0,
        links_trimmed: 0,
        bytes_trimmed: 0,
        value_passes: 0,
        ..*counted
    }
}

impl CleanerTurns {
    /// The state before the first turn of a loop.
    pub(super) fn begun(store: &Store) -> Self {
        Self {
            counted: store.cleaner.stats(),
        }
    }

    /// Called at the end of a turn of the cleaner's loop, when both of its
    /// passes were made.
    pub(super) fn ended(&mut self, store: &Store) {
        let reserve = &store.troubles.reserve;
        if !reserve.kept() {
            return;
        }
        let counted = store.cleaner.stats();
        let at_rest = work(&counted) == work(&self.counted);
        self.counted = counted;
        reserve.turn_ended(at_rest, || store.marks());
    }
}
