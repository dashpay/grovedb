//! The rules of `docs/lignindb/tree-layout.md`, sections 5 to 8, one at a
//! time.

use lignin_format::tree_page::{CellContent, Side, TreeCell, TreeLink};

use super::{
    errors::{link, node, root, HASH},
    grove::Grove,
    *,
};

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
    (Rng(u64::from(index)).next() as u32).to_be_bytes()
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
        let carried = report
            .carried
            .iter()
            .find(|carried| Some(carried.from) == from);
        let carried = carried.expect("the node that took the place was carried");
        if carried.cause == Cause::Delete {
            assert_eq!(Some(carried.to.page()), was.map(|was| was.page()));
            taken += 1;
        }
    }
    grove.check();
    assert!(
        taken > 20,
        "{taken} nodes took the page of the node they replaced"
    );
    assert!(grove.fragments.counters().carries[Cause::Delete].cells >= taken);
}

#[test]
fn a_node_that_replaces_the_last_node_of_a_page_stays_where_it_is() {
    let mut grove = grove(HALVING);
    grove.inline_max = 5000;
    for key in [b"m", b"c", b"x"] {
        grove.put(0, key, &value(3000, 8));
    }
    // The root is alone in its page, and what hung below it is in another.
    let below = grove.page_of(0, b"x");
    assert_eq!(grove.page_of(0, b"m"), page(1));
    assert_eq!((grove.page_of(0, b"c"), below), (page(2), page(2)));
    let report = grove.delete(0, b"m");
    assert!(report.carried.is_empty());
    assert_eq!(report.freed, [page(1)]);
    assert_eq!(grove.root_at, grove.node(0, b"x").at);
    assert_eq!(grove.page_of(0, b"x"), below);
    grove.check();
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
        assert!(
            used > capacity / 4 && used < capacity * 3 / 4,
            "{id} holds {used}"
        );
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
    let changed = grove.mem.shadow.records[from..]
        .iter()
        .map(|(page, _)| *page);
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
            let cell = view
                .node(host.slot())
                .expect("the host is where the report says");
            let root = cell.hosted_roots().nth(usize::from(position)).flatten();
            assert_eq!(root, Some(carried.to.address()));
            let with_host = report
                .carried
                .iter()
                .any(|other| other.to.address() == host);
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
    grove.clear(hosted);
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
    assert!(report
        .carried
        .iter()
        .all(|carried| carried.to.page() == page(2)));
    grove.check();

    // Now its page holds nothing else the write could move.
    let error = loop {
        let before = (grove.mem.frames.clone(), grove.mem.shadow.records.len());
        match grove.alone(0, b"m", &value(len, 7)) {
            Ok(_) => len += 50,
            Err(error) => {
                assert_eq!(
                    grove.mem.frames, before.0,
                    "a refused write changes no page"
                );
                assert_eq!(grove.mem.shadow.records.len(), before.1);
                break error;
            }
        }
    };
    assert!(matches!(error, FragmentError::NoRoom { page: Some(id) } if id == page(1)));
    assert!(error
        .to_string()
        .ends_with("is full of cells the write does not hold"));
    assert!(std::error::Error::source(&error).is_none());
    let unplaced = FragmentError::NoRoom { page: None };
    assert_eq!(unplaced.to_string(), "a new page is above its capacity");
    grove.check();
}

#[test]
fn a_hosted_merk_that_leaves_a_host_the_write_does_not_write_is_still_pointed_at() {
    let mut grove = grove(HALVING);
    grove.inline_max = 5000;
    grove.put(0, b"m", &value(3200, 1));
    grove.put(0, b"c", &value(2500, 2));
    grove.put(0, b"x", b"small");
    let hosted = grove.host(0, b"c", false);
    for index in 0..7 {
        grove.put(hosted, &key(index), &value(150, 3));
    }
    assert_eq!(grove.mem.frames.len(), 1);
    let host = grove.node(0, b"c").at.expect("placed");
    let root = grove.merks[hosted].root.expect("the hosted Merk has nodes");
    let root_at = grove.nodes[root].at.expect("placed");

    // A node below `x` fills the page. The write holds `x` and the root
    // above it, and not `c`, whose Merk is what has to leave.
    let report = grove.put(0, b"y", &value(400, 4));
    assert_eq!(report.created, [page(2)]);
    assert_eq!(grove.node(0, b"c").at, Some(host));
    let moved = report
        .carried
        .iter()
        .find(|carried| carried.from == root_at);
    let moved = moved.expect("the root of the hosted Merk was carried");
    assert_eq!(moved.hosted_by, Some((host, 0)));
    assert_eq!((moved.node, moved.to.page()), (None, page(2)));
    assert!(report.nodes.iter().all(|node| node.address() != host));
    let view = grove.mem.view(host.page());
    let roots: Vec<_> = view
        .node(host.slot())
        .expect("the host")
        .hosted_roots()
        .collect();
    assert_eq!(roots, [Some(moved.to.address())]);
    grove.check();
}

#[test]
fn a_root_whose_host_is_not_in_the_write_stays_and_is_not_let_go_of() {
    let mut grove = grove(LayoutParams::PROPOSED);
    for key in [b"m", b"c", b"x"] {
        grove.put(0, key, b"root");
    }
    let hosted = grove.host(0, b"c", false);
    let host = grove.node(0, b"c").at.expect("placed");
    let pointer = |grove: &Grove| {
        let view = grove.mem.view(host.page());
        let cell = view.node(host.slot()).expect("the host stays");
        cell.hosted_roots().next().flatten()
    };

    // Three writes that stop at the root of the hosted Merk. The third
    // rotates it: the node that was the root hangs below another now.
    grove.put_stopped(hosted, &[1], b"one");
    let first = grove.put_stopped(hosted, &[2], b"two").roots[0];
    assert_eq!(first, grove.node(hosted, &[1]).at);
    let from = grove.mem.shadow.records.len();
    let report = grove.put_stopped(hosted, &[3], b"three");
    assert_eq!(grove.merks[hosted].root, grove.find(hosted, &[2]));
    assert_eq!(report.roots, [grove.node(hosted, &[2]).at]);
    assert_ne!(report.roots[0], first);
    assert!(report.carried.is_empty() && report.left.is_empty());
    assert_eq!(grove.mem.count(from, Rec::Remove), 0);
    // The host's cell is behind. The new root is where its directory
    // entry says, marked as the root of its fragment, with the old one
    // below it.
    assert_eq!(pointer(&grove), None);
    assert_eq!(grove.check_merk(hosted), 3);

    // The root is deleted, and the node that takes its place is one the
    // write does not write.
    grove.delete_stopped(hosted, &[3]);
    let report = grove.delete_stopped(hosted, &[2]);
    assert_eq!(report.roots, [grove.node(hosted, &[1]).at]);
    assert!(report.carried.is_empty());
    assert_eq!(grove.check_merk(hosted), 1);
    grove.put_stopped(hosted, &[4], b"four");
    assert_eq!(grove.check_merk(hosted), 2);

    // Finalize writes the host with the root the Merk has now. Nothing is
    // let go of, and every mark is right.
    let cells = grove.mem.cells();
    grove.finalize(hosted);
    assert_eq!(pointer(&grove), grove.node(hosted, &[1]).at);
    assert_eq!(grove.mem.cells(), cells);
    grove.check();

    // A Merk whose root is in the page of its host is cleared in a write
    // that holds neither. The root stays where it is, with nothing
    // pointing at it once the host is written.
    let second = grove.host(0, b"x", false);
    grove.put(second, &[9], b"nine");
    let root = grove.node(second, &[9]).at.expect("placed");
    let host = grove.node(0, b"x").at.expect("placed");
    assert_eq!(root.page(), host.page());
    assert!(!grove
        .mem
        .view(root.page())
        .node(root.slot())
        .expect("placed")
        .is_fragment_root());
    let cells = grove.mem.cells();
    let from = grove.mem.shadow.records.len();
    let report = grove.clear_stopped(second);
    assert_eq!(report.roots, [None]);
    assert!(report.left.is_empty() && grove.mem.cells() == cells);
    assert_eq!(grove.mem.count(from, Rec::Mark), 1);
    assert_eq!(grove.mem.shadow.records.len(), from + 1);
    grove.finalize(second);
    let view = grove.mem.view(root.page());
    assert!(view.node(root.slot()).expect("it stays").is_fragment_root());
    let roots: Vec<_> = view
        .node(host.slot())
        .expect("the host")
        .hosted_roots()
        .collect();
    assert_eq!(roots, [None]);
    grove.check();

    // The same with the old root left where it is, in the page of the
    // node that is the root now.
    grove.put_stopped(hosted, &[5], b"five");
    grove.put_stopped(hosted, &[6], b"six");
    assert_eq!(grove.merks[hosted].root, grove.find(hosted, &[4]));
    assert_eq!(grove.check_merk(hosted), 4);
    grove.finalize(hosted);
    grove.check();
}

/// What a write that stops at the root of a Merk did to the Merk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// It deleted the only node: the host points at a free slot.
    Deleted,
    /// It cleared the Merk: the host points at a cell for the reclaimer.
    Cleared,
    /// It gave the root another value: the host points at the root, whose
    /// mark is the one it had when the host was in its page.
    Kept,
}

#[test]
fn a_pointer_that_is_behind_is_not_followed() {
    // The root nodes and the cells of a cleared Merk in one page, and two
    // more Merks under `c`, with one node each, in the same page. The write
    // that names the roots again writes the host, or moves it to the key
    // `d` (D5).
    let cases = [
        (Stop::Deleted, false),
        (Stop::Cleared, false),
        (Stop::Kept, false),
        (Stop::Kept, true),
        (Stop::Deleted, true),
    ];
    for (stop, renamed) in cases {
        let mut grove = crowded();
        let beside = grove.host(0, b"c", false);
        grove.put(beside, &[7], b"seven");
        let hosted = grove.host(0, b"c", false);
        grove.put(hosted, &[1], b"one");
        let host = grove.node(0, b"c").at.expect("placed");
        let root = grove.node(hosted, &[1]).at.expect("placed");
        let other = grove.node(beside, &[7]).at.expect("placed");
        assert_eq!((root.page(), other.page()), (host.page(), host.page()));
        let ghosts = grove.ghosts;

        let report = match stop {
            Stop::Deleted => grove.delete_stopped(hosted, &[1]),
            Stop::Cleared => grove.clear_stopped(hosted),
            Stop::Kept => grove.put_stopped(hosted, &[1], b"uno"),
        };
        let lives = stop == Stop::Kept;
        assert_eq!(report.roots, [lives.then_some(root)]);
        let stored = |grove: &Grove, key: &[u8]| {
            let at = grove.node(0, key).at.expect("placed");
            let view = grove.mem.view(at.page());
            let cell = view.node(at.slot()).expect("the host");
            let mut roots = cell.hosted_roots();
            let last = roots.next_back().flatten();
            (roots.next_back().flatten(), last)
        };
        let marked = |grove: &Grove, at: CellAddress| {
            let view = grove.mem.view(at.page());
            view.node(at.slot()).map(|cell| cell.is_fragment_root())
        };
        let behind = (Some(other), Some(root));
        assert_eq!(stored(&grove, b"c"), behind, "the host is behind");

        // A write that holds the host and does not rewrite its element
        // keeps the pointers as they are.
        grove.touch(0, b"c");
        assert_eq!(grove.node(0, b"c").at, Some(host));
        assert_eq!(
            stored(&grove, b"c"),
            behind,
            "the pointers keep their bytes"
        );

        // Before the host is written with its roots, another write fills
        // the page and carries the host with the root of the grove. No
        // pointer of a host that is behind is followed: nothing is read
        // through one, and what the two name stays where it is.
        let mut len = 400;
        while grove.mem.frames.len() == 1 {
            grove.put(0, b"x", &value(len, 6));
            len += 400;
        }
        assert_eq!(grove.page_of(0, b"c"), page(2));
        assert_eq!(
            stored(&grove, b"c"),
            behind,
            "the pointers keep their bytes"
        );
        let stayed = grove.ghosts + 1 + usize::from(lives);
        assert_eq!(grove.mem.view(page(1)).len(), stayed);
        assert_eq!(grove.ghosts, ghosts + usize::from(stop == Stop::Cleared));
        let expected = match stop {
            Stop::Deleted => None,
            Stop::Cleared => Some(true),
            // The mark is behind as the pointer is: the host left the page.
            Stop::Kept => Some(false),
        };
        assert_eq!(marked(&grove, root), expected);
        assert_eq!(marked(&grove, other), Some(false));

        // The write that names the roots again puts the marks right, in a
        // page it changes for nothing else: in the host, or in the new
        // cell that takes its Merks over.
        let from = grove.mem.shadow.records.len();
        let said = grove.mem.said_behind;
        let key: &[u8] = match renamed {
            true => {
                grove.rename(0, b"c", b"d");
                assert!(grove.mem.said_behind > said, "the old host was behind");
                assert_eq!(grove.page_of(0, b"d"), page(2));
                b"d"
            }
            false => {
                grove.finalize(hosted);
                b"c"
            }
        };
        assert_eq!(stored(&grove, key), (Some(other), lives.then_some(root)));
        let records = &grove.mem.shadow.records[from..];
        let first: Vec<Rec> = records
            .iter()
            .filter(|(id, _)| *id == page(1))
            .map(|(_, rec)| *rec)
            .collect();
        assert_eq!(first, vec![Rec::Mark; 1 + usize::from(lives)]);
        assert_eq!(marked(&grove, other), Some(true));
        if lives {
            assert_eq!(marked(&grove, root), Some(true));
        }
        assert!(grove.behind().is_empty());
        grove.check();
    }
}

#[test]
fn a_pointer_that_is_behind_gives_no_node_the_page_of_a_deleted_one() {
    // The host `h` and the root `a` of its Merk in the first page, and `b`,
    // the right child of `a`, in the second. No fragment is small here.
    let mut grove = grove(params(100, 64, 75, 35));
    grove.inline_max = 5000;
    grove.plain_names = true;
    grove.put(0, b"h", &value(400, 0));
    let hosted = grove.host(0, b"h", false);
    grove.put(hosted, b"a", &value(3900, 1));
    grove.put(hosted, b"b", &value(3900, 2));
    let at = |grove: &Grove, key: &[u8]| grove.node(hosted, key).at.expect("placed");
    let (host, a, b) = (
        grove.node(0, b"h").at.expect("placed"),
        at(&grove, b"a"),
        at(&grove, b"b"),
    );
    assert_eq!(
        (host.page(), a.page(), b.page()),
        (page(1), page(1), page(2))
    );

    // A write that stops at the root puts `c` in, and `b` rises above `a`.
    // The host still names `a`.
    let report = grove.put_stopped(hosted, b"c", b"small");
    assert_eq!(report.roots, [Some(b)]);
    assert_eq!(grove.merks[hosted].root, grove.find(hosted, b"b"));
    assert_eq!((at(&grove, b"a"), at(&grove, b"b")), (a, b));
    let pointer = |grove: &Grove| {
        let view = grove.mem.view(host.page());
        let cell = view.node(host.slot()).expect("the host");
        cell.hosted_roots().next()
    };
    assert_eq!(pointer(&grove), Some(Some(a)));

    // The write that deletes `a` holds the host and names `b` as the root.
    // The host's pointer named `a`, but it was behind: `b` became the root
    // in the write before, and takes no page from `a` now (R2).
    let report = grove.delete(hosted, b"a");
    assert!(report.carried.is_empty(), "{:?}", report.carried);
    assert_eq!(at(&grove, b"b"), b);
    assert_eq!(pointer(&grove), Some(Some(b)));
    assert!(grove.behind().is_empty());
    let carries = grove.fragments.counters().carries;
    assert_eq!(carries[Cause::Delete].cells, 0);
    grove.check();
}

#[test]
fn a_node_that_becomes_the_root_below_a_host_that_is_behind_stays_where_it_is() {
    // The host `h`, the root `b` of its Merk and the left child `a` of `b`
    // in the first page, and `c`, the right child of `b`, in the second.
    for behind in [false, true] {
        let mut grove = grove(params(100, 64, 75, 35));
        grove.inline_max = 5000;
        grove.plain_names = true;
        grove.put(0, b"h", &value(400, 0));
        let hosted = grove.host(0, b"h", false);
        grove.put(hosted, b"b", &value(3900, 1));
        grove.put(hosted, b"c", &value(3900, 2));
        grove.put(hosted, b"a", b"small");
        let at = |grove: &Grove, key: &[u8]| grove.node(hosted, key).at.expect("placed");
        let (b, c) = (at(&grove, b"b"), at(&grove, b"c"));
        assert_eq!(
            (b.page(), c.page(), at(&grove, b"a").page()),
            (page(1), page(2), page(1))
        );
        if behind {
            let report = grove.put_stopped(hosted, b"b", &value(3900, 3));
            assert_eq!(report.roots, [Some(b)]);
        }

        // `b` is deleted, and `c` is the root. A host that is not behind
        // shows that `c` takes the place of `b`, and `c` takes its page
        // (R2). A host that is behind shows nothing: `c` stays where it is,
        // as it would in a write that stops below the host.
        let report = grove.delete(hosted, b"b");
        assert_eq!(grove.merks[hosted].root, grove.find(hosted, b"c"));
        let moves: Vec<_> = report.carried.iter().map(|cell| cell.cause).collect();
        match behind {
            false => {
                assert_eq!(moves, [Cause::Delete]);
                assert_eq!(at(&grove, b"c").page(), page(1));
            }
            true => {
                assert_eq!(moves, []);
                assert_eq!(at(&grove, b"c"), c);
            }
        }
        assert!(grove.behind().is_empty());
        grove.check();
    }
}

#[test]
fn a_merk_is_dropped_after_the_hosts_inside_it_that_are_behind_are_written() {
    // A Merk under `c` with three nodes, each the host of a Merk with one
    // node. A write stops below each of the three hosts: one deletes the
    // node, one clears the Merk and one puts a second node.
    let build = || {
        let mut grove = grove(LayoutParams::PROPOSED);
        for key in [b"m", b"c", b"x"] {
            grove.put(0, key, b"root");
        }
        let inner = grove.host(0, b"c", false);
        let mut nested = Vec::new();
        for index in 0..3 {
            grove.put(inner, &key(index), b"host");
            nested.push(grove.host(inner, &key(index), false));
            grove.put(nested[index as usize], &[1], b"one");
        }
        grove.delete_stopped(nested[0], &[1]);
        grove.clear_stopped(nested[1]);
        grove.put_stopped(nested[2], &[2], b"two");
        assert_eq!(grove.behind(), nested);
        (grove, inner)
    };

    // The three hosts are written with their roots, a write each, and then
    // the Merk is dropped: one write more than D1 counts for each host.
    let (mut grove, inner) = build();
    let writes = grove.fragments.counters().writes;
    grove.clear(inner);
    assert_eq!(grove.fragments.counters().writes, writes + 4);
    assert!(grove.behind().is_empty());
    grove.check();

    // Dropped as it is, the Merk keeps pointers that are behind in cells
    // that nothing writes again, and their page fails the check that every
    // committed page passes.
    let (mut grove, inner) = build();
    grove.unsettled = true;
    let writes = grove.fragments.counters().writes;
    grove.clear(inner);
    assert_eq!(grove.fragments.counters().writes, writes + 1);
    let mut pages = grove.mem.frames.keys();
    assert!(pages.any(|id| grove.mem.view(*id).tree_page().check_fragments().is_err()));
}

/// Two pages: the root `b` in the first and its left child `a` in the
/// second. `b` has its value out of line, in the value log.
fn across_two_pages(params: LayoutParams) -> (Grove, [CellAddress; 2]) {
    let mut grove = grove(params);
    grove.inline_max = 5000;
    grove.put(0, b"b", &value(3900, 1));
    grove.put(0, b"a", &value(3900, 2));
    grove.put(0, b"c", &value(400, 3));
    grove.put(0, b"b", &value(6000, 4));
    let Grove { log, mem, .. } = &mut grove;
    log.write_back(mem);
    let at = [b"a", b"b"].map(|key| grove.node(0, key).at.expect("placed"));
    assert_eq!((at[0].page(), at[1].page()), (page(2), page(1)));
    assert_eq!(grove.merks[0].root, grove.find(0, b"b"));
    (grove, at)
}

/// The value pointer the cell at `at` holds.
fn pointer_of(grove: &Grove, at: CellAddress) -> Option<ValuePointer> {
    let view = grove.mem.view(at.page());
    view.node(at.slot()).expect("placed").value().pointer()
}

#[test]
fn a_rotation_across_two_pages_carries_no_cell_and_no_value() {
    // No fragment is cut for its size here, and none is small.
    let (mut grove, [a, b]) = across_two_pages(params(100, 64, 75, 35));
    let held = pointer_of(&grove, b);
    assert!(held.is_some());

    // Three nodes to the left of `a` make the left side of `b` too high:
    // the node `0`, in the second page, rises above `b`, which is in the
    // first, and `a` goes from `0` to `b`. The links between the two pages
    // change, and no rule has a cell to carry.
    let from = grove.mem.shadow.records.len();
    for key in [b"0", b"/", b"."] {
        let report = grove.put(0, key, &value(200, 5));
        assert!(report.carried.is_empty() && report.left.is_empty());
        assert!(report.created.is_empty());
    }
    assert_eq!(grove.merks[0].root, grove.find(0, b"0"), "the tree rotated");
    assert_eq!(grove.node(0, b"0").right, grove.find(0, b"b"));
    assert_eq!(grove.node(0, b"b").left, grove.find(0, b"a"));
    assert_eq!(grove.page_of(0, b"0"), a.page());
    assert_eq!(grove.mem.count(from, Rec::Remove), 0);
    assert_eq!(grove.mem.count(from, Rec::Insert), 3);
    assert_eq!(
        (grove.node(0, b"a").at, grove.node(0, b"b").at),
        (Some(a), Some(b))
    );
    assert_eq!(pointer_of(&grove, b), held, "the value is where it was");
    assert!(grove.log.carried.is_empty() && grove.log.released.is_empty());
    grove.check();
}

#[test]
fn a_fragment_that_a_rotation_leaves_small_joins_its_parent_by_the_merge_rule() {
    // The same two pages under a layout in which a fragment below 512
    // bytes is small. With `c` gone, `b` is alone in its page.
    let (mut grove, [a, b]) = across_two_pages(params(100, 16, 75, 35));
    grove.delete(0, b"c");
    assert_eq!(grove.node(0, b"b").at, Some(b), "a root stays where it is");
    let held = pointer_of(&grove, b).expect("the value is in the value log");

    // A node to the left of `a` rotates `a` above `b`. That leaves `b` a
    // small fragment whose parent is in a page with room: the case of M1,
    // which carries it there in the same write, with its value.
    let report = grove.put(0, b"0", &value(200, 5));
    assert_eq!(grove.merks[0].root, grove.find(0, b"a"), "the tree rotated");
    assert_eq!(report.carried.len(), 1);
    let carried = report.carried[0];
    assert_eq!((carried.from, carried.cause), (b, Cause::Merge));
    assert_eq!(carried.to.page(), a.page());
    assert_eq!(report.freed, [b.page()]);
    let told = (ValuePlace::Placed(held), b.page(), a.page());
    assert_eq!(grove.log.carried, [told]);
    let carries = grove.fragments.counters().carries;
    assert_eq!(
        (carries[Cause::Merge].cells, carries[Cause::Merge].values),
        (1, 1)
    );
    grove.check();
}

#[test]
fn rotations_across_many_pages_carry_cells_only_out_of_pages_that_are_nearly_full() {
    // No fragment of this grove is small, no fragment is cut for its size,
    // and no page takes a fragment in. So nothing but a full page carries a
    // cell: not a rotation, across pages or not, and not a hash.
    //
    // What a rotation does to two known pages is in the two tests above.
    // This one is the broad guard, and its bound is not exact: a page that
    // a cell left was within `GROWTH` bytes of its capacity before the
    // write, which is what one write can add to a page here, a new cell
    // and a root that a split below sent up.
    const GROWTH: usize = 700;
    let mut grove = grove(params(100, 64, 0, 0));
    let capacity = grove.fragments.layout().capacity();
    let mut carrying = 0;
    for index in 0..1500u32 {
        let used: Vec<(PageId, usize)> = grove
            .mem
            .frames
            .keys()
            .map(|id| (*id, grove.mem.used(*id)))
            .collect();
        let report = grove.put(0, &key(index), &vec![index as u8; 400]);
        if index % 9 == 0 {
            let Grove { log, mem, .. } = &mut grove;
            log.write_back(mem);
        }
        if report.carried.is_empty() {
            continue;
        }
        carrying += 1;
        let full = |id: &PageId| {
            let before = used.iter().find(|(page, _)| page == id);
            before.is_some_and(|(_, used)| used + GROWTH > capacity)
        };
        assert!(
            report.left.iter().all(full),
            "a cell left a page that was not nearly full"
        );
    }
    grove.check();
    let carries = grove.fragments.counters().carries;
    assert_eq!(
        carries[Cause::Merge].cells + carries[Cause::Delete].cells,
        0
    );
    assert!(carries[Cause::Split].values > 100);
    println!(
        "{carrying} of 1500 writes carried cells, with {} rotations in {} pages",
        grove.rotations,
        grove.mem.frames.len()
    );
    assert!(grove.rotations > 500 && carrying < 100 && grove.mem.frames.len() > 30);
}

/// Puts a cell straight into a page, as an earlier write left it.
fn stored_cell(
    mem: &mut Mem,
    page: PageId,
    key: &[u8],
    value_len: usize,
    children: [Option<CellAddress>; 2],
    fragment_root: bool,
) -> CellAddress {
    let child = |child: Option<CellAddress>| {
        child.map(|child| TreeLink {
            child,
            hash: &HASH,
            heights: (0, 0),
            child_key_len: 1,
            aggregate: &[0],
        })
    };
    let value = vec![key[0]; value_len];
    let content = CellContent {
        key,
        feature: &[0],
        value_hash: &HASH,
        kv_hash: &HASH,
        left: child(children[0]),
        right: child(children[1]),
        hosted_roots: &[],
        value: CellValue::Inline(&value),
        fragment_root,
    };
    let handle = mem.write(page, |page| page.insert(&content));
    handle.expect("the cell fits").address()
}

#[test]
fn the_first_page_that_is_read_and_has_the_room_is_taken() {
    // Four pages. The first holds `t` and its left child `u`. The children
    // of `u` are `a` and `b`, each in a page of its own. The right child of
    // `t` is `y`, in the fourth page, which three cells below `a` and `b`
    // fill up to 300 bytes.
    let layout = Layout::proposed(SIZE);
    let (mut mem, mut log) = (Mem::new(SIZE), Log::default());
    let mut fragments = Fragments::new(layout);
    let [top, of_a, of_b, full] = [(); 4].map(|()| mem.create().expect("a page"));
    let x1 = stored_cell(&mut mem, full, b"1", 2600, [None, None], true);
    let x2 = stored_cell(&mut mem, full, b"2", 2600, [None, None], true);
    let y = stored_cell(&mut mem, full, b"y", 100, [None, None], true);
    let bare = TreeCell::len_of(1, 1, None, None, 0, 0) + 2;
    let rest = layout.capacity() - mem.used(full) - bare - 300;
    let x3 = stored_cell(&mut mem, full, b"3", rest, [None, None], true);
    let a = stored_cell(&mut mem, of_a, b"a", 800, [Some(x1), Some(x2)], true);
    let b = stored_cell(&mut mem, of_b, b"b", 2800, [Some(x3), None], true);
    let u = stored_cell(&mut mem, top, b"u", 10, [Some(a), Some(b)], false);
    let t = stored_cell(&mut mem, top, b"t", 10, [Some(u), Some(y)], true);
    assert_eq!(mem.used(full), layout.capacity() - 300);

    // The hints say that the page of `a` is the fuller one, and it is the
    // emptier one. Both have room for what leaves.
    let (used_a, used_b) = (mem.used(of_a), mem.used(of_b));
    assert!(used_a < 1200 && used_b > 2800 && used_b + 700 < layout.seed_limit());
    fragments.hints_mut().note(of_a, 4000);
    fragments.hints_mut().note(of_b, 3000);

    // `y` grows, the fourth page is full, and `y` leaves it. Its fragment
    // is not small, so it goes to a page beside it: to the one its hint
    // made the first choice, which is read and has the room. The other is
    // not read to compare the two.
    let longer = vec![b'y'; 480];
    let nodes = [
        NodeWrite {
            left: link(Target::Kept),
            right: link(Target::Kept),
            ..node(Some(t), b"t")
        },
        NodeWrite {
            value: ValueWrite::Inline(&longer),
            ..node(Some(y), b"y")
        },
    ];
    mem.touched.clear();
    let write = Write {
        nodes: &nodes,
        roots: &[root(Some(t), Target::Kept)],
        ..Write::default()
    };
    let report = fragments
        .apply(&mut mem, &mut log, &write)
        .expect("the write is applied");
    assert_eq!(report.carried.len(), 1);
    let carried = report.carried[0];
    assert_eq!((carried.from, carried.cause), (y, Cause::Split));
    assert_eq!(carried.to.page(), of_a);
    assert!(carried.to.address() != y && report.created.is_empty());
    assert!(!mem.touched.contains(&of_b), "the other page was not read");
    assert_eq!(fragments.hints().used(of_a), Some(mem.used(of_a)));
    assert_eq!(fragments.hints().used(of_b), Some(3000));
    assert_eq!(report.roots, [Some(t)]);
    for id in [top, of_a, of_b, full] {
        let view = mem.view(id);
        view.tree_page().check(SIZE).expect("a valid page");
        view.tree_page()
            .check_fragments()
            .expect("the marks are what the links say");
    }
    let view = mem.view(top);
    let right = view.node(t.slot()).expect("placed").link(Side::Right);
    assert_eq!(right.map(|link| link.child), Some(carried.to.address()));
}

#[test]
fn a_page_with_a_stale_hint_is_taken_if_it_has_the_room() {
    let mut grove = grown_of(PageSize::DEFAULT, LayoutParams::PROPOSED, 3000, 60);
    let seed_limit = grove.fragments.layout().seed_limit();
    let (mut idle, mut unexplained) = (0, 0);
    for index in 3000..5500u32 {
        // Every hint is somewhere between nothing and twice what the page
        // holds: too low for some pages and too high for others.
        let pages: Vec<PageId> = grove.mem.frames.keys().copied().collect();
        for id in pages {
            let stale = grove.rng.below(2 * grove.mem.used(id) + 1);
            grove.fragments.hints_mut().note(id, stale);
        }
        grove.mem.touched.clear();
        let from = grove.mem.shadow.records.len();
        let report = grove.put(0, &key(index), &value(60, 2));
        let changed = grove.mem.shadow.records[from..]
            .iter()
            .map(|(page, _)| *page);
        let mut held: Vec<PageId> = changed.chain(report.freed.iter().copied()).collect();
        held.extend(report.nodes.iter().map(|node| node.page()));
        let mut read = grove.mem.touched.clone();
        read.sort_unstable();
        read.dedup();
        // The bytes of the largest group of cells that went to one page.
        let mut groups: Vec<(PageId, usize)> = Vec::new();
        for carried in &report.carried {
            let view = grove.mem.view(carried.to.page());
            let cell = view.resolve(carried.to).expect("the handle is good").cell();
            let bytes = cell.encoded_len() + 2;
            match groups
                .iter_mut()
                .find(|(page, _)| *page == carried.to.page())
            {
                Some(group) => group.1 += bytes,
                None => groups.push((carried.to.page(), bytes)),
            }
        }
        let largest = groups.iter().map(|group| group.1).max();
        // A page that was read and not used had no room for what left.
        for id in read.iter().filter(|page| !held.contains(page)) {
            idle += 1;
            match largest {
                Some(bytes) => assert!(grove.mem.used(*id) + bytes > seed_limit),
                None => unexplained += 1,
            }
        }
    }
    grove.fragments.hints_mut().clear();
    grove.check();
    println!("{idle} pages were read and had no room, {unexplained} of them in a write that carried nothing");
    assert!(idle > 0 && unexplained <= 2);
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
        (
            params(50, 1, 75, 35),
            LayoutError::SmallFragment,
            "a small fragment is at most half a page",
        ),
        (
            params(9, 64, 75, 35),
            LayoutError::LargestFragment,
            "the largest fragment is 10 to 100 percent of a page",
        ),
        (
            params(101, 16, 75, 35),
            LayoutError::LargestFragment,
            "the largest fragment is 10 to 100 percent of a page",
        ),
        (
            params(50, 4, 75, 35),
            LayoutError::SmallAgainstLargest,
            "three small fragments must fit in a largest one",
        ),
        (
            params(50, 16, 101, 35),
            LayoutError::SeedLimit,
            "the seed limit is at most a full page",
        ),
        (
            params(50, 16, 30, 35),
            LayoutError::MergeThreshold,
            "the merge threshold is at most the seed limit",
        ),
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
