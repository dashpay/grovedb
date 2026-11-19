//! The order a scan reaches the cells of a page in, and the locality of a
//! page's values.

use lignin_format::tree_page::{CellContent, TreeLink};

use super::*;

const PAGE: u32 = 4;

fn here(raw: u16) -> CellAddress {
    CellAddress::new(page_id(PAGE), slot(raw))
}

fn elsewhere(raw: u16) -> CellAddress {
    CellAddress::new(page_id(9), slot(raw))
}

fn link(child: CellAddress) -> Option<TreeLink<'static>> {
    Some(TreeLink {
        child,
        hash: &HASH,
        heights: (1, 1),
        child_key_len: 1,
        aggregate: &[],
    })
}

fn cell<'a>(key: &'a [u8], value: CellValue<'a>) -> CellContent<'a> {
    CellContent {
        key,
        feature: &[0],
        value_hash: &HASH,
        kv_hash: &HASH,
        left: None,
        right: None,
        hosted_roots: &[],
        value,
        fragment_root: false,
    }
}

/// A page with `cells`, each in the slot of its position, from 1.
fn page_of(cells: &[CellContent<'_>]) -> Vec<u8> {
    let mut frame = vec![0; SIZE.get()];
    let mut nodes =
        NodePageMut::create(&mut frame, SIZE, page_id(PAGE), Lease::take(), ()).expect("a page");
    for cell in cells {
        nodes.insert(cell).expect("fits");
    }
    frame
}

fn order_of(frame: &[u8]) -> Vec<u16> {
    let page = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
    scan_order(&page).into_iter().map(SlotId::get).collect()
}

fn pointer(file: u32, offset: u64, len: u32) -> ValuePointer {
    ValuePointer::new(FileId::new(file).expect("a file"), offset, len).expect("a pointer")
}

#[test]
fn the_cells_of_a_fragment_come_in_key_order() {
    //         m(1)
    //     c(2)    t(3)
    //       f(4)      z: in another page
    let frame = page_of(&[
        CellContent {
            left: link(here(2)),
            right: link(here(3)),
            fragment_root: true,
            ..cell(b"m", CellValue::Inline(b""))
        },
        CellContent {
            right: link(here(4)),
            ..cell(b"c", CellValue::Inline(b""))
        },
        CellContent {
            right: link(elsewhere(1)),
            ..cell(b"t", CellValue::Inline(b""))
        },
        cell(b"f", CellValue::Inline(b"")),
    ]);
    assert_eq!(order_of(&frame), [2, 4, 1, 3]);
    assert_eq!(order_of(&page_of(&[])), [0u16; 0]);
}

/// A Merk that a node hosts in its own page is read behind the node, and
/// before what is to the node's right.
#[test]
fn a_hosted_merk_comes_behind_its_host() {
    let roots = [Some(here(4)), None, Some(elsewhere(2)), Some(here(5))];
    let frame = page_of(&[
        CellContent {
            left: link(here(2)),
            right: link(here(3)),
            hosted_roots: &roots,
            fragment_root: true,
            ..cell(b"host", CellValue::Inline(b""))
        },
        cell(b"a", CellValue::Inline(b"")),
        cell(b"z", CellValue::Inline(b"")),
        CellContent {
            left: link(here(6)),
            ..cell(b"q", CellValue::Inline(b""))
        },
        cell(b"b", CellValue::Inline(b"")),
        cell(b"p", CellValue::Inline(b"")),
    ]);
    assert_eq!(order_of(&frame), [2, 1, 6, 4, 5, 3]);
}

/// The fragments of a page follow each other by their first keys, whatever
/// slots they are in.
#[test]
fn the_fragments_of_a_page_come_by_their_first_keys() {
    let frame = page_of(&[
        CellContent {
            left: link(here(2)),
            fragment_root: true,
            ..cell(b"x", CellValue::Inline(b""))
        },
        cell(b"w", CellValue::Inline(b"")),
        CellContent {
            right: link(here(4)),
            fragment_root: true,
            ..cell(b"d", CellValue::Inline(b""))
        },
        cell(b"e", CellValue::Inline(b"")),
        CellContent {
            fragment_root: true,
            ..cell(b"k", CellValue::Inline(b""))
        },
        // Two fragments that begin with one key, as two Merks can.
        CellContent {
            fragment_root: true,
            ..cell(b"k", CellValue::Inline(b""))
        },
    ]);
    assert_eq!(order_of(&frame), [3, 4, 5, 6, 2, 1]);
}

/// A page whose links are not those of fragments still gives every cell
/// once: links that lead in a circle, a cell that two cells name, a cell
/// that no fragment root reaches, and a link to a slot that holds no cell.
#[test]
fn every_cell_is_given_once_whatever_the_links_say() {
    let frame = page_of(&[
        CellContent {
            left: link(here(2)),
            right: link(here(3)),
            fragment_root: true,
            ..cell(b"a", CellValue::Inline(b""))
        },
        CellContent {
            left: link(here(1)),
            right: link(here(3)),
            ..cell(b"b", CellValue::Inline(b""))
        },
        CellContent {
            right: link(here(40)),
            ..cell(b"c", CellValue::Inline(b""))
        },
        CellContent {
            left: link(here(5)),
            ..cell(b"d", CellValue::Inline(b""))
        },
        CellContent {
            left: link(here(4)),
            ..cell(b"e", CellValue::Inline(b""))
        },
    ]);
    let mut order = order_of(&frame);
    assert_eq!(order[..3], [2, 3, 1]);
    order.sort();
    assert_eq!(order, [1, 2, 3, 4, 5]);
}

#[test]
fn a_run_is_values_that_lie_back_to_back_in_scan_order() {
    let extent = |len: u32| u64::from(len) + 18;
    assert_eq!(Locality::of_pointers(&[]), Locality::default());
    assert!(Locality::default().is_sequential());

    // Three values back to back, then one in another file at the offset
    // that would follow, then one that leaves a gap, then one before it.
    let first = pointer(3, 20, 300);
    let second = pointer(3, first.offset() + extent(300), 500);
    let third = pointer(3, second.offset() + extent(500), 257);
    let other_file = pointer(4, third.offset() + extent(257), 300);
    let gap = pointer(4, other_file.offset() + extent(300) + 1, 300);
    let before = pointer(4, 20, 300);
    let pointers = [first, second, third, other_file, gap, before];
    let locality = Locality::of_pointers(&pointers);
    assert_eq!(
        (locality.values, locality.runs, locality.jumps()),
        (6, 4, 3)
    );
    assert_eq!(
        locality.bytes,
        [300, 500, 257, 300, 300, 300]
            .map(extent)
            .iter()
            .sum::<u64>()
    );
    assert!(!locality.is_sequential());
    assert!(Locality::of_pointers(&pointers[..3]).is_sequential());

    // The same values in another order are other runs: the order is the
    // scan's.
    let reversed = Locality::of_pointers(&[third, second, first]);
    assert_eq!((reversed.values, reversed.runs), (3, 3));
}

/// The locality of a page is that of the pointers its cells hold, in scan
/// order: a value in its cell and a pending value are in no file.
#[test]
fn the_locality_of_a_page_counts_the_values_in_the_value_log() {
    let pending = PendingValue::new([9; 8], 400).expect("a pending value");
    let low = pointer(2, 20, 300);
    let high = pointer(2, 20 + 318, 300);
    let frame = page_of(&[
        CellContent {
            left: link(here(2)),
            right: link(here(3)),
            fragment_root: true,
            ..cell(b"m", CellValue::OutOfLine(high))
        },
        CellContent {
            left: link(here(4)),
            ..cell(b"c", CellValue::OutOfLine(low))
        },
        cell(b"t", CellValue::Pending(pending)),
        cell(b"a", CellValue::Inline(b"in its cell")),
    ]);
    let page = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
    let locality = Locality::of(&page);
    assert_eq!(
        (locality.values, locality.runs, locality.bytes),
        (2, 1, 636)
    );
}

#[test]
fn a_page_is_regrouped_when_enough_of_its_values_are_jumped_to() {
    let lying = |values, runs| Locality {
        values,
        runs,
        bytes: 0,
    };
    assert_eq!(RegroupPolicy::default(), RegroupPolicy::DEFAULT);
    assert_eq!(RegroupPolicy::DEFAULT, RegroupPolicy::new(25));
    assert_eq!(RegroupPolicy::ALWAYS, RegroupPolicy::new(0));
    for policy in [RegroupPolicy::ALWAYS, RegroupPolicy::DEFAULT] {
        // A page with no value in the value log, or whose values are one
        // run, is never regrouped.
        assert!(!policy.applies(lying(0, 0)));
        assert!(!policy.applies(lying(1, 1)));
        assert!(!policy.applies(lying(100, 1)));
        assert!(policy.applies(lying(100, 100)));
    }
    assert!(RegroupPolicy::ALWAYS.applies(lying(100, 2)));
    assert!(!RegroupPolicy::DEFAULT.applies(lying(100, 25)));
    assert!(RegroupPolicy::DEFAULT.applies(lying(100, 26)));
    // A scan jumps to fewer values than a page has, so from 100 on no
    // page is regrouped.
    assert!(RegroupPolicy::new(99).applies(lying(100, 100)));
    assert!(!RegroupPolicy::new(100).applies(lying(100, 100)));
}
