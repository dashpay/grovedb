//! Readers and the subtree directory (`crate::snapshot`, `tree-layout.md`
//! D6, `transactions.md` CV9, T-CL1): a snapshot and an operation that
//! pins no version read the records of a subtree through its entry in the
//! directory of the version they read, and read nothing where it has none.
//! The handle they ask is one that was loaded while their version was the
//! latest.

use std::sync::Arc;

use lignin_format::CellAddress;
use lignin_store::load::LoadBuffers;
use lignin_tree::{directory::DirectoryError, drop::Progress};

use super::{
    model::{self, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, ENTERED},
    subtrees::{drop_outer, planted, planted_records, INNER, OUTER},
    *,
};
use crate::snapshot::{LatestLease, Operation, ReadError, ReadPath};

/// Commits `batch` in a transaction of its own.
fn commit(world: &mut World<'_>, batch: &OwnedBatch) {
    let tx = world.begin();
    world.apply(&tx, batch).expect("the batch is applied");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
}

fn subtrees(subtrees: Vec<OwnedSubtree>) -> OwnedBatch {
    OwnedBatch {
        subtrees,
        ..OwnedBatch::default()
    }
}

/// The keys of the nodes of the Merk with the root `root`, in order, as
/// `operation` reads them.
fn nodes(operation: &Operation<'_>, root: Option<CellAddress>) -> Vec<Vec<u8>> {
    let reader = operation.nodes();
    let mut buffers = LoadBuffers::new();
    let mut cursor = lignin_tree::cursor::Cursor::open(&reader, root);
    (cursor.seek_to_first(&reader, &mut buffers)).expect("a seek");
    let mut keys = Vec::new();
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        keys.push(key);
        cursor.next(&reader, &mut buffers).expect("a step");
    }
    keys
}

/// What `operation` reads of the two planted subtrees: their flat records
/// by key, their flat records under a cursor over every column, and the
/// nodes of each.
fn read(operation: &Operation<'_>) -> (usize, usize, usize, usize) {
    let open = |subtree| operation.open(subtree).expect("the directory is read");
    let mut by_key = 0;
    for (unit, key) in planted_records() {
        if let Some(opened) = open(unit.subtree) {
            let mut out = Vec::new();
            let found = opened.get(unit.column, key, &mut out).expect("a read");
            by_key += usize::from(found);
        }
    }
    let (mut under, mut counts) = (0, [0, 0]);
    for (at, name) in [OUTER, INNER].into_iter().enumerate() {
        let Some(opened) = open(model::subtree(name)) else {
            continue;
        };
        assert_eq!(opened.subtree(), model::subtree(name));
        for column in Column::ALL {
            let mut cursor = opened.cursor(column);
            cursor.seek_to_first().expect("a seek");
            while cursor.key().is_some() {
                let mut out = Vec::new();
                assert!(cursor.value(&mut out).expect("a value") && !out.is_empty());
                under += 1;
                cursor.next().expect("a step");
            }
        }
        counts[at] = nodes(operation, opened.root()).len();
    }
    (by_key, under, counts[0], counts[1])
}

/// The value under the key `key` of the aux column under the prefix of
/// `name`, read from the pages of the latest version with no directory:
/// whether the record is in the pages.
fn in_pages(db: &Db, versions: &Versions, name: u8, key: &[u8]) -> bool {
    let guard = versions.guard();
    let pages = db.committed(&guard, guard.latest());
    let stored = [&model::subtree(name).as_bytes()[..], key].concat();
    let mut out = Vec::new();
    (db.keyspaces().aux.get(&pages, &stored, &mut out)).expect("a read")
}

/// A snapshot at a version after a drop reads nothing of the dropped
/// subtree or of the one nested in it, in any column, before what the drop
/// left is reclaimed and after; a snapshot from before the drop reads all
/// of it for as long as it is held, and the reclaimer waits for it.
#[test]
fn a_reader_reads_the_records_of_a_subtree_through_its_entry() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let reads = db.reads(&versions);
    let all = (5, 5, 3, 2);

    let before = reads.snapshot();
    assert_eq!(read(&before.operation()), all);
    // A subtree that was never entered has no record for a reader.
    let never = model::subtree(ENTERED + 50);
    assert!(before.operation().open(never).expect("a lookup").is_none());
    // A reader keeps a subtree it opened from one operation to the next.
    let kept = {
        let operation = before.operation();
        let inner = operation.open(model::subtree(INNER)).expect("a lookup");
        let inner = inner.expect("the subtree");
        assert!(inner.entry().host().is_some() && inner.root().is_some());
        assert!(format!("{inner:?}").contains("Opened"));
        inner.keep()
    };

    let tx = world.begin();
    drop_outer(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();

    // From the drop's version on, nothing of either subtree: for a
    // snapshot, and for an operation that pins no version.
    let after = reads.snapshot();
    assert_eq!(read(&after.operation()), (0, 0, 0, 0));
    let mut lease = LatestLease::new();
    assert_eq!(read(&reads.operation(&mut lease)), (0, 0, 0, 0));
    // What the subtree below left is in the pages still, and no reader is
    // shown it. The dropped subtree's own records went with the drop.
    assert!(in_pages(&db, &versions, INNER, b"ia"));
    assert!(!in_pages(&db, &versions, OUTER, b"a"));

    // The snapshot from before reads all of it, and opens the subtree it
    // kept with no page read.
    assert_eq!(read(&before.operation()), all);
    {
        let operation = before.operation();
        let gets = |db: &Db| db.pool.stats().hits + db.pool.stats().misses;
        let asked = gets(&db);
        let inner = operation.resume(&kept).expect("no lookup");
        let inner = inner.expect("the subtree it kept");
        assert_eq!(gets(&db), asked, "no page was asked for");
        let mut out = Vec::new();
        assert!(inner.get(Column::Aux, b"ia", &mut out).expect("a read"));
        let mut cursor = inner.cursor(Column::Meta);
        cursor.seek(b"").expect("a seek");
        assert_eq!(cursor.key(), Some(&b"im"[..]));
        assert!(format!("{cursor:?}").contains("RecordCursor"));
        cursor.seek_for_prev(b"zz").expect("a seek");
        assert_eq!(cursor.key(), Some(&b"im"[..]));
        cursor.prev().expect("a step");
        assert_eq!(cursor.key(), None);
        cursor.seek_to_last().expect("a seek");
        assert_eq!(cursor.key(), Some(&b"im"[..]));
    }
    // At the later version what it kept is looked up again, and is gone.
    assert!(after.operation().resume(&kept).expect("a lookup").is_none());

    // The reclaimer waits for the snapshot from before the drop.
    let swept = db.sweep(&versions);
    assert!(swept.waiting && swept.merks.is_empty());
    assert_eq!(read(&before.operation()), all);
    assert!(in_pages(&db, &versions, INNER, b"ia"));
    drop(before);
    let swept = db.sweep(&versions);
    assert!(!swept.waiting && swept.blocked.is_none(), "{swept:?}");
    assert_eq!(swept.merks.len(), 1);
    assert_eq!(swept.merks[0].flat_records, 2, "the two records below");
    // Reclaimed, and still nothing for a reader of the drop's version or
    // of the latest.
    assert!(!in_pages(&db, &versions, INNER, b"ia"));
    assert_eq!(read(&after.operation()), (0, 0, 0, 0));
    assert_eq!(read(&reads.operation(&mut lease)), (0, 0, 0, 0));
    world.check_committed();

    // A read path that was made of the pages alone opens no subtree.
    let pages_alone = ReadPath::new(&versions, db.loader());
    let snapshot = pages_alone.snapshot();
    let operation = snapshot.operation();
    assert!(matches!(operation.open(never), Err(ReadError::NoRecords)));
    let error = ReadError::NoRecords;
    assert!(error.to_string().contains("names no records"));
    assert!(std::error::Error::source(&error).is_none());
    // A snapshot whose directory could not be loaded says so at every
    // open, with what the load said.
    let error = ReadError::Unloaded(Arc::new(DirectoryError::IdsExhausted));
    assert!(error.to_string().contains("when the snapshot was taken"));
    assert!(std::error::Error::source(&error).is_some());
}

/// A snapshot has the drops of its version from the moment it is taken,
/// and does not learn of them when it first opens a subtree. What a drop
/// left may be reclaimed while a snapshot of the drop's version is held,
/// and the record of the drop then goes from a page the snapshot reads,
/// while the entry of a subtree that was below can stay in a page version
/// that only readers of older versions have: a handle that is loaded then
/// would open that subtree. An operation that pins no version loads its
/// handle at its first open, and opens nothing if a commit was published
/// before.
#[test]
fn a_reader_knows_the_drops_of_its_version_however_late_it_opens() {
    // The entries of these three are in the first leaves of the directory,
    // far from the records of the drops, which are in its last.
    const BESIDE: u8 = 4;
    const BELOW: u8 = 5;
    const HOST: u8 = 6;
    let below = model::subtree(BELOW);
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);

    // Two of the store's subtrees leave, to be entered again in other
    // places, and what they left is reclaimed.
    let leave = [BESIDE, BELOW].map(|name| OwnedSubtree::Drop(model::subtree(name)));
    commit(&mut world, &subtrees(leave.to_vec()));
    let swept = db.sweep(&versions);
    assert!(!swept.waiting && swept.blocked.is_none(), "{swept:?}");

    // One is entered below a node of `HOST`, with a node and a record.
    let tx = world.begin();
    let mut host = world.merk(&tx, HOST, |items| {
        items.insert(b"host".to_vec(), b"tree".to_vec());
    });
    host.hosted.insert(b"host".to_vec(), vec![below]);
    let nested = world.merk(&tx, BELOW, |items| {
        items.insert(b"one".to_vec(), vec![3; 20]);
    });
    let batch = OwnedBatch {
        merks: vec![nested, host],
        records: vec![put(model::unit(Column::Aux, BELOW), b"ia", b"7")],
        subtrees: vec![OwnedSubtree::Enter(
            below,
            Some((model::subtree(HOST), b"host".to_vec(), 0)),
        )],
    };
    world.apply(&tx, &batch).expect("the subtree is entered");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    // `HOST` is dropped. A snapshot of that version opens nothing yet.
    commit(
        &mut world,
        &subtrees(vec![OwnedSubtree::Drop(model::subtree(HOST))]),
    );
    let reads = db.reads(&versions);
    let snapshot = reads.snapshot();
    let dropped_at = snapshot.version();

    // The reclaimer reads what the drop left at that version, so it waits
    // for no reader of it. An operation that pins no version begins.
    let (mut left, _, mut wait) = db.left(&versions);
    assert_eq!(left.len(), 1);
    assert!(wait.poll(&versions), "no reader is of an older version");
    let mut lease = LatestLease::new();
    let late = reads.operation(&mut lease);
    assert_eq!(late.at(), dropped_at);

    // A commit beside: it writes the leaf of the directory that has the
    // entry of the subtree below, and not the one with the record of the
    // drop. Then everything the drop left is taken out.
    let beside = OwnedSubtree::Enter(model::subtree(BESIDE), None);
    commit(&mut world, &subtrees(vec![beside]));
    for reclaim in &mut left {
        loop {
            match db.reclaim(&versions, reclaim, 64) {
                Progress::Done => break,
                Progress::More => {}
                Progress::Blocked(busy) => panic!("a step was refused: {busy:?}"),
            }
        }
    }

    // What this test is about: a handle that is loaded for the drop's
    // version only now finds no record of the drop, and the page version
    // of that version still has the entry of the subtree below.
    {
        let guard = versions.guard();
        let pages = db.committed(&guard, dropped_at);
        let loaded_late = db.directory_at(&pages);
        let entry = loaded_late.open(&pages, below.as_bytes());
        assert!(entry.expect("a lookup").is_some());
    }
    // The snapshot's handle was loaded when it was taken, and has the drop.
    for name in [HOST, BELOW] {
        let operation = snapshot.operation();
        let opened = operation.open(model::subtree(name)).expect("a lookup");
        assert!(opened.is_none(), "{opened:?}");
    }
    // The operation that pins no version would load its handle now, with
    // a commit published since it began: it opens nothing, and says so.
    match late.open(below) {
        Err(error @ ReadError::Moved { at, latest }) => {
            assert_eq!((at, latest), (dropped_at, versions.latest()));
            assert_ne!(at, latest);
            assert!(error.to_string().contains("the latest is"));
            assert!(std::error::Error::source(&error).is_none());
        }
        other => panic!("the latest version moved, and not {other:?}"),
    }
    drop(late);
    // Its next operation reads the latest version, with a handle of that.
    let next = reads.operation(&mut lease);
    assert!(next.open(below).expect("a lookup").is_none());
    let beside = next.open(model::subtree(BESIDE)).expect("a lookup");
    assert!(beside.is_some_and(|beside| beside.root().is_none()));
}

/// A reader whose directory cannot be loaded opens nothing. A snapshot
/// learns it when it is taken and answers every open with it; an
/// operation that pins no version learns it at its first open.
#[test]
fn a_reader_whose_directory_cannot_be_loaded_opens_nothing() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    // A record of the aux column whose stored key begins as the records
    // of dropped Merks do in a directory.
    let tx = world.begin();
    world.write(&tx, vec![put(model::unit(Column::Aux, 5), b"k", b"v")]);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    // The aux column in the place of the directory: no directory is there.
    let keyspaces = db.keyspaces();
    let reads = ReadPath::new(&versions, db.loader());
    let reads = reads.with_records(&db.values, keyspaces, keyspaces.aux);
    assert!(format!("{reads:?}").contains("Records"));

    let snapshot = reads.snapshot();
    for name in [1, 5] {
        let operation = snapshot.operation();
        match operation.open(model::subtree(name)) {
            Err(error @ ReadError::Unloaded(_)) => {
                assert!(error.to_string().contains("when the snapshot was taken"));
                assert!(std::error::Error::source(&error).is_some());
            }
            other => panic!("the directory was not loaded, and not {other:?}"),
        }
    }
    let mut lease = LatestLease::new();
    let operation = reads.operation(&mut lease);
    match operation.open(model::subtree(1)) {
        Err(error @ ReadError::Directory(_)) => {
            assert!(error.to_string().starts_with("subtree directory: "));
            assert!(std::error::Error::source(&error).is_some());
        }
        other => panic!("the directory is not loaded, and not {other:?}"),
    }
}

/// A subtree that a reader kept is of one database: an operation of
/// another database looks it up again, also at a version of the same
/// number, and one of the same database and version reads nothing for it.
#[test]
fn a_kept_subtree_is_looked_up_again_in_another_database() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let reads = db.reads(&versions);
    let snapshot = reads.snapshot();
    let kept = {
        let operation = snapshot.operation();
        let inner = operation.open(model::subtree(INNER)).expect("a lookup");
        inner.expect("the subtree").keep()
    };

    // Another database, at a version of the same number, where the prefix
    // has no subtree.
    let (other, other_versions) = Db::new(128);
    let mut other_world = World::new(&other, &other_versions);
    let tx = other_world.begin();
    other_world.write(&tx, vec![put(model::unit(Column::Aux, 0), b"k", b"v")]);
    assert!(matches!(other_world.commit(tx, &[]), Outcome::Committed(_)));
    let other_snapshot = other.reads(&other_versions).snapshot();
    assert_eq!(other_snapshot.version(), snapshot.version());
    let operation = other_snapshot.operation();
    assert!(operation.resume(&kept).expect("a lookup").is_none());

    // Another snapshot of the database it was kept in, at that version.
    // Taking it reads one path of the directory, down to where the records
    // of dropped Merks would be.
    let gets = |db: &Db| db.pool.stats().hits + db.pool.stats().misses;
    let asked = gets(&db);
    let again = reads.snapshot();
    let loaded = gets(&db) - asked;
    println!("a snapshot of a read path with records: {loaded} pages asked of the pool");
    assert!((1..=3).contains(&loaded), "{loaded}");
    let operation = again.operation();
    let asked = gets(&db);
    let inner = operation.resume(&kept).expect("no lookup");
    assert_eq!(gets(&db), asked, "no page was asked for");
    assert_eq!(inner.expect("the subtree").subtree(), model::subtree(INNER));
}

/// A subtree that is entered again under a prefix that a drop left has the
/// records that were written since, and none of before, for a reader of
/// the version it was entered at.
#[test]
fn a_reader_of_a_subtree_that_was_entered_again_reads_none_of_the_records_of_before() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let tx = world.begin();
    drop_outer(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    // Both are entered again, with one record under the one below.
    let tx = world.begin();
    let mut outer = world.merk(&tx, OUTER, |items| {
        items.insert(b"host".to_vec(), b"again".to_vec());
    });
    (outer.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
    let host = (model::subtree(OUTER), b"host".to_vec(), 0);
    let batch = model::OwnedBatch {
        merks: vec![outer],
        records: vec![put(model::unit(Column::Aux, INNER), b"new", b"1")],
        subtrees: vec![
            model::OwnedSubtree::Enter(model::subtree(OUTER), None),
            model::OwnedSubtree::Enter(model::subtree(INNER), Some(host)),
        ],
    };
    world
        .apply(&tx, &batch)
        .expect("the subtrees are entered again");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();

    let reads = db.reads(&versions);
    let snapshot = reads.snapshot();
    let operation = snapshot.operation();
    assert_eq!(read(&operation), (0, 1, 1, 0));
    let inner = operation.open(model::subtree(INNER)).expect("a lookup");
    let inner = inner.expect("the subtree that was entered again");
    let mut out = Vec::new();
    assert!(inner.get(Column::Aux, b"new", &mut out).expect("a read"));
    assert!(!inner.get(Column::Aux, b"ia", &mut out).expect("a read"));
    assert!(!inner.get(Column::Meta, b"im", &mut out).expect("a read"));
    assert_eq!(db.private_ids(), 0);
}
