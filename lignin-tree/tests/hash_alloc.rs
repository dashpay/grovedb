//! An evaluation allocates for its lists and not for each node, measured at
//! the allocator.
//!
//! The test binary runs under an allocator that counts the allocations of
//! the calling thread. A path of marked nodes, each the left child of the
//! one before it, is evaluated from its top, for a short path and for a
//! long one. The long path costs a few allocations more, as its lists
//! grow, and not one for each of its nodes.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::HashMap,
};

use lignin_format::{
    delta::Delta,
    tree_page::{CellContent, CellValue, Hash, HashField, Side, TreeLink},
    CellAddress, PageId, PageSize, SlotId,
};
use lignin_tree::{
    fragment::{FragmentError, PageMut, Pages},
    hash::{check, HashRules, Hashes, Refusal, Stale},
    node::{ChangeSink, Lease, NodeError, NodePage, NodePageMut, NodeRef},
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

const SIZE: PageSize = PageSize::DEFAULT;
/// How many nodes of the path a page holds.
const PER_PAGE: usize = 100;

/// Hashes that take in every byte they are given, with no allocation.
struct Mixing;

fn mix(parts: &[&[u8]]) -> Hash {
    let mut state = 0xCBF2_9CE4_8422_2325u64;
    let mut hash = [0; 32];
    for (part, at) in parts.iter().zip((0..32).cycle()) {
        for byte in *part {
            state = (state ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01B3);
        }
        state = state.rotate_left(17) ^ part.len() as u64;
        hash[at] ^= state as u8;
        hash[(at + 11) % 32] ^= (state >> 23) as u8;
    }
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte ^= (state >> (index % 8 * 8)) as u8;
    }
    hash
}

impl HashRules for Mixing {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        _: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        Ok(mix(&[b"value", value]))
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        Ok(mix(&[b"kv", key, value_hash]))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        let none = [0; 32];
        let child = |side| node.link(side).map_or(&none, |link| link.hash);
        Ok(mix(&[
            b"node",
            node.kv_hash(),
            child(Side::Left),
            child(Side::Right),
        ]))
    }
}

/// A sink that counts the hash records it is told.
#[derive(Default)]
struct Records(usize);

impl ChangeSink for Records {
    fn created(&mut self, _: PageId, _: u64) {}

    fn changed(&mut self, _: PageId, change: &Delta<'_>) {
        self.0 += usize::from(matches!(change, Delta::Hash { .. }));
    }
}

/// Tree pages in memory.
struct Mem {
    lease: Lease,
    frames: HashMap<PageId, Vec<u8>>,
    records: Records,
}

impl Pages for Mem {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        let frame = self.frames.get(&page).expect("the page");
        Ok(read(NodePage::open(frame, self.lease)?))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        let frame = self.frames.get_mut(&page).expect("the page");
        let sink: &mut dyn ChangeSink = &mut self.records;
        let mut page = NodePageMut::open(frame, SIZE, self.lease, sink)?;
        Ok(change(&mut page)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        unreachable!("an evaluation starts no page")
    }

    fn free(&mut self, _: PageId) -> Result<(), FragmentError> {
        unreachable!("an evaluation frees no page")
    }
}

fn address(node: usize) -> CellAddress {
    let page = PageId::new((node / PER_PAGE) as u32 + 1).expect("not zero");
    let slot = SlotId::new((node % PER_PAGE) as u16 + 1).expect("not zero");
    CellAddress::new(page, slot)
}

/// A path of `len` nodes over several pages, with no hash made: node `n` is
/// the left child of node `n - 1`. Every hash of the path is marked.
fn path(len: usize) -> (Mem, Hashes) {
    let lease = Lease::take();
    let mut mem = Mem {
        lease,
        frames: HashMap::new(),
        records: Records::default(),
    };
    let mut hashes = Hashes::new();
    for first in (0..len).step_by(PER_PAGE) {
        let id = address(first).page();
        let mut frame = vec![0; SIZE.get()];
        let mut nodes = NodePageMut::create(&mut frame, SIZE, id, lease, ()).expect("a page");
        for node in first..len.min(first + PER_PAGE) {
            let key = (node as u32).to_be_bytes();
            let below = (node + 1 < len).then(|| TreeLink {
                child: address(node + 1),
                hash: &[0; 32],
                heights: (1, 0),
                child_key_len: 4,
                aggregate: &[0],
            });
            let handle = nodes.insert(&CellContent {
                key: &key,
                feature: &[0],
                value_hash: &[0; 32],
                kv_hash: &[0; 32],
                left: below,
                right: None,
                hosted_roots: &[],
                value: CellValue::Inline(b"a value that is in its cell"),
                fragment_root: node == first,
            });
            assert_eq!(handle.expect("the cell fits").address(), address(node));
            hashes.mark(address(node), HashField::Value);
            if below.is_some() {
                hashes.mark(address(node), HashField::Child(Side::Left));
            }
        }
        mem.frames.insert(id, frame);
    }
    (mem, hashes)
}

/// The allocations of evaluating a path of `len` marked nodes from its top.
fn allocations(len: usize) -> u64 {
    let (mut mem, mut hashes) = path(len);
    assert_eq!(hashes.len(), len);
    let before = ALLOCATIONS.with(Cell::get);
    let root = hashes.hash_of(&mut mem, &mut (), &mut Mixing, address(0));
    let allocations = ALLOCATIONS.with(Cell::get) - before;

    root.expect("the path is evaluated");
    assert!(hashes.is_clean());
    let counters = *hashes.counters();
    assert_eq!(counters.nodes, len as u64);
    assert_eq!(counters.node_hashes, len as u64);
    assert_eq!(counters.value_hashes, len as u64);
    assert_eq!(counters.kv_hashes, len as u64);
    // One record for each hash of the path: every node's own two, and a
    // child's in every node but the last.
    assert_eq!(mem.records.0, 3 * len - 1);
    for node in 0..len {
        let differing = check(&mut mem, &mut (), &mut Mixing, address(node));
        assert_eq!(differing.expect("checked"), Stale::NONE);
    }
    allocations
}

#[test]
fn an_evaluation_allocates_for_its_lists_and_not_for_each_node() {
    let (short, long) = (allocations(40), allocations(840));
    assert!(short <= 24, "a short path: {short} allocations");
    assert!(
        long <= short + 24,
        "800 more nodes cost {} more allocations",
        long - short
    );
}
