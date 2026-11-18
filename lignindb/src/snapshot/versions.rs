//! The versions of a database: the latest published one, who pins which,
//! and the clock of retirements with the guards that are held against it.

use std::fmt;

use lignin_format::CommitVersion;
use lignin_store::alloc::{Epoch, Pins};

use super::{
    error::PublishError,
    slots::{Slot, Slots},
    sync::{atomic, AtomicBool, AtomicU64, Ordering, Padded},
};

/// What is asked to reclaim when the oldest pin may have moved up: the page
/// id allocator, the buffer pool, and whatever else holds something back
/// for readers (`transactions.md` PN6, ID3).
///
/// [`Versions::reclaim`] calls it, one call at a time, on the thread of a
/// committer or of a reader that dropped a pin. So it does what is cheap,
/// and hands anything that waits or does file I/O to background work.
pub trait Reclaim: Send + Sync {
    /// Asks every reclaimer to take its step. Each of them puts the three
    /// questions to `pins` in their order (`page-alloc.md` A9).
    fn reclaim(&self, pins: &Versions);
}

/// The versions of one open database. See the
/// [module documentation](super).
///
/// Every method takes `&self` and may be called from any thread. None takes
/// a lock or waits for another thread.
pub struct Versions {
    latest: Padded<AtomicU64>,
    epoch: Padded<AtomicU64>,
    pins: Slots,
    guards: Slots,
    reclaim: Option<Box<dyn Reclaim>>,
    reclaim_wanted: Padded<AtomicBool>,
    reclaim_running: Padded<AtomicBool>,
}

impl fmt::Debug for Versions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Versions")
            .field("latest", &self.latest())
            .field("oldest_pin", &self.oldest_pin())
            .field("pins", &self.pins_held())
            .field("epoch", &self.epoch())
            .field("guards", &self.guards_held())
            .finish_non_exhaustive()
    }
}

impl Versions {
    /// The versions of a database whose latest committed version is
    /// `latest`: [`CommitVersion::EMPTY`] for a new one, and otherwise the
    /// version recovery landed on. Nothing is pinned, and nobody is asked
    /// to reclaim.
    pub fn new(latest: CommitVersion) -> Self {
        Self::build(latest, None)
    }

    /// As [`Self::new`], with `reclaim` to ask whenever the oldest pin may
    /// have moved up ([`Self::reclaim`]).
    pub fn with_reclaim(latest: CommitVersion, reclaim: Box<dyn Reclaim>) -> Self {
        Self::build(latest, Some(reclaim))
    }

    fn build(latest: CommitVersion, reclaim: Option<Box<dyn Reclaim>>) -> Self {
        Self {
            latest: Padded(atomic(latest.get())),
            epoch: Padded(atomic(0)),
            pins: Slots::new(),
            guards: Slots::new(),
            reclaim,
            reclaim_wanted: Padded(AtomicBool::new(false)),
            reclaim_running: Padded(AtomicBool::new(false)),
        }
    }

    /// The latest published version, *L*.
    ///
    /// This is for those who do not read pages by it: `flush()` for its
    /// target (DU1a), a committer under the commit lock. Whoever reads a
    /// page at the latest version reads it through its guard
    /// ([`OperationGuard::latest`]), so that the guard is taken first (PN5).
    pub fn latest(&self) -> CommitVersion {
        version(self.latest.0.load(Ordering::SeqCst))
    }

    /// Publishes the commit of `version` (CM3 step 8): from here on a new
    /// pin is on `version`, and a pin taken before sees nothing of it.
    ///
    /// The caller holds the commit lock, has appended the commit's log
    /// group and has installed its page versions. `version` is the one
    /// after the latest (CV1), and anything else is refused and changes
    /// nothing ([`PublishError::NotNext`]). The caller asks for a reclaim
    /// ([`Self::reclaim`]) once it has given the commit lock back.
    pub fn publish(&self, version: CommitVersion) -> Result<(), PublishError> {
        let latest = self.latest.0.load(Ordering::SeqCst);
        let published = Some(version) == self::version(latest).next()
            && self
                .latest
                .0
                .compare_exchange(latest, version.get(), Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
        if published {
            Ok(())
        } else {
            Err(PublishError::NotNext {
                latest: self.latest(),
                version,
            })
        }
    }

    /// Pins the latest version (PN1, PN3). The pin is held until it is
    /// dropped, by any thread.
    ///
    /// It takes no lock and cannot lose a race with reclamation (PN4). The
    /// version is announced where [`Pins::oldest_pin`] looks, and the pin
    /// is given out only once the announced version has been found to be
    /// the latest still. If a commit was published in between, the newer
    /// version is announced and looked at again.
    pub fn pin(&self) -> VersionPin<'_> {
        let mut pinned = self.latest.0.load(Ordering::SeqCst);
        let slot = self.pins.claim(pinned);
        loop {
            let latest = self.latest.0.load(Ordering::SeqCst);
            if latest == pinned {
                break;
            }
            pinned = latest;
            slot.announce(pinned);
        }
        VersionPin {
            versions: self,
            slot,
            version: version(pinned),
        }
    }

    /// Takes an operation guard (PN5). An operation takes it before it
    /// reads the latest version or a page id, and holds it for as long as
    /// it uses what it read.
    ///
    /// The guard is of the epoch that is current when it is given out, by
    /// the same two steps as a pin: announce, then look again.
    pub fn guard(&self) -> OperationGuard<'_> {
        let mut taken = self.epoch.0.load(Ordering::SeqCst);
        let slot = self.guards.claim(taken);
        loop {
            let epoch = self.epoch.0.load(Ordering::SeqCst);
            if epoch == taken {
                break;
            }
            taken = epoch;
            slot.announce(taken);
        }
        OperationGuard {
            versions: self,
            slot,
            epoch: Epoch::new(taken),
        }
    }

    /// The current epoch of the clock of retirements.
    pub fn epoch(&self) -> Epoch {
        Epoch::new(self.epoch.0.load(Ordering::SeqCst))
    }

    /// How many version pins are held, each counted at the moment the count
    /// passes it.
    pub fn pins_held(&self) -> usize {
        self.pins.claimed()
    }

    /// How many operation guards are held, counted the same way.
    pub fn guards_held(&self) -> usize {
        self.guards.claimed()
    }

    /// Asks the [`Reclaim`] this was made with to reclaim, if there is one.
    ///
    /// Call it after a commit is published, with the commit lock given
    /// back. A pin that is dropped on a version before the latest calls it
    /// itself. No call is owed when a guard is released (`page-alloc.md`
    /// N8).
    ///
    /// One reclaim runs at a time. A call that finds one running leaves
    /// word and returns, and the thread that runs it starts another once it
    /// is done: a reclaim always starts after the last call that asked.
    pub fn reclaim(&self) {
        let Some(reclaim) = &self.reclaim else {
            return;
        };
        if !self.reclaim_wanted.0.load(Ordering::SeqCst) {
            self.reclaim_wanted.0.store(true, Ordering::SeqCst);
        }
        while !self.reclaim_running.0.load(Ordering::SeqCst)
            && self
                .reclaim_running
                .0
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            if self.reclaim_wanted.0.swap(false, Ordering::SeqCst) {
                reclaim.reclaim(self);
            }
            self.reclaim_running.0.store(false, Ordering::SeqCst);
            if !self.reclaim_wanted.0.load(Ordering::SeqCst) {
                break;
            }
        }
    }
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("only versions are stored and announced")
}

/// The three questions a reclaimer asks (`transactions.md` PN6,
/// `page-alloc.md` N1 to N3).
///
/// Each answer is read from what is the case when it is asked, and stays a
/// true lower bound afterwards: the latest version and the epoch only go
/// up, and a pin or a guard that is given out later has looked at them
/// after it announced itself.
impl Pins for Versions {
    fn oldest_pin(&self) -> CommitVersion {
        let latest = self.latest.0.load(Ordering::SeqCst);
        version(latest.min(self.pins.min()))
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.epoch.0.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn oldest_guard(&self) -> Epoch {
        let epoch = self.epoch.0.load(Ordering::SeqCst);
        Epoch::new(epoch.min(self.guards.min()))
    }
}

/// A version pin (`transactions.md` PN1): for as long as it lives, every
/// page version that is visible at its version stays readable, and no page
/// id that is reachable there names another page (PN2).
///
/// It never blocks anything and is never refused (PN7). Held for long, it
/// costs disk space, page ids and memory that could be reclaimed.
pub struct VersionPin<'v> {
    versions: &'v Versions,
    slot: Slot<'v>,
    version: CommitVersion,
}

impl<'v> VersionPin<'v> {
    /// The pinned version: the latest one at the moment the pin was taken.
    pub fn version(&self) -> CommitVersion {
        self.version
    }

    /// The versions the pin is held in.
    pub fn versions(&self) -> &'v Versions {
        self.versions
    }
}

impl fmt::Debug for VersionPin<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VersionPin").field(&self.version).finish()
    }
}

impl Drop for VersionPin<'_> {
    fn drop(&mut self) {
        self.slot.vacate();
        if self.version < self.versions.latest() {
            self.versions.reclaim();
        }
    }
}

/// An operation guard (`transactions.md` PN5): for as long as it lives,
/// nothing that was retired while it was held is reclaimed, so every page
/// id, page-table entry and file location the operation read stays good.
pub struct OperationGuard<'v> {
    versions: &'v Versions,
    slot: Slot<'v>,
    epoch: Epoch,
}

impl OperationGuard<'_> {
    /// The epoch the guard was taken in.
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// The latest published version, read under the guard. An operation
    /// that holds no version pin reads its pages at this version (RD4).
    pub fn latest(&self) -> CommitVersion {
        self.versions.latest()
    }
}

/// A reader of nodes borrows the guard, so that no page or node it read
/// outlives it.
impl lignin_tree::node::OperationGuard for OperationGuard<'_> {}

impl fmt::Debug for OperationGuard<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OperationGuard").field(&self.epoch).finish()
    }
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.slot.vacate();
    }
}
