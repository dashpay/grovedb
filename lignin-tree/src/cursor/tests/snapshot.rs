//! A cursor is stable under the version it was opened on
//! (`transactions.md` RD6): commits that come later rewrite its pages,
//! free one and give its id to another page, and the cursor reads what its
//! version has.

use super::*;

/// Goes `steps` nodes on from where `cursor` stands, or to the end, and
/// returns the records it came to.
fn go_on(
    cursor: &mut Cursor,
    reader: &NodeReader<'_, Guard>,
    steps: usize,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut buffers = LoadBuffers::new();
    let mut found = Vec::new();
    while found.len() < steps {
        let moved = match cursor.valid() {
            true => cursor.next(reader, &mut buffers),
            false => cursor.seek_to_first(reader, &mut buffers),
        };
        match arrived(cursor, moved) {
            Some(record) => found.push(record),
            None => break,
        }
    }
    found
}

/// Every record of the tree at `root`, read backward by a new cursor.
fn backward(reader: &NodeReader<'_, Guard>, root: Option<CellAddress>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(reader, root);
    let mut found = Vec::new();
    let mut moved = cursor.seek_to_last(reader, &mut buffers);
    while let Some(record) = arrived(&cursor, moved) {
        found.push(record);
        moved = cursor.prev(reader, &mut buffers);
    }
    found.reverse();
    found
}

#[test]
fn a_cursor_reads_its_version_while_commits_rewrite_free_and_reuse_its_pages() {
    let mut rng = Rng(40);
    let shared = keys(&mut rng, 600);
    let pick = |rng: &mut Rng| -> Vec<Vec<u8>> {
        let picked = shared.iter().filter(|_| rng.below(3) != 0);
        picked.cloned().collect()
    };
    let guard = Guard;
    let pool = pool_of(4);

    // Commit 1: a tree in 30 pages.
    let first = Tree::balanced(&pick(&mut rng), 1);
    let mut disk = Disk::new();
    let mut layout = Layout::new();
    let (cells, first_root) = layout.place(&first, Placement::Scattered(30), &mut rng);
    let pages: Vec<PageId> = cells.keys().copied().collect();
    disk.plant(1, &cells);
    let first_records: Vec<_> = first.records().into_iter().collect();
    let third = first_records.len() / 3;

    // A cursor is opened at version 1 and reads a third of the tree.
    let first_lease = Lease::take();
    let reader = NodeReader::new(disk.loader(&pool), &guard, first_lease, version(1));
    let mut cursor = Cursor::open(&reader, first_root);
    let mut found = go_on(&mut cursor, &reader, third);

    // Commit 2 takes every cell out of every page of the tree, frees one
    // of the pages, and puts another tree into the others: other nodes in
    // the slots the cursor's way names, many of them with the same keys.
    let (gone, kept) = pages.split_last().expect("pages");
    let second = Tree::balanced(&pick(&mut rng), 2);
    let mut again = Layout::starting_at(layout.next);
    let (mut cells, second_root) =
        again.place_among(&second, Placement::Scattered(0), kept, &mut rng);
    for &id in kept {
        let planted = cells.remove(&id).unwrap_or_default();
        disk.rewrite(&pool, id, 2, |nodes| {
            let slots: Vec<SlotId> = nodes
                .view()
                .nodes()
                .map(|node| node.address().slot())
                .collect();
            for slot in slots {
                nodes
                    .node_mut(slot)
                    .expect("a node")
                    .remove()
                    .expect("it leaves");
            }
            for cell in &planted {
                cell.insert_into(nodes);
            }
        });
    }
    disk.free(&pool, *gone, 2);

    // The cursor goes on at version 1, from where it stood.
    let reader = NodeReader::new(disk.loader(&pool), &guard, first_lease, version(1));
    found.extend(go_on(&mut cursor, &reader, third));

    // Commit 3 gives the freed id to a new page, with a third tree in it.
    let third_tree = Tree::balanced(&keys(&mut rng, PAGE_NODES), 3);
    let (cells, third_root) = Layout::starting_at(layout.next).place_among(
        &third_tree,
        Placement::Scattered(0),
        &[*gone],
        &mut rng,
    );
    disk.plant(3, &cells);

    // The cursor reads the rest of version 1, and walked from the root
    // once: no handle on its way ended.
    let reader = NodeReader::new(disk.loader(&pool), &guard, first_lease, version(1));
    found.extend(go_on(&mut cursor, &reader, usize::MAX));
    assert_eq!(found, first_records);
    assert_eq!((cursor.stats().descents, cursor.stats().refinds), (1, 0));
    assert_eq!(backward(&reader, first_root), first_records);

    // Each later version is read by its own readers, under their leases.
    let second_records: Vec<_> = second.records().into_iter().collect();
    let third_records: Vec<_> = third_tree.records().into_iter().collect();
    for (at, root, records) in [
        (2, second_root, &second_records),
        (3, second_root, &second_records),
        (3, third_root, &third_records),
    ] {
        let reader = NodeReader::new(disk.loader(&pool), &guard, Lease::take(), version(at));
        let mut cursor = Cursor::open(&reader, root);
        assert_eq!(&go_on(&mut cursor, &reader, usize::MAX), records);
        assert_eq!(&backward(&reader, root), records);
    }

    // At version 2 the freed page is no page, and a cursor whose root is
    // in it says so.
    let reader = NodeReader::new(disk.loader(&pool), &guard, Lease::take(), version(2));
    let mut cursor = Cursor::open(&reader, third_root);
    let error = cursor
        .seek_to_first(&reader, &mut LoadBuffers::new())
        .expect_err("the page is freed at version 2");
    assert!(matches!(
        error,
        CursorError::Node(NodeError::Load(lignin_store::load::LoadError::NoPage { freed: Some(at), .. }))
            if at == version(2)
    ));
}

/// Two cursors of one reader's lease, and cursors of other versions, do
/// not disturb each other: a cursor keeps nothing in the store.
#[test]
fn cursors_of_several_versions_read_side_by_side() {
    let mut rng = Rng(41);
    let mut disk = Disk::new();
    let mut layout = Layout::new();
    let pool = pool_of(2);
    let guard = Guard;

    // Three versions of one tree: each commit writes the whole tree again
    // into new pages, as a tree that grows would its changed ones.
    let mut versions = Vec::new();
    for at in 1..=3u8 {
        let tree = Tree::balanced(&keys(&mut rng, 150 * at as usize), at);
        let (cells, root) = layout.place(&tree, Placement::Beside(4), &mut rng);
        disk.plant(at as u64, &cells);
        versions.push((tree.records().into_iter().collect::<Vec<_>>(), root));
    }

    let leases = [Lease::take(), Lease::take(), Lease::take()];
    let reader = |at: usize| {
        NodeReader::new(
            disk.loader(&pool),
            &guard,
            leases[at],
            version(at as u64 + 1),
        )
    };
    // Four cursors: one for each version, and a second one at version 1.
    let at = [0, 1, 2, 0];
    let mut cursors: Vec<Cursor> = at
        .iter()
        .map(|&at| Cursor::open(&reader(at), versions[at].1))
        .collect();
    let mut found = vec![Vec::new(); cursors.len()];
    let mut ended = vec![false; cursors.len()];
    while ended.contains(&false) {
        let which = rng.below(cursors.len());
        if ended[which] {
            continue;
        }
        let steps = 1 + rng.below(9);
        let more = go_on(&mut cursors[which], &reader(at[which]), steps);
        ended[which] = more.len() < steps;
        found[which].extend(more);
    }
    for (which, records) in found.iter().enumerate() {
        assert_eq!(records, &versions[at[which]].0);
        assert_eq!(cursors[which].stats().descents, 1);
    }
}
