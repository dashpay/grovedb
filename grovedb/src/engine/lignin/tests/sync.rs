//! State sync between the engines (GROVE-07).
//!
//! One grove is made on each engine, with every kind of tree the engine
//! takes: a tree of each kind whose data is one Merk, trees in trees, a
//! tree of each append-only kind, an indexed tree of each kind with
//! children and a child tree, references, a tree that was emptied, and
//! trees that nothing was written into, an indexed and an append-only one
//! among them. A second database of either engine is then synced from
//! it, through the calls a node makes: `start_snapshot_syncing`,
//! `fetch_chunk`, `apply_chunk` and `commit_session`.
//!
//! [`Side`] is an engine a test names, with what is checked of a database
//! on it once it is closed: on LigninDB the physical checker reads every
//! page of it, with every hash of every Merk node made again by Merk's
//! rules.

use std::{
    collections::{BTreeSet, VecDeque},
    path::Path,
    pin::Pin,
};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::{costs::ElementCostExtensions, tree_type::ElementTreeTypeExtensions},
    proofs::{Node, Op, Query},
    store::NodeStore,
    tree::value_hash,
    ChunkProducer, Merk, Restorer, TreeFeatureType, TreeType,
};
use grovedb_storage::{
    error::WriteScopeError,
    prefix::{build_prefix, secondary_prefix_for},
    Error as StorageError, FlatStore, RawIterator,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{self, Answer, Appended, Bulk, Commitment, Dense, History, Mmr, Private},
    indexed::{entry, BigSum, Count, CountBigSum, CountSum, Indexed, Sum, SumAndAverage},
    nested::{made_by, number, segments, KINDS},
    queries::path,
    LigninEngine,
};
use crate::{
    engine::Engine,
    operations::indexed_tree::{axis_secondary_tree_type, axis_storage_tag},
    query_result_type::QueryResultType,
    reference_path::ReferencePathType,
    replication::{
        utils::{decode_vec_ops, encode_vec_ops, pack_nested_bytes, unpack_nested_bytes},
        MultiStateSyncSession, RestoreCommitMode, CURRENT_STATE_SYNC_VERSION,
    },
    Element, Error, GroveDb, PathQuery,
};

const VERSION: u16 = CURRENT_STATE_SYNC_VERSION;

/// The `subtrees_batch_size` of a session of these tests: how many subtrees
/// a session finishes before the subtrees it has discovered since wait for
/// those in flight to drain. Fewer than a tree of the grove holds trees, so
/// that discovered subtrees do wait.
const IN_FLIGHT: usize = 4;

/// How many rows a tree of each kind holds: enough for its Merk to be
/// several chunks.
const ROWS: usize = 23;

/// How many children an indexed tree holds beside its child tree.
const CHILDREN: usize = 7;

const KINDS_AT: &[u8] = b"kinds";
const APPENDED_AT: &[u8] = b"appended";
const INDEXED_AT: &[u8] = b"indexed";
const REFERENCES_AT: &[u8] = b"references";
const EMPTIED_AT: &[u8] = b"emptied";
const UNTOUCHED_AT: &[u8] = b"untouched";

/// An engine of these tests.
trait Side: Engine {
    const NAME: &'static str;

    /// What is checked of the database in `dir`, which nothing has open.
    fn check(dir: &Path, v: &GroveVersion);

    /// Takes the open database of `engine` out of the check that its
    /// close makes in a debug build, for a test that leaves in it what
    /// that check finds.
    fn unchecked(engine: &Self);

    /// What is checked of the database in `dir`, which nothing has open
    /// and which holds the commits of a restore that did not finish.
    fn check_incomplete(dir: &Path, v: &GroveVersion);
}

impl Side for LigninEngine {
    const NAME: &'static str = "LigninDB";

    fn check(dir: &Path, v: &GroveVersion) {
        append_only::check(dir, v);
    }

    fn unchecked(engine: &Self) {
        engine.database().unchecked_at_close();
    }

    /// The physical checker finds one thing in it: the value hash of a
    /// tree's element, which commits to a Merk that the restore had not
    /// come to, is not the hash of what the element hosts. Every Merk the
    /// database holds has its entry, under its host, and its host points
    /// at it.
    fn check_incomplete(dir: &Path, v: &GroveVersion) {
        use grovedb_merk::store::lignin::hash::MerkHashRules;
        use lignindb::{Db, Options};
        let db = Db::open(dir, &Options::default()).expect("the database opens again");
        let mut rules = MerkHashRules::for_check(v);
        let report = db.close_and_verify(Some(&mut rules)).expect("a close");
        let rules: BTreeSet<String> = (report.findings.iter())
            .map(|finding| format!("{:?}", finding.rule))
            .collect();
        assert_eq!(
            rules,
            BTreeSet::from(["Hash(Value)".to_string()]),
            "{:#?}",
            report.findings
        );
    }
}

#[cfg(feature = "rocksdb_storage")]
impl Side for crate::RocksDbEngine {
    const NAME: &'static str = "RocksDB";

    fn check(_dir: &Path, _v: &GroveVersion) {}

    fn unchecked(_engine: &Self) {}

    fn check_incomplete(_dir: &Path, _v: &GroveVersion) {}
}

/// The engine `E` behind the handles of the suite's fault seam, which
/// forward to it: a restore through them is `E`'s.
impl<E: Side> Side for crate::tests::faultable_reads::RefusedReads<E> {
    const NAME: &'static str = "an engine behind the fault seam";

    fn check(dir: &Path, v: &GroveVersion) {
        E::check(dir, v);
    }

    fn unchecked(engine: &Self) {
        E::unchecked(&engine.0);
    }

    fn check_incomplete(dir: &Path, v: &GroveVersion) {
        E::check_incomplete(dir, v);
    }
}

/// The subtrees of a grove, by what their data is.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shape {
    /// The path of every tree whose data is one Merk, the root's first,
    /// and whether the tree takes sum items.
    merks: Vec<(Vec<Vec<u8>>, bool)>,
    /// The path of every tree that keeps its data as flat records.
    flats: Vec<Vec<Vec<u8>>>,
    /// The path of every indexed tree, with its axes.
    indexed: Vec<(Vec<Vec<u8>>, Vec<IndexAxis>)>,
}

impl Shape {
    /// The prefix of every subtree of the grove: of the Merks, the flat
    /// keyspaces, and the primary and the secondaries of the indexed
    /// trees. The root Merk is named by the prefix of no path, as a chunk
    /// id names it.
    fn prefixes(&self) -> BTreeSet<[u8; 32]> {
        let prefix = |path: &Vec<Vec<u8>>| match path.is_empty() {
            true => [0; 32],
            false => build_prefix(segments(path).as_slice().into()).value,
        };
        let mut prefixes: BTreeSet<[u8; 32]> = (self.merks.iter().map(|(path, _)| path))
            .chain(&self.flats)
            .map(prefix)
            .collect();
        for (path, axes) in &self.indexed {
            let primary = prefix(path);
            prefixes.insert(primary);
            for axis in axes {
                prefixes.insert(secondary_prefix_for(&primary, axis_storage_tag(*axis)).value);
            }
        }
        prefixes
    }
}

fn row(n: usize) -> Vec<u8> {
    (n as u32 + 1)
        .wrapping_mul(0x9e37_79b9)
        .to_be_bytes()
        .to_vec()
}

/// Inserts `element` under `key` of the tree at `at`, and says whether the
/// grove version took it.
fn put<E: Engine>(
    db: &GroveDb<E>,
    at: &[Vec<u8>],
    key: &[u8],
    element: Element,
    v: &GroveVersion,
) -> bool {
    let inserted = db.insert(segments(at).as_slice(), key, element, None, None, v);
    inserted.unwrap().is_ok()
}

fn must_put<E: Engine>(
    db: &GroveDb<E>,
    at: &[Vec<u8>],
    key: &[u8],
    element: Element,
    v: &GroveVersion,
) {
    assert!(
        put(db, at, key, element, v),
        "an insert under {key:?} is taken"
    );
}

/// A tree of an append-only kind with values in it and one that nothing
/// was appended to, where the grove version makes the kind.
fn appended<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion, shape: &mut Shape) {
    let above = path(&[APPENDED_AT]);
    let filled = T::NAME.as_bytes();
    let empty = format!("{} with nothing", T::NAME).into_bytes();
    if !put(db, &above, filled, T::empty(None), v) {
        return;
    }
    for n in 0..T::DIRECT {
        let appended = T::append(db, &[APPENDED_AT], filled, n, None, v);
        appended.unwrap().expect("an append");
    }
    must_put(db, &above, &empty, T::empty(None), v);
    shape.flats.push(path(&[APPENDED_AT, filled]));
    shape.flats.push(path(&[APPENDED_AT, &empty]));
}

/// An indexed tree of one kind with children and a child that is a tree
/// with elements of its own, and one that nothing was written into, where
/// the grove version makes the kind.
fn indexed<I: Indexed, E: Engine>(db: &GroveDb<E>, v: &GroveVersion, shape: &mut Shape) {
    let above = path(&[INDEXED_AT]);
    let filled = I::NAME.as_bytes();
    let empty = format!("{} with nothing", I::NAME).into_bytes();
    if !put(db, &above, filled, I::empty(), v) {
        return;
    }
    let at: [&[u8]; 2] = [INDEXED_AT, filled];
    for n in 0..CHILDREN {
        let inserted = I::insert(db, &at, &row(n), I::leaf(n), None, v);
        inserted.unwrap().expect("a child");
    }
    let inserted = I::insert(db, &at, b"branch", I::branch(), None, v);
    inserted.unwrap().expect("a child that is a tree");
    let branch = path(&[INDEXED_AT, filled, b"branch"]);
    for n in 0..3 {
        must_put(db, &branch, &row(n), I::in_branch(n), v);
    }
    must_put(db, &above, &empty, I::empty(), v);
    shape.indexed.push((path(&at), I::axes()));
    shape.indexed.push((path(&[INDEXED_AT, &empty]), I::axes()));
    let sums = I::in_branch(0).is_sum_item();
    shape.merks.push((branch, sums));
}

/// Makes the grove of these tests in `db`, and says what subtrees it has.
fn grove<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Shape {
    let mut shape = Shape::default();
    shape.merks.push((Vec::new(), false));
    let tops = [
        KINDS_AT,
        APPENDED_AT,
        INDEXED_AT,
        REFERENCES_AT,
        EMPTIED_AT,
        UNTOUCHED_AT,
    ];
    for top in tops {
        must_put(db, &[], top, Element::empty_tree(), v);
        shape.merks.push((path(&[top]), false));
    }

    for kind in &KINDS[..made_by(v)] {
        let at = path(&[KINDS_AT, kind.name.as_bytes()]);
        must_put(db, &at[..1], &at[1], (kind.make)(), v);
        for n in 0..ROWS {
            must_put(db, &at, &row(n), number(kind.sums, n as i64 * 3 - 20), v);
        }
        shape.merks.push((at, kind.sums));
    }

    // Trees in a tree, three deep, with a tree at the bottom that nothing
    // is written into.
    let plain = path(&[KINDS_AT, b"tree"]);
    let deep = path(&[KINDS_AT, b"tree", b"deep"]);
    let deeper = path(&[KINDS_AT, b"tree", b"deep", b"deeper"]);
    let bottom = path(&[KINDS_AT, b"tree", b"deep", b"deeper", b"bottom"]);
    must_put(db, &plain, b"deep", Element::empty_tree(), v);
    must_put(db, &deep, b"deeper", Element::empty_sum_tree(), v);
    must_put(db, &deeper, b"bottom", Element::empty_count_tree(), v);
    for n in 0..5 {
        must_put(db, &deep, &row(n), Element::new_item(vec![n as u8; 40]), v);
        must_put(db, &deeper, &row(n), Element::new_sum_item(n as i64 - 2), v);
    }
    shape.merks.push((deep, false));
    shape.merks.push((deeper, true));
    shape.merks.push((bottom, false));

    let references = path(&[REFERENCES_AT]);
    let item = Element::new_item(b"what the references lead to".to_vec());
    must_put(db, &references, b"target", item, v);
    let to_a_sibling = ReferencePathType::SiblingReference(b"target".to_vec());
    let to_a_reference = ReferencePathType::SiblingReference(b"to a sibling".to_vec());
    let to_a_row =
        ReferencePathType::AbsolutePathReference(path(&[KINDS_AT, b"tree", b"deep", &row(2)]));
    for (key, to) in [
        (&b"to a sibling"[..], to_a_sibling),
        (b"to a reference", to_a_reference),
        (b"to a row", to_a_row),
    ] {
        must_put(db, &references, key, Element::new_reference(to), v);
    }

    let emptied = path(&[EMPTIED_AT]);
    must_put(db, &emptied, b"gone", Element::new_item(vec![1]), v);
    let deleted = db.delete(segments(&emptied).as_slice(), b"gone", None, None, v);
    deleted.unwrap().expect("a delete");

    appended::<Mmr, E>(db, v, &mut shape);
    appended::<Bulk, E>(db, v, &mut shape);
    appended::<Dense, E>(db, v, &mut shape);
    appended::<Commitment, E>(db, v, &mut shape);
    appended::<Private, E>(db, v, &mut shape);

    indexed::<Count, E>(db, v, &mut shape);
    indexed::<Sum, E>(db, v, &mut shape);
    indexed::<CountSum, E>(db, v, &mut shape);
    indexed::<SumAndAverage, E>(db, v, &mut shape);
    indexed::<BigSum, E>(db, v, &mut shape);
    indexed::<CountBigSum, E>(db, v, &mut shape);
    shape
}

/// What a grove of the grove version `number` holds: the Merks, the flat
/// keyspaces and the indexed trees. A grove version that does not make a
/// kind leaves its trees out, so the counts are asserted.
fn assert_shape(shape: &Shape, number: usize) {
    let kinds = if number >= 5 { 12 } else { 9 };
    let append_only = if number >= 4 { 5 } else { 4 };
    let with_axes = if number >= 5 { 6 } else { 4 };
    // The root, six trees under it, a tree of each kind, three trees in a
    // tree, and the child tree of each filled indexed tree.
    assert_eq!(shape.merks.len(), 1 + 6 + kinds + 3 + with_axes);
    assert_eq!(shape.flats.len(), 2 * append_only);
    assert_eq!(shape.indexed.len(), 2 * with_axes);
}

/// A grove on the engine `E`, with its directory.
struct Made<E: Engine> {
    dir: tempfile::TempDir,
    db: GroveDb<E>,
    shape: Shape,
    root: [u8; 32],
}

fn open<E: Engine>(dir: &Path) -> GroveDb<E> {
    GroveDb::<E>::open_with_engine(dir).expect("a database")
}

impl<E: Engine> Made<E> {
    fn new(v: &GroveVersion) -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let db = open::<E>(dir.path());
        let shape = grove(&db, v);
        let root = db.root_hash(None, v).unwrap().expect("a root hash");
        Self {
            dir,
            db,
            shape,
            root,
        }
    }
}

fn walked<C: RawIterator>(mut cursor: C, named: &str, records: &mut Vec<String>) {
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        let key = hex::encode(cursor.key().unwrap().expect("a key"));
        let record = hex::encode(cursor.value().unwrap().expect("a record"));
        records.push(format!("{named} {key} {record}"));
        cursor.next().unwrap();
    }
    cursor.status().expect("a cursor that walked");
}

/// Every stored record of the subtrees of `shape` in `db`, as a cursor of
/// a read transaction shows it: of a Merk each key with its node record,
/// which holds the node's value, its feature, its hashes and its links,
/// and of a flat keyspace each key with its value.
fn records<E: Engine>(db: &GroveDb<E>, shape: &Shape) -> Vec<String> {
    let engine = db.engine();
    let transaction = engine.start_read_transaction();
    let mut records = Vec::new();
    let merks = shape.merks.iter().map(|(at, _)| at);
    for at in merks.chain(shape.indexed.iter().map(|(at, _)| at)) {
        let handle = engine.subtree(segments(at).as_slice().into(), None, &transaction);
        let handle = handle.unwrap();
        walked(NodeStore::cursor(&handle), &format!("{at:?}"), &mut records);
    }
    for (at, axes) in &shape.indexed {
        let primary = build_prefix(segments(at).as_slice().into()).value;
        for axis in axes {
            let prefix = secondary_prefix_for(&primary, axis_storage_tag(*axis)).value;
            let handle = engine.subtree_by_prefix(prefix, None, &transaction);
            let handle = handle.unwrap();
            let named = format!("{at:?} {axis:?}");
            walked(NodeStore::cursor(&handle), &named, &mut records);
        }
    }
    for at in &shape.flats {
        let handle = engine.subtree(segments(at).as_slice().into(), None, &transaction);
        let handle = handle.unwrap();
        walked(FlatStore::cursor(&handle), &format!("{at:?}"), &mut records);
    }
    records
}

fn everything(at: &[Vec<u8>]) -> PathQuery {
    let mut query = Query::new();
    query.insert_all();
    PathQuery::new_unsized(at.to_vec(), query)
}

/// Reads of the whole grove, with what each costs: the elements of every
/// tree, a proof of them, the element of every tree that keeps its data
/// flat, and the verification of the grove.
fn reads<E: Engine>(db: &GroveDb<E>, shape: &Shape, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    h.read("root hash", db.root_hash(None, v));
    let merks = shape.merks.iter().map(|(at, _)| at);
    for at in merks.chain(shape.indexed.iter().map(|(at, _)| at)) {
        let all = everything(at);
        let pairs = QueryResultType::QueryKeyElementPairResultType;
        let read = db.query_raw(&all, true, true, true, pairs, None, v);
        h.elements(format!("elements of {at:?}"), read);
        h.read(format!("proof of {at:?}"), db.prove_query(&all, None, v));
    }
    for at in &shape.flats {
        let (key, above) = at.split_last().expect("a tree under the root");
        let read = db.get(segments(above).as_slice(), key, None, v);
        h.element(format!("element of {at:?}"), read);
    }
    let verified = db.verify_grovedb(None, true, false, v);
    h.plain("verify", verified.map(|issues| issues.len()));
    h.answers
}

fn append_more<'db, T: Appended, E: Engine>(h: &mut History<'db, E>, shape: &Shape) {
    let (db, v) = (h.db, h.v);
    for name in [T::NAME.to_string(), format!("{} with nothing", T::NAME)] {
        if !shape.flats.contains(&path(&[APPENDED_AT, name.as_bytes()])) {
            continue;
        }
        for n in [T::DIRECT, T::DIRECT + 1] {
            let appended = T::append(db, &[APPENDED_AT], name.as_bytes(), n, None, v);
            h.write(format!("append to {name}"), appended);
        }
    }
}

fn index_more<'db, I: Indexed, E: Engine>(h: &mut History<'db, E>, shape: &Shape) {
    let (db, v) = (h.db, h.v);
    for name in [I::NAME.to_string(), format!("{} with nothing", I::NAME)] {
        let at: [&[u8]; 2] = [INDEXED_AT, name.as_bytes()];
        if !shape.indexed.iter().any(|(made, _)| *made == path(&at)) {
            continue;
        }
        for n in [CHILDREN, CHILDREN + 1] {
            let inserted = I::insert(db, &at, &row(n), I::leaf(n), None, v);
            h.write(format!("a child of {name}"), inserted);
        }
        let removed = I::delete(db, &at, &row(CHILDREN), None, v);
        h.write(format!("a child of {name} goes"), removed);
    }
}

/// Writes into every subtree of the grove, with what each costs and the
/// root hash after it: an indexed tree in the place of the tree that
/// nothing was written into, a row more in every other Merk, appends to
/// every append-only tree, children of every indexed tree, and a row
/// replaced and one deleted.
fn writes<E: Engine>(db: &GroveDb<E>, shape: &Shape, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);

    // The tree that nothing was written into is deleted, an indexed tree
    // is made under its key, and children are written into it: the first
    // write into the primary and the secondaries of a tree whose key held
    // another tree before.
    let untouched = path(&[UNTOUCHED_AT]);
    let deleted = db.delete(super::ROOT, UNTOUCHED_AT, None, None, v);
    h.write("the tree nothing was written into goes", deleted);
    let made = db.insert(super::ROOT, UNTOUCHED_AT, Count::empty(), None, None, v);
    h.write("an indexed tree under its key", made);
    for n in 0..3 {
        let child = Count::insert(db, &[UNTOUCHED_AT], &row(n), Count::leaf(n), None, v);
        h.write("a child of the indexed tree under its key", child);
    }

    for (at, sums) in shape.merks.iter().filter(|(at, _)| *at != untouched) {
        let element = number(*sums, 77);
        let inserted = db.insert(segments(at).as_slice(), b"added", element, None, None, v);
        h.write(format!("a row more in {at:?}"), inserted);
    }
    let plain = path(&[KINDS_AT, b"tree"]);
    let plain = segments(&plain);
    let again = db.insert(plain.as_slice(), &row(1), number(false, 9), None, None, v);
    h.write("a row replaced", again);
    let deleted = db.delete(plain.as_slice(), &row(2), None, None, v);
    h.write("a row deleted", deleted);

    append_more::<Mmr, E>(&mut h, shape);
    append_more::<Bulk, E>(&mut h, shape);
    append_more::<Dense, E>(&mut h, shape);
    append_more::<Commitment, E>(&mut h, shape);
    append_more::<Private, E>(&mut h, shape);

    index_more::<Count, E>(&mut h, shape);
    index_more::<Sum, E>(&mut h, shape);
    index_more::<CountSum, E>(&mut h, shape);
    index_more::<SumAndAverage, E>(&mut h, shape);
    index_more::<BigSum, E>(&mut h, shape);
    index_more::<CountBigSum, E>(&mut h, shape);

    let verified = db.verify_grovedb(None, true, false, v);
    h.plain("verify", verified.map(|issues| issues.len()));
    h.answers
}

/// A request of a session, with the payload its source answered it with.
type Served = (Vec<u8>, Vec<u8>);

type Session<'db, T> = Pin<Box<MultiStateSyncSession<'db, T>>>;

/// The subtree prefix each chunk id of `request` names, read from the
/// bytes of the id and not by the decoder a node reads it with: the first
/// 32 bytes of the id. The first request of a session is no such id: it is
/// the root hash of the grove, alone, and names the root Merk, whose
/// prefix is that of no path.
fn named(root: &[u8; 32], request: &[u8]) -> Vec<[u8; 32]> {
    if request == root {
        return vec![[0; 32]];
    }
    let ids = unpack_nested_bytes(request).expect("a packed request");
    let prefix = |id: &Vec<u8>| {
        assert!(id.len() > 32, "a chunk id holds more than a prefix");
        <[u8; 32]>::try_from(&id[..32]).expect("32 bytes")
    };
    ids.iter().map(prefix).collect()
}

/// `payload` with the part of each subtree as `change` leaves it.
fn changed(
    root: &[u8; 32],
    request: &[u8],
    payload: &[u8],
    change: &mut dyn FnMut(&[u8; 32], Vec<u8>) -> Vec<u8>,
) -> Vec<u8> {
    let parts = unpack_nested_bytes(payload).expect("a packed payload");
    let prefixes = named(root, request);
    assert_eq!(prefixes.len(), parts.len());
    let parts = prefixes.iter().zip(parts);
    pack_nested_bytes(parts.map(|(prefix, part)| change(prefix, part)).collect())
        .expect("a payload is packed again")
}

/// The requests of one session against one source: those the session has
/// asked for and was not answered yet, and those that were served.
struct Requests<'s, S: Engine> {
    source: &'s GroveDb<S>,
    v: &'s GroveVersion,
    asked: VecDeque<Vec<u8>>,
    served: Vec<Served>,
}

impl<'s, S: Engine> Requests<'s, S> {
    fn new(source: &'s GroveDb<S>, root: [u8; 32], v: &'s GroveVersion) -> Self {
        Self {
            source,
            v,
            asked: VecDeque::from([root.to_vec()]),
            served: Vec::new(),
        }
    }

    /// The next request with the payload the source answers it with, or
    /// `None` once the session has asked for nothing more.
    fn fetch(&mut self) -> Option<Served> {
        let request = self.asked.pop_front()?;
        let fetched = self.source.fetch_chunk(&request, None, VERSION, self.v);
        let payload = fetched.expect("the source serves the request");
        self.served.push((request.clone(), payload.clone()));
        Some((request, payload))
    }

    /// Gives `session` the payload of a request, and takes in what it asks
    /// for next.
    fn apply<T: Engine>(
        &mut self,
        session: &mut Session<'_, T>,
        request: &[u8],
        payload: &[u8],
    ) -> Result<(), Error> {
        let more = session.apply_chunk(request, payload, VERSION, self.v)?;
        self.asked.extend(more);
        Ok(())
    }
}

fn begin<'db, T: Engine>(
    target: &'db GroveDb<T>,
    root: [u8; 32],
    mode: RestoreCommitMode,
    v: &GroveVersion,
) -> Result<Session<'db, T>, Error> {
    target.start_snapshot_syncing_with_mode(root, IN_FLIGHT, VERSION, mode, v)
}

/// A whole session: `target` is synced from `source` and the session is
/// committed. Returns every request with its payload, in the order the
/// session made them, and how many times the session committed before its
/// end.
fn whole<S: Engine, T: Engine>(
    source: &GroveDb<S>,
    target: &GroveDb<T>,
    mode: RestoreCommitMode,
    v: &GroveVersion,
) -> (Vec<Served>, usize) {
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    let mut session = begin(target, root, mode, v).expect("a session starts");
    let mut requests = Requests::new(source, root, v);
    while let Some((request, payload)) = requests.fetch() {
        let applied = requests.apply(&mut session, &request, &payload);
        applied.expect("the chunk is taken");
    }
    assert!(session.is_sync_completed());
    let commits = session.intermediate_commits();
    target
        .commit_session(session, v)
        .expect("the session commits");
    (requests.served, commits)
}

/// What a sync left, and what the synced grove answers.
#[derive(Debug, PartialEq)]
struct Round {
    served: Vec<Served>,
    /// How many times the session committed before its end.
    commits: usize,
    /// How many proofs the grove version of the grove does not make.
    unproven: usize,
    records: Vec<String>,
    reads: Vec<Answer>,
    writes: Vec<Answer>,
}

/// Every call of a history was answered, but for the proofs that a grove
/// version below 3 does not make: those that need the proof version these
/// grove versions do not have, and are refused as not supported with the
/// name of that proof version. They are proofs of an indexed tree or of a
/// tree in one, and of the tree that commits to a count and a sum, and of
/// no other tree. Returns how many those are. A history is compared with
/// the same history on another grove, and calls that every grove refuses
/// alike would compare as equal.
fn assert_answered(answers: &[Answer], shape: &Shape) -> usize {
    let proof = |at: &Vec<Vec<u8>>| format!("proof of {at:?}");
    let in_indexed = |at: &&Vec<Vec<u8>>| at.len() > 1 && at[0] == INDEXED_AT;
    let merks = shape.merks.iter().map(|(at, _)| at);
    let indexed = shape.indexed.iter().map(|(at, _)| at);
    let mut later: Vec<String> = merks.chain(indexed).filter(in_indexed).map(proof).collect();
    later.push(proof(&path(&[
        KINDS_AT,
        b"provable count provable sum tree",
    ])));

    let mut unproven = 0;
    for (call, _, answer) in answers {
        if answer.starts_with("Ok(") {
            continue;
        }
        let old_version = answer.starts_with("Err(NotSupported(") && answer.contains("V1 proof");
        assert!(later.contains(call) && old_version, "{call}: {answer}");
        unproven += 1;
    }
    unproven
}

/// Syncs a new database on `T` from `source`: it ends on the root hash of
/// the source, its verification finds no issue, and the database is
/// checked as it is closed after the sync and again after every subtree
/// was written.
fn round<S: Side, T: Side>(source: &Made<S>, mode: RestoreCommitMode, v: &GroveVersion) -> Round {
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    let (served, commits) = whole(&source.db, &target, mode, v);
    let root = target.root_hash(None, v).unwrap().expect("a root hash");
    assert_eq!(root, source.root, "{} from {}", T::NAME, S::NAME);
    let issues = target.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty());
    let records = records(&target, &source.shape);
    let reads = reads(&target, &source.shape, v);
    let unproven = assert_answered(&reads, &source.shape);
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);

    let target = open::<T>(dir.path());
    let writes = writes(&target, &source.shape, v);
    assert_eq!(assert_answered(&writes, &source.shape), 0);
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);
    Round {
        served,
        commits,
        unproven,
        records,
        reads,
        writes,
    }
}

/// LigninDB is synced from LigninDB on every grove version: the synced
/// grove has the root hash, the records and the answers of its source,
/// and takes the writes its source takes.
#[test]
fn a_grove_on_lignindb_syncs_from_one_on_lignindb() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let source = Made::<LigninEngine>::new(v);
        assert_shape(&source.shape, number + 1);
        let synced = round::<LigninEngine, LigninEngine>(&source, RestoreCommitMode::Atomic, v);
        assert_eq!(synced.records, records(&source.db, &source.shape));
        assert_eq!(synced.reads, reads(&source.db, &source.shape, v));
        assert_eq!(synced.writes, writes(&source.db, &source.shape, v));
    }
}

/// An engine that wraps another passes on what a restore asks of it: a
/// database behind the handles of the suite's fault seam is synced, to
/// the root hash and the records of its source, and the LigninDB database
/// under it passes its check.
#[test]
fn a_grove_syncs_into_a_database_behind_the_fault_seam() {
    use crate::tests::faultable_reads::RefusedReads;
    let v = GroveVersion::latest();
    let source = Made::<LigninEngine>::new(v);
    let atomic = RestoreCommitMode::Atomic;
    let synced = round::<LigninEngine, RefusedReads<LigninEngine>>(&source, atomic, v);
    assert_eq!(synced.records, records(&source.db, &source.shape));
}

/// Of the subtrees that the elements of this grove host, a synced grove on
/// LigninDB has an entry in the subtree directory for each one the restore
/// wrote into and for none of the others: for a Merk that has a node, for
/// an append-only tree that has an entry, and for every Merk of an indexed
/// tree of which one has a node. The grove it was synced from has the same
/// entries and one more, of the tree that was written into and emptied.
#[test]
fn a_synced_grove_has_an_entry_for_each_subtree_the_restore_wrote_into() {
    let v = GroveVersion::latest();
    let source = Made::<LigninEngine>::new(v);
    let shape = &source.shape;
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<LigninEngine>(dir.path());
    whole(&source.db, &target, RestoreCommitMode::Atomic, v);

    // Each subtree with whether it holds a record, and the subtrees of an
    // indexed tree with whether one of them does.
    let engine = target.engine();
    let transaction = engine.start_read_transaction();
    let prefix_of = |at: &Vec<Vec<u8>>| build_prefix(segments(at).as_slice().into()).value;
    let mut subtrees: Vec<(String, [u8; 32], bool)> = Vec::new();
    for (at, _) in shape.merks.iter().filter(|(at, _)| !at.is_empty()) {
        let handle = engine.subtree(segments(at).as_slice().into(), None, &transaction);
        let mut nodes = Vec::new();
        walked(NodeStore::cursor(&handle.unwrap()), "", &mut nodes);
        subtrees.push((format!("{at:?}"), prefix_of(at), !nodes.is_empty()));
    }
    for at in &shape.flats {
        let handle = engine.subtree(segments(at).as_slice().into(), None, &transaction);
        let mut flat = Vec::new();
        walked(FlatStore::cursor(&handle.unwrap()), "", &mut flat);
        subtrees.push((format!("{at:?}"), prefix_of(at), !flat.is_empty()));
    }
    for (at, axes) in &shape.indexed {
        let primary = prefix_of(at);
        let mut prefixes = vec![primary];
        let secondary = |axis: &IndexAxis| secondary_prefix_for(&primary, axis_storage_tag(*axis));
        prefixes.extend(axes.iter().map(|axis| secondary(axis).value));
        let mut nodes = Vec::new();
        for prefix in &prefixes {
            let handle = engine.subtree_by_prefix(*prefix, None, &transaction);
            walked(NodeStore::cursor(&handle.unwrap()), "", &mut nodes);
        }
        let named = prefixes.iter().enumerate();
        subtrees
            .extend(named.map(|(n, prefix)| (format!("{at:?} {n}"), *prefix, !nodes.is_empty())));
    }
    drop(transaction);

    let emptied = format!("{:?}", path(&[EMPTIED_AT]));
    let (mut with, mut without) = (0, 0);
    for (name, prefix, holds) in subtrees {
        let synced = entry(&target, prefix).is_some();
        assert_eq!(synced, holds, "{name}");
        let made = entry(&source.db, prefix).is_some();
        assert_eq!(made, holds || name == emptied, "{name}");
        match holds {
            true => with += 1,
            false => without += 1,
        }
    }
    // Without: the emptied tree and the two trees nothing was written
    // into, one append-only tree of each kind, and every Merk of one
    // indexed tree of each kind.
    let nothing = |at: &Vec<Vec<u8>>| at.last().is_some_and(|key| key.ends_with(b" with nothing"));
    let of_indexed = shape.indexed.iter().filter(|(at, _)| nothing(at));
    let of_indexed: usize = of_indexed.map(|(_, axes)| 1 + axes.len()).sum();
    assert!(
        of_indexed >= 2 * 6,
        "{of_indexed} Merks of empty indexed trees"
    );
    assert_eq!(without, 3 + shape.flats.len() / 2 + of_indexed);
    assert!(with > without, "{with} subtrees hold data");
}

/// On every grove version a grove is made on each engine and synced in
/// the four directions. Every sync ends on the root hash of its source
/// and passes `verify_grovedb` (`round`). The four sessions make the same
/// requests and are served the same bytes, so a chunk of LigninDB is the
/// chunk RocksDB serves, and the requests name the prefix of every subtree
/// of the grove and no other. The four synced groves hold the records of
/// the sources, value for value and hash for hash, answer every read as
/// the sources do at the same cost, and take the same writes at the same
/// cost with the same root hash after each.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_grove_syncs_between_the_engines_in_all_four_directions() {
    type Rocks = crate::RocksDbEngine;
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let rocks = Made::<Rocks>::new(v);
        let lignin = Made::<LigninEngine>::new(v);
        assert_shape(&rocks.shape, number);
        assert_eq!(rocks.shape, lignin.shape);
        assert_eq!(rocks.root, lignin.root);
        let shape = &rocks.shape;

        let atomic = RestoreCommitMode::Atomic;
        let first = round::<Rocks, Rocks>(&rocks, atomic, v);
        let others = [
            round::<Rocks, LigninEngine>(&rocks, atomic, v),
            round::<LigninEngine, Rocks>(&lignin, atomic, v),
            round::<LigninEngine, LigninEngine>(&lignin, atomic, v),
        ];
        for (direction, other) in others.iter().enumerate() {
            let told = format!("direction {direction} on grove version {number}");
            assert_eq!(first.served, other.served, "{told}");
            assert_eq!(first.commits, 0, "{told}");
            assert_eq!(other.commits, 0, "{told}");
            assert_eq!(first.unproven, other.unproven, "{told}");
            assert_eq!(first.records, other.records, "{told}");
            assert_eq!(first.reads, other.reads, "{told}");
            assert_eq!(first.writes, other.writes, "{told}");
        }

        // The sources hold and answer what the synced groves do.
        for (source, read, written) in [
            (
                records(&rocks.db, shape),
                reads(&rocks.db, shape, v),
                writes(&rocks.db, shape, v),
            ),
            (
                records(&lignin.db, shape),
                reads(&lignin.db, shape, v),
                writes(&lignin.db, shape, v),
            ),
        ] {
            assert_eq!(first.records, source, "grove version {number}");
            assert_eq!(first.reads, read, "grove version {number}");
            assert_eq!(first.writes, written, "grove version {number}");
        }

        // Grove versions 1 and 2 make no proof that needs the later proof
        // version; every other read is answered on every version.
        assert_eq!(first.unproven > 0, number < 3, "grove version {number}");

        // A chunk id names its subtree by the 32 bytes of its prefix, and
        // the first request is the root hash of the grove.
        assert_eq!(first.served[0].0, rocks.root.to_vec());
        let later = first.served[1..].iter();
        assert!(later
            .clone()
            .all(|(request, _)| *request != rocks.root.to_vec()));
        let asked: BTreeSet<[u8; 32]> = (first.served.iter())
            .flat_map(|(request, _)| named(&rocks.root, request))
            .collect();
        assert_eq!(asked, shape.prefixes(), "grove version {number}");
        // The session came back to a subtree for the chunks below its
        // first, so there are more chunk ids than subtrees.
        let ids = (first.served.iter()).map(|(request, _)| named(&rocks.root, request).len());
        let ids: usize = ids.sum();
        assert!(ids > asked.len(), "{ids} chunk ids");

        lignin.db.flush().expect("a flush");
        drop(lignin.db);
        LigninEngine::check(lignin.dir.path(), v);
        println!(
            "grove version {number}: {} subtrees, {} requests of {ids} chunk ids, {} bytes \
             served, {} records, {} reads, {} writes",
            asked.len(),
            first.served.len(),
            first.served.iter().map(|(_, p)| p.len()).sum::<usize>(),
            first.records.len(),
            first.reads.len(),
            first.writes.len(),
        );
    }
}

/// The prefix of the tree of the grove that the lifecycle tests refuse a
/// chunk of, or stop before: a tree two trees deep, so that the root Merk
/// and the tree above it were handed over by then.
fn victim() -> [u8; 32] {
    let at = path(&[KINDS_AT, b"tree"]);
    build_prefix(segments(&at).as_slice().into()).value
}

/// The first chunk of a subtree's payload with its last item given as a
/// node of another kind of tree: with the feature of a node that adds to a
/// sum. An item of a plain tree and that node hash alike, so the chunk
/// hashes to what its restore expects, and is refused at that node.
fn with_a_foreign_node(part: Vec<u8>) -> Vec<u8> {
    let mut chunks = unpack_nested_bytes(&part).expect("the chunks of a subtree");
    let mut ops = decode_vec_ops(&chunks[0]).expect("a chunk");
    let item = ops.iter_mut().rev().find_map(|op| match op {
        Op::Push(node @ Node::KV(..)) | Op::PushInverted(node @ Node::KV(..)) => Some(node),
        _ => None,
    });
    let item = item.expect("a chunk with an item");
    let Node::KV(key, value) = item.clone() else {
        unreachable!("an item was found");
    };
    let hash = value_hash(&value).unwrap();
    let foreign = TreeFeatureType::SummedMerkNode(1);
    *item = Node::KVValueHashFeatureType(key, value, hash, foreign);
    chunks[0] = encode_vec_ops(ops).expect("a chunk is encoded again");
    pack_nested_bytes(chunks).expect("the chunks are packed again")
}

/// What a database that a session left without committing must be: what
/// it was before the session.
struct Before {
    root: [u8; 32],
    records: Vec<String>,
}

impl Before {
    fn of<T: Engine>(target: &GroveDb<T>, shape: &Shape, v: &GroveVersion) -> Self {
        Self {
            root: target.root_hash(None, v).unwrap().expect("a root hash"),
            records: records(target, shape),
        }
    }

    fn assert_left<T: Engine>(&self, target: &GroveDb<T>, shape: &Shape, v: &GroveVersion) {
        assert_eq!(
            target.root_hash(None, v).unwrap().expect("a root"),
            self.root
        );
        assert_eq!(records(target, shape), self.records);
    }
}

/// The life of sessions on one target: a session that is refused a chunk,
/// a session that is abandoned between two hand-overs, and a session that
/// runs to its commit.
///
/// - A chunk that hashes to what its restore expects and is refused at a
///   node ends the session: the same chunk as the source served it, and
///   the commit, are refused by the session, which asks no restorer again.
/// - A session that is refused a chunk, and one that is dropped while
///   subtrees are in flight, leave the target with the root hash and the
///   records it had, and the closed database passes its check.
/// - The session after them ends on the root hash of the source.
fn sessions<S: Side, T: Side>(v: &GroveVersion) {
    let source = Made::<S>::new(v);
    let shape = &source.shape;
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    let before = Before::of(&target, shape, v);
    let atomic = RestoreCommitMode::Atomic;

    {
        let mut session = begin(&target, source.root, atomic, v).expect("a session starts");
        let mut requests = Requests::new(&source.db, source.root, v);
        let refused = loop {
            let (request, payload) = requests.fetch().expect("the victim is asked for");
            if !named(&source.root, &request).contains(&victim()) {
                let applied = requests.apply(&mut session, &request, &payload);
                applied.expect("the chunk is taken");
                continue;
            }
            let altered =
                changed(
                    &source.root,
                    &request,
                    &payload,
                    &mut |prefix, part| match *prefix == victim() {
                        true => with_a_foreign_node(part),
                        false => part,
                    },
                );
            let error = requests.apply(&mut session, &request, &altered);
            let error = error.expect_err("a chunk with a foreign node is refused");
            assert!(
                error
                    .to_string()
                    .contains("does not belong to the tree type"),
                "{error}"
            );
            break (request, payload);
        };
        assert!(requests.served.len() > 2, "subtrees were handed over");
        let (request, payload) = refused;
        let again = requests.apply(&mut session, &request, &payload);
        let again = again.expect_err("a failed session takes no chunk");
        assert!(again.to_string().contains("session has failed"), "{again}");
        assert!(!session.is_sync_completed());
        let committed = target.commit_session(session, v);
        let committed = committed.expect_err("a failed session does not commit");
        assert!(committed.to_string().contains("failed"), "{committed}");
    }
    before.assert_left(&target, shape, v);

    {
        let mut session = begin(&target, source.root, atomic, v).expect("a second session");
        let mut requests = Requests::new(&source.db, source.root, v);
        loop {
            let (request, payload) = requests.fetch().expect("the victim is asked for");
            if named(&source.root, &request).contains(&victim()) {
                break;
            }
            let applied = requests.apply(&mut session, &request, &payload);
            applied.expect("the chunk is taken");
        }
        assert!(requests.served.len() > 2, "subtrees were handed over");
        assert!(!session.is_sync_completed());
        drop(session);
    }
    before.assert_left(&target, shape, v);
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);

    let target = open::<T>(dir.path());
    before.assert_left(&target, shape, v);
    whole(&source.db, &target, atomic, v);
    assert_eq!(
        target.root_hash(None, v).unwrap().expect("a root"),
        source.root
    );
    let issues = target.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty());
    assert_eq!(records(&target, shape), records(&source.db, shape));
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);
}

#[test]
fn a_refused_chunk_and_an_abandoned_session_leave_nothing_on_lignindb() {
    for v in [&GROVE_VERSIONS[0], GroveVersion::latest()] {
        sessions::<LigninEngine, LigninEngine>(v);
    }
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_refused_chunk_and_an_abandoned_session_leave_nothing_between_the_engines() {
    type Rocks = crate::RocksDbEngine;
    let v = GroveVersion::latest();
    sessions::<Rocks, Rocks>(v);
    sessions::<Rocks, LigninEngine>(v);
    sessions::<LigninEngine, Rocks>(v);
}

/// A session that may commit what it has restored before its end, with a
/// budget of one byte and one subtree in flight: it commits more than
/// three times before its end, ends on the root hash of its source with
/// the records of its source, and passes the checks of [`round`].
/// One that is dropped after such a commit leaves a database that says it
/// holds an incomplete restore and refuses the next session, and in which
/// every Merk that a commit of the session holds is entered and named by
/// its host ([`Side::check_incomplete`]).
fn in_parts<S: Side, T: Side>(v: &GroveVersion) {
    let source = Made::<S>::new(v);
    let shape = &source.shape;
    let parts = RestoreCommitMode::Incremental {
        budget_bytes: 1,
        max_subtrees_in_flight: 1,
    };

    let synced = round::<S, T>(&source, parts, v);
    assert!(
        synced.commits > 3,
        "{} commits before the end",
        synced.commits
    );
    assert_eq!(synced.records, records(&source.db, shape));

    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    {
        let mut session = begin(&target, source.root, parts, v).expect("a session starts");
        let mut requests = Requests::new(&source.db, source.root, v);
        while session.intermediate_commits() < 3 {
            let (request, payload) = requests.fetch().expect("a request");
            let applied = requests.apply(&mut session, &request, &payload);
            applied.expect("the chunk is taken");
        }
        assert!(!session.is_sync_completed());
        drop(session);
    }
    assert!(target.has_incomplete_restore().expect("a read of the mark"));
    let starts = begin(&target, source.root, parts, v).is_ok();
    assert!(!starts, "a marked database starts no session");
    target.flush().expect("a flush");
    T::unchecked(target.engine());
    drop(target);
    T::check_incomplete(dir.path(), v);
}

#[test]
fn a_session_that_commits_in_parts_leaves_a_sound_database_on_lignindb() {
    in_parts::<LigninEngine, LigninEngine>(GroveVersion::latest());
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_session_that_commits_in_parts_syncs_between_the_engines() {
    type Rocks = crate::RocksDbEngine;
    let v = GroveVersion::latest();
    in_parts::<Rocks, Rocks>(v);
    in_parts::<Rocks, LigninEngine>(v);
    in_parts::<LigninEngine, Rocks>(v);
}

/// A database that holds a grove is synced after a wipe, and ends on the
/// root hash of its source.
fn after_a_wipe<S: Side, T: Side>(v: &GroveVersion) {
    let source = Made::<S>::new(v);
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    must_put(&target, &[], b"of before", Element::empty_tree(), v);
    let of_before = path(&[b"of before"]);
    must_put(
        &target,
        &of_before,
        b"item",
        Element::new_item(vec![7; 9]),
        v,
    );
    target.wipe().expect("a wipe");
    whole(&source.db, &target, RestoreCommitMode::Atomic, v);
    assert_eq!(
        target.root_hash(None, v).unwrap().expect("a root"),
        source.root
    );
    assert_eq!(
        records(&target, &source.shape),
        records(&source.db, &source.shape)
    );
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);
}

#[test]
fn a_database_that_was_wiped_is_synced_on_lignindb() {
    after_a_wipe::<LigninEngine, LigninEngine>(GroveVersion::latest());
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_database_that_was_wiped_is_synced_between_the_engines() {
    type Rocks = crate::RocksDbEngine;
    let v = GroveVersion::latest();
    after_a_wipe::<Rocks, Rocks>(v);
    after_a_wipe::<Rocks, LigninEngine>(v);
    after_a_wipe::<LigninEngine, Rocks>(v);
}

/// On LigninDB a restore over a Merk that holds nodes is refused where the
/// Merk is handed over, so a session into a database that holds a grove
/// and was not wiped fails at the root Merk and leaves the database as it
/// was. So does a session whose source holds no element, before anything
/// is handed over.
#[test]
fn a_session_into_a_grove_that_was_not_wiped_is_refused_on_lignindb() {
    let v = GroveVersion::latest();
    let source = Made::<LigninEngine>::new(v);
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<LigninEngine>(dir.path());
    must_put(&target, &[], b"of before", Element::empty_tree(), v);
    let before = Before::of(&target, &source.shape, v);
    let of_before = target.get(super::ROOT, b"of before", None, v);
    let of_before = of_before.unwrap().expect("the tree of before");

    let atomic = RestoreCommitMode::Atomic;
    let mut session = begin(&target, source.root, atomic, v).expect("a session starts");
    let mut requests = Requests::new(&source.db, source.root, v);
    let refused = loop {
        let (request, payload) = requests.fetch().expect("the root Merk is not handed over");
        if let Err(error) = requests.apply(&mut session, &request, &payload) {
            break error;
        }
    };
    assert!(refused.to_string().contains("NotSupported"), "{refused}");
    drop(session);
    before.assert_left(&target, &source.shape, v);
    let read = target.get(super::ROOT, b"of before", None, v);
    assert_eq!(read.unwrap().expect("the tree of before"), of_before);

    // From a source with no element the session is handed no Merk: it
    // finds the root Merk of its target not empty, and fails there.
    let nothing_dir = tempfile::tempdir().expect("a directory");
    let nothing = open::<LigninEngine>(nothing_dir.path());
    let root = nothing.root_hash(None, v).unwrap().expect("a root hash");
    let mut session = begin(&target, root, atomic, v).expect("a session starts");
    let mut requests = Requests::new(&nothing, root, v);
    let (request, payload) = requests.fetch().expect("the root Merk is asked for");
    let refused = requests.apply(&mut session, &request, &payload);
    let refused = refused.expect_err("a root Merk that holds nodes is no empty one");
    assert!(
        refused.to_string().contains("non-null root hash"),
        "{refused}"
    );
    drop(session);
    before.assert_left(&target, &source.shape, v);
    drop(target);
    LigninEngine::check(dir.path(), v);
}

/// What a receiving node makes of an empty chunk in the place of a
/// subtree's payload: the answer LigninDB gives a request that names a
/// subtree of flat records as a Merk. For a Merk that its parent commits
/// to as holding nodes, for an append-only tree and for an indexed tree,
/// the session is refused the chunk, commits nothing, and leaves the
/// target as it was.
fn an_empty_chunk_is_refused<S: Side, T: Side>(v: &GroveVersion) -> Vec<String> {
    let source = Made::<S>::new(v);
    let shape = &source.shape;
    let victims = [
        path(&[KINDS_AT, b"tree"]),
        path(&[KINDS_AT, b"provable count tree"]),
        path(&[APPENDED_AT, Mmr::NAME.as_bytes()]),
        path(&[APPENDED_AT, Bulk::NAME.as_bytes()]),
        path(&[APPENDED_AT, Dense::NAME.as_bytes()]),
        path(&[APPENDED_AT, Commitment::NAME.as_bytes()]),
        path(&[APPENDED_AT, Private::NAME.as_bytes()]),
        path(&[INDEXED_AT, Count::NAME.as_bytes()]),
    ];
    let empty = pack_nested_bytes(vec![vec![]]).expect("an empty chunk");
    let mut refusals = Vec::new();
    for at in victims {
        let hollowed = build_prefix(segments(&at).as_slice().into()).value;
        assert!(
            shape.prefixes().contains(&hollowed),
            "{at:?} is in the grove"
        );
        let dir = tempfile::tempdir().expect("a directory");
        let target = open::<T>(dir.path());
        let before = Before::of(&target, shape, v);
        let atomic = RestoreCommitMode::Atomic;
        let mut session = begin(&target, source.root, atomic, v).expect("a session starts");
        let mut requests = Requests::new(&source.db, source.root, v);
        let refused = loop {
            let (request, payload) = requests.fetch().expect("the subtree is asked for");
            let mut emptied = false;
            let altered =
                changed(
                    &source.root,
                    &request,
                    &payload,
                    &mut |prefix, part| match *prefix == hollowed {
                        true => {
                            emptied = true;
                            empty.clone()
                        }
                        false => part,
                    },
                );
            match requests.apply(&mut session, &request, &altered) {
                Ok(()) => assert!(!emptied, "an empty chunk for {at:?} is taken"),
                Err(error) => {
                    assert!(emptied, "{error}");
                    break error;
                }
            }
        };
        assert!(!session.is_sync_completed());
        let committed = target.commit_session(session, v);
        assert!(committed.is_err(), "a session that was refused commits");
        before.assert_left(&target, shape, v);
        refusals.push(format!("{at:?}: {refused}"));
        target.flush().expect("a flush");
        drop(target);
        T::check(dir.path(), v);
    }
    refusals
}

/// What a receiving node makes of an indexed tree that holds children and
/// is served as one that holds none. Its header request is answered with
/// a header that gives the hash of an empty Merk for the primary and for
/// each axis, and an empty root chunk of the primary, which is what
/// LigninDB answers a header request that names a subtree of flat
/// records. The request of each secondary after it is answered with an
/// empty chunk. The session takes the header and the empty Merks, and
/// refuses them where it holds the roots of the tree's Merks against what
/// the element commits to. It commits nothing, and leaves the target as
/// it was.
fn an_indexed_tree_served_as_empty_is_refused<S: Side, T: Side>(v: &GroveVersion) -> String {
    use grovedb_merk::tree::hash::NULL_HASH;

    use crate::replication::indexed_sync::IndexedHeader;
    let source = Made::<S>::new(v);
    let shape = &source.shape;
    let at = path(&[INDEXED_AT, CountSum::NAME.as_bytes()]);
    let primary = build_prefix(segments(&at).as_slice().into()).value;
    let axes = CountSum::axes();
    assert!(axes.len() > 1, "a tree with more than one axis");
    let secondary = |axis: &IndexAxis| secondary_prefix_for(&primary, axis_storage_tag(*axis));
    let secondaries: Vec<[u8; 32]> = axes.iter().map(|axis| secondary(axis).value).collect();
    let header = IndexedHeader {
        primary_root_hash: NULL_HASH,
        axes: axes.iter().map(|axis| (axis.tag(), NULL_HASH)).collect(),
    };
    let page = pack_nested_bytes(vec![header.encode(), Vec::new()]).expect("a header page");
    let page = pack_nested_bytes(vec![page]).expect("the payload of the primary");
    let empty = pack_nested_bytes(vec![vec![]]).expect("an empty chunk");

    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    let before = Before::of(&target, shape, v);
    let atomic = RestoreCommitMode::Atomic;
    let mut session = begin(&target, source.root, atomic, v).expect("a session starts");
    let mut requests = Requests::new(&source.db, source.root, v);
    let mut emptied = BTreeSet::new();
    let refused = loop {
        let (request, payload) = requests.fetch().expect("the tree is asked for");
        let altered = changed(&source.root, &request, &payload, &mut |prefix, part| {
            if *prefix != primary && !secondaries.contains(prefix) {
                return part;
            }
            emptied.insert(*prefix);
            match *prefix == primary {
                true => page.clone(),
                false => empty.clone(),
            }
        });
        if let Err(error) = requests.apply(&mut session, &request, &altered) {
            break error;
        }
    };
    // The header was served, and an empty chunk for the secondary of every
    // axis: the session came to the last of them.
    assert_eq!(emptied.len(), 1 + secondaries.len(), "{refused}");
    assert!(!session.is_sync_completed());
    let committed = target.commit_session(session, v);
    assert!(committed.is_err(), "a session that was refused commits");
    before.assert_left(&target, shape, v);
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);
    refused.to_string()
}

#[test]
fn an_empty_chunk_for_a_subtree_that_holds_data_is_refused_on_lignindb() {
    let v = GroveVersion::latest();
    let refusals = an_empty_chunk_is_refused::<LigninEngine, LigninEngine>(v);
    assert!(refusals[0].contains("empty payload for a subtree"));
    assert!(refusals[2].contains("non-merk page is empty"));
    let refused = an_indexed_tree_served_as_empty_is_refused::<LigninEngine, LigninEngine>(v);
    assert!(refused.contains("failed joint verification"), "{refused}");
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn an_empty_chunk_for_a_subtree_that_holds_data_is_refused_on_both_engines() {
    type Rocks = crate::RocksDbEngine;
    let v = GroveVersion::latest();
    let on_rocksdb = an_empty_chunk_is_refused::<Rocks, Rocks>(v);
    assert_eq!(
        on_rocksdb,
        an_empty_chunk_is_refused::<Rocks, LigninEngine>(v)
    );
    assert_eq!(
        on_rocksdb,
        an_empty_chunk_is_refused::<LigninEngine, Rocks>(v)
    );
    for refusal in &on_rocksdb {
        println!("{refusal}");
    }

    let on_rocksdb = an_indexed_tree_served_as_empty_is_refused::<Rocks, Rocks>(v);
    assert!(
        on_rocksdb.contains("failed joint verification"),
        "{on_rocksdb}"
    );
    let on_lignindb = an_indexed_tree_served_as_empty_is_refused::<Rocks, LigninEngine>(v);
    assert_eq!(on_rocksdb, on_lignindb);
    let from_lignindb = an_indexed_tree_served_as_empty_is_refused::<LigninEngine, Rocks>(v);
    assert_eq!(on_rocksdb, from_lignindb);
    println!("{on_rocksdb}");
}

/// A grove for the engine's own call: a tree, an indexed tree with two
/// axes and an item under the root, and nothing written into the trees.
fn hosts(db: &GroveDb<LigninEngine>, v: &GroveVersion) {
    must_put(db, &[], b"tree", Element::empty_sum_tree(), v);
    must_put(db, &[], b"indexed", SumAndAverage::empty(), v);
    must_put(db, &[], b"item", Element::new_item(vec![3; 5]), v);
}

fn prefix_of(at: &[&[u8]]) -> [u8; 32] {
    build_prefix(at.into()).value
}

/// What the directory holds of a subtree as `transaction` has it.
fn entered(
    transaction: &super::super::LigninTransaction<'_>,
    prefix: [u8; 32],
) -> Option<grovedb_merk::store::lignin::LigninEntry> {
    use grovedb_merk::store::lignin::{LigninNodeStore, LigninSubtree};
    let handle = LigninNodeStore::read_only(transaction.view(), LigninSubtree::alone(prefix));
    handle.entry().expect("a read of the directory")
}

/// What a read of the grove shows through `transaction`: the root hash and
/// every node record of the root Merk.
fn shown<'db>(
    db: &'db GroveDb<LigninEngine>,
    transaction: &super::super::LigninTransaction<'db>,
    v: &GroveVersion,
) -> ([u8; 32], Vec<String>) {
    let root = db.root_hash(Some(transaction), v).unwrap().expect("a root");
    let handle = db.engine().subtree(super::ROOT, None, transaction).unwrap();
    let mut records = Vec::new();
    walked(NodeStore::cursor(&handle), "root", &mut records);
    (root, records)
}

/// The engine's call for a restore, on LigninDB: it enters the subtrees
/// the element at the path hosts, each under the element at its position
/// with its tree type, and those of an indexed tree together. Called
/// again it enters nothing more, and no call changes the root hash or a
/// node record. An element that is no tree, a key with no element, and a
/// subtree that is entered at another place than its element names are
/// refused. All of it goes with the transaction when it is rolled back.
#[test]
fn the_subtrees_of_an_element_are_entered_for_a_restore() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<LigninEngine>(dir.path());
    hosts(&db, v);
    let engine = db.engine();
    let tree = prefix_of(&[b"tree"]);
    let primary = prefix_of(&[b"indexed"]);
    let secondaries: Vec<[u8; 32]> = (SumAndAverage::axes().into_iter())
        .map(|axis| secondary_prefix_for(&primary, axis_storage_tag(axis)).value)
        .collect();
    assert_eq!(secondaries.len(), 2);

    let transaction = db.start_transaction();
    let before = shown(&db, &transaction, v);
    for prefix in [tree, primary].iter().chain(&secondaries) {
        assert_eq!(entered(&transaction, *prefix), None);
    }

    let at_tree: &[&[u8]] = &[b"tree"];
    let at_indexed: &[&[u8]] = &[b"indexed"];
    let adopted = engine.adopt_restored_subtrees(super::ROOT, &transaction);
    adopted.expect("the root has no element, and nothing is done for it");
    for _ in 0..2 {
        let adopted = engine.adopt_restored_subtrees(at_tree.into(), &transaction);
        adopted.expect("the subtree of a tree");
        let adopted = engine.adopt_restored_subtrees(at_indexed.into(), &transaction);
        adopted.expect("the subtrees of an indexed tree");

        let of_tree = entered(&transaction, tree).expect("the tree is entered");
        assert_eq!(of_tree.tree_type().expect("a tree type"), TreeType::SumTree);
        assert_eq!(of_tree.root_key, None);
        let host = of_tree.host.expect("under its element");
        assert_eq!(
            (host.subtree, &host.key[..], host.position),
            ([0; 32], &b"tree"[..], 0)
        );

        let types = [SumAndAverage::empty().tree_type().expect("a tree")]
            .into_iter()
            .chain(
                SumAndAverage::axes()
                    .into_iter()
                    .map(axis_secondary_tree_type),
            );
        let all = [primary].into_iter().chain(secondaries.iter().copied());
        for (position, (prefix, tree_type)) in all.zip(types).enumerate() {
            let of_it = entered(&transaction, prefix).expect("a Merk of the indexed tree");
            assert_eq!(of_it.tree_type().expect("a tree type"), tree_type);
            let host = of_it.host.expect("under its element");
            assert_eq!((host.subtree, &host.key[..]), ([0; 32], &b"indexed"[..]));
            assert_eq!(usize::from(host.position), position);
            assert_eq!(of_it.root_key, None);
        }
        assert_eq!(shown(&db, &transaction, v), before);
    }

    let at_item: &[&[u8]] = &[b"item"];
    let refused = engine.adopt_restored_subtrees(at_item.into(), &transaction);
    assert!(
        matches!(refused, Err(StorageError::NotSupported(_))),
        "{refused:?}"
    );
    let at_nothing: &[&[u8]] = &[b"no element"];
    let refused = engine.adopt_restored_subtrees(at_nothing.into(), &transaction);
    assert!(
        matches!(refused, Err(StorageError::NotSupported(_))),
        "{refused:?}"
    );
    assert_eq!(entered(&transaction, prefix_of(at_item)), None);
    assert_eq!(entered(&transaction, prefix_of(at_nothing)), None);
    assert_eq!(shown(&db, &transaction, v), before);

    db.rollback_transaction(&transaction).expect("a rollback");
    for prefix in [tree, primary].iter().chain(&secondaries) {
        assert_eq!(entered(&transaction, *prefix), None);
    }

    // A subtree that is entered at another place than its element names:
    // here the subtree of the tree, under the indexed tree as the second
    // subtree it hosts. The call for the tree is refused, and enters
    // nothing.
    {
        use grovedb_merk::store::lignin::{LigninHost, LigninNodeStore, LigninSubtree};
        let elsewhere = LigninSubtree {
            prefix: tree,
            host: Some(LigninHost {
                subtree: [0; 32],
                key: b"indexed".to_vec(),
                hosted: vec![primary, tree],
            }),
        };
        let above = LigninSubtree {
            prefix: primary,
            host: elsewhere.host.clone(),
        };
        for subtree in [above, elsewhere] {
            let handle = LigninNodeStore::immediate(transaction.view(), subtree);
            handle.enter(TreeType::SumTree).expect("an enter");
        }
        let refused = engine.adopt_restored_subtrees(at_tree.into(), &transaction);
        assert!(
            matches!(refused, Err(StorageError::NotSupported(_))),
            "{refused:?}"
        );
        // The indexed tree finds the place of its first secondary taken:
        // the subtree directory refuses the enter, and none is entered.
        let refused = engine.adopt_restored_subtrees(at_indexed.into(), &transaction);
        assert!(
            matches!(refused, Err(StorageError::StorageError(_))),
            "{refused:?}"
        );
        for prefix in &secondaries {
            assert_eq!(entered(&transaction, *prefix), None);
        }
        db.rollback_transaction(&transaction).expect("a rollback");
    }
    drop(transaction);
    assert_eq!(entry(&db, tree), None);
    drop(db);
    LigninEngine::check(dir.path(), v);
}

/// A read that the transaction refuses is the error of the call, and is
/// not taken for a subtree that is not entered. While a write scope of the
/// transaction has written the Merk the element is in, the read of the
/// element is refused; while one holds a subtree of the element, the read
/// of that subtree's entry is. Each time the call fails with the scope's
/// refusal and enters nothing, also not the subtrees whose entries it had
/// read as absent by then.
#[test]
fn a_refused_read_fails_the_call_and_enters_nothing() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<LigninEngine>(dir.path());
    hosts(&db, v);
    let engine = db.engine();
    let transaction = db.start_transaction();
    let at_tree: &[&[u8]] = &[b"tree"];
    {
        let scope = engine.open_write_scope(&transaction).expect("a scope");
        let through = LigninEngine::scope_transaction(&scope);
        let written = db.insert(
            super::ROOT,
            b"beside",
            Element::new_item(vec![1]),
            None,
            Some(through),
            v,
        );
        written
            .unwrap()
            .expect("a write of the root Merk through the scope");

        let refused = engine.adopt_restored_subtrees(at_tree.into(), &transaction);
        let overlap = StorageError::WriteScope(WriteScopeError::Overlap);
        assert_eq!(
            refused.expect_err("the read is refused").to_string(),
            overlap.to_string()
        );
        engine
            .rollback_write_scope(scope)
            .expect("the scope is rolled back");
    }
    assert_eq!(entered(&transaction, prefix_of(at_tree)), None);
    let adopted = engine.adopt_restored_subtrees(at_tree.into(), &transaction);
    adopted.expect("the read is answered once the scope is closed");
    let of_tree = entered(&transaction, prefix_of(at_tree)).expect("the tree is entered");

    // The read of an entry is refused, and the read of the element is not.
    // A scope has entered the second secondary of the indexed tree, alone:
    // it holds that subtree, and not the Merk the element is in. The call
    // reads no entry for the primary and for the first secondary, is
    // refused the entry of the second, and fails with the scope's refusal
    // before it enters anything: a refused read that was taken for a
    // subtree that is not entered would leave the first two entered.
    let at_indexed: &[&[u8]] = &[b"indexed"];
    let primary = prefix_of(at_indexed);
    let secondaries: Vec<[u8; 32]> = (SumAndAverage::axes().into_iter())
        .map(|axis| secondary_prefix_for(&primary, axis_storage_tag(axis)).value)
        .collect();
    {
        use grovedb_merk::store::lignin::{LigninHost, LigninNodeStore, LigninSubtree};
        let scope = engine.open_write_scope(&transaction).expect("a scope");
        let through = LigninEngine::scope_transaction(&scope);
        let second = LigninSubtree {
            prefix: secondaries[1],
            host: Some(LigninHost {
                subtree: [0; 32],
                key: b"indexed".to_vec(),
                hosted: vec![primary, secondaries[0], secondaries[1]],
            }),
        };
        let handle = LigninNodeStore::immediate(through.view(), second);
        (handle.enter(TreeType::SumTree)).expect("an enter through the scope");

        let refused = engine.adopt_restored_subtrees(at_indexed.into(), &transaction);
        let overlap = StorageError::WriteScope(WriteScopeError::Overlap);
        assert_eq!(
            refused.expect_err("the read is refused").to_string(),
            overlap.to_string()
        );
        engine
            .rollback_write_scope(scope)
            .expect("the scope is rolled back");
    }
    for prefix in [primary].iter().chain(&secondaries) {
        assert_eq!(entered(&transaction, *prefix), None);
    }
    assert_eq!(entered(&transaction, prefix_of(at_tree)), Some(of_tree));
}

/// Restores the Merk at `at` of `target`, in the transaction, from the
/// chunks of the Merk at `at` of `source`, through an immediate handle:
/// the root Merk for the empty path, and otherwise the Merk of a tree
/// under the root.
fn handed_over(
    source: &GroveDb<LigninEngine>,
    target: &GroveDb<LigninEngine>,
    transaction: &super::super::LigninTransaction<'_>,
    at: &[&[u8]],
    v: &GroveVersion,
) {
    let cost = Some(&Element::value_defined_cost_for_serialized_value);
    let reading = source.engine().start_read_transaction();
    let from = source.engine().subtree(at.into(), None, &reading).unwrap();
    let into = target.engine().immediate_subtree(at.into(), transaction);
    let into = into.unwrap();
    let (from, into) = match at.first() {
        None => (
            Merk::open_base(from, TreeType::NormalTree, cost, v),
            Merk::open_base(into, TreeType::NormalTree, cost, v),
        ),
        Some(key) => {
            let element = source.get(super::ROOT, key, None, v);
            let element = element.unwrap().expect("the tree");
            let (root_key, tree_type) = element.root_key_and_tree_type_owned().expect("a tree");
            (
                Merk::open_layered_with_root_key(from, root_key.clone(), tree_type, cost, v),
                Merk::open_layered_with_root_key(into, root_key, tree_type, cost, v),
            )
        }
    };
    let from = from.unwrap().expect("the Merk of the source");
    let into = into.unwrap().expect("the Merk of the target");
    let mut producer = ChunkProducer::new(&from).expect("a producer");
    let mut restorer = Restorer::new(into, from.root_hash().unwrap(), None);
    let mut pending = vec![vec![]];
    while let Some(chunk_id) = pending.pop() {
        let (chunk, _) = producer.chunk(&chunk_id, v).expect("a chunk");
        let more = restorer.process_chunk(&chunk_id, chunk, v);
        pending.extend(more.expect("the chunk is taken"));
    }
    let restored = restorer.finalize_with_grovedb_elements(v);
    restored.expect("the restore finishes");
}

/// A Merk that a restore hands over under an element is named in the
/// element's node by the call after the hand-over: the committed database
/// passes the physical checker, and the root hash and the node records of
/// the Merk above are what they were before the call, so no value and no
/// hash changed. Without the call the same commit leaves a host that does
/// not point at its Merk, which the checker reports.
#[test]
fn a_merk_that_was_handed_over_is_named_in_its_host() {
    use grovedb_merk::store::lignin::hash::MerkHashRules;
    use lignindb::{Db, Options};
    let v = GroveVersion::latest();
    let source_dir = tempfile::tempdir().expect("a directory");
    let source = open::<LigninEngine>(source_dir.path());
    must_put(&source, &[], b"tree", Element::empty_sum_tree(), v);
    for n in 0..40 {
        let sum = Element::new_sum_item(n as i64);
        must_put(&source, &path(&[b"tree"]), &row(n), sum, v);
    }
    let source_root = source.root_hash(None, v).unwrap().expect("a root");
    let at: &[&[u8]] = &[b"tree"];

    for named in [true, false] {
        let dir = tempfile::tempdir().expect("a directory");
        let target = open::<LigninEngine>(dir.path());
        let engine = target.engine();
        let transaction = target.start_transaction();
        handed_over(&source, &target, &transaction, &[], v);
        let entered = engine.adopt_restored_subtrees(at.into(), &transaction);
        entered.expect("the subtree is entered");
        let before = shown(&target, &transaction, v);
        handed_over(&source, &target, &transaction, at, v);
        if named {
            for _ in 0..2 {
                let adopted = engine.adopt_restored_subtrees(at.into(), &transaction);
                adopted.expect("the Merk is named in its host");
            }
        }
        assert_eq!(shown(&target, &transaction, v), before);
        let committed = target.commit_transaction(transaction);
        committed.unwrap().expect("a commit");
        let root = target.root_hash(None, v).unwrap().expect("a root");
        assert_eq!(root, source_root);
        if !named {
            // The close of a debug build would find what is read below.
            engine.database().unchecked_at_close();
        }
        drop(target);

        let db = Db::open(dir.path(), &Options::default()).expect("the database opens again");
        let mut rules = MerkHashRules::for_check(v);
        let report = db.close_and_verify(Some(&mut rules)).expect("a close");
        assert_eq!(report.is_sound(), named, "{:#?}", report.findings);
        if !named {
            let told = format!("{:?}", report.findings);
            assert!(told.contains("HostPointer"), "{told}");
        }
    }
}

/// The store puts the node of a host again through an immediate handle,
/// and for a key that holds a node: a handle that does not write at once
/// and a key with no node are refused, and neither changes what a read
/// shows.
#[test]
fn the_store_refuses_to_name_the_merks_of_a_host_it_cannot_put() {
    use grovedb_merk::store::lignin::{LigninNodeStore, LigninSubtree};
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<LigninEngine>(dir.path());
    hosts(&db, v);
    let transaction = db.start_transaction();
    let before = shown(&db, &transaction, v);
    let root = || LigninSubtree::alone(prefix_of(&[]));
    let hosted = [prefix_of(&[b"tree"])];

    let reading = LigninNodeStore::read_only(transaction.view(), root());
    let refused = reading.name_hosted(b"tree", &hosted);
    let refused = format!("{:?}", refused.expect_err("a handle that reads"));
    assert!(refused.contains("does not write at once"), "{refused}");
    drop(reading);

    let writing = LigninNodeStore::immediate(transaction.view(), root());
    let refused = writing.name_hosted(b"no element", &hosted);
    let refused = format!("{:?}", refused.expect_err("a key with no node"));
    assert!(refused.contains("no node of its Merk"), "{refused}");
    drop(writing);
    assert_eq!(shown(&db, &transaction, v), before);
}

/// On an engine for which a subtree is its prefix the call does nothing:
/// it answers for any path, and the records of the grove are what they
/// were.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn on_rocksdb_the_call_for_a_restore_does_nothing() {
    let v = GroveVersion::latest();
    let made = Made::<crate::RocksDbEngine>::new(v);
    let before = records(&made.db, &made.shape);
    let transaction = made.db.start_transaction();
    let paths: [&[&[u8]]; 3] = [&[], &[KINDS_AT], &[b"no element"]];
    for at in paths {
        let adopted = made
            .db
            .engine()
            .adopt_restored_subtrees(at.into(), &transaction);
        adopted.expect("nothing is done");
    }
    made.db
        .commit_transaction(transaction)
        .unwrap()
        .expect("a commit");
    assert_eq!(records(&made.db, &made.shape), before);
    assert_eq!(
        made.db.root_hash(None, v).unwrap().expect("a root"),
        made.root
    );
    let _ = &made.dir;
}

/// What the process holds in memory, in bytes: now, and at most since
/// [`forget_the_peak`].
#[cfg(target_os = "linux")]
fn resident() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").expect("the status of the process");
    let field = |name: &str| {
        let line = status.lines().find(|line| line.starts_with(name));
        let kib = line.and_then(|line| line.split_whitespace().nth(1));
        kib.and_then(|kib| kib.parse::<u64>().ok()).expect("a size") * 1024
    };
    (field("VmRSS:"), field("VmHWM:"))
}

#[cfg(target_os = "linux")]
fn forget_the_peak() {
    std::fs::write("/proc/self/clear_refs", "5").expect("the peak is reset");
}

/// The memory of a session that restores one large Merk, as the resident
/// memory of a process that does nothing else: a tree of
/// `GROVE07_NODES` items of `GROVE07_VALUE` bytes under keys of 8 bytes is
/// made on RocksDB, closed and opened again, and a database on the engine
/// `E` is synced from it. It is a measurement, run by hand, one process
/// for each engine and size.
#[cfg(all(target_os = "linux", feature = "rocksdb_storage"))]
fn memory_of_a_sync<E: Side>() {
    use crate::batch::QualifiedGroveDbOp;
    let number = |name: &str, default: usize| {
        let set = std::env::var(name).ok().and_then(|set| set.parse().ok());
        set.unwrap_or(default)
    };
    let nodes = number("GROVE07_NODES", 20_000);
    assert!(nodes > 0, "a Merk of no node has no memory for a node");
    let value = number("GROVE07_VALUE", 32);
    let v = GroveVersion::latest();

    let source_dir = tempfile::tempdir().expect("a directory");
    {
        let source = open::<crate::RocksDbEngine>(source_dir.path());
        must_put(&source, &[], b"large", Element::empty_tree(), v);
        for first in (0..nodes).step_by(5_000) {
            let ops = (first..nodes.min(first + 5_000)).map(|n| {
                let key = (n as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes();
                let item = Element::new_item(vec![n as u8; value]);
                QualifiedGroveDbOp::insert_or_replace_op(path(&[b"large"]), key.to_vec(), item)
            });
            let applied = source.apply_batch(ops.collect(), None, None, v);
            applied.unwrap().expect("a batch of items");
        }
        source.flush().expect("a flush");
    }
    let source = open::<crate::RocksDbEngine>(source_dir.path());
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<E>(dir.path());

    forget_the_peak();
    let (before, _) = resident();
    let began = std::time::Instant::now();
    let atomic = RestoreCommitMode::Atomic;
    let mut session = begin(&target, root, atomic, v).expect("a session starts");
    let mut asked = VecDeque::from([root.to_vec()]);
    let (mut requests, mut bytes) = (0usize, 0usize);
    while let Some(request) = asked.pop_front() {
        let payload = source
            .fetch_chunk(&request, None, VERSION, v)
            .expect("a chunk");
        requests += 1;
        bytes += payload.len();
        let more = session.apply_chunk(&request, &payload, VERSION, v);
        asked.extend(more.expect("the chunk is taken"));
    }
    assert!(session.is_sync_completed());
    let (applied, applied_peak) = resident();
    target
        .commit_session(session, v)
        .expect("the session commits");
    let (committed, peak) = resident();
    let took = began.elapsed();
    assert_eq!(target.root_hash(None, v).unwrap().expect("a root"), root);

    let each = |held: u64| held.saturating_sub(before) / nodes as u64;
    println!(
        "MEMORY engine={} nodes={nodes} value={value} requests={requests} payload={bytes} \
         seconds={:.1} before={before} | bytes for a node: every chunk applied {} (most until \
         then {}), committed {}, most of the session {}",
        E::NAME,
        took.as_secs_f64(),
        each(applied),
        each(applied_peak),
        each(committed),
        each(peak),
    );
}

#[cfg(all(target_os = "linux", feature = "rocksdb_storage"))]
#[test]
#[ignore = "a measurement: run by hand, one process at a time"]
fn the_memory_of_a_sync_of_a_large_merk_into_lignindb() {
    memory_of_a_sync::<LigninEngine>();
}

#[cfg(all(target_os = "linux", feature = "rocksdb_storage"))]
#[test]
#[ignore = "a measurement: run by hand, one process at a time"]
fn the_memory_of_a_sync_of_a_large_merk_into_rocksdb() {
    memory_of_a_sync::<crate::RocksDbEngine>();
}
