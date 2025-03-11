//! Common tests

use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::{
    make_deep_tree, reference_path::ReferencePathType, BidirectionalReference, TempGroveDb,
    ANOTHER_TEST_LEAF, TEST_LEAF,
};
use crate::{operations::proof::util::ProvedPathKeyValues, Element, Error};

/// Compare result tuples
pub fn compare_result_tuples(
    result_set: ProvedPathKeyValues,
    expected_result_set: Vec<(Vec<u8>, Vec<u8>)>,
) {
    assert_eq!(expected_result_set.len(), result_set.len());
    for i in 0..expected_result_set.len() {
        assert_eq!(expected_result_set[i].0, result_set[i].key);
        assert_eq!(expected_result_set[i].1, result_set[i].value);
    }
}

fn deserialize_and_extract_item_bytes(raw_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let elem = Element::deserialize(raw_bytes, GroveVersion::latest())?;
    match elem {
        Element::Item(item, _) => Ok(item),
        _ => Err(Error::CorruptedPath("expected only item type".to_string())),
    }
}

/// Compare result sets
pub fn compare_result_sets(elements: &Vec<Vec<u8>>, result_set: &ProvedPathKeyValues) {
    for i in 0..elements.len() {
        assert_eq!(
            deserialize_and_extract_item_bytes(&result_set[i].value).unwrap(),
            elements[i]
        )
    }
}

pub(crate) fn make_tree_with_bidi_references(version: &GroveVersion) -> TempGroveDb {
    let db = make_deep_tree(&version);

    let transaction = db.start_transaction();

    // Let's say we're deleting `deep_leaf` with an existing references chain
    // that goes like
    // test_leaf/innertree:ref -> another_test_leaf/innertree2:ref2 ->
    //   -> deep_leaf/deep_node_1/deeper_1:ref3 ->
    //   -> deep_leaf/deep_node_2/deeper_3:ref4 ->
    //   -> deep_leaf/deep_node_1/deeper_2:key5
    //

    db.insert(
        &[b"deep_leaf".as_ref(), b"deep_node_1", b"deeper_2"],
        b"key5",
        Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        None,
        Some(&transaction),
        version,
    )
    .unwrap()
    .unwrap();

    db.insert(
        &[b"deep_leaf".as_ref(), b"deep_node_2", b"deeper_3"],
        b"ref4",
        Element::BidirectionalReference(BidirectionalReference {
            forward_reference_path: ReferencePathType::UpstreamRootHeightReference(
                1,
                vec![
                    b"deep_node_1".to_vec(),
                    b"deeper_2".to_vec(),
                    b"key5".to_vec(),
                ],
            ),
            backward_reference_slot: 0,
            cascade_on_update: true,
            max_hop: None,
            flags: None,
        }),
        None,
        Some(&transaction),
        version,
    )
    .unwrap()
    .unwrap();

    db.insert(
        &[b"deep_leaf".as_ref(), b"deep_node_1", b"deeper_1"],
        b"ref3",
        Element::BidirectionalReference(BidirectionalReference {
            forward_reference_path: ReferencePathType::UpstreamRootHeightReference(
                1,
                vec![
                    b"deep_node_2".to_vec(),
                    b"deeper_3".to_vec(),
                    b"ref4".to_vec(),
                ],
            ),
            backward_reference_slot: 0,
            cascade_on_update: true,
            max_hop: None,
            flags: None,
        }),
        None,
        Some(&transaction),
        version,
    )
    .unwrap()
    .unwrap();

    db.insert(
        &[ANOTHER_TEST_LEAF, b"innertree2"],
        b"ref2",
        Element::BidirectionalReference(BidirectionalReference {
            forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec(),
                b"ref3".to_vec(),
            ]),
            backward_reference_slot: 0,
            cascade_on_update: true,
            max_hop: None,
            flags: None,
        }),
        None,
        Some(&transaction),
        version,
    )
    .unwrap()
    .unwrap();

    db.insert(
        &[TEST_LEAF, b"innertree"],
        b"ref",
        Element::BidirectionalReference(BidirectionalReference {
            forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                ANOTHER_TEST_LEAF.to_vec(),
                b"innertree2".to_vec(),
                b"ref2".to_vec(),
            ]),
            backward_reference_slot: 0,
            cascade_on_update: true,
            max_hop: None,
            flags: None,
        }),
        None,
        Some(&transaction),
        version,
    )
    .unwrap()
    .unwrap();

    db.commit_transaction(transaction).unwrap().unwrap();

    db
}

pub(crate) const EMPTY_PATH: SubtreePath<'static, [u8; 0]> = SubtreePath::empty();
