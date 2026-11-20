//! Flat runs of the log (`log-format.md` 4.7): `FlatRun::decode` with
//! `changes` on what it accepts, and `FlatRun::apply` on valid pages.
//!
//! The input is pieces, each a `u16` length and its bytes: a base image,
//! then runs. A run has no checksum of its own.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    flat_page::FlatPage,
    wal::{FlatChange, FlatRun, FlatRunWriter},
    FormatVersion,
};
use lignin_format_fuzz::{ok, page_size, pieces, Input};

const V1: FormatVersion = FormatVersion::V1;

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let capacity = match input.control >> 1 & 7 {
        7 => FlatPage::MAX_LEN,
        6 => usize::from(input.control),
        choice => page_size(choice).image_capacity(),
    };
    let pieces = pieces(input.bytes);
    let Some((&base, runs)) = pieces.split_first() else {
        return;
    };
    let base = ok(FlatPage::decode(base, V1)).map(|page| page.as_bytes());
    let mut page = base.map(<[u8]>::to_vec);
    for run in runs.iter().take(16) {
        let Some(run) = decoded(run) else {
            continue;
        };
        let mut out = vec![0xA5];
        match ok(run.apply(page.as_deref(), V1, capacity, &mut out)) {
            Some(()) => {
                let image = &out[1..];
                let read = FlatPage::decode(image, V1).expect("a page that a run built");
                assert_eq!(read.as_bytes(), image);
                assert_eq!(read.id(), run.page());
                assert!(image.len() <= capacity);
                page = Some(image.to_vec());
            }
            None => assert_eq!(out, [0xA5]),
        }
    }
});

/// Decodes a run and checks that writing what it holds gives it back.
fn decoded(bytes: &[u8]) -> Option<FlatRun<'_>> {
    let run = ok(FlatRun::decode(bytes, V1))?;
    assert_eq!(run.as_bytes(), bytes);
    assert_eq!(run.encoded_len(), bytes.len());
    assert_eq!(run.version(), V1);
    let changes = run.changes();
    let count = changes.len();
    let mut written = Vec::new();
    let mut writer = FlatRunWriter::new(run.page(), &mut written);
    let mut seen = 0;
    for change in changes {
        if let FlatChange::Image(image) = change {
            assert_eq!(seen, 0);
            assert_eq!(run.image(), Some(image));
            let page = FlatPage::decode(image, V1).expect("an image the run holds");
            assert_eq!(page.id(), run.page());
        }
        writer.push(&change).expect("a change that was read");
        seen += 1;
    }
    assert_eq!(seen, count);
    assert_eq!(ok(writer.finish()), Some(bytes.len()));
    assert_eq!(written, bytes);
    Some(run)
}
