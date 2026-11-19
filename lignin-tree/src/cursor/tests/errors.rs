//! What a cursor refuses, and what it does with a page it cannot read.

use std::error::Error;

use lignin_store::load::LoadError;

use super::*;

fn address(page_raw: u32, slot: u16) -> CellAddress {
    CellAddress::new(page(page_raw), SlotId::new(slot).expect("not zero"))
}

/// A cell with the key `key` in `slot`, with the children it is given.
fn cell(slot: u16, key: &[u8], left: Option<CellAddress>, right: Option<CellAddress>) -> Planted {
    Planted {
        slot: SlotId::new(slot).expect("not zero"),
        key: key.to_vec(),
        value: value_of(1, key),
        left: left.map(|child| (child, 1)),
        right: right.map(|child| (child, 1)),
        fragment_root: true,
    }
}

#[test]
fn a_reader_of_another_version_or_lease_is_refused_and_the_cursor_stays() {
    let store = planted(Tree::balanced(&numbered(20), 1), Placement::Beside(4), 80);
    let pool = pool_of(2);
    let guard = Guard;
    let lease = Lease::take();
    let loader = store.disk.loader(&pool);
    let reader = NodeReader::new(loader, &guard, lease, version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, store.root);
    let key = numbered_key;
    let moved = cursor.seek(&reader, &mut buffers, &key(7));
    assert_eq!(arrived(&cursor, moved).expect("a node").0, key(7));
    let (handle, stats) = (cursor.handle(), cursor.stats());

    let other_lease = Lease::take();
    let later = NodeReader::new(loader, &guard, lease, version(2));
    let stranger = NodeReader::new(loader, &guard, other_lease, version(1));
    for step in MOVES {
        for (other, at, under) in [(&later, 2, lease), (&stranger, 1, other_lease)] {
            let error = step(&mut cursor, other, &mut buffers).expect_err("another reader");
            assert!(matches!(
                error,
                CursorError::OtherReader { opened_at, opened_under, given_at, given_under }
                    if (opened_at, opened_under) == (version(1), lease)
                        && (given_at, given_under) == (version(at), under)
            ));
            assert_eq!(
                error.to_string(),
                format!(
                    "a cursor opened at {} under {lease} was given a reader at {} under {under}",
                    version(1),
                    version(at)
                )
            );
            assert!(error.source().is_none());
            // The cursor is as it was.
            assert_eq!(cursor.key(), Some(key(7).as_slice()));
            assert_eq!((cursor.handle(), cursor.stats()), (handle, stats));
        }
    }
    let moved = cursor.next(&reader, &mut buffers);
    assert_eq!(arrived(&cursor, moved).expect("a node").0, key(8));
}

#[test]
fn a_root_or_a_link_that_names_no_node_is_an_error_and_leaves_no_position() {
    // "m" at the root, with "d" on its left and a right link into a page
    // that is not there. "d" has a left link to a slot that holds no cell.
    let mut disk = Disk::new();
    let mut cells = Cells::new();
    cells.insert(
        page(1),
        vec![
            cell(1, b"m", Some(address(1, 2)), Some(address(9, 1))),
            cell(2, b"d", Some(address(1, 5)), None),
        ],
    );
    disk.plant(1, &cells);
    let pool = pool_of(2);
    let guard = Guard;
    let reader = NodeReader::new(disk.loader(&pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let root = Some(address(1, 1));

    // A root in a slot that holds no cell.
    let mut cursor = Cursor::open(&reader, Some(address(1, 7)));
    let error = cursor
        .seek_to_first(&reader, &mut buffers)
        .expect_err("no cell");
    assert!(matches!(
        error,
        CursorError::Node(NodeError::NoNode { address: at }) if at == address(1, 7)
    ));
    assert_eq!(
        error.to_string(),
        format!("cursor: no node at {}", address(1, 7))
    );
    assert!(error
        .source()
        .is_some_and(|source| source.is::<NodeError>()));
    assert!(!cursor.valid());

    // A walk down that meets the bad link, from the root and in a step.
    cursor.retarget(root);
    fn seek_a<'r>(
        cursor: &mut Cursor,
        reader: &NodeReader<'r, Guard>,
        buffers: &mut LoadBuffers,
    ) -> Came<'r> {
        cursor.seek(reader, buffers, b"a")
    }
    fn seek_a_for_prev<'r>(
        cursor: &mut Cursor,
        reader: &NodeReader<'r, Guard>,
        buffers: &mut LoadBuffers,
    ) -> Came<'r> {
        cursor.seek_for_prev(reader, buffers, b"a")
    }
    let ways_down: [Move; 3] = [FIRST, seek_a, seek_a_for_prev];
    for go_down in ways_down {
        let moved = cursor.seek(&reader, &mut buffers, b"m");
        assert_eq!(arrived(&cursor, moved).expect("a node").0, b"m");
        let error = go_down(&mut cursor, &reader, &mut buffers).expect_err("no cell");
        assert!(matches!(
            error,
            CursorError::Node(NodeError::NoNode { address: at }) if at == address(1, 5)
        ));
        assert!(!cursor.valid() && cursor.key().is_none() && cursor.depth().is_none());
    }

    let moved = cursor.seek(&reader, &mut buffers, b"m");
    assert_eq!(arrived(&cursor, moved).expect("a node").0, b"m");
    let error = cursor.next(&reader, &mut buffers).expect_err("no page");
    assert!(matches!(
        error,
        CursorError::Node(NodeError::Load(LoadError::NoPage { freed: None, .. }))
    ));
    assert!(error.to_string().starts_with("cursor: page load: "));
    assert!(!cursor.valid());
    // A cursor that an error left is not positioned, and can seek again.
    assert_eq!(go(&mut cursor, &reader, &mut buffers, NEXT), None);
    let moved = cursor.seek_for_prev(&reader, &mut buffers, b"z");
    let error = moved.expect_err("the way to the greatest key is the bad link");
    assert!(matches!(error, CursorError::Node(NodeError::Load(_))));
    let moved = cursor.seek_for_prev(&reader, &mut buffers, b"k");
    assert_eq!(arrived(&cursor, moved).expect("a node").0, b"d");
    let moved = cursor.prev(&reader, &mut buffers);
    assert!(matches!(
        moved,
        Err(CursorError::Node(NodeError::NoNode { .. }))
    ));
}

/// A way down is as long as the heights in a link allow, and no longer:
/// links that lead back into the tree end a call with an error.
#[test]
fn a_way_that_is_longer_than_any_tree_is_refused() {
    let guard = Guard;
    // A tree that only ever grew to the right: a chain.
    for (nodes, fits) in [(Cursor::MAX_DEPTH, true), (Cursor::MAX_DEPTH + 1, false)] {
        let tree = Tree::grown(&numbered(nodes), 1);
        let store = planted(tree, Placement::Beside(PAGE_NODES), 81);
        let pool = pool_of(2);
        let reader = NodeReader::new(store.disk.loader(&pool), &guard, Lease::take(), version(1));
        let mut buffers = LoadBuffers::new();
        let mut cursor = Cursor::open(&reader, store.root);
        let moved = cursor.seek_to_last(&reader, &mut buffers);
        if fits {
            let last = arrived(&cursor, moved).expect("the last node");
            assert_eq!(last.0, numbered_key(nodes as u32 - 1));
            assert_eq!(cursor.depth(), Some(Cursor::MAX_DEPTH - 1));
            let moved = cursor.prev(&reader, &mut buffers);
            assert_eq!(
                arrived(&cursor, moved).expect("a node").0,
                numbered_key(nodes as u32 - 2)
            );
        } else {
            let error = moved.expect_err("too long a way");
            assert!(matches!(
                error,
                CursorError::TooDeep {
                    limit: Cursor::MAX_DEPTH,
                    ..
                }
            ));
            assert!(!cursor.valid());
            // The nodes that are not that deep are reached.
            let key = numbered_key(99);
            let moved = cursor.seek(&reader, &mut buffers, &key);
            assert_eq!(arrived(&cursor, moved).expect("a node").0, key);
        }
    }

    // A node whose left link names the node itself.
    let mut disk = Disk::new();
    let mut cells = Cells::new();
    cells.insert(page(1), vec![cell(1, b"m", Some(address(1, 1)), None)]);
    disk.plant(1, &cells);
    let pool = pool_of(1);
    let reader = NodeReader::new(disk.loader(&pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, Some(address(1, 1)));
    for moved in [
        cursor.seek_to_first(&reader, &mut buffers),
        cursor.seek(&reader, &mut buffers, b"a"),
    ] {
        let error = moved.expect_err("the way never ends");
        assert!(matches!(
            error,
            CursorError::TooDeep { at, limit } if at == address(1, 1) && limit == Cursor::MAX_DEPTH
        ));
        assert_eq!(
            error.to_string(),
            format!(
                "the way down to the node at {} is longer than {} nodes",
                address(1, 1),
                Cursor::MAX_DEPTH
            )
        );
        assert!(error.source().is_none());
    }
}

/// A step checks that it went on, so a scan of a tree whose links lead
/// back, or whose keys are not in order, ends with an error and not never.
#[test]
fn a_step_that_does_not_go_on_is_refused() {
    let guard = Guard;
    // "m", whose left link names "m" itself; "b" and "c", each the right
    // child of the other; and "t", whose right child has a smaller key.
    let mut disk = Disk::new();
    let mut cells = Cells::new();
    cells.insert(
        page(1),
        vec![
            cell(1, b"m", Some(address(1, 1)), None),
            cell(2, b"b", None, Some(address(1, 3))),
            cell(3, b"c", None, Some(address(1, 2))),
            cell(4, b"t", None, Some(address(1, 5))),
            cell(5, b"s", None, None),
        ],
    );
    disk.plant(1, &cells);
    let pool = pool_of(1);
    let reader = NodeReader::new(disk.loader(&pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();

    for (root, from, step, comes_to, back) in [
        (address(1, 1), &b"m"[..], PREV, address(1, 1), true),
        (address(1, 3), b"c", NEXT, address(1, 2), false),
        (address(1, 4), b"t", NEXT, address(1, 5), false),
    ] {
        let mut cursor = Cursor::open(&reader, Some(root));
        let moved = cursor.seek(&reader, &mut buffers, from);
        assert_eq!(arrived(&cursor, moved).expect("a node").0, from);
        let error = step(&mut cursor, &reader, &mut buffers).expect_err("no step on");
        assert!(matches!(
            error,
            CursorError::OutOfOrder { at, back: went_back } if at == comes_to && went_back == back
        ));
        assert_eq!(
            error.to_string(),
            format!(
                "the key of the node at {comes_to} is not {} the key the cursor stepped from",
                if back { "before" } else { "after" }
            )
        );
        assert!(error.source().is_none());
        assert!(!cursor.valid());
    }

    // The step from "b" to "c" goes on, and the scan ends at the next one.
    let mut cursor = Cursor::open(&reader, Some(address(1, 2)));
    let moved = cursor.seek_to_first(&reader, &mut buffers);
    assert_eq!(arrived(&cursor, moved).expect("a node").0, b"b");
    let on = go(&mut cursor, &reader, &mut buffers, NEXT);
    assert_eq!(on.expect("a node").0, b"c");
    let error = cursor.next(&reader, &mut buffers).expect_err("back to b");
    assert!(matches!(error, CursorError::OutOfOrder { back: false, .. }));
}

#[test]
fn a_cursor_and_its_counts_are_shown_and_copied() {
    let store = planted(Tree::balanced(&numbered(5), 1), Placement::Own, 82);
    let pool = pool_of(1);
    let guard = Guard;
    let reader = NodeReader::new(store.disk.loader(&pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, store.root);
    assert_eq!(cursor.stats(), CursorStats::default());
    let moved = cursor.seek_to_first(&reader, &mut buffers);
    assert!(arrived(&cursor, moved).is_some());

    // A copy goes its own way from where the cursor stands.
    let mut copy = cursor.clone();
    let moved = copy.next(&reader, &mut buffers);
    assert_eq!(arrived(&copy, moved).expect("a node").0, numbered_key(1));
    assert_eq!(cursor.key(), Some(numbered_key(0).as_slice()));
    let shown = format!("{cursor:?}");
    assert!(shown.starts_with("Cursor {") && shown.contains("stats: CursorStats {"));
}
