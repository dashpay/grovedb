//! The first acceptance sentence of TREE-05: a by-prefix open is one
//! directory lookup.

use super::{super::record::entry_key, *};

/// A grove of `width` Merks below the root, each with `width` below it,
/// and so on for `depth` levels.
fn wide(width: usize, depth: usize) -> (Grove, Vec<Vec<String>>) {
    let mut grove = Grove::new();
    let mut paths: Vec<Vec<String>> = vec![Vec::new()];
    let mut level = paths.clone();
    for _ in 0..depth {
        let mut next = Vec::new();
        for above in &level {
            for index in 0..width {
                let mut at = above.clone();
                at.push(format!("element-{index:03}"));
                let keys: Vec<&str> = at.iter().map(String::as_str).collect();
                grove.add(&keys);
                next.push(at);
            }
        }
        paths.extend(next.iter().cloned());
        level = next;
    }
    (grove, paths)
}

fn keys(path: &[String]) -> Vec<&str> {
    path.iter().map(String::as_str).collect()
}

#[test]
fn an_open_by_prefix_is_one_lookup() {
    let (mut grove, paths) = wide(6, 4);
    assert_eq!(paths.len(), 1 + 6 + 36 + 216 + 1296);
    let levels = grove.pages.levels(&grove.directory);
    assert!(levels >= 2, "the directory has branches: {levels} levels");

    // One lookup is one way down the keyspace: a page of each level, and
    // nothing else. It is what reading the entry's record alone costs.
    let one_lookup = |grove: &Grove, at: &[String]| {
        let prefix = prefix_of(&path(&keys(at)));
        let (found, read) = grove
            .pages
            .reads(|| grove.directory.open(&grove.pages, &prefix));
        let (stored, record) = grove.pages.reads(|| {
            let mut value = Vec::new();
            let key = entry_key(&prefix);
            let tree = grove.directory.tree();
            tree.get(&grove.pages, &key, &mut value).expect("a read")
        });
        assert_eq!(read, (levels, 0), "an open reads one page of each level");
        assert_eq!(read, record, "an open reads what its record costs to read");
        (found.expect("a lookup"), stored)
    };
    for at in &paths {
        let (found, stored) = one_lookup(&grove, at);
        let found = found.expect("the Merk is entered");
        assert!(stored);
        assert_eq!(found.depth(), at.len());
    }
    // A prefix that nothing is entered under costs the same.
    let (found, stored) = one_lookup(&grove, &["nothing".to_owned()]);
    assert!(found.is_none() && !stored);

    // So does a prefix whose entry a drop left: the lookup reads the
    // entry, and knows from it alone that the Merk is not in use.
    let dropped = &paths[1];
    let under: Vec<&Vec<String>> = paths[2..]
        .iter()
        .filter(|at| at.starts_with(dropped))
        .collect();
    assert_eq!(under.len(), 6 + 36 + 216);
    grove.unlink(&keys(dropped));
    assert_eq!(grove.pages.levels(&grove.directory), levels);
    for at in under {
        let (found, stored) = one_lookup(&grove, at);
        assert!(found.is_none(), "no open returns a left-over entry");
        assert!(stored, "the entry is still there");
    }
    let (found, stored) = one_lookup(&grove, dropped);
    assert!(found.is_none() && !stored);
    for at in paths.iter().filter(|at| !at.starts_with(dropped)) {
        assert!(one_lookup(&grove, at).0.is_some());
    }
    grove.check();
}

#[test]
fn an_entry_that_is_too_long_for_its_page_is_one_lookup_and_one_value() {
    // An entry holds eight bytes for each Merk above its own, so 400
    // Merks deep it is longer than a page of the smallest size keeps in
    // itself, and its bytes are out of line.
    let mut grove = Grove::new();
    let names: Vec<String> = (0..400).map(|level| format!("{level}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    for depth in 1..=names.len() {
        grove.add(&names[..depth]);
    }
    let levels = grove.pages.levels(&grove.directory);
    let prefix = prefix_of(&path(&names));
    let (found, read) = grove
        .pages
        .reads(|| grove.directory.open(&grove.pages, &prefix));
    let found = found.expect("a lookup").expect("the Merk is entered");
    assert_eq!(found.depth(), 400);
    assert!(found.as_bytes().len() > grove.pages.config.max_inline());
    assert_eq!(read, (levels, 1));
    let above = grove.entry(&names[..399]);
    assert_eq!(found.parent_id(), Some(above.id()));
    assert_eq!(found.lineage().count(), 400);
    grove.check();

    // A drop near the top takes all of them out of every lookup.
    grove.unlink(&names[..2]);
    let (found, read) = grove
        .pages
        .reads(|| grove.directory.open(&grove.pages, &prefix));
    assert!(found.expect("a lookup").is_none());
    assert_eq!(read, (levels, 1));
    assert_eq!(grove.check().left_over, 398);
}

#[test]
fn an_open_returns_what_was_entered() {
    let mut grove = Grove::new();
    let top = grove.entry(&[]);
    assert_eq!((top.id(), top.root(), top.host()), (id(1), None, None));
    assert_eq!((top.tree_type(), top.depth()), (&b"tree"[..], 0));
    assert!(grove.directory.is_live(&top));

    let second = grove.add(&["a"]);
    let at = grove.root(&["a"]);
    let entry = grove.entry(&["a"]);
    assert_eq!((entry.id(), second), (id(2), id(2)));
    let root = entry.root().expect("a root");
    assert_eq!((root.address, root.key), (at, &b"root"[..]));
    let host = entry.host().expect("a host");
    assert_eq!(
        (host.parent, host.key, host.position),
        (&prefix_of(&[]), &b"a"[..], 0)
    );
    assert_eq!(entry.lineage().collect::<Vec<_>>(), [id(1)]);
    assert_eq!(
        grove.directory.next_id(&grove.pages).expect("a header"),
        id(3)
    );

    let prefix = prefix_of(&path(&["a"]));
    let owner = grove
        .directory
        .root_owner(&grove.pages, at)
        .expect("a lookup");
    assert_eq!(owner, Some(prefix));
    let nowhere = grove.directory.root_owner(&grove.pages, cell(77, 7));
    assert_eq!(nowhere.expect("a lookup"), None);

    // The tree type is whatever bytes it is given, and can be given again.
    let directory = &grove.directory;
    directory
        .set_tree_type(&mut grove.pages, &prefix, &[0xFF; 255])
        .expect("another type");
    assert_eq!(grove.entry(&["a"]).tree_type(), [0xFF; 255]);
    let revision = grove.pages.revision;
    directory
        .set_tree_type(&mut grove.pages, &prefix, &[0xFF; 255])
        .expect("the same type");
    assert_eq!(
        grove.pages.revision, revision,
        "the same type writes nothing"
    );
    let entry = grove.entry(&["a"]);
    assert_eq!(entry.root().map(|root| root.address), Some(at));
    assert_eq!(entry.host().map(|host| host.key), Some(&b"a"[..]));

    // A directory that is loaded from its pages is the same directory.
    let loaded = Directory::load(&grove.pages, directory.root_page(), grove.pages.config);
    let loaded = loaded.expect("a directory");
    assert_eq!(loaded.root_page(), directory.root_page());
    assert_eq!(
        loaded.open(&grove.pages, &prefix).expect("a lookup"),
        Some(entry)
    );
    assert_eq!(grove.check().live, 2);
}

#[test]
fn a_host_has_a_merk_at_each_of_its_positions() {
    let mut grove = Grove::new();
    grove.root(&[]);
    let top = prefix_of(&[]);
    let mut entered = Vec::new();
    for position in [0, 1, 255] {
        let prefix = prefix_of(&[(b"indexed".to_vec(), position)]);
        let host = Host {
            parent: &top,
            key: b"indexed",
            position,
        };
        let added = grove
            .directory
            .add(&mut grove.pages, &prefix, b"axis", Some(host));
        entered.push((position, prefix, added.expect("entered")));
    }
    let children = grove.children(id(1));
    assert_eq!(children.len(), 3);
    for (child, (position, prefix, id)) in children.iter().zip(&entered) {
        assert_eq!(
            (child.position, child.prefix, child.id),
            (*position, *prefix, *id)
        );
        assert_eq!(child.key, b"indexed");
        let entry = grove
            .directory
            .open(&grove.pages, prefix)
            .expect("a lookup");
        let entry = entry.expect("entered");
        assert_eq!(entry.host().map(|host| host.position), Some(*position));
    }
    assert_eq!(grove.check().live, 4);
}

/// The Merk with a node beside a prefix is the nearest in the order of the
/// prefixes whose entry is in use and names a root: a Merk with no node,
/// and what a drop left, are passed.
#[test]
fn the_merk_with_a_node_beside_a_prefix_is_the_nearest_in_use() {
    let (mut grove, paths) = wide(5, 2);
    // Every third Merk of the second level gets a root. The root of the
    // grove and the Merks of the first level have one, as hosts.
    for at in paths.iter().filter(|at| at.len() == 2).step_by(3) {
        grove.root(&keys(at));
    }
    // One Merk of the first level is dropped: its entry goes, those of
    // the five below it are left in the directory, and all six are of no
    // state.
    let dropped = &paths[2];
    assert_eq!(dropped.len(), 1);
    grove.unlink(&keys(dropped));

    let rooted: Vec<Prefix> = {
        let mut rooted: Vec<Prefix> = (paths.iter())
            .filter(|at| !at.starts_with(dropped))
            .filter_map(|at| {
                let entry = grove.entry(&keys(at));
                entry.root().map(|_| prefix_of(&path(&keys(at))))
            })
            .collect();
        rooted.sort_unstable();
        rooted
    };
    assert_eq!(rooted.len(), 1 + 4 + 7);
    let left_over = (paths.iter()).filter(|at| at.starts_with(dropped)).count();
    assert_eq!(left_over, 6);

    let beside = |from: &Prefix, back: bool| {
        (grove.directory)
            .rooted_beside(&grove.pages, from, None, back)
            .expect("a walk")
    };
    // From every prefix of the grove, entered or dropped, with a root or
    // with none, and from two that are not in the directory.
    let entered: Vec<Prefix> = paths.iter().map(|at| prefix_of(&path(&keys(at)))).collect();
    for from in entered.iter().chain(&[[0x55; 32], [0xff; 32]]) {
        let after = rooted.iter().find(|prefix| *prefix > from).copied();
        let before = rooted.iter().rev().find(|prefix| *prefix < from).copied();
        assert_eq!(beside(from, false), after, "after {from:?}");
        assert_eq!(beside(from, true), before, "before {from:?}");
    }
    assert_eq!(beside(&rooted[rooted.len() - 1], false), None);
    assert_eq!(beside(&rooted[0], true), None);

    // The walk reads the entries it passes and nothing else: from the last
    // Merk with a node, its own entry and the entries behind it. A seek, a
    // step and the read of an entry are each one way down the keyspace at
    // most.
    let last = rooted[rooted.len() - 1];
    let behind = entered.iter().filter(|prefix| **prefix > last).count() as u64;
    let levels = grove.pages.levels(&grove.directory);
    let (found, read, _) = walked(&grove, &last, None, false);
    assert_eq!(found, None);
    assert!(read <= levels * 2 * (1 + behind), "{read}, {behind} behind");
    let (all, whole) = grove.pages.reads(|| {
        let mut walk = grove.directory.entries();
        std::iter::from_fn(|| walk.next(&grove.pages).expect("an entry")).count()
    });
    // The entry of the dropped Merk went with the drop.
    assert_eq!(all, entered.len() - 1);
    assert!(read * 2 < whole.0, "{read} of {whole:?}");
}

/// What `rooted_beside` answers, the pages it read for it by itself, and
/// the pages that the walk by steps read, which the answer is held to in
/// this build and which is made within the call.
fn walked(
    grove: &Grove,
    from: &Prefix,
    until: Option<&Prefix>,
    back: bool,
) -> (Option<Prefix>, u64, u64) {
    let directory = &grove.directory;
    let (by_steps, steps) = (grove.pages).reads(|| {
        let walk = directory.rooted_beside_by_steps(&grove.pages, from, until, back);
        walk.expect("a walk")
    });
    let (found, both) = (grove.pages).reads(|| {
        let walk = directory.rooted_beside(&grove.pages, from, until, back);
        walk.expect("a walk")
    });
    assert_eq!(found, by_steps);
    assert_eq!((steps.1, both.1), (0, 0), "no entry here is out of line");
    (found, both.0 - steps.0, steps.0)
}

/// How many leaves of the directory's keyspace hold entries.
fn leaves_of_entries(grove: &Grove) -> u64 {
    fn below(grove: &Grove, page: PageId) -> u64 {
        match FlatPage::decode(&grove.pages.pages[&page], V1).expect("a page") {
            FlatPage::Leaf(leaf) => {
                let holds = |slot| leaf.key(slot).map(|key| key.to_vec()[0]);
                let entry = Some(RecordKind::Entry.code());
                let last = leaf.len().saturating_sub(1);
                u64::from(holds(0) <= entry && entry <= holds(last))
            }
            FlatPage::Branch(branch) => (0..=branch.len())
                .map(|index| below(grove, branch.child(index).expect("a child")))
                .sum(),
        }
    }
    below(grove, grove.directory.root_page())
}

/// The walk past Merks with no node holds a leaf across its entries: past
/// 1,799 of them it asks for a few pages for each leaf of entries, where
/// the walk by steps asks for two pages for each entry. And it answers
/// what the model says from every seventh entry and from the three that
/// have a node, both ways, with a prefix to end at and with none.
#[test]
fn the_walk_past_merks_with_no_node_asks_for_pages_by_the_leaf_not_by_the_entry() {
    let (mut grove, paths) = wide(2_000, 1);
    let mut entered: Vec<(Prefix, &Vec<String>)> = (paths[1..].iter())
        .map(|at| (prefix_of(&path(&keys(at))), at))
        .collect();
    entered.sort();
    // The root of the grove has a node, as a host, and of the Merks below
    // it two that have 1,799 between them.
    let (near, far) = (entered[100], entered[1_900]);
    grove.root(&keys(near.1));
    grove.root(&keys(far.1));
    let (near, far) = (near.0, far.0);
    let first = prefix_of(&[]);
    assert!(first < entered[0].0);

    let levels = grove.pages.levels(&grove.directory);
    let leaves = leaves_of_entries(&grove);
    assert!(
        levels >= 2 && leaves >= 8,
        "{levels} levels, {leaves} leaves"
    );
    for (from, back, to) in [(near, false, far), (far, true, near)] {
        let (found, read, by_steps) = walked(&grove, &from, None, back);
        assert_eq!(found, Some(to));
        assert!(by_steps >= 2 * 1_799, "{by_steps} by steps");
        assert!(
            read <= 2 * levels * (leaves + 1),
            "{read} pages, {leaves} leaves of {levels} levels"
        );
        assert!(read * 20 < by_steps, "{read} against {by_steps}");
    }

    // From every seventh entry, and from the three with a node.
    let from = (entered.iter().step_by(7).map(|(prefix, _)| *prefix)).chain([first, near, far]);
    let rooted = [first, near, far];
    for from in from {
        let after = rooted.iter().find(|prefix| **prefix > from).copied();
        let before = rooted.iter().rev().find(|prefix| **prefix < from).copied();
        assert_eq!(walked(&grove, &from, None, false).0, after);
        assert_eq!(walked(&grove, &from, None, true).0, before);
        // A prefix to end at is not reached: the Merk there is not
        // answered, and one before it is.
        for until in [&entered[1_000].0, &near, &far] {
            let back = *until < from;
            let found = if back { before } else { after };
            let within = found.filter(|found| match back {
                false => found < until,
                true => found > until,
            });
            if *until != from {
                assert_eq!(walked(&grove, &from, Some(until), back).0, within);
            }
        }
    }
}
