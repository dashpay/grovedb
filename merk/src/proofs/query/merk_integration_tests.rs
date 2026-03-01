macro_rules! compare_result_tuples_not_optional {
    ($result_set:expr, $expected_result_set:expr) => {
        assert_eq!(
            $expected_result_set.len(),
            $result_set.len(),
            "Result set lengths do not match"
        );
        for i in 0..$expected_result_set.len() {
            assert_eq!(
                $expected_result_set[i].0, $result_set[i].key,
                "Key mismatch at index {}",
                i
            );
            assert_eq!(
                &$expected_result_set[i].1,
                $result_set[i].value.as_ref().expect("expected value"),
                "Value mismatch at index {}",
                i
            );
        }
    };
}

use super::{
    super::{encoding::encode_into, *},
    *,
};
use crate::{
    proofs::query::verify,
    test_utils::make_tree_seq,
    tree::{NoopCommit, PanicSource, RefWalker, TreeNode},
    TreeFeatureType::BasicMerkNode,
};

fn make_3_node_tree() -> TreeNode {
    let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode)
        .unwrap()
        .attach(
            true,
            Some(TreeNode::new(vec![3], vec![3], None, BasicMerkNode).unwrap()),
        )
        .attach(
            false,
            Some(TreeNode::new(vec![7], vec![7], None, BasicMerkNode).unwrap()),
        );
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");
    tree
}

fn make_6_node_tree() -> TreeNode {
    let two_tree = TreeNode::new(vec![2], vec![2], None, BasicMerkNode).unwrap();
    let four_tree = TreeNode::new(vec![4], vec![4], None, BasicMerkNode).unwrap();
    let mut three_tree = TreeNode::new(vec![3], vec![3], None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(two_tree))
        .attach(false, Some(four_tree));
    three_tree
        .commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    let seven_tree = TreeNode::new(vec![7], vec![7], None, BasicMerkNode).unwrap();
    let mut eight_tree = TreeNode::new(vec![8], vec![8], None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(seven_tree));
    eight_tree
        .commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    let mut root_tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(three_tree))
        .attach(false, Some(eight_tree));
    root_tree
        .commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    root_tree
}

fn verify_keys_test(keys: Vec<Vec<u8>>, expected_result: Vec<Option<Vec<u8>>>) {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let (proof, ..) = walker
        .create_proof(
            keys.clone()
                .into_iter()
                .map(QueryItem::Key)
                .collect::<Vec<_>>()
                .as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");
    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    let expected_hash = [
        148, 227, 127, 84, 149, 54, 117, 188, 32, 85, 176, 25, 96, 127, 170, 90, 148, 196, 218, 30,
        5, 109, 112, 3, 120, 138, 194, 28, 27, 49, 119, 125,
    ];

    let mut query = Query::new();
    for key in keys.iter() {
        query.insert_key(key.clone());
    }

    let result = query
        .verify_proof(bytes.as_slice(), None, true, expected_hash)
        .unwrap()
        .expect("verify failed");

    let mut values = std::collections::HashMap::new();
    for proved_value in result.result_set {
        assert!(values
            .insert(proved_value.key, proved_value.value)
            .is_none());
    }

    for (key, expected_value) in keys.iter().zip(expected_result.iter()) {
        assert_eq!(
            values.get(key).and_then(|a| a.as_ref()),
            expected_value.as_ref()
        );
    }
}

#[test]
fn test_query_merge_single_key() {
    // single key test
    let mut query_one = Query::new();
    query_one.insert_key(b"a".to_vec());
    let mut query_two = Query::new();
    query_two.insert_key(b"b".to_vec());
    query_one.merge_with(query_two);
    let mut expected_query = Query::new();
    expected_query.insert_key(b"a".to_vec());
    expected_query.insert_key(b"b".to_vec());
    assert_eq!(query_one, expected_query);
}

#[test]
fn test_query_merge_range() {
    // range test
    let mut query_one = Query::new();
    query_one.insert_range(b"a".to_vec()..b"c".to_vec());
    let mut query_two = Query::new();
    query_two.insert_key(b"b".to_vec());
    query_one.merge_with(query_two);
    let mut expected_query = Query::new();
    expected_query.insert_range(b"a".to_vec()..b"c".to_vec());
    assert_eq!(query_one, expected_query);
}

#[test]
fn test_query_merge_conditional_query() {
    // conditional query test
    let mut query_one = Query::new();
    query_one.insert_key(b"a".to_vec());
    let mut insert_all_query = Query::new();
    insert_all_query.insert_all();
    query_one.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(insert_all_query));

    let mut query_two = Query::new();
    query_two.insert_key(b"b".to_vec());
    query_one.merge_with(query_two);

    let mut expected_query = Query::new();
    expected_query.insert_key(b"a".to_vec());
    expected_query.insert_key(b"b".to_vec());
    let mut insert_all_query = Query::new();
    insert_all_query.insert_all();
    expected_query.add_conditional_subquery(
        QueryItem::Key(b"a".to_vec()),
        None,
        Some(insert_all_query),
    );
    assert_eq!(query_one, expected_query);
}

#[test]
fn test_query_merge_deep_conditional_query() {
    // deep conditional query
    // [a, b, c]
    // [a, c, d]
    let mut query_one = Query::new();
    query_one.insert_key(b"a".to_vec());
    let mut query_one_b = Query::new();
    query_one_b.insert_key(b"b".to_vec());
    let mut query_one_c = Query::new();
    query_one_c.insert_key(b"c".to_vec());
    query_one_b.add_conditional_subquery(QueryItem::Key(b"b".to_vec()), None, Some(query_one_c));
    query_one.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(query_one_b));

    let mut query_two = Query::new();
    query_two.insert_key(b"a".to_vec());
    let mut query_two_c = Query::new();
    query_two_c.insert_key(b"c".to_vec());
    let mut query_two_d = Query::new();
    query_two_d.insert_key(b"d".to_vec());
    query_two_c.add_conditional_subquery(QueryItem::Key(b"c".to_vec()), None, Some(query_two_d));
    query_two.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(query_two_c));
    query_one.merge_with(query_two);

    let mut expected_query = Query::new();
    expected_query.insert_key(b"a".to_vec());
    let mut query_b_c = Query::new();
    query_b_c.insert_key(b"b".to_vec());
    query_b_c.insert_key(b"c".to_vec());
    let mut query_c = Query::new();
    query_c.insert_key(b"c".to_vec());
    let mut query_d = Query::new();
    query_d.insert_key(b"d".to_vec());

    query_b_c.add_conditional_subquery(QueryItem::Key(b"b".to_vec()), None, Some(query_c));
    query_b_c.add_conditional_subquery(QueryItem::Key(b"c".to_vec()), None, Some(query_d));

    expected_query.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(query_b_c));
    assert_eq!(query_one, expected_query);
}

#[test]
fn root_verify() {
    verify_keys_test(vec![vec![5]], vec![Some(vec![5])]);
}

#[test]
fn single_verify() {
    verify_keys_test(vec![vec![3]], vec![Some(vec![3])]);
}

#[test]
fn double_verify() {
    verify_keys_test(vec![vec![3], vec![5]], vec![Some(vec![3]), Some(vec![5])]);
}

#[test]
fn double_verify_2() {
    verify_keys_test(vec![vec![3], vec![7]], vec![Some(vec![3]), Some(vec![7])]);
}

#[test]
fn triple_verify() {
    verify_keys_test(
        vec![vec![3], vec![5], vec![7]],
        vec![Some(vec![3]), Some(vec![5]), Some(vec![7])],
    );
}

#[test]
fn left_edge_absence_verify() {
    verify_keys_test(vec![vec![2]], vec![None]);
}

#[test]
fn right_edge_absence_verify() {
    verify_keys_test(vec![vec![8]], vec![None]);
}

#[test]
fn inner_absence_verify() {
    verify_keys_test(vec![vec![6]], vec![None]);
}

#[test]
fn absent_and_present_verify() {
    verify_keys_test(vec![vec![5], vec![6]], vec![Some(vec![5]), None]);
}

#[test]
fn node_variant_conversion() {
    let mut tree = make_6_node_tree();
    let walker = RefWalker::new(&mut tree, PanicSource {});

    assert_eq!(walker.to_kv_node(), Node::KV(vec![5], vec![5]));
    assert_eq!(
        walker.to_kvhash_node(),
        Node::KVHash([
            61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238, 68,
            142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
        ])
    );
    assert_eq!(
        walker.to_kvdigest_node(),
        Node::KVDigest(
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        ),
    );
    assert_eq!(
        walker.to_hash_node().unwrap(),
        Node::Hash([
            47, 88, 45, 83, 28, 53, 123, 233, 238, 140, 130, 174, 250, 220, 210, 37, 3, 215, 82,
            177, 190, 30, 154, 156, 35, 214, 144, 79, 40, 41, 218, 142
        ])
    );
}

#[test]
fn empty_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let (proof, absence, ..) = walker
        .create_proof(vec![].as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169, 82,
            205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238, 68,
            142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84, 143,
            196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let res = Query::new()
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    assert!(res.result_set.is_empty());
}

#[test]
fn root_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Key(vec![5])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169, 82,
            205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84, 143,
            196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![5], vec![5])]);
}

#[test]
fn leaf_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Key(vec![3])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [3] maps to SumItem discriminant (3), which has simple value hash,
    // so it uses Node::KV instead of Node::KVValueHash for tamper-resistance.
    assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238, 68,
            142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84, 143,
            196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![3], vec![3])]);
}

#[test]
fn double_leaf_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Key(vec![3]), QueryItem::Key(vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [3] maps to SumItem discriminant (3) → simple hash → Node::KV
    assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238, 68,
            142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    // Value [7] maps to CountSumTree discriminant (7) → combined hash →
    // Node::KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![7],
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![3], vec![3]), (vec![7], vec![7])]
    );
}

#[test]
fn all_nodes_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![
        QueryItem::Key(vec![3]),
        QueryItem::Key(vec![5]),
        QueryItem::Key(vec![7]),
    ];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [3] maps to SumItem discriminant (3) → simple hash → Node::KV
    assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
    // Value [5] maps to BigSumTree discriminant (5) → combined hash →
    // Node::KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    // Value [7] maps to CountSumTree discriminant (7) → combined hash →
    // Node::KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![7],
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![3], vec![3]), (vec![5], vec![5]), (vec![7], vec![7])]
    );
}

#[test]
fn global_edge_absence_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Key(vec![8])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169, 82,
            205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238, 68,
            142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, true));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, Vec::<(Vec<u8>, Vec<u8>)>::new());
}

#[test]
fn absence_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Key(vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169, 82,
            205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, Vec::<(Vec<u8>, Vec<u8>)>::new());
}

#[test]
fn doc_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode)
        .unwrap()
        .attach(
            true,
            Some(
                TreeNode::new(vec![2], vec![2], None, BasicMerkNode)
                    .unwrap()
                    .attach(
                        true,
                        Some(TreeNode::new(vec![1], vec![1], None, BasicMerkNode).unwrap()),
                    )
                    .attach(
                        false,
                        Some(
                            TreeNode::new(vec![4], vec![4], None, BasicMerkNode)
                                .unwrap()
                                .attach(
                                    true,
                                    Some(
                                        TreeNode::new(vec![3], vec![3], None, BasicMerkNode)
                                            .unwrap(),
                                    ),
                                ),
                        ),
                    ),
            ),
        )
        .attach(
            false,
            Some(
                TreeNode::new(vec![9], vec![9], None, BasicMerkNode)
                    .unwrap()
                    .attach(
                        true,
                        Some(
                            TreeNode::new(vec![7], vec![7], None, BasicMerkNode)
                                .unwrap()
                                .attach(
                                    true,
                                    Some(
                                        TreeNode::new(vec![6], vec![6], None, BasicMerkNode)
                                            .unwrap(),
                                    ),
                                )
                                .attach(
                                    false,
                                    Some(
                                        TreeNode::new(vec![8], vec![8], None, BasicMerkNode)
                                            .unwrap(),
                                    ),
                                ),
                        ),
                    )
                    .attach(
                        false,
                        Some(
                            TreeNode::new(vec![11], vec![11], None, BasicMerkNode)
                                .unwrap()
                                .attach(
                                    true,
                                    Some(
                                        TreeNode::new(vec![10], vec![10], None, BasicMerkNode)
                                            .unwrap(),
                                    ),
                                ),
                        ),
                    ),
            ),
        );
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .unwrap();

    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![
        QueryItem::Key(vec![1]),
        QueryItem::Key(vec![2]),
        QueryItem::Key(vec![3]),
        QueryItem::Key(vec![4]),
    ];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [1] maps to Reference discriminant (1) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![1],
            vec![1],
            [
                32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131, 208, 25, 73, 98, 245, 209, 227,
                170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199, 149, 144, 21
            ]
        )))
    );
    // Value [2] maps to Tree discriminant (2) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![2],
            vec![2],
            [
                183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16, 139,
                136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    // Value [3] maps to SumItem discriminant (3) → simple hash → KV
    assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
    // Value [4] maps to SumTree discriminant (4) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238, 68,
            142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            12, 156, 232, 212, 220, 65, 226, 32, 91, 101, 248, 64, 225, 206, 63, 12, 153, 191, 183,
            10, 233, 251, 249, 76, 184, 200, 88, 57, 219, 2, 250, 113
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    // Note: We no longer check exact byte encoding since Node::KV is now used for
    // value [3] (SumItem discriminant) instead of Node::KVValueHash. The
    // node-by-node assertions above verify the proof structure correctly.

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![1], vec![1]),
            (vec![2], vec![2]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
        ]
    );
}

#[test]
fn query_item_merge() {
    let mine = QueryItem::Range(vec![10]..vec![30]);
    let other = QueryItem::Range(vec![15]..vec![20]);
    assert_eq!(mine.merge(&other), QueryItem::Range(vec![10]..vec![30]));

    let mine = QueryItem::RangeInclusive(vec![10]..=vec![30]);
    let other = QueryItem::Range(vec![20]..vec![30]);
    assert_eq!(
        mine.merge(&other),
        QueryItem::RangeInclusive(vec![10]..=vec![30])
    );

    let mine = QueryItem::Key(vec![5]);
    let other = QueryItem::Range(vec![1]..vec![10]);
    assert_eq!(mine.merge(&other), QueryItem::Range(vec![1]..vec![10]));

    let mine = QueryItem::Key(vec![10]);
    let other = QueryItem::RangeInclusive(vec![1]..=vec![10]);
    assert_eq!(
        mine.merge(&other),
        QueryItem::RangeInclusive(vec![1]..=vec![10])
    );
}

#[test]
fn query_insert() {
    let mut query = Query::new();
    query.insert_key(vec![2]);
    query.insert_range(vec![3]..vec![5]);
    query.insert_range_inclusive(vec![5]..=vec![7]);
    query.insert_range(vec![4]..vec![6]);
    query.insert_key(vec![5]);

    let mut iter = query.items.iter();
    assert_eq!(format!("{:?}", iter.next()), "Some(Key([2]))");
    assert_eq!(
        format!("{:?}", iter.next()),
        "Some(RangeInclusive([3]..=[7]))"
    );
    assert_eq!(iter.next(), None);
}

#[test]
fn range_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_tree_seq(10, grove_version);
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Range(
        vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
    )];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
            197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123, 117,
            31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172, 237,
            19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 5],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 6],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![0, 0, 0, 0, 0, 0, 0, 7],
            [
                18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220, 56,
                190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
            188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
        ]
    );
    assert_eq!(res.limit, None);

    // right to left test
    let mut tree = make_tree_seq(10, grove_version);
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Range(
        vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
    )];
    let (proof, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new_with_direction(false);
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
        ]
    );
}

#[test]
fn range_proof_inclusive() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_tree_seq(10, grove_version);
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeInclusive(
        vec![0, 0, 0, 0, 0, 0, 0, 5]..=vec![0, 0, 0, 0, 0, 0, 0, 7],
    )];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
            197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123, 117,
            31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172, 237,
            19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 5],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 6],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 7],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
            188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
        ]
    );
    assert_eq!(res.limit, None);

    // right_to_left proof
    let mut tree = make_tree_seq(10, grove_version);
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeInclusive(
        vec![0, 0, 0, 0, 0, 0, 0, 5]..=vec![0, 0, 0, 0, 0, 0, 0, 7],
    )];
    let (proof, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();

    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
        ]
    );
}

#[test]
fn range_from_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            85, 217, 56, 226, 204, 53, 103, 145, 201, 33, 178, 80, 207, 194, 104, 128, 199, 145,
            156, 208, 152, 255, 209, 24, 140, 222, 204, 193, 211, 26, 118, 58
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![7],
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![8],
            vec![8],
            [
                205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49, 158,
                250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, true));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![5], vec![5]), (vec![7], vec![7]), (vec![8], vec![8])]
    );
    assert_eq!(res.limit, None);

    // Limit result set to 1 item
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::Key(vec![5])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![5], vec![5])]);
    assert_eq!(res.limit, Some(0));

    // Limit result set to 2 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![
        QueryItem::Key(vec![5]),
        QueryItem::Key(vec![6]),
        QueryItem::Key(vec![7]),
    ];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![5], vec![5]), (vec![7], vec![7])]
    );
    assert_eq!(res.limit, Some(0));

    // Limit result set to 100 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(100), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeFrom(vec![5]..)];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![5], vec![5]), (vec![7], vec![7]), (vec![8], vec![8])]
    );
    assert_eq!(res.limit, Some(97));

    // right_to_left test
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (true, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![8], vec![8]), (vec![7], vec![7]), (vec![5], vec![5])]
    );
}

#[test]
fn range_to_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeTo(..vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [2] maps to Tree discriminant (2) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![2],
            vec![2],
            [
                183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16, 139,
                136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
            ]
        )))
    );
    // Value [3] maps to SumItem discriminant (3) → simple hash → KV
    assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
    assert_eq!(iter.next(), Some(&Op::Parent));
    // Value [4] maps to SumTree discriminant (4) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    // Value [5] maps to BigSumTree discriminant (5) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250, 165,
            180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (true, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![2], vec![2]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
            (vec![5], vec![5]),
        ]
    );
    assert_eq!(res.limit, None);

    // Limit result set to 1 item
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeTo(..vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![2])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
    assert_eq!(res.limit, Some(0));

    // Limit result set to 2 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeTo(..vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![3])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![2], vec![2]), (vec![3], vec![3])]
    );
    assert_eq!(res.limit, Some(0));

    // Limit result set to 100 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeTo(..vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(100), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeTo(..vec![6])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![2], vec![2]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
            (vec![5], vec![5]),
        ]
    );
    assert_eq!(res.limit, Some(96));

    // right_to_left proof
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeTo(..vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (false, true));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![5], vec![5]),
            (vec![4], vec![4]),
            (vec![3], vec![3]),
            (vec![2], vec![2]),
        ]
    );

    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeTo(..vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![5], vec![5]), (vec![4], vec![4])]
    );
    assert_eq!(res.limit, Some(0));
}

#[test]
fn range_to_proof_inclusive() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [2] maps to Tree discriminant (2) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![2],
            vec![2],
            [
                183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16, 139,
                136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
            ]
        )))
    );
    // Value [3] maps to SumItem discriminant (3) → simple hash → KV
    assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
    assert_eq!(iter.next(), Some(&Op::Parent));
    // Value [4] maps to SumTree discriminant (4) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    // Value [5] maps to BigSumTree discriminant (5) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250, 165,
            180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (true, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![2], vec![2]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
            (vec![5], vec![5]),
        ]
    );
    assert_eq!(res.limit, None);

    // Limit result set to 1 item
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![2])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
    assert_eq!(res.limit, Some(0));

    // Limit result set to 2 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![3])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![2], vec![2]), (vec![3], vec![3])]
    );
    assert_eq!(res.limit, Some(0));

    // Limit result set to 100 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(100), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![2], vec![2]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
            (vec![5], vec![5]),
        ]
    );
    assert_eq!(res.limit, Some(96));

    // right_to_left proof
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (false, true));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![5], vec![5]),
            (vec![4], vec![4]),
            (vec![3], vec![3]),
            (vec![2], vec![2]),
        ]
    );

    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![5], vec![5])]);
    assert_eq!(res.limit, Some(0));
}

#[test]
fn range_after_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
            241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![3],
            [
                210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205, 4,
                107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![7],
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![8],
            vec![8],
            [
                205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49, 158,
                250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, true));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![4], vec![4]),
            (vec![5], vec![5]),
            (vec![7], vec![7]),
            (vec![8], vec![8]),
        ]
    );
    assert_eq!(res.limit, None);

    // Limit result set to 1 item
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![4])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![4], vec![4])]);
    assert_eq!(res.limit, Some(0));

    // Limit result set to 2 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![5])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![4], vec![4]), (vec![5], vec![5])]
    );
    assert_eq!(res.limit, Some(0));

    // Limit result set to 100 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(100), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfter(vec![3]..)];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![4], vec![4]),
            (vec![5], vec![5]),
            (vec![7], vec![7]),
            (vec![8], vec![8]),
        ]
    );
    assert_eq!(res.limit, Some(96));

    // right_to_left proof
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (true, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![8], vec![8]),
            (vec![7], vec![7]),
            (vec![5], vec![5]),
            (vec![4], vec![4]),
        ]
    );

    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(3), false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (true, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(3), false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![8], vec![8]), (vec![7], vec![7]), (vec![5], vec![5])]
    );
    assert_eq!(res.limit, Some(0));
}

#[test]
fn range_after_to_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
            241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![3],
            [
                210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205, 4,
                107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250, 165,
            180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![4], vec![4]), (vec![5], vec![5])]
    );
    assert_eq!(res.limit, None);

    // Limit result set to 1 item
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![4])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![4], vec![4])]);
    assert_eq!(res.limit, Some(0));

    // Limit result set to 2 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![5])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![4], vec![4]), (vec![5], vec![5])]
    );
    assert_eq!(res.limit, Some(0));

    // Limit result set to 100 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(100), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![4], vec![4]), (vec![5], vec![5])]
    );
    assert_eq!(res.limit, Some(98));

    // right_to_left
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![5], vec![5]), (vec![4], vec![4])]
    );

    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(300), false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(300), false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![5], vec![5]), (vec![4], vec![4])]
    );
    assert_eq!(res.limit, Some(298));
}

#[test]
fn range_after_to_proof_inclusive() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    iter.next();
    let _ = Some(&Op::Push(Node::Hash([
        121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4, 241,
        127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30,
    ])));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![3],
            [
                210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205, 4,
                107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![7],
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250, 165,
            180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![4], vec![4]), (vec![5], vec![5]), (vec![7], vec![7])]
    );
    assert_eq!(res.limit, None);

    // Limit result set to 1 item
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![4])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![4], vec![4])]);
    assert_eq!(res.limit, Some(0));

    // Limit result set to 2 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![5])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![4], vec![4]), (vec![5], vec![5])]
    );
    assert_eq!(res.limit, Some(0));

    // Limit result set to 100 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(100), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![4], vec![4]), (vec![5], vec![5]), (vec![7], vec![7])]
    );
    assert_eq!(res.limit, Some(97));

    // right_to_left proof
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![7], vec![7]), (vec![5], vec![5]), (vec![4], vec![4])]
    );
}

#[test]
fn range_full_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFull(..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [2] maps to Tree discriminant (2) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![2],
            vec![2],
            [
                183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16, 139,
                136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
            ]
        )))
    );
    // Value [3] maps to SumItem discriminant (3) → simple hash → KV
    assert_eq!(iter.next(), Some(&Op::Push(Node::KV(vec![3], vec![3]))));
    assert_eq!(iter.next(), Some(&Op::Parent));
    // Value [4] maps to SumTree discriminant (4) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    // Value [5] maps to BigSumTree discriminant (5) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    // Value [7] maps to CountSumTree discriminant (7) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![7],
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    // Value [8] maps to ProvableCountTree discriminant (8) → combined hash →
    // KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![8],
            vec![8],
            [
                205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49, 158,
                250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(iter.next(), Some(&Op::Child));

    assert!(iter.next().is_none());
    assert_eq!(absence, (true, true));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![2], vec![2]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
            (vec![5], vec![5]),
            (vec![7], vec![7]),
            (vec![8], vec![8]),
        ]
    );
    assert_eq!(res.limit, None);

    // Limit result set to 1 item
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFull(..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![2])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
    assert_eq!(res.limit, Some(0));

    // Limit result set to 2 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFull(..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![3])];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![2], vec![2]), (vec![3], vec![3])]
    );
    assert_eq!(res.limit, Some(0));

    // Limit result set to 100 items
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFull(..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(100), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let equivalent_query_items = vec![QueryItem::RangeFull(..)];
    let (equivalent_proof, equivalent_absence, ..) = walker
        .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(proof, equivalent_proof);
    assert_eq!(absence, equivalent_absence);

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![2], vec![2]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
            (vec![5], vec![5]),
            (vec![7], vec![7]),
            (vec![8], vec![8]),
        ]
    );
    assert_eq!(res.limit, Some(94));

    // right_to_left proof
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFull(..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (true, true));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![8], vec![8]),
            (vec![7], vec![7]),
            (vec![5], vec![5]),
            (vec![4], vec![4]),
            (vec![3], vec![3]),
            (vec![2], vec![2]),
        ]
    );

    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFull(..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), Some(2), false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    assert_eq!(absence, (true, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(2), false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![8], vec![8]), (vec![7], vec![7])]
    );
    assert_eq!(res.limit, Some(0));
}

#[test]
fn proof_with_limit() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFrom(vec![2]..)];
    let (proof, _, status) = walker
        .create_proof(query_items.as_slice(), Some(1), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    // TODO: Add this test for other range types
    assert_eq!(status.limit, Some(0));

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVValueHash(
            vec![2],
            vec![2],
            [
                183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16, 139,
                136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
            ]
        )))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            126, 128, 159, 241, 207, 26, 88, 61, 163, 18, 218, 189, 45, 220, 124, 96, 118, 68, 61,
            95, 230, 75, 145, 218, 178, 227, 63, 137, 79, 153, 182, 12
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            56, 181, 68, 232, 233, 83, 180, 104, 74, 123, 143, 25, 174, 80, 132, 201, 61, 108, 131,
            89, 204, 90, 128, 199, 164, 25, 3, 146, 39, 127, 12, 105
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238, 68,
            142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            133, 188, 175, 131, 60, 89, 221, 135, 133, 53, 205, 110, 58, 56, 128, 58, 1, 227, 75,
            122, 83, 20, 125, 44, 149, 44, 62, 130, 252, 134, 105, 200
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
    assert_eq!(res.limit, Some(0));
}

#[test]
fn right_to_left_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_6_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::RangeFrom(vec![3]..)];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, false, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    // Value [8] maps to ProvableCountTree discriminant (8) → combined hash →
    // KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::PushInverted(Node::KVValueHash(
            vec![8],
            vec![8],
            [
                205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49, 158,
                250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
            ]
        )))
    );
    // Value [7] maps to CountSumTree discriminant (7) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::PushInverted(Node::KVValueHash(
            vec![7],
            vec![7],
            [
                63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102, 51,
                109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::ChildInverted));
    // Value [5] maps to BigSumTree discriminant (5) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::PushInverted(Node::KVValueHash(
            vec![5],
            vec![5],
            [
                116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::ParentInverted));
    // Value [4] maps to SumTree discriminant (4) → combined hash → KVValueHash
    assert_eq!(
        iter.next(),
        Some(&Op::PushInverted(Node::KVValueHash(
            vec![4],
            vec![4],
            [
                198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51, 227,
                215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
            ]
        )))
    );
    // Value [3] maps to SumItem discriminant (3) → simple hash → KV
    assert_eq!(
        iter.next(),
        Some(&Op::PushInverted(Node::KV(vec![3], vec![3])))
    );
    assert_eq!(iter.next(), Some(&Op::ParentInverted));
    assert_eq!(
        iter.next(),
        Some(&Op::PushInverted(Node::Hash([
            121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
            241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::ChildInverted));
    assert_eq!(iter.next(), Some(&Op::ChildInverted));
    assert_eq!(iter.next(), None);

    assert_eq!(absence, (true, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new_with_direction(false);
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![8], vec![8]),
            (vec![7], vec![7]),
            (vec![5], vec![5]),
            (vec![4], vec![4]),
            (vec![3], vec![3]),
        ]
    );
}

#[test]
fn range_proof_missing_upper_bound() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_tree_seq(10, grove_version);
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![QueryItem::Range(
        vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 6, 5],
    )];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
            197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123, 117,
            31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172, 237,
            19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 5],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 6],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![0, 0, 0, 0, 0, 0, 0, 7],
            [
                18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220, 56,
                190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
            188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
        ]
    );
}

#[test]
fn range_proof_missing_lower_bound() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_tree_seq(10, grove_version);
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let query_items = vec![
        // 7 is not inclusive
        QueryItem::Range(vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7]),
    ];
    let (proof, absence, ..) = walker
        .create_proof(query_items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut iter = proof.iter();
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
            197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVHash([
            95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123, 117,
            31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172, 237,
            19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
        ])))
    );
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![0, 0, 0, 0, 0, 0, 0, 5],
            [
                18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220, 56,
                190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KV(
            vec![0, 0, 0, 0, 0, 0, 0, 6],
            vec![123; 60],
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::KVDigest(
            vec![0, 0, 0, 0, 0, 0, 0, 7],
            [
                18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220, 56,
                190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
            ]
        )))
    );
    assert_eq!(iter.next(), Some(&Op::Parent));
    assert_eq!(
        iter.next(),
        Some(&Op::Push(Node::Hash([
            161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
            188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
        ])))
    );
    assert_eq!(iter.next(), Some(&Op::Child));
    assert_eq!(iter.next(), Some(&Op::Child));
    assert!(iter.next().is_none());
    assert_eq!(absence, (false, false));

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    let mut query = Query::new();
    for item in query_items {
        query.insert_item(item);
    }
    let res = query
        .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
        .unwrap()
        .unwrap();
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60])]
    );
}

#[test]
fn subset_proof() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_tree_seq(10, grove_version);
    let expected_hash = tree.hash().unwrap().to_owned();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    // 1..10 prove range full, subset 7
    let mut query = Query::new();
    query.insert_all();

    let (proof, ..) = walker
        .create_proof(query.items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    // subset query
    let mut query = Query::new();
    query.insert_key(vec![0, 0, 0, 0, 0, 0, 0, 6]);

    let res = query
        .verify_proof(bytes.as_slice(), None, true, expected_hash)
        .unwrap()
        .unwrap();

    assert_eq!(res.result_set.len(), 1);
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![(vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60])]
    );

    // 1..10 prove (2..=5, 7..10) subset (3..=4, 7..=8)
    let mut query = Query::new();
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 2]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
    query.insert_range(vec![0, 0, 0, 0, 0, 0, 0, 7]..vec![0, 0, 0, 0, 0, 0, 0, 10]);
    let (proof, ..) = walker
        .create_proof(query.items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    let mut query = Query::new();
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 3]..=vec![0, 0, 0, 0, 0, 0, 0, 4]);
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 7]..=vec![0, 0, 0, 0, 0, 0, 0, 8]);
    let res = query
        .verify_proof(bytes.as_slice(), None, true, expected_hash)
        .unwrap()
        .unwrap();

    assert_eq!(res.result_set.len(), 4);
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 3], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 8], vec![123; 60]),
        ]
    );

    // 1..10 prove (2..=5, 6..10) subset (4..=8)
    let mut query = Query::new();
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 2]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
    query.insert_range(vec![0, 0, 0, 0, 0, 0, 0, 6]..vec![0, 0, 0, 0, 0, 0, 0, 10]);
    let (proof, ..) = walker
        .create_proof(query.items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    let mut query = Query::new();
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 4]..=vec![0, 0, 0, 0, 0, 0, 0, 8]);
    let res = query
        .verify_proof(bytes.as_slice(), None, true, expected_hash)
        .unwrap()
        .unwrap();

    assert_eq!(res.result_set.len(), 5);
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 8], vec![123; 60]),
        ]
    );

    // 1..10 prove (1..=3, 2..=5) subset (1..=5)
    let mut query = Query::new();
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 1]..=vec![0, 0, 0, 0, 0, 0, 0, 3]);
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 2]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
    let (proof, ..) = walker
        .create_proof(query.items.as_slice(), None, true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    let mut query = Query::new();
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 1]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
    let res = query
        .verify_proof(bytes.as_slice(), None, true, expected_hash)
        .unwrap()
        .unwrap();

    assert_eq!(res.result_set.len(), 5);
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 1], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 2], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 3], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
        ]
    );

    // 1..10 prove full (..) limit to 5, subset (1..=5)
    let mut query = Query::new();
    query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
    let (proof, ..) = walker
        .create_proof(query.items.as_slice(), Some(5), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    let mut query = Query::new();
    query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 1]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
    let res = query
        .verify_proof(bytes.as_slice(), Some(5), true, expected_hash)
        .unwrap()
        .unwrap();

    assert_eq!(res.result_set.len(), 5);
    compare_result_tuples_not_optional!(
        res.result_set,
        vec![
            (vec![0, 0, 0, 0, 0, 0, 0, 1], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 2], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 3], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
            (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
        ]
    );
}

#[test]
fn break_subset_proof() {
    let grove_version = GroveVersion::latest();
    // TODO: move this to where you'd set the constraints for this definition
    // goal is to show that ones limit and offset values are involved
    // whether a query is subset or not now also depends on the state
    // queries essentially highlight parts of the tree, a query
    // is a subset of another query if all the nodes it highlights
    // are also highlighted by the original query
    // with limit and offset the nodes a query highlights now depends on state
    // hence it's impossible to know if something is subset at definition time

    let mut tree = make_tree_seq(10, grove_version);
    let expected_hash = tree.hash().unwrap().to_owned();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    // 1..10 prove full (..) limit to 3, subset (1..=3)
    let mut query = Query::new();
    query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
    let (proof, ..) = walker
        .create_proof(query.items.as_slice(), Some(3), true, grove_version)
        .unwrap()
        .expect("create_proof errored");

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    // Try to query 4
    let mut query = Query::new();
    query.insert_key(vec![0, 0, 0, 0, 0, 0, 0, 4]);
    assert!(query
        .verify_proof(bytes.as_slice(), Some(3), true, expected_hash)
        .unwrap()
        .is_err());

    // if limit offset parameters are different from generation then proof
    // verification returns an error Try superset proof with increased limit
    let mut query = Query::new();
    query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
    assert!(query
        .verify_proof(bytes.as_slice(), Some(4), true, expected_hash)
        .unwrap()
        .is_err());

    // Try superset proof with less limit
    let mut query = Query::new();
    query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
    assert!(query
        .verify_proof(bytes.as_slice(), Some(2), true, expected_hash)
        .unwrap()
        .is_err());
}

#[test]
fn query_from_vec() {
    let query_items = vec![QueryItem::Range(
        vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
    )];
    let query = Query::from(query_items);

    let expected = vec![QueryItem::Range(
        vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
    )];
    assert_eq!(query.items, expected);
}

#[test]
fn query_into_vec() {
    let mut query = Query::new();
    query.insert_item(QueryItem::Range(
        vec![0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
    ));
    let query_vec: Vec<QueryItem> = query.into();
    let expected = [QueryItem::Range(
        vec![0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
    )];
    assert_eq!(
        query_vec.first().unwrap().lower_bound(),
        expected.first().unwrap().lower_bound()
    );
    assert_eq!(
        query_vec.first().unwrap().upper_bound(),
        expected.first().unwrap().upper_bound()
    );
}

#[test]
fn query_item_from_vec_u8() {
    let query_items: Vec<u8> = vec![42];
    let query = QueryItem::from(query_items);

    let expected = QueryItem::Key(vec![42]);
    assert_eq!(query, expected);
}

#[test]
#[allow(deprecated)]
fn verify_ops() {
    let grove_version = GroveVersion::latest();
    let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode).unwrap();
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    let root_hash = tree.hash().unwrap();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let (proof, ..) = walker
        .create_proof(
            vec![QueryItem::Key(vec![5])].as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");
    let mut bytes = vec![];

    encode_into(proof.iter(), &mut bytes);

    let map = verify::verify(&bytes, root_hash).unwrap().unwrap();
    assert_eq!(
        map.get(vec![5].as_slice()).unwrap().unwrap(),
        vec![5].as_slice()
    );
}

#[test]
#[allow(deprecated)]
#[should_panic(expected = "verify failed")]
fn verify_ops_mismatched_hash() {
    let grove_version = GroveVersion::latest();
    let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode).unwrap();
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    let mut walker = RefWalker::new(&mut tree, PanicSource {});

    let (proof, ..) = walker
        .create_proof(
            vec![QueryItem::Key(vec![5])].as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");
    let mut bytes = vec![];

    encode_into(proof.iter(), &mut bytes);

    let _map = verify::verify(&bytes, [42; 32])
        .unwrap()
        .expect("verify failed");
}

#[test]
#[should_panic(expected = "verify failed")]
fn verify_query_mismatched_hash() {
    let grove_version = GroveVersion::latest();
    let mut tree = make_3_node_tree();
    let mut walker = RefWalker::new(&mut tree, PanicSource {});
    let keys = vec![vec![5], vec![7]];
    let (proof, ..) = walker
        .create_proof(
            keys.clone()
                .into_iter()
                .map(QueryItem::Key)
                .collect::<Vec<_>>()
                .as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");
    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    let mut query = Query::new();
    for key in keys.iter() {
        query.insert_key(key.clone());
    }

    let _result = query
        .verify_proof(bytes.as_slice(), None, true, [42; 32])
        .unwrap()
        .expect("verify failed");
}

/// Test with 5 Element::Item values showing that tampering IS NOW DETECTED
/// With our fix, Items use Node::KV which causes the verifier to recompute
/// the hash from the value bytes, so any tampering changes the root hash.
#[test]
fn test_5_item_tree_tampering_detected_with_elements() {
    let grove_version = GroveVersion::latest();

    // Create serialized Element::Item values
    let val1 = grovedb_element::Element::new_item(b"aaa".to_vec())
        .serialize(grove_version)
        .unwrap();
    let val2 = grovedb_element::Element::new_item(b"bbb".to_vec())
        .serialize(grove_version)
        .unwrap();
    let val3 = grovedb_element::Element::new_item(b"ccc".to_vec())
        .serialize(grove_version)
        .unwrap();
    let val4 = grovedb_element::Element::new_item(b"ddd".to_vec())
        .serialize(grove_version)
        .unwrap();
    let val5 = grovedb_element::Element::new_item(b"eee".to_vec())
        .serialize(grove_version)
        .unwrap();

    // Build a 5-node tree manually:
    //           [3]
    //          /   \
    //       [2]     [4]
    //       /         \
    //     [1]         [5]

    // Create leaf nodes first
    let one_tree = TreeNode::new(vec![1], val1.clone(), None, BasicMerkNode).unwrap();
    let five_tree = TreeNode::new(vec![5], val5.clone(), None, BasicMerkNode).unwrap();

    // Create [2] with [1] as left child
    let mut two_tree = TreeNode::new(vec![2], val2.clone(), None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(one_tree));
    two_tree
        .commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    // Create [4] with [5] as right child
    let mut four_tree = TreeNode::new(vec![4], val4.clone(), None, BasicMerkNode)
        .unwrap()
        .attach(false, Some(five_tree));
    four_tree
        .commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    // Create root [3] with [2] as left and [4] as right
    let mut tree = TreeNode::new(vec![3], val3.clone(), None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(two_tree))
        .attach(false, Some(four_tree));
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    let expected_root = tree.hash().unwrap();

    println!("=== Tree Structure ===");
    println!("Tree with 5 Element::Item values:");
    println!("           [3] Item(ccc)");
    println!("          /   \\");
    println!("     [2] Item(bbb)   [4] Item(ddd)");
    println!("       /         \\");
    println!("   [1] Item(aaa)       [5] Item(eee)");
    println!();
    println!("Root hash: {}", hex::encode(expected_root));
    println!();

    // Query for key 1 (bottom left leaf)
    let keys = vec![vec![1]];
    let mut walker = RefWalker::new(&mut tree, PanicSource {});
    let (proof, ..) = walker
        .create_proof(
            keys.clone()
                .into_iter()
                .map(QueryItem::Key)
                .collect::<Vec<_>>()
                .as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");

    println!("=== Proof Structure for key [1] ===");
    println!("Path to [1]: root[3] -> left[2] -> left[1]");
    println!();
    println!("Proof operations:");
    for (i, op) in proof.iter().enumerate() {
        let desc = match op {
            Op::Push(node) => format!("Push({})", node),
            Op::PushInverted(node) => format!("PushInverted({})", node),
            Op::Parent => "Parent".to_string(),
            Op::Child => "Child".to_string(),
            Op::ParentInverted => "ParentInverted".to_string(),
            Op::ChildInverted => "ChildInverted".to_string(),
        };
        println!("  Op {}: {}", i, desc);
    }
    println!();

    // Verify that the proof uses Node::KV (not KVValueHash) for Items
    let has_kv_node = proof
        .iter()
        .any(|op| matches!(op, Op::Push(Node::KV(..)) | Op::PushInverted(Node::KV(..))));
    assert!(
        has_kv_node,
        "Element::Item should produce Node::KV in proof (tamper-resistant)"
    );
    println!("VERIFIED: Proof uses Node::KV for Element::Item (tamper-resistant)");
    println!();

    // Encode and verify original
    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    println!("=== Encoded Proof ({} bytes) ===", bytes.len());

    let mut query = Query::new();
    query.insert_key(vec![1]);
    let result = query
        .verify_proof(bytes.as_slice(), None, true, expected_root)
        .unwrap()
        .expect("original verify failed");
    println!("Original verification: PASSED");
    println!("  Key: {:?}", result.result_set[0].key);
    println!();

    // Tamper with the value - find "aaa" within the serialized Element
    println!("=== Tampering Attempt ===");
    let mut tampered = bytes.clone();
    let original_value = b"aaa";
    let fake_value = b"XXX"; // Same length

    let mut found = false;
    for i in 0..tampered.len().saturating_sub(original_value.len()) {
        if &tampered[i..i + original_value.len()] == original_value {
            println!("Found value 'aaa' at byte position {}", i);
            tampered[i..i + original_value.len()].copy_from_slice(fake_value);
            println!("Replaced with 'XXX'");
            found = true;
            break;
        }
    }
    assert!(found, "Should find value to tamper");
    println!();

    // Try to verify tampered proof
    println!("=== Verification of Tampered Proof ===");
    let mut query2 = Query::new();
    query2.insert_key(vec![1]);

    let (tampered_root, _tampered_result) = query2
        .execute_proof(tampered.as_slice(), None, true)
        .unwrap()
        .expect("execute_proof failed");

    println!("Expected root: {}", hex::encode(expected_root));
    println!("Tampered root: {}", hex::encode(tampered_root));

    if tampered_root == expected_root {
        panic!("SECURITY BUG: Tampering was NOT detected! Root hash should have changed.");
    } else {
        println!();
        println!("=== TAMPERING DETECTED ===");
        println!("Root hash changed - proof verification would fail!");
        println!();
        println!("WHY TAMPERING IS NOW DETECTED:");
        println!("  Node::KV contains only (key, value) - no separate value_hash");
        println!("  The verifier computes value_hash = H(value) during verification");
        println!("  Any change to value bytes changes the computed hash");
        println!("  This causes the root hash to differ, failing verification");
        println!();
        println!("SECURITY IMPROVEMENT:");
        println!("  Element::Item now uses Node::KV instead of Node::KVValueHash");
        println!("  This makes single-Merk proofs tamper-resistant for Items");
    }
}

/// Test with 5 raw (non-Element) values showing that tampering IS DETECTED
/// Raw Merk values now default to Node::KV for tamper-resistance.
#[test]
fn test_5_item_tree_tampering_detected_raw_values() {
    // Build a 5-node tree manually with RAW values (not Elements):
    //           [3]
    //          /   \
    //       [2]     [4]
    //       /         \
    //     [1]         [5]

    // Create leaf nodes first with raw byte values
    let one_tree = TreeNode::new(vec![1], b"aaa".to_vec(), None, BasicMerkNode).unwrap();
    let five_tree = TreeNode::new(vec![5], b"eee".to_vec(), None, BasicMerkNode).unwrap();

    // Create [2] with [1] as left child
    let mut two_tree = TreeNode::new(vec![2], b"bbb".to_vec(), None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(one_tree));
    two_tree
        .commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    // Create [4] with [5] as right child
    let mut four_tree = TreeNode::new(vec![4], b"ddd".to_vec(), None, BasicMerkNode)
        .unwrap()
        .attach(false, Some(five_tree));
    four_tree
        .commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    // Create root [3] with [2] as left and [4] as right
    let mut tree = TreeNode::new(vec![3], b"ccc".to_vec(), None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(two_tree))
        .attach(false, Some(four_tree));
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    let expected_root = tree.hash().unwrap();

    println!("=== Tree Structure (RAW values) ===");
    println!("Tree with 5 raw byte values (NOT Element):");
    println!("           [3] 'ccc'");
    println!("          /   \\");
    println!("     [2] 'bbb'   [4] 'ddd'");
    println!("       /         \\");
    println!("   [1] 'aaa'       [5] 'eee'");
    println!();
    println!("Root hash: {}", hex::encode(expected_root));
    println!();

    // Query for key 1 (bottom left leaf)
    let grove_version = GroveVersion::latest();
    let keys = vec![vec![1]];
    let mut walker = RefWalker::new(&mut tree, PanicSource {});
    let (proof, ..) = walker
        .create_proof(
            keys.clone()
                .into_iter()
                .map(QueryItem::Key)
                .collect::<Vec<_>>()
                .as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");

    println!("=== Proof Structure for key [1] ===");
    println!("Proof operations:");
    for (i, op) in proof.iter().enumerate() {
        let desc = match op {
            Op::Push(node) => format!("Push({})", node),
            Op::PushInverted(node) => format!("PushInverted({})", node),
            Op::Parent => "Parent".to_string(),
            Op::Child => "Child".to_string(),
            Op::ParentInverted => "ParentInverted".to_string(),
            Op::ChildInverted => "ChildInverted".to_string(),
        };
        println!("  Op {}: {}", i, desc);
    }
    println!();

    // Verify that raw values now use Node::KV (tamper-resistant default)
    let has_kv_node = proof
        .iter()
        .any(|op| matches!(op, Op::Push(Node::KV(..)) | Op::PushInverted(Node::KV(..))));
    assert!(
        has_kv_node,
        "Raw values should now produce Node::KV (tamper-resistant default)"
    );
    println!("Raw values now use Node::KV (tamper-resistant by default)");
    println!();

    // Encode and verify original
    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);
    println!("=== Encoded Proof ({} bytes) ===", bytes.len());

    let mut query = Query::new();
    query.insert_key(vec![1]);
    let result = query
        .verify_proof(bytes.as_slice(), None, true, expected_root)
        .unwrap()
        .expect("original verify failed");
    println!("Original verification: PASSED");
    println!("  Key: {:?}", result.result_set[0].key);
    println!(
        "  Value: {:?}",
        String::from_utf8_lossy(result.result_set[0].value.as_ref().unwrap())
    );
    println!();

    // Tamper with the value
    println!("=== Tampering Attempt ===");
    let mut tampered = bytes.clone();
    let original_value = b"aaa";
    let fake_value = b"XXX"; // Same length

    let mut found = false;
    for i in 0..tampered.len().saturating_sub(original_value.len()) {
        if &tampered[i..i + original_value.len()] == original_value {
            println!("Found value 'aaa' at byte position {}", i);
            tampered[i..i + original_value.len()].copy_from_slice(fake_value);
            println!("Replaced with 'XXX'");
            found = true;
            break;
        }
    }
    assert!(found, "Should find value to tamper");
    println!();

    // Try to verify tampered proof
    println!("=== Verification of Tampered Proof ===");
    let mut query2 = Query::new();
    query2.insert_key(vec![1]);

    let (tampered_root, _tampered_result) = query2
        .execute_proof(tampered.as_slice(), None, true)
        .unwrap()
        .expect("execute_proof failed");

    println!("Expected root: {}", hex::encode(expected_root));
    println!("Tampered root: {}", hex::encode(tampered_root));

    if tampered_root == expected_root {
        panic!("SECURITY BUG: Tampering was NOT detected! Root hash should have changed.");
    } else {
        println!();
        println!("=== TAMPERING DETECTED ===");
        println!("Root hash changed - proof verification would fail!");
        println!();
        println!("Raw Merk values now default to Node::KV, making them tamper-resistant.");
        println!("Only GroveDB subtrees/references use KVValueHash (for combined hashes).");
    }
}

/// Test that tampering is detected for values with invalid Element
/// discriminants These values default to Node::KV, making them
/// tamper-resistant
#[test]
fn test_tampering_detected_invalid_discriminant() {
    let grove_version = GroveVersion::latest();

    // Create a tree with values that have invalid Element discriminants (>= 10)
    // These will default to Node::KV (tamper-resistant)
    // Values 99, 100, 101 are invalid Element discriminants
    let left = TreeNode::new(vec![3], vec![100], None, BasicMerkNode).unwrap();
    let right = TreeNode::new(vec![7], vec![101], None, BasicMerkNode).unwrap();
    let mut tree = TreeNode::new(vec![5], vec![99], None, BasicMerkNode)
        .unwrap()
        .attach(true, Some(left))
        .attach(false, Some(right));
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .expect("commit failed");

    let expected_root = tree.hash().unwrap();

    let mut walker = RefWalker::new(&mut tree, PanicSource {});
    let keys = vec![vec![5]];
    let (proof, ..) = walker
        .create_proof(
            keys.clone()
                .into_iter()
                .map(QueryItem::Key)
                .collect::<Vec<_>>()
                .as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");

    // Verify that the proof uses Node::KV (tamper-resistant) for invalid
    // discriminants
    let has_kv_node = proof
        .iter()
        .any(|op| matches!(op, Op::Push(Node::KV(..)) | Op::PushInverted(Node::KV(..))));
    assert!(
        has_kv_node,
        "Invalid discriminants should produce Node::KV (tamper-resistant)"
    );

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    // Verify original proof works
    let mut query = Query::new();
    for key in keys.iter() {
        query.insert_key(key.clone());
    }
    let result = query
        .verify_proof(bytes.as_slice(), None, true, expected_root)
        .unwrap()
        .expect("original verify failed");
    assert_eq!(result.result_set[0].key, vec![5]);
    assert_eq!(result.result_set[0].value.as_ref().unwrap(), &vec![99]);

    // Now tamper with the value bytes in the proof
    // Node::KV format: [opcode][key_len][key][value_len_u16][value]
    // Tampering the value will change the computed hash
    let mut tampered = bytes.clone();

    // Find and tamper the value (which is [99])
    // Change it to [200]
    let mut found = false;
    for i in 0..tampered.len() {
        // Look for opcode 0x03 (Push KV) or 0x0a (PushInverted KV)
        if tampered[i] == 0x03 || tampered[i] == 0x0a {
            // Format: opcode(1) + key_len(1) + key + value_len(2) + value
            if i + 1 >= tampered.len() {
                continue;
            }
            let key_len = tampered[i + 1] as usize;
            let value_len_pos = i + 2 + key_len;
            if value_len_pos + 2 > tampered.len() {
                continue;
            }
            // The `ed` crate uses big-endian encoding for u16
            let value_len =
                u16::from_be_bytes([tampered[value_len_pos], tampered[value_len_pos + 1]]) as usize;
            let value_pos = value_len_pos + 2;
            if value_pos + value_len > tampered.len() {
                continue;
            }
            // Tamper the value bytes (change all to 200)
            for j in 0..value_len {
                tampered[value_pos + j] = 200;
            }
            found = true;
            break;
        }
    }
    assert!(found, "Should find KV node to tamper");

    // Try to verify tampered proof with same expected root
    let mut query2 = Query::new();
    for key in keys.iter() {
        query2.insert_key(key.clone());
    }

    // Use execute_proof to get the computed root
    let (tampered_root, _tampered_result) = query2
        .execute_proof(tampered.as_slice(), None, true)
        .unwrap()
        .expect("execute_proof failed");

    // Check that tampering was detected via root hash change
    if tampered_root == expected_root {
        panic!("SECURITY BUG: Tampering was NOT detected! Root hash should have changed.");
    } else {
        // Tampering detected - this is the expected behavior
        println!(
            "Tampering detected: root hash changed from {:?} to {:?}",
            hex::encode(expected_root),
            hex::encode(tampered_root)
        );
        println!(
            "Node::KV nodes are tamper-resistant because the verifier computes value_hash from \
             value bytes"
        );
    }
}

/// Test that KVValueHash is still used for values with Tree/Reference
/// discriminants These have combined hashes and REQUIRE KVValueHash at
/// the Merk level. Tampering at single-Merk level is still possible but
/// caught by GroveDB's multi-layer proofs.
#[test]
fn test_kvvaluehash_still_used_for_tree_discriminants() {
    let grove_version = GroveVersion::latest();

    // make_3_node_tree uses values [3], [5], [7] which map to:
    // 3 = SumItem (simple hash -> KV)
    // 5 = BigSumTree (combined hash -> KVValueHash)
    // 7 = CountSumTree (combined hash -> KVValueHash)
    let mut tree = make_3_node_tree();
    let expected_root = tree.hash().unwrap();

    // Query for key [5] which has value [5] (BigSumTree discriminant)
    let mut walker = RefWalker::new(&mut tree, PanicSource {});
    let keys = vec![vec![5]];
    let (proof, ..) = walker
        .create_proof(
            keys.clone()
                .into_iter()
                .map(QueryItem::Key)
                .collect::<Vec<_>>()
                .as_slice(),
            None,
            true,
            grove_version,
        )
        .unwrap()
        .expect("failed to create proof");

    // Verify that the proof uses Node::KVValueHash for BigSumTree discriminant
    let has_kv_value_hash = proof.iter().any(|op| {
        matches!(
            op,
            Op::Push(Node::KVValueHash(..)) | Op::PushInverted(Node::KVValueHash(..))
        )
    });
    assert!(
        has_kv_value_hash,
        "Tree discriminant (5=BigSumTree) should produce Node::KVValueHash"
    );

    let mut bytes = vec![];
    encode_into(proof.iter(), &mut bytes);

    // Verify original proof works
    let mut query = Query::new();
    for key in keys.iter() {
        query.insert_key(key.clone());
    }
    let result = query
        .verify_proof(bytes.as_slice(), None, true, expected_root)
        .unwrap()
        .expect("original verify failed");
    assert_eq!(result.result_set[0].key, vec![5]);
    assert_eq!(result.result_set[0].value.as_ref().unwrap(), &vec![5]);

    // Tamper with the value bytes in the KVValueHash node
    let mut tampered = bytes.clone();
    let mut found = false;
    for i in 0..tampered.len() {
        // Look for opcode 0x04 (KVValueHash)
        if tampered[i] == 0x04 {
            if i + 1 >= tampered.len() {
                continue;
            }
            let key_len = tampered[i + 1] as usize;
            let value_len_pos = i + 2 + key_len;
            if value_len_pos + 2 > tampered.len() {
                continue;
            }
            let value_len =
                u16::from_be_bytes([tampered[value_len_pos], tampered[value_len_pos + 1]]) as usize;
            let value_pos = value_len_pos + 2;
            if value_pos + value_len > tampered.len() {
                continue;
            }
            // Tamper the value bytes (change to 9)
            for j in 0..value_len {
                tampered[value_pos + j] = 9;
            }
            found = true;
            break;
        }
    }
    assert!(found, "Should find KVValueHash node to tamper");

    // Execute tampered proof
    let mut query2 = Query::new();
    for key in keys.iter() {
        query2.insert_key(key.clone());
    }
    let (tampered_root, tampered_result) = query2
        .execute_proof(tampered.as_slice(), None, true)
        .unwrap()
        .expect("execute_proof failed");

    // For KVValueHash, tampering is NOT detected at single-Merk level
    // This is expected - these are subtree placeholders where combined hash
    // verification happens at the GroveDB level
    if tampered_root == expected_root {
        println!(
            "As expected: KVValueHash (for Tree discriminants) allows value tampering at \
             single-Merk level"
        );
        println!(
            "Tampered value returned: {:?}",
            tampered_result.result_set[0].value
        );
        println!("This is BY DESIGN - GroveDB's multi-layer proofs catch this tampering");
    } else {
        panic!("Unexpected: root hash changed for KVValueHash node");
    }
}
