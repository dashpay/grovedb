//! Reading every part of a tree page, and changing a valid one.

use lignin_format::{
    tree_page::{
        CellContent, CellValue, HashField, Side, TreeCell, TreeLink, TreePage, TreePageMut,
    },
    value_log::{PendingValue, ValuePointer},
    CellAddress, FileId, PageId, PageSize, SlotId,
};

use crate::{ok, take_u16, take_up_to};

/// Calls every accessor of a page that may never have been checked. None
/// may panic, whatever the bytes are.
pub fn touch_page(page: &TreePage<'_>, size: PageSize) {
    let _ = (
        page.version(),
        page.id(),
        page.encoded_len(),
        page.slot_count(),
    );
    let _ = (page.cell_count(), page.is_empty(), page.departures());
    let _ = (
        page.unused_bytes(),
        page.free_bytes(size),
        page.used_bytes(),
    );
    let _ = (page.lowest_free_slot(), page.has_pending_value());
    let _ = page.fragment_roots().count();
    let _ = page.pending_values().count();
    for (_, cell) in page.cells() {
        touch_cell(&cell);
        if let Some(pointer) = cell.value_pointer() {
            let _ = page.find_value_pointer(&pointer);
        }
        for side in Side::BOTH {
            if let Some(link) = cell.link(side) {
                let _ = page.cell_by_address(link.child);
            }
        }
    }
    let _ = page.cells().rev().count();
    for index in 0..page.slot_count().min(64) + 1 {
        if let Some(slot) = SlotId::from_index(index) {
            let _ = page.cell(slot);
        }
    }
    let image = page.to_image();
    assert!(image.len() <= TreePage::MAX_LEN);
    let _ = ok(page.check(size));
    let _ = ok(page.check_fragments());
}

/// Calls every accessor of a cell.
pub fn touch_cell(cell: &TreeCell<'_>) {
    let _ = (cell.key(), cell.feature(), cell.value_len(), cell.value());
    let _ = (
        cell.value_pointer(),
        cell.pending_value(),
        cell.value_place(),
    );
    let _ = (cell.is_fragment_root(), cell.value_hash(), cell.kv_hash());
    for field in HashField::ALL {
        let _ = cell.hash(field);
    }
    for side in Side::BOTH {
        let _ = (
            cell.has_link(side),
            cell.link(side).map(|link| link.encoded_len()),
        );
    }
    let _ = (cell.hosted_root_count(), cell.hosted_roots().count());
    let _ = cell.hosted_roots().rev().count();
    let value = cell.value();
    let _ = (
        value.is_inline(),
        value.inline(),
        value.pointer(),
        value.pending(),
    );
    let _ = (value.place(), value.stored_len());
}

/// What a cell holds, as the writer takes it.
pub fn content_of<'a>(cell: &TreeCell<'a>, roots: &'a [Option<CellAddress>]) -> CellContent<'a> {
    CellContent {
        key: cell.key(),
        feature: cell.feature(),
        value_hash: cell.value_hash(),
        kv_hash: cell.kv_hash(),
        left: cell.link(Side::Left),
        right: cell.link(Side::Right),
        hosted_roots: roots,
        value: cell.value(),
        fragment_root: cell.is_fragment_root(),
    }
}

/// Checks a page that `TreePage::decode` accepted: what each reader says
/// agrees with the others, and every cell is what the writer writes for
/// what it holds.
pub fn check_valid(page: &TreePage<'_>, size: PageSize) {
    assert_eq!(ok(page.check(size)), Some(()));
    assert_eq!(page.to_image(), page.as_bytes());
    assert_eq!(page.used_bytes(), page.encoded_len());
    assert_eq!(page.unused_bytes(), 0);
    assert_eq!(page.cells().count(), page.cell_count());
    assert_eq!(page.is_empty(), page.cell_count() == 0);
    assert!(page.free_bytes(size) <= size.image_capacity());
    let mut pending = false;
    for (slot, cell) in page.cells() {
        touch_cell(&cell);
        assert_eq!(page.cell(slot), Some(cell));
        let address = CellAddress::new(page.id(), slot);
        assert_eq!(page.cell_by_address(address), Some(cell));
        assert_eq!(
            ok(TreeCell::decode(cell.as_bytes(), page.version())),
            Some(cell)
        );
        assert!(cell.encoded_len() <= size.max_cell_len());
        assert!(cell.key().len() <= TreeCell::MAX_KEY_LEN);

        let roots: Vec<_> = cell.hosted_roots().collect();
        assert_eq!(roots.len(), cell.hosted_root_count());
        let content = content_of(&cell, &roots);
        assert_eq!(content.encoded_len(), cell.encoded_len());
        assert_eq!(ok(content.encode()).as_deref(), Some(cell.as_bytes()));
        let value = cell.value();
        assert_eq!(value.place(), cell.value_place());
        if let Some(inline) = value.inline() {
            assert_eq!(inline.len() as u64, u64::from(cell.value_len()));
        }
        if let Some(place) = cell.value_place() {
            assert_eq!(place.value_len(), cell.value_len());
            assert_eq!(CellValue::of_place(place), value);
        }
        pending |= cell.pending_value().is_some();
        if let Some(pointer) = cell.value_pointer() {
            let found = page.find_value_pointer(&pointer).expect("its own pointer");
            assert!(found <= slot);
        }
        for side in Side::BOTH {
            assert_eq!(cell.has_link(side), cell.link(side).is_some());
            assert_eq!(
                cell.hash(HashField::Child(side)),
                cell.link(side).map(|link| link.hash)
            );
        }
    }
    assert_eq!(page.has_pending_value(), pending);
    assert_eq!(page.pending_values().count() > 0, pending);
    let free = page.lowest_free_slot();
    assert!(page.cell(free).is_none());
    let _ = ok(page.check_fragments());
}

/// A sequence of changes read from `ops`, made to a page in a buffer that
/// started as a valid page. After every change, refused or not, the page is
/// still valid, and a refused change leaves every byte as it was (W4, W5).
pub fn mutate(buffer: &mut [u8], size: PageSize, mut ops: &[u8]) {
    let version = lignin_format::FormatVersion::V1;
    let mut steps = 0;
    while let Some((&op, rest)) = ops.split_first() {
        ops = rest;
        steps += 1;
        if steps > 64 {
            return;
        }
        let before = buffer.to_vec();
        let result = {
            let mut page = TreePageMut::open(buffer, size, version).expect("a valid page");
            let free = page.free_bytes();
            assert_eq!(free, page.page().free_bytes(size));
            change(&mut page, op, &mut ops)
        };
        if ok(result).is_none() {
            assert_eq!(buffer, &before[..], "a refused change wrote");
        }
        let page = TreePage::from_validated(buffer, version).expect("a valid page");
        assert_eq!(ok(page.check(size)), Some(()), "a change broke the page");
        let image = page.to_image();
        let decoded = TreePage::decode(&image, version, size).expect("the image of a valid page");
        assert_eq!(decoded.cell_count(), page.cell_count());
    }
}

fn slot(page: &TreePageMut<'_>, ops: &mut &[u8]) -> SlotId {
    let raw = take_u16(ops).unwrap_or(0);
    let count = page.page().slot_count() + 2;
    SlotId::from_index(usize::from(raw) % count).unwrap_or(SlotId::MIN)
}

fn hash(ops: &mut &[u8]) -> [u8; 32] {
    let mut hash = [0; 32];
    let bytes = take_up_to(ops, 32);
    hash[..bytes.len()].copy_from_slice(bytes);
    hash
}

fn address(ops: &mut &[u8]) -> Option<CellAddress> {
    let page = PageId::new(u32::from(take_u16(ops)?))?;
    Some(CellAddress::new(page, SlotId::new(take_u16(ops)?)?))
}

fn value<'a>(ops: &mut &'a [u8]) -> CellValue<'a> {
    let len = take_u16(ops).unwrap_or(0);
    match len % 4 {
        0 => CellValue::Inline(take_up_to(ops, usize::from(len >> 2))),
        1 => ValuePointer::new(FileId::MIN, 20 + u64::from(len), 1 + u32::from(len))
            .map_or(CellValue::Inline(&[]), CellValue::OutOfLine),
        2 => PendingValue::new(
            [(len >> 8) as u8 | 1, 0, 0, 0, 0, 0, 0, 0],
            1 + u32::from(len),
        )
        .map_or(CellValue::Inline(&[]), CellValue::Pending),
        _ => CellValue::Inline(&[]),
    }
}

fn change(
    page: &mut TreePageMut<'_>,
    op: u8,
    ops: &mut &[u8],
) -> Result<(), lignin_format::FormatError> {
    let slot = slot(page, ops);
    match op % 14 {
        0 => page.remove(slot),
        1 => page.set_hash(slot, HashField::ALL[usize::from(op >> 4) % 4], &hash(ops)),
        2 => {
            let child = address(ops).unwrap_or(CellAddress::new(PageId::MIN, SlotId::MIN));
            page.set_child(slot, side(op), child)
        }
        3 => page.set_fragment_root(slot, op & 0x10 != 0),
        4 => {
            let root = address(ops);
            page.set_hosted_root(slot, usize::from(op >> 4), root)
        }
        5 => {
            let roots: Vec<_> = (0..usize::from(op >> 4)).map(|_| address(ops)).collect();
            page.set_hosted_roots(slot, &roots)
        }
        6 => {
            let len = usize::from(take_u16(ops).unwrap_or(0) & 0x1FF);
            let feature = take_up_to(ops, len).to_vec();
            page.set_feature(slot, &feature)
        }
        7 => {
            let value = value(ops);
            page.set_value(slot, value)
        }
        8 => {
            let hash = hash(ops);
            let len = usize::from(take_u16(ops).unwrap_or(0) & 0x1FF);
            let aggregate = take_up_to(ops, len).to_vec();
            let child = address(ops).unwrap_or(CellAddress::new(PageId::MIN, SlotId::MIN));
            let link = TreeLink {
                child,
                hash: &hash,
                heights: (op, op >> 1),
                child_key_len: op >> 2,
                aggregate: &aggregate,
            };
            page.set_link(slot, side(op), (op & 0x20 == 0).then_some(&link))
        }
        9 => page.compact(),
        10 | 11 => {
            let from = self::slot(page, ops);
            let copy = page.page().cell(from).map(|cell| cell.as_bytes().to_vec());
            let Some(copy) = copy else {
                return page.remove(slot);
            };
            let cell = TreeCell::decode(&copy, lignin_format::FormatVersion::V1)
                .expect("a cell of a valid page");
            if op % 14 == 10 {
                page.insert_encoded(slot, cell)
            } else {
                page.replace_encoded(slot, cell)
            }
        }
        _ => {
            let key_len = usize::from(take_u16(ops).unwrap_or(0) & 0x1FF);
            let key = take_up_to(ops, key_len).to_vec();
            let value = value(ops);
            let hashes = (hash(ops), hash(ops));
            let roots: Vec<_> = (0..usize::from(op >> 5)).map(|_| address(ops)).collect();
            let content = CellContent {
                key: &key,
                feature: &[],
                value_hash: &hashes.0,
                kv_hash: &hashes.1,
                left: None,
                right: None,
                hosted_roots: &roots,
                value,
                fragment_root: op & 0x10 != 0,
            };
            if op % 14 == 12 {
                page.insert(slot, &content)
            } else {
                page.replace(slot, &content)
            }
        }
    }
}

fn side(op: u8) -> Side {
    Side::BOTH[usize::from(op >> 7)]
}
