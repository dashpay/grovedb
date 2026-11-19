//! The lazy range delete of a flat keyspace, and the freeing of the pages
//! it left.

use std::collections::BTreeMap;

use lignin_format::{value_log::ValuePointer, PageId};
use lignin_store::alloc::PageState;

use super::*;
use crate::flat::{FlatTree, Unlinked};

type Model = BTreeMap<Vec<u8>, Vec<u8>>;

fn key(n: usize) -> Vec<u8> {
    format!("key/{n:07}").into_bytes()
}

/// A value that stays in its page, and now and then one that does not.
fn value(n: usize) -> Vec<u8> {
    match n % 23 {
        0 => bytes(n as u64, 1_400),
        _ => bytes(n as u64, 30 + n % 40),
    }
}

/// A store with one keyspace of `count` records and the keyspace of the
/// lazy range deletes, written back.
struct Built {
    db: Db,
    tree: FlatTree,
    drops: FlatDrops,
    model: Model,
}

fn build(count: usize) -> Built {
    let db = Db::new();
    let mut txn = db.begin();
    let tree = FlatTree::create(&mut txn.flat, db.config).expect("a tree");
    let drops = FlatDrops::create(&mut txn.flat, db.config).expect("the records");
    assert_eq!(FlatDrops::open(drops.root_page(), db.config), drops);
    assert_eq!(drops.tree().root(), drops.root_page());
    db.commit(txn);
    let mut model = Model::new();
    for chunk in 0..count.div_ceil(500) {
        let mut txn = db.begin();
        for n in chunk * 500..count.min(chunk * 500 + 500) {
            tree.put(&mut txn.flat, &key(n), &value(n)).expect("a put");
            model.insert(key(n), value(n));
        }
        db.commit(txn);
    }
    db.write_back();
    Built {
        db,
        tree,
        drops,
        model,
    }
}

/// The value pointers that the entries of every keyspace of these tests
/// hold at the latest version.
fn held(db: &Db, trees: &[&FlatTree]) -> Vec<ValuePointer> {
    let pointers = trees.iter().flat_map(|tree| scan_flat(db, tree).pointers);
    pointers.collect()
}

/// Frees what every lazy range delete of the latest version left, `steps`
/// page steps at a time, and checks that no record is left.
fn free_all(db: &Db, drops: &FlatDrops, steps: usize) -> FlatReclaimed {
    let done = free_unlinked_by(db, drops, steps);
    let reclaimer = db.reclaimer();
    assert!(reclaimer
        .flat_drops(drops, &GUARD)
        .expect("the records")
        .is_empty());
    done
}

/// A lazy range delete copies the leaf at each end of its range and the
/// branches above them, whatever lies between, and the keyspace reads as
/// after a range delete.
#[test]
fn unlinking_a_range_writes_its_two_ends_and_nothing_between() {
    let mut copied = Vec::new();
    for count in [4_000, 40_000] {
        let Built {
            db,
            tree,
            drops,
            mut model,
        } = build(count);
        let before = scan_flat(&db, &tree).pages;
        let (start, end) = (key(count / 10), key(count - count / 10));

        // A range delete that reads what it frees copies every page of it.
        let mut eager = db.begin();
        tree.delete_range(&mut eager.flat, &start, Some(&end))
            .expect("a range delete");
        let eager_pages = eager.flat.written().count() + eager.flat.freed().count();
        db.roll_back(eager);

        let mut txn = db.begin();
        let at = txn.tree.at;
        let record = drops
            .unlink_range(&mut txn.flat, &tree, &start, Some(&end), at)
            .expect("unlinked");
        let lazy_pages = txn.flat.written().count() + txn.flat.freed().count();
        db.commit(txn);
        assert_eq!(record, Some(1));
        assert!(eager_pages > 10 * lazy_pages || count < 10_000);
        copied.push(lazy_pages);

        model.retain(|key, _| *key < start || *key >= end);
        let expected: Vec<_> = model.clone().into_iter().collect();
        assert!(flat_records(&db, &tree) == expected);
        // The pages between are out of the tree, and still pages.
        let after = scan_flat(&db, &tree).pages;
        let left: Vec<PageId> = before.difference(&after).copied().collect();
        assert!(left.len() > count / 200);
        let still: Vec<_> = left
            .iter()
            .filter(|page| db.current(**page).is_some())
            .collect();
        assert!(still.len() > left.len() * 8 / 10);

        db.tick();
        let done = free_all(&db, &drops, usize::MAX);
        assert_eq!(done.records, 1);
        assert_eq!(done.pages_freed as usize, still.len());
        assert!(done.values > 0 && done.value_bytes == done.values * 1_400);
        for page in left {
            assert!(db.current(page).is_none());
            assert_eq!(db.state(page), PageState::Retired);
        }
        assert!(flat_records(&db, &tree) == expected);
        db.check_counts(&held(&db, &[&tree, drops.tree()]));
    }
    // Ten times the records, and the same pages written, give or take a
    // level of the tree.
    assert!(copied[1] <= copied[0] + 4, "{copied:?}");
}

/// The freeing can stop after any step and begin again from the record: a
/// page that was freed is passed, also when its id names another page by
/// then.
#[test]
fn freeing_begins_again_from_the_record_and_passes_what_is_gone() {
    let Built {
        db,
        tree,
        drops,
        mut model,
    } = build(12_000);
    let mut txn = db.begin();
    let at = txn.tree.at;
    drops
        .unlink_range(&mut txn.flat, &tree, &key(1_000), Some(&key(11_000)), at)
        .expect("unlinked");
    db.commit(txn);
    model.retain(|key, _| *key < self::key(1_000) || *key >= self::key(11_000));
    db.tick();

    let reclaimer = db.reclaimer();
    let [left] = &reclaimer.flat_drops(&drops, &GUARD).expect("the records")[..] else {
        panic!("one range delete was made");
    };
    assert_eq!((left.id, left.at), (1, at));
    let mut done = FlatReclaimed::default();
    let mut other = None;
    let mut other_model = Model::new();
    let mut calls = 0;
    loop {
        // Each call begins at the record, with nothing kept of the last.
        let progress = reclaimer
            .reclaim_flat(&drops, left, &GUARD, 7, &mut done)
            .expect("the steps");
        calls += 1;
        if progress == Progress::Done {
            break;
        }
        assert_eq!(progress, Progress::More);
        assert_eq!(done.pages_freed, calls * 7);
        // The ids that were freed go to the pages of another keyspace,
        // which the freeing must not take for the pages it is after.
        db.tick();
        let reused = db.reclaim_ids();
        assert!(!reused.is_empty());
        let mut txn = db.begin();
        let tree = *other
            .get_or_insert_with(|| FlatTree::create(&mut txn.flat, db.config).expect("a tree"));
        for n in 0..400 {
            let key = key(calls as usize * 1_000 + n);
            tree.put(&mut txn.flat, &key, &value(n)).expect("a put");
            other_model.insert(key, value(n));
        }
        db.commit(txn);
        db.tick();
    }
    assert!(calls > 5);
    let other = other.expect("the other keyspace");
    let expected: Vec<_> = other_model.into_iter().collect();
    assert!(flat_records(&db, &other) == expected);
    let expected: Vec<_> = model.into_iter().collect();
    assert!(flat_records(&db, &tree) == expected);
    db.write_back();
    db.check_counts(&held(&db, &[&tree, &other, drops.tree()]));
    // The record is gone, and a call with it finds nothing to do.
    assert!(reclaimer
        .flat_drops(&drops, &GUARD)
        .expect("the records")
        .is_empty());
    let freed = done.pages_freed;
    let again = reclaimer.reclaim_flat(&drops, left, &GUARD, usize::MAX, &mut done);
    assert_eq!(again.expect("a look"), Progress::Done);
    assert_eq!((done.pages_freed, done.records), (freed, 1));
    // One step freed each page.
    let steps: Vec<PageId> = db.reclaims().into_iter().map(|(page, ..)| page).collect();
    assert_eq!(steps.len() as u64, done.pages_freed + 1);
}

/// Keys that are too long for a page are kept in the trees of buckets.
/// A range that holds them unlinks those trees as it unlinks pages: the
/// tree of a bucket in a leaf at an end of the range, and through its leaf
/// the tree of a bucket between.
#[test]
fn the_trees_of_long_keys_are_unlinked_and_freed_with_their_range() {
    let Built {
        db,
        tree,
        drops,
        mut model,
    } = build(6_000);
    let limit = db.config.key_limit();
    let long = |n: usize, tail: usize| {
        let mut key = key(n);
        key.resize(limit, b'-');
        key.extend_from_slice(format!("{tail:04}").as_bytes());
        key
    };
    let mut txn = db.begin();
    // One bucket just inside the start of the range, in the leaf at that
    // end, and others in the middle of it.
    for n in [1_000, 2_500, 3_500, 4_200] {
        for tail in 0..60 {
            let value = value(n + tail);
            tree.put(&mut txn.flat, &long(n, tail), &value)
                .expect("a put");
            model.insert(long(n, tail), value);
        }
    }
    db.commit(txn);
    db.write_back();
    let before = scan_flat(&db, &tree).pages;

    let (start, end) = (key(1_000), key(5_000));
    let mut txn = db.begin();
    let at = txn.tree.at;
    drops
        .unlink_range(&mut txn.flat, &tree, &start, Some(&end), at)
        .expect("unlinked");
    db.commit(txn);
    model.retain(|key, _| *key < start || *key >= end);
    let expected: Vec<_> = model.into_iter().collect();
    assert!(flat_records(&db, &tree) == expected);

    let [left] = &db
        .reclaimer()
        .flat_drops(&drops, &GUARD)
        .expect("the records")[..]
    else {
        panic!("one range delete was made");
    };
    // The tree of a bucket is in the record as a page with no branch
    // above it.
    assert!(left.pages.iter().any(|page| page.above.is_none()));
    assert!(left.pages.iter().any(|page| page.above.is_some()));

    db.tick();
    let done = free_all(&db, &drops, 3);
    let after = scan_flat(&db, &tree).pages;
    for page in before.difference(&after) {
        assert!(db.current(*page).is_none(), "{page} is left");
    }
    assert!(done.pages_freed > 20);
    assert!(flat_records(&db, &tree) == expected);
    db.check_counts(&held(&db, &[&tree, drops.tree()]));
}

/// One keyspace among several that share a tree, and a whole tree.
#[test]
fn a_keyspace_and_a_whole_tree_are_unlinked() {
    let db = Db::new();
    let mut txn = db.begin();
    let tree = FlatTree::create(&mut txn.flat, db.config).expect("a tree");
    let drops = FlatDrops::create(&mut txn.flat, db.config).expect("the records");
    db.commit(txn);
    let mut model = Model::new();
    for (space, count) in [
        (&b"aux/"[..], 3_000),
        (&[0xFF, 0xFF][..], 3_000),
        (b"meta/", 900),
    ] {
        let mut txn = db.begin();
        for n in 0..count {
            let key = [space, &key(n)[..]].concat();
            tree.put(&mut txn.flat, &key, &value(n)).expect("a put");
            model.insert(key, value(n));
        }
        db.commit(txn);
    }
    db.write_back();

    // The keyspace whose prefix has no key after it: it ends the tree.
    for (prefix, id) in [(&[0xFF, 0xFF][..], 1), (b"aux/", 2)] {
        let mut txn = db.begin();
        let at = txn.tree.at;
        let record = drops
            .unlink_prefix(&mut txn.flat, &tree, prefix, at)
            .expect("unlinked");
        assert_eq!(record, Some(id));
        db.commit(txn);
        model.retain(|key, _| !key.starts_with(prefix));
        let expected: Vec<_> = model.clone().into_iter().collect();
        assert!(flat_records(&db, &tree) == expected);
    }
    assert_eq!(model.len(), 900);
    db.tick();
    assert!(free_all(&db, &drops, usize::MAX).pages_freed > 20);

    // Everything: the root stays, as an empty leaf.
    let mut txn = db.begin();
    let at = txn.tree.at;
    let record = drops
        .unlink_prefix(&mut txn.flat, &tree, b"", at)
        .expect("unlinked");
    assert_eq!(record, Some(3));
    assert_eq!(drops.next_id(&txn.flat).expect("the header"), 4);
    db.commit(txn);
    assert!(flat_records(&db, &tree).is_empty());
    db.tick();
    assert!(free_all(&db, &drops, usize::MAX).pages_freed > 2);
    assert_eq!(scan_flat(&db, &tree).pages.len(), 1);
    db.check_counts(&held(&db, &[&tree, drops.tree()]));

    // A range that takes no page out whole leaves no record.
    let mut txn = db.begin();
    let at = txn.tree.at;
    for n in 0..10 {
        tree.put(&mut txn.flat, &key(n), b"short").expect("a put");
    }
    let record = drops.unlink_range(&mut txn.flat, &tree, &key(2), Some(&key(5)), at);
    assert_eq!(record.expect("unlinked"), None);
    db.commit(txn);
    assert_eq!(flat_records(&db, &tree).len(), 7);
    assert!(db
        .reclaimer()
        .flat_drops(&drops, &GUARD)
        .expect("the records")
        .is_empty());
}

/// Pages that the transaction of the range delete wrote itself are born
/// with its commit: they wait for the next commit, and are freed then.
/// A page that a write transaction has copied waits for that.
#[test]
fn a_page_that_is_newborn_or_copied_waits() {
    let Built {
        db,
        tree,
        drops,
        mut model,
    } = build(3_000);
    let mut txn = db.begin();
    let at = txn.tree.at;
    // The transaction writes into the range, and then unlinks it.
    for n in (500..2_500).step_by(3) {
        tree.put(&mut txn.flat, &key(n), &value(n + 1))
            .expect("a put");
    }
    drops
        .unlink_range(&mut txn.flat, &tree, &key(300), Some(&key(2_700)), at)
        .expect("unlinked");
    let born = db.commit(txn);
    assert_eq!(born, at.next().expect("a version"));
    model.retain(|key, _| *key < self::key(300) || *key >= self::key(2_700));

    let reclaimer = db.reclaimer();
    let [left] = &reclaimer.flat_drops(&drops, &GUARD).expect("the records")[..] else {
        panic!("one range delete was made");
    };
    let mut done = FlatReclaimed::default();
    let waits = reclaimer.reclaim_flat(&drops, left, &GUARD, usize::MAX, &mut done);
    assert_eq!(waits.expect("a look"), Progress::Blocked(Busy::Newborn));
    assert_eq!(done.pages_freed, 0);
    db.tick();

    // A writer holds a copy of one of the pages.
    let held = left.pages[left.pages.len() / 2].page;
    let mut txn = db.begin();
    crate::flat::FlatPagesMut::take(&mut txn.flat, held).expect("a copy");
    let waits = reclaimer.reclaim_flat(&drops, left, &GUARD, usize::MAX, &mut done);
    assert_eq!(waits.expect("the steps"), Progress::Blocked(Busy::Copied));
    assert!(done.pages_freed > 0 && db.current(held).is_some());
    db.roll_back(txn);

    let steps = reclaimer.reclaim_flat(&drops, left, &GUARD, 0, &mut done);
    assert_eq!(steps.expect("no step"), Progress::More);
    let ends = reclaimer.reclaim_flat(&drops, left, &GUARD, usize::MAX, &mut done);
    assert_eq!(ends.expect("the steps"), Progress::Done);
    assert_eq!(done.records, 1);
    let expected: Vec<_> = model.into_iter().collect();
    assert!(flat_records(&db, &tree) == expected);
    db.write_back();
    db.check_counts(&held_all(&db, &tree, &drops));
}

fn held_all(db: &Db, tree: &FlatTree, drops: &FlatDrops) -> Vec<ValuePointer> {
    held(db, &[tree, drops.tree()])
}

/// A record that is not one is refused where it is read, and pages that
/// are not what a range delete leaves are not freed.
#[test]
fn a_record_or_a_page_that_is_not_one_is_refused() {
    let Built {
        db, tree, drops, ..
    } = build(2_000);
    let record = |key: &[u8], value: &[u8]| {
        let mut txn = db.begin();
        let records = drops.tree();
        records.delete_prefix(&mut txn.flat, &[2]).expect("emptied");
        records.put(&mut txn.flat, key, value).expect("a put");
        db.commit(txn);
        db.reclaimer().flat_drops(&drops, &GUARD)
    };
    let id = [2, 0, 0, 0, 0, 0, 0, 0, 9];
    let page = |raw: u32, known: u8, level: u8| {
        let mut value = 1u64.to_le_bytes().to_vec();
        value.extend_from_slice(&raw.to_le_bytes());
        value.extend_from_slice(&[known, level]);
        value
    };
    for (key, value) in [
        (&id[..5], page(7, 0, 0)),
        (&id[..], vec![1, 0, 0]),
        (&id[..], 0u64.to_le_bytes().to_vec()),
        (&id[..], [page(7, 0, 0), vec![1]].concat()),
        (&id[..], 1u64.to_le_bytes().to_vec()),
        (&id[..], page(0, 0, 0)),
        (&id[..], page(7, 0, 3)),
        (&id[..], page(7, 2, 0)),
    ] {
        let refused = record(key, &value);
        assert!(matches!(refused, Err(DropError::Record(_))), "{value:?}");
    }

    // A record that names a page of the tree under a level it is not at,
    // a tree page, and a page twice.
    db.tick();
    let root = tree.root();
    let mut done = FlatReclaimed::default();
    let reclaimer = db.reclaimer();
    let mut refused = |pages: Vec<Unlinked>| {
        let left = FlatDrop {
            id: 9,
            at: db.latest(),
            pages,
        };
        let refused = reclaimer.reclaim_flat(&drops, &left, &GUARD, usize::MAX, &mut done);
        assert!(
            matches!(refused, Err(DropError::Damaged { .. })),
            "{refused:?}"
        );
    };
    refused(vec![Unlinked {
        page: root,
        above: Some(1),
    }]);
    let whole = Unlinked {
        page: root,
        above: None,
    };
    refused(vec![whole, whole]);
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    let node = grove.put(0, b"a node", b"its value");
    grove.flush(&mut txn);
    db.commit(txn);
    db.tick();
    refused(vec![Unlinked {
        page: grove.nodes[node].at.expect("a cell").page(),
        above: None,
    }]);
    assert_eq!(done.pages_freed, 0);
    assert_eq!(flat_records(&db, &tree).len(), 2_000);

    // A keyspace with no header gives out no number.
    let mut txn = db.begin();
    drops.tree().delete(&mut txn.flat, &[1]).expect("a delete");
    assert!(matches!(
        drops.next_id(&txn.flat),
        Err(DropError::Record(_))
    ));
    let at = txn.tree.at;
    let refused = drops.unlink_prefix(&mut txn.flat, &tree, b"", at);
    assert!(matches!(refused, Err(DropError::Record(_))));
    db.roll_back(txn);
    let mut txn = db.begin();
    drops
        .tree()
        .put(&mut txn.flat, &[1], &u64::MAX.to_le_bytes())
        .expect("a put");
    let refused = drops.unlink_prefix(&mut txn.flat, &tree, b"", at);
    assert!(matches!(refused, Err(DropError::IdsExhausted)));
    db.roll_back(txn);
}
