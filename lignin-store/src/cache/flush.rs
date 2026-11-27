//! What write-back asks of the pool: which page versions no segment holds
//! yet, by the commit that bore them, and when one of them may go.

use lignin_format::{CommitVersion, PageId};

use super::{
    error::CacheError,
    frame::{flag, kind, NO_VERSION},
    guard::PageRef,
    pool::{evictable, published, BufferPool, Held},
    sync::lock,
};
use crate::alloc::Pins;

impl BufferPool {
    /// The oldest commit that bore a page version write-back still owes, if
    /// there is one. Every change of every commit before it is in a
    /// segment, or is of a page that was freed since (`page-table.md` K1,
    /// T18).
    pub fn oldest_unwritten(&self) -> Option<CommitVersion> {
        let unwritten = lock(&self.turnover.unwritten);
        let oldest = unwritten.versions.keys().next().copied();
        oldest.and_then(CommitVersion::new)
    }

    /// Adds to `out` every commit that bore a page version write-back still
    /// owes, oldest first.
    pub fn unwritten_versions(&self, out: &mut Vec<CommitVersion>) {
        let unwritten = lock(&self.turnover.unwritten);
        out.extend(
            unwritten
                .versions
                .keys()
                .copied()
                .filter_map(CommitVersion::new),
        );
    }

    /// Adds to `out` the pages of which the commit `version` bore a version
    /// that write-back still owes, in no order.
    ///
    /// A page is on the list of the commit that bore its current
    /// representation: a page step that changes a page version whose
    /// content was in a segment puts the page back on that commit's list.
    pub fn unwritten_pages(&self, version: CommitVersion, out: &mut Vec<PageId>) {
        let unwritten = lock(&self.turnover.unwritten);
        if let Some(pages) = unwritten.versions.get(&version.get()) {
            out.extend_from_slice(pages);
        }
    }

    /// Records that the content of `frame` is in a segment and that the page
    /// table has its link. The page version leaves the unwritten lists, and
    /// the frame can be evicted once it has no pin.
    ///
    /// The version of the page before it that a segment holds, the newest
    /// such in the pool, is spent from here on: write-back checks no run
    /// against it any more. It is listed, and [`Self::reclaim`] takes it
    /// out once no reader can ask for it, if it has no pin then.
    ///
    /// The link is pushed before this is called: a reader that misses the
    /// page afterwards reads the page table, and has to find it there.
    ///
    /// It fails, and write-back still owes the page version, if `frame` is
    /// not how the page version is stored any more, because a page step
    /// replaced it while it was being written ([`CacheError::Replaced`]: the
    /// same answer is given for a frame of a page that was freed meanwhile
    /// and that the pool has dropped, of which nothing is owed); if
    /// it has attached values, which are not in the segment
    /// ([`CacheError::AttachedValues`]); or if write-back did not owe it
    /// ([`CacheError::NotUnwritten`]).
    pub fn mark_written(&self, frame: &PageRef<'_>) -> Result<(), CacheError> {
        self.settle(frame, false)
    }

    /// Keeps a superseded page version in the pool for the readers that can
    /// still ask for it, instead of writing it back (`transactions.md` FL7,
    /// for a page version that was superseded with a pending value). It
    /// leaves the unwritten lists, and stays, with its attached values,
    /// until [`Self::reclaim`] finds that no reader can ask for it.
    ///
    /// It fails as [`Self::mark_written`] does, except that attached values
    /// are no obstacle, and with [`CacheError::StillCurrent`] for the
    /// current version of a page, which readers of every later version
    /// need.
    pub fn hold_for_readers(&self, frame: &PageRef<'_>) -> Result<(), CacheError> {
        self.settle(frame, true)
    }

    fn settle(&self, frame: &PageRef<'_>, hold: bool) -> Result<(), CacheError> {
        if !frame.is_of(self) {
            return Err(CacheError::WrongPool);
        }
        let page = frame.page();
        let born = frame.born();
        let header = frame.frame();
        let bucket = self.bucket(page.get());
        let _step = lock(&bucket.step);
        let chain = bucket.chain.write();
        let flags = header.flags();
        let superseded = header.superseded();
        if header.kind() != kind::PUBLISHED {
            return Err(CacheError::Replaced { page, born });
        }
        if flags & flag::UNWRITTEN == 0 {
            return Err(CacheError::NotUnwritten { page, born });
        }
        if hold && superseded == NO_VERSION {
            return Err(CacheError::StillCurrent { page, born });
        }
        if !hold && flags & flag::ATTACHED != 0 {
            return Err(CacheError::AttachedValues { page, born });
        }
        let kept = if hold { flag::HELD } else { 0 };
        header.set_flags((flags & !flag::UNWRITTEN) | kept);
        // Of the versions of the page that are in the pool, the newest
        // before this one that a segment holds was the page under this
        // one's run, and no run is checked against it any more. It alone
        // is listed here. An older one that is in the pool stays what it
        // is, a frame that can be evicted: one that was listed earlier is
        // still listed, and one that was not is the clock's.
        let spent = (!hold)
            .then(|| {
                published(&chain, page)
                    .map(|entry| entry.frame())
                    .filter(|before| before.born() < born.get() && before.flags() == 0)
                    .max_by_key(|before| before.born())
            })
            .flatten()
            .map(|before| Held {
                page,
                born: before.born(),
                superseded: before.superseded(),
                epoch: None,
            });
        drop(chain);
        let mut unwritten = lock(&self.turnover.unwritten);
        unwritten.remove(born.get(), page);
        if hold {
            unwritten.held.push(Held {
                page,
                born: born.get(),
                superseded,
                epoch: None,
            });
        }
        unwritten.spend(spent);
        Ok(())
    }

    /// Drops the page versions that are held for readers and that no reader
    /// can ask for any more, and returns how many went. It also takes out
    /// the spent page versions that no reader can ask for any more
    /// ([`PoolStats::releases`](super::PoolStats::releases)), which the
    /// number it returns does not count.
    ///
    /// A held page version that was superseded at version *w* goes once no
    /// version pin is below *w*, and once every operation that was running
    /// when that was found has ended (`transactions.md` PN6, FL6). The pool
    /// asks `pins` the three questions in their order, as the page id
    /// allocator does, and never waits: a page version that does not pass
    /// stays until a later call.
    ///
    /// A spent page version is one that is superseded and in a segment,
    /// of a page whose later version is in a segment too
    /// ([`Self::mark_written`]) or that was freed ([`Self::free`]). The
    /// pool keeps it for no one but the readers on a version before the
    /// one that superseded it, so a listed one is asked the same three
    /// questions and then leaves its frame to the next page that needs
    /// one, where it would otherwise wait for the clock, which cannot tell
    /// it from a page that is read. It is taken out as an eviction takes a
    /// frame out: only with no pin on it. One that has a pin at that
    /// moment is listed no more and is the clock's from there on. A reader
    /// that asks for a version that was taken out all the same reads it
    /// from its segments, as after an eviction.
    ///
    /// Call it where the allocator's reclaim is called: after a commit is
    /// published and after the oldest pin is dropped. With nothing held and
    /// nothing spent it asks nothing.
    pub fn reclaim<P: Pins + ?Sized>(&self, pins: &P) -> usize {
        let mut gone = {
            let mut unwritten = lock(&self.turnover.unwritten);
            if unwritten.held.is_empty() && unwritten.spent.is_empty() {
                return 0;
            }
            std::mem::take(&mut unwritten.gone)
        };
        let oldest = pins.oldest_pin().get();
        let epoch = pins.next_epoch();
        let guard = pins.oldest_guard();
        let behind = |version: &mut Held| {
            version.superseded <= oldest && *version.epoch.get_or_insert(epoch) <= guard
        };

        gone.clear();
        let mut unwritten = lock(&self.turnover.unwritten);
        unwritten.held.retain_mut(|held| {
            let goes = behind(held);
            if goes {
                gone.push((held.page, held.born));
            }
            !goes
        });
        let held = gone.len();
        unwritten.spent.retain_mut(|spent| {
            let goes = behind(spent);
            if goes {
                gone.push((spent.page, spent.born));
            }
            !goes
        });
        drop(unwritten);

        for &(page, born) in &gone[held..] {
            let mut chain = self.bucket(page.get()).chain.write();
            // The frame may be gone, by the clock, or not be spent any
            // more: the listing is a hint.
            let spent = published(&chain, page)
                .find(|entry| {
                    let frame = entry.frame();
                    frame.born() == born && frame.superseded() != NO_VERSION && evictable(frame)
                })
                .map(|entry| entry.pointer());
            let freed =
                spent.and_then(|pointer| chain.take_unpinned(&self.arena, pointer, evictable));
            drop(chain);
            if let Some(frame) = freed {
                self.turnover.releases.add(1);
                self.release(frame);
            }
        }
        gone.truncate(held);

        for &(page, born) in &gone {
            let bucket = self.bucket(page.get());
            let step = lock(&bucket.step);
            let mut chain = bucket.chain.write();
            let held = published(&chain, page)
                .find(|entry| entry.frame().born() == born && entry.frame().has(flag::HELD))
                .map(|entry| entry.pointer());
            let freed = held.and_then(|pointer| chain.retire(&self.arena, pointer));
            drop(chain);
            drop(step);
            if let Some(frame) = freed {
                self.release(frame);
            }
        }
        let held = gone.len();
        lock(&self.turnover.unwritten).gone = gone;
        held
    }
}
