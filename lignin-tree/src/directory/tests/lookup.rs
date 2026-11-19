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
