//! Log records by themselves (`log-format.md` sections 4 and 6):
//! `Record::decode`, `record_len`, `read_logged_value`,
//! `read_pending_value`, and what a background change does to the page it
//! names (`ValueMove::is_due_in_tree`, `is_due_in_flat_leaf`,
//! `apply_to_flat_leaf`).
//!
//! The input is pieces, each a `u16` length and its bytes: records, a tree
//! page image, a flat page image, and a pending value's place.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    flat_page::FlatPage,
    tree_page::TreePage,
    value_log::ValuePlace,
    wal::{read_logged_value, read_pending_value, record_len, Change, LogAddress, Record},
    FormatVersion, PageId, PageSize,
};
use lignin_format_fuzz::{default_frame_len, ok, pieces, sealed_records, Input};

const V1: FormatVersion = FormatVersion::V1;

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let pieces = pieces(input.bytes);
    let piece = |index: usize| pieces.get(index).copied().unwrap_or_default();
    let records = sealed_records(
        &Input {
            control: input.control,
            bytes: piece(0),
        },
        default_frame_len,
    );
    let tree = ok(TreePage::decode(piece(1), V1, PageSize::MAX));
    let flat = ok(FlatPage::decode(piece(2), V1)).and_then(FlatPage::leaf);
    let pending = piece(3)
        .first_chunk::<{ ValuePlace::ENCODED_LEN }>()
        .and_then(|place| ok(ValuePlace::decode(*place)))
        .and_then(|place| place.pending());

    let mut rest = records.as_slice();
    while !rest.is_empty() {
        let len = ok(record_len(rest, V1));
        if let Some((value, value_len)) = ok(read_logged_value(rest, V1)) {
            assert!(value_len <= rest.len());
            assert_eq!(value.bytes().is_some(), value.carried().is_none());
            if let Some(bytes) = value.bytes() {
                let whole = bytes.to_vec();
                assert_eq!(whole.len(), bytes.value_len() as usize);
                assert_eq!(bytes.owner(), value.owner());
            }
            if let Some(pending) = pending {
                let read = ok(read_pending_value(&pending, rest, value.owner(), V1));
                if read.is_some() {
                    assert_eq!(pending.value_len(), value.value_len());
                    assert!(LogAddress::of(&pending).is_ok());
                }
            }
        }
        let Some((record, record_len)) = ok(Record::decode(rest, V1)) else {
            return;
        };
        assert_eq!(len, Some(record_len));
        let _ = record.kind_code();
        let bytes = &rest[..record_len];
        match record {
            Record::Start(start) => {
                let _ = (start.base(), start.after());
            }
            Record::Value(part) => {
                assert!(part.data().len() <= part.value_len() as usize);
                let _ = (part.owner(), part.part(), part.is_last());
            }
            Record::Carried(carried) => {
                let _ = (carried.owner(), carried.source_owner(), carried.value_len());
            }
            Record::Part(part) => {
                let _ = (part.version(), part.part(), part.is_last(), part.is_empty());
                for entry in part.entries() {
                    let _ = entry.page();
                }
            }
            Record::Change(change) => self::change(bytes, &change, tree.as_ref(), flat.as_ref()),
            Record::Mark(mark) => {
                let mut written = Vec::new();
                mark.write(&mut written).expect("a mark that was read");
                assert_eq!(written, bytes);
            }
        }
        rest = &rest[record_len..];
    }
});

fn change(
    bytes: &[u8],
    change: &Change<'_>,
    tree: Option<&TreePage<'_>>,
    flat: Option<&lignin_format::flat_page::FlatLeaf<'_>>,
) {
    let mut written = Vec::new();
    change.write(&mut written).expect("a change that was read");
    assert_eq!(written, bytes);
    let page: PageId = change.page();
    let value = match change {
        Change::Reclaim(reclaim) => {
            let released: Vec<_> = reclaim.released().collect();
            assert_eq!(released.len(), reclaim.released().len());
            if let Some(removed) = reclaim.removed() {
                assert_eq!(removed.page(), page);
            }
            let _ = reclaim.freed();
            return;
        }
        Change::Value(value) => value,
    };
    assert_eq!(value.page(), page);
    assert_eq!(value.from().value_len(), value.to().value_len());
    assert_eq!(value.released(), value.from().pointer());
    assert_eq!(value.delta().is_some(), value.slot().is_some());
    if let Some(tree) = tree {
        let due = value.is_due_in_tree(tree);
        if due {
            let cell = tree
                .cell(value.slot().expect("a tree change"))
                .expect("a due cell");
            assert_eq!(cell.value_place(), Some(value.from()));
        }
    }
    let Some(leaf) = flat else {
        return;
    };
    let image = leaf.as_bytes();
    let due = ok(value.is_due_in_flat_leaf(image, V1));
    let mut out = vec![0x5A];
    let applied = ok(value.apply_to_flat_leaf(image, V1, FlatPage::MAX_LEN, &mut out));
    match applied {
        Some(()) => {
            assert_eq!(due, Some(true));
            let page = FlatPage::decode(&out[1..], V1).expect("a leaf a change built");
            let leaf = page.leaf().expect("a leaf");
            assert!(value.to().find_in_flat_leaf(&leaf).is_some());
        }
        None => assert_eq!(out, [0x5A]),
    }
}
