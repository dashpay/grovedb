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

    /// Set element to an item without flags that can be targeted by
    /// bidirectional references
    pub fn new_item_allowing_bidirectional_references(item_value: Vec<u8>) -> Self {
        Element::ItemWithBackwardsReferences(item_value, Vec::new(), None)
    }

    /// Set element to an item with flags that can be targeted by
    /// bidirectional references
    pub fn new_item_allowing_bidirectional_references_with_flags(
        item_value: Vec<u8>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ItemWithBackwardsReferences(item_value, Vec::new(), flags)
    }

    /// Set element to a sum item without flags that can be targeted by
    /// bidirectional references
    pub fn new_sum_item_allowing_bidirectional_references(value: i64) -> Self {
        Element::SumItemWithBackwardsReferences(value, Vec::new(), None)
    }

    /// Set element to a sum item with flags that can be targeted by
    /// bidirectional references
    pub fn new_sum_item_allowing_bidirectional_references_with_flags(
        value: i64,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::SumItemWithBackwardsReferences(value, Vec::new(), flags)
    }

    /// Set element to a bidirectional reference without flags. The
    /// `backward_references` list is bookkeeping maintained by insertion —
    /// anything supplied here is overwritten by the write path.
    pub fn new_bidirectional_reference(reference_path: ReferencePathType) -> Self {
        Element::BidirectionalReference(crate::bidirectional_reference::BidirectionalReference {
            forward_reference_path: reference_path,
            cascade_on_update: false,
            max_hop: None,
            backward_references: Vec::new(),
            flags: None,
        })
    }

    /// Set element to a bidirectional reference with every knob exposed. The
    /// `backward_references` list is bookkeeping maintained by insertion.
    pub fn new_bidirectional_reference_with_options(
        reference_path: ReferencePathType,
        max_hop: MaxReferenceHop,
        cascade_on_update: bool,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::BidirectionalReference(crate::bidirectional_reference::BidirectionalReference {
            forward_reference_path: reference_path,
            cascade_on_update,
            max_hop,
            backward_references: Vec::new(),
            flags,
        })
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

    /// Set element to a reference-with-sum-item without flags or max hops.
    ///
    /// `sum_value` is the explicit weight that propagates to a sum-bearing
    /// parent — independent of whatever the reference resolves to.
    pub fn new_reference_with_sum_item(
        reference_path: ReferencePathType,
        sum_value: SumValue,
    ) -> Self {
        Element::ReferenceWithSumItem(reference_path, None, sum_value, None)
    }

    /// Set element to a reference-with-sum-item with flags.
    pub fn new_reference_with_sum_item_with_flags(
        reference_path: ReferencePathType,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ReferenceWithSumItem(reference_path, None, sum_value, flags)
    }

    /// Set element to a reference-with-sum-item with max hops, no flags.
    pub fn new_reference_with_sum_item_with_hops(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        sum_value: SumValue,
    ) -> Self {
        Element::ReferenceWithSumItem(reference_path, max_reference_hop, sum_value, None)
    }

    /// Set element to a reference-with-sum-item with max hops and flags.
    pub fn new_reference_with_sum_item_with_max_hops_and_flags(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ReferenceWithSumItem(reference_path, max_reference_hop, sum_value, flags)
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

    /// Set element to default empty provable sum tree without flags.
    ///
    /// `ProvableSumTree` is the sum analogue of `ProvableCountTree`: it
    /// bakes the per-node sum into the node hash so that aggregate-sum
    /// range queries can be cryptographically verified.
    pub fn empty_provable_sum_tree() -> Self {
        Element::new_provable_sum_tree(Default::default())
    }

    /// Set element to default empty provable sum tree with flags.
    pub fn empty_provable_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_provable_sum_tree_with_flags(Default::default(), flags)
    }

    /// Set element to a provable sum tree without flags.
    pub fn new_provable_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::ProvableSumTree(maybe_root_key, 0, None)
    }

    /// Set element to a provable sum tree with flags.
    pub fn new_provable_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableSumTree(maybe_root_key, 0, flags)
    }

    /// Set element to a provable sum tree with flags and sum value.
    pub fn new_provable_sum_tree_with_flags_and_sum_value(
        maybe_root_key: Option<Vec<u8>>,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableSumTree(maybe_root_key, sum_value, flags)
    }

    /// Set element to default empty provable count provable sum tree without
    /// flags.
    ///
    /// `ProvableCountProvableSumTree` bakes BOTH the per-node count AND the
    /// per-node sum into the node hash, enabling both
    /// `AggregateCountOnRange` and `AggregateSumOnRange` proofs against the
    /// same tree.
    pub fn empty_provable_count_provable_sum_tree() -> Self {
        Element::new_provable_count_provable_sum_tree(Default::default())
    }

    /// Set element to default empty provable count provable sum tree with
    /// flags.
    pub fn empty_provable_count_provable_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_provable_count_provable_sum_tree_with_flags(Default::default(), flags)
    }

    /// Set element to a provable count provable sum tree without flags.
    pub fn new_provable_count_provable_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::ProvableCountProvableSumTree(maybe_root_key, 0, 0, None)
    }

    /// Set element to a provable count provable sum tree with flags.
    pub fn new_provable_count_provable_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableCountProvableSumTree(maybe_root_key, 0, 0, flags)
    }

    /// Set element to a provable count provable sum tree with flags, count,
    /// and sum value.
    pub fn new_provable_count_provable_sum_tree_with_flags_and_sum_and_count_value(
        maybe_root_key: Option<Vec<u8>>,
        count_value: CountValue,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableCountProvableSumTree(maybe_root_key, count_value, sum_value, flags)
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

    /// Set element to an empty private document store.
    ///
    /// Returns `InvalidInput` unless `entry_size` is in `1..=65535` and
    /// `chunk_power` is in `1..=16` (the underlying `BulkAppendTree` dense-buffer height
    /// range). Unlike `empty_commitment_tree` / `empty_bulk_append_tree`,
    /// the constraints are enforced eagerly here: the configuration is
    /// committed into the state root, so an unusable config must never be
    /// constructible.
    pub fn empty_private_document_store(
        entry_size: u32,
        chunk_power: u8,
    ) -> Result<Self, ElementError> {
        Self::empty_private_document_store_with_flags(entry_size, chunk_power, None)
    }

    /// Set element to an empty private document store with flags.
    ///
    /// Same validation as [`Element::empty_private_document_store`].
    pub fn empty_private_document_store_with_flags(
        entry_size: u32,
        chunk_power: u8,
        flags: Option<ElementFlags>,
    ) -> Result<Self, ElementError> {
        if entry_size == 0 || entry_size > u16::MAX as u32 {
            // Upper bound keeps `2^16 * entry_size` (the worst-case
            // compaction blob) inside the u32 `added_bytes` field, so the
            // worst-case storage estimate stays a real bound. An entry
            // larger than 64 KiB is outside this type's design envelope
            // anyway.
            return Err(ElementError::InvalidInput(
                "private document store entry_size must be in 1..=65535",
            ));
        }
        if !(1..=16).contains(&chunk_power) {
            return Err(ElementError::InvalidInput(
                "private document store chunk_power must be between 1 and 16",
            ));
        }
        Ok(Element::PrivateDocumentStore(
            0,
            entry_size,
            chunk_power,
            flags,
        ))
    }

    /// Set element to a private document store with all fields.
    ///
    /// Restoration constructor: unchecked, mirroring `new_commitment_tree` /
    /// `new_bulk_append_tree` — it rebuilds an element from already-validated
    /// state (stored bytes, batch metadata). Invalid configurations are
    /// rejected at every real ingress: the `empty_*` constructors, the direct
    /// and batch insert paths, and both (de)serialization codecs
    /// (`Element::serialize` / `Element::deserialize` / serde) via
    /// [`Element::validate_private_document_store_config`].
    pub fn new_private_document_store(
        total_count: u64,
        entry_size: u32,
        chunk_power: u8,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::PrivateDocumentStore(total_count, entry_size, chunk_power, flags)
    }

    /// Set element to an empty provable sum-indexed tree without flags.
    pub fn empty_provable_sum_indexed_tree() -> Self {
        Element::ProvableSumIndexedTree(None, None, 0, None)
    }

    /// Set element to an empty provable sum-indexed tree with flags.
    pub fn empty_provable_sum_indexed_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::ProvableSumIndexedTree(None, None, 0, flags)
    }

    /// Construct a provable sum-indexed tree with given primary/secondary
    /// root keys and aggregate sum.
    pub fn new_provable_sum_indexed_tree_with_root_keys_and_sum_value(
        primary_root_key: Option<Vec<u8>>,
        secondary_root_key: Option<Vec<u8>>,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableSumIndexedTree(primary_root_key, secondary_root_key, sum_value, flags)
    }

    /// Set element to an empty provable count-indexed tree without flags.
    pub fn empty_provable_count_indexed_tree() -> Self {
        Element::ProvableCountIndexedTree(None, None, 0, None)
    }

    /// Set element to an empty provable count-indexed tree with flags.
    pub fn empty_provable_count_indexed_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::ProvableCountIndexedTree(None, None, 0, flags)
    }

    /// Construct a provable count-indexed tree with given primary/secondary
    /// root keys and aggregate count.
    pub fn new_provable_count_indexed_tree_with_root_keys_and_count_value(
        primary_root_key: Option<Vec<u8>>,
        secondary_root_key: Option<Vec<u8>>,
        count_value: CountValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::ProvableCountIndexedTree(primary_root_key, secondary_root_key, count_value, flags)
    }

    /// Validate a `ProvableCountProvableSumIndexedTree` axes TLV: sorted by
    /// tag ascending, no duplicate tags, 1..=3 entries, every tag in
    /// `0..=2` (matching [`crate::indexed::IndexAxis`]).
    ///
    /// Public so write paths that accept a caller-supplied
    /// `Element::ProvableCountProvableSumIndexedTree` (e.g. direct/batch
    /// empty-tree creation, which would otherwise hash the axes via
    /// `axes_digest` without validating them) can enforce the same
    /// canonical-axes invariant the constructors do. The `Element` enum
    /// is `pub`, so callers can build a PCPSIT with invalid / duplicate /
    /// unsorted axes that the constructors would have rejected.
    pub fn validate_pcpsit_axes(axes: &[(u8, Option<Vec<u8>>)]) -> Result<(), ElementError> {
        if axes.is_empty() {
            return Err(ElementError::InvalidInput(
                "ProvableCountProvableSumIndexedTree axes must have at least one entry",
            ));
        }
        if axes.len() > 3 {
            return Err(ElementError::InvalidInput(
                "ProvableCountProvableSumIndexedTree axes must have at most three entries",
            ));
        }
        let mut prev: Option<u8> = None;
        for (tag, _) in axes {
            // Every tag must be a known axis (0..=2). Re-using the
            // canonical mapping in IndexAxis::try_from_tag.
            crate::indexed::IndexAxis::try_from_tag(*tag)?;
            match prev {
                Some(p) if *tag <= p => {
                    return Err(ElementError::InvalidInput(
                        "ProvableCountProvableSumIndexedTree axes must be sorted ascending by \
                         tag with no duplicates",
                    ));
                }
                _ => prev = Some(*tag),
            }
        }
        Ok(())
    }

    /// Set element to an empty provable count + provable sum indexed tree
    /// without flags. `axes` must be canonical (sorted by tag, no
    /// duplicates, 1..=3 entries).
    pub fn empty_provable_count_provable_sum_indexed_tree(
        axes: Vec<(u8, Option<Vec<u8>>)>,
    ) -> Result<Self, ElementError> {
        Self::validate_pcpsit_axes(&axes)?;
        Ok(Element::ProvableCountProvableSumIndexedTree(
            None, 0, 0, axes, None,
        ))
    }

    /// Set element to an empty provable count + provable sum indexed tree
    /// with flags. See [`empty_provable_count_provable_sum_indexed_tree`].
    pub fn empty_provable_count_provable_sum_indexed_tree_with_flags(
        axes: Vec<(u8, Option<Vec<u8>>)>,
        flags: Option<ElementFlags>,
    ) -> Result<Self, ElementError> {
        Self::validate_pcpsit_axes(&axes)?;
        Ok(Element::ProvableCountProvableSumIndexedTree(
            None, 0, 0, axes, flags,
        ))
    }

    /// Construct a provable count + provable sum indexed tree with given
    /// primary root key, aggregate count, aggregate sum, and canonical
    /// axes TLV. Returns `InvalidInput` if `axes` is empty, has more than
    /// three entries, contains an unknown tag, or is not strictly sorted
    /// ascending by tag.
    pub fn new_provable_count_provable_sum_indexed_tree(
        primary_root_key: Option<Vec<u8>>,
        count_value: CountValue,
        sum_value: SumValue,
        axes: Vec<(u8, Option<Vec<u8>>)>,
        flags: Option<ElementFlags>,
    ) -> Result<Self, ElementError> {
        Self::validate_pcpsit_axes(&axes)?;
        Ok(Element::ProvableCountProvableSumIndexedTree(
            primary_root_key,
            count_value,
            sum_value,
            axes,
            flags,
        ))
    }

    /// Wrap an element in `NonCounted` so it contributes 0 to its parent count
    /// tree's aggregate count when inserted. Sums (if any) still propagate.
    ///
    /// Returns `InvalidInput` if `inner` is already wrapped in any wrapper
    /// variant (`NonCounted`, `NotSummed`, or `NotCountedOrSummed`) — the
    /// wrappers are mutually exclusive and may not nest in either direction.
    /// Use `into_non_counted` to wrap idempotently when `inner` may already
    /// be `NonCounted`; use that helper's `Result` return for the
    /// cross-wrapper case.
    ///
    /// Note: at insert time the parent must be `CountTree` or
    /// `CountSumTree` (the non-provable count-bearing variants). Provable
    /// count parents reject the wrapper at the merk-layer insert guard.
    pub fn new_non_counted(inner: Element) -> Result<Self, ElementError> {
        if matches!(
            inner,
            Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_)
        ) {
            return Err(ElementError::InvalidInput(
                "NonCounted cannot wrap another wrapper",
            ));
        }
        if matches!(
            inner,
            Element::BidirectionalReference(..)
                | Element::ItemWithBackwardsReferences(..)
                | Element::SumItemWithBackwardsReferences(..)
        ) {
            return Err(ElementError::InvalidInput(
                "NonCounted cannot wrap backward-references elements",
            ));
        }
        Ok(Element::NonCounted(Box::new(inner)))
    }

    /// Wrap `self` in `NonCounted`. If `self` is already `NonCounted`,
    /// returns it unchanged (idempotent on `NonCounted`).
    ///
    /// Returns `InvalidInput` if `self` is any other wrapper variant — the
    /// wrappers are mutually exclusive. Callers that need the unconditional
    /// wrapping path should ensure the input is a non-wrapper variant
    /// before calling.
    pub fn into_non_counted(self) -> Result<Self, ElementError> {
        match self {
            Element::NonCounted(_) => {
                // Re-validate the idempotent path: even though only valid
                // NonCounted values should reach here via construction, a
                // hand-built nested-wrapper value would slip through without
                // this check.
                self.validate_wrapper_invariants()?;
                Ok(self)
            }
            Element::NotSummed(_) => Err(ElementError::InvalidInput(
                "cannot wrap NotSummed in NonCounted; wrappers are mutually exclusive",
            )),
            Element::NotCountedOrSummed(_) => Err(ElementError::InvalidInput(
                "cannot wrap NotCountedOrSummed in NonCounted; wrappers are mutually exclusive",
            )),
            other => Ok(Element::NonCounted(Box::new(other))),
        }
    }

    /// Wrap a sum-tree variant in `NotSummed` so it contributes 0 to its
    /// parent sum tree's running sum when inserted. Counts (if any) still
    /// propagate.
    ///
    /// Only the six sum-bearing tree variants are accepted: `SumTree`,
    /// `BigSumTree`, `CountSumTree`, `ProvableCountSumTree`,
    /// `ProvableSumTree`, `ProvableCountProvableSumTree`. Any other
    /// element — including items, sum items, references, non-sum trees, and
    /// any wrapper (`NonCounted`, `NotSummed`, `NotCountedOrSummed`) — is
    /// rejected with `InvalidInput`.
    pub fn new_not_summed(inner: Element) -> Result<Self, ElementError> {
        match inner {
            Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::CountSumTree(..)
            | Element::ProvableCountSumTree(..)
            | Element::ProvableSumTree(..)
            | Element::ProvableCountProvableSumTree(..) => Ok(Element::NotSummed(Box::new(inner))),
            _ => Err(ElementError::InvalidInput(
                "NotSummed inner element must be a sum-tree variant (SumTree, BigSumTree, \
                 CountSumTree, ProvableCountSumTree, ProvableSumTree, or \
                 ProvableCountProvableSumTree)",
            )),
        }
    }

    /// Wrap `self` in `NotSummed`. If `self` is already `NotSummed`, returns
    /// it unchanged (idempotent on `NotSummed`).
    ///
    /// Returns `InvalidInput` if `self` is any other wrapper (the three
    /// wrappers are mutually exclusive) or any non-sum-tree variant.
    /// Mirrors [`Element::into_non_counted`].
    pub fn into_not_summed(self) -> Result<Self, ElementError> {
        match self {
            Element::NotSummed(_) => {
                // Re-validate the idempotent path; see `into_non_counted`
                // for rationale.
                self.validate_wrapper_invariants()?;
                Ok(self)
            }
            Element::NonCounted(_) => Err(ElementError::InvalidInput(
                "cannot wrap NonCounted in NotSummed; wrappers are mutually exclusive",
            )),
            Element::NotCountedOrSummed(_) => Err(ElementError::InvalidInput(
                "cannot wrap NotCountedOrSummed in NotSummed; wrappers are mutually exclusive",
            )),
            other => Self::new_not_summed(other),
        }
    }

    /// Wrap a sum-tree variant in `NotCountedOrSummed` so it contributes 0
    /// to BOTH its parent's running sum AND its parent's count when
    /// inserted.
    ///
    /// Only the six sum-bearing tree variants are accepted: `SumTree`,
    /// `BigSumTree`, `CountSumTree`, `ProvableCountSumTree`,
    /// `ProvableSumTree`, `ProvableCountProvableSumTree`. Any other
    /// element — including items, sum items, references, non-sum trees, and
    /// any wrapper (`NonCounted`, `NotSummed`, `NotCountedOrSummed`) — is
    /// rejected with `InvalidInput`.
    ///
    /// Note: at insert time the parent must be `CountSumTree`. Provable
    /// count-and-sum parents (`ProvableCountSumTree`,
    /// `ProvableCountProvableSumTree`) reject the wrapper at the
    /// merk-layer insert guard — they cryptographically commit the
    /// count (and, for PCPS, the sum) into every node hash and must
    /// reflect actual contents.
    pub fn new_not_counted_or_summed(inner: Element) -> Result<Self, ElementError> {
        match inner {
            Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::CountSumTree(..)
            | Element::ProvableCountSumTree(..)
            | Element::ProvableSumTree(..)
            | Element::ProvableCountProvableSumTree(..) => {
                Ok(Element::NotCountedOrSummed(Box::new(inner)))
            }
            _ => Err(ElementError::InvalidInput(
                "NotCountedOrSummed inner element must be a sum-bearing tree variant (SumTree, \
                 BigSumTree, CountSumTree, ProvableCountSumTree, ProvableSumTree, or \
                 ProvableCountProvableSumTree)",
            )),
        }
    }

    /// Wrap `self` in `NotCountedOrSummed`. If `self` is already
    /// `NotCountedOrSummed`, returns it unchanged (idempotent).
    ///
    /// Returns `InvalidInput` if `self` is any other wrapper (the three
    /// wrappers are mutually exclusive) or any non-sum-tree variant.
    pub fn into_not_counted_or_summed(self) -> Result<Self, ElementError> {
        match self {
            Element::NotCountedOrSummed(_) => {
                // Re-validate the idempotent path; see `into_non_counted`
                // for rationale.
                self.validate_wrapper_invariants()?;
                Ok(self)
            }
            Element::NonCounted(_) => Err(ElementError::InvalidInput(
                "cannot wrap NonCounted in NotCountedOrSummed; wrappers are mutually exclusive",
            )),
            Element::NotSummed(_) => Err(ElementError::InvalidInput(
                "cannot wrap NotSummed in NotCountedOrSummed; wrappers are mutually exclusive",
            )),
            other => Self::new_not_counted_or_summed(other),
        }
    }
}
