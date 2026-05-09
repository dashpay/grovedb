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

    /// Returns the wrapped element if `self` is `NonCounted`, else `self`.
    /// Use this when you need to inspect the actual element type and don't
    /// care whether it is wrapped.
    pub fn underlying(&self) -> &Element {
        match self {
            Element::NonCounted(inner) => inner,
            other => other,
        }
    }

    /// Mutable variant of [`underlying`].
    pub fn underlying_mut(&mut self) -> &mut Element {
        match self {
            Element::NonCounted(inner) => inner,
            other => other,
        }
    }

    /// Owned variant of [`underlying`].
    pub fn into_underlying(self) -> Element {
        match self {
            Element::NonCounted(inner) => *inner,
            other => other,
        }
    }

    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else.
    ///
    /// `NonCounted` delegates to its inner element — sums still propagate
    /// when the wrapper is inserted into a sum-bearing parent.
    pub fn sum_value_or_default(&self) -> i64 {
        match self {
            Element::NonCounted(inner) => inner.sum_value_or_default(),
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _)
            | Element::CountSumTree(_, _, sum_value, _)
            | Element::ProvableCountSumTree(_, _, sum_value, _) => *sum_value,
            _ => 0,
        }
    }

    /// Decoded the integer value in the CountTree element type, returns 1 for
    /// everything else.
    ///
    /// `NonCounted` returns 0 — the wrapper's whole purpose is to contribute
    /// nothing to the parent count tree.
    pub fn count_value_or_default(&self) -> u64 {
        match self {
            Element::NonCounted(_) => 0,
            Element::CountTree(_, count_value, _)
            | Element::CountSumTree(_, count_value, ..)
            | Element::ProvableCountTree(_, count_value, _)
            | Element::ProvableCountSumTree(_, count_value, ..) => *count_value,
            _ => 1,
        }
    }

    /// Decoded the count and sum values from the element type, returns (1, 0)
    /// for elements without count/sum semantics.
    ///
    /// `NonCounted` returns `(0, inner_sum)` — count is suppressed, sum still
    /// propagates.
    pub fn count_sum_value_or_default(&self) -> (u64, i64) {
        match self {
            Element::NonCounted(inner) => (0, inner.sum_value_or_default()),
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _) => (1, *sum_value),
            Element::CountTree(_, count_value, _) => (*count_value, 0),
            Element::CountSumTree(_, count_value, sum_value, _)
            | Element::ProvableCountSumTree(_, count_value, sum_value, _) => {
                (*count_value, *sum_value)
            }
            Element::ProvableCountTree(_, count_value, _) => (*count_value, 0),
            _ => (1, 0),
        }
    }

    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else. `NonCounted` delegates to its inner.
    pub fn big_sum_value_or_default(&self) -> i128 {
        match self {
            Element::NonCounted(inner) => inner.big_sum_value_or_default(),
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _)
            | Element::CountSumTree(_, _, sum_value, _)
            | Element::ProvableCountSumTree(_, _, sum_value, _) => *sum_value as i128,
            Element::BigSumTree(_, sum_value, _) => *sum_value,
            _ => 0,
        }
    }

    /// Decoded the integer value in the SumItem element type. Looks through
    /// a `NonCounted` wrapper.
    pub fn as_sum_item_value(&self) -> Result<i64, ElementError> {
        match self.underlying() {
            Element::SumItem(value, _) => Ok(*value),
            Element::ItemWithSumItem(_, value, _) => Ok(*value),
            _ => Err(ElementError::WrongElementType("expected a sum item")),
        }
    }

    /// Decoded the integer value in the SumItem element type. Looks through
    /// a `NonCounted` wrapper.
    pub fn into_sum_item_value(self) -> Result<i64, ElementError> {
        match self.into_underlying() {
            Element::SumItem(value, _) => Ok(value),
            Element::ItemWithSumItem(_, value, _) => Ok(value),
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
    /// through a `NonCounted` wrapper.
    pub fn into_reference_path_type(self) -> Result<ReferencePathType, ElementError> {
        match self.into_underlying() {
            Element::Reference(value, ..) => Ok(value),
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
                | Element::CommitmentTree(..)
                | Element::MmrTree(..)
                | Element::BulkAppendTree(..)
                | Element::DenseAppendOnlyFixedSizeTree(..)
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
                | Element::CommitmentTree(..)
                | Element::MmrTree(..)
                | Element::BulkAppendTree(..)
                | Element::DenseAppendOnlyFixedSizeTree(..)
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
        )
    }

    /// Check if the element is a reference. Looks through `NonCounted`.
    pub fn is_reference(&self) -> bool {
        matches!(self.underlying(), Element::Reference(..))
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
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags) => flags,
            Element::NonCounted(inner) => inner.get_flags(),
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
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags) => flags,
            Element::NonCounted(inner) => inner.get_flags_owned(),
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
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags) => flags,
            Element::NonCounted(inner) => inner.get_flags_mut(),
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
            | Element::ItemWithSumItem(.., flags)
            | Element::CommitmentTree(.., flags)
            | Element::MmrTree(.., flags)
            | Element::BulkAppendTree(.., flags)
            | Element::DenseAppendOnlyFixedSizeTree(.., flags) => *flags = new_flags,
            Element::NonCounted(inner) => inner.set_flags(new_flags),
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
        let once = inner.clone().into_non_counted();
        let twice = once.clone().into_non_counted();
        assert_eq!(once, twice);
        assert!(twice.is_non_counted());
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
            msg.contains("NonCounted") || msg.contains("non_counted"),
            "error should mention nested NonCounted: {}",
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
