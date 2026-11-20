//! What changes which page version is current: private copies, the commit
//! that installs them, the freeing of a page, and the page step of
//! background work.

use std::fmt;

use lignin_format::{CommitVersion, PageId};

use super::{
    error::{CacheError, StepRefusal},
    frame::{flag, kind, Exclusive, Links, NO_VERSION},
    guard::{Blank, PageRef, PrivateRef},
    pool::{current, lookup, privates, published, BufferPool, Held, Miss},
    sync::{lock, MutexGuard},
};

/// Who a private copy belongs to: a write scope, or a write transaction for
/// what its scopes have merged into it (`transactions.md` LT4, LT5).
///
/// The pool only compares two of these. Whoever opens transactions and
/// scopes gives each a number of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnerId(u64);

impl OwnerId {
    /// The owner with this number.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The owner's number.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for OwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "owner {}", self.0)
    }
}

fn version(raw: u64) -> Option<CommitVersion> {
    CommitVersion::new(raw)
}

/// Why [`BufferPool::make_private_once`] made no private copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotMade {
    /// The page version to copy is not in the pool, and the owner has no
    /// copy of the page. The caller loads the page and asks again.
    Miss(Miss),
    /// The owner already holds a private copy of the page.
    AlreadyPrivate,
}

/// Checks that a commit of `version` can follow every version of `page` in
/// the chain.
fn follows(chain: &Links, page: PageId, version: CommitVersion) -> Result<(), CacheError> {
    let newest = published(chain, page)
        .map(|entry| entry.frame().born())
        .max();
    let after = newest.is_none_or(|newest| newest < version.get());
    if version == CommitVersion::EMPTY || !after {
        return Err(CacheError::VersionNotAfter {
            page,
            version,
            current: newest.and_then(self::version),
        });
    }
    Ok(())
}

impl BufferPool {
    /// The private copy that `owner` has of `page`, pinned. If it has none,
    /// one is made from the page version a reader at version `at` sees, and
    /// from then on background work leaves that page alone
    /// (`transactions.md` PV7).
    ///
    /// The copy is made once for an owner, whatever number of its
    /// operations ask at the same moment (LT5): all of them get the one
    /// frame. Making the copy is a page step. An owner whose copies have
    /// one writer each asks with [`Self::make_private_once`] instead.
    ///
    /// A miss says the page version to copy is not in the pool. The caller
    /// loads it ([`Self::install_loaded`]) and asks again while it still
    /// holds what it loaded: a page that nothing pins can be evicted before
    /// the second call, and in a pool whose every frame is in use it is.
    pub fn make_private(
        &self,
        owner: OwnerId,
        page: PageId,
        at: CommitVersion,
    ) -> Result<PrivateRef<'_>, Miss> {
        let (Ok(copy) | Err(copy)) = self.copy_for(owner, page, at)?;
        Ok(copy)
    }

    /// A private copy of `page` for `owner`, which has none: made as
    /// [`Self::make_private`] makes one, and refused
    /// ([`NotMade::AlreadyPrivate`]) if the owner has a copy of the page.
    ///
    /// That the owner has no copy is found in the page step that makes the
    /// copy. So of any number of callers that ask this way for one owner
    /// and page at the same moment, the one whose step makes the copy is
    /// given it and every other one is refused: an owner whose pages have one writer each, all of
    /// which ask this way, needs no check of its own. A caller of
    /// [`Self::make_private`] is given the copy all the same.
    ///
    /// A copy that exists is found before the page version to copy is
    /// looked for, so [`NotMade::Miss`] says as well that the owner had no
    /// copy then. The caller loads the page and asks again, as for
    /// [`Self::make_private`].
    pub fn make_private_once(
        &self,
        owner: OwnerId,
        page: PageId,
        at: CommitVersion,
    ) -> Result<PrivateRef<'_>, NotMade> {
        match self.copy_for(owner, page, at) {
            Ok(Ok(made)) => Ok(made),
            Ok(Err(_existing)) => Err(NotMade::AlreadyPrivate),
            Err(miss) => Err(NotMade::Miss(miss)),
        }
    }

    /// The private copy of `page` that this call made for `owner`, or, as
    /// the inner error, the one the owner had.
    pub(super) fn copy_for(
        &self,
        owner: OwnerId,
        page: PageId,
        at: CommitVersion,
    ) -> Result<Result<PrivateRef<'_>, PrivateRef<'_>>, Miss> {
        let bucket = self.bucket(page.get());
        let find = || {
            let chain = bucket.chain.read();
            let existing = privates(&chain, page)
                .find(|entry| entry.frame().owner() == owner.get())
                .map(|entry| entry.pin(&self.arena));
            if let Some(pin) = existing {
                return Ok(Err(PrivateRef::new(self, pin)));
            }
            let source = lookup(&chain, page, at)?
                .freeze(&self.arena)
                .expect("the lookup gives page versions");
            drop(chain);
            Ok(Ok(PageRef::new(self, source)))
        };

        // The source is pinned before the frame for the copy is taken, so
        // that taking the frame cannot evict it.
        let mut source = match find()? {
            Ok(source) => source,
            Err(existing) => return Ok(Err(existing)),
        };
        let mut blank = self.blank();
        let step = lock(&bucket.step);
        // Before the step began, a page step may have replaced the frame
        // that was pinned, or another operation of this owner may have made
        // the copy. From here on neither can happen: both are page steps.
        let made =
            privates(&bucket.chain.read(), page).any(|entry| entry.frame().owner() == owner.get());
        if made || source.frame().kind() != kind::PUBLISHED {
            source = match find()? {
                Ok(source) => source,
                Err(existing) => return Ok(Err(existing)),
            };
        }

        blank.copy_from(&source);
        let frame = blank.into_frame();
        let header = frame.header();
        header.set_page_raw(page.get());
        header.set_owner(owner.get());
        header.set_base(source.born().get());
        frame.set_kind(kind::PRIVATE);
        let pin = bucket
            .chain
            .write()
            .link(&self.arena, frame)
            .pin(&self.arena);
        drop(step);
        Ok(Ok(PrivateRef::new(self, pin)))
    }

    /// A private copy for a page that `owner`'s transaction created: every
    /// byte zero, and made from no page version.
    ///
    /// It fails with [`CacheError::AlreadyPrivate`] if the owner has a copy
    /// of that page.
    pub fn new_private(&self, owner: OwnerId, page: PageId) -> Result<PrivateRef<'_>, CacheError> {
        let mut blank = self.blank();
        blank.fill(0);
        let bucket = self.bucket(page.get());
        let step = lock(&bucket.step);
        let mut chain = bucket.chain.write();
        if privates(&chain, page).any(|entry| entry.frame().owner() == owner.get()) {
            return Err(CacheError::AlreadyPrivate { page, owner });
        }
        let frame = blank.into_frame();
        let header = frame.header();
        header.set_page_raw(page.get());
        header.set_owner(owner.get());
        header.set_base(NO_VERSION);
        frame.set_kind(kind::PRIVATE);
        let pin = chain.link(&self.arena, frame).pin(&self.arena);
        drop(chain);
        drop(step);
        Ok(PrivateRef::new(self, pin))
    }

    /// The private copy that `owner` has of `page`, pinned, if it has one.
    pub fn private(&self, owner: OwnerId, page: PageId) -> Option<PrivateRef<'_>> {
        let chain = self.bucket(page.get()).chain.read();
        let pin = privates(&chain, page)
            .find(|entry| entry.frame().owner() == owner.get())
            .map(|entry| entry.pin(&self.arena));
        drop(chain);
        pin.map(|pin| PrivateRef::new(self, pin))
    }

    /// Gives a private copy to another owner: a scope's copy to its
    /// transaction, when the scope merges.
    ///
    /// It fails with [`CacheError::AlreadyPrivate`] if `to` has a copy of
    /// that page. The two are then reconciled by the caller (LT4), and the
    /// scope's copy is discarded.
    pub fn transfer(&self, copy: &PrivateRef<'_>, to: OwnerId) -> Result<(), CacheError> {
        if !copy.is_of(self) {
            return Err(CacheError::WrongPool);
        }
        let page = copy.page();
        let bucket = self.bucket(page.get());
        let _step = lock(&bucket.step);
        let chain = bucket.chain.write();
        let mut copies = privates(&chain, page);
        let Some(entry) = copies.find(|entry| entry.pointer() == copy.pointer()) else {
            return Err(CacheError::NotPrivate { page });
        };
        if entry.frame().owner() == to.get() {
            return Ok(());
        }
        if privates(&chain, page).any(|other| other.frame().owner() == to.get()) {
            return Err(CacheError::AlreadyPrivate { page, owner: to });
        }
        entry.frame().set_owner(to.get());
        Ok(())
    }

    /// The exclusive latch of a copy that is to be installed or discarded,
    /// taken without waiting.
    fn latch_of<'l>(&self, copy: &'l PrivateRef<'_>) -> Result<Exclusive<'l>, CacheError> {
        if !copy.is_of(self) {
            return Err(CacheError::WrongPool);
        }
        let page = copy.page();
        match copy.pin().try_write() {
            Some(Some(latch)) => Ok(latch),
            Some(None) => Err(CacheError::NotPrivate { page }),
            None => Err(CacheError::Latched { page }),
        }
    }

    /// Drops a private copy: its transaction rolled it back, rebuilt it, or
    /// reconciled it into another. The frame goes when its last handle
    /// does. A handle that is left, `copy` among them, still reads what the
    /// frame held and can no longer change it.
    ///
    /// It takes the copy's latch and does not wait for it. It fails, and the
    /// copy stays, with [`CacheError::Latched`] if an operation still holds
    /// that latch, and with [`CacheError::NotPrivate`] if the copy was
    /// installed or discarded before.
    pub fn discard(&self, copy: &PrivateRef<'_>) -> Result<(), CacheError> {
        let latch = self.latch_of(copy)?;
        let bucket = self.bucket(copy.page().get());
        let step = lock(&bucket.step);
        bucket
            .chain
            .write()
            .retire_pinned(&self.arena, copy.pointer());
        drop(step);
        drop(latch);
        Ok(())
    }

    /// Turns a private copy into the page version that the commit `version`
    /// bears (`transactions.md` CM3 step 7), and returns it pinned. `copy`
    /// is left as a handle that reads the page and can no longer change it.
    ///
    /// The page version it replaces is superseded at `version`. The new one
    /// is unwritten: it stays in the pool until write-back has put it into a
    /// segment ([`Self::mark_written`]). Nothing is visible to a reader
    /// before the commit is published, because no reader asks at `version`
    /// before then.
    ///
    /// Installing is a page step. It takes the copy's latch and does not
    /// wait for it: a commit runs when every scope of its transaction is
    /// closed (CM1), so no operation holds a latch of its copies, and a
    /// commit waits for no latch while it holds the counts lock (LT7).
    ///
    /// The commit holds the commit lock and has rebased its transaction, so
    /// the copy was made from the current version of its page. If the pool
    /// can see otherwise, it refuses, and the copy stays private:
    /// [`CacheError::BaseNotCurrent`], [`CacheError::VersionNotAfter`],
    /// [`CacheError::Latched`], or [`CacheError::NotPrivate`] for a copy
    /// that was installed or discarded before.
    pub fn install<'p>(
        &'p self,
        copy: &PrivateRef<'p>,
        version: CommitVersion,
    ) -> Result<PageRef<'p>, CacheError> {
        let latch = self.latch_of(copy)?;
        let page = copy.page();
        let bucket = self.bucket(page.get());
        let step = lock(&bucket.step);
        let chain = bucket.chain.write();

        let frame = latch.frame();
        let base = frame.base();
        let currently = current(&chain, page);
        let newest = published(&chain, page)
            .map(|entry| entry.frame().born())
            .max();
        let known_current = match currently {
            Some(entry) => Some(entry.frame().born()),
            None => newest.filter(|&newest| base != NO_VERSION && newest > base),
        };
        let base_superseded = published(&chain, page)
            .any(|entry| entry.frame().born() == base && entry.frame().superseded() != NO_VERSION);
        if known_current.is_some_and(|current| current != base) || base_superseded {
            return Err(CacheError::BaseNotCurrent {
                page,
                base: self::version(base),
                current: known_current.and_then(self::version),
            });
        }
        follows(&chain, page, version)?;

        if let Some(entry) = currently {
            entry.frame().set_superseded(version.get());
        }
        frame.set_born(version.get());
        frame.set_superseded(NO_VERSION);
        frame.set_flags(flag::UNWRITTEN | (frame.flags() & flag::ATTACHED));
        let sealed = latch.publish();
        frame.touch();
        chain.advance_stamp();
        // The reader's pin is taken under the table's lock, before a page
        // step can replace the frame.
        let frozen = copy.pin().again().into_frozen();
        drop(chain);
        lock(&self.turnover.unwritten).add(version.get(), page);
        drop(step);
        drop(sealed);
        Ok(PageRef::new(
            self,
            frozen.expect("the frame was made a page version under this lock"),
        ))
    }

    /// Records that the commit `version` freed `page` (`transactions.md` CM3
    /// step 7): the current version of the page is superseded at `version`,
    /// and nothing follows it.
    ///
    /// The commit gives the page its free link in the page table first
    /// (`page-table.md` T8) and calls this after it, so that a reader that
    /// misses the page from here on finds the free link.
    ///
    /// What write-back still owed of the page is never written back
    /// (`page-table.md` T18). Those page versions leave the unwritten lists
    /// and stay in the pool, where they cannot be evicted, until
    /// [`Self::reclaim`] finds that no reader can ask for them (FL6).
    ///
    /// It fails with [`CacheError::VersionNotAfter`] if `version` is not
    /// after every version of the page in the pool.
    pub fn free(&self, page: PageId, version: CommitVersion) -> Result<(), CacheError> {
        let bucket = self.bucket(page.get());
        let step = lock(&bucket.step);
        let chain = bucket.chain.write();
        follows(&chain, page, version)?;
        if let Some(entry) = current(&chain, page) {
            entry.frame().set_superseded(version.get());
        }
        let mut held = Vec::new();
        for entry in published(&chain, page) {
            let frame = entry.frame();
            let flags = frame.flags();
            if flags & flag::UNWRITTEN != 0 {
                frame.set_flags((flags & !flag::UNWRITTEN) | flag::HELD);
                held.push(Held {
                    page,
                    born: frame.born(),
                    superseded: frame.superseded(),
                    epoch: None,
                });
            }
        }
        chain.advance_stamp();
        drop(chain);
        let mut unwritten = lock(&self.turnover.unwritten);
        for entry in &held {
            unwritten.remove(entry.born, page);
        }
        unwritten.held.extend(held);
        drop(unwritten);
        drop(step);
        Ok(())
    }

    /// Begins a page step on `page` (`transactions.md` PV7): until the
    /// returned guard is dropped, no commit installs a version of the page
    /// or frees it, no private copy of it is made or given up, and no other
    /// page step runs on it.
    ///
    /// This is the lock of level 5 in the lock order LT7. A thread that
    /// holds it calls nothing of the pool but the guard's own methods,
    /// [`Self::get`], [`Self::get_version`] and [`Self::blank`], and takes
    /// no latch. The lock is shared by the pages of one part of the pool's
    /// table, so a thread holds one page step at a time: a second one, on
    /// whatever page, can wait for the first for ever.
    pub fn page_step(&self, page: PageId) -> PageStep<'_> {
        PageStep {
            pool: self,
            page,
            _step: lock(&self.bucket(page.get()).step),
        }
    }
}

/// A page step in progress on one page ([`BufferPool::page_step`]).
///
/// Background work that changes the bytes of a published page (a value
/// pointer rewrite, the placing of a pending value, the removal of what a
/// dropped subtree left) does it here: it looks at the current version,
/// builds the changed page in a frame of its own, and puts that frame in
/// place of the old one with [`Self::replace`], which makes the three checks
/// of PV7. Readers that hold the old frame keep reading it unchanged (LT2).
pub struct PageStep<'p> {
    pool: &'p BufferPool,
    page: PageId,
    _step: MutexGuard<'p, ()>,
}

impl<'p> PageStep<'p> {
    /// The page the step is on.
    pub fn page(&self) -> PageId {
        self.page
    }

    /// The current version of the page, pinned, if it is in the pool.
    pub fn current(&self) -> Option<PageRef<'p>> {
        let chain = self.pool.bucket(self.page.get()).chain.read();
        let frozen = current(&chain, self.page).and_then(|entry| entry.freeze(&self.pool.arena));
        drop(chain);
        frozen.map(|frozen| PageRef::new(self.pool, frozen))
    }

    /// Whether the page version that the commit `born` bore is still to be
    /// written back: the pool has it, as it was installed or as a page
    /// step left it, and lists it as unwritten.
    ///
    /// It is not once the page was freed ([`BufferPool::free`] takes what
    /// was owed of a page off the lists), also when the id names another
    /// page by now, whose versions were born later. Whoever writes back
    /// asks this in the step in which it pushes the link of the version's
    /// block (`page-table.md` T18).
    pub fn owes(&self, born: CommitVersion) -> bool {
        let chain = self.pool.bucket(self.page.get()).chain.read();
        published(&chain, self.page).any(|entry| {
            let frame = entry.frame();
            frame.born() == born.get() && frame.has(flag::UNWRITTEN)
        })
    }

    /// How many private copies of the page open write transactions hold.
    pub fn private_copies(&self) -> usize {
        let chain = self.pool.bucket(self.page.get()).chain.read();
        privates(&chain, self.page).count()
    }

    /// Whether `frame` is how the current version of the page is stored now:
    /// no commit has superseded or freed the page since `frame` was pinned,
    /// and no page step has replaced the frame.
    pub fn is_current(&self, frame: &PageRef<'_>) -> bool {
        let chain = self.pool.bucket(self.page.get()).chain.read();
        current(&chain, self.page).is_some_and(|entry| entry.pointer() == frame.pointer())
    }

    /// Puts `new` in place of `old` as the current version of the page, born
    /// at the same commit, and returns it pinned. `new` is what the caller
    /// made of `old` ([`Blank::copy_from`]).
    ///
    /// It is refused, and nothing changes, unless `old` is still how the
    /// current version of the page is stored ([`StepRefusal::NotCurrent`])
    /// and no open write transaction holds a private copy of the page
    /// ([`StepRefusal::PrivateCopy`]). A frame of another pool is refused
    /// as well ([`StepRefusal::WrongPool`]). A caller that has to write a record
    /// before the change makes both checks first, with [`Self::is_current`]
    /// and [`Self::private_copies`]: under the same guard they cannot turn
    /// false.
    ///
    /// The new frame is unwritten, whatever the old one was: write-back owes
    /// its content to a segment.
    pub fn replace(&self, old: &PageRef<'_>, new: Blank<'p>) -> Result<PageRef<'p>, StepRefusal> {
        let pool = self.pool;
        let page = self.page;
        if !new.is_of(pool) {
            return Err(StepRefusal::WrongPool);
        }
        let mut chain = pool.bucket(page.get()).chain.write();
        let Some(entry) = current(&chain, page).filter(|entry| entry.pointer() == old.pointer())
        else {
            return Err(StepRefusal::NotCurrent);
        };
        if privates(&chain, page).next().is_some() {
            return Err(StepRefusal::PrivateCopy);
        }
        let born = entry.frame().born();
        let owed = entry.frame().has(flag::UNWRITTEN);

        let frame = new.into_frame();
        let header = frame.header();
        header.set_page_raw(page.get());
        header.set_born(born);
        header.set_superseded(NO_VERSION);
        header.set_flags(flag::UNWRITTEN | (header.flags() & flag::ATTACHED));
        header.touch();
        frame.set_kind(kind::PUBLISHED);
        let frozen = chain.link(&pool.arena, frame).freeze(&pool.arena);
        chain.retire_pinned(&pool.arena, old.pointer());
        chain.advance_stamp();
        drop(chain);
        if !owed {
            lock(&pool.turnover.unwritten).add(born, page);
        }
        Ok(PageRef::new(
            pool,
            frozen.expect("the frame is a page version"),
        ))
    }
}

impl fmt::Debug for PageStep<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageStep")
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}
