//! The shape of the tree: how full its pages are, how it grows and
//! shrinks, and what decides its bytes.

use super::*;

fn fill(pages: &Mem) -> f64 {
    let leaves: Vec<usize> = pages
        .pages
        .values()
        .filter(|image| image[0] == 1)
        .map(Vec::len)
        .collect();
    leaves.iter().sum::<usize>() as f64 / (leaves.len() * pages.config.capacity()) as f64
}

#[test]
fn keys_that_arrive_in_order_leave_full_pages() {
    let config = small();
    // The keys of append-only trees: positions that count up, under the
    // prefixes of several subtrees, which take turns.
    let (mut pages, tree) = Mem::tree(config);
    for n in 0..4_000u64 {
        for subtree in [3u8, 5, 9] {
            let key = [&[subtree; 32][..], &n.to_be_bytes()].concat();
            tree.put(&mut pages, &key, &[subtree; 24]).expect("a put");
        }
    }
    assert_eq!(records(&pages, &tree).len(), 12_000);
    assert!(
        fill(&pages) > 0.88,
        "leaves are {:.0}% full",
        100.0 * fill(&pages)
    );

    // The same keys in no order: pages are split in the middle.
    let (mut shuffled, tree) = Mem::tree(config);
    let mut rng = SimRng::new(7);
    let mut keys: Vec<Vec<u8>> = (0..12_000u64)
        .map(|n| [&[3 + 2 * (n % 3) as u8; 32][..], &(n / 3).to_be_bytes()].concat())
        .collect();
    let subtree = |key: &[u8]| if key[0] == 7 { 9 } else { key[0] };
    while !keys.is_empty() {
        let key = keys.swap_remove(rng.below(keys.len() as u64) as usize);
        let key = [&[subtree(&key); 32][..], &key[32..]].concat();
        tree.put(&mut shuffled, &key, &[key[0]; 24]).expect("a put");
    }
    assert!(records(&shuffled, &tree) == records(&pages, &FlatTree::open(tree.root(), config)));
    assert!(
        (0.6..0.8).contains(&fill(&shuffled)),
        "{:.2}",
        fill(&shuffled)
    );

    // What the keys of a page share is stored once: an entry of 64 bytes
    // of key and value takes less than that.
    let entry = pages.pages.values().map(Vec::len).sum::<usize>() / 12_000;
    assert!(entry < 5 + 8 + 24 + 4, "{entry} bytes for an entry");
}

#[test]
fn the_tree_grows_and_shrinks_by_levels_and_keeps_its_root() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let root = tree.root();
    // Neighbours that differ late, in groups that differ early: separators
    // are long, and a branch that holds those of several groups holds few.
    let key = |n: u32| [&[b'g', (n % 100) as u8][..], &[b'x'; 900], &n.to_be_bytes()].concat();
    let mut levels = vec![0];
    for n in 0..8_000 {
        tree.put(&mut pages, &key(n), &[n as u8; 200])
            .expect("a put");
        let level = pages.image(root).level();
        if levels.last() != Some(&level) {
            levels.push(level);
            assert_eq!(records(&pages, &tree).len(), n as usize + 1);
        }
    }
    assert!(levels.len() >= 4 && levels.is_sorted(), "{levels:?}");
    assert_eq!(records(&pages, &tree).len(), 8_000);
    pages.commit();

    for n in 0..8_000 {
        assert!(tree.delete(&mut pages, &key(n)).expect("a delete"));
        let level = pages.image(root).level();
        if levels.last() != Some(&level) {
            assert!(level < *levels.last().expect("a level"));
            levels.push(level);
            assert_eq!(records(&pages, &tree).len(), 7_999 - n as usize);
        }
    }
    assert_eq!(levels.last(), Some(&0));
    assert_eq!(pages.pages.keys().collect::<Vec<_>>(), [&root]);
    assert_eq!(pages.image(root).as_bytes().len(), 12);
}

/// The prefix of a page is what its keys share. A key that shares less
/// with them makes the prefix shorter, and the page is cut where the keys
/// stop sharing, not into as many pages as the longer keys would fill.
#[test]
fn a_key_that_shares_less_with_its_page_is_cut_off_from_it() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let shared = vec![b'x'; 1_000];
    let key = |n: u16| [&shared[..], &n.to_be_bytes()].concat();
    for n in 0..6_000 {
        tree.put(&mut pages, &key(n), b"").expect("a put");
    }
    // A leaf holds a thousand of these keys, as two bytes each.
    assert!(pages.pages.len() <= 8, "{} pages", pages.pages.len());
    let before = pages.pages.len();

    // Keys that do not begin as the others do: before all of them, behind
    // all of them, and one that begins as they do and ends early.
    tree.put(&mut pages, b"a", b"first").expect("a put");
    tree.put(&mut pages, b"z", b"last").expect("a put");
    tree.put(&mut pages, &shared[..500], b"short")
        .expect("a put");
    tree.put(
        &mut pages,
        &[&shared[..], b"\x0b\xb8-"].concat(),
        b"between",
    )
    .expect("a put");
    let found = records(&pages, &tree);
    assert_eq!(found.len(), 6_004);
    assert_eq!(found[0], (b"a".to_vec(), b"first".to_vec()));
    assert_eq!(found[1], (shared[..500].to_vec(), b"short".to_vec()));
    assert_eq!(found[6_003], (b"z".to_vec(), b"last".to_vec()));
    assert!(
        pages.pages.len() <= before + 4,
        "{} pages",
        pages.pages.len()
    );

    // The page of a key that leaves shares more again.
    assert!(tree.delete(&mut pages, b"a").expect("a delete"));
    assert!(tree.delete(&mut pages, &shared[..500]).expect("a delete"));
    assert_eq!(records(&pages, &tree).len(), 6_002);
    tree.delete_range(&mut pages, b"", None)
        .expect("a range delete");
    assert_eq!(pages.pages.len(), 1);
}

/// `format.md` B9: the bytes are a function of what was done.
#[test]
fn equal_histories_give_equal_pages() {
    let config = small();
    let run = || {
        let mut rng = SimRng::new(0xb9);
        let (mut pages, tree) = Mem::tree(config);
        let mut model = super::model::Model::new();
        for _ in 0..3_000 {
            let key = super::model::near(&mut rng, &model, config);
            match rng.below(4) {
                0 => {
                    tree.delete(&mut pages, &key).expect("a delete");
                    model.remove(&key);
                }
                _ => {
                    let value = super::model::value(&mut rng, config);
                    tree.put(&mut pages, &key, &value).expect("a put");
                    model.insert(key, value);
                }
            }
        }
        pages.pages
    };
    assert!(run() == run());
}

#[test]
fn the_sizes_come_from_the_page_size() {
    for (size, capacity) in [(8_192, 8_192), (32_768, 32_768), (65_536, 65_535)] {
        let config = FlatConfig::new(PageSize::new(size).expect("a page size"));
        assert_eq!(config.capacity(), capacity);
        assert_eq!(
            (config.key_limit(), config.max_inline()),
            (capacity / 8, capacity / 8)
        );
        // Two entries of the longest key and value fit one page, and so do
        // two separators: a page that is too long can always be cut.
        let most = config.with_max_inline(capacity / 4).expect("a limit");
        assert!(12 + 2 * (5 + most.key_limit() + 1 + most.max_inline()) <= capacity);
        assert!(16 + 2 * (6 + most.key_limit() + 1) <= capacity);
        for limit in [0, 15, capacity / 4 + 1] {
            let refused = config.with_max_inline(limit);
            assert!(matches!(refused, Err(FlatError::InlineLimit { .. })));
        }
        assert_eq!(
            config.with_max_inline(16).expect("a limit").max_inline(),
            16
        );
    }
}
