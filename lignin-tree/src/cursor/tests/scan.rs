//! The acceptance test of TREE-06: a scan of n elements pins O(height)
//! pages at a time and never descends from the root per step.

use super::*;

/// What one scan of a whole tree asked of its pool.
#[derive(Debug)]
struct Scanned {
    /// The pages asked for, by step: the seek first.
    asked: Vec<u64>,
    /// The most nodes that were above the cursor.
    deepest: usize,
}

/// Scans the whole tree with a cursor of its own, forward or with `back`
/// backward, and checks every record. No node is kept from one step to
/// the next, so every pin in the pool is the cursor's.
fn scan(store: &Store, pool: &BufferPool, back: bool) -> Scanned {
    let guard = Guard;
    let reader = NodeReader::new(store.disk.loader(pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, store.root);
    let mut expected: Vec<_> = store.tree.records().into_iter().collect();
    if back {
        expected.reverse();
    }

    let mut scanned = Scanned {
        asked: Vec::new(),
        deepest: 0,
    };
    let mut found = Vec::new();
    let (start, on) = if back { (LAST, PREV) } else { (FIRST, NEXT) };
    loop {
        let (before, gets) = (cursor.stats().lookups, pool.stats());
        let step = if found.is_empty() { start } else { on };
        let arrived = go(&mut cursor, &reader, &mut buffers, step);
        // What the cursor counts is what the pool was asked.
        let asked = cursor.stats().lookups - before;
        let after = pool.stats();
        assert_eq!(
            (after.hits + after.misses) - (gets.hits + gets.misses),
            asked
        );
        // No page was pinned when another was asked for: a miss with
        // every frame pinned takes a frame beside the fixed ones.
        assert_eq!((after.overflows, pool.overflow_frames()), (0, 0));
        scanned.asked.push(asked);
        let Some(record) = arrived else {
            break;
        };
        scanned.deepest = scanned.deepest.max(cursor.depth().expect("positioned"));
        found.push(record);
    }
    assert_eq!(found, expected);
    assert!(!cursor.valid());
    // One walk from the root: the seek.
    assert_eq!(
        cursor.stats(),
        CursorStats {
            descents: 1,
            refinds: 0,
            lookups: scanned.asked.iter().sum(),
        }
    );
    scanned
}

/// Every node in a page of its own is the layout that costs a cursor
/// most: every link leaves its page. The pool has one frame, so a cursor
/// that held two pages at any moment would make it take a second one.
#[test]
fn a_scan_holds_one_page_at_a_time_and_walks_from_the_root_once() {
    let nodes = 3_000;
    let store = planted(
        Tree::balanced(&keys(&mut Rng(1), nodes), 1),
        Placement::Own,
        1,
    );
    let depths = store.tree.depths();
    let height = depths.iter().max().expect("a node") + 1;
    assert!(height >= 12, "a tree of {nodes} nodes is {height} high");
    assert_eq!(store.pages.len(), nodes);
    // What a walk from the root for every step would ask.
    let from_the_root: usize = depths.iter().map(|depth| depth + 1).sum();

    for back in [false, true] {
        let pool = pool_of(1);
        let scanned = scan(&store, &pool, back);
        assert_eq!(scanned.deepest, height - 1);
        let (seek, steps) = scanned.asked.split_first().expect("the seek");
        let asked: u64 = steps.iter().sum();
        // The seek walks one way down. A step asks for the page it stands
        // on, for each page it goes down into, and for the page it comes
        // up to: over the scan each link is gone down once and each node
        // come up to once.
        assert!(*seek as usize <= height + 1, "the seek asked {seek} pages");
        assert!(
            asked as usize <= 3 * nodes,
            "{asked} pages for {nodes} steps"
        );
        assert!(steps.iter().all(|&asked| asked as usize <= height + 1));
        println!(
            "{} scan, {nodes} nodes in {nodes} pages, height {height}, a pool of {} frame: \
             the seek asked {seek} pages and the steps {asked} ({:.2} a step, at most {}); \
             a walk from the root for each step would ask {from_the_root} ({:.2} a step); \
             overflow frames {}",
            if back { "backward" } else { "forward" },
            pool.frames(),
            asked as f64 / nodes as f64,
            steps.iter().max().expect("a step"),
            from_the_root as f64 / nodes as f64,
            pool.stats().overflows,
        );
    }
}

/// Fragments as the layout rules make them: a node is beside its parent
/// while the page has room. Consecutive keys are then mostly in one page,
/// and a step mostly asks for that page alone.
#[test]
fn a_scan_of_clustered_pages_asks_about_one_page_a_step() {
    let nodes = 6_000;
    let store = planted(
        Tree::balanced(&numbered(nodes), 1),
        Placement::Beside(PAGE_NODES),
        2,
    );
    let pages = store.pages.len();
    assert!(pages * PAGE_NODES < nodes * 2, "{pages} pages");
    for back in [false, true] {
        let pool = pool_of(1);
        let scanned = scan(&store, &pool, back);
        let asked: u64 = scanned.asked[1..].iter().sum();
        // One for the page of each step, and two for each link that
        // leaves its page: down it once, and up past it once.
        assert!(asked as usize <= nodes + 2 * pages);
        println!(
            "{} scan, {nodes} nodes in {pages} pages: the steps asked {asked} pages \
             ({:.3} a step)",
            if back { "backward" } else { "forward" },
            asked as f64 / nodes as f64,
        );
    }
}

/// A tree that was never balanced, with its nodes anywhere: the cursor
/// takes any shape and any placement (`tree-layout.md` F5).
#[test]
fn a_scan_takes_any_shape_and_any_placement() {
    let mut rng = Rng(3);
    let mut shuffled = keys(&mut rng, 1_500);
    rng.shuffle(&mut shuffled);
    let store = planted(Tree::grown(&shuffled, 1), Placement::Scattered(80), 3);
    for (pool, back) in [
        (pool_of(1), false),
        (pool_of(1), true),
        (pool_of(7), false),
        (roomy(80), true),
    ] {
        let scanned = scan(&store, &pool, back);
        assert!(scanned.deepest >= 12);
    }
}

/// The caller's nodes are the caller's pins: what a step returned stays
/// readable, with the page it is in, while the cursor goes on.
#[test]
fn a_node_that_a_step_returned_stays_readable_while_the_cursor_goes_on() {
    let store = planted(Tree::balanced(&numbered(200), 1), Placement::Own, 4);
    let pool = pool_of(2);
    let guard = Guard;
    let reader = NodeReader::new(store.disk.loader(&pool), &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, store.root);

    let first = cursor
        .seek_to_first(&reader, &mut buffers)
        .expect("readable")
        .expect("a node");
    let mut kept = vec![first];
    for _ in 0..40 {
        let node = cursor.next(&reader, &mut buffers).expect("readable");
        kept.push(node.expect("a node"));
    }
    let found: Vec<_> = kept.iter().map(|node| record(node).0).collect();
    assert_eq!(found, numbered(41));
}
