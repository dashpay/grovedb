use std::fmt;

use crate::{
    estimated_costs::{
        BIG_SUM_LAYER_COST_SIZE, LAYER_COST_SIZE, SUM_AND_COUNT_LAYER_COST_SIZE,
        SUM_LAYER_COST_SIZE, SUM_VALUE_EXTRA_COST,
    },
    merk::NodeType,
    Error, TreeFeatureType,
};

/// The cost of a tree
pub const TREE_COST_SIZE: u32 = LAYER_COST_SIZE; // 3

/// The cost of a sum item
///
/// It is 11 because we have 9 bytes for the sum value
/// 1 byte for the item type
/// 1 byte for the flags option
pub const SUM_ITEM_COST_SIZE: u32 = SUM_VALUE_EXTRA_COST + 2; // 11

/// The cost of a sum tree
pub const SUM_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 12

/// The cost of a big sum tree
pub const BIG_SUM_TREE_COST_SIZE: u32 = BIG_SUM_LAYER_COST_SIZE; // 19

/// The cost of a count tree
pub const COUNT_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 12

/// The cost of a count tree
pub const COUNT_SUM_TREE_COST_SIZE: u32 = SUM_AND_COUNT_LAYER_COST_SIZE; // 21

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum MaybeTree {
    Tree(TreeType),
    NotTree,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TreeType {
    NormalTree = 0,
    SumTree = 1,
    BigSumTree = 2,
    CountTree = 3,
    CountSumTree = 4,
    ProvableCountTree = 5,
}

pub trait CostSize {
    fn cost_size(&self) -> u32;
}

impl CostSize for TreeType {
    fn cost_size(&self) -> u32 {
        match self {
            TreeType::NormalTree => TREE_COST_SIZE,
            TreeType::SumTree => SUM_TREE_COST_SIZE,
            TreeType::BigSumTree => BIG_SUM_TREE_COST_SIZE,
            TreeType::CountTree => COUNT_TREE_COST_SIZE,
            TreeType::CountSumTree => COUNT_SUM_TREE_COST_SIZE,
            TreeType::ProvableCountTree => COUNT_TREE_COST_SIZE,
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
            n => Err(Error::UnknownTreeType(format!("got {}, max is 5", n))), // Error handling
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
        };
        write!(f, "{}", s)
    }
}

impl TreeType {
    pub fn allows_sum_item(&self) -> bool {
        match self {
            TreeType::NormalTree => false,
            TreeType::SumTree => true,
            TreeType::BigSumTree => true,
            TreeType::CountTree => false,
            TreeType::CountSumTree => true,
            TreeType::ProvableCountTree => false,
        }
    }

    pub const fn inner_node_type(&self) -> NodeType {
        match self {
            TreeType::NormalTree => NodeType::NormalNode,
            TreeType::SumTree => NodeType::SumNode,
            TreeType::BigSumTree => NodeType::BigSumNode,
            TreeType::CountTree => NodeType::CountNode,
            TreeType::CountSumTree => NodeType::CountSumNode,
            TreeType::ProvableCountTree => NodeType::ProvableCountNode,
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
        }
    }
}
