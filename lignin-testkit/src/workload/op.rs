//! The operations a workload is made of.
//!
//! A workload is a flat stream of [`Op`]s. The types hold plain data (paths,
//! keys, elements), so a consumer can apply a stream to a GroveDB (see
//! [`super::apply()`]), record it, replay it, or read it without a database at
//! all, as a tree-layout simulation does.

use std::collections::BTreeSet;

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
    /// One read. A read only runs while the open transaction holds no
    /// write: nothing but reads and steps that write nothing
    /// ([`Op::keeps_transaction`]) lies between it and the last operation
    /// that ended a transaction ([`Op::ends_transaction`]). The same read
    /// can then be proved against committed state.
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
    /// A step on the open transaction itself, or on a snapshot read
    /// transaction held beside it.
    Transaction(TransactionOp),
    /// One `apply_batch_with_element_flags_update` call with the two
    /// callbacks Drive applies a batch with: how a state transition is
    /// applied on Platform. Where the batch replaces an element, the first
    /// callback merges the storage flags stored with the ones written, so
    /// the flags keep the epoch the element was created in and the bytes
    /// each later epoch added. Where it frees bytes, the second splits them
    /// by the epochs in the flags, and the cost names them by owner and
    /// epoch for the refund.
    FlagsBatch(Vec<BatchOp>),
    /// One `apply_partial_batch_with_element_flags_update` call with the
    /// two callbacks Drive passes there. They differ from those of an
    /// [`Op::FlagsBatch`] in two ways. An update that changes the size of
    /// an element is refused when it names another owner, unless it
    /// shortens an element whose flags hold one epoch, which keeps its
    /// flags. And an update that keeps the size stores the flags as
    /// written.
    FlagsPartialBatch(PartialBatch),
    /// An [`Op::FlagsBatch`] with the first callback Drive passes, from
    /// protocol version 15, with a batch that updates a document: one that
    /// may hand the document to another owner.
    FlagsHandoverBatch(HandoverBatch),
}

/// A batch that may hand elements over to other owners.
///
/// Drive merges the flags as in an [`Op::FlagsBatch`] and then stores the
/// owner the flags written name, whatever the update does to the size: a
/// transfer or a purchase moves the stored bytes to the new owner. Where the
/// stored owner or the written one is among `full_handover_owners`, and the
/// two differ, the update is accounted as a replacement instead: the stored
/// element is freed whole, to its owner and by its epochs, and the element
/// written is stored with its flags as written and charged whole. That is
/// how a gas sponsor and the owner of a document it paid for are kept
/// apart. GroveDB accepts that answer from grove version 5. The owners are
/// a set, as in Drive, and are encoded in ascending order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoverBatch {
    /// The operations of the batch.
    pub ops: Vec<BatchOp>,
    /// The owners a handover to or from which is a full one.
    pub full_handover_owners: BTreeSet<[u8; 32]>,
}

/// A step that is neither a write nor a read of the block's state: the open
/// transaction is rolled back or dropped, a savepoint is set or returned to,
/// or a snapshot read transaction is opened, read through and closed.
///
/// GroveDB has no call that releases a savepoint. One that is not returned
/// to stays on the transaction's stack until the transaction ends, which is
/// how Platform leaves the savepoint of every state transition it keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionOp {
    /// `GroveDb::rollback_transaction`: every write since the last commit is
    /// discarded, with every savepoint, and the transaction stays open.
    Rollback,
    /// The open transaction is dropped without a commit and without a
    /// rollback, and another is started. The transaction belongs to the
    /// caller of [`super::apply()`], which is told to do so.
    Abandon,
    /// `GroveDb::set_savepoint`: a savepoint is pushed on the stack of the
    /// open transaction.
    SetSavepoint,
    /// `GroveDb::rollback_to_savepoint`: every write since the newest
    /// savepoint is discarded and that savepoint is popped.
    RollbackToSavepoint,
    /// `GroveDb::start_snapshot_read_transaction`: the committed state of
    /// this moment is pinned. The snapshot belongs to the caller of
    /// [`super::apply()`], which is told to open it. At most one is open.
    OpenSnapshot,
    /// The snapshot is dropped.
    CloseSnapshot,
    /// A read through the open snapshot: it returns what was committed when
    /// the snapshot was opened, whatever has been committed since. Never
    /// proven: GroveDB proves the latest committed state.
    SnapshotRead(Read),
    /// A read inside the open transaction, between its writes, which it
    /// sees: how Drive checks what is stored before it builds the batch of
    /// a state transition. Never proven: GroveDB proves committed state.
    BlockRead(Read),
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
    /// `GroveDb::insert_if_not_exists`, or, when the element found is
    /// returned, `GroveDb::insert_if_not_exists_return_existing_element`:
    /// the element is stored unless the key is taken.
    InsertIfNotExists {
        /// The subtree to insert into.
        path: Path,
        /// The key to insert at.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
        /// Whether the call returns the element it found in the way.
        return_existing: bool,
    },
    /// `GroveDb::insert_if_changed_value`: the element is stored unless the
    /// same element is stored at the key already.
    InsertIfChanged {
        /// The subtree to insert into.
        path: Path,
        /// The key to insert at.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
    },
    /// `GroveDb::delete_if_empty_tree`: the tree at the key is deleted
    /// unless it holds something.
    DeleteIfEmptyTree {
        /// The subtree to delete from.
        path: Path,
        /// The key of the tree.
        key: Vec<u8>,
    },
    /// The key is deleted, and then every tree above it that the delete
    /// left empty, up to the root tree or to a stop height.
    DeleteUpTree {
        /// The subtree to delete from.
        path: Path,
        /// The key to delete.
        key: Vec<u8>,
        /// The length of the path the climb stops at: nothing stored
        /// directly in a subtree at a path of that length is deleted.
        /// Without one the climb ends at the root tree, whose keys GroveDB
        /// refuses to delete this way.
        stop_path_height: Option<u16>,
        /// How the deletes are applied. Directly is
        /// `GroveDb::delete_up_tree_while_empty`. Built is what Drive does:
        /// `GroveDb::delete_operations_for_delete_up_tree_while_empty`
        /// returns the deletes as batch operations, and one batch applies
        /// them.
        built: bool,
    },
    /// What Drive does to delete one key: the delete is built as a batch
    /// operation by `GroveDb::delete_operation_for_delete_internal`, which
    /// reads the key to learn whether it holds a tree, and one batch
    /// applies it. The key is stored, and a tree it holds is empty: GroveDB
    /// refuses any other.
    DeleteBuilt {
        /// The subtree to delete from.
        path: Path,
        /// The key to delete.
        key: Vec<u8>,
    },
    /// An insert into an indexed tree through the method of its type, such
    /// as `GroveDb::insert_into_count_indexed_tree`.
    IndexedInsert {
        /// The path of the indexed tree itself, its own key last.
        path: Path,
        /// The key to insert at.
        key: Vec<u8>,
        /// The element to store.
        element: Element,
        /// The type of the indexed tree.
        tree: IndexedTree,
    },
    /// A delete from an indexed tree through the method of its type, such
    /// as `GroveDb::delete_from_count_indexed_tree`.
    IndexedDelete {
        /// The path of the indexed tree itself, its own key last.
        path: Path,
        /// The key to delete.
        key: Vec<u8>,
        /// The type of the indexed tree.
        tree: IndexedTree,
    },
    /// `GroveDb::reconcile_indexed_tree_secondaries`: the secondary trees of
    /// an indexed tree are rebuilt from what it holds.
    ReconcileIndexed {
        /// The path of the indexed tree itself, its own key last.
        path: Path,
    },
    /// `GroveDb::commitment_tree_insert`, the typed method: the payload is
    /// handed over as a note ciphertext.
    CommitmentInsert {
        /// The path of the commitment tree itself, its own key last.
        path: Path,
        /// The note commitment, a canonical Pallas base field element.
        cmx: [u8; 32],
        /// The nullifier of the spent note.
        rho: [u8; 32],
        /// The value commitment.
        cv_net: [u8; 32],
        /// The serialized note ciphertext.
        payload: Vec<u8>,
    },
    /// `GroveDb::delete_with_sectional_storage_function`: [`Write::Delete`]
    /// with the callback of an [`Op::FlagsBatch`] that splits the bytes the
    /// delete frees by the epochs in the flags of what it deletes.
    SectionalDelete {
        /// The subtree to delete from.
        path: Path,
        /// The key to delete.
        key: Vec<u8>,
    },
    /// `GroveDb::delete_up_tree_while_empty_with_sectional_storage`: a
    /// direct [`Write::DeleteUpTree`] with the same callback, which splits
    /// the bytes of the key and of every tree deleted above it.
    SectionalDeleteUpTree {
        /// The subtree to delete from.
        path: Path,
        /// The key to delete.
        key: Vec<u8>,
        /// The length of the path the climb stops at, as in
        /// [`Write::DeleteUpTree`].
        stop_path_height: Option<u16>,
    },
}

/// The types of indexed tree, each written through methods of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexedTree {
    /// `ProvableCountIndexedTree`: `…_count_indexed_tree`.
    Count,
    /// `ProvableSumIndexedTree`: `…_provable_sum_indexed_tree`.
    Sum,
    /// `ProvableCountProvableSumIndexedTree`:
    /// `…_provable_count_provable_sum_indexed_tree`.
    CountSum,
    /// `ProvableBigSumIndexedTree`: `…_provable_big_sum_indexed_tree`.
    BigSum,
    /// `ProvableCountProvableBigSumIndexedTree`:
    /// `…_provable_count_provable_big_sum_indexed_tree`.
    CountBigSum,
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
///
/// Two reads are equal exactly when they encode the same. GroveDB's query
/// types do not derive `Eq`, and they compare the conditional branches of a
/// query without their order, which a query's meaning and its encoding both
/// depend on.
#[derive(Clone, Debug)]
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
    /// `GroveDb::has_raw`: whether a key is stored, read from storage
    /// without opening the subtree.
    HasRaw {
        /// The subtree holding the key.
        path: Path,
        /// The key to look for.
        key: Vec<u8>,
    },
    /// `GroveDb::is_empty_tree`.
    IsEmptyTree {
        /// The path of the tree itself, its own key last.
        path: Path,
    },
    /// One element by key through one of the other `get` methods.
    Fetch {
        /// The subtree holding the key.
        path: Path,
        /// The key to read.
        key: Vec<u8>,
        /// Which method reads it.
        fetch: Fetch,
        /// Whether the subtree's nodes may be read through GroveDB's cache.
        /// Without it the element is read from storage directly.
        cache: bool,
    },
    /// A typed read of an append-only tree.
    Tree {
        /// The path of the tree itself, its own key last.
        path: Path,
        /// What to read.
        read: TreeRead,
    },
    /// `GroveDb::query_keys_optional`, or `GroveDb::query_raw_keys_optional`
    /// for elements as stored: a query for keys, answered with a row for
    /// every key asked for, empty where nothing is stored. The query names
    /// keys only and has a limit.
    ///
    /// Every grove version proves it: `GroveDb::prove_query`, verified by
    /// `GroveDb::verify_query_with_absence_proof`, which gives the same
    /// rows, the absent keys included.
    KeysOptional {
        /// The query.
        query: Box<PathQuery>,
        /// Whether elements are returned as stored.
        raw: bool,
    },
    /// `GroveDb::query_item_value`: the values of the items a query
    /// selects, references followed. With sums it is
    /// `GroveDb::query_item_value_or_sum`, which also returns what a sum
    /// item or a tree with an aggregate holds.
    ItemValues {
        /// The query.
        query: Box<PathQuery>,
        /// Whether sums are returned as well.
        or_sums: bool,
    },
    /// `GroveDb::query_sums`: the sums of the sum items a query selects.
    SumValues {
        /// The query.
        query: Box<PathQuery>,
    },
    /// `GroveDb::query_many_raw`: several path queries merged into one and
    /// run, elements returned as stored.
    Many {
        /// The queries.
        queries: Vec<PathQuery>,
    },
    /// The sum of a key range as 128 bits, which a
    /// `ProvableCountProvableBigSumTree` and a `ProvableBigSumTree` answer:
    /// `GroveDb::query_aggregate_big_sum`, with the count, which the first
    /// of the two keeps, `GroveDb::query_aggregate_count_and_big_sum`, and
    /// the forms of both that answer for each selected key.
    ///
    /// A grove version that stores such a tree proves the query:
    /// `GroveDb::prove_query`, verified by the verifier named after the
    /// reader, such as `GroveDb::verify_aggregate_big_sum_query`.
    BigSum {
        /// The query: an aggregate over a key range, or for the forms that
        /// answer per key, keys with that aggregate as their subquery.
        query: Box<PathQuery>,
        /// Whether the count is returned with the sum.
        with_count: bool,
        /// Whether there is one answer for each selected key.
        per_key: bool,
    },
    /// A proof GroveDB builds from committed state through an entry point
    /// other than `GroveDb::prove_query`. The read returns the proof bytes.
    Prove(Prove),
}

/// The `get` method a [`Read::Fetch`] reads through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetch {
    /// `GroveDb::get_caching_optional`: references followed.
    Resolved,
    /// `GroveDb::get_raw`, or without the cache
    /// `GroveDb::get_raw_caching_optional`: the element as stored.
    Raw,
    /// `GroveDb::get_raw_optional`, or without the cache
    /// `GroveDb::get_raw_optional_caching_optional`: the element as stored,
    /// or nothing where the key or its subtree is missing.
    RawOptional,
}

/// A typed read of an append-only tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRead {
    /// `GroveDb::mmr_tree_get_value`: a leaf by index.
    MmrValue(u64),
    /// `GroveDb::mmr_tree_root_hash`.
    MmrRootHash,
    /// `GroveDb::mmr_tree_leaf_count`.
    MmrLeafCount,
    /// `GroveDb::bulk_get_value`: a value by position.
    BulkValue(u64),
    /// `GroveDb::bulk_get_range`: a page of values.
    BulkRange {
        /// The position of the first value.
        start: u64,
        /// The most values to return.
        limit: u16,
    },
    /// `GroveDb::bulk_get_chunk`: a completed chunk by index.
    BulkChunk(u64),
    /// `GroveDb::bulk_get_buffer`: the values not yet in a chunk.
    BulkBuffer,
    /// `GroveDb::bulk_count`.
    BulkCount,
    /// `GroveDb::bulk_chunk_count`.
    BulkChunkCount,
    /// `GroveDb::dense_tree_get`: a value by position.
    DenseValue(u16),
    /// `GroveDb::dense_tree_root_hash`.
    DenseRootHash,
    /// `GroveDb::dense_tree_count`.
    DenseCount,
    /// `GroveDb::commitment_tree_get_value`: a note by position.
    CommitmentValue(u64),
    /// `GroveDb::commitment_tree_get_range`: a page of notes.
    CommitmentRange {
        /// The position of the first note.
        start: u64,
        /// The most notes to return.
        limit: u16,
    },
    /// `GroveDb::commitment_tree_anchor`.
    CommitmentAnchor,
    /// `GroveDb::commitment_tree_count`.
    CommitmentCount,
    /// `GroveDb::private_document_store_get_value`: an entry by position.
    PrivateDocumentValue(u64),
    /// `GroveDb::private_document_store_get_range`: a page of entries.
    PrivateDocumentRange {
        /// The position of the first entry.
        start: u64,
        /// The most entries to return.
        limit: u16,
    },
    /// `GroveDb::private_document_store_count`.
    PrivateDocumentCount,
}

/// A proof built through an entry point of its own.
#[derive(Clone, Debug)]
pub enum Prove {
    /// `GroveDb::get_proved_path_query`, the method Drive proves every
    /// query through: `GroveDb::prove_query` for a caller that has a
    /// transaction to hand it, which it refuses. It is given none.
    PathQuery(Box<PathQuery>),
    /// `GroveDb::prove_query_many`: one proof for several path queries.
    Many(Vec<PathQuery>),
    /// `GroveDb::prove_bulk_position_range`: a page of the values of a
    /// `BulkAppendTree`, a `CommitmentTree` or a `PrivateDocumentStore`.
    BulkPositionRange {
        /// The subtree holding the tree.
        path: Path,
        /// The key of the tree.
        key: Vec<u8>,
        /// The position of the first value.
        start: u64,
        /// The most values to prove.
        limit: u16,
    },
    /// `GroveDb::prove_trunk_chunk`: the top of a tree down to a depth.
    TrunkChunk {
        /// The path of the tree itself, its own key last.
        path: Path,
        /// The most levels a chunk holds.
        max_depth: u8,
        /// The fewest levels a chunk holds, for a tree that commits its
        /// count.
        min_depth: Option<u8>,
    },
    /// `GroveDb::prove_branch_chunk`: the part of a tree below one of its
    /// keys, down to a depth.
    BranchChunk {
        /// The path of the tree itself, its own key last.
        path: Path,
        /// The key the branch hangs from.
        key: Vec<u8>,
        /// The most levels to return.
        depth: u8,
    },
    /// `GroveDb::fetch_chunk`: what a node serves to another that is
    /// syncing its state. A chunk is asked for by the hash of the subtree's
    /// path, its root key and its type, which a syncing node reads from the
    /// tree's element; [`super::apply()`] reads them the same way, so the
    /// stream names the subtree by its path. The root tree is asked for by
    /// the root hash.
    Chunk {
        /// The path of the subtree itself, its own key last. Empty for the
        /// root tree.
        path: Path,
        /// The chunks of the subtree to serve.
        ids: ChunkIds,
    },
}

/// The chunks of one subtree a [`Prove::Chunk`] asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkIds {
    /// Chunks of a Merk tree. Each is the walk from the root of the tree to
    /// the root of the chunk, one byte per step: 1 to the left, 0 to the
    /// right. None at all asks for the chunk at the root.
    Merk(Vec<Vec<u8>>),
    /// A page of the entries of an append-only tree, which has no Merk
    /// nodes to serve.
    Entries {
        /// The position of the first entry of the page.
        start: u64,
        /// What the tree's element says of its size: the MMR size of an
        /// `MmrTree`, the number of entries of any other tree.
        state: u64,
        /// The parameter of the tree: the height of a
        /// `DenseAppendOnlyFixedSizeTree`, nothing for an `MmrTree`, the
        /// chunk power of any other tree.
        parameter: u8,
    },
}

impl PartialEq for Read {
    fn eq(&self, other: &Self) -> bool {
        let (mut this, mut that) = (Vec::new(), Vec::new());
        self.encode(&mut this);
        other.encode(&mut that);
        this == that
    }
}

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
                batch.encode(out);
            }
            Op::FlushPendingDrops => out.push(5),
            Op::Transaction(step) => {
                out.push(6);
                step.encode(out);
            }
            Op::FlagsBatch(ops) => {
                out.push(7);
                put_len(out, ops.len());
                for op in ops {
                    op.encode(out);
                }
            }
            Op::FlagsPartialBatch(batch) => {
                out.push(8);
                batch.encode(out);
            }
            Op::FlagsHandoverBatch(batch) => {
                out.push(9);
                put_len(out, batch.ops.len());
                for op in &batch.ops {
                    op.encode(out);
                }
                put_len(out, batch.full_handover_owners.len());
                for owner in &batch.full_handover_owners {
                    out.extend_from_slice(owner);
                }
            }
        }
    }

    /// Every element the op stores, outermost form.
    pub fn elements(&self) -> Vec<&Element> {
        match self {
            Op::Write(
                Write::Insert { element, .. }
                | Write::InsertIfNotExists { element, .. }
                | Write::InsertIfChanged { element, .. }
                | Write::IndexedInsert { element, .. },
            ) => vec![element],
            Op::Batch(_)
            | Op::PartialBatch(_)
            | Op::FlagsBatch(_)
            | Op::FlagsPartialBatch(_)
            | Op::FlagsHandoverBatch(_) => self.batch_ops().filter_map(BatchOp::element).collect(),
            Op::Write(_)
            | Op::Read(_)
            | Op::Commit
            | Op::FlushPendingDrops
            | Op::Transaction(_) => Vec::new(),
        }
    }

    /// The operations of the batch the op applies, those the caller adds to
    /// a partial batch last. None for an op that is no batch.
    pub fn batch_ops(&self) -> impl Iterator<Item = &BatchOp> {
        let (ops, add_on): (&[BatchOp], &[BatchOp]) = match self {
            Op::Batch(ops) | Op::FlagsBatch(ops) => (ops, &[]),
            Op::FlagsHandoverBatch(batch) => (&batch.ops, &[]),
            Op::PartialBatch(batch) | Op::FlagsPartialBatch(batch) => (&batch.ops, &batch.add_on),
            Op::Write(_)
            | Op::Read(_)
            | Op::Commit
            | Op::FlushPendingDrops
            | Op::Transaction(_) => (&[], &[]),
        };
        ops.iter().chain(add_on)
    }

    /// Whether the op reads or reclaims committed state and leaves the open
    /// transaction as it is: a read or [`Op::FlushPendingDrops`].
    pub fn follows_a_commit(&self) -> bool {
        matches!(self, Op::Read(_) | Op::FlushPendingDrops)
    }

    /// Whether the open transaction holds no write once the op is applied:
    /// it was committed, rolled back whole or dropped. What the transaction
    /// reads is then the committed state.
    pub fn ends_transaction(&self) -> bool {
        matches!(
            self,
            Op::Commit | Op::Transaction(TransactionOp::Rollback | TransactionOp::Abandon)
        )
    }

    /// Whether the op leaves the writes of the open transaction as they
    /// are: a read of any kind, [`Op::FlushPendingDrops`], a savepoint
    /// being set or the snapshot being opened or closed.
    pub fn keeps_transaction(&self) -> bool {
        matches!(
            self,
            Op::Read(_)
                | Op::FlushPendingDrops
                | Op::Transaction(
                    TransactionOp::SetSavepoint
                        | TransactionOp::OpenSnapshot
                        | TransactionOp::CloseSnapshot
                        | TransactionOp::SnapshotRead(_)
                        | TransactionOp::BlockRead(_)
                )
        )
    }
}

impl PartialBatch {
    fn encode(&self, out: &mut Vec<u8>) {
        for ops in [&self.ops, &self.add_on] {
            put_len(out, ops.len());
            for op in ops {
                op.encode(out);
            }
        }
        put_option(out, self.pause_height, |out, height| out.push(height));
    }
}

impl TransactionOp {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            TransactionOp::Rollback => out.push(0),
            TransactionOp::Abandon => out.push(1),
            TransactionOp::SetSavepoint => out.push(2),
            TransactionOp::RollbackToSavepoint => out.push(3),
            TransactionOp::OpenSnapshot => out.push(4),
            TransactionOp::CloseSnapshot => out.push(5),
            TransactionOp::SnapshotRead(read) => {
                out.push(6);
                read.encode(out);
            }
            TransactionOp::BlockRead(read) => {
                out.push(7);
                read.encode(out);
            }
        }
    }
}

impl IndexedTree {
    fn encode(self, out: &mut Vec<u8>) {
        out.push(match self {
            IndexedTree::Count => 0,
            IndexedTree::Sum => 1,
            IndexedTree::CountSum => 2,
            IndexedTree::BigSum => 3,
            IndexedTree::CountBigSum => 4,
        });
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
            Write::InsertIfNotExists {
                path,
                key,
                element,
                return_existing,
            } => {
                put_insert(out, 7, path, key, element);
                out.push(u8::from(*return_existing));
            }
            Write::InsertIfChanged { path, key, element } => put_insert(out, 8, path, key, element),
            Write::DeleteIfEmptyTree { path, key } => {
                out.push(9);
                put_path(out, path);
                put_bytes(out, key);
            }
            Write::DeleteUpTree {
                path,
                key,
                stop_path_height,
                built,
            } => {
                out.push(10);
                put_path(out, path);
                put_bytes(out, key);
                put_stop_height(out, *stop_path_height);
                out.push(u8::from(*built));
            }
            Write::DeleteBuilt { path, key } => {
                out.push(11);
                put_path(out, path);
                put_bytes(out, key);
            }
            Write::IndexedInsert {
                path,
                key,
                element,
                tree,
            } => {
                put_insert(out, 12, path, key, element);
                tree.encode(out);
            }
            Write::IndexedDelete { path, key, tree } => {
                out.push(13);
                put_path(out, path);
                put_bytes(out, key);
                tree.encode(out);
            }
            Write::ReconcileIndexed { path } => {
                out.push(14);
                put_path(out, path);
            }
            Write::CommitmentInsert {
                path,
                cmx,
                rho,
                cv_net,
                payload,
            } => {
                out.push(15);
                put_path(out, path);
                out.extend_from_slice(cmx);
                out.extend_from_slice(rho);
                out.extend_from_slice(cv_net);
                put_bytes(out, payload);
            }
            Write::SectionalDelete { path, key } => {
                out.push(16);
                put_path(out, path);
                put_bytes(out, key);
            }
            Write::SectionalDeleteUpTree {
                path,
                key,
                stop_path_height,
            } => {
                out.push(17);
                put_path(out, path);
                put_bytes(out, key);
                put_stop_height(out, *stop_path_height);
            }
        }
    }
}

fn put_stop_height(out: &mut Vec<u8>, stop_path_height: Option<u16>) {
    put_option(out, stop_path_height, |out, height| {
        out.extend_from_slice(&height.to_le_bytes())
    });
}

impl BatchOp {
    /// The subtree the op works in: the tree an append goes to, and for
    /// every other operation the tree that holds its key.
    pub fn path(&self) -> &Path {
        match self {
            BatchOp::InsertOrReplace { path, .. }
            | BatchOp::InsertOnly { path, .. }
            | BatchOp::InsertIfNotExists { path, .. }
            | BatchOp::Replace { path, .. }
            | BatchOp::Patch { path, .. }
            | BatchOp::RefreshReference { path, .. }
            | BatchOp::Delete { path, .. }
            | BatchOp::DeleteTree { path, .. }
            | BatchOp::Append { path, .. }
            | BatchOp::Move { path, .. }
            | BatchOp::DropFlat { path, .. } => path,
        }
    }

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
            Read::HasRaw { path, key } => {
                out.push(7);
                put_path(out, path);
                put_bytes(out, key);
            }
            Read::IsEmptyTree { path } => {
                out.push(8);
                put_path(out, path);
            }
            Read::Fetch {
                path,
                key,
                fetch,
                cache,
            } => {
                out.push(9);
                put_path(out, path);
                put_bytes(out, key);
                out.push(match fetch {
                    Fetch::Resolved => 0,
                    Fetch::Raw => 1,
                    Fetch::RawOptional => 2,
                });
                out.push(u8::from(*cache));
            }
            Read::Tree { path, read } => {
                out.push(10);
                put_path(out, path);
                read.encode(out);
            }
            Read::KeysOptional { query, raw } => {
                out.push(11);
                put_path_query(out, query);
                out.push(u8::from(*raw));
            }
            Read::ItemValues { query, or_sums } => {
                out.push(12);
                put_path_query(out, query);
                out.push(u8::from(*or_sums));
            }
            Read::SumValues { query } => {
                out.push(13);
                put_path_query(out, query);
            }
            Read::Many { queries } => {
                out.push(14);
                put_path_queries(out, queries);
            }
            Read::BigSum {
                query,
                with_count,
                per_key,
            } => {
                out.push(15);
                put_path_query(out, query);
                out.push(u8::from(*with_count));
                out.push(u8::from(*per_key));
            }
            Read::Prove(prove) => {
                out.push(16);
                prove.encode(out);
            }
        }
    }
}

fn put_path_queries(out: &mut Vec<u8>, queries: &[PathQuery]) {
    put_len(out, queries.len());
    for query in queries {
        put_path_query(out, query);
    }
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_range(out: &mut Vec<u8>, tag: u8, start: u64, limit: u16) {
    out.push(tag);
    put_u64(out, start);
    out.extend_from_slice(&limit.to_le_bytes());
}

impl TreeRead {
    fn encode(&self, out: &mut Vec<u8>) {
        let at = |out: &mut Vec<u8>, tag: u8, position: u64| {
            out.push(tag);
            put_u64(out, position);
        };
        match *self {
            TreeRead::MmrValue(index) => at(out, 0, index),
            TreeRead::MmrRootHash => out.push(1),
            TreeRead::MmrLeafCount => out.push(2),
            TreeRead::BulkValue(position) => at(out, 3, position),
            TreeRead::BulkRange { start, limit } => put_range(out, 4, start, limit),
            TreeRead::BulkChunk(index) => at(out, 5, index),
            TreeRead::BulkBuffer => out.push(6),
            TreeRead::BulkCount => out.push(7),
            TreeRead::BulkChunkCount => out.push(8),
            TreeRead::DenseValue(position) => at(out, 9, u64::from(position)),
            TreeRead::DenseRootHash => out.push(10),
            TreeRead::DenseCount => out.push(11),
            TreeRead::CommitmentValue(position) => at(out, 12, position),
            TreeRead::CommitmentRange { start, limit } => put_range(out, 13, start, limit),
            TreeRead::CommitmentAnchor => out.push(14),
            TreeRead::CommitmentCount => out.push(15),
            TreeRead::PrivateDocumentValue(position) => at(out, 16, position),
            TreeRead::PrivateDocumentRange { start, limit } => put_range(out, 17, start, limit),
            TreeRead::PrivateDocumentCount => out.push(18),
        }
    }
}

impl Prove {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Prove::Many(queries) => {
                out.push(0);
                put_path_queries(out, queries);
            }
            Prove::BulkPositionRange {
                path,
                key,
                start,
                limit,
            } => {
                out.push(1);
                put_path(out, path);
                put_bytes(out, key);
                put_u64(out, *start);
                out.extend_from_slice(&limit.to_le_bytes());
            }
            Prove::TrunkChunk {
                path,
                max_depth,
                min_depth,
            } => {
                out.push(2);
                put_path(out, path);
                out.push(*max_depth);
                put_option(out, *min_depth, |out, depth| out.push(depth));
            }
            Prove::BranchChunk { path, key, depth } => {
                out.push(3);
                put_path(out, path);
                put_bytes(out, key);
                out.push(*depth);
            }
            Prove::PathQuery(query) => {
                out.push(5);
                put_path_query(out, query);
            }
            Prove::Chunk { path, ids } => {
                out.push(4);
                put_path(out, path);
                match ids {
                    ChunkIds::Merk(local_ids) => {
                        out.push(0);
                        put_path(out, local_ids);
                    }
                    ChunkIds::Entries {
                        start,
                        state,
                        parameter,
                    } => {
                        out.push(1);
                        put_u64(out, *start);
                        put_u64(out, *state);
                        out.push(*parameter);
                    }
                }
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
        | TreeType::ProvableCountProvableBigSumTree
        | TreeType::CountBigSumTree
        | TreeType::ProvableBigSumTree
        | TreeType::ProvableBigSumIndexedTree
        | TreeType::ProvableCountProvableBigSumIndexedTree => 0,
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
