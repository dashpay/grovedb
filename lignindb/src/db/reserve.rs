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
    /// How often the session released its reserve.
    pub releases: u64,
    /// The bytes those releases gave back.
    pub bytes_released: u64,
    /// Fills that returned with the reserve whole and durable: the open's,
    /// and those after a release.
    pub fills: u64,
    /// Fills that failed. Such a fill leaves no reserve.
    pub fills_failed: u64,
    /// What the last fill or release that failed said.
    pub last_error: Option<String>,
}

/// How far a session has come, by two of its counts: what a fill of the
/// reserve after a release waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Marks {
    /// Table checkpoints that were installed.
    pub(super) checkpoints: u64,
    /// Commits that were published.
    pub(super) published: u64,
}

/// Whether the session holds its reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// A fill returned with the reserve whole, and it was not released
    /// since.
    Held,
    /// The reserve is to be filled: it was released, or a fill failed.
    /// The marks are those of the first turn of the cleaner's loop that
    /// ended since the device last refused the session something for
    /// room, or those of the fill that failed if that came later. A fill
    /// is tried once both have moved on.
    Owed(Option<Marks>),
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
    /// for one caller at a time ([`Reserve`]).
    hold: Mutex<Hold>,
    /// Whether the reserve was released and the loops of the background
    /// work were not told of it yet.
    untold: AtomicBool,
    counts: Counts,
}

/// The reserve of a session, if it keeps one, with what the session knows
/// of it.
#[derive(Debug, Default)]
pub(super) struct SessionReserve(Option<Kept>);

impl SessionReserve {
    /// The reserve of `len` bytes of the database in `dir`, which is not
    /// filled yet, or none with a `len` of 0. Nothing is read or made.
    pub(super) fn new(vfs: Arc<dyn Vfs>, dir: &Path, len: u64) -> Self {
        Self((len > 0).then(|| Kept {
            reserve: Reserve::of(vfs, dir),
            len,
            hold: Mutex::new(Hold::Owed(None)),
            untold: AtomicBool::new(false),
            counts: Counts::default(),
        }))
    }

    /// Whether the session keeps a reserve.
    pub(super) fn kept(&self) -> bool {
        self.0.is_some()
    }

    /// The fill of an open. One that fails leaves the reserve to be
    /// filled as after a release.
    pub(super) fn fill_at_open(&self) {
        if let Some(kept) = &self.0 {
            kept.fill(&mut lock(&kept.hold), None);
        }
    }

    /// What the session does with an error it was given: if a device that
    /// is full caused it, a reserve that is held is released, and a fill
    /// of one that is owed waits for what follows this refusal. The second
    /// makes no operation on a file.
    pub(super) fn refused(&self, error: &(dyn Error + 'static)) {
        let Some(kept) = &self.0 else { return };
        if !out_of_space(error) {
            return;
        }
        let mut hold = lock(&kept.hold);
        if *hold != Hold::Held {
            kept.set(&mut hold, Hold::Owed(None));
            return;
        }
        match kept.reserve.release() {
            Ok(bytes) => {
                kept.counts.releases.fetch_add(1, Ordering::Relaxed);
                (kept.counts.bytes_released).fetch_add(bytes, Ordering::Relaxed);
                kept.set(&mut hold, Hold::Owed(None));
                kept.untold.store(true, Ordering::Release);
            }
            // The reserve is held still, and the next refusal for room
            // releases it.
            Err(error) => kept.failed("the release", &error),
        }
    }

    /// Whether the reserve was released since this was last asked: the
    /// loops of the background work are told of a release once
    /// ([`Shared::room_made`](super::store::Shared::room_made)).
    pub(super) fn take_release(&self) -> bool {
        let Some(kept) = &self.0 else { return false };
        kept.untold.swap(false, Ordering::AcqRel)
    }

    /// What follows a turn of the cleaner's loop, for a reserve that is to
    /// be filled. `at_rest` says whether the cleaner was at rest in the
    /// turn, and `marks` how far the session has come.
    ///
    /// The first turn that ends after a refusal for room notes the marks.
    /// A fill is tried at the end of a later turn in which the cleaner was
    /// at rest, once a commit was published and a table checkpoint was
    /// installed since the marks were noted, or since the last fill that
    /// failed.
    pub(super) fn turn_ended(&self, at_rest: bool, marks: Marks) {
        let Some(kept) = &self.0 else { return };
        let mut hold = lock(&kept.hold);
        let since = match *hold {
            Hold::Held => return,
            Hold::Owed(None) => {
                kept.set(&mut hold, Hold::Owed(Some(marks)));
                return;
            }
            Hold::Owed(Some(since)) => since,
        };
        let moved_on = marks.checkpoints > since.checkpoints && marks.published > since.published;
        if at_rest && moved_on {
            kept.fill(&mut hold, Some(marks));
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
    /// Fills the reserve, under the lock on what the session holds. After
    /// a fill that failed the reserve is owed from `marks` on.
    fn fill(&self, hold: &mut MutexGuard<'_, Hold>, marks: Option<Marks>) {
        match self.reserve.fill(self.len) {
            Ok(_) => {
                self.counts.fills.fetch_add(1, Ordering::Relaxed);
                self.set(hold, Hold::Held);
            }
            Err(error) => {
                self.counts.fills_failed.fetch_add(1, Ordering::Relaxed);
                self.failed("the fill", &error);
                self.set(hold, Hold::Owed(marks));
            }
        }
    }

    fn set(&self, hold: &mut MutexGuard<'_, Hold>, to: Hold) {
        **hold = to;
        (self.counts.held).store(to == Hold::Held, Ordering::Relaxed);
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
        reserve.turn_ended(at_rest, store.marks());
    }
}
