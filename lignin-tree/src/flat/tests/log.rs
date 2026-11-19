//! What a commit logs for a flat page.

use lignin_format::flat_page::{FlatBranchBuilder, FlatLeafBuilder};

use super::{super::log::stamp_pending, *};

/// The records of the run that the last commit wrote for the root.
fn root_run(pages: &Mem, tree: &FlatTree) -> Vec<String> {
    pages.last_run(tree.root())
}

#[test]
fn a_run_holds_what_the_changes_to_its_page_come_to() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let key = |n: u32| [&b"key-"[..], &n.to_be_bytes()].concat();

    // A page the commit made is an image.
    for n in 0..40 {
        tree.put(&mut pages, &key(n * 2), &[7; 100]).expect("a put");
    }
    let (changed, logged) = pages.commit();
    assert_eq!(changed, [tree.root()]);
    assert_eq!(root_run(&pages, &tree), ["Image"]);
    let image = pages.pages[&tree.root()].len();
    assert_eq!(logged, 4 + 3 + image);

    // A put is an insert or a replace, of the length of its entry.
    tree.put(&mut pages, &key(5), &[1; 30]).expect("a put");
    assert_eq!(pages.commit().1, 4 + 6 + 8 + 30);
    assert_eq!(root_run(&pages, &tree), ["Insert"]);
    tree.put(&mut pages, &key(4), &[2; 30]).expect("a put");
    pages.commit();
    assert_eq!(root_run(&pages, &tree), ["Replace"]);

    // A delete is a remove, and deletes that are neighbours are a range.
    assert!(tree.delete(&mut pages, &key(4)).expect("a delete"));
    assert_eq!(pages.commit().1, 4 + 3 + 8);
    assert_eq!(root_run(&pages, &tree), ["Remove"]);
    tree.delete_range(&mut pages, &key(10), Some(&key(30)))
        .expect("a range delete");
    assert_eq!(pages.commit().1, 4 + 5 + 8);
    assert_eq!(root_run(&pages, &tree), ["RemoveRange"]);

    // Many writes of one page between two commits are one run of what is
    // left of them: a key that came and went is in no record.
    tree.put(&mut pages, &key(41), b"comes").expect("a put");
    tree.put(&mut pages, &key(41), b"and goes").expect("a put");
    assert!(tree.delete(&mut pages, &key(41)).expect("a delete"));
    assert_eq!(pages.commit(), (Vec::new(), 0));
    assert!(tree.delete(&mut pages, &key(0)).expect("a delete"));
    assert!(tree.delete(&mut pages, &key(2)).expect("a delete"));
    assert!(tree.delete(&mut pages, &key(60)).expect("a delete"));
    tree.put(&mut pages, &key(1), b"new").expect("a put");
    tree.put(&mut pages, &key(62), b"other").expect("a put");
    pages.commit();
    assert_eq!(
        root_run(&pages, &tree),
        ["RemoveRange", "Remove", "Insert", "Replace"]
    );

    // Where a page holds other entries altogether, the image is shorter
    // than the records.
    tree.delete_range(&mut pages, b"", None)
        .expect("a range delete");
    for n in 0..40 {
        tree.put(&mut pages, &key(1_000 + n), &[9; 90])
            .expect("a put");
    }
    let (_, logged) = pages.commit();
    assert_eq!(root_run(&pages, &tree), ["Image"]);
    assert_eq!(logged, 4 + 3 + pages.pages[&tree.root()].len());
}

#[test]
fn a_split_and_a_merge_are_records_of_the_pages_they_touch() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let key = |n: u32| [&b"key-"[..], &n.to_be_bytes()].concat();
    for n in 0..60 {
        tree.put(&mut pages, &key(n), &[7; 100]).expect("a put");
    }
    pages.commit();
    assert!(pages.image(tree.root()).leaf().is_some());

    // The root splits: it is another kind of page, so it is an image, and
    // so are the two new leaves.
    for n in 60..80 {
        tree.put(&mut pages, &key(n), &[7; 100]).expect("a put");
    }
    let (changed, _) = pages.commit();
    assert_eq!(changed.len(), 3);
    assert!(changed
        .iter()
        .all(|&page| pages.last_run(page) == ["Image"]));
    let root = pages.image(tree.root()).branch().expect("a branch");
    let (first, second) = (
        root.child(0).expect("a child"),
        root.child(1).expect("a child"),
    );

    // A leaf below a branch splits in its middle: the half that leaves it
    // is a range of removes, the new leaf is an image, and the branch gets
    // a child.
    let between = |n: u32| [key(n), b"+".to_vec()].concat();
    for n in 0..60 {
        tree.put(&mut pages, &between(n), &[7; 100]).expect("a put");
        if pages.image(tree.root()).len() > 1 {
            break;
        }
    }
    let (changed, _) = pages.commit();
    let new = *changed.last().expect("a new page");
    // The new separator is longer than the one the branch had, so the
    // two share less than one alone, and the branch's prefix is shorter.
    assert_eq!(pages.last_run(tree.root()), ["InsertChild", "SetPrefix"]);
    assert_eq!(pages.last_run(new), ["Image"]);
    let split = pages.last_run(first);
    assert_eq!(
        split.first().map(String::as_str),
        Some("RemoveRange"),
        "{split:?}"
    );
    assert!(split[1..]
        .iter()
        .all(|record| record == "Insert" || record == "SetPrefix"));

    // Two leaves merge: the left one takes the entries, the right one is
    // freed, and the branch loses the separator with the child to its
    // right.
    let root = pages.image(tree.root()).branch().expect("a branch");
    assert_eq!(
        (root.child(0), root.child(1), root.child(2)),
        (Some(first), Some(new), Some(second))
    );
    tree.delete_range(&mut pages, &key(4), Some(&key(55)))
        .expect("a range delete");
    let (changed, _) = pages.commit();
    assert!(changed.contains(&tree.root()) && changed.contains(&first));
    assert_eq!(pages.last_run(tree.root()), ["Remove", "SetPrefix"]);
    assert_eq!(pages.log.last().expect("a commit").freed, [new]);
    let root = pages.image(tree.root()).branch().expect("a branch");
    assert_eq!(
        (root.len(), root.child(0), root.child(1)),
        (1, Some(first), Some(second))
    );
}

#[test]
fn a_branch_that_no_record_describes_is_an_image() {
    let page = page_id(9);
    let child = |n: u32| page_id(100 + n);
    let branch = |level: u8, first: u32, separators: &[(&[u8], u32)], prefix: &[u8]| {
        let mut image =
            FlatBranchBuilder::new(page, prefix, 8_192, level, child(first)).expect("a branch");
        for &(separator, to) in separators {
            image.push(separator, child(to)).expect("a separator");
        }
        image.finish()
    };
    let run = |base: &[u8], image: &[u8]| {
        let mut bytes = Vec::new();
        let mut run = FlatRunWriter::new(page, &mut bytes);
        push_changes(Some(base), image, &mut run).expect("records");
        run.finish().expect("a run");
        let run = FlatRun::decode(&bytes, V1).expect("a run");
        let mut applied = Vec::new();
        run.apply(Some(base), V1, 8_192, &mut applied)
            .expect("the run applies");
        assert_eq!(applied, image);
        run.changes()
            .map(|change| {
                format!("{change:?}")
                    .split([' ', '('])
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let wide: Vec<Vec<u8>> = (0..40u8).map(|n| [&b"ab"[..], &[n; 30]].concat()).collect();
    let wide: Vec<(&[u8], u32)> = wide
        .iter()
        .zip(1..)
        .map(|(key, n)| (key.as_slice(), n))
        .collect();
    let base = branch(1, 0, &wide, b"ab");

    // Another first child, and another level: no record says either.
    assert_eq!(run(&base, &branch(1, 77, &wide, b"ab")), ["Image"]);
    assert_eq!(run(&base, &branch(2, 0, &wide, b"ab")), ["Image"]);
    // A leaf in the place of a branch.
    let leaf = FlatLeafBuilder::new(page, b"", 8_192)
        .expect("a leaf")
        .finish();
    assert_eq!(run(&base, &leaf), ["Image"]);
    assert_eq!(run(&leaf, &base), ["Image"]);

    // A separator that gets another child leaves and comes again.
    let mut other = wide.clone();
    other[7].1 = 70;
    assert_eq!(
        run(&base, &branch(1, 0, &other, b"ab")),
        ["Remove", "InsertChild"]
    );
    // Separators that leave together are a range, also around one that
    // arrives between them.
    let mut fewer = wide.clone();
    let arrives = [wide[3].0, b"+"].concat();
    fewer.splice(3..6, [(arrives.as_slice(), 90)]);
    assert_eq!(
        run(&base, &branch(1, 0, &fewer, b"ab")),
        ["RemoveRange", "InsertChild"]
    );
    // Another prefix alone is one record.
    assert_eq!(run(&base, &branch(1, 0, &wide, b"a")), ["SetPrefix"]);
    assert_eq!(run(&base, &branch(1, 0, &wide, b"")), ["SetPrefix"]);

    assert!(!is_changed(Some(&base), &base).expect("pages"));
    assert!(is_changed(Some(&base), &leaf).expect("pages"));
    assert!(is_changed(None, &base).expect("pages"));
    // A frame holds its image at its start, and what follows is not it.
    let frame = [base.clone(), vec![0xee; 100]].concat();
    assert!(!is_changed(Some(&frame), &base).expect("pages"));
    assert!(is_changed(Some(&[0; 40]), &base).is_err());
}

#[test]
fn a_commit_puts_the_place_of_a_new_value_into_its_entry() {
    let pending =
        |id: u64, len: u32| PendingValue::new(id.to_le_bytes(), len).expect("a pending value");
    let place = |value: PendingValue| ValuePlace::Pending(value).encode();
    let pointer = ValuePointer::new(FileId::MIN, 20, 300)
        .expect("a pointer")
        .encode();
    let (first, second) = (
        place(pending(1 << 63 | 1, 300)),
        place(pending(1 << 63 | 2, 300)),
    );
    let mut leaf = FlatLeafBuilder::new(page_id(4), b"key-", 8_192).expect("a leaf");
    leaf.push(b"key-a", FlatValue::Inline(&[3; 16]))
        .expect("an entry");
    leaf.push(b"key-b", FlatValue::OutOfLine(&first))
        .expect("an entry");
    leaf.push(b"key-c", FlatValue::OutOfLine(&pointer))
        .expect("an entry");
    leaf.push(b"key-d", FlatValue::OutOfLine(&second))
        .expect("an entry");
    // Value bytes that are those of a place, in the page, are a value.
    leaf.push(b"key-e", FlatValue::Inline(&second))
        .expect("an entry");
    let image = leaf.finish();

    let logged = pending(45, 300);
    let mut stamped = [image.clone(), vec![0; 50]].concat();
    assert!(stamp_pending(&mut stamped, 3, logged).expect("a leaf"));
    let read = FlatPage::decode(&stamped, V1)
        .expect("a page")
        .leaf()
        .expect("a leaf");
    assert_eq!(
        read.get(b"key-d"),
        Some(FlatValue::OutOfLine(&place(logged)))
    );
    assert_eq!(read.get(b"key-b"), Some(FlatValue::OutOfLine(&first)));
    assert_eq!(read.get(b"key-e"), Some(FlatValue::Inline(&second)));
    let differ = image.iter().zip(&stamped).filter(|(a, b)| a != b).count();
    assert!(differ <= 8 && stamped[image.len()..] == [0; 50]);

    // An entry with a value in the page, one with a pointer, one that is
    // not there and one whose pending value has another length are left
    // alone, and a branch has no entry.
    let before = stamped.clone();
    for slot in [0, 2, 4, 5] {
        assert!(!stamp_pending(&mut stamped, slot, logged).expect("a leaf"));
    }
    assert!(!stamp_pending(&mut stamped, 1, pending(46, 301)).expect("a leaf"));
    assert_eq!(stamped, before);
    let mut branch = FlatBranchBuilder::new(page_id(4), b"", 8_192, 1, page_id(5))
        .expect("a branch")
        .finish();
    assert!(!stamp_pending(&mut branch, 0, logged).expect("a page"));
    assert!(stamp_pending(&mut [0; 12], 0, logged).is_err());
}
