//! The pool: its frames, its table of what they hold, the read path, the
//! load path and eviction.

use std::{collections::BTreeMap, fmt};

use lignin_format::{CommitVersion, PageId, PageSize};

use super::{
    error::CacheError,
    frame::{
        flag, kind, Arena, Chain, Entry, Frame, Links, Moment, OwnedFrame, Pinned, Unlocked,
        Unpinned, NO_VERSION,
    },
    guard::{Blank, PageRef},
    lanes::Lanes,
    sync::{lock, Counter, Mutex, Padded},
};
use crate::alloc::Epoch;

/// How a pool is sized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// The store's page size. A frame holds one page of it.
    pub page_size: PageSize,
    /// The memory the pool may use for what it caches, in bytes. It is
    /// allocated when the pool is made, and it includes each frame's
    /// header, the pool's table and its lanes: less than 512 bytes a frame
    /// beside the page.
    pub capacity: usize,
}

/// A page version that was asked for and is not in the pool
/// ([`BufferPool::get`]).
///
/// It is what the pool knew at that moment, which
/// [`BufferPool::install_loaded`] needs to say for which versions the page
/// that is then loaded stands, and whether it is still what the reader
/// asked for. It is good for that one load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Miss {
    pub(super) page: PageId,
    pub(super) at: CommitVersion,
    pub(super) stamp: u64,
    pub(super) newer: u64,
}

impl Miss {
    /// The page that was asked for.
    pub fn page(&self) -> PageId {
        self.page
    }

    /// The version it was asked at.
    pub fn at(&self) -> CommitVersion {
        self.at
    }
}

/// What became of a page that was loaded ([`BufferPool::install_loaded`]).
#[derive(Debug)]
pub enum Loaded<'p> {
    /// The page is in the pool, pinned.
    Page(PageRef<'p>),
    /// The pool changed while the page was being read, and what was read
    /// may be out of date. Nothing was installed, and the frame comes back
    /// to be filled again.
    Again(Blank<'p>),
}

/// Counts of what the pool has done since it was made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Calls of [`BufferPool::get`] that found the page version.
    pub hits: u64,
    /// Calls of [`BufferPool::get`] that did not.
    pub misses: u64,
    /// Page versions taken out of the pool to make room.
    pub evictions: u64,
    /// Frames allocated beside the fixed ones, because none of those could
    /// be evicted.
    pub overflows: u64,
}

/// What the pool holds at one moment, from a walk over all of it
/// ([`BufferPool::census`]).
///
/// The walk takes each part of the pool's table in turn, so with other
/// threads at work the numbers are of slightly different moments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Census {
    /// Page versions in the pool.
    pub published: usize,
    /// Private copies in the pool.
    pub private: usize,
    /// Frames with at least one pin.
    pub pinned: usize,
    /// Page versions that write-back still owes.
    pub unwritten: usize,
    /// Page versions that are held for readers and will never be written
    /// back.
    pub held: usize,
    /// Frames that cannot be evicted now: pinned, private, unwritten, held,
    /// or with attached values.
    pub unevictable: usize,
    /// Frames beside the fixed ones that are in the pool's table.
    pub overflow: usize,
    /// Those of them that cannot be evicted now. The pool keeps the two
    /// equal: a frame beside the fixed ones leaves as soon as it could be
    /// evicted.
    pub overflow_unevictable: usize,
}

/// One part of the pool's table, on a cache line of its own: a reader of
/// one part is not slowed by a change of the next.
#[repr(align(64))]
pub(super) struct Bucket {
    /// What makes a page step one step (`transactions.md` PV7, LT7 level 5).
    pub(super) step: Mutex<()>,
    pub(super) chain: Chain,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Held {
    pub(super) page: PageId,
    pub(super) born: u64,
    pub(super) superseded: u64,
    pub(super) epoch: Option<Epoch>,
}

/// The page versions no segment holds: by the commit that bore them, those
/// write-back owes, and apart from them those that are held for readers.
#[derive(Debug, Default)]
pub(super) struct Unwritten {
    pub(super) versions: BTreeMap<u64, Vec<PageId>>,
    pub(super) held: Vec<Held>,
}

impl Unwritten {
    pub(super) fn add(&mut self, born: u64, page: PageId) {
        self.versions.entry(born).or_default().push(page);
    }

    pub(super) fn remove(&mut self, born: u64, page: PageId) {
        if let Some(pages) = self.versions.get_mut(&born) {
            if let Some(at) = pages.iter().position(|&other| other == page) {
                pages.swap_remove(at);
            }
            if pages.is_empty() {
                self.versions.remove(&born);
            }
        }
    }
}

/// What changes with every frame that comes and goes.
pub(super) struct Turnover {
    pub(super) free: Mutex<Vec<OwnedFrame>>,
    hand: Mutex<usize>,
    pub(super) unwritten: Mutex<Unwritten>,
    overflow: Counter,
    attached: Counter,
    evictions: Counter,
    overflows: Counter,
}

/// The buffer pool. See the [module documentation](super).
pub struct BufferPool {
    pub(super) arena: Arena,
    pub(super) buckets: Box<[Bucket]>,
    bucket_bits: u32,
    capacity: usize,
    /// On cache lines of its own, away from what every reader reads.
    pub(super) turnover: Padded<Turnover>,
}

impl fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferPool")
            .field("frames", &self.frames())
            .field("frame_len", &self.frame_len())
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

/// Whether a linked frame with no pin may be taken out of the pool.
pub(super) fn evictable(frame: &Frame) -> bool {
    frame.kind() == kind::PUBLISHED && frame.flags() == 0
}

pub(super) fn published(chain: &Links, page: PageId) -> impl Iterator<Item = Entry<'_>> {
    chain.iter().filter(move |entry| {
        entry.frame().page_raw() == page.get() && entry.frame().kind() == kind::PUBLISHED
    })
}

pub(super) fn privates(chain: &Links, page: PageId) -> impl Iterator<Item = Entry<'_>> {
    chain.iter().filter(move |entry| {
        entry.frame().page_raw() == page.get() && entry.frame().kind() == kind::PRIVATE
    })
}

pub(super) fn current(chain: &Links, page: PageId) -> Option<Entry<'_>> {
    published(chain, page).find(|entry| entry.frame().superseded() == NO_VERSION)
}

/// The page version of `page` that a reader at `at` sees, if it is in the
/// chain. Otherwise what the chain knows for the load.
pub(super) fn lookup(chain: &Links, page: PageId, at: CommitVersion) -> Result<Entry<'_>, Miss> {
    let mut newer = NO_VERSION;
    for entry in published(chain, page) {
        let born = entry.frame().born();
        if born > at.get() {
            newer = newer.min(born);
        } else if at.get() < entry.frame().superseded() {
            return Ok(entry);
        }
    }
    Err(Miss {
        page,
        at,
        stamp: chain.stamp(),
        newer,
    })
}

/// Whether `frame` holds the version of `page` that a reader at `at` sees.
/// Whoever asks has seen that the frame is a page version.
fn stands_for(frame: &Frame, page: PageId, at: CommitVersion) -> bool {
    frame.page_raw() == page.get() && frame.born() <= at.get() && at.get() < frame.superseded()
}

impl BufferPool {
    /// The bytes the fixed part of a pool takes: `count` pages of
    /// `frame_len` bytes, their headers, their places on the free list, a
    /// table of `buckets` parts and `lanes` lanes.
    fn cost(count: usize, frame_len: usize, buckets: usize, lanes: usize) -> Option<usize> {
        let frame = frame_len
            .checked_add(Arena::HEADER_LEN)?
            .checked_add(size_of::<OwnedFrame>())?;
        count
            .checked_mul(frame)?
            .checked_add(buckets.checked_mul(size_of::<Bucket>())?)?
            .checked_add(lanes.checked_mul(Lanes::LANE_LEN)?)
    }

    /// The bytes the fixed part of a pool of `count` frames takes, with the
    /// table and the lanes such a pool is given.
    fn fixed_cost(count: usize, frame_len: usize) -> Option<usize> {
        let buckets = Self::buckets_for(count)?;
        Self::cost(count, frame_len, buckets, Lanes::count_for(count))
    }

    /// The parts of the table of a pool of `count` frames: at least two for
    /// a frame.
    fn buckets_for(count: usize) -> Option<usize> {
        count.checked_mul(2)?.checked_next_power_of_two()
    }

    /// A pool of as many frames as `config.capacity` bytes hold.
    ///
    /// All of that memory is allocated here. The pool allocates more only
    /// for a frame that it needs while no frame can be evicted, and gives
    /// that back as soon as the frame could be.
    pub fn new(config: PoolConfig) -> Result<Self, CacheError> {
        let frame_len = config.page_size.get();
        let fits = |count: usize| {
            Self::fixed_cost(count, frame_len).is_some_and(|cost| cost <= config.capacity)
        };
        if !fits(1) {
            return Err(CacheError::CapacityTooSmall {
                capacity: config.capacity,
                frame: Self::fixed_cost(1, frame_len).ok_or(CacheError::CapacityTooLarge)?,
            });
        }
        // The cost of the fixed part rises with the number of frames, so
        // the most frames that fit are found by halving.
        let (mut count, mut beyond) = (1, config.capacity / frame_len + 1);
        while beyond - count > 1 {
            let middle = count + (beyond - count) / 2;
            if fits(middle) {
                count = middle;
            } else {
                beyond = middle;
            }
        }
        let buckets = Self::buckets_for(count).ok_or(CacheError::CapacityTooLarge)?;
        Self::with_frames(count, frame_len, buckets, config.capacity)
    }

    /// A pool of `count` frames of `frame_len` bytes, with a table of
    /// `buckets` parts, a power of two.
    pub(super) fn with_frames(
        count: usize,
        frame_len: usize,
        buckets: usize,
        capacity: usize,
    ) -> Result<Self, CacheError> {
        assert!(buckets.is_power_of_two());
        let (arena, free) = Arena::new(count, frame_len)?;
        let buckets = (0..buckets)
            .map(|_| Bucket {
                step: Mutex::new(()),
                chain: Chain::new(),
            })
            .collect::<Box<[_]>>();
        Ok(Self {
            arena,
            bucket_bits: buckets.len().trailing_zeros(),
            buckets,
            capacity,
            turnover: Padded(Turnover {
                free: Mutex::new(free),
                hand: Mutex::new(0),
                unwritten: Mutex::new(Unwritten::default()),
                overflow: Counter::default(),
                attached: Counter::default(),
                evictions: Counter::default(),
                overflows: Counter::default(),
            }),
        })
    }

    /// The bytes of one frame: the store's page size.
    pub fn frame_len(&self) -> usize {
        self.arena.frame_len()
    }

    /// The number of frames of the fixed part.
    pub fn frames(&self) -> usize {
        self.arena.len()
    }

    /// The configured size in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The bytes the fixed part takes. It is at most [`Self::capacity`].
    pub fn fixed_memory(&self) -> usize {
        Self::cost(
            self.arena.len(),
            self.arena.frame_len(),
            self.buckets.len(),
            self.arena.lanes().len(),
        )
        .expect("it was allocated")
    }

    /// The bytes the pool has allocated now: the fixed part, every frame
    /// beside it, and every attached value. A value that several frames
    /// share is counted once for each.
    ///
    /// What is beside the fixed part belongs to frames that cannot be
    /// evicted, so this never exceeds the configured size plus those
    /// frames. The lists of unwritten pages ([`Self::unwritten_pages`]),
    /// four bytes a page, are not counted.
    pub fn memory(&self) -> usize {
        self.fixed_memory()
            + self.turnover.overflow.get() * self.overflow_cost()
            + self.turnover.attached.get()
    }

    /// The bytes one frame beside the fixed part takes.
    pub fn overflow_cost(&self) -> usize {
        self.arena.frame_len() + Arena::HEADER_LEN
    }

    /// The frames allocated beside the fixed part now.
    pub fn overflow_frames(&self) -> usize {
        self.turnover.overflow.get()
    }

    /// The bytes of the values attached to frames now.
    pub fn attached_bytes(&self) -> usize {
        self.turnover.attached.get()
    }

    /// What the pool has done since it was made.
    pub fn stats(&self) -> PoolStats {
        let (hits, misses) = self.arena.lanes().counts();
        PoolStats {
            hits,
            misses,
            evictions: self.turnover.evictions.get() as u64,
            overflows: self.turnover.overflows.get() as u64,
        }
    }

    /// Counts what the pool holds, by walking all of it. Not for a path
    /// that has to be fast.
    pub fn census(&self) -> Census {
        let mut census = Census::default();
        for bucket in &self.buckets {
            let chain = bucket.chain.read();
            for entry in chain.iter() {
                let frame = entry.frame();
                let pinned = !self.arena.has_no_pin(frame);
                let unevictable = pinned || !evictable(frame);
                match frame.kind() {
                    kind::PUBLISHED => census.published += 1,
                    kind::PRIVATE => census.private += 1,
                    _ => {}
                }
                census.pinned += usize::from(pinned);
                census.unwritten += usize::from(frame.has(flag::UNWRITTEN));
                census.held += usize::from(frame.has(flag::HELD));
                census.unevictable += usize::from(unevictable);
                if frame.is_overflow() {
                    census.overflow += 1;
                    census.overflow_unevictable += usize::from(unevictable);
                }
            }
        }
        census
    }

    pub(super) fn bucket(&self, page: u32) -> &Bucket {
        let hash = u64::from(page).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let index = match self.bucket_bits {
            0 => 0,
            bits => (hash >> (64 - bits)) as usize,
        };
        &self.buckets[index]
    }

    pub(super) fn count_attached(&self, bytes: usize) {
        self.turnover.attached.add(bytes);
    }

    pub(super) fn count_detached(&self, bytes: usize) {
        self.turnover.attached.sub(bytes);
    }

    /// The version of `page` that a reader at version `at` sees
    /// (`transactions.md` PV1), pinned.
    ///
    /// A hit allocates nothing, takes no latch, and waits for nothing but
    /// one step of another thread on the same part of the pool's table. A
    /// miss returns what [`Self::install_loaded`] needs once the page has
    /// been read from its segment.
    ///
    /// A hit on a page in one of the fixed frames takes no lock either,
    /// and writes nothing that another reader of the page writes: the pin
    /// is announced in a lane of the calling thread's own. The lock of the
    /// part of the table is taken for a miss, for a page in a frame beside
    /// the fixed ones, while that part is being changed, and by a thread
    /// that holds more pins at once than its lane has cells.
    pub fn get(&self, page: PageId, at: CommitVersion) -> Result<PageRef<'_>, Miss> {
        self.get_while(page, at, |_| {})
    }

    /// [`Self::get`], with `meanwhile` told the moments of a read that
    /// takes no lock: for a test that changes the pool under the reader at
    /// one of them.
    pub(super) fn get_while(
        &self,
        page: PageId,
        at: CommitVersion,
        meanwhile: impl FnMut(Moment),
    ) -> Result<PageRef<'_>, Miss> {
        let bucket = self.bucket(page.get());
        let wanted = |frame: &Frame| stands_for(frame, page, at);
        match bucket.chain.pin_unlocked(&self.arena, wanted, meanwhile) {
            Unlocked::Found(frozen) => {
                frozen.frame().touch();
                self.arena.lanes().count_hit();
                return Ok(PageRef::new(self, frozen));
            }
            Unlocked::Not => {}
            Unlocked::Dead => self.release_dead(),
        }
        let chain = bucket.chain.read();
        let found = lookup(&chain, page, at).map(|entry| {
            entry.frame().touch();
            entry
                .freeze(&self.arena)
                .expect("the lookup gives page versions")
        });
        drop(chain);
        match found {
            Ok(frozen) => {
                self.arena.lanes().count_hit();
                Ok(PageRef::new(self, frozen))
            }
            Err(miss) => {
                self.arena.lanes().count_miss();
                Err(miss)
            }
        }
    }

    /// The page version of `page` that the commit `born` bore, pinned, if
    /// it is in the pool.
    pub fn get_version(&self, page: PageId, born: CommitVersion) -> Option<PageRef<'_>> {
        let chain = self.bucket(page.get()).chain.read();
        let frozen = published(&chain, page)
            .find(|entry| entry.frame().born() == born.get())
            .and_then(|entry| entry.freeze(&self.arena));
        drop(chain);
        frozen.map(|frozen| PageRef::new(self, frozen))
    }

    /// A frame to fill: for a page that is being loaded
    /// ([`Self::install_loaded`]), or for a page step
    /// ([`PageStep::replace`](super::PageStep::replace)).
    ///
    /// If every frame is in use and none can be evicted, the frame is
    /// allocated beside the fixed ones.
    pub fn blank(&self) -> Blank<'_> {
        Blank::new(self, self.acquire())
    }

    /// Puts a page that was read from its segments into the pool, and
    /// returns it pinned.
    ///
    /// `blank` holds the page as the commit `born` left it: the newest
    /// version of the page at or before the version `miss` was for, as the
    /// page table gives it (`page-table.md` T5). `next` is the version of
    /// the link that follows it in the page table's chain, of whatever kind,
    /// if there is one. Both are read from the page table after the miss.
    ///
    /// From those and from what it holds itself, the pool knows for which
    /// versions the page stands. When another thread loaded the same page
    /// version first, that frame is returned and `blank` is given back.
    ///
    /// [`Loaded::Again`] says that a commit or a page step changed this part
    /// of the pool's table since the miss, so that what was read may be out
    /// of date. Nothing was installed. The caller asks [`Self::get`] again,
    /// and if that misses, reads the page table again and fills the frame
    /// again.
    ///
    /// It fails if what it is told cannot be: the page was born after the
    /// version it was asked at ([`CacheError::LoadedTooNew`]), a newer
    /// version is known at or before that version
    /// ([`CacheError::LoadedSuperseded`]), `blank` has attached values,
    /// which no segment holds ([`CacheError::AttachedValues`]), or `blank`
    /// is a frame of another pool ([`CacheError::WrongPool`]).
    pub fn install_loaded<'p>(
        &'p self,
        blank: Blank<'p>,
        miss: Miss,
        born: CommitVersion,
        next: Option<CommitVersion>,
    ) -> Result<Loaded<'p>, CacheError> {
        let Miss { page, at, .. } = miss;
        if !blank.is_of(self) {
            return Err(CacheError::WrongPool);
        }
        if born > at {
            return Err(CacheError::LoadedTooNew { page, born, at });
        }
        if blank.has_attached() {
            return Err(CacheError::AttachedValues { page, born });
        }
        let superseded = |superseded: u64| CacheError::LoadedSuperseded {
            page,
            born,
            superseded: CommitVersion::new(superseded).expect("a version"),
            at,
        };
        let mut bound = next.map_or(NO_VERSION, CommitVersion::get).min(miss.newer);
        if bound <= at.get() {
            return Err(superseded(bound));
        }

        let bucket = self.bucket(page.get());
        let step = lock(&bucket.step);
        let mut chain = bucket.chain.write();
        if chain.stamp() != miss.stamp {
            return Ok(Loaded::Again(blank));
        }
        let mut same = None;
        for entry in published(&chain, page) {
            let other = entry.frame().born();
            if other == born.get() {
                same = Some(entry);
            } else if other > born.get() {
                bound = bound.min(other);
            }
        }
        if bound <= at.get() {
            return Err(superseded(bound));
        }
        let frozen = match same {
            Some(entry) => {
                entry.frame().touch();
                entry.freeze(&self.arena)
            }
            None => {
                let frame = blank.into_frame();
                let header = frame.header();
                header.set_page_raw(page.get());
                header.set_born(born.get());
                header.set_superseded(bound);
                header.set_flags(0);
                header.touch();
                frame.set_kind(kind::PUBLISHED);
                chain.link(&self.arena, frame).freeze(&self.arena)
            }
        };
        drop(chain);
        drop(step);
        Ok(Loaded::Page(PageRef::new(
            self,
            frozen.expect("the frame is a page version"),
        )))
    }

    /// A frame nobody else can reach: a free one, one that held a page
    /// version nobody needs in memory, a dead one whose last pin is gone, or
    /// a new one beside the fixed ones.
    pub(super) fn acquire(&self) -> OwnedFrame {
        let free = lock(&self.turnover.free).pop();
        let frame = free
            .or_else(|| self.evict())
            .or_else(|| self.take_dead())
            .unwrap_or_else(|| {
                self.turnover.overflow.add(1);
                self.turnover.overflows.add(1);
                self.arena.allocate_overflow()
            });
        frame.set_kind(kind::BLANK);
        frame
    }

    /// The clock: goes round the fixed frames, takes the mark off each one
    /// that was used since it last passed, and evicts the first that was
    /// not and that nothing keeps in memory.
    fn evict(&self) -> Option<OwnedFrame> {
        let mut hand = lock(&self.turnover.hand);
        let count = self.arena.len();
        for _ in 0..2 * count {
            let index = *hand;
            *hand = (index + 1) % count;
            let frame = self.arena.frame(index);
            let page = frame.page_raw();
            if page == 0 || !evictable(frame) || !frame.is_unpinned() {
                continue;
            }
            if frame.take_referenced() {
                continue;
            }
            let mut chain = self.bucket(page).chain.write();
            let taken = chain.take_unpinned(&self.arena, self.arena.pointer(index), evictable);
            drop(chain);
            if let Some(mut frame) = taken {
                self.turnover.evictions.add(1);
                self.reset(&mut frame);
                return Some(frame);
            }
            // The frame has a pin after all. It keeps its mark, as a
            // frame with a counted pin does, so the hand passes it once
            // more before this part of the table is locked for it again.
            frame.touch();
        }
        None
    }

    fn reset(&self, frame: &mut OwnedFrame) {
        let attached = frame.reset();
        self.turnover.attached.sub(attached);
    }

    /// A dead frame whose last pin is gone, made free.
    fn take_dead(&self) -> Option<OwnedFrame> {
        let mut frame = self.arena.take_dead()?;
        self.reset(&mut frame);
        Some(frame)
    }

    /// Puts every dead frame whose last pin is gone on the free list.
    fn release_dead(&self) {
        while let Some(frame) = self.arena.take_dead() {
            self.release(frame);
        }
    }

    /// Takes back a frame nobody else can reach.
    pub(super) fn release(&self, mut frame: OwnedFrame) {
        assert!(self.arena.owns(&frame), "a frame of another pool");
        self.reset(&mut frame);
        if frame.header().is_overflow() {
            self.arena.free_overflow(frame);
            self.turnover.overflow.sub(1);
        } else {
            lock(&self.turnover.free).push(frame);
        }
    }

    /// Gives a pin back. A frame beside the fixed ones leaves the pool here
    /// if nothing else keeps it, and so does a frame that was retired, if
    /// this was its last pin.
    pub(super) fn unpin(&self, pin: Pinned<'_>) {
        if !pin.is_announced() && pin.frame().is_overflow() {
            let chain = &self.bucket(pin.frame().page_raw()).chain;
            // The lock is given back at the end of this statement, before
            // the frame is.
            let freed = chain.write().unpin_linked(&self.arena, pin, evictable);
            if let Some(frame) = freed {
                self.release(frame);
            }
            return;
        }
        match pin.unpin() {
            Unpinned::Done => {}
            Unpinned::Owned(frame) => self.release(frame),
            Unpinned::Dead => self.release_dead(),
        }
    }
}

impl Drop for BufferPool {
    fn drop(&mut self) {
        for bucket in self.buckets.iter_mut() {
            for frame in bucket.chain.take_all(&mut self.arena) {
                if frame.header().is_overflow() {
                    self.arena.free_overflow(frame);
                }
            }
        }
    }
}
