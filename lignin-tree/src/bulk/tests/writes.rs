//! Fragment maintenance takes the pages of a load as its own: writes that
//! follow a load are applied to them, and the tree stays what it is.

use std::collections::BTreeMap;

use lignin_format::tree_page::Hash;

use super::*;
use crate::fragment::{
    Fragments, HostedWrite, LinkWrite, NodeWrite, RootWrite, Target, ValueWrite, Write,
};

/// A link of a cell, read from its page.
#[derive(Debug, Clone)]
struct Link {
    child: CellAddress,
    hash: Hash,
    heights: (u8, u8),
    child_key_len: u8,
    aggregate: Vec<u8>,
}

/// A cell on the path of a write, read from its page.
#[derive(Debug, Clone)]
struct Held {
    at: CellAddress,
    key: Vec<u8>,
    feature: Vec<u8>,
    value_hash: Hash,
    kv_hash: Hash,
    links: [Option<Link>; 2],
}

fn held(mem: &Mem, at: CellAddress) -> Held {
    let node = mem.node(at);
    let link = |side| {
        let link = node.link(side)?;
        Some(Link {
            child: link.child,
            hash: *link.hash,
            heights: link.heights,
            child_key_len: link.child_key_len,
            aggregate: link.aggregate.to_vec(),
        })
    };
    Held {
        at,
        key: node.key().to_vec(),
        feature: node.feature().to_vec(),
        value_hash: *node.value_hash(),
        kv_hash: *node.kv_hash(),
        links: [link(Side::Left), link(Side::Right)],
    }
}

/// Every key of the tree below the cell at `root`, with its value.
fn tree(mem: &Mem, log: &Log, root: CellAddress) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let (mut found, mut open) = (BTreeMap::new(), vec![root]);
    while let Some(at) = open.pop() {
        let node = mem.node(at);
        let value = log.value(node.value(), at.page());
        assert!(found.insert(node.key().to_vec(), value).is_none());
        let links = Side::BOTH.into_iter().filter_map(|side| node.link(side));
        open.extend(links.map(|link| link.child));
    }
    found
}

/// Writes a new leaf below a node that a walk from `root` ends at, as a
/// GroveDB operation does: with every node up to the root, each with
/// another hash of the child it was reached through. Returns the root.
fn insert(
    bench: &mut Bench,
    fragments: &mut Fragments,
    rng: &mut Rng,
    root: CellAddress,
    key: &[u8],
    value: &[u8],
) -> CellAddress {
    let mut path = vec![(held(&bench.mem, root), rng.below(2))];
    while let Some(link) = {
        let (node, side) = path.last().expect("the root");
        node.links[*side].clone()
    } {
        path.push((held(&bench.mem, link.child), rng.below(2)));
    }
    let hash = digest(&[key]);
    let pending = bench.log.hold(value).expect("a value");
    let leaf = NodeWrite {
        at: None,
        key,
        feature: &[0],
        value_hash: &hash,
        kv_hash: &hash,
        left: None,
        right: None,
        hosted: HostedWrite::Keep,
        value: pending.map_or(ValueWrite::Inline(value), ValueWrite::Pending),
    };
    let mut nodes = vec![leaf];
    for (depth, (node, side)) in path.iter().enumerate().rev() {
        let below = nodes.len() - 1;
        let link = |at: usize| {
            let link = match (&node.links[at], at == *side) {
                (Some(link), true) => LinkWrite {
                    child: Target::Node(below),
                    hash: &hash,
                    heights: link.heights,
                    child_key_len: link.child_key_len,
                    aggregate: &link.aggregate,
                },
                (Some(link), false) => LinkWrite {
                    child: Target::At(link.child),
                    hash: &link.hash,
                    heights: link.heights,
                    child_key_len: link.child_key_len,
                    aggregate: &link.aggregate,
                },
                (None, true) if depth == path.len() - 1 => LinkWrite {
                    child: Target::Node(0),
                    hash: &hash,
                    heights: (0, 0),
                    child_key_len: key.len() as u8,
                    aggregate: &[],
                },
                (None, _) => return None,
            };
            Some(link)
        };
        nodes.push(NodeWrite {
            at: Some(node.at),
            key: &node.key,
            feature: &node.feature,
            value_hash: &node.value_hash,
            kv_hash: &node.kv_hash,
            left: link(0),
            right: link(1),
            hosted: HostedWrite::Keep,
            value: ValueWrite::Keep,
        });
    }
    let roots = [RootWrite {
        was: Some(root),
        now: Some(Target::Node(nodes.len() - 1)),
        anchored: false,
    }];
    let write = Write {
        nodes: &nodes,
        roots: &roots,
        ..Write::default()
    };
    let report = fragments.apply(&mut bench.mem, &mut bench.log, &write);
    let report = report.expect("a write to loaded pages");
    report.roots[0].expect("the root")
}

#[test]
fn fragment_maintenance_takes_the_pages_of_a_load() {
    for (seed, long) in [(1, 0), (2, 30)] {
        let (mut bench, mut rng) = (Bench::proposed(), Rng(seed));
        let mut model = Model::new(0, 0, long, 0);
        let top = model.merk(&mut rng, 3_000, 0).expect("a Merk");
        let subtree = bench.subtree(&model, top);
        let mut root = bench.root(subtree).expect("a root").address;
        let report = bench.finish();
        let loaded_pages = bench.mem.frames.len();
        assert!(report.counters.fill_percent(&bench.layout) > 88.0);

        let mut fragments = Fragments::new(bench.layout);
        for page in &report.pages {
            fragments.hints_mut().note(page.page, page.used);
        }
        let mut expected = tree(&bench.mem, &bench.log, root);
        assert_eq!(expected.len(), 3_000);
        for added in 0..1_500u32 {
            let key = [b"new".as_slice(), &added.to_be_bytes()].concat();
            let value_len = match rng.chance(long) {
                true => THRESHOLD + 1 + rng.below(100),
                false => rng.below(100),
            };
            let value = rng.bytes(value_len);
            root = insert(&mut bench, &mut fragments, &mut rng, root, &key, &value);
            expected.insert(key, value);
            if added % 100 == 0 {
                for id in bench.mem.frames.keys() {
                    let page = bench.mem.view(*id).tree_page();
                    page.check_fragments().expect("the fragments of a page");
                }
            }
        }
        assert_eq!(
            tree(&bench.mem, &bench.log, root),
            expected,
            "the tree is what it was"
        );
        assert_eq!(bench.mem.cells(), 4_500);
        bench.mem.check();
        let counters = fragments.counters();
        assert!(counters.carries.total().cells > 0, "full pages were split");
        assert!(bench.mem.frames.len() > loaded_pages);
    }
}
