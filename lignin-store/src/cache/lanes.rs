//! Lanes: what each thread writes for itself, so that a reader of a page
//! that every reader reads writes no cache line another reader writes.
//!
//! A lane is two cache lines: the cells in which a thread announces the
//! frames it has pinned without counting, and its counts of hits and
//! misses. A thread uses the lane of its number. The numbers of threads
//! that have ended are given out again, so two threads share a lane only
//! when more threads run at one time than the pool has lanes.
//!
//! This file has no unsafe code, and the access rules of `frame.rs` rest on
//! it as they rest on a lock (A6): an announcement that its announcer has
//! confirmed is a pin, and [`Lanes::announces`] finds every announcement
//! that was made before it looked.

use super::sync::{
    settled_u32, settled_usize, thread_number, AtomicU32, AtomicUsize, Counter, Ordering,
};

/// The cells of one lane: with its two counts they fill 128 bytes.
#[cfg(not(loom))]
pub(super) const CELLS: usize = 28;
#[cfg(loom)]
pub(super) const CELLS: usize = 2;

/// What a cell holds while nobody has claimed it. A claimed cell holds the
/// number of a frame of the arena, plus one.
const VACANT: u32 = 0;

#[repr(align(128))]
struct Lane {
    hits: Counter,
    misses: Counter,
    cells: [AtomicU32; CELLS],
}

/// The lanes of one pool. Their number is fixed when the pool is made.
pub(super) struct Lanes {
    lanes: Box<[Lane]>,
    /// How many lanes, from the first, have ever been announced in. It only
    /// goes up.
    used: AtomicUsize,
}

impl std::fmt::Debug for Lanes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lanes")
            .field("lanes", &self.lanes.len())
            .finish_non_exhaustive()
    }
}

/// One announcement: a claimed cell that names a frame. Its holder alone
/// writes the cell until it withdraws.
#[derive(Debug)]
pub(super) struct Announcement<'l>(&'l AtomicU32);

impl Announcement<'_> {
    /// Takes the announcement back and gives the cell up.
    ///
    /// The store is `SeqCst`, as is the look at the frame that follows it:
    /// whoever retires the frame marks it dead and then looks for
    /// announcements, so one of the two sees the other, and a dead frame
    /// whose last pin this was does not stay unnoticed.
    pub(super) fn withdraw(self) {
        self.0.store(VACANT, Ordering::SeqCst);
    }
}

impl Lanes {
    /// The bytes one lane takes.
    pub(super) const LANE_LEN: usize = size_of::<Lane>();

    /// The most lanes a pool has.
    const MOST: usize = 128;

    /// How many lanes a pool of `frames` frames has: a power of two, at
    /// most one for every two frames, so that they take a fixed share of a
    /// small pool, and no more than [`Self::MOST`].
    pub(super) fn count_for(frames: usize) -> usize {
        let most = (frames / 2).clamp(1, Self::MOST);
        1 << most.ilog2()
    }

    pub(super) fn new(count: usize) -> Self {
        assert!(count.is_power_of_two(), "a thread finds its lane by a mask");
        Self {
            lanes: (0..count)
                .map(|_| Lane {
                    hits: Counter::default(),
                    misses: Counter::default(),
                    cells: std::array::from_fn(|_| settled_u32(VACANT)),
                })
                .collect(),
            used: settled_usize(0),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.lanes.len()
    }

    /// The number of the calling thread's lane.
    fn mine(&self) -> usize {
        thread_number() & (self.lanes.len() - 1)
    }

    /// Announces the frame of number `frame` in a cell of the calling
    /// thread's lane, or returns `None` if every cell of the lane is
    /// claimed.
    ///
    /// The announcement is one `SeqCst` step, and the lane was counted as
    /// used, `SeqCst`, before it. So whoever looks for announcements after
    /// a `SeqCst` store of its own, and misses this one, made that store
    /// before this announcement: the announcer finds it when it looks next.
    pub(super) fn announce(&self, frame: u32) -> Option<Announcement<'_>> {
        let mine = self.mine();
        if self.used.load(Ordering::SeqCst) <= mine {
            self.used.fetch_max(mine + 1, Ordering::SeqCst);
        }
        let word = frame.checked_add(1).expect("a frame of the arena");
        self.lanes[mine]
            .cells
            .iter()
            .find(|cell| {
                cell.load(Ordering::Relaxed) == VACANT
                    && cell
                        .compare_exchange(VACANT, word, Ordering::SeqCst, Ordering::Relaxed)
                        .is_ok()
            })
            .map(Announcement)
    }

    /// Whether a cell announces the frame of number `frame` at the moment
    /// this looks at it.
    pub(super) fn announces(&self, frame: u32) -> bool {
        let word = frame.checked_add(1).expect("a frame of the arena");
        let used = self.used.load(Ordering::SeqCst).min(self.lanes.len());
        self.lanes[..used].iter().any(|lane| {
            lane.cells
                .iter()
                .any(|cell| cell.load(Ordering::SeqCst) == word)
        })
    }

    /// Counts a hit for the calling thread.
    pub(super) fn count_hit(&self) {
        self.lanes[self.mine()].hits.add(1);
    }

    /// Counts a miss for the calling thread.
    pub(super) fn count_miss(&self) {
        self.lanes[self.mine()].misses.add(1);
    }

    /// The hits and the misses of every thread.
    pub(super) fn counts(&self) -> (u64, u64) {
        self.lanes.iter().fold((0, 0), |(hits, misses), lane| {
            (
                hits + lane.hits.get() as u64,
                misses + lane.misses.get() as u64,
            )
        })
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn a_lane_is_two_cache_lines_of_its_own() {
        assert_eq!((size_of::<Lane>(), align_of::<Lane>()), (128, 128));
    }

    #[test]
    fn a_cell_is_claimed_by_one_announcement_at_a_time() {
        let lanes = Lanes::new(2);
        assert!(!lanes.announces(7));
        let held: Vec<_> = std::iter::from_fn(|| lanes.announce(7)).collect();
        assert_eq!(held.len(), CELLS, "a lane has no more cells");
        assert!(lanes.announces(7) && !lanes.announces(6) && !lanes.announces(8));
        let mut held = held.into_iter();
        held.next().expect("a cell").withdraw();
        assert!(lanes.announces(7));
        let other = lanes.announce(0).expect("the cell that was given back");
        assert!(lanes.announces(0));
        assert!(lanes.announce(1).is_none());
        held.for_each(Announcement::withdraw);
        assert!(!lanes.announces(7) && lanes.announces(0));
        other.withdraw();
        assert!(!lanes.announces(0));
    }

    #[test]
    fn each_thread_counts_for_itself_and_the_counts_add_up() {
        let lanes = Lanes::new(4);
        std::thread::scope(|scope| {
            for _ in 0..6 {
                scope.spawn(|| {
                    for _ in 0..1_000 {
                        lanes.count_hit();
                    }
                    lanes.count_miss();
                });
            }
        });
        assert_eq!(lanes.counts(), (6_000, 6));
    }

    #[test]
    fn an_announcement_in_any_lane_is_found() {
        let lanes = Lanes::new(4);
        std::thread::scope(|scope| {
            for frame in 0..16 {
                let lanes = &lanes;
                scope.spawn(move || {
                    let held = lanes.announce(frame).expect("a cell");
                    assert!(lanes.announces(frame));
                    held.withdraw();
                });
            }
        });
        assert!((0..16).all(|frame| !lanes.announces(frame)));
    }
}
