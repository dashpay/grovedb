//! The logical node of [`super::node`] against Merk's own encoding.

use grovedb_merk::{
    tree::{kv::ValueDefinedCostType, AggregateData, TreeNode},
    Link, TreeFeatureType,
};
use grovedb_version::version::GroveVersion;

use super::{
    node::{Aggregate, CostClass, LogicalLink, LogicalNode, HASH_LEN},
    rules::feature_cost_size,
};

fn link(key_len: usize, aggregate: Aggregate) -> LogicalLink {
    LogicalLink {
        key: vec![0xAB; key_len],
        hash: [key_len as u8; HASH_LEN],
        left_height: 3,
        right_height: 4,
        aggregate,
    }
}

const COUNTS: [u64; 5] = [0, 127, 128, 70_000, u64::MAX];
const SUMS: [i64; 6] = [0, -1, 63, -65, i64::MAX, i64::MIN];
const BIG_SUMS: [i128; 3] = [0, -7, i128::MAX];

fn features() -> Vec<(TreeFeatureType, Aggregate)> {
    let mut all = vec![(TreeFeatureType::BasicMerkNode, Aggregate::None)];
    for sum in SUMS {
        all.push((TreeFeatureType::SummedMerkNode(sum), Aggregate::Sum(sum)));
        all.push((
            TreeFeatureType::ProvableSummedMerkNode(sum),
            Aggregate::ProvableSum(sum),
        ));
    }
    for sum in BIG_SUMS {
        all.push((
            TreeFeatureType::BigSummedMerkNode(sum),
            Aggregate::BigSum(sum),
        ));
        all.push((
            TreeFeatureType::ProvableBigSummedMerkNode(sum),
            Aggregate::ProvableBigSum(sum),
        ));
    }
    for count in COUNTS {
        all.push((
            TreeFeatureType::CountedMerkNode(count),
            Aggregate::Count(count),
        ));
        all.push((
            TreeFeatureType::ProvableCountedMerkNode(count),
            Aggregate::ProvableCount(count),
        ));
        for sum in SUMS {
            all.push((
                TreeFeatureType::CountedSummedMerkNode(count, sum),
                Aggregate::CountAndSum(count, sum),
            ));
            all.push((
                TreeFeatureType::ProvableCountedSummedMerkNode(count, sum),
                Aggregate::ProvableCountAndSum(count, sum),
            ));
            all.push((
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum),
                Aggregate::ProvableCountAndProvableSum(count, sum),
            ));
        }
        for sum in BIG_SUMS {
            all.push((
                TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(count, sum),
                Aggregate::ProvableCountAndProvableBigSum(count, sum),
            ));
            all.push((
                TreeFeatureType::CountedBigSummedMerkNode(count, sum),
                Aggregate::CountAndBigSum(count, sum),
            ));
        }
    }
    all
}

fn merk_aggregate(aggregate: &Aggregate) -> AggregateData {
    match *aggregate {
        Aggregate::None => AggregateData::NoAggregateData,
        Aggregate::Sum(sum) => AggregateData::Sum(sum),
        Aggregate::BigSum(sum) => AggregateData::BigSum(sum),
        Aggregate::Count(count) => AggregateData::Count(count),
        Aggregate::CountAndSum(count, sum) => AggregateData::CountAndSum(count, sum),
        Aggregate::ProvableCount(count) => AggregateData::ProvableCount(count),
        Aggregate::ProvableCountAndSum(count, sum) => {
            AggregateData::ProvableCountAndSum(count, sum)
        }
        Aggregate::ProvableSum(sum) => AggregateData::ProvableSum(sum),
        Aggregate::ProvableCountAndProvableSum(count, sum) => {
            AggregateData::ProvableCountAndProvableSum(count, sum)
        }
        Aggregate::ProvableCountAndProvableBigSum(count, sum) => {
            AggregateData::ProvableCountAndProvableBigSum(count, sum)
        }
        Aggregate::CountAndBigSum(count, sum) => AggregateData::CountAndBigSum(count, sum),
        Aggregate::ProvableBigSum(sum) => AggregateData::ProvableBigSum(sum),
    }
}

fn merk_link(link: &LogicalLink) -> Link {
    Link::Reference {
        hash: link.hash,
        child_heights: (link.left_height, link.right_height),
        key: link.key.clone(),
        aggregate_data: merk_aggregate(&link.aggregate),
    }
}

fn merk_node(node: &LogicalNode, feature: TreeFeatureType) -> TreeNode {
    TreeNode::from_fields(
        b"key".to_vec(),
        node.value.clone(),
        node.kv_hash,
        node.left.as_ref().map(merk_link),
        node.right.as_ref().map(merk_link),
        feature,
    )
    .unwrap()
}

#[test]
fn the_record_is_the_one_merk_writes_for_every_feature_and_link() {
    let all = features();
    for (index, (feature, aggregate)) in all.iter().enumerate() {
        let (_, other) = all[(index * 7 + 3) % all.len()];
        for (left, right) in [
            (None, None),
            (Some(link(0, other)), None),
            (None, Some(link(255, *aggregate))),
            (Some(link(32, *aggregate)), Some(link(1, other))),
        ] {
            for value_len in [0, 1, 200] {
                let mut node = LogicalNode {
                    left: left.clone(),
                    right: right.clone(),
                    feature: *aggregate,
                    kv_hash: [7; HASH_LEN],
                    value_hash: [0; HASH_LEN],
                    value: vec![0x5A; value_len],
                };
                let merk = merk_node(&node, *feature);
                node.value_hash = *merk.value_hash();
                let record = merk.encode();
                assert_eq!(node.encode(), record, "{feature:?}");
                assert_eq!(node.record_len(), record.len() as u64, "{feature:?}");
                assert_eq!(
                    node.record_len(),
                    merk.encoding_length() as u64,
                    "{feature:?}"
                );
                assert_eq!(LogicalNode::parse(&record), Some(node.clone()));

                let decoded = TreeNode::decode_raw(
                    &node.encode(),
                    b"key".to_vec(),
                    None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    GroveVersion::latest(),
                )
                .expect("Merk decodes the record");
                assert_eq!(decoded.feature_type(), *feature);
                assert_eq!(decoded.value_as_slice(), node.value.as_slice());

                for (side, link) in [(true, &node.left), (false, &node.right)] {
                    let sizes = link
                        .as_ref()
                        .map(|link| (link.key.len() as u32 + 36, link.aggregate.payload_len()));
                    assert_eq!(merk.child_ref_and_sum_size(side), sizes);
                    if let (Some(link), Some((key_len, aggregate_len))) = (link, sizes) {
                        assert_eq!(link.encoded_len(), u64::from(key_len + aggregate_len));
                    }
                }
            }
        }
    }
}

#[test]
fn the_cost_class_and_payload_length_are_merks() {
    for (feature, aggregate) in features() {
        let merk = feature.tree_feature_specialized_type_and_length();
        assert_eq!(
            merk.as_ref().map(|(_, len)| *len),
            aggregate.cost_class().map(|_| aggregate.payload_len()),
            "{feature:?}"
        );
        assert_eq!(
            merk.map(|(cost_type, _)| feature_cost_size(&cost_type)),
            aggregate.cost_class().map(CostClass::size),
            "{feature:?}"
        );
        assert_eq!(
            feature.node_type().cost(),
            aggregate.cost_class().map_or(0, CostClass::size)
        );
    }
}
