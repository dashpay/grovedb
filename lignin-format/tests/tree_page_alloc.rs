//! Reading a tree page allocates nothing, and neither does a change that is
//! made where the cell is.
//!
//! The test binary runs under an allocator that counts every allocation made
//! on the calling thread.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

use lignin_format::{
    tree_page::{CellContent, CellValue, HashField, Side, TreeLink, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePointer},
    CellAddress, FileId, FormatVersion, PageId, PageSize, SlotId,
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

const VERSION: FormatVersion = FormatVersion::CURRENT;
const SIZE: PageSize = PageSize::DEFAULT;
const PAGE: PageId = PageId::MIN;
const CELLS: u16 = 120;
const AGGREGATE: [u8; 9] = [3, 0, 0, 0, 0, 0, 0, 0, 7];

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

fn pointer(index: u16) -> ValuePointer {
    let file = FileId::new(12).expect("not zero");
    ValuePointer::new(file, 20 + u64::from(index) * 500, 440).expect("valid")
}

fn pending(index: u16) -> PendingValue {
    PendingValue::new(u64::from(index).to_le_bytes(), 440).expect("valid")
}

fn link<'a>(child: u16, hash: &'a [u8; 32]) -> TreeLink<'a> {
    TreeLink {
        child: CellAddress::new(PAGE, slot(child)),
        hash,
        heights: (3, 2),
        child_key_len: 32,
        aggregate: &AGGREGATE,
    }
}

/// A page of 120 nodes shaped as one fragment: node `n` has the children
/// `2n` and `2n + 1`. Every other value is out of line, and every other of
/// those is not in the value log yet.
fn frame() -> Vec<u8> {
    let mut frame = vec![0; SIZE.get()];
    let mut page = TreePageMut::init(&mut frame, SIZE, PAGE, 0).expect("fits");
    for index in 1..=CELLS {
        let key = [index as u8; 32];
        let hashes = [[index as u8; 32], [!index as u8; 32]];
        let child = |child: u16| (child <= CELLS).then(|| link(child, &hashes[0]));
        let value = [index as u8; 40];
        let roots = [Some(CellAddress::new(PageId::MAX, slot(index))), None];
        page.insert(
            slot(index),
            &CellContent {
                key: &key,
                feature: &AGGREGATE,
                value_hash: &hashes[0],
                kv_hash: &hashes[1],
                left: child(2 * index),
                right: child(2 * index + 1),
                hosted_roots: if index % 7 == 0 { &roots } else { &[] },
                value: match index % 4 {
                    0 => CellValue::Pending(pending(index)),
                    2 => CellValue::OutOfLine(pointer(index)),
                    _ => CellValue::Inline(&value),
                },
                fragment_root: index == 1,
            },
        )
        .expect("fits");
    }
    frame
}

fn read_everything(page: &TreePage<'_>) -> u64 {
    let mut seen = 0u64;
    for (slot, cell) in page.cells() {
        seen += u64::from(slot.get());
        seen += cell.key().len() as u64 + cell.feature().len() as u64;
        seen += u64::from(cell.value_len()) + cell.encoded_len() as u64;
        seen += u64::from(cell.value_hash()[0]) + u64::from(cell.kv_hash()[0]);
        seen += u64::from(cell.is_fragment_root());
        for field in HashField::ALL {
            seen += cell.hash(field).map_or(0, |hash| u64::from(hash[31]));
        }
        for side in Side::BOTH {
            if let Some(link) = cell.link(side) {
                seen += u64::from(link.child.slot().get()) + u64::from(link.child_key_len);
                seen += u64::from(link.heights.0) + link.aggregate.len() as u64;
                seen += page
                    .cell_by_address(link.child)
                    .map_or(0, |child| child.key().len()) as u64;
            }
        }
        seen += cell.hosted_roots().flatten().count() as u64;
        seen += match cell.value() {
            CellValue::Inline(value) => value.len() as u64,
            CellValue::OutOfLine(pointer) => pointer.offset(),
            CellValue::Pending(pending) => u64::from_le_bytes(pending.id()),
        };
    }
    seen += u64::from(page.has_pending_value());
    seen += page.pending_values().count() as u64;
    black_box(seen)
}

#[test]
fn reading_a_tree_page_allocates_nothing() {
    let frame = frame();
    let image = TreePage::from_validated(&frame, VERSION)
        .expect("a page")
        .to_image();

    let (page, allocations) =
        allocations_during(|| TreePage::decode(black_box(&image), VERSION, SIZE).expect("valid"));
    assert_eq!(allocations, 0, "decoding an image");
    assert_eq!(page.cell_count(), usize::from(CELLS));

    let (in_frame, allocations) =
        allocations_during(|| TreePage::from_validated(black_box(&frame), VERSION).expect("valid"));
    assert_eq!(allocations, 0, "borrowing a page in a buffer");

    for page in [page, in_frame] {
        let (seen, allocations) = allocations_during(|| read_everything(&page));
        assert_eq!(allocations, 0, "reading every part of every cell");
        assert!(seen > 0);

        let (found, allocations) = allocations_during(|| {
            let roots = page.fragment_roots().count();
            let owner = page.find_value_pointer(black_box(&pointer(CELLS - 2)));
            let free = page.lowest_free_slot();
            page.check_fragments().expect("one fragment");
            (roots, owner, free, page.free_bytes(SIZE), page.departures())
        });
        assert_eq!(allocations, 0, "the lookups over a page");
        assert_eq!((found.0, found.1), (1, Some(slot(CELLS - 2))));
        assert_eq!(found.2, slot(CELLS + 1));
    }
}

#[test]
fn a_change_made_where_the_cell_is_allocates_nothing() {
    let mut frame = frame();
    let hash = [0xEE; 32];
    let roots = [None, Some(CellAddress::new(PageId::MAX, SlotId::MAX))];
    let new_value = [0x55; 40];
    let new_feature = [9; 9];

    let (page, allocations) = allocations_during(|| {
        TreePageMut::open(black_box(&mut frame), SIZE, VERSION).expect("a page")
    });
    assert_eq!(allocations, 0, "opening a page for changes");
    let mut page = page;

    let ((), allocations) = allocations_during(|| {
        for index in 1..=CELLS {
            let slot = slot(index);
            page.set_hash(slot, HashField::Value, &hash)
                .expect("stored");
            page.set_hash(slot, HashField::KeyValue, &hash)
                .expect("stored");
            page.set_fragment_root(slot, index % 3 == 0)
                .expect("a cell");
            page.set_feature(slot, &new_feature).expect("a cell");
            if 2 * index <= CELLS {
                let child = CellAddress::new(PageId::MAX, slot);
                page.set_hash(slot, HashField::Child(Side::Left), &hash)
                    .expect("stored");
                page.set_child(slot, Side::Left, child).expect("a link");
                page.set_link(slot, Side::Left, Some(&link(index, &hash)))
                    .expect("a cell");
            }
            if index % 7 == 0 {
                page.set_hosted_root(slot, 1, roots[1]).expect("a pointer");
                page.set_hosted_roots(slot, &roots).expect("a cell");
            }
            let value = match index % 2 {
                0 => CellValue::OutOfLine(pointer(index + 1)),
                _ => CellValue::Inline(&new_value),
            };
            page.set_value(slot, value).expect("a cell");
        }
    });
    assert_eq!(allocations, 0, "changes of the same size");

    // A new cell goes to the end of the page, and a cell leaves, with no
    // allocation either, while the page has room at its end.
    let ((), allocations) = allocations_during(|| {
        let key = [0xAB; 32];
        let content = CellContent {
            key: &key,
            feature: &[],
            value_hash: &hash,
            kv_hash: &hash,
            left: None,
            right: None,
            hosted_roots: &[],
            value: CellValue::Inline(&new_value),
            fragment_root: true,
        };
        for index in CELLS + 1..=CELLS + 20 {
            page.insert(slot(index), &content).expect("fits");
        }
        for index in CELLS + 1..=CELLS + 20 {
            page.remove(slot(index)).expect("occupied");
        }
    });
    assert_eq!(allocations, 0, "an insert at the end and a removal");

    let page = page.page();
    assert_eq!(page.cell_count(), usize::from(CELLS));
    assert_eq!(page.departures(), 20);
    page.check(SIZE).expect("still a valid page");
    let cell = page.cell(slot(14)).expect("a cell");
    assert_eq!(cell.value_hash(), &hash);
    assert_eq!(cell.feature(), new_feature);
    assert_eq!(cell.value(), CellValue::OutOfLine(pointer(15)));
    assert_eq!(cell.hosted_roots().collect::<Vec<_>>(), roots);
    assert_eq!(cell.link(Side::Left), Some(link(14, &hash)));
}
