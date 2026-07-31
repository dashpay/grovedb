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
/// Aggregate data associated with tree nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AggregateData {
    /// No aggregate data.
    NoAggregateData,
    /// A signed 64-bit sum value.
    Sum(i64),
    /// A signed 128-bit sum value for large sums.
    BigSum(i128),
    /// An unsigned 64-bit element count.
    Count(u64),
    /// A combined element count and sum.
    CountAndSum(u64, i64),
    /// A provable unsigned 64-bit element count.
    ProvableCount(u64),
    /// A provable combined element count and sum.
    ProvableCountAndSum(u64, i64),
    /// A provable signed 64-bit sum value (sum baked into node hash).
    ///
    /// Distinct from `Sum` so the hash dispatch in `Tree::hash_for_link` and
    /// the `commit` path can route a `ProvableSumTree` aggregate through
    /// `node_hash_with_sum` instead of the plain `node_hash`. Arithmetic
    /// semantics are identical to `Sum` (i64, checked-add aggregation);
    /// only the hash treatment differs.
    ProvableSum(i64),
    /// A provable count AND provable sum, with BOTH baked into the node
    /// hash via `node_hash_with_count_and_sum`.
    ///
    /// Distinct from `ProvableCountAndSum` (which carries the same
    /// `(u64, i64)` payload but only hashes the count, used by
    /// `ProvableCountSumTree`). The variant tag is what the hash dispatch
    /// uses to route this aggregate through the dual-axis hash function,
    /// so `ProvableCountAndProvableSum` cannot be unified with
    /// `ProvableCountAndSum` even though the fields are identical.
    ProvableCountAndProvableSum(u64, i64),
}

#[cfg(feature = "minimal")]
impl AggregateData {
    /// Returns the tree type corresponding to this aggregate data variant.
    pub fn parent_tree_type(&self) -> TreeType {
        match self {
            AggregateData::NoAggregateData => TreeType::NormalTree,
            AggregateData::Sum(_) => TreeType::SumTree,
            AggregateData::BigSum(_) => TreeType::BigSumTree,
            AggregateData::Count(_) => TreeType::CountTree,
            AggregateData::CountAndSum(..) => TreeType::CountSumTree,
            AggregateData::ProvableCount(_) => TreeType::ProvableCountTree,
            AggregateData::ProvableCountAndSum(..) => TreeType::ProvableCountSumTree,
            AggregateData::ProvableSum(_) => TreeType::ProvableSumTree,
            AggregateData::ProvableCountAndProvableSum(..) => {
                TreeType::ProvableCountProvableSumTree
            }
        }
    }

    /// Returns the INDEXED tree type for this aggregate, for callers that
    /// already know from context that the tree is an indexed primary.
    ///
    /// [`parent_tree_type`](Self::parent_tree_type) cannot answer this. An
    /// indexed primary carries exactly the same aggregate payload as its
    /// non-indexed counterpart — a `ProvableCountProvableSumIndexedTree` and a
    /// `ProvableCountProvableSumTree` both carry
    /// `ProvableCountAndProvableSum` — so the payload alone is ambiguous and
    /// `parent_tree_type` resolves it to the non-indexed type. The
    /// disambiguation has to come from the caller: the estimators reach this
    /// only from `ReplaceAggregateIndexedTreeRootKeys`, an op emitted solely
    /// for indexed primaries.
    ///
    /// The distinction is not cosmetic. An indexed element serializes its
    /// per-axis secondary state, so the non-indexed type's `cost_size()` is
    /// *smaller* than the indexed element's own minimum payload — 21 against
    /// 28 bytes for PCPSIT — and using it under-charges the estimate.
    ///
    /// Returns `None` for the variants no indexed tree can carry.
    pub fn indexed_parent_tree_type(&self) -> Option<TreeType> {
        match self {
            AggregateData::ProvableCount(_) => Some(TreeType::ProvableCountIndexedTree),
            AggregateData::ProvableSum(_) => Some(TreeType::ProvableSumIndexedTree),
            AggregateData::ProvableCountAndProvableSum(..) => {
                Some(TreeType::ProvableCountProvableSumIndexedTree)
            }
            _ => None,
        }
    }

    /// Returns the sum value as `i64`, or 0 if not a sum variant.
    pub fn as_sum_i64(&self) -> i64 {
        match self {
            AggregateData::NoAggregateData => 0,
            AggregateData::Sum(s) => *s,
            AggregateData::BigSum(i) => {
                if *i > i64::MAX as i128 {
                    i64::MAX
                } else if *i < i64::MIN as i128 {
                    i64::MIN
                } else {
                    *i as i64
                }
            }
            AggregateData::Count(_) => 0,
            AggregateData::CountAndSum(_, s) => *s,
            AggregateData::ProvableCount(_) => 0,
            AggregateData::ProvableCountAndSum(_, s) => *s,
            AggregateData::ProvableSum(s) => *s,
            AggregateData::ProvableCountAndProvableSum(_, s) => *s,
        }
    }

    /// Returns the count value as `u64`, or 0 if not a count variant.
    pub fn as_count_u64(&self) -> u64 {
        match self {
            AggregateData::NoAggregateData => 0,
            AggregateData::Sum(_) => 0,
            AggregateData::BigSum(_) => 0,
            AggregateData::Count(c) => *c,
            AggregateData::CountAndSum(c, _) => *c,
            AggregateData::ProvableCount(c) => *c,
            AggregateData::ProvableCountAndSum(c, _) => *c,
            AggregateData::ProvableSum(_) => 0,
            AggregateData::ProvableCountAndProvableSum(c, _) => *c,
        }
    }

    /// Returns the sum value as `i128`, or 0 if not a sum variant.
    pub fn as_summed_i128(&self) -> i128 {
        match self {
            AggregateData::NoAggregateData => 0,
            AggregateData::Sum(s) => *s as i128,
            AggregateData::BigSum(i) => *i,
            AggregateData::Count(_) => 0,
            AggregateData::CountAndSum(_, s) => *s as i128,
            AggregateData::ProvableCount(_) => 0,
            AggregateData::ProvableCountAndSum(_, s) => *s as i128,
            AggregateData::ProvableSum(s) => *s as i128,
            AggregateData::ProvableCountAndProvableSum(_, s) => *s as i128,
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
            // `ProvableSummedMerkNode` maps to its own
            // `AggregateData::ProvableSum` variant so the hash dispatch
            // (in `Tree::hash_for_link` and `commit`) can route a
            // ProvableSumTree through `node_hash_with_sum`. Arithmetic
            // semantics still mirror a plain `Sum` aggregation.
            TreeFeatureType::ProvableSummedMerkNode(val) => AggregateData::ProvableSum(val),
            // `ProvableCountedAndProvableSummedMerkNode` carries both
            // axes and the hash dispatch routes it through
            // `node_hash_with_count_and_sum`. Distinct from
            // `ProvableCountedSummedMerkNode` (which only hashes the
            // count) — see `AggregateData::ProvableCountAndProvableSum`.
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                AggregateData::ProvableCountAndProvableSum(count, sum)
            }
        }
    }
}

#[cfg(test)]
#[cfg(feature = "minimal")]
mod tests {
    use super::*;

    /// The indexed mapping must NOT collapse onto `parent_tree_type`.
    ///
    /// Each indexed primary carries the same aggregate payload as its
    /// non-indexed counterpart, so a regression to `parent_tree_type` is
    /// invisible unless something asserts the two disagree — and it
    /// under-sizes every indexed element in the estimators.
    #[test]
    fn indexed_parent_tree_type_disagrees_with_parent_tree_type() {
        use crate::tree_type::CostSize;

        for agg in [
            AggregateData::ProvableCount(3),
            AggregateData::ProvableSum(7),
            AggregateData::ProvableCountAndProvableSum(1, 2),
        ] {
            let indexed = agg
                .indexed_parent_tree_type()
                .expect("every provable aggregate maps to an indexed tree type");
            assert_ne!(
                indexed,
                agg.parent_tree_type(),
                "{agg:?} must map to a distinct INDEXED tree type"
            );
            assert!(
                indexed.cost_size() > agg.parent_tree_type().cost_size(),
                "{agg:?}: the indexed element serializes per-axis secondary \
                 state, so it cannot cost less than the non-indexed one"
            );
        }

        assert_eq!(
            AggregateData::ProvableCountAndProvableSum(1, 2)
                .indexed_parent_tree_type()
                .unwrap()
                .cost_size()
                - AggregateData::ProvableCountAndProvableSum(1, 2)
                    .parent_tree_type()
                    .cost_size(),
            7,
            "PCPSIT's axes TLV is the 7 bytes the old mapping dropped"
        );

        // Aggregates no indexed tree can carry get no indexed type.
        for agg in [
            AggregateData::NoAggregateData,
            AggregateData::Sum(1),
            AggregateData::BigSum(1),
            AggregateData::Count(1),
            AggregateData::CountAndSum(1, 2),
            AggregateData::ProvableCountAndSum(1, 2),
        ] {
            assert_eq!(agg.indexed_parent_tree_type(), None, "{agg:?}");
        }
    }

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
        assert_eq!(
            AggregateData::ProvableSum(7).parent_tree_type(),
            TreeType::ProvableSumTree
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
        // BigSum underflow => saturates to i64::MIN
        assert_eq!(
            AggregateData::BigSum(i64::MIN as i128 - 1).as_sum_i64(),
            i64::MIN
        );
        assert_eq!(AggregateData::Count(99).as_sum_i64(), 0);
        assert_eq!(AggregateData::CountAndSum(5, 20).as_sum_i64(), 20);
        assert_eq!(AggregateData::ProvableCount(3).as_sum_i64(), 0);
        assert_eq!(AggregateData::ProvableCountAndSum(1, -7).as_sum_i64(), -7);
        assert_eq!(AggregateData::ProvableSum(42).as_sum_i64(), 42);
        assert_eq!(AggregateData::ProvableSum(-1).as_sum_i64(), -1);
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
        assert_eq!(AggregateData::ProvableSum(42).as_count_u64(), 0);
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
        assert_eq!(AggregateData::ProvableSum(42).as_summed_i128(), 42);
        assert_eq!(AggregateData::ProvableSum(-1).as_summed_i128(), -1);
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
        // `ProvableSummedMerkNode` maps to its dedicated
        // `AggregateData::ProvableSum` variant.
        assert_eq!(
            AggregateData::from(TreeFeatureType::ProvableSummedMerkNode(42)),
            AggregateData::ProvableSum(42)
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::ProvableSummedMerkNode(-1)),
            AggregateData::ProvableSum(-1)
        );
        // ProvableCountedAndProvableSummedMerkNode maps to the dual-axis
        // ProvableCountAndProvableSum variant — distinct from
        // ProvableCountAndSum (which uses the count-only hash dispatch).
        assert_eq!(
            AggregateData::from(TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                7, -42
            )),
            AggregateData::ProvableCountAndProvableSum(7, -42)
        );
        assert_eq!(
            AggregateData::from(TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                u64::MAX,
                i64::MIN
            )),
            AggregateData::ProvableCountAndProvableSum(u64::MAX, i64::MIN)
        );
    }

    /// AggregateData::ProvableCountAndProvableSum coverage for the three
    /// helper accessors and parent_tree_type. Sibling to the existing
    /// per-variant tests; without these the new arm shows as uncovered.
    #[test]
    fn aggregate_data_provable_count_and_provable_sum_helpers() {
        let agg = AggregateData::ProvableCountAndProvableSum(7, -42);
        assert_eq!(
            agg.parent_tree_type(),
            TreeType::ProvableCountProvableSumTree
        );
        assert_eq!(agg.as_sum_i64(), -42);
        assert_eq!(agg.as_count_u64(), 7);
        assert_eq!(agg.as_summed_i128(), -42);

        // Extremes — both axes go through the boundary.
        let agg_max = AggregateData::ProvableCountAndProvableSum(u64::MAX, i64::MAX);
        assert_eq!(agg_max.as_sum_i64(), i64::MAX);
        assert_eq!(agg_max.as_count_u64(), u64::MAX);
        assert_eq!(agg_max.as_summed_i128(), i64::MAX as i128);

        let agg_min = AggregateData::ProvableCountAndProvableSum(0, i64::MIN);
        assert_eq!(agg_min.as_sum_i64(), i64::MIN);
        assert_eq!(agg_min.as_count_u64(), 0);
        assert_eq!(agg_min.as_summed_i128(), i64::MIN as i128);
    }
}
