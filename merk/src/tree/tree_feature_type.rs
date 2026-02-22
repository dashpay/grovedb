//! Merk tree feature type

#[cfg(feature = "minimal")]
use grovedb_costs::TreeCostType;
// Re-export TreeFeatureType from grovedb-query
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_query::proofs::TreeFeatureType;
#[cfg(feature = "minimal")]
use integer_encoding::VarInt;

#[cfg(any(feature = "minimal", feature = "verify"))]
use self::TreeFeatureType::{
    BasicMerkNode, BigSummedMerkNode, CountedMerkNode, CountedSummedMerkNode, SummedMerkNode,
};
#[cfg(feature = "minimal")]
use crate::merk::NodeType;
#[cfg(feature = "minimal")]
use crate::tree_type::TreeType;

/// Extension trait for TreeFeatureType methods specific to merk
#[cfg(feature = "minimal")]
pub trait TreeFeatureTypeMerkExt {
    /// Get the NodeType for this feature type
    fn node_type(&self) -> NodeType;

    /// Get encoding cost of self
    fn encoding_cost(&self) -> usize;

    /// Get length of encoded feature type with TreeCostType
    fn tree_feature_specialized_type_and_length(&self) -> Option<(TreeCostType, u32)>;
}

#[cfg(feature = "minimal")]
impl TreeFeatureTypeMerkExt for TreeFeatureType {
    fn node_type(&self) -> NodeType {
        match self {
            BasicMerkNode => NodeType::NormalNode,
            SummedMerkNode(_) => NodeType::SumNode,
            BigSummedMerkNode(_) => NodeType::BigSumNode,
            CountedMerkNode(_) => NodeType::CountNode,
            CountedSummedMerkNode(..) => NodeType::CountSumNode,
            TreeFeatureType::ProvableCountedMerkNode(_) => NodeType::ProvableCountNode,
            TreeFeatureType::ProvableCountedSummedMerkNode(..) => NodeType::ProvableCountSumNode,
        }
    }

    #[inline]
    fn encoding_cost(&self) -> usize {
        match self {
            BasicMerkNode => 1,
            SummedMerkNode(_sum) => 9,
            BigSummedMerkNode(_) => 17,
            CountedMerkNode(_) => 9,
            CountedSummedMerkNode(..) => 17,
            TreeFeatureType::ProvableCountedMerkNode(_) => 9,
            TreeFeatureType::ProvableCountedSummedMerkNode(..) => 17,
        }
    }

    #[inline]
    fn tree_feature_specialized_type_and_length(&self) -> Option<(TreeCostType, u32)> {
        match self {
            BasicMerkNode => None,
            SummedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            BigSummedMerkNode(_) => Some((TreeCostType::TreeFeatureUses16Bytes, 16)),
            CountedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            CountedSummedMerkNode(count, sum) => Some((
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
                count.encode_var_vec().len() as u32 + sum.encode_var_vec().len() as u32,
            )),
            TreeFeatureType::ProvableCountedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            TreeFeatureType::ProvableCountedSummedMerkNode(count, sum) => Some((
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
                count.encode_var_vec().len() as u32 + sum.encode_var_vec().len() as u32,
            )),
        }
    }
}

#[cfg(feature = "minimal")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AggregateData {
    NoAggregateData,
    Sum(i64),
    BigSum(i128),
    Count(u64),
    CountAndSum(u64, i64),
    ProvableCount(u64),
    ProvableCountAndSum(u64, i64),
}

#[cfg(feature = "minimal")]
impl AggregateData {
    pub fn parent_tree_type(&self) -> TreeType {
        match self {
            AggregateData::NoAggregateData => TreeType::NormalTree,
            AggregateData::Sum(_) => TreeType::SumTree,
            AggregateData::BigSum(_) => TreeType::BigSumTree,
            AggregateData::Count(_) => TreeType::CountTree,
            AggregateData::CountAndSum(..) => TreeType::CountSumTree,
            AggregateData::ProvableCount(_) => TreeType::ProvableCountTree,
            AggregateData::ProvableCountAndSum(..) => TreeType::ProvableCountSumTree,
        }
    }

    pub fn as_sum_i64(&self) -> i64 {
        match self {
            AggregateData::NoAggregateData => 0,
            AggregateData::Sum(s) => *s,
            AggregateData::BigSum(i) => {
                let max = i64::MAX as i128;
                if *i > max {
                    i64::MAX
                } else {
                    *i as i64
                }
            }
            AggregateData::Count(_) => 0,
            AggregateData::CountAndSum(_, s) => *s,
            AggregateData::ProvableCount(_) => 0,
            AggregateData::ProvableCountAndSum(_, s) => *s,
        }
    }

    pub fn as_count_u64(&self) -> u64 {
        match self {
            AggregateData::NoAggregateData => 0,
            AggregateData::Sum(_) => 0,
            AggregateData::BigSum(_) => 0,
            AggregateData::Count(c) => *c,
            AggregateData::CountAndSum(c, _) => *c,
            AggregateData::ProvableCount(c) => *c,
            AggregateData::ProvableCountAndSum(c, _) => *c,
        }
    }

    pub fn as_summed_i128(&self) -> i128 {
        match self {
            AggregateData::NoAggregateData => 0,
            AggregateData::Sum(s) => *s as i128,
            AggregateData::BigSum(i) => *i,
            AggregateData::Count(_) => 0,
            AggregateData::CountAndSum(_, s) => *s as i128,
            AggregateData::ProvableCount(_) => 0,
            AggregateData::ProvableCountAndSum(_, s) => *s as i128,
        }
    }
}

#[cfg(feature = "minimal")]
impl From<TreeFeatureType> for AggregateData {
    fn from(value: TreeFeatureType) -> Self {
        match value {
            BasicMerkNode => AggregateData::NoAggregateData,
            SummedMerkNode(val) => AggregateData::Sum(val),
            BigSummedMerkNode(val) => AggregateData::BigSum(val),
            CountedMerkNode(val) => AggregateData::Count(val),
            CountedSummedMerkNode(count, sum) => AggregateData::CountAndSum(count, sum),
            TreeFeatureType::ProvableCountedMerkNode(val) => AggregateData::ProvableCount(val),
            TreeFeatureType::ProvableCountedSummedMerkNode(count, sum) => {
                AggregateData::ProvableCountAndSum(count, sum)
            }
        }
    }
}
