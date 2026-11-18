//! Which element variants, tree types and reference path types a stream
//! produced.
//!
//! The variant lists below are written once and expand into two things: an
//! exhaustive `match` naming each variant, and the list of names a stream is
//! checked against. A variant added to GroveDB therefore stops this file
//! compiling until it is named here, and once it is named, the coverage test
//! fails until a workload produces it. Nothing has to be kept in step by
//! hand.

use std::collections::BTreeSet;

use grovedb::{element::reference_path::ReferencePathType, Element, TreeType};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use super::op::{BatchOp, Op};

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

/// What a stream has produced so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    elements: BTreeSet<&'static str>,
    tree_types: BTreeSet<&'static str>,
    reference_paths: BTreeSet<&'static str>,
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
        if let Op::Batch(ops) = op {
            for op in ops {
                if let BatchOp::RefreshReference { reference, .. } = op {
                    self.reference_paths
                        .insert(reference_path_variant(reference));
                }
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
