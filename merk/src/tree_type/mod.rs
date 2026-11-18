#[cfg(feature = "minimal")]
mod costs;
use std::fmt;

#[cfg(feature = "minimal")]
pub use costs::*;
use grovedb_element::ElementType;

#[cfg(feature = "minimal")]
use crate::merk::NodeType;
use crate::{Error, TreeFeatureType};

/// Represents a value that is either a tree or not a tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum MaybeTree {
    /// The value is a tree of the given type.
    Tree(TreeType),
    /// The value is not a tree.
    NotTree,
}

/// The type of a Merk subtree, determining its node structure and aggregation behavior.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TreeType {
    /// A standard Merk tree with no aggregation.
    NormalTree,
    /// A tree that maintains a running sum of descendant sum items.
    SumTree,
    /// A tree that maintains a 256-bit running sum of descendant sum items.
    BigSumTree,
    /// A tree that counts its elements.
    CountTree,
    /// A tree that both counts elements and maintains a running sum.
    CountSumTree,
    /// A count tree with provable count support.
    ProvableCountTree,
    /// A count-sum tree with provable count support.
    ProvableCountSumTree,
    /// A commitment tree with a configurable chunk power parameter.
    CommitmentTree(u8),
    /// A Merkle Mountain Range tree.
    MmrTree,
    /// A bulk-append optimized tree with a configurable chunk power parameter.
    BulkAppendTree(u8),
    /// A dense append-only tree with fixed-size entries and a configurable height.
    DenseAppendOnlyFixedSizeTree(u8),
    /// A sum tree with provable sum support — the aggregate `i64` sum is
    /// baked into every node's hash via `node_hash_with_sum`. This is the
    /// sum-side counterpart to `ProvableCountTree`: tampering with the
    /// stored sum changes the node hash and is therefore catchable by
    /// proof verification, unlike the plain `SumTree` where the sum is
    /// stored alongside but not bound into the hash. Uses dedicated
    /// proof-node families (`KVSum`, `KVHashSum`, `KVDigestSum`,
    /// `KVRefValueHashSum`, `HashWithSum`, and the `AggregateSumOnRange`
    /// query).
    ProvableSumTree,
    /// A tree that maintains BOTH a provable count AND a provable sum.
    /// Both aggregates are baked into every node's hash via
    /// `node_hash_with_count_and_sum`, so a single tree supports both
    /// `AggregateCountOnRange` AND `AggregateSumOnRange` proofs against
    /// the same root hash. Uses dedicated proof-node families
    /// (`KVCountSum`, `KVHashCountSum`, `KVDigestCountSum`,
    /// `KVRefValueHashCountSum`, `HashWithCountAndSum`).
    ProvableCountProvableSumTree,
    /// A provable sum-indexed tree's primary Merk: same node shape as
    /// `ProvableSumTree` (uses `ProvableSummedMerkNode` aggregation). The
    /// secondary Merk pointed to by the parent element is a
    /// `ProvableCountProvableSumTree` opened at a derived storage prefix,
    /// keyed by `(sum_sortable_be ‖ original_key)`. The primary stays
    /// lean (sum-only nodes); the secondary carries BOTH aggregates so
    /// positional queries against the sum ranking are provable via
    /// count-bound subtree-skip commitments — offset pagination in
    /// O(log n + k) proof size (k = page size), rank-of-key in
    /// O(log n).
    ProvableSumIndexedTree,
    /// A provable count-indexed tree's primary Merk: same node shape as
    /// `ProvableCountTree` (uses `ProvableCountedMerkNode` aggregation).
    /// The secondary Merk pointed to by the parent element is a regular
    /// `ProvableCountTree` opened at a derived storage prefix, keyed by
    /// `(count_be ‖ original_key)`. Count-provable already, so offset
    /// pagination over the count ranking is O(log n + k) proof size
    /// (k = page size) out of the box.
    ProvableCountIndexedTree,
    /// A provable count + provable sum indexed tree's primary Merk: same
    /// node shape as `ProvableCountProvableSumTree` (uses
    /// `ProvableCountedAndProvableSummedMerkNode` aggregation). The parent
    /// element carries a TLV list of 1..=3 secondary Merks, one per
    /// selected axis (count, sum, avg). Each secondary lives at its own
    /// derived storage prefix; the count axis opens as a
    /// `ProvableCountTree` while the sum and avg axes open as
    /// `ProvableCountProvableSumTree`s — every axis therefore carries a
    /// hash-bound count aggregate, so all of them support count-bound
    /// offset pagination, and the sum/avg axes additionally support
    /// sum-on-range proofs.
    ProvableCountProvableSumIndexedTree,
    /// A private document store: an append-only store of fixed-size opaque
    /// entries over a `BulkAppendTree`, with the `{entry_size, chunk_power}`
    /// configuration bound into the state root. Carries the chunk power
    /// (log2 of the epoch size) like `CommitmentTree` / `BulkAppendTree`;
    /// the entry size lives only in the Element (it does not affect Merk
    /// node layout).
    PrivateDocumentStore(u8),
    /// A tree that maintains BOTH a provable count AND a provable 128-bit
    /// sum: the wide counterpart of `ProvableCountProvableSumTree`, for
    /// sums that can pass the i64 range. Both aggregates are baked into
    /// every node's hash via `node_hash_with_count_and_big_sum`. Uses
    /// dedicated proof-node families (`KVCountBigSum`,
    /// `KVHashCountBigSum`, `KVDigestCountBigSum`,
    /// `KVRefValueHashCountBigSum`, `HashWithCountAndBigSum`).
    ProvableCountProvableBigSumTree,
    /// A tree that maintains a count and a 128-bit sum in its element, not
    /// in node hashes: the wide counterpart of `CountSumTree`. Like it, the
    /// tree accepts children that contribute nothing to the count, the sum
    /// or both.
    CountBigSumTree,
    /// A tree that maintains a provable 128-bit sum and no count: the wide
    /// counterpart of `ProvableSumTree`. The sum is baked into every node's
    /// hash via `node_hash_with_big_sum`. Uses dedicated proof-node families
    /// (`KVBigSum`, `KVHashBigSum`, `KVDigestBigSum`, `KVRefValueHashBigSum`,
    /// `HashWithBigSum`).
    ProvableBigSumTree,
    /// The primary of a `ProvableBigSumIndexedTree`: mirrors a
    /// `ProvableBigSumTree` at the Merk-node level, with a secondary Merk
    /// ordered by each entry's 128-bit sum.
    ProvableBigSumIndexedTree,
    /// The primary of a `ProvableCountProvableBigSumIndexedTree`: mirrors a
    /// `ProvableCountProvableBigSumTree` at the Merk-node level, with one
    /// secondary Merk per selected axis.
    ProvableCountProvableBigSumIndexedTree,
}

impl TreeType {
    /// Returns the stable discriminant for this tree type.
    /// Used for serialization where `as u8` was previously used on the C-like
    /// enum.
    pub fn discriminant(&self) -> u8 {
        match self {
            TreeType::NormalTree => 0,
            TreeType::SumTree => 1,
            TreeType::BigSumTree => 2,
            TreeType::CountTree => 3,
            TreeType::CountSumTree => 4,
            TreeType::ProvableCountTree => 5,
            TreeType::ProvableCountSumTree => 6,
            TreeType::CommitmentTree(_) => 7,
            TreeType::MmrTree => 8,
            TreeType::BulkAppendTree(_) => 9,
            TreeType::DenseAppendOnlyFixedSizeTree(_) => 10,
            TreeType::ProvableSumTree => 11,
            TreeType::ProvableCountProvableSumTree => 12,
            TreeType::ProvableSumIndexedTree => 13,
            TreeType::ProvableBigSumIndexedTree => 20,
            TreeType::ProvableCountIndexedTree => 14,
            TreeType::ProvableCountProvableSumIndexedTree => 15,
            TreeType::ProvableCountProvableBigSumIndexedTree => 21,
            TreeType::PrivateDocumentStore(_) => 16,
            TreeType::ProvableCountProvableBigSumTree => 17,
            TreeType::CountBigSumTree => 18,
            TreeType::ProvableBigSumTree => 19,
        }
    }
}

impl TryFrom<u8> for TreeType {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(TreeType::NormalTree),
            1 => Ok(TreeType::SumTree),
            2 => Ok(TreeType::BigSumTree),
            3 => Ok(TreeType::CountTree),
            4 => Ok(TreeType::CountSumTree),
            5 => Ok(TreeType::ProvableCountTree),
            6 => Ok(TreeType::ProvableCountSumTree),
            7 => Ok(TreeType::CommitmentTree(0)),
            8 => Ok(TreeType::MmrTree),
            9 => Ok(TreeType::BulkAppendTree(0)),
            10 => Ok(TreeType::DenseAppendOnlyFixedSizeTree(0)),
            11 => Ok(TreeType::ProvableSumTree),
            12 => Ok(TreeType::ProvableCountProvableSumTree),
            13 => Ok(TreeType::ProvableSumIndexedTree),
            14 => Ok(TreeType::ProvableCountIndexedTree),
            15 => Ok(TreeType::ProvableCountProvableSumIndexedTree),
            16 => Ok(TreeType::PrivateDocumentStore(0)),
            17 => Ok(TreeType::ProvableCountProvableBigSumTree),
            18 => Ok(TreeType::CountBigSumTree),
            19 => Ok(TreeType::ProvableBigSumTree),
            20 => Ok(TreeType::ProvableBigSumIndexedTree),
            21 => Ok(TreeType::ProvableCountProvableBigSumIndexedTree),
            n => Err(Error::UnknownTreeType(format!("got {}, max is 21", n))),
        }
    }
}

impl fmt::Display for TreeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match *self {
            TreeType::NormalTree => "Normal Tree",
            TreeType::SumTree => "Sum Tree",
            TreeType::BigSumTree => "Big Sum Tree",
            TreeType::CountTree => "Count Tree",
            TreeType::CountSumTree => "Count Sum Tree",
            TreeType::ProvableCountTree => "Provable Count Tree",
            TreeType::ProvableCountSumTree => "Provable Count Sum Tree",
            TreeType::CommitmentTree(_) => "Commitment Tree",
            TreeType::MmrTree => "MMR Tree",
            TreeType::BulkAppendTree(_) => "BulkAppendTree",
            TreeType::DenseAppendOnlyFixedSizeTree(_) => "Dense Tree",
            TreeType::ProvableSumTree => "Provable Sum Tree",
            TreeType::ProvableCountProvableSumTree => "Provable Count Provable Sum Tree",
            TreeType::ProvableSumIndexedTree => "Provable Sum Indexed Tree",
            TreeType::ProvableBigSumIndexedTree => "Provable Big Sum Indexed Tree",
            TreeType::ProvableCountIndexedTree => "Provable Count Indexed Tree",
            TreeType::ProvableCountProvableSumIndexedTree => {
                "Provable Count Provable Sum Indexed Tree"
            }
            TreeType::ProvableCountProvableBigSumIndexedTree => {
                "Provable Count Provable Big Sum Indexed Tree"
            }
            TreeType::PrivateDocumentStore(_) => "Private Document Store",
            TreeType::ProvableCountProvableBigSumTree => "Provable Count Provable Big Sum Tree",
            TreeType::CountBigSumTree => "Count Big Sum Tree",
            TreeType::ProvableBigSumTree => "Provable Big Sum Tree",
        };
        write!(f, "{}", s)
    }
}

impl TreeType {
    /// Returns true for tree types that store data in the data namespace as
    /// non-Merk entries.  These types have an always-empty Merk subtree and
    /// never contain child subtrees.
    pub fn uses_non_merk_data_storage(&self) -> bool {
        // NOTE: `ProvableSumTree` is intentionally NOT in this list — it is
        // a standard Merk-backed tree, just like `SumTree`.
        matches!(
            self,
            TreeType::CommitmentTree(_)
                | TreeType::MmrTree
                | TreeType::BulkAppendTree(_)
                | TreeType::DenseAppendOnlyFixedSizeTree(_)
                | TreeType::PrivateDocumentStore(_)
        )
    }

    /// Returns whether this tree type carries a count aggregate that children
    /// can contribute to. Only count-bearing trees may host
    /// `Element::NonCounted` children — in any other parent the wrapper would
    /// have no semantic effect, so it is rejected at insert time.
    pub const fn is_count_bearing(&self) -> bool {
        matches!(
            self,
            TreeType::CountTree
                | TreeType::CountSumTree
                | TreeType::CountBigSumTree
                | TreeType::ProvableCountTree
                | TreeType::ProvableCountSumTree
                | TreeType::ProvableCountProvableSumTree
                | TreeType::ProvableCountProvableBigSumTree
                | TreeType::ProvableCountIndexedTree
                | TreeType::ProvableCountProvableSumIndexedTree
                | TreeType::ProvableCountProvableBigSumIndexedTree
        )
    }

    /// Returns whether this tree type is an indexed tree's primary Merk.
    /// Indexed-tree primaries carry one or more secondary Merks that live
    /// at derived storage prefixes. Used by code that needs to distinguish
    /// indexed-tree primaries (where children are mirrored into secondaries)
    /// from ordinary trees.
    pub const fn is_indexed_primary(&self) -> bool {
        matches!(
            self,
            TreeType::ProvableSumIndexedTree
                | TreeType::ProvableBigSumIndexedTree
                | TreeType::ProvableCountIndexedTree
                | TreeType::ProvableCountProvableSumIndexedTree
                | TreeType::ProvableCountProvableBigSumIndexedTree
        )
    }

    /// Returns whether this tree type is a count-indexed tree's primary
    /// Merk (single-axis count). Kept for callers that specifically need
    /// the count-only case; new code should prefer `is_indexed_primary`.
    pub const fn is_count_indexed_primary(&self) -> bool {
        matches!(self, TreeType::ProvableCountIndexedTree)
    }

    /// Returns whether this tree type carries a sum aggregate that children
    /// can contribute to. Only sum-bearing trees may host
    /// `Element::NotSummed` children — in any other parent the wrapper would
    /// have no semantic effect, so it is rejected at insert time.
    pub const fn is_sum_bearing(&self) -> bool {
        matches!(
            self,
            TreeType::SumTree
                | TreeType::BigSumTree
                | TreeType::CountSumTree
                | TreeType::CountBigSumTree
                | TreeType::ProvableCountSumTree
                | TreeType::ProvableSumTree
                | TreeType::ProvableBigSumTree
                | TreeType::ProvableCountProvableSumTree
                | TreeType::ProvableCountProvableBigSumTree
                | TreeType::ProvableSumIndexedTree
                | TreeType::ProvableBigSumIndexedTree
                | TreeType::ProvableCountProvableSumIndexedTree
                | TreeType::ProvableCountProvableBigSumIndexedTree
        )
    }

    /// Returns whether this tree type carries BOTH a count and a sum
    /// aggregate. Equivalent to `is_count_bearing() && is_sum_bearing()`.
    ///
    /// NOTE: this predicate intentionally still includes
    /// `ProvableCountSumTree`. It answers the structural question
    /// "does this tree track both axes?" — used by aggregate logic.
    /// For the wrapper-acceptance question ("may this parent host a
    /// `NotCountedOrSummed` child?"), use
    /// `accepts_not_counted_or_summed_children`, which additionally
    /// rejects the `Provable*` variants.
    pub const fn is_count_and_sum_bearing(&self) -> bool {
        matches!(
            self,
            TreeType::CountSumTree
                | TreeType::CountBigSumTree
                | TreeType::ProvableCountSumTree
                | TreeType::ProvableCountProvableSumTree
                | TreeType::ProvableCountProvableBigSumTree
                | TreeType::ProvableCountProvableSumIndexedTree
                | TreeType::ProvableCountProvableBigSumIndexedTree
        )
    }

    /// Returns whether this parent tree type may host an
    /// `Element::NonCounted` child.
    ///
    /// Stricter than `is_count_bearing`: the wrapper is allowed only in
    /// the non-provable count-bearing trees (`CountTree`, `CountSumTree`,
    /// `CountIndexedTree`). `ProvableCountTree`,
    /// `ProvableCountSumTree`, and `ProvableCountIndexedTree` bind their
    /// aggregate count into every node's hash via `node_hash_with_count`,
    /// so a `NonCounted` child would commit a cryptographic count that
    /// diverges from the actual number of stored elements — confusing
    /// for callers and a footgun for proof-driven readers. The wrapper
    /// is rejected at insert time in those parents.
    ///
    /// Indexed-tree primaries are all `Provable*` and therefore excluded
    /// from this list for the same reason: their aggregate count (when
    /// they have one) is bound into node hashes.
    pub const fn accepts_non_counted_children(&self) -> bool {
        matches!(
            self,
            TreeType::CountTree | TreeType::CountSumTree | TreeType::CountBigSumTree
        )
    }

    /// Returns whether this parent tree type may host an
    /// `Element::NotCountedOrSummed` child.
    ///
    /// Stricter than `is_count_and_sum_bearing`: only the non-provable
    /// count-and-sum-bearing tree (`CountSumTree`) is accepted.
    /// `ProvableCountSumTree` is excluded for the same reason as in
    /// `accepts_non_counted_children` — the count (and sum) are
    /// cryptographically committed and must reflect actual contents.
    pub const fn accepts_not_counted_or_summed_children(&self) -> bool {
        matches!(self, TreeType::CountSumTree | TreeType::CountBigSumTree)
    }

    /// Returns whether this tree type allows sum items as children.
    pub fn allows_sum_item(&self) -> bool {
        match self {
            TreeType::NormalTree => false,
            TreeType::SumTree => true,
            TreeType::BigSumTree => true,
            TreeType::CountTree => false,
            TreeType::CountSumTree => true,
            TreeType::ProvableCountTree => false,
            TreeType::ProvableCountSumTree => true, // allows sum items
            TreeType::CommitmentTree(_) => false,
            TreeType::MmrTree => false,
            TreeType::BulkAppendTree(_) => false,
            TreeType::DenseAppendOnlyFixedSizeTree(_) => false,
            TreeType::ProvableSumTree => true,
            TreeType::ProvableCountProvableSumTree => true,
            // ProvableSumIndexedTree's primary mirrors ProvableSumTree (sum
            // items allowed); ProvableCountIndexedTree's primary mirrors
            // ProvableCountTree (no sum items); PCPS-indexed mirrors
            // ProvableCountProvableSumTree (sum items allowed).
            TreeType::ProvableSumIndexedTree => true,
            TreeType::ProvableBigSumIndexedTree => true,
            TreeType::ProvableCountIndexedTree => false,
            TreeType::ProvableCountProvableSumIndexedTree => true,
            TreeType::ProvableCountProvableBigSumIndexedTree => true,
            TreeType::PrivateDocumentStore(_) => false,
            TreeType::ProvableCountProvableBigSumTree => true,
            TreeType::CountBigSumTree => true,
            TreeType::ProvableBigSumTree => true,
        }
    }

    #[cfg(feature = "minimal")]
    /// Returns the inner node type used by nodes within this tree type.
    pub const fn inner_node_type(&self) -> NodeType {
        match self {
            TreeType::NormalTree => NodeType::NormalNode,
            TreeType::SumTree => NodeType::SumNode,
            TreeType::BigSumTree => NodeType::BigSumNode,
            TreeType::CountTree => NodeType::CountNode,
            TreeType::CountSumTree => NodeType::CountSumNode,
            TreeType::ProvableCountTree => NodeType::ProvableCountNode,
            TreeType::ProvableCountSumTree => NodeType::ProvableCountSumNode,
            TreeType::CommitmentTree(_) => NodeType::NormalNode,
            TreeType::MmrTree => NodeType::NormalNode,
            TreeType::BulkAppendTree(_) => NodeType::NormalNode,
            TreeType::DenseAppendOnlyFixedSizeTree(_) => NodeType::NormalNode,
            TreeType::ProvableSumTree => NodeType::ProvableSumNode,
            TreeType::ProvableCountProvableSumTree => NodeType::ProvableCountProvableSumNode,
            // The primary of a ProvableSumIndexedTree mirrors a
            // ProvableSumTree.
            TreeType::ProvableSumIndexedTree => NodeType::ProvableSumNode,
            TreeType::ProvableBigSumIndexedTree => NodeType::ProvableBigSumNode,
            // The primary of a ProvableCountIndexedTree mirrors a
            // ProvableCountTree.
            TreeType::ProvableCountIndexedTree => NodeType::ProvableCountNode,
            // The primary of a ProvableCountProvableSumIndexedTree mirrors
            // a ProvableCountProvableSumTree.
            TreeType::ProvableCountProvableSumIndexedTree => NodeType::ProvableCountProvableSumNode,
            TreeType::ProvableCountProvableBigSumIndexedTree => {
                NodeType::ProvableCountProvableBigSumNode
            }
            TreeType::PrivateDocumentStore(_) => NodeType::NormalNode,
            TreeType::ProvableCountProvableBigSumTree => NodeType::ProvableCountProvableBigSumNode,
            TreeType::CountBigSumTree => NodeType::CountBigSumNode,
            TreeType::ProvableBigSumTree => NodeType::ProvableBigSumNode,
        }
    }

    /// Returns the feature type for an empty tree of this type.
    pub fn empty_tree_feature_type(&self) -> TreeFeatureType {
        match self {
            TreeType::NormalTree => TreeFeatureType::BasicMerkNode,
            TreeType::SumTree => TreeFeatureType::SummedMerkNode(0),
            TreeType::BigSumTree => TreeFeatureType::BigSummedMerkNode(0),
            TreeType::CountTree => TreeFeatureType::CountedMerkNode(0),
            TreeType::CountSumTree => TreeFeatureType::CountedSummedMerkNode(0, 0),
            TreeType::ProvableCountTree => TreeFeatureType::ProvableCountedMerkNode(0),
            TreeType::ProvableCountSumTree => TreeFeatureType::ProvableCountedSummedMerkNode(0, 0),
            TreeType::CommitmentTree(_) => TreeFeatureType::BasicMerkNode,
            TreeType::MmrTree => TreeFeatureType::BasicMerkNode,
            TreeType::BulkAppendTree(_) => TreeFeatureType::BasicMerkNode,
            TreeType::DenseAppendOnlyFixedSizeTree(_) => TreeFeatureType::BasicMerkNode,
            TreeType::ProvableSumTree => TreeFeatureType::ProvableSummedMerkNode(0),
            TreeType::ProvableCountProvableSumTree => {
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(0, 0)
            }
            // The primary of a ProvableSumIndexedTree mirrors a
            // ProvableSumTree.
            TreeType::ProvableSumIndexedTree => TreeFeatureType::ProvableSummedMerkNode(0),
            TreeType::ProvableBigSumIndexedTree => TreeFeatureType::ProvableBigSummedMerkNode(0),
            // The primary of a ProvableCountIndexedTree mirrors a
            // ProvableCountTree.
            TreeType::ProvableCountIndexedTree => TreeFeatureType::ProvableCountedMerkNode(0),
            // The primary of a ProvableCountProvableSumIndexedTree mirrors
            // a ProvableCountProvableSumTree.
            TreeType::ProvableCountProvableSumIndexedTree => {
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(0, 0)
            }
            TreeType::ProvableCountProvableBigSumIndexedTree => {
                TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(0, 0)
            }
            TreeType::PrivateDocumentStore(_) => TreeFeatureType::BasicMerkNode,
            TreeType::ProvableCountProvableBigSumTree => {
                TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(0, 0)
            }
            TreeType::CountBigSumTree => TreeFeatureType::CountedBigSummedMerkNode(0, 0),
            TreeType::ProvableBigSumTree => TreeFeatureType::ProvableBigSummedMerkNode(0),
        }
    }

    /// Converts TreeType to the corresponding ElementType for proof generation.
    ///
    /// This is used to determine the correct proof node type based on
    /// the parent tree type. The returned ElementType is used with
    /// `ElementType::proof_node_type()` to select the appropriate
    /// proof node format.
    pub fn to_element_type(&self) -> Option<ElementType> {
        match self {
            TreeType::NormalTree => Some(ElementType::Tree),
            TreeType::SumTree => Some(ElementType::SumTree),
            TreeType::BigSumTree => Some(ElementType::BigSumTree),
            TreeType::CountTree => Some(ElementType::CountTree),
            TreeType::CountSumTree => Some(ElementType::CountSumTree),
            TreeType::ProvableCountTree => Some(ElementType::ProvableCountTree),
            TreeType::ProvableCountSumTree => Some(ElementType::ProvableCountSumTree),
            TreeType::CommitmentTree(_) => Some(ElementType::CommitmentTree),
            TreeType::MmrTree => Some(ElementType::MmrTree),
            TreeType::BulkAppendTree(_) => Some(ElementType::BulkAppendTree),
            TreeType::DenseAppendOnlyFixedSizeTree(_) => {
                Some(ElementType::DenseAppendOnlyFixedSizeTree)
            }
            TreeType::ProvableSumTree => Some(ElementType::ProvableSumTree),
            TreeType::ProvableCountProvableSumTree => {
                Some(ElementType::ProvableCountProvableSumTree)
            }
            TreeType::ProvableSumIndexedTree => Some(ElementType::ProvableSumIndexedTree),
            TreeType::ProvableBigSumIndexedTree => Some(ElementType::ProvableBigSumIndexedTree),
            TreeType::ProvableCountIndexedTree => Some(ElementType::ProvableCountIndexedTree),
            TreeType::ProvableCountProvableSumIndexedTree => {
                Some(ElementType::ProvableCountProvableSumIndexedTree)
            }
            TreeType::ProvableCountProvableBigSumIndexedTree => {
                Some(ElementType::ProvableCountProvableBigSumIndexedTree)
            }
            TreeType::PrivateDocumentStore(_) => Some(ElementType::PrivateDocumentStore),
            TreeType::ProvableCountProvableBigSumTree => {
                Some(ElementType::ProvableCountProvableBigSumTree)
            }
            TreeType::CountBigSumTree => Some(ElementType::CountBigSumTree),
            TreeType::ProvableBigSumTree => Some(ElementType::ProvableBigSumTree),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_type_discriminant_roundtrip() {
        let variants = [
            TreeType::NormalTree,
            TreeType::SumTree,
            TreeType::BigSumTree,
            TreeType::CountTree,
            TreeType::CountSumTree,
            TreeType::ProvableCountTree,
            TreeType::ProvableCountSumTree,
            TreeType::CommitmentTree(5),
            TreeType::MmrTree,
            TreeType::BulkAppendTree(3),
            TreeType::DenseAppendOnlyFixedSizeTree(8),
            TreeType::ProvableSumTree,
            TreeType::ProvableCountProvableSumTree,
            TreeType::PrivateDocumentStore(4),
            TreeType::ProvableCountProvableBigSumTree,
        ];
        for v in &variants {
            let d = v.discriminant();
            let back = TreeType::try_from(d).unwrap();
            // Roundtrip preserves the variant (inner values default to 0 for parameterized types)
            assert_eq!(d, back.discriminant());
        }
    }

    #[test]
    fn tree_type_try_from_invalid() {
        assert!(TreeType::try_from(22u8).is_err());
        assert!(TreeType::try_from(255u8).is_err());
    }

    #[test]
    fn private_document_store_tree_type_basics() {
        // Discriminant round-trip (chunk_power defaults to 0 through the byte).
        assert_eq!(TreeType::PrivateDocumentStore(4).discriminant(), 16);
        assert_eq!(
            TreeType::try_from(16u8).unwrap(),
            TreeType::PrivateDocumentStore(0)
        );
        let t = TreeType::PrivateDocumentStore(4);
        assert_eq!(format!("{}", t), "Private Document Store");
        assert!(t.uses_non_merk_data_storage());
        assert!(!t.is_count_bearing());
        assert!(!t.is_sum_bearing());
        assert!(!t.is_count_and_sum_bearing());
        assert!(!t.is_indexed_primary());
        assert!(!t.is_count_indexed_primary());
        assert!(!t.accepts_non_counted_children());
        assert!(!t.accepts_not_counted_or_summed_children());
        assert!(!t.allows_sum_item());
        assert_eq!(t.inner_node_type(), NodeType::NormalNode);
        assert_eq!(t.empty_tree_feature_type(), TreeFeatureType::BasicMerkNode);
        assert_eq!(t.to_element_type(), Some(ElementType::PrivateDocumentStore));
        assert_eq!(t.cost_size(), PRIVATE_DOCUMENT_STORE_COST_SIZE);
    }

    #[test]
    fn indexed_tree_types_round_trip_through_discriminant() {
        for v in [
            TreeType::ProvableSumIndexedTree,
            TreeType::ProvableCountIndexedTree,
            TreeType::ProvableCountProvableSumIndexedTree,
        ] {
            let d = v.discriminant();
            let back = TreeType::try_from(d).unwrap();
            assert_eq!(v, back);
            assert!(v.is_indexed_primary());
        }
        // PCIT and PCPSIT are count-bearing; PSIT is not.
        assert!(TreeType::ProvableCountIndexedTree.is_count_bearing());
        assert!(TreeType::ProvableCountProvableSumIndexedTree.is_count_bearing());
        assert!(!TreeType::ProvableSumIndexedTree.is_count_bearing());
        // PSIT and PCPSIT are sum-bearing; PCIT is not.
        assert!(TreeType::ProvableSumIndexedTree.is_sum_bearing());
        assert!(TreeType::ProvableCountProvableSumIndexedTree.is_sum_bearing());
        assert!(!TreeType::ProvableCountIndexedTree.is_sum_bearing());
        // Only PCIT is the single-axis count primary (kept for legacy callers).
        assert!(TreeType::ProvableCountIndexedTree.is_count_indexed_primary());
        assert!(!TreeType::ProvableSumIndexedTree.is_count_indexed_primary());
        assert!(!TreeType::ProvableCountProvableSumIndexedTree.is_count_indexed_primary());
    }

    #[test]
    fn tree_type_display_all_variants() {
        assert_eq!(format!("{}", TreeType::NormalTree), "Normal Tree");
        assert_eq!(format!("{}", TreeType::SumTree), "Sum Tree");
        assert_eq!(format!("{}", TreeType::BigSumTree), "Big Sum Tree");
        assert_eq!(format!("{}", TreeType::CountTree), "Count Tree");
        assert_eq!(format!("{}", TreeType::CountSumTree), "Count Sum Tree");
        assert_eq!(
            format!("{}", TreeType::ProvableCountTree),
            "Provable Count Tree"
        );
        assert_eq!(
            format!("{}", TreeType::ProvableCountSumTree),
            "Provable Count Sum Tree"
        );
        assert_eq!(
            format!("{}", TreeType::CommitmentTree(0)),
            "Commitment Tree"
        );
        assert_eq!(format!("{}", TreeType::MmrTree), "MMR Tree");
        assert_eq!(format!("{}", TreeType::BulkAppendTree(0)), "BulkAppendTree");
        assert_eq!(
            format!("{}", TreeType::DenseAppendOnlyFixedSizeTree(0)),
            "Dense Tree"
        );
        assert_eq!(
            format!("{}", TreeType::ProvableSumTree),
            "Provable Sum Tree"
        );
        assert_eq!(
            format!("{}", TreeType::ProvableCountProvableSumTree),
            "Provable Count Provable Sum Tree"
        );
    }

    #[test]
    fn uses_non_merk_data_storage() {
        assert!(!TreeType::NormalTree.uses_non_merk_data_storage());
        assert!(!TreeType::SumTree.uses_non_merk_data_storage());
        assert!(!TreeType::BigSumTree.uses_non_merk_data_storage());
        assert!(!TreeType::CountTree.uses_non_merk_data_storage());
        assert!(!TreeType::CountSumTree.uses_non_merk_data_storage());
        assert!(!TreeType::ProvableCountTree.uses_non_merk_data_storage());
        assert!(!TreeType::ProvableCountSumTree.uses_non_merk_data_storage());
        assert!(TreeType::CommitmentTree(0).uses_non_merk_data_storage());
        assert!(TreeType::MmrTree.uses_non_merk_data_storage());
        assert!(TreeType::BulkAppendTree(0).uses_non_merk_data_storage());
        assert!(TreeType::DenseAppendOnlyFixedSizeTree(0).uses_non_merk_data_storage());
        assert!(!TreeType::ProvableSumTree.uses_non_merk_data_storage());
        assert!(!TreeType::ProvableCountProvableSumTree.uses_non_merk_data_storage());
    }

    #[test]
    fn is_count_bearing() {
        assert!(!TreeType::NormalTree.is_count_bearing());
        assert!(!TreeType::SumTree.is_count_bearing());
        assert!(!TreeType::BigSumTree.is_count_bearing());
        assert!(TreeType::CountTree.is_count_bearing());
        assert!(TreeType::CountSumTree.is_count_bearing());
        assert!(TreeType::ProvableCountTree.is_count_bearing());
        assert!(TreeType::ProvableCountSumTree.is_count_bearing());
        assert!(!TreeType::CommitmentTree(0).is_count_bearing());
        assert!(!TreeType::MmrTree.is_count_bearing());
        assert!(!TreeType::BulkAppendTree(0).is_count_bearing());
        assert!(!TreeType::DenseAppendOnlyFixedSizeTree(0).is_count_bearing());
        // ProvableSumTree carries a sum aggregate, not a count.
        assert!(!TreeType::ProvableSumTree.is_count_bearing());
        // ProvableCountProvableSumTree carries BOTH a count AND a sum.
        assert!(TreeType::ProvableCountProvableSumTree.is_count_bearing());
    }

    #[test]
    fn is_sum_bearing() {
        assert!(!TreeType::NormalTree.is_sum_bearing());
        assert!(TreeType::SumTree.is_sum_bearing());
        assert!(TreeType::BigSumTree.is_sum_bearing());
        assert!(!TreeType::CountTree.is_sum_bearing());
        assert!(TreeType::CountSumTree.is_sum_bearing());
        assert!(!TreeType::ProvableCountTree.is_sum_bearing());
        assert!(TreeType::ProvableCountSumTree.is_sum_bearing());
        assert!(!TreeType::CommitmentTree(0).is_sum_bearing());
        assert!(!TreeType::MmrTree.is_sum_bearing());
        assert!(!TreeType::BulkAppendTree(0).is_sum_bearing());
        assert!(!TreeType::DenseAppendOnlyFixedSizeTree(0).is_sum_bearing());
        assert!(TreeType::ProvableSumTree.is_sum_bearing());
        assert!(TreeType::ProvableCountProvableSumTree.is_sum_bearing());
    }

    #[test]
    fn is_count_and_sum_bearing() {
        assert!(!TreeType::NormalTree.is_count_and_sum_bearing());
        assert!(!TreeType::SumTree.is_count_and_sum_bearing());
        assert!(!TreeType::BigSumTree.is_count_and_sum_bearing());
        assert!(!TreeType::CountTree.is_count_and_sum_bearing());
        assert!(TreeType::CountSumTree.is_count_and_sum_bearing());
        assert!(!TreeType::ProvableCountTree.is_count_and_sum_bearing());
        assert!(TreeType::ProvableCountSumTree.is_count_and_sum_bearing());
        assert!(!TreeType::CommitmentTree(0).is_count_and_sum_bearing());
        assert!(!TreeType::MmrTree.is_count_and_sum_bearing());
        assert!(!TreeType::BulkAppendTree(0).is_count_and_sum_bearing());
        assert!(!TreeType::DenseAppendOnlyFixedSizeTree(0).is_count_and_sum_bearing());
        assert!(!TreeType::ProvableSumTree.is_count_and_sum_bearing());
        // ProvableCountProvableSumTree is the dual-axis variant: both
        // count and sum aggregates are carried, and NotCountedOrSummed
        // children are accepted.
        assert!(TreeType::ProvableCountProvableSumTree.is_count_and_sum_bearing());

        // Equivalence: is_count_and_sum_bearing if and only if both is_count_bearing
        // and is_sum_bearing.
        for tt in [
            TreeType::NormalTree,
            TreeType::SumTree,
            TreeType::BigSumTree,
            TreeType::CountTree,
            TreeType::CountSumTree,
            TreeType::ProvableCountTree,
            TreeType::ProvableCountSumTree,
            TreeType::ProvableSumTree,
            TreeType::ProvableCountProvableSumTree,
            TreeType::CommitmentTree(0),
            TreeType::MmrTree,
            TreeType::BulkAppendTree(0),
            TreeType::DenseAppendOnlyFixedSizeTree(0),
        ] {
            assert_eq!(
                tt.is_count_and_sum_bearing(),
                tt.is_count_bearing() && tt.is_sum_bearing(),
                "mismatch for {:?}",
                tt
            );
        }
    }

    #[test]
    fn accepts_non_counted_children() {
        // Allowed: non-provable count-bearing parents.
        assert!(TreeType::CountTree.accepts_non_counted_children());
        assert!(TreeType::CountSumTree.accepts_non_counted_children());
        // Rejected: provable count-bearing parents (cryptographic count
        // would diverge from actual element count). Includes the
        // dual-axis PCPS host — its count is hash-committed too. All
        // indexed-tree primaries are Provable* and rejected for the
        // same reason.
        assert!(!TreeType::ProvableCountTree.accepts_non_counted_children());
        assert!(!TreeType::ProvableCountSumTree.accepts_non_counted_children());
        assert!(!TreeType::ProvableCountProvableSumTree.accepts_non_counted_children());
        assert!(!TreeType::ProvableSumIndexedTree.accepts_non_counted_children());
        assert!(!TreeType::ProvableCountIndexedTree.accepts_non_counted_children());
        assert!(!TreeType::ProvableCountProvableSumIndexedTree.accepts_non_counted_children());
        // Everything else: also rejected.
        assert!(!TreeType::NormalTree.accepts_non_counted_children());
        assert!(!TreeType::SumTree.accepts_non_counted_children());
        assert!(!TreeType::BigSumTree.accepts_non_counted_children());
        assert!(!TreeType::CommitmentTree(0).accepts_non_counted_children());
        assert!(!TreeType::MmrTree.accepts_non_counted_children());
        assert!(!TreeType::BulkAppendTree(0).accepts_non_counted_children());
        assert!(!TreeType::DenseAppendOnlyFixedSizeTree(0).accepts_non_counted_children());
        assert!(!TreeType::ProvableSumTree.accepts_non_counted_children());

        // Implication: every parent that accepts a NonCounted child is
        // count-bearing, but not vice-versa (Provable* are count-bearing
        // and reject the wrapper).
        for tt in [
            TreeType::NormalTree,
            TreeType::SumTree,
            TreeType::BigSumTree,
            TreeType::CountTree,
            TreeType::CountSumTree,
            TreeType::ProvableCountTree,
            TreeType::ProvableCountSumTree,
            TreeType::ProvableSumTree,
            TreeType::ProvableCountProvableSumTree,
            TreeType::ProvableSumIndexedTree,
            TreeType::ProvableCountIndexedTree,
            TreeType::ProvableCountProvableSumIndexedTree,
        ] {
            if tt.accepts_non_counted_children() {
                assert!(tt.is_count_bearing(), "{:?}", tt);
            }
        }
    }

    #[test]
    fn accepts_not_counted_or_summed_children() {
        // Only CountSumTree accepts NotCountedOrSummed.
        assert!(TreeType::CountSumTree.accepts_not_counted_or_summed_children());
        // Provable count-and-sum-bearing parents are rejected — committed
        // count (and, for PCPS, sum) would diverge from actual contents.
        assert!(!TreeType::ProvableCountSumTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::ProvableCountProvableSumTree.accepts_not_counted_or_summed_children());
        // Single-axis trees: rejected (would suppress an axis they don't track).
        assert!(!TreeType::NormalTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::SumTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::BigSumTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::CountTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::ProvableCountTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::CommitmentTree(0).accepts_not_counted_or_summed_children());
        assert!(!TreeType::MmrTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::BulkAppendTree(0).accepts_not_counted_or_summed_children());
        assert!(!TreeType::DenseAppendOnlyFixedSizeTree(0).accepts_not_counted_or_summed_children());
        assert!(!TreeType::ProvableSumTree.accepts_not_counted_or_summed_children());
        // Indexed-tree primaries are all Provable*; the wrapper is rejected
        // for the same reason as on plain Provable* trees.
        assert!(!TreeType::ProvableSumIndexedTree.accepts_not_counted_or_summed_children());
        assert!(!TreeType::ProvableCountIndexedTree.accepts_not_counted_or_summed_children());
        assert!(
            !TreeType::ProvableCountProvableSumIndexedTree.accepts_not_counted_or_summed_children()
        );
    }

    #[test]
    fn allows_sum_item() {
        assert!(!TreeType::NormalTree.allows_sum_item());
        assert!(TreeType::SumTree.allows_sum_item());
        assert!(TreeType::BigSumTree.allows_sum_item());
        assert!(!TreeType::CountTree.allows_sum_item());
        assert!(TreeType::CountSumTree.allows_sum_item());
        assert!(!TreeType::ProvableCountTree.allows_sum_item());
        assert!(TreeType::ProvableCountSumTree.allows_sum_item());
        assert!(!TreeType::CommitmentTree(0).allows_sum_item());
        assert!(!TreeType::MmrTree.allows_sum_item());
        assert!(!TreeType::BulkAppendTree(0).allows_sum_item());
        assert!(!TreeType::DenseAppendOnlyFixedSizeTree(0).allows_sum_item());
        assert!(TreeType::ProvableSumTree.allows_sum_item());
        assert!(TreeType::ProvableCountProvableSumTree.allows_sum_item());
    }

    #[test]
    fn empty_tree_feature_type_all_variants() {
        assert_eq!(
            TreeType::NormalTree.empty_tree_feature_type(),
            TreeFeatureType::BasicMerkNode
        );
        assert_eq!(
            TreeType::SumTree.empty_tree_feature_type(),
            TreeFeatureType::SummedMerkNode(0)
        );
        assert_eq!(
            TreeType::BigSumTree.empty_tree_feature_type(),
            TreeFeatureType::BigSummedMerkNode(0)
        );
        assert_eq!(
            TreeType::CountTree.empty_tree_feature_type(),
            TreeFeatureType::CountedMerkNode(0)
        );
        assert_eq!(
            TreeType::CountSumTree.empty_tree_feature_type(),
            TreeFeatureType::CountedSummedMerkNode(0, 0)
        );
        assert_eq!(
            TreeType::ProvableCountTree.empty_tree_feature_type(),
            TreeFeatureType::ProvableCountedMerkNode(0)
        );
        assert_eq!(
            TreeType::ProvableCountSumTree.empty_tree_feature_type(),
            TreeFeatureType::ProvableCountedSummedMerkNode(0, 0)
        );
        assert_eq!(
            TreeType::CommitmentTree(0).empty_tree_feature_type(),
            TreeFeatureType::BasicMerkNode
        );
        assert_eq!(
            TreeType::MmrTree.empty_tree_feature_type(),
            TreeFeatureType::BasicMerkNode
        );
        assert_eq!(
            TreeType::BulkAppendTree(0).empty_tree_feature_type(),
            TreeFeatureType::BasicMerkNode
        );
        assert_eq!(
            TreeType::DenseAppendOnlyFixedSizeTree(0).empty_tree_feature_type(),
            TreeFeatureType::BasicMerkNode
        );
        assert_eq!(
            TreeType::ProvableSumTree.empty_tree_feature_type(),
            TreeFeatureType::ProvableSummedMerkNode(0)
        );
        assert_eq!(
            TreeType::ProvableCountProvableSumTree.empty_tree_feature_type(),
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(0, 0)
        );
    }

    #[test]
    fn to_element_type_all_variants() {
        assert_eq!(
            TreeType::NormalTree.to_element_type(),
            Some(ElementType::Tree)
        );
        assert_eq!(
            TreeType::SumTree.to_element_type(),
            Some(ElementType::SumTree)
        );
        assert_eq!(
            TreeType::BigSumTree.to_element_type(),
            Some(ElementType::BigSumTree)
        );
        assert_eq!(
            TreeType::CountTree.to_element_type(),
            Some(ElementType::CountTree)
        );
        assert_eq!(
            TreeType::CountSumTree.to_element_type(),
            Some(ElementType::CountSumTree)
        );
        assert_eq!(
            TreeType::ProvableCountTree.to_element_type(),
            Some(ElementType::ProvableCountTree)
        );
        assert_eq!(
            TreeType::ProvableCountSumTree.to_element_type(),
            Some(ElementType::ProvableCountSumTree)
        );
        assert_eq!(
            TreeType::CommitmentTree(0).to_element_type(),
            Some(ElementType::CommitmentTree)
        );
        assert_eq!(
            TreeType::MmrTree.to_element_type(),
            Some(ElementType::MmrTree)
        );
        assert_eq!(
            TreeType::BulkAppendTree(0).to_element_type(),
            Some(ElementType::BulkAppendTree)
        );
        assert_eq!(
            TreeType::DenseAppendOnlyFixedSizeTree(0).to_element_type(),
            Some(ElementType::DenseAppendOnlyFixedSizeTree)
        );
        assert_eq!(
            TreeType::ProvableSumTree.to_element_type(),
            Some(ElementType::ProvableSumTree)
        );
        assert_eq!(
            TreeType::ProvableCountProvableSumTree.to_element_type(),
            Some(ElementType::ProvableCountProvableSumTree)
        );
    }

    /// The wide count and sum tree: discriminant 17, a Merk-backed tree
    /// that carries a count and a sum, takes sum items, and, committing
    /// both into its node hashes, refuses children that hide either.
    #[test]
    fn provable_count_provable_big_sum_tree_type() {
        let tree_type = TreeType::ProvableCountProvableBigSumTree;
        assert_eq!(tree_type.discriminant(), 17);
        assert_eq!(TreeType::try_from(17u8).unwrap(), tree_type);
        assert_eq!(
            tree_type.to_string(),
            "Provable Count Provable Big Sum Tree"
        );

        assert!(!tree_type.uses_non_merk_data_storage());
        assert!(tree_type.is_count_bearing());
        assert!(tree_type.is_sum_bearing());
        assert!(tree_type.is_count_and_sum_bearing());
        assert!(!tree_type.is_indexed_primary());
        assert!(tree_type.allows_sum_item());
        assert!(!tree_type.accepts_non_counted_children());
        assert!(!tree_type.accepts_not_counted_or_summed_children());

        assert_eq!(
            tree_type.inner_node_type(),
            NodeType::ProvableCountProvableBigSumNode
        );
        assert_eq!(
            tree_type.empty_tree_feature_type(),
            TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(0, 0)
        );
        assert_eq!(
            tree_type.to_element_type(),
            Some(ElementType::ProvableCountProvableBigSumTree)
        );
        // Layer (3) + count (9) + big sum (16).
        assert_eq!(tree_type.cost_size(), 28);
    }

    #[test]
    fn count_big_sum_tree_and_provable_big_sum_tree_types() {
        let counted = TreeType::CountBigSumTree;
        assert_eq!(counted.discriminant(), 18);
        assert_eq!(TreeType::try_from(18u8).unwrap(), counted);
        assert_eq!(counted.to_string(), "Count Big Sum Tree");
        assert!(!counted.uses_non_merk_data_storage());
        assert!(counted.is_count_bearing());
        assert!(counted.is_sum_bearing());
        assert!(counted.is_count_and_sum_bearing());
        assert!(!counted.is_indexed_primary());
        assert!(counted.allows_sum_item());
        // Like `CountSumTree`, and unlike the trees with a provable count.
        assert!(counted.accepts_non_counted_children());
        assert!(counted.accepts_not_counted_or_summed_children());
        assert_eq!(counted.inner_node_type(), NodeType::CountBigSumNode);
        assert_eq!(
            counted.empty_tree_feature_type(),
            TreeFeatureType::CountedBigSummedMerkNode(0, 0)
        );
        assert_eq!(
            counted.to_element_type(),
            Some(ElementType::CountBigSumTree)
        );
        assert_eq!(counted.cost_size(), 28);

        let provable = TreeType::ProvableBigSumTree;
        assert_eq!(provable.discriminant(), 19);
        assert_eq!(TreeType::try_from(19u8).unwrap(), provable);
        assert_eq!(provable.to_string(), "Provable Big Sum Tree");
        assert!(!provable.uses_non_merk_data_storage());
        assert!(!provable.is_count_bearing());
        assert!(provable.is_sum_bearing());
        assert!(!provable.is_count_and_sum_bearing());
        assert!(!provable.is_indexed_primary());
        assert!(provable.allows_sum_item());
        assert!(!provable.accepts_non_counted_children());
        assert!(!provable.accepts_not_counted_or_summed_children());
        assert_eq!(provable.inner_node_type(), NodeType::ProvableBigSumNode);
        assert_eq!(
            provable.empty_tree_feature_type(),
            TreeFeatureType::ProvableBigSummedMerkNode(0)
        );
        assert_eq!(
            provable.to_element_type(),
            Some(ElementType::ProvableBigSumTree)
        );
        assert_eq!(provable.cost_size(), 19);
    }
}
