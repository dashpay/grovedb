//! The operations a workload is made of.
//!
//! A workload is a flat stream of [`Op`]s. The types hold plain data (paths,
//! keys, elements), so a consumer can apply a stream to a GroveDB (see
//! [`super::apply`]), record it, replay it, or read it without a database at
//! all, as a tree-layout simulation does.

use grovedb::{
    element::reference_path::ReferencePathType, AggregateSumPathQuery, Element, ElementFlags,
    PathQuery, TreeType,
};
use grovedb_version::version::v1::GROVE_V1;

use super::query_encoding::{put_path_query, put_sum_path_query};

/// A path from the GroveDB root to a subtree.
pub type Path = Vec<Vec<u8>>;

/// One step of a workload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// One call of a direct write method.
    Write(Write),
    /// One `apply_batch` call: the unit a state transition is.
    Batch(Vec<BatchOp>),
    /// One read. Reads only follow a [`Op::Commit`], a
    /// [`Op::FlushPendingDrops`] or another read, so the same read can be
    /// proved against committed state.
    Read(Read),
    /// The end of a block: the open transaction is committed.
    Commit,
    /// One `apply_partial_batch` call: a batch applied in two segments, the
    /// second chosen by the caller once it has seen what the first cost.
    PartialBatch(PartialBatch),
    /// `GroveDb::flush_pending_prefix_drops`: the storage of every subtree
    /// dropped flat by a committed block is reclaimed. It works on the
    /// database itself, outside any transaction, so it only follows a
    /// [`Op::Commit`], as Platform calls it after each block.
    FlushPendingDrops,
}

/// A batch applied in two segments.
///
/// GroveDB applies `ops` up to the pause height, hands the cost so far to
/// its caller and applies what the caller returns with the rest. A workload
/// fixes that answer in advance as `add_on`, so the stream stays plain data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialBatch {
    /// The operations of the first segment.
    pub ops: Vec<BatchOp>,
    /// The operations the caller adds once the first segment has paused.
    pub add_on: Vec<BatchOp>,
    /// The height the first segment pauses at, counted from the root tree.
    /// `None` is GroveDB's default: the root tree, height 1.
    pub pause_height: Option<u8>,
}

/// A direct (non-batch) write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    /// `GroveDb::insert` with the default options.
    Insert {
        /// The subtree to insert into.
        path: Path,
        /// The key to insert at.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
    },
    /// `GroveDb::delete`, allowed to remove a tree that is not empty.
    Delete {
        /// The subtree to delete from.
        path: Path,
        /// The key to delete.
        key: Vec<u8>,
    },
    /// `GroveDb::clear_subtree`, allowed to remove child subtrees.
    ClearSubtree {
        /// The subtree to empty.
        path: Path,
    },
    /// An append to an append-only tree through its typed method.
    Append {
        /// The path of the append-only tree itself, its own key last.
        path: Path,
        /// What to append.
        entry: AppendEntry,
    },
    /// `GroveDb::put_aux`: a record in the auxiliary keyspace, which no tree
    /// commits to.
    PutAux {
        /// The key of the record.
        key: Vec<u8>,
        /// The bytes to store.
        value: Vec<u8>,
    },
    /// `GroveDb::delete_aux`.
    DeleteAux {
        /// The key of the record.
        key: Vec<u8>,
    },
    /// `GroveDb::drop_flat_subtree`: a tree holding no child trees is
    /// removed without being opened, and a record naming its storage is
    /// written to the meta keyspace for [`Op::FlushPendingDrops`].
    DropFlat {
        /// The subtree holding the tree.
        path: Path,
        /// The key of the tree.
        key: Vec<u8>,
    },
}

/// One operation of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchOp {
    /// Insert, replacing whatever is stored at the key.
    InsertOrReplace {
        /// The subtree to insert into.
        path: Path,
        /// The key to insert at.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
    },
    /// Insert at a key the workload knows to be free; no existence check.
    InsertOnly {
        /// The subtree to insert into.
        path: Path,
        /// The key to insert at.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
    },
    /// Insert unless the key is taken, in which case the op is skipped.
    InsertIfNotExists {
        /// The subtree to insert into.
        path: Path,
        /// The key to insert at.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
    },
    /// Replace the element stored at a key.
    Replace {
        /// The subtree holding the key.
        path: Path,
        /// The key to replace.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
    },
    /// Rewrite a stored reference so its value hash follows its target.
    RefreshReference {
        /// The subtree holding the reference.
        path: Path,
        /// The key of the reference.
        key: Vec<u8>,
        /// The reference path, as stored.
        reference: ReferencePathType,
        /// The hop limit, as stored.
        max_hop: Option<u8>,
        /// The flags, as stored.
        flags: Option<ElementFlags>,
        /// Whether the reference is stored inside a `NonCounted` wrapper.
        non_counted: bool,
        /// Whether GroveDB writes the fields above without reading the
        /// stored reference first.
        trusted: bool,
    },
    /// Delete an element that is not a tree.
    Delete {
        /// The subtree to delete from.
        path: Path,
        /// The key to delete.
        key: Vec<u8>,
    },
    /// Delete a tree and everything under it.
    DeleteTree {
        /// The subtree to delete from.
        path: Path,
        /// The key of the tree.
        key: Vec<u8>,
        /// The type of the tree being deleted.
        tree_type: TreeType,
    },
    /// An append to an append-only tree.
    Append {
        /// The path of the append-only tree itself, its own key last.
        path: Path,
        /// What to append.
        entry: AppendEntry,
    },
    /// Replace the element stored at a key, declaring by how many bytes it
    /// grows or shrinks.
    Patch {
        /// The subtree holding the key.
        path: Path,
        /// The key to patch.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
        /// The size of the new element less the size of the old one.
        change_in_bytes: i32,
    },
    /// Move an element to another key of the same subtree. A tree keeps
    /// everything stored under it.
    Move {
        /// The subtree holding the key.
        path: Path,
        /// The key the element is at.
        key: Vec<u8>,
        /// The key it moves to.
        new_key: Vec<u8>,
    },
    /// Delete a tree holding no child trees without opening it: the batch
    /// form of [`Write::DropFlat`].
    DropFlat {
        /// The subtree to delete from.
        path: Path,
        /// The key of the tree.
        key: Vec<u8>,
        /// The type of the tree being dropped.
        tree_type: TreeType,
    },
}

/// A value appended to an append-only tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppendEntry {
    /// A leaf of an `MmrTree`.
    Mmr(Vec<u8>),
    /// A value of a `BulkAppendTree`.
    Bulk(Vec<u8>),
    /// A value of a `DenseAppendOnlyFixedSizeTree`.
    Dense(Vec<u8>),
    /// An entry of a `PrivateDocumentStore`, of the store's entry size.
    PrivateDocument(Vec<u8>),
    /// A note of a `CommitmentTree`. Batch only: GroveDB's direct method
    /// takes a typed ciphertext, not bytes.
    Commitment {
        /// The note commitment, a canonical Pallas base field element.
        cmx: [u8; 32],
        /// The nullifier of the spent note.
        rho: [u8; 32],
        /// The value commitment.
        cv_net: [u8; 32],
        /// The serialized note ciphertext.
        payload: Vec<u8>,
    },
}

/// A read.
#[derive(Clone, Debug, PartialEq)]
pub enum Read {
    /// `GroveDb::get`: one element, references followed.
    Get {
        /// The subtree holding the key.
        path: Path,
        /// The key to read.
        key: Vec<u8>,
    },
    /// A path query, to run with `GroveDb::query` and to prove with
    /// `GroveDb::prove_query`.
    Query(Query),
    /// A path query in GroveDB's whole grammar: conditional subqueries,
    /// offsets, limits per subquery, aggregates over a key range and the
    /// read modes of indexed trees. It runs through
    /// `GroveDb::run_path_query`, which returns elements as stored.
    PathQuery {
        /// The query.
        query: Box<PathQuery>,
        /// Whether GroveDB proves this query on the grove version the
        /// stream was generated for: `GroveDb::prove_query`, verified by
        /// `GroveDb::verify_path_query`. A query with an offset is proven
        /// only as a page of a tree that commits its count.
        proven: bool,
    },
    /// A path query in GroveDB's own form, run by the readers that return
    /// rows: `GroveDb::query`, which follows references, or
    /// `GroveDb::query_raw`. They take an offset with any query, where
    /// `GroveDb::run_path_query` refuses one next to a subquery. Never
    /// proven.
    Rows {
        /// The query.
        query: Box<PathQuery>,
        /// Whether elements are returned as stored (`GroveDb::query_raw`).
        raw: bool,
    },
    /// A walk over sum items that stops at a running total:
    /// `GroveDb::query_aggregate_sums_with_options`. Never proven.
    SumBudget {
        /// Where to walk, what to select and when to stop.
        query: Box<AggregateSumPathQuery>,
        /// Whether an element carrying no sum is skipped. Otherwise it is an
        /// error.
        skip_non_sum_items: bool,
        /// Whether a reference is skipped. Otherwise it is followed.
        ignore_references: bool,
    },
    /// `GroveDb::get_aux`: one record of the auxiliary keyspace.
    GetAux {
        /// The key of the record.
        key: Vec<u8>,
    },
    /// `GroveDb::get_aux_by_key_prefix`: every auxiliary record whose key
    /// starts with a prefix.
    AuxByPrefix {
        /// The prefix.
        prefix: Vec<u8>,
    },
}

// GroveDB's query types compare field by field over bytes and integers, so
// the comparison is total although they do not derive `Eq`.
impl Eq for Read {}

/// A path query, kept as plain data.
///
/// [`Query::to_path_query`] gives the GroveDB form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// The subtree the query starts in.
    pub path: Path,
    /// What to select in that subtree.
    pub items: Vec<QueryItem>,
    /// Applied to every selected tree: follow this path below it.
    pub subquery_path: Option<Path>,
    /// Applied to every selected tree, after `subquery_path`: select
    /// everything there.
    pub subquery_all: bool,
    /// The most results to return.
    pub limit: Option<u16>,
    /// Ascending key order when true.
    pub left_to_right: bool,
    /// Return elements as stored (`GroveDb::query_raw`): references are not
    /// followed and trees may be selected. Otherwise references are followed
    /// and every selected element must be a value (`GroveDb::query`).
    pub raw: bool,
}

/// One selection of a [`Query`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryItem {
    /// One key.
    Key(Vec<u8>),
    /// `start..end`.
    Range(Vec<u8>, Vec<u8>),
    /// `start..=end`.
    RangeInclusive(Vec<u8>, Vec<u8>),
    /// Every key.
    RangeFull,
    /// `start..`.
    RangeFrom(Vec<u8>),
    /// `..end`.
    RangeTo(Vec<u8>),
    /// Every key above `start`.
    RangeAfter(Vec<u8>),
}

impl Op {
    /// Appends the canonical encoding of the op to `out`.
    ///
    /// The encoding is what [`StreamDigest`] hashes. Integers are fixed-width
    /// little-endian, byte strings and lists carry a `u32` length, an
    /// optional value follows a presence byte, and an element is written as
    /// its GroveDB serialization, which is consensus data and the same on
    /// every grove version. A GroveDB query is written field by field in
    /// the same conventions.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Op::Write(write) => {
                out.push(0);
                write.encode(out);
            }
            Op::Batch(ops) => {
                out.push(1);
                put_len(out, ops.len());
                for op in ops {
                    op.encode(out);
                }
            }
            Op::Read(read) => {
                out.push(2);
                read.encode(out);
            }
            Op::Commit => out.push(3),
            Op::PartialBatch(batch) => {
                out.push(4);
                for ops in [&batch.ops, &batch.add_on] {
                    put_len(out, ops.len());
                    for op in ops {
                        op.encode(out);
                    }
                }
                put_option(out, batch.pause_height, |out, height| out.push(height));
            }
            Op::FlushPendingDrops => out.push(5),
        }
    }

    /// Every element the op stores, outermost form.
    pub fn elements(&self) -> Vec<&Element> {
        match self {
            Op::Write(Write::Insert { element, .. }) => vec![element],
            Op::Batch(ops) => ops.iter().filter_map(BatchOp::element).collect(),
            Op::PartialBatch(batch) => batch
                .ops
                .iter()
                .chain(&batch.add_on)
                .filter_map(BatchOp::element)
                .collect(),
            Op::Write(_) | Op::Read(_) | Op::Commit | Op::FlushPendingDrops => Vec::new(),
        }
    }

    /// Whether the op reads or reclaims committed state and leaves the open
    /// transaction as it is: a read or [`Op::FlushPendingDrops`].
    pub fn follows_a_commit(&self) -> bool {
        matches!(self, Op::Read(_) | Op::FlushPendingDrops)
    }
}

impl Write {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Write::Insert { path, key, element } => put_insert(out, 0, path, key, element),
            Write::Delete { path, key } => {
                out.push(1);
                put_path(out, path);
                put_bytes(out, key);
            }
            Write::ClearSubtree { path } => {
                out.push(2);
                put_path(out, path);
            }
            Write::Append { path, entry } => {
                out.push(3);
                put_path(out, path);
                entry.encode(out);
            }
            Write::PutAux { key, value } => {
                out.push(4);
                put_bytes(out, key);
                put_bytes(out, value);
            }
            Write::DeleteAux { key } => {
                out.push(5);
                put_bytes(out, key);
            }
            Write::DropFlat { path, key } => {
                out.push(6);
                put_path(out, path);
                put_bytes(out, key);
            }
        }
    }
}

impl BatchOp {
    /// The element the op stores, if it stores one.
    pub fn element(&self) -> Option<&Element> {
        match self {
            BatchOp::InsertOrReplace { element, .. }
            | BatchOp::InsertOnly { element, .. }
            | BatchOp::InsertIfNotExists { element, .. }
            | BatchOp::Replace { element, .. }
            | BatchOp::Patch { element, .. } => Some(element),
            BatchOp::RefreshReference { .. }
            | BatchOp::Delete { .. }
            | BatchOp::DeleteTree { .. }
            | BatchOp::Append { .. }
            | BatchOp::Move { .. }
            | BatchOp::DropFlat { .. } => None,
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            BatchOp::InsertOrReplace { path, key, element } => {
                put_insert(out, 0, path, key, element)
            }
            BatchOp::InsertOnly { path, key, element } => put_insert(out, 1, path, key, element),
            BatchOp::InsertIfNotExists { path, key, element } => {
                put_insert(out, 2, path, key, element)
            }
            BatchOp::Replace { path, key, element } => put_insert(out, 3, path, key, element),
            BatchOp::RefreshReference {
                path,
                key,
                reference,
                max_hop,
                flags,
                non_counted,
                trusted,
            } => {
                out.push(4);
                put_path(out, path);
                put_bytes(out, key);
                put_element(
                    out,
                    &Element::Reference(reference.clone(), *max_hop, flags.clone()),
                );
                out.push(u8::from(*non_counted));
                out.push(u8::from(*trusted));
            }
            BatchOp::Delete { path, key } => {
                out.push(5);
                put_path(out, path);
                put_bytes(out, key);
            }
            BatchOp::DeleteTree {
                path,
                key,
                tree_type,
            } => {
                out.push(6);
                put_path(out, path);
                put_bytes(out, key);
                put_tree_type(out, *tree_type);
            }
            BatchOp::Append { path, entry } => {
                out.push(7);
                put_path(out, path);
                entry.encode(out);
            }
            BatchOp::Patch {
                path,
                key,
                element,
                change_in_bytes,
            } => {
                put_insert(out, 8, path, key, element);
                out.extend_from_slice(&change_in_bytes.to_le_bytes());
            }
            BatchOp::Move { path, key, new_key } => {
                out.push(9);
                put_path(out, path);
                put_bytes(out, key);
                put_bytes(out, new_key);
            }
            BatchOp::DropFlat {
                path,
                key,
                tree_type,
            } => {
                out.push(10);
                put_path(out, path);
                put_bytes(out, key);
                put_tree_type(out, *tree_type);
            }
        }
    }
}

impl AppendEntry {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            AppendEntry::Mmr(value) => {
                out.push(0);
                put_bytes(out, value);
            }
            AppendEntry::Bulk(value) => {
                out.push(1);
                put_bytes(out, value);
            }
            AppendEntry::Dense(value) => {
                out.push(2);
                put_bytes(out, value);
            }
            AppendEntry::PrivateDocument(entry) => {
                out.push(3);
                put_bytes(out, entry);
            }
            AppendEntry::Commitment {
                cmx,
                rho,
                cv_net,
                payload,
            } => {
                out.push(4);
                out.extend_from_slice(cmx);
                out.extend_from_slice(rho);
                out.extend_from_slice(cv_net);
                put_bytes(out, payload);
            }
        }
    }
}

impl Read {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Read::Get { path, key } => {
                out.push(0);
                put_path(out, path);
                put_bytes(out, key);
            }
            Read::Query(query) => {
                out.push(1);
                put_path(out, &query.path);
                put_len(out, query.items.len());
                for item in &query.items {
                    item.encode(out);
                }
                match &query.subquery_path {
                    Some(path) => {
                        out.push(1);
                        put_path(out, path);
                    }
                    None => out.push(0),
                }
                out.push(u8::from(query.subquery_all));
                match query.limit {
                    Some(limit) => {
                        out.push(1);
                        out.extend_from_slice(&limit.to_le_bytes());
                    }
                    None => out.push(0),
                }
                out.push(u8::from(query.left_to_right));
                out.push(u8::from(query.raw));
            }
            Read::PathQuery { query, proven } => {
                out.push(2);
                put_path_query(out, query);
                out.push(u8::from(*proven));
            }
            Read::SumBudget {
                query,
                skip_non_sum_items,
                ignore_references,
            } => {
                out.push(3);
                put_sum_path_query(out, query);
                out.push(u8::from(*skip_non_sum_items));
                out.push(u8::from(*ignore_references));
            }
            Read::GetAux { key } => {
                out.push(4);
                put_bytes(out, key);
            }
            Read::AuxByPrefix { prefix } => {
                out.push(5);
                put_bytes(out, prefix);
            }
            Read::Rows { query, raw } => {
                out.push(6);
                put_path_query(out, query);
                out.push(u8::from(*raw));
            }
        }
    }
}

impl QueryItem {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            QueryItem::Key(key) => {
                out.push(0);
                put_bytes(out, key);
            }
            QueryItem::Range(start, end) => {
                out.push(1);
                put_bytes(out, start);
                put_bytes(out, end);
            }
            QueryItem::RangeInclusive(start, end) => {
                out.push(2);
                put_bytes(out, start);
                put_bytes(out, end);
            }
            QueryItem::RangeFull => out.push(3),
            QueryItem::RangeFrom(start) => {
                out.push(4);
                put_bytes(out, start);
            }
            QueryItem::RangeTo(end) => {
                out.push(5);
                put_bytes(out, end);
            }
            QueryItem::RangeAfter(start) => {
                out.push(6);
                put_bytes(out, start);
            }
        }
    }
}

pub(crate) fn put_len(out: &mut Vec<u8>, len: usize) {
    let len = u32::try_from(len).expect("a workload length fits in a u32");
    out.extend_from_slice(&len.to_le_bytes());
}

pub(crate) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

/// A presence byte, then the value when there is one.
pub(crate) fn put_option<T>(
    out: &mut Vec<u8>,
    value: Option<T>,
    put: impl FnOnce(&mut Vec<u8>, T),
) {
    match value {
        Some(value) => {
            out.push(1);
            put(out, value);
        }
        None => out.push(0),
    }
}

pub(crate) fn put_path(out: &mut Vec<u8>, path: &[Vec<u8>]) {
    put_len(out, path.len());
    for segment in path {
        put_bytes(out, segment);
    }
}

fn put_element(out: &mut Vec<u8>, element: &Element) {
    let bytes = element
        .serialize(&GROVE_V1)
        .expect("a workload only builds elements GroveDB can serialize");
    put_bytes(out, &bytes);
}

fn put_insert(out: &mut Vec<u8>, tag: u8, path: &[Vec<u8>], key: &[u8], element: &Element) {
    out.push(tag);
    put_path(out, path);
    put_bytes(out, key);
    put_element(out, element);
}

fn put_tree_type(out: &mut Vec<u8>, tree_type: TreeType) {
    out.push(tree_type.discriminant());
    let parameter = match tree_type {
        TreeType::CommitmentTree(parameter)
        | TreeType::BulkAppendTree(parameter)
        | TreeType::DenseAppendOnlyFixedSizeTree(parameter)
        | TreeType::PrivateDocumentStore(parameter) => parameter,
        TreeType::NormalTree
        | TreeType::SumTree
        | TreeType::BigSumTree
        | TreeType::CountTree
        | TreeType::CountSumTree
        | TreeType::ProvableCountTree
        | TreeType::ProvableCountSumTree
        | TreeType::MmrTree
        | TreeType::ProvableSumTree
        | TreeType::ProvableCountProvableSumTree
        | TreeType::ProvableSumIndexedTree
        | TreeType::ProvableCountIndexedTree
        | TreeType::ProvableCountProvableSumIndexedTree
        | TreeType::ProvableCountProvableBigSumTree => 0,
    };
    out.push(parameter);
}

/// A running digest of a workload stream.
///
/// Two streams have the same digest exactly when they are the same sequence
/// of operations, so a pinned digest is the proof that a seed gives the same
/// stream on another machine.
#[derive(Clone, Debug)]
pub struct StreamDigest {
    hasher: blake3::Hasher,
    ops: u64,
    scratch: Vec<u8>,
}

impl Default for StreamDigest {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamDigest {
    /// An empty digest.
    pub fn new() -> Self {
        Self {
            hasher: blake3::Hasher::new(),
            ops: 0,
            scratch: Vec::new(),
        }
    }

    /// The digest of a whole stream.
    pub fn of<'a>(ops: impl IntoIterator<Item = &'a Op>) -> Self {
        let mut digest = Self::new();
        for op in ops {
            digest.update(op);
        }
        digest
    }

    /// Adds one operation.
    pub fn update(&mut self, op: &Op) {
        self.scratch.clear();
        op.encode(&mut self.scratch);
        let len = u32::try_from(self.scratch.len()).expect("an encoded op fits in a u32");
        self.hasher.update(&len.to_le_bytes());
        self.hasher.update(&self.scratch);
        self.ops += 1;
    }

    /// The number of operations added.
    pub fn ops(&self) -> u64 {
        self.ops
    }

    /// The digest of everything added so far.
    pub fn finish(&self) -> [u8; 32] {
        let mut hasher = self.hasher.clone();
        hasher.update(&self.ops.to_le_bytes());
        *hasher.finalize().as_bytes()
    }

    /// The digest as lower-case hex.
    pub fn hex(&self) -> String {
        hex::encode(self.finish())
    }
}
