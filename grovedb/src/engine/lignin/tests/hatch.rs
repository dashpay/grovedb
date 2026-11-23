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
use grovedb_storage::FlatStore;
use lignindb::{verify::Census, Db, Options};

use super::{version, ROOT};
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
    dir: tempfile::TempDir,
    db: GroveDb<E>,
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

/// The reads and the proofs of the pool, each as text with its cost.
fn pool_answers<E: Engine>(db: &GroveDb<E>) -> Vec<(String, String, OperationCost)> {
    let v = version();
    let mut answers = Vec::new();
    let mut said = |name: &str, answer: String, cost: OperationCost| {
        answers.push((name.to_owned(), answer, cost));
    };
    let anchor = db.commitment_tree_anchor(&POOL, NOTES, None, v);
    let cost = anchor.cost.clone();
    let anchor = anchor.unwrap().expect("an anchor").to_bytes();
    said("anchor", format!("{anchor:?}"), cost);
    let count = db.commitment_tree_count(&POOL, NOTES, None, v);
    said("count", format!("{:?}", count.value), count.cost);
    let last = NOTE_COUNT as u64 - 1;
    for position in [0, 7, 8, 63, last, last + 1] {
        let value = db.commitment_tree_get_value(&POOL, NOTES, position, None, v);
        said("value", format!("{:?}", value.value), value.cost);
    }
    for (start, limit) in [(0, 20), (60, 30), (0, u16::MAX)] {
        let page = db.commitment_tree_get_range(&POOL, NOTES, start, limit, None, v);
        said("range", format!("{:?}", page.value), page.cost);
    }
    let element = db.get_raw((&POOL).into(), NOTES, None, v);
    let bytes = element.value.map(|element| element.serialize(v));
    said("element", format!("{bytes:?}"), element.cost);
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    let read = db.query_raw(&all_of(&POOL, None), true, true, true, pairs, None, v);
    said("the pool", format!("{:?}", read.value), read.cost);

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
        let proof = db.prove_query(&query, None, v);
        said(name, format!("{:?}", proof.value), proof.cost);
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
    answers: Vec<(String, String, OperationCost)>,
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
    drop(handle);
    (db.engine().apply_batch(batch, &tx).unwrap()).expect("the records beside the nodes");
    db.commit_transaction(tx).unwrap().expect("a commit");
}

fn counted_answers<E: Engine>(db: &GroveDb<E>) -> Vec<(String, OperationCost)> {
    let v = version();
    let mut answers = Vec::new();
    let element = db.get_raw(ROOT, COUNTED, None, v);
    let bytes = element.value.map(|element| element.serialize(v));
    answers.push((format!("{bytes:?}"), element.cost));
    for n in [0, 17, 267, ITEMS - 1, ITEMS] {
        let read = db.get([COUNTED].as_ref(), &item_key(n), None, v);
        answers.push((format!("{:?}", read.value), read.cost));
    }
    let mut some = Query::new();
    some.insert_range(item_key(9)..item_key(10));
    let path = vec![COUNTED.to_vec()];
    let query = PathQuery::new(path, SizedQuery::new(some, Some(300), None));
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    let read = db.query_raw(&query, true, true, true, pairs, None, v);
    answers.push((format!("{:?}", read.value), read.cost));
    let proof = db.prove_query(&query, None, v);
    answers.push((format!("{:?}", proof.value), proof.cost));
    answers
}

#[derive(Debug, PartialEq)]
struct Counted {
    dump: Vec<u8>,
    root: [u8; 32],
    answers: Vec<(String, OperationCost)>,
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
    let records = [dumped.data_records, dumped.aux_records, dumped.meta_records];
    assert_eq!(records, [u64::from(ITEMS), 1, 1], "{names}");

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

/// How many nodes the tree under `COUNTED` holds, as `through` reads it.
fn records_of<'db, E: Engine>(db: &'db GroveDb<E>, through: TransactionArg<'db, '_, E>) -> u64 {
    let dumped = db.dump_subtree(ROOT, COUNTED, Vec::new(), through, version());
    dumped.expect("a dump").data_records
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

/// A transaction writes an item into a tree that holds none, and a load
/// with no transaction fills the tree beside it and commits. Gives how
/// the transaction's commit ended.
fn a_load_beside_a_writer_of_its_subtree<E: Hatched>() -> Result<(), crate::Error> {
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
    (target.db)
        .load_subtree(ROOT, COUNTED, &dump[..], None, v)
        .expect("a load beside the transaction");
    let committed = target.db.commit_transaction(tx).unwrap();
    drop(target);
    committed
}

/// The difference that `engine.md` records ("Dump and load"): on LigninDB
/// a load is a range over the nodes of its subtree to the conflict rule
/// (`transactions.md` LD6, CF5), and the transaction that wrote a node
/// there does not commit.
#[test]
fn a_transaction_that_wrote_into_a_loaded_subtree_conflicts_on_lignindb() {
    let committed = a_load_beside_a_writer_of_its_subtree::<super::LigninEngine>();
    let error = committed.expect_err("a conflict");
    assert!(error.is_commit_conflict(), "{error:?}");
}

/// On RocksDB the two meet by the keys they wrote, and here they wrote
/// none in common: the transaction commits over the load, and the tree
/// holds the nodes of both under an element that names one.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_transaction_that_wrote_into_a_loaded_subtree_commits_on_rocksdb() {
    a_load_beside_a_writer_of_its_subtree::<crate::RocksDbEngine>().expect("a commit");
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
