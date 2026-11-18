mod handles;
mod reading;
mod store;
mod writing;

use lignin_format::{
    tree_page::{CellContent, CellValue, Hash, TreeLink},
    CellAddress, CommitVersion, PageId, PageSize, SlotId,
};

use super::*;

const SIZE: PageSize = PageSize::DEFAULT;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn address(page_raw: u32, slot_raw: u16) -> CellAddress {
    CellAddress::new(page(page_raw), slot(slot_raw))
}

fn frame() -> Vec<u8> {
    vec![0; SIZE.get()]
}

/// A node with no child, whose feature tells it from every other node.
fn leaf<'a>(key: &'a [u8], feature: &'a [u8], value: &'a [u8]) -> CellContent<'a> {
    CellContent {
        key,
        feature,
        value_hash: &[0x11; 32],
        kv_hash: &[0x22; 32],
        left: None,
        right: None,
        hosted_roots: &[],
        value: CellValue::Inline(value),
        fragment_root: true,
    }
}

fn link<'a>(child: CellAddress, hash: &'a Hash, aggregate: &'a [u8]) -> TreeLink<'a> {
    TreeLink {
        child,
        hash,
        heights: (3, 2),
        child_key_len: 5,
        aggregate,
    }
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}
