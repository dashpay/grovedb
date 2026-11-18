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

use ed::Encode;
pub use grovedb_costs::cost_model::{
    BatchCharge, ImmediateBatchCharge, Keyspace, StorageCostModel, PREFIX_LEN,
};
use grovedb_version::version::GroveVersion;

use crate::{tree::TreeNode, Error};

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
/// from the lengths of its parts. It is what a fetch of the node loads,
/// whatever a store keeps of it.
///
/// The feature of the node and each of its links are measured by their own
/// encoders. A kind of feature or of aggregate that Merk gains is therefore
/// measured as Merk encodes it, with no list of kinds to keep here.
///
/// A node that cannot be encoded has no record length: one with a link
/// that is modified and not yet committed.
pub fn node_record_len(model: StorageCostModel, node: &TreeNode) -> Result<usize, Error> {
    let feature_len = node
        .feature_type()
        .encoding_length()
        .map_err(Error::EdError)?;
    let link_len = |left: bool| {
        node.link(left)
            .map(Encode::encoding_length)
            .transpose()
            .map_err(Error::EdError)
    };
    Ok(model.node_record_len(
        node.value_as_slice().len(),
        feature_len,
        link_len(true)?,
        link_len(false)?,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ed::Decode;
    use grovedb_version::version::GROVE_VERSIONS;

    use super::*;
    use crate::{
        tree::AggregateData,
        Link,
        TreeFeatureType::{self, *},
    };

    const MODEL: StorageCostModel = StorageCostModel::V0;
    const COUNTS: [u64; 5] = [0, 127, 128, 70_000, u64::MAX];
    const SUMS: [i64; 6] = [0, -1, 63, -65, i64::MAX, i64::MIN];
    const BIG_SUMS: [i128; 3] = [0, -7, i128::MAX];

    fn with_counts_and<S: Copy>(
        sums: &[S],
        make: fn(u64, S) -> TreeFeatureType,
    ) -> Vec<TreeFeatureType> {
        COUNTS
            .iter()
            .flat_map(|count| sums.iter().map(move |sum| make(*count, *sum)))
            .collect()
    }

    /// The values of one kind of feature to measure, and the kind to go on
    /// with. Every kind is named, so a kind Merk gains does not compile
    /// until it is given values here.
    fn values_and_next_kind(
        kind: TreeFeatureType,
    ) -> (Vec<TreeFeatureType>, Option<TreeFeatureType>) {
        match kind {
            BasicMerkNode => (vec![BasicMerkNode], Some(SummedMerkNode(0))),
            SummedMerkNode(_) => (
                SUMS.map(SummedMerkNode).to_vec(),
                Some(BigSummedMerkNode(0)),
            ),
            BigSummedMerkNode(_) => (
                BIG_SUMS.map(BigSummedMerkNode).to_vec(),
                Some(CountedMerkNode(0)),
            ),
            CountedMerkNode(_) => (
                COUNTS.map(CountedMerkNode).to_vec(),
                Some(CountedSummedMerkNode(0, 0)),
            ),
            CountedSummedMerkNode(..) => (
                with_counts_and(&SUMS, CountedSummedMerkNode),
                Some(ProvableCountedMerkNode(0)),
            ),
            ProvableCountedMerkNode(_) => (
                COUNTS.map(ProvableCountedMerkNode).to_vec(),
                Some(ProvableCountedSummedMerkNode(0, 0)),
            ),
            ProvableCountedSummedMerkNode(..) => (
                with_counts_and(&SUMS, ProvableCountedSummedMerkNode),
                Some(ProvableSummedMerkNode(0)),
            ),
            ProvableSummedMerkNode(_) => (
                SUMS.map(ProvableSummedMerkNode).to_vec(),
                Some(ProvableCountedAndProvableSummedMerkNode(0, 0)),
            ),
            ProvableCountedAndProvableSummedMerkNode(..) => (
                with_counts_and(&SUMS, ProvableCountedAndProvableSummedMerkNode),
                Some(ProvableCountedAndProvableBigSummedMerkNode(0, 0)),
            ),
            ProvableCountedAndProvableBigSummedMerkNode(..) => (
                with_counts_and(&BIG_SUMS, ProvableCountedAndProvableBigSummedMerkNode),
                Some(CountedBigSummedMerkNode(0, 0)),
            ),
            CountedBigSummedMerkNode(..) => (
                with_counts_and(&BIG_SUMS, CountedBigSummedMerkNode),
                Some(ProvableBigSummedMerkNode(0)),
            ),
            ProvableBigSummedMerkNode(_) => {
                (BIG_SUMS.map(ProvableBigSummedMerkNode).to_vec(), None)
            }
        }
    }

    fn features() -> Vec<TreeFeatureType> {
        std::iter::successors(Some(values_and_next_kind(BasicMerkNode)), |(_, next)| {
            next.map(values_and_next_kind)
        })
        .flat_map(|(values, _)| values)
        .collect()
    }

    fn link(key_len: usize, aggregate_of: TreeFeatureType) -> Link {
        Link::Reference {
            hash: [3; 32],
            child_heights: (1, 2),
            key: vec![0xCD; key_len],
            aggregate_data: AggregateData::from(aggregate_of),
        }
    }

    fn node(
        value_len: usize,
        feature: TreeFeatureType,
        left: Option<Link>,
        right: Option<Link>,
    ) -> TreeNode {
        TreeNode::from_fields(
            b"key".to_vec(),
            vec![0x5A; value_len],
            [7; 32],
            left,
            right,
            feature,
        )
        .unwrap()
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
        for (index, feature) in all.iter().enumerate() {
            let other = all[(index * 7 + 3) % all.len()];
            for (left, right) in [
                (None, None),
                (Some(link(0, other)), None),
                (None, Some(link(255, *feature))),
                (Some(link(32, *feature)), Some(link(1, other))),
            ] {
                for value_len in [0, 1, 200, 70_000] {
                    let node = node(value_len, *feature, left.clone(), right.clone());
                    let record_len = node_record_len(MODEL, &node).expect("a node Merk encodes");
                    assert_eq!(record_len, node.encode().len(), "{feature:?}");
                    assert_eq!(record_len, node.encoding_length(), "{feature:?}");
                }
            }
        }
    }

    #[test]
    fn the_sizes_merk_gives_with_a_node_write_add_up_to_its_record_length() {
        let all = features();
        for (index, feature) in all.iter().enumerate() {
            let other = all[(index * 5 + 1) % all.len()];
            let node = node(9, *feature, Some(link(4, other)), Some(link(32, *feature)));
            let feature_len = feature
                .tree_feature_specialized_type_and_length()
                .map_or(0, |(_, len)| len as usize);
            let link_len = |left: bool| {
                node.child_ref_and_sum_size(left)
                    .map(|(key_and_fixed_len, aggregate_len)| {
                        (key_and_fixed_len + aggregate_len) as usize
                    })
            };
            assert_eq!(
                MODEL.node_record_len(9, 1 + feature_len, link_len(true), link_len(false)),
                node.encode().len(),
                "{feature:?}"
            );
        }
    }

    #[test]
    fn every_kind_merk_decodes_is_measured() {
        let all = features();
        let feature_tags: BTreeSet<u8> = all
            .iter()
            .map(|feature| feature.encode().unwrap()[0])
            .collect();
        let aggregate_tag_at = 1 + 4 + 32 + 2;
        let aggregate_tags: BTreeSet<u8> = all
            .iter()
            .map(|feature| link(4, *feature).encode().unwrap()[aggregate_tag_at])
            .collect();
        assert_eq!(feature_tags, aggregate_tags);

        for tag in 0..=u8::MAX {
            let mut feature = vec![tag];
            feature.extend([0; 40]);
            assert_eq!(
                TreeFeatureType::decode(feature.as_slice()).is_ok(),
                feature_tags.contains(&tag),
                "feature tag {tag}"
            );

            let mut link = link(4, BasicMerkNode).encode().unwrap();
            link[aggregate_tag_at] = tag;
            link.extend([0; 40]);
            assert_eq!(
                Link::decode(link.as_slice()).is_ok(),
                aggregate_tags.contains(&tag),
                "aggregate tag {tag}"
            );
        }
    }

    #[test]
    fn a_node_that_cannot_be_encoded_has_no_record_length() {
        let unencodable = node(1, BasicMerkNode, None, Some(link(256, BasicMerkNode)));
        assert!(matches!(
            node_record_len(MODEL, &unencodable),
            Err(Error::EdError(_))
        ));
        let unencodable = node(1, BasicMerkNode, Some(link(256, BasicMerkNode)), None);
        assert!(matches!(
            node_record_len(MODEL, &unencodable),
            Err(Error::EdError(_))
        ));
    }
}
