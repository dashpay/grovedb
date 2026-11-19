//! What an evaluation does, one behaviour at a time.

use std::collections::BTreeSet;

use super::{
    acceptance::{calls_on, key, nested, records_since, stored_on, value},
    grove::Grove,
    *,
};

#[test]
fn a_hash_that_is_made_and_equals_the_stored_one_is_not_written() {
    let (mut grove, _, b) = nested();
    grove.lazy = true;
    let changed = key(b, 42);
    let same = grove.node(b, &changed).value.clone();
    let path = grove.root_path(b, &changed);
    let root_hash = grove.expected_root();
    let (calls, records) = (grove.rules.calls.len(), grove.mem.shadow.hashes.len());
    let before = *grove.hashes.counters();

    grove.put(b, &changed, &same);
    assert_eq!(grove.hashes.len(), path.len(), "the path is marked");
    assert_eq!(grove.root_hash(), root_hash);
    // Every hash of the path was made, and none of them differs.
    assert_eq!(
        grove.rules.since(calls),
        calls_on(&grove, std::slice::from_ref(&path), true)
    );
    assert_eq!(grove.mem.shadow.hashes.len(), records, "no hash record");
    let counters = *grove.hashes.counters();
    let stored = stored_on(&grove, &[path]).len() as u64;
    assert_eq!(counters.unchanged - before.unchanged, stored);
    assert_eq!(counters.written, before.written);
    assert!(grove.hashes.is_clean());
    grove.check_hashes();
}

#[test]
fn an_evaluation_goes_down_through_marked_hashes_only() {
    let (mut grove, a, _) = nested();
    let changed = key(a, 5);
    let path = grove.root_path(a, &changed);
    assert!(path.len() > 3);
    let at = grove.at(a, &changed);
    let new_value = value(4_242, grove.nodes[path[0]].value.len());
    grove
        .mem
        .write(at.page(), |page| {
            let mut node = page.node_mut(at.slot()).expect("the node");
            node.set_value(CellValue::Inline(&new_value))
        })
        .expect("the value is set");
    grove.nodes[path[0]].value = new_value;

    // The node is marked, and nothing above it is.
    grove.hashes.mark(at, HashField::Value);
    let (steps, calls) = (grove.mem.steps.len(), grove.rules.calls.len());
    grove.evaluate(grove.root_at.expect("a root"));
    assert_eq!(grove.mem.steps.len(), steps, "the root has no mark");
    assert_eq!(grove.rules.calls.len(), calls);
    assert_eq!(grove.hashes.len(), 1);

    grove.evaluate(at);
    assert!(grove.hashes.is_clean());
    assert_eq!(
        grove.rules.since(calls),
        [(Rule::Value, changed.clone()), (Rule::Kv, changed.clone())]
    );
    assert_eq!(grove.mem.steps[steps..].len(), 2, "a read and a write");

    // The hash of the node in its parent's link is stale now, and nothing
    // says so: the marks were not complete. `check` finds it there, and in
    // no node above.
    let parent = &grove.nodes[path[1]];
    let side = match parent.left == Some(path[0]) {
        true => Side::Left,
        false => Side::Right,
    };
    let parent_at = parent.at.expect("a cell");
    assert_eq!(grove.differing(at), Stale::NONE);
    assert_eq!(
        grove.differing(parent_at),
        Stale::of(HashField::Child(side))
    );
    let above = grove.nodes[path[2]].at.expect("a cell");
    assert_eq!(grove.differing(above), Stale::NONE);

    // Marked up to the root, it is evaluated.
    for pair in path.windows(2) {
        let (below, above) = (&grove.nodes[pair[0]], &grove.nodes[pair[1]]);
        let field = match (above.merk == below.merk, above.left == Some(pair[0])) {
            (false, _) => HashField::Value,
            (true, true) => HashField::Child(Side::Left),
            (true, false) => HashField::Child(Side::Right),
        };
        grove.hashes.mark(above.at.expect("a cell"), field);
    }
    grove.check_marks();
    assert_eq!(grove.root_hash(), grove.expected_root());
    grove.check_hashes();
}

#[test]
fn the_hash_of_a_hosted_root_is_made_for_its_host_also_beside_the_path() {
    let mut grove = Grove::new(false);
    for number in 0..20 {
        grove.put(0, &key(0, number), &value(number, 30));
    }
    let first = grove.host(0, &key(0, 5));
    let second = grove.host(0, &key(0, 5));
    let empty = grove.host(0, &key(0, 5));
    for number in 0..30 {
        grove.put(first, &key(first, number), &value(number, 30));
        grove.put(second, &key(second, number), &value(number, 30));
    }
    grove.check_hashes();
    let host = grove.at(0, &key(0, 5));
    let roots: Vec<_> = grove.mem.node(host).hosted_roots().collect();
    assert_eq!(
        roots,
        [
            Some(grove.root_of(first)),
            Some(grove.root_of(second)),
            None
        ]
    );
    assert!(grove.merks[empty].root.is_none());

    grove.lazy = true;
    let changed = key(first, 3);
    let calls = grove.rules.calls.len();
    grove.put(first, &changed, &value(77, 30));
    let path = grove.root_path(first, &changed);
    assert_eq!(grove.root_hash(), grove.expected_root());

    // Beside the path, the root of the other Merk was hashed for the host,
    // once, and nothing below it.
    let beside = grove.nodes[grove.merks[second].root.expect("a root")]
        .key
        .clone();
    let mut expected = calls_on(&grove, &[path], true);
    expected.push((Rule::Node, beside));
    expected.sort();
    assert_eq!(grove.rules.since(calls), expected);
    grove.check_hashes();
}

#[test]
fn a_value_that_is_not_in_its_cell_is_read_for_its_hash() {
    let (mut grove, a, _) = nested();
    grove.lazy = true;
    let changed = key(a, 200);
    let large = value(1, 700);
    grove.put(a, &changed, &large);
    let at = grove.at(a, &changed);
    assert!(grove.mem.node(at).value().pending().is_some());

    // A value a commit has not put into the value log yet.
    assert_eq!(grove.log.reads, 0);
    assert_eq!(grove.root_hash(), grove.expected_root());
    assert_eq!(grove.log.reads, 1, "the value is read once");
    grove.check_hashes();

    // A value in the value log.
    grove.log.write_back(&mut grove.mem);
    assert!(grove.mem.node(at).value().pointer().is_some());
    let (records, reads) = (grove.mem.shadow.hashes.len(), grove.log.reads);
    grove.hashes.mark(at, HashField::Value);
    grove.evaluate(at);
    assert_eq!(grove.log.reads, reads + 1);
    assert_eq!(grove.mem.shadow.hashes.len(), records, "the same hash");
    grove.check_hashes();

    // The element of a host, which is hashed with the root it hosts.
    let host = key(0, 17);
    grove.put(0, &host, &value(2, 900));
    assert!(grove
        .mem
        .node(grove.at(0, &host))
        .value()
        .pending()
        .is_some());
    let reads = grove.log.reads;
    assert_eq!(grove.root_hash(), grove.expected_root());
    assert_eq!(grove.log.reads, reads + 1);
    grove.check_hashes();
}

#[test]
fn a_write_that_stops_at_a_stop_point_is_evaluated_up_to_there() {
    let (mut grove, a, b) = nested();
    grove.lazy = true;
    let changed = key(b, 250);
    let path = grove.root_path(b, &changed);
    let inside: Vec<usize> = path
        .iter()
        .copied()
        .take_while(|node| grove.nodes[*node].merk == b)
        .collect();
    let host = grove.at(a, &key(a, 123));
    let host_cell = grove.mem.node(host).cell().as_bytes().to_vec();
    let (calls, records) = (grove.rules.calls.len(), grove.mem.shadow.hashes.len());

    // The write holds no node above the root of its Merk, and marks none.
    let new_value = value(7, grove.nodes[path[0]].value.len());
    grove.put_stopped(b, &changed, &new_value);
    let in_merk: BTreeSet<CellAddress> = inside
        .iter()
        .map(|node| grove.nodes[*node].at.expect("a cell"))
        .collect();
    let marked: BTreeSet<CellAddress> = grove.hashes.marked().map(|(at, _)| at).collect();
    assert_eq!(marked, in_merk);
    grove.check_marks();

    // The root hash of the Merk is asked for: what is below is evaluated,
    // and nothing above is read.
    let root = grove.root_of(b);
    let root_key = grove.nodes[*inside.last().expect("a root")].key.clone();
    let steps = grove.mem.steps.len();
    assert_eq!(grove.hash_of(root), grove.expected(b, &root_key).hash);
    let mut expected: Vec<(Rule, Vec<u8>)> = inside
        .iter()
        .map(|node| (Rule::Node, grove.nodes[*node].key.clone()))
        .collect();
    expected.extend([(Rule::Value, changed.clone()), (Rule::Kv, changed.clone())]);
    expected.sort();
    assert_eq!(grove.rules.since(calls), expected);
    assert_eq!(records_since(&grove, records).len(), inside.len() + 1);
    let pages: BTreeSet<PageId> = in_merk.iter().map(|at| at.page()).collect();
    for step in &grove.mem.steps[steps..] {
        let (Step::Read(page) | Step::Write(page)) = step;
        assert!(pages.contains(page), "{step:?} is outside the Merk");
    }
    assert!(grove.hashes.is_clean());
    assert_eq!(grove.mem.node(host).cell().as_bytes(), host_cell);

    // The host is behind: its value hash waits for its element.
    assert_eq!(grove.mem.behind, [host]);
    grove.hashes.mark(host, HashField::Value);
    let (mem, log, rules) = (&mut grove.mem, &mut grove.log, &mut grove.rules);
    let error = grove.hashes.evaluate(mem, log, rules, host);
    let error = error.expect_err("the host is behind");
    assert!(matches!(error, HashError::HostBehind { host: behind } if behind == host));
    let error = grove.hashes.evaluate_all(mem, log, rules);
    assert!(matches!(error, Err(HashError::HostBehind { .. })));
    let error = check(mem, log, rules, host);
    assert!(matches!(error, Err(HashError::HostBehind { .. })));
    assert_eq!(grove.hashes.forget(host).len(), 2, "it is still marked");
    assert_eq!(grove.mem.node(host).cell().as_bytes(), host_cell);

    // The write that carries the change up names the root, marks the host
    // and all above it, and is evaluated when the root hash is asked for.
    grove.finalize(b);
    assert!(grove.mem.behind.is_empty());
    grove.check_marks();
    let calls = grove.rules.calls.len();
    assert_eq!(grove.root_hash(), grove.expected_root());
    let above: Vec<usize> = path[inside.len()..].to_vec();
    let mut expected: Vec<(Rule, Vec<u8>)> = above
        .iter()
        .map(|node| (Rule::Node, grove.nodes[*node].key.clone()))
        .collect();
    expected.push((Rule::Node, root_key));
    for node in &above {
        let entry = &grove.nodes[*node];
        if !entry.hosted.is_empty() {
            expected.push((Rule::Value, entry.key.clone()));
            expected.push((Rule::Kv, entry.key.clone()));
        }
    }
    expected.sort();
    assert_eq!(grove.rules.since(calls), expected);
    grove.check_hashes();
}

#[test]
fn the_marks_go_back_with_their_frames() {
    let (mut grove, _, b) = nested();
    grove.lazy = true;
    grove.put(b, &key(b, 9), &value(5, 50));

    // A savepoint: the frames, and the marks.
    let frames = grove.mem.frames.clone();
    let records = grove.mem.shadow.frames.clone();
    let marks = grove.hashes.clone();
    let node = grove.find(b, &key(b, 300)).expect("the key");
    let old_value = grove.nodes[node].value.clone();
    let report = grove.put(b, &key(b, 300), &value(6, old_value.len()));
    assert!(report.carried.is_empty(), "no cell left its place");
    let evaluated = grove.root_hash();
    assert!(grove.hashes.is_clean());
    assert_ne!(grove.hashes, marks);

    // Back to it.
    grove.mem.frames = frames;
    grove.mem.shadow.frames = records;
    grove.hashes = marks.clone();
    grove.nodes[node].value = old_value;
    assert_eq!(grove.hashes, marks);
    grove.check_marks();
    let again = grove.root_hash();
    assert_eq!(again, grove.expected_root());
    assert_ne!(again, evaluated);
    grove.check_hashes();
}

#[test]
fn a_writer_that_makes_its_hashes_has_no_mark() {
    // Every write of this grove made its hashes and stated them, as Merk
    // does today: nothing was marked, and no rule was called.
    let (mut grove, _, b) = nested();
    assert!(grove.hashes.is_clean());
    assert!(grove.rules.calls.is_empty());
    assert_eq!(*grove.hashes.counters(), Counters::default());

    // A writer that makes a hash that is marked takes the mark off.
    grove.lazy = true;
    let changed = key(b, 77);
    grove.put(b, &changed, &value(3, 50));
    let path = grove.root_path(b, &changed);
    for &node in &path {
        let entry = grove.nodes[node].clone();
        let at = entry.at.expect("a cell");
        let expected = grove.expected(entry.merk, &entry.key);
        for field in grove.hashes.stale(at).fields() {
            let hash = match field {
                HashField::Value => expected.value_hash,
                HashField::KeyValue => expected.kv_hash,
                HashField::Child(side) => {
                    let child = match side {
                        Side::Left => entry.left,
                        Side::Right => entry.right,
                    };
                    let child = &grove.nodes[child.expect("a marked child")];
                    grove.expected(child.merk, &child.key).hash
                }
            };
            grove
                .mem
                .write(at.page(), |page| {
                    let mut node = page.node_mut(at.slot()).expect("the node");
                    node.set_hash(field, &hash)
                })
                .expect("the hash is set");
            grove.hashes.unmark(at, field);
        }
    }
    assert!(grove.hashes.is_clean());
    grove.evaluate_all();
    assert!(grove.rules.calls.is_empty());
    grove.check_hashes();
}

#[test]
fn nodes_that_were_never_hashed_are_hashed_once_when_it_is_asked_for() {
    let mut grove = Grove::new(true);
    for number in 0..150 {
        let len = 20 + number % 60;
        grove.put(0, &key(0, number * 59 % 150), &value(number, len));
        if number % 50 == 0 {
            grove.check_marks();
        }
    }
    let hosted = grove.host(0, &key(0, 75));
    for number in 0..150 {
        let len = 20 + number % 90;
        grove.put(hosted, &key(hosted, number * 67 % 150), &value(number, len));
    }
    for number in (1..150).step_by(3) {
        grove.delete(0, &key(0, number));
        grove.delete(hosted, &key(hosted, number));
    }
    grove.check_marks();
    assert!(grove.rules.calls.is_empty(), "nothing was asked for");
    assert!(grove.mem.shadow.hashes.is_empty());
    assert!(
        grove.carried_marked > 0,
        "cells were carried with their marks"
    );
    assert!(grove.marks_dropped > 0, "marked cells and links went");
    assert_eq!(grove.hashes.len(), grove.cells().len(), "every node");

    assert_eq!(grove.root_hash(), grove.expected_root());
    assert!(grove.hashes.is_clean());
    grove.check_hashes();
    let counters = *grove.hashes.counters();
    let nodes = grove.cells().len() as u64;
    assert_eq!(counters.nodes, nodes);
    assert_eq!(counters.value_hashes, nodes);
    assert_eq!(counters.kv_hashes, nodes);
    assert_eq!(counters.node_hashes, nodes);
    assert_eq!(grove.rules.calls.len() as u64, 3 * nodes);
}

/// A grove in which a Merk was dropped with marks in it.
fn dropped() -> (Grove, Vec<CellAddress>) {
    let (mut grove, a, b) = nested();
    grove.lazy = true;
    grove.put(b, &key(b, 10), &value(1, 50));
    grove.put(b, &key(b, 400), &value(2, 50));
    grove.delete(a, &key(a, 123));
    assert!(!grove.merks[b].alive);
    grove.check_marks();
    let marked = grove.hashes.marked().map(|(at, _)| at);
    let left: Vec<CellAddress> = marked.filter(|at| grove.ghosts.contains(at)).collect();
    assert!(left.len() > 4, "the marks in the dropped Merk stay");
    (grove, left)
}

#[test]
fn the_marks_in_a_dropped_merk_are_evaluated_or_forgotten() {
    // Evaluated with the others: no hash of a live node is touched by it.
    let (mut grove, left) = dropped();
    let calls = grove.rules.calls.len();
    grove.evaluate_all();
    let evaluated = grove.rules.calls.len() - calls;
    assert_eq!(grove.root_hash(), grove.expected_root());
    grove.check_hashes();

    // Or forgotten by their holder, which saves their hashes.
    let (mut grove, _) = dropped();
    for at in &left {
        assert!(!grove.hashes.forget(*at).is_empty());
    }
    let calls = grove.rules.calls.len();
    grove.evaluate_all();
    assert!(grove.rules.calls.len() - calls < evaluated);
    assert_eq!(grove.root_hash(), grove.expected_root());
    grove.check_hashes();
}
