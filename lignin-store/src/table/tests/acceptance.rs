//! The first acceptance sentence: lookup is O(1) with no hashing.
//!
//! The second, the size of the table for one million pages, is in
//! `tests/page_table.rs`, where it has a process to itself.

use super::{super::chains::NODES_READ, *};

fn nodes_read(read: impl FnOnce()) -> u64 {
    NODES_READ.with(|nodes| nodes.set(0));
    read();
    NODES_READ.with(|nodes| nodes.get())
}

/// A lookup reads the page's own slot of the array and nothing else: one
/// node for the newest link, and one node for each link of the chain it
/// follows. The number does not depend on how many pages the table holds or
/// on the page id, and no key is hashed or compared on the way.
#[test]
fn a_lookup_reads_one_slot_however_many_pages_the_table_holds() {
    for pages in [1u32, 1_000, 100_000, 1_000_000] {
        let table = PageTable::new();
        for raw_page in 1..=pages {
            table
                .push(page(raw_page), base(3, at(1, 20 + u64::from(raw_page))))
                .unwrap();
        }
        table.push(page(1), delta(4, at(2, 20))).unwrap();
        table.push(page(1), delta(5, at(2, 100))).unwrap();
        table.push(page(u32::MAX), base(1, at(3, 20))).unwrap();

        let mut chain = Vec::new();
        let samples = [1, pages / 2 + 1, pages, u32::MAX];
        for raw_page in samples {
            assert_eq!(nodes_read(|| _ = table.newest(page(raw_page))), 1);
        }
        for raw_page in samples.iter().filter(|raw_page| **raw_page != 1) {
            let read = || _ = table.read(page(*raw_page), version(9), &mut chain);
            assert_eq!(nodes_read(read), 1, "{pages} pages, page {raw_page}");
        }
        let whole = nodes_read(|| _ = table.read(page(1), version(9), &mut chain));
        assert_eq!((whole, chain.len()), (3, 3));
        let oldest = nodes_read(|| _ = table.read(page(1), version(3), &mut chain));
        assert_eq!((oldest, chain.len()), (3, 1));
        for missing in [pages + 1, 3_000_000_000] {
            assert_eq!(nodes_read(|| _ = table.newest(page(missing))), 1);
        }
    }
}
