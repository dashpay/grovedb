//! What write-back asks of the pool: which page versions no segment holds
//! yet, by the commit that bore them, and when one of them may go.

use lignin_format::{CommitVersion, PageId};

use super::{
    error::CacheError,
    frame::{flag, kind, NO_VERSION},
    guard::PageRef,
    pool::{published, BufferPool, Held},
    sync::{lock, write},
};
use crate::alloc::Pins;

impl BufferPool {
    /// The oldest commit that bore a page version write-back still owes, if
    /// there is one. Every change of every commit before it is in a
    /// segment, or is of a page that was freed since (`page-table.md` K1,
    /// T18).
    pub fn oldest_unwritten(&self) -> Option<CommitVersion> {
        let unwritten = lock(&self.unwritten);
        let oldest = unwritten.versions.keys().next().copied();
        oldest.and_then(CommitVersion::new)
    }

    /// Adds to `out` every commit that bore a page version write-back still
    /// owes, oldest first.
    pub fn unwritten_versions(&self, out: &mut Vec<CommitVersion>) {
        let unwritten = lock(&self.unwritten);
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
        let unwritten = lock(&self.unwritten);
        if let Some(pages) = unwritten.versions.get(&version.get()) {
            out.extend_from_slice(pages);
        }
    }

    /// Records that the content of `frame` is in a segment and that the page
    /// table has its link. The page version leaves the unwritten lists, and
    /// the frame can be evicted once it has no pin.
    ///
    /// The link is pushed before this is called: a reader that misses the
    /// page afterwards reads the page table, and has to find it there.
    ///
    /// It fails, and write-back still owes the page version, if `frame` is
    /// not how the page version is stored any more, because a page step
    /// replaced it while it was being written ([`CacheError::Replaced`]); if
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
        let page = frame.page();
        let born = frame.born();
        let header = frame.frame();
        let bucket = self.bucket(page.get());
        let _step = lock(&bucket.step);
        let chain = write(&bucket.chain);
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
        drop(chain);
        let mut unwritten = lock(&self.unwritten);
        unwritten.remove(born.get(), page);
        if hold {
            unwritten.held.push(Held {
                page,
                born: born.get(),
                superseded,
                epoch: None,
            });
        }
        Ok(())
    }

    /// Drops the page versions that are held for readers and that no reader
    /// can ask for any more, and returns how many went.
    ///
    /// A held page version that was superseded at version *w* goes once no
    /// version pin is below *w*, and once every operation that was running
    /// when that was found has ended (`transactions.md` PN6, FL6). The pool
    /// asks `pins` the three questions in their order, as the page id
    /// allocator does, and never waits: a page version that does not pass
    /// stays until a later call.
    ///
    /// Call it where the allocator's reclaim is called: after a commit is
    /// published and after the oldest pin is dropped. With nothing held it
    /// asks nothing.
    pub fn reclaim<P: Pins + ?Sized>(&self, pins: &P) -> usize {
        if lock(&self.unwritten).held.is_empty() {
            return 0;
        }
        let oldest = pins.oldest_pin().get();
        let epoch = pins.next_epoch();
        let guard = pins.oldest_guard();

        let mut gone = Vec::new();
        lock(&self.unwritten).held.retain_mut(|held| {
            if held.superseded > oldest {
                return true;
            }
            if *held.epoch.get_or_insert(epoch) > guard {
                return true;
            }
            gone.push((held.page, held.born));
            false
        });

        for &(page, born) in &gone {
            let bucket = self.bucket(page.get());
            let step = lock(&bucket.step);
            let mut chain = write(&bucket.chain);
            let held = published(&chain, page)
                .find(|entry| entry.frame().born() == born && entry.frame().has(flag::HELD))
                .map(|entry| entry.pointer());
            let freed = held.and_then(|pointer| chain.retire(pointer));
            drop(chain);
            drop(step);
            if let Some(frame) = freed {
                self.release(frame);
            }
        }
        gone.len()
    }
}
