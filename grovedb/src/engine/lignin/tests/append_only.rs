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
//!
//! Each tree has a second history, [`chain`]: the tree in the fifth and
//! in the third of five trees, one inside the other, where every append
//! is carried up to the root.

use grovedb_bulk_append_tree::deserialize_chunk_blob;
use grovedb_commitment_tree::{
    serialize_ciphertext, DashMemo, NoteBytesData, TransmittedNoteCiphertext,
};
use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::{proofs::Query, store::lignin::hash::MerkHashRules};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignindb::{Db, Options};

use super::{
    whole::{text, Whole},
    Lignin,
};
use crate::{
    batch::QualifiedGroveDbOp,
    engine::Engine,
    operations::delete::DeleteOptions,
    query_result_type::{QueryResultElement, QueryResultElements, QueryResultType},
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
    pub(super) fn read<T: Whole>(
        &mut self,
        name: impl Into<String>,
        answer: CostResult<T, Error>,
    ) -> Option<T> {
        let cost = answer.cost.clone();
        let value = answer.unwrap();
        self.answers.push((name.into(), cost, text(&value)));
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
                let pair = |result| match result {
                    QueryResultElement::KeyElementPairResultItem((key, element)) => {
                        (key, lossless(element, v))
                    }
                    other => panic!("a result that is no pair of a key and an element: {other}"),
                };
                let read: Vec<_> = elements.elements.into_iter().map(pair).collect();
                (read, skipped)
            }),
        );
    }

    /// Takes in a call that has no cost.
    pub(super) fn plain<T: Whole>(&mut self, name: impl Into<String>, answer: Result<T, Error>) {
        let name = name.into();
        self.answers
            .push((name, OperationCost::default(), text(&answer)));
    }

    /// Takes in a call that writes, and the root hash that `through` reads
    /// after it.
    pub(super) fn write_in<T: Whole>(
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
    pub(super) fn write<T: Whole>(
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

/// Opens the database in `dir` again, which nothing has open, closes it,
/// and has the physical checker read it as a store that is still: every
/// rule of the layout, the data column of every subtree by the tree type
/// of its entry, and every stored hash of every Merk node against the
/// hash that Merk's rules make on the grove version `v`.
pub(super) fn check(dir: &std::path::Path, v: &GroveVersion) {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    db.check_kinds_by(grovedb_merk::store::lignin::keeps_flat_records);
    let mut rules = MerkHashRules::for_check(v);
    let report = db.close_and_verify(Some(&mut rules)).expect("a close");
    assert!(report.is_sound(), "{:#?}", report.findings);
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
pub(super) trait Appended {
    /// What the history calls the tree.
    const NAME: &'static str;
    /// How many values the history appends to one tree with no
    /// transaction and no batch: enough to pass what the tree does at a
    /// boundary of its own (a full chunk, a new peak).
    const DIRECT: usize;

    /// How many of the calls that [`Self::reads`] records are answered
    /// in a history on the grove version `number`, by the name of each.
    fn answered(number: usize) -> Vec<(&'static str, usize)>;

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

    /// What the answers of a history on a grove version that has the kind
    /// must hold beside what every kind's must.
    fn held(_answers: &[Answer]) {}

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

fn shown<T: Whole>(answer: CostResult<T, Error>) -> CostResult<String, Error> {
    answer.map_ok(|answer| text(&answer))
}

pub(super) struct Mmr;

impl Appended for Mmr {
    const DIRECT: usize = 19;
    const NAME: &'static str = "mmr";

    fn answered(number: usize) -> Vec<(&'static str, usize)> {
        let hashes = if number <= 3 { 23 } else { 25 };
        vec![("root hash", hashes), ("count", 25), ("value", 100)]
    }

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

pub(super) struct Bulk;

/// The chunk power of the bulk-append trees, the commitment trees and the
/// private-document stores of the histories: a chunk of four values, so
/// that a history fills several.
const CHUNK_POWER: u8 = 2;

impl Appended for Bulk {
    const DIRECT: usize = 14;
    const NAME: &'static str = "bulk";

    fn answered(number: usize) -> Vec<(&'static str, usize)> {
        let (buffers, values, chunks) = if number <= 3 {
            (22, 88, 68)
        } else {
            (24, 96, 72)
        };
        vec![
            ("count", 24),
            ("chunks", 24),
            ("buffer", buffers),
            ("value", values),
            ("chunk", chunks),
            ("range", values),
        ]
    }

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

    /// A full chunk is read with the values that filled it: some chunk
    /// decodes to exactly the first four values of the tree at the root.
    fn held(answers: &[Answer]) {
        let first: Vec<Vec<u8>> = (0..1 << CHUNK_POWER).map(Self::value).collect();
        let decoded = format!(", Some(Some({first:?}))))");
        let chunks = answers.iter().filter(|(call, ..)| call == "chunk");
        let full = chunks.filter(|(.., answer)| answer.ends_with(&decoded));
        assert!(full.count() > 0, "no chunk with the first four values");
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
            // A chunk with the values it decodes to.
            let read = db.bulk_get_chunk(path, key, chunk, through, v);
            let read = read.map_ok(|blob| {
                let values = blob
                    .as_deref()
                    .map(|blob| deserialize_chunk_blob(blob).ok());
                (blob, values)
            });
            h.read("chunk", read);
        }
        for (start, limit) in [(0, 3), (at.saturating_sub(5), 7), (at, 2), (0, u16::MAX)] {
            let page = db.bulk_get_range(path, key, start, limit, through, v);
            h.read("range", page);
        }
    }
}

pub(super) struct Dense;

/// The height of the dense trees of the histories: 31 positions.
const DENSE_HEIGHT: u8 = 5;

impl Appended for Dense {
    const DIRECT: usize = 9;
    const NAME: &'static str = "dense";

    fn answered(number: usize) -> Vec<(&'static str, usize)> {
        let hashes = if number <= 3 { 20 } else { 22 };
        vec![("root hash", hashes), ("count", 22), ("value", 110)]
    }

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

pub(super) struct Commitment;

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

    fn answered(number: usize) -> Vec<(&'static str, usize)> {
        let (anchors, values) = if number <= 3 { (21, 84) } else { (23, 92) };
        vec![
            ("anchor", anchors),
            ("count", 23),
            ("value", values),
            ("range", values),
        ]
    }

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

pub(super) struct Private;

/// The entry size of the private-document stores of the histories.
const ENTRY_SIZE: u32 = 16;

impl Appended for Private {
    const DIRECT: usize = 11;
    const NAME: &'static str = "private";

    /// The pages of a store are read from grove version 5 on.
    fn answered(number: usize) -> Vec<(&'static str, usize)> {
        let pages = if number <= 4 { 0 } else { 92 };
        vec![("count", 23), ("value", 92), ("range", pages)]
    }

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
        // Which reads of the tree's file are answered. A history reads
        // a tree at many stages, three of them under a key that holds no
        // such tree, and up to grove version 3 two more of a tree whose
        // clear left its element as it was. Among the answers are a
        // value that is there and a position past the end, and for a
        // kind that has them a page of values and an empty one, a chunk
        // that is full and one that is not there, and a buffer that
        // holds values.
        if has_tree(number) {
            let answered = |call: &str, with: &str| {
                let is = |(named, _, answer): &&Answer| named == call && answer.contains(with);
                answers.iter().filter(is).count()
            };
            let seen: Vec<_> = (T::answered(number).iter())
                .map(|(call, _)| (*call, count(call)))
                .collect();
            assert_eq!(seen, T::answered(number), "{} on version {number}", T::NAME);
            let has = |call: &str| {
                seen.iter()
                    .any(|(named, taken)| *named == call && *taken > 0)
            };
            let needles: [(&str, &[&str]); 4] = [
                ("value", &["Ok(Some([", "Ok(None)"]),
                ("range", &["entries: [(", "entries: []"]),
                ("chunk", &["Ok((Some([", "Ok((None, None))"]),
                ("buffer", &["Ok([["]),
            ];
            for (call, with) in needles.into_iter().filter(|(call, _)| has(call)) {
                for with in with {
                    assert!(answered(call, with) > 0, "{} {call} {with}", T::NAME);
                }
            }
            T::held(answers);
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

/// The five trees of a chain, one inside the other under the root: five
/// kinds that every grove version makes.
const CHAIN: [(&[u8], fn() -> Element); 5] = [
    (b"one", Element::empty_tree),
    (b"two", Element::empty_sum_tree),
    (b"three", Element::empty_count_tree),
    (b"four", Element::empty_provable_count_tree),
    (b"five", Element::empty_count_sum_tree),
];

/// The history of one kind of append-only tree below other trees: a tree
/// of the kind in the fifth tree of a chain and one in the third, and
/// every append carried up to the root (`engine.md`, "Root-hash
/// propagation").
///
/// An item in the fifth tree; appends to the two trees of the kind in
/// turn, with the elements of the five trees above read after them; appends in a transaction, with the root hash that
/// the transaction has and the one outside it, committed, and in one that
/// is rolled back; a batch of an append to each beside an item in the
/// second tree; the proofs of the deepest tree and the grove's own check.
fn chain<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tree = T::NAME.as_bytes();
    let keys: Vec<&[u8]> = CHAIN.iter().map(|(key, _)| *key).collect();
    for (depth, (key, make)) in CHAIN.iter().enumerate() {
        h.write(
            "tree",
            db.insert(&keys[..depth], key, make(), None, None, v),
        );
    }
    let (middle, bottom) = (&keys[..3], keys.as_slice());
    // The fifth tree holds an item on every grove version, whether or not
    // the version has the kind.
    let first = Element::new_item(b"first".to_vec());
    h.write("item", db.insert(bottom, b"item", first, None, None, v));
    h.write(
        "made",
        db.insert(middle, tree, T::empty(None), None, None, v),
    );
    let flagged = T::empty(Some(vec![4, 2]));
    h.write("made", db.insert(bottom, tree, flagged, None, None, v));
    let above = |h: &mut History<'_, E>| {
        for (depth, key) in keys.iter().enumerate() {
            h.element("above", db.get(&keys[..depth], key, None, v));
        }
    };
    above(&mut h);

    for n in 0..6 {
        h.write("append", T::append(db, bottom, tree, n, None, v));
        h.write("append", T::append(db, middle, tree, 50 + n, None, v));
    }
    above(&mut h);
    T::reads(&mut h, bottom, tree, 5, None);
    T::reads(&mut h, middle, tree, 5, None);

    let tx = db.start_transaction();
    for n in 0..2 {
        let appended = T::append(db, bottom, tree, 100 + n, Some(&tx), v);
        h.write_in("append", appended, Some(&tx));
    }
    h.read("root outside", db.root_hash(None, v));
    h.write("commit", db.commit_transaction(tx));

    let tx = db.start_transaction();
    let appended = T::append(db, bottom, tree, 110, Some(&tx), v);
    h.write_in("append", appended, Some(&tx));
    h.plain("rollback", db.rollback_transaction(&tx));
    drop(tx);
    h.read("root after rollback", db.root_hash(None, v));

    let item = Element::new_item(b"beside".to_vec());
    let batch = vec![
        T::op(owned(bottom, tree), 120),
        T::op(owned(middle, tree), 121),
        QualifiedGroveDbOp::insert_or_replace_op(
            owned(&keys[..1], keys[1]),
            b"item".to_vec(),
            item,
        ),
    ];
    h.write("batch", db.apply_batch(batch, None, None, v));
    above(&mut h);
    T::reads(&mut h, bottom, tree, 8, None);
    proofs::<T, E>(&mut h, bottom, tree, 0, 9);
    proofs::<T, E>(&mut h, middle, tree, 2, 4);
    verified(&mut h);
    h.answers
}

/// What a chain's history must have done on the grove version `number`:
/// the five trees and the item are made, the elements of the trees are
/// read, and the grove's own check finds nothing wrong. On a version that has the kind, both trees
/// of it are made, every append and the batch are taken, and the root
/// hash follows each of them: no two of the root hashes after the writes
/// are equal. The commit leaves the root hash that its transaction had,
/// which the grove outside did not have before, and the rollback leaves
/// the one of the commit. On a version that has not the kind, no tree of
/// it is made and nothing is appended.
fn chain_told<T: Appended>(has_tree: impl Fn(usize) -> bool) -> impl Fn(&[Answer], usize) {
    move |answers, number| {
        let count = |call: &str| taken(answers, call);
        assert_eq!(
            [
                count("tree"),
                count("item"),
                count("above"),
                count("verified")
            ],
            [5, 1, 15, 1],
            "{} on version {number}",
            T::NAME
        );
        assert!(sound(answers), "{} on version {number}", T::NAME);
        let seen = [count("made"), count("append"), count("batch")];
        if !has_tree(number) {
            assert_eq!(seen, [0, 0, 0], "{} on version {number}", T::NAME);
            return;
        }
        assert_eq!(seen, [2, 15, 1], "{} on version {number}", T::NAME);
        // The root hash after each call, by the name of the call.
        let after = |calls: &[&str]| -> Vec<&str> {
            (answers.windows(2))
                .filter(|pair| calls.contains(&pair[0].0.as_str()))
                .map(|pair| {
                    assert_eq!(pair[1].0, "root", "after a {}", pair[0].0);
                    assert!(pair[1].2.starts_with("Ok("), "a root hash is read");
                    pair[1].2.as_str()
                })
                .collect()
        };
        let roots = after(&["tree", "item", "made", "append", "batch"]);
        let mut different = roots.clone();
        different.sort_unstable();
        different.dedup();
        assert_eq!(roots.len(), 5 + 1 + 2 + 15 + 1);
        assert_eq!(
            different.len(),
            roots.len(),
            "{} on version {number}",
            T::NAME
        );
        let named = |call: &str| -> &str {
            let mut found = answers.iter().filter(|(named, ..)| named == call);
            let answer = found.next().expect("the call is in the history");
            assert!(found.next().is_none(), "one {call}");
            answer.2.as_str()
        };
        // The first twelve appends have no transaction, the next two are
        // those of the transaction that commits.
        let appends = after(&["append"]);
        let committed = after(&["commit"]);
        assert_eq!(committed, [appends[13]], "the commit leaves its own root");
        assert_eq!(named("root outside"), appends[11]);
        assert_eq!(named("root after rollback"), appends[13]);
    }
}

macro_rules! five_trees_deep {
    ($($test:ident: $kind:ty, $has:expr;)*) => {$(
        #[test]
        fn $test() {
            on_both_engines!(chain::<$kind, _>, chain_told::<$kind>($has));
        }
    )*};
}

five_trees_deep! {
    the_root_hash_follows_an_append_to_an_mmr_tree_five_trees_deep: Mmr, |_| true;
    the_root_hash_follows_an_append_to_a_bulk_append_tree_five_trees_deep: Bulk, |_| true;
    the_root_hash_follows_an_append_to_a_dense_tree_five_trees_deep: Dense, |_| true;
    the_root_hash_follows_an_append_to_a_commitment_tree_five_trees_deep: Commitment, |_| true;
    the_root_hash_follows_an_append_to_a_private_document_store_five_trees_deep: Private, |number| number >= 4;
}

/// A tree that holds an append-only tree, walked for the trees in it:
/// before the tree has a record and after. Then the records of the
/// append-only tree under the cursor that walks the nodes of a Merk, each
/// with what the looks at it are charged.
///
/// RocksDB's cursor over the nodes of a subtree is the cursor of its data
/// keyspace, which shows the records of an append-only tree with their
/// bytes. On the grove versions whose walk takes every record there for an
/// element, GroveDB makes of such a record what it makes of it, and the
/// walk over a tree with records ends as it ends on RocksDB (D-197).
fn walked<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let outer: &[&[u8]] = &[b"outer"];
    let tree = T::NAME.as_bytes();
    h.write(
        "made",
        db.insert(ROOT, b"outer", Element::empty_tree(), None, None, v),
    );
    h.write(
        "made",
        db.insert(outer, tree, T::empty(None), None, None, v),
    );
    h.write(
        "item",
        db.insert(outer, b"item", Element::new_item(vec![5; 9]), None, None, v),
    );
    h.read("subtrees", db.find_subtrees(&outer.into(), None, v));
    for n in 0..3 {
        h.write("append", T::append(db, outer, tree, n, None, v));
    }
    h.read("subtrees", db.find_subtrees(&outer.into(), None, v));

    raw_walk(&mut h, &[b"outer", tree]);
    h.answers
}

/// Walks the node cursor of the subtree at `path` from its first record
/// to its end, or to a record that it shows no value of, and takes in
/// what the cursor shows at each place: whether
/// it is valid, the key, the value and its status, with what the looks
/// and the step to the place are charged.
fn raw_walk<E: Engine>(h: &mut History<'_, E>, path: &[&[u8]]) {
    use grovedb_merk::store::{NodeCursor, NodeStore};

    let engine = h.db.engine();
    let transaction = engine.start_read_transaction();
    let handle = engine.subtree(path.into(), None, &transaction).unwrap();
    let mut cursor = NodeStore::cursor(&handle);
    let mut cost = OperationCost::default();
    cursor.seek_to_first().unwrap_add_cost(&mut cost);
    loop {
        let valid = cursor.valid().unwrap_add_cost(&mut cost);
        let key = cursor.key().unwrap_add_cost(&mut cost).map(<[u8]>::to_vec);
        let value = (cursor.value().unwrap_add_cost(&mut cost)).map(<[u8]>::to_vec);
        let status = cursor.status();
        let shown = format!("{valid} {key:?} {value:?} {status:?}");
        (h.answers).push(("record".to_owned(), std::mem::take(&mut cost), shown));
        if !valid || value.is_none() {
            break;
        }
        cursor.next().unwrap_add_cost(&mut cost);
    }
}

/// What [`walked`] must have come to on LigninDB: on a grove version that
/// has the kind, the cursor showed records, each with a value.
fn walked_told(has_tree: impl Fn(usize) -> bool) -> impl Fn(&[Answer], usize) {
    move |answers, number| {
        let records = answers.iter().filter(|(name, ..)| name == "record");
        let shown: Vec<_> =
            (records.filter(|(.., shown)| shown.starts_with("true Some("))).collect();
        let with_value = |(.., shown): &&Answer| shown.ends_with(") Ok(())");
        assert!(shown.iter().all(with_value), "v{number}: {shown:?}");
        assert_eq!(!shown.is_empty(), has_tree(number), "v{number}");
    }
}

#[test]
fn a_walk_for_the_trees_beside_an_append_only_tree_answers_as_on_rocksdb() {
    on_both_engines!(walked::<Mmr, _>, walked_told(|_| true));
    on_both_engines!(walked::<Bulk, _>, walked_told(|_| true));
    on_both_engines!(walked::<Dense, _>, walked_told(|_| true));
    on_both_engines!(walked::<Commitment, _>, walked_told(|_| true));
    on_both_engines!(walked::<Private, _>, walked_told(|number| number >= 4));
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

/// What the close of a LigninDB database finds wrong, in a debug build:
/// the text of its panic, or nothing.
#[cfg(all(feature = "rocksdb_storage", debug_assertions))]
fn found_at_close(db: GroveDb<crate::LigninEngine>) -> Option<String> {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let closed = catch_unwind(AssertUnwindSafe(move || drop(db)));
    closed.err().map(|panic| {
        let said = panic.downcast_ref::<String>().expect("a text");
        assert!(said.contains("is left unsound at version"), "{said}");
        said.clone()
    })
}

/// The records that [`raw_walk`] took in.
#[cfg(all(feature = "rocksdb_storage", debug_assertions))]
fn records(answers: &[Answer]) -> Vec<&str> {
    let records = answers.iter().filter(|(name, ..)| name == "record");
    records.map(|(.., shown)| shown.as_str()).collect()
}

/// One batch makes a tree that keeps flat records and a keyed child under
/// it, and the node cursor of the tree's subtree is walked. Grove versions
/// 1 to 3 take the batch, and the level of the child runs as a Merk under
/// the prefix of the tree (upstream refuses it from grove version 4).
#[cfg(all(feature = "rocksdb_storage", debug_assertions))]
fn keyed_child_walked<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tree = T::NAME.as_bytes();
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(vec![], tree.to_vec(), T::empty(None)),
        QualifiedGroveDbOp::insert_or_replace_op(
            owned(ROOT, tree),
            b"child".to_vec(),
            Element::new_item(b"a keyed child".to_vec()),
        ),
    ];
    h.write("batch", db.apply_batch(ops, None, None, v));
    raw_walk(&mut h, &[tree]);
    h.answers
}

/// D-280, the first of the two states that grove versions 1 to 3 come to
/// and that the tree type of an entry does not tell apart: a node of a
/// Merk under an entry whose tree type keeps flat records. The node is a
/// cell of the Merk's tree, and the node cursor shows it as RocksDB's
/// does, with the same bytes and charges: every answer of the history is
/// RocksDB's.
///
/// The close of a debug build finds nothing: the cell of the tree's
/// element hosts the Merk of the keyed level, and the checker's hash
/// rule for a tree that keeps flat records has a place for that one
/// Merk (D-287).
#[cfg(all(feature = "rocksdb_storage", debug_assertions))]
#[test]
fn a_keyed_child_under_a_tree_of_flat_records_is_walked_as_on_rocksdb() {
    fn on_both<T: Appended>() {
        for (number, v) in GROVE_VERSIONS.iter().enumerate().take(3) {
            let number = number + 1;
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            let on_rocksdb = keyed_child_walked::<T, _>(&db, v);
            let shown = records(&on_rocksdb);
            assert_eq!(shown.len(), 2, "{} on v{number}: {shown:?}", T::NAME);
            assert!(shown[0].starts_with("true Some([99, 104, 105, 108, 100]) Some("));
            assert!(shown[0].ends_with(") Ok(())"), "{}", shown[0]);
            assert_eq!(shown[1], "false None None Ok(())");

            let dir = tempfile::tempdir().expect("a directory");
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let on_lignin = keyed_child_walked::<T, _>(&db, v);
            assert_eq!(on_lignin, on_rocksdb, "{} on v{number}", T::NAME);
            assert_eq!(found_at_close(db), None, "{} on v{number}", T::NAME);
        }
    }
    on_both::<Mmr>();
    on_both::<Bulk>();
    on_both::<Dense>();
    on_both::<Commitment>();
}

/// A sum-indexed tree is written into and emptied, a batch writes an
/// empty MMR tree over it, and a value is appended to that tree. Then the
/// node cursor of the subtree is walked.
#[cfg(all(feature = "rocksdb_storage", debug_assertions))]
fn over_an_emptied_indexed_tree<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    use super::indexed::{Indexed, Sum};

    let mut h = History::new(db, v);
    let indexed: &[&[u8]] = &[b"i"];
    h.write("made", db.insert(ROOT, b"i", Sum::empty(), None, None, v));
    let child = Sum::insert(db, indexed, b"child", Sum::leaf(4), None, v);
    h.write("child", child);
    h.write("emptied", Sum::delete(db, indexed, b"child", None, v));
    let over = QualifiedGroveDbOp::insert_or_replace_op(vec![], b"i".to_vec(), Mmr::empty(None));
    h.write("over", db.apply_batch(vec![over], None, None, v));
    h.write("append", Mmr::append(db, ROOT, b"i", 0, None, v));
    raw_walk(&mut h, &[b"i"]);
    h.answers
}

/// D-280, the second of the two histories: on grove version 3 a batch
/// writes an MMR tree over an indexed tree that was emptied, and a value
/// is appended to the MMR tree. The engine takes the Merks of the
/// indexed tree out of the directory with the batch, and the append
/// enters the subtree as the MMR tree's. The node cursor shows the
/// record as RocksDB's does, every answer of the history is RocksDB's,
/// and the close of a debug build finds nothing.
#[cfg(all(feature = "rocksdb_storage", debug_assertions))]
#[test]
fn a_flat_tree_over_an_emptied_indexed_tree_is_walked_as_on_rocksdb() {
    let v = &GROVE_VERSIONS[2];
    let dir = tempfile::tempdir().expect("a directory");
    let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
        .expect("a database on RocksDB");
    let on_rocksdb = over_an_emptied_indexed_tree(&db, v);
    for (call, _, answer) in &on_rocksdb {
        let written = ["made", "child", "emptied", "over", "append"].contains(&call.as_str());
        assert!(!written || answer.starts_with("Ok("), "{call}: {answer}");
    }
    let required = records(&on_rocksdb);
    assert_eq!(required.len(), 2, "{required:?}");
    assert!(required[0].starts_with("true Some([128, 0, 0, 0, 0, 0, 0, 0]) Some("));
    assert!(required[0].ends_with(") Ok(())"), "{}", required[0]);
    assert_eq!(required[1], "false None None Ok(())");

    let dir = tempfile::tempdir().expect("a directory");
    let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
    let on_lignin = over_an_emptied_indexed_tree(&db, v);
    assert_eq!(on_lignin, on_rocksdb);
    assert_eq!(found_at_close(db), None);
}

/// What a history does with a keyed child under a tree that keeps flat
/// records ([`keyed_child`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Keyed {
    /// One batch makes the tree and the child, and values are appended
    /// afterwards.
    BeforeAppends,
    /// The tree is made and values are appended, and a batch inserts the
    /// child afterwards.
    AfterAppends,
    /// One batch makes the tree and the child, and nothing is appended.
    NoAppend,
}

/// A keyed child under a tree that keeps flat records. Grove versions 1
/// to 3 take a batch that inserts one (the refusal begins with grove
/// version 4), and its level runs as a Merk under the prefix of the
/// tree, beside the flat records of the tree. The history: the tree and
/// the child, with two appends before the child, after it or not at all;
/// the element, the typed reads, the child read by its key and by
/// queries, the emptiness, the grove's own check, the delete of the
/// tree, and the element and the check after it.
fn keyed_child<T: Appended, E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    how: Keyed,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tree = T::NAME.as_bytes();
    let at_tree: &[&[u8]] = &[tree];
    let made = || QualifiedGroveDbOp::insert_or_replace_op(vec![], tree.to_vec(), T::empty(None));
    let child = || {
        let item = Element::new_item(b"a keyed child".to_vec());
        QualifiedGroveDbOp::insert_or_replace_op(owned(ROOT, tree), b"child".to_vec(), item)
    };
    let appends = |h: &mut History<'_, E>| {
        for n in 0..2 {
            h.write("append", T::append(db, ROOT, tree, n, None, v));
        }
    };
    match how {
        Keyed::BeforeAppends => {
            h.write(
                "batch",
                db.apply_batch(vec![made(), child()], None, None, v),
            );
            appends(&mut h);
        }
        Keyed::AfterAppends => {
            h.write("batch", db.apply_batch(vec![made()], None, None, v));
            appends(&mut h);
            h.write("batch", db.apply_batch(vec![child()], None, None, v));
        }
        Keyed::NoAppend => {
            h.write(
                "batch",
                db.apply_batch(vec![made(), child()], None, None, v),
            );
        }
    }
    h.element("element", db.get(ROOT, tree, None, v));
    T::reads(&mut h, ROOT, tree, 1, None);
    h.element("get child", db.get(at_tree, b"child", None, v));
    let elements = QueryResultType::QueryKeyElementPairResultType;
    let mut keyed = Query::new();
    keyed.insert_key(b"child".to_vec());
    let mut all = Query::new();
    all.insert_all();
    for (name, query) in [("query child", keyed), ("query all", all)] {
        let query = PathQuery::new_unsized(owned(ROOT, tree), query);
        let read = db.query_raw(&query, true, true, true, elements, None, v);
        h.read(
            name,
            read.map_ok(|(found, skipped)| (text(&found), skipped)),
        );
    }
    h.read("empty", db.is_empty_tree(at_tree, None, v));
    verified(&mut h);
    let allowed = Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    });
    h.write("delete", db.delete(ROOT, tree, allowed, None, v));
    h.element("element", db.get(ROOT, tree, None, v));
    verified(&mut h);
    h.answers
}

/// Runs [`keyed_child`] on RocksDB and on LigninDB on the grove version
/// `v`. Gives the answers of each, and whether the store that LigninDB
/// leaves is sound: at its close, which reads it with the physical
/// checker in a debug build, and read by the checker afterwards.
#[cfg(feature = "rocksdb_storage")]
fn keyed_child_on_both<T: Appended>(
    v: &GroveVersion,
    how: Keyed,
) -> (Vec<Answer>, Vec<Answer>, bool) {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let on_rocksdb = {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path());
        keyed_child::<T, _>(&db.expect("a database on RocksDB"), v, how)
    };
    let dir = tempfile::tempdir().expect("a directory");
    let db = Lignin::open_with_engine(dir.path()).expect("a database");
    let on_lignin = keyed_child::<T, _>(&db, v, how);
    db.flush().expect("a flush");
    let closed = catch_unwind(AssertUnwindSafe(move || drop(db))).is_ok();
    let sound = closed && catch_unwind(|| check(dir.path(), v)).is_ok();
    (on_rocksdb, on_lignin, sound)
}

/// Grove versions 1 to 3 take a batch that inserts a keyed child under a
/// tree that keeps flat records, and a node that replays them has to
/// answer as RocksDB does (D-282). RocksDB's answers are asserted as the
/// required ones: the batch, both appends and the delete of the tree are
/// taken, the typed reads count both values, and the grove's own check
/// finds nothing wrong once the tree is deleted.
///
/// On LigninDB every answer, cost and root hash is RocksDB's in the
/// three histories:
///
/// - **with no append**;
/// - **with the child before the appends** (D-282): the appends are
///   taken beside the nodes of the Merk of the child's level, since the
///   transaction takes a flat record there under a tree that keeps flat
///   records;
/// - **with the child after the appends** (D-283): the query of every
///   key under the tree fails on both engines alike, and the delete of
///   the tree is taken.
///
/// The store is sound at the close of each
/// ([`a_keyed_child_under_a_tree_of_flat_records_leaves_a_sound_store`]).
#[cfg(feature = "rocksdb_storage")]
fn a_keyed_child_under<T: Appended>() {
    let step = |answers: &[Answer], call: &str| {
        let found = answers.iter().position(|(named, ..)| named == call);
        found.unwrap_or_else(|| panic!("no {call} in the history"))
    };
    for how in [Keyed::BeforeAppends, Keyed::AfterAppends, Keyed::NoAppend] {
        for (number, v) in GROVE_VERSIONS.iter().enumerate().take(3) {
            let case = format!("{}, {how:?}, grove version {}", T::NAME, number + 1);
            let (on_rocksdb, on_lignin, sound) = keyed_child_on_both::<T>(v, how);
            assert_eq!(on_rocksdb.len(), on_lignin.len(), "{case}");

            // What is required.
            let appends = if how == Keyed::NoAppend { 0 } else { 2 };
            let batches = if how == Keyed::AfterAppends { 2 } else { 1 };
            let required = [("batch", batches), ("append", appends), ("delete", 1)];
            for (call, times) in required {
                assert_eq!(taken(&on_rocksdb, call), times, "{case}: {call}");
            }
            let counted = &on_rocksdb[step(&on_rocksdb, "count")].2;
            assert_eq!(counted, &format!("Ok({appends})"), "{case}");
            let last = on_rocksdb.last().expect("a last step");
            assert_eq!((last.0.as_str(), last.2.as_str()), ("verified", "Ok([])"));

            let same = |steps: std::ops::Range<usize>, but: &str| {
                for at in steps {
                    if on_rocksdb[at].0 != but {
                        assert_eq!(on_lignin[at], on_rocksdb[at], "{case}: step {at}");
                    }
                }
            };
            match how {
                Keyed::NoAppend => {
                    same(0..on_rocksdb.len(), "");
                    assert!(sound, "{case}");
                }
                Keyed::BeforeAppends => {
                    // The appends are taken beside the nodes of the Merk
                    // of the keyed child's level.
                    same(0..on_rocksdb.len(), "");
                    assert_eq!(taken(&on_lignin, "append"), 2, "{case}");
                    assert!(sound, "{case}");
                }
                Keyed::AfterAppends => {
                    // The query of everything under the tree fails on
                    // both engines alike, on the record that it reads as
                    // a node, and the delete of the tree is taken.
                    same(0..on_rocksdb.len(), "");
                    let all = step(&on_rocksdb, "query all");
                    assert!(on_rocksdb[all].2.starts_with("Err("), "{case}");
                    assert!(sound, "{case}");
                }
            }
        }
    }
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_keyed_child_under_an_mmr_tree_is_held_to_what_rocksdb_answers() {
    a_keyed_child_under::<Mmr>();
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_keyed_child_under_a_bulk_append_tree_is_held_to_what_rocksdb_answers() {
    a_keyed_child_under::<Bulk>();
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_keyed_child_under_a_dense_tree_is_held_to_what_rocksdb_answers() {
    a_keyed_child_under::<Dense>();
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_keyed_child_under_a_commitment_tree_is_held_to_what_rocksdb_answers() {
    a_keyed_child_under::<Commitment>();
}

/// The batch that inserts a keyed child under a tree that keeps flat
/// records puts the node of the tree's element as one that hosts the
/// Merk of the child's level, in the first place, and Merk states the
/// value hash of that element with the root hash of the Merk in it. The
/// hash rule of the store allows the cell of such an element that one
/// Merk (`merk/src/store/lignin/hash`, D-287). The batch is taken at
/// RocksDB's cost and root hash, and the store that is closed right
/// after it is sound, on grove versions 1 to 3, for each of the four
/// kinds they have.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_keyed_child_under_a_tree_of_flat_records_leaves_a_sound_store() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn case<T: Appended>(v: &GroveVersion) -> (bool, bool) {
        let tree = T::NAME.as_bytes();
        let batch = || {
            let item = Element::new_item(b"a keyed child".to_vec());
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(vec![], tree.to_vec(), T::empty(None)),
                QualifiedGroveDbOp::insert_or_replace_op(
                    owned(ROOT, tree),
                    b"child".to_vec(),
                    item,
                ),
            ]
        };
        let on_rocksdb = {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path());
            let db = db.expect("a database on RocksDB");
            let applied = db.apply_batch(batch(), None, None, v);
            applied.value.expect("RocksDB takes the batch");
            (
                applied.cost,
                db.root_hash(None, v).unwrap().expect("a root"),
            )
        };
        let dir = tempfile::tempdir().expect("a directory");
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        let applied = db.apply_batch(batch(), None, None, v);
        applied.value.expect("LigninDB takes the batch");
        let on_lignin = (
            applied.cost,
            db.root_hash(None, v).unwrap().expect("a root"),
        );
        db.flush().expect("a flush");
        let closed = catch_unwind(AssertUnwindSafe(move || drop(db))).is_ok();
        let sound = closed && catch_unwind(|| check(dir.path(), v)).is_ok();
        (on_lignin == on_rocksdb, sound)
    }
    for (number, v) in GROVE_VERSIONS.iter().enumerate().take(3) {
        let cases = [
            (Mmr::NAME, case::<Mmr>(v)),
            (Bulk::NAME, case::<Bulk>(v)),
            (Dense::NAME, case::<Dense>(v)),
            (Commitment::NAME, case::<Commitment>(v)),
        ];
        for (name, (equal, sound)) in cases {
            let case = format!("{name}, grove version {}", number + 1);
            assert!(equal, "{case}: the cost and the root hash are RocksDB's");
            assert!(sound, "{case}: the store is closed sound");
        }
    }
}
