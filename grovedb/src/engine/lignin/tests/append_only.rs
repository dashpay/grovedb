//! The append-only trees on the LigninDB engine: an MMR tree, a
//! bulk-append tree, a dense tree, a commitment tree and a private-document
//! store keep their data as flat records of their subtree (`flat-store.md`).
//!
//! Each tree has a history: every public operation of its file in
//! `grovedb::operations`, directly and through a batch, in and outside a
//! transaction, with the operations of GroveDB that reach the tree from
//! outside (its element read, its delete, its clear, its emptiness). A
//! history is written once, for any engine, and run on LigninDB and on
//! RocksDB on every grove version. Every answer, every `OperationCost` and
//! the root hash after every write are compared, and the physical checker
//! reads the database that LigninDB leaves.

use std::fmt::Debug;

use grovedb_commitment_tree::{
    serialize_ciphertext, DashMemo, NoteBytesData, TransmittedNoteCiphertext,
};
use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::{proofs::Query, store::lignin::hash::MerkHashRules};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignindb::{verify::Scope, Db, Options};

use super::Lignin;
use crate::{
    batch::QualifiedGroveDbOp,
    engine::Engine,
    operations::delete::DeleteOptions,
    query_result_type::{QueryResultElements, QueryResultType},
    Element, Error, GroveDb, PathQuery, TransactionArg,
};

/// One call of a history, with what it answered: its cost, and what it
/// returned or how it failed, as text.
pub(super) type Answer = (String, OperationCost, String);

/// The calls of a history on one database, with their answers.
pub(super) struct History<'db, E: Engine> {
    pub(super) db: &'db GroveDb<E>,
    pub(super) v: &'db GroveVersion,
    pub(super) answers: Vec<Answer>,
}

impl<'db, E: Engine> History<'db, E> {
    pub(super) fn new(db: &'db GroveDb<E>, v: &'db GroveVersion) -> Self {
        Self {
            db,
            v,
            answers: Vec::new(),
        }
    }

    /// Takes in a call that only reads, and gives what it returned.
    pub(super) fn read<T: Debug>(
        &mut self,
        name: impl Into<String>,
        answer: CostResult<T, Error>,
    ) -> Option<T> {
        let cost = answer.cost.clone();
        let value = answer.unwrap();
        self.answers.push((name.into(), cost, format!("{value:?}")));
        value.ok()
    }

    /// Takes in a read of an element, with the bytes of the element: the
    /// text of an element leaves out what two elements can differ in.
    pub(super) fn element(&mut self, name: impl Into<String>, answer: CostResult<Element, Error>) {
        let v = self.v;
        self.read(name, answer.map_ok(|element| lossless(element, v)));
    }

    /// Takes in a read of elements by a query, each with its key and its
    /// bytes.
    pub(super) fn elements(
        &mut self,
        name: impl Into<String>,
        answer: CostResult<(QueryResultElements, u16), Error>,
    ) {
        let v = self.v;
        self.read(
            name,
            answer.map_ok(|(elements, skipped)| {
                let pairs = elements.to_key_elements().into_iter();
                let read: Vec<_> = pairs
                    .map(|(key, element)| (key, lossless(element, v)))
                    .collect();
                (read, skipped)
            }),
        );
    }

    /// Takes in a call that has no cost.
    pub(super) fn plain<T: Debug>(&mut self, name: impl Into<String>, answer: Result<T, Error>) {
        let name = name.into();
        self.answers
            .push((name, OperationCost::default(), format!("{answer:?}")));
    }

    /// Takes in a call that writes, and the root hash that `through` reads
    /// after it.
    pub(super) fn write_in<T: Debug>(
        &mut self,
        name: impl Into<String>,
        answer: CostResult<T, Error>,
        through: TransactionArg<'db, '_, E>,
    ) -> Option<T> {
        let written = self.read(name, answer);
        let root = self.db.root_hash(through, self.v);
        self.read("root", root);
        written
    }

    /// Takes in a call that writes with no transaction, and the root hash
    /// after it.
    pub(super) fn write<T: Debug>(
        &mut self,
        name: impl Into<String>,
        answer: CostResult<T, Error>,
    ) -> Option<T> {
        self.write_in(name, answer, None)
    }
}

/// An element with the bytes it is stored as.
fn lossless(element: Element, v: &GroveVersion) -> (Element, Vec<u8>) {
    let bytes = element.serialize(v).expect("the bytes of an element");
    (element, bytes)
}

/// How many of `answers` are of a call named `call` that succeeded.
pub(super) fn taken(answers: &[Answer], call: &str) -> usize {
    let ok = |(named, _, answer): &&Answer| named == call && answer.starts_with("Ok(");
    answers.iter().filter(ok).count()
}

/// Whether the grove's own check found nothing wrong, each time a history
/// ran it.
pub(super) fn sound(answers: &[Answer]) -> bool {
    let mut checks = answers.iter().filter(|(call, ..)| call == "verified");
    checks.all(|(.., found)| found == "Ok([])")
}

/// Opens the database in `dir` again, which nothing has open, and has
/// the physical checker read it: every rule of the layout, and every
/// stored hash of every Merk node against the hash that Merk's rules make
/// on the grove version `v`.
pub(super) fn check(dir: &std::path::Path, v: &GroveVersion) {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    let mut rules = MerkHashRules::for_check(v);
    let report = db.check(Scope::Still, Some(&mut rules));
    assert!(report.is_sound(), "{:#?}", report.findings);
    db.close().expect("a close");
}

/// Runs `$history` on LigninDB and on RocksDB on every grove version, and
/// compares what the two engines answered, call for call. `$told` is
/// handed the answers of LigninDB with the number of the grove version,
/// and asserts what the history must have done there.
macro_rules! on_both_engines {
    ($history:path, $told:expr) => {{
        let told = $told;
        for (number, v) in GROVE_VERSIONS.iter().enumerate() {
            let number = number + 1;
            let dir = tempfile::tempdir().expect("a directory");
            let on_lignin = {
                let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                let answers = $history(&db, v);
                db.flush().expect("a flush");
                answers
            };
            check(dir.path(), v);
            told(&on_lignin, number);
            #[cfg(feature = "rocksdb_storage")]
            {
                let dir = tempfile::tempdir().expect("a directory");
                let db = GroveDb::<$crate::RocksDbEngine>::open_with_engine(dir.path())
                    .expect("a database on RocksDB");
                let on_rocksdb = $history(&db, v);
                assert_eq!(
                    on_lignin.len(),
                    on_rocksdb.len(),
                    "the calls on grove version {number}"
                );
                for (step, (lignin, rocksdb)) in on_lignin.iter().zip(&on_rocksdb).enumerate() {
                    assert_eq!(lignin, rocksdb, "call {step} on grove version {number}");
                }
            }
        }
    }};
}
pub(super) use on_both_engines;

const ROOT: &[&[u8]] = &[];

fn owned(path: &[&[u8]], key: &[u8]) -> Vec<Vec<u8>> {
    let mut owned: Vec<Vec<u8>> = path.iter().map(|segment| segment.to_vec()).collect();
    owned.push(key.to_vec());
    owned
}

/// One kind of append-only tree, as its history uses it.
trait Appended {
    /// What the history calls the tree.
    const NAME: &'static str;
    /// How many values the history appends to one tree with no
    /// transaction and no batch: enough to pass what the tree does at a
    /// boundary of its own (a full chunk, a new peak).
    const DIRECT: usize;

    /// The calls that [`Self::reads`] records, by the name of each.
    const READS: &'static [&'static str];

    /// The element of an empty tree.
    fn empty(flags: Option<Vec<u8>>) -> Element;

    /// The `n`th value of a history.
    fn value(n: usize) -> Vec<u8>;

    /// Appends the `n`th value to the tree `key` under `path`.
    fn append<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        n: usize,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<String, Error>;

    /// The batch operation that appends the `n`th value to `tree`.
    fn op(tree: Vec<Vec<u8>>, n: usize) -> QualifiedGroveDbOp;

    /// The key a query names the position `n` of the tree by.
    fn position(n: u64) -> Vec<u8> {
        n.to_be_bytes().to_vec()
    }

    /// Every read of the tree's own file, at and around the position `at`.
    fn reads<'db, E: Engine>(
        h: &mut History<'db, E>,
        path: &[&[u8]],
        key: &[u8],
        at: u64,
        through: TransactionArg<'db, '_, E>,
    );
}

fn shown<T: Debug>(answer: CostResult<T, Error>) -> CostResult<String, Error> {
    answer.map_ok(|answer| format!("{answer:?}"))
}

struct Mmr;

impl Appended for Mmr {
    const DIRECT: usize = 19;
    const NAME: &'static str = "mmr";
    const READS: &'static [&'static str] = &["root hash", "count", "value"];

    fn empty(flags: Option<Vec<u8>>) -> Element {
        Element::empty_mmr_tree_with_flags(flags)
    }

    fn value(n: usize) -> Vec<u8> {
        vec![n as u8; 1 + (n * 37) % 300]
    }

    fn append<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        n: usize,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<String, Error> {
        shown(db.mmr_tree_append(path, key, Self::value(n), through, v))
    }

    fn op(tree: Vec<Vec<u8>>, n: usize) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::mmr_tree_append_op(tree, Self::value(n))
    }

    fn reads<'db, E: Engine>(
        h: &mut History<'db, E>,
        path: &[&[u8]],
        key: &[u8],
        at: u64,
        through: TransactionArg<'db, '_, E>,
    ) {
        let (db, v) = (h.db, h.v);
        h.read("root hash", db.mmr_tree_root_hash(path, key, through, v));
        h.read("count", db.mmr_tree_leaf_count(path, key, through, v));
        for leaf in [0, at.saturating_sub(1), at, at + 1] {
            let value = db.mmr_tree_get_value(path, key, leaf, through, v);
            h.read("value", value);
        }
    }
}

struct Bulk;

/// The chunk power of the bulk-append trees, the commitment trees and the
/// private-document stores of the histories: a chunk of four values, so
/// that a history fills several.
const CHUNK_POWER: u8 = 2;

impl Appended for Bulk {
    const DIRECT: usize = 14;
    const NAME: &'static str = "bulk";
    const READS: &'static [&'static str] =
        &["count", "chunks", "buffer", "value", "chunk", "range"];

    fn empty(flags: Option<Vec<u8>>) -> Element {
        Element::empty_bulk_append_tree_with_flags(CHUNK_POWER, flags).expect("a chunk power")
    }

    fn value(n: usize) -> Vec<u8> {
        vec![n as u8 ^ 0x5a; 2 + (n * 53) % 200]
    }

    fn append<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        n: usize,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<String, Error> {
        shown(db.bulk_append(path, key, Self::value(n), through, v))
    }

    fn op(tree: Vec<Vec<u8>>, n: usize) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::bulk_append_op(tree, Self::value(n))
    }

    fn reads<'db, E: Engine>(
        h: &mut History<'db, E>,
        path: &[&[u8]],
        key: &[u8],
        at: u64,
        through: TransactionArg<'db, '_, E>,
    ) {
        let (db, v) = (h.db, h.v);
        h.read("count", db.bulk_count(path, key, through, v));
        h.read("chunks", db.bulk_chunk_count(path, key, through, v));
        h.read("buffer", db.bulk_get_buffer(path, key, through, v));
        for position in [0, at.saturating_sub(1), at, at + 1] {
            let value = db.bulk_get_value(path, key, position, through, v);
            h.read("value", value);
        }
        for chunk in [0, at >> CHUNK_POWER, (at >> CHUNK_POWER) + 1] {
            h.read("chunk", db.bulk_get_chunk(path, key, chunk, through, v));
        }
        for (start, limit) in [(0, 3), (at.saturating_sub(5), 7), (at, 2), (0, u16::MAX)] {
            let page = db.bulk_get_range(path, key, start, limit, through, v);
            h.read("range", page);
        }
    }
}

struct Dense;

/// The height of the dense trees of the histories: 31 positions.
const DENSE_HEIGHT: u8 = 5;

impl Appended for Dense {
    const DIRECT: usize = 9;
    const NAME: &'static str = "dense";
    const READS: &'static [&'static str] = &["root hash", "count", "value"];

    fn empty(flags: Option<Vec<u8>>) -> Element {
        Element::empty_dense_tree_with_flags(DENSE_HEIGHT, flags)
    }

    fn value(n: usize) -> Vec<u8> {
        vec![n as u8 ^ 0xc3; 1 + (n * 29) % 120]
    }

    fn append<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        n: usize,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<String, Error> {
        shown(db.dense_tree_insert(path, key, Self::value(n), through, v))
    }

    fn op(tree: Vec<Vec<u8>>, n: usize) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::dense_tree_insert_op(tree, Self::value(n))
    }

    fn position(n: u64) -> Vec<u8> {
        (n as u16).to_be_bytes().to_vec()
    }

    fn reads<'db, E: Engine>(
        h: &mut History<'db, E>,
        path: &[&[u8]],
        key: &[u8],
        at: u64,
        through: TransactionArg<'db, '_, E>,
    ) {
        let (db, v) = (h.db, h.v);
        h.read("root hash", db.dense_tree_root_hash(path, key, through, v));
        h.read("count", db.dense_tree_count(path, key, through, v));
        for position in [0, at.saturating_sub(1), at, at + 1, u64::from(u16::MAX)] {
            let value = db.dense_tree_get(path, key, position as u16, through, v);
            h.read("value", value);
        }
    }
}

struct Commitment;

fn note(n: usize, tag: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = n as u8;
    bytes[1] = tag;
    bytes
}

fn ciphertext(n: usize) -> TransmittedNoteCiphertext<DashMemo> {
    let mut epk = [0u8; 32];
    epk[0] = n as u8;
    epk[1] = (n as u8).wrapping_add(1);
    let mut enc = [0u8; 104];
    enc[0] = n as u8;
    enc[1] = 0xec;
    let mut out = [0u8; 80];
    out[0] = n as u8;
    out[1] = 0x0c;
    TransmittedNoteCiphertext::from_parts(epk, NoteBytesData(enc), out)
}

impl Appended for Commitment {
    const DIRECT: usize = 11;
    const NAME: &'static str = "commitment";
    const READS: &'static [&'static str] = &["anchor", "count", "value", "range"];

    fn empty(flags: Option<Vec<u8>>) -> Element {
        Element::empty_commitment_tree_with_flags(CHUNK_POWER, flags).expect("a chunk power")
    }

    fn value(n: usize) -> Vec<u8> {
        serialize_ciphertext(&ciphertext(n))
    }

    fn append<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        n: usize,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<String, Error> {
        shown(db.commitment_tree_insert(
            path,
            key,
            note(n, 0),
            note(n, 0xaa),
            note(n, 0xcc),
            ciphertext(n),
            through,
            v,
        ))
    }

    fn op(tree: Vec<Vec<u8>>, n: usize) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::commitment_tree_insert_op(
            tree,
            note(n, 0),
            note(n, 0xaa),
            note(n, 0xcc),
            Self::value(n),
        )
    }

    fn reads<'db, E: Engine>(
        h: &mut History<'db, E>,
        path: &[&[u8]],
        key: &[u8],
        at: u64,
        through: TransactionArg<'db, '_, E>,
    ) {
        let (db, v) = (h.db, h.v);
        let anchor = db.commitment_tree_anchor(path, key, through, v);
        h.read("anchor", anchor.map_ok(|anchor| anchor.to_bytes()));
        h.read("count", db.commitment_tree_count(path, key, through, v));
        for position in [0, at.saturating_sub(1), at, at + 1] {
            let value = db.commitment_tree_get_value(path, key, position, through, v);
            h.read("value", value);
        }
        for (start, limit) in [(0, 3), (at.saturating_sub(5), 7), (at, 2), (0, u16::MAX)] {
            let page = db.commitment_tree_get_range(path, key, start, limit, through, v);
            h.read("range", page);
        }
    }
}

struct Private;

/// The entry size of the private-document stores of the histories.
const ENTRY_SIZE: u32 = 16;

impl Appended for Private {
    const DIRECT: usize = 11;
    const NAME: &'static str = "private";
    const READS: &'static [&'static str] = &["count", "value", "range"];

    fn empty(flags: Option<Vec<u8>>) -> Element {
        Element::empty_private_document_store_with_flags(ENTRY_SIZE, CHUNK_POWER, flags)
            .expect("an entry size and a chunk power")
    }

    fn value(n: usize) -> Vec<u8> {
        vec![n as u8 ^ 0x3c; ENTRY_SIZE as usize]
    }

    fn append<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        n: usize,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<String, Error> {
        shown(db.private_document_store_insert(path, key, Self::value(n), through, v))
    }

    fn op(tree: Vec<Vec<u8>>, n: usize) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::private_document_store_insert_op(tree, Self::value(n))
    }

    fn reads<'db, E: Engine>(
        h: &mut History<'db, E>,
        path: &[&[u8]],
        key: &[u8],
        at: u64,
        through: TransactionArg<'db, '_, E>,
    ) {
        let (db, v) = (h.db, h.v);
        h.read(
            "count",
            db.private_document_store_count(path, key, through, v),
        );
        for position in [0, at.saturating_sub(1), at, at + 1] {
            let value = db.private_document_store_get_value(path, key, position, through, v);
            h.read("value", value);
        }
        for (start, limit) in [(0, 3), (at.saturating_sub(5), 7), (at, 2), (0, u16::MAX)] {
            let page = db.private_document_store_get_range(path, key, start, limit, through, v);
            h.read("range", page);
        }
    }
}

/// The proof of the positions `from` to `to` of the tree `key` under
/// `path`, and the element of the tree as the grove's checker and a query
/// with no proof read it.
fn proofs<T: Appended, E: Engine>(
    h: &mut History<'_, E>,
    path: &[&[u8]],
    key: &[u8],
    from: u64,
    to: u64,
) {
    let (db, v) = (h.db, h.v);
    let mut positions = Query::new();
    positions.insert_range_inclusive(T::position(from)..=T::position(to));
    let mut tree = Query::new();
    tree.insert_key(key.to_vec());
    tree.set_subquery(positions);
    let above = path.iter().map(|segment| segment.to_vec()).collect();
    let query = PathQuery::new_unsized(above, tree);
    h.read("proof", db.prove_query(&query, None, v));
    let mut element = Query::new();
    element.insert_key(key.to_vec());
    let above = path.iter().map(|segment| segment.to_vec()).collect();
    let query = PathQuery::new_unsized(above, element);
    h.read("element proof", db.prove_query(&query, None, v));
    let elements = QueryResultType::QueryKeyElementPairResultType;
    let read = db.query_raw(&query, true, true, true, elements, None, v);
    h.elements("query", read);
}

/// What the grove's own check finds wrong: the paths it names, in order.
pub(super) fn verified<E: Engine>(h: &mut History<'_, E>) {
    let issues = h.db.verify_grovedb(None, true, false, h.v);
    h.plain(
        "verified",
        issues.map(|issues| {
            let mut paths: Vec<_> = issues.into_keys().collect();
            paths.sort();
            paths
        }),
    );
}

/// The history of one kind of append-only tree on the engine `E`.
///
/// Trees of the kind at the root, with flags under a tree two deep and
/// under a count tree; values appended one by one past the tree's own
/// boundaries, with every read of its file between them; proofs of its
/// positions and of its element, and the grove's own check; the reads and
/// the append on an element of another kind and on a key that holds none;
/// appends in a transaction that is rolled back to a savepoint and
/// committed, and in one that is rolled back whole; appends through
/// batches, alone, several to one tree, to two trees beside an item, in a
/// transaction, and in a batch whose last operation fails; the tree made
/// and filled by one batch; its emptiness, its clear and its delete, empty
/// and filled, and the same key written again.
fn history<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let item = |n: usize| Element::new_item(vec![n as u8; 5 + n]);
    let tree = T::NAME.as_bytes();

    h.write("made", db.insert(ROOT, tree, T::empty(None), None, None, v));
    h.write(
        "tree",
        db.insert(ROOT, b"docs", Element::empty_tree(), None, None, v),
    );
    let docs: &[&[u8]] = &[b"docs"];
    h.write(
        "tree",
        db.insert(docs, b"inner", Element::empty_tree(), None, None, v),
    );
    let deep: &[&[u8]] = &[b"docs", b"inner"];
    let counted: &[&[u8]] = &[b"counted"];
    let in_counted: &[&[u8]] = &[b"counted", tree];
    let at_tree: &[&[u8]] = &[tree];
    let gone: &[&[u8]] = &[b"gone"];
    h.write(
        "made",
        db.insert(deep, tree, T::empty(Some(vec![7, 7, 7])), None, None, v),
    );
    h.write(
        "tree",
        db.insert(ROOT, b"counted", Element::empty_count_tree(), None, None, v),
    );
    h.write(
        "made",
        db.insert(counted, tree, T::empty(None), None, None, v),
    );
    h.element("element", db.get(ROOT, tree, None, v));
    h.element("raw element", db.get_raw(deep.into(), tree, None, v));
    h.read("empty", db.is_empty_tree(at_tree, None, v));
    T::reads(&mut h, ROOT, tree, 0, None);

    // One by one, past the tree's own boundaries.
    for n in 0..T::DIRECT {
        h.write("append", T::append(db, ROOT, tree, n, None, v));
        if n % 3 == 0 || n + 1 == T::DIRECT {
            T::reads(&mut h, ROOT, tree, n as u64, None);
            h.element("element", db.get(ROOT, tree, None, v));
        }
    }
    for n in 0..5 {
        h.write("append", T::append(db, deep, tree, 100 + n, None, v));
        h.write("append", T::append(db, counted, tree, 200 + n, None, v));
    }
    T::reads(&mut h, deep, tree, 4, None);
    h.element("element", db.get(deep, tree, None, v));
    h.element("element", db.get(ROOT, b"counted", None, v));
    h.read("empty", db.is_empty_tree(at_tree, None, v));
    proofs::<T, E>(&mut h, ROOT, tree, 0, T::DIRECT as u64 + 3);
    proofs::<T, E>(&mut h, ROOT, tree, 2, 5);
    proofs::<T, E>(&mut h, deep, tree, 1, 3);
    proofs::<T, E>(&mut h, counted, tree, 0, 0);
    verified(&mut h);

    // An element of another kind, and a key that holds none.
    h.write("append", T::append(db, ROOT, b"docs", 0, None, v));
    h.write("append", T::append(db, ROOT, b"nothing", 0, None, v));
    h.write("append", T::append(db, gone, tree, 0, None, v));
    T::reads(&mut h, ROOT, b"docs", 1, None);
    T::reads(&mut h, ROOT, b"nothing", 1, None);

    // A transaction: its appends are its own until it commits, and a
    // rollback to a savepoint takes back what came after it.
    let tx = db.start_transaction();
    for n in 0..3 {
        let appended = T::append(db, ROOT, tree, 300 + n, Some(&tx), v);
        h.write_in("append", appended, Some(&tx));
    }
    T::reads(&mut h, ROOT, tree, T::DIRECT as u64 + 2, Some(&tx));
    T::reads(&mut h, ROOT, tree, T::DIRECT as u64 + 2, None);
    h.plain("savepoint", db.set_savepoint(&tx));
    for n in 0..2 {
        let appended = T::append(db, deep, tree, 310 + n, Some(&tx), v);
        h.write_in("append", appended, Some(&tx));
    }
    h.plain("rollback to savepoint", db.rollback_to_savepoint(&tx));
    T::reads(&mut h, deep, tree, 5, Some(&tx));
    let appended = T::append(db, deep, tree, 320, Some(&tx), v);
    h.write_in("append", appended, Some(&tx));
    h.write("commit", db.commit_transaction(tx));
    T::reads(&mut h, ROOT, tree, T::DIRECT as u64 + 2, None);
    T::reads(&mut h, deep, tree, 5, None);

    let tx = db.start_transaction();
    for n in 0..4 {
        let appended = T::append(db, ROOT, tree, 330 + n, Some(&tx), v);
        h.write_in("append", appended, Some(&tx));
    }
    h.plain("rollback", db.rollback_transaction(&tx));
    T::reads(&mut h, ROOT, tree, T::DIRECT as u64 + 3, Some(&tx));
    drop(tx);
    h.read("root", db.root_hash(None, v));

    // Batches.
    let at_root = || owned(ROOT, tree);
    h.write(
        "batch",
        db.apply_batch(vec![T::op(at_root(), 400)], None, None, v),
    );
    let several = (0..6).map(|n| T::op(at_root(), 410 + n)).collect();
    h.write("batch", db.apply_batch(several, None, None, v));
    let mixed = vec![
        T::op(at_root(), 420),
        T::op(owned(deep, tree), 421),
        QualifiedGroveDbOp::insert_or_replace_op(vec![b"docs".to_vec()], b"item".to_vec(), item(1)),
        T::op(owned(deep, tree), 422),
        T::op(owned(counted, tree), 423),
    ];
    h.write("batch", db.apply_batch(mixed, None, None, v));
    T::reads(&mut h, ROOT, tree, T::DIRECT as u64 + 10, None);
    T::reads(&mut h, deep, tree, 7, None);

    let tx = db.start_transaction();
    let in_tx = (0..3).map(|n| T::op(at_root(), 430 + n)).collect();
    let applied = db.apply_batch(in_tx, None, Some(&tx), v);
    h.write_in("batch", applied, Some(&tx));
    T::reads(&mut h, ROOT, tree, T::DIRECT as u64 + 12, Some(&tx));
    h.write("commit", db.commit_transaction(tx));

    let tx = db.start_transaction();
    let undone = (0..3).map(|n| T::op(at_root(), 440 + n)).collect();
    let applied = db.apply_batch(undone, None, Some(&tx), v);
    h.write_in("batch", applied, Some(&tx));
    h.plain("rollback", db.rollback_transaction(&tx));
    drop(tx);
    h.read("root", db.root_hash(None, v));

    // A batch whose last operation fails leaves nothing, and the next
    // batch is applied. The appends of a batch are worked out before its
    // other operations, tree by tree in an order that upstream leaves to
    // a hash map, so the operation that fails is none of them.
    let failing = vec![
        T::op(at_root(), 450),
        T::op(at_root(), 451),
        QualifiedGroveDbOp::insert_or_replace_op(owned(gone, b"deeper"), b"item".to_vec(), item(2)),
    ];
    h.write("failing batch", db.apply_batch(failing, None, None, v));
    T::reads(&mut h, ROOT, tree, T::DIRECT as u64 + 13, None);
    h.write(
        "batch",
        db.apply_batch(vec![T::op(at_root(), 453)], None, None, v),
    );

    // A tree that one batch makes beside an item, and the next fills.
    // GroveDB takes no append to a tree of the same batch.
    let fresh = vec![
        QualifiedGroveDbOp::insert_or_replace_op(vec![], b"fresh".to_vec(), T::empty(None)),
        QualifiedGroveDbOp::insert_or_replace_op(owned(ROOT, b"docs"), b"beside".to_vec(), item(3)),
    ];
    h.write("fresh batch", db.apply_batch(fresh, None, None, v));
    let fill = vec![
        T::op(owned(ROOT, b"fresh"), 460),
        T::op(owned(ROOT, b"fresh"), 461),
    ];
    h.write("batch", db.apply_batch(fill, None, None, v));
    T::reads(&mut h, ROOT, b"fresh", 1, None);
    h.write("append", T::append(db, ROOT, b"fresh", 462, None, v));
    T::reads(&mut h, ROOT, b"fresh", 2, None);

    // The clear and the delete, of a filled tree and of an empty one.
    h.plain("cleared", db.clear_subtree(in_counted, None, None, v));
    h.read("root", db.root_hash(None, v));
    T::reads(&mut h, counted, tree, 3, None);
    h.element("element", db.get(counted, tree, None, v));
    h.write("append", T::append(db, counted, tree, 500, None, v));
    T::reads(&mut h, counted, tree, 1, None);

    h.write("delete", db.delete(deep, tree, None, None, v));
    let allowed = Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    });
    h.write("delete", db.delete(deep, tree, allowed, None, v));
    T::reads(&mut h, deep, tree, 3, None);
    h.write(
        "made",
        db.insert(deep, tree, T::empty(Some(vec![9])), None, None, v),
    );
    T::reads(&mut h, deep, tree, 0, None);
    h.write("append", T::append(db, deep, tree, 510, None, v));
    h.write("append", T::append(db, deep, tree, 511, None, v));
    T::reads(&mut h, deep, tree, 1, None);
    h.write(
        "made",
        db.insert(ROOT, b"unused", T::empty(None), None, None, v),
    );
    h.write("delete", db.delete(ROOT, b"unused", None, None, v));
    h.element("element", db.get(ROOT, b"unused", None, v));
    proofs::<T, E>(&mut h, ROOT, tree, 0, 40);
    proofs::<T, E>(&mut h, deep, tree, 0, 1);
    proofs::<T, E>(&mut h, ROOT, b"unused", 0, 1);
    verified(&mut h);
    h.answers
}

/// What a history must have done on the grove version `number`, or the
/// comparison of the engines says nothing of the tree. The grove's own
/// check finds nothing wrong before the clear, and the element of every
/// tree is proven and read. On a version that has the kind: every tree is made, every append
/// of a value to one of them is taken, and every batch but the failing
/// one is applied. Up to
/// grove version 3 the clear of an append-only tree leaves its element as
/// it was, and the append after the clear is refused on every engine. On
/// a version that has not the kind, no tree is made.
fn told<T: Appended>(has_tree: impl Fn(usize) -> bool) -> impl Fn(&[Answer], usize) {
    move |answers, number| {
        let count = |call: &str| taken(answers, call);
        let appends = T::DIRECT + 24 - usize::from(number <= 3);
        let seen = [
            count("made"),
            count("append"),
            count("batch"),
            count("fresh batch"),
            count("failing batch"),
        ];
        let failed = || -> Vec<(usize, &str)> {
            (answers.iter().enumerate())
                .filter(|(_, (call, _, answer))| {
                    ["made", "append", "batch"].contains(&call.as_str())
                        && !answer.starts_with("Ok(")
                })
                .map(|(at, (.., answer))| (at, answer.as_str()))
                .collect()
        };
        // Up to grove version 3 the clear leaves the element of the
        // cleared tree as it was, and the grove's own check names that
        // tree from then on, on every engine.
        let checks: Vec<_> = (answers.iter())
            .filter(|(call, ..)| call == "verified")
            .map(|(.., found)| found == "Ok([])")
            .collect();
        // On the latest version, which has every read of every kind: a
        // history reads a tree at many stages, three of them under a key
        // that holds no such tree. Every other read of the tree's file is
        // answered, and among the answers are a value that is there, a
        // position past the end, and for a kind with pages a page of
        // values.
        if number == GROVE_VERSIONS.len() {
            let all = |call: &str| answers.iter().filter(|(named, ..)| named == call).count();
            let answered = |call: &str, start: &str| {
                let is = |(named, _, answer): &&Answer| named == call && answer.contains(start);
                answers.iter().filter(is).count()
            };
            let stages = all("count");
            for call in T::READS {
                let each = all(call) / stages;
                assert_eq!(all(call), each * stages, "{} reads {call}", T::NAME);
                assert_eq!(count(call), each * (stages - 3), "{} reads {call}", T::NAME);
            }
            assert!(answered("value", "Ok(Some([") > stages, "{}", T::NAME);
            assert!(answered("value", "Ok(None)") > 3, "{}", T::NAME);
            if T::READS.contains(&"range") {
                assert!(answered("range", "entries: [(") > stages, "{}", T::NAME);
                assert!(answered("range", "entries: []") > 3, "{}", T::NAME);
            }
        }
        let stale = has_tree(number) && number <= 3;
        assert_eq!(checks, [true, !stale], "{} on version {number}", T::NAME);
        let read = [count("element proof"), count("query"), count("verified")];
        assert_eq!(read, [7, 7, 2], "{} on version {number}", T::NAME);
        // On the latest version every kind proves its positions.
        if number == GROVE_VERSIONS.len() {
            assert_eq!(count("proof"), 7, "{}", T::NAME);
        }
        match has_tree(number) {
            true => assert_eq!(
                seen,
                [5, appends, 7, 1, 0],
                "{} on version {number}: {:#?}",
                T::NAME,
                failed()
            ),
            false => assert_eq!(seen[..3], [0, 0, 0], "{} on version {number}", T::NAME),
        }
    }
}

#[test]
fn an_mmr_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Mmr, _>, told::<Mmr>(|_| true));
}

#[test]
fn a_bulk_append_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Bulk, _>, told::<Bulk>(|_| true));
}

#[test]
fn a_dense_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Dense, _>, told::<Dense>(|_| true));
}

#[test]
fn a_commitment_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Commitment, _>, told::<Commitment>(|_| true));
}

#[test]
fn a_private_document_store_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Private, _>, told::<Private>(|number| number >= 4));
}
