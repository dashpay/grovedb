//! A table of announcements: each holder writes one number where everyone
//! can read it, and a scan gives the smallest.
//!
//! Version pins announce the version they pin, and operation guards the
//! epoch they were taken in. Claiming a cell, changing what it says and
//! giving it back are each one atomic write to a cell that only its holder
//! writes. Nothing here takes a lock or waits.

use std::sync::OnceLock;

use super::sync::{atomic, AtomicU64, Ordering, Padded};

/// What a cell holds while nobody has claimed it. No announcement is this
/// number: a commit version is below it, and so is every epoch.
pub(super) const VACANT: u64 = u64::MAX;

/// The cells of the first chunk. Each chunk after it has twice the cells of
/// the one before.
#[cfg(not(loom))]
const FIRST_CHUNK: usize = 256;
#[cfg(loom)]
const FIRST_CHUNK: usize = 2;

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

/// The table. It grows by a chunk when every cell is claimed, and never
/// shrinks.
pub(super) struct Slots {
    first: Chunk,
}

impl Slots {
    pub(super) fn new() -> Self {
        Self {
            first: Chunk::new(FIRST_CHUNK),
        }
    }

    /// Claims a vacant cell and announces `value` in it, in one step.
    pub(super) fn claim(&self, value: u64) -> Slot<'_> {
        debug_assert_ne!(value, VACANT);
        let home = home();
        let mut chunk = &self.first;
        loop {
            let len = chunk.cells.len();
            for probe in 0..len {
                let cell = &chunk.cells[home.wrapping_add(probe) % len].0;
                if cell
                    .compare_exchange(VACANT, value, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return Slot { cell };
                }
            }
            chunk = chunk.next.get_or_init(|| Box::new(Chunk::new(2 * len)));
        }
    }

    fn cells(&self) -> impl Iterator<Item = &AtomicU64> {
        std::iter::successors(Some(&self.first), |chunk| {
            chunk.next.get().map(|next| &**next)
        })
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
