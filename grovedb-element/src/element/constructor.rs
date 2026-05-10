//! Constructor
//! Functions for setting an element's type

use crate::{
    element::{BigSumValue, CountValue, Element, ElementFlags, MaxReferenceHop, SumValue},
    error::ElementError,
    reference_path::ReferencePathType,
};

impl Element {
    /// Set element to default empty tree without flags
    pub fn empty_tree() -> Self {
        Element::new_tree(Default::default())
    }

    /// Set element to default empty tree with flags
    pub fn empty_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_tree_with_flags(Default::default(), flags)
    }

    /// Set element to default empty sum tree without flags
    pub fn empty_sum_tree() -> Self {
        Element::new_sum_tree(Default::default())
    }

    /// Set element to default empty big sum tree without flags
    pub fn empty_big_sum_tree() -> Self {
        Element::new_big_sum_tree(Default::default())
    }

    /// Set element to default empty count tree without flags
    pub fn empty_count_tree() -> Self {
        Element::new_count_tree(Default::default())
    }

    /// Set element to default empty count sum tree without flags
    pub fn empty_count_sum_tree() -> Self {
        Element::new_count_sum_tree(Default::default())
    }

    /// Set element to default empty sum tree with flags
    pub fn empty_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_sum_tree_with_flags(Default::default(), flags)
    }

    /// Set element to default empty sum tree with flags
    pub fn empty_big_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_big_sum_tree_with_flags(Default::default(), flags)
    }

    /// Set element to default empty count tree with flags
    pub fn empty_count_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_count_tree_with_flags(Default::default(), flags)
    }

    /// Set element to default empty count sum tree with flags
    pub fn empty_count_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_count_sum_tree_with_flags(Default::default(), flags)
    }

    /// Set element to an item without flags
    pub fn new_item(item_value: Vec<u8>) -> Self {
        Element::Item(item_value, None)
    }

    /// Set element to an item with flags
    pub fn new_item_with_flags(item_value: Vec<u8>, flags: Option<ElementFlags>) -> Self {
        Element::Item(item_value, flags)
    }

    /// Set element to a sum item without flags
    pub fn new_sum_item(value: i64) -> Self {
        Element::SumItem(value, None)
    }

    /// Set element to a sum item with flags
    pub fn new_sum_item_with_flags(value: i64, flags: Option<ElementFlags>) -> Self {
        Element::SumItem(value, flags)
    }

    /// Set element to an item with sum value (no flags)
    pub fn new_item_with_sum_item(item_value: Vec<u8>, sum_value: SumValue) -> Self {
        Element::ItemWithSumItem(item_value, sum_value, None)
    }

    /// Set element to an item with sum value and flags
    pub fn new_item_with_sum_item_with_flags(
        item_value: Vec<u8>,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ItemWithSumItem(item_value, sum_value, flags)
    }

    /// Set element to a reference without flags
    pub fn new_reference(reference_path: ReferencePathType) -> Self {
        Element::Reference(reference_path, None, None)
    }

    /// Set element to a reference with flags
    pub fn new_reference_with_flags(
        reference_path: ReferencePathType,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Reference(reference_path, None, flags)
    }

    /// Set element to a reference with hops, no flags
    pub fn new_reference_with_hops(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
    ) -> Self {
        Element::Reference(reference_path, max_reference_hop, None)
    }

    /// Set element to a reference with max hops and flags
    pub fn new_reference_with_max_hops_and_flags(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Reference(reference_path, max_reference_hop, flags)
    }

    /// Set element to a tree without flags
    pub fn new_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::Tree(maybe_root_key, None)
    }

    /// Set element to a tree with flags
    pub fn new_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Tree(maybe_root_key, flags)
    }

    /// Set element to a sum tree without flags
    pub fn new_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::SumTree(maybe_root_key, 0, None)
    }

    /// Set element to a sum tree with flags
    pub fn new_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::SumTree(maybe_root_key, 0, flags)
    }

    /// Set element to a sum tree with flags and sum value
    pub fn new_sum_tree_with_flags_and_sum_value(
        maybe_root_key: Option<Vec<u8>>,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::SumTree(maybe_root_key, sum_value, flags)
    }

    /// Set element to a big sum tree without flags
    pub fn new_big_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::BigSumTree(maybe_root_key, 0, None)
    }

    /// Set element to a big sum tree with flags
    pub fn new_big_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::BigSumTree(maybe_root_key, 0, flags)
    }

    /// Set element to a big sum tree with flags and sum value
    pub fn new_big_sum_tree_with_flags_and_sum_value(
        maybe_root_key: Option<Vec<u8>>,
        big_sum_value: BigSumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::BigSumTree(maybe_root_key, big_sum_value, flags)
    }

    /// Set element to a count tree without flags
    pub fn new_count_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::CountTree(maybe_root_key, 0, None)
    }

    /// Set element to a count tree with flags
    pub fn new_count_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::CountTree(maybe_root_key, 0, flags)
    }

    /// Set element to a count tree with flags and sum value
    pub fn new_count_tree_with_flags_and_count_value(
        maybe_root_key: Option<Vec<u8>>,
        count_value: CountValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::CountTree(maybe_root_key, count_value, flags)
    }

    /// Set element to a count sum tree without flags
    pub fn new_count_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::CountSumTree(maybe_root_key, 0, 0, None)
    }

    /// Set element to a count sum tree with flags
    pub fn new_count_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::CountSumTree(maybe_root_key, 0, 0, flags)
    }

    /// Set element to a count sum tree with flags and sum value
    pub fn new_count_sum_tree_with_flags_and_sum_and_count_value(
        maybe_root_key: Option<Vec<u8>>,
        count_value: CountValue,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::CountSumTree(maybe_root_key, count_value, sum_value, flags)
    }

    /// Set element to default empty provable count tree without flags
    pub fn empty_provable_count_tree() -> Self {
        Element::new_provable_count_tree(Default::default())
    }

    /// Set element to default empty provable count tree with flags
    pub fn empty_provable_count_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_provable_count_tree_with_flags(Default::default(), flags)
    }

    /// Set element to a provable count tree without flags
    pub fn new_provable_count_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::ProvableCountTree(maybe_root_key, 0, None)
    }

    /// Set element to a provable count tree with flags
    pub fn new_provable_count_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableCountTree(maybe_root_key, 0, flags)
    }

    /// Set element to a provable count tree with flags and count value
    pub fn new_provable_count_tree_with_flags_and_count_value(
        maybe_root_key: Option<Vec<u8>>,
        count_value: CountValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableCountTree(maybe_root_key, count_value, flags)
    }

    /// Set element to default empty provable count sum tree without flags
    pub fn empty_provable_count_sum_tree() -> Self {
        Element::new_provable_count_sum_tree(Default::default())
    }

    /// Set element to default empty provable count sum tree with flags
    pub fn empty_provable_count_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_provable_count_sum_tree_with_flags(Default::default(), flags)
    }

    /// Set element to a provable count sum tree without flags
    pub fn new_provable_count_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::ProvableCountSumTree(maybe_root_key, 0, 0, None)
    }

    /// Set element to a provable count sum tree with flags
    pub fn new_provable_count_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableCountSumTree(maybe_root_key, 0, 0, flags)
    }

    /// Set element to a provable count sum tree with flags, count, and sum
    /// value
    pub fn new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
        maybe_root_key: Option<Vec<u8>>,
        count_value: CountValue,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableCountSumTree(maybe_root_key, count_value, sum_value, flags)
    }

    /// Set element to an empty commitment tree.
    ///
    /// Returns `InvalidInput` if `chunk_power > 31`.
    pub fn empty_commitment_tree(chunk_power: u8) -> Result<Self, ElementError> {
        if chunk_power > 31 {
            return Err(ElementError::InvalidInput("chunk_power must be <= 31"));
        }
        Ok(Element::CommitmentTree(0, chunk_power, None))
    }

    /// Set element to an empty commitment tree with flags.
    ///
    /// Returns `InvalidInput` if `chunk_power > 31`.
    pub fn empty_commitment_tree_with_flags(
        chunk_power: u8,
        flags: Option<ElementFlags>,
    ) -> Result<Self, ElementError> {
        if chunk_power > 31 {
            return Err(ElementError::InvalidInput("chunk_power must be <= 31"));
        }
        Ok(Element::CommitmentTree(0, chunk_power, flags))
    }

    /// Set element to a commitment tree with all fields
    pub fn new_commitment_tree(
        total_count: u64,
        chunk_power: u8,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::CommitmentTree(total_count, chunk_power, flags)
    }

    /// Set element to an empty MMR tree
    pub fn empty_mmr_tree() -> Self {
        Element::MmrTree(0, None)
    }

    /// Set element to an empty MMR tree with flags
    pub fn empty_mmr_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::MmrTree(0, flags)
    }

    /// Set element to an MMR tree with the given size
    pub fn new_mmr_tree(mmr_size: u64, flags: Option<ElementFlags>) -> Self {
        Element::MmrTree(mmr_size, flags)
    }

    /// Set element to an empty bulk append tree without flags.
    ///
    /// Returns `InvalidInput` if `chunk_power > 31`.
    pub fn empty_bulk_append_tree(chunk_power: u8) -> Result<Self, ElementError> {
        if chunk_power > 31 {
            return Err(ElementError::InvalidInput("chunk_power must be <= 31"));
        }
        Ok(Element::BulkAppendTree(0, chunk_power, None))
    }

    /// Set element to an empty bulk append tree with flags.
    ///
    /// Returns `InvalidInput` if `chunk_power > 31`.
    pub fn empty_bulk_append_tree_with_flags(
        chunk_power: u8,
        flags: Option<ElementFlags>,
    ) -> Result<Self, ElementError> {
        if chunk_power > 31 {
            return Err(ElementError::InvalidInput("chunk_power must be <= 31"));
        }
        Ok(Element::BulkAppendTree(0, chunk_power, flags))
    }

    /// Set element to a bulk append tree with all fields
    pub fn new_bulk_append_tree(
        total_count: u64,
        chunk_power: u8,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::BulkAppendTree(total_count, chunk_power, flags)
    }

    /// Set element to an empty dense tree without flags
    pub fn empty_dense_tree(height: u8) -> Self {
        Element::DenseAppendOnlyFixedSizeTree(0, height, None)
    }

    /// Set element to an empty dense tree with flags
    pub fn empty_dense_tree_with_flags(height: u8, flags: Option<ElementFlags>) -> Self {
        Element::DenseAppendOnlyFixedSizeTree(0, height, flags)
    }

    /// Set element to a dense tree with all fields
    pub fn new_dense_tree(count: u16, height: u8, flags: Option<ElementFlags>) -> Self {
        Element::DenseAppendOnlyFixedSizeTree(count, height, flags)
    }

    /// Wrap an element in `NonCounted` so it contributes 0 to its parent count
    /// tree's aggregate count when inserted. Sums (if any) still propagate.
    ///
    /// Returns `InvalidInput` if `inner` is already wrapped in any wrapper
    /// variant (`NonCounted` or `NotSummed`) — the wrappers are mutually
    /// exclusive and may not nest in either direction. Use
    /// `into_non_counted` to wrap idempotently when `inner` may already be
    /// `NonCounted`; use that helper's `Result` return for the
    /// cross-wrapper case.
    pub fn new_non_counted(inner: Element) -> Result<Self, ElementError> {
        if matches!(inner, Element::NonCounted(_) | Element::NotSummed(_)) {
            return Err(ElementError::InvalidInput(
                "NonCounted cannot wrap another wrapper",
            ));
        }
        Ok(Element::NonCounted(Box::new(inner)))
    }

    /// Wrap `self` in `NonCounted`. If `self` is already `NonCounted`,
    /// returns it unchanged (idempotent on `NonCounted`).
    ///
    /// Returns `InvalidInput` if `self` is `NotSummed` — the two wrappers
    /// are mutually exclusive. Callers that need the unconditional wrapping
    /// path should ensure the input is a non-wrapper variant before calling.
    pub fn into_non_counted(self) -> Result<Self, ElementError> {
        match self {
            Element::NonCounted(_) => Ok(self),
            Element::NotSummed(_) => Err(ElementError::InvalidInput(
                "cannot wrap NotSummed in NonCounted; wrappers are mutually exclusive",
            )),
            other => Ok(Element::NonCounted(Box::new(other))),
        }
    }

    /// Wrap a sum-tree variant in `NotSummed` so it contributes 0 to its
    /// parent sum tree's running sum when inserted. Counts (if any) still
    /// propagate.
    ///
    /// Only the four sum-tree variants are accepted: `SumTree`, `BigSumTree`,
    /// `CountSumTree`, `ProvableCountSumTree`. Any other element — including
    /// items, sum items, references, non-sum trees, and any wrapper
    /// (`NonCounted`, `NotSummed`) — is rejected with `InvalidInput`.
    pub fn new_not_summed(inner: Element) -> Result<Self, ElementError> {
        match inner {
            Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::CountSumTree(..)
            | Element::ProvableCountSumTree(..) => Ok(Element::NotSummed(Box::new(inner))),
            _ => Err(ElementError::InvalidInput(
                "NotSummed inner element must be a sum-tree variant (SumTree, BigSumTree, \
                 CountSumTree, or ProvableCountSumTree)",
            )),
        }
    }

    /// Wrap `self` in `NotSummed` without re-validating the inner element's
    /// type. Panics if `self` is not a sum-tree variant. Used by batch
    /// propagation paths that have already established the element will be
    /// a sum-tree type.
    pub fn into_not_summed_unchecked(self) -> Self {
        match Self::new_not_summed(self) {
            Ok(wrapped) => wrapped,
            Err(_) => panic!(
                "into_not_summed_unchecked called on non-sum-tree element — caller must ensure \
                 the element is one of SumTree/BigSumTree/CountSumTree/ProvableCountSumTree"
            ),
        }
    }
}
