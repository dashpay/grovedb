//! What a drop writes, and what the reclaimer finds of it afterwards
//! (`lignin_tree::drop`, `tree-layout.md` D1 and D6, `transactions.md` CV9
//! and CL6): the flat records of a dropped subtree are unlinked unread,
//! every column is given to the drop, and frames that are made again over
//! another base make the drop again with that base.

use std::sync::atomic::Ordering;

use lignin_tree::{
    directory::DirectoryError,
    drop::{Busy, Progress},
};

use super::{
    model::{self, value, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, Rng, ENTERED},
    *,
};

/// Three subtrees that no store of the tests has entered, whose records
/// lie side by side in every column: the one in the middle is dropped.
const BEFORE: u8 = ENTERED + 20;
const DROPPED: u8 = ENTERED + 21;
const AFTER: u8 = ENTERED + 22;

fn aux(name: u8) -> Unit {
    model::unit(Column::Aux, name)
}

fn meta(name: u8) -> Unit {
    model::unit(Column::Meta, name)
}

fn record(index: usize) -> Vec<u8> {
    format!("record-{index:05}").into_bytes()
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn subtrees(subtrees: Vec<OwnedSubtree>) -> OwnedBatch {
    OwnedBatch {
        subtrees,
        ..OwnedBatch::default()
    }
}

/// Writes the aux records `from..to` of `name` through `tx`, and a meta
/// record beside every fiftieth.
fn records<'s>(world: &World<'s>, tx: &Tx<'s>, rng: &mut Rng, name: u8, from: usize, to: usize) {
    let mut writes = Vec::new();
    for index in from..to {
        writes.push(put(aux(name), &record(index), &value(rng, 90)));
        if index % 50 == 0 {
            writes.push(put(meta(name), &record(index), &value(rng, 40)));
        }
    }
    world.write(tx, writes);
}

/// Commits the three subtrees, each a Merk of three nodes, with `own` flat
/// records under the one that will be dropped and 300 under each of its
/// neighbours.
fn planted(world: &mut World<'_>, rng: &mut Rng, own: usize) {
    let tx = world.begin();
    let mut merks = Vec::new();
    for name in [BEFORE, DROPPED, AFTER] {
        merks.push(world.merk(&tx, name, |items| {
            for node in 0..3u8 {
                items.insert(vec![b'n', node], vec![name; 30]);
            }
        }));
    }
    let entered =
        [BEFORE, DROPPED, AFTER].map(|name| OwnedSubtree::Enter(model::subtree(name), None));
    let batch = OwnedBatch {
        merks,
        subtrees: entered.to_vec(),
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the subtrees are entered");
    assert!(committed(world.commit(tx, &[])));
    for (name, count) in [(BEFORE, 300), (AFTER, 300), (DROPPED, own)] {
        for from in (0..count).step_by(500) {
            let tx = world.begin();
            records(world, &tx, rng, name, from, count.min(from + 500));
            assert!(committed(world.commit(tx, &[])));
        }
    }
    world.check_committed();
}

fn drop_it<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let batch = subtrees(vec![OwnedSubtree::Drop(model::subtree(DROPPED))]);
    world.apply(tx, &batch).expect("the subtree is dropped");
}

/// `tree-layout.md` D1: what a drop reads and writes of the flat columns
/// does not grow with what the dropped subtree has in them. It reads one
/// path of each column and writes the leaf at each end of the subtree's
/// range, the branches above them and one record of the pages between.
/// With the range delete that reads every leaf it frees, the transaction
/// held a copy of every leaf of the range.
#[test]
fn a_drop_reads_and_writes_the_two_ends_of_its_flat_records() {
    let (mut counts, mut between) = (Vec::new(), Vec::new());
    for own in [400, 4_000] {
        let (db, versions) = Db::new(1_024);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(17);
        planted(&mut world, &mut rng, own);

        let tx = world.begin();
        // The frames are made, with their directory, by a write beside.
        world.write(&tx, vec![put(aux(1), b"beside", b"it")]);
        let gets = |db: &Db| db.pool.stats().hits + db.pool.stats().misses;
        let before = (gets(&db), tx.txn.stats());
        drop_it(&world, &tx);
        let (read, held) = (gets(&db) - before.0, tx.txn.stats());
        let flat = held.flat_pages - before.1.flat_pages;
        let tree = held.tree_pages - before.1.tree_pages;
        println!(
            "a drop of a subtree with {own} aux records: {read} pages asked of the pool, \
             {flat} flat pages copied, {tree} tree pages copied"
        );
        counts.push((read, flat, tree));
        world.check(&tx);
        assert!(committed(world.commit(tx, &[])));
        world.check_committed();
        // The pages between the two ends wait for the reclaimer, under one
        // record for each column that had some.
        let (_, left, _) = db.left(&versions);
        let pages: usize = left.iter().map(|drop| drop.pages.len()).sum();
        assert!(!left.is_empty() && left.len() <= Column::ALL.len());
        println!("  {pages} pages or runs of pages are left for the reclaimer");
        between.push(pages);
    }
    let (small, large) = (counts[0], counts[1]);
    assert_eq!((small.2, large.2), (1, 1), "the page of the Merk's root");
    // Ten times the records, and ten times the leaves between the two
    // ends: what is read and written grows by one level of the column, and
    // stays far below the pages that are left unread.
    assert!(
        between[1] >= 40 && between[1] >= 8 * between[0],
        "{between:?}"
    );
    assert!(
        small.1 <= 8 && large.1 <= small.1 + 3,
        "flat pages written: {counts:?}"
    );
    assert!(
        small.0 <= 30 && large.0 <= small.0 + 8,
        "pages read: {counts:?}"
    );
}

/// A column that a drop is not given keeps the records of the dropped
/// subtree, and shows them to one that is entered again under its prefix
/// (`lignin_tree::drop::Flats`). So a drop is given every column the store
/// has: this fails to build when [`Keyspaces`] gets a keyspace, and fails
/// when that keyspace is not among those a drop is given.
#[test]
fn a_drop_is_given_every_column_of_the_store() {
    let (db, versions) = Db::new(32);
    let keyspaces = db.keyspaces();
    // The write path has the keyspace of the unlinked pages as the store
    // was made with it.
    let unlinked = db.path(&versions).flat_drops();
    assert_eq!(unlinked.root_page(), db.flat_drops().root_page());
    let Keyspaces {
        data,
        aux,
        roots,
        meta,
    } = keyspaces;
    let all = [data, aux, roots, meta];
    let given: Vec<_> = keyspaces
        .columns()
        .iter()
        .map(|column| column.tree)
        .collect();
    assert_eq!(given.len(), all.len());
    for (at, tree) in all.iter().enumerate() {
        assert!(
            given.contains(tree),
            "the keyspace {at} is not given to a drop"
        );
        let others = all.iter().filter(|other| other.root() == tree.root());
        assert_eq!(others.count(), 1, "each keyspace has pages of its own");
    }
    assert!(keyspaces
        .columns()
        .iter()
        .all(|column| column.lead.is_empty()));
    // And every column names one of them.
    let place = |column: Column| match column {
        Column::Data => 0,
        Column::Aux => 1,
        Column::Roots => 2,
        Column::Meta => 3,
    };
    let named: Vec<usize> = Column::ALL.into_iter().map(place).collect();
    assert_eq!(named, (0..all.len()).collect::<Vec<_>>());
    for column in Column::ALL {
        assert_eq!(keyspaces.of(column), all[place(column)]);
    }
}

/// What happens between a drop and the commit of its transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Between {
    Nothing,
    /// A commit outside lands: the frames are made again over it (WT7).
    Commit,
    /// The drop is taken back to a mark and made again, and then writes
    /// after it are taken back, which makes the frames again with the
    /// drop in them.
    Rewind,
    /// An operation after the drop fails with pages changed: the frames
    /// are made again from the write set, with the drop in it (WT10).
    Refusal,
}

/// Drops a subtree with pages of committed flat records, and with records
/// that the dropping transaction wrote itself, commits, and has the
/// reclaimer take out what was left. Returns the flat pages that were
/// freed, the records of unlinked pages, and the tree pages freed.
fn dropped_and_swept(between: Between) -> (u64, usize, u64) {
    let (db, versions) = Db::new(1_024);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(23);
    planted(&mut world, &mut rng, 1_500);

    let tx = world.begin();
    // Records of its own under the subtree, in leaves of its own: pages
    // that are born with its commit.
    records(&world, &tx, &mut rng, DROPPED, 1_500, 2_200);
    let rebuilds = tx.txn.stats().rebuilds;
    match between {
        Between::Nothing | Between::Commit => drop_it(&world, &tx),
        Between::Refusal => {
            drop_it(&world, &tx);
            // Two subtrees at one place of one host: the first is entered,
            // and the directory refuses the second.
            let host = (model::subtree(BEFORE), vec![b'n', 0], 0);
            let enter =
                |name: u8| OwnedSubtree::Enter(model::subtree(ENTERED + name), Some(host.clone()));
            let error = world.apply(&tx, &subtrees(vec![enter(30), enter(31)]));
            let error = error.expect_err("the host has a subtree there");
            assert!(
                matches!(
                    &error,
                    TxnError::Directory(DirectoryError::HostTaken { position: 0, .. })
                ),
                "{error}"
            );
        }
        Between::Rewind => {
            let (mark, writes) = (tx.txn.mark(), tx.model.borrow().writes.len());
            drop_it(&world, &tx);
            world.check(&tx);
            assert!(tx.txn.rewind(mark));
            tx.model.borrow_mut().writes.truncate(writes);
            // The records are read again, and the drop is made again.
            world.check(&tx);
            drop_it(&world, &tx);
            let (mark, writes) = (tx.txn.mark(), tx.model.borrow().writes.len());
            world.write(&tx, vec![put(aux(BEFORE), b"later", b"taken back")]);
            assert!(tx.txn.rewind(mark));
            tx.model.borrow_mut().writes.truncate(writes);
        }
    }
    world.check(&tx);
    if between == Between::Commit {
        // A commit that meets nothing of the transaction, in no column.
        world.edit_outside(
            3,
            |items| drop(items.insert(b"outside".to_vec(), vec![7; 40])),
            &[&tx],
        );
        world.check(&tx);
    }
    let made_again = match between {
        Between::Nothing => 0,
        Between::Commit | Between::Refusal => 1,
        Between::Rewind => 2,
    };
    assert_eq!(tx.txn.stats().rebuilds, rebuilds + made_again);
    let outcome = world.commit(tx, &[]);
    let Outcome::Committed(version) = outcome else {
        panic!("the drop commits: {outcome:?}");
    };
    world.check_committed();

    // The records of the unlinked pages carry the version the transaction
    // committed over, whatever version it read at when it made the drop:
    // the pages it wrote itself are born with the next one.
    let (merks, left, mut wait) = db.left(&versions);
    assert_eq!(merks.len(), 1);
    assert!(!left.is_empty());
    for drop in &left {
        assert_eq!(drop.at.next(), Some(version), "the base of the commit");
    }
    assert!(wait.poll(&versions), "no reader of before is left");
    // A page that the latest commit bore waits for the next commit.
    let early = db.sweep(&versions);
    assert_eq!(early.blocked, Some(Busy::Newborn));
    world.edit_outside(
        4,
        |items| drop(items.insert(b"next".to_vec(), vec![8; 40])),
        &[],
    );
    let swept = db.sweep(&versions);
    assert_eq!(swept.blocked, None, "{swept:?}");
    assert_eq!(swept.flat.miscount.as_ref().map(|_| ()), None);
    // Every record of the neighbours is read, and the tree pages hold no
    // cell that nothing reaches.
    world.check_committed();
    assert_eq!(
        *db.counts.census.lock().expect("a census"),
        (u64::from(ENTERED) + 2, 0, 0)
    );
    // Nothing is left for a second sweep, which frees nothing twice.
    let (merks, left_now, _) = db.left(&versions);
    assert!(merks.is_empty() && left_now.is_empty());
    let again = db.sweep(&versions);
    assert_eq!((again.flat.pages_freed, again.merks.len()), (0, 0));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);

    let flat = early.flat.pages_freed + swept.flat.pages_freed;
    let records = early.flat_drops.len() + swept.flat_drops.len();
    assert_eq!(records, left.len());
    let tree = (early.merks.iter().chain(&swept.merks)).map(|merk| merk.pages_freed);
    (flat, records, tree.sum())
}

/// D-093 item 4: a record of unlinked pages carries the version its writer
/// read at, and the reclaimer takes the next one for the commit. A
/// transaction whose frames are made again over another base, after a
/// commit outside or a rewind, makes the drop again with that base. With
/// the base of the first time, the pages it wrote itself would be taken
/// for pages of a later commit and never be freed. Frames that are made
/// again after an operation failed make the drop again as well.
#[test]
fn a_drop_that_is_made_again_over_another_base_leaves_every_page_to_be_freed_once() {
    let plain = dropped_and_swept(Between::Nothing);
    println!(
        "a drop of 2,200 flat records in 2 columns: {} flat pages freed under {} records, {} \
         tree pages freed",
        plain.0, plain.1, plain.2
    );
    assert!(plain.0 >= 20, "the pages between the two ends: {plain:?}");
    assert_eq!(dropped_and_swept(Between::Commit), plain);
    assert_eq!(dropped_and_swept(Between::Rewind), plain);
    assert_eq!(dropped_and_swept(Between::Refusal), plain);
}

/// The data of a subtree that keeps it flat goes with the drop as the
/// records of its other columns do: nothing is under the prefix in the
/// data column afterwards, for the transaction and in the pages, and a
/// subtree that is entered there again has only what is written since.
#[test]
fn a_drop_takes_the_flat_data_of_a_subtree() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let subtree = model::subtree(DROPPED);
    let data = model::unit(Column::Data, DROPPED);
    let enter = || subtrees(vec![OwnedSubtree::Enter(subtree, None)]);
    let in_pages = |column: Column, key: &[u8]| {
        let guard = versions.guard();
        let pages = db.committed(&guard, guard.latest());
        let stored = [&subtree.as_bytes()[..], key].concat();
        let tree = db.keyspaces().of(column);
        (tree.get(&pages, &stored, &mut Vec::new())).expect("a read")
    };

    let tx = world.begin();
    world.apply(&tx, &enter()).expect("the subtree is entered");
    let written = [
        put(data, b"d1", b"1"),
        put(data, b"d2", b"2"),
        put(aux(DROPPED), b"a", b"3"),
    ];
    world.write(&tx, written.to_vec());
    assert!(committed(world.commit(tx, &[])));
    assert!(in_pages(Column::Data, b"d1") && in_pages(Column::Aux, b"a"));

    let tx = world.begin();
    drop_it(&world, &tx);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    for (column, key) in [
        (Column::Data, &b"d1"[..]),
        (Column::Data, b"d2"),
        (Column::Aux, b"a"),
    ] {
        assert!(!in_pages(column, key), "{column:?}");
    }

    let tx = world.begin();
    world.apply(&tx, &enter()).expect("it is entered again");
    world.write(&tx, vec![put(data, b"d3", b"3")]);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let snapshot = db.reads(&versions).snapshot();
    let operation = snapshot.operation();
    let opened = operation.open(subtree).expect("a lookup");
    let mut cursor = opened.expect("the subtree").cursor(Column::Data);
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), Some(&b"d3"[..]));
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), None);
}

/// TREE-09 asked for this of the task that runs a reclaimer beside a writer
/// of `lignindb::txn`: the reclaimer takes out what a drop left, a page
/// step at a time, while a transaction writes Merks beside the dropped one
/// and flat records in the leaves that hold records which go. After every
/// step every node and every flat record is read, through the writing
/// transaction and as committed.
///
/// The two take turns here, on one thread. A step is refused while the
/// writer holds a copy of its page, and made after the writer's commit.
#[test]
fn a_reclaimer_beside_a_writer_leaves_every_node_and_record_whole() {
    let (db, versions) = Db::new(1_024);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(31);
    // Small Merks, whose cells share pages, and one of them hosts a Merk
    // that has flat records of its own: those are the reclaimer's.
    let nested = ENTERED + 23;
    let tx = world.begin();
    let mut merks = Vec::new();
    for name in [BEFORE, DROPPED, AFTER, nested] {
        merks.push(world.merk(&tx, name, |items| {
            for node in 0..8u8 {
                items.insert(vec![b'n', name, node], value(&mut rng, 40));
            }
        }));
    }
    let host = vec![b'n', DROPPED, 5];
    merks[1]
        .hosted
        .insert(host.clone(), vec![model::subtree(nested)]);
    let alone = |name| OwnedSubtree::Enter(model::subtree(name), None);
    let below = OwnedSubtree::Enter(
        model::subtree(nested),
        Some((model::subtree(DROPPED), host, 0)),
    );
    merks.swap(1, 3);
    let batch = OwnedBatch {
        merks,
        subtrees: vec![alone(BEFORE), alone(DROPPED), alone(AFTER), below],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the subtrees are made");
    for name in [BEFORE, DROPPED, AFTER, nested] {
        records(&world, &tx, &mut rng, name, 0, 260);
    }
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    let tx = world.begin();
    drop_it(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.counts.dropped.load(Ordering::Relaxed), 1);

    let (mut left, flat, mut wait) = db.left(&versions);
    assert!(wait.poll(&versions));
    assert_eq!(left.len(), 1);
    let mut reclaim = left.pop().expect("the dropped Merk");
    let mut writer = world.begin();
    let (mut refused, mut rounds) = (Vec::new(), 0);
    while !reclaim.is_done() {
        rounds += 1;
        assert!(
            rounds < 400,
            "the reclaiming ends: {:?}",
            reclaim.reclaimed()
        );
        // The writer changes a Merk beside the dropped one, and flat
        // records at the end of the keys before those that go.
        let name = [BEFORE, AFTER][rounds % 2];
        world.edit(&writer, name, |items| {
            items.insert(vec![b'w', rounds as u8], value(&mut rng, 70));
            if rounds % 3 == 0 {
                let first = items.keys().next().cloned().expect("a node");
                items.remove(&first);
            }
        });
        let from = 1_000 + rounds * 3;
        records(&world, &writer, &mut rng, name, from, from + 3);
        // One step. It is refused for a page the writer holds a copy of,
        // and for a page the latest commit bore.
        if let Progress::Blocked(busy) = db.reclaim(&versions, &mut reclaim, 1) {
            assert!(matches!(busy, Busy::Copied | Busy::Newborn), "{busy:?}");
            refused.push(busy);
        }
        // Every node and record, through the writer and as committed.
        world.check(&writer);
        world.check(&world.begin());
        if rounds % 4 == 0 {
            assert!(committed(world.commit(writer, &[])));
            writer = world.begin();
            // While cells of the Merk go, they are known to the reclaiming
            // alone, and a check of the pages would take them for lost.
            if !reclaim.holds_cells() {
                world.check_committed();
            }
        }
    }
    assert!(committed(world.commit(writer, &[])));
    world.check_committed();
    let done = reclaim.reclaimed();
    assert_eq!(done.merks.len(), 1, "the Merk that was below");
    assert_eq!(done.cells, 16, "every cell of both Merks");
    assert!(
        done.flat_records > 260,
        "the flat records of the Merk below: {done:?}"
    );
    println!("a reclaiming beside a writer: {rounds} rounds, refused {refused:?}, {done:?}");
    assert!(
        refused.contains(&Busy::Copied),
        "a step met a copy of the writer: {refused:?}"
    );
    // The unlinked pages of the dropped subtree's own flat records.
    let swept = db.sweep(&versions);
    assert_eq!(swept.blocked, None);
    assert_eq!(swept.flat_drops.len(), flat.len());
    world.check_committed();
    assert_eq!(
        *db.counts.census.lock().expect("a census"),
        (u64::from(ENTERED) + 2, 0, 0)
    );
    assert_eq!(db.private_ids(), 0);
}
