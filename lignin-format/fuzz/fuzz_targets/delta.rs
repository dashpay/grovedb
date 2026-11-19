//! Runs of page deltas (`page-delta.md`): `DeltaRun::decode` with `deltas`
//! and `created` on what it accepts, and `DeltaRun::apply`, `apply_chain`
//! and `consolidate` on valid pages only (A4).
//!
//! The input is pieces, each a `u16` length and its bytes: a base image,
//! then runs. Bits 1 and 2 of the control byte pick the page size.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    delta::{apply_chain, consolidate, DeltaRun, DeltaRunWriter},
    tree_page::{TreePage, TreePageMut},
    FormatVersion, PageId, PageSize,
};
use lignin_format_fuzz::{ok, pieces, tree::check_valid, Input};

const V1: FormatVersion = FormatVersion::V1;

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let size = input.page_size(1);
    let pieces = pieces(input.bytes);
    let Some((&base, runs)) = pieces.split_first() else {
        return;
    };
    let runs: Vec<DeltaRun<'_>> = runs.iter().filter_map(|run| decoded(run)).collect();
    let base = ok(TreePage::decode(base, V1, size));
    if let Some(base) = &base {
        check_valid(base, size);
    }
    let Some(page) = base
        .map(|base| base.id())
        .or_else(|| runs.first().map(DeltaRun::page))
    else {
        return;
    };
    let base = base.map(|base| base.as_bytes());
    alone(base, &runs, size);
    chain(page, base, &runs, size);
});

/// Decodes a run and checks that writing what it holds gives it back.
fn decoded(bytes: &[u8]) -> Option<DeltaRun<'_>> {
    let run = ok(DeltaRun::decode(bytes, V1))?;
    assert_eq!(run.as_bytes(), bytes);
    assert_eq!(run.encoded_len(), bytes.len());
    assert_eq!(run.version(), V1);
    let deltas = run.deltas();
    let count = deltas.len();
    let mut written = Vec::new();
    let mut writer = DeltaRunWriter::new(run.page(), run.created(), &mut written);
    let mut seen = 0;
    for delta in deltas {
        let _ = delta.slot();
        let mut alone = Vec::new();
        delta
            .encode_into(&mut alone)
            .expect("a delta that was read");
        assert_eq!(alone.len(), delta.encoded_len());
        writer.push(&delta).expect("a delta that was read");
        seen += 1;
    }
    assert_eq!(seen, count);
    assert_eq!(ok(writer.finish()), Some(bytes.len()));
    assert_eq!(written, bytes);
    Some(run)
}

fn frame_of(base: Option<&[u8]>, size: PageSize) -> Vec<u8> {
    let mut frame = vec![0; size.image_capacity()];
    if let Some(base) = base {
        frame[..base.len()].copy_from_slice(base);
    }
    frame
}

/// Applies each run by itself to the base, or to nothing if it creates its
/// page, whole and record by record. Whatever a record does or refuses, the
/// frame holds a valid page; a refused record changes nothing and ends the
/// run (A3, A4).
fn alone(base: Option<&[u8]>, runs: &[DeltaRun<'_>], size: PageSize) {
    for run in runs.iter().take(8) {
        if base.is_none() && run.created().is_none() {
            continue;
        }
        let mut frame = frame_of(base, size);
        let applied = ok(run.apply(&mut frame, size, V1)).is_some();
        let page = TreePage::from_validated(&frame, V1).expect("a page");
        assert_eq!(ok(page.check(size)), Some(()), "a run broke the page");

        let mut stepped = frame_of(base, size);
        match run.created() {
            Some(departures) => {
                TreePageMut::init(&mut stepped, size, run.page(), departures).expect("a frame");
            }
            None => {
                let page = TreePage::from_validated(&stepped, V1).expect("a page");
                if page.id() != run.page() {
                    assert!(!applied);
                    continue;
                }
            }
        }
        let mut refused = false;
        for delta in run.deltas() {
            let before = stepped.clone();
            let mut page = TreePageMut::open(&mut stepped, size, V1).expect("a valid page");
            if ok(delta.apply(&mut page)).is_none() {
                assert_eq!(stepped, before, "a refused record wrote");
                refused = true;
                break;
            }
        }
        assert_eq!(applied, !refused);
        assert_eq!(stepped, frame);
    }
}

/// Applies the runs as one chain, and consolidates them, which must agree.
fn chain(page: PageId, base: Option<&[u8]>, runs: &[DeltaRun<'_>], size: PageSize) {
    let mut consolidated = Vec::new();
    let result = ok(consolidate(page, base, runs, V1, size, &mut consolidated));
    if result.is_none() {
        assert!(consolidated.is_empty());
    }
    let creates = runs.first().is_some_and(|run| run.created().is_some());
    if base.is_none() && !creates {
        assert!(result.is_none());
        return;
    }
    let mut frame = frame_of(base, size);
    let applied = ok(apply_chain(page, &mut frame, size, V1, runs));
    let after = TreePage::from_validated(&frame, V1).expect("a page");
    assert_eq!(ok(after.check(size)), Some(()), "a chain broke the page");
    assert_eq!(applied.is_some(), result.is_some());
    if result.is_some() {
        assert_eq!(after.to_image(), consolidated);
        let image = TreePage::decode(&consolidated, V1, size).expect("a consolidated image");
        assert_eq!(image.id(), page);
        assert_eq!(ok(image.check(size)), Some(()));
    }
}
