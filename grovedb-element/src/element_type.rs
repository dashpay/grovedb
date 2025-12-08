//! Element type enum for efficient type checking from serialized bytes.

use crate::error::ElementError;

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
    /// Used for: Item, SumItem, ItemWithSumItem
    Kv,

    /// Use `Node::KVValueHash` - the verifier trusts the provided value_hash.
    ///
    /// Required because `value_hash = combine_hash(H(value), other_hash)` and
    /// the verifier doesn't have access to `other_hash` at the merk level.
    ///
    /// Security comes from GroveDB's multi-layer proof structure.
    ///
    /// Used for: Tree, SumTree, BigSumTree, CountTree, CountSumTree,
    ///           ProvableCountTree, Reference
    KvValueHash,
}

/// Element type discriminants matching the Element enum serialization order.
/// These correspond to the bincode serialization of the Element enum.
///
/// IMPORTANT: These values must match the order of variants in the Element
/// enum. If Element enum order changes, these must be updated accordingly.
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
}

impl ElementType {
    /// Get the ElementType from a serialized Element's first byte.
    ///
    /// This is an O(1) operation that avoids full deserialization.
    ///
    /// # Arguments
    /// * `serialized_value` - The serialized Element bytes
    ///
    /// # Returns
    /// * `Ok(ElementType)` - The element type
    /// * `Err(ElementError)` - If the value is empty or has an unknown
    ///   discriminant
    pub fn from_serialized_value(serialized_value: &[u8]) -> Result<Self, ElementError> {
        let first_byte = serialized_value.first().ok_or_else(|| {
            ElementError::CorruptedData("Cannot get element type from empty value".to_string())
        })?;

        Self::try_from(*first_byte)
    }

    /// Returns the type of proof node that should be used for this element
    /// type.
    ///
    /// - `ProofNodeType::Kv` for items (verifier computes hash from value)
    /// - `ProofNodeType::KvValueHash` for trees/references (verifier trusts
    ///   hash)
    #[inline]
    pub fn proof_node_type(&self) -> ProofNodeType {
        if self.has_simple_value_hash() {
            ProofNodeType::Kv
        } else {
            ProofNodeType::KvValueHash
        }
    }

    /// Returns true if this element type uses a simple value hash (H(value)).
    ///
    /// Item types have `value_hash = H(serialized_element)`.
    /// These can safely use `Node::KV` in proofs because the verifier
    /// can recompute the hash from the value.
    #[inline]
    pub fn has_simple_value_hash(&self) -> bool {
        matches!(
            self,
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
    #[inline]
    pub fn is_tree(&self) -> bool {
        matches!(
            self,
            ElementType::Tree
                | ElementType::SumTree
                | ElementType::BigSumTree
                | ElementType::CountTree
                | ElementType::CountSumTree
                | ElementType::ProvableCountTree
        )
    }

    /// Returns true if this element type is a reference.
    #[inline]
    pub fn is_reference(&self) -> bool {
        matches!(self, ElementType::Reference)
    }

    /// Returns true if this element type is any kind of item (not a tree or
    /// reference).
    #[inline]
    pub fn is_item(&self) -> bool {
        matches!(
            self,
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
        }
    }
}

impl TryFrom<u8> for ElementType {
    type Error = ElementError;

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
        assert!(ElementType::try_from(10).is_err());
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
    }

    #[test]
    fn test_proof_node_type() {
        use super::ProofNodeType;

        // Items should use Kv (verifier computes hash)
        assert_eq!(ElementType::Item.proof_node_type(), ProofNodeType::Kv);
        assert_eq!(ElementType::SumItem.proof_node_type(), ProofNodeType::Kv);
        assert_eq!(
            ElementType::ItemWithSumItem.proof_node_type(),
            ProofNodeType::Kv
        );

        // Trees and references should use KvValueHash (verifier trusts hash)
        assert_eq!(
            ElementType::Reference.proof_node_type(),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::Tree.proof_node_type(),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::SumTree.proof_node_type(),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::BigSumTree.proof_node_type(),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::CountTree.proof_node_type(),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::CountSumTree.proof_node_type(),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::ProvableCountTree.proof_node_type(),
            ProofNodeType::KvValueHash
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
    }

    #[test]
    fn test_is_tree() {
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
    }
}
