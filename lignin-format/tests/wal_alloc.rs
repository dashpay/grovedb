//! Reading a log file allocates nothing: not the scan, and not going
//! through the values and the entries of its groups.
//!
//! The test binary runs under an allocator that counts every allocation made
//! on the calling thread.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

use lignin_format::{
    delta::Delta,
    flat_page::FlatValue,
    tree_page::HashField,
    value_log::{ValuePlace, ValuePointer},
    wal::{
        self, CheckpointMark, Entry, FlatChange, GroupWriter, LogCursor, LogItem, LogStart,
        ValueMove,
    },
    CommitVersion, FileId, FormatVersion, PageId, SlotId,
};

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn count() {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

// SAFETY: every method counts and then hands its arguments, unchanged, to
// the same method of `System`, so the memory is what `System` gives and takes
// back under the contract the caller keeps. Counting adds to thread-local
// cells that are constants at first and have no destructor, so it allocates
// nothing, and it cannot unwind short of a count that passes 2^63.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count();
        // SAFETY: the caller keeps the contract of `GlobalAlloc::alloc`, which
        // is the contract of `System.alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count();
        // SAFETY: the caller keeps the contract of `GlobalAlloc::alloc_zeroed`, which
        // is the contract of `System.alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count();
        // SAFETY: the caller keeps the contract of `GlobalAlloc::realloc`, which
        // is the contract of `System.realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller keeps the contract of `GlobalAlloc::dealloc`, which
        // is the contract of `System.dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let before = ALLOCATIONS.get();
    let result = work();
    (result, ALLOCATIONS.get() - before)
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("a page id")
}

fn log_of(groups: u64) -> Vec<u8> {
    let start = LogStart::FIRST;
    let mut log = Vec::new();
    wal::write_file_start(FileId::MIN, &start, &mut log).expect("a start");
    for raw_version in 1..=groups {
        let version = CommitVersion::new(raw_version).expect("a version");
        let at = start.address(log.len() as u64).expect("an address");
        let mut group = GroupWriter::new(version, at, &mut log).expect("a group");
        let pending = group
            .value(page(3), &[raw_version as u8; 400])
            .expect("a value");
        for raw_page in 1..8 {
            group
                .tree_run_with(page(raw_page), None, |run| {
                    run.push(&Delta::Hash {
                        slot: SlotId::MIN,
                        field: HashField::KeyValue,
                        hash: &[raw_page as u8; 32],
                    })
                })
                .expect("a tree page");
        }
        group
            .flat_run_with(page(9), |run| {
                run.push(&FlatChange::Insert {
                    key: b"a key of the flat keyspace",
                    value: FlatValue::Inline(b"and its value"),
                })
            })
            .expect("a flat page");
        group.freed(page(10)).expect("a freed page");
        let old = ValuePointer::new(FileId::MIN, 20 + raw_version, 400).expect("a pointer");
        group.released(&old).expect("a pointer");
        group.finish();

        let to = ValuePointer::new(FileId::MIN, 9000 + raw_version, 400).expect("a pointer");
        ValueMove::in_tree(page(3), SlotId::MIN, ValuePlace::Pending(pending), to)
            .expect("a move")
            .write(&mut log)
            .expect("a change");
        CheckpointMark::new(version)
            .write(&mut log)
            .expect("a mark");
    }
    log
}

#[test]
fn the_allocator_counts_allocations_on_this_thread() {
    let ((), allocations) = allocations_during(|| drop(black_box(vec![1u8; 64])));
    assert!(allocations > 0);
}

#[test]
fn reading_a_log_file_allocates_nothing() {
    let log = log_of(50);
    let (seen, allocations) = allocations_during(|| {
        let mut seen = [0usize; 6];
        let mut scan = wal::scan(black_box(&log)).expect("a log");
        for item in scan.by_ref() {
            match item {
                LogItem::Commit(group) => {
                    seen[0] += 1;
                    for (_, value) in group.values() {
                        let bytes = value.bytes().expect("the bytes");
                        seen[1] += bytes.parts().map(<[u8]>::len).sum::<usize>();
                    }
                    for entry in group.entries() {
                        seen[2] += 1;
                        match entry {
                            Entry::Tree(run) => seen[3] += run.deltas().count(),
                            Entry::Flat(run) => seen[3] += run.changes().count(),
                            _ => {}
                        }
                    }
                }
                LogItem::Change(change) => seen[4] += usize::from(change.page() == page(3)),
                LogItem::Mark(_) => seen[5] += 1,
            }
        }
        assert!(scan.tail().failure().is_none());
        seen
    });
    assert_eq!(seen, [50, 50 * 400, 50 * 10, 50 * 8, 50, 50]);
    assert_eq!(allocations, 0);
}

#[test]
fn reading_a_log_record_by_record_allocates_nothing() {
    let log = log_of(50);
    let (items, allocations) = allocations_during(|| {
        let (header, start) = wal::read_file_start(black_box(&log)).expect("a start");
        let mut cursor = LogCursor::new(&header, &start).expect("a log");
        let mut items = 0;
        while (cursor.offset() as usize) < log.len() {
            let at = cursor.offset() as usize;
            let len = wal::record_len(&log[at..], FormatVersion::CURRENT).expect("a frame");
            let step = cursor.step(&log[at..at + len]).expect("a record");
            items += usize::from(step.ends_item);
        }
        cursor.finish().expect("whole");
        items
    });
    assert_eq!(items, 150);
    assert_eq!(allocations, 0);
}
