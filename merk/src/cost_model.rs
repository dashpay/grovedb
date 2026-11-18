//! The engine-neutral cost model, as Merk and the stores under it reach it.
//!
//! What the storage layer charges is defined by the functions of
//! [`grovedb_costs::cost_model`], which are re-exported here. Each takes
//! logical sizes and returns an `OperationCost`. A store that keeps Merk's
//! nodes for an engine charges through them and works out no charge of its
//! own. The storage contexts under [`store::kv`](crate::store::kv) do so
//! today, and the [`NodeStore`](crate::store::NodeStore) of another engine
//! has to.
//!
//! This module adds what only Merk knows: which version of those rules a
//! grove version charges by, and the sizes of a node as the rules take
//! them. Merk's own cost rules, which decide what a node's key and value
//! cost and which nodes are fetched, stay where they are, in
//! [`tree`](crate::tree).
//!
//! The rules are specified in `docs/lignindb/cost-model.md`.

pub use grovedb_costs::cost_model::{
    BatchCharge, ImmediateBatchCharge, Keyspace, StorageCostModel, PREFIX_LEN,
};
use grovedb_version::version::GroveVersion;

use crate::tree::TreeNode;

/// The version of the storage layer's cost rules that `grove_version`
/// charges by.
///
/// Grove versions 1 to 5 charge by the first. The storage contexts of
/// `grovedb-storage` are opened with no grove version and charge by the
/// first as well, so a grove version that is given another has to reach
/// them too.
pub fn storage_cost_model(_grove_version: &GroveVersion) -> StorageCostModel {
    StorageCostModel::V0
}

/// The record length of `node`: the number of bytes its encoding takes,
/// from the sizes of its fields.
///
/// It is what a fetch of the node loads, whatever a store keeps of it.
pub fn node_record_len(model: StorageCostModel, node: &TreeNode) -> usize {
    let feature_len = node
        .feature_type()
        .tree_feature_specialized_type_and_length()
        .map_or(0, |(_, len)| len);
    model.node_record_len(
        node.value_as_slice().len(),
        feature_len,
        node.child_ref_and_sum_size(true),
        node.child_ref_and_sum_size(false),
    )
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GROVE_VERSIONS;

    use super::*;
    use crate::{tree::AggregateData, Link, TreeFeatureType};

    const COUNTS: [u64; 5] = [0, 127, 128, 70_000, u64::MAX];
    const SUMS: [i64; 6] = [0, -1, 63, -65, i64::MAX, i64::MIN];
    const BIG_SUMS: [i128; 3] = [0, -7, i128::MAX];

    fn features() -> Vec<TreeFeatureType> {
        let mut all = vec![TreeFeatureType::BasicMerkNode];
        for sum in SUMS {
            all.push(TreeFeatureType::SummedMerkNode(sum));
            all.push(TreeFeatureType::ProvableSummedMerkNode(sum));
        }
        for sum in BIG_SUMS {
            all.push(TreeFeatureType::BigSummedMerkNode(sum));
        }
        for count in COUNTS {
            all.push(TreeFeatureType::CountedMerkNode(count));
            all.push(TreeFeatureType::ProvableCountedMerkNode(count));
            for sum in SUMS {
                all.push(TreeFeatureType::CountedSummedMerkNode(count, sum));
                all.push(TreeFeatureType::ProvableCountedSummedMerkNode(count, sum));
                all.push(TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    count, sum,
                ));
            }
            for sum in BIG_SUMS {
                all.push(TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(count, sum));
            }
        }
        all
    }

    fn link(key_len: usize, aggregate_of: TreeFeatureType) -> Link {
        Link::Reference {
            hash: [3; 32],
            child_heights: (1, 2),
            key: vec![0xCD; key_len],
            aggregate_data: AggregateData::from(aggregate_of),
        }
    }

    #[test]
    fn every_grove_version_charges_by_the_first_storage_rules() {
        for grove_version in GROVE_VERSIONS {
            assert_eq!(storage_cost_model(grove_version), StorageCostModel::V0);
        }
    }

    #[test]
    fn the_record_length_is_the_length_of_the_record_merk_writes() {
        let all = features();
        let mut kinds = std::collections::HashSet::new();
        for (index, feature) in all.iter().enumerate() {
            let other = all[(index * 7 + 3) % all.len()];
            kinds.insert(std::mem::discriminant(feature));
            for (left, right) in [
                (None, None),
                (Some(link(0, other)), None),
                (None, Some(link(255, *feature))),
                (Some(link(32, *feature)), Some(link(1, other))),
            ] {
                for value_len in [0, 1, 200, 70_000] {
                    let node = TreeNode::from_fields(
                        b"key".to_vec(),
                        vec![0x5A; value_len],
                        [7; 32],
                        left.clone(),
                        right.clone(),
                        *feature,
                    )
                    .unwrap();
                    let record_len = node_record_len(StorageCostModel::V0, &node);
                    assert_eq!(record_len, node.encode().len(), "{feature:?}");
                    assert_eq!(record_len, node.encoding_length(), "{feature:?}");
                }
            }
        }
        assert_eq!(kinds.len(), 10);
    }
}
