#[cfg(feature = "full")]
pub mod common;
#[cfg(feature = "full")]
mod query_tests;
#[cfg(feature = "full")]
mod sum_tree_tests;
#[cfg(feature = "full")]
mod tree_hashes_tests;

#[cfg(feature = "full")]
use std::{
    ops::{Deref, DerefMut},
    option::Option::None,
};

#[cfg(feature = "full")]
use ::visualize::{Drawer, Visualize};
#[cfg(feature = "full")]
use tempfile::TempDir;

#[cfg(feature = "full")]
use super::*;
#[cfg(feature = "full")]
use crate::{
    query_result_type::QueryResultType::QueryKeyElementPairResultType,
    reference_path::ReferencePathType, tests::common::compare_result_tuples,
};

#[cfg(feature = "full")]
pub const TEST_LEAF: &[u8] = b"test_leaf";
#[cfg(feature = "full")]
pub const ANOTHER_TEST_LEAF: &[u8] = b"test_leaf2";
#[cfg(feature = "full")]
const DEEP_LEAF: &[u8] = b"deep_leaf";

#[cfg(feature = "full")]
/// GroveDB wrapper to keep temp directory alive
pub struct TempGroveDb {
    _tmp_dir: TempDir,
    grove_db: GroveDb,
}

#[cfg(feature = "full")]
impl DerefMut for TempGroveDb {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.grove_db
    }
}

#[cfg(feature = "full")]
impl Deref for TempGroveDb {
    type Target = GroveDb;

    fn deref(&self) -> &Self::Target {
        &self.grove_db
    }
}

#[cfg(feature = "full")]
impl Visualize for TempGroveDb {
    fn visualize<W: std::io::Write>(&self, drawer: Drawer<W>) -> std::io::Result<Drawer<W>> {
        self.grove_db.visualize(drawer)
    }
}

#[cfg(feature = "full")]
/// A helper method to create an empty GroveDB
pub fn make_empty_grovedb() -> TempGroveDb {
    let tmp_dir = TempDir::new().unwrap();
    let db = GroveDb::open(tmp_dir.path()).unwrap();
    TempGroveDb {
        _tmp_dir: tmp_dir,
        grove_db: db,
    }
}

#[cfg(feature = "full")]
/// A helper method to create GroveDB with one leaf for a root tree
pub fn make_test_grovedb() -> TempGroveDb {
    // Tree Structure
    // root
    //  test_leaf
    //  another_test_leaf
    let tmp_dir = TempDir::new().unwrap();
    let mut db = GroveDb::open(tmp_dir.path()).unwrap();
    add_test_leaves(&mut db);
    TempGroveDb {
        _tmp_dir: tmp_dir,
        grove_db: db,
    }
}

#[cfg(feature = "full")]
fn add_test_leaves(db: &mut GroveDb) {
    db.insert([], TEST_LEAF, Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful root tree leaf insert");
    db.insert([], ANOTHER_TEST_LEAF, Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful root tree leaf 2 insert");
}

#[cfg(feature = "full")]
pub fn make_deep_tree() -> TempGroveDb {
    // Tree Structure
    // root
    //     test_leaf
    //         innertree
    //             k1,v1
    //             k2,v2
    //             k3,v3
    //         innertree4
    //             k4,v4
    //             k5,v5
    //     another_test_leaf
    //         innertree2
    //             k3,v3
    //         innertree3
    //             k4,v4
    //     deep_leaf
    //          deep_node_1
    //              deeper_node_1
    //                  k1,v1
    //                  k2,v2
    //                  k3,v3
    //              deeper_node_2
    //                  k4,v4
    //                  k5,v5
    //                  k6,v6
    //          deep_node_2
    //              deeper_node_3
    //                  k7,v7
    //                  k8,v8
    //                  k9,v9
    //              deeper_node_4
    //                  k10,v10
    //                  k11,v11

    // Insert elements into grovedb instance
    let temp_db = make_test_grovedb();

    // add an extra root leaf
    temp_db
        .insert([], DEEP_LEAF, Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful root tree leaf insert");

    // Insert level 1 nodes
    temp_db
        .insert([TEST_LEAF], b"innertree", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF],
            b"innertree4",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF],
            b"innertree2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF],
            b"innertree3",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF],
            b"deep_node_1",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF],
            b"deep_node_2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    // Insert level 2 nodes
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key2",
            Element::new_item(b"value2".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key3",
            Element::new_item(b"value3".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree4"],
            b"key4",
            Element::new_item(b"value4".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree4"],
            b"key5",
            Element::new_item(b"value5".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree2"],
            b"key3",
            Element::new_item(b"value3".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree3"],
            b"key4",
            Element::new_item(b"value4".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1"],
            b"deeper_node_1",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1"],
            b"deeper_node_2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_2"],
            b"deeper_node_3",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_2"],
            b"deeper_node_4",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    // Insert level 3 nodes
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1", b"deeper_node_1"],
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1", b"deeper_node_1"],
            b"key2",
            Element::new_item(b"value2".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1", b"deeper_node_1"],
            b"key3",
            Element::new_item(b"value3".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1", b"deeper_node_2"],
            b"key4",
            Element::new_item(b"value4".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1", b"deeper_node_2"],
            b"key5",
            Element::new_item(b"value5".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_1", b"deeper_node_2"],
            b"key6",
            Element::new_item(b"value6".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_2", b"deeper_node_3"],
            b"key7",
            Element::new_item(b"value7".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_2", b"deeper_node_3"],
            b"key8",
            Element::new_item(b"value8".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_2", b"deeper_node_3"],
            b"key9",
            Element::new_item(b"value9".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_2", b"deeper_node_4"],
            b"key10",
            Element::new_item(b"value10".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [DEEP_LEAF, b"deep_node_2", b"deeper_node_4"],
            b"key11",
            Element::new_item(b"value11".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
}

#[cfg(feature = "full")]
#[test]
fn test_init() {
    let tmp_dir = TempDir::new().unwrap();
    GroveDb::open(tmp_dir).expect("empty tree is ok");
}

#[cfg(feature = "full")]
#[test]
fn test_element_with_flags() {
    let db = make_test_grovedb();

    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("should insert subtree successfully");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"elem1",
        Element::new_item(b"flagless".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("should insert subtree successfully");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"elem2",
        Element::new_item_with_flags(b"flagged".to_vec(), Some([4, 5, 6, 7, 8].to_vec())),
        None,
        None,
    )
    .unwrap()
    .expect("should insert subtree successfully");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"elem3",
        Element::new_tree_with_flags(None, Some([1].to_vec())),
        None,
        None,
    )
    .unwrap()
    .expect("should insert subtree successfully");
    db.insert(
        [TEST_LEAF, b"key1", b"elem3"],
        b"elem4",
        Element::new_reference_with_flags(
            ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"key1".to_vec(),
                b"elem2".to_vec(),
            ]),
            Some([9].to_vec()),
        ),
        None,
        None,
    )
    .unwrap()
    .expect("should insert subtree successfully");

    let element_without_flag = db
        .get([TEST_LEAF, b"key1"], b"elem1", None)
        .unwrap()
        .expect("should get successfully");
    let element_with_flag = db
        .get([TEST_LEAF, b"key1"], b"elem2", None)
        .unwrap()
        .expect("should get successfully");
    let tree_element_with_flag = db
        .get([TEST_LEAF, b"key1"], b"elem3", None)
        .unwrap()
        .expect("should get successfully");
    let flagged_ref_follow = db
        .get([TEST_LEAF, b"key1", b"elem3"], b"elem4", None)
        .unwrap()
        .expect("should get successfully");

    let mut query = Query::new();
    query.insert_key(b"elem4".to_vec());
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"elem3".to_vec()],
        SizedQuery::new(query, None, None),
    );
    let (flagged_ref_no_follow, _) = db
        .query_raw(&path_query, QueryKeyElementPairResultType, None)
        .unwrap()
        .expect("should get successfully");

    assert_eq!(
        element_without_flag,
        Element::Item(b"flagless".to_vec(), None)
    );
    assert_eq!(
        element_with_flag,
        Element::Item(b"flagged".to_vec(), Some([4, 5, 6, 7, 8].to_vec()))
    );
    assert_eq!(tree_element_with_flag.get_flags(), &Some([1].to_vec()));
    assert_eq!(
        flagged_ref_follow,
        Element::Item(b"flagged".to_vec(), Some([4, 5, 6, 7, 8].to_vec()))
    );
    assert_eq!(
        flagged_ref_no_follow.to_key_elements()[0],
        (
            b"elem4".to_vec(),
            Element::Reference(
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"key1".to_vec(),
                    b"elem2".to_vec()
                ]),
                None,
                Some([9].to_vec())
            )
        )
    );

    // Test proofs with flags
    let mut query = Query::new();
    query.insert_all();

    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
        SizedQuery::new(query, None, None),
    );
    let proof = db
        .prove_query(&path_query)
        .unwrap()
        .expect("should successfully create proof");
    let (root_hash, result_set) =
        GroveDb::verify_query(&proof, &path_query).expect("should verify proof");
    assert_eq!(root_hash, db.grove_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);
    assert_eq!(
        Element::deserialize(&result_set[0].1).expect("should deserialize element"),
        Element::Item(b"flagless".to_vec(), None)
    );
    assert_eq!(
        Element::deserialize(&result_set[1].1).expect("should deserialize element"),
        Element::Item(b"flagged".to_vec(), Some([4, 5, 6, 7, 8].to_vec()))
    );
    assert_eq!(
        Element::deserialize(&result_set[2].1)
            .expect("should deserialize element")
            .get_flags(),
        &Some([1].to_vec())
    );
}

#[cfg(feature = "full")]
#[test]
fn test_cannot_update_populated_tree_item() {
    // This test shows that you cannot update a tree item
    // in a way that disconnects it's root hash from that of
    // the merk it points to.
    let db = make_deep_tree();

    let old_element = db
        .get([TEST_LEAF], b"innertree", None)
        .unwrap()
        .expect("should fetch item");

    let new_element = Element::empty_tree();
    db.insert([TEST_LEAF], b"innertree", new_element.clone(), None, None)
        .unwrap()
        .expect_err("should not override tree");

    let current_element = db
        .get([TEST_LEAF], b"innertree", None)
        .unwrap()
        .expect("should fetch item");

    assert_eq!(current_element, old_element);
    assert_ne!(current_element, new_element);
}

#[cfg(feature = "full")]
#[test]
fn test_changes_propagated() {
    let db = make_test_grovedb();
    let old_hash = db.root_hash(None).unwrap().unwrap();
    let element = Element::new_item(b"ayy".to_vec());

    // Insert some nested subtrees
    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 1 insert");

    let _merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF])
        .unwrap()
        .unwrap();

    db.insert(
        [TEST_LEAF, b"key1"],
        b"key2",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree 2 insert");

    db.insert(
        [TEST_LEAF, b"key1", b"key2"],
        b"key3",
        element.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");

    assert_eq!(
        db.get([TEST_LEAF, b"key1", b"key2"], b"key3", None)
            .unwrap()
            .expect("successful get"),
        element
    );
    assert_ne!(old_hash, db.root_hash(None).unwrap().unwrap());
}

// TODO: Add solid test cases to this
#[cfg(feature = "full")]
#[test]
fn test_references() {
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"merk_1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"merk_1"],
        b"key1",
        Element::new_item(b"value1".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"merk_1"],
        b"key2",
        Element::new_item(b"value2".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert([TEST_LEAF], b"merk_2", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    // db.insert([TEST_LEAF, b"merk_2"], b"key2",
    // Element::new_item(b"value2".to_vec()), None).expect("successful subtree
    // insert");
    db.insert(
        [TEST_LEAF, b"merk_2"],
        b"key1",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"merk_1".to_vec(),
            b"key1".to_vec(),
        ])),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"merk_2"],
        b"key2",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"merk_1".to_vec(),
            b"key2".to_vec(),
        ])),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    assert!(db.get([TEST_LEAF], b"merk_1", None).unwrap().is_ok());
    assert!(db.get([TEST_LEAF], b"merk_2", None).unwrap().is_ok());
}

#[cfg(feature = "full")]
#[test]
fn test_follow_references() {
    let db = make_test_grovedb();
    let element = Element::new_item(b"ayy".to_vec());

    // Insert an item to refer to
    db.insert([TEST_LEAF], b"key2", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 1 insert");
    db.insert([TEST_LEAF, b"key2"], b"key3", element.clone(), None, None)
        .unwrap()
        .expect("successful value insert");

    // Insert a reference
    db.insert(
        [TEST_LEAF],
        b"reference_key",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"key2".to_vec(),
            b"key3".to_vec(),
        ])),
        None,
        None,
    )
    .unwrap()
    .expect("successful reference insert");

    assert_eq!(
        db.get([TEST_LEAF], b"reference_key", None)
            .unwrap()
            .expect("successful get"),
        element
    );
}

#[cfg(feature = "full")]
#[test]
fn test_reference_must_point_to_item() {
    let db = make_test_grovedb();

    let result = db
        .insert(
            [TEST_LEAF],
            b"reference_key_1",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"reference_key_2".to_vec(),
            ])),
            None,
            None,
        )
        .unwrap();

    assert!(matches!(result, Err(Error::MissingReference(_))));
}

#[cfg(feature = "full")]
#[test]
fn test_too_many_indirections() {
    use crate::operations::get::MAX_REFERENCE_HOPS;
    let db = make_test_grovedb();

    let keygen = |idx| format!("key{}", idx).bytes().collect::<Vec<u8>>();

    db.insert(
        [TEST_LEAF],
        b"key0",
        Element::new_item(b"oops".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");

    for i in 1..=(MAX_REFERENCE_HOPS) {
        db.insert(
            [TEST_LEAF],
            &keygen(i),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                keygen(i - 1),
            ])),
            None,
            None,
        )
        .unwrap()
        .expect("successful reference insert");
    }

    // Add one more reference
    db.insert(
        [TEST_LEAF],
        &keygen(MAX_REFERENCE_HOPS + 1),
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            keygen(MAX_REFERENCE_HOPS),
        ])),
        None,
        None,
    )
    .unwrap()
    .expect("expected insert");

    let result = db
        .get([TEST_LEAF], &keygen(MAX_REFERENCE_HOPS + 1), None)
        .unwrap();

    assert!(matches!(result, Err(Error::ReferenceLimit)));
}

#[cfg(feature = "full")]
#[test]
fn test_reference_value_affects_state() {
    let db_one = make_test_grovedb();
    db_one
        .insert([TEST_LEAF], b"key1", Element::new_item(vec![0]), None, None)
        .unwrap()
        .expect("should insert item");
    db_one
        .insert(
            [ANOTHER_TEST_LEAF],
            b"ref",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"key1".to_vec(),
            ])),
            None,
            None,
        )
        .unwrap()
        .expect("should insert item");

    let db_two = make_test_grovedb();
    db_two
        .insert([TEST_LEAF], b"key1", Element::new_item(vec![0]), None, None)
        .unwrap()
        .expect("should insert item");
    db_two
        .insert(
            [ANOTHER_TEST_LEAF],
            b"ref",
            Element::new_reference(ReferencePathType::UpstreamRootHeightReference(
                0,
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            )),
            None,
            None,
        )
        .unwrap()
        .expect("should insert item");

    assert_ne!(
        db_one
            .root_hash(None)
            .unwrap()
            .expect("should return root hash"),
        db_two
            .root_hash(None)
            .unwrap()
            .expect("should return toor hash")
    );
}

#[cfg(feature = "full")]
#[test]
fn test_tree_structure_is_persistent() {
    let tmp_dir = TempDir::new().unwrap();
    let element = Element::new_item(b"ayy".to_vec());
    // Create a scoped GroveDB
    let prev_root_hash = {
        let mut db = GroveDb::open(tmp_dir.path()).unwrap();
        add_test_leaves(&mut db);

        // Insert some nested subtrees
        db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF, b"key1"],
            b"key2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree 2 insert");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1", b"key2"],
            b"key3",
            element.clone(),
            None,
            None,
        )
        .unwrap()
        .expect("successful value insert");
        assert_eq!(
            db.get([TEST_LEAF, b"key1", b"key2"], b"key3", None)
                .unwrap()
                .expect("successful get 1"),
            element
        );
        db.root_hash(None).unwrap().unwrap()
    };
    // Open a persisted GroveDB
    let db = GroveDb::open(tmp_dir).unwrap();
    assert_eq!(
        db.get([TEST_LEAF, b"key1", b"key2"], b"key3", None)
            .unwrap()
            .expect("successful get 2"),
        element
    );
    assert!(db
        .get([TEST_LEAF, b"key1", b"key2"], b"key4", None)
        .unwrap()
        .is_err());
    assert_eq!(prev_root_hash, db.root_hash(None).unwrap().unwrap());
}

#[cfg(feature = "full")]
#[test]
fn test_root_tree_leaves_are_noted() {
    let db = make_test_grovedb();
    db.check_subtree_exists_path_not_found([TEST_LEAF], None)
        .unwrap()
        .expect("should exist");
    db.check_subtree_exists_path_not_found([ANOTHER_TEST_LEAF], None)
        .unwrap()
        .expect("should exist");
}

#[cfg(feature = "full")]
#[test]
fn test_proof_for_invalid_path_root_key() {
    let db = make_test_grovedb();

    let query = Query::new();
    let path_query = PathQuery::new_unsized(vec![b"invalid_path_key".to_vec()], query);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);
}

#[cfg(feature = "full")]
#[test]
fn test_proof_for_invalid_path() {
    let db = make_deep_tree();

    let query = Query::new();
    let path_query =
        PathQuery::new_unsized(vec![b"deep_leaf".to_vec(), b"invalid_key".to_vec()], query);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);

    let query = Query::new();
    let path_query = PathQuery::new_unsized(
        vec![
            b"deep_leaf".to_vec(),
            b"deep_node_1".to_vec(),
            b"invalid_key".to_vec(),
        ],
        query,
    );

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);

    let query = Query::new();
    let path_query = PathQuery::new_unsized(
        vec![
            b"deep_leaf".to_vec(),
            b"deep_node_1".to_vec(),
            b"deeper_node_1".to_vec(),
            b"invalid_key".to_vec(),
        ],
        query,
    );

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);

    let query = Query::new();
    let path_query = PathQuery::new_unsized(
        vec![
            b"deep_leaf".to_vec(),
            b"early_invalid_key".to_vec(),
            b"deeper_node_1".to_vec(),
            b"invalid_key".to_vec(),
        ],
        query,
    );

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);
}

#[cfg(feature = "full")]
#[test]
fn test_proof_for_non_existent_data() {
    let temp_db = make_test_grovedb();

    let mut query = Query::new();
    query.insert_key(b"key1".to_vec());

    // path to empty subtree
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_without_subquery_with_reference() {
    // Tree Structure
    // root
    //     test_leaf
    //         innertree
    //             k1,v1
    //             k2,v2
    //             k3,v3
    //     another_test_leaf
    //         innertree2
    //             k3,v3
    //             k4, reference to k1 in innertree
    //             k5, reference to k4 in innertree3
    //         innertree3
    //             k4,v4

    // Insert elements into grovedb instance
    let temp_db = make_test_grovedb();
    // Insert level 1 nodes
    temp_db
        .insert([TEST_LEAF], b"innertree", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF],
            b"innertree2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF],
            b"innertree3",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    // Insert level 2 nodes
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key2",
            Element::new_item(b"value2".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key3",
            Element::new_item(b"value3".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree2"],
            b"key3",
            Element::new_item(b"value3".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree2"],
            b"key4",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"innertree".to_vec(),
                b"key1".to_vec(),
            ])),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree3"],
            b"key4",
            Element::new_item(b"value4".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree2"],
            b"key5",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                ANOTHER_TEST_LEAF.to_vec(),
                b"innertree3".to_vec(),
                b"key4".to_vec(),
            ])),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

    // Single key query
    let mut query = Query::new();
    query.insert_range_from(b"key4".to_vec()..);

    let path_query = PathQuery::new_unsized(
        vec![ANOTHER_TEST_LEAF.to_vec(), b"innertree2".to_vec()],
        query,
    );

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    assert_eq!(
        hex::encode(&proof),
        "0200000000000000850198ebd6dc7e1c82951c41fcfa6487711cac6a399\
        ebb01bb979cbe4a51e0b2f08d06046b6579340009000676616c75653100b\
        f2f052b01c2bb83ff3a40504d42b5b9141c582a3e0c98679189b33a24478\
        a6f1006046b6579350009000676616c75653400f084ffdbc429a89c9b662\
        0e7224d73c2ee505eb7e6fb5eb574e1a8dc8b0d088411010000000000000\
        058040a696e6e6572747265653200080201046b657934008ba21f835b2ff\
        60f16b7fccfbda107bec3da0c4709357d40de223d769547ec21013a09015\
        5ea7d14038c7062d94930798f885a19d6ebff8a87489a1debf6656047110\
        1000000000000005e02cfb7d035b8f4a3631be46c597510a16770c15c743\
        31b3dc8dcb577a206e49675040a746573745f6c65616632000e02010a696\
        e6e657274726565320049870f2813c0c3c5c105a988c0ef1372178245152\
        fa9a43b209a6b6d95589bdc11"
    );
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    let r1 = Element::new_item(b"value1".to_vec()).serialize().unwrap();
    let r2 = Element::new_item(b"value4".to_vec()).serialize().unwrap();

    compare_result_tuples(
        result_set,
        vec![(b"key4".to_vec(), r1), (b"key5".to_vec(), r2)],
    );
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_without_subquery() {
    // Tree Structure
    // root
    //     test_leaf
    //         innertree
    //             k1,v1
    //             k2,v2
    //             k3,v3
    //     another_test_leaf
    //         innertree2
    //             k3,v3
    //         innertree3
    //             k4,v4

    // Insert elements into grovedb instance
    let temp_db = make_test_grovedb();
    // Insert level 1 nodes
    temp_db
        .insert([TEST_LEAF], b"innertree", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF],
            b"innertree2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF],
            b"innertree3",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    // Insert level 2 nodes
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key2",
            Element::new_item(b"value2".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [TEST_LEAF, b"innertree"],
            b"key3",
            Element::new_item(b"value3".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree2"],
            b"key3",
            Element::new_item(b"value3".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
    temp_db
        .insert(
            [ANOTHER_TEST_LEAF, b"innertree3"],
            b"key4",
            Element::new_item(b"value4".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

    // Single key query
    let mut query = Query::new();
    query.insert_key(b"key1".to_vec());

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    assert_eq!(
        hex::encode(proof.as_slice()),
        "02000000000000005503046b6579310009000676616c756531000201865\
        5e18e4555b0b65bbcec64c749db6b9ad84231969fb4fbe769a3093d10f21\
        00198ebd6dc7e1c82951c41fcfa6487711cac6a399ebb01bb979cbe4a51e\
        0b2f08d110100000000000000350409696e6e65727472656500080201046\
        b657932004910536da659a3dbdbcf68c4a6630e72de4ba20cfc60b08b3dd\
        45b4225a599b601000000000000005c0409746573745f6c656166000d020\
        109696e6e65727472656500fafa16d06e8d8696dae443731ae2a4eae521e\
        4a9a79c331c8a7e22e34c0f1a6e01b55f830550604719833d54ce2bf139a\
        ff4bb699fa4111b9741633554318792c511",
    );
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    let r1 = Element::new_item(b"value1".to_vec()).serialize().unwrap();
    compare_result_tuples(result_set, vec![(b"key1".to_vec(), r1)]);

    // Range query + limit
    let mut query = Query::new();
    query.insert_range_after(b"key1".to_vec()..);
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
        SizedQuery::new(query, Some(1), None),
    );

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    let r1 = Element::new_item(b"value2".to_vec()).serialize().unwrap();
    compare_result_tuples(result_set, vec![(b"key2".to_vec(), r1)]);

    // Range query + offset + limit
    let mut query = Query::new();
    query.insert_range_after(b"key1".to_vec()..);
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
        SizedQuery::new(query, Some(1), Some(1)),
    );

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    let r1 = Element::new_item(b"value3".to_vec()).serialize().unwrap();
    compare_result_tuples(result_set, vec![(b"key3".to_vec(), r1)]);

    // Range query + direction + limit
    let mut query = Query::new_with_direction(false);
    query.insert_all();
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
        SizedQuery::new(query, Some(2), None),
    );

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    let r1 = Element::new_item(b"value3".to_vec()).serialize().unwrap();
    let r2 = Element::new_item(b"value2".to_vec()).serialize().unwrap();
    compare_result_tuples(
        result_set,
        vec![(b"key3".to_vec(), r1), (b"key2".to_vec(), r2)],
    );
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_with_default_subquery() {
    let temp_db = make_deep_tree();

    let mut query = Query::new();
    query.insert_all();

    let mut subq = Query::new();
    subq.insert_all();
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);

    let keys = [
        b"key1".to_vec(),
        b"key2".to_vec(),
        b"key3".to_vec(),
        b"key4".to_vec(),
        b"key5".to_vec(),
    ];
    let values = [
        b"value1".to_vec(),
        b"value2".to_vec(),
        b"value3".to_vec(),
        b"value4".to_vec(),
        b"value5".to_vec(),
    ];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);

    let mut query = Query::new();
    query.insert_range_after(b"innertree".to_vec()..);

    let mut subq = Query::new();
    subq.insert_all();
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 2);

    let keys = [b"key4".to_vec(), b"key5".to_vec()];
    let values = [b"value4".to_vec(), b"value5".to_vec()];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);

    // range subquery
    let mut query = Query::new();
    query.insert_all();

    let mut subq = Query::new();
    subq.insert_range_after_to_inclusive(b"key1".to_vec()..=b"key4".to_vec());
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query).expect(
        "should
    execute proof",
    );

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    let keys = [b"key2".to_vec(), b"key3".to_vec(), b"key4".to_vec()];
    let values = [b"value2".to_vec(), b"value3".to_vec(), b"value4".to_vec()];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);

    // deep tree test
    let mut query = Query::new();
    query.insert_all();

    let mut subq = Query::new();
    subq.insert_all();

    let mut sub_subquery = Query::new();
    sub_subquery.insert_all();

    subq.set_subquery(sub_subquery);
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![DEEP_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 11);

    let keys = [
        b"key1".to_vec(),
        b"key2".to_vec(),
        b"key3".to_vec(),
        b"key4".to_vec(),
        b"key5".to_vec(),
        b"key6".to_vec(),
        b"key7".to_vec(),
        b"key8".to_vec(),
        b"key9".to_vec(),
        b"key10".to_vec(),
        b"key11".to_vec(),
    ];
    let values = [
        b"value1".to_vec(),
        b"value2".to_vec(),
        b"value3".to_vec(),
        b"value4".to_vec(),
        b"value5".to_vec(),
        b"value6".to_vec(),
        b"value7".to_vec(),
        b"value8".to_vec(),
        b"value9".to_vec(),
        b"value10".to_vec(),
        b"value11".to_vec(),
    ];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_with_subquery_key() {
    let temp_db = make_deep_tree();

    let mut query = Query::new();
    query.insert_all();

    let mut subq = Query::new();
    subq.insert_all();

    query.set_subquery_key(b"deeper_node_1".to_vec());
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![DEEP_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    let keys = [b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()];
    let values = [b"value1".to_vec(), b"value2".to_vec(), b"value3".to_vec()];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_with_key_and_subquery() {
    let temp_db = make_deep_tree();

    let mut query = Query::new();
    query.insert_key(b"deep_node_1".to_vec());

    let mut subq = Query::new();
    subq.insert_all();

    query.set_subquery_key(b"deeper_node_1".to_vec());
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![DEEP_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    let keys = [b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()];
    let values = [b"value1".to_vec(), b"value2".to_vec(), b"value3".to_vec()];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_with_conditional_subquery() {
    let temp_db = make_deep_tree();

    let mut query = Query::new();
    query.insert_all();

    let mut subquery = Query::new();
    subquery.insert_all();

    let mut final_subquery = Query::new();
    final_subquery.insert_all();

    subquery.add_conditional_subquery(
        QueryItem::Key(b"deeper_node_4".to_vec()),
        None,
        Some(final_subquery),
    );

    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(vec![DEEP_LEAF.to_vec()], query);
    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());

    let keys = [
        b"deeper_node_1".to_vec(),
        b"deeper_node_2".to_vec(),
        b"key10".to_vec(),
        b"key11".to_vec(),
    ];
    assert_eq!(result_set.len(), keys.len());

    // TODO: Is this defined behaviour
    for (index, key) in keys.iter().enumerate() {
        assert_eq!(&result_set[index].0, key);
    }

    // Default + Conditional subquery
    let mut query = Query::new();
    query.insert_all();

    let mut subquery = Query::new();
    subquery.insert_all();

    let mut final_conditional_subquery = Query::new();
    final_conditional_subquery.insert_all();

    let mut final_default_subquery = Query::new();
    final_default_subquery.insert_range_inclusive(b"key3".to_vec()..=b"key6".to_vec());

    subquery.add_conditional_subquery(
        QueryItem::Key(b"deeper_node_4".to_vec()),
        None,
        Some(final_conditional_subquery),
    );
    subquery.set_subquery(final_default_subquery);

    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(vec![DEEP_LEAF.to_vec()], query);
    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 6);

    let keys = [
        b"key3".to_vec(),
        b"key4".to_vec(),
        b"key5".to_vec(),
        b"key6".to_vec(),
        b"key10".to_vec(),
        b"key11".to_vec(),
    ];
    let values = [
        b"value3".to_vec(),
        b"value4".to_vec(),
        b"value5".to_vec(),
        b"value6".to_vec(),
        b"value10".to_vec(),
        b"value11".to_vec(),
    ];
    let elements = values
        .map(|x| Element::new_item(x).serialize().unwrap())
        .to_vec();
    // compare_result_sets(&elements, &result_set);
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_with_sized_query() {
    let temp_db = make_deep_tree();

    let mut query = Query::new();
    query.insert_all();

    let mut subquery = Query::new();
    subquery.insert_all();

    let mut final_conditional_subquery = Query::new();
    final_conditional_subquery.insert_all();

    let mut final_default_subquery = Query::new();
    final_default_subquery.insert_range_inclusive(b"key3".to_vec()..=b"key6".to_vec());

    subquery.add_conditional_subquery(
        QueryItem::Key(b"deeper_node_4".to_vec()),
        None,
        Some(final_conditional_subquery),
    );
    subquery.set_subquery(final_default_subquery);
    // subquery.set_subquery_key(b"key3".to_vec());

    query.set_subquery(subquery);

    let path_query = PathQuery::new(
        vec![DEEP_LEAF.to_vec()],
        SizedQuery::new(query, Some(3), Some(1)),
    );
    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    let keys = [b"key4".to_vec(), b"key5".to_vec(), b"key6".to_vec()];
    let values = [b"value4".to_vec(), b"value5".to_vec(), b"value6".to_vec()];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_path_query_proofs_with_direction() {
    let temp_db = make_deep_tree();

    let mut query = Query::new_with_direction(false);
    query.insert_all();

    let mut subquery = Query::new_with_direction(false);
    subquery.insert_all();

    let mut final_conditional_subquery = Query::new_with_direction(false);
    final_conditional_subquery.insert_all();

    let mut final_default_subquery = Query::new_with_direction(false);
    final_default_subquery.insert_range_inclusive(b"key3".to_vec()..=b"key6".to_vec());

    subquery.add_conditional_subquery(
        QueryItem::Key(b"deeper_node_4".to_vec()),
        None,
        Some(final_conditional_subquery),
    );
    subquery.set_subquery(final_default_subquery);

    query.set_subquery(subquery);

    let path_query = PathQuery::new(
        vec![DEEP_LEAF.to_vec()],
        SizedQuery::new(query, Some(3), Some(1)),
    );
    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    let keys = [b"key10".to_vec(), b"key6".to_vec(), b"key5".to_vec()];
    let values = [b"value10".to_vec(), b"value6".to_vec(), b"value5".to_vec()];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);

    // combined directions
    let mut query = Query::new();
    query.insert_all();

    let mut subq = Query::new_with_direction(false);
    subq.insert_all();

    let mut sub_subquery = Query::new();
    sub_subquery.insert_all();

    subq.set_subquery(sub_subquery);
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![DEEP_LEAF.to_vec()], query);

    let proof = temp_db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) =
        GroveDb::verify_query(proof.as_slice(), &path_query).expect("should execute proof");

    assert_eq!(hash, temp_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 11);

    let keys = [
        b"key4".to_vec(),
        b"key5".to_vec(),
        b"key6".to_vec(),
        b"key1".to_vec(),
        b"key2".to_vec(),
        b"key3".to_vec(),
        b"key10".to_vec(),
        b"key11".to_vec(),
        b"key7".to_vec(),
        b"key8".to_vec(),
        b"key9".to_vec(),
    ];
    let values = [
        b"value4".to_vec(),
        b"value5".to_vec(),
        b"value6".to_vec(),
        b"value1".to_vec(),
        b"value2".to_vec(),
        b"value3".to_vec(),
        b"value10".to_vec(),
        b"value11".to_vec(),
        b"value7".to_vec(),
        b"value8".to_vec(),
        b"value9".to_vec(),
    ];
    let elements = values.map(|x| Element::new_item(x).serialize().unwrap());
    let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
    compare_result_tuples(result_set, expected_result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_checkpoint() {
    let db = make_test_grovedb();
    let element1 = Element::new_item(b"ayy".to_vec());

    db.insert([], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("cannot insert a subtree 1 into GroveDB");
    db.insert(
        [b"key1".as_ref()],
        b"key2",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("cannot insert a subtree 2 into GroveDB");
    db.insert(
        [b"key1".as_ref(), b"key2".as_ref()],
        b"key3",
        element1.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("cannot insert an item into GroveDB");

    assert_eq!(
        db.get([b"key1".as_ref(), b"key2".as_ref()], b"key3", None)
            .unwrap()
            .expect("cannot get from grovedb"),
        element1
    );

    let tempdir_parent = TempDir::new().expect("cannot open tempdir");
    let checkpoint_tempdir = tempdir_parent.path().join("checkpoint");
    db.create_checkpoint(&checkpoint_tempdir)
        .expect("cannot create checkpoint");

    let checkpoint_db =
        GroveDb::open(checkpoint_tempdir).expect("cannot open grovedb from checkpoint");

    assert_eq!(
        db.get([b"key1".as_ref(), b"key2".as_ref()], b"key3", None)
            .unwrap()
            .expect("cannot get from grovedb"),
        element1
    );
    assert_eq!(
        checkpoint_db
            .get([b"key1".as_ref(), b"key2".as_ref()], b"key3", None)
            .unwrap()
            .expect("cannot get from checkpoint"),
        element1
    );

    let element2 = Element::new_item(b"ayy2".to_vec());
    let element3 = Element::new_item(b"ayy3".to_vec());

    checkpoint_db
        .insert([b"key1".as_ref()], b"key4", element2.clone(), None, None)
        .unwrap()
        .expect("cannot insert into checkpoint");

    db.insert([b"key1".as_ref()], b"key4", element3.clone(), None, None)
        .unwrap()
        .expect("cannot insert into GroveDB");

    assert_eq!(
        checkpoint_db
            .get([b"key1".as_ref()], b"key4", None)
            .unwrap()
            .expect("cannot get from checkpoint"),
        element2,
    );

    assert_eq!(
        db.get([b"key1".as_ref()], b"key4", None)
            .unwrap()
            .expect("cannot get from GroveDB"),
        element3
    );

    checkpoint_db
        .insert([b"key1".as_ref()], b"key5", element3.clone(), None, None)
        .unwrap()
        .expect("cannot insert into checkpoint");

    db.insert([b"key1".as_ref()], b"key6", element3.clone(), None, None)
        .unwrap()
        .expect("cannot insert into GroveDB");

    assert!(matches!(
        checkpoint_db
            .get([b"key1".as_ref()], b"key6", None)
            .unwrap(),
        Err(Error::PathKeyNotFound(_))
    ));

    assert!(matches!(
        db.get([b"key1".as_ref()], b"key5", None).unwrap(),
        Err(Error::PathKeyNotFound(_))
    ));
}

#[cfg(feature = "full")]
#[test]
fn test_is_empty_tree() {
    let db = make_test_grovedb();

    // Create an empty tree with no elements
    db.insert([TEST_LEAF], b"innertree", Element::empty_tree(), None, None)
        .unwrap()
        .unwrap();

    assert!(db
        .is_empty_tree([TEST_LEAF, b"innertree"], None)
        .unwrap()
        .expect("path is valid tree"));

    // add an element to the tree to make it non empty
    db.insert(
        [TEST_LEAF, b"innertree"],
        b"key1",
        Element::new_item(b"hello".to_vec()),
        None,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(!db
        .is_empty_tree([TEST_LEAF, b"innertree"], None)
        .unwrap()
        .expect("path is valid tree"));
}

#[cfg(feature = "full")]
#[test]
fn transaction_should_be_aborted_when_rollback_is_called() {
    let item_key = b"key3";

    let db = make_test_grovedb();
    let transaction = db.start_transaction();

    let element1 = Element::new_item(b"ayy".to_vec());

    let result = db
        .insert([TEST_LEAF], item_key, element1, None, Some(&transaction))
        .unwrap();

    assert!(matches!(result, Ok(())));

    db.rollback_transaction(&transaction).unwrap();

    let result = db.get([TEST_LEAF], item_key, Some(&transaction)).unwrap();
    assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
}

#[cfg(feature = "full")]
#[test]
fn transaction_should_be_aborted() {
    let db = make_test_grovedb();
    let transaction = db.start_transaction();

    let item_key = b"key3";
    let element = Element::new_item(b"ayy".to_vec());

    db.insert([TEST_LEAF], item_key, element, None, Some(&transaction))
        .unwrap()
        .unwrap();

    drop(transaction);

    // Transactional data shouldn't be committed to the main database
    let result = db.get([TEST_LEAF], item_key, None).unwrap();
    assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
}

#[cfg(feature = "full")]
#[test]
fn test_subtree_pairs_iterator() {
    let db = make_test_grovedb();
    let element = Element::new_item(b"ayy".to_vec());
    let element2 = Element::new_item(b"lmao".to_vec());

    // Insert some nested subtrees
    db.insert([TEST_LEAF], b"subtree1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 1 insert");
    db.insert(
        [TEST_LEAF, b"subtree1"],
        b"subtree11",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        [TEST_LEAF, b"subtree1", b"subtree11"],
        b"key1",
        element.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");
    assert_eq!(
        db.get([TEST_LEAF, b"subtree1", b"subtree11"], b"key1", None)
            .unwrap()
            .expect("successful get 1"),
        element
    );
    db.insert(
        [TEST_LEAF, b"subtree1", b"subtree11"],
        b"key0",
        element.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");
    db.insert(
        [TEST_LEAF, b"subtree1"],
        b"subtree12",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree 3 insert");
    db.insert(
        [TEST_LEAF, b"subtree1"],
        b"key1",
        element.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");
    db.insert(
        [TEST_LEAF, b"subtree1"],
        b"key2",
        element2.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");

    // Iterate over subtree1 to see if keys of other subtrees messed up
    // let mut iter = db
    //     .elements_iterator(&[TEST_LEAF, b"subtree1"], None)
    //     .expect("cannot create iterator");
    let storage_context = db
        .grove_db
        .db
        .get_storage_context([TEST_LEAF, b"subtree1"])
        .unwrap();
    let mut iter = Element::iterator(storage_context.raw_iter()).unwrap();
    assert_eq!(
        iter.next().unwrap().unwrap(),
        Some((b"key1".to_vec(), element))
    );
    assert_eq!(
        iter.next().unwrap().unwrap(),
        Some((b"key2".to_vec(), element2))
    );
    let subtree_element = iter.next().unwrap().unwrap().unwrap();
    assert_eq!(subtree_element.0, b"subtree11".to_vec());
    assert!(matches!(subtree_element.1, Element::Tree(..)));
    let subtree_element = iter.next().unwrap().unwrap().unwrap();
    assert_eq!(subtree_element.0, b"subtree12".to_vec());
    assert!(matches!(subtree_element.1, Element::Tree(..)));
    assert!(matches!(iter.next().unwrap(), Ok(None)));
}

#[cfg(feature = "full")]
#[test]
fn test_find_subtrees() {
    let element = Element::new_item(b"ayy".to_vec());
    let db = make_test_grovedb();
    // Insert some nested subtrees
    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 1 insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"key2",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert([TEST_LEAF, b"key1", b"key2"], b"key3", element, None, None)
        .unwrap()
        .expect("successful value insert");
    db.insert([TEST_LEAF], b"key4", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 3 insert");
    let subtrees = db
        .find_subtrees(vec![TEST_LEAF], None)
        .unwrap()
        .expect("cannot get subtrees");
    assert_eq!(
        vec![
            vec![TEST_LEAF],
            vec![TEST_LEAF, b"key1"],
            vec![TEST_LEAF, b"key4"],
            vec![TEST_LEAF, b"key1", b"key2"],
        ],
        subtrees
    );
}

#[cfg(feature = "full")]
#[test]
fn test_root_subtree_has_root_key() {
    let db = make_test_grovedb();
    let storage = db.db.get_storage_context([]).unwrap();
    let root_merk = Merk::open_base(storage, false)
        .unwrap()
        .expect("expected to get root merk");
    let (_, root_key, _) = root_merk
        .root_hash_key_and_sum()
        .unwrap()
        .expect("expected to get root hash, key and sum");
    assert!(root_key.is_some())
}

#[cfg(feature = "full")]
#[test]
fn test_get_subtree() {
    let db = make_test_grovedb();
    let element = Element::new_item(b"ayy".to_vec());

    // Returns error is subtree is not valid
    {
        let subtree = db.get([TEST_LEAF], b"invalid_tree", None).unwrap();
        assert!(subtree.is_err());

        // Doesn't return an error for subtree that exists but empty
        let subtree = db.get([], TEST_LEAF, None).unwrap();
        assert!(subtree.is_ok());
    }

    // Insert some nested subtrees
    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 1 insert");

    let key1_tree = db
        .get([], TEST_LEAF, None)
        .unwrap()
        .expect("expected to get a root tree");

    assert!(
        matches!(key1_tree, Element::Tree(Some(_), _)),
        "{}",
        format!(
            "expected tree with root key, got {:?}",
            if let Element::Tree(tree, ..) = key1_tree {
                format!("{:?}", tree)
            } else {
                "not a tree".to_string()
            }
        )
    );

    db.insert(
        [TEST_LEAF, b"key1"],
        b"key2",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree 2 insert");

    // Insert an element into subtree
    db.insert(
        [TEST_LEAF, b"key1", b"key2"],
        b"key3",
        element.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");
    db.insert([TEST_LEAF], b"key4", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 3 insert");

    // Retrieve subtree instance
    // Check if it returns the same instance that was inserted
    {
        let subtree_storage = db
            .grove_db
            .db
            .get_storage_context([TEST_LEAF, b"key1", b"key2"])
            .unwrap();
        let subtree =
            Merk::open_layered_with_root_key(subtree_storage, Some(b"key3".to_vec()), false)
                .unwrap()
                .expect("cannot open merk");
        let result_element = Element::get(&subtree, b"key3").unwrap().unwrap();
        assert_eq!(result_element, Element::new_item(b"ayy".to_vec()));
    }
    // Insert a new tree with transaction
    let transaction = db.start_transaction();

    db.insert(
        [TEST_LEAF, b"key1"],
        b"innertree",
        Element::empty_tree(),
        None,
        Some(&transaction),
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"key1", b"innertree"],
        b"key4",
        element,
        None,
        Some(&transaction),
    )
    .unwrap()
    .expect("successful value insert");

    // Retrieve subtree instance with transaction
    let subtree_storage = db
        .grove_db
        .db
        .get_transactional_storage_context([TEST_LEAF, b"key1", b"innertree"], &transaction)
        .unwrap();
    let subtree = Merk::open_layered_with_root_key(subtree_storage, Some(b"key4".to_vec()), false)
        .unwrap()
        .expect("cannot open merk");
    let result_element = Element::get(&subtree, b"key4").unwrap().unwrap();
    assert_eq!(result_element, Element::new_item(b"ayy".to_vec()));

    // Should be able to retrieve instances created before transaction
    let subtree_storage = db
        .grove_db
        .db
        .get_storage_context([TEST_LEAF, b"key1", b"key2"])
        .unwrap();
    let subtree = Merk::open_layered_with_root_key(subtree_storage, Some(b"key3".to_vec()), false)
        .unwrap()
        .expect("cannot open merk");
    let result_element = Element::get(&subtree, b"key3").unwrap().unwrap();
    assert_eq!(result_element, Element::new_item(b"ayy".to_vec()));
}

#[cfg(feature = "full")]
#[test]
fn test_get_full_query() {
    let db = make_test_grovedb();

    // Insert a couple of subtrees first
    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert([TEST_LEAF], b"key2", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    // Insert some elements into subtree
    db.insert(
        [TEST_LEAF, b"key1"],
        b"key3",
        Element::new_item(b"ayya".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"key4",
        Element::new_item(b"ayyb".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"key5",
        Element::new_item(b"ayyc".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");
    db.insert(
        [TEST_LEAF, b"key2"],
        b"key6",
        Element::new_item(b"ayyd".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");

    let path1 = vec![TEST_LEAF.to_vec(), b"key1".to_vec()];
    let path2 = vec![TEST_LEAF.to_vec(), b"key2".to_vec()];
    let mut query1 = Query::new();
    let mut query2 = Query::new();
    query1.insert_range_inclusive(b"key3".to_vec()..=b"key4".to_vec());
    query2.insert_key(b"key6".to_vec());

    let path_query1 = PathQuery::new_unsized(path1, query1);
    let path_query2 = PathQuery::new_unsized(path2, query2);

    assert_eq!(
        db.query_many_raw(
            &[&path_query1, &path_query2],
            QueryKeyElementPairResultType,
            None
        )
        .unwrap()
        .expect("expected successful get_query")
        .to_key_elements(),
        vec![
            (b"key3".to_vec(), Element::new_item(b"ayya".to_vec())),
            (b"key4".to_vec(), Element::new_item(b"ayyb".to_vec())),
            (b"key6".to_vec(), Element::new_item(b"ayyd".to_vec())),
        ]
    );
}

#[cfg(feature = "full")]
#[test]
fn test_aux_uses_separate_cf() {
    let element = Element::new_item(b"ayy".to_vec());
    let db = make_test_grovedb();
    // Insert some nested subtrees
    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree 1 insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"key2",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        [TEST_LEAF, b"key1", b"key2"],
        b"key3",
        element.clone(),
        None,
        None,
    )
    .unwrap()
    .expect("successful value insert");

    db.put_aux(b"key1", b"a", None, None)
        .unwrap()
        .expect("cannot put aux");
    db.put_aux(b"key2", b"b", None, None)
        .unwrap()
        .expect("cannot put aux");
    db.put_aux(b"key3", b"c", None, None)
        .unwrap()
        .expect("cannot put aux");
    db.delete_aux(b"key3", None, None)
        .unwrap()
        .expect("cannot delete from aux");

    assert_eq!(
        db.get([TEST_LEAF, b"key1", b"key2"], b"key3", None)
            .unwrap()
            .expect("cannot get element"),
        element
    );
    assert_eq!(
        db.get_aux(b"key1", None)
            .unwrap()
            .expect("cannot get from aux"),
        Some(b"a".to_vec())
    );
    assert_eq!(
        db.get_aux(b"key2", None)
            .unwrap()
            .expect("cannot get from aux"),
        Some(b"b".to_vec())
    );
    assert_eq!(
        db.get_aux(b"key3", None)
            .unwrap()
            .expect("cannot get from aux"),
        None
    );
    assert_eq!(
        db.get_aux(b"key4", None)
            .unwrap()
            .expect("cannot get from aux"),
        None
    );
}

#[cfg(feature = "full")]
#[test]
fn test_aux_with_transaction() {
    let element = Element::new_item(b"ayy".to_vec());
    let aux_value = b"ayylmao".to_vec();
    let key = b"key".to_vec();
    let db = make_test_grovedb();
    let transaction = db.start_transaction();

    // Insert a regular data with aux data in the same transaction
    db.insert([TEST_LEAF], &key, element, None, Some(&transaction))
        .unwrap()
        .expect("unable to insert");
    db.put_aux(&key, &aux_value, None, Some(&transaction))
        .unwrap()
        .expect("unable to insert aux value");
    assert_eq!(
        db.get_aux(&key, Some(&transaction))
            .unwrap()
            .expect("unable to get aux value"),
        Some(aux_value.clone())
    );
    // Cannot reach the data outside of transaction
    assert_eq!(
        db.get_aux(&key, None)
            .unwrap()
            .expect("unable to get aux value"),
        None
    );
    // And should be able to get data when committed
    db.commit_transaction(transaction)
        .unwrap()
        .expect("unable to commit transaction");
    assert_eq!(
        db.get_aux(&key, None)
            .unwrap()
            .expect("unable to get committed aux value"),
        Some(aux_value)
    );
}

#[cfg(feature = "full")]
#[test]
fn test_root_hash() {
    let db = make_test_grovedb();
    // Check hashes are different if tree is edited
    let old_root_hash = db.root_hash(None).unwrap();
    db.insert(
        [TEST_LEAF],
        b"key1",
        Element::new_item(b"ayy".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("unable to insert an item");
    assert_ne!(old_root_hash.unwrap(), db.root_hash(None).unwrap().unwrap());

    // Check isolation
    let transaction = db.start_transaction();

    db.insert(
        [TEST_LEAF],
        b"key2",
        Element::new_item(b"ayy".to_vec()),
        None,
        Some(&transaction),
    )
    .unwrap()
    .expect("unable to insert an item");
    let root_hash_outside = db.root_hash(None).unwrap().unwrap();
    assert_ne!(
        db.root_hash(Some(&transaction)).unwrap().unwrap(),
        root_hash_outside
    );

    assert_eq!(db.root_hash(None).unwrap().unwrap(), root_hash_outside);
    db.commit_transaction(transaction).unwrap().unwrap();
    assert_ne!(db.root_hash(None).unwrap().unwrap(), root_hash_outside);
}

#[cfg(feature = "full")]
#[test]
fn test_get_non_existing_root_leaf() {
    let db = make_test_grovedb();
    assert!(matches!(db.get([], b"ayy", None).unwrap(), Err(_)));
}

#[cfg(feature = "full")]
#[test]
fn test_check_subtree_exists_function() {
    let db = make_test_grovedb();
    db.insert(
        [TEST_LEAF],
        b"key_scalar",
        Element::new_item(b"ayy".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("cannot insert item");
    db.insert(
        [TEST_LEAF],
        b"key_subtree",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("cannot insert item");

    // Empty tree path means root always exist
    assert!(db
        .check_subtree_exists_invalid_path([], None)
        .unwrap()
        .is_ok());

    // TEST_LEAF should be a tree
    assert!(db
        .check_subtree_exists_invalid_path([TEST_LEAF], None)
        .unwrap()
        .is_ok());

    // TEST_LEAF.key_subtree should be a tree
    assert!(db
        .check_subtree_exists_invalid_path([TEST_LEAF, b"key_subtree"], None)
        .unwrap()
        .is_ok());

    // TEST_LEAF.key_scalar should NOT be a tree
    assert!(matches!(
        db.check_subtree_exists_invalid_path([TEST_LEAF, b"key_scalar"], None)
            .unwrap(),
        Err(Error::InvalidPath(_))
    ));
}

#[cfg(feature = "full")]
#[test]
fn test_tree_value_exists_method_no_tx() {
    let db = make_test_grovedb();
    // Test keys in non-root tree
    db.insert(
        [TEST_LEAF],
        b"key",
        Element::new_item(b"ayy".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("cannot insert item");
    assert!(db.has_raw([TEST_LEAF], b"key", None).unwrap().unwrap());
    assert!(!db.has_raw([TEST_LEAF], b"badkey", None).unwrap().unwrap());

    // Test keys for a root tree
    db.insert([], b"leaf", Element::empty_tree(), None, None)
        .unwrap()
        .expect("cannot insert item");

    assert!(db.has_raw([], b"leaf", None).unwrap().unwrap());
    assert!(db.has_raw([], TEST_LEAF, None).unwrap().unwrap());
    assert!(!db.has_raw([], b"badleaf", None).unwrap().unwrap());
}

#[cfg(feature = "full")]
#[test]
fn test_tree_value_exists_method_tx() {
    let db = make_test_grovedb();
    let tx = db.start_transaction();
    // Test keys in non-root tree
    db.insert(
        [TEST_LEAF],
        b"key",
        Element::new_item(b"ayy".to_vec()),
        None,
        Some(&tx),
    )
    .unwrap()
    .expect("cannot insert item");
    assert!(db.has_raw([TEST_LEAF], b"key", Some(&tx)).unwrap().unwrap());
    assert!(!db.has_raw([TEST_LEAF], b"key", None).unwrap().unwrap());

    // Test keys for a root tree
    db.insert([], b"leaf", Element::empty_tree(), None, Some(&tx))
        .unwrap()
        .expect("cannot insert item");
    assert!(db.has_raw([], b"leaf", Some(&tx)).unwrap().unwrap());
    assert!(!db.has_raw([], b"leaf", None).unwrap().unwrap());

    db.commit_transaction(tx)
        .unwrap()
        .expect("cannot commit transaction");
    assert!(db.has_raw([TEST_LEAF], b"key", None).unwrap().unwrap());
    assert!(db.has_raw([], b"leaf", None).unwrap().unwrap());
}
