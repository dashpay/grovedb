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
            n => Err(Error::UnknownTreeType(format!("got {}, max is 12", n))),
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
                | TreeType::ProvableCountTree
                | TreeType::ProvableCountSumTree
                | TreeType::ProvableCountProvableSumTree
        )
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
                | TreeType::ProvableCountSumTree
                | TreeType::ProvableSumTree
                | TreeType::ProvableCountProvableSumTree
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
                | TreeType::ProvableCountSumTree
                | TreeType::ProvableCountProvableSumTree
        )
    }

    /// Returns whether this parent tree type may host an
    /// `Element::NonCounted` child.
    ///
    /// Stricter than `is_count_bearing`: the wrapper is allowed only in
    /// the non-provable count-bearing trees (`CountTree`, `CountSumTree`).
    /// `ProvableCountTree` / `ProvableCountSumTree` bind their aggregate
    /// count into every node's hash via `node_hash_with_count`, so a
    /// `NonCounted` child would commit a cryptographic count that
    /// diverges from the actual number of stored elements — confusing
    /// for callers and a footgun for proof-driven readers. The wrapper
    /// is rejected at insert time in those parents.
    pub const fn accepts_non_counted_children(&self) -> bool {
        matches!(self, TreeType::CountTree | TreeType::CountSumTree)
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
        matches!(self, TreeType::CountSumTree)
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
        assert!(TreeType::try_from(13u8).is_err());
        assert!(TreeType::try_from(255u8).is_err());
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

        // Equivalence: is_count_and_sum_bearing iff both is_count_bearing
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
        // dual-axis PCPS host — its count is hash-committed too.
        assert!(!TreeType::ProvableCountTree.accepts_non_counted_children());
        assert!(!TreeType::ProvableCountSumTree.accepts_non_counted_children());
        assert!(!TreeType::ProvableCountProvableSumTree.accepts_non_counted_children());
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
}
