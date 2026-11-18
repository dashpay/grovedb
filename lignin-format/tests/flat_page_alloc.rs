//! Reading a flat page allocates nothing.
//!
//! The test binary runs under an allocator that counts every allocation made
//! on the calling thread.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

use lignin_format::{
    flat_page::{FlatBranchBuilder, FlatLeafBuilder, FlatPage, FlatValue},
    value_log::ValuePointer,
    FileId, FormatVersion, PageId,
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

const PREFIX: [u8; 32] = [0xA5; 32];
const ENTRIES: u64 = 400;
const VALUE_LEN: usize = 24;
const CAPACITY: usize = 16_384;

fn key(index: u64) -> Vec<u8> {
    [PREFIX.as_slice(), &(index * 2).to_be_bytes()].concat()
}

fn leaf_image() -> Vec<u8> {
    let mut builder = FlatLeafBuilder::new(&PREFIX, CAPACITY).expect("fits");
    for index in 0..ENTRIES {
        let value = [index as u8; VALUE_LEN];
        let pointer = ValuePointer::new(FileId::MIN, 20 + index, VALUE_LEN as u32)
            .expect("valid")
            .encode();
        let value = if index % 5 == 0 {
            FlatValue::OutOfLine(&pointer)
        } else {
            FlatValue::Inline(&value)
        };
        builder.push(&key(index), value).expect("fits");
    }
    builder.finish()
}

fn branch_image() -> Vec<u8> {
    let first_child = PageId::new(1).expect("non-zero");
    let mut builder = FlatBranchBuilder::new(&PREFIX, CAPACITY, 1, first_child).expect("fits");
    for index in 0..ENTRIES {
        let child = PageId::new(index as u32 + 2).expect("non-zero");
        builder.push(&key(index), child).expect("fits");
    }
    builder.finish()
}

fn probes() -> Vec<Vec<u8>> {
    let mut probes: Vec<Vec<u8>> = (0..ENTRIES)
        .flat_map(|index| {
            let present = key(index);
            let mut absent = present.clone();
            *absent.last_mut().expect("not empty") |= 1;
            [present, absent]
        })
        .collect();
    probes.extend([
        Vec::new(),
        PREFIX[..7].to_vec(),
        PREFIX.to_vec(),
        vec![0x00; 40],
        vec![0xFF; 40],
        [PREFIX.as_slice(), &[0xFF; 20]].concat(),
    ]);
    probes
}

#[test]
fn the_allocator_counts_allocations_on_this_thread() {
    let (vec, allocations) = allocations_during(|| black_box(vec![1u8; 100]));
    assert_eq!(vec.len(), 100);
    assert!(allocations >= 1);
    let (boxed, allocations) = allocations_during(|| black_box(Box::new(7u64)));
    assert_eq!(*boxed, 7);
    assert_eq!(allocations, 1);
}

#[test]
fn binary_search_over_a_leaf_does_not_allocate() {
    let image = leaf_image();
    let probes = probes();

    let ((found, missing, value_bytes), allocations) = allocations_during(|| {
        let page = FlatPage::decode(&image, FormatVersion::V1).expect("valid");
        let leaf = page.leaf().expect("leaf");
        let (mut found, mut missing, mut value_bytes) = (0u64, 0u64, 0usize);
        for probe in &probes {
            match leaf.search(black_box(probe)) {
                Ok(slot) => {
                    found += 1;
                    value_bytes += leaf.value(slot).expect("in range").bytes().len();
                    let key = leaf.key(slot).expect("in range");
                    assert!(key == **probe);
                    assert!(key.cmp_bytes(probe).is_eq());
                    assert!(Some(key) == leaf.iter().nth(slot).map(|(key, _)| key));
                }
                Err(slot) => {
                    missing += 1;
                    assert!(slot <= leaf.len());
                    assert!(leaf.get(probe).is_none());
                }
            }
        }
        (found, missing, value_bytes)
    });

    assert_eq!(allocations, 0);
    assert_eq!(found, ENTRIES);
    assert_eq!(missing, ENTRIES + 6);
    let out_of_line = ENTRIES as usize / 5;
    assert_eq!(
        value_bytes,
        (ENTRIES as usize - out_of_line) * VALUE_LEN + out_of_line * ValuePointer::ENCODED_LEN
    );
}

#[test]
fn reading_a_leaf_does_not_allocate() {
    let image = leaf_image();
    let mut frame = image.clone();
    frame.resize(CAPACITY, 0);

    let ((entries, out_of_line, key_bytes), allocations) = allocations_during(|| {
        let page = FlatPage::from_validated(&frame, FormatVersion::V1).expect("valid");
        assert_eq!(page.as_bytes().len(), image.len());
        let leaf = page.leaf().expect("leaf");
        let (mut entries, mut out_of_line, mut key_bytes) = (0u64, 0u64, 0usize);
        let mut previous = None;
        for (key, value) in leaf {
            entries += 1;
            out_of_line += u64::from(!value.is_inline());
            key_bytes += key.len();
            assert!(previous.is_none_or(|previous| previous < key));
            previous = Some(key);
        }
        assert_eq!(leaf.iter().rev().count(), leaf.len());
        (entries, out_of_line, key_bytes)
    });

    assert_eq!(allocations, 0);
    assert_eq!(entries, ENTRIES);
    assert_eq!(out_of_line, ENTRIES / 5);
    assert_eq!(key_bytes, ENTRIES as usize * 40);
}

#[test]
fn routing_through_a_branch_does_not_allocate() {
    let image = branch_image();
    let probes = probes();

    let mut frame = image.clone();
    frame.resize(CAPACITY, 0);

    let (children, allocations) = allocations_during(|| {
        let decoded = FlatPage::decode(&image, FormatVersion::V1).expect("valid");
        let borrowed = FlatPage::from_validated(&frame, FormatVersion::V1).expect("valid");
        let mut children = 0u64;
        for page in [decoded, borrowed] {
            let branch = page.branch().expect("branch");
            for probe in &probes {
                let index = branch.child_index(black_box(probe));
                let child = branch.child_for(probe).expect("valid page");
                assert_eq!(child.get() as usize, index + 1);
                let below = index.checked_sub(1).and_then(|slot| branch.separator(slot));
                assert!(below.is_none_or(|separator| separator.cmp_bytes(probe).is_le()));
                let above = branch.separator(index);
                assert!(above.is_none_or(|separator| separator.cmp_bytes(probe).is_gt()));
                assert!(below < above || above.is_none());
                children += u64::from(child.get());
            }
        }
        children
    });

    assert_eq!(allocations, 0);
    assert!(children > 0);
}
