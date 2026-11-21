//! The inline threshold.

use lignin_format::{
    tree_page::{CellContent, TreeCell, TreeLink, TreePageError},
    FormatError,
};

use super::*;

const SIZES: [usize; 4] = [8_192, 16_384, 32_768, 65_536];

fn sizes() -> impl Iterator<Item = PageSize> {
    SIZES
        .map(|bytes| PageSize::new(bytes).expect("a page size"))
        .into_iter()
}

/// A cell with every part but its value as long as a cell can have it, and
/// a value of `value_len` bytes in it.
fn largest_cell(page: PageId, value_len: usize, insert: impl FnOnce(&CellContent<'_>)) {
    let (key, feature, aggregate) = ([1; 255], [2; 255], [3; 255]);
    let roots = [Some(CellAddress::new(page, slot(9))); 255];
    let value = vec![4; value_len];
    let link = TreeLink {
        child: CellAddress::new(page, slot(7)),
        hash: &HASH,
        heights: (1, 1),
        child_key_len: 255,
        aggregate: &aggregate,
    };
    insert(&CellContent {
        key: &key,
        feature: &feature,
        value_hash: &HASH,
        kv_hash: &HASH,
        left: Some(link),
        right: Some(link),
        hosted_roots: &roots,
        value: CellValue::Inline(&value),
        fragment_root: true,
    });
}

#[test]
fn the_default_threshold_is_256_bytes_at_every_page_size() {
    assert_eq!(InlinePolicy::DEFAULT.threshold(), 256);
    assert_eq!(InlinePolicy::default(), InlinePolicy::DEFAULT);
    for size in sizes() {
        let policy = InlinePolicy::new(size, InlinePolicy::DEFAULT_THRESHOLD);
        assert_eq!(policy.expect("valid"), InlinePolicy::DEFAULT);
        assert!(InlinePolicy::max_threshold(size) >= 256, "{size:?}");
    }
    // The bound grows with the page: what is left of half a page once every
    // other part of a cell is as long as it can be.
    let bounds: Vec<usize> = sizes().map(InlinePolicy::max_threshold).collect();
    assert_eq!(bounds, [1_376, 5_472, 13_664, 30_048]);
}

/// The rule the threshold is bounded by (`tree-layout.md` C7, `tree-page.md`
/// TP14): a cell with its slot is smaller than half a page. So a value at
/// the largest threshold fits whatever else its cell holds, and no key,
/// feature or link can push a value of that length out of its cell.
#[test]
fn a_value_at_the_largest_threshold_fits_in_the_largest_cell() {
    for size in sizes() {
        let max = InlinePolicy::max_threshold(size);
        let page = page_id(1);
        let mut frame = vec![0; size.get()];
        let mut nodes =
            NodePageMut::create(&mut frame, size, page, Lease::take(), ()).expect("a page");
        largest_cell(page, max, |cell| {
            assert_eq!(cell.encoded_len(), size.max_cell_len());
            nodes.insert(cell).expect("the largest cell fits");
        });
        largest_cell(page, max, |cell| {
            nodes.insert(cell).expect("and so does a second one");
        });
        largest_cell(page, max + 1, |cell| {
            let refused = nodes.insert(cell).expect_err("one byte more does not");
            let too_large = matches!(
                refused,
                NodeError::Page(FormatError::TreePage(TreePageError::CellTooLarge { .. }))
            );
            assert!(too_large, "{refused}");
        });

        let policy = InlinePolicy::new(size, max).expect("the largest threshold");
        assert!(policy.is_inline(max) && !policy.is_inline(max + 1));
        let refused = InlinePolicy::new(size, max + 1).expect_err("above the bound");
        assert!(
            matches!(refused, ValueError::Threshold { threshold, max: bound, .. }
                if threshold == max + 1 && bound == max),
            "{refused}"
        );
    }
}

/// The place of a value out of line takes 12 bytes of its cell, so a value
/// of up to 12 bytes gains nothing by leaving, and no policy sends it away.
#[test]
fn the_smallest_threshold_is_the_length_of_a_place() {
    assert_eq!(InlinePolicy::MIN_THRESHOLD, TreeCell::VALUE_LOCATION_LEN);
    assert_eq!(InlinePolicy::MIN_THRESHOLD, 12);
    let policy = InlinePolicy::new(SIZE, 12).expect("the smallest threshold");
    assert!(policy.is_inline(12) && !policy.is_inline(13));
    let refused = InlinePolicy::new(SIZE, 11).expect_err("below a place");
    assert_eq!(
        refused.to_string(),
        "an inline threshold of 11 bytes is not between 12 and 1376"
    );
    assert!(std::error::Error::source(&refused).is_none());
}

/// The length of a value decides where it goes, and nothing else does: an
/// empty value is never out of line (`value-log-format.md` V2), a value up
/// to the threshold is written into its cell, and a longer one is written
/// as a pending value of its length.
#[test]
fn the_length_of_a_value_alone_decides_where_it_goes() {
    let mut values = TreeValues::new(InlinePolicy::DEFAULT);
    assert_eq!(values.policy(), InlinePolicy::DEFAULT);
    assert!(matches!(values.write(b""), Ok(ValueWrite::Inline(b""))));
    for len in [1, 12, 255, 256] {
        let value = bytes(len as u64, len);
        let write = values.write(&value).expect("a value");
        assert_eq!(write, ValueWrite::Inline(&value[..]), "{len}");
    }
    for len in [257, 400, 70_000] {
        let value = bytes(len as u64, len);
        let ValueWrite::Pending(pending) = values.write(&value).expect("a value") else {
            panic!("a value of {len} bytes stays in its cell");
        };
        assert_eq!(pending.value_len() as usize, len);
        // No write placed it, so it can be given up, once.
        assert!(values.forget(pending));
        assert!(!values.forget(pending));
    }
    assert_eq!(values.pages().count(), 0);

    let low = InlinePolicy::new(SIZE, 16).expect("a threshold");
    let mut values = TreeValues::new(low);
    assert!(matches!(values.write(&[5; 16]), Ok(ValueWrite::Inline(_))));
    assert!(matches!(values.write(&[5; 17]), Ok(ValueWrite::Pending(_))));
}

/// Each error says what failed, and names the error below it.
#[test]
fn each_error_says_what_failed() {
    use std::error::Error;

    use lignin_store::{cache::CacheError, load::LoadError, values::ValueStoreError};

    let page = page_id(7);
    let plain = [
        (
            ValueError::TooLong { len: 4_294_967_296 },
            "a value of 4294967296 bytes is too long",
        ),
        (
            ValueError::NoValue { page },
            "the bytes of a pending value of page 7 are missing",
        ),
        (
            ValueError::NoCell { page },
            "no cell of page 7 holds a value that is kept for it",
        ),
        (
            ValueError::Shared { page },
            "two cells of page 7 hold one pending value",
        ),
        (
            ValueError::NotCovered { page },
            "no durable manifest covers the regrouped values of page 7",
        ),
    ];
    for (error, says) in plain {
        assert_eq!(error.to_string(), says);
        assert!(error.source().is_none());
    }

    let address = CellAddress::new(page, slot(2));
    let frame_size = || LoadError::FrameSize {
        page_size: 8_192,
        frame_len: 4_096,
    };
    let wrapped: [(ValueError, &str); 7] = [
        (FormatError::MissingId.into(), "format: "),
        (NodeError::NoNode { address }.into(), "node: "),
        (frame_size().into(), "page load: "),
        (CacheError::WrongPool.into(), "buffer pool: "),
        (ValueStoreError::ReadOnly.into(), "value log: "),
        (
            FragmentError::Pages("closed".into()).into(),
            "pages: pages: closed",
        ),
        (ValueError::Log("full".into()), "log: full"),
    ];
    for (error, says) in wrapped {
        assert!(error.to_string().starts_with(says), "{error}");
        let below = error.source().expect("the error below it");
        assert!(error.to_string().ends_with(&below.to_string()), "{error}");
    }
}
