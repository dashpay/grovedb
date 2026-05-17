//! Helpers
//! Implements helper functions in Element

use grovedb_version::{check_grovedb_v0, version::GroveVersion};
use integer_encoding::VarInt;

use crate::{
    element::{Element, ElementFlags},
    error::ElementError,
    reference_path::{path_from_reference_path_type, ReferencePathType},
};

impl Element {
    /// Returns `true` if this element is wrapped in `Element::NonCounted`.
    /// The wrapper suppresses count propagation to the parent count tree but
    /// leaves all other behavior (storage, hashing, sum propagation, internal
    /// aggregation) unchanged.
    pub fn is_non_counted(&self) -> bool {
        matches!(self, Element::NonCounted(_))
    }

    /// Returns `true` if this element is wrapped in `Element::NotSummed`.
    /// The wrapper suppresses sum propagation to the parent sum tree but
    /// leaves all other behavior (storage, hashing, count propagation,
    /// internal aggregation) unchanged.
    pub fn is_not_summed(&self) -> bool {
        matches!(self, Element::NotSummed(_))
    }

    /// Returns `true` if this element is wrapped in
    /// `Element::NotCountedOrSummed`. The wrapper suppresses BOTH count
    /// and sum propagation to the parent tree but leaves all other
    /// behavior (storage, hashing, internal aggregation) unchanged.
    pub fn is_not_counted_or_summed(&self) -> bool {
        matches!(self, Element::NotCountedOrSummed(_))
    }

    /// Returns `true` if this element is wrapped in any of the wrapper
    /// variants (`NonCounted`, `NotSummed`, `NotCountedOrSummed`). Useful
    /// for paths that need to add the +1 wrapper-byte cost overhead
    /// regardless of which wrapper is in use.
    pub fn is_wrapped(&self) -> bool {
        matches!(
            self,
            Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_)
        )
    }

    /// Returns the wrapped element if `self` is any wrapper (`NonCounted`,
    /// `NotSummed`, or `NotCountedOrSummed`), else `self`. Use this when
    /// you need to inspect the actual element type and don't care whether
    /// it is wrapped.
    ///
    /// Only unwraps one level — the constructors and (de)serializers reject
    /// any wrapper nesting, so a single unwrap is always sufficient.
    pub fn underlying(&self) -> &Element {
        match self {
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner,
            other => other,
        }
    }

    /// Mutable variant of [`underlying`].
    pub fn underlying_mut(&mut self) -> &mut Element {
        match self {
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner,
            other => other,
        }
    }

    /// Owned variant of [`underlying`].
    pub fn into_underlying(self) -> Element {
        match self {
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => *inner,
            other => other,
        }
    }

    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else.
    ///
    /// `NonCounted` delegates to its inner element — sums still propagate
    /// when the wrapper is inserted into a sum-bearing parent.
    /// `NotSummed` and `NotCountedOrSummed` return 0 — those wrappers'
    /// purpose is to contribute nothing to the parent sum tree.
    /// `ReferenceWithSumItem` returns the explicit sum value carried on the
    /// variant — independent of the resolved target's value.
    pub fn sum_value_or_default(&self) -> i64 {
        match self {
            Element::NonCounted(inner) => inner.sum_value_or_default(),
            Element::NotSummed(_) | Element::NotCountedOrSummed(_) => 0,
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _)
            | Element::CountSumTree(_, _, sum_value, _)
            | Element::ProvableCountSumTree(_, _, sum_value, _)
            | Element::ProvableSumTree(_, sum_value, _)
            | Element::ReferenceWithSumItem(_, _, sum_value, _) => *sum_value,
            _ => 0,
        }
    }

    /// Decoded the integer value in the CountTree element type, returns 1 for
    /// everything else.
    ///
    /// `NonCounted` and `NotCountedOrSummed` return 0 — both wrappers
    /// suppress the parent count contribution.
    /// `NotSummed` delegates to its inner — counts still propagate.
    pub fn count_value_or_default(&self) -> u64 {
        match self {
            Element::NonCounted(_) | Element::NotCountedOrSummed(_) => 0,
            Element::NotSummed(inner) => inner.count_value_or_default(),
            Element::CountTree(_, count_value, _)
            | Element::CountSumTree(_, count_value, ..)
            | Element::ProvableCountTree(_, count_value, _)
            | Element::ProvableCountSumTree(_, count_value, ..)
            | Element::CountIndexedTree(.., count_value, _)
            | Element::ProvableCountIndexedTree(.., count_value, _) => *count_value,
            _ => 1,
        }
    }

    /// Decoded the count and sum values from the element type, returns (1, 0)
    /// for elements without count/sum semantics.
    ///
    /// `NonCounted` returns `(0, inner_sum)` — count is suppressed, sum still
    /// propagates.
    /// `NotSummed` returns `(inner_count, 0)` — sum is suppressed, count
    /// still propagates.
    /// `NotCountedOrSummed` returns `(0, 0)` — both are suppressed.
    /// `ReferenceWithSumItem` returns `(1, sum_value)` — counts as one
    /// element like a plain reference, contributes its explicit sum.
    pub fn count_sum_value_or_default(&self) -> (u64, i64) {
        match self {
            Element::NonCounted(inner) => (0, inner.sum_value_or_default()),
            Element::NotSummed(inner) => (inner.count_value_or_default(), 0),
            Element::NotCountedOrSummed(_) => (0, 0),
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _)
            | Element::ProvableSumTree(_, sum_value, _)
            | Element::ReferenceWithSumItem(_, _, sum_value, _) => (1, *sum_value),
            Element::CountTree(_, count_value, _) => (*count_value, 0),
            Element::CountSumTree(_, count_value, sum_value, _)
            | Element::ProvableCountSumTree(_, count_value, sum_value, _) => {
                (*count_value, *sum_value)
            }
            Element::ProvableCountTree(_, count_value, _) => (*count_value, 0),
            Element::CountIndexedTree(.., count_value, _)
            | Element::ProvableCountIndexedTree(.., count_value, _) => (*count_value, 0),
            _ => (1, 0),
        }
    }

    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else. `NonCounted` delegates to its inner. `NotSummed`
    /// and `NotCountedOrSummed` return 0. `ReferenceWithSumItem` returns
    /// its explicit i64 sum cast to i128.
    pub fn big_sum_value_or_default(&self) -> i128 {
        match self {
            Element::NonCounted(inner) => inner.big_sum_value_or_default(),
            Element::NotSummed(_) | Element::NotCountedOrSummed(_) => 0,
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _)
            | Element::CountSumTree(_, _, sum_value, _)
            | Element::ProvableCountSumTree(_, _, sum_value, _)
            | Element::ProvableSumTree(_, sum_value, _)
            | Element::ReferenceWithSumItem(_, _, sum_value, _) => *sum_value as i128,
            Element::BigSumTree(_, sum_value, _) => *sum_value,
            _ => 0,
        }
    }

    /// Decoded the integer value in the SumItem element type. Looks through
    /// a `NonCounted` wrapper. Also returns the explicit sum from
    /// `ReferenceWithSumItem`.
    pub fn as_sum_item_value(&self) -> Result<i64, ElementError> {
        match self.underlying() {
            Element::SumItem(value, _) => Ok(*value),
            Element::ItemWithSumItem(_, value, _) => Ok(*value),
            Element::ReferenceWithSumItem(_, _, value, _) => Ok(*value),
            _ => Err(ElementError::WrongElementType("expected a sum item")),
        }
    }

    /// Decoded the integer value in the SumItem element type. Looks through
    /// a `NonCounted` wrapper. Also returns the explicit sum from
    /// `ReferenceWithSumItem`.
    pub fn into_sum_item_value(self) -> Result<i64, ElementError> {
        match self.into_underlying() {
            Element::SumItem(value, _) => Ok(value),
            Element::ItemWithSumItem(_, value, _) => Ok(value),
            Element::ReferenceWithSumItem(_, _, value, _) => Ok(value),
            _ => Err(ElementError::WrongElementType("expected a sum item")),
        }
    }

    /// Decoded the integer value in the SumTree element type. Looks through
    /// a `NonCounted` wrapper.
    pub fn as_sum_tree_value(&self) -> Result<i64, ElementError> {
        match self.underlying() {
            Element::SumTree(_, value, _) => Ok(*value),
            _ => Err(ElementError::WrongElementType("expected a sum tree")),
        }
    }

    /// Decoded the integer value in the SumTree element type. Looks through
    /// a `NonCounted` wrapper.
    pub fn into_sum_tree_value(self) -> Result<i64, ElementError> {
        match self.into_underlying() {
            Element::SumTree(_, value, _) => Ok(value),
            _ => Err(ElementError::WrongElementType("expected a sum tree")),
        }
    }

    /// Gives the item value in the Item element type. Looks through a
    /// `NonCounted` wrapper.
    pub fn as_item_bytes(&self) -> Result<&[u8], ElementError> {
        match self.underlying() {
            Element::Item(value, _) => Ok(value),
            Element::ItemWithSumItem(value, ..) => Ok(value),
            _ => Err(ElementError::WrongElementType("expected an item")),
        }
    }

    /// Gives the item value in the Item element type. Looks through a
    /// `NonCounted` wrapper.
    pub fn into_item_bytes(self) -> Result<Vec<u8>, ElementError> {
        match self.into_underlying() {
            Element::Item(value, _) => Ok(value),
            Element::ItemWithSumItem(value, ..) => Ok(value),
            _ => Err(ElementError::WrongElementType("expected an item")),
        }
    }

    /// Gives the reference path type in the Reference element type. Looks
    /// through a `NonCounted` wrapper. Accepts both `Reference` and
    /// `ReferenceWithSumItem`.
    pub fn into_reference_path_type(self) -> Result<ReferencePathType, ElementError> {
        match self.into_underlying() {
            Element::Reference(value, ..) => Ok(value),
            Element::ReferenceWithSumItem(value, ..) => Ok(value),
            _ => Err(ElementError::WrongElementType("expected a reference")),
        }
    }

    /// Check if the element is a sum tree. Looks through `NonCounted`.
    pub fn is_sum_tree(&self) -> bool {
        matches!(self.underlying(), Element::SumTree(..))
    }

    /// Check if the element is a big sum tree. Looks through `NonCounted`.
    pub fn is_big_sum_tree(&self) -> bool {
        matches!(self.underlying(), Element::BigSumTree(..))
    }

    /// Check if the element is a provable sum tree. Looks through wrappers.
    pub fn is_provable_sum_tree(&self) -> bool {
        matches!(self.underlying(), Element::ProvableSumTree(..))
    }

    /// Decoded sum value from a `ProvableSumTree`. Looks through wrappers.
    pub fn as_provable_sum_tree_value(&self) -> Result<i64, ElementError> {
        match self.underlying() {
            Element::ProvableSumTree(_, value, _) => Ok(*value),
            _ => Err(ElementError::WrongElementType(
                "expected a provable sum tree",
            )),
        }
    }

    /// Owned variant of [`as_provable_sum_tree_value`].
    pub fn into_provable_sum_tree_value(self) -> Result<i64, ElementError> {
        match self.into_underlying() {
            Element::ProvableSumTree(_, value, _) => Ok(value),
            _ => Err(ElementError::WrongElementType(
                "expected a provable sum tree",
            )),
        }
    }

    /// Check if the element is a tree but not a sum tree. Looks through
    /// `NonCounted`.
    pub fn is_basic_tree(&self) -> bool {
        matches!(self.underlying(), Element::Tree(..))
    }

    /// Check if the element is a tree. Looks through `NonCounted`.
    pub fn is_any_tree(&self) -> bool {
        matches!(
            self.underlying(),
            Element::SumTree(..)
                | Element::Tree(..)
                | Element::BigSumTree(..)
                | Element::CountTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableSumTree(..)
                | Element::CommitmentTree(..)
                | Element::MmrTree(..)
                | Element::BulkAppendTree(..)
                | Element::DenseAppendOnlyFixedSizeTree(..)
                | Element::CountIndexedTree(..)
                | Element::ProvableCountIndexedTree(..)
        )
    }

    /// Check if the element is a count-indexed tree (carries both a primary
    /// count Merk and a secondary count-ordered Merk). Looks through
    /// `NonCounted`.
    pub fn is_count_indexed_tree(&self) -> bool {
        matches!(
            self.underlying(),
            Element::CountIndexedTree(..) | Element::ProvableCountIndexedTree(..)
        )
    }

    /// Check if the element is a commitment tree. Looks through `NonCounted`.
    pub fn is_commitment_tree(&self) -> bool {
        matches!(self.underlying(), Element::CommitmentTree(..))
    }

    /// Check if the element is an MMR tree. Looks through `NonCounted`.
    pub fn is_mmr_tree(&self) -> bool {
        matches!(self.underlying(), Element::MmrTree(..))
    }

    /// Check if the element is a bulk append tree. Looks through `NonCounted`.
    pub fn is_bulk_append_tree(&self) -> bool {
        matches!(self.underlying(), Element::BulkAppendTree(..))
    }

    /// Check if the element is a dense append-only fixed-size tree. Looks
    /// through `NonCounted`.
    pub fn is_dense_tree(&self) -> bool {
        matches!(self.underlying(), Element::DenseAppendOnlyFixedSizeTree(..))
    }

    /// Check if the element is a tree type that stores data in the data
    /// namespace as non-Merk entries.  These tree types have an always-empty
    /// Merk (root_key = None) and never contain child subtrees. The data
    /// namespace must be cleared directly rather than iterated as Merk
    /// elements.
    ///
    /// Note: This must be kept in sync with
    /// `TreeType::uses_non_merk_data_storage()` in the merk crate.
    /// Looks through `NonCounted`.
    pub fn uses_non_merk_data_storage(&self) -> bool {
        matches!(
            self.underlying(),
            Element::CommitmentTree(..)
                | Element::MmrTree(..)
                | Element::BulkAppendTree(..)
                | Element::DenseAppendOnlyFixedSizeTree(..)
        )
    }

    /// Returns the entry count for non-Merk data tree types, or `None` for
    /// regular Merk trees and non-tree elements.  This is used by delete
    /// and is_empty_tree operations to determine emptiness without
    /// iterating the data namespace. Looks through `NonCounted`.
    pub fn non_merk_entry_count(&self) -> Option<u64> {
        match self.underlying() {
            Element::CommitmentTree(count, ..) => Some(*count),
            Element::MmrTree(mmr_size, _) => Some(*mmr_size),
            Element::BulkAppendTree(count, ..) => Some(*count),
            Element::DenseAppendOnlyFixedSizeTree(count, ..) => Some(*count as u64),
            _ => None,
        }
    }

    /// Check if the element is a non-empty tree that should have child data.
    ///
    /// For merk-backed trees this means the root key is `Some(_)` (at least one
    /// node). Non-merk tree types (MmrTree, BulkAppendTree, CommitmentTree,
    /// DenseAppendOnlyFixedSizeTree) always carry their own data and are
    /// considered non-empty regardless of their count field.
    /// Looks through `NonCounted`.
    pub fn is_non_empty_tree(&self) -> bool {
        matches!(
            self.underlying(),
            Element::Tree(Some(_), _)
                | Element::SumTree(Some(_), ..)
                | Element::BigSumTree(Some(_), ..)
                | Element::CountTree(Some(_), ..)
                | Element::CountSumTree(Some(_), ..)
                | Element::ProvableCountTree(Some(_), ..)
                | Element::ProvableCountSumTree(Some(_), ..)
                | Element::ProvableSumTree(Some(_), ..)
                | Element::CommitmentTree(..)
                | Element::MmrTree(..)
                | Element::BulkAppendTree(..)
                | Element::DenseAppendOnlyFixedSizeTree(..)
                | Element::CountIndexedTree(Some(_), ..)
                | Element::CountIndexedTree(_, Some(_), ..)
                | Element::ProvableCountIndexedTree(Some(_), ..)
                | Element::ProvableCountIndexedTree(_, Some(_), ..)
        )
    }

    /// Check if the element is a non-empty Merk-backed tree.
    ///
    /// Returns true only for standard Merk trees (Tree, SumTree, BigSumTree,
    /// CountTree, CountSumTree, ProvableCountTree, ProvableCountSumTree)
    /// with a `Some(_)` root key. Excludes non-Merk tree types (MmrTree,
    /// BulkAppendTree, CommitmentTree, DenseAppendOnlyFixedSizeTree).
    /// Looks through `NonCounted`.
    pub fn is_non_empty_merk_tree(&self) -> bool {
        matches!(
            self.underlying(),
            Element::Tree(Some(_), _)
                | Element::SumTree(Some(_), ..)
                | Element::BigSumTree(Some(_), ..)
                | Element::CountTree(Some(_), ..)
                | Element::CountSumTree(Some(_), ..)
                | Element::ProvableCountTree(Some(_), ..)
                | Element::ProvableCountSumTree(Some(_), ..)
                | Element::ProvableSumTree(Some(_), ..)
                | Element::CountIndexedTree(Some(_), ..)
                | Element::CountIndexedTree(_, Some(_), ..)
                | Element::ProvableCountIndexedTree(Some(_), ..)
                | Element::ProvableCountIndexedTree(_, Some(_), ..)
        )
    }

    /// Check if the element is a reference. Looks through `NonCounted`. Both
    /// `Reference` and `ReferenceWithSumItem` are references — they share
    /// the resolution path and combined-value-hash proof shape; the only
    /// difference is that `ReferenceWithSumItem` carries an additional
    /// `SumValue` that propagates into sum-bearing parents.
    pub fn is_reference(&self) -> bool {
        matches!(
            self.underlying(),
            Element::Reference(..) | Element::ReferenceWithSumItem(..)
        )
    }

    /// Check if the element is specifically a `ReferenceWithSumItem`. Looks
    /// through `NonCounted`. Use `is_reference` when you only care that the
    /// element is some kind of reference; use this when you need to
    /// distinguish the sum-bearing variant.
    pub fn is_reference_with_sum_item(&self) -> bool {
        matches!(self.underlying(), Element::ReferenceWithSumItem(..))
    }

    /// Check if the element is an item. Looks through `NonCounted`.
    pub fn is_any_item(&self) -> bool {
        matches!(
            self.underlying(),
            Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..)
        )
    }

    /// Check if the element is a basic item. Looks through `NonCounted`.
    pub fn is_basic_item(&self) -> bool {
        matches!(self.underlying(), Element::Item(..))
    }

    /// Check if the element has a basic item value (Item or ItemWithSumItem).
    /// Looks through `NonCounted`.
    pub fn has_basic_item(&self) -> bool {
        matches!(
            self.underlying(),
            Element::Item(..) | Element::ItemWithSumItem(..)
        )
    }

    /// Check if the element is a sum item. Looks through `NonCounted`.
    pub fn is_sum_item(&self) -> bool {
        matches!(
            self.underlying(),
            Element::SumItem(..) | Element::ItemWithSumItem(..)
        )
    }

    /// Check if the element is an item-with-sum-item. Looks through
    /// `NonCounted`.
    pub fn is_item_with_sum_item(&self) -> bool {
        matches!(self.underlying(), Element::ItemWithSumItem(..))
    }

    /// Grab the optional flag stored in an element. For `NonCounted`, returns
    /// the inner element's flags.
    pub fn get_flags(&self) -> &Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags)
            | Element::ProvableCountSumTree(.., flags)
            | Element::ProvableSumTree(.., flags)
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags)
            | Element::CountIndexedTree(.., flags)
            | Element::ProvableCountIndexedTree(.., flags)
            | Element::ReferenceWithSumItem(.., flags) => flags,
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.get_flags(),
        }
    }

    /// Grab the optional flag stored in an element. For `NonCounted`, returns
    /// the inner element's flags.
    pub fn get_flags_owned(self) -> Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags)
            | Element::ProvableCountSumTree(.., flags)
            | Element::ProvableSumTree(.., flags)
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags)
            | Element::CountIndexedTree(.., flags)
            | Element::ProvableCountIndexedTree(.., flags)
            | Element::ReferenceWithSumItem(.., flags) => flags,
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.get_flags_owned(),
        }
    }

    /// Grab the optional flag stored in an element as mutable. For
    /// `NonCounted`, returns a mutable reference to the inner element's flags.
    pub fn get_flags_mut(&mut self) -> &mut Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags)
            | Element::ProvableCountSumTree(.., flags)
            | Element::ProvableSumTree(.., flags)
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags)
            | Element::CountIndexedTree(.., flags)
            | Element::ProvableCountIndexedTree(.., flags)
            | Element::ReferenceWithSumItem(.., flags) => flags,
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.get_flags_mut(),
        }
    }

    /// Sets the optional flag stored in an element. For `NonCounted`, sets
    /// the inner element's flags.
    pub fn set_flags(&mut self, new_flags: Option<ElementFlags>) {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags)
            | Element::ProvableCountSumTree(.., flags)
            | Element::ProvableSumTree(.., flags)
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags)
            | Element::CountIndexedTree(.., flags)
            | Element::ProvableCountIndexedTree(.., flags)
            | Element::ReferenceWithSumItem(.., flags) => *flags = new_flags,
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.set_flags(new_flags),
        }
    }

    /// Get the required item space
    pub fn required_item_space(
        len: u32,
        flag_len: u32,
        grove_version: &GroveVersion,
    ) -> Result<u32, ElementError> {
        check_grovedb_v0!(
            "required_item_space",
            grove_version.grovedb_versions.element.required_item_space
        );
        Ok(len + len.required_space() as u32 + flag_len + flag_len.required_space() as u32 + 1)
    }

    /// Convert the reference to an absolute reference. Looks through a
    /// `NonCounted` wrapper, converting the inner reference and re-wrapping.
    pub fn convert_if_reference_to_absolute_reference(
        self,
        path: &[&[u8]],
        key: Option<&[u8]>,
    ) -> Result<Element, ElementError> {
        // Convert any non-absolute reference type to an absolute one
        // we do this here because references are aggregated first then followed later
        // to follow non-absolute references, we need the path they are stored at
        // this information is lost during the aggregation phase.
        Ok(match self {
            Element::Reference(ref reference_path_type, max_hop, ref flags) => {
                match reference_path_type {
                    ReferencePathType::AbsolutePathReference(..) => self,
                    _ => {
                        // Element is a reference and is not absolute.
                        // build the stored path for this reference
                        let absolute_path =
                            path_from_reference_path_type(reference_path_type.clone(), path, key)?;
                        // return an absolute reference that contains this info
                        Element::Reference(
                            ReferencePathType::AbsolutePathReference(absolute_path),
                            max_hop,
                            flags.clone(),
                        )
                    }
                }
            }
            Element::ReferenceWithSumItem(
                ref reference_path_type,
                max_hop,
                sum_value,
                ref flags,
            ) => {
                match reference_path_type {
                    ReferencePathType::AbsolutePathReference(..) => self,
                    _ => {
                        // Mirror the Reference arm: rebuild as absolute,
                        // preserving the sum value.
                        let absolute_path =
                            path_from_reference_path_type(reference_path_type.clone(), path, key)?;
                        Element::ReferenceWithSumItem(
                            ReferencePathType::AbsolutePathReference(absolute_path),
                            max_hop,
                            sum_value,
                            flags.clone(),
                        )
                    }
                }
            }
            Element::NonCounted(inner) => Element::NonCounted(Box::new(
                inner.convert_if_reference_to_absolute_reference(path, key)?,
            )),
            other => other,
        })
    }
}

#[cfg(test)]
mod non_counted_tests {
    use grovedb_version::version::GroveVersion;

    use crate::element::Element;

    #[test]
    fn new_non_counted_wraps_basic_item() {
        let inner = Element::Item(b"x".to_vec(), None);
        let wrapped = Element::new_non_counted(inner.clone()).expect("wrap ok");
        assert!(wrapped.is_non_counted());
        assert_eq!(wrapped.underlying(), &inner);
    }

    #[test]
    fn new_non_counted_rejects_nested_wrapper() {
        let inner = Element::Item(b"x".to_vec(), None);
        let once = Element::new_non_counted(inner).expect("first wrap ok");
        assert!(Element::new_non_counted(once).is_err());
    }

    #[test]
    fn into_non_counted_is_idempotent() {
        let inner = Element::Item(b"x".to_vec(), None);
        let once = inner.clone().into_non_counted().expect("wrap ok");
        let twice = once.clone().into_non_counted().expect("rewrap ok");
        assert_eq!(once, twice);
        assert!(twice.is_non_counted());
    }

    #[test]
    fn into_non_counted_rejects_not_summed() {
        // The two wrappers are mutually exclusive — wrapping NotSummed in
        // NonCounted must fail rather than silently succeed (which would
        // produce a doubly-wrapped element that the (de)serializer rejects
        // anyway, but earlier in the pipeline).
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        assert!(ns.into_non_counted().is_err());
    }

    #[test]
    fn new_non_counted_rejects_not_summed() {
        // Symmetric to `new_not_summed_rejects_non_counted` — `new_non_counted`
        // must reject any pre-existing wrapper inner, including NotSummed.
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        assert!(Element::new_non_counted(ns).is_err());
    }

    #[test]
    fn predicates_look_through_wrapper() {
        let tree = Element::new_tree(None);
        let nc_tree = Element::new_non_counted(tree).expect("wrap ok");
        assert!(nc_tree.is_any_tree());
        assert!(nc_tree.is_basic_tree());

        let sum_item = Element::new_sum_item(7);
        let nc_sum = Element::new_non_counted(sum_item).expect("wrap ok");
        assert!(nc_sum.is_sum_item());
        assert!(nc_sum.is_any_item());
        assert!(!nc_sum.is_any_tree());
        // sum still propagates
        assert_eq!(nc_sum.sum_value_or_default(), 7);
    }

    #[test]
    fn count_value_or_default_is_zero_for_non_counted() {
        // Bare ProvableCountTree with internal count 5 contributes 5.
        let pct = Element::new_provable_count_tree_with_flags_and_count_value(None, 5, None);
        assert_eq!(pct.count_value_or_default(), 5);

        // NonCounted wrapper suppresses that to 0.
        let nc_pct = Element::new_non_counted(pct).expect("wrap ok");
        assert_eq!(nc_pct.count_value_or_default(), 0);

        // Non-tree elements normally count as 1; wrapped they count as 0.
        let item = Element::new_item(b"x".to_vec());
        assert_eq!(item.count_value_or_default(), 1);
        let nc_item = Element::new_non_counted(item).expect("wrap ok");
        assert_eq!(nc_item.count_value_or_default(), 0);
    }

    #[test]
    fn count_sum_value_or_default_zeros_count_keeps_sum() {
        let sum_item = Element::new_sum_item(42);
        assert_eq!(sum_item.count_sum_value_or_default(), (1, 42));

        let nc_sum = Element::new_non_counted(sum_item).expect("wrap ok");
        assert_eq!(nc_sum.count_sum_value_or_default(), (0, 42));
    }

    #[test]
    fn flags_delegate_through_wrapper() {
        let flags = Some(vec![1, 2, 3]);
        let item = Element::new_item_with_flags(b"x".to_vec(), flags.clone());
        let nc_item = Element::new_non_counted(item).expect("wrap ok");
        assert_eq!(nc_item.get_flags(), &flags);
    }

    #[test]
    fn bincode_round_trip_through_wrapper() {
        let grove_version = GroveVersion::latest();
        let inner = Element::SumItem(7, Some(vec![9, 8]));
        let wrapped = Element::new_non_counted(inner).expect("wrap ok");
        let bytes = wrapped.serialize(grove_version).expect("serialize ok");
        let back = Element::deserialize(&bytes, grove_version).expect("deserialize ok");
        assert_eq!(back, wrapped);
    }

    #[test]
    fn deserialize_rejects_nested_non_counted() {
        // Construct nested NonCounted manually, bypassing the constructor's check.
        let inner = Element::NonCounted(Box::new(Element::Item(b"x".to_vec(), None)));
        let bad = Element::NonCounted(Box::new(inner));
        // serialize() also rejects, but for the test we want to verify the
        // *deserialize* path. Use bincode directly to bypass our serialize()
        // safety check.
        use bincode::config;
        let cfg = config::standard().with_big_endian().with_no_limit();
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("bincode encode ok");
        let grove_version = GroveVersion::latest();
        assert!(Element::deserialize(&bytes, grove_version).is_err());
    }

    /// The pre-check before bincode decode is the actual stack-overflow
    /// guard: a long chain of wrapper bytes is rejected without bincode
    /// recursing through them. This synthesizes the malicious payload
    /// directly (no construction goes through `serialize`).
    #[test]
    fn deserialize_rejects_long_nested_wrapper_chain_without_recursion() {
        let grove_version = GroveVersion::latest();
        // 1024 wrapper bytes followed by a base item. With the post-check
        // alone, bincode would recurse through all 1024 Box<Element>
        // wrappers before our check fires — pre-check stops it on byte 1.
        let mut bytes = vec![15u8; 1024];
        bytes.extend_from_slice(&[0, 0, 0]); // Item with empty value, no flags
        let err = Element::deserialize(&bytes, grove_version)
            .expect_err("nested wrapper bytes must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("NonCounted") || msg.contains("non_counted") || msg.contains("wrapper"),
            "error should mention nested wrapper: {}",
            msg
        );
    }

    #[test]
    fn serialize_rejects_nested_non_counted() {
        let inner = Element::NonCounted(Box::new(Element::Item(b"x".to_vec(), None)));
        let bad = Element::NonCounted(Box::new(inner));
        let grove_version = GroveVersion::latest();
        assert!(bad.serialize(grove_version).is_err());
    }
}

#[cfg(test)]
mod not_summed_tests {
    use grovedb_version::version::GroveVersion;

    use crate::element::Element;

    #[test]
    fn new_not_summed_wraps_sum_tree_variants() {
        // The four sum-tree variants are accepted.
        for inner in [
            Element::new_sum_tree(None),
            Element::new_big_sum_tree(None),
            Element::new_count_sum_tree(None),
            Element::new_provable_count_sum_tree(None),
        ] {
            let wrapped = Element::new_not_summed(inner.clone()).expect("wrap ok");
            assert!(wrapped.is_not_summed());
            assert_eq!(wrapped.underlying(), &inner);
        }
    }

    #[test]
    fn new_not_summed_rejects_non_sum_tree_inner() {
        // Items, sum items, references, and non-sum trees are all rejected.
        assert!(Element::new_not_summed(Element::new_item(b"x".to_vec())).is_err());
        assert!(Element::new_not_summed(Element::new_sum_item(7)).is_err());
        assert!(Element::new_not_summed(Element::new_tree(None)).is_err());
        assert!(Element::new_not_summed(Element::new_count_tree(None)).is_err());
        assert!(Element::new_not_summed(Element::new_provable_count_tree(None)).is_err());
        // Wrappers cannot nest in either direction.
        let nc = Element::new_non_counted(Element::new_sum_tree(None)).expect("nc ok");
        assert!(Element::new_not_summed(nc).is_err());
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("ns ok");
        assert!(Element::new_not_summed(ns).is_err());
    }

    #[test]
    fn predicates_look_through_wrapper() {
        let st = Element::new_sum_tree_with_flags_and_sum_value(Some(b"r".to_vec()), 100, None);
        let ns = Element::new_not_summed(st).expect("wrap ok");
        // Tree predicates pass through.
        assert!(ns.is_any_tree());
        assert!(ns.is_sum_tree());
        assert!(ns.is_non_empty_tree());
        assert!(!ns.is_any_item());
        assert!(!ns.is_basic_tree());
    }

    #[test]
    fn sum_value_or_default_is_zero_for_not_summed() {
        // Bare SumTree with internal sum 100 contributes 100.
        let st = Element::new_sum_tree_with_flags_and_sum_value(None, 100, None);
        assert_eq!(st.sum_value_or_default(), 100);

        // NotSummed wrapper suppresses that to 0.
        let ns = Element::new_not_summed(st).expect("wrap ok");
        assert_eq!(ns.sum_value_or_default(), 0);
        assert_eq!(ns.big_sum_value_or_default(), 0);
    }

    #[test]
    fn count_value_or_default_propagates_through_not_summed() {
        // A NotSummed-wrapped CountSumTree(_, 5, 100, _) still contributes
        // count = 5. Only the sum is suppressed.
        let cst =
            Element::new_count_sum_tree_with_flags_and_sum_and_count_value(None, 5, 100, None);
        assert_eq!(cst.count_value_or_default(), 5);
        assert_eq!(cst.sum_value_or_default(), 100);

        let ns = Element::new_not_summed(cst).expect("wrap ok");
        assert_eq!(ns.count_value_or_default(), 5);
        assert_eq!(ns.sum_value_or_default(), 0);
    }

    #[test]
    fn count_sum_value_or_default_zeros_sum_keeps_count() {
        let cst = Element::new_count_sum_tree_with_flags_and_sum_and_count_value(None, 3, 42, None);
        assert_eq!(cst.count_sum_value_or_default(), (3, 42));

        let ns = Element::new_not_summed(cst).expect("wrap ok");
        assert_eq!(ns.count_sum_value_or_default(), (3, 0));
    }

    #[test]
    fn flags_delegate_through_wrapper() {
        let flags = Some(vec![1, 2, 3]);
        let st = Element::new_sum_tree_with_flags(None, flags.clone());
        let ns = Element::new_not_summed(st).expect("wrap ok");
        assert_eq!(ns.get_flags(), &flags);
    }

    #[test]
    fn bincode_round_trip_through_wrapper() {
        let grove_version = GroveVersion::latest();
        let inner = Element::new_sum_tree_with_flags_and_sum_value(
            Some(b"root".to_vec()),
            42,
            Some(vec![9, 8]),
        );
        let wrapped = Element::new_not_summed(inner).expect("wrap ok");
        let bytes = wrapped.serialize(grove_version).expect("serialize ok");
        let back = Element::deserialize(&bytes, grove_version).expect("deserialize ok");
        assert_eq!(back, wrapped);
    }

    #[test]
    fn deserialize_rejects_nested_wrappers() {
        // Construct nested wrappers manually, bypassing the constructor.
        // serialize() rejects too, but use bincode directly to test the
        // *deserialize* path.
        use bincode::config;
        let cfg = config::standard().with_big_endian().with_no_limit();
        let grove_version = GroveVersion::latest();

        // NotSummed(NotSummed(SumTree)).
        let nested_nn = Element::NotSummed(Box::new(Element::NotSummed(Box::new(
            Element::SumTree(None, 0, None),
        ))));
        let bytes = bincode::encode_to_vec(&nested_nn, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());

        // NotSummed(NonCounted(SumTree)).
        let cross = Element::NotSummed(Box::new(Element::NonCounted(Box::new(Element::SumTree(
            None, 0, None,
        )))));
        let bytes = bincode::encode_to_vec(&cross, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());

        // NonCounted(NotSummed(SumTree)).
        let cross2 = Element::NonCounted(Box::new(Element::NotSummed(Box::new(Element::SumTree(
            None, 0, None,
        )))));
        let bytes = bincode::encode_to_vec(&cross2, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());
    }

    /// The pre-check before bincode decode is the actual stack-overflow
    /// guard: a long chain of wrapper bytes is rejected without bincode
    /// recursing through them. Mirrors the NonCounted long-chain test.
    #[test]
    fn deserialize_rejects_long_nested_wrapper_chain_without_recursion() {
        let grove_version = GroveVersion::latest();
        // 1024 wrapper bytes (alternating NotSummed/NotSummed) followed by a
        // base sum tree. With the post-check alone, bincode would recurse
        // through all 1024 Box<Element> wrappers — pre-check stops it on
        // byte 1.
        let mut bytes = vec![16u8; 1024];
        // Append a minimal valid SumTree (disc 4, no root key, varint sum 0,
        // no flags).
        bytes.extend_from_slice(&[4, 0, 0, 0]);
        let err = Element::deserialize(&bytes, grove_version)
            .expect_err("nested wrapper bytes must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("wrapper") || msg.contains("Wrapper"),
            "error should mention nested wrapper: {}",
            msg
        );
    }

    #[test]
    fn deserialize_rejects_not_summed_with_non_sum_tree_inner() {
        // A NotSummed wrapping a plain Item must be rejected at deserialize.
        use bincode::config;
        let cfg = config::standard().with_big_endian().with_no_limit();
        let bad = Element::NotSummed(Box::new(Element::Item(b"x".to_vec(), None)));
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("encode");
        let grove_version = GroveVersion::latest();
        assert!(Element::deserialize(&bytes, grove_version).is_err());
    }

    #[test]
    fn serialize_rejects_not_summed_with_non_sum_tree_inner() {
        let bad = Element::NotSummed(Box::new(Element::Item(b"x".to_vec(), None)));
        let grove_version = GroveVersion::latest();
        assert!(bad.serialize(grove_version).is_err());
    }
}

#[cfg(test)]
mod flag_accessor_tests {
    // Targets the per-variant match arms in `get_flags`, `get_flags_owned`,
    // `get_flags_mut`, and `set_flags`. Each aggregate-bearing variant
    // (CountSumTree, ProvableCountTree, ProvableCountSumTree, ProvableSumTree)
    // gets a direct round-trip exercise so the per-variant pattern lines
    // register as covered.
    use crate::element::Element;

    fn flags() -> Option<Vec<u8>> {
        Some(vec![0xCA, 0xFE])
    }

    fn flags_b() -> Option<Vec<u8>> {
        Some(vec![0xBE, 0xEF])
    }

    fn check_accessors_round_trip(initial: Element) {
        // get_flags_owned (covers the matched arm on the consuming path).
        let got_owned = initial.clone().get_flags_owned();
        assert_eq!(got_owned, flags());

        // get_flags_mut (covers the &mut arm).
        let mut mutable = initial.clone();
        let slot = mutable.get_flags_mut();
        assert_eq!(slot, &flags());
        *slot = flags_b();
        assert_eq!(mutable.get_flags(), &flags_b());

        // set_flags (covers the &mut arm of set_flags).
        let mut e = initial;
        e.set_flags(flags_b());
        assert_eq!(e.get_flags(), &flags_b());
    }

    #[test]
    fn count_sum_tree_flag_accessors() {
        let e = Element::CountSumTree(None, 0, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn provable_count_tree_flag_accessors() {
        let e = Element::ProvableCountTree(None, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn provable_count_sum_tree_flag_accessors() {
        let e = Element::ProvableCountSumTree(None, 0, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn provable_sum_tree_flag_accessors() {
        let e = Element::ProvableSumTree(None, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn item_with_sum_item_flag_accessors() {
        let e = Element::ItemWithSumItem(b"x".to_vec(), 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn commitment_tree_flag_accessors() {
        let e = Element::CommitmentTree(0, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn mmr_tree_flag_accessors() {
        let e = Element::MmrTree(0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn bulk_append_tree_flag_accessors() {
        let e = Element::BulkAppendTree(0, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn dense_append_only_tree_flag_accessors() {
        let e = Element::DenseAppendOnlyFixedSizeTree(0, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn big_sum_tree_flag_accessors() {
        let e = Element::BigSumTree(None, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn count_tree_flag_accessors() {
        let e = Element::CountTree(None, 0, flags());
        check_accessors_round_trip(e);
    }

    #[test]
    fn flag_accessors_delegate_through_not_summed() {
        // Drives the `Element::NotSummed(inner) => inner.set_flags(...)` arm
        // (and matching arms in the other two accessors).
        let inner = Element::new_sum_tree_with_flags(None, flags());
        let mut wrapper = Element::new_not_summed(inner).expect("wrap ok");

        assert_eq!(wrapper.clone().get_flags_owned(), flags());
        let slot = wrapper.get_flags_mut();
        assert_eq!(slot, &flags());
        *slot = flags_b();
        assert_eq!(wrapper.get_flags(), &flags_b());

        wrapper.set_flags(None);
        assert_eq!(wrapper.get_flags(), &None);
    }

    #[test]
    fn flag_accessors_delegate_through_non_counted() {
        // Drives the `Element::NonCounted(inner) => ...` arm in the same
        // three accessors. (Was uncovered when no test invoked set_flags via
        // a NonCounted wrapper.)
        let inner = Element::new_item_with_flags(b"x".to_vec(), flags());
        let mut wrapper = Element::new_non_counted(inner).expect("wrap ok");

        assert_eq!(wrapper.clone().get_flags_owned(), flags());
        let slot = wrapper.get_flags_mut();
        assert_eq!(slot, &flags());
        *slot = flags_b();
        assert_eq!(wrapper.get_flags(), &flags_b());

        wrapper.set_flags(None);
        assert_eq!(wrapper.get_flags(), &None);
    }
}

#[cfg(test)]
mod not_counted_or_summed_tests {
    use grovedb_version::version::GroveVersion;

    use crate::element::Element;

    #[test]
    fn new_not_counted_or_summed_wraps_sum_tree_variants() {
        // All five sum-bearing tree variants are accepted, including the
        // newer `ProvableSumTree` (whose own twin slot 0xC1 was hand-assigned
        // because base 19 doesn't fit the `0xC0 | base` formula).
        for inner in [
            Element::new_sum_tree(None),
            Element::new_big_sum_tree(None),
            Element::new_count_sum_tree(None),
            Element::new_provable_count_sum_tree(None),
            Element::new_provable_sum_tree(None),
        ] {
            let wrapped = Element::new_not_counted_or_summed(inner.clone()).expect("wrap ok");
            assert!(wrapped.is_not_counted_or_summed());
            assert!(wrapped.is_wrapped());
            assert_eq!(wrapped.underlying(), &inner);
        }
    }

    #[test]
    fn new_not_counted_or_summed_rejects_non_sum_tree_inner() {
        // Items, sum items, references, and non-sum trees are all rejected.
        assert!(Element::new_not_counted_or_summed(Element::new_item(b"x".to_vec())).is_err());
        assert!(Element::new_not_counted_or_summed(Element::new_sum_item(7)).is_err());
        assert!(Element::new_not_counted_or_summed(Element::new_tree(None)).is_err());
        assert!(Element::new_not_counted_or_summed(Element::new_count_tree(None)).is_err());
        assert!(
            Element::new_not_counted_or_summed(Element::new_provable_count_tree(None)).is_err()
        );

        // Wrappers cannot nest in either direction.
        let nc = Element::new_non_counted(Element::new_sum_tree(None)).expect("nc ok");
        assert!(Element::new_not_counted_or_summed(nc).is_err());
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("ns ok");
        assert!(Element::new_not_counted_or_summed(ns).is_err());
        let ncos =
            Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("ncos ok");
        assert!(Element::new_not_counted_or_summed(ncos).is_err());
    }

    #[test]
    fn into_not_counted_or_summed_is_idempotent_and_rejects_other_wrappers() {
        // Idempotent on NotCountedOrSummed.
        let ncos =
            Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let twice = ncos
            .clone()
            .into_not_counted_or_summed()
            .expect("rewrap ok");
        assert_eq!(ncos, twice);

        // Rejects NonCounted/NotSummed.
        let nc = Element::new_non_counted(Element::new_sum_tree(None)).expect("nc ok");
        assert!(nc.into_not_counted_or_summed().is_err());
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("ns ok");
        assert!(ns.into_not_counted_or_summed().is_err());

        // Rejects non-sum-tree inners (delegates to new_not_counted_or_summed).
        assert!(Element::new_item(b"x".to_vec())
            .into_not_counted_or_summed()
            .is_err());

        // Wraps bare sum tree.
        let wrapped = Element::new_sum_tree(None)
            .into_not_counted_or_summed()
            .expect("wrap ok");
        assert!(wrapped.is_not_counted_or_summed());
    }

    #[test]
    fn into_non_counted_and_into_not_summed_reject_not_counted_or_summed_inner() {
        // The cross-wrapper rejection arms in into_non_counted /
        // into_not_summed must fire for NotCountedOrSummed.
        let ncos =
            Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("ncos ok");
        assert!(ncos.clone().into_non_counted().is_err());
        assert!(ncos.into_not_summed().is_err());
    }

    #[test]
    fn predicates_look_through_wrapper() {
        let cst = Element::new_count_sum_tree_with_flags_and_sum_and_count_value(
            Some(b"r".to_vec()),
            7,
            100,
            None,
        );
        let ncos = Element::new_not_counted_or_summed(cst).expect("wrap ok");
        assert!(ncos.is_any_tree());
        assert!(ncos.is_non_empty_tree());
        assert!(ncos.is_non_empty_merk_tree());
        assert!(!ncos.is_any_item());
        assert!(!ncos.is_basic_tree());
        assert!(!ncos.is_sum_tree()); // inner is CountSumTree, not SumTree
    }

    #[test]
    fn sum_and_count_helpers_zero_for_not_counted_or_summed() {
        // Inner CountSumTree(_, 5, 100) — both axes suppressed when wrapped.
        let cst =
            Element::new_count_sum_tree_with_flags_and_sum_and_count_value(None, 5, 100, None);
        assert_eq!(cst.count_value_or_default(), 5);
        assert_eq!(cst.sum_value_or_default(), 100);
        assert_eq!(cst.big_sum_value_or_default(), 100);
        assert_eq!(cst.count_sum_value_or_default(), (5, 100));

        let ncos = Element::new_not_counted_or_summed(cst).expect("wrap ok");
        assert_eq!(ncos.count_value_or_default(), 0);
        assert_eq!(ncos.sum_value_or_default(), 0);
        assert_eq!(ncos.big_sum_value_or_default(), 0);
        assert_eq!(ncos.count_sum_value_or_default(), (0, 0));
    }

    #[test]
    fn implicit_count_one_suppressed_for_sum_tree_inner() {
        // A bare SumTree contributes count=1 to a count-bearing parent by
        // default. NotCountedOrSummed must suppress that to 0.
        let st = Element::new_sum_tree_with_flags_and_sum_value(None, 50, None);
        assert_eq!(st.count_value_or_default(), 1);
        assert_eq!(st.sum_value_or_default(), 50);

        let ncos = Element::new_not_counted_or_summed(st).expect("wrap ok");
        assert_eq!(ncos.count_value_or_default(), 0);
        assert_eq!(ncos.sum_value_or_default(), 0);
        assert_eq!(ncos.count_sum_value_or_default(), (0, 0));
    }

    #[test]
    fn flags_delegate_through_wrapper() {
        let flags = Some(vec![1, 2, 3]);
        let st = Element::new_sum_tree_with_flags(None, flags.clone());
        let mut ncos = Element::new_not_counted_or_summed(st).expect("wrap ok");
        assert_eq!(ncos.get_flags(), &flags);
        // get_flags_mut allows mutation through the wrapper.
        *ncos.get_flags_mut() = Some(vec![9]);
        assert_eq!(ncos.get_flags(), &Some(vec![9]));
        // set_flags routes through the wrapper.
        ncos.set_flags(Some(vec![1, 2]));
        assert_eq!(ncos.get_flags(), &Some(vec![1, 2]));
        // get_flags_owned consumes self.
        assert_eq!(ncos.get_flags_owned(), Some(vec![1, 2]));
    }

    #[test]
    fn underlying_methods_unwrap_one_level() {
        let inner = Element::SumTree(Some(b"r".to_vec()), 42, None);
        let ncos = Element::new_not_counted_or_summed(inner.clone()).expect("wrap ok");

        // Borrow.
        assert_eq!(ncos.underlying(), &inner);
        // Owned.
        assert_eq!(ncos.clone().into_underlying(), inner);
        // Mutable.
        let mut ncos_mut = ncos;
        let m = ncos_mut.underlying_mut();
        assert!(matches!(m, Element::SumTree(..)));
    }

    #[test]
    fn bincode_round_trip_through_wrapper() {
        let grove_version = GroveVersion::latest();
        let inner = Element::new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
            Some(b"root".to_vec()),
            3,
            42,
            Some(vec![9, 8]),
        );
        let wrapped = Element::new_not_counted_or_summed(inner).expect("wrap ok");
        let bytes = wrapped.serialize(grove_version).expect("serialize ok");
        let back = Element::deserialize(&bytes, grove_version).expect("deserialize ok");
        assert_eq!(back, wrapped);
    }

    #[test]
    fn deserialize_rejects_nested_or_cross_wrappers() {
        // serialize() also rejects, but bincode-encode directly to test the
        // *deserialize* pre-check and post-check paths.
        use bincode::config;
        let cfg = config::standard().with_big_endian().with_no_limit();
        let grove_version = GroveVersion::latest();

        // NotCountedOrSummed(NotCountedOrSummed(SumTree))
        let bad = Element::NotCountedOrSummed(Box::new(Element::NotCountedOrSummed(Box::new(
            Element::SumTree(None, 0, None),
        ))));
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());

        // NotCountedOrSummed(NonCounted(SumTree))
        let bad = Element::NotCountedOrSummed(Box::new(Element::NonCounted(Box::new(
            Element::SumTree(None, 0, None),
        ))));
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());

        // NotCountedOrSummed(NotSummed(SumTree))
        let bad = Element::NotCountedOrSummed(Box::new(Element::NotSummed(Box::new(
            Element::SumTree(None, 0, None),
        ))));
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());

        // NonCounted(NotCountedOrSummed(SumTree))
        let bad = Element::NonCounted(Box::new(Element::NotCountedOrSummed(Box::new(
            Element::SumTree(None, 0, None),
        ))));
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());

        // NotSummed(NotCountedOrSummed(SumTree))
        let bad = Element::NotSummed(Box::new(Element::NotCountedOrSummed(Box::new(
            Element::SumTree(None, 0, None),
        ))));
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("encode");
        assert!(Element::deserialize(&bytes, grove_version).is_err());
    }

    #[test]
    fn deserialize_rejects_not_counted_or_summed_with_non_sum_tree_inner() {
        // A NotCountedOrSummed wrapping a plain Item must be rejected at
        // deserialize, exercising the post-check arm.
        use bincode::config;
        let cfg = config::standard().with_big_endian().with_no_limit();
        let bad = Element::NotCountedOrSummed(Box::new(Element::Item(b"x".to_vec(), None)));
        let bytes = bincode::encode_to_vec(&bad, cfg).expect("encode");
        let grove_version = GroveVersion::latest();
        assert!(Element::deserialize(&bytes, grove_version).is_err());
    }

    #[test]
    fn serialize_rejects_not_counted_or_summed_with_non_sum_tree_inner() {
        let bad = Element::NotCountedOrSummed(Box::new(Element::Item(b"x".to_vec(), None)));
        let grove_version = GroveVersion::latest();
        assert!(bad.serialize(grove_version).is_err());
    }

    #[test]
    fn deserialize_rejects_long_nested_wrapper_chain_without_recursion() {
        let grove_version = GroveVersion::latest();
        // 1024 NotCountedOrSummed wrapper bytes (17 = NCOS wrapper
        // discriminant) followed by a valid SumTree. Pre-check stops on
        // byte 1.
        let mut bytes = vec![17u8; 1024];
        bytes.extend_from_slice(&[4, 0, 0, 0]);
        let err = Element::deserialize(&bytes, grove_version)
            .expect_err("nested wrapper bytes must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("wrapper") || msg.contains("Wrapper"),
            "error should mention wrapper: {}",
            msg
        );
    }
}
