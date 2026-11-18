//! The allocator: one lock around the state of every page id.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::{Mutex, MutexGuard, PoisonError},
};

use lignin_format::{CommitVersion, PageId};

use super::{error::AllocError, states::States};

/// A moment of the engine's clock of retirements (`transactions.md` PN6).
///
/// The clock belongs to whoever keeps the operation guards ([`Pins`]). The
/// allocator only compares two of its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(u64);

impl Epoch {
    /// The epoch with this number.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The epoch's number.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What the allocator asks of whoever keeps the version pins and the
/// operation guards: the two tests of `transactions.md` PN6, one counted in
/// commit versions and one in epochs.
///
/// A value that one of these methods returned stays true: the oldest pin
/// and the oldest guard never go down. The allocator calls them with its
/// own lock not held.
pub trait Pins {
    /// The oldest version a pin can reach, *H* of PN3. No version pin that
    /// is held now is on an earlier version, and none that is taken later
    /// will be. When nothing is pinned it is the latest published version.
    ///
    /// It is never above the version of the last commit that was published
    /// (CM3 step 8). A commit that is installed and not yet published does
    /// not count: a reader can still pin the version before it.
    fn oldest_pin(&self) -> CommitVersion;

    /// Starts the next epoch and returns it. Every operation guard that is
    /// held now was taken in an earlier epoch, and every guard that is
    /// taken from now on is taken in this epoch or a later one.
    ///
    /// An operation takes its guard before it reads the latest committed
    /// version, and holds it for as long as it uses a page id it read
    /// (PN5).
    fn next_epoch(&self) -> Epoch;

    /// The epoch of the oldest operation guard. No guard that is held now
    /// was taken in an earlier epoch, and none that is taken later will be.
    /// When no guard is held it is the current epoch.
    fn oldest_guard(&self) -> Epoch;
}

/// What a page id is to the allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageState {
    /// Nothing uses the id: [`PageAllocator::allocate`] may hand it out.
    Free,
    /// An open transaction holds the id. No commit has published a page
    /// under it, so nothing else has seen it.
    Private,
    /// The id names a committed page.
    Live,
    /// A commit freed the page, and the id waits until no reader can reach
    /// the page any more.
    Retired,
}

impl fmt::Display for PageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Free => "free",
            Self::Private => "private",
            Self::Live => "live",
            Self::Retired => "retired",
        })
    }
}

/// How many page ids are in each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Census {
    /// The highest id that has been in use, or `None` if none has. Every
    /// id above it is free.
    pub high_water: Option<PageId>,
    /// The ids that open transactions hold.
    pub private: u64,
    /// The ids that name a committed page.
    pub live: u64,
    /// The ids of freed pages that cannot be used again yet.
    pub retired: u64,
}

impl Census {
    /// The free ids at or below [`Self::high_water`].
    pub fn free_below_high_water(&self) -> u64 {
        let below = self.high_water.map_or(0, |page| u64::from(page.get()));
        below - self.private - self.live - self.retired
    }
}

/// Ids that no version pin can reach any more, and that wait for the
/// operation guards that were held when that was found.
#[derive(Clone)]
struct Draining {
    epoch: Epoch,
    freed_at: CommitVersion,
    pages: Vec<PageId>,
}

#[derive(Clone)]
struct Inner {
    states: States,
    /// The retired ids that a version pin may still reach, by the version
    /// that freed them.
    waiting: BTreeMap<CommitVersion, Vec<PageId>>,
    /// The retired ids that no version pin can reach, in the order of their
    /// epochs.
    draining: VecDeque<Draining>,
}

impl Inner {
    fn expect(
        &self,
        page: PageId,
        wanted: PageState,
        refuse: impl FnOnce(PageId, PageState) -> AllocError,
    ) -> Result<(), AllocError> {
        let state = self.states.get(page);
        if state == wanted {
            Ok(())
        } else {
            Err(refuse(page, state))
        }
    }

    fn reclaim(
        &mut self,
        oldest_pin: CommitVersion,
        epoch: Epoch,
        oldest_guard: Epoch,
        released: &mut Vec<PageId>,
    ) {
        while let Some(entry) = self.waiting.first_entry() {
            if *entry.key() > oldest_pin {
                break;
            }
            let (freed_at, pages) = entry.remove_entry();
            let at = self
                .draining
                .partition_point(|drained| drained.epoch <= epoch);
            self.draining.insert(
                at,
                Draining {
                    epoch,
                    freed_at,
                    pages,
                },
            );
        }
        while let Some(drained) = self
            .draining
            .pop_front_if(|drained| drained.epoch <= oldest_guard)
        {
            for page in drained.pages {
                self.states.set(page, PageState::Free);
                released.push(page);
            }
        }
    }
}

/// The page id allocator: which ids are in use, and which may be handed
/// out.
///
/// See [the module documentation](super) for the states of an id and the
/// rules on moving between them.
///
/// Every method takes `&self` and may be called from any thread. Each is
/// one step under the allocator's own lock, and nothing else is locked or
/// called while that lock is held.
pub struct PageAllocator {
    inner: Mutex<Inner>,
}

impl Default for PageAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PageAllocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let census = self.census();
        f.debug_struct("PageAllocator")
            .field("high_water", &census.high_water)
            .field("private", &census.private)
            .field("live", &census.live)
            .field("retired", &census.retired)
            .finish()
    }
}

impl PageAllocator {
    /// An allocator of a database with no page: every id is free.
    pub fn new() -> Self {
        Self::from_parts(States::new(PageId::MAX), BTreeMap::new())
    }

    pub(super) fn from_parts(
        states: States,
        waiting: BTreeMap<CommitVersion, Vec<PageId>>,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                states,
                waiting,
                draining: VecDeque::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands out the lowest free id. It is private to the caller's
    /// transaction until [`Self::publish`] or [`Self::release`].
    ///
    /// Fails with [`AllocError::Exhausted`] if no id is free.
    pub fn allocate(&self) -> Result<PageId, AllocError> {
        let mut inner = self.lock();
        let page = inner.states.lowest_free().ok_or(AllocError::Exhausted)?;
        inner.states.set(page, PageState::Private);
        Ok(page)
    }

    /// Takes back a private id that is not going to be committed: its
    /// transaction or scope rolled back, or emptied the page again before
    /// it committed. The id is free at once, because nothing else has seen
    /// it.
    ///
    /// Fails with [`AllocError::NotPrivate`] for an id in any other state.
    pub fn release(&self, page: PageId) -> Result<(), AllocError> {
        let mut inner = self.lock();
        inner.expect(page, PageState::Private, |page, state| {
            AllocError::NotPrivate { page, state }
        })?;
        inner.states.set(page, PageState::Free);
        Ok(())
    }

    /// Records that a commit installs a page under a private id. The id
    /// then names a committed page.
    ///
    /// Fails with [`AllocError::NotPrivate`] for an id in any other state.
    pub fn publish(&self, page: PageId) -> Result<(), AllocError> {
        let mut inner = self.lock();
        inner.expect(page, PageState::Private, |page, state| {
            AllocError::NotPrivate { page, state }
        })?;
        inner.states.set(page, PageState::Live);
        Ok(())
    }

    /// Retires the id of a committed page that was freed at the version
    /// `freed_at`: the page can be reached at versions below `freed_at`
    /// and at no other.
    ///
    /// A commit that frees a page calls this with its own version, before
    /// it is published. Whoever reclaims the pages of a dropped subtree
    /// calls it with the version of the drop, or with any later version up
    /// to the latest published one.
    ///
    /// The id must still name the page the caller means: the allocator
    /// cannot tell a page from a later one under the same id. A commit
    /// knows it once its conflict check has passed, and the pages of a
    /// dropped subtree are freed by its reclaimer alone.
    ///
    /// The id is not handed out again before [`Self::reclaim`] finds that
    /// no reader can reach the page.
    ///
    /// Fails with [`AllocError::NotLive`] for an id that does not name a
    /// committed page, and with [`AllocError::FreedByNoCommit`] for
    /// version 0.
    pub fn free(&self, page: PageId, freed_at: CommitVersion) -> Result<(), AllocError> {
        let mut inner = self.lock();
        inner.expect(page, PageState::Live, |page, state| AllocError::NotLive {
            page,
            state,
        })?;
        if freed_at == CommitVersion::EMPTY {
            return Err(AllocError::FreedByNoCommit { page });
        }
        inner.states.set(page, PageState::Retired);
        inner.waiting.entry(freed_at).or_default().push(page);
        Ok(())
    }

    /// Frees every retired id that no reader can reach any more, and adds
    /// those ids to `released`.
    ///
    /// An id that was freed at version *w* is free again once both of
    /// these hold:
    ///
    /// - no version pin is below *w*: [`Pins::oldest_pin`] is *w* or later
    /// - every operation guard that was held when a call of this found that
    ///   has been released: [`Pins::oldest_guard`] has reached the epoch
    ///   that the call started
    ///
    /// So an id can take two calls. With no pin below *w* and no guard
    /// held, one call frees it.
    ///
    /// Call this after a commit is published and after the oldest pin is
    /// dropped. An id that waits only for a guard is freed by the next
    /// call, whenever that comes. A call that finds nothing to do changes
    /// nothing.
    pub fn reclaim(&self, pins: &dyn Pins, released: &mut Vec<PageId>) {
        let oldest_pin = pins.oldest_pin();
        let epoch = pins.next_epoch();
        let oldest_guard = pins.oldest_guard();
        self.lock()
            .reclaim(oldest_pin, epoch, oldest_guard, released);
    }

    /// The state of `page`.
    pub fn state(&self, page: PageId) -> PageState {
        self.lock().states.get(page)
    }

    /// How many ids are in each state.
    pub fn census(&self) -> Census {
        let inner = self.lock();
        Census {
            high_water: inner.states.high_water(),
            private: inner.states.count(PageState::Private),
            live: inner.states.count(PageState::Live),
            retired: inner.states.count(PageState::Retired),
        }
    }

    /// Adds to `out` every retired id with the version its page was freed
    /// at, in no particular order.
    pub fn retired(&self, out: &mut Vec<(PageId, CommitVersion)>) {
        let inner = self.lock();
        let waiting = inner
            .waiting
            .iter()
            .map(|(freed_at, pages)| (*freed_at, pages));
        let draining = inner
            .draining
            .iter()
            .map(|drained| (drained.freed_at, &drained.pages));
        for (freed_at, pages) in waiting.chain(draining) {
            out.extend(pages.iter().map(|page| (*page, freed_at)));
        }
    }

    /// Adds to `out` the next ids that are not free, with their states,
    /// from the page `from` on, and returns the page to go on from, or
    /// `None` when the walk is over. The ids are in ascending order.
    ///
    /// One call is one step, over a run of at most 4096 ids, and it can add
    /// nothing and still return a page to go on from. A walk over every id,
    /// from [`PageId::MIN`] until `None`, lets changes in between its
    /// calls.
    pub fn scan(&self, from: PageId, out: &mut Vec<(PageId, PageState)>) -> Option<PageId> {
        self.lock().states.scan(from, out)
    }
}

#[cfg(test)]
impl PageAllocator {
    /// An allocator whose ids end at `limit`.
    pub(super) fn with_limit(limit: PageId) -> Self {
        Self::from_parts(States::new(limit), BTreeMap::new())
    }

    /// A copy of the allocator as it is now.
    pub(super) fn fork(&self) -> Self {
        Self {
            inner: Mutex::new(self.lock().clone()),
        }
    }

    /// The retired ids that a pin may still reach, by the version that
    /// freed them, and the ones that wait for guards, by their epoch.
    #[allow(clippy::type_complexity)]
    pub(super) fn stages(
        &self,
    ) -> (
        Vec<(CommitVersion, Vec<PageId>)>,
        Vec<(Epoch, CommitVersion, Vec<PageId>)>,
    ) {
        let inner = self.lock();
        let waiting = inner
            .waiting
            .iter()
            .map(|(freed_at, pages)| (*freed_at, pages.clone()))
            .collect();
        let draining = inner
            .draining
            .iter()
            .map(|drained| (drained.epoch, drained.freed_at, drained.pages.clone()))
            .collect();
        (waiting, draining)
    }
}
