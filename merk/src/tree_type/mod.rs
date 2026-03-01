#[cfg(feature = "minimal")]
mod costs;
use std::fmt;

#[cfg(feature = "minimal")]
pub use costs::*;
use grovedb_element::ElementType;

#[cfg(feature = "minimal")]
use crate::merk::NodeType;
use crate::{Error, TreeFeatureType};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum MaybeTree {
    Tree(TreeType),
    NotTree,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TreeType {
    NormalTree,
    SumTree,
    BigSumTree,
    CountTree,
    CountSumTree,
    ProvableCountTree,
    ProvableCountSumTree,
    CommitmentTree(u8),
    MmrTree,
    BulkAppendTree(u8),
    DenseAppendOnlyFixedSizeTree(u8),
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
            n => Err(Error::UnknownTreeType(format!("got {}, max is 10", n))),
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
        };
        write!(f, "{}", s)
    }
}

impl TreeType {
    /// Returns true for tree types that store data in the data namespace as
    /// non-Merk entries.  These types have an always-empty Merk subtree and
    /// never contain child subtrees.
    pub fn uses_non_merk_data_storage(&self) -> bool {
        matches!(
            self,
            TreeType::CommitmentTree(_)
                | TreeType::MmrTree
                | TreeType::BulkAppendTree(_)
                | TreeType::DenseAppendOnlyFixedSizeTree(_)
        )
    }

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
        }
    }

    #[cfg(feature = "minimal")]
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
        }
    }

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
        }
    }
}
