//! The flat records of subtrees: what a drop and an enter do to them, what
//! a reader finds through the directory, and what the reclaimer takes out
//! (`transactions.md` CV9 and T-CL1, for every column).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
};

use lignin_format::{flat_page::FlatPage, PageId};
use lignin_store::load::FRAME_FORMAT;

use super::{
    super::step::{seen, EntryRemoved, Seen, Step},
    *,
};
use crate::{
    directory::{record::entry_key, DirectoryError, Host, Prefix},
    flat::{FlatConfig, FlatPages, FlatPagesMut, FlatTree},
};

type Records = BTreeMap<Vec<u8>, Vec<u8>>;

fn node(n: usize) -> Vec<u8> {
    format!("node-{n:04}").into_bytes()
}

fn record(n: usize) -> Vec<u8> {
    format!("rec/{n:05}").into_bytes()
}

/// A key that a page does not store as it is: it is kept in a bucket, with
/// the other keys that begin as it does.
fn long(db: &Db, n: usize) -> Vec<u8> {
    let mut key = vec![b'L'; db.config.key_limit() + 40];
    key.extend_from_slice(format!("{n:03}").as_bytes());
    key
}

/// A value that stays in its page, and now and then one that does not.
fn held(n: usize, salt: u64) -> Vec<u8> {
    match n % 7 {
        0 => bytes(salt * 100_000 + n as u64, 1_400),
        _ => bytes(salt * 100_000 + n as u64, 20 + n % 30),
    }
}

/// Gives `merk` flat records in every column: `count` in the column that
/// has a keyspace of its own, with three long keys, and a few in each of
/// the two that share one.
fn give(db: &Db, grove: &mut Grove, merk: usize, count: usize, salt: u64) {
    for n in 0..count {
        grove.put_flat(merk, 0, &record(n), &held(n, salt));
    }
    for n in 0..3 {
        grove.put_flat(merk, 0, &long(db, n), &held(n + 1, salt + 50));
    }
    for n in 0..count.min(12) {
        grove.put_flat(merk, 1, &record(n), &held(n + 1, salt + 60));
        grove.put_flat(merk, 2, &record(n), &held(n + 2, salt + 70));
    }
}

/// A grove whose root Merk hosts two Merks: `outer`, which the tests drop,
/// and `beside`, which stays. A node of `outer` hosts `inner`, and a node
/// of `inner` hosts `deepest`. Every Merk has flat records in every
/// column, and those of `inner` lie between those of two Merks that stay
/// or go otherwise.
struct Built {
    db: Db,
    grove: Grove,
    host: usize,
    outer: usize,
    inner: usize,
    deepest: usize,
    beside: usize,
}

fn build(records: usize) -> Built {
    let built = build_in(Db::new(), records);
    built.db.write_back();
    built
}

/// A store in which every value of more than sixteen bytes is out of line:
/// the records of the directory as well.
fn out_of_line() -> Db {
    let mut db = Db::new();
    let limit = FlatConfig::MIN_INLINE;
    db.config = db.config.with_max_inline(limit).expect("a limit");
    db
}

/// The same in `db`, with no value written back.
fn build_in(db: Db, records: usize) -> Built {
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    for n in 0..12 {
        grove.put(0, &node(n), b"a value");
    }
    let host = grove.put(0, b"host", b"a tree");
    let other = grove.put(0, b"beside", b"a tree");
    let outer = grove.host(host);
    let beside = grove.host(other);
    let nodes: Vec<usize> = (0..20)
        .map(|n| grove.put(outer, &node(n), b"in the outer one"))
        .collect();
    let inner = grove.host(nodes[3]);
    let inside: Vec<usize> = (0..8)
        .map(|n| grove.put(inner, &node(n), b"in the inner one"))
        .collect();
    let deepest = grove.host(inside[1]);
    grove.put(deepest, b"deep", b"in the deepest one");
    for n in 0..6 {
        grove.put(beside, &node(n), b"beside");
    }
    grove.flush(&mut txn);
    db.commit(txn);

    for (merk, count, salt) in [
        (0, 20, 1),
        (outer, records, 2),
        (beside, records, 3),
        (inner, records, 4),
        (deepest, 40, 5),
    ] {
        let mut txn = db.begin();
        give(&db, &mut grove, merk, count, salt);
        grove.flush(&mut txn);
        db.commit(txn);
    }
    grove.check(&db);
    Built {
        db,
        grove,
        host,
        outer,
        inner,
        deepest,
        beside,
    }
}

/// The records that the column `column` stores under `prefix`, whatever
/// the directory says of the prefix: what no reader does.
fn raw<P: FlatPages + ?Sized>(pages: &P, grove: &Grove, prefix: &Prefix, column: usize) -> Records {
    let column = &grove.columns[column];
    let mut cursor = column.tree.cursor_within(&column.range(prefix));
    cursor.seek_to_first(pages).expect("a seek");
    let mut found = Records::new();
    while cursor.valid() {
        let mut value = Vec::new();
        assert!(cursor.value(pages, &mut value).expect("a value"));
        found.insert(cursor.key().expect("a key").to_vec(), value);
        cursor.next(pages).expect("a step");
    }
    found
}

/// The leaf of `tree` that the stored key `key` is in, or would be, at the
/// latest version.
fn leaf_with(db: &Db, tree: &FlatTree, key: &[u8]) -> PageId {
    let reader = db.reader();
    let mut page = tree.root();
    loop {
        let frame = reader.page(page).expect("a page");
        match FlatPage::from_validated(&frame, FRAME_FORMAT).expect("a flat page") {
            FlatPage::Branch(branch) => {
                page = branch.child(branch.child_index(key)).expect("a child");
            }
            FlatPage::Leaf(_) => return page,
        }
    }
}

fn flats<'a, 'd>(txn: &'a mut Txn<'d>, grove: &'a Grove) -> Flats<'a, Counted<Writer<'d>>> {
    Flats {
        at: txn.tree.at,
        pages: &mut txn.flat,
        columns: &grove.columns,
        drops: &grove.drops,
    }
}

/// A drop takes the flat records of the dropped Merk out of every column,
/// and reads nothing of the Merks below it. Their records stay in the
/// pages, and no reader is shown them: not in the transaction that drops,
/// and not at any version after it. A snapshot from before reads them all.
/// Reclaiming takes them out, with the values they held, and nothing of a
/// Merk in use.
#[test]
fn a_drop_takes_the_flat_records_out_of_reach_and_reclaiming_takes_them_out() {
    let Built {
        db,
        mut grove,
        outer,
        inner,
        deepest,
        beside,
        ..
    } = build(300);
    let gone = [outer, inner, deepest];
    let prefix = |merk: usize| grove.merks[merk].prefix;
    let prefixes = gone.map(prefix);
    let kept = prefix(beside);
    let had: Vec<[Records; 3]> = gone
        .iter()
        .map(|&merk| [0, 1, 2].map(|column| grove.flat_of(merk, column)))
        .collect();
    // Each has records in a keyspace of several leaves, a bucket of long
    // keys and values out of line.
    assert!(scan_flat(&db, &grove.columns[0].tree).leaves > 12);
    assert!(had
        .iter()
        .all(|of| of.iter().all(|column| !column.is_empty())));
    assert!(had[1][0]
        .keys()
        .any(|key| key.len() > db.config.key_limit()));
    assert!(had[1][0].values().any(|value| value.len() > 1_000));

    // A snapshot from before the drop.
    let before = db.latest();
    db.pins.pinned.store(before.get(), Ordering::SeqCst);
    let directory_before = grove.directory(&db);

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    // In the transaction that drops, no reader finds a record of the
    // three, in any column.
    for of in &prefixes {
        for column in 0..3 {
            let found = grove.read_flat(&txn.flat, &grove.directory, of, column);
            assert_eq!(found, None);
        }
    }
    for column in 0..3 {
        let found = grove.read_flat(&txn.flat, &grove.directory, &kept, column);
        assert_eq!(found, Some(grove.flat_of(beside, column)));
    }
    // The records of the dropped Merk are out of the columns, and those of
    // the Merks below it are where they were: the drop walked nothing.
    for (column, (inner, deepest)) in had[1].iter().zip(&had[2]).enumerate() {
        assert!(raw(&txn.flat, &grove, &prefixes[0], column).is_empty());
        assert_eq!(&raw(&txn.flat, &grove, &prefixes[1], column), inner);
        assert_eq!(&raw(&txn.flat, &grove, &prefixes[2], column), deepest);
    }
    db.commit(txn);

    // The snapshot reads what was there, through its directory.
    let then = db.reader_at(before);
    for (of, had) in prefixes.iter().zip(&had) {
        for (column, had) in had.iter().enumerate() {
            let found = grove.read_flat(&then, &directory_before, of, column);
            assert_eq!(found.as_ref(), Some(had));
        }
    }
    // A reader of the version after the drop finds none.
    let now = db.reader();
    let directory = grove.directory(&db);
    for of in &prefixes {
        for column in 0..3 {
            assert_eq!(grove.read_flat(&now, &directory, of, column), None);
        }
    }
    drop((then, now));
    grove.check(&db);
    db.pins.pinned.store(u64::MAX, Ordering::SeqCst);

    let nested: u64 = had[1..]
        .iter()
        .flat_map(|of| of.iter().map(|column| column.len() as u64))
        .sum();
    let out_of_line: u64 = had[1..]
        .iter()
        .flat_map(|of| of.iter().flat_map(Records::values))
        .filter(|value| value.len() > 1_000)
        .count() as u64;
    let done = reclaim_all(&db, &grove);
    let [done] = &done[..] else {
        panic!("one Merk was dropped");
    };
    assert_eq!(done.flat_records, nested);
    assert!(done.flat_leaves >= 4);
    assert!(done.values >= out_of_line, "{done:?}");
    assert_eq!(done.pending_values, 0);
    check_reclaimed(&db, &grove);
}

/// A Merk that is entered again under the prefix of a dropped Merk, or of
/// a Merk that was below one, starts with no flat record in any column,
/// before anything is reclaimed. The records it is given stay through the
/// reclaiming, also those that have the keys and the values of the old
/// ones.
#[test]
fn a_merk_entered_again_before_the_reclaiming_starts_with_no_flat_record() {
    let Built {
        db,
        mut grove,
        host,
        outer,
        inner,
        deepest,
        beside,
        ..
    } = build(300);
    let outer_prefix = grove.merks[outer].prefix;
    let inner_prefix = grove.merks[inner].prefix;
    let old_inner = grove.flat_of(inner, 0);
    let old_outer = grove.flat_of(outer, 0);
    let deepest_records: u64 = (0..3).map(|c| grove.flat_of(deepest, c).len() as u64).sum();

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);

    // The outer one is entered again where it was, and one under the
    // prefix of the inner one below a Merk that was never dropped.
    let mut txn = db.begin();
    let again = grove.host_again(host, 0, outer_prefix);
    grove.put(again, b"a node", b"of the Merk that is there now");
    let under = grove.merks[beside].root.expect("a root");
    let inner_again = grove.host_under(under, inner_prefix);
    grove.put(inner_again, b"a node", b"of the Merk that is there now");
    grove.flush(&mut txn);
    for of in [&outer_prefix, &inner_prefix] {
        for column in 0..3 {
            let found = grove.read_flat(&txn.flat, &grove.directory, of, column);
            assert_eq!(found, Some(Records::new()));
            // Nothing is under the prefix for anyone: what was left there
            // went with the enter.
            assert!(raw(&txn.flat, &grove, of, column).is_empty());
        }
    }
    for (n, (key, value)) in old_inner.iter().enumerate() {
        match n % 3 {
            0 => grove.put_flat(inner_again, 0, key, value),
            1 => grove.put_flat(inner_again, 0, key, b"another value"),
            _ => {}
        }
    }
    for (key, value) in old_outer.iter().step_by(2) {
        grove.put_flat(again, 0, key, value);
    }
    give(&db, &mut grove, again, 30, 8);
    give(&db, &mut grove, inner_again, 30, 9);
    grove.flush(&mut txn);
    db.commit(txn);
    grove.check(&db);

    // The reclaimer finds another Merk under the inner prefix, and takes
    // out the records of the deepest one alone.
    let done = reclaim_all(&db, &grove);
    assert_eq!(done[0].flat_records, deepest_records);
    check_reclaimed(&db, &grove);
    assert!(grove.flat_of(inner_again, 0).len() > old_inner.len() / 2);
}

/// The same while the reclaimer is at work: a Merk is entered under the
/// prefix of a nested Merk whose flat records the reclaimer has begun to
/// take out, and is given records under the keys that are still there.
/// The reclaimer takes out no record of it.
#[test]
fn a_merk_entered_again_during_the_reclaiming_keeps_its_flat_records() {
    let Built {
        db,
        mut grove,
        outer,
        inner,
        beside,
        ..
    } = build(300);
    let inner_prefix = grove.merks[inner].prefix;
    let old_inner = grove.flat_of(inner, 0);
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);
    db.tick();

    let directory = grove.directory(&db);
    let [dropped] = drops(&db, &directory)[..] else {
        panic!("one Merk was dropped");
    };
    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(dropped);
    // One page step at a time, until some records of the inner Merk are
    // gone and some are still there.
    let left = loop {
        let steps = db.reclaims().len();
        let progress = reclaimer
            .reclaim(&mut reclaim, &directory, &db.columns(), &GUARD, 1)
            .expect("a step");
        assert_eq!(progress, Progress::More);
        // A call makes no more page steps than it was asked for.
        assert!(db.reclaims().len() <= steps + 1);
        grove.check(&db);
        let left = raw(&db.reader(), &grove, &inner_prefix, 0);
        if left.len() < old_inner.len() {
            break left;
        }
    };
    assert!(left.len() > old_inner.len() / 2, "one leaf at a time");

    let mut txn = db.begin();
    let under = grove.merks[beside].root.expect("a root");
    let inner_again = grove.host_under(under, inner_prefix);
    grove.put(inner_again, b"a node", b"of the Merk that is there now");
    grove.flush(&mut txn);
    assert!(raw(&txn.flat, &grove, &inner_prefix, 0).is_empty());
    for (key, value) in &old_inner {
        grove.put_flat(inner_again, 0, key, value);
    }
    grove.flush(&mut txn);
    db.commit(txn);
    grove.check(&db);

    // The reclaimer goes on with the directory it has, which knows the
    // Merk as it was dropped.
    finish(&db, &directory, &mut reclaim, 1);
    assert!(reclaim.miscount().is_none());
    assert_eq!(
        raw(&db.reader(), &grove, &inner_prefix, 0),
        grove.flat_of(inner_again, 0)
    );
    free_unlinked(&db, &db.drops());
    check_reclaimed(&db, &grove);

    // And after the reclaiming, the prefix of the dropped Merk itself.
    let mut txn = db.begin();
    let outer_prefix = grove.merks[outer].prefix;
    let outer_again = grove.host_under(under, outer_prefix);
    grove.put(outer_again, b"a node", b"of the Merk that is there now");
    grove.flush(&mut txn);
    for column in 0..3 {
        let found = grove.read_flat(&txn.flat, &grove.directory, &outer_prefix, column);
        assert_eq!(found, Some(Records::new()));
    }
    give(&db, &mut grove, outer_again, 25, 11);
    grove.flush(&mut txn);
    db.commit(txn);
    assert!(drops(&db, &grove.directory(&db)).is_empty());
    check_reclaimed(&db, &grove);
}

/// A step is made on a leaf only as it was read. A Merk that is entered
/// again under the prefix between the reclaimer's look at the leaves and
/// its steps, with records under the same keys, keeps them all.
///
/// The leaves at the two ends of the range are other versions by then, and
/// no step is made on them. A leaf between them was unlinked whole by the
/// enter, and is as it was: a step takes its entries out of a page that
/// waits to be freed, which no reader of the new Merk reaches, and the
/// freeing releases what is left in it.
#[test]
fn a_leaf_that_a_commit_changed_since_the_look_loses_no_entry() {
    let Built {
        db,
        mut grove,
        outer,
        inner,
        beside,
        ..
    } = build(300);
    let inner_prefix = grove.merks[inner].prefix;
    let old_inner = grove.flat_of(inner, 0);
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);
    db.tick();

    // The reclaimer's look: the leaves that hold the records at the latest
    // version, and the keys in each.
    let column = grove.columns[0];
    let reclaimer = db.reclaimer();
    let mut keys_of: BTreeMap<PageId, Vec<Vec<u8>>> = BTreeMap::new();
    for key in old_inner.keys() {
        if key.len() < db.config.key_limit() / 2 {
            let stored = grove.stored_key(inner, 0, key);
            let leaf = leaf_with(&db, &column.tree, &stored);
            keys_of.entry(leaf).or_default().push(stored);
        }
    }
    assert!(keys_of.len() >= 3, "two ends and a leaf between");
    let looked: Vec<_> = keys_of
        .keys()
        .map(|leaf| reclaimer.at(*leaf, db.latest()).expect("a look"))
        .map(|frame| frame.expect("the leaf"))
        .collect();

    let mut txn = db.begin();
    let under = grove.merks[beside].root.expect("a root");
    let inner_again = grove.host_under(under, inner_prefix);
    grove.put(inner_again, b"a node", b"of the Merk that is there now");
    for (key, value) in &old_inner {
        grove.put_flat(inner_again, 0, key, value);
    }
    grove.flush(&mut txn);
    db.commit(txn);

    let (mut stale, mut unlinked) = (0, 0);
    for (frame, keys) in looked.iter().zip(keys_of.values()) {
        let step = reclaimer.remove_entries(frame, db.config.capacity(), keys);
        match step.expect("a step or none") {
            None => stale += 1,
            Some(Step::Made(removed)) => {
                assert!(removed.miscount.is_none());
                unlinked += 1;
            }
            Some(Step::Refused(busy)) => panic!("nothing holds a page: {busy:?}"),
        }
    }
    drop(looked);
    assert!(stale >= 1 && unlinked >= 1, "{stale} {unlinked}");
    // The Merk that is there now has every record it was given.
    grove.check(&db);
    assert_eq!(
        raw(&db.reader(), &grove, &inner_prefix, 0),
        grove.flat_of(inner_again, 0)
    );
    // And every value is released once: by the step that took its entry
    // out, or with the page.
    reclaim_all(&db, &grove);
    check_reclaimed(&db, &grove);
}

/// A cleared Merk keeps its own flat records, as it keeps its entry. Those
/// of the Merks below it are shown to no reader from the clear on, and
/// reclaiming takes them out.
#[test]
fn a_cleared_merk_keeps_its_flat_records_and_those_below_it_go() {
    let Built {
        db,
        mut grove,
        outer,
        inner,
        deepest,
        ..
    } = build(200);
    let own = [0, 1, 2].map(|column| grove.flat_of(outer, column));
    let outer_prefix = grove.merks[outer].prefix;
    let below = [inner, deepest].map(|merk| grove.merks[merk].prefix);
    let nested: u64 = [inner, deepest]
        .iter()
        .flat_map(|&merk| (0..3).map(move |column| (merk, column)))
        .map(|(merk, column)| grove.flat_of(merk, column).len() as u64)
        .sum();

    let mut txn = db.begin();
    let left = grove.drop_merks(&mut txn, &[outer], true);
    assert_eq!(left.len(), 1);
    for (column, own) in own.iter().enumerate() {
        let found = grove.read_flat(&txn.flat, &grove.directory, &outer_prefix, column);
        assert_eq!(found.as_ref(), Some(own));
        for of in &below {
            let found = grove.read_flat(&txn.flat, &grove.directory, of, column);
            assert_eq!(found, None);
        }
    }
    // The clear wrote no page of a column.
    let columns: BTreeSet<PageId> = db.columns().iter().map(|c| c.tree.root()).collect();
    let mut of_columns = BTreeSet::new();
    for root in &columns {
        of_columns.extend(scan_flat(&db, &FlatTree::open(*root, db.config)).pages);
    }
    assert!(txn.flat.wrote.is_disjoint(&of_columns));
    db.commit(txn);
    grove.check(&db);

    let done = reclaim_all(&db, &grove);
    assert_eq!(done[0].flat_records, nested);
    check_reclaimed(&db, &grove);
    for (column, own) in own.iter().enumerate() {
        assert_eq!(&grove.flat_of(outer, column), own);
    }

    // The cleared Merk is given nodes, records and a Merk below it under
    // the prefix that one had before.
    let mut txn = db.begin();
    let nodes: Vec<usize> = (0..5)
        .map(|n| grove.put(outer, &node(n), b"after the clear"))
        .collect();
    let again = grove.host_under(nodes[2], below[0]);
    grove.put(again, b"a node", b"of the Merk that is there now");
    grove.flush(&mut txn);
    give(&db, &mut grove, again, 20, 12);
    grove.put_flat(outer, 1, b"one more", b"after the clear");
    grove.flush(&mut txn);
    db.commit(txn);
    check_reclaimed(&db, &grove);
}

/// A write transaction holds a copy of a leaf that has flat records of a
/// Merk below the dropped one. No entry of that leaf is taken out until
/// the transaction has committed, and what it wrote is there afterwards.
#[test]
fn the_flat_records_wait_while_a_writer_has_a_copy_of_their_leaf() {
    let Built {
        db,
        mut grove,
        outer,
        inner,
        beside,
        ..
    } = build(200);
    let inner_prefix = grove.merks[inner].prefix;
    let had = grove.flat_of(inner, 0);
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);
    db.tick();
    let directory = grove.directory(&db);
    let [dropped] = drops(&db, &directory)[..] else {
        panic!("one Merk was dropped");
    };

    // The writer changes a record of a Merk in use whose keys come just
    // before those of the inner one, and has the leaf where they begin.
    let column = grove.columns[0];
    let first = grove.stored_key(inner, 0, &record(0));
    let leaf = leaf_with(&db, &column.tree, &first);
    let mut txn = db.begin();
    let image = txn.flat.take(leaf).expect("a copy");
    txn.flat.write(leaf, &image).expect("as it was");
    grove.put_flat(beside, 0, b"one more", b"written beside the reclaimer");
    grove.flush(&mut txn);

    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(dropped);
    for _ in 0..3 {
        let progress =
            reclaimer.reclaim(&mut reclaim, &directory, &db.columns(), &GUARD, usize::MAX);
        assert_eq!(progress.expect("a look"), Progress::Blocked(Busy::Copied));
    }
    // The deepest Merk went before the inner one, and of the inner one no
    // record went: its first leaf is held.
    assert!(reclaim.reclaimed().flat_records > 0);
    assert_eq!(raw(&db.reader(), &grove, &inner_prefix, 0), had);
    db.commit(txn);

    finish(&db, &directory, &mut reclaim, usize::MAX);
    free_unlinked(&db, &db.drops());
    check_reclaimed(&db, &grove);
    assert_eq!(
        grove
            .flat_of(beside, 0)
            .get(&b"one more"[..])
            .map(Vec::as_slice),
        Some(&b"written beside the reclaimer"[..])
    );
}

/// Reclaiming one page step at a time comes to the same end, with a state
/// of whole pages and of whole records between any two steps, and no call
/// makes more steps than it was asked for.
#[test]
fn the_flat_records_go_step_by_step() {
    let Built {
        db,
        mut grove,
        outer,
        ..
    } = build(150);
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);
    db.tick();
    let directory = grove.directory(&db);
    let [dropped] = drops(&db, &directory)[..] else {
        panic!("one Merk was dropped");
    };
    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(dropped);
    let columns = db.columns();
    for budget in [1, 2, 3].into_iter().cycle() {
        let steps = db.reclaims().len();
        let progress = reclaimer
            .reclaim(&mut reclaim, &directory, &columns, &GUARD, budget)
            .expect("a step");
        let made = db.reclaims().len() - steps;
        assert!(made <= budget, "{made} steps of {budget}");
        db.check_pages();
        grove.check(&db);
        match progress {
            Progress::Done => break,
            // A call that says there is more has used what it was given.
            Progress::More => assert_eq!(made, budget),
            Progress::Blocked(busy) => panic!("nothing holds a page: {busy:?}"),
        }
    }
    assert!(reclaim.is_done() && reclaim.miscount().is_none());
    free_unlinked(&db, &db.drops());
    check_reclaimed(&db, &grove);
}

/// `enter` is `Directory::add` with the one thing more that the rule of the
/// flat records asks: a prefix that has a left-over entry loses the
/// records under it. A new prefix costs no page of a column, and a Merk
/// that is refused changes nothing.
#[test]
fn an_enter_unlinks_what_a_left_over_entry_has_and_nothing_else() {
    let Built {
        db,
        mut grove,
        outer,
        inner,
        beside,
        ..
    } = build(120);
    let inner_prefix = grove.merks[inner].prefix;
    let beside_prefix = grove.merks[beside].prefix;
    let parent = grove.merks[0].prefix;
    let host = |key: &'static [u8]| Host {
        parent: &parent,
        key,
        position: 0,
    };
    let of_columns = |db: &Db| {
        let mut pages = BTreeSet::new();
        for column in db.columns() {
            pages.extend(scan_flat(db, &column.tree).pages);
        }
        pages
    };

    // A prefix that no Merk ever had: the directory is written, and no
    // page of a column.
    let mut txn = db.begin();
    let new = [0x42; 32];
    let directory = grove.directory(&db);
    let entered = enter(
        &directory,
        &mut flats(&mut txn, &grove),
        &new,
        b"tree",
        Some(host(b"a")),
    );
    entered.expect("entered");
    assert!(!txn.flat.wrote.is_empty());
    assert!(txn.flat.wrote.is_disjoint(&of_columns(&db)));
    // A prefix that a Merk in use has: refused, and nothing more is
    // written.
    let wrote = txn.flat.wrote.clone();
    let refused = enter(
        &directory,
        &mut flats(&mut txn, &grove),
        &beside_prefix,
        b"tree",
        Some(host(b"b")),
    );
    assert!(matches!(
        refused,
        Err(DropError::Directory(DirectoryError::Exists { .. }))
    ));
    assert_eq!(txn.flat.wrote, wrote);
    db.roll_back(txn);

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);

    // A prefix that a left-over entry has: its records leave every column.
    let mut txn = db.begin();
    let directory = grove.directory(&db);
    for column in 0..3 {
        assert!(!raw(&txn.flat, &grove, &inner_prefix, column).is_empty());
    }
    let entered = enter(
        &directory,
        &mut flats(&mut txn, &grove),
        &inner_prefix,
        b"tree",
        Some(host(b"c")),
    );
    entered.expect("entered");
    for column in 0..3 {
        assert!(raw(&txn.flat, &grove, &inner_prefix, column).is_empty());
    }
    assert!(!txn.flat.wrote.is_disjoint(&of_columns(&db)));
    db.roll_back(txn);
    grove.check(&db);
}

/// What a drop reads and writes of the flat columns does not grow with
/// what the dropped Merk has in them: the leaf at each end of its range in
/// each column and the branches above them, and the record of the pages
/// between.
#[test]
fn a_drop_reads_and_writes_the_two_ends_of_its_flat_records() {
    let mut counts = Vec::new();
    for records in [0, 8, 400, 4_000] {
        let db = Db::new();
        let mut txn = db.begin();
        let mut grove = Grove::new(&db, &mut txn);
        for n in 0..12 {
            grove.put(0, &node(n), b"a value");
        }
        let first = grove.put(0, b"host-a", b"a tree");
        let second = grove.put(0, b"host-b", b"a tree");
        let third = grove.put(0, b"host-c", b"a tree");
        // The records of the Merk that is dropped lie between those of
        // two that stay.
        let (before, merk, after) = (grove.host(first), grove.host(second), grove.host(third));
        for of in [before, merk, after] {
            grove.put(of, b"a node", b"of a Merk");
        }
        grove.flush(&mut txn);
        db.commit(txn);
        for (of, count) in [(before, 300_usize), (after, 300), (merk, records)] {
            for chunk in 0..count.div_ceil(500) {
                let mut txn = db.begin();
                for n in chunk * 500..count.min(chunk * 500 + 500) {
                    grove.put_flat(of, 0, &record(n), &held(n, of as u64));
                    if n % 50 == 0 {
                        grove.put_flat(of, 1, &record(n), &held(n + 1, of as u64));
                    }
                }
                grove.flush(&mut txn);
                db.commit(txn);
            }
        }
        db.write_back();
        let column = scan_flat(&db, &grove.columns[0].tree);
        let of_columns: BTreeSet<PageId> = [0, 1]
            .into_iter()
            .flat_map(|n| scan_flat(&db, &grove.columns[n].tree).pages)
            .collect();
        let of_directory = scan_flat(&db, grove.directory(&db).tree()).pages;

        let mut txn = db.begin();
        txn.count_from_here();
        grove.drop_merks(&mut txn, &[merk], false);
        let read = txn.flat.read.borrow().clone();
        let wrote = txn.flat.wrote.clone();
        let tree_read = txn.tree.read.len();
        let tree_wrote = txn.tree.changed().len();
        db.commit(txn);
        grove.check(&db);
        let among =
            |pages: &BTreeSet<PageId>, of: &BTreeSet<PageId>| pages.intersection(of).count();
        println!(
            "a drop of a Merk with {records} flat records in a column of {} pages ({} leaves): \
             {} flat pages read ({} of the columns, {} of the directory), {} written ({} of \
             the columns, {} of the directory); {tree_read} tree pages read, {tree_wrote} \
             written",
            column.pages.len(),
            column.leaves,
            read.len(),
            among(&read, &of_columns),
            among(&read, &of_directory),
            wrote.len(),
            among(&wrote, &of_columns),
            among(&wrote, &of_directory),
        );
        if records == 0 {
            // With no record, each column is read down one path, and none
            // of their pages is written.
            assert_eq!(among(&wrote, &of_columns), 0);
        }
        let (read, wrote) = (read.len(), wrote.len());
        counts.push((read, wrote, tree_read, tree_wrote));
        reclaim_all(&db, &grove);
        check_reclaimed(&db, &grove);
    }
    let [none, few, many, most] = counts[..] else {
        panic!("four sizes");
    };
    // Nothing of the tree pages depends on the flat records.
    assert!([few, many, most]
        .iter()
        .all(|of| (of.2, of.3) == (none.2, none.3)));
    assert!(none.1 <= few.1);
    // Ten times the records, and a page or two more for the taller tree.
    assert!(most.0 <= many.0 + 4, "{many:?} {most:?}");
    assert!(most.1 <= many.1 + 4, "{many:?} {most:?}");
    assert!(most.0 < 30 && most.1 < 20, "{most:?}");
}

/// A record whose value is out of line is known by the place of the value.
/// Write-back gives the value its place between the reclaimer's look and
/// its step: the step is not made, though the record is the one it was.
/// The reclaimer looks again and takes it out.
#[test]
fn a_record_whose_value_got_its_place_since_the_look_is_looked_at_again() {
    let Built {
        db,
        mut grove,
        outer,
        inner,
        ..
    } = build_in(out_of_line(), 40);
    let inner_prefix = grove.merks[inner].prefix;
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);
    db.tick();
    let directory = grove.directory(&db);
    let tree = *directory.tree();
    let key = entry_key(&inner_prefix);
    let look = || seen(&db.reader(), &tree, &key).expect("a look");
    let looked = look().expect("the left-over entry");
    assert!(matches!(looked, Seen::OutOfLine(_)));

    db.write_back();
    assert_ne!(look().as_ref(), Some(&looked));
    let reclaimer = db.reclaimer();
    let step = reclaimer.remove_entry(&tree, &key, &looked);
    let step = step.expect("no step");
    assert!(matches!(step, Step::Made(EntryRemoved::Changed)));
    let stored = directory.stored(&db.reader(), &inner_prefix);
    let stored = stored.expect("a lookup").expect("the entry");
    assert!(!directory.is_live(&stored));

    let done = reclaim_all(&db, &grove);
    assert!(done[0].records >= 6, "{:?}", done[0]);
    check_reclaimed(&db, &grove);
}

/// A writer commits records of the Merks in use, enters a Merk again under
/// the prefix of a nested one with records under the keys that one had,
/// and writes the values back, while the reclaimer takes its steps on
/// another thread. Afterwards the columns hold what the writer wrote and
/// nothing else, and every value is counted once. Every other round has
/// the records of the directory out of line, so that their places change
/// under the reclaimer.
#[test]
fn a_writer_and_the_reclaimer_at_once_leave_the_flat_records_whole() {
    let mut beside_it = 0;
    for round in 0..6 {
        let db = match round % 2 {
            0 => Db::new(),
            _ => out_of_line(),
        };
        let Built {
            db,
            mut grove,
            outer,
            inner,
            beside,
            ..
        } = build_in(db, 150 + round * 40);
        if round % 3 != 2 {
            db.write_back();
        }
        let inner_prefix = grove.merks[inner].prefix;
        let old_inner = grove.flat_of(inner, 0);
        let mut txn = db.begin();
        grove.drop_merks(&mut txn, &[outer], false);
        db.commit(txn);
        db.tick();
        let directory = grove.directory(&db);
        let [dropped] = drops(&db, &directory)[..] else {
            panic!("one Merk was dropped");
        };
        let finished = AtomicBool::new(false);
        let commits = AtomicU64::new(0);
        // The reclaimer lets the writer commit after every step, so the
        // two do run beside each other however the threads are run.
        let let_commit = || {
            let seen = commits.load(Ordering::SeqCst);
            while commits.load(Ordering::SeqCst) == seen {
                thread::yield_now();
            }
        };

        thread::scope(|scope| {
            let reclaiming = scope.spawn(|| {
                let reclaimer = db.reclaimer();
                let mut reclaim = Reclaim::new(dropped);
                let columns = db.columns();
                loop {
                    match reclaimer.reclaim(&mut reclaim, &directory, &columns, &GUARD, 1) {
                        Ok(Progress::Done) => break,
                        Ok(Progress::More) => let_commit(),
                        Ok(Progress::Blocked(_)) => thread::yield_now(),
                        Err(error) => panic!("{error}"),
                    }
                }
                assert!(reclaim.miscount().is_none());
                // And the pages that the enters of the writer unlinked so
                // far, whose records write-back moves as well.
                let drops = db.drops();
                let mut freed = FlatReclaimed::default();
                for left in reclaimer.flat_drops(&drops, &GUARD).expect("the records") {
                    loop {
                        match reclaimer.reclaim_flat(&drops, &left, &GUARD, 2, &mut freed) {
                            Ok(Progress::Done) => break,
                            Ok(Progress::More) => let_commit(),
                            Ok(Progress::Blocked(_)) => thread::yield_now(),
                            Err(error) => panic!("{error}"),
                        }
                    }
                }
                assert!(freed.miscount.is_none());
                finished.store(true, Ordering::SeqCst);
            });

            let under = grove.merks[beside].root.expect("a root");
            let mut again = None;
            let (mut n, mut after) = (0, 0);
            while after < 3 {
                match finished.load(Ordering::SeqCst) {
                    true => after += 1,
                    false => beside_it += 1,
                }
                let mut txn = db.begin();
                if n == round % 3 {
                    let merk = grove.host_under(under, inner_prefix);
                    grove.put(merk, b"a node", b"of the Merk that is there now");
                    grove.flush(&mut txn);
                    for (key, value) in &old_inner {
                        grove.put_flat(merk, 0, key, value);
                    }
                    again = Some(merk);
                }
                grove.put_flat(beside, 0, &record(n % 40), &held(n, 20));
                grove.put_flat(beside, 0, &record(1_000 + n), &held(n, 21));
                grove.put_flat(beside, 2, &record(n % 7), &held(n, 22));
                if let Some(merk) = again {
                    grove.put_flat(merk, 0, &record(n % 60), &held(n, 23));
                    grove.put_flat(merk, 1, &record(n % 9), &held(n, 24));
                }
                grove.put(0, &node(n % 12), &held(n, 25));
                grove.flush(&mut txn);
                db.commit(txn);
                db.write_back();
                commits.fetch_add(1, Ordering::SeqCst);
                n += 1;
                assert!(n < 100_000, "the reclaiming does not end");
            }
            reclaiming.join().expect("the reclaimer");
            assert!(again.is_some());
        });

        db.write_back();
        free_unlinked(&db, &db.drops());
        check_reclaimed(&db, &grove);
    }
    eprintln!("{beside_it} commits beside the reclaimer");
    assert!(beside_it > 30);
}
