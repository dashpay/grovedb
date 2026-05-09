//! Element type enum for efficient type checking from serialized bytes.

use crate::error::ElementError;

/// Bincode discriminant byte for `Element::NonCounted`. Tied to the
/// declaration order of the `Element` enum (0-indexed, 16th variant).
///
/// `ElementType` does NOT have a direct variant at this byte — when
/// `from_serialized_value` sees this discriminant, it reads the next byte to
/// resolve the inner type and returns a synthetic `NonCountedXxx` variant
/// (high bit set). The `test_element_serialization_discriminants_match_element_type`
/// test pins this constant to the actual bincode encoding.
pub const NON_COUNTED_WRAPPER_DISCRIMINANT: u8 = 15;

/// High bit set on every `NonCountedXxx` discriminant. The base type can be
/// recovered with `disc & NON_COUNTED_BASE_MASK`, and "is non-counted" is
/// `disc & NON_COUNTED_FLAG != 0`.
pub const NON_COUNTED_FLAG: u8 = 0x80;

/// Mask to recover the base type discriminant from a `NonCountedXxx`
/// discriminant.
pub const NON_COUNTED_BASE_MASK: u8 = 0x7F;

/// Indicates which type of proof node should be used when generating proofs.
///
/// This determines whether the verifier will recompute the value hash (secure)
/// or trust the provided value hash (required for combined hashes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofNodeType {
    /// Use `Node::KV` - the verifier will compute `value_hash = H(value)`.
    ///
    /// This is secure because any tampering with the value bytes will cause
    /// the computed hash to differ, failing verification.
    ///
    /// Used for: Item, SumItem, ItemWithSumItem (in regular trees)
    Kv,

    /// Use `Node::KVValueHash` - the verifier trusts the provided value_hash.
    ///
    /// Required because `value_hash = combine_hash(H(value), other_hash)` and
    /// the verifier doesn't have access to `other_hash` at the merk level.
    ///
    /// Security comes from GroveDB's multi-layer proof structure.
    ///
    /// Used for: All tree types (Tree, SumTree, BigSumTree, CountTree,
    ///           CountSumTree, ProvableCountTree) when NOT inside a
    ///           ProvableCountTree parent
    KvValueHash,

    /// Use `Node::KVRefValueHash` - like KVValueHash but for references.
    ///
    /// At the merk layer, this generates `KVValueHash` (since merk doesn't
    /// know about references). GroveDB post-processes these nodes to
    /// `Node::KVRefValueHash` with the dereferenced value.
    ///
    /// Required for references in regular trees because:
    /// 1. They need combined hash for reference resolution
    /// 2. GroveDB needs to identify them for post-processing
    ///
    /// Used for: Reference (in regular trees, not ProvableCountTree)
    KvRefValueHash,

    /// Use `Node::KVCount` - the verifier will compute `value_hash = H(value)`
    /// and include the count in the node hash calculation.
    ///
    /// This is secure because:
    /// 1. Tampering with value bytes causes hash mismatch (like KV)
    /// 2. Tampering with count causes hash mismatch (count is in node_hash)
    ///
    /// Used for: Item, SumItem, ItemWithSumItem (inside ProvableCountTree)
    KvCount,

    /// Use `Node::KVValueHashFeatureType` - like KVValueHash but includes the
    /// feature type (count) in the node hash calculation.
    ///
    /// Required for subtrees inside ProvableCountTree because:
    /// 1. They need combined hash (like KVValueHash) for subtree root hash
    /// 2. They need count included in node_hash for tamper resistance
    ///
    /// Used for: Tree, SumTree, BigSumTree, CountTree, CountSumTree,
    ///           ProvableCountTree (inside ProvableCountTree)
    KvValueHashFeatureType,

    /// Use `Node::KVRefValueHashCount` - like KVRefValueHash but includes
    /// the count in the node hash calculation.
    ///
    /// At the merk layer, this generates `KVValueHashFeatureType` (since merk
    /// doesn't know about references). GroveDB post-processes these nodes to
    /// `Node::KVRefValueHashCount` with the dereferenced value.
    ///
    /// Required for references inside ProvableCountTree because:
    /// 1. They need combined hash (like KVRefValueHash) for reference
    ///    resolution
    /// 2. They need count included in node_hash for tamper resistance
    ///
    /// Used for: Reference (inside ProvableCountTree)
    KvRefValueHashCount,
}

/// Element type discriminants.
///
/// Base types (0..=14) match the bincode serialization order of the `Element`
/// enum. Non-counted twins (128..=142) are synthetic — they encode "this is a
/// `NonCounted` wrapper around an inner element of base type
/// `disc & 0x7F`". The on-disk representation of `Element::NonCounted` still
/// uses the wrapper byte `NON_COUNTED_WRAPPER_DISCRIMINANT` (15) followed by
/// the inner element's bytes; `from_serialized_value` synthesizes the
/// `NonCountedXxx` variant by peeking at the second byte.
///
/// IMPORTANT: Base values (0..=14) must match the order of variants in the
/// `Element` enum. The `test_element_serialization_discriminants_match_element_type`
/// test catches drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ElementType {
    /// An ordinary value - discriminant 0
    Item = 0,
    /// A reference to an object by its path - discriminant 1
    Reference = 1,
    /// A subtree container - discriminant 2
    Tree = 2,
    /// Signed integer value for sum trees - discriminant 3
    SumItem = 3,
    /// Sum tree - discriminant 4
    SumTree = 4,
    /// Big sum tree (i128) - discriminant 5
    BigSumTree = 5,
    /// Count tree - discriminant 6
    CountTree = 6,
    /// Count and sum tree combined - discriminant 7
    CountSumTree = 7,
    /// Provable count tree - discriminant 8
    ProvableCountTree = 8,
    /// Item with sum value - discriminant 9
    ItemWithSumItem = 9,
    /// Provable count sum tree - discriminant 10
    ProvableCountSumTree = 10,
    /// Orchard-style commitment tree - discriminant 11
    CommitmentTree = 11,
    /// MMR (Merkle Mountain Range) tree - discriminant 12
    MmrTree = 12,
    /// Bulk-append tree - discriminant 13
    BulkAppendTree = 13,
    /// Dense fixed-sized Merkle tree - discriminant 14
    DenseAppendOnlyFixedSizeTree = 14,
    // 15 is reserved as the on-disk wrapper byte and has no direct
    // ElementType variant.
    /// Non-counted wrapper around `Item` - discriminant 128
    NonCountedItem = 128,
    /// Non-counted wrapper around `Reference` - discriminant 129
    NonCountedReference = 129,
    /// Non-counted wrapper around `Tree` - discriminant 130
    NonCountedTree = 130,
    /// Non-counted wrapper around `SumItem` - discriminant 131
    NonCountedSumItem = 131,
    /// Non-counted wrapper around `SumTree` - discriminant 132
    NonCountedSumTree = 132,
    /// Non-counted wrapper around `BigSumTree` - discriminant 133
    NonCountedBigSumTree = 133,
    /// Non-counted wrapper around `CountTree` - discriminant 134
    NonCountedCountTree = 134,
    /// Non-counted wrapper around `CountSumTree` - discriminant 135
    NonCountedCountSumTree = 135,
    /// Non-counted wrapper around `ProvableCountTree` - discriminant 136
    NonCountedProvableCountTree = 136,
    /// Non-counted wrapper around `ItemWithSumItem` - discriminant 137
    NonCountedItemWithSumItem = 137,
    /// Non-counted wrapper around `ProvableCountSumTree` - discriminant 138
    NonCountedProvableCountSumTree = 138,
    /// Non-counted wrapper around `CommitmentTree` - discriminant 139
    NonCountedCommitmentTree = 139,
    /// Non-counted wrapper around `MmrTree` - discriminant 140
    NonCountedMmrTree = 140,
    /// Non-counted wrapper around `BulkAppendTree` - discriminant 141
    NonCountedBulkAppendTree = 141,
    /// Non-counted wrapper around `DenseAppendOnlyFixedSizeTree` - discriminant 142
    NonCountedDenseAppendOnlyFixedSizeTree = 142,
}

impl ElementType {
    /// Get the ElementType from a serialized Element's leading bytes.
    ///
    /// Reads byte 0 in the common case. When byte 0 is the
    /// `NON_COUNTED_WRAPPER_DISCRIMINANT`, also reads byte 1 to resolve the
    /// inner type and returns the corresponding `NonCountedXxx` variant
    /// (with bit 7 set).
    ///
    /// # Arguments
    /// * `serialized_value` - The serialized Element bytes
    ///
    /// # Returns
    /// * `Ok(ElementType)` - The element type
    /// * `Err(ElementError)` - If the value is empty, truncated (wrapper
    ///   without inner byte), or has an unknown discriminant
    pub fn from_serialized_value(serialized_value: &[u8]) -> Result<Self, ElementError> {
        let first_byte = *serialized_value.first().ok_or_else(|| {
            ElementError::CorruptedData("Cannot get element type from empty value".to_string())
        })?;

        if first_byte == NON_COUNTED_WRAPPER_DISCRIMINANT {
            let inner_byte = *serialized_value.get(1).ok_or_else(|| {
                ElementError::CorruptedData(
                    "NonCounted wrapper has no inner element discriminant byte".to_string(),
                )
            })?;
            // Reject nested wrappers up front.
            if inner_byte == NON_COUNTED_WRAPPER_DISCRIMINANT {
                return Err(ElementError::CorruptedData(
                    "NonCounted cannot wrap another NonCounted".to_string(),
                ));
            }
            Self::try_from(NON_COUNTED_FLAG | inner_byte)
        } else {
            Self::try_from(first_byte)
        }
    }

    /// Returns true if this is a `NonCountedXxx` discriminant (bit 7 set).
    #[inline]
    pub const fn is_non_counted(self) -> bool {
        (self as u8) & NON_COUNTED_FLAG != 0
    }

    /// Returns the underlying base ElementType, stripping the NonCounted bit.
    /// For base types, returns `self` unchanged.
    #[inline]
    pub fn base(self) -> ElementType {
        if self.is_non_counted() {
            // Safe: every NonCountedXxx is constructed from a valid base
            // discriminant 0..=14, so masking the high bit yields a valid
            // base discriminant.
            ElementType::try_from((self as u8) & NON_COUNTED_BASE_MASK)
                .expect("NonCounted twin always has a valid base")
        } else {
            self
        }
    }

    /// Returns the type of proof node that should be used for this element
    /// type, given the parent tree type.
    ///
    /// ## Regular trees (Tree, SumTree, BigSumTree, CountTree, CountSumTree)
    ///
    /// All use `node_hash(kv_hash, left, right)` — feature_type is NOT hashed.
    ///
    /// | Role                               | Node type (V0)  | Node type (V1)                       |
    /// |------------------------------------|-----------------|--------------------------------------|
    /// | Queried item                       | `Kv`            | `Kv`                                 |
    /// | Queried non-empty tree (no subqry) | `KvValueHash`   | `KvValueHashFeatureTypeWithChildHash`|
    /// | Queried empty tree                 | `KvValueHash`   | `KvValueHash`                        |
    /// | Queried ref                        | `KvRefValueHash`| `KvRefValueHash`                     |
    ///
    /// Note: non-empty trees WITH a subquery descend into the child layer;
    /// the tree node appears as `KvValueHash` in the parent layer proof.
    ///
    /// ## ProvableCountTree / ProvableCountSumTree
    ///
    /// Use `node_hash_with_count(kv_hash, left, right, count)` — the count
    /// IS included in the hash, so every proof node must carry it.
    ///
    /// | Role                               | Node type (V0)           | Node type (V1)                       |
    /// |------------------------------------|--------------------------|--------------------------------------|
    /// | Queried item                       | `KvCount`                | `KvCount`                            |
    /// | Queried non-empty tree (no subqry) | `KvValueHashFeatureType` | `KvValueHashFeatureTypeWithChildHash`|
    /// | Queried empty tree                 | `KvValueHashFeatureType` | `KvValueHashFeatureType`             |
    /// | Queried ref                        | `KvRefValueHashCount`    | `KvRefValueHashCount`                |
    ///
    /// Non-queried and boundary nodes are handled separately in the merk
    /// proof generation code (`KVHash`/`KVHashCount`, `KVDigest`/`KVDigestCount`).
    ///
    /// See also: docs/book/src/proof-system.md "Proof Node Types by Tree Type"
    ///
    /// # Arguments
    /// * `parent_tree_type` - The type of tree containing this element, or
    ///   `None` for root-level elements
    ///
    /// The `NonCounted` wrapper is transparent for proof-node-type
    /// selection: both `self` and `parent_tree_type` are normalized via
    /// `base()` before dispatch. The proof shape is determined by the inner
    /// element type, not by the wrapper.
    #[inline]
    pub fn proof_node_type(&self, parent_tree_type: Option<ElementType>) -> ProofNodeType {
        let parent_base = parent_tree_type.map(|t| t.base());
        let is_provable_count_tree = matches!(
            parent_base,
            Some(ElementType::ProvableCountTree) | Some(ElementType::ProvableCountSumTree)
        );

        let base = self.base();
        if base.has_simple_value_hash() {
            // Items (Item, SumItem, ItemWithSumItem)
            if is_provable_count_tree {
                ProofNodeType::KvCount
            } else {
                ProofNodeType::Kv
            }
        } else if base.is_reference() {
            // References need combined hash (for reference resolution).
            // In ProvableCountTree, they also need the count in node_hash.
            // GroveDB post-processes these to KVRefValueHash/KVRefValueHashCount.
            if is_provable_count_tree {
                ProofNodeType::KvRefValueHashCount
            } else {
                ProofNodeType::KvRefValueHash
            }
        } else {
            // Subtrees (Tree, SumTree, BigSumTree, CountTree, CountSumTree,
            // ProvableCountTree)
            if is_provable_count_tree {
                ProofNodeType::KvValueHashFeatureType
            } else {
                ProofNodeType::KvValueHash
            }
        }
    }

    /// Returns true if this element type uses a simple value hash (H(value)).
    ///
    /// Item types have `value_hash = H(serialized_element)`.
    /// These can safely use `Node::KV` in proofs because the verifier
    /// can recompute the hash from the value. Looks through the
    /// `NonCounted` wrapper.
    #[inline]
    pub fn has_simple_value_hash(&self) -> bool {
        matches!(
            self.base(),
            ElementType::Item | ElementType::SumItem | ElementType::ItemWithSumItem
        )
    }

    /// Returns true if this element type uses a combined value hash.
    ///
    /// Subtrees and References have `value_hash = combine_hash(H(value),
    /// other_hash)`.
    /// - For subtrees: `other_hash` is the child merk root hash
    /// - For references: `other_hash` is the referenced element's value hash
    ///
    /// These must use `Node::KVValueHash` in proofs because the verifier
    /// cannot recompute the combined hash without additional information.
    #[inline]
    pub fn has_combined_value_hash(&self) -> bool {
        !self.has_simple_value_hash()
    }

    /// Returns true if this element type is any kind of tree (subtree).
    /// Looks through the `NonCounted` wrapper.
    #[inline]
    pub fn is_tree(&self) -> bool {
        matches!(
            self.base(),
            ElementType::Tree
                | ElementType::SumTree
                | ElementType::BigSumTree
                | ElementType::CountTree
                | ElementType::CountSumTree
                | ElementType::ProvableCountTree
                | ElementType::ProvableCountSumTree
                | ElementType::CommitmentTree
                | ElementType::MmrTree
                | ElementType::BulkAppendTree
                | ElementType::DenseAppendOnlyFixedSizeTree
        )
    }

    /// Returns true if this element type is a reference. Looks through the
    /// `NonCounted` wrapper.
    #[inline]
    pub fn is_reference(&self) -> bool {
        matches!(self.base(), ElementType::Reference)
    }

    /// Returns true if this element type is any kind of item (not a tree or
    /// reference). Looks through the `NonCounted` wrapper.
    #[inline]
    pub fn is_item(&self) -> bool {
        matches!(
            self.base(),
            ElementType::Item | ElementType::SumItem | ElementType::ItemWithSumItem
        )
    }

    /// Returns a human-readable string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            ElementType::Item => "item",
            ElementType::Reference => "reference",
            ElementType::Tree => "tree",
            ElementType::SumItem => "sum item",
            ElementType::SumTree => "sum tree",
            ElementType::BigSumTree => "big sum tree",
            ElementType::CountTree => "count tree",
            ElementType::CountSumTree => "count sum tree",
            ElementType::ProvableCountTree => "provable count tree",
            ElementType::ItemWithSumItem => "item with sum item",
            ElementType::ProvableCountSumTree => "provable count sum tree",
            ElementType::CommitmentTree => "commitment tree",
            ElementType::MmrTree => "mmr tree",
            ElementType::BulkAppendTree => "bulk_append_tree",
            ElementType::DenseAppendOnlyFixedSizeTree => "dense_tree",
            ElementType::NonCountedItem => "non_counted item",
            ElementType::NonCountedReference => "non_counted reference",
            ElementType::NonCountedTree => "non_counted tree",
            ElementType::NonCountedSumItem => "non_counted sum item",
            ElementType::NonCountedSumTree => "non_counted sum tree",
            ElementType::NonCountedBigSumTree => "non_counted big sum tree",
            ElementType::NonCountedCountTree => "non_counted count tree",
            ElementType::NonCountedCountSumTree => "non_counted count sum tree",
            ElementType::NonCountedProvableCountTree => "non_counted provable count tree",
            ElementType::NonCountedItemWithSumItem => "non_counted item with sum item",
            ElementType::NonCountedProvableCountSumTree => "non_counted provable count sum tree",
            ElementType::NonCountedCommitmentTree => "non_counted commitment tree",
            ElementType::NonCountedMmrTree => "non_counted mmr tree",
            ElementType::NonCountedBulkAppendTree => "non_counted bulk_append_tree",
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree => "non_counted dense_tree",
        }
    }
}

impl TryFrom<u8> for ElementType {
    type Error = ElementError;

    /// Maps a discriminant byte to an `ElementType`.
    ///
    /// `NON_COUNTED_WRAPPER_DISCRIMINANT` (15) on its own is rejected: it is
    /// the on-disk wrapper byte and must be paired with the inner element's
    /// discriminant byte. Use `from_serialized_value` for that.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ElementType::Item),
            1 => Ok(ElementType::Reference),
            2 => Ok(ElementType::Tree),
            3 => Ok(ElementType::SumItem),
            4 => Ok(ElementType::SumTree),
            5 => Ok(ElementType::BigSumTree),
            6 => Ok(ElementType::CountTree),
            7 => Ok(ElementType::CountSumTree),
            8 => Ok(ElementType::ProvableCountTree),
            9 => Ok(ElementType::ItemWithSumItem),
            10 => Ok(ElementType::ProvableCountSumTree),
            11 => Ok(ElementType::CommitmentTree),
            12 => Ok(ElementType::MmrTree),
            13 => Ok(ElementType::BulkAppendTree),
            14 => Ok(ElementType::DenseAppendOnlyFixedSizeTree),
            // 15 is the raw NonCounted wrapper byte; from_serialized_value
            // resolves it by reading the inner discriminant.
            128 => Ok(ElementType::NonCountedItem),
            129 => Ok(ElementType::NonCountedReference),
            130 => Ok(ElementType::NonCountedTree),
            131 => Ok(ElementType::NonCountedSumItem),
            132 => Ok(ElementType::NonCountedSumTree),
            133 => Ok(ElementType::NonCountedBigSumTree),
            134 => Ok(ElementType::NonCountedCountTree),
            135 => Ok(ElementType::NonCountedCountSumTree),
            136 => Ok(ElementType::NonCountedProvableCountTree),
            137 => Ok(ElementType::NonCountedItemWithSumItem),
            138 => Ok(ElementType::NonCountedProvableCountSumTree),
            139 => Ok(ElementType::NonCountedCommitmentTree),
            140 => Ok(ElementType::NonCountedMmrTree),
            141 => Ok(ElementType::NonCountedBulkAppendTree),
            142 => Ok(ElementType::NonCountedDenseAppendOnlyFixedSizeTree),
            _ => Err(ElementError::CorruptedData(format!(
                "Unknown element type discriminant: {}",
                value
            ))),
        }
    }
}

impl std::fmt::Display for ElementType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_element_type_from_discriminant() {
        assert_eq!(ElementType::try_from(0).unwrap(), ElementType::Item);
        assert_eq!(ElementType::try_from(1).unwrap(), ElementType::Reference);
        assert_eq!(ElementType::try_from(2).unwrap(), ElementType::Tree);
        assert_eq!(ElementType::try_from(3).unwrap(), ElementType::SumItem);
        assert_eq!(ElementType::try_from(4).unwrap(), ElementType::SumTree);
        assert_eq!(ElementType::try_from(5).unwrap(), ElementType::BigSumTree);
        assert_eq!(ElementType::try_from(6).unwrap(), ElementType::CountTree);
        assert_eq!(ElementType::try_from(7).unwrap(), ElementType::CountSumTree);
        assert_eq!(
            ElementType::try_from(8).unwrap(),
            ElementType::ProvableCountTree
        );
        assert_eq!(
            ElementType::try_from(9).unwrap(),
            ElementType::ItemWithSumItem
        );
        assert_eq!(
            ElementType::try_from(10).unwrap(),
            ElementType::ProvableCountSumTree
        );
        assert_eq!(
            ElementType::try_from(11).unwrap(),
            ElementType::CommitmentTree
        );
        assert_eq!(ElementType::try_from(12).unwrap(), ElementType::MmrTree);
        assert_eq!(
            ElementType::try_from(13).unwrap(),
            ElementType::BulkAppendTree
        );
        assert_eq!(
            ElementType::try_from(14).unwrap(),
            ElementType::DenseAppendOnlyFixedSizeTree
        );
        // 15 is the raw NonCounted wrapper byte and is rejected by TryFrom;
        // it has no direct ElementType variant (use from_serialized_value).
        assert!(ElementType::try_from(15).is_err());
        assert!(ElementType::try_from(16).is_err());

        // High-bit twins
        assert_eq!(
            ElementType::try_from(128).unwrap(),
            ElementType::NonCountedItem
        );
        assert_eq!(
            ElementType::try_from(129).unwrap(),
            ElementType::NonCountedReference
        );
        assert_eq!(
            ElementType::try_from(142).unwrap(),
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree
        );
        // Bytes between the base and twin ranges are invalid.
        assert!(ElementType::try_from(127).is_err());
        // Bytes past the highest twin are invalid.
        assert!(ElementType::try_from(143).is_err());
    }

    #[test]
    fn test_non_counted_helpers() {
        // is_non_counted: high bit means non-counted
        assert!(!ElementType::Item.is_non_counted());
        assert!(!ElementType::Tree.is_non_counted());
        assert!(ElementType::NonCountedItem.is_non_counted());
        assert!(ElementType::NonCountedTree.is_non_counted());
        assert!(ElementType::NonCountedDenseAppendOnlyFixedSizeTree.is_non_counted());

        // base() strips the wrapper and returns the underlying type.
        assert_eq!(ElementType::Item.base(), ElementType::Item);
        assert_eq!(ElementType::NonCountedItem.base(), ElementType::Item);
        assert_eq!(ElementType::NonCountedTree.base(), ElementType::Tree);
        assert_eq!(
            ElementType::NonCountedProvableCountTree.base(),
            ElementType::ProvableCountTree
        );

        // The discriminant relationship: twin = base | 0x80
        assert_eq!(
            ElementType::NonCountedItem as u8,
            ElementType::Item as u8 | NON_COUNTED_FLAG
        );
        assert_eq!(
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree as u8,
            ElementType::DenseAppendOnlyFixedSizeTree as u8 | NON_COUNTED_FLAG
        );
    }

    #[test]
    fn test_simple_vs_combined_hash() {
        // Items have simple hash
        assert!(ElementType::Item.has_simple_value_hash());
        assert!(ElementType::SumItem.has_simple_value_hash());
        assert!(ElementType::ItemWithSumItem.has_simple_value_hash());

        // Trees and references have combined hash
        assert!(ElementType::Reference.has_combined_value_hash());
        assert!(ElementType::Tree.has_combined_value_hash());
        assert!(ElementType::SumTree.has_combined_value_hash());
        assert!(ElementType::BigSumTree.has_combined_value_hash());
        assert!(ElementType::CountTree.has_combined_value_hash());
        assert!(ElementType::CountSumTree.has_combined_value_hash());
        assert!(ElementType::ProvableCountTree.has_combined_value_hash());

        // The wrapper is transparent: NonCountedItem still hashes simply.
        assert!(ElementType::NonCountedItem.has_simple_value_hash());
        assert!(ElementType::NonCountedSumItem.has_simple_value_hash());
        assert!(ElementType::NonCountedTree.has_combined_value_hash());
        assert!(ElementType::NonCountedReference.has_combined_value_hash());
    }

    #[test]
    fn test_proof_node_type_regular_tree() {
        use super::ProofNodeType;

        // In regular trees (or None parent), items should use Kv
        assert_eq!(ElementType::Item.proof_node_type(None), ProofNodeType::Kv);
        assert_eq!(
            ElementType::SumItem.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::Kv
        );
        assert_eq!(
            ElementType::ItemWithSumItem.proof_node_type(Some(ElementType::SumTree)),
            ProofNodeType::Kv
        );

        // References should use KvRefValueHash (verifier trusts hash, GroveDB
        // post-processes)
        assert_eq!(
            ElementType::Reference.proof_node_type(None),
            ProofNodeType::KvRefValueHash
        );

        // Trees should use KvValueHash (verifier trusts hash)
        assert_eq!(
            ElementType::Tree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::SumTree.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::BigSumTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::CountTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::CountSumTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::ProvableCountTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
    }

    #[test]
    fn test_proof_node_type_provable_count_tree() {
        use super::ProofNodeType;

        let pct = Some(ElementType::ProvableCountTree);

        // In ProvableCountTree, items should use KvCount (count in hash)
        assert_eq!(
            ElementType::Item.proof_node_type(pct),
            ProofNodeType::KvCount
        );
        assert_eq!(
            ElementType::SumItem.proof_node_type(pct),
            ProofNodeType::KvCount
        );
        assert_eq!(
            ElementType::ItemWithSumItem.proof_node_type(pct),
            ProofNodeType::KvCount
        );

        // References use KvRefValueHashCount (combined hash + count)
        // GroveDB post-processes these with dereferenced values
        assert_eq!(
            ElementType::Reference.proof_node_type(pct),
            ProofNodeType::KvRefValueHashCount
        );

        // Subtrees use KvValueHashFeatureType (combined hash + count)
        assert_eq!(
            ElementType::Tree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::SumTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::BigSumTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::CountTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::CountSumTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::ProvableCountTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
    }

    #[test]
    fn test_proof_node_type_provable_count_sum_tree() {
        use super::ProofNodeType;

        let pcst = Some(ElementType::ProvableCountSumTree);

        // Items use KvCount in ProvableCountSumTree (same as ProvableCountTree)
        assert_eq!(
            ElementType::Item.proof_node_type(pcst),
            ProofNodeType::KvCount
        );

        // References use KvRefValueHashCount
        assert_eq!(
            ElementType::Reference.proof_node_type(pcst),
            ProofNodeType::KvRefValueHashCount
        );

        // Subtrees use KvValueHashFeatureType
        assert_eq!(
            ElementType::Tree.proof_node_type(pcst),
            ProofNodeType::KvValueHashFeatureType
        );
    }

    #[test]
    fn test_proof_node_type_through_non_counted_wrapper() {
        use super::ProofNodeType;

        // Wrapping doesn't change proof shape — both self and parent fall
        // back to base() before dispatch.
        assert_eq!(
            ElementType::NonCountedItem.proof_node_type(None),
            ProofNodeType::Kv,
        );
        assert_eq!(
            ElementType::NonCountedItem
                .proof_node_type(Some(ElementType::NonCountedProvableCountTree)),
            ProofNodeType::KvCount,
        );
        assert_eq!(
            ElementType::NonCountedReference
                .proof_node_type(Some(ElementType::ProvableCountSumTree)),
            ProofNodeType::KvRefValueHashCount,
        );
        assert_eq!(
            ElementType::NonCountedTree.proof_node_type(Some(ElementType::ProvableCountTree)),
            ProofNodeType::KvValueHashFeatureType,
        );
    }

    #[test]
    fn test_from_serialized_value() {
        // Test with valid first bytes
        assert_eq!(
            ElementType::from_serialized_value(&[0, 1, 2, 3]).unwrap(),
            ElementType::Item
        );
        assert_eq!(
            ElementType::from_serialized_value(&[2, 0, 0]).unwrap(),
            ElementType::Tree
        );

        // Test with empty value
        assert!(ElementType::from_serialized_value(&[]).is_err());

        // Test with unknown discriminant
        assert!(ElementType::from_serialized_value(&[255]).is_err());

        // NonCounted wrapper: the leading byte is the wrapper discriminant
        // (15) and the next byte is the inner type's discriminant. The
        // returned type is the synthetic NonCountedXxx with bit 7 set.
        assert_eq!(
            ElementType::from_serialized_value(&[15, 0, 1, 2, 3]).unwrap(),
            ElementType::NonCountedItem
        );
        assert_eq!(
            ElementType::from_serialized_value(&[15, 6]).unwrap(),
            ElementType::NonCountedCountTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[15, 14]).unwrap(),
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree
        );
        // Truncated wrapper (no inner byte) is rejected.
        assert!(ElementType::from_serialized_value(&[15]).is_err());
        // Nested NonCounted is rejected.
        assert!(ElementType::from_serialized_value(&[15, 15]).is_err());
        // Wrapper with unknown inner discriminant is rejected.
        assert!(ElementType::from_serialized_value(&[15, 200]).is_err());
    }

    #[test]
    fn test_is_tree() {
        // Base types
        assert!(!ElementType::Item.is_tree());
        assert!(!ElementType::Reference.is_tree());
        assert!(ElementType::Tree.is_tree());
        assert!(!ElementType::SumItem.is_tree());
        assert!(ElementType::SumTree.is_tree());
        assert!(ElementType::BigSumTree.is_tree());
        assert!(ElementType::CountTree.is_tree());
        assert!(ElementType::CountSumTree.is_tree());
        assert!(ElementType::ProvableCountTree.is_tree());
        assert!(!ElementType::ItemWithSumItem.is_tree());
        assert!(ElementType::CommitmentTree.is_tree());
        assert!(ElementType::MmrTree.is_tree());
        assert!(ElementType::BulkAppendTree.is_tree());
        assert!(ElementType::DenseAppendOnlyFixedSizeTree.is_tree());

        // The wrapper is transparent: NonCountedTree is a tree, NonCountedItem is not.
        assert!(!ElementType::NonCountedItem.is_tree());
        assert!(!ElementType::NonCountedReference.is_tree());
        assert!(ElementType::NonCountedTree.is_tree());
        assert!(ElementType::NonCountedSumTree.is_tree());
        assert!(ElementType::NonCountedProvableCountTree.is_tree());
        assert!(ElementType::NonCountedDenseAppendOnlyFixedSizeTree.is_tree());

        // is_item / is_reference also see through the wrapper.
        assert!(ElementType::NonCountedItem.is_item());
        assert!(ElementType::NonCountedSumItem.is_item());
        assert!(ElementType::NonCountedReference.is_reference());
    }

    /// Verifies that serialized Element discriminants match ElementType
    /// constants.
    ///
    /// This test ensures that the ElementType enum values stay in sync with
    /// the actual bincode serialization of Element variants. If the Element
    /// enum order changes, this test will catch the drift.
    #[test]
    fn test_element_serialization_discriminants_match_element_type() {
        use grovedb_version::version::GroveVersion;

        use crate::{element::Element, reference_path::ReferencePathType};

        let grove_version = GroveVersion::latest();

        // Build vector of (Element, ElementType, variant_name) for all 10 variants
        let test_cases: Vec<(Element, ElementType, &str)> = vec![
            // discriminant 0
            (
                Element::Item(vec![1, 2, 3], None),
                ElementType::Item,
                "Item",
            ),
            // discriminant 1
            (
                Element::Reference(
                    ReferencePathType::AbsolutePathReference(vec![vec![1]]),
                    None,
                    None,
                ),
                ElementType::Reference,
                "Reference",
            ),
            // discriminant 2
            (Element::Tree(None, None), ElementType::Tree, "Tree"),
            // discriminant 3
            (Element::SumItem(42, None), ElementType::SumItem, "SumItem"),
            // discriminant 4
            (
                Element::SumTree(None, 0, None),
                ElementType::SumTree,
                "SumTree",
            ),
            // discriminant 5
            (
                Element::BigSumTree(None, 0, None),
                ElementType::BigSumTree,
                "BigSumTree",
            ),
            // discriminant 6
            (
                Element::CountTree(None, 0, None),
                ElementType::CountTree,
                "CountTree",
            ),
            // discriminant 7
            (
                Element::CountSumTree(None, 0, 0, None),
                ElementType::CountSumTree,
                "CountSumTree",
            ),
            // discriminant 8
            (
                Element::ProvableCountTree(None, 0, None),
                ElementType::ProvableCountTree,
                "ProvableCountTree",
            ),
            // discriminant 9
            (
                Element::ItemWithSumItem(vec![1, 2, 3], 42, None),
                ElementType::ItemWithSumItem,
                "ItemWithSumItem",
            ),
            // discriminant 10
            (
                Element::ProvableCountSumTree(None, 0, 0, None),
                ElementType::ProvableCountSumTree,
                "ProvableCountSumTree",
            ),
            // discriminant 11
            (
                Element::CommitmentTree(0, 10, None),
                ElementType::CommitmentTree,
                "CommitmentTree",
            ),
            // discriminant 12
            (Element::MmrTree(0, None), ElementType::MmrTree, "MmrTree"),
            // discriminant 13
            (
                Element::BulkAppendTree(0, 2, None),
                ElementType::BulkAppendTree,
                "BulkAppendTree",
            ),
            // discriminant 14
            (
                Element::DenseAppendOnlyFixedSizeTree(0, 1, None),
                ElementType::DenseAppendOnlyFixedSizeTree,
                "DenseAppendOnlyFixedSizeTree",
            ),
        ];

        // Verify we're testing all 15 base discriminants (0-14)
        assert_eq!(
            test_cases.len(),
            15,
            "Expected 15 base Element variants in test, got {}",
            test_cases.len()
        );

        for (element, expected_type, variant_name) in test_cases {
            let serialized = element
                .serialize(grove_version)
                .unwrap_or_else(|e| panic!("Failed to serialize {}: {:?}", variant_name, e));

            // Verify serialized buffer is non-empty
            assert!(
                !serialized.is_empty(),
                "Serialized {} should not be empty",
                variant_name
            );

            // Verify first byte matches ElementType discriminant
            let first_byte = serialized[0];
            let expected_discriminant = expected_type as u8;

            assert_eq!(
                first_byte, expected_discriminant,
                "Element::{} serialized with discriminant {}, but ElementType::{} = {}. The \
                 Element enum order may have changed!",
                variant_name, first_byte, variant_name, expected_discriminant
            );

            // Also verify round-trip through ElementType::from_serialized_value
            let parsed_type = ElementType::from_serialized_value(&serialized).unwrap_or_else(|e| {
                panic!(
                    "Failed to parse ElementType from serialized {}: {:?}",
                    variant_name, e
                )
            });

            assert_eq!(
                parsed_type, expected_type,
                "ElementType::from_serialized_value for {} returned {:?}, expected {:?}",
                variant_name, parsed_type, expected_type
            );
        }
    }

    /// Pins the bincode discriminant for `Element::NonCounted` to
    /// `NON_COUNTED_WRAPPER_DISCRIMINANT`. If someone reorders the `Element`
    /// enum and pushes `NonCounted` to a different position, this catches it
    /// — `from_serialized_value` reads byte 1 specifically when byte 0 is
    /// this constant.
    #[test]
    fn test_non_counted_wrapper_discriminant_pinned() {
        use grovedb_version::version::GroveVersion;

        use crate::element::Element;

        let grove_version = GroveVersion::latest();

        // Pick one inner element per category to verify the wrapper byte +
        // inner byte resolve to the right NonCountedXxx.
        let cases: Vec<(Element, ElementType, u8, &str)> = vec![
            (
                Element::NonCounted(Box::new(Element::Item(vec![1, 2, 3], None))),
                ElementType::NonCountedItem,
                0,
                "NonCounted(Item)",
            ),
            (
                Element::NonCounted(Box::new(Element::SumItem(7, None))),
                ElementType::NonCountedSumItem,
                3,
                "NonCounted(SumItem)",
            ),
            (
                Element::NonCounted(Box::new(Element::CountTree(None, 5, None))),
                ElementType::NonCountedCountTree,
                6,
                "NonCounted(CountTree)",
            ),
            (
                Element::NonCounted(Box::new(Element::ProvableCountTree(None, 5, None))),
                ElementType::NonCountedProvableCountTree,
                8,
                "NonCounted(ProvableCountTree)",
            ),
        ];

        for (element, expected_type, expected_inner_disc, name) in cases {
            let serialized = element
                .serialize(grove_version)
                .unwrap_or_else(|e| panic!("Failed to serialize {}: {:?}", name, e));

            assert!(
                serialized.len() >= 2,
                "Serialized {} should have at least 2 bytes",
                name
            );
            assert_eq!(
                serialized[0], NON_COUNTED_WRAPPER_DISCRIMINANT,
                "{}: first byte should be the wrapper discriminant (15)",
                name
            );
            assert_eq!(
                serialized[1], expected_inner_disc,
                "{}: second byte should match the inner element's discriminant",
                name
            );

            let parsed = ElementType::from_serialized_value(&serialized)
                .unwrap_or_else(|e| panic!("Failed to parse {}: {:?}", name, e));
            assert_eq!(
                parsed, expected_type,
                "{}: from_serialized_value returned {:?}, expected {:?}",
                name, parsed, expected_type
            );
            // And the synthetic discriminant follows the 0x80|base rule.
            assert_eq!(
                parsed as u8,
                expected_inner_disc | NON_COUNTED_FLAG,
                "{}: NonCountedXxx = inner_disc | 0x80",
                name
            );
        }
    }
}
