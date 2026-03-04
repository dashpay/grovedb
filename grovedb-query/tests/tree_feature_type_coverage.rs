use ed::{Decode, Encode};
use grovedb_query::proofs::{NodeType, TreeFeatureType};

#[test]
fn tree_feature_type_count_returns_correct_values() {
    assert_eq!(TreeFeatureType::BasicMerkNode.count(), None);
    assert_eq!(TreeFeatureType::SummedMerkNode(10).count(), None);
    assert_eq!(TreeFeatureType::BigSummedMerkNode(10).count(), None);
    assert_eq!(TreeFeatureType::CountedMerkNode(42).count(), Some(42));
    assert_eq!(
        TreeFeatureType::CountedSummedMerkNode(5, -3).count(),
        Some(5)
    );
    assert_eq!(
        TreeFeatureType::ProvableCountedMerkNode(99).count(),
        Some(99)
    );
    assert_eq!(
        TreeFeatureType::ProvableCountedSummedMerkNode(7, 8).count(),
        Some(7)
    );
}

#[test]
fn tree_feature_type_node_type_maps_correctly() {
    assert_eq!(
        TreeFeatureType::BasicMerkNode.node_type(),
        NodeType::NormalNode
    );
    assert_eq!(
        TreeFeatureType::SummedMerkNode(0).node_type(),
        NodeType::SumNode
    );
    assert_eq!(
        TreeFeatureType::BigSummedMerkNode(0).node_type(),
        NodeType::BigSumNode
    );
    assert_eq!(
        TreeFeatureType::CountedMerkNode(0).node_type(),
        NodeType::CountNode
    );
    assert_eq!(
        TreeFeatureType::CountedSummedMerkNode(0, 0).node_type(),
        NodeType::CountSumNode
    );
    assert_eq!(
        TreeFeatureType::ProvableCountedMerkNode(0).node_type(),
        NodeType::ProvableCountNode
    );
    assert_eq!(
        TreeFeatureType::ProvableCountedSummedMerkNode(0, 0).node_type(),
        NodeType::ProvableCountSumNode
    );
}

#[test]
fn tree_feature_type_encoding_cost_matches_expected() {
    assert_eq!(TreeFeatureType::BasicMerkNode.encoding_cost(), 1);
    assert_eq!(TreeFeatureType::SummedMerkNode(0).encoding_cost(), 9);
    assert_eq!(TreeFeatureType::BigSummedMerkNode(0).encoding_cost(), 17);
    assert_eq!(TreeFeatureType::CountedMerkNode(0).encoding_cost(), 9);
    assert_eq!(
        TreeFeatureType::CountedSummedMerkNode(0, 0).encoding_cost(),
        17
    );
    assert_eq!(
        TreeFeatureType::ProvableCountedMerkNode(0).encoding_cost(),
        9
    );
    assert_eq!(
        TreeFeatureType::ProvableCountedSummedMerkNode(0, 0).encoding_cost(),
        17
    );
}

#[test]
fn node_type_feature_len_and_cost() {
    assert_eq!(NodeType::NormalNode.feature_len(), 1);
    assert_eq!(NodeType::NormalNode.cost(), 0);
    assert_eq!(NodeType::SumNode.feature_len(), 9);
    assert_eq!(NodeType::SumNode.cost(), 8);
    assert_eq!(NodeType::BigSumNode.feature_len(), 17);
    assert_eq!(NodeType::BigSumNode.cost(), 16);
    assert_eq!(NodeType::CountNode.feature_len(), 9);
    assert_eq!(NodeType::CountNode.cost(), 8);
    assert_eq!(NodeType::CountSumNode.feature_len(), 17);
    assert_eq!(NodeType::CountSumNode.cost(), 16);
    assert_eq!(NodeType::ProvableCountNode.feature_len(), 9);
    assert_eq!(NodeType::ProvableCountNode.cost(), 8);
    assert_eq!(NodeType::ProvableCountSumNode.feature_len(), 17);
    assert_eq!(NodeType::ProvableCountSumNode.cost(), 16);
}

#[test]
fn tree_feature_type_encode_decode_round_trip_all_variants() {
    let variants = vec![
        TreeFeatureType::BasicMerkNode,
        TreeFeatureType::SummedMerkNode(-42),
        TreeFeatureType::BigSummedMerkNode(i128::MAX),
        TreeFeatureType::CountedMerkNode(999),
        TreeFeatureType::CountedSummedMerkNode(10, -20),
        TreeFeatureType::ProvableCountedMerkNode(77),
        TreeFeatureType::ProvableCountedSummedMerkNode(33, 44),
    ];

    for variant in variants {
        let mut encoded = vec![];
        variant.encode_into(&mut encoded).expect("encode failed");

        let encoding_len = variant.encoding_length().unwrap();
        assert_eq!(
            encoded.len(),
            encoding_len,
            "encoding_length mismatch for {:?}",
            variant
        );

        let decoded = TreeFeatureType::decode(&encoded[..]).expect("decode failed");
        assert_eq!(decoded, variant, "round-trip mismatch for {:?}", variant);
    }
}

#[test]
fn tree_feature_type_decode_unknown_tag_errors() {
    let bytes = [7u8]; // tag 7 doesn't exist
    let err = TreeFeatureType::decode(&bytes[..]);
    assert!(err.is_err());
}
