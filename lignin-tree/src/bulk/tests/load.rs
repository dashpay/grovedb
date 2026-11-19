//! What a load leaves: the grove as it was given, in whole fragments, in
//! pages that are full.

use lignin_format::PageSize;

use super::*;
use crate::{
    fragment::LayoutParams,
    value::{InlinePolicy, TreeValues},
};

fn layout(size: PageSize, largest_fragment_percent: u32) -> Layout {
    let params = LayoutParams {
        largest_fragment_percent,
        small_fragment_divisor: if largest_fragment_percent < 25 {
            64
        } else {
            16
        },
        ..LayoutParams::PROPOSED
    };
    Layout::new(size, params).expect("a layout")
}

fn params(least_fill_percent: u32, most_fill_percent: u32) -> LoadParams {
    LoadParams {
        least_fill_percent,
        most_fill_percent,
    }
}

/// Loads a Merk of `count` nodes of `model` as the root Merk of a grove,
/// and returns where its root went and what the load did.
fn load(
    bench: &mut Bench,
    model: &mut Model,
    rng: &mut Rng,
    count: usize,
) -> (usize, Root, LoadReport) {
    let top = model.merk(rng, count, 0).expect("a Merk with a node");
    let subtree = bench.subtree(model, top);
    let root = bench.root(subtree).expect("a root");
    assert_eq!(root.hash, model.nodes[top].hash, "the root hash");
    (top, root, bench.finish())
}

#[test]
fn a_grove_is_loaded_as_it_was_given() {
    for seed in 0..120 {
        let mut rng = Rng(seed);
        let largest = [50, 100, 30][rng.below(3)];
        let fill = [(90, 100), (80, 95), (0, 100), (100, 100)][rng.below(4)];
        let mut bench = Bench::new(layout(SIZE, largest), params(fill.0, fill.1));
        let mut model = Model::new(2, 8, 25, 40);
        let count = 1 + rng.below(700);
        let (top, root, report) = load(&mut bench, &mut model, &mut rng, count);

        let nodes = model.nodes.len();
        assert_eq!(bench.compare(&model, top, root.address, &report), nodes);
        assert_eq!(bench.mem.cells(), nodes, "no cell but those of the grove");
        bench.mem.check();
        bench.mem.check_fragment_sizes(&bench.layout);
        let reads = bench.mem.reads.len();
        bench.check_hashes();

        let counters = report.counters;
        assert_eq!(counters.nodes, nodes as u64);
        assert_eq!(counters.pages as usize, bench.mem.frames.len());
        assert_eq!(report.pages.len(), bench.mem.frames.len());
        for page in &report.pages {
            assert_eq!(page.used, bench.mem.used(page.page), "the hint of a page");
        }
        let used: usize = report.pages.iter().map(|page| page.used).sum();
        assert_eq!(counters.cell_bytes as usize, used);
        let fragments: usize = bench
            .mem
            .frames
            .keys()
            .map(|page| bench.mem.fragments(*page).len())
            .sum();
        assert_eq!(counters.fragments as usize, fragments);
        let long = model
            .nodes
            .iter()
            .filter(|node| node.value.len() > THRESHOLD);
        assert_eq!(counters.values as usize, long.clone().count());
        let long_bytes: usize = long.map(|node| node.value.len()).sum();
        assert_eq!(counters.value_bytes as usize, long_bytes);
        let owned = bench.log.values.values().filter(|value| value.1.is_some());
        assert_eq!(
            owned.count(),
            counters.values as usize,
            "every value has its page"
        );

        let made = model
            .nodes
            .iter()
            .filter(|node| node.given.is_none())
            .count();
        let read: usize = model
            .nodes
            .iter()
            .map(|node| {
                let at = |root: &&Option<(usize, Hosting)>| matches!(root, Some((_, Hosting::At)));
                node.hosted.iter().filter(at).count()
            })
            .sum();
        assert_eq!(
            bench.rules.count(Rule::Value),
            made,
            "a value hash for each node"
        );
        assert_eq!(
            bench.rules.count(Rule::Kv),
            nodes,
            "a key-value hash for each node"
        );
        assert_eq!(
            bench.rules.count(Rule::Node),
            nodes + read,
            "a hash for each node"
        );
        assert_eq!(counters.value_hashes as usize, made);
        assert_eq!(counters.kv_hashes as usize, nodes);
        assert_eq!(counters.node_hashes as usize, nodes + read);
        assert_eq!(reads, read, "a page is read for a root's hash only");

        assert_eq!(
            bench.mem.shadow.inserts, nodes,
            "one insert record for each cell"
        );
        assert_eq!(bench.mem.shadow.others, 0, "and no other record");
        let writes: usize = bench.mem.writes.values().sum();
        assert_eq!(writes, fragments, "one step for each fragment");
    }
}

#[test]
fn the_same_stream_gives_the_same_pages() {
    let pages = |_| {
        let (mut bench, mut model, mut rng) = (Bench::proposed(), Model::new(2, 8, 25, 40), Rng(7));
        load(&mut bench, &mut model, &mut rng, 900);
        bench.mem.frames
    };
    assert!(pages(0) == pages(1));
}

/// A balanced Merk of `count` nodes with short values, loaded under
/// `params`. Returns the pages the load left before it ended, what it did,
/// and where the root went.
fn filled(
    count: usize,
    largest: u32,
    fill: LoadParams,
) -> (Bench, Vec<LoadedPage>, LoadReport, Root) {
    let mut bench = Bench::new(layout(SIZE, largest), fill);
    let (mut model, mut rng) = (Model::new(0, 0, 0, 0), Rng(count as u64));
    model.skew = 0;
    let top = model.merk(&mut rng, count, 0).expect("a Merk");
    let subtree = bench.subtree(&model, top);
    let root = bench.root(subtree).expect("a root");
    let left: Vec<LoadedPage> = bench.loader.drain_pages().collect();
    let report = bench.finish();
    assert_eq!(bench.compare(&model, top, root.address, &report), count);
    bench.mem.check();
    bench.mem.check_fragment_sizes(&bench.layout);
    (bench, left, report, root)
}

#[test]
fn pages_are_at_least_as_full_as_asked() {
    for (largest, least, most) in [
        (50, 90, 100),
        (50, 95, 100),
        (50, 90, 95),
        (100, 90, 100),
        (25, 90, 100),
    ] {
        let (bench, left, report, _) = filled(20_000, largest, params(least, most));
        let capacity = bench.layout.capacity();
        assert!(left.len() > 100, "many pages: {}", left.len());
        for page in &left {
            let percent = page.used * 100 / capacity;
            assert!(
                page.used >= capacity * least as usize / 100,
                "a page that is {percent}% full, with a largest fragment of {largest}%"
            );
            assert!(page.used <= capacity * most as usize / 100, "{percent}%");
        }
        assert!(
            report.pages.len() <= 6,
            "one page for each level is left open"
        );
        let fill = report.counters.fill_percent(&bench.layout);
        assert!(fill >= f64::from(least), "{fill}% in all");
        assert!(fill <= f64::from(most));
    }
}

#[test]
fn a_page_holds_at_least_two_fragments_of_the_proposed_layout() {
    let (bench, left, ..) = filled(20_000, 50, LoadParams::DEFAULT);
    for page in &left {
        let fragments = bench.mem.fragments(page.page);
        assert!(fragments.len() >= 2, "{fragments:?}");
        assert!(fragments.len() <= 8, "{fragments:?}");
    }
}

#[test]
fn a_page_that_is_left_is_not_written_again() {
    let mut bench = Bench::proposed();
    let (mut model, mut rng) = (Model::new(1, 5, 20, 50), Rng(3));
    let top = model.merk(&mut rng, 4_000, 0).expect("a Merk");
    let mut left = Vec::new();
    let mut open = vec![(top, false)];
    let mut done: Vec<Loaded> = Vec::new();
    // The nodes of the top Merk one by one, to look at the pages in between.
    while let Some((index, below_done)) = open.pop() {
        let node = &model.nodes[index];
        if !below_done {
            open.push((index, true));
            open.extend(node.right.map(|right| (right, false)));
            open.extend(node.left.map(|left| (left, false)));
            continue;
        }
        let right = node.right.map(|_| done.pop().expect("the right subtree"));
        let left_child = node.left.map(|_| done.pop().expect("the left subtree"));
        let hosted = bench.hosted(&model, index, false);
        let loaded = bench.node(&model.load(index, [left_child, right], &hosted));
        done.push(loaded.expect("a node"));
        for page in bench.loader.drain_pages() {
            left.push((page, bench.mem.writes[&page.page]));
        }
    }
    let root = bench.root(done.pop().expect("the root")).expect("a root");
    let report = bench.finish();
    assert!(left.len() > 20, "{}", left.len());
    for (page, writes) in &left {
        assert_eq!(
            bench.mem.writes[&page.page], *writes,
            "{} was left",
            page.page
        );
        assert_eq!(bench.mem.used(page.page), page.used);
    }
    let mut all = report.clone();
    all.pages.extend(left.iter().map(|(page, _)| *page));
    assert_eq!(all.pages.len(), bench.mem.frames.len());
    let nodes = model.nodes.len();
    assert_eq!(bench.compare(&model, top, root.address, &report), nodes);
}

#[test]
fn a_path_from_the_root_passes_a_few_pages() {
    let (bench, _, report, root) = filled(20_000, 50, LoadParams::DEFAULT);
    let levels = report.pages.len();
    assert_eq!(levels, 4, "sixteen fragments below each fragment of roots");
    let (mut leaves, mut pages_of_all, mut most) = (0, 0, 0);
    let mut open = vec![(root.address, 1)];
    while let Some((at, pages)) = open.pop() {
        let links = Side::BOTH
            .into_iter()
            .filter_map(|side| bench.mem.node(at).link(side));
        let below: Vec<_> = links.map(|link| link.child).collect();
        if below.is_empty() {
            leaves += 1;
            pages_of_all += pages;
            most = most.max(pages);
        }
        let step = |child: CellAddress| (child, pages + usize::from(child.page() != at.page()));
        open.extend(below.into_iter().map(step));
    }
    let mean = pages_of_all as f64 / f64::from(leaves);
    println!("{mean:.2} pages on a path, at most {most}");
    assert!(mean < levels as f64 + 0.5, "{mean} pages on a path");
    assert!(most <= 2 * levels, "{most} pages on a path");
}

#[test]
fn a_small_merk_stays_with_its_host() {
    let mut bench = Bench::proposed();
    let leaves = [b"a", b"c"].map(|key| bench.leaf(key).expect("a leaf"));
    let merk = bench.node(&NodeLoad {
        left: link(leaves[0]),
        right: link(leaves[1]),
        ..plain(b"b", b"the root of a small Merk")
    });
    let merk = merk.expect("a root");
    let hash = bench.loader.hash_of(merk).expect("its hash");
    let host = bench.node(&NodeLoad {
        hosted: &[HostedLoad::Empty, HostedLoad::Loaded(merk)],
        ..plain(b"host", b"a tree element")
    });
    let root = bench.root(host.expect("a host")).expect("a root");
    assert!(bench.loader.drain_hosted().len() == 1, "one hosted root");
    let report = bench.finish();
    assert!(report.hosted.is_empty(), "it was taken");

    bench.mem.check();
    let page = root.address.page();
    assert_eq!(bench.mem.fragments(page), [(bench.mem.used(page), 4)]);
    let host = bench.mem.node(root.address);
    let roots: Vec<_> = host.hosted_roots().collect();
    let hosted = roots[1].expect("the Merk's root");
    assert_eq!(roots[0], None);
    assert!(!bench.mem.node(hosted).is_fragment_root());
    assert_eq!(
        host.value_hash(),
        &value_rule(b"a tree element", &[None, Some(hash)])
    );
}

#[test]
fn a_host_is_above_the_page_of_a_root_that_was_placed() {
    let mut bench = Bench::proposed();
    let merk = bench.leaf(b"the root of a Merk").expect("a leaf");
    let placed = bench.root(merk).expect("a root");
    assert!(bench.mem.reads.is_empty());
    let host = bench.node(&NodeLoad {
        hosted: &[HostedLoad::At(placed.address)],
        ..plain(b"host", b"a tree element")
    });
    let root = bench.root(host.expect("a host")).expect("a root");
    assert_eq!(
        bench.mem.reads,
        [placed.address.page()],
        "for the root's hash"
    );
    assert_ne!(root.address.page(), placed.address.page());
    let report = bench.finish();
    assert_eq!(report.pages.len(), 2, "a page of each level");
    assert!(
        report.hosted.is_empty(),
        "a root by its address is not reported"
    );
    bench.mem.check();
    let host = bench.mem.node(root.address);
    assert_eq!(host.hosted_roots().next(), Some(Some(placed.address)));
    assert_eq!(
        host.value_hash(),
        &value_rule(b"a tree element", &[Some(placed.hash)])
    );
    assert!(bench.mem.node(placed.address).is_fragment_root());
}

#[test]
fn a_host_with_a_given_value_hash_reads_no_page() {
    let mut bench = Bench::proposed();
    let merk = bench.leaf(b"the root of a Merk").expect("a leaf");
    let placed = bench.root(merk).expect("a root");
    let host = bench.node(&NodeLoad {
        value_hash: Some(&[7; 32]),
        hosted: &[HostedLoad::At(placed.address)],
        ..plain(b"host", b"a tree element")
    });
    let root = bench.root(host.expect("a host")).expect("a root");
    assert!(bench.mem.reads.is_empty());
    assert_eq!(bench.rules.count(Rule::Value), 1, "the leaf's");
    let host = bench.mem.node(root.address);
    assert_eq!(host.value_hash(), &[7; 32]);
    assert_eq!(host.kv_hash(), &kv_rule(b"host", &[7; 32]));
}

#[test]
fn a_host_can_name_a_root_of_an_earlier_load() {
    let mut bench = Bench::proposed();
    let merk = bench.leaf(b"the root of a Merk").expect("a leaf");
    let placed = bench.root(merk).expect("a root");
    bench.finish();
    let host = bench.node(&NodeLoad {
        hosted: &[HostedLoad::At(placed.address)],
        ..plain(b"host", b"a tree element")
    });
    let root = bench.root(host.expect("a host")).expect("a root");
    assert_ne!(
        root.address.page(),
        placed.address.page(),
        "a load writes new pages"
    );
    bench.finish();
    bench.mem.check();
}

#[test]
fn a_cell_above_the_largest_fragment_is_a_fragment_of_its_own() {
    let mut bench = Bench::new(layout(SIZE, 10), LoadParams::DEFAULT);
    let largest = bench.layout.largest_fragment();
    let leaves = [b"a", b"c"].map(|key| bench.leaf(key).expect("a leaf"));
    let (key, feature, value) = ([b'k'; 255], [b'f'; 255], [b'v'; 256]);
    let big = bench.node(&NodeLoad {
        feature: &feature,
        left: link(leaves[0]),
        right: link(leaves[1]),
        ..plain(&key, &value)
    });
    let root = bench.root(big.expect("a node")).expect("a root");
    bench.finish();
    bench.mem.check();
    bench.mem.check_fragment_sizes(&bench.layout);
    let page = root.address.page();
    let fragments = bench.mem.fragments(page);
    assert_eq!(fragments.len(), 1);
    assert!(
        fragments[0].0 > largest && fragments[0].1 == 1,
        "{fragments:?}"
    );
    assert_eq!(
        bench.mem.frames.len(),
        2,
        "the leaves are in the page below"
    );
}

#[test]
fn a_chain_has_a_level_for_every_fragment() {
    let mut bench = Bench::proposed();
    let mut below = bench.leaf(&0u32.to_be_bytes()).expect("a leaf");
    for key in 1..400u32 {
        let node = bench.node(&NodeLoad {
            left: link(below),
            ..plain(&key.to_be_bytes(), &[9; 100])
        });
        below = node.expect("a node");
    }
    let root = bench.root(below).expect("a root");
    let report = bench.finish();
    bench.mem.check();
    bench.mem.check_fragment_sizes(&bench.layout);
    assert_eq!(bench.mem.cells(), 400);
    assert!(report.pages.len() > 10, "{}", report.pages.len());
    let (mut at, mut cells) = (Some(root.address), 0);
    while let Some(cell) = at {
        cells += 1;
        at = bench.mem.node(cell).link(Side::Left).map(|link| link.child);
    }
    assert_eq!(cells, 400);
}

#[test]
fn the_values_of_a_scope_take_the_values_of_a_load() {
    let layout = Layout::proposed(SIZE);
    let mut loader = Loader::new(layout, LoadParams::default()).expect("a fill");
    assert_eq!(loader.layout(), &layout);
    let (mut mem, mut rules) = (Mem::new(SIZE), Rules::default());
    let mut values = TreeValues::new(InlinePolicy::DEFAULT);
    assert_eq!(values.hold(&[1; 256]).expect("a short value"), None);

    let document = [9; 400];
    let leaf = loader.node(
        &mut mem,
        &mut values,
        &mut rules,
        &plain(b"document", &document),
    );
    let top = loader.node(
        &mut mem,
        &mut values,
        &mut rules,
        &NodeLoad {
            left: link(leaf.expect("a leaf")),
            ..plain(b"reference", &[7; 40])
        },
    );
    let root = loader.root(&mut mem, &mut values, top.expect("a node"));
    let root = root.expect("a root");
    assert_eq!(loader.counters().values, 1);
    assert_eq!(loader.counters().value_bytes, 400);
    loader.finish().expect("a load");

    let page = root.address.page();
    assert_eq!(
        values.pages().collect::<Vec<_>>(),
        [page],
        "the page owns the value"
    );
    let top = mem.node(root.address);
    assert_eq!(top.value(), CellValue::Inline(&[7; 40]));
    let leaf = mem.node(top.link(Side::Left).expect("a link").child);
    let pending = leaf.value().pending().expect("a value out of line");
    assert_eq!(pending.value_len(), 400);
    assert_eq!(leaf.value_hash(), &value_rule(&document, &[]));
}

#[test]
fn a_load_of_nothing_fills_nothing() {
    let layout = Layout::proposed(SIZE);
    let loader = Loader::new(layout, LoadParams::DEFAULT).expect("a fill");
    let report = loader.finish().expect("a load");
    assert_eq!(report, LoadReport::default());
    assert_eq!(report.counters.fill_percent(&layout), 0.0);
}
