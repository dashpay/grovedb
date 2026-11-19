//! What is refused, and what a tree that is not one is told apart by.

use std::error::Error;

use lignin_format::{
    flat_page::{FlatBranchBuilder, FlatLeafBuilder},
    FormatError,
};
use lignin_store::{
    alloc::AllocError,
    cache::CacheError,
    load::LoadError,
    values::{Miscount, ValueStoreError},
};

use super::*;

#[test]
fn every_error_says_what_it_is_and_where_it_comes_from() {
    let page = page_id(7);
    let pointer = ValuePointer::new(FileId::MIN, 20, 5).expect("a pointer");
    let caused: [(FlatError, &str); 5] = [
        (FormatError::MissingId.into(), "flat page: "),
        (
            LoadError::FrameSize {
                page_size: 8_192,
                frame_len: 4_096,
            }
            .into(),
            "page load: ",
        ),
        (CacheError::WrongPool.into(), "buffer pool: "),
        (
            ValueStoreError::Miscounted {
                pointer,
                why: Miscount::UnknownFile,
            }
            .into(),
            "value log: ",
        ),
        (AllocError::Exhausted.into(), "page ids: "),
    ];
    for (error, begins) in caused {
        let source = error.source().expect("a source").to_string();
        assert_eq!(error.to_string(), format!("{begins}{source}"));
    }
    let own = [
        (
            FlatError::Damaged {
                page,
                what: "the image of another page",
            },
            "flat page 7: the image of another page",
        ),
        (
            FlatError::ValueTooLong { len: 5_000_000_000 },
            "a value of 5000000000 bytes is longer than the value log holds",
        ),
        (
            FlatError::InlineLimit {
                limit: 3,
                min: 16,
                max: 2_048,
            },
            "an inline limit of 3 bytes is not between 16 and 2048",
        ),
        (
            FlatError::NoValue { page },
            "page 7 holds a pending value whose bytes are not with it",
        ),
        (FlatError::Freed { page }, "page 7 was freed by this writer"),
    ];
    for (error, message) in own {
        assert_eq!(error.to_string(), message);
        assert!(error.source().is_none());
        assert!(!format!("{error:?}").is_empty());
    }
}

/// A leaf of the page `page` with entries of ten bytes of value each.
fn leaf_image(page: PageId, keys: &[&[u8]]) -> Vec<u8> {
    let prefix = match keys {
        [first, .., last] => &first[..common_prefix_len(first, last)],
        [only] => only,
        [] => &[],
    };
    let mut leaf = FlatLeafBuilder::new(page, prefix, 8_192).expect("a leaf");
    for key in keys {
        leaf.push(key, FlatValue::Inline(&[1; 10]))
            .expect("an entry");
    }
    leaf.finish()
}

fn branch_image(page: PageId, level: u8, first: PageId, rest: &[(&[u8], PageId)]) -> Vec<u8> {
    let mut branch = FlatBranchBuilder::new(page, b"", 8_192, level, first).expect("a branch");
    for &(separator, child) in rest {
        branch.push(separator, child).expect("a separator");
    }
    branch.finish()
}

fn damaged<T: std::fmt::Debug>(result: Result<T, FlatError>) -> (PageId, &'static str) {
    match result {
        Err(FlatError::Damaged { page, what }) => (page, what),
        other => panic!("not refused as damage: {other:?}"),
    }
}

#[test]
fn pages_that_are_not_a_tree_are_refused() {
    let config = small();
    let tree = FlatTree::open(page_id(1), config);
    assert_eq!((tree.root(), tree.config()), (page_id(1), config));
    let with = |images: Vec<Vec<u8>>| {
        let mut pages = Mem::new(config);
        for (n, image) in images.into_iter().enumerate() {
            pages.pages.insert(page_id(n as u32 + 1), image);
        }
        pages.next_page = 100;
        pages
    };
    let (one, two, three, four, five) =
        (page_id(1), page_id(2), page_id(3), page_id(4), page_id(5));

    // The root's place holds the image of another page.
    let pages = with(vec![leaf_image(two, &[b"a"])]);
    assert_eq!(
        damaged(tree.get(&pages, b"a", &mut Vec::new())),
        (one, "the image of another page")
    );
    let mut cursor = tree.cursor();
    assert_eq!(damaged(cursor.seek_to_first(&pages)).0, one);
    assert_eq!(damaged(cursor.seek_to_last(&pages)).0, one);

    // A branch of level 2 over a leaf.
    let level = "a page that is not one level below the branch that names it";
    let mut pages = with(vec![
        branch_image(one, 2, two, &[(b"m", three)]),
        leaf_image(two, &[b"a"]),
        leaf_image(three, &[b"x"]),
    ]);
    assert_eq!(
        damaged(tree.get(&pages, b"a", &mut Vec::new())),
        (two, level)
    );
    assert_eq!(damaged(tree.put(&mut pages, b"z", b"")), (three, level));
    assert_eq!(damaged(tree.delete(&mut pages, b"a")), (two, level));
    assert_eq!(damaged(tree.cursor().seek(&pages, b"n")), (three, level));

    // A branch whose children are a branch and a leaf: found when the two
    // are to be merged, at the one that is at the wrong level.
    let mut pages = with(vec![
        branch_image(one, 2, two, &[(b"m", three)]),
        branch_image(two, 1, four, &[(b"c", five)]),
        leaf_image(three, &[b"x"]),
        leaf_image(four, &[b"a"]),
        leaf_image(five, &[b"d"]),
    ]);
    let found = damaged(tree.delete(&mut pages, b"a"));
    assert_eq!(found, (three, level));

    // A stored key of a bucket's length whose value is no bucket.
    let head = vec![b'h'; config.key_limit()];
    let stored = [head.as_slice(), &[0]].concat();
    let mut pages = with(vec![leaf_image(one, &[&stored])]);
    let long = [head.as_slice(), b"rest"].concat();
    let bucket = "a bucket of long keys that names no page";
    assert_eq!(
        damaged(tree.get(&pages, &long, &mut Vec::new())),
        (one, bucket)
    );
    assert_eq!(damaged(tree.put(&mut pages, &long, b"")), (one, bucket));
    assert_eq!(damaged(tree.delete(&mut pages, &long)), (one, bucket));
    assert_eq!(
        damaged(tree.delete_range(&mut pages, &long, None)),
        (one, bucket)
    );
    assert_eq!(damaged(tree.cursor().seek_to_first(&pages)), (one, bucket));

    // Bytes that are no flat page at all.
    let pages = with(vec![vec![3; 40]]);
    let refused = tree.get(&pages, b"a", &mut Vec::new());
    assert!(matches!(refused, Err(FlatError::Page(_))));
    // A page that is not there.
    let pages = with(Vec::new());
    let refused = tree.get(&pages, b"a", &mut Vec::new());
    assert!(matches!(refused, Err(FlatError::Freed { page }) if page == one));
}

#[test]
fn a_value_whose_bytes_are_not_with_its_page_is_an_error() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let big = vec![5; config.max_inline() + 1];
    tree.put(&mut pages, b"key", &big).expect("a put");
    assert_eq!(get(&pages, &tree, b"key"), Some(big));
    pages.pending.clear();
    let unread = tree.get(&pages, b"key", &mut Vec::new());
    assert!(matches!(unread, Err(FlatError::NoValue { page }) if page == tree.root()));
    let mut cursor = tree.cursor();
    cursor.seek_to_first(&pages).expect("a seek");
    assert!(matches!(
        cursor.value(&pages, &mut Vec::new()),
        Err(FlatError::NoValue { .. })
    ));
}
