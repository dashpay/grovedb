//! The dump and the load of one subtree (GROVE-09, feature
//! `unsafe-dump-load`): `GroveDb::dump_subtree`, `load_subtree`,
//! `subtree_root_as` and `replace_subtree_root`, on LigninDB and, where
//! the RocksDB engine is compiled in, on RocksDB and from each engine to
//! the other.
//!
//! The flow is the one of Platform's devnet shielded-pool snapshot
//! (`platform-requirements.md` P-30): a pool is filled on one grove, its
//! notes are dumped, and a second grove whose pool is empty takes them
//! inside the transaction that builds its trees.

use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};
use grovedb_costs::OperationCost;
use grovedb_merk::proofs::Query;
use grovedb_merk::store::{NodeChangeSet, NodeStore};
use grovedb_storage::{FlatStore, RawIterator};
use lignindb::{verify::Census, Db, Options};

use super::{version, Lignin, ROOT};
use crate::{
    batch::QualifiedGroveDbOp,
    engine::{Engine, SubtreeHandle},
    operations::dump_load::{DumpLoadError, DumpedSubtree},
    query_result_type::QueryResultType,
    Element, GroveDb, PathQuery, SizedQuery, TransactionArg,
};

/// Platform's `RootTree::ShieldedBalances`, its main pool under it, and
/// the keys of the pool's five children.
const BALANCES: &[u8] = &[16];
const MAIN: &[u8] = b"M";
const TOTAL: &[u8] = &[32];
const NULLIFIERS: &[u8] = &[64];
const BY_HEIGHT: &[u8] = &[96];
const NOTES: &[u8] = &[128];
const ANCHORS: &[u8] = &[192];
const POOL: [&[u8]; 2] = [BALANCES, MAIN];

/// Eight notes fill a chunk, so the notes of these tests are in chunks,
/// in the nodes of the range over them and in the buffer.
const CHUNK_POWER: u8 = 3;
const NOTE_COUNT: usize = 77;

/// What an engine of these tests does with the directory of a database
/// that was closed.
trait Hatched: Engine {
    const NAME: &'static str;

    /// Checks the closed database, and counts its pages where the engine
    /// has a checker.
    fn closed(dir: &std::path::Path) -> Option<Census>;
}

impl Hatched for super::LigninEngine {
    const NAME: &'static str = "LigninDB";

    fn closed(dir: &std::path::Path) -> Option<Census> {
        let db = Db::open(dir, &Options::default()).expect("the database opens again");
        let mut rules = grovedb_merk::store::lignin::hash::MerkHashRules::for_check(version());
        let report = db.close_and_verify(Some(&mut rules)).expect("a close");
        assert!(report.is_sound(), "{:#?}", report.findings);
        Some(report.census)
    }
}

#[cfg(feature = "rocksdb_storage")]
impl Hatched for crate::RocksDbEngine {
    const NAME: &'static str = "RocksDB";

    fn closed(_: &std::path::Path) -> Option<Census> {
        None
    }
}

struct Grove<E: Engine> {
    db: GroveDb<E>,
    dir: tempfile::TempDir,
}

impl<E: Hatched> Grove<E> {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<E>::open_with_engine(dir.path()).expect("a database");
        Self { dir, db }
    }

    fn close(self) -> Option<Census> {
        let Self { dir, db } = self;
        drop(db);
        E::closed(dir.path())
    }
}

fn note(n: usize, tag: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..2].copy_from_slice(&(n as u16).to_be_bytes());
    bytes[2] = tag;
    bytes
}

fn ciphertext(n: usize) -> TransmittedNoteCiphertext<DashMemo> {
    let mut epk = [0u8; 32];
    epk[..2].copy_from_slice(&(n as u16).to_be_bytes());
    let mut enc = [0xecu8; 104];
    enc[0] = n as u8;
    let mut out = [0x0cu8; 80];
    out[0] = n as u8;
    TransmittedNoteCiphertext::from_parts(epk, NoteBytesData(enc), out)
}

/// The trees of a shielded pool as Platform's genesis makes them, in its
/// order, with something in each tree beside the notes.
fn pool<'db, E: Engine>(db: &'db GroveDb<E>, through: TransactionArg<'db, '_, E>) {
    let v = version();
    let trees: [(&[&[u8]], &[u8], Element); 7] = [
        (&[], BALANCES, Element::empty_sum_tree()),
        (&[BALANCES], MAIN, Element::empty_sum_tree()),
        (
            &POOL,
            NOTES,
            Element::empty_commitment_tree(CHUNK_POWER).expect("a chunk power"),
        ),
        (&POOL, NULLIFIERS, Element::empty_provable_count_tree()),
        (&POOL, ANCHORS, Element::empty_tree()),
        (&POOL, TOTAL, Element::new_sum_item(0)),
        (&POOL, BY_HEIGHT, Element::empty_tree()),
    ];
    for (path, key, element) in trees {
        db.insert(path, key, element, None, through, v)
            .unwrap()
            .expect("a tree of the pool");
    }
    for n in 0..9u8 {
        let around: [(&[u8], [u8; 32], Element); 3] = [
            (NULLIFIERS, [n; 32], Element::new_item(vec![])),
            (ANCHORS, [n ^ 0xa5; 32], Element::new_item(vec![n; 8])),
            (BY_HEIGHT, [n ^ 0x5a; 32], Element::new_item(vec![n; 32])),
        ];
        for (tree, key, element) in around {
            db.insert(
                [BALANCES, MAIN, tree].as_ref(),
                &key,
                element,
                None,
                through,
                v,
            )
            .unwrap()
            .expect("an entry of a tree beside the notes");
        }
    }
    db.insert(&POOL, TOTAL, Element::new_sum_item(7_700), None, through, v)
        .unwrap()
        .expect("the balance of the pool");
}

/// Appends note `n`, and gives what the append answered and cost.
fn append<'db, E: Engine>(
    db: &'db GroveDb<E>,
    n: usize,
    through: TransactionArg<'db, '_, E>,
) -> (String, OperationCost) {
    let appended = db.commitment_tree_insert(
        &POOL,
        NOTES,
        note(n, 0),
        note(n, 0xaa),
        note(n, 0xcc),
        ciphertext(n),
        through,
        version(),
    );
    let cost = appended.cost.clone();
    (format!("{:?}", appended.unwrap().expect("an append")), cost)
}

fn root<E: Engine>(db: &GroveDb<E>) -> [u8; 32] {
    db.root_hash(None, version()).unwrap().expect("a root hash")
}

/// The path query of the keys of `path`, each with what `under` selects
/// below it.
fn all_of(path: &[&[u8]], under: Option<Query>) -> PathQuery {
    let mut query = Query::new();
    query.insert_all();
    if let Some(under) = under {
        query.set_subquery(under);
    }
    let path = path.iter().map(|segment| segment.to_vec()).collect();
    PathQuery::new(path, SizedQuery::new(query, None, None))
}

/// One answer of a grove: what was asked, the answer as text, its cost.
type Answer = (String, String, OperationCost);

/// Takes in a call that has to succeed, and gives what it returned.
fn answered<T: std::fmt::Debug>(
    answers: &mut Vec<Answer>,
    name: &str,
    call: grovedb_costs::CostResult<T, crate::Error>,
) -> T {
    let cost = call.cost.clone();
    let value = call
        .unwrap()
        .unwrap_or_else(|error| panic!("{name}: {error:?}"));
    answers.push((name.to_owned(), format!("{value:?}"), cost));
    value
}

/// The reads and the proofs of the pool, each as text with its cost. Each
/// has to succeed, and to hold what the pool holds.
fn pool_answers<E: Engine>(db: &GroveDb<E>) -> Vec<Answer> {
    let v = version();
    let mut answers = Vec::new();
    let anchor = db.commitment_tree_anchor(&POOL, NOTES, None, v);
    answered(
        &mut answers,
        "anchor",
        anchor.map_ok(|anchor| anchor.to_bytes()),
    );
    let count = db.commitment_tree_count(&POOL, NOTES, None, v);
    assert_eq!(answered(&mut answers, "count", count), NOTE_COUNT as u64);
    let last = NOTE_COUNT as u64 - 1;
    for position in [0, 7, 8, 63, last, last + 1] {
        let value = db.commitment_tree_get_value(&POOL, NOTES, position, None, v);
        let value = answered(&mut answers, "value", value);
        assert_eq!(value.is_some(), position <= last, "the note at {position}");
    }
    for (start, limit, held) in [(0, 20, 20), (60, 30, 17), (0, u16::MAX, NOTE_COUNT)] {
        let page = db.commitment_tree_get_range(&POOL, NOTES, start, limit, None, v);
        let page = answered(&mut answers, "range", page);
        assert_eq!(page.entries.len(), held, "the notes from {start}");
    }
    let element = db.get_raw((&POOL).into(), NOTES, None, v);
    let bytes = element.map_ok(|element| element.serialize(v).expect("the bytes of an element"));
    answered(&mut answers, "element", bytes);
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    let read = db.query_raw(&all_of(&POOL, None), true, true, true, pairs, None, v);
    let (read, _) = answered(&mut answers, "the pool", read);
    assert_eq!(read.elements.len(), 5, "the children of the pool");

    let mut notes = Query::new();
    notes.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=last.to_be_bytes().to_vec());
    let mut the_notes = Query::new();
    the_notes.insert_key(NOTES.to_vec());
    the_notes.set_subquery(notes);
    let fetch = PathQuery::new(
        POOL.iter().map(|segment| segment.to_vec()).collect(),
        SizedQuery::new(the_notes, None, None),
    );
    for (name, query) in [
        ("proof of the pool", all_of(&POOL, None)),
        ("proof of the notes", fetch),
    ] {
        let proof = answered(&mut answers, name, db.prove_query(&query, None, v));
        let (proved, _) = GroveDb::verify_query(&proof, &query, v)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(proved, root(db), "{name}");
    }
    answers
}

/// Everything that one flow gave and that every flow has to give alike,
/// whatever the engines of its two groves.
#[derive(Debug, PartialEq)]
struct Flow {
    dump: Vec<u8>,
    dumped: DumpedSubtree,
    root: [u8; 32],
    answers: Vec<Answer>,
    appended: (String, OperationCost),
    root_after: [u8; 32],
}

/// Bakes the notes of a pool on `S` and applies them on `T`.
fn bake_and_apply<S: Hatched, T: Hatched>() -> Flow {
    let v = version();
    let names = format!("from {} to {}", S::NAME, T::NAME);

    // The bake: a pool that was filled append by append.
    let source = Grove::<S>::new();
    pool(&source.db, None);
    for n in 0..NOTE_COUNT {
        append(&source.db, n, None);
    }
    let mut dump = Vec::new();
    let dumped = (source.db)
        .dump_subtree(&POOL, NOTES, &mut dump, None, v)
        .expect("a dump");
    assert_eq!(dumped.bytes, dump.len() as u64);
    assert!(dumped.data_records > NOTE_COUNT as u64 / 8, "{dumped:?}");
    let element = source.db.get_raw((&POOL).into(), NOTES, None, v).unwrap();
    assert_eq!(element.expect("the notes"), dumped.element);

    // The apply, in the transaction that makes the trees of the pool, as
    // Platform's genesis does.
    let target = Grove::<T>::new();
    let tx = target.db.start_transaction();
    pool(&target.db, Some(&tx));
    let loaded = (target.db)
        .load_subtree(&POOL, NOTES, &dump[..], Some(&tx), v)
        .expect("a load");
    assert_eq!(loaded, dumped, "{names}");
    let made = (target.db)
        .subtree_root_as(&POOL, NOTES, &loaded.element, Some(&tx), v)
        .expect("the root of the loaded notes");
    assert_eq!(made, loaded.root, "{names}");
    (target.db)
        .replace_subtree_root(&POOL, NOTES, loaded.element, loaded.root, Some(&tx), v)
        .unwrap()
        .expect("the element of the notes");
    target.db.commit_transaction(tx).unwrap().expect("a commit");

    // The target is the source.
    let root_at_source = root(&source.db);
    assert_eq!(root(&target.db), root_at_source, "{names}");
    let issues = target.db.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty(), "{names}");
    let answers = pool_answers(&source.db);
    assert_eq!(pool_answers(&target.db), answers, "{names}");
    let mut again = Vec::new();
    (target.db)
        .dump_subtree(&POOL, NOTES, &mut again, None, v)
        .expect("a dump of the target");
    assert_eq!(again, dump, "{names}");

    // An append lands on both alike.
    let appended = append(&source.db, NOTE_COUNT, None);
    assert_eq!(append(&target.db, NOTE_COUNT, None), appended, "{names}");
    let root_after = root(&source.db);
    assert_eq!(root(&target.db), root_after, "{names}");
    assert_ne!(root_after, root_at_source);
    let issues = target.db.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty(), "{names}");

    source.close();
    target.close();
    Flow {
        dump,
        dumped,
        root: root_at_source,
        answers,
        appended,
        root_after,
    }
}

/// The acceptance test of GROVE-09: the bake and the apply of a devnet
/// shielded pool on LigninDB.
#[test]
fn the_notes_of_a_shielded_pool_are_baked_and_applied_on_lignindb() {
    let flow = bake_and_apply::<super::LigninEngine, super::LigninEngine>();
    assert_eq!(flow.dumped.aux_records, 0);
    assert_eq!(flow.dumped.root_key_records, 0);
}

/// The same flow on RocksDB and from each engine to the other gives the
/// bytes, the answers and the costs it gives on LigninDB: a dump names no
/// engine.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_dump_of_one_engine_is_applied_on_the_other() {
    use super::LigninEngine as L;
    use crate::RocksDbEngine as R;
    let on_lignin = bake_and_apply::<L, L>();
    assert_eq!(bake_and_apply::<R, R>(), on_lignin, "on RocksDB");
    assert_eq!(bake_and_apply::<R, L>(), on_lignin, "baked on RocksDB");
    assert_eq!(bake_and_apply::<L, R>(), on_lignin, "baked on LigninDB");
}

const COUNTED: &[u8] = b"counted";
const ITEMS: u32 = 4_000;

fn item_key(n: u32) -> Vec<u8> {
    let mut key = n.wrapping_mul(0x9e37_79b9).to_be_bytes().to_vec();
    key.extend_from_slice(b"-item");
    key
}

/// An item of 40 bytes, and every 250th of 5,000: a value that is too long
/// for its cell.
fn item(n: u32) -> Element {
    let length = if n % 250 == 17 { 5_000 } else { 40 };
    Element::new_item((0..length).map(|at| (at as u32 ^ n) as u8).collect())
}

/// A count tree under the root with `ITEMS` items, written by batches of
/// 400, an aux record, a meta record and a record of its root key.
fn counted<E: Engine>(db: &GroveDb<E>) {
    let v = version();
    (db.insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap())
    .expect("a count tree");
    for batch in (0..ITEMS).collect::<Vec<_>>().chunks(400) {
        let ops = batch.iter().map(|n| {
            QualifiedGroveDbOp::insert_or_replace_op(vec![COUNTED.to_vec()], item_key(*n), item(*n))
        });
        (db.apply_batch(ops.collect(), None, None, v).unwrap()).expect("a batch of items");
    }
    // The record of the root key holds the key of the root node, as the
    // record that a restore writes for a Merk does.
    let held = db.get_raw(ROOT, COUNTED, None, v).unwrap();
    let Element::CountTree(Some(root_key), ..) = held.expect("the count tree") else {
        panic!("a count tree with a root");
    };
    let tx = db.start_transaction();
    let batch = E::Batch::default();
    let handle = (db.engine())
        .subtree([COUNTED].as_ref().into(), Some(&batch), &tx)
        .unwrap();
    (handle
        .aux()
        .put(b"an aux key", b"an aux value", None)
        .unwrap())
    .expect("an aux record");
    (handle
        .meta()
        .put(b"a meta key", b"a meta value", None)
        .unwrap())
    .expect("a meta record");
    let mut change_set = handle.change_set();
    (change_set.put_root_key(&root_key, None)).expect("a record of the root key");
    (handle.commit(change_set).unwrap()).expect("a change set");
    drop(handle);
    (db.engine().apply_batch(batch, &tx).unwrap()).expect("the records beside the nodes");
    db.commit_transaction(tx).unwrap().expect("a commit");
}

fn counted_answers<E: Engine>(db: &GroveDb<E>) -> Vec<Answer> {
    let v = version();
    let mut answers = Vec::new();
    let element = db.get_raw(ROOT, COUNTED, None, v);
    let bytes = element.map_ok(|element| element.serialize(v).expect("the bytes of an element"));
    answered(&mut answers, "element", bytes);
    for n in [0, 17, 267, ITEMS - 1] {
        let read = db.get([COUNTED].as_ref(), &item_key(n), None, v);
        assert_eq!(answered(&mut answers, "item", read), item(n));
    }
    // An item that is not there is not found, at the same cost.
    let absent = db.get([COUNTED].as_ref(), &item_key(ITEMS), None, v);
    assert!(
        matches!(absent.value, Err(crate::Error::PathKeyNotFound(_))),
        "{:?}",
        absent.value
    );
    answers.push((
        "no item".to_owned(),
        format!("{:?}", absent.value),
        absent.cost,
    ));

    let mut some = Query::new();
    some.insert_range(vec![0x40]..vec![0x60]);
    let path = vec![COUNTED.to_vec()];
    let query = PathQuery::new(path, SizedQuery::new(some, Some(300), None));
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    let read = db.query_raw(&query, true, true, true, pairs, None, v);
    let (read, _) = answered(&mut answers, "a range of items", read);
    assert_eq!(
        read.elements.len(),
        300,
        "the items of the range, up to the limit"
    );
    let proof = answered(
        &mut answers,
        "proof of the range",
        db.prove_query(&query, None, v),
    );
    let (proved, rows) = GroveDb::verify_query(&proof, &query, v).expect("a proof that verifies");
    assert_eq!((proved, rows.len()), (root(db), 300));
    answers
}

#[derive(Debug, PartialEq)]
struct Counted {
    dump: Vec<u8>,
    root: [u8; 32],
    answers: Vec<Answer>,
    inserted: OperationCost,
    root_after: [u8; 32],
}

/// Dumps a count tree of `S` and loads it on `T`, with no transaction.
/// Gives what both groves gave alike, and the pages of each where its
/// engine counts them.
fn dump_and_load_counted<S: Hatched, T: Hatched>() -> (Counted, Option<Census>, Option<Census>) {
    let v = version();
    let names = format!("from {} to {}", S::NAME, T::NAME);
    let source = Grove::<S>::new();
    counted(&source.db);
    let mut dump = Vec::new();
    let dumped = (source.db)
        .dump_subtree(ROOT, COUNTED, &mut dump, None, v)
        .expect("a dump");
    let records = [
        dumped.data_records,
        dumped.aux_records,
        dumped.root_key_records,
        dumped.meta_records,
    ];
    assert_eq!(records, [u64::from(ITEMS), 1, 1, 1], "{names}");

    let target = Grove::<T>::new();
    (target.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    let loaded = (target.db)
        .load_subtree(ROOT, COUNTED, &dump[..], None, v)
        .expect("a load");
    assert_eq!(loaded, dumped, "{names}");
    let made = (target.db)
        .subtree_root_as(ROOT, COUNTED, &loaded.element, None, v)
        .expect("the root of the loaded Merk");
    assert_eq!(made, loaded.root, "{names}");
    (target.db)
        .replace_subtree_root(ROOT, COUNTED, loaded.element, loaded.root, None, v)
        .unwrap()
        .expect("the element of the count tree");

    let root_at_source = root(&source.db);
    assert_eq!(root(&target.db), root_at_source, "{names}");
    let issues = target.db.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty(), "{names}");
    let answers = counted_answers(&source.db);
    assert_eq!(counted_answers(&target.db), answers, "{names}");
    let mut again = Vec::new();
    (target.db)
        .dump_subtree(ROOT, COUNTED, &mut again, None, v)
        .expect("a dump of the target");
    assert_eq!(again, dump, "{names}");

    let insert = |db: &GroveDb<_>| {
        let inserted = db.insert([COUNTED].as_ref(), b"one more", item(1), None, None, v);
        let cost = inserted.cost.clone();
        inserted.unwrap().expect("an insert");
        cost
    };
    let inserted = insert(&source.db);
    let at_target = {
        let inserted = (target.db).insert([COUNTED].as_ref(), b"one more", item(1), None, None, v);
        let cost = inserted.cost.clone();
        inserted.unwrap().expect("an insert");
        cost
    };
    assert_eq!(at_target, inserted, "{names}");
    let root_after = root(&source.db);
    assert_eq!(root(&target.db), root_after, "{names}");

    let pages = (source.close(), target.close());
    let outcome = Counted {
        dump,
        root: root_at_source,
        answers,
        inserted,
        root_after,
    };
    (outcome, pages.0, pages.1)
}

/// A tree with Merk nodes below it goes through the pair, handed to the
/// engine as one whole change set: its pages are the loader's.
#[test]
fn a_count_tree_is_loaded_whole_and_its_pages_are_the_loaders() {
    let (_, grown, loaded) = dump_and_load_counted::<super::LigninEngine, super::LigninEngine>();
    let (grown, loaded) = (grown.expect("a census"), loaded.expect("a census"));
    eprintln!(
        "grown node by node: {} tree pages, {} cells; loaded: {} tree pages, {} cells",
        grown.tree_pages, grown.cells, loaded.tree_pages, loaded.cells
    );
    // The same cells: the items, the count tree and the root of the grove.
    assert_eq!(loaded.live_cells, u64::from(ITEMS) + 2, "{loaded:?}");
    assert_eq!(grown.live_cells, loaded.live_cells, "{grown:?}");
    // The element names the Merk it hosts in both, as its own cell does
    // for the Merk that grew under it.
    assert_eq!(grown.hosted_roots, 1, "{grown:?}");
    assert_eq!(loaded.hosted_roots, 1, "{loaded:?}");
    // In at least a tenth fewer pages than the Merk that grew node by
    // node, which splits its pages as it grows (`tree-layout.md`).
    assert!(
        loaded.tree_pages * 10 <= grown.tree_pages * 9,
        "{loaded:?} {grown:?}"
    );
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_count_tree_of_one_engine_is_loaded_on_the_other() {
    use super::LigninEngine as L;
    use crate::RocksDbEngine as R;
    let (on_lignin, ..) = dump_and_load_counted::<L, L>();
    assert_eq!(dump_and_load_counted::<R, R>().0, on_lignin, "on RocksDB");
    assert_eq!(dump_and_load_counted::<R, L>().0, on_lignin, "from RocksDB");
    assert_eq!(
        dump_and_load_counted::<L, R>().0,
        on_lignin,
        "from LigninDB"
    );
}

/// The dump of a small count tree: `items` items under `COUNTED`.
fn small_dump<E: Hatched>(items: &[&[u8]]) -> Vec<u8> {
    let v = version();
    let source = Grove::<E>::new();
    (source.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    for key in items {
        let item = Element::new_item(key.to_vec());
        (source.db)
            .insert([COUNTED].as_ref(), key, item, None, None, v)
            .unwrap()
            .expect("an item");
    }
    let mut dump = Vec::new();
    (source.db)
        .dump_subtree(ROOT, COUNTED, &mut dump, None, v)
        .expect("a dump");
    source.close();
    dump
}

/// The keys of the records that the data keyspace of the tree under
/// `COUNTED` holds, as `through` reads it: of its nodes, or with `flat` of
/// its flat records.
fn keys_of<'db, E: Engine>(
    db: &'db GroveDb<E>,
    through: TransactionArg<'db, '_, E>,
    flat: bool,
) -> Vec<Vec<u8>> {
    fn keys<C: RawIterator>(mut cursor: C) -> Vec<Vec<u8>> {
        let mut keys = Vec::new();
        cursor.seek_to_first().unwrap();
        while cursor.valid().unwrap() {
            keys.push(cursor.key().unwrap().expect("a key").to_vec());
            cursor.next().unwrap();
        }
        cursor.status().expect("a cursor that did not fail");
        keys
    }
    let own;
    let tx = match through {
        Some(tx) => tx,
        None => {
            own = db.start_transaction();
            &own
        }
    };
    let handle = (db.engine())
        .subtree([COUNTED].as_ref().into(), None, tx)
        .unwrap();
    if flat {
        keys(FlatStore::cursor(&handle))
    } else {
        keys(NodeStore::cursor(&handle))
    }
}

/// How many records that keyspace holds.
fn data_of<'db, E: Engine>(
    db: &'db GroveDb<E>,
    through: TransactionArg<'db, '_, E>,
    flat: bool,
) -> u64 {
    keys_of(db, through, flat).len() as u64
}

fn records_of<'db, E: Engine>(db: &'db GroveDb<E>, through: TransactionArg<'db, '_, E>) -> u64 {
    data_of(db, through, false)
}

/// A load is a write of its transaction: nothing outside the transaction
/// reads it before the commit, and it leaves with a rollback.
/// `ingest_subtree_sst` of the RocksDB engine writes past the transaction.
fn a_load_is_of_its_transaction<E: Hatched>() {
    let v = version();
    let dump = small_dump::<E>(&[b"a", b"b", b"c"]);
    let target = Grove::<E>::new();
    (target.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    let tx = target.db.start_transaction();
    (target.db)
        .load_subtree(ROOT, COUNTED, &dump[..], Some(&tx), v)
        .expect("a load");
    assert_eq!(
        (
            records_of(&target.db, Some(&tx)),
            records_of(&target.db, None)
        ),
        (3, 0),
        "{}",
        E::NAME
    );
    // The subtree holds records now, as the transaction reads it.
    let again = target
        .db
        .load_subtree(ROOT, COUNTED, &dump[..], Some(&tx), v);
    assert!(matches!(again, Err(DumpLoadError::Refused(_))), "{again:?}");
    target.db.rollback_transaction(&tx).expect("a rollback");
    assert_eq!(
        (
            records_of(&target.db, Some(&tx)),
            records_of(&target.db, None)
        ),
        (0, 0),
        "{}",
        E::NAME
    );

    let loaded = (target.db)
        .load_subtree(ROOT, COUNTED, &dump[..], Some(&tx), v)
        .expect("a load after the rollback");
    (target.db)
        .replace_subtree_root(ROOT, COUNTED, loaded.element, loaded.root, Some(&tx), v)
        .unwrap()
        .expect("the element");
    // A dump through the transaction is of what the transaction reads,
    // and a dump with no transaction of what is committed.
    let through = (target.db)
        .dump_subtree(ROOT, COUNTED, Vec::new(), Some(&tx), v)
        .expect("a dump through the transaction");
    let outside = (target.db)
        .dump_subtree(ROOT, COUNTED, Vec::new(), None, v)
        .expect("a dump with no transaction");
    assert_eq!(
        (through.data_records, outside.data_records),
        (3, 0),
        "{}",
        E::NAME
    );
    target.db.commit_transaction(tx).unwrap().expect("a commit");
    assert_eq!(records_of(&target.db, None), 3, "{}", E::NAME);
    let issues = target.db.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty());
    target.close();
}

#[test]
fn a_load_is_a_write_of_its_transaction_on_lignindb() {
    a_load_is_of_its_transaction::<super::LigninEngine>();
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_load_is_a_write_of_its_transaction_on_rocksdb() {
    a_load_is_of_its_transaction::<crate::RocksDbEngine>();
}

/// A stream that inserts an item into the tree under `COUNTED`, with no
/// transaction, when its first bytes arrive.
struct CommitsBeside<'db, E: Engine> {
    db: &'db GroveDb<E>,
    taken: Vec<u8>,
    committed: bool,
}

impl<E: Engine> std::io::Write for CommitsBeside<'_, E> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.committed {
            self.committed = true;
            let late = Element::new_item(b"late".to_vec());
            (self.db)
                .insert([COUNTED].as_ref(), b"late", late, None, None, version())
                .unwrap()
                .expect("an insert beside the dump");
        }
        self.taken.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A dump with no transaction is of the state that was committed when it
/// began: a commit that lands after its first bytes are out is in none of
/// its records.
fn a_dump_is_of_the_state_it_began_on<E: Hatched>() {
    let v = version();
    let grove = Grove::<E>::new();
    (grove.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    for key in [&b"a"[..], b"b", b"c"] {
        let item = Element::new_item(key.to_vec());
        (grove.db)
            .insert([COUNTED].as_ref(), key, item, None, None, v)
            .unwrap()
            .expect("an item");
    }
    let mut before = Vec::new();
    (grove.db)
        .dump_subtree(ROOT, COUNTED, &mut before, None, v)
        .expect("a dump");

    let mut out = CommitsBeside {
        db: &grove.db,
        taken: Vec::new(),
        committed: false,
    };
    let dumped = (grove.db)
        .dump_subtree(ROOT, COUNTED, &mut out, None, v)
        .expect("a dump beside a commit");
    assert!(out.committed, "{}", E::NAME);
    assert_eq!(dumped.data_records, 3, "{}", E::NAME);
    assert_eq!(out.taken, before, "{}", E::NAME);
    assert_eq!(records_of(&grove.db, None), 4, "{}", E::NAME);
    grove.close();
}

#[test]
fn a_dump_is_of_the_state_it_began_on_on_lignindb() {
    a_dump_is_of_the_state_it_began_on::<super::LigninEngine>();
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_dump_is_of_the_state_it_began_on_on_rocksdb() {
    a_dump_is_of_the_state_it_began_on::<crate::RocksDbEngine>();
}

/// A load through a snapshot read transaction is refused by the engine,
/// for a Merk and for a tree that keeps its data flat, and writes nothing.
fn a_load_through_a_snapshot_is_refused<E: Hatched>() {
    let v = version();
    let nodes = small_dump::<E>(&[b"a", b"b", b"c"]);
    let flat = {
        let source = Grove::<E>::new();
        (source.db)
            .insert(ROOT, COUNTED, Element::empty_mmr_tree(), None, None, v)
            .unwrap()
            .expect("an MMR tree");
        for n in 0..5u8 {
            (source.db)
                .mmr_tree_append(ROOT, COUNTED, vec![n; 20], None, v)
                .unwrap()
                .expect("an append");
        }
        let mut dump = Vec::new();
        (source.db)
            .dump_subtree(ROOT, COUNTED, &mut dump, None, v)
            .expect("a dump");
        source.close();
        dump
    };
    for (empty, dump, is_flat) in [
        (Element::empty_count_tree(), nodes, false),
        (Element::empty_mmr_tree(), flat, true),
    ] {
        let target = Grove::<E>::new();
        (target.db)
            .insert(ROOT, COUNTED, empty, None, None, v)
            .unwrap()
            .expect("an empty tree");
        let snapshot = target.db.start_snapshot_read_transaction();
        let loaded = (target.db).load_subtree(ROOT, COUNTED, &dump[..], Some(&snapshot), v);
        match loaded {
            Err(DumpLoadError::Grove(error)) => {
                let said = format!("{error:?}");
                assert!(said.contains("SnapshotReadOnlyTransaction"), "{said}");
            }
            other => panic!("{}: {other:?}", E::NAME),
        }
        drop(snapshot);
        assert_eq!(data_of(&target.db, None, is_flat), 0, "{}", E::NAME);
        let loaded = (target.db)
            .load_subtree(ROOT, COUNTED, &dump[..], None, v)
            .expect("a load with no transaction");
        assert!(loaded.data_records > 0, "{}", E::NAME);
        (target.db)
            .replace_subtree_root(ROOT, COUNTED, loaded.element, loaded.root, None, v)
            .unwrap()
            .expect("the element");
        let issues = target.db.verify_grovedb(None, true, false, v);
        assert!(issues.expect("a verification").is_empty(), "{}", E::NAME);
        target.close();
    }
}

#[test]
fn a_load_through_a_snapshot_is_refused_on_lignindb() {
    a_load_through_a_snapshot_is_refused::<super::LigninEngine>();
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_load_through_a_snapshot_is_refused_on_rocksdb() {
    a_load_through_a_snapshot_is_refused::<crate::RocksDbEngine>();
}

/// A transaction writes an item into a tree that holds none, and a load
/// with no transaction fills the tree beside it and commits. Gives how
/// the transaction's commit ended, and the nodes the tree holds after it.
fn a_load_beside_a_writer_of_its_subtree<E: Hatched>() -> (Result<(), crate::Error>, u64) {
    let v = version();
    let dump = small_dump::<E>(&[b"a", b"b", b"c"]);
    let target = Grove::<E>::new();
    (target.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    let tx = target.db.start_transaction();
    let other = Element::new_item(b"of the transaction".to_vec());
    (target.db)
        .insert([COUNTED].as_ref(), b"z", other, None, Some(&tx), v)
        .unwrap()
        .expect("an insert in the transaction");
    let loaded = (target.db)
        .load_subtree(ROOT, COUNTED, &dump[..], None, v)
        .expect("a load beside the transaction");
    let committed = target.db.commit_transaction(tx).unwrap();
    let nodes = records_of(&target.db, None);
    if committed.is_err() {
        finish(&target.db, loaded);
    }
    target.close();
    (committed, nodes)
}

/// Two transactions each load a dump into one empty tree, with no node
/// key in common and with no replacement of the element, and commit one
/// after the other. Gives how the second commit ended, and the nodes the
/// tree holds after it.
fn two_loads_into_one_subtree<E: Hatched>() -> (Result<(), crate::Error>, u64) {
    let v = version();
    let one = small_dump::<E>(&[b"a", b"b", b"c"]);
    let other = small_dump::<E>(&[b"x", b"y"]);
    let target = Grove::<E>::new();
    (target.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    let first = target.db.start_transaction();
    let second = target.db.start_transaction();
    let loaded = (target.db)
        .load_subtree(ROOT, COUNTED, &one[..], Some(&first), v)
        .expect("a load");
    (target.db)
        .load_subtree(ROOT, COUNTED, &other[..], Some(&second), v)
        .expect("a load beside it");
    (target.db.commit_transaction(first).unwrap()).expect("the first commit");
    let committed = target.db.commit_transaction(second).unwrap();
    let nodes = records_of(&target.db, None);
    if committed.is_err() {
        finish(&target.db, loaded);
    }
    target.close();
    (committed, nodes)
}

/// A LigninDB store that is closed after the load of a Merk with nodes
/// and before the replacement of its element: the physical checker has
/// one finding, of the rule for a value hash, and follows one hosted
/// root. With the element replaced, the grove verifies and the checker
/// passes the store.
#[test]
fn a_store_between_a_load_and_its_replacement_is_reported_for_one_value_hash() {
    let v = version();
    let dump = small_dump::<super::LigninEngine>(&[b"a", b"b", b"c"]);
    let target = Grove::<super::LigninEngine>::new();
    (target.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    let loaded = (target.db)
        .load_subtree(ROOT, COUNTED, &dump[..], None, v)
        .expect("a load");
    let Grove { dir, db } = target;
    // The check that a debug build makes at a close would stop the test
    // on the same finding.
    db.engine().database().unchecked_at_close();
    drop(db);

    let store = Db::open(dir.path(), &Options::default()).expect("the store opens again");
    let mut rules = grovedb_merk::store::lignin::hash::MerkHashRules::for_check(v);
    let report = store.close_and_verify(Some(&mut rules)).expect("a close");
    let found: Vec<String> = (report.findings.iter())
        .map(|finding| format!("{:?}", finding.rule))
        .collect();
    assert_eq!(found, ["Hash(Value)"], "{:#?}", report.findings);
    assert_eq!(report.census.hosted_roots, 1, "{:?}", report.census);

    let db = Lignin::open_with_engine(dir.path()).expect("the grove opens again");
    root_is_as_stated(&db, &loaded, None);
    replace(&db, &loaded, None);
    let issues = db.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty());
    drop(db);
    let census = <super::LigninEngine as Hatched>::closed(dir.path()).expect("a census");
    assert_eq!(census.hosted_roots, 1, "{census:?}");
}

/// Replaces the element of the tree under `COUNTED` after a load that was
/// committed without it, and has the grove verified. The two tests that
/// commit a load alone call it where the other writer lost, which is on
/// LigninDB: the physical checker of a close passes no grove that is left
/// between a load of nodes and its replacement.
fn finish<E: Engine>(db: &GroveDb<E>, loaded: DumpedSubtree) {
    let v = version();
    db.replace_subtree_root(ROOT, COUNTED, loaded.element, loaded.root, None, v)
        .unwrap()
        .expect("the element");
    let issues = db.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty());
}

/// Two transactions each apply a dump to one empty tree, the load and the
/// replacement of the element, both before either commits, and commit one
/// after the other. The second commit is a conflict on both engines: both
/// wrote the node of the element while both were open.
fn the_second_of_two_applies_conflicts<E: Hatched>() {
    let v = version();
    let one = small_dump::<E>(&[b"a", b"b", b"c"]);
    let other = small_dump::<E>(&[b"x", b"y"]);
    let target = Grove::<E>::new();
    (target.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    let first = target.db.start_transaction();
    let second = target.db.start_transaction();
    for (dump, tx) in [(&one, &first), (&other, &second)] {
        let loaded = (target.db)
            .load_subtree(ROOT, COUNTED, &dump[..], Some(tx), v)
            .expect("a load");
        root_is_as_stated(&target.db, &loaded, Some(tx));
        replace(&target.db, &loaded, Some(tx));
    }
    (target.db.commit_transaction(first).unwrap()).expect("the first commit");
    let error = (target.db.commit_transaction(second).unwrap()).expect_err("a conflict");
    assert!(error.is_commit_conflict(), "{}: {error:?}", E::NAME);
    assert_eq!(records_of(&target.db, None), 3, "{}", E::NAME);
    let issues = target.db.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty(), "{}", E::NAME);
    target.close();
}

/// Two transactions each load a dump into one empty tree, with no key in
/// common, and each finds the root its dump states. The first replaces
/// the element and commits, and only then does the second replace the
/// element, and commit. Gives how the second commit ended, the nodes the
/// tree holds after it, and whether its element is then the one of the
/// first dump, and of the second.
fn an_apply_whose_replacement_follows_the_commit_of_another<E: Hatched>(
) -> (Result<(), crate::Error>, Vec<Vec<u8>>, [bool; 2]) {
    let v = version();
    let one = small_dump::<E>(&[b"a", b"b", b"c"]);
    let other = small_dump::<E>(&[b"x", b"y"]);
    let target = Grove::<E>::new();
    (target.db)
        .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
        .unwrap()
        .expect("a count tree");
    let first = target.db.start_transaction();
    let second = target.db.start_transaction();
    let loaded = (target.db)
        .load_subtree(ROOT, COUNTED, &one[..], Some(&first), v)
        .expect("a load");
    let beside = (target.db)
        .load_subtree(ROOT, COUNTED, &other[..], Some(&second), v)
        .expect("a load beside it");
    assert_ne!(loaded.element, beside.element);
    root_is_as_stated(&target.db, &loaded, Some(&first));
    root_is_as_stated(&target.db, &beside, Some(&second));
    replace(&target.db, &loaded, Some(&first));
    (target.db.commit_transaction(first).unwrap()).expect("the first commit");
    replace(&target.db, &beside, Some(&second));
    let committed = target.db.commit_transaction(second).unwrap();
    let nodes = keys_of(&target.db, None, false);
    let held = (target.db.get_raw(ROOT, COUNTED, None, v).unwrap()).expect("the count tree");
    target.close();
    (
        committed,
        nodes,
        [held == loaded.element, held == beside.element],
    )
}

/// The second step of an apply: the root that the loaded records give, as
/// `through` reads them, is the root the dump states.
fn root_is_as_stated<'db, E: Engine>(
    db: &'db GroveDb<E>,
    loaded: &DumpedSubtree,
    through: TransactionArg<'db, '_, E>,
) {
    let made = db
        .subtree_root_as(ROOT, COUNTED, &loaded.element, through, version())
        .expect("the root of the loaded Merk");
    assert_eq!(made, loaded.root);
}

/// The third step of an apply: the element of the dump takes the place of
/// the one under `COUNTED`.
fn replace<'db, E: Engine>(
    db: &'db GroveDb<E>,
    loaded: &DumpedSubtree,
    through: TransactionArg<'db, '_, E>,
) {
    let element = loaded.element.clone();
    db.replace_subtree_root(ROOT, COUNTED, element, loaded.root, through, version())
        .unwrap()
        .expect("the element");
}

/// On LigninDB the second apply does not commit, whenever it replaces.
#[test]
fn an_apply_whose_replacement_follows_another_commit_conflicts_on_lignindb() {
    let (committed, nodes, element_of) =
        an_apply_whose_replacement_follows_the_commit_of_another::<super::LigninEngine>();
    let error = committed.expect_err("a conflict");
    assert!(error.is_commit_conflict(), "{error:?}");
    assert_eq!(nodes, [b"a", b"b", b"c"], "the nodes of the first apply");
    assert_eq!(element_of, [true, false], "the element of the first apply");
}

/// On RocksDB it commits: it wrote the node of the element after the other
/// commit, and no key of its load is a key of the other. The tree holds
/// the nodes of both under the element of the second.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn an_apply_whose_replacement_follows_another_commit_commits_on_rocksdb() {
    let (committed, nodes, element_of) =
        an_apply_whose_replacement_follows_the_commit_of_another::<crate::RocksDbEngine>();
    committed.expect("a commit");
    assert_eq!(
        nodes,
        [b"a", b"b", b"c", b"x", b"y"],
        "the nodes of both applies"
    );
    assert_eq!(element_of, [false, true], "the element of the second apply");
}

#[test]
fn the_second_of_two_applies_conflicts_on_lignindb() {
    the_second_of_two_applies_conflicts::<super::LigninEngine>();
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn the_second_of_two_applies_conflicts_on_rocksdb() {
    the_second_of_two_applies_conflicts::<crate::RocksDbEngine>();
}

/// On LigninDB the second of two loads into one subtree does not commit.
#[test]
fn the_second_of_two_loads_into_one_subtree_conflicts_on_lignindb() {
    let (committed, nodes) = two_loads_into_one_subtree::<super::LigninEngine>();
    let error = committed.expect_err("a conflict");
    assert!(error.is_commit_conflict(), "{error:?}");
    assert_eq!(nodes, 3, "the nodes of the first load");
}

/// On RocksDB both commit, and the tree holds the nodes of two Merks: a
/// caller lets one writer at a time into the subtree it applies a dump to.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn two_loads_into_one_subtree_both_commit_on_rocksdb() {
    let (committed, nodes) = two_loads_into_one_subtree::<crate::RocksDbEngine>();
    committed.expect("a commit");
    assert_eq!(nodes, 5, "the nodes of both loads");
}

/// The difference that `engine.md` records ("Dump and load"): on LigninDB
/// a load is a range over the nodes of its subtree to the conflict rule
/// (`transactions.md` LD6, CF5), and the transaction that wrote a node
/// there does not commit.
#[test]
fn a_transaction_that_wrote_into_a_loaded_subtree_conflicts_on_lignindb() {
    let (committed, nodes) = a_load_beside_a_writer_of_its_subtree::<super::LigninEngine>();
    let error = committed.expect_err("a conflict");
    assert!(error.is_commit_conflict(), "{error:?}");
    assert_eq!(nodes, 3, "the nodes of the load");
}

/// On RocksDB the two meet by the keys they wrote, and here they wrote
/// none in common: the transaction commits over the load, and the tree
/// holds the nodes of both under an element that names one.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_transaction_that_wrote_into_a_loaded_subtree_commits_on_rocksdb() {
    let (committed, nodes) = a_load_beside_a_writer_of_its_subtree::<crate::RocksDbEngine>();
    committed.expect("a commit");
    assert_eq!(
        nodes, 4,
        "the nodes of the load and the one of the transaction"
    );
}

/// The resident memory of this process and the most it was since the mark
/// was last cleared, in bytes.
#[cfg(target_os = "linux")]
fn resident() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").expect("the status");
    let kilobytes = |name: &str| {
        let line = status.lines().find(|line| line.starts_with(name));
        let number = line.and_then(|line| line.split_whitespace().nth(1));
        number
            .and_then(|number| number.parse::<u64>().ok())
            .expect("a size")
            * 1024
    };
    (kilobytes("VmRSS:"), kilobytes("VmHWM:"))
}

/// A measurement, not a check: what a load of a large Merk holds. It is
/// run alone, once to bake and once to load, so that the memory of the
/// bake is not in the reading of the load:
///
/// ```text
/// GROVE09_DUMP=/scratch/merk.dump GROVE09_NODES=250000 GROVE09_VALUE=400 \
///     GROVE09_ENGINE=lignin <test binary> --ignored --exact --nocapture \
///     engine::lignin::tests::hatch::the_memory_of_a_load_of_a_large_merk
/// ```
///
/// With no file at `GROVE09_DUMP` it makes a count tree of `GROVE09_NODES`
/// items of `GROVE09_VALUE` bytes on RocksDB and dumps it there. With the
/// file it loads it into a grove of `GROVE09_ENGINE` in `GROVE09_TARGET`,
/// which it leaves in place, and prints the readings.
#[cfg(all(target_os = "linux", feature = "rocksdb_storage"))]
#[test]
#[ignore = "a measurement, run alone with its variables set"]
fn the_memory_of_a_load_of_a_large_merk() {
    let variable = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is set"));
    let dump = std::path::PathBuf::from(variable("GROVE09_DUMP"));
    let v = version();
    if !dump.exists() {
        let nodes: u32 = variable("GROVE09_NODES").parse().expect("a number");
        let value: usize = variable("GROVE09_VALUE").parse().expect("a number");
        let source = Grove::<crate::RocksDbEngine>::new();
        (source.db)
            .insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
            .unwrap()
            .expect("a count tree");
        for batch in (0..nodes).collect::<Vec<_>>().chunks(2_000) {
            let ops = batch.iter().map(|n| {
                let bytes = (0..value).map(|at| (at as u32 ^ n) as u8).collect();
                let item = Element::new_item(bytes);
                QualifiedGroveDbOp::insert_or_replace_op(vec![COUNTED.to_vec()], item_key(*n), item)
            });
            (source.db.apply_batch(ops.collect(), None, None, v).unwrap()).expect("a batch");
        }
        let out = std::io::BufWriter::new(std::fs::File::create(&dump).expect("a file"));
        let dumped = (source.db)
            .dump_subtree(ROOT, COUNTED, out, None, v)
            .expect("a dump");
        println!(
            "baked {} nodes in {} bytes, root {:?}",
            dumped.data_records,
            dumped.bytes,
            root(&source.db)
        );
        return;
    }
    fn load<E: Hatched>(dump: &std::path::Path, dir: &std::path::Path) {
        let v = version();
        let db = GroveDb::<E>::open_with_engine(dir).expect("a database");
        db.insert(ROOT, COUNTED, Element::empty_count_tree(), None, None, v)
            .unwrap()
            .expect("a count tree");
        std::fs::write("/proc/self/clear_refs", "5").expect("the mark is cleared");
        let (before, _) = resident();
        let tx = db.start_transaction();
        let input = std::io::BufReader::new(std::fs::File::open(dump).expect("the dump"));
        let loaded = db
            .load_subtree(ROOT, COUNTED, input, Some(&tx), v)
            .expect("a load");
        let (after_load, most_in_load) = resident();
        db.replace_subtree_root(ROOT, COUNTED, loaded.element, loaded.root, Some(&tx), v)
            .unwrap()
            .expect("the element");
        db.commit_transaction(tx).unwrap().expect("a commit");
        let (after_commit, most) = resident();
        let nodes = loaded.data_records;
        let each = |bytes: u64| bytes.saturating_sub(before) / nodes;
        println!(
            "{}: {nodes} nodes, a dump of {} bytes ({} for a node); bytes for a node over the \
             {before} resident before the load: {} at most in the load, {} after it, {} at most \
             to the end of the commit, {} after the commit; root {:?}",
            E::NAME,
            loaded.bytes,
            loaded.bytes / nodes,
            each(most_in_load),
            each(after_load),
            each(most),
            each(after_commit),
            root(&db)
        );
    }
    let dir = std::path::PathBuf::from(variable("GROVE09_TARGET"));
    match variable("GROVE09_ENGINE").as_str() {
        "lignin" => load::<super::LigninEngine>(&dump, &dir),
        "rocksdb" => load::<crate::RocksDbEngine>(&dump, &dir),
        other => panic!("no engine {other}"),
    }
}
