//! Operations that hold a guard and no version pin, beside a commit and a
//! turn of write-back (`transactions.md` RD4, PN5): the read of
//! [`Db::read`], and a read of a write transaction.
//!
//! Each reads the latest version of its beginning. A page it looks up
//! after a commit superseded it must still have the link of that version,
//! whatever write-back has written since: write-back trims the chain of a
//! page it gave a base image only once the operations that were running
//! have ended (`lignin_store::flush`), and a later turn makes the trim.
//!
//! The pool of these tests is far smaller than the database, and the page
//! version that the commit superseded is pushed out of it before the
//! operation looks the page up ([`push_out_of_the_pool`]): a pool that
//! still holds that version answers the reader itself, and the page table
//! is not asked.

use std::sync::{atomic::AtomicBool, mpsc};

use lignin_format::{CommitVersion, PageId};
use lignin_tree::flat::FlatTree;

use super::*;
use crate::{
    snapshot::LatestLease,
    txn::Unit,
    verify::{self, Grove, Scope},
};

/// How many subtrees the database of these tests has: enough for a
/// subtree directory of several leaves, so that a lookup of the first
/// subtree and one of the last read no leaf in common.
const SUBTREES: u32 = 600;

/// How many nodes the Merk of these tests has, and how long their values
/// are: its pages are several times the frames of [`POOL`].
const NODES: u32 = 4000;
const VALUE_LEN: usize = 64;

/// The pool of the session that reads.
const POOL: usize = 128 << 10;

/// How long a thread waits for the other before it gives the test up.
const PATIENCE: Duration = Duration::from_secs(30);

const KEY: &[u8] = b"k";

fn first() -> Subtree {
    numbered(0)
}

fn last() -> Subtree {
    numbered(SUBTREES - 1)
}

/// The subtree of the Merk: beside the last one in the directory.
fn crowded() -> Subtree {
    numbered(SUBTREES - 2)
}

fn aux(subtree: Subtree, value: &[u8]) -> (Column, Subtree, Vec<u8>, Vec<u8>) {
    (Column::Aux, subtree, KEY.to_vec(), value.to_vec())
}

fn put(db: &Db, subtree: Subtree, value: &[u8]) {
    let writes = Writes {
        records: vec![aux(subtree, value)],
        ..Writes::default()
    };
    writes.commit(db);
}

fn turn(db: &Db) {
    (db.shared.write_back().tick()).expect("a turn of write-back");
}

/// The links of the page table and its pages. In the database of these
/// tests every page has one link, its base image, once nothing waits to
/// be trimmed.
fn links(db: &Db) -> (u64, u64) {
    let table = &db.shared.store.table;
    (table.links(), table.pages())
}

/// A database of [`SUBTREES`] subtrees: the first and the last have an aux
/// record `old` each, the only records of their column, and the one
/// before the last has a Merk. It was written back and closed, and is
/// opened again with a pool of [`POOL`], in which the open leaves the
/// pages it read itself. Nothing is written back but by [`turn`].
fn reopened(dir: &Path, vfs: &Watched) -> Db {
    let options = Options {
        vfs: Arc::new(vfs.clone()),
        ..options()
    };
    // Both sessions have neither thread from their open on: a turn of
    // write-back's thread would write back, and a round of the reclaimer's
    // would read pages into the pool, before either was ended.
    {
        let db = open_at_rest(dir, &options).expect("a new database");
        let entered = Writes {
            enter: (0..SUBTREES)
                .map(|number| (numbered(number), None))
                .collect(),
            records: vec![aux(first(), b"old"), aux(last(), b"old")],
            ..Writes::default()
        };
        entered.commit(&db);
        let merk = Writes {
            merks: vec![(crowded(), Merk::sized(b'k', NODES, VALUE_LEN))],
            ..Writes::default()
        };
        merk.commit(&db);
        turn(&db);
        db.shared.finish_reclaiming();
        (db.shared.write_back().finish()).expect("the database is closed");
    }
    let small = Options {
        cache_size: POOL,
        ..options
    };
    let db = open_at_rest(dir, &small).expect("the database is opened again");
    let (links, pages) = links(&db);
    assert_eq!(links, pages, "every page has its base image and no more");
    db
}

/// Whether the pool holds the page of the aux records as a reader at
/// `at` sees it. The two aux records of these tests are in one page, the
/// root of their keyspace.
fn pool_holds_the_aux_page(db: &Db, at: CommitVersion) -> bool {
    let store = &db.shared.store;
    let page = store.catalog().keyspaces.aux.root();
    store.pool.get(page, at).is_ok()
}

/// Pushes the page versions that are written and that nobody holds out of
/// the pool, by a read of the Merk through an operation of its own, and
/// checks what the tests need of it: the pool has no version of the page
/// of the aux records for a reader at `at` any more, the one a commit
/// loaded and superseded. Such a reader asks the page table.
fn push_out_of_the_pool(db: &Db, at: CommitVersion) {
    let mut lease = LatestLease::new();
    let read = seen(&db.read(&mut lease), crowded(), KEY).expect("the Merk");
    assert_eq!(read.0, NODES as usize);
    assert!(
        !pool_holds_the_aux_page(db, at),
        "the pool still holds the page of the aux records as a reader at version {at} sees it"
    );
}

/// Runs the physical invariant checker on the latest version of `db`,
/// which nothing writes to, and fails with what it finds.
fn check(db: &Db) {
    let store = &*db.shared.store;
    let catalog = store.catalog();
    let columns = catalog.keyspaces.columns();
    let other = [FlatTree::open(PageId::MIN, store.config())];
    let parts = verify::Store {
        loader: store.loader(),
        table: &store.table,
        segments: &store.segments,
        values: &store.values,
        page_size: store.page_size,
        ids: Some(&store.ids),
        grove: Grove::Directory {
            tree: catalog.directory,
            columns: &columns,
            data: None,
        },
        flat_drops: Some(catalog.flat_drops),
        flat: &other,
        values_counted: true,
    };
    let guard = db.shared.versions.guard();
    let options = verify::Options::new(db.version(), Scope::Still);
    let report = verify::check(&parts, &guard, options, None);
    assert!(
        report.findings.is_empty() && report.more_findings == 0,
        "{report}"
    );
}

/// The trims that waited are made by the next turn of write-back once no
/// operation runs: every page has one link again, the latest version of
/// the aux record of `of` is `value`, and the physical checker finds
/// nothing.
fn nothing_waits_after_a_turn(db: &Db, of: Subtree, value: &[u8]) {
    turn(db);
    let (links, pages) = links(db);
    assert_eq!(links, pages, "a link was left untrimmed");
    let mut lease = LatestLease::new();
    let read = seen(&db.read(&mut lease), of, KEY).expect("the subtree");
    assert_eq!(read.1, [(Column::Aux, value.to_vec())]);
    check(db);
}

/// What an operation reads of the first subtree when a commit supersedes
/// the page of its aux record after the operation began and before it
/// looked the page up, and with `pass` write-back has its turn after that
/// commit. With `pinned` the operation is a snapshot's, and otherwise it
/// holds its guard alone.
fn an_operation_reads_under_a_commit(pass: bool, pinned: bool) {
    let dir = temp();
    let db = reopened(dir.path(), &Watched::default());
    let mut lease = LatestLease::new();
    let snapshot = pinned.then(|| db.snapshot());
    let operation = match &snapshot {
        Some(snapshot) => snapshot.operation(),
        None => db.read(&mut lease),
    };
    let began_at = operation.at();

    put(&db, first(), b"new");
    assert!(db.version() > began_at);
    if pass {
        turn(&db);
    }
    push_out_of_the_pool(&db, began_at);
    let read = seen(&operation, first(), KEY).expect("the subtree");
    assert_eq!(read, (0, vec![(Column::Aux, b"old".to_vec())]));
    if pass {
        let (links, pages) = links(&db);
        assert!(links > pages, "the links of the operation's version stay");
    }

    drop(operation);
    drop(snapshot);
    nothing_waits_after_a_turn(&db, first(), b"new");
}

/// **An operation that pins nothing reads the version of its beginning
/// under its guard**, also when a commit supersedes a page it has not
/// read yet and write-back then writes the page's new image: the link of
/// the version it reads is still in the page's chain. The two tests after
/// it are its controls.
#[test]
fn an_operation_that_pins_nothing_reads_its_version_after_a_commit_and_a_turn_of_write_back() {
    an_operation_reads_under_a_commit(true, false);
}

#[test]
fn an_operation_that_pins_nothing_reads_its_version_after_a_commit() {
    an_operation_reads_under_a_commit(false, false);
}

#[test]
fn a_snapshot_reads_its_version_after_a_commit_and_a_turn_of_write_back() {
    an_operation_reads_under_a_commit(true, true);
}

/// What a read of a write transaction gives of the aux record of the
/// first subtree, when the commit of another writer, and with `pass` a
/// turn of write-back after it, are made in the middle of the read: after
/// the read took its guard and its version, while it waits to read the
/// leaf of the subtree directory that names the first subtree, and before
/// it looks up the page of the record.
///
/// The other writer writes the aux record of the last subtree, which is
/// in the same page and under another leaf of the directory, and reads
/// the Merk beside it: it needs no page that the read is loading.
fn a_transaction_reads_under_a_commit(pass: bool) {
    let dir = temp();
    let vfs = Watched::default();
    let db = reopened(dir.path(), &vfs);
    // The root of the subtree directory comes into the pool, with the
    // leaf that the other writer reads.
    let mut lease = LatestLease::new();
    assert!((db.read(&mut lease).open(last()))
        .expect("a lookup")
        .is_some());

    let (parked, is_parked) = mpsc::channel::<()>();
    let (go_on, goes_on) = mpsc::channel::<()>();
    let went_on = Arc::new(AtomicBool::new(false));
    let txn = db.begin();
    let began_at = db.version();
    let mut value = Vec::new();
    let (read, landed) = thread::scope(|scope| {
        let db = &db;
        let other = scope.spawn(move || {
            if is_parked.recv_timeout(PATIENCE).is_err() {
                return false;
            }
            put(db, last(), b"new");
            if pass {
                turn(db);
            }
            push_out_of_the_pool(db, began_at);
            go_on.send(()).is_ok()
        });
        let gone_on = went_on.clone();
        vfs.before_the_next_read_of_a_segment(move || {
            parked.send(()).expect("the other writer");
            gone_on.store(goes_on.recv_timeout(PATIENCE).is_ok(), Ordering::Release);
        });
        let read = txn.get(Unit::new(Column::Aux, first()), KEY, &mut value);
        (read, other.join().expect("the other writer"))
    });
    assert!(
        landed && went_on.load(Ordering::Acquire),
        "the other writer did not commit in the middle of the read"
    );
    assert!(db.version() > began_at);
    assert!(read.unwrap_or_else(|error| panic!("the read of the transaction: {error}")));
    assert_eq!(value, b"old");
    // The record has the same value at both versions. That the read took
    // the page at the version of its beginning is seen in the pool, which
    // holds that page version again, loaded through the page table.
    assert!(
        pool_holds_the_aux_page(&db, began_at),
        "the read did not load the page at the version of its beginning"
    );
    if pass {
        let (links, pages) = links(&db);
        assert!(links > pages, "the links of the read's version stay");
    }

    drop(txn);
    nothing_waits_after_a_turn(&db, last(), b"new");
}

/// **A read of a write transaction reads the version of its beginning
/// under its guard** (RD4), as an operation of [`Db::read`] does: it
/// holds no pin either, and the commit of another writer and a turn of
/// write-back can come between its guard and its lookup of a page. The
/// test after it is its control.
#[test]
fn a_read_of_a_write_transaction_reads_its_version_after_a_commit_and_a_turn_of_write_back() {
    a_transaction_reads_under_a_commit(true);
}

#[test]
fn a_read_of_a_write_transaction_reads_its_version_after_a_commit() {
    a_transaction_reads_under_a_commit(false);
}
