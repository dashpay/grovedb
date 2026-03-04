//! Merk tree feature type

// Re-export TreeFeatureType and NodeType from grovedb-query
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_query::proofs::{NodeType, TreeFeatureType};

#[cfg(any(feature = "minimal", feature = "verify"))]
use self::TreeFeatureType::{
    BasicMerkNode, BigSummedMerkNode, CountedMerkNode, CountedSummedMerkNode, SummedMerkNode,
};
#[cfg(feature = "minimal")]
use crate::tree_type::TreeType;

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

#[cfg(test)]
#[cfg(feature = "minimal")]
mod tests {
    use super::*;

    #[test]
    fn aggregate_data_parent_tree_type_all_variants() {
        assert_eq!(
            AggregateData::NoAggregateData.parent_tree_type(),
            TreeType::NormalTree
        );
        assert_eq!(AggregateData::Sum(42).parent_tree_type(), TreeType::SumTree);
        assert_eq!(
            AggregateData::BigSum(100).parent_tree_type(),
            TreeType::BigSumTree
        );
        assert_eq!(
            AggregateData::Count(10).parent_tree_type(),
            TreeType::CountTree
        );
        assert_eq!(
            AggregateData::CountAndSum(5, 20).parent_tree_type(),
            TreeType::CountSumTree
        );
        assert_eq!(
            AggregateData::ProvableCount(3).parent_tree_type(),
            TreeType::ProvableCountTree
        );
        assert_eq!(
            AggregateData::ProvableCountAndSum(1, 2).parent_tree_type(),
            TreeType::ProvableCountSumTree
        );
    }

    #[test]
    fn aggregate_data_as_sum_i64_all_variants() {
        assert_eq!(AggregateData::NoAggregateData.as_sum_i64(), 0);
        assert_eq!(AggregateData::Sum(42).as_sum_i64(), 42);
        assert_eq!(AggregateData::Sum(-10).as_sum_i64(), -10);
        assert_eq!(AggregateData::BigSum(100).as_sum_i64(), 100);
        // BigSum overflow => saturates to i64::MAX
        assert_eq!(
            AggregateData::BigSum(i64::MAX as i128 + 1).as_sum_i64(),
            i64::MAX
        );
        assert_eq!(AggregateData::Count(99).as_sum_i64(), 0);
        assert_eq!(AggregateData::CountAndSum(5, 20).as_sum_i64(), 20);
        assert_eq!(AggregateData::ProvableCount(3).as_sum_i64(), 0);
        assert_eq!(AggregateData::ProvableCountAndSum(1, -7).as_sum_i64(), -7);
    }

    #[test]
    fn aggregate_data_as_count_u64_all_variants() {
        assert_eq!(AggregateData::NoAggregateData.as_count_u64(), 0);
        assert_eq!(AggregateData::Sum(42).as_count_u64(), 0);
        assert_eq!(AggregateData::BigSum(100).as_count_u64(), 0);
        assert_eq!(AggregateData::Count(99).as_count_u64(), 99);
        assert_eq!(AggregateData::CountAndSum(5, 20).as_count_u64(), 5);
        assert_eq!(AggregateData::ProvableCount(3).as_count_u64(), 3);
        assert_eq!(AggregateData::ProvableCountAndSum(7, -1).as_count_u64(), 7);
    }

    #[test]
    fn aggregate_data_as_summed_i128_all_variants() {
        assert_eq!(AggregateData::NoAggregateData.as_summed_i128(), 0);
        assert_eq!(AggregateData::Sum(42).as_summed_i128(), 42);
        assert_eq!(AggregateData::BigSum(i128::MAX).as_summed_i128(), i128::MAX);
        assert_eq!(AggregateData::Count(99).as_summed_i128(), 0);
        assert_eq!(AggregateData::CountAndSum(5, -20).as_summed_i128(), -20);
        assert_eq!(AggregateData::ProvableCount(3).as_summed_i128(), 0);
        assert_eq!(
            AggregateData::ProvableCountAndSum(1, 50).as_summed_i128(),
            50
        );
    }

    #[test]
    fn aggregate_data_from_tree_feature_type_all_variants() {
        assert_eq!(
            AggregateData::from(TreeFeatureType::BasicMerkNode),
            AggregateData::NoAggregateData
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::SummedMerkNode(42)),
            AggregateData::Sum(42)
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::BigSummedMerkNode(100)),
            AggregateData::BigSum(100)
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::CountedMerkNode(10)),
            AggregateData::Count(10)
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::CountedSummedMerkNode(5, 20)),
            AggregateData::CountAndSum(5, 20)
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::ProvableCountedMerkNode(3)),
            AggregateData::ProvableCount(3)
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::ProvableCountedSummedMerkNode(1, 2)),
            AggregateData::ProvableCountAndSum(1, 2)
        );
    }
}
