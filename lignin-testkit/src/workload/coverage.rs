//! Which element variants, tree types, reference path types and operations
//! a stream produced.
//!
//! The variant lists below are written once and expand into two things: an
//! exhaustive `match` naming each variant, and the list of names a stream is
//! checked against. A variant added to GroveDB therefore stops this file
//! compiling until it is named here, and once it is named, the coverage test
//! fails until a workload produces it. Nothing has to be kept in step by
//! hand.
//!
//! [`Operation`] lists what a stream makes GroveDB do: the writes, batches
//! and reads every workload is made of, the operations that reach storage
//! in a way of their own, and the remaining ones that charge a cost of
//! their own, in the groups the plan names for these generators. They are
//! not the variants of one GroveDB type, so that list is written here; the
//! coverage test fails when the suite stops producing one of them.
//! [`super::surface`] names the public GroveDB method behind each.

use std::collections::BTreeSet;

use grovedb::{
    element::reference_path::ReferencePathType, AggregateKind, AxisTraversal, Element, PathQuery,
    PathQueryShape, Query, TreeType,
};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use super::{
    op::{
        AppendEntry, BatchOp, ChunkIds, Fetch, IndexedTree, Op, Prove, Read, TransactionOp,
        TreeRead, Write,
    },
    Features,
};

macro_rules! named_variants {
    (
        $(#[$list_doc:meta])* $list:ident,
        $(#[$name_doc:meta])* $name:ident($ty:ident):
        $($variant:ident),+ $(,)?
    ) => {
        $(#[$list_doc])*
        pub const $list: &[&str] = &[$(stringify!($variant)),+];

        $(#[$name_doc])*
        pub fn $name(value: &$ty) -> &'static str {
            match value {
                $($ty::$variant { .. } => stringify!($variant)),+
            }
        }
    };
}

named_variants! {
    /// The name of every `Element` variant.
    ELEMENT_VARIANTS,
    /// The name of an element's outermost variant.
    element_variant(Element):
    Item,
    Reference,
    Tree,
    SumItem,
    SumTree,
    BigSumTree,
    CountTree,
    CountSumTree,
    ProvableCountTree,
    ItemWithSumItem,
    ProvableCountSumTree,
    CommitmentTree,
    MmrTree,
    BulkAppendTree,
    DenseAppendOnlyFixedSizeTree,
    NonCounted,
    NotSummed,
    NotCountedOrSummed,
    ReferenceWithSumItem,
    ProvableSumTree,
    ProvableCountProvableSumTree,
    ProvableSumIndexedTree,
    ProvableCountIndexedTree,
    ProvableCountProvableSumIndexedTree,
    PrivateDocumentStore,
    BidirectionalReference,
    ItemWithBackwardsReferences,
    SumItemWithBackwardsReferences,
    ItemWithSumItemWithBackwardsReferences,
    ProvableCountProvableBigSumTree,
}

named_variants! {
    /// The name of every `TreeType` variant.
    TREE_TYPES,
    /// The name of a tree type's variant.
    tree_type_variant(TreeType):
    NormalTree,
    SumTree,
    BigSumTree,
    CountTree,
    CountSumTree,
    ProvableCountTree,
    ProvableCountSumTree,
    CommitmentTree,
    MmrTree,
    BulkAppendTree,
    DenseAppendOnlyFixedSizeTree,
    ProvableSumTree,
    ProvableCountProvableSumTree,
    ProvableSumIndexedTree,
    ProvableCountIndexedTree,
    ProvableCountProvableSumIndexedTree,
    PrivateDocumentStore,
    ProvableCountProvableBigSumTree,
}

named_variants! {
    /// The name of every `ReferencePathType` variant.
    REFERENCE_PATH_TYPES,
    /// The name of a reference path's variant.
    reference_path_variant(ReferencePathType):
    AbsolutePathReference,
    UpstreamRootHeightReference,
    UpstreamRootHeightWithParentPathAdditionReference,
    UpstreamFromElementHeightReference,
    CousinReference,
    RemovedCousinReference,
    SiblingReference,
}

macro_rules! operations {
    ($($group:literal: $($variant:ident = $doc:literal),+;)+) => {
        /// Something a stream makes GroveDB do.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Operation {
            $($(#[doc = $doc] $variant,)+)+
        }

        impl Operation {
            /// Every operation, in the order of the plan's list.
            pub const ALL: &'static [Operation] = &[$($(Operation::$variant),+),+];

            /// The name of the operation.
            pub fn name(self) -> &'static str {
                match self {
                    $($(Operation::$variant => stringify!($variant),)+)+
                }
            }

            /// The entry of the plan's list the operation belongs to.
            pub fn group(self) -> &'static str {
                match self {
                    $($(Operation::$variant)|+ => $group,)+
                }
            }
        }

        /// The groups of operations, in the order of the plan's list.
        pub const OPERATION_GROUPS: &[&str] = &[$($group),+];
    };
}

operations! {
    "writes, batches and reads":
        Insert = "`GroveDb::insert`.",
        Delete = "`GroveDb::delete`.",
        ClearSubtree = "`GroveDb::clear_subtree`.",
        MmrAppend = "`GroveDb::mmr_tree_append`.",
        BulkAppend = "`GroveDb::bulk_append`.",
        DenseInsert = "`GroveDb::dense_tree_insert`.",
        PrivateDocumentInsert = "`GroveDb::private_document_store_insert`.",
        Batch = "`GroveDb::apply_batch`.",
        Get = "`GroveDb::get`.",
        Query = "`GroveDb::query`.",
        QueryRaw = "`GroveDb::query_raw`.",
        PathQueryRun = "`GroveDb::run_path_query`.",
        Commit = "The end of a block: the transaction is committed.";
    "patch":
        Patch = "A batch `Patch`.";
    "move":
        Move = "A batch `Move`.";
    "partial batches":
        PartialBatch = "`GroveDb::apply_partial_batch`.";
    "aux records":
        AuxPut = "`GroveDb::put_aux`.",
        AuxDelete = "`GroveDb::delete_aux`.",
        AuxGet = "`GroveDb::get_aux`.",
        AuxByPrefix = "`GroveDb::get_aux_by_key_prefix`.";
    "meta records":
        FlatDrop = "`GroveDb::drop_flat_subtree`, which writes a pending-drop record.",
        FlatDropInBatch = "A batch `DeleteTree` that drops flat, which writes the same record.",
        PendingDropsFlush = "`GroveDb::flush_pending_prefix_drops`, which reads and removes them.";
    "query offsets":
        QueryOffset = "A query that skips rows by reading past them.",
        CountOffsetPage = "A proven page of a tree that commits its count, skipped by count.",
        InstanceOffset = "`Query::offset`: a page by count for each subquery.";
    "conditional subqueries":
        ConditionalSubquery = "A query with a subquery of its own for some keys.",
        InstanceLimit = "`Query::limit`: a limit of its own for each subquery.";
    "aggregate queries":
        AggregateCount = "The count of a key range.",
        AggregateSum = "The sum of a key range.",
        AggregateCountAndSum = "The count and the sum of a key range.",
        AggregateCountPerKey = "The count of a key range for each selected key.",
        AggregateSumPerKey = "The sum of a key range for each selected key.",
        AggregateCountAndSumPerKey = "The count and the sum of a key range for each selected key.",
        SumBudget = "`GroveDb::query_aggregate_sums`: a walk that stops at a running total.",
        SumBudgetWindow = "The same walk as a query read mode, which is proven.",
        AxisRankedPage = "A page of an indexed tree in the order of an aggregate.",
        AxisBounded = "The entries of an indexed tree whose aggregate is in a range.",
        AxisRankOfKey = "The rank of a key in an indexed tree.",
        AxisAggregate = "An aggregate over the entries whose aggregate is in a range.",
        BranchedAxisRead = "One axis read for each of several indexed trees.";
    "conditional inserts and deletes":
        InsertIfNotExists = "`GroveDb::insert_if_not_exists`.",
        InsertIfNotExistsReturnExisting =
            "`GroveDb::insert_if_not_exists_return_existing_element`.",
        InsertIfChanged = "`GroveDb::insert_if_changed_value`.",
        DeleteIfEmptyTree = "`GroveDb::delete_if_empty_tree`.",
        DeleteUpTree = "`GroveDb::delete_up_tree_while_empty`.",
        DeleteUpTreeBuilt =
            "`GroveDb::delete_operations_for_delete_up_tree_while_empty`, applied as a batch.",
        DeleteBuilt = "`GroveDb::delete_operation_for_delete_internal`, applied as a batch.";
    "raw existence checks":
        HasRaw = "`GroveDb::has_raw`.",
        IsEmptyTree = "`GroveDb::is_empty_tree`.",
        GetRaw = "`GroveDb::get_raw`.",
        GetRawOptional = "`GroveDb::get_raw_optional`.",
        GetUncached = "`GroveDb::get_caching_optional` without the cache.",
        GetRawUncached = "`GroveDb::get_raw_caching_optional` without the cache.",
        GetRawOptionalUncached =
            "`GroveDb::get_raw_optional_caching_optional` without the cache.",
        BlockRead = "A read inside the open transaction, which sees its writes.";
    "direct writes to indexed trees":
        IndexedInsertCount = "`GroveDb::insert_into_count_indexed_tree`.",
        IndexedInsertSum = "`GroveDb::insert_into_provable_sum_indexed_tree`.",
        IndexedInsertCountSum =
            "`GroveDb::insert_into_provable_count_provable_sum_indexed_tree`.",
        IndexedDeleteCount = "`GroveDb::delete_from_count_indexed_tree`.",
        IndexedDeleteSum = "`GroveDb::delete_from_provable_sum_indexed_tree`.",
        IndexedDeleteCountSum =
            "`GroveDb::delete_from_provable_count_provable_sum_indexed_tree`.",
        IndexedReconcile = "`GroveDb::reconcile_indexed_tree_secondaries`.";
    "typed reads of the append-only trees":
        MmrValue = "`GroveDb::mmr_tree_get_value`.",
        MmrRootHash = "`GroveDb::mmr_tree_root_hash`.",
        MmrLeafCount = "`GroveDb::mmr_tree_leaf_count`.",
        BulkValue = "`GroveDb::bulk_get_value`.",
        BulkRange = "`GroveDb::bulk_get_range`.",
        BulkChunk = "`GroveDb::bulk_get_chunk`.",
        BulkBuffer = "`GroveDb::bulk_get_buffer`.",
        BulkCount = "`GroveDb::bulk_count`.",
        BulkChunkCount = "`GroveDb::bulk_chunk_count`.",
        DenseValue = "`GroveDb::dense_tree_get`.",
        DenseRootHash = "`GroveDb::dense_tree_root_hash`.",
        DenseCount = "`GroveDb::dense_tree_count`.",
        CommitmentValue = "`GroveDb::commitment_tree_get_value`.",
        CommitmentRange = "`GroveDb::commitment_tree_get_range`.",
        CommitmentAnchor = "`GroveDb::commitment_tree_anchor`.",
        CommitmentCount = "`GroveDb::commitment_tree_count`.",
        PrivateDocumentValue = "`GroveDb::private_document_store_get_value`.",
        PrivateDocumentRange = "`GroveDb::private_document_store_get_range`.",
        PrivateDocumentCount = "`GroveDb::private_document_store_count`.",
        CommitmentInsert = "`GroveDb::commitment_tree_insert`, the typed write.";
    "chunk proofs":
        TrunkChunk = "`GroveDb::prove_trunk_chunk`.",
        BranchChunk = "`GroveDb::prove_branch_chunk`.",
        RootChunk = "`GroveDb::fetch_chunk` of the root tree, asked for by the root hash.",
        RootTreeChunk = "`GroveDb::fetch_chunk` of chunks below the root of the root tree.",
        MerkChunk = "`GroveDb::fetch_chunk` of the chunk at the root of a Merk tree.",
        MerkChunkBelowRoot = "`GroveDb::fetch_chunk` of chunks below the root of a Merk tree.",
        EntriesChunk = "`GroveDb::fetch_chunk` of a page of an append-only tree.";
    "remaining query entry points":
        KeysOptional = "`GroveDb::query_keys_optional`.",
        RawKeysOptional = "`GroveDb::query_raw_keys_optional`.",
        AbsenceProof = "A proof that some of the keys asked for are not stored.",
        ItemValues = "`GroveDb::query_item_value`.",
        ItemValuesOrSums = "`GroveDb::query_item_value_or_sum`.",
        SumValues = "`GroveDb::query_sums`.",
        ManyRaw = "`GroveDb::query_many_raw`.",
        ProveMany = "`GroveDb::prove_query_many`.",
        BigSum = "`GroveDb::query_aggregate_big_sum`.",
        CountAndBigSum = "`GroveDb::query_aggregate_count_and_big_sum`.",
        BigSumPerKey = "`GroveDb::query_aggregate_big_sum_per_key`.",
        CountAndBigSumPerKey = "`GroveDb::query_aggregate_count_and_big_sum_per_key`.",
        BulkPositionRangeProof = "`GroveDb::prove_bulk_position_range`.";
    "rollbacks and savepoints":
        Rollback = "`GroveDb::rollback_transaction`.",
        Abandon = "A transaction dropped without a commit.",
        Savepoint = "`Transaction::set_savepoint`.",
        SavepointRollback = "`Transaction::rollback_to_savepoint`.",
        NestedSavepoint = "A savepoint set while another is on the stack.",
        SavepointKept = "A savepoint still on the stack when its transaction commits.",
        SnapshotRead = "A read through `GroveDb::start_snapshot_read_transaction`.",
        SnapshotReadAfterCommit = "A read through a snapshot older than the last commit.";
}

impl Features {
    /// Whether a stream generated for these features can hold `operation`.
    pub fn supports(&self, operation: Operation) -> bool {
        match operation {
            Operation::Move => self.move_element,
            Operation::FlatDrop | Operation::FlatDropInBatch | Operation::PendingDropsFlush => {
                self.flat_drop
            }
            Operation::CountOffsetPage => self.v1_proofs,
            Operation::InstanceOffset => self.instance_offsets,
            Operation::InstanceLimit => self.instance_limits,
            Operation::SumBudgetWindow
            | Operation::AxisRankedPage
            | Operation::AxisBounded
            | Operation::AxisRankOfKey
            | Operation::AxisAggregate
            | Operation::BranchedAxisRead => self.read_modes,
            Operation::Patch
            | Operation::PartialBatch
            | Operation::AuxPut
            | Operation::AuxDelete
            | Operation::AuxGet
            | Operation::AuxByPrefix
            | Operation::QueryOffset
            | Operation::ConditionalSubquery
            | Operation::AggregateCount
            | Operation::AggregateSum
            | Operation::AggregateCountAndSum
            | Operation::AggregateCountPerKey
            | Operation::AggregateSumPerKey
            | Operation::AggregateCountAndSumPerKey
            | Operation::SumBudget => true,
            _ => true,
        }
    }
}

/// What a stream has produced so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    elements: BTreeSet<&'static str>,
    tree_types: BTreeSet<&'static str>,
    reference_paths: BTreeSet<&'static str>,
    operations: BTreeSet<Operation>,
    /// How many savepoints are on the stack of the open transaction.
    savepoints: u32,
    /// How many blocks were committed since the open snapshot was opened.
    commits_under_snapshot: Option<u32>,
}

impl Coverage {
    /// Nothing produced yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The coverage of a whole stream.
    pub fn of<'a>(ops: impl IntoIterator<Item = &'a Op>) -> Self {
        let mut coverage = Self::new();
        for op in ops {
            coverage.record(op);
        }
        coverage
    }

    /// Counts what one operation stores and does.
    ///
    /// An element counts under its outermost variant. A tree counts under
    /// the type of the subtree it creates, looked up through a wrapper. The
    /// operations that depend on what came before, such as a savepoint set
    /// inside another, are counted as the stream is recorded in order.
    pub fn record(&mut self, op: &Op) {
        for element in op.elements() {
            self.elements.insert(element_variant(element));
            if let Some(tree_type) = element.tree_type() {
                self.tree_types.insert(tree_type_variant(&tree_type));
            }
            match element.underlying() {
                Element::Reference(path, ..) | Element::ReferenceWithSumItem(path, ..) => {
                    self.reference_paths.insert(reference_path_variant(path));
                }
                Element::BidirectionalReference(reference, _) => {
                    self.reference_paths
                        .insert(reference_path_variant(&reference.forward_reference_path));
                }
                _ => {}
            }
        }
        let batch_ops: Vec<&BatchOp> = match op {
            Op::Batch(ops) => ops.iter().collect(),
            Op::PartialBatch(batch) => batch.ops.iter().chain(&batch.add_on).collect(),
            Op::Write(_)
            | Op::Read(_)
            | Op::Commit
            | Op::FlushPendingDrops
            | Op::Transaction(_) => Vec::new(),
        };
        for batch_op in batch_ops {
            match batch_op {
                BatchOp::RefreshReference { reference, .. } => {
                    self.reference_paths
                        .insert(reference_path_variant(reference));
                }
                BatchOp::Patch { .. } => {
                    self.operations.insert(Operation::Patch);
                }
                BatchOp::Move { .. } => {
                    self.operations.insert(Operation::Move);
                }
                BatchOp::DropFlat { .. } => {
                    self.operations.insert(Operation::FlatDropInBatch);
                }
                BatchOp::InsertOrReplace { .. }
                | BatchOp::InsertOnly { .. }
                | BatchOp::InsertIfNotExists { .. }
                | BatchOp::Replace { .. }
                | BatchOp::Delete { .. }
                | BatchOp::DeleteTree { .. }
                | BatchOp::Append { .. } => {}
            }
        }
        match op {
            Op::PartialBatch(_) => self.produced(Operation::PartialBatch),
            Op::FlushPendingDrops => self.produced(Operation::PendingDropsFlush),
            Op::Batch(_) => self.produced(Operation::Batch),
            Op::Write(write) => self.produced(write_operation(write)),
            Op::Read(read) => self.operations.extend(read_operations(read)),
            Op::Commit => {
                self.produced(Operation::Commit);
                if self.savepoints > 0 {
                    self.produced(Operation::SavepointKept);
                }
                self.savepoints = 0;
                if let Some(commits) = &mut self.commits_under_snapshot {
                    *commits += 1;
                }
            }
            Op::Transaction(step) => self.record_transaction_op(step),
        }
    }

    fn produced(&mut self, operation: Operation) {
        self.operations.insert(operation);
    }

    fn record_transaction_op(&mut self, step: &TransactionOp) {
        match step {
            TransactionOp::Rollback => {
                self.produced(Operation::Rollback);
                self.savepoints = 0;
            }
            TransactionOp::Abandon => {
                self.produced(Operation::Abandon);
                self.savepoints = 0;
            }
            TransactionOp::SetSavepoint => {
                self.produced(Operation::Savepoint);
                if self.savepoints > 0 {
                    self.produced(Operation::NestedSavepoint);
                }
                self.savepoints += 1;
            }
            TransactionOp::RollbackToSavepoint => {
                self.produced(Operation::SavepointRollback);
                self.savepoints = self.savepoints.saturating_sub(1);
            }
            TransactionOp::OpenSnapshot => self.commits_under_snapshot = Some(0),
            TransactionOp::CloseSnapshot => self.commits_under_snapshot = None,
            TransactionOp::SnapshotRead(read) => {
                self.produced(Operation::SnapshotRead);
                if self
                    .commits_under_snapshot
                    .is_some_and(|commits| commits > 0)
                {
                    self.produced(Operation::SnapshotReadAfterCommit);
                }
                self.operations.extend(read_operations(read));
            }
            TransactionOp::BlockRead(read) => {
                self.produced(Operation::BlockRead);
                self.operations.extend(read_operations(read));
            }
        }
    }

    /// The element variants produced.
    pub fn elements(&self) -> &BTreeSet<&'static str> {
        &self.elements
    }

    /// The tree types produced.
    pub fn tree_types(&self) -> &BTreeSet<&'static str> {
        &self.tree_types
    }

    /// The reference path types produced.
    pub fn reference_paths(&self) -> &BTreeSet<&'static str> {
        &self.reference_paths
    }

    /// The operations produced.
    pub fn operations(&self) -> &BTreeSet<Operation> {
        &self.operations
    }

    /// The operations not produced yet.
    pub fn missing_operations(&self) -> Vec<Operation> {
        Operation::ALL
            .iter()
            .copied()
            .filter(|operation| !self.operations.contains(operation))
            .collect()
    }

    /// The element variants not produced yet.
    pub fn missing_elements(&self) -> Vec<&'static str> {
        missing(ELEMENT_VARIANTS, &self.elements)
    }

    /// The tree types not produced yet.
    pub fn missing_tree_types(&self) -> Vec<&'static str> {
        missing(TREE_TYPES, &self.tree_types)
    }

    /// The reference path types not produced yet.
    pub fn missing_reference_paths(&self) -> Vec<&'static str> {
        missing(REFERENCE_PATH_TYPES, &self.reference_paths)
    }
}

fn missing(all: &[&'static str], seen: &BTreeSet<&'static str>) -> Vec<&'static str> {
    all.iter()
        .copied()
        .filter(|name| !seen.contains(name))
        .collect()
}

/// The operation a direct write is.
fn write_operation(write: &Write) -> Operation {
    match write {
        Write::Insert { .. } => Operation::Insert,
        Write::Delete { .. } => Operation::Delete,
        Write::ClearSubtree { .. } => Operation::ClearSubtree,
        Write::Append { entry, .. } => match entry {
            AppendEntry::Mmr(_) => Operation::MmrAppend,
            AppendEntry::Bulk(_) => Operation::BulkAppend,
            AppendEntry::Dense(_) => Operation::DenseInsert,
            AppendEntry::PrivateDocument(_) => Operation::PrivateDocumentInsert,
            AppendEntry::Commitment { .. } => Operation::Batch,
        },
        Write::PutAux { .. } => Operation::AuxPut,
        Write::DeleteAux { .. } => Operation::AuxDelete,
        Write::DropFlat { .. } => Operation::FlatDrop,
        Write::InsertIfNotExists {
            return_existing, ..
        } => {
            if *return_existing {
                Operation::InsertIfNotExistsReturnExisting
            } else {
                Operation::InsertIfNotExists
            }
        }
        Write::InsertIfChanged { .. } => Operation::InsertIfChanged,
        Write::DeleteIfEmptyTree { .. } => Operation::DeleteIfEmptyTree,
        Write::DeleteUpTree { built, .. } => {
            if *built {
                Operation::DeleteUpTreeBuilt
            } else {
                Operation::DeleteUpTree
            }
        }
        Write::DeleteBuilt { .. } => Operation::DeleteBuilt,
        Write::IndexedInsert { tree, .. } => match tree {
            IndexedTree::Count => Operation::IndexedInsertCount,
            IndexedTree::Sum => Operation::IndexedInsertSum,
            IndexedTree::CountSum => Operation::IndexedInsertCountSum,
        },
        Write::IndexedDelete { tree, .. } => match tree {
            IndexedTree::Count => Operation::IndexedDeleteCount,
            IndexedTree::Sum => Operation::IndexedDeleteSum,
            IndexedTree::CountSum => Operation::IndexedDeleteCountSum,
        },
        Write::ReconcileIndexed { .. } => Operation::IndexedReconcile,
        Write::CommitmentInsert { .. } => Operation::CommitmentInsert,
    }
}

/// The operations a read stands for.
fn read_operations(read: &Read) -> Vec<Operation> {
    let rows_reader = |raw: bool| {
        if raw {
            Operation::QueryRaw
        } else {
            Operation::Query
        }
    };
    match read {
        Read::Get { .. } => vec![Operation::Get],
        Read::Query(query) => vec![rows_reader(query.raw)],
        Read::PathQuery { query, proven } => {
            let mut operations = query_operations(query, *proven);
            operations.push(Operation::PathQueryRun);
            operations
        }
        Read::Rows { query, raw } => {
            let mut operations = selection_operations(query);
            operations.push(rows_reader(*raw));
            operations
        }
        Read::SumBudget { .. } => vec![Operation::SumBudget],
        Read::GetAux { .. } => vec![Operation::AuxGet],
        Read::AuxByPrefix { .. } => vec![Operation::AuxByPrefix],
        Read::HasRaw { .. } => vec![Operation::HasRaw],
        Read::IsEmptyTree { .. } => vec![Operation::IsEmptyTree],
        Read::Fetch { fetch, cache, .. } => vec![match (fetch, cache) {
            (Fetch::Resolved, true) => Operation::Get,
            (Fetch::Resolved, false) => Operation::GetUncached,
            (Fetch::Raw, true) => Operation::GetRaw,
            (Fetch::Raw, false) => Operation::GetRawUncached,
            (Fetch::RawOptional, true) => Operation::GetRawOptional,
            (Fetch::RawOptional, false) => Operation::GetRawOptionalUncached,
        }],
        Read::Tree { read, .. } => vec![tree_read_operation(*read)],
        Read::KeysOptional { raw, .. } => vec![
            if *raw {
                Operation::RawKeysOptional
            } else {
                Operation::KeysOptional
            },
            Operation::AbsenceProof,
        ],
        Read::ItemValues { or_sums, .. } => vec![if *or_sums {
            Operation::ItemValuesOrSums
        } else {
            Operation::ItemValues
        }],
        Read::SumValues { .. } => vec![Operation::SumValues],
        Read::Many { .. } => vec![Operation::ManyRaw],
        Read::BigSum {
            with_count,
            per_key,
            ..
        } => vec![match (with_count, per_key) {
            (false, false) => Operation::BigSum,
            (true, false) => Operation::CountAndBigSum,
            (false, true) => Operation::BigSumPerKey,
            (true, true) => Operation::CountAndBigSumPerKey,
        }],
        Read::Prove(prove) => vec![match prove {
            Prove::Many(_) => Operation::ProveMany,
            Prove::BulkPositionRange { .. } => Operation::BulkPositionRangeProof,
            Prove::TrunkChunk { .. } => Operation::TrunkChunk,
            Prove::BranchChunk { .. } => Operation::BranchChunk,
            Prove::Chunk { path, ids } => match ids {
                ChunkIds::Entries { .. } => Operation::EntriesChunk,
                ChunkIds::Merk(ids) if path.is_empty() && ids.is_empty() => Operation::RootChunk,
                ChunkIds::Merk(_) if path.is_empty() => Operation::RootTreeChunk,
                ChunkIds::Merk(ids) if ids.iter().any(|id| !id.is_empty()) => {
                    Operation::MerkChunkBelowRoot
                }
                ChunkIds::Merk(_) => Operation::MerkChunk,
            },
        }],
    }
}

fn tree_read_operation(read: TreeRead) -> Operation {
    match read {
        TreeRead::MmrValue(_) => Operation::MmrValue,
        TreeRead::MmrRootHash => Operation::MmrRootHash,
        TreeRead::MmrLeafCount => Operation::MmrLeafCount,
        TreeRead::BulkValue(_) => Operation::BulkValue,
        TreeRead::BulkRange { .. } => Operation::BulkRange,
        TreeRead::BulkChunk(_) => Operation::BulkChunk,
        TreeRead::BulkBuffer => Operation::BulkBuffer,
        TreeRead::BulkCount => Operation::BulkCount,
        TreeRead::BulkChunkCount => Operation::BulkChunkCount,
        TreeRead::DenseValue(_) => Operation::DenseValue,
        TreeRead::DenseRootHash => Operation::DenseRootHash,
        TreeRead::DenseCount => Operation::DenseCount,
        TreeRead::CommitmentValue(_) => Operation::CommitmentValue,
        TreeRead::CommitmentRange { .. } => Operation::CommitmentRange,
        TreeRead::CommitmentAnchor => Operation::CommitmentAnchor,
        TreeRead::CommitmentCount => Operation::CommitmentCount,
        TreeRead::PrivateDocumentValue(_) => Operation::PrivateDocumentValue,
        TreeRead::PrivateDocumentRange { .. } => Operation::PrivateDocumentRange,
        TreeRead::PrivateDocumentCount => Operation::PrivateDocumentCount,
    }
}

/// The operations a query stands for when it selects rows: what a
/// [`Read::Rows`] is, whose readers take an offset with any query.
fn selection_operations(path_query: &PathQuery) -> Vec<Operation> {
    let mut operations = Vec::new();
    let query = &path_query.query.query;
    if has_conditional_subquery(query) {
        operations.push(Operation::ConditionalSubquery);
    }
    if query.has_instance_limit_anywhere() {
        operations.push(Operation::InstanceLimit);
    }
    if query.has_instance_offset_anywhere() {
        operations.push(Operation::InstanceOffset);
    }
    if path_query.query.offset.is_some_and(|offset| offset > 0) {
        operations.push(Operation::QueryOffset);
    }
    operations
}

/// The operations a [`Read::PathQuery`] stands for.
///
/// A query GroveDB's grammar refuses stands for none: no workload produces
/// one.
fn query_operations(path_query: &PathQuery, proven: bool) -> Vec<Operation> {
    let Ok(shape) = path_query.classify() else {
        return Vec::new();
    };
    let mut operations = selection_operations(path_query);
    if proven && matches!(shape, PathQueryShape::CountOffsetPaginated { .. }) {
        for operation in &mut operations {
            if *operation == Operation::QueryOffset {
                *operation = Operation::CountOffsetPage;
            }
        }
    }
    let axis_operation = |traversal: &AxisTraversal| match traversal {
        AxisTraversal::RankedPage { .. } => Operation::AxisRankedPage,
        AxisTraversal::Bounded { .. } => Operation::AxisBounded,
        AxisTraversal::RankOfKey { .. } => Operation::AxisRankOfKey,
        AxisTraversal::AggregateOverValueRange { .. } => Operation::AxisAggregate,
    };
    match shape {
        PathQueryShape::AggregateLeaf { kind, .. } => operations.push(match kind {
            AggregateKind::Count => Operation::AggregateCount,
            AggregateKind::Sum => Operation::AggregateSum,
            AggregateKind::CountAndSum => Operation::AggregateCountAndSum,
        }),
        PathQueryShape::AggregateCarrier { kind, .. } => operations.push(match kind {
            AggregateKind::Count => Operation::AggregateCountPerKey,
            AggregateKind::Sum => Operation::AggregateSumPerKey,
            AggregateKind::CountAndSum => Operation::AggregateCountAndSumPerKey,
        }),
        PathQueryShape::AxisRead { axis } => {
            operations.push(axis_operation(&axis.traversal));
        }
        PathQueryShape::BranchedAxisRead { axis, .. } => {
            operations.push(Operation::BranchedAxisRead);
            operations.push(axis_operation(&axis.traversal));
        }
        PathQueryShape::SumBudget { .. } => operations.push(Operation::SumBudgetWindow),
        PathQueryShape::KeySelection | PathQueryShape::CountOffsetPaginated { .. } => {}
    }
    operations
}

fn has_conditional_subquery(query: &Query) -> bool {
    let branches = query.conditional_subquery_branches.as_ref();
    branches.is_some_and(|branches| !branches.is_empty())
        || branches
            .into_iter()
            .flat_map(|branches| branches.values())
            .chain([&query.default_subquery_branch])
            .filter_map(|branch| branch.subquery.as_deref())
            .any(has_conditional_subquery)
}
