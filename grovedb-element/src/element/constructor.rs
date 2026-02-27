//! Constructor
//! Functions for setting an element's type

use crate::{
    element::{BigSumValue, CountValue, Element, ElementFlags, MaxReferenceHop, SumValue},
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

    /// Set element to an empty commitment tree
    pub fn empty_commitment_tree(chunk_power: u8) -> Self {
        assert!(chunk_power <= 31, "chunk_power must be <= 31");
        Element::CommitmentTree(0, chunk_power, None)
    }

    /// Set element to an empty commitment tree with flags
    pub fn empty_commitment_tree_with_flags(chunk_power: u8, flags: Option<ElementFlags>) -> Self {
        assert!(chunk_power <= 31, "chunk_power must be <= 31");
        Element::CommitmentTree(0, chunk_power, flags)
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

    /// Set element to an empty bulk append tree without flags
    pub fn empty_bulk_append_tree(chunk_power: u8) -> Self {
        assert!(chunk_power <= 31, "chunk_power must be <= 31");
        Element::BulkAppendTree(0, chunk_power, None)
    }

    /// Set element to an empty bulk append tree with flags
    pub fn empty_bulk_append_tree_with_flags(chunk_power: u8, flags: Option<ElementFlags>) -> Self {
        assert!(chunk_power <= 31, "chunk_power must be <= 31");
        Element::BulkAppendTree(0, chunk_power, flags)
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
}
