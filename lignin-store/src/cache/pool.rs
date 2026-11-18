//! The pool: its frames, its table of what they hold, the read path, the
//! load path and eviction.

use std::{collections::BTreeMap, fmt};

use lignin_format::{tree_page::TreePageSize, CommitVersion, PageId};

use super::{
    error::CacheError,
    frame::{flag, kind, Arena, Chain, Entry, Frame, OwnedFrame, Pinned, NO_VERSION},
    guard::{Blank, PageRef},
    sync::{lock, read, write, Counter, Mutex, RwLock},
};
use crate::alloc::Epoch;

/// How a pool is sized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// The store's page size. A frame holds one page of it.
    pub page_size: TreePageSize,
    /// The memory the pool may use for what it caches, in bytes. It is
    /// allocated when the pool is made, and it includes each frame's header
    /// and the pool's table.
    pub capacity: usize,
}

/// A page version that was asked for and is not in the pool
/// ([`BufferPool::get`]).
///
/// It is what the pool knew at that moment, which
/// [`BufferPool::install_loaded`] needs to say for which versions the page
/// that is then loaded stands. It is good for that one load.
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

pub(super) struct Bucket {
    /// What makes a page step one step (`transactions.md` PV7, LT7 level 5).
    pub(super) step: Mutex<()>,
    pub(super) chain: RwLock<Chain>,
    hits: Counter,
    misses: Counter,
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

/// The buffer pool. See the [module documentation](super).
pub struct BufferPool {
    pub(super) arena: Arena,
    pub(super) buckets: Box<[Bucket]>,
    bucket_bits: u32,
    pub(super) free: Mutex<Vec<OwnedFrame>>,
    hand: Mutex<usize>,
    pub(super) unwritten: Mutex<Unwritten>,
    capacity: usize,
    overflow: Counter,
    attached: Counter,
    evictions: Counter,
    overflows: Counter,
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

pub(super) fn published(chain: &Chain, page: PageId) -> impl Iterator<Item = Entry<'_>> {
    chain.iter().filter(move |entry| {
        entry.frame().page_raw() == page.get() && entry.frame().kind() == kind::PUBLISHED
    })
}

pub(super) fn privates(chain: &Chain, page: PageId) -> impl Iterator<Item = Entry<'_>> {
    chain.iter().filter(move |entry| {
        entry.frame().page_raw() == page.get() && entry.frame().kind() == kind::PRIVATE
    })
}

pub(super) fn current(chain: &Chain, page: PageId) -> Option<Entry<'_>> {
    published(chain, page).find(|entry| entry.frame().superseded() == NO_VERSION)
}

/// The page version of `page` that a reader at `at` sees, if it is in the
/// chain. Otherwise what the chain knows for the load.
pub(super) fn lookup(chain: &Chain, page: PageId, at: CommitVersion) -> Result<Entry<'_>, Miss> {
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
        stamp: chain.stamp,
        newer,
    })
}

impl BufferPool {
    /// The bytes one frame of the fixed part takes: a page, its header, its
    /// place on the free list and its share of the table.
    fn frame_cost(frame_len: usize) -> usize {
        frame_len + Arena::HEADER_LEN + size_of::<OwnedFrame>() + 4 * size_of::<Bucket>()
    }

    /// A pool of as many frames as `config.capacity` bytes hold.
    ///
    /// All of that memory is allocated here. The pool allocates more only
    /// for a frame that it needs while no frame can be evicted, and gives
    /// that back as soon as the frame could be.
    pub fn new(config: PoolConfig) -> Result<Self, CacheError> {
        let frame_len = config.page_size.get();
        let frame = Self::frame_cost(frame_len);
        let count = config.capacity / frame;
        if count == 0 {
            return Err(CacheError::CapacityTooSmall {
                capacity: config.capacity,
                frame,
            });
        }
        let buckets = count
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(CacheError::CapacityTooLarge)?;
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
                chain: RwLock::new(Chain::new()),
                hits: Counter::default(),
                misses: Counter::default(),
            })
            .collect::<Box<[_]>>();
        Ok(Self {
            arena,
            bucket_bits: buckets.len().trailing_zeros(),
            buckets,
            free: Mutex::new(free),
            hand: Mutex::new(0),
            unwritten: Mutex::new(Unwritten::default()),
            capacity,
            overflow: Counter::default(),
            attached: Counter::default(),
            evictions: Counter::default(),
            overflows: Counter::default(),
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
        self.arena.len() * (self.arena.frame_len() + Arena::HEADER_LEN + size_of::<OwnedFrame>())
            + self.buckets.len() * size_of::<Bucket>()
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
        self.fixed_memory() + self.overflow.get() * self.overflow_cost() + self.attached.get()
    }

    /// The bytes one frame beside the fixed part takes.
    pub fn overflow_cost(&self) -> usize {
        self.arena.frame_len() + Arena::HEADER_LEN
    }

    /// The frames allocated beside the fixed part now.
    pub fn overflow_frames(&self) -> usize {
        self.overflow.get()
    }

    /// The bytes of the values attached to frames now.
    pub fn attached_bytes(&self) -> usize {
        self.attached.get()
    }

    /// What the pool has done since it was made.
    pub fn stats(&self) -> PoolStats {
        let sum = |count: fn(&Bucket) -> usize| -> u64 {
            self.buckets.iter().map(|bucket| count(bucket) as u64).sum()
        };
        PoolStats {
            hits: sum(|bucket| bucket.hits.get()),
            misses: sum(|bucket| bucket.misses.get()),
            evictions: self.evictions.get() as u64,
            overflows: self.overflows.get() as u64,
        }
    }

    /// Counts what the pool holds, by walking all of it. Not for a path
    /// that has to be fast.
    pub fn census(&self) -> Census {
        let mut census = Census::default();
        for bucket in &self.buckets {
            let chain = read(&bucket.chain);
            for entry in chain.iter() {
                let frame = entry.frame();
                let pinned = frame.pins() > 0;
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
        self.attached.add(bytes);
    }

    pub(super) fn count_detached(&self, bytes: usize) {
        self.attached.sub(bytes);
    }

    /// The version of `page` that a reader at version `at` sees
    /// (`transactions.md` PV1), pinned.
    ///
    /// A hit allocates nothing, takes no latch, and waits for nothing but
    /// one step of another thread on the same part of the pool's table. A
    /// miss returns what [`Self::install_loaded`] needs once the page has
    /// been read from its segment.
    pub fn get(&self, page: PageId, at: CommitVersion) -> Result<PageRef<'_>, Miss> {
        let bucket = self.bucket(page.get());
        let chain = read(&bucket.chain);
        let found = lookup(&chain, page, at).map(|entry| {
            entry.frame().touch();
            entry
                .freeze(&self.arena)
                .expect("the lookup gives page versions")
        });
        drop(chain);
        match found {
            Ok(frozen) => {
                bucket.hits.add(1);
                Ok(PageRef::new(self, frozen))
            }
            Err(miss) => {
                bucket.misses.add(1);
                Err(miss)
            }
        }
    }

    /// The page version of `page` that the commit `born` bore, pinned, if
    /// it is in the pool.
    pub fn get_version(&self, page: PageId, born: CommitVersion) -> Option<PageRef<'_>> {
        let chain = read(&self.bucket(page.get()).chain);
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

    /// Puts a page that was read from its segment into the pool, and returns
    /// it pinned.
    ///
    /// `blank` holds the page as the commit `born` left it: the newest
    /// version of the page at or before the version `miss` was for, as the
    /// page table gives it (`page-table.md` T5). `next` is the version of
    /// the link that follows it in the page table's chain, of whatever kind,
    /// if there is one. Both are read from the page table after the miss.
    ///
    /// From those and from what it holds itself, the pool works out for
    /// which versions the page stands. When another thread loaded the same
    /// page version first, that frame is returned and `blank` is given back.
    ///
    /// It fails if what it is told cannot be: the page was born after the
    /// version it was asked at ([`CacheError::LoadedTooNew`]), a newer
    /// version is known at or before that version
    /// ([`CacheError::LoadedSuperseded`]), or `blank` has attached values,
    /// which no segment holds ([`CacheError::AttachedValues`]).
    pub fn install_loaded<'p>(
        &'p self,
        blank: Blank<'p>,
        miss: Miss,
        born: CommitVersion,
        next: Option<CommitVersion>,
    ) -> Result<PageRef<'p>, CacheError> {
        let Miss { page, at, .. } = miss;
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
        let mut chain = write(&bucket.chain);
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
        if bound == NO_VERSION && chain.stamp != miss.stamp {
            bound = at.get().saturating_add(1);
        }
        let frozen = match same {
            Some(entry) => {
                if entry.frame().superseded() < bound {
                    entry.frame().set_superseded(bound);
                }
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
                header.set_kind(kind::PUBLISHED);
                header.touch();
                chain.link(frame).freeze(&self.arena)
            }
        };
        drop(chain);
        drop(step);
        Ok(PageRef::new(
            self,
            frozen.expect("the frame is a page version"),
        ))
    }

    /// A frame nobody else can reach: a free one, one that held a page
    /// version nobody needs in memory, or a new one beside the fixed ones.
    pub(super) fn acquire(&self) -> OwnedFrame {
        let free = lock(&self.free).pop();
        let frame = free.or_else(|| self.evict()).unwrap_or_else(|| {
            self.overflow.add(1);
            self.overflows.add(1);
            self.arena.allocate_overflow()
        });
        frame.header().set_kind(kind::BLANK);
        frame
    }

    /// The clock: goes round the fixed frames, takes the mark off each one
    /// that was used since it last passed, and evicts the first that was
    /// not and that nothing keeps in memory.
    fn evict(&self) -> Option<OwnedFrame> {
        let mut hand = lock(&self.hand);
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
            let mut chain = write(&self.bucket(page).chain);
            let taken = chain.take_unpinned(self.arena.pointer(index), evictable);
            drop(chain);
            if let Some(mut frame) = taken {
                self.evictions.add(1);
                self.reset(&mut frame);
                return Some(frame);
            }
        }
        None
    }

    fn reset(&self, frame: &mut OwnedFrame) {
        let attached = frame.reset();
        self.attached.sub(attached);
    }

    /// Takes back a frame nobody else can reach.
    pub(super) fn release(&self, mut frame: OwnedFrame) {
        self.reset(&mut frame);
        if frame.header().is_overflow() {
            self.arena.free_overflow(frame);
            self.overflow.sub(1);
        } else {
            lock(&self.free).push(frame);
        }
    }

    /// Gives a pin back. A frame beside the fixed ones leaves the pool here
    /// if nothing else keeps it.
    pub(super) fn unpin(&self, pin: Pinned<'_>) {
        let freed = if pin.frame().is_overflow() {
            let page = pin.frame().page_raw();
            let mut chain = write(&self.bucket(page).chain);
            chain.unpin_linked(pin, evictable)
        } else {
            pin.unpin()
        };
        if let Some(frame) = freed {
            self.release(frame);
        }
    }
}

impl Drop for BufferPool {
    fn drop(&mut self) {
        for bucket in self.buckets.iter_mut() {
            let chain = match bucket.chain.get_mut() {
                Ok(chain) => chain,
                Err(poisoned) => poisoned.into_inner(),
            };
            for frame in chain.take_all(&mut self.arena) {
                if frame.header().is_overflow() {
                    self.arena.free_overflow(frame);
                }
            }
        }
    }
}
