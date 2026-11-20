//! What the commits of one open database share.

use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard, PoisonError,
    },
};

use lignin_format::CommitVersion;
use lignin_store::flush::{FlushError, Published};

use super::error::Stopped;
use crate::snapshot::Versions;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a commit keeps from one commit to the next, under the commit lock.
#[derive(Debug, Default)]
pub(super) struct Scratch {
    /// The bytes of the group that is being written.
    pub(super) group: Vec<u8>,
}

/// Counts of what the commits of a session did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommitStats {
    /// Commits that were published.
    pub published: u64,
    /// Commits of an empty write set, which took no version (CV2).
    pub empty: u64,
    /// Commits that failed with a conflict (CF2).
    pub conflicts: u64,
    /// Commits that were not made: their group is not in the log.
    pub failed: u64,
    /// Commits that were published and whose sync failed.
    pub not_durable: u64,
    /// The bytes of the groups that were appended.
    pub group_bytes: u64,
    /// Groups that were written again because the log no longer ended
    /// where they were written for (`log-format.md` LG22).
    pub rewritten: u64,
    /// Rollbacks that were made through the committer.
    pub rollbacks: u64,
    /// Waits for write-back, after a commit or a rollback, that ended with
    /// write-back still over its bounds
    /// ([`Room::Stalled`](lignin_store::flush::Room::Stalled)).
    pub stalls: u64,
    /// Errors of write-back in a turn that a committer made or waited for,
    /// after its commit or its rollback. They change no commit.
    pub write_back_errors: u64,
}

#[derive(Debug, Default)]
struct Counters {
    published: AtomicU64,
    empty: AtomicU64,
    conflicts: AtomicU64,
    failed: AtomicU64,
    not_durable: AtomicU64,
    group_bytes: AtomicU64,
    rewritten: AtomicU64,
    rollbacks: AtomicU64,
    stalls: AtomicU64,
    write_back_errors: AtomicU64,
}

/// What the commits of one open database share: the commit lock, whether
/// the session still takes commits, and counts.
///
/// It is made when the database is opened and lives beside its stores. A
/// [`Committer`](super::Committer) is a view of the stores and of this.
#[derive(Default)]
pub struct CommitState {
    /// The commit lock (`transactions.md` CM2, level 1 of LT7).
    commit: Mutex<Scratch>,
    stopped: Mutex<Option<Stopped>>,
    /// Whether a sync of the log that fails stops the session.
    ends_at_failed_sync: bool,
    counters: Counters,
    /// Whom an error of write-back in a committer's own turn is told.
    report: Option<Report>,
    /// What a test runs between the last step before a publication and
    /// the publication, with the commit lock held.
    #[cfg(test)]
    before_publication: Mutex<Option<Hook>>,
}

/// What a test runs before a version is published.
#[cfg(test)]
type Hook = Box<dyn Fn(CommitVersion) + Send + Sync>;

/// Whom an error of write-back is told.
type Report = Box<dyn Fn(&FlushError) + Send + Sync>;

/// The commit lock, held (`transactions.md` CM2): no commit is made and no
/// version is published until this is dropped. Beside a commit, the
/// sampling step of a checkpoint takes it (CK2), and holds it for that
/// step and no longer.
pub struct CommitLock<'a> {
    pub(super) scratch: MutexGuard<'a, Scratch>,
}

impl fmt::Debug for CommitLock<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommitLock").finish_non_exhaustive()
    }
}

impl CommitState {
    /// The state of a session that has made no commit.
    pub fn new() -> Self {
        Self::default()
    }

    /// As [`Self::new`], and `report` is told every error of write-back in
    /// a turn that a committer made or waited for after its commit or its
    /// rollback, as the caller of
    /// [`WriteBack::run`](lignin_store::flush::WriteBack::run) is told
    /// those of the background loop. Such an error changes no commit: it
    /// is counted, and without `report` that is all.
    pub fn reporting(report: impl Fn(&FlushError) + Send + Sync + 'static) -> Self {
        Self {
            report: Some(Box::new(report)),
            ..Self::default()
        }
    }

    /// As `self`, and the session stops at a sync of the log that fails
    /// ([`Stopped::NotDurable`]): the commit whose sync failed is
    /// published and reported [`NotDurable`], and every commit after it is
    /// refused with [`Stopped`], also one that was waiting for the commit
    /// lock. Without this the session goes on, and its caller ends it
    /// (see "A sync that failed stays failed for the session" in the
    /// [module documentation](super)).
    ///
    /// [`NotDurable`]: super::CommitError::NotDurable
    /// [`Stopped`]: super::CommitError::Stopped
    pub fn ending_at_a_failed_sync(mut self) -> Self {
        self.ends_at_failed_sync = true;
        self
    }

    /// Stops a session that ends at a failed sync: a sync of the log
    /// failed with `version` the latest. The caller holds the commit
    /// lock, so no commit is published after the sync that failed.
    pub(crate) fn sync_failed(&self, version: CommitVersion, error: &FlushError) {
        if self.ends_at_failed_sync {
            self.stop(Stopped::NotDurable {
                version,
                reason: error.to_string(),
            });
        }
    }

    /// Takes the commit lock (CM2). It waits for a commit that is being
    /// made, and for nothing else.
    pub fn lock(&self) -> CommitLock<'_> {
        CommitLock {
            scratch: lock(&self.commit),
        }
    }

    /// Why the session takes no commit any more, if it does not.
    pub fn stopped(&self) -> Option<Stopped> {
        lock(&self.stopped).clone()
    }

    /// Stops the session: from here on it takes no commit. The first
    /// reason stands, and is returned.
    pub(super) fn stop(&self, reason: Stopped) -> Stopped {
        lock(&self.stopped).get_or_insert(reason).clone()
    }

    /// The counts so far.
    pub fn stats(&self) -> CommitStats {
        let counters = &self.counters;
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        CommitStats {
            published: read(&counters.published),
            empty: read(&counters.empty),
            conflicts: read(&counters.conflicts),
            failed: read(&counters.failed),
            not_durable: read(&counters.not_durable),
            group_bytes: read(&counters.group_bytes),
            rewritten: read(&counters.rewritten),
            rollbacks: read(&counters.rollbacks),
            stalls: read(&counters.stalls),
            write_back_errors: read(&counters.write_back_errors),
        }
    }

    /// Has `hook` run before each version is published from here on, on
    /// the committing thread, which holds the commit lock: everything of
    /// the commit is done but the publication.
    #[cfg(test)]
    pub(crate) fn run_before_publication(
        &self,
        hook: impl Fn(CommitVersion) + Send + Sync + 'static,
    ) {
        *lock(&self.before_publication) = Some(Box::new(hook));
    }

    #[cfg(test)]
    pub(super) fn before_publication(&self, version: CommitVersion) {
        if let Some(hook) = &*lock(&self.before_publication) {
            hook(version);
        }
    }

    /// Counts an error of write-back and tells it to whoever asked.
    pub(super) fn write_back_failed(&self, error: &FlushError) {
        (self.counters.write_back_errors).fetch_add(1, Ordering::Relaxed);
        if let Some(report) = &self.report {
            report(error);
        }
    }

    pub(super) fn count(&self, what: Counted) {
        let counters = &self.counters;
        let counter = match what {
            Counted::Published { group_len } => {
                counters.group_bytes.fetch_add(group_len, Ordering::Relaxed);
                &counters.published
            }
            Counted::Empty => &counters.empty,
            Counted::Conflict => &counters.conflicts,
            Counted::Failed => &counters.failed,
            Counted::NotDurable => &counters.not_durable,
            Counted::Rewritten => &counters.rewritten,
            Counted::Rollback => &counters.rollbacks,
            Counted::Stall => &counters.stalls,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

impl fmt::Debug for CommitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommitState")
            .field("stopped", &self.stopped())
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// One thing a commit counts.
#[derive(Debug, Clone, Copy)]
pub(super) enum Counted {
    Published { group_len: u64 },
    Empty,
    Conflict,
    Failed,
    NotDurable,
    Rewritten,
    Rollback,
    Stall,
}

/// What write-back asks of whoever publishes commits and keeps the pins
/// and the guards (`lignin_store::flush`): the latest published version,
/// every commit up to which has installed its pages (CM3 steps 7 and 8),
/// and an operation guard for a piece of its work (PN5).
impl Published for Versions {
    fn latest(&self) -> CommitVersion {
        Versions::latest(self)
    }

    fn guarded(&self, work: &mut dyn FnMut()) {
        let _guard = self.guard();
        work()
    }
}
