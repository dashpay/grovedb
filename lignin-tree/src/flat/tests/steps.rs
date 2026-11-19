//! What a write concludes from a page, and when: nothing before the page
//! is its own, and nothing is written where nothing left.

use lignin_format::flat_page::{FlatBranchBuilder, FlatLeafBuilder};

use super::*;

/// The leaf `page` without the entry of `key`, as a step of background
/// work that takes the entry out leaves it.
fn without(pages: &Mem, page: PageId, key: &[u8]) -> Vec<u8> {
    let leaf = pages.image(page).leaf().expect("a leaf");
    let capacity = pages.config.capacity();
    let mut built = FlatLeafBuilder::new(page, leaf.prefix(), capacity).expect("a leaf");
    for (found, value) in leaf.iter().filter(|(found, _)| found.to_vec() != key) {
        built.push_key(found, value).expect("an entry");
    }
    built.finish()
}

/// A step of background work takes an entry out of a leaf between a
/// write's look at the leaf and its copy of it (`transactions.md` PV7):
/// the write changes the entries of the leaf as it took it.
#[test]
fn a_write_changes_the_leaf_as_it_took_it() {
    let key = |n: u8| vec![b'k', n];
    for write in 0..6 {
        let (mut pages, tree) = Mem::tree(small());
        let mut model = BTreeMap::new();
        for n in 0..10 {
            tree.put(&mut pages, &key(n), &[n; 9]).expect("a put");
            model.insert(key(n), vec![n; 9]);
        }
        pages.commit();
        let root = tree.root();
        pages.step_before_take = Some((root, without(&pages, root, &key(2))));
        model.remove(&key(2));
        match write {
            0 => {
                tree.put(&mut pages, &key(5), b"again").expect("a put");
                model.insert(key(5), b"again".to_vec());
            }
            1 => {
                tree.put(&mut pages, &key(2), b"back").expect("a put");
                model.insert(key(2), b"back".to_vec());
            }
            2 => {
                assert!(tree.delete(&mut pages, &key(5)).expect("a delete"));
                model.remove(&key(5));
            }
            // The entry was there when the delete looked.
            3 => assert!(!tree.delete(&mut pages, &key(2)).expect("a delete")),
            4 => {
                tree.delete_range(&mut pages, &key(4), Some(&key(7)))
                    .expect("a range delete");
                model.retain(|found, _| !(key(4)..key(7)).contains(found));
            }
            _ => tree
                .delete_range(&mut pages, &key(2), Some(&key(3)))
                .expect("a range delete"),
        }
        assert!(pages.step_before_take.is_none(), "the leaf was not taken");
        let expected: Vec<_> = model.into_iter().collect();
        assert!(records(&pages, &tree) == expected, "write {write}");
        pages.commit();
    }
}

fn leaf_image(page: PageId, keys: &[&[u8]]) -> Vec<u8> {
    let mut leaf = FlatLeafBuilder::new(page, b"", 4_096).expect("a leaf");
    for key in keys {
        leaf.push(key, FlatValue::Inline(b"value"))
            .expect("an entry");
    }
    leaf.finish()
}

fn branch_image(page: PageId, first: PageId, rest: &[(&[u8], PageId)]) -> Vec<u8> {
    let mut branch = FlatBranchBuilder::new(page, b"", 4_096, 1, first).expect("a branch");
    for &(separator, child) in rest {
        branch.push(separator, child).expect("a separator");
    }
    branch.finish()
}

fn with(images: Vec<Vec<u8>>) -> (Mem, FlatTree) {
    let mut pages = Mem::new(small());
    for (n, image) in images.into_iter().enumerate() {
        pages.pages.insert(page_id(n as u32 + 1), image);
    }
    pages.base = pages.pages.clone();
    pages.next_page = 100;
    (pages, FlatTree::open(page_id(1), small()))
}

/// Leaves that are far under a quarter of a page and would fit one page
/// together are left alone by a range delete that takes nothing out of
/// them: it copies no page, writes none, and leaves nothing for the log.
#[test]
fn a_range_delete_that_removes_nothing_changes_nothing() {
    let (one, two, three, four) = (page_id(1), page_id(2), page_id(3), page_id(4));
    let (mut pages, tree) = with(vec![
        branch_image(one, two, &[(b"h", three), (b"p", four)]),
        leaf_image(two, &[b"a", b"c"]),
        leaf_image(three, &[b"i", b"k"]),
        leaf_image(four, &[b"q", b"s"]),
    ]);
    let before = pages.pages.clone();
    let unchanged = |pages: &mut Mem| {
        assert!(pages.taken.is_empty() && pages.freed.is_empty());
        assert!(pages.pages == before);
        assert_eq!(pages.commit(), (Vec::new(), 0));
    };
    for (start, end) in [
        (&b"b"[..], Some(&b"c"[..])),
        (b"d", Some(b"i")),
        (b"j", Some(b"j\0")),
        (b"l", Some(b"q")),
        (b"t", None),
        (b"k", Some(b"a")),
        (b"k", Some(b"k")),
    ] {
        tree.delete_range(&mut pages, start, end)
            .expect("a range delete");
        unchanged(&mut pages);
    }
    for prefix in [&b"b"[..], b"j", b"z", b"ia"] {
        tree.delete_prefix(&mut pages, prefix)
            .expect("a prefix delete");
        unchanged(&mut pages);
    }

    // One that takes an entry looks at the leaf that lost it: the leaves
    // are merged, one after the other, and the root is the leaf that is
    // left.
    tree.delete_range(&mut pages, b"i", Some(b"j"))
        .expect("a range delete");
    let found: Vec<Vec<u8>> = records(&pages, &tree)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(found, [&b"a"[..], b"c", b"k", b"q", b"s"]);
    assert_eq!(pages.pages.len(), 1);
}

/// Pages that name each other in a ring, or one page twice, end a write
/// with an error and not without an end.
#[test]
fn pages_that_are_reached_twice_end_a_write_with_an_error() {
    let (one, two, three) = (page_id(1), page_id(2), page_id(3));
    let level = "a page that is not one level below the branch that names it";
    let damaged = |result: Result<(), FlatError>| match result {
        Err(FlatError::Damaged { page, what }) => (page, what),
        other => panic!("not refused as damage: {other:?}"),
    };

    // A branch that is its own child.
    let ring = || {
        with(vec![
            branch_image(one, two, &[(b"m", one)]),
            leaf_image(two, &[b"a"]),
        ])
    };
    let (mut pages, tree) = ring();
    assert_eq!(
        damaged(tree.delete_range(&mut pages, b"", None)),
        (one, level)
    );
    let (mut pages, tree) = ring();
    assert_eq!(
        damaged(tree.delete_range(&mut pages, b"n", Some(b"o"))),
        (one, level)
    );
    let (mut pages, tree) = ring();
    assert_eq!(damaged(tree.put(&mut pages, b"x", b"")), (one, level));
    assert_eq!(damaged(tree.cursor().seek_to_last(&pages)), (one, level));

    // A leaf that a branch names twice is freed once.
    let (mut pages, tree) = with(vec![
        branch_image(one, two, &[(b"h", three), (b"p", three)]),
        leaf_image(two, &[b"a"]),
        leaf_image(three, &[b"i"]),
    ]);
    let twice = tree.delete_range(&mut pages, b"b", None);
    assert!(matches!(twice, Err(FlatError::Freed { page }) if page == three));

    // A bucket whose tree is the tree it is in.
    let config = small();
    let head = vec![b'h'; config.key_limit()];
    let mut leaf = FlatLeafBuilder::new(one, b"", 4_096).expect("a leaf");
    leaf.push(
        &[head.as_slice(), &[0]].concat(),
        FlatValue::Inline(&one.encode()),
    )
    .expect("an entry");
    let (mut pages, tree) = with(vec![leaf.finish()]);
    // Every key is as many trees deep as it has heads, and no deeper.
    let long = [head.as_slice(), &head, &head, b"rest"].concat();
    assert_eq!(get(&pages, &tree, &long), None);
    // A cursor goes into the tree of a bucket once.
    let ring = "a bucket of long keys whose tree is a tree it is in";
    let mut cursor = tree.cursor();
    for seek in [Cursor::seek_to_first::<Mem>, Cursor::seek_to_last::<Mem>] {
        assert_eq!(damaged(seek(&mut cursor, &pages)), (one, ring));
        assert!(!cursor.valid());
    }
    // A seek goes as deep as its key is long, and finds nothing there.
    cursor.seek(&pages, &long).expect("a seek");
    assert!(!cursor.valid());
    let inside = tree.delete_range(&mut pages, b"", None);
    assert!(matches!(inside, Err(FlatError::Freed { page }) if page == one));

    // The same with a record before the bucket: the step that would go
    // into the tree again is the one that fails.
    let mut leaf = FlatLeafBuilder::new(one, b"", 4_096).expect("a leaf");
    leaf.push(b"a", FlatValue::Inline(b"value"))
        .expect("an entry");
    leaf.push(
        &[head.as_slice(), &[0]].concat(),
        FlatValue::Inline(&one.encode()),
    )
    .expect("an entry");
    let (pages, tree) = with(vec![leaf.finish()]);
    let mut cursor = tree.cursor();
    cursor.seek_to_first(&pages).expect("a seek");
    assert_eq!(cursor.key(), Some(&b"a"[..]));
    assert_eq!(damaged(cursor.next(&pages)), (one, ring));
    assert!(!cursor.valid());

    // A bucket whose value is out of line names no page, for a range
    // delete as for a read: the value is not dropped as a record's.
    let place = ValuePointer::new(FileId::MIN, 20, 600)
        .expect("a pointer")
        .encode();
    let mut leaf = FlatLeafBuilder::new(one, b"", 4_096).expect("a leaf");
    leaf.push(
        &[head.as_slice(), &[0]].concat(),
        FlatValue::OutOfLine(&place),
    )
    .expect("an entry");
    let (mut pages, tree) = with(vec![leaf.finish()]);
    let names = "a bucket of long keys that names no page";
    assert_eq!(
        damaged(tree.delete_range(&mut pages, b"", None)),
        (one, names)
    );
    assert!(pages.released.is_empty());
}
