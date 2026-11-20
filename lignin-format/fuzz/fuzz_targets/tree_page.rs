//! Tree pages (`tree-page.md`): `TreePage::decode`, `TreePage::check`,
//! `TreePage::check_fragments` and `TreeCell::decode`, every accessor on
//! what `TreePage::from_validated` borrows, and `TreePageMut` on pages that
//! decode (W5).
//!
//! An image has no checksum. Bits 1 and 2 of the control byte pick the page
//! size. The bytes after the image are the changes made to it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    tree_page::{TreeCell, TreePage},
    FormatVersion,
};
use lignin_format_fuzz::{
    ok,
    tree::{check_valid, mutate, touch_cell, touch_page},
    Input,
};

const V1: FormatVersion = FormatVersion::V1;

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let size = input.page_size(1);
    let bytes = input.bytes;
    if input.bit(3)
        && let Some(cell) = ok(TreeCell::decode(bytes, V1))
    {
        assert_eq!(cell.as_bytes(), bytes);
        touch_cell(&cell);
    }
    let Some(page) = ok(TreePage::decode(bytes, V1, size)) else {
        if let Some(page) = ok(TreePage::from_validated(bytes, V1)) {
            touch_page(&page, size);
        }
        return;
    };
    check_valid(&page, size);
    let validated = TreePage::from_validated(bytes, V1).expect("a page that decodes");
    assert_eq!(validated.as_bytes(), page.as_bytes());
    touch_page(&validated, size);
    let ops = &bytes[page.encoded_len()..];
    let mut buffer = vec![0; size.image_capacity()];
    buffer[..page.encoded_len()].copy_from_slice(page.as_bytes());
    mutate(&mut buffer, size, ops);
});
