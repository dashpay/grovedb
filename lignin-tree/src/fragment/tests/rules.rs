//! The rules of `docs/lignindb/tree-layout.md`, sections 5 to 8, one at a
//! time.

use super::{grove::Grove, *};

fn params(largest: u32, small: u32, seed: u32, merge: u32) -> LayoutParams {
    LayoutParams {
        largest_fragment_percent: largest,
        small_fragment_divisor: small,
        seed_limit_percent: seed,
        merge_below_percent: merge,
    }
}

/// Pages that are only ever halved: a fragment may fill its page, one that
/// leaves starts a fresh page, and nothing is merged.
const HALVING: LayoutParams = LayoutParams {
    largest_fragment_percent: 100,
    small_fragment_divisor: 8,
    seed_limit_percent: 0,
    merge_below_percent: 0,
};

fn grove(params: LayoutParams) -> Grove {
    grove_of(SIZE, params)
}

fn grove_of(size: PageSize, params: LayoutParams) -> Grove {
    let layout = Layout::new(size, params).expect("a layout");
    let mut grove = Grove::new(layout, 5);
    grove.plain_names = true;
    grove
}

/// Keys in an order that has nothing to do with their value.
fn key(index: u32) -> [u8; 4] {
    index.wrapping_mul(2_654_435_761).to_be_bytes()
}

fn value(len: usize, fill: u8) -> Vec<u8> {
    vec![fill; len]
}

/// A grove with `count` nodes in its root Merk.
fn grown(params: LayoutParams, count: u32, value_len: usize) -> Grove {
    grown_of(SIZE, params, count, value_len)
}

fn grown_of(size: PageSize, params: LayoutParams, count: u32, value_len: usize) -> Grove {
    let mut grove = grove_of(size, params);
    for index in 0..count {
        grove.put(0, &key(index), &value(value_len, index as u8));
    }
    grove.check();
    grove
}

#[test]
fn a_new_node_goes_to_its_parent_and_a_root_to_its_host() {
    let mut grove = grove(LayoutParams::PROPOSED);
    let report = grove.put(0, b"m", b"first");
    assert_eq!(report.created, [page(1)]);
    assert_eq!(report.roots, [Some(address(1, 1))]);
    assert_eq!(report.nodes[0].address(), address(1, 1));
    for key in [b"c", b"x", b"a"] {
        let report = grove.put(0, key, b"more");
        assert!(report.created.is_empty() && report.carried.is_empty());
        assert!(report.left.is_empty() && report.freed.is_empty());
    }
    assert_eq!(grove.node(0, b"a").at, Some(address(1, 4)));

    let hosted = grove.host(0, b"c", false);
    let report = grove.put(hosted, b"inside", b"hosted");
    assert!(report.created.is_empty());
    assert_eq!(grove.page_of(hosted, b"inside"), page(1));
    let view = grove.mem.view(page(1));
    let host = view.node(grove.node(0, b"c").at.expect("placed").slot());
    let roots: Vec<_> = host.expect("the host").hosted_roots().collect();
    assert_eq!(roots, [grove.node(hosted, b"inside").at]);

    // A slot that a node left is the next one taken (S4).
    grove.delete(0, b"a");
    grove.put(0, b"b", b"again");
    assert_eq!(grove.node(0, b"b").at, Some(address(1, 4)));
    grove.check();
    assert_eq!(grove.mem.cells(), 5);
}

#[test]
fn a_rotation_changes_links_and_carries_no_cell() {
    let mut grove = grove(LayoutParams::PROPOSED);
    grove.put(0, &[1], b"one");
    grove.put(0, &[2], b"two");
    let from = grove.mem.shadow.records.len();
    let report = grove.put(0, &[3], b"three");
    assert_eq!(grove.merks[0].root, grove.find(0, &[2]), "the tree rotated");
    assert!(report.carried.is_empty() && report.left.is_empty());
    assert_eq!(report.roots, [grove.node(0, &[2]).at]);
    assert_eq!(grove.mem.count(from, Rec::Insert), 1);
    assert_eq!(grove.mem.count(from, Rec::Remove), 0);
    assert!(grove.mem.count(from, Rec::Link) >= 2);
    grove.check();

    // Across pages too: in a grove of many pages, a write puts in the cells
    // of its new nodes and of the cells a rule carried, and no other. The
    // grove checks that count on every write.
    let grove = grown(LayoutParams::PROPOSED, 1200, 90);
    assert!(grove.mem.frames.len() > 20);
    assert_eq!(grove.log.carried.len() + grove.log.released.len(), 0);
}

#[test]
fn a_node_that_takes_the_place_of_a_deleted_node_takes_its_page() {
    let mut grove = grown(LayoutParams::PROPOSED, 500, 150);
    let mut taken = 0;
    for _ in 0..60 {
        let successor = |node: &grove::Node| {
            let mut at = node.right?;
            while let Some(left) = grove.nodes[at].left {
                at = left;
            }
            node.left.map(|_| at)
        };
        let page_of = |node: usize| grove.nodes[node].at.map(|at| at.page());
        let pair = grove.nodes.iter().enumerate().find_map(|(id, node)| {
            let successor = successor(node).filter(|_| node.alive)?;
            (page_of(successor) != page_of(id)).then_some((id, successor))
        });
        let Some((gone, successor)) = pair else { break };
        let key = grove.nodes[gone].key.clone();
        let (was, from) = (grove.nodes[gone].at, grove.nodes[successor].at);
        let report = grove.delete(0, &key);
        let carried = report.carried.iter().find(|carried| Some(carried.from) == from);
        let carried = carried.expect("the node that took the place was carried");
        if carried.cause == Cause::Delete {
            assert_eq!(Some(carried.to.page()), was.map(|was| was.page()));
            taken += 1;
        }
    }
    grove.check();
    assert!(taken > 20, "{taken} nodes took the page of the node they replaced");
    assert!(grove.fragments.counters().carries[Cause::Delete].cells >= taken);
}

#[test]
fn a_full_page_sends_a_part_of_its_fragment_to_a_fresh_page() {
    let mut grove = grove(HALVING);
    let mut index = 0;
    let report = loop {
        let report = grove.put(0, &key(index), &value(100, 1));
        index += 1;
        if index > 1 && !report.created.is_empty() {
            break report;
        }
    };
    assert_eq!(report.created, [page(2)]);
    assert_eq!(report.left, [page(1)]);
    assert!(!report.carried.is_empty());
    for carried in &report.carried {
        assert_eq!((carried.cause, carried.to.page()), (Cause::Split, page(2)));
        assert_eq!(carried.from.page(), page(1));
        assert_eq!(carried.hosted_by, None);
    }
    assert_eq!(grove.root_at.map(|at| at.page()), Some(page(1)));
    let capacity = grove.fragments.layout().capacity();
    for id in [page(1), page(2)] {
        let used = grove.mem.used(id);
        assert!(used > capacity / 4 && used < capacity * 3 / 4, "{id} holds {used}");
    }
    grove.check();
}

#[test]
fn a_fragment_that_leaves_goes_to_a_page_beside_it_that_has_room() {
    // Pages of 32 KiB, so that a fragment is some sixty cells and not a
    // dozen.
    let halved = grown_of(PageSize::DEFAULT, params(50, 16, 0, 0), 8000, 60);
    let seeded = grown_of(PageSize::DEFAULT, LayoutParams::PROPOSED, 8000, 60);
    println!(
        "seed limit 0: {} pages, fill {:.1}%; seed limit 75: {} pages, fill {:.1}%",
        halved.mem.frames.len(),
        halved.fill() * 100.0,
        seeded.mem.frames.len(),
        seeded.fill() * 100.0
    );
    assert!(seeded.mem.frames.len() < halved.mem.frames.len());
    assert!(seeded.fill() > halved.fill() + 0.02);
}

/// The pages `grove` read or changed in its last write that the write
/// neither changed nor holds a node in.
fn read_for_nothing(grove: &Grove, from: usize, report: &Report) -> usize {
    let changed = grove.mem.shadow.records[from..].iter().map(|(page, _)| *page);
    let mut held: Vec<PageId> = changed.chain(report.freed.iter().copied()).collect();
    held.extend(report.nodes.iter().map(|node| node.page()));
    let mut read = grove.mem.touched.clone();
    read.sort_unstable();
    read.dedup();
    read.iter().filter(|page| !held.contains(page)).count()
}

#[test]
fn a_page_is_not_read_to_choose_it() {
    // With no hint, a write reads the pages it holds a node in and the
    // pages it changes, and no other.
    let mut blind = grove_of(PageSize::DEFAULT, LayoutParams::PROPOSED);
    let mut hinted = grove_of(PageSize::DEFAULT, LayoutParams::PROPOSED);
    let mut idle = 0;
    for index in 0..8000 {
        blind.fragments.hints_mut().clear();
        blind.mem.touched.clear();
        let from = blind.mem.shadow.records.len();
        let report = blind.put(0, &key(index), &value(60, 2));
        assert_eq!(read_for_nothing(&blind, from, &report), 0);

        hinted.mem.touched.clear();
        let from = hinted.mem.shadow.records.len();
        let report = hinted.put(0, &key(index), &value(60, 2));
        idle += read_for_nothing(&hinted, from, &report);
    }
    blind.check();
    hinted.check();
    // With hints a page beside is chosen and read, and nearly always used.
    // A rule that runs later can take the fragment on to another page.
    println!(
        "without hints {} pages, with hints {}; {idle} reads of a page that was not used",
        blind.mem.frames.len(),
        hinted.mem.frames.len()
    );
    assert!(hinted.mem.frames.len() < blind.mem.frames.len());
    assert!(idle <= 25, "{idle} pages were read and not used");
}

#[test]
fn a_wrong_hint_costs_a_read_and_nothing_else() {
    let mut grove = grove(LayoutParams::PROPOSED);
    for index in 0..1500 {
        let pages: Vec<PageId> = grove.mem.frames.keys().copied().collect();
        for (place, id) in pages.into_iter().enumerate() {
            match (index + place as u32) % 3 {
                0 => grove.fragments.hints_mut().note(id, 0),
                1 => grove.fragments.hints_mut().note(id, 60_000),
                _ => grove.fragments.hints_mut().forget(id),
            }
        }
        // The write corrects the hints of the pages it reads, so the grove
        // finds them exact where it looks.
        let report = grove.put(0, &key(index), &value(100, 3));
        for id in grove.mem.frames.keys() {
            if !report.nodes.iter().any(|node| node.page() == *id) {
                grove.fragments.hints_mut().forget(*id);
            }
        }
    }
    grove.check();
    assert!(grove.mem.frames.len() > 20);
}

#[test]
fn a_fragment_above_the_largest_size_is_split_where_it_is() {
    let capped = grown(LayoutParams::PROPOSED, 2000, 100);
    let free = grown(params(100, 16, 75, 35), 2000, 100);
    let layout = capped.fragments.layout();
    let bound = layout.largest_fragment() + layout.small_fragment() + 400;
    println!(
        "the largest fragment: {} bytes with a limit of {}, {} without one",
        capped.largest_fragment(),
        layout.largest_fragment(),
        free.largest_fragment()
    );
    assert!(capped.largest_fragment() <= bound);
    assert!(free.largest_fragment() > bound);
}

#[test]
fn a_small_hosted_merk_goes_with_its_host() {
    let mut grove = grove(LayoutParams::PROPOSED);
    let mut together = 0;
    let mut seen = |grove: &Grove, report: Report| {
        for carried in &report.carried {
            let Some((host, position)) = carried.hosted_by else {
                continue;
            };
            let view = grove.mem.view(host.page());
            let cell = view.node(host.slot()).expect("the host is where the report says");
            let root = cell.hosted_roots().nth(usize::from(position)).flatten();
            assert_eq!(root, Some(carried.to.address()));
            let with_host = report.carried.iter().any(|other| other.to.address() == host);
            together += usize::from(with_host && host.page() == carried.to.page());
        }
    };
    for index in 0..900 {
        let key = key(index);
        let report = grove.put(0, &key, &value(40, 4));
        seen(&grove, report);
        if index % 2 == 0 {
            let hosted = grove.host(0, &key, index % 4 == 0);
            let report = grove.put(hosted, b"only", b"small");
            seen(&grove, report);
        }
    }
    grove.check();
    println!("{together} hosted roots went with their host");
    assert!(together > 0);
}

#[test]
fn deleting_every_node_frees_every_page() {
    let mut grove = grown(LayoutParams::PROPOSED, 900, 120);
    let pages = grove.mem.frames.len();
    assert!(pages > 15);
    for index in (0..900).rev() {
        grove.delete(0, &key(index));
        if index % 100 == 0 {
            grove.check();
        }
    }
    grove.check();
    assert!(grove.mem.frames.is_empty() && grove.root_at.is_none());
    let counters = grove.fragments.counters();
    assert_eq!((counters.created, counters.removed), (900, 900));
    assert_eq!(counters.pages_created, counters.pages_freed);
    assert_eq!(grove.mem.freed.len() as u64, counters.pages_freed);
    assert!(counters.carries[Cause::Merge].cells > 0);
    assert!(grove.fragments.hints().is_empty());
}

#[test]
fn a_page_that_a_delete_leaves_nearly_empty_gives_its_fragments_away() {
    // No fragment of these groves is small, so only a page below the merge
    // threshold gives one away.
    let run = |merge_below: u32| {
        let mut grove = grown(params(50, 64, 75, merge_below), 1500, 100);
        for index in 0..1500 {
            if index % 10 < 7 {
                grove.delete(0, &key(index));
            }
        }
        grove.check();
        grove
    };
    let (merged, kept) = (run(35), run(0));
    println!(
        "merge threshold 35: {} pages, fill {:.1}%, {} cells carried; none: {} pages, fill {:.1}%",
        merged.mem.frames.len(),
        merged.fill() * 100.0,
        merged.fragments.counters().carries[Cause::Merge].cells,
        kept.mem.frames.len(),
        kept.fill() * 100.0
    );
    assert_eq!(kept.fragments.counters().carries[Cause::Merge].cells, 0);
    assert!(merged.fragments.counters().carries[Cause::Merge].cells > 0);
    assert!(merged.mem.frames.len() < kept.mem.frames.len());
}

/// A grove whose first page holds its three root nodes and, for the rest,
/// what a cleared Merk left there.
fn crowded() -> Grove {
    let mut grove = grove(HALVING);
    grove.inline_max = 5000;
    for key in [b"m", b"c", b"x"] {
        grove.put(0, key, b"root");
    }
    let hosted = grove.host(0, b"c", false);
    let capacity = grove.fragments.layout().capacity();
    let mut index = 0;
    while grove.mem.used(page(1)) < capacity * 7 / 10 {
        grove.put(hosted, &key(index), &value(100, 5));
        index += 1;
    }
    let report = grove.clear(hosted);
    assert_eq!(report.orphaned.len(), 1);
    assert_eq!(grove.mem.frames.len(), 1);
    assert_eq!(grove.mem.cells(), 3 + grove.ghosts);
    grove.check();
    grove
}

#[test]
fn a_root_that_a_root_pointer_names_is_carried_with_its_fragment() {
    let mut grove = crowded();
    let ghosts = grove.ghosts;
    let mut len = 400;
    while grove.mem.frames.len() == 1 {
        grove.put(0, b"x", &value(len, 6));
        len += 400;
    }
    // The three nodes left together, and the cells nothing points at stay.
    assert_eq!(grove.root_at.map(|at| at.page()), Some(page(2)));
    assert_eq!(grove.mem.view(page(1)).len(), ghosts);
    assert_eq!(grove.mem.view(page(2)).len(), 3);
    grove.check();
}

#[test]
fn a_node_that_nothing_in_the_write_points_at_stays_in_its_page() {
    let mut grove = crowded();
    let root = grove.node(0, b"m").at;
    let mut len = 1500;
    let report = loop {
        let report = grove.alone(0, b"m", &value(len, 7)).expect("there is room");
        len += 50;
        if !report.carried.is_empty() {
            break report;
        }
    };
    // What hung below it left, and the links in its cell were rewritten.
    assert_eq!(grove.node(0, b"m").at, root);
    assert_eq!(report.nodes[0].address(), root.expect("placed"));
    assert!(report.roots.is_empty());
    // One of the two is small enough to have come back (M1).
    assert!((1..=2).contains(&report.carried.len()));
    assert!(report.carried.iter().all(|carried| carried.node.is_none()));
    assert!(report.carried.iter().all(|carried| carried.to.page() == page(2)));
    grove.check();

    // Now its page holds nothing else the write could move.
    let error = loop {
        let before = (grove.mem.frames.clone(), grove.mem.shadow.records.len());
        match grove.alone(0, b"m", &value(len, 7)) {
            Ok(_) => len += 50,
            Err(error) => {
                assert_eq!(grove.mem.frames, before.0, "a refused write changes no page");
                assert_eq!(grove.mem.shadow.records.len(), before.1);
                break error;
            }
        }
    };
    assert!(matches!(error, FragmentError::NoRoom { page: Some(id) } if id == page(1)));
    assert!(error.to_string().ends_with("is full of cells the write does not hold"));
    assert!(std::error::Error::source(&error).is_none());
    let unplaced = FragmentError::NoRoom { page: None };
    assert_eq!(unplaced.to_string(), "a new page is above its capacity");
    grove.check();
}

#[test]
fn the_parts_that_leave_a_full_page_come_closest_to_half_of_it() {
    use super::plan::choose_parts;
    // Every choice of up to twelve parts is compared.
    assert_eq!(choose_parts(&[100, 300, 250], 340), [0, 2]);
    assert_eq!(choose_parts(&[500], 10), [0]);
    // Of two choices that are equally close, the one with fewer bytes.
    assert_eq!(choose_parts(&[100, 300], 200), [0]);
    // Then the one whose last part comes first.
    assert_eq!(choose_parts(&[100, 100, 100], 100), [0]);
    assert_eq!(choose_parts(&[100, 200, 100, 200], 300), [0, 1]);
    // With more parts they are taken largest first, each only if it brings
    // the sum closer.
    let parts: Vec<usize> = (1..=14).map(|part| part * 10).collect();
    assert_eq!(choose_parts(&parts, 277), [0, 12, 13]);
    assert_eq!(choose_parts(&parts, 1), [13]);
}

#[test]
fn the_parameters_are_checked_against_each_other() {
    let layout = Layout::proposed(PageSize::DEFAULT);
    assert_eq!(layout.page_size(), PageSize::DEFAULT);
    assert_eq!(layout.params(), LayoutParams::default());
    assert_eq!(layout.capacity(), 32_746);
    assert_eq!(layout.small_fragment(), 2_048);
    assert_eq!(layout.largest_fragment(), 16_373);
    assert_eq!(layout.seed_limit(), 24_559);
    assert_eq!(layout.merge_below(), 11_461);
    assert_eq!(Layout::proposed(PageSize::MAX).capacity(), 65_513);

    let refused = |params| Layout::new(SIZE, params).expect_err("refused");
    let cases = [
        (params(50, 1, 75, 35), LayoutError::SmallFragment, "a small fragment is at most half a page"),
        (params(9, 64, 75, 35), LayoutError::LargestFragment, "the largest fragment is 10 to 100 percent of a page"),
        (params(101, 16, 75, 35), LayoutError::LargestFragment, "the largest fragment is 10 to 100 percent of a page"),
        (params(50, 4, 75, 35), LayoutError::SmallAgainstLargest, "three small fragments must fit in a largest one"),
        (params(50, 16, 101, 35), LayoutError::SeedLimit, "the seed limit is at most a full page"),
        (params(50, 16, 30, 35), LayoutError::MergeThreshold, "the merge threshold is at most the seed limit"),
    ];
    for (params, error, text) in cases {
        assert_eq!(refused(params), error);
        assert_eq!(error.to_string(), text);
    }

    let mut fragments = Fragments::new(layout);
    assert_eq!(fragments.layout(), &layout);
    fragments.set_layout(Layout::proposed(SIZE));
    assert_eq!(fragments.layout().page_size(), SIZE);
    assert_eq!(fragments.counters(), &Counters::default());
}

#[test]
fn a_hint_is_what_a_page_held_when_it_was_seen() {
    let grove = grown(LayoutParams::PROPOSED, 200, 100);
    let mut hints = Hints::new();
    assert!(hints.is_empty() && hints.used(page(1)).is_none());
    for id in grove.mem.frames.keys() {
        hints.see(&grove.mem.view(*id));
        assert_eq!(hints.used(*id), Some(grove.mem.used(*id)));
        assert_eq!(hints.used(*id), grove.fragments.hints().used(*id));
    }
    assert_eq!(hints.len(), grove.mem.frames.len());
    hints.note(page(1), 12);
    assert_eq!(hints.used(page(1)), Some(12));
    hints.forget(page(1));
    assert_eq!(hints.len(), grove.mem.frames.len() - 1);
    hints.clear();
    assert!(hints.is_empty());
}
