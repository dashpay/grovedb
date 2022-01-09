use std::ops::{Deref, DerefMut};

use merk::test_utils::TempMerk;
use tempdir::TempDir;
use rand::Rng;

use super::*;

const TEST_LEAF: &[u8] = b"test_leaf";
const ANOTHER_TEST_LEAF: &[u8] = b"test_leaf2";

/// GroveDB wrapper to keep temp directory alive
struct TempGroveDb {
    _tmp_dir: TempDir,
    db: GroveDb,
}

impl DerefMut for TempGroveDb {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.db
    }
}

impl Deref for TempGroveDb {
    type Target = GroveDb;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

/// A helper method to create GroveDB with one leaf for a root tree
fn make_grovedb() -> TempGroveDb {
    let tmp_dir = TempDir::new("db").unwrap();
    let mut db = GroveDb::open(tmp_dir.path()).unwrap();
    add_test_leafs(&mut db);
    TempGroveDb {
        _tmp_dir: tmp_dir,
        db,
    }
}

fn add_test_leafs(db: &mut GroveDb) {
    db.insert(&[], TEST_LEAF.to_vec(), Element::empty_tree())
        .expect("successful root tree leaf insert");
    db.insert(&[], ANOTHER_TEST_LEAF.to_vec(), Element::empty_tree())
        .expect("successful root tree leaf 2 insert");
}

#[test]
fn test_init() {
    let tmp_dir = TempDir::new("db").unwrap();
    GroveDb::open(tmp_dir).expect("empty tree is ok");
}

#[test]
fn test_insert_value_to_merk() {
    let mut db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    db.insert(&[TEST_LEAF], b"key".to_vec(), element.clone())
        .expect("successful insert");
    assert_eq!(
        db.get(&[TEST_LEAF], b"key").expect("succesful get"),
        element
    );
}

#[test]
fn test_insert_value_to_subtree() {
    let mut db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());

    // Insert a subtree first
    db.insert(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree insert");
    // Insert an element into subtree
    db.insert(&[TEST_LEAF, b"key1"], b"key2".to_vec(), element.clone())
        .expect("successful value insert");
    assert_eq!(
        db.get(&[TEST_LEAF, b"key1"], b"key2")
            .expect("succesful get"),
        element
    );
}

#[test]
fn test_changes_propagated() {
    let mut db = make_grovedb();
    let old_hash = db.root_tree.root();
    let element = Element::Item(b"ayy".to_vec());

    // Insert some nested subtrees
    db.insert(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(
        &[TEST_LEAF, b"key1"],
        b"key2".to_vec(),
        Element::empty_tree(),
    )
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        &[TEST_LEAF, b"key1", b"key2"],
        b"key3".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    assert_eq!(
        db.get(&[TEST_LEAF, b"key1", b"key2"], b"key3")
            .expect("succesful get"),
        element
    );
    assert_ne!(old_hash, db.root_tree.root());
}

#[test]
fn test_follow_references() {
    let mut db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());

    // Insert a reference
    db.insert(
        &[TEST_LEAF],
        b"reference_key".to_vec(),
        Element::Reference(vec![TEST_LEAF.to_vec(), b"key2".to_vec(), b"key3".to_vec()]),
    )
    .expect("successful reference insert");

    // Insert an item to refer to
    db.insert(&[TEST_LEAF], b"key2".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(&[TEST_LEAF, b"key2"], b"key3".to_vec(), element.clone())
        .expect("successful value insert");
    assert_eq!(
        db.get(&[TEST_LEAF], b"reference_key")
            .expect("succesful get"),
        element
    );
}

#[test]
fn test_cyclic_references() {
    let mut db = make_grovedb();

    db.insert(
        &[TEST_LEAF],
        b"reference_key_1".to_vec(),
        Element::Reference(vec![TEST_LEAF.to_vec(), b"reference_key_2".to_vec()]),
    )
    .expect("successful reference 1 insert");

    db.insert(
        &[TEST_LEAF],
        b"reference_key_2".to_vec(),
        Element::Reference(vec![TEST_LEAF.to_vec(), b"reference_key_1".to_vec()]),
    )
    .expect("successful reference 2 insert");

    assert!(matches!(
        db.get(&[TEST_LEAF], b"reference_key_1").unwrap_err(),
        Error::CyclicReference
    ));
}

#[test]
fn test_too_many_indirections() {
    use crate::operations::get::MAX_REFERENCE_HOPS;
    let mut db = make_grovedb();

    let keygen = |idx| format!("key{}", idx).bytes().collect::<Vec<u8>>();

    db.insert(
        &[TEST_LEAF],
        b"key0".to_vec(),
        Element::Item(b"oops".to_vec()),
    )
    .expect("successful item insert");

    for i in 1..=(MAX_REFERENCE_HOPS + 1) {
        db.insert(
            &[TEST_LEAF],
            keygen(i),
            Element::Reference(vec![TEST_LEAF.to_vec(), keygen(i - 1)]),
        )
        .expect("successful reference insert");
    }

    assert!(matches!(
        db.get(&[TEST_LEAF], &keygen(MAX_REFERENCE_HOPS + 1))
            .unwrap_err(),
        Error::ReferenceLimit
    ));
}

#[test]
fn test_tree_structure_is_presistent() {
    let tmp_dir = TempDir::new("db").unwrap();
    let element = Element::Item(b"ayy".to_vec());
    // Create a scoped GroveDB
    {
        let mut db = GroveDb::open(tmp_dir.path()).unwrap();
        add_test_leafs(&mut db);

        // Insert some nested subtrees
        db.insert(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
            .expect("successful subtree 1 insert");
        db.insert(
            &[TEST_LEAF, b"key1"],
            b"key2".to_vec(),
            Element::empty_tree(),
        )
        .expect("successful subtree 2 insert");
        // Insert an element into subtree
        db.insert(
            &[TEST_LEAF, b"key1", b"key2"],
            b"key3".to_vec(),
            element.clone(),
        )
        .expect("successful value insert");
        assert_eq!(
            db.get(&[TEST_LEAF, b"key1", b"key2"], b"key3")
                .expect("succesful get 1"),
            element
        );
    }
    // Open a persisted GroveDB
    let db = GroveDb::open(tmp_dir).unwrap();
    assert_eq!(
        db.get(&[TEST_LEAF, b"key1", b"key2"], b"key3")
            .expect("succesful get 2"),
        element
    );
    assert!(db.get(&[TEST_LEAF, b"key1", b"key2"], b"key4").is_err());
}

#[test]
fn test_root_tree_leafs_are_noted() {
    let db = make_grovedb();
    let mut hm = HashMap::new();
    hm.insert(GroveDb::compress_subtree_key(&[TEST_LEAF], None), 0);
    hm.insert(GroveDb::compress_subtree_key(&[ANOTHER_TEST_LEAF], None), 1);
    assert_eq!(db.root_leaf_keys, hm);
    assert_eq!(db.root_tree.leaves_len(), 2);
}

#[test]
fn test_proof_construction() {
    // Tree Structure
    // root
    //     test_leaf
    //         innertree
    //             k1,v1
    //             k2,v2
    //     another_test_leaf
    //         innertree2
    //             k3,v3
    //         innertree3
    //             k4,v4

    // Insert elements into grovedb instance
    let mut temp_db = make_grovedb();
    // Insert level 1 nodes
    temp_db
        .insert(&[TEST_LEAF], b"innertree".to_vec(), Element::empty_tree())
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF],
            b"innertree2".to_vec(),
            Element::empty_tree(),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF],
            b"innertree3".to_vec(),
            Element::empty_tree(),
        )
        .expect("successful subtree insert");
    // Insert level 2 nodes
    temp_db
        .insert(
            &[TEST_LEAF, b"innertree"],
            b"key1".to_vec(),
            Element::Item(b"value1".to_vec()),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[TEST_LEAF, b"innertree"],
            b"key2".to_vec(),
            Element::Item(b"value2".to_vec()),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF, b"innertree2"],
            b"key3".to_vec(),
            Element::Item(b"value3".to_vec()),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF, b"innertree3"],
            b"key4".to_vec(),
            Element::Item(b"value4".to_vec()),
        )
        .expect("successful subtree insert");

    // Manually construct HADS bottom up
    // Insert level 2 nodes
    let mut inner_tree = TempMerk::new();
    let value_one = Element::Item(b"value1".to_vec());
    value_one.insert(&mut inner_tree, b"key1".to_vec());
    let value_two = Element::Item(b"value2".to_vec());
    value_two.insert(&mut inner_tree, b"key2".to_vec());

    let mut inner_tree_2 = TempMerk::new();
    let value_three = Element::Item(b"value3".to_vec());
    value_three.insert(&mut inner_tree_2, b"key3".to_vec());

    let mut inner_tree_3 = TempMerk::new();
    let value_four = Element::Item(b"value4".to_vec());
    value_four.insert(&mut inner_tree_3, b"key4".to_vec());
    // Insert level 1 nodes
    let mut test_leaf = TempMerk::new();
    let inner_tree_root = Element::Tree(inner_tree.root_hash());
    inner_tree_root.insert(&mut test_leaf, b"innertree".to_vec());
    let mut another_test_leaf = TempMerk::new();
    let inner_tree_2_root = Element::Tree(inner_tree_2.root_hash());
    inner_tree_2_root.insert(&mut another_test_leaf, b"innertree2".to_vec());
    let inner_tree_3_root = Element::Tree(inner_tree_3.root_hash());
    inner_tree_3_root.insert(&mut another_test_leaf, b"innertree3".to_vec());
    // Insert root nodes
    let leaves = [test_leaf.root_hash(), another_test_leaf.root_hash()];
    let root_tree = MerkleTree::<Sha256>::from_leaves(&leaves);

    // Proof construction
    // Generating a proof for two paths
    // root -> test_leaf -> innertree (prove both k1 and k2)
    // root -> another_test_leaf -> innertree3 (prove k4)
    // root -> another_test_leaf -> innertree2 (prove k3)

    // Build reusable query objects
    let mut path_one_query = Query::new();
    path_one_query.insert_key(b"key1".to_vec());
    path_one_query.insert_key(b"key2".to_vec());

    let mut path_two_query = Query::new();
    path_two_query.insert_key(b"key4".to_vec());

    let mut path_three_query = Query::new();
    path_three_query.insert_key(b"key3".to_vec());

    // Get grovedb proof
    let proof = temp_db
        .proof(vec![
            PathQuery::new_unsized_basic(&[TEST_LEAF, b"innertree"], path_one_query),
            PathQuery::new_unsized_basic(&[ANOTHER_TEST_LEAF, b"innertree3"], path_two_query),
            PathQuery::new_unsized_basic(&[ANOTHER_TEST_LEAF, b"innertree2"], path_three_query),
        ])
        .unwrap();

    // Deserialize the proof
    let proof: Proof = bincode::deserialize(&proof).unwrap();

    // Perform assertions
    assert_eq!(proof.query_paths.len(), 3);
    assert_eq!(proof.query_paths[0], &[TEST_LEAF, b"innertree"]);
    assert_eq!(proof.query_paths[1], &[ANOTHER_TEST_LEAF, b"innertree3"]);
    assert_eq!(proof.query_paths[2], &[ANOTHER_TEST_LEAF, b"innertree2"]);

    // For path 1 to path 3, there are 9 nodes
    // root is repeated three times and another_test_leaf is repeated twice
    // Accounting for duplication, there are 6 unique nodes
    // root, test_leaf, another_test_leaf, innertree, innertree2, innertree3
    // proof.proofs contains all nodes except the root so we expect 5 sub proofs
    assert_eq!(proof.proofs.len(), 5);

    // Check that all the subproofs were constructed correctly for each path and
    // subpath
    let path_one_as_vec = GroveDb::compress_subtree_key(&[TEST_LEAF, b"innertree"], None);
    let path_two_as_vec = GroveDb::compress_subtree_key(&[ANOTHER_TEST_LEAF, b"innertree3"], None);
    let path_three_as_vec =
        GroveDb::compress_subtree_key(&[ANOTHER_TEST_LEAF, b"innertree2"], None);
    let test_leaf_path_as_vec = GroveDb::compress_subtree_key(&[TEST_LEAF], None);
    let another_test_leaf_path_as_vec = GroveDb::compress_subtree_key(&[ANOTHER_TEST_LEAF], None);

    let proof_for_path_one = proof.proofs.get(&path_one_as_vec).unwrap();
    let proof_for_path_two = proof.proofs.get(&path_two_as_vec).unwrap();
    let proof_for_path_three = proof.proofs.get(&path_three_as_vec).unwrap();
    let proof_for_test_leaf = proof.proofs.get(&test_leaf_path_as_vec).unwrap();
    let proof_for_another_test_leaf = proof.proofs.get(&another_test_leaf_path_as_vec).unwrap();

    // Assert path 1 proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"key1".to_vec());
    proof_query.insert_key(b"key2".to_vec());
    assert_eq!(*proof_for_path_one, inner_tree.prove(proof_query, None, None, true).unwrap());

    // Assert path 2 proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"key4".to_vec());
    assert_eq!(
        *proof_for_path_two,
        inner_tree_3.prove(proof_query, None, None, true).unwrap()
    );

    // Assert path 3 proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"key3".to_vec());
    assert_eq!(
        *proof_for_path_three,
        inner_tree_2.prove(proof_query, None, None, true).unwrap()
    );

    // Assert test leaf proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"innertree".to_vec());
    assert_eq!(*proof_for_test_leaf, test_leaf.prove(proof_query, None, None, true).unwrap());

    // Assert another test leaf proof
    // another test leaf appeared in two path,
    // hence it should contain proofs for both keys
    let mut proof_query = Query::new();
    proof_query.insert_key(b"innertree2".to_vec());
    proof_query.insert_key(b"innertree3".to_vec());
    assert_eq!(
        *proof_for_another_test_leaf,
        another_test_leaf.prove(proof_query, None, None, true).unwrap()
    );

    // Check that the root proof is valid
    // Root proof should contain proof for both test_leaf and another_test_leaf
    let test_leaf_root_key = GroveDb::compress_subtree_key(&[], Some(TEST_LEAF));
    let another_test_leaf_root_key = GroveDb::compress_subtree_key(&[], Some(ANOTHER_TEST_LEAF));
    assert_eq!(
        proof.root_proof,
        root_tree
            .proof(&[
                temp_db.root_leaf_keys[&test_leaf_root_key],
                temp_db.root_leaf_keys[&another_test_leaf_root_key],
            ])
            .to_bytes()
    );

    // Assert that we got the correct root leaf keys
    assert_eq!(proof.root_leaf_keys.len(), 2);
    assert_eq!(proof.root_leaf_keys[&test_leaf_root_key], 0);
    assert_eq!(proof.root_leaf_keys[&another_test_leaf_root_key], 1);
}

#[test]
fn test_successful_proof_verification() {
    // Build a grovedb database
    // Tree Structure
    // root
    //     test_leaf
    //         innertree
    //             k1,v1
    //             k2,v2
    //     another_test_leaf
    //         innertree2
    //             k3,v3
    //         innertree3
    //             k4,v4

    // Insert elements into grovedb instance
    let mut temp_db = make_grovedb();
    // Insert level 1 nodes
    temp_db
        .insert(&[TEST_LEAF], b"innertree".to_vec(), Element::empty_tree())
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF],
            b"innertree2".to_vec(),
            Element::empty_tree(),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF],
            b"innertree3".to_vec(),
            Element::empty_tree(),
        )
        .expect("successful subtree insert");
    // Insert level 2 nodes
    temp_db
        .insert(
            &[TEST_LEAF, b"innertree"],
            b"key1".to_vec(),
            Element::Item(b"value1".to_vec()),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[TEST_LEAF, b"innertree"],
            b"key2".to_vec(),
            Element::Item(b"value2".to_vec()),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF, b"innertree2"],
            b"key3".to_vec(),
            Element::Item(b"value3".to_vec()),
        )
        .expect("successful subtree insert");
    temp_db
        .insert(
            &[ANOTHER_TEST_LEAF, b"innertree3"],
            b"key4".to_vec(),
            Element::Item(b"value4".to_vec()),
        )
        .expect("successful subtree insert");

    // Single query proof verification
    let mut path_one_query = Query::new();
    path_one_query.insert_key(b"key1".to_vec());
    path_one_query.insert_key(b"key2".to_vec());

    let proof = temp_db
        .proof(vec![PathQuery::new_unsized_basic(
            &[TEST_LEAF, b"innertree"],
            path_one_query,
        )])
        .unwrap();

    // Assert correct root hash
    let (root_hash, result_maps) = GroveDb::execute_proof(proof).unwrap();
    assert_eq!(temp_db.root_tree.root().unwrap(), root_hash);

    // Assert correct result object
    // Proof query was for two keys key1 and key2
    let path_as_vec = GroveDb::compress_subtree_key(&[TEST_LEAF, b"innertree"], None);
    let result_map = result_maps.get(&path_as_vec).unwrap();
    let elem_1: Element = bincode::deserialize(result_map.get(b"key1").unwrap().unwrap()).unwrap();
    let elem_2: Element = bincode::deserialize(result_map.get(b"key2").unwrap().unwrap()).unwrap();
    assert_eq!(elem_1, Element::Item(b"value1".to_vec()));
    assert_eq!(elem_2, Element::Item(b"value2".to_vec()));

    // Multi query proof verification
    let mut path_two_query = Query::new();
    path_two_query.insert_key(b"key4".to_vec());

    let mut path_three_query = Query::new();
    path_three_query.insert_key(b"key3".to_vec());

    // Get grovedb proof
    let proof = temp_db
        .proof(vec![
            PathQuery::new_unsized_basic(&[ANOTHER_TEST_LEAF, b"innertree3"], path_two_query),
            PathQuery::new_unsized_basic(&[ANOTHER_TEST_LEAF, b"innertree2"], path_three_query),
        ])
        .unwrap();

    // Assert correct root hash
    let (root_hash, result_maps) = GroveDb::execute_proof(proof).unwrap();
    assert_eq!(temp_db.root_tree.root().unwrap(), root_hash);

    // Assert correct result object
    let path_one_as_vec = GroveDb::compress_subtree_key(&[ANOTHER_TEST_LEAF, b"innertree3"], None);
    let result_map = result_maps.get(&path_one_as_vec).unwrap();
    let elem: Element = bincode::deserialize(result_map.get(b"key4").unwrap().unwrap()).unwrap();
    assert_eq!(elem, Element::Item(b"value4".to_vec()));

    let path_two_as_vec = GroveDb::compress_subtree_key(&[ANOTHER_TEST_LEAF, b"innertree2"], None);
    let result_map = result_maps.get(&path_two_as_vec).unwrap();
    let elem: Element = bincode::deserialize(result_map.get(b"key3").unwrap().unwrap()).unwrap();
    assert_eq!(elem, Element::Item(b"value3".to_vec()));
}

#[test]
fn test_checkpoint() {
    let mut db = make_grovedb();
    let element1 = Element::Item(b"ayy".to_vec());

    db.insert(&[], b"key1".to_vec(), Element::empty_tree())
        .expect("cannot insert a subtree 1 into GroveDB");
    db.insert(&[b"key1"], b"key2".to_vec(), Element::empty_tree())
        .expect("cannot insert a subtree 2 into GroveDB");
    db.insert(&[b"key1", b"key2"], b"key3".to_vec(), element1.clone())
        .expect("cannot insert an item into GroveDB");
    assert_eq!(
        db.get(&[b"key1", b"key2"], b"key3")
            .expect("cannot get from grovedb"),
        element1
    );

    let checkpoint_tempdir = TempDir::new("checkpoint").expect("cannot open tempdir");
    let mut checkpoint = db
        .checkpoint(checkpoint_tempdir.path().join("checkpoint"))
        .expect("cannot create a checkpoint");

    assert_eq!(
        db.get(&[b"key1", b"key2"], b"key3")
            .expect("cannot get from grovedb"),
        element1
    );
    assert_eq!(
        checkpoint
            .get(&[b"key1", b"key2"], b"key3")
            .expect("cannot get from checkpoint"),
        element1
    );

    let element2 = Element::Item(b"ayy2".to_vec());
    let element3 = Element::Item(b"ayy3".to_vec());

    checkpoint
        .insert(&[b"key1"], b"key4".to_vec(), element2.clone())
        .expect("cannot insert into checkpoint");

    db.insert(&[b"key1"], b"key4".to_vec(), element3.clone())
        .expect("cannot insert into GroveDB");

    assert_eq!(
        checkpoint
            .get(&[b"key1"], b"key4")
            .expect("cannot get from checkpoint"),
        element2,
    );

    assert_eq!(
        db.get(&[b"key1"], b"key4")
            .expect("cannot get from GroveDB"),
        element3
    );

    checkpoint
        .insert(&[b"key1"], b"key5".to_vec(), element3.clone())
        .expect("cannot insert into checkpoint");

    db.insert(&[b"key1"], b"key6".to_vec(), element3.clone())
        .expect("cannot insert into GroveDB");

    assert!(matches!(
        checkpoint.get(&[b"key1"], b"key6"),
        Err(Error::InvalidPath(_))
    ));

    assert!(matches!(
        db.get(&[b"key1"], b"key5"),
        Err(Error::InvalidPath(_))
    ));
}

#[test]
fn test_insert_if_not_exists() {
    let mut db = make_grovedb();

    // Insert twice at the same path
    assert_eq!(
        db.insert_if_not_exists(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
            .expect("Provided valid path"),
        true
    );
    assert_eq!(
        db.insert_if_not_exists(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
            .expect("Provided valid path"),
        false
    );

    // Should propagate errors from insertion
    let result = db.insert_if_not_exists(
        &[TEST_LEAF, b"unknown"],
        b"key1".to_vec(),
        Element::empty_tree(),
    );
    assert!(matches!(result, Err(Error::InvalidPath(_))));
}

#[test]
fn test_subtree_pairs_iterator() {
    let mut db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    let element2 = Element::Item(b"lmao".to_vec());

    // Insert some nested subtrees
    db.insert(&[TEST_LEAF], b"subtree1".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(
        &[TEST_LEAF, b"subtree1"],
        b"subtree11".to_vec(),
        Element::empty_tree(),
    )
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        &[TEST_LEAF, b"subtree1", b"subtree11"],
        b"key1".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    assert_eq!(
        db.get(&[TEST_LEAF, b"subtree1", b"subtree11"], b"key1")
            .expect("succesful get 1"),
        element
    );
    db.insert(
        &[TEST_LEAF, b"subtree1", b"subtree11"],
        b"key0".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    db.insert(
        &[TEST_LEAF, b"subtree1"],
        b"subtree12".to_vec(),
        Element::empty_tree(),
    )
    .expect("successful subtree 3 insert");
    db.insert(&[TEST_LEAF, b"subtree1"], b"key1".to_vec(), element.clone())
        .expect("succesful value insert");
    db.insert(
        &[TEST_LEAF, b"subtree1"],
        b"key2".to_vec(),
        element2.clone(),
    )
    .expect("succesful value insert");

    // Iterate over subtree1 to see if keys of other subtrees messed up
    let mut iter = db
        .elements_iterator(&[TEST_LEAF, b"subtree1"])
        .expect("cannot create iterator");
    assert_eq!(iter.next().unwrap(), Some((b"key1".to_vec(), element)));
    assert_eq!(iter.next().unwrap(), Some((b"key2".to_vec(), element2)));
    let subtree_element = iter.next().unwrap().unwrap();
    assert_eq!(subtree_element.0, b"subtree11".to_vec());
    assert!(matches!(subtree_element.1, Element::Tree(_)));
    let subtree_element = iter.next().unwrap().unwrap();
    assert_eq!(subtree_element.0, b"subtree12".to_vec());
    assert!(matches!(subtree_element.1, Element::Tree(_)));
    assert!(matches!(iter.next(), Ok(None)));
}

#[test]
fn test_compress_path_not_possible_collision() {
    let path_a = [b"aa".as_ref(), b"b"];
    let path_b = [b"a".as_ref(), b"ab"];
    assert_ne!(
        GroveDb::compress_subtree_key(&path_a, None),
        GroveDb::compress_subtree_key(&path_b, None)
    );
    assert_eq!(
        GroveDb::compress_subtree_key(&path_a, None),
        GroveDb::compress_subtree_key(&path_a, None),
    );
}

#[test]
fn test_element_deletion() {
    let mut db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    db.insert(&[TEST_LEAF], b"key".to_vec(), element.clone())
        .expect("successful insert");
    let root_hash = db.root_tree.root().unwrap();
    assert!(db.delete(&[TEST_LEAF], b"key".to_vec()).is_ok(),);
    assert!(matches!(
        db.get(&[TEST_LEAF], b"key"),
        Err(Error::InvalidPath(_))
    ));
    assert_ne!(root_hash, db.root_tree.root().unwrap());
}

#[test]
fn test_find_subtrees() {
    let element = Element::Item(b"ayy".to_vec());
    let mut db = make_grovedb();
    // Insert some nested subtrees
    db.insert(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(
        &[TEST_LEAF, b"key1"],
        b"key2".to_vec(),
        Element::empty_tree(),
    )
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        &[TEST_LEAF, b"key1", b"key2"],
        b"key3".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    db.insert(&[TEST_LEAF], b"key4".to_vec(), Element::empty_tree())
        .expect("successful subtree 3 insert");
    let subtrees = db
        .find_subtrees(vec![TEST_LEAF.to_vec()])
        .expect("cannot get subtrees");
    assert_eq!(
        vec![
            vec![TEST_LEAF.to_vec()],
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            vec![TEST_LEAF.to_vec(), b"key4".to_vec()],
            vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"key2".to_vec()],
        ],
        subtrees
    );
}

#[test]
fn test_subtree_deletion() {
    let element = Element::Item(b"ayy".to_vec());
    let mut db = make_grovedb();
    // Insert some nested subtrees
    db.insert(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(
        &[TEST_LEAF, b"key1"],
        b"key2".to_vec(),
        Element::empty_tree(),
    )
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        &[TEST_LEAF, b"key1", b"key2"],
        b"key3".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    db.insert(&[TEST_LEAF], b"key4".to_vec(), Element::empty_tree())
        .expect("successful subtree 3 insert");

    let root_hash = db.root_tree.root().unwrap();
    db.delete(&[TEST_LEAF], b"key1".to_vec())
        .expect("unable to delete subtree");
    assert!(matches!(
        db.get(&[TEST_LEAF, b"key1", b"key2"], b"key3"),
        Err(Error::InvalidPath(_))
    ));
    assert_eq!(db.subtrees.len(), 3); // TEST_LEAF, ANOTHER_TEST_LEAF and TEST_LEAF.key4 stay
    assert!(db.get(&[TEST_LEAF], b"key4").is_ok());
    assert_ne!(root_hash, db.root_tree.root().unwrap());
}

#[test]
fn test_get_full_query() {
    let mut db = make_grovedb();

    // Insert a couple of subtrees first
    db.insert(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree insert");
    db.insert(&[TEST_LEAF], b"key2".to_vec(), Element::empty_tree())
        .expect("successful subtree insert");
    // Insert some elements into subtree
    db.insert(
        &[TEST_LEAF, b"key1"],
        b"key3".to_vec(),
        Element::Item(b"ayya".to_vec()),
    )
    .expect("successful value insert");
    db.insert(
        &[TEST_LEAF, b"key1"],
        b"key4".to_vec(),
        Element::Item(b"ayyb".to_vec()),
    )
    .expect("successful value insert");
    db.insert(
        &[TEST_LEAF, b"key1"],
        b"key5".to_vec(),
        Element::Item(b"ayyc".to_vec()),
    )
    .expect("successful value insert");
    db.insert(
        &[TEST_LEAF, b"key2"],
        b"key6".to_vec(),
        Element::Item(b"ayyd".to_vec()),
    )
    .expect("successful value insert");

    let path1 = vec![TEST_LEAF, b"key1"];
    let path2 = vec![TEST_LEAF, b"key2"];
    let mut query1 = Query::new();
    let mut query2 = Query::new();
    query1.insert_range_inclusive(b"key3".to_vec()..=b"key4".to_vec());
    query2.insert_key(b"key6".to_vec());

    let path_query1 = PathQuery::new_unsized_basic(&path1, query1);
    let path_query2 = PathQuery::new_unsized_basic(&path2, query2);

    assert_eq!(
        db.get_path_queries(&[&path_query1, &path_query2])
            .expect("expected successful get_query"),
        vec![
            subtree::Element::Item(b"ayya".to_vec()),
            subtree::Element::Item(b"ayyb".to_vec()),
            subtree::Element::Item(b"ayyd".to_vec()),
        ]
    );
}

fn populate_tree_for_range_subquery(mut db: TempGroveDb) {
    // Insert a couple of subtrees first
    for i in b"1985"..b"2000" {
        db.insert(&[TEST_LEAF], i.to_vec(), Element::empty_tree())
            .expect("successful subtree insert");
        // Insert element 0
        // Insert some elements into subtree
        db.insert(
            &[TEST_LEAF, i],
            b"0".to_vec(),
            Element::empty_tree()).expect("successful subtree insert");

        for j in b"100"..b"130" {
            db.insert(
                &[TEST_LEAF, i, b"0"],
                rand::thread_rng().gen::<[u8; 32]>(),
                Element::Item(j.to_vec()),
            )
                .expect("successful value insert");
        }
    }
}

#[test]
fn test_get_full_query_with_non_unique_subquery() {
    let mut db = make_grovedb();

    populate_tree_for_range_subquery(db);

    let path = vec![TEST_LEAF];
    let mut query = Query::new();
    query1.insert_range_inclusive(b"1988".to_vec()..=b"1992".to_vec());

    let subquery_key = b"0";

    let mut query = Query::new();
    query.insert_key()

    let path_query = PathQuery::new_unsized(&path1, query1, Some(&subquery_key), None);
    let (elements, skipped) = db.get_path_query(&path_query)
        .expect("expected successful get_path_query");
    assert_eq!(
        elements
        vec![
            subtree::Element::Item(b"ayya".to_vec()),
            subtree::Element::Item(b"ayyb".to_vec()),
            subtree::Element::Item(b"ayyd".to_vec()),
        ]
    );
}

#[test]
fn test_aux_uses_separate_cf() {
    let element = Element::Item(b"ayy".to_vec());
    let mut db = make_grovedb();
    // Insert some nested subtrees
    db.insert(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(
        &[TEST_LEAF, b"key1"],
        b"key2".to_vec(),
        Element::empty_tree(),
    )
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        &[TEST_LEAF, b"key1", b"key2"],
        b"key3".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");

    db.put_aux(b"key1", b"a").expect("cannot put aux");
    db.put_aux(b"key2", b"b").expect("cannot put aux");
    db.put_aux(b"key3", b"c").expect("cannot put aux");
    db.delete_aux(b"key3").expect("cannot delete from aux");

    assert_eq!(
        db.get(&[TEST_LEAF, b"key1", b"key2"], b"key3")
            .expect("cannot get element"),
        element
    );
    assert_eq!(
        db.get_aux(b"key1").expect("cannot get from aux"),
        Some(b"a".to_vec())
    );
    assert_eq!(
        db.get_aux(b"key2").expect("cannot get from aux"),
        Some(b"b".to_vec())
    );
    assert_eq!(db.get_aux(b"key3").expect("cannot get from aux"), None,);
    assert_eq!(db.get_aux(b"key4").expect("cannot get from aux"), None);
}
