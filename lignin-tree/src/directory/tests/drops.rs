//! Dropping and clearing a Merk, and what is left until the reclaimer has
//! come: the directory's part of T-CL1 (`transactions.md` 15.2).

use super::*;

/// `a` with `a/b`, `a/b/c` and `a/d` below it, and `e` beside it, each
/// with a root.
fn nested() -> Grove {
    let mut grove = Grove::new();
    for keys in [
        &["a"][..],
        &["a", "b"],
        &["a", "b", "c"],
        &["a", "d"],
        &["e"],
    ] {
        grove.add(keys);
        grove.root(keys);
    }
    grove
}

const BELOW_A: [&[&str]; 3] = [&["a", "b"], &["a", "b", "c"], &["a", "d"]];

#[test]
fn a_drop_writes_one_record_and_nothing_of_the_merks_below() {
    let mut grove = nested();
    let a = grove.entry(&["a"]);
    let a_root = a.root().expect("a root").address;
    let below: Vec<Entry> = BELOW_A.iter().map(|keys| grove.entry(keys)).collect();
    let before =
        [RecordKind::Entry, RecordKind::Child, RecordKind::Root].map(|kind| grove.keys(kind).len());
    assert_eq!(before, [6, 5, 6]);

    let dropped = grove.unlink(&["a"]);
    assert_eq!((dropped.id, dropped.root), (a.id(), Some(a_root)));
    assert_eq!(grove.drops(), [dropped]);
    // Its entry, the record of its host and the record of its root are
    // gone. Those of the Merks below are as they were, byte for byte.
    let after =
        [RecordKind::Entry, RecordKind::Child, RecordKind::Root].map(|kind| grove.keys(kind).len());
    assert_eq!(after, [5, 4, 5]);
    assert!(grove.stored(&["a"]).is_none());
    for (keys, entry) in BELOW_A.iter().zip(&below) {
        assert_eq!(grove.stored(keys).as_ref(), Some(entry));
        assert!(!grove.directory.is_live(entry));
        // No open returns one of them, by this handle or by one that is
        // loaded from the pages.
        assert!(grove.open(keys).is_none());
        let loaded = Directory::load(
            &grove.pages,
            grove.directory.root_page(),
            grove.pages.config,
        );
        let prefix = prefix_of(&path(keys));
        let found = loaded.expect("a directory").open(&grove.pages, &prefix);
        assert!(found.expect("a lookup").is_none());
    }
    assert!(grove.open(&["e"]).is_some() && grove.open(&[]).is_some());
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (2, 3, 1));
    // The Merks that were below it are found under its id.
    let children = grove.children(dropped.id);
    let keys: Vec<&[u8]> = children.iter().map(|child| child.key.as_slice()).collect();
    assert_eq!(keys, [b"b", b"d"]);
    assert_eq!(grove.children(children[0].id).len(), 1);
}

#[test]
fn a_drop_costs_the_same_whatever_is_below() {
    // What a drop reads and writes does not grow with the Merks below.
    let cost = |below: usize| {
        let mut grove = Grove::new();
        // Enough Merks beside it for the keyspace to have branches.
        for index in 0..300 {
            grove.add(&[&format!("beside-{index}")]);
        }
        grove.add(&["a"]);
        grove.root(&["a"]);
        for index in 0..below {
            grove.add(&["a", &format!("below-{index}")]);
        }
        let levels = grove.pages.levels(&grove.directory);
        assert!(levels >= 2);
        let (revision, (read, _)) = (grove.pages.revision, grove.pages.read.get());
        grove.unlink(&["a"]);
        let written = grove.pages.revision - revision;
        let read = grove.pages.read.get().0 - read;
        assert_eq!(grove.check().left_over, below as u64);
        (read / levels, written)
    };
    // Four records, three taken out and one put in, and a few lookups.
    for below in [0, 2_000] {
        let (lookups, written) = cost(below);
        assert!(
            lookups <= 12 && written <= 6,
            "{lookups} lookups, {written} pages"
        );
    }
}

#[test]
fn a_snapshot_from_before_the_drop_opens_the_whole_subtree() {
    let mut grove = nested();
    // The pages of the version before the drop, and a handle of them.
    let snapshot = grove.pages.clone();
    let before = grove.directory.clone();
    let entries: Vec<Entry> = BELOW_A.iter().map(|keys| grove.entry(keys)).collect();
    let dropped = grove.unlink(&["a"]);

    let open = |directory: &Directory, pages: &Pages, keys: &[&str]| {
        let prefix = prefix_of(&path(keys));
        directory.open(pages, &prefix).expect("a lookup")
    };
    for (keys, entry) in BELOW_A.iter().zip(&entries) {
        assert_eq!(open(&before, &snapshot, keys).as_ref(), Some(entry));
        let loaded = Directory::load(&snapshot, before.root_page(), snapshot.config);
        let loaded = loaded.expect("a directory");
        assert_eq!(open(&loaded, &snapshot, keys).as_ref(), Some(entry));
    }
    assert!(open(&before, &snapshot, &["a"]).is_some());
    assert_eq!(before.check(&snapshot).expect("a directory").left_over, 0);

    // It still does after the reclaimer has taken everything out of the
    // pages of the later version.
    assert_eq!(grove.reclaim(dropped.id), 3);
    for (keys, entry) in BELOW_A.iter().zip(&entries) {
        assert_eq!(open(&before, &snapshot, keys).as_ref(), Some(entry));
        assert!(grove.stored(keys).is_none());
    }
}

#[test]
fn what_a_drop_left_is_reclaimed_the_innermost_first() {
    let mut grove = nested();
    let dropped = grove.unlink(&["a"]);
    let b = grove.children(dropped.id).remove(0);
    let c = grove.children(b.id).remove(0);
    let d = grove.children(dropped.id).remove(1);
    assert_eq!(
        (b.key.as_slice(), c.key.as_slice(), d.key.as_slice()),
        (&b"b"[..], &b"c"[..], &b"d"[..])
    );

    // `b` has `c` below it, and the dropped Merk has both below it.
    let error = grove
        .directory
        .left_by(&grove.pages, dropped.id, &b)
        .expect_err("not yet");
    assert!(
        matches!(error, DirectoryError::Nested { id } if id == b.id),
        "{error}"
    );
    let error = grove
        .directory
        .reclaimed(&mut grove.pages, dropped.id)
        .expect_err("not yet");
    assert!(
        matches!(error, DirectoryError::Nested { id } if id == dropped.id),
        "{error}"
    );

    // What `c` left: the record of its root, its entry, the record of its
    // host, in that order.
    let left = grove
        .directory
        .left_by(&grove.pages, b.id, &c)
        .expect("its records");
    let kinds: Vec<_> = left.iter().map(|key| RecordKind::of(key)).collect();
    assert_eq!(
        kinds,
        [RecordKind::Root, RecordKind::Entry, RecordKind::Child].map(Some)
    );
    let mut steps = 0;
    for (parent, child) in [(b.id, &c), (dropped.id, &b), (dropped.id, &d)] {
        // Each record is taken out by itself, as a page step would, and
        // the directory is right after each.
        for key in grove
            .directory
            .left_by(&grove.pages, parent, child)
            .expect("records")
        {
            let tree = *grove.directory.tree();
            assert!(tree.delete(&mut grove.pages, &key).expect("taken out"));
            grove.check();
            steps += 1;
            for keys in BELOW_A {
                assert!(grove.open(keys).is_none());
            }
        }
    }
    assert_eq!(steps, 9);
    let reclaimed = grove.directory.reclaimed(&mut grove.pages, dropped.id);
    reclaimed.expect("all of it is reclaimed");
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (2, 0, 0));
    let left = [
        RecordKind::Entry,
        RecordKind::Child,
        RecordKind::Root,
        RecordKind::Dropped,
    ]
    .map(|kind| grove.keys(kind).len());
    assert_eq!(left, [2, 1, 2, 0]);
    for keys in BELOW_A {
        assert!(grove.open(keys).is_none() && grove.stored(keys).is_none());
    }

    let error = grove
        .directory
        .reclaimed(&mut grove.pages, dropped.id)
        .expect_err("gone");
    assert!(
        matches!(error, DirectoryError::NotDropped { id } if id == dropped.id),
        "{error}"
    );
    // A handle that still has the id of what is reclaimed reads the same.
    let root = grove.directory.root_page();
    let mut stale = Directory::load(&grove.pages, root, grove.pages.config).expect("a directory");
    let e = prefix_of(&path(&["e"]));
    let again = stale.unlink(&mut grove.pages.clone(), &e).expect("dropped");
    assert!(stale
        .open(&grove.pages, &prefix_of(&[]))
        .expect("a lookup")
        .is_some());
    assert_eq!(
        again.root,
        grove.entry(&["e"]).root().map(|root| root.address)
    );
}

#[test]
fn a_subtree_entered_again_under_a_dropped_prefix_starts_empty() {
    let mut grove = nested();
    let old: Vec<MerkId> = BELOW_A.iter().map(|keys| grove.entry(keys).id()).collect();
    let old_b_root = grove.entry(&["a", "b"]).root().expect("a root").address;
    let dropped = grove.unlink(&["a"]);

    // The same path again, before anything is reclaimed.
    let a = grove.add(&["a"]);
    assert!(a > dropped.id);
    let entry = grove.entry(&["a"]);
    assert_eq!((entry.id(), entry.root()), (a, None));
    assert!(grove.children(a).is_empty());
    // What is below the dropped Merk does not come back with it.
    for keys in BELOW_A {
        assert!(grove.open(keys).is_none() && grove.stored(keys).is_some());
    }
    // A Merk below it takes the place of the entry that was left, and has
    // nothing of it.
    let b = grove.add(&["a", "b"]);
    let entry = grove.entry(&["a", "b"]);
    assert_eq!((entry.id(), entry.root()), (b, None));
    assert!(!old.contains(&b));
    assert_eq!(entry.lineage().collect::<Vec<_>>(), [id(1), a]);
    assert_eq!(
        grove
            .directory
            .root_owner(&grove.pages, old_b_root)
            .expect("a lookup"),
        None
    );
    assert!(grove.children(b).is_empty());
    // The Merk that was below the old `a/b` stays out of every lookup,
    // though a Merk is entered again at every prefix above it.
    assert!(grove.open(&["a", "b", "c"]).is_none());
    assert!(grove.open(&["a", "d"]).is_none());
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (4, 2, 1));

    // The reclaimer takes what the old Merks left and nothing of the new.
    let old_b = grove.children(dropped.id).remove(0);
    assert_eq!(old_b.id, old[0]);
    let old_c = grove.children(old_b.id).remove(0);
    grove
        .directory
        .reclaim_child(&mut grove.pages, old_b.id, &old_c)
        .expect("reclaimed");
    let left = grove
        .directory
        .left_by(&grove.pages, dropped.id, &old_b)
        .expect("its records");
    assert_eq!(left.len(), 1, "its entry is another Merk's now");
    assert_eq!(RecordKind::of(&left[0]), Some(RecordKind::Child));
    assert_eq!(grove.reclaim(dropped.id), 2);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (4, 0, 0));
    assert_eq!(grove.entry(&["a", "b"]).id(), b);
    grove.add(&["a", "b", "c"]);
    assert!(grove.open(&["a", "b", "c"]).is_some());
    assert_eq!(grove.check().live, 5);
}

#[test]
fn a_clear_keeps_the_merk_and_drops_what_it_held() {
    let mut grove = nested();
    let before = grove.entry(&["a"]);
    let root = before.root().expect("a root").address;
    let prefix = prefix_of(&path(&["a"]));
    let directory = &mut grove.directory;
    let dropped = directory.clear(&mut grove.pages, &prefix).expect("cleared");
    let dropped = dropped.expect("it had nodes");
    assert_eq!((dropped.id, dropped.root), (before.id(), Some(root)));

    // The Merk is entered as one with no node, under another id, with the
    // host and the type it had.
    let after = grove.entry(&["a"]);
    assert!(after.id() > before.id());
    assert_eq!((after.root(), after.host()), (None, before.host()));
    assert_eq!(after.tree_type(), before.tree_type());
    assert_eq!(after.lineage().collect::<Vec<_>>(), [id(1)]);
    assert_eq!(
        grove
            .directory
            .root_owner(&grove.pages, root)
            .expect("a lookup"),
        None
    );
    let hosted = grove.children(id(1));
    assert_eq!(hosted[0].id, after.id());
    for keys in BELOW_A {
        assert!(grove.open(keys).is_none() && grove.stored(keys).is_some());
    }
    assert!(grove.children(after.id()).is_empty());
    assert_eq!(grove.children(dropped.id).len(), 2);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (3, 3, 1));

    // What is entered below it afterwards is in use.
    grove.add(&["a", "b"]);
    assert_eq!(grove.entry(&["a", "b"]).parent_id(), Some(after.id()));
    assert!(grove.open(&["a", "b", "c"]).is_none());
    assert_eq!(grove.reclaim(dropped.id), 3);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (4, 0, 0));

    // A Merk with no node has nothing to clear.
    let prefix = prefix_of(&path(&["a", "b"]));
    let revision = grove.pages.revision;
    let directory = &mut grove.directory;
    assert_eq!(
        directory
            .clear(&mut grove.pages, &prefix)
            .expect("nothing to clear"),
        None
    );
    assert_eq!(grove.pages.revision, revision);
    // A Merk with no host is cleared as any other.
    let dropped = directory
        .clear(&mut grove.pages, &prefix_of(&[]))
        .expect("cleared");
    assert_eq!(dropped.map(|dropped| dropped.id), Some(id(1)));
    assert!(grove.open(&["a"]).is_none() && grove.open(&["e"]).is_none());
    assert_eq!(grove.entry(&[]).root(), None);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (1, 3, 1));
}

#[test]
fn a_merk_with_no_node_leaves_a_record_and_no_id_to_look_for() {
    let mut grove = Grove::new();
    grove.add(&["a"]);
    let entry = grove.entry(&["a"]);
    let dropped = grove.unlink(&["a"]);
    assert_eq!((dropped.id, dropped.root), (entry.id(), None));
    assert_eq!(grove.drops(), [dropped]);
    assert_eq!(grove.keys(RecordKind::Child).len(), 0);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (1, 0, 1));
    // Nothing was below it, so no handle holds its id.
    let loaded = Directory::load(
        &grove.pages,
        grove.directory.root_page(),
        grove.pages.config,
    );
    loaded
        .expect("a directory")
        .check(&grove.pages)
        .expect("the same directory");
    assert_eq!(grove.reclaim(dropped.id), 0);
    assert_eq!(grove.check().dropped, 0);
}

#[test]
fn a_drop_below_a_drop_is_reclaimed_by_itself() {
    let mut grove = nested();
    let b = grove.unlink(&["a", "b"]);
    let a = grove.unlink(&["a"]);
    assert_eq!(grove.drops().len(), 2);
    // `a/b` was taken out of its host when it was dropped: it is not
    // below `a` any more, and what was below it is found under its own id.
    let below_a: Vec<Vec<u8>> = grove
        .children(a.id)
        .into_iter()
        .map(|child| child.key)
        .collect();
    assert_eq!(below_a, [b"d"]);
    assert_eq!(grove.children(b.id).len(), 1);
    assert!(grove.open(&["a", "b", "c"]).is_none());
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (2, 2, 2));
    assert_eq!(grove.reclaim(a.id), 1);
    assert!(grove.open(&["a", "b", "c"]).is_none());
    assert_eq!(grove.reclaim(b.id), 1);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (2, 0, 0));
}

#[test]
fn a_drop_or_a_reclaim_that_cannot_be_is_refused_and_writes_nothing() {
    let mut grove = nested();
    let pages = grove.pages.pages.clone();
    let directory = &mut grove.directory;
    let nothing = prefix_of(&path(&["nothing"]));
    for missing in [
        directory.unlink(&mut grove.pages, &nothing).map(|_| ()),
        directory.clear(&mut grove.pages, &nothing).map(|_| ()),
    ] {
        let error = missing.expect_err("nothing there");
        assert!(matches!(error, DirectoryError::NotFound { prefix } if prefix == nothing));
    }

    // A Merk in use is not what a dropped Merk left.
    let a = grove.entry(&["a"]);
    let d = grove.children(a.id()).remove(1);
    let error = grove
        .directory
        .left_by(&grove.pages, a.id(), &d)
        .expect_err("in use");
    let d_prefix = prefix_of(&path(&["a", "d"]));
    assert!(
        matches!(error, DirectoryError::InUse { prefix } if prefix == d_prefix),
        "{error}"
    );
    let directory = &mut grove.directory;
    let error = directory
        .reclaim_child(&mut grove.pages, a.id(), &d)
        .expect_err("in use");
    assert!(matches!(error, DirectoryError::InUse { .. }), "{error}");
    let error = directory
        .reclaimed(&mut grove.pages, a.id())
        .expect_err("not dropped");
    assert!(
        matches!(error, DirectoryError::NotDropped { id } if id == a.id()),
        "{error}"
    );
    assert_eq!(grove.pages.pages, pages);

    // A Merk that lost its last node while a Merk is still entered below
    // it cannot be dropped: nothing would find that Merk again.
    let a_prefix = prefix_of(&path(&["a"]));
    grove
        .directory
        .set_roots(&mut grove.pages, &[named(&a_prefix, None)])
        .expect("no root");
    let pages = grove.pages.pages.clone();
    let directory = &mut grove.directory;
    let error = directory
        .unlink(&mut grove.pages, &a_prefix)
        .expect_err("Merks below it");
    assert!(
        matches!(error, DirectoryError::NotEmpty { prefix } if prefix == a_prefix),
        "{error}"
    );
    assert_eq!(grove.pages.pages, pages);
    // And a left-over Merk is dropped by nobody.
    grove.root(&["a"]);
    grove.unlink(&["a"]);
    let left = prefix_of(&path(&["a", "b"]));
    let directory = &mut grove.directory;
    let error = directory
        .unlink(&mut grove.pages, &left)
        .expect_err("left over");
    assert!(matches!(error, DirectoryError::NotFound { .. }), "{error}");
    let error = directory
        .set_tree_type(&mut grove.pages, &left, b"t")
        .expect_err("left over");
    assert!(matches!(error, DirectoryError::NotFound { .. }), "{error}");
}

#[test]
fn a_merk_takes_the_place_of_an_entry_whose_root_record_the_reclaimer_took() {
    let mut grove = nested();
    let dropped = grove.unlink(&["a"]);
    let b = grove.children(dropped.id).remove(0);
    let c = grove.children(b.id).remove(0);
    // The reclaimer has taken the record of the root of `a/b/c`, and not
    // yet its entry.
    let left = grove.directory.left_by(&grove.pages, b.id, &c);
    let left = left.expect("its records");
    let tree = *grove.directory.tree();
    assert!(tree.delete(&mut grove.pages, &left[0]).expect("taken out"));
    grove.check();
    for keys in [&["a"][..], &["a", "b"], &["a", "b", "c"]] {
        grove.add(keys);
    }
    assert_eq!(grove.entry(&["a", "b", "c"]).root(), None);
    // It goes on with what is left: the record of the host.
    let left = grove.directory.left_by(&grove.pages, b.id, &c);
    assert_eq!(left.expect("its records").len(), 1);
    assert_eq!(grove.reclaim(dropped.id), 3);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (5, 0, 0));
}

/// The pages of a reader under which a page step is made: after a number
/// of page reads, the pages are those of `after`.
struct Stepped<'a> {
    before: &'a Pages,
    after: &'a Pages,
    reads_before: u64,
    reads: std::cell::Cell<u64>,
}

impl FlatPages for Stepped<'_> {
    type Page<'a>
        = &'a [u8]
    where
        Self: 'a;

    fn page(&self, page: PageId) -> Result<&[u8], FlatError> {
        self.reads.set(self.reads.get() + 1);
        match self.reads.get() > self.reads_before {
            true => self.after.page(page),
            false => self.before.page(page),
        }
    }

    fn read_value(
        &self,
        held: &&[u8],
        page: PageId,
        slot: usize,
        place: lignin_format::value_log::ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        self.after.read_value(held, page, slot, place, out)
    }

    fn revision(&self) -> crate::flat::Revision {
        self.before.revision()
    }
}

#[test]
fn a_walk_passes_a_record_that_a_page_step_takes_out_under_it() {
    let mut grove = nested();
    let dropped = grove.unlink(&["a"]);
    let all: Vec<Vec<u8>> = grove
        .children(dropped.id)
        .into_iter()
        .map(|child| child.key)
        .collect();
    assert_eq!(all, [b"b", b"d"]);
    // The reclaimer takes the record of `a/d` out of its leaf in a page
    // step, at any moment of a reader's walk over the two records.
    let before = grove.pages.clone();
    let d = grove.children(dropped.id).remove(1);
    grove
        .directory
        .reclaim_child(&mut grove.pages, dropped.id, &d)
        .expect("reclaimed");
    let mut walks = std::collections::BTreeSet::new();
    for reads_before in 0..12 {
        let pages = Stepped {
            before: &before,
            after: &grove.pages,
            reads_before,
            reads: std::cell::Cell::new(0),
        };
        let mut walk = grove.directory.children(dropped.id);
        let mut keys = Vec::new();
        while let Some(child) = walk.next(&pages).expect("a child record") {
            keys.push(child.key);
        }
        // The walk gives the record or passes it, and gives nothing else.
        assert!(keys == all || keys == all[..1], "{keys:?}");
        walks.insert(keys.len());
    }
    assert_eq!(walks.len(), 2);
}
