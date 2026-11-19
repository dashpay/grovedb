//! A cursor against an ordered map, over random moves.

use std::ops::Bound;

use super::*;

type Records = BTreeMap<Vec<u8>, Vec<u8>>;

/// What the map says a move comes to.
struct Expect<'m> {
    records: &'m Records,
}

impl Expect<'_> {
    fn range(
        &self,
        low: Bound<&[u8]>,
        high: Bound<&[u8]>,
    ) -> std::collections::btree_map::Range<'_, Vec<u8>, Vec<u8>> {
        self.records.range::<[u8], _>((low, high))
    }

    fn at_or_after(&self, key: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        self.pick(self.range(Bound::Included(key), Bound::Unbounded).next())
    }

    fn at_or_before(&self, key: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        self.pick(
            self.range(Bound::Unbounded, Bound::Included(key))
                .next_back(),
        )
    }

    fn after(&self, key: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        self.pick(self.range(Bound::Excluded(key), Bound::Unbounded).next())
    }

    fn before(&self, key: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        self.pick(
            self.range(Bound::Unbounded, Bound::Excluded(key))
                .next_back(),
        )
    }

    fn pick(&self, record: Option<(&Vec<u8>, &Vec<u8>)>) -> Option<(Vec<u8>, Vec<u8>)> {
        record.map(|(key, value)| (key.clone(), value.clone()))
    }
}

/// A key to look for: one of the tree's, or one that may lie between two
/// of them, before the first or after the last.
fn probe(rng: &mut Rng, records: &Records) -> Vec<u8> {
    if rng.below(3) == 0 || records.is_empty() {
        return keys(rng, 1).pop().expect("one key");
    }
    let mut key = records
        .keys()
        .nth(rng.below(records.len()))
        .expect("in range")
        .clone();
    match rng.below(4) {
        0 => key.push(0),
        1 => {
            key.pop();
        }
        _ => {}
    }
    key
}

/// Moves a cursor over `trees`, which are in one store, by chance, and
/// compares every move with the map of the tree it is on.
fn check(
    trees: &[(Tree, Option<CellAddress>)],
    disk: &Disk,
    frames: usize,
    seed: u64,
    moves: usize,
) {
    let pool = pool_of(frames);
    let guard = Guard;
    let lease = Lease::take();
    let mut buffers = LoadBuffers::new();
    let mut rng = Rng(seed);
    let records: Vec<Records> = trees.iter().map(|(tree, _)| tree.records()).collect();

    let mut on = 0;
    let mut here: Option<(Vec<u8>, Vec<u8>)> = None;
    let opened = NodeReader::new(disk.loader(&pool), &guard, lease, version(1));
    let mut cursor = Cursor::open(&opened, trees[on].1);
    assert_eq!((cursor.at(), cursor.lease()), (version(1), lease));
    for _ in 0..moves {
        // Every move is an operation of its own, with a reader of its own.
        let reader = NodeReader::new(disk.loader(&pool), &guard, lease, version(1));
        let expect = Expect {
            records: &records[on],
        };
        let position = here.as_ref().map(|(key, _)| key.as_slice());
        let (moved, expected) = match rng.below(12) {
            0 | 1 => {
                let key = probe(&mut rng, expect.records);
                (
                    cursor.seek(&reader, &mut buffers, &key),
                    expect.at_or_after(&key),
                )
            }
            2 | 3 => {
                let key = probe(&mut rng, expect.records);
                (
                    cursor.seek_for_prev(&reader, &mut buffers, &key),
                    expect.at_or_before(&key),
                )
            }
            4 => (
                cursor.seek_to_first(&reader, &mut buffers),
                expect.at_or_after(&[]),
            ),
            5 => (
                cursor.seek_to_last(&reader, &mut buffers),
                expect.pick(expect.records.iter().next_back()),
            ),
            6..=8 => (
                cursor.next(&reader, &mut buffers),
                position.and_then(|key| expect.after(key)),
            ),
            9 | 10 => (
                cursor.prev(&reader, &mut buffers),
                position.and_then(|key| expect.before(key)),
            ),
            _ if rng.below(4) == 0 => {
                on = rng.below(trees.len());
                cursor.retarget(trees[on].1);
                assert_eq!(cursor.root(), trees[on].1);
                assert!(!cursor.valid() && cursor.key().is_none());
                (Ok(None), None)
            }
            _ => (cursor.node(&reader, &mut buffers), here.clone()),
        };
        here = arrived(&cursor, moved);
        assert_eq!(here, expected);
    }
    // Every seek walked from the root, and nothing else did.
    assert_eq!(cursor.stats().refinds, 0);
    assert_eq!(pool.stats().overflows, 0);
}

#[test]
fn random_moves_agree_with_an_ordered_map() {
    let shapes: [(usize, bool, Placement); 6] = [
        (1, true, Placement::Own),
        (2, true, Placement::Beside(2)),
        (40, false, Placement::Beside(PAGE_NODES)),
        (300, true, Placement::Own),
        (700, false, Placement::Scattered(40)),
        (1_200, true, Placement::Beside(5)),
    ];
    for (seed, (nodes, balanced, placement)) in shapes.into_iter().enumerate() {
        let seed = seed as u64 + 10;
        let mut rng = Rng(seed);
        let mut keys = keys(&mut rng, nodes);
        let tree = if balanced {
            Tree::balanced(&keys, 1)
        } else {
            rng.shuffle(&mut keys);
            Tree::grown(&keys, 1)
        };
        let store = planted(tree, placement, seed);
        for frames in [1, 3, 64] {
            let trees = [(store.tree.clone(), store.root)];
            check(&trees, &store.disk, frames, seed, 2_500);
        }
    }
}

/// A page holds the cells of any number of Merks (`tree-layout.md` F2),
/// and the same key can be in several of them. A cursor reads the nodes of
/// its own tree, and after a retarget those of the other.
#[test]
fn trees_that_share_pages_and_keys_are_read_apart() {
    let mut rng = Rng(20);
    let shared = keys(&mut rng, 300);
    let mut layout = Layout::new();
    let mut disk = Disk::new();
    let pages: Vec<PageId> = (0..40).map(|_| layout.new_page()).collect();
    let mut cells = Cells::new();
    let mut trees = Vec::new();
    for mark in 1..=3u8 {
        let own: Vec<_> = shared
            .iter()
            .filter(|_| rng.below(3) != 0)
            .cloned()
            .collect();
        let tree = Tree::balanced(&own, mark);
        let (more, root) = layout.place_among(&tree, Placement::Scattered(0), &pages, &mut rng);
        for (page, planted) in more {
            cells.entry(page).or_default().extend(planted);
        }
        trees.push((tree, root));
    }
    trees.push((Tree::default(), None));
    disk.plant(1, &cells);
    for frames in [1, 5, 50] {
        check(&trees, &disk, frames, 21, 4_000);
    }
}

#[test]
fn a_tree_with_no_node_has_no_position() {
    let disk = Disk::new();
    let pool = pool_of(1);
    let guard = Guard;
    let reader = NodeReader::new(disk.loader(&pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, None);
    assert_eq!(cursor.root(), None);
    assert!(!cursor.valid());
    assert_eq!(
        (cursor.key(), cursor.handle(), cursor.depth()),
        (None, None, None)
    );

    for step in MOVES {
        assert_eq!(go(&mut cursor, &reader, &mut buffers, step), None);
    }
    // The four seeks looked at the root, which is not there, and no page.
    assert_eq!(
        cursor.stats(),
        CursorStats {
            descents: 4,
            refinds: 0,
            lookups: 0
        }
    );
    assert_eq!(pool.stats().misses, 0);
}

/// Stepping past an end leaves the cursor not positioned, and a step from
/// there stays there, in either direction.
#[test]
fn a_step_past_an_end_leaves_no_position() {
    let store = planted(Tree::balanced(&numbered(3), 1), Placement::Own, 30);
    let pool = pool_of(2);
    let guard = Guard;
    let reader = NodeReader::new(store.disk.loader(&pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, store.root);
    let key = numbered_key;

    let last = go(&mut cursor, &reader, &mut buffers, LAST);
    assert_eq!(last.expect("a node").0, key(2));
    assert_eq!(go(&mut cursor, &reader, &mut buffers, NEXT), None);
    assert_eq!(go(&mut cursor, &reader, &mut buffers, PREV), None);
    assert_eq!(go(&mut cursor, &reader, &mut buffers, NEXT), None);

    let first = go(&mut cursor, &reader, &mut buffers, FIRST);
    assert_eq!(first.expect("a node").0, key(0));
    assert_eq!(go(&mut cursor, &reader, &mut buffers, PREV), None);
    assert_eq!(go(&mut cursor, &reader, &mut buffers, NODE), None);

    // A seek before the first key, backward, and after the last, forward.
    let moved = cursor.seek_for_prev(&reader, &mut buffers, &[]);
    assert_eq!(arrived(&cursor, moved), None);
    let moved = cursor.seek(&reader, &mut buffers, &[0xFF; 5]);
    assert_eq!(arrived(&cursor, moved), None);
}
