//! Readers and the subtree directory (`crate::snapshot`, `tree-layout.md`
//! D6, `transactions.md` CV9, T-CL1): a snapshot and an operation that
//! pins no version read the records of a subtree through its entry in the
//! directory of the version they read, and read nothing where it has none.

use lignin_format::CellAddress;
use lignin_store::load::LoadBuffers;

use super::{
    model,
    store::{Db, Outcome, ENTERED},
    subtrees::{drop_outer, planted, planted_records, INNER, OUTER},
    *,
};
use crate::snapshot::{LatestLease, Operation, ReadError, ReadPath};

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
