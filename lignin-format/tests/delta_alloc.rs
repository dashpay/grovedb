//! Reading a run of page deltas allocates nothing, and neither does applying
//! a record that changes a cell where it is.
//!
//! The test binary runs under an allocator that counts every allocation made
//! on the calling thread.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

use lignin_format::{
    delta::{apply_chain, Delta, DeltaRun, DeltaRunWriter, RootList},
    tree_page::{
        CellContent, CellValue, HashField, Side, TreeLink, TreePage, TreePageMut, TreePageSize,
    },
    value_log::ValuePointer,
    CellAddress, FileId, FormatVersion, PageId, SlotId,
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

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
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
const SIZE: TreePageSize = TreePageSize::DEFAULT;
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
/// `2n` and `2n + 1`. Every node has two hosted-root pointers and a value
/// out of line.
fn frame() -> Vec<u8> {
    let mut frame = vec![0; SIZE.get()];
    let mut page = TreePageMut::init(&mut frame, SIZE, PAGE, 0).expect("fits");
    for index in 1..=CELLS {
        let key = [index as u8; 32];
        let hashes = [[index as u8; 32], [!index as u8; 32]];
        let child = |child: u16| (child <= CELLS).then(|| link(child, &hashes[0]));
        page.insert(
            slot(index),
            &CellContent {
                key: &key,
                feature: &AGGREGATE,
                value_hash: &hashes[0],
                kv_hash: &hashes[1],
                left: child(2 * index),
                right: child(2 * index + 1),
                hosted_roots: &[Some(CellAddress::new(PageId::MAX, slot(index))), None],
                value: CellValue::OutOfLine(pointer(index)),
                fragment_root: index == 1,
            },
        )
        .expect("fits");
    }
    frame
}

/// A run with every record that changes a cell where it is, for each of the
/// first 59 nodes, which have both children.
fn run() -> Vec<u8> {
    let hash = [0xEE; 32];
    let feature = [9; 9];
    let roots = [None, Some(CellAddress::new(PageId::MAX, SlotId::MAX))];
    let mut bytes = Vec::new();
    let mut writer = DeltaRunWriter::new(PAGE, None, &mut bytes);
    for index in 1..CELLS / 2 {
        let slot = slot(index);
        let mut push = |delta: Delta<'_>| writer.push(&delta).expect("a record");
        for field in HashField::ALL {
            push(Delta::Hash {
                slot,
                field,
                hash: &hash,
            });
        }
        for side in Side::BOTH {
            push(Delta::Child {
                slot,
                side,
                child: CellAddress::new(PageId::MAX, slot),
            });
            push(Delta::Link {
                slot,
                side,
                link: Some(link(index, &hash)),
            });
        }
        push(Delta::FragmentRoot {
            slot,
            fragment_root: index % 2 == 0,
        });
        push(Delta::HostedRoot {
            slot,
            index: 0,
            root: None,
        });
        push(Delta::HostedRoots {
            slot,
            roots: RootList::from(roots.as_slice()),
        });
        push(Delta::Feature {
            slot,
            feature: &feature,
        });
        push(Delta::Value {
            slot,
            value: CellValue::OutOfLine(pointer(index + 1000)),
        });
    }
    writer.finish().expect("a run");
    bytes
}

#[test]
fn reading_a_run_allocates_nothing() {
    let bytes = run();
    let (run, allocations) =
        allocations_during(|| DeltaRun::decode(black_box(&bytes), VERSION).expect("a run"));
    assert_eq!(allocations, 0, "decoding a run");
    assert_eq!(run.deltas().len(), usize::from(CELLS / 2 - 1) * 13);

    let (seen, allocations) = allocations_during(|| {
        let mut seen = 0u64;
        for delta in run.deltas() {
            seen += u64::from(delta.slot().get()) + delta.encoded_len() as u64;
            seen += match delta {
                Delta::Hash { hash, .. } => u64::from(hash[0]),
                Delta::Child { child, .. } => u64::from(child.slot().get()),
                Delta::Link { link, .. } => link.map_or(0, |link| link.aggregate.len() as u64),
                Delta::HostedRoots { roots, .. } => roots.iter().flatten().count() as u64,
                Delta::Feature { feature, .. } => feature.len() as u64,
                Delta::Value { value, .. } => value.stored_len() as u64,
                _ => 0,
            };
        }
        black_box(seen)
    });
    assert_eq!(allocations, 0, "reading every change of a run");
    assert!(seen > 0);
}

#[test]
fn applying_a_change_made_where_the_cell_is_allocates_nothing() {
    let mut frame = frame();
    let before = frame.clone();
    let bytes = run();
    let run = DeltaRun::decode(&bytes, VERSION).expect("a run");

    let (result, allocations) =
        allocations_during(|| run.apply(black_box(&mut frame), SIZE, VERSION));
    result.expect("applies");
    assert_eq!(allocations, 0, "applying a run");

    let (result, allocations) = allocations_during(|| {
        apply_chain(PAGE, black_box(&mut frame), SIZE, VERSION, [&run, &run])
    });
    result.expect("applies");
    assert_eq!(allocations, 0, "applying a chain");

    // The records were applied, and no cell moved for them.
    let page = TreePage::from_validated(&frame, VERSION).expect("a page");
    let old = TreePage::from_validated(&before, VERSION).expect("a page");
    assert_eq!(page.unused_bytes(), old.unused_bytes());
    assert_eq!(page.encoded_len(), old.encoded_len());
    for index in 1..CELLS / 2 {
        let cell = page.cell(slot(index)).expect("a cell");
        assert_eq!(cell.value_hash(), &[0xEE; 32]);
        assert_eq!(cell.feature(), [9; 9]);
        assert_eq!(cell.value_pointer(), Some(pointer(index + 1000)));
        assert_eq!(cell.is_fragment_root(), index % 2 == 0);
        assert_eq!(
            cell.link(Side::Left).map(|link| link.child),
            Some(CellAddress::new(PAGE, slot(index)))
        );
        assert_eq!(cell.hosted_roots().next(), Some(None));
        assert_eq!(
            cell.encoded_len(),
            old.cell(slot(index)).expect("a cell").encoded_len()
        );
    }
}
