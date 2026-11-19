//! "Changing one value recomputes exactly the nodes on its root path, once."
//! The second sentence, "loading a page computes no hash", is in `store`.

use std::collections::BTreeSet;

use super::{grove::Grove, *};

pub(super) fn key(merk: usize, number: usize) -> Vec<u8> {
    format!("merk {merk} key {number:04}").into_bytes()
}

pub(super) fn value(number: usize, len: usize) -> Vec<u8> {
    let bytes = (0..len).map(|index| (number * 31 + index * 7) as u8);
    bytes.collect()
}

/// A grove of three Merks, one inside the other, with its hashes made: 60
/// nodes in the root Merk, 300 in the Merk `a` that one of them hosts, and
/// 500 in the Merk `b` that a node of `a` hosts.
pub(super) fn nested() -> (Grove, usize, usize) {
    let mut grove = Grove::new(false);
    for number in 0..60 {
        grove.put(0, &key(0, number * 37 % 60), &value(number, 40));
    }
    let a = grove.host(0, &key(0, 17));
    for number in 0..300 {
        grove.put(a, &key(a, number * 113 % 300), &value(number, 60));
    }
    let b = grove.host(a, &key(a, 123));
    for number in 0..500 {
        grove.put(b, &key(b, number * 211 % 500), &value(number, 50));
    }
    grove.check_hashes();
    assert!(grove.hashes.is_empty());
    assert!(grove.mem.frames.len() > 8, "the grove is in several pages");
    (grove, a, b)
}

pub(super) const fn order(field: HashField) -> u8 {
    match field {
        HashField::Value => 0,
        HashField::KeyValue => 1,
        HashField::Child(Side::Left) => 2,
        HashField::Child(Side::Right) => 3,
    }
}

/// The stored hashes that change when the nodes of `paths` do: where, and
/// which. Each path starts at a node whose value changed and ends at the
/// root of the grove.
pub(super) fn stored_on(grove: &Grove, paths: &[Vec<usize>]) -> Vec<(CellAddress, u8)> {
    let at = |node: usize| grove.nodes[node].at.expect("a node of the path has a cell");
    let mut stored = BTreeSet::new();
    for path in paths {
        stored.insert((at(path[0]), order(HashField::Value)));
        stored.insert((at(path[0]), order(HashField::KeyValue)));
        for pair in path.windows(2) {
            let (below, above) = (pair[0], pair[1]);
            let parent = &grove.nodes[above];
            if parent.merk != grove.nodes[below].merk {
                stored.insert((at(above), order(HashField::Value)));
                stored.insert((at(above), order(HashField::KeyValue)));
                continue;
            }
            let side = match parent.left == Some(below) {
                true => Side::Left,
                false => Side::Right,
            };
            stored.insert((at(above), order(HashField::Child(side))));
        }
    }
    stored.into_iter().collect()
}

/// The calls of the rules that make those hashes, each once: the hash of
/// every node of a path, and the value hash and the key-value hash of the
/// nodes whose value changed and of the hosts above them.
pub(super) fn calls_on(
    grove: &Grove,
    paths: &[Vec<usize>],
    with_root: bool,
) -> Vec<(Rule, Vec<u8>)> {
    let mut calls = BTreeSet::new();
    for path in paths {
        for (index, &node) in path.iter().enumerate() {
            let entry = &grove.nodes[node];
            let is_root = index + 1 == path.len();
            if with_root || !is_root {
                calls.insert((Rule::Node, entry.key.clone()));
            }
            let hosts_the_path = index > 0 && grove.nodes[path[index - 1]].merk != entry.merk;
            if index == 0 || hosts_the_path {
                calls.insert((Rule::Value, entry.key.clone()));
                calls.insert((Rule::Kv, entry.key.clone()));
            }
        }
    }
    calls.into_iter().collect()
}

pub(super) fn records_since(grove: &Grove, from: usize) -> Vec<(CellAddress, u8)> {
    let records = grove.mem.shadow.hashes[from..].iter();
    let mut records: Vec<_> = records
        .map(|record| (record.at, order(record.field)))
        .collect();
    records.sort_unstable();
    records
}

#[test]
fn changing_one_value_recomputes_exactly_the_nodes_on_its_root_path_once() {
    let (mut grove, a, b) = nested();
    grove.lazy = true;
    let changed = key(b, 321);
    let path = grove.root_path(b, &changed);
    let merks: Vec<usize> = path.iter().map(|node| grove.nodes[*node].merk).collect();
    assert!(merks.contains(&0) && merks.contains(&a) && merks.contains(&b));
    assert!(
        path.len() >= 12,
        "a path through three Merks: {}",
        path.len()
    );

    // The change computes nothing: it marks.
    let calls = grove.rules.calls.len();
    let before = *grove.hashes.counters();
    grove.put(b, &changed, b"another value of another length");
    assert_eq!(grove.rules.calls.len(), calls);
    assert_eq!(*grove.hashes.counters(), before);
    let on_path: BTreeSet<CellAddress> = path
        .iter()
        .map(|node| grove.nodes[*node].at.expect("a cell"))
        .collect();
    let path_pages: BTreeSet<PageId> = on_path.iter().map(|at| at.page()).collect();
    assert!(path_pages.len() > 1, "the path crosses pages");
    let marked: BTreeSet<CellAddress> = grove.hashes.marked().map(|(at, _)| at).collect();
    assert_eq!(marked, on_path, "the marks are the root path");
    grove.check_marks();

    // One request for the root hash makes every hash of the path, once.
    let (records, steps) = (grove.mem.shadow.hashes.len(), grove.mem.steps.len());
    let others = grove.mem.shadow.others;
    let root_hash = grove.root_hash();
    assert_eq!(root_hash, grove.expected_root());
    assert_eq!(
        grove.rules.since(calls),
        calls_on(&grove, std::slice::from_ref(&path), true),
        "the rules were called for the nodes of the path, each once"
    );
    assert_eq!(
        records_since(&grove, records),
        stored_on(&grove, std::slice::from_ref(&path)),
        "each hash that changed is one hash record"
    );
    assert_eq!(
        grove.mem.shadow.others, others,
        "an evaluation writes hashes and nothing else"
    );
    let hosts = 2;
    let counters = *grove.hashes.counters();
    assert_eq!(counters.nodes - before.nodes, path.len() as u64);
    assert_eq!(counters.node_hashes - before.node_hashes, path.len() as u64);
    assert_eq!(counters.value_hashes - before.value_hashes, 1 + hosts);
    assert_eq!(counters.kv_hashes - before.kv_hashes, 1 + hosts);
    let written = (path.len() - 1 - hosts as usize) + 2 * (1 + hosts as usize);
    assert_eq!(counters.written - before.written, written as u64);
    assert_eq!(counters.unchanged, before.unchanged);

    // It read and wrote the pages of the path, each node in two steps.
    let stepped = &grove.mem.steps[steps..];
    assert_eq!(stepped.len(), 2 * path.len());
    for step in stepped {
        let (Step::Read(page) | Step::Write(page)) = step;
        assert!(path_pages.contains(page), "{step:?} is off the path");
    }

    // Nothing is left to do.
    assert!(grove.hashes.is_empty());
    grove.check_hashes();
    let (calls, records, steps) = (
        grove.rules.calls.len(),
        grove.mem.shadow.hashes.len(),
        grove.mem.steps.len(),
    );
    grove.evaluate_all();
    grove.evaluate(grove.root_at.expect("a root"));
    assert_eq!(grove.rules.calls.len(), calls, "once");
    assert_eq!(grove.mem.steps.len(), steps);

    // The hash of the root is in no cell: asking again makes that one hash.
    let root_key = grove.nodes[*path.last().expect("a root")].key.clone();
    assert_eq!(grove.root_hash(), root_hash);
    assert_eq!(grove.rules.since(calls), [(Rule::Node, root_key)]);
    assert_eq!(grove.mem.shadow.hashes.len(), records);
}

#[test]
fn a_node_that_several_changes_pass_through_is_recomputed_once() {
    let (mut grove, a, b) = nested();
    grove.lazy = true;
    let changes = [(b, 7), (b, 8), (b, 499), (a, 250), (a, 3), (0, 44), (b, 7)];
    let calls = grove.rules.calls.len();
    let records = grove.mem.shadow.hashes.len();
    let mut paths = Vec::new();
    for (round, (merk, number)) in changes.into_iter().enumerate() {
        grove.put(merk, &key(merk, number), &value(round + 1000, 45));
        paths.push(grove.root_path(merk, &key(merk, number)));
        grove.check_marks();
    }
    assert_eq!(grove.rules.calls.len(), calls, "nothing was asked for yet");
    let shared = paths.iter().filter(|path| path.contains(&paths[0][3]));
    assert!(shared.count() > 1, "the paths share nodes below the root");

    assert_eq!(grove.root_hash(), grove.expected_root());
    assert_eq!(grove.rules.since(calls), calls_on(&grove, &paths, true));
    assert_eq!(records_since(&grove, records), stored_on(&grove, &paths));
    grove.check_hashes();
}

#[test]
fn a_commit_evaluates_every_mark_whatever_the_order_of_the_addresses() {
    let (mut grove, a, b) = nested();
    grove.lazy = true;
    let changes = [(b, 100), (a, 299), (0, 0), (b, 101), (a, 150)];
    let calls = grove.rules.calls.len();
    let mut paths = Vec::new();
    for (round, (merk, number)) in changes.into_iter().enumerate() {
        grove.put(merk, &key(merk, number), &value(round + 2000, 30));
        paths.push(grove.root_path(merk, &key(merk, number)));
    }
    let first = grove.hashes.marked().next().expect("a mark").0;
    assert_ne!(Some(first), grove.root_at, "the first mark is not the root");

    grove.evaluate_all();
    // Every stored hash was made once. The hash of the root of the grove is
    // stored nowhere, and nobody asked for it.
    assert_eq!(grove.rules.since(calls), calls_on(&grove, &paths, false));
    grove.check_hashes();
    assert_eq!(grove.root_hash(), grove.expected_root());
}
