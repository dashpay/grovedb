//! A table of announcements: each holder writes one number where everyone
//! can read it, and a scan gives the smallest.
//!
//! Version pins announce the version they pin, and operation guards the
//! epoch they were taken in. Claiming a cell, changing what it says and
//! giving it back are each one atomic write to a cell that only its holder
//! writes. None of that takes a lock or waits.
//!
//! The one place a thread can wait is growth. When every cell is claimed,
//! a chunk of cells is linked behind the last through a `OnceLock`, and a
//! thread that links one can wait there for another thread that is linking
//! the same chunk, for as long as it takes to store a pointer. The table
//! never shrinks, so that happens a few times in the life of a database.

use std::sync::OnceLock;

use super::sync::{atomic, AtomicU64, Ordering, Padded};

/// What a cell holds while nobody has claimed it. No announcement is this
/// number: a commit version is below it, and so is every epoch.
pub(super) const VACANT: u64 = u64::MAX;

/// The cells of the first chunk. Each chunk after it has twice the cells of
/// the one before.
#[cfg(not(loom))]
pub(super) const FIRST_CHUNK: usize = 256;
#[cfg(loom)]
pub(super) const FIRST_CHUNK: usize = 2;

/// The cells between the places where two threads start to look for a
/// vacant one, so that a thread that holds a few claims at once stays among
/// cells no other thread starts at.
#[cfg(not(loom))]
const SPREAD: usize = 8;

struct Chunk {
    cells: Box<[Padded<AtomicU64>]>,
    next: OnceLock<Box<Chunk>>,
}

impl Chunk {
    fn new(len: usize) -> Self {
        Self {
            cells: (0..len).map(|_| Padded(atomic(VACANT))).collect(),
            next: OnceLock::new(),
        }
    }
}

/// Where the calling thread starts to look for a vacant cell.
#[cfg(not(loom))]
fn home() -> usize {
    use std::sync::atomic::AtomicUsize;

    static THREADS: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static HOME: usize = THREADS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_mul(SPREAD);
    }
    HOME.try_with(|home| *home).unwrap_or(0)
}

#[cfg(loom)]
fn home() -> usize {
    0
}

/// The cell of the first chunk that the calling thread starts at, for the
/// measurement of the readers (`tests::scaling`).
#[cfg(all(test, not(loom)))]
pub(super) fn home_of_this_thread() -> usize {
    home() % FIRST_CHUNK
}

/// The table. It grows by a chunk when every cell is claimed, and never
/// shrinks.
pub(super) struct Slots {
    first: Chunk,
    /// How many chunks are linked behind the first. A claim counts a chunk
    /// here before it claims a cell in it, and a scan reads the count
    /// before it reads a cell, both `SeqCst`. A chunk's link is not: it is
    /// a `OnceLock`. So the count is what orders a claim in a new chunk
    /// before the scans that have to see it.
    grown: AtomicU64,
}

impl Slots {
    pub(super) fn new() -> Self {
        Self {
            first: Chunk::new(FIRST_CHUNK),
            grown: atomic(0),
        }
    }

    /// Claims a vacant cell and announces `value` in it, in one step.
    pub(super) fn claim(&self, value: u64) -> Slot<'_> {
        debug_assert_ne!(value, VACANT);
        let home = home();
        let mut chunk = &self.first;
        let mut behind_first = 0;
        loop {
            let len = chunk.cells.len();
            for probe in 0..len {
                let cell = &chunk.cells[home.wrapping_add(probe) % len].0;
                if cell.load(Ordering::SeqCst) == VACANT
                    && cell
                        .compare_exchange(VACANT, value, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    return Slot { cell };
                }
            }
            if chunk.next.get().is_none() {
                let _ = chunk.next.set(Box::new(Chunk::new(2 * len)));
            }
            chunk = chunk.next.get().expect("a chunk was linked just now");
            behind_first += 1;
            self.grown.fetch_max(behind_first, Ordering::SeqCst);
        }
    }

    /// Claims a cell that announces what `source` holds, and returns the
    /// cell with the announced number once `source` has been found to hold
    /// that number still: announce, then look again. If `source` has moved
    /// on in between, its newer number is announced and looked at again,
    /// and the last part of the answer says that an older announcement was
    /// withdrawn that way.
    ///
    /// So the number returned was the one in `source` at a moment after it
    /// was announced. That is what makes an answer of [`Self::min`] stay
    /// true when it was taken after a read of `source`.
    pub(super) fn claim_current(&self, source: &AtomicU64) -> (Slot<'_>, u64, bool) {
        let mut announced = source.load(Ordering::SeqCst);
        let slot = self.claim(announced);
        let mut withdrew = false;
        loop {
            let now = source.load(Ordering::SeqCst);
            if now == announced {
                return (slot, announced, withdrew);
            }
            announced = now;
            slot.announce(announced);
            withdrew = true;
        }
    }

    /// The cells of every chunk that was counted before this was called.
    fn cells(&self) -> impl Iterator<Item = &AtomicU64> {
        let chunks = 1 + self.grown.load(Ordering::SeqCst) as usize;
        std::iter::successors(Some(&self.first), |chunk| {
            chunk.next.get().map(|next| &**next)
        })
        .take(chunks)
        .flat_map(|chunk| chunk.cells.iter().map(|cell| &cell.0))
    }

    /// The smallest number announced in a cell at the moment the scan
    /// passes it, or [`VACANT`] if it passes none.
    pub(super) fn min(&self) -> u64 {
        self.cells()
            .map(|cell| cell.load(Ordering::SeqCst))
            .min()
            .unwrap_or(VACANT)
    }

    /// How many cells are claimed, each counted as the scan passes it.
    pub(super) fn claimed(&self) -> usize {
        self.cells()
            .filter(|cell| cell.load(Ordering::SeqCst) != VACANT)
            .count()
    }

    /// How many cells the table has.
    #[cfg(all(test, not(loom)))]
    pub(super) fn capacity(&self) -> usize {
        self.cells().count()
    }
}

/// A claimed cell. Its holder alone writes it, until it vacates it.
pub(super) struct Slot<'s> {
    cell: &'s AtomicU64,
}

impl Slot<'_> {
    /// Replaces what the cell announces.
    pub(super) fn announce(&self, value: u64) {
        debug_assert_ne!(value, VACANT);
        self.cell.store(value, Ordering::SeqCst);
    }

    /// Gives the cell back. The holder does not use it after this.
    pub(super) fn vacate(&self) {
        self.cell.store(VACANT, Ordering::SeqCst);
    }
}
