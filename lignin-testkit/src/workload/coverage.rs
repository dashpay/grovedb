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
//! [`Operation`] lists the GroveDB operations that reach storage in a way of
//! their own, the ones the plan names for these generators. They are not the
//! variants of one GroveDB type, so that list is written here; the coverage
//! test fails when the suite stops producing one of them.

use std::collections::BTreeSet;

use grovedb::{
    element::reference_path::ReferencePathType, AggregateKind, AxisTraversal, Element, PathQuery,
    PathQueryShape, Query, TreeType,
};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use super::{
    op::{BatchOp, Op, Read, Write},
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
        /// A GroveDB operation that reaches storage in a way of its own.
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

        /// The plan's list of operations that reach storage in their own
        /// way.
        pub const OPERATION_GROUPS: &[&str] = &[$($group),+];
    };
}

operations! {
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
        AggregatePerKey = "One aggregate of a key range for each selected key.",
        SumBudget = "`GroveDb::query_aggregate_sums`: a walk that stops at a running total.",
        SumBudgetWindow = "The same walk as a query read mode, which is proven.",
        AxisRankedPage = "A page of an indexed tree in the order of an aggregate.",
        AxisBounded = "The entries of an indexed tree whose aggregate is in a range.",
        AxisRankOfKey = "The rank of a key in an indexed tree.",
        AxisAggregate = "An aggregate over the entries whose aggregate is in a range.",
        BranchedAxisRead = "One axis read for each of several indexed trees.";
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
            | Operation::AggregatePerKey
            | Operation::SumBudget => true,
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

    /// Counts what one operation stores.
    ///
    /// An element counts under its outermost variant. A tree counts under
    /// the type of the subtree it creates, looked up through a wrapper.
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
            Op::Write(_) | Op::Read(_) | Op::Commit | Op::FlushPendingDrops => Vec::new(),
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
            Op::PartialBatch(_) => {
                self.operations.insert(Operation::PartialBatch);
            }
            Op::FlushPendingDrops => {
                self.operations.insert(Operation::PendingDropsFlush);
            }
            Op::Write(Write::PutAux { .. }) => {
                self.operations.insert(Operation::AuxPut);
            }
            Op::Write(Write::DeleteAux { .. }) => {
                self.operations.insert(Operation::AuxDelete);
            }
            Op::Write(Write::DropFlat { .. }) => {
                self.operations.insert(Operation::FlatDrop);
            }
            Op::Read(Read::GetAux { .. }) => {
                self.operations.insert(Operation::AuxGet);
            }
            Op::Read(Read::AuxByPrefix { .. }) => {
                self.operations.insert(Operation::AuxByPrefix);
            }
            Op::Read(Read::SumBudget { .. }) => {
                self.operations.insert(Operation::SumBudget);
            }
            Op::Read(Read::PathQuery { query, proven }) => {
                self.operations.extend(query_operations(query, *proven));
            }
            Op::Read(Read::Rows { query, .. }) => {
                self.operations.extend(query_operations(query, false));
            }
            Op::Write(Write::Insert { .. } | Write::Delete { .. })
            | Op::Write(Write::ClearSubtree { .. } | Write::Append { .. })
            | Op::Read(Read::Get { .. } | Read::Query(_))
            | Op::Batch(_)
            | Op::Commit => {}
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

/// The operations one path query stands for.
///
/// A query GroveDB's grammar refuses stands for none: no workload produces
/// one.
fn query_operations(path_query: &PathQuery, proven: bool) -> Vec<Operation> {
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
        operations.push(if proven {
            Operation::CountOffsetPage
        } else {
            Operation::QueryOffset
        });
    }
    let axis_operation = |traversal: &AxisTraversal| match traversal {
        AxisTraversal::RankedPage { .. } => Operation::AxisRankedPage,
        AxisTraversal::Bounded { .. } => Operation::AxisBounded,
        AxisTraversal::RankOfKey { .. } => Operation::AxisRankOfKey,
        AxisTraversal::AggregateOverValueRange { .. } => Operation::AxisAggregate,
    };
    match path_query.classify() {
        Ok(PathQueryShape::AggregateLeaf { kind, .. }) => operations.push(match kind {
            AggregateKind::Count => Operation::AggregateCount,
            AggregateKind::Sum => Operation::AggregateSum,
            AggregateKind::CountAndSum => Operation::AggregateCountAndSum,
        }),
        Ok(PathQueryShape::AggregateCarrier { .. }) => {
            operations.push(Operation::AggregatePerKey);
        }
        Ok(PathQueryShape::AxisRead { axis }) => {
            operations.push(axis_operation(&axis.traversal));
        }
        Ok(PathQueryShape::BranchedAxisRead { axis, .. }) => {
            operations.push(Operation::BranchedAxisRead);
            operations.push(axis_operation(&axis.traversal));
        }
        Ok(PathQueryShape::SumBudget { .. }) => operations.push(Operation::SumBudgetWindow),
        Ok(PathQueryShape::KeySelection | PathQueryShape::CountOffsetPaginated { .. }) | Err(_) => {
        }
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
