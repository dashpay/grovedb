//! The rules of `docs/lignindb/tree-layout.md`, sections 5 to 8, one at a
//! time.

use lignin_format::tree_page::{CellContent, Side, TreeCell, TreeLink, TreePage};

use super::{
    errors::{link, node, root, HASH},
    grove::Grove,
    *,
};
use crate::fragment::plan::{
    self, counted,
    held::{walks, Walks},
};

fn params(largest: u32, small: u32, seed: u32, merge: u32) -> LayoutParams {
    LayoutParams {
        largest_fragment_percent: largest,
        small_fragment_divisor: small,
        seed_limit_percent: seed,
        merge_below_percent: merge,
        upkeep: Upkeep::Written,
    }
}

/// Pages that are only ever halved: a fragment may fill its page, one that
/// leaves starts a fresh page, and nothing is merged.
const HALVING: LayoutParams = LayoutParams {
    largest_fragment_percent: 100,
    small_fragment_divisor: 8,
    seed_limit_percent: 0,
    merge_below_percent: 0,
    upkeep: Upkeep::Written,
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
    // dozen. The two layouts differ by the seed limit alone, under each
    // upkeep.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let with = |seed| LayoutParams {
            upkeep,
            ..params(50, 16, seed, 0)
        };
        let halved = grown_of(PageSize::DEFAULT, with(0), 8000, 60);
        let seeded = grown_of(PageSize::DEFAULT, with(75), 8000, 60);
        println!(
            "{upkeep:?}: seed limit 0: {} pages, fill {:.1}%; seed limit 75: {} pages, fill {:.1}%",
            halved.mem.frames.len(),
            halved.fill() * 100.0,
            seeded.mem.frames.len(),
            seeded.fill() * 100.0
        );
        assert!(seeded.mem.frames.len() < halved.mem.frames.len());
        assert!(seeded.fill() > halved.fill() + 0.02);
    }
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
    // The two layouts differ by the largest size alone, under each upkeep.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let with = |largest| LayoutParams {
            upkeep,
            ..params(largest, 16, 75, 35)
        };
        let capped = grown(with(50), 2000, 100);
        let free = grown(with(100), 2000, 100);
        let layout = capped.fragments.layout();
        let bound = layout.largest_fragment() + layout.small_fragment() + 400;
        println!(
            "{upkeep:?}: the largest fragment: {} bytes with a limit of {}, {} without one",
            capped.largest_fragment(),
            layout.largest_fragment(),
            free.largest_fragment()
        );
        assert!(capped.largest_fragment() <= bound);
        assert!(free.largest_fragment() > bound);
    }
}

/// What the walks that count a fragment's bytes were held to on this
/// thread, since `before`.
fn walks_since(before: Walks) -> Walks {
    let now = walks();
    Walks {
        walks: now.walks - before.walks,
        ended_at_limit: now.ended_at_limit - before.ended_at_limit,
        read_along: now.read_along - before.read_along,
        left_at_host: now.left_at_host - before.left_at_host,
    }
}

#[test]
fn the_cells_of_a_fragment_are_counted_in_one_step_on_their_page() {
    // A Merk whose fragments hold tens of cells. Under the upkeep of every
    // node, a write that rewrites one node counts the fragment the node is
    // in up to the largest size, and the pages are asked for far fewer
    // steps than the walks read cells: a walk reads the cells of its
    // fragment in the step it read the first of them in.
    let every_node = LayoutParams {
        upkeep: Upkeep::Written,
        ..LayoutParams::PROPOSED
    };
    let mut grove = grown_of(PageSize::DEFAULT, every_node, 6000, 60);
    let before = walks();
    grove.mem.touched.clear();
    for index in 0..200 {
        grove.touch(0, &key(index * 7));
    }
    grove.check();
    let held = walks_since(before);
    let steps = grove.mem.touched.len() as u64;
    println!("{held:?}, in {steps} steps of the pages");
    assert!(held.walks >= 200, "{held:?}");
    assert!(held.read_along > 10 * held.walks, "{held:?}");
    assert!(steps < held.read_along / 2, "{steps} steps, {held:?}");
}

#[test]
fn a_walk_is_held_cell_by_cell_wherever_it_ends() {
    // Hosts with small Merks in their pages, among nodes with values long
    // enough for fragments to reach the largest size: the walks of these
    // writes end at their limit and at the last cell, read cells along a
    // step and leave it at hosts. Each of them was held to the walk cell
    // by cell, which panics where the two differ.
    let before = walks();
    let mut grove = grove(LayoutParams::PROPOSED);
    for index in 0..900 {
        let key = key(index);
        grove.put(0, &key, &value(120, 4));
        if index % 2 == 0 {
            let hosted = grove.host(0, &key, index % 4 == 0);
            grove.put(hosted, b"only", b"small");
        }
    }
    for index in 0..300 {
        grove.touch(0, &key(index * 3));
    }
    grove.check();
    let held = walks_since(before);
    println!("{held:?}");
    assert!(held.walks > 1000, "{held:?}");
    assert!(held.ended_at_limit > 0, "{held:?}");
    assert!(held.ended_at_limit < held.walks, "{held:?}");
    assert!(held.read_along > 0, "{held:?}");
    assert!(held.left_at_host > 0, "{held:?}");
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
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        a_rotation_leaves_a_small_fragment(upkeep);
    }
}

/// The body of the test above, under `upkeep`: the rotation changes the
/// cell of `b`, which loses a link, so either upkeep brings it to M1.
fn a_rotation_leaves_a_small_fragment(upkeep: Upkeep) {
    // The same two pages under a layout in which a fragment below 512
    // bytes is small. With `c` gone, `b` is alone in its page.
    let params = LayoutParams {
        upkeep,
        ..params(100, 16, 75, 35)
    };
    let (mut grove, [a, b]) = across_two_pages(params);
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
    stored_host(mem, page, key, value_len, children, &[], fragment_root)
}

/// Puts a cell that hosts the Merks with the roots `hosted` straight into
/// a page, as an earlier write left it.
fn stored_host(
    mem: &mut Mem,
    page: PageId,
    key: &[u8],
    value_len: usize,
    children: [Option<CellAddress>; 2],
    hosted: &[Option<CellAddress>],
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
        hosted_roots: hosted,
        value: CellValue::Inline(&value),
        fragment_root,
    };
    let handle = mem.write(page, |page| page.insert(&content));
    handle.expect("the cell fits").address()
}

/// The bytes the cells at `cells` take in their pages with their slots,
/// read from the pages.
fn taken(mem: &Mem, cells: &[CellAddress]) -> usize {
    let taken = |at: &CellAddress| {
        let view = mem.view(at.page());
        let node = view.node(at.slot()).expect("the cell is in its page");
        node.cell().encoded_len() + TreePage::SLOT_LEN
    };
    cells.iter().map(taken).sum()
}

/// A write that names the cell at `top` under `key`, keeps what it points
/// at and changes nothing: its plan has read that cell and no other before
/// its rules run.
fn kept<'a>(top: CellAddress, key: &'a [u8], nodes: &'a mut Vec<NodeWrite<'a>>) -> Write<'a> {
    nodes.push(NodeWrite {
        left: link(Target::Kept),
        right: link(Target::Kept),
        ..node(Some(top), key)
    });
    Write {
        nodes,
        ..Write::default()
    }
}

#[test]
fn a_walk_counts_what_its_page_holds_in_one_step() {
    // One page: `m` over `c` and `x`, and `c` over `a` and `e`. Each cell
    // has a value of another length, so a sum of lengths names its cells.
    let layout = Layout::proposed(SIZE);
    let mut mem = Mem::new(SIZE);
    let page = mem.create().expect("a page");
    let a = stored_cell(&mut mem, page, b"a", 11, [None, None], false);
    let e = stored_cell(&mut mem, page, b"e", 23, [None, None], false);
    let c = stored_cell(&mut mem, page, b"c", 47, [Some(a), Some(e)], false);
    let x = stored_cell(&mut mem, page, b"x", 95, [None, None], false);
    let m = stored_cell(&mut mem, page, b"m", 5, [Some(c), Some(x)], true);
    let lens = [a, e, c, x, m].map(|at| taken(&mem, &[at]));
    let distinct: std::collections::BTreeSet<usize> = lens.into_iter().collect();
    assert_eq!(distinct.len(), 5, "{lens:?}");
    let mut nodes = Vec::new();
    let write = kept(m, b"m", &mut nodes);

    // To the end. A walk counts a cell before what the cell points at, and
    // the right side first. The plan read `m` for the write, in a step,
    // and the walk reads the four other cells in one.
    mem.touched.clear();
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, usize::MAX).expect("a count");
    assert_eq!(cells, [m, x, c, e, a]);
    assert_eq!(bytes, taken(&mem, &[m, x, c, e, a]));
    assert_eq!(mem.touched, [page, page]);

    // A limit that the cell which was read for the write reaches: the walk
    // reads nothing.
    mem.touched.clear();
    let limit = taken(&mem, &[m]);
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit).expect("a count");
    assert_eq!((bytes, cells), (limit, vec![m]));
    assert_eq!(mem.touched, [page]);

    // One byte more: the next cell is read and counted whole, and no cell
    // after it is counted.
    mem.touched.clear();
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit + 1).expect("a count");
    assert_eq!(cells, [m, x]);
    assert_eq!(bytes, taken(&mem, &[m, x]));
    assert_eq!(mem.touched, [page, page]);

    // A limit that is reached inside the step, by the second cell the
    // step reads.
    mem.touched.clear();
    let limit = taken(&mem, &[m, x, c]);
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit).expect("a count");
    assert_eq!((bytes, cells), (limit, vec![m, x, c]));
    assert_eq!(mem.touched, [page, page]);

    // One byte more: the third cell of the step, whose bytes this total
    // has without those of the cell after it.
    mem.touched.clear();
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit + 1).expect("a count");
    assert_eq!(cells, [m, x, c, e]);
    assert_eq!(bytes, taken(&mem, &[m, x, c, e]));
    assert_eq!(mem.touched, [page, page]);
}

#[test]
fn a_walk_names_a_missing_cell_when_it_comes_to_it() {
    // One page: `m` over `c` and `x`, where the left link of `c` names a
    // slot of the page that holds no cell.
    let layout = Layout::proposed(SIZE);
    let mut mem = Mem::new(SIZE);
    let page = mem.create().expect("a page");
    let gone = CellAddress::new(page, slot(40));
    let x = stored_cell(&mut mem, page, b"x", 95, [None, None], false);
    let c = stored_cell(&mut mem, page, b"c", 47, [Some(gone), None], false);
    let m = stored_cell(&mut mem, page, b"m", 5, [Some(c), Some(x)], true);
    assert!(mem.view(page).node(gone.slot()).is_none());
    let mut nodes = Vec::new();
    let write = kept(m, b"m", &mut nodes);

    // A limit that `c` reaches: the walk ends there, in the step that read
    // `x` and `c`, and the slot that `c` names is not looked at.
    mem.touched.clear();
    let limit = taken(&mem, &[m, x, c]);
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit).expect("a count");
    assert_eq!((bytes, cells), (limit, vec![m, x, c]));
    assert_eq!(mem.touched, [page, page]);

    // One byte more, and to the end: the walk comes to the slot in the
    // same step, and the count is refused with the address it names.
    for limit in [limit + 1, usize::MAX] {
        mem.touched.clear();
        let refused = counted(&layout, &mut mem, &write, m, limit);
        let FragmentError::Write(error) = refused.expect_err("no cell is there") else {
            panic!("the write is what is refused");
        };
        assert_eq!(error, WriteError::NoNode { address: gone });
        assert_eq!(mem.touched, [page, page]);
    }
}

#[test]
fn a_pointer_back_to_a_cell_above_is_refused_with_that_cells_address() {
    // One page: `m` over `c` and `x`, `c` over `d` and `y`, and the left
    // link of `d` names `m`. The write names `m` and nothing in it points
    // at `m`, so the plan knows no parent of `m` when it comes to `d`.
    let layout = Layout::proposed(SIZE);
    let mut mem = Mem::new(SIZE);
    let page = mem.create().expect("a page");
    // A cell takes the lowest free slot of its page: `m` is put fifth.
    let above = CellAddress::new(page, slot(5));
    let d = stored_cell(&mut mem, page, b"d", 11, [Some(above), None], false);
    let y = stored_cell(&mut mem, page, b"y", 3000, [None, None], false);
    let c = stored_cell(&mut mem, page, b"c", 47, [Some(d), Some(y)], false);
    let x = stored_cell(&mut mem, page, b"x", 3000, [None, None], false);
    let m = stored_cell(&mut mem, page, b"m", 5, [Some(c), Some(x)], true);
    assert_eq!(m, above);
    let mut nodes = Vec::new();
    let write = kept(m, b"m", &mut nodes);
    let refused = |counted: Result<(usize, Vec<CellAddress>), FragmentError>| {
        let FragmentError::Write(error) = counted.expect_err("the cells lead in a circle") else {
            panic!("the write is what is refused");
        };
        error
    };

    // A count that ends before `d` does not meet the link.
    let limit = taken(&mem, &[m, x, c, y]);
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit).expect("a count");
    assert_eq!((bytes, cells), (limit, vec![m, x, c, y]));

    // A count that comes to `d` is refused with the address of `m`, in
    // the step that read `d`: with a limit that a second count of `m`
    // would reach, and with no limit, where a walk that took the link
    // would not end.
    for limit in [taken(&mem, &[m, x, c, y, d]) + 1, usize::MAX] {
        mem.touched.clear();
        let error = refused(counted(&layout, &mut mem, &write, m, limit));
        assert_eq!(error, WriteError::TwoPointers { address: Some(m) });
        assert_eq!(mem.touched, [page, page]);
    }

    // So is a write that counts the fragment in full: one that leaves the
    // page above its capacity, which is then settled (section 6.2).
    let longer = vec![b'm'; layout.capacity() - mem.used(page) + 64];
    let nodes = [NodeWrite {
        left: link(Target::Kept),
        right: link(Target::Kept),
        value: ValueWrite::Inline(&longer),
        ..node(Some(m), b"m")
    }];
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };
    let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
    let from = mem.shadow.records.len();
    let applied = fragments.apply(&mut mem, &mut log, &write);
    let FragmentError::Write(error) = applied.expect_err("the cells lead in a circle") else {
        panic!("the write is what is refused");
    };
    assert_eq!(error, WriteError::TwoPointers { address: Some(m) });
    assert_eq!(mem.shadow.records.len(), from, "no page was changed");

    // The pointer of a host is refused as a link is. Another page: `m`
    // over `h` and `x`, where `h` hosts a Merk whose root it says is `m`.
    let mut mem = Mem::new(SIZE);
    let page = mem.create().expect("a page");
    let above = CellAddress::new(page, slot(3));
    let h = stored_host(
        &mut mem,
        page,
        b"h",
        31,
        [None, None],
        &[Some(above)],
        false,
    );
    let x = stored_cell(&mut mem, page, b"x", 95, [None, None], false);
    let m = stored_cell(&mut mem, page, b"m", 5, [Some(h), Some(x)], true);
    assert_eq!(m, above);
    let mut nodes = Vec::new();
    let write = kept(m, b"m", &mut nodes);
    for limit in [taken(&mem, &[m, x, h]) + 1, usize::MAX] {
        let error = refused(counted(&layout, &mut mem, &write, m, limit));
        assert_eq!(error, WriteError::TwoPointers { address: Some(m) });
    }

    // The cells that lead to the reader are gone through to the last. A
    // third page: `m` over `c` and `x`, `c` over `d`, `d` over `e`, and
    // the left link of `e` names `m`, three cells above it.
    let mut mem = Mem::new(SIZE);
    let page = mem.create().expect("a page");
    let above = CellAddress::new(page, slot(5));
    let e = stored_cell(&mut mem, page, b"e", 23, [Some(above), None], false);
    let d = stored_cell(&mut mem, page, b"d", 11, [Some(e), None], false);
    let c = stored_cell(&mut mem, page, b"c", 47, [Some(d), None], false);
    let x = stored_cell(&mut mem, page, b"x", 95, [None, None], false);
    let m = stored_cell(&mut mem, page, b"m", 5, [Some(c), Some(x)], true);
    assert_eq!(m, above);
    let mut nodes = Vec::new();
    let write = kept(m, b"m", &mut nodes);
    let limit = taken(&mem, &[m, x, c, d]);
    let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit).expect("a count");
    assert_eq!((bytes, cells), (limit, vec![m, x, c, d]));
    for limit in [taken(&mem, &[m, x, c, d, e]) + 1, usize::MAX] {
        let error = refused(counted(&layout, &mut mem, &write, m, limit));
        assert_eq!(error, WriteError::TwoPointers { address: Some(m) });
    }
}

#[test]
fn a_cell_that_leads_to_a_written_node_is_taken_as_its_parent() {
    // One page: `g` over `p`, `p` over `a`, `a` over `l`. The write names
    // `g` and `a`, and no node of it points at `a`. A count from `g`
    // reads `p`, whose link names `a`: a cell the plan knows, which points
    // at `l` and is not above `p`. That is how the plan comes to know the
    // parent of a node like `a`, and it is not a pointer back.
    let layout = Layout::proposed(SIZE);
    let mut mem = Mem::new(SIZE);
    let page = mem.create().expect("a page");
    let l = stored_cell(&mut mem, page, b"l", 11, [None, None], false);
    let a = stored_cell(&mut mem, page, b"a", 23, [Some(l), None], false);
    let p = stored_cell(&mut mem, page, b"p", 47, [Some(a), None], false);
    let g = stored_cell(&mut mem, page, b"g", 5, [Some(p), None], true);
    let keeps_left = |at: CellAddress, key: &'static [u8]| NodeWrite {
        left: link(Target::Kept),
        ..node(Some(at), key)
    };
    let nodes = [keeps_left(g, b"g"), keeps_left(a, b"a")];
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };

    // The plan read `g` and `a` for the write, a step each, and the walk
    // reads `p` and `l` in one.
    mem.touched.clear();
    let (bytes, cells) = counted(&layout, &mut mem, &write, g, usize::MAX).expect("a count");
    assert_eq!(cells, [g, p, a, l]);
    assert_eq!(bytes, taken(&mem, &[g, p, a, l]));
    assert_eq!(mem.touched, [page, page, page]);
}

/// The proposed layout with the largest size measured in what a write
/// changed, whichever upkeep is proposed.
fn of_what_changed() -> LayoutParams {
    LayoutParams {
        upkeep: Upkeep::Changed,
        ..LayoutParams::PROPOSED
    }
}

#[test]
fn a_cell_rewritten_in_place_has_its_fragment_measured_only_where_every_fragment_is() {
    // The same Merk, grown the same way under each setting, and then 200
    // writes that each rewrite one node and the nodes above it as they
    // are. Where every node of a write brings its fragment, each fragment
    // on the way is counted twice: up to the largest size (L1), and up to
    // the small size (M1). Where the largest size is for what a write
    // changed, the second count is the only one, and it ends at the small
    // size.
    let mut written = Walks::default();
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let mut grove = grown_of(PageSize::DEFAULT, params, 6000, 60);
        assert_eq!(grove.fragments.layout().upkeep(), upkeep);
        let before = walks();
        let mut carried = 0;
        for index in 0..200 {
            carried += grove.touch(0, &key(index * 7)).carried.len();
        }
        grove.check();
        let held = walks_since(before);
        println!("{upkeep:?}: {held:?}, {carried} cells carried");
        assert_eq!(carried, 0);
        match upkeep {
            Upkeep::Written => {
                assert!(held.walks >= 400, "{held:?}");
                written = held;
            }
            Upkeep::Changed => {
                assert_eq!(held.walks * 2, written.walks, "{held:?}");
                assert_eq!(held.walks, held.ended_at_limit, "{held:?}");
                assert!(held.read_along * 4 < written.read_along, "{held:?}");
            }
        }
    }
}

/// Two pages, as earlier writes can leave them: `t` in one, and its child
/// `u` in the other with `a` and `b` below it, whose values make the
/// fragment under `u` larger than the largest size of `layout`. Returns
/// the pages and where `t`, `u`, `a` and `b` are.
fn above_the_largest_size(layout: &Layout) -> (Mem, [CellAddress; 4]) {
    let mut mem = Mem::new(SIZE);
    let [top, below] = [(); 2].map(|()| mem.create().expect("a page"));
    let half = layout.largest_fragment() / 2;
    let a = stored_cell(&mut mem, below, b"a", half, [None, None], false);
    let b = stored_cell(&mut mem, below, b"b", half, [None, None], false);
    let u = stored_cell(&mut mem, below, b"u", 10, [Some(a), Some(b)], true);
    let t = stored_cell(&mut mem, top, b"t", 10, [Some(u), None], true);
    assert!(taken(&mem, &[u, a, b]) > layout.largest_fragment());
    assert!(mem.used(below) <= layout.capacity());
    (mem, [t, u, a, b])
}

/// A write of `t`, `u` and `a` as they are: every link kept, and nothing
/// in the write points at `t`.
fn rewritten<'a>(t: CellAddress, u: CellAddress, a: CellAddress) -> [NodeWrite<'a>; 3] {
    [
        NodeWrite {
            left: link(Target::Kept),
            ..node(Some(t), b"t")
        },
        NodeWrite {
            left: link(Target::Kept),
            right: link(Target::Kept),
            ..node(Some(u), b"u")
        },
        node(Some(a), b"a"),
    ]
}

#[test]
fn a_fragment_above_the_largest_size_waits_for_a_write_that_changes_it() {
    // The fragment of every node of a write measured: a write that
    // rewrites `a` in place has the fragment counted, and its root goes up
    // to the page of `t` (L1).
    let layout = Layout::proposed_under(SIZE, Upkeep::Written);
    let (mut mem, [t, u, a, _]) = above_the_largest_size(&layout);
    let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
    let nodes = rewritten(t, u, a);
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };
    let report = fragments
        .apply(&mut mem, &mut log, &write)
        .expect("applied");
    let split: Vec<_> = report
        .carried
        .iter()
        .map(|cell| (cell.from, cell.cause))
        .collect();
    assert_eq!(split, [(u, Cause::Split)]);
    assert_eq!(report.carried[0].to.address().page(), t.page());

    // The largest size for what a write changed alone: the same write
    // carries nothing. It counts the fragment once, for M1, and that count
    // ends at the small size.
    let layout = Layout::new(SIZE, of_what_changed()).expect("a layout");
    let (mut mem, [t, u, a, _]) = above_the_largest_size(&layout);
    let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
    let nodes = rewritten(t, u, a);
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };
    let before = walks();
    let report = fragments
        .apply(&mut mem, &mut log, &write)
        .expect("applied");
    assert!(report.carried.is_empty(), "{:?}", report.carried);
    let counted = walks_since(before);
    assert_eq!((counted.walks, counted.ended_at_limit), (1, 1));

    // A write that gives `a` a child changes the fragment: `a` has one
    // link more and a cell arrives. Now the fragment is counted, and its
    // root goes up.
    let mut nodes = rewritten(t, u, a).to_vec();
    nodes[2].left = link(Target::Node(3));
    nodes.push(node(None, b"0"));
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };
    let report = fragments
        .apply(&mut mem, &mut log, &write)
        .expect("applied");
    let split: Vec<_> = report
        .carried
        .iter()
        .map(|cell| (cell.from, cell.cause))
        .collect();
    assert!(split.contains(&(u, Cause::Split)), "{split:?}");
}

#[test]
fn a_cell_whose_feature_gets_longer_is_not_rewritten_in_place() {
    // The pages of the test above, and the same write, except that it
    // gives `a` a feature of three bytes where it had one. Every pointer
    // is as it was and no value changes, and the cell of `a` is longer:
    // the fragment is counted, and its root goes up, under either upkeep.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let (mut mem, [t, u, a, _]) = above_the_largest_size(&layout);
        let before = taken(&mem, &[a]);
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let mut nodes = rewritten(t, u, a);
        nodes[2].feature = &[1, 2, 3];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let split: Vec<_> = report
            .carried
            .iter()
            .map(|cell| (cell.from, cell.cause))
            .collect();
        assert_eq!(split, [(u, Cause::Split)], "{upkeep:?}");
        let view = mem.view(a.page());
        let stored = view.node(a.slot()).expect("`a` stays where it is");
        assert_eq!(stored.feature(), [1, 2, 3]);
        assert_eq!(taken(&mem, &[a]), before + 2);
    }
}

#[test]
fn a_cell_that_keeps_its_bytes_with_two_parts_changed_is_not_rewritten_in_place() {
    // The same pages again. The write gives `a` a feature that is two
    // bytes longer and a value that is two bytes shorter: the cell takes
    // the bytes it took, and two of its parts have another length. It is
    // a changed cell, so the fragment is counted and its root goes up,
    // under either upkeep.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
        let (mut mem, [t, u, a, _]) = above_the_largest_size(&layout);
        let before = taken(&mem, &[a]);
        let shorter = vec![b'a'; layout.largest_fragment() / 2 - 2];
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let mut nodes = rewritten(t, u, a);
        nodes[2].feature = &[1, 2, 3];
        nodes[2].value = ValueWrite::Inline(&shorter);
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let split: Vec<_> = report
            .carried
            .iter()
            .map(|cell| (cell.from, cell.cause))
            .collect();
        assert_eq!(split, [(u, Cause::Split)], "{upkeep:?}");
        let view = mem.view(a.page());
        let stored = view.node(a.slot()).expect("`a` stays where it is");
        assert_eq!(stored.feature(), [1, 2, 3]);
        assert_eq!(taken(&mem, &[a]), before, "as long as it was");
    }
}

#[test]
fn what_is_left_below_a_root_that_went_up_is_cut_by_the_turns_that_are_not_passed_over() {
    // `t` in one page. In the other its child `r` over `b` over `c` over
    // `a`, a chain within the largest size. The write gives `a` a longer
    // value and rewrites the four cells above it as they are. The
    // fragment is above the largest size then.
    //
    // Under the upkeep of every node each of `r`, `b` and `c` has a turn,
    // and each goes up: what is left after the first two is still above
    // the size. Where the largest size is for what a write changed, their
    // turns are passed over and `a` has the one turn there is: `r` goes
    // up, and `b` over `c` over `a` stays above the size, for a later
    // write that changes a cell of it.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
        let largest = layout.largest_fragment();
        let mut mem = Mem::new(SIZE);
        let [top, below] = [(); 2].map(|()| mem.create().expect("a page"));
        let a = stored_cell(&mut mem, below, b"a", 1500, [None, None], false);
        let c = stored_cell(&mut mem, below, b"c", 700, [Some(a), None], false);
        let b = stored_cell(&mut mem, below, b"b", 700, [Some(c), None], false);
        let r = stored_cell(&mut mem, below, b"r", 10, [Some(b), None], true);
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(r), None], true);
        assert!(taken(&mem, &[r, b, c, a]) <= largest);
        assert!(taken(&mem, &[c, a]) + 2000 > largest && taken(&mem, &[a]) + 2000 <= largest);
        let longer = value(1500 + 2000, 7);
        let keeps_left = |at: CellAddress, key: &'static [u8]| NodeWrite {
            left: link(Target::Kept),
            ..node(Some(at), key)
        };
        let nodes = [
            keeps_left(t, b"t"),
            keeps_left(r, b"r"),
            keeps_left(b, b"b"),
            keeps_left(c, b"c"),
            NodeWrite {
                value: ValueWrite::Inline(&longer),
                ..node(Some(a), b"a")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let mut carried: Vec<_> = (report.carried.iter())
            .map(|cell| (cell.from, cell.cause, cell.to.address().page()))
            .collect();
        carried.sort_by_key(|(from, ..)| from.slot());
        let went_up: &[CellAddress] = match upkeep {
            Upkeep::Written => &[r, b, c],
            Upkeep::Changed => &[r],
        };
        let mut expected: Vec<_> = (went_up.iter())
            .map(|from| (*from, Cause::Split, top))
            .collect();
        expected.sort_by_key(|(from, ..)| from.slot());
        assert_eq!(carried, expected, "{upkeep:?}");
        assert_eq!(report.nodes[4].address(), a, "`a` stays where it is");
        let left: Vec<_> = report.nodes[2..]
            .iter()
            .map(|node| node.address())
            .collect();
        match upkeep {
            Upkeep::Written => assert!(taken(&mem, &[a]) <= largest),
            Upkeep::Changed => {
                assert_eq!(left, [b, c, a], "what is left stays where it is");
                assert!(taken(&mem, &left) > largest);
                assert!(mem.used(below) <= layout.capacity());
            }
        }
    }
}

#[test]
fn a_root_that_went_up_is_looked_at_once_under_either_upkeep() {
    // Three pages: `t`; below it `p` over `b`; and below `b` its child `r`
    // over `a`. The write gives `r` a longer value and rewrites `b`, `p`
    // and `t` as they are. `r` and `a` are above the largest size then, so
    // `r` goes up to the page of `b`. There `p`, `b` and `r` are one
    // fragment above the size, so `p` goes up. What is left, `b` over `r`,
    // is still above it, and stays so under either upkeep, for a later
    // write: `r` is a cell that the rule carried, which is looked at once
    // where it arrived, and the turn of `b` was before `r` arrived, or is
    // passed over.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
        let largest = layout.largest_fragment();
        let mut mem = Mem::new(SIZE);
        let [top, mid, low] = [(); 3].map(|()| mem.create().expect("a page"));
        let a = stored_cell(&mut mem, low, b"a", 1800, [None, None], false);
        let r = stored_cell(&mut mem, low, b"r", 1500, [Some(a), None], true);
        let b = stored_cell(&mut mem, mid, b"b", 1800, [Some(r), None], false);
        let p = stored_cell(&mut mem, mid, b"p", 10, [Some(b), None], true);
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(p), None], true);
        let more = 900;
        assert!(taken(&mem, &[r, a]) <= largest && taken(&mem, &[p, b]) <= largest);
        assert!(taken(&mem, &[r, a]) + more > largest);
        assert!(taken(&mem, &[b, r]) + more > largest);
        let small = layout.small_fragment();
        for cell in [a, r, b] {
            assert!((small..=largest - more).contains(&taken(&mem, &[cell])));
        }
        let longer = value(1500 + more, 7);
        let keeps_left = |at: CellAddress, key: &'static [u8]| NodeWrite {
            left: link(Target::Kept),
            ..node(Some(at), key)
        };
        let nodes = [
            keeps_left(t, b"t"),
            keeps_left(p, b"p"),
            keeps_left(b, b"b"),
            NodeWrite {
                left: link(Target::Kept),
                value: ValueWrite::Inline(&longer),
                ..node(Some(r), b"r")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let mut carried: Vec<_> = (report.carried.iter())
            .map(|cell| (cell.from, cell.cause, cell.to.address().page()))
            .collect();
        carried.sort_by_key(|(from, ..)| (from.page(), from.slot()));
        let mut expected = [(r, Cause::Split, mid), (p, Cause::Split, top)];
        expected.sort_by_key(|(from, ..)| (from.page(), from.slot()));
        assert_eq!(carried, expected, "{upkeep:?}");
        let (b_now, r_now) = (report.nodes[2].address(), report.nodes[3].address());
        assert_eq!((b_now, r_now.page()), (b, mid), "{upkeep:?}");
        assert!(taken(&mem, &[b_now, r_now]) > largest, "{upkeep:?}");
    }
}

#[test]
fn a_page_that_a_root_went_up_into_is_settled_alike_under_either_upkeep() {
    // The three pages again, with more in the middle one. `b` has a second
    // child there, `s`, which the write does not name. Beside them are
    // `x`, a node that nothing in the write points at, and two cells that
    // the write does not hold. The write gives `r` and `x` longer values
    // and rewrites `b`, `p` and `t` as they are.
    //
    // `r` goes up into the middle page and `p` up out of it (L1), and the
    // page is above its capacity. What can leave it is the fragment under
    // `b`, the one cell there that the write holds above `s`: it is split,
    // `b` goes up, and `r` and `s` leave (6.2). Had L1 sent `b` up for its
    // size first, `s` would be a fragment the write does not hold, `x`
    // cannot leave, and the write would be refused. So a root that the
    // rule carried is looked at once under either upkeep, and both settle
    // the page alike.
    let mut carried_under = Vec::new();
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
        let (largest, capacity) = (layout.largest_fragment(), layout.capacity());
        let mut mem = Mem::new(SIZE);
        let [top, mid, low] = [(); 3].map(|()| mem.create().expect("a page"));
        let a = stored_cell(&mut mem, low, b"a", 2100, [None, None], false);
        let r = stored_cell(&mut mem, low, b"r", 1500, [Some(a), None], true);
        let s = stored_cell(&mut mem, mid, b"s", 1900, [None, None], false);
        let b = stored_cell(&mut mem, mid, b"b", 500, [Some(r), Some(s)], false);
        let p = stored_cell(&mut mem, mid, b"p", 100, [Some(b), None], true);
        let x = stored_cell(&mut mem, mid, b"x", 200, [None, None], true);
        let others =
            [b"1", b"2"].map(|key| stored_cell(&mut mem, mid, key, 2200, [None, None], true));
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(p), None], true);
        let (r_more, x_more) = (1000, 1900);
        assert!(taken(&mem, &[r, a]) <= largest && taken(&mem, &[p, b, s]) <= largest);
        assert!(taken(&mem, &[r, a]) + r_more > largest);
        assert!(taken(&mem, &[b, r, s]) + r_more > largest);
        assert!((taken(&mem, &[b, r, s]) + r_more) * 2 >= capacity);
        assert!(mem.used(mid) <= capacity);
        // With `b`, `p` and `r` gone and `s` left, the page would hold more
        // than its capacity.
        let stays = taken(&mem, &[s, x, others[0], others[1]]) + x_more;
        assert!(stays > capacity && stays - taken(&mem, &[s]) <= capacity);
        let (longer_r, longer_x) = (value(1500 + r_more, 7), value(200 + x_more, 9));
        let keeps = |at: CellAddress, key: &'static [u8], right: bool| NodeWrite {
            left: link(Target::Kept),
            right: right.then(|| link(Target::Kept)).flatten(),
            ..node(Some(at), key)
        };
        let nodes = [
            keeps(t, b"t", false),
            keeps(p, b"p", false),
            keeps(b, b"b", true),
            NodeWrite {
                left: link(Target::Kept),
                value: ValueWrite::Inline(&longer_r),
                ..node(Some(r), b"r")
            },
            NodeWrite {
                value: ValueWrite::Inline(&longer_x),
                ..node(Some(x), b"x")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .unwrap_or_else(|error| panic!("{upkeep:?}: {error}"));
        let mut carried: Vec<_> = (report.carried.iter())
            .map(|cell| (cell.from, cell.cause, cell.to.address().page()))
            .collect();
        carried.sort_by_key(|(from, ..)| (from.page(), from.slot()));
        let from: Vec<_> = carried.iter().map(|(from, ..)| *from).collect();
        for cell in [p, b, r, s] {
            assert!(from.contains(&cell), "{upkeep:?}: {carried:?}");
        }
        assert_eq!(report.nodes[4].address(), x, "`x` stays where it is");
        assert!(mem.used(mid) <= capacity, "{upkeep:?}");
        for id in [top, mid, low] {
            let view = mem.view(id);
            view.tree_page().check(SIZE).expect("a valid page");
        }
        carried_under.push(carried);
    }
    assert_eq!(carried_under[0], carried_under[1]);
}

#[test]
fn a_page_with_two_changed_cells_below_one_root_is_settled_alike_under_either_upkeep() {
    // Three pages. `t`; below it a chain `p` over `k` over `b`, where `b`
    // has two children: `j`, with the children `r` (in the third page,
    // over `a`) and `d`, and `s`, which the write does not name. Beside
    // them in the middle page are `x`, a node that nothing in the write
    // points at, and two cells that the write does not hold. The write
    // gives `r`, `d` and `x` longer values and rewrites `j`, `b`, `k`, `p`
    // and `t` as they are.
    //
    // `r` goes up into the middle page, which makes the fragment under
    // `p` too large after the turns of `p`, `k`, `b` and `j`. The turn of
    // `d` sends `p` up, and that of `r`, where it arrived, `k`. `b` stays:
    // it is the one cell the write holds above `s`, and the page, which
    // is above its capacity, is settled by a split of its fragment. Had
    // the turn of `d` gone on to send `k` and `b` up, as it would to
    // bring what is left within the size, `s` could not leave, and the
    // write would be refused. Both upkeeps send the same cells up and
    // settle the page alike.
    let mut carried_under = Vec::new();
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
        let (largest, capacity) = (layout.largest_fragment(), layout.capacity());
        let mut mem = Mem::new(SIZE);
        let [top, mid, low] = [(); 3].map(|()| mem.create().expect("a page"));
        let a = stored_cell(&mut mem, low, b"a", 1980, [None, None], false);
        let r = stored_cell(&mut mem, low, b"r", 1500, [Some(a), None], true);
        let d = stored_cell(&mut mem, mid, b"d", 20, [None, None], false);
        let s = stored_cell(&mut mem, mid, b"s", 1780, [None, None], false);
        let j = stored_cell(&mut mem, mid, b"j", 50, [Some(r), Some(d)], false);
        let b = stored_cell(&mut mem, mid, b"b", 100, [Some(j), Some(s)], false);
        let k = stored_cell(&mut mem, mid, b"k", 50, [Some(b), None], false);
        let p = stored_cell(&mut mem, mid, b"p", 50, [Some(k), None], true);
        let x = stored_cell(&mut mem, mid, b"x", 150, [None, None], true);
        let others =
            [b"1", b"2"].map(|key| stored_cell(&mut mem, mid, key, 2080, [None, None], true));
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(p), None], true);
        let (r_more, x_more) = (1000, 1800);
        let chain = [p, k, b, j, d, s];
        assert!(taken(&mem, &chain) <= largest && taken(&mem, &[r, a]) <= largest);
        assert!(taken(&mem, &[r, a]) + r_more > largest);
        // With `r` in it, the fragment is above the size under `p`, under
        // `k` and under `b`.
        assert!(taken(&mem, &[b, j, d, s, r]) + r_more > largest);
        assert!(mem.used(mid) <= capacity);
        // Without a held cell above it, `s` stays, and the page with it
        // would hold more than its capacity.
        let stays = taken(&mem, &[s, x, others[0], others[1]]) + x_more;
        assert!(stays > capacity && stays - taken(&mem, &[s]) <= capacity);
        let (longer_r, longer_x) = (value(1500 + r_more, 7), value(150 + x_more, 9));
        let longer_d = value(21, 3);
        let keeps = |at: CellAddress, key: &'static [u8], right: bool| NodeWrite {
            left: link(Target::Kept),
            right: right.then(|| link(Target::Kept)).flatten(),
            ..node(Some(at), key)
        };
        let nodes = [
            keeps(t, b"t", false),
            keeps(p, b"p", false),
            keeps(k, b"k", false),
            keeps(b, b"b", true),
            keeps(j, b"j", true),
            NodeWrite {
                left: link(Target::Kept),
                value: ValueWrite::Inline(&longer_r),
                ..node(Some(r), b"r")
            },
            NodeWrite {
                value: ValueWrite::Inline(&longer_d),
                ..node(Some(d), b"d")
            },
            NodeWrite {
                value: ValueWrite::Inline(&longer_x),
                ..node(Some(x), b"x")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .unwrap_or_else(|error| panic!("{upkeep:?}: {error}"));
        let mut carried: Vec<_> = (report.carried.iter())
            .map(|cell| (cell.from, cell.cause, cell.to.address().page()))
            .collect();
        carried.sort_by_key(|(from, ..)| (from.page(), from.slot()));
        let from: Vec<_> = carried.iter().map(|(from, ..)| *from).collect();
        for cell in [p, k, b, r] {
            assert!(from.contains(&cell), "{upkeep:?}: {carried:?}");
        }
        assert_eq!(report.nodes[7].address(), x, "`x` stays where it is");
        assert!(mem.used(mid) <= capacity, "{upkeep:?}");
        for id in [top, mid, low] {
            let view = mem.view(id);
            view.tree_page().check(SIZE).expect("a valid page");
        }
        carried_under.push(carried);
    }
    assert_eq!(carried_under[0], carried_under[1]);
}

/// Two pages with a pointer from the first into the second and back, and
/// one write on them.
///
/// In the first page: `x`, a node that nothing in the write points at,
/// `s`, and cells the write does not hold, which leave `free` bytes of the
/// page. In the second: the children of `x`, `k` and `y`, and a Merk of
/// `hosted` cells that `k` hosts. `s` is the child of `k`. The write gives
/// `x` a value that is longer by `x_more` and `k` one that is longer by
/// `k_more`, which is nothing where `k` is rewritten in place.
struct PointerBack {
    mem: Mem,
    pages: [PageId; 2],
    x: CellAddress,
    k: CellAddress,
    s: CellAddress,
    /// `k` with the Merk it hosts.
    host: Vec<CellAddress>,
    value_of_x: Vec<u8>,
    value_of_k: Vec<u8>,
}

impl PointerBack {
    fn new(layout: &Layout, hosted: usize, free: usize, [x_more, k_more]: [usize; 2]) -> Self {
        let capacity = layout.capacity();
        let mut mem = Mem::new(SIZE);
        let pages = [(); 2].map(|()| mem.create().expect("a page"));
        let [first, second] = pages;
        let mut below = None;
        let mut host = Vec::new();
        for key in b"ABCDEFGHIJKLMNOPQRSTUVWX".iter().take(hosted) {
            let cell = stored_cell(&mut mem, second, &[*key], 130, [below, None], false);
            host.push(cell);
            below = Some(cell);
        }
        let s = stored_cell(&mut mem, first, b"s", 130, [None, None], true);
        let y = stored_cell(&mut mem, second, b"y", 10, [None, None], true);
        let k = stored_host(&mut mem, second, b"k", 60, [Some(s), None], &[below], true);
        host.push(k);
        let x = stored_cell(&mut mem, first, b"x", 80, [Some(k), Some(y)], true);
        for key in [&b"1"[..], b"2", b"3", b"4", b"5"] {
            let room = capacity.saturating_sub(mem.used(first) + free);
            if room < 200 {
                break;
            }
            stored_cell(
                &mut mem,
                first,
                key,
                room.min(2200) - 120,
                [None, None],
                true,
            );
        }
        assert!(mem.used(first) <= capacity && mem.used(second) <= capacity);
        Self {
            mem,
            pages,
            x,
            k,
            s,
            host,
            value_of_x: value(80 + x_more, 9),
            value_of_k: value(60 + k_more, 4),
        }
    }

    /// The nodes of the write, and the pages to make it on.
    fn write(&mut self) -> ([NodeWrite<'_>; 2], &mut Mem) {
        let nodes = [
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::Kept),
                value: ValueWrite::Inline(&self.value_of_x),
                ..node(Some(self.x), b"x")
            },
            NodeWrite {
                left: link(Target::Kept),
                value: ValueWrite::Inline(&self.value_of_k),
                ..node(Some(self.k), b"k")
            },
        ];
        (nodes, &mut self.mem)
    }
}

#[test]
fn a_write_that_finds_no_room_for_what_changed_alone_is_planned_for_every_node() {
    // The pages of [`PointerBack`], where `k` with the Merk it hosts is
    // above the largest size and the longer value of `x` takes the first
    // page above its capacity.
    //
    // Nothing that the write holds in that page can make room: `x` stays
    // where it is (T3) and has no cell below it there. Under the upkeep of
    // every node `k` has a turn at L1 and goes up into the page, and with
    // `k` above it `s` can leave: the page is settled by sending `k` and
    // `s` down to the second page. Where the largest size is for what a
    // write changed, the turn of `k` is passed over, and the plan finds no
    // room. The write is then planned for every node, and is taken as it
    // is under that upkeep.
    let mut left_under = Vec::new();
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
        let (largest, capacity) = (layout.largest_fragment(), layout.capacity());
        let mut pages = PointerBack::new(&layout, 16, 30, [100, 0]);
        let [first, second] = pages.pages;
        assert!(taken(&pages.mem, &pages.host) > largest);
        assert!(pages.mem.used(first) + 100 > capacity);
        let (x, k, s) = (pages.x, pages.k, pages.s);
        let (nodes, mem) = pages.write();
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());

        // The plan under this upkeep and no other.
        let alone = plan::plan_once(&layout, fragments.hints(), mem, &write);
        match upkeep {
            Upkeep::Written => assert!(alone.is_ok()),
            Upkeep::Changed => assert!(
                matches!(alone, Err(FragmentError::NoRoom { page: Some(page) }) if page == first),
                "{:?}",
                alone.err()
            ),
        }

        let report = fragments
            .apply(mem, &mut log, &write)
            .unwrap_or_else(|error| panic!("{upkeep:?}: {error}"));
        assert_eq!(report.nodes[0].address(), x, "`x` stays where it is");
        let mut carried: Vec<_> = (report.carried.iter())
            .map(|cell| (cell.from, cell.cause, cell.to.address().page()))
            .collect();
        carried.sort_by_key(|(from, ..)| (from.page(), from.slot()));
        // `k` went up and came down again with `s`: it is where it was,
        // and `s` is the one cell in another page than before.
        assert_eq!(carried, [(s, Cause::Split, second)], "{upkeep:?}");
        assert_eq!(report.nodes[1].address().page(), second);
        let _ = k;
        for id in [first, second] {
            assert!(mem.used(id) <= capacity, "{upkeep:?}");
            mem.view(id).tree_page().check(SIZE).expect("a valid page");
        }
        left_under.push((carried, mem.used(first), mem.used(second)));
    }
    assert_eq!(left_under[0], left_under[1]);
}

#[test]
fn a_write_is_planned_once_more_under_the_layout_it_has() {
    // The pages of the test above under a layout whose largest size is
    // the whole page. No fragment is above it, so `k` stays below under
    // either upkeep, nothing in the first page can leave, and the write
    // is refused for room under both. A second plan under the proposed
    // parameters, and not under the write's own with the other upkeep,
    // would send `k` up and take the write.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            largest_fragment_percent: 100,
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let mut pages = PointerBack::new(&layout, 16, 30, [100, 0]);
        let first = pages.pages[0];
        let (nodes, mem) = pages.write();
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let no_room = |error: Option<FragmentError>| matches!(error, Some(FragmentError::NoRoom { page: Some(page) }) if page == first);
        let alone = plan::plan_once(&layout, fragments.hints(), mem, &write);
        assert!(no_room(alone.err()), "{upkeep:?}");
        let applied = fragments.apply(mem, &mut log, &write);
        assert!(no_room(applied.err()), "{upkeep:?}");
    }
}

/// Whether a write was taken, or refused for room: as a write is applied
/// ([`Fragments::apply`]), and by the plan under its layout and no other
/// ([`plan::plan_once`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Taken {
    applied: bool,
    alone: bool,
}

/// Plans `write` on `mem` under `layout` and no other, and then applies it.
fn taken_on(layout: Layout, mem: &mut Mem, write: &Write<'_>, pages: &[PageId]) -> Taken {
    let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
    let room = |outcome: Result<(), FragmentError>| match outcome {
        Ok(()) => true,
        Err(FragmentError::NoRoom { .. }) => false,
        Err(error) => panic!("{:?}: {error}", layout.upkeep()),
    };
    let alone = room(plan::plan_once(&layout, fragments.hints(), mem, write).map(drop));
    let applied = room(fragments.apply(mem, &mut log, write).map(drop));
    if applied {
        for id in pages {
            assert!(mem.used(*id) <= layout.capacity(), "{:?}", layout.upkeep());
            mem.view(*id).tree_page().check(SIZE).expect("a valid page");
        }
    }
    Taken { applied, alone }
}

/// One write on the pages of [`PointerBack`], made from `seed`, under
/// `upkeep`. The seed says how many cells the Merk has that `k` hosts,
/// which decides whether `k` is above the largest size with it, how much
/// of the first page is free, and by how much `x` and `k` grow.
fn pointer_back_write(seed: u64, upkeep: Upkeep) -> Taken {
    let mut rng = Rng(seed);
    let mut between = |low: usize, high: usize| low + rng.below(high - low);
    let layout = Layout::proposed_under(SIZE, upkeep);
    let (hosted, free) = (between(4, 25), between(0, 700));
    let more = [between(0, 9) * 100, [0, 0, 0, 40][between(0, 4)]];
    let mut pages = PointerBack::new(&layout, hosted, free, more);
    let ids = pages.pages;
    let (nodes, mem) = pages.write();
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };
    taken_on(layout, mem, &write, &ids)
}

/// One write on three pages that are made from `seed`, under `upkeep`.
///
/// The pages are those of the two tests above with their sizes and their
/// depth drawn from the seed. `t` is alone in the first. The second has a
/// chain of one to four cells under `t`, whose last, `b`, has two
/// children there: `j`, which has `r` and `d` below it, and `s`. Beside
/// them are `x`, a node that nothing in the write points at, and cells
/// the write does not hold, which fill the page to within what the seed
/// leaves free. `r` is in the third page, over `a`. The write rewrites
/// the chain, `b` and `j` as they are and gives `r`, `d` and `x` values
/// that are longer by what the seed says, which can be nothing.
fn crowded_write(seed: u64, upkeep: Upkeep) -> Taken {
    let mut rng = Rng(seed);
    let mut between = |low: usize, high: usize| low + rng.below(high - low);
    let layout = Layout::proposed_under(SIZE, upkeep);
    let capacity = layout.capacity();
    let mut mem = Mem::new(SIZE);
    let [top, mid, low] = [(); 3].map(|()| mem.create().expect("a page"));
    let a = stored_cell(&mut mem, low, b"a", between(600, 2400), [None, None], false);
    let r_len = between(600, 2200);
    let r = stored_cell(&mut mem, low, b"r", r_len, [Some(a), None], true);
    let d_len = between(10, 200);
    let d = stored_cell(&mut mem, mid, b"d", d_len, [None, None], false);
    let s = stored_cell(&mut mem, mid, b"s", between(400, 2200), [None, None], false);
    let j = stored_cell(
        &mut mem,
        mid,
        b"j",
        between(20, 300),
        [Some(r), Some(d)],
        false,
    );
    let depth = between(1, 5);
    let mut chain = Vec::new();
    let mut below = stored_cell(
        &mut mem,
        mid,
        b"b",
        between(50, 400),
        [Some(j), Some(s)],
        false,
    );
    chain.push((below, &b"b"[..], true));
    for key in [&b"k"[..], b"l", b"m", b"p"].into_iter().take(depth) {
        let top_of_chain = chain.len() == depth;
        let len = between(20, 200);
        below = stored_cell(&mut mem, mid, key, len, [Some(below), None], top_of_chain);
        chain.push((below, key, false));
    }
    let x_len = between(100, 400);
    let x = stored_cell(&mut mem, mid, b"x", x_len, [None, None], true);
    // The cells the write does not hold, up to what the seed leaves free.
    let free = between(0, 900);
    for key in [&b"1"[..], b"2", b"3", b"4"] {
        let room = capacity.saturating_sub(mem.used(mid) + free);
        if room < 200 {
            break;
        }
        let len = room.min(between(1200, 2400)) - 120;
        stored_cell(&mut mem, mid, key, len, [None, None], true);
    }
    assert!(mem.used(mid) <= capacity && mem.used(low) <= capacity);
    let t = stored_cell(&mut mem, top, b"t", 10, [Some(below), None], true);

    let grown = |len: usize, by: usize| value(len + by, 5);
    let (r_more, d_more) = (between(0, 4) * 500, between(0, 3) * between(1, 150));
    let x_more = between(0, 5) * 500;
    let (longer_r, longer_d) = (grown(r_len, r_more), grown(d_len, d_more));
    let longer_x = grown(x_len, x_more);
    let keeps = |at: CellAddress, key: &'static [u8], right: bool| NodeWrite {
        left: link(Target::Kept),
        right: right.then(|| link(Target::Kept)).flatten(),
        ..node(Some(at), key)
    };
    let changed = |at: CellAddress, key: &'static [u8], more: usize, longer| NodeWrite {
        value: match more {
            0 => ValueWrite::Keep,
            _ => ValueWrite::Inline(longer),
        },
        ..node(Some(at), key)
    };
    let mut nodes = vec![keeps(t, b"t", false)];
    for (at, key, both) in chain.iter().rev() {
        let key: &'static [u8] = match *key {
            b"b" => b"b",
            b"k" => b"k",
            b"l" => b"l",
            b"m" => b"m",
            _ => b"p",
        };
        nodes.push(keeps(*at, key, *both));
    }
    nodes.push(keeps(j, b"j", true));
    nodes.push(NodeWrite {
        left: link(Target::Kept),
        ..changed(r, b"r", r_more, &longer_r)
    });
    nodes.push(changed(d, b"d", d_more, &longer_d));
    nodes.push(changed(x, b"x", x_more, &longer_x));
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };
    taken_on(layout, &mut mem, &write, &[top, mid, low])
}

#[test]
fn no_write_of_a_crowded_page_is_refused_for_room_under_what_changed_alone() {
    // Two families of writes on crowded pages, four thousand of each, and
    // each write made under both upkeeps on the same pages. A write that
    // the upkeep of every node takes is taken where the largest size is
    // for what a write changed.
    //
    // Both families have writes that are refused under both: those in
    // which the node that nothing points at grows by more than its page
    // can give. In the first ([`crowded_write`]) some are refused under
    // the upkeep of every node alone, where L1 has sent up the cells the
    // write holds in the page and none is left to make room with. In the
    // second ([`pointer_back_write`]) some are taken because they are
    // planned again: the plan for what a write changed finds no room, and
    // the plan for every node of the write does.
    let families: [fn(u64, Upkeep) -> Taken; 2] = [crowded_write, pointer_back_write];
    let mut counts = Vec::new();
    for family in families {
        let (mut taken, mut refused, mut by_changed_alone, mut again) = (0, 0, 0, 0);
        for seed in 0..4000 {
            let written = family(seed, Upkeep::Written);
            let changed = family(seed, Upkeep::Changed);
            assert_eq!(written.alone, written.applied, "seed {seed}");
            assert!(
                changed.applied || !written.applied,
                "seed {seed}: refused for room where the largest size is for what a write \
                 changed, and taken under the upkeep of every node"
            );
            match (written.applied, changed.applied) {
                (true, _) => taken += 1,
                (false, false) => refused += 1,
                (false, true) => by_changed_alone += 1,
            }
            again += usize::from(changed.applied && !changed.alone);
        }
        println!(
            "{taken} writes taken under both upkeeps ({again} of them planned again), \
             {refused} refused for room under both, {by_changed_alone} taken where the \
             largest size is for what a write changed alone"
        );
        assert!(taken > 1000 && refused > 50, "{taken}, {refused}");
        counts.push((by_changed_alone, again));
    }
    let [(by_changed_alone, _), (_, again)] = counts[..] else {
        panic!("two families");
    };
    assert!(by_changed_alone > 0 && again > 0);
}

#[test]
fn a_host_that_is_behind_and_has_its_pointers_stated_is_not_rewritten_in_place() {
    // The pages of the tests above, where `u` hosts a Merk and is behind:
    // its pointer names a slot that holds no cell, and is not followed
    // (T3). The write states the pointers of `u` as none. What the pointer
    // held is not known to the plan, so the cell counts as changed: the
    // fragment under `u` is counted, and its root goes up, under either
    // upkeep.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
        let mut mem = Mem::new(SIZE);
        let [top, below] = [(); 2].map(|()| mem.create().expect("a page"));
        let half = layout.largest_fragment() / 2;
        let gone = CellAddress::new(below, slot(40));
        let a = stored_cell(&mut mem, below, b"a", half, [None, None], false);
        let b = stored_cell(&mut mem, below, b"b", half, [None, None], false);
        let u = stored_host(
            &mut mem,
            below,
            b"u",
            10,
            [Some(a), Some(b)],
            &[Some(gone)],
            true,
        );
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(u), None], true);
        assert!(taken(&mem, &[u, a, b]) > layout.largest_fragment());
        assert!(mem.view(below).node(gone.slot()).is_none());
        mem.behind.push(u);
        let nodes = [
            NodeWrite {
                left: link(Target::Kept),
                ..node(Some(t), b"t")
            },
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::Kept),
                hosted: HostedWrite::Set(&[None]),
                ..node(Some(u), b"u")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let carried = report.carried.iter();
        let carried: Vec<_> = carried.map(|cell| (cell.from, cell.cause)).collect();
        assert_eq!(carried, [(u, Cause::Split)], "{upkeep:?}");
        let now = report.nodes[1].address();
        assert_eq!(now.page(), top);
        let view = mem.view(top);
        let host = view.node(now.slot()).expect("the host, carried");
        assert_eq!(host.hosted_roots().collect::<Vec<_>>(), [None]);
    }
}

#[test]
fn a_page_in_which_the_write_holds_a_cell_is_known_to_it_without_a_hint() {
    // The four pages of `the_first_page_that_is_read_and_has_the_room_is_
    // taken`, with no hint for any of them. The write gives `y` a longer
    // value, so `y` leaves its full page, and it rewrites `t`, `u` and `a`
    // as they are. A page beside `y` is one that the plan knows the room
    // of. It knows the page of `a`, since the write holds `a` there, and
    // not the page of `b`, of which it has no hint: `y` goes to the page
    // of `a`, under either upkeep, and no page is started.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let layout = Layout::proposed_under(SIZE, upkeep);
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
        assert!(mem.used(of_a) + 700 < layout.seed_limit());
        assert!(mem.used(of_b) + 700 < layout.seed_limit());
        assert_eq!(fragments.hints().used(of_a), None);

        let longer = vec![b'y'; 480];
        let keeps_both = |at: CellAddress, key: &'static [u8]| NodeWrite {
            left: link(Target::Kept),
            right: link(Target::Kept),
            ..node(Some(at), key)
        };
        let nodes = [
            keeps_both(t, b"t"),
            keeps_both(u, b"u"),
            keeps_both(a, b"a"),
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
        let carried = report.carried.iter();
        let carried: Vec<_> = carried
            .map(|cell| (cell.from, cell.cause, cell.to.page()))
            .collect();
        assert_eq!(carried, [(y, Cause::Split, of_a)], "{upkeep:?}");
        assert!(report.created.is_empty(), "{upkeep:?}");
        assert!(!mem.touched.contains(&of_b), "{upkeep:?}: no hint, no read");
    }
}

#[test]
fn a_fragment_that_grows_by_a_pointer_alone_is_brought_to_the_rules_under_either_upkeep() {
    // `t` in one page, with `u` on its left and `w` on its right, both in
    // the other. `u` has `v` and `x` below it. The write exchanges `w` and
    // `x`: no cell gets longer or shorter, and the fragment under `u`
    // grows above the largest size by what `u` points at alone. Its root
    // goes up (L1) under either upkeep, since the cell of `u` is changed.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let mut mem = Mem::new(SIZE);
        let [top, below] = [(); 2].map(|()| mem.create().expect("a page"));
        let half = layout.largest_fragment() / 2;
        let v = stored_cell(&mut mem, below, b"v", half, [None, None], false);
        let x = stored_cell(&mut mem, below, b"x", 10, [None, None], false);
        let w = stored_cell(&mut mem, below, b"w", half, [None, None], true);
        let u = stored_cell(&mut mem, below, b"u", 10, [Some(v), Some(x)], true);
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(u), Some(w)], true);
        assert!(taken(&mem, &[u, v, x]) <= layout.largest_fragment());
        assert!(taken(&mem, &[u, v, w]) > layout.largest_fragment());
        let nodes = [
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::At(x)),
                ..node(Some(t), b"t")
            },
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::At(w)),
                ..node(Some(u), b"u")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let carried = report.carried.iter();
        let carried: Vec<_> = carried.map(|cell| (cell.from, cell.cause)).collect();
        assert!(
            carried.contains(&(u, Cause::Split)),
            "{upkeep:?}: {carried:?}"
        );
    }
}

#[test]
fn a_fragment_that_grows_by_a_hosted_pointer_alone_is_brought_to_the_rules_under_either_upkeep() {
    // As above, with a host: `h` has `v` below it and hosts a Merk whose
    // root is `r`, and `t` has `w` on its right. The write makes `w` the
    // root of the Merk that `h` hosts and `r` the right child of `t`. No
    // cell gets longer or shorter, and the fragment under `h` grows above
    // the largest size by the pointer of `h` alone.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let mut mem = Mem::new(SIZE);
        let [top, below] = [(); 2].map(|()| mem.create().expect("a page"));
        let half = layout.largest_fragment() / 2;
        let v = stored_cell(&mut mem, below, b"v", half, [None, None], false);
        let r = stored_cell(&mut mem, below, b"r", 10, [None, None], false);
        let w = stored_cell(&mut mem, below, b"w", half, [None, None], true);
        let h = stored_host(&mut mem, below, b"h", 10, [Some(v), None], &[Some(r)], true);
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(h), Some(w)], true);
        assert!(taken(&mem, &[h, v, r]) <= layout.largest_fragment());
        assert!(taken(&mem, &[h, v, w]) > layout.largest_fragment());
        let hosted = [Some(Target::At(w))];
        let nodes = [
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::At(r)),
                ..node(Some(t), b"t")
            },
            NodeWrite {
                left: link(Target::Kept),
                hosted: HostedWrite::Set(&hosted),
                ..node(Some(h), b"h")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let carried = report.carried.iter();
        let carried: Vec<_> = carried.map(|cell| (cell.from, cell.cause)).collect();
        assert!(
            carried.contains(&(h, Cause::Split)),
            "{upkeep:?}: {carried:?}"
        );
    }
}

#[test]
fn a_small_fragment_beside_a_page_with_room_joins_it_under_either_upkeep() {
    // `t` in one page and its child `u` alone in another: a fragment far
    // below the small size, whose parent's page has room for it (M1).
    // Whether that page has room is not up to the cells of the fragment,
    // so a write that rewrites `u` in place brings it to the page of `t`
    // under either upkeep.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let mut mem = Mem::new(SIZE);
        let [top, below] = [(); 2].map(|()| mem.create().expect("a page"));
        let u = stored_cell(&mut mem, below, b"u", 10, [None, None], true);
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(u), None], true);
        let nodes = [
            NodeWrite {
                left: link(Target::Kept),
                ..node(Some(t), b"t")
            },
            node(Some(u), b"u"),
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let carried = report.carried.iter();
        let carried: Vec<_> = carried.map(|cell| (cell.from, cell.cause)).collect();
        assert_eq!(carried, [(u, Cause::Merge)], "{upkeep:?}");
        assert_eq!(report.carried[0].to.address().page(), top);
        assert_eq!(report.freed, [below]);
    }
}

#[test]
fn a_root_that_goes_up_brings_the_fragment_it_joins_to_the_largest_size_under_either_upkeep() {
    // Three pages: `t`; below it `g` with `p`; below `g` its other child
    // `r` with `a`. Both fragments are within the largest size, and the
    // one under `g` would not be with `r` in it. The write gives `a` a
    // child and rewrites `r`, `g` and `t` as they are. The fragment under
    // `r` is above the largest size then, so `r` goes up to the page of
    // `g` (L1). A cell that is carried is not rewritten in place, whatever
    // the write did to it: it brings the fragment it joins, and `g` goes
    // up in the same write.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let mut mem = Mem::new(SIZE);
        let [top, mid, low] = [(); 3].map(|()| mem.create().expect("a page"));
        let half = layout.largest_fragment() / 2;
        let a = stored_cell(&mut mem, low, b"a", half - 600, [None, None], false);
        let r = stored_cell(&mut mem, low, b"r", half, [Some(a), None], true);
        let p = stored_cell(&mut mem, mid, b"d", half + 600, [None, None], false);
        let g = stored_cell(&mut mem, mid, b"g", 10, [Some(p), Some(r)], true);
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(g), None], true);
        assert!(taken(&mem, &[r, a]) <= layout.largest_fragment());
        assert!(taken(&mem, &[g, p]) <= layout.largest_fragment());
        assert!(taken(&mem, &[g, p, r]) > layout.largest_fragment());
        let more = value(700, 7);
        let nodes = [
            NodeWrite {
                left: link(Target::Kept),
                ..node(Some(t), b"t")
            },
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::Kept),
                ..node(Some(g), b"g")
            },
            NodeWrite {
                left: link(Target::Kept),
                ..node(Some(r), b"r")
            },
            NodeWrite {
                left: link(Target::Node(4)),
                ..node(Some(a), b"a")
            },
            NodeWrite {
                value: ValueWrite::Inline(&more),
                ..node(None, b"0")
            },
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let carried = report.carried.iter();
        let carried: Vec<_> = carried.map(|cell| (cell.from, cell.cause)).collect();
        assert_eq!(
            carried,
            [(r, Cause::Split), (g, Cause::Split)],
            "{upkeep:?}"
        );
        let to = |index: usize| report.carried[index].to.address().page();
        assert_eq!((to(0), to(1)), (mid, top), "{upkeep:?}");
    }
}

#[test]
fn the_fragment_that_leaves_a_full_page_is_that_of_the_last_node_of_the_write_in_it() {
    // No largest size here, so that the page is settled and nothing else.
    // `t` in one page. In the other its children: `f` with `c` below it,
    // and `u`. The write gives `f` a longer value, which leaves the page
    // above its capacity, and rewrites `u` in place. `u` comes after `f`
    // in the write, so the fragment the write holds in the page is that
    // of `u`, which leaves (section 6.2), under either upkeep: a cell
    // that is rewritten in place is held as any other.
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..params(100, 16, 75, 35)
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let mut mem = Mem::new(SIZE);
        let [top, below] = [(); 2].map(|()| mem.create().expect("a page"));
        let c = stored_cell(&mut mem, below, b"c", 1500, [None, None], false);
        let f = stored_cell(&mut mem, below, b"f", 1000, [Some(c), None], true);
        let u = stored_cell(&mut mem, below, b"u", 3500, [None, None], true);
        let t = stored_cell(&mut mem, top, b"t", 10, [Some(f), Some(u)], true);
        let longer = value(3400, 3);
        assert!(mem.used(below) + longer.len() - 1000 > layout.capacity());
        let nodes = [
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::Kept),
                ..node(Some(t), b"t")
            },
            NodeWrite {
                left: link(Target::Kept),
                value: ValueWrite::Inline(&longer),
                ..node(Some(f), b"f")
            },
            node(Some(u), b"u"),
        ];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        let carried = report.carried.iter();
        let carried: Vec<_> = carried.map(|cell| (cell.from, cell.cause)).collect();
        assert_eq!(carried, [(u, Cause::Split)], "{upkeep:?}");
    }
}

#[test]
fn a_page_is_not_read_for_a_cell_rewritten_in_place_where_the_largest_size_is_for_what_changed() {
    // One cell in one page, and a write that rewrites it as it is. Where
    // every node of a write brings its fragment, the plan reads how full
    // the page is. Where the largest size is for what a write changed it
    // does not: the cell takes the bytes it took.
    let mut reads = Vec::new();
    for upkeep in [Upkeep::Written, Upkeep::Changed] {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        let layout = Layout::new(SIZE, params).expect("a layout");
        let mut mem = Mem::new(SIZE);
        let page = mem.create().expect("a page");
        let x = stored_cell(&mut mem, page, b"x", 10, [None, None], true);
        let nodes = [node(Some(x), b"x")];
        let write = Write {
            nodes: &nodes,
            ..Write::default()
        };
        let (mut fragments, mut log) = (Fragments::new(layout), Log::default());
        mem.touched.clear();
        let report = fragments
            .apply(&mut mem, &mut log, &write)
            .expect("applied");
        assert!(report.carried.is_empty());
        assert!(mem.touched.iter().all(|touched| *touched == page));
        reads.push(mem.touched.len());
    }
    assert_eq!(reads[0], reads[1] + 1, "{reads:?}");
}

#[test]
fn a_walk_leaves_its_step_at_a_host() {
    // One page: `m` over `h` and `x`, where `h` hosts a Merk whose root
    // `r` is in the page.
    let layout = Layout::proposed(SIZE);
    for host_first in [false, true] {
        let mut mem = Mem::new(SIZE);
        let page = mem.create().expect("a page");
        let r = stored_cell(&mut mem, page, b"r", 13, [None, None], false);
        let h = stored_host(&mut mem, page, b"h", 31, [None, None], &[Some(r)], false);
        let x = stored_cell(&mut mem, page, b"x", 95, [None, None], false);
        let sides = match host_first {
            false => [Some(h), Some(x)],
            true => [Some(x), Some(h)],
        };
        let m = stored_cell(&mut mem, page, b"m", 5, sides, true);
        let mut nodes = Vec::new();
        let write = kept(m, b"m", &mut nodes);

        // The pointer of `h` is followed. The walk asks the pages whether
        // `h` is behind when it has read it, between two steps: the step
        // in which it read `h` ends there, and the cells after `h` are
        // read in another. It asks once, of `h`, after the step for the
        // write and the one in which it read `h`.
        mem.touched.clear();
        let (bytes, cells) = counted(&layout, &mut mem, &write, m, usize::MAX).expect("a count");
        let order = match host_first {
            false => vec![m, x, h, r],
            true => vec![m, h, r, x],
        };
        assert_eq!(cells, order, "host first: {host_first}");
        assert_eq!(bytes, taken(&mem, &[m, x, h, r]));
        assert_eq!(mem.touched, [page, page, page], "host first: {host_first}");
        assert_eq!(mem.asked, [(h, 2)], "host first: {host_first}");
        assert_eq!(mem.said_behind, 0);

        // A limit that `h` reaches: the pages are asked of `h` once, as
        // before, the walk ends at `h`, and no step follows the one that
        // read it.
        mem.touched.clear();
        mem.asked.clear();
        let through = match host_first {
            false => vec![m, x, h],
            true => vec![m, h],
        };
        let limit = taken(&mem, &through);
        let (bytes, cells) = counted(&layout, &mut mem, &write, m, limit).expect("a count");
        assert_eq!((bytes, cells), (limit, through), "host first: {host_first}");
        assert_eq!(mem.touched, [page, page], "host first: {host_first}");
        assert_eq!(mem.asked, [(h, 2)], "host first: {host_first}");
        assert_eq!(mem.said_behind, 0);

        // `h` is behind: its pointer is not followed, and the root it
        // names is neither read nor counted.
        mem.behind.push(h);
        mem.touched.clear();
        mem.asked.clear();
        let (bytes, cells) = counted(&layout, &mut mem, &write, m, usize::MAX).expect("a count");
        let order = match host_first {
            false => vec![m, x, h],
            true => vec![m, h, x],
        };
        assert_eq!(cells, order, "host first: {host_first}");
        assert_eq!(bytes, taken(&mem, &[m, x, h]));
        let steps = match host_first {
            false => vec![page, page],
            true => vec![page, page, page],
        };
        assert_eq!(mem.touched, steps, "host first: {host_first}");
        assert_eq!(mem.asked, [(h, 2)], "host first: {host_first}");
        assert_eq!(mem.said_behind, 1);
    }
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
