//! Flat pages (`flat-page.md`): `FlatPage::decode`, and every accessor on
//! what `FlatPage::from_validated` borrows, checked or not.
//!
//! An image has no checksum, so the input is used as it is: the control byte
//! picks the reader, and the bytes after the image are keys to search for.

#![no_main]

use std::cmp::Ordering;

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    flat_page::{
        common_prefix_len, shortest_separator, FlatBranch, FlatBranchBuilder, FlatLeaf,
        FlatLeafBuilder, FlatPage, FlatValue,
    },
    value_log::{ValuePlace, ValuePointer},
    FormatVersion,
};
use lignin_format_fuzz::{ok, Input};

const V1: FormatVersion = FormatVersion::V1;

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let bytes = input.bytes;
    let probes = probes(bytes);
    match ok(FlatPage::decode(bytes, V1)) {
        Some(page) => {
            let validated = FlatPage::from_validated(bytes, V1).expect("a page that decodes");
            assert_eq!(validated.as_bytes(), page.as_bytes());
            checked(page, &probes);
        }
        None => {
            if let Some(page) = ok(FlatPage::from_validated(bytes, V1)) {
                unchecked(page, &probes);
            }
        }
    }
});

/// Keys to look for: the bytes after the image's stated length, cut into
/// pieces of the lengths their first bytes give.
fn probes(bytes: &[u8]) -> Vec<&[u8]> {
    let len = bytes
        .get(2..4)
        .map_or(0, |len| u16::from_le_bytes([len[0], len[1]]) as usize);
    let mut rest = bytes.get(len..).unwrap_or_default();
    let mut probes = vec![&[][..]];
    while let Some((&len, after)) = rest.split_first() {
        let (probe, after) = after.split_at(usize::from(len).min(after.len()));
        probes.push(probe);
        rest = after;
    }
    probes
}

fn unchecked(page: FlatPage<'_>, probes: &[&[u8]]) {
    let _ = (
        page.kind(),
        page.id(),
        page.level(),
        page.prefix(),
        page.is_empty(),
    );
    let _ = page.has_pending_value();
    match page {
        FlatPage::Leaf(leaf) => {
            for slot in 0..=leaf.len() {
                if let Some((key, value)) = leaf.entry(slot) {
                    let _ = (
                        key.to_vec(),
                        key.is_empty(),
                        value.is_inline(),
                        value.is_pending(),
                    );
                }
                let _ = (leaf.key(slot), leaf.value(slot));
            }
            let _ = leaf.iter().count();
            let _ = leaf.iter().rev().count();
            for probe in probes {
                let _ = (leaf.search(probe), leaf.get(probe));
            }
        }
        FlatPage::Branch(branch) => {
            for slot in 0..=branch.len() + 1 {
                let _ = (branch.separator(slot), branch.child(slot));
            }
            for probe in probes {
                let _ = (branch.child_index(probe), branch.child_for(probe));
            }
        }
    }
}

fn checked(page: FlatPage<'_>, probes: &[&[u8]]) {
    assert_eq!(page.is_empty(), page.len() == 0);
    match page {
        FlatPage::Leaf(leaf) => {
            assert_eq!(page.level(), 0);
            assert!(page.branch().is_none());
            checked_leaf(leaf, probes);
        }
        FlatPage::Branch(branch) => {
            assert_ne!(page.level(), 0);
            assert!(!page.has_pending_value());
            checked_branch(branch, probes);
        }
    }
}

fn checked_leaf(leaf: FlatLeaf<'_>, probes: &[&[u8]]) {
    let entries: Vec<_> = leaf.iter().collect();
    assert_eq!(entries.len(), leaf.len());
    let mut backwards: Vec<_> = leaf.iter().rev().collect();
    backwards.reverse();
    assert_eq!(backwards, entries);
    assert!(leaf.key(leaf.len()).is_none() && leaf.value(leaf.len()).is_none());

    let mut builder =
        FlatLeafBuilder::new(leaf.id(), leaf.prefix(), leaf.as_bytes().len()).expect("its header");
    let mut pending = false;
    for (slot, (key, value)) in entries.iter().enumerate() {
        let whole = key.to_vec();
        assert_eq!(key.len(), whole.len());
        assert_eq!(key.prefix(), leaf.prefix());
        assert!(key == &whole[..]);
        assert_eq!(key.cmp_bytes(&whole), Ordering::Equal);
        assert_eq!(leaf.search(&whole), Ok(slot));
        assert_eq!(leaf.get(&whole), Some(*value));
        if slot > 0 {
            assert!(entries[slot - 1].0 < *key);
            let before = entries[slot - 1].0.to_vec();
            let separator = shortest_separator(&before, &whole);
            assert!(separator > &before[..] && separator <= &whole[..]);
            assert!(common_prefix_len(&before, &whole) < whole.len());
        }
        if let FlatValue::OutOfLine(place) = value {
            let place: [u8; 16] = (*place).try_into().expect("a place is 16 bytes");
            let place = ValuePlace::decode(place).expect("a page that decodes");
            pending |= place.is_pending();
            assert_eq!(value.is_pending(), place.is_pending());
            if let Some(pointer) = place.pointer() {
                let found = pointer
                    .find_in_flat_leaf(&leaf)
                    .expect("the pointer is there");
                assert!(found <= slot);
            }
        } else {
            assert!(!value.is_pending());
        }
        builder
            .push_key(*key, *value)
            .expect("an entry that was read");
        assert_eq!(
            FlatLeaf::entry_len(key.suffix().len(), value.bytes().len()),
            FlatLeaf::ENTRY_OVERHEAD + key.suffix().len() + value.bytes().len()
        );
    }
    assert_eq!(leaf.has_pending_value(), pending);
    assert_eq!(builder.len(), leaf.len());
    assert_eq!(builder.encoded_len(), leaf.as_bytes().len());
    assert_eq!(builder.finish(), leaf.as_bytes());

    for probe in probes {
        match leaf.search(probe) {
            Ok(slot) => assert!(entries[slot].0 == **probe),
            Err(slot) => {
                assert!(slot == 0 || entries[slot - 1].0.cmp_bytes(probe) == Ordering::Less);
                assert!(
                    slot == entries.len() || entries[slot].0.cmp_bytes(probe) == Ordering::Greater
                );
                assert!(leaf.get(probe).is_none());
            }
        }
    }
    let unplaced = ValuePointer::new(lignin_format::FileId::MAX, 20, 1).expect("a pointer");
    if let Some(slot) = unplaced.find_in_flat_leaf(&leaf) {
        assert!(slot < leaf.len());
    }
}

fn checked_branch(branch: FlatBranch<'_>, probes: &[&[u8]]) {
    let first = branch.child(0).expect("a branch has a first child");
    let mut builder = FlatBranchBuilder::new(
        branch.id(),
        branch.prefix(),
        branch.as_bytes().len(),
        branch.level(),
        first,
    )
    .expect("its header");
    let mut separators = Vec::new();
    for slot in 0..branch.len() {
        let separator = branch.separator(slot).expect("a slot of the branch");
        let child = branch.child(slot + 1).expect("a child of the branch");
        let whole = separator.to_vec();
        assert_eq!(branch.child_index(&whole), slot + 1);
        assert_eq!(branch.child_for(&whole), Some(child));
        if let Some(before) = separators.last() {
            assert!(before < &whole);
        }
        builder
            .push_key(separator, child)
            .expect("an entry that was read");
        assert_eq!(
            FlatBranch::entry_len(separator.suffix().len()),
            FlatBranch::ENTRY_OVERHEAD + separator.suffix().len()
        );
        separators.push(whole);
    }
    assert!(branch.separator(branch.len()).is_none());
    assert!(branch.child(branch.len() + 1).is_none());
    assert_eq!(builder.len(), branch.len());
    assert_eq!(builder.finish(), branch.as_bytes());

    for probe in probes {
        let index = branch.child_index(probe);
        assert!(index <= branch.len());
        let below = separators.partition_point(|separator| separator.as_slice() <= *probe);
        assert_eq!(index, below);
        assert_eq!(branch.child_for(probe), branch.child(index));
    }
}
