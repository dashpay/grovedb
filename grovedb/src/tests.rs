use std::{
    ops::{Deref, DerefMut},
    os::unix::fs::chroot,
};

use merk::test_utils::TempMerk;
use tempdir::TempDir;

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
    hm.insert(TEST_LEAF.to_vec(), 0);
    hm.insert(ANOTHER_TEST_LEAF.to_vec(), 1);
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
            ProofQuery {
                path: &[TEST_LEAF, b"innertree"],
                query: path_one_query,
            },
            ProofQuery {
                path: &[ANOTHER_TEST_LEAF, b"innertree3"],
                query: path_two_query,
            },
            ProofQuery {
                path: &[ANOTHER_TEST_LEAF, b"innertree2"],
                query: path_three_query,
            },
        ])
        .unwrap();

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
    let path_one_as_vec = GroveDb::compress_path(proof.query_paths[0], None);
    let path_two_as_vec = GroveDb::compress_path(proof.query_paths[1], None);
    let path_three_as_vec = GroveDb::compress_path(proof.query_paths[2], None);
    let test_leaf_path_as_vec = GroveDb::compress_path(&[TEST_LEAF], None);
    let another_test_leaf_path_as_vec = GroveDb::compress_path(&[ANOTHER_TEST_LEAF], None);

    let proof_for_path_one = proof.proofs.get(&path_one_as_vec).unwrap();
    let proof_for_path_two = proof.proofs.get(&path_two_as_vec).unwrap();
    let proof_for_path_three = proof.proofs.get(&path_three_as_vec).unwrap();
    let proof_for_test_leaf = proof.proofs.get(&test_leaf_path_as_vec).unwrap();
    let proof_for_another_test_leaf = proof.proofs.get(&another_test_leaf_path_as_vec).unwrap();

    // Assert path 1 proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"key1".to_vec());
    proof_query.insert_key(b"key2".to_vec());
    assert_eq!(*proof_for_path_one, inner_tree.prove(proof_query).unwrap());

    // Assert path 2 proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"key4".to_vec());
    assert_eq!(
        *proof_for_path_two,
        inner_tree_3.prove(proof_query).unwrap()
    );

    // Assert path 3 proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"key3".to_vec());
    assert_eq!(
        *proof_for_path_three,
        inner_tree_2.prove(proof_query).unwrap()
    );

    // Assert test leaf proof
    let mut proof_query = Query::new();
    proof_query.insert_key(b"innertree".to_vec());
    assert_eq!(*proof_for_test_leaf, test_leaf.prove(proof_query).unwrap());

    // Assert another test leaf proof
    // another test leaf appeared in two path,
    // hence it should contain proofs for both keys
    let mut proof_query = Query::new();
    proof_query.insert_key(b"innertree2".to_vec());
    proof_query.insert_key(b"innertree3".to_vec());
    assert_eq!(
        *proof_for_another_test_leaf,
        another_test_leaf.prove(proof_query).unwrap()
    );
}

// #[test]
// fn test_proof_construction() {
//     let mut temp_db = make_grovedb();
//     temp_db
//         .insert(&[TEST_LEAF], b"innertree".to_vec(), Element::empty_tree())
//         .expect("successful subtree insert");
//     temp_db
//         .insert(
//             &[TEST_LEAF, b"innertree"],
//             b"key1".to_vec(),
//             Element::Item(b"value1".to_vec()),
//         )
//         .expect("successful item insert");
//     temp_db
//         .insert(
//             &[TEST_LEAF, b"innertree"],
//             b"key2".to_vec(),
//             Element::Item(b"value2".to_vec()),
//         )
//         .expect("successful item insert");
//
//     // Manually build the ads structures
//     let mut inner_tree_merk = TempMerk::new();
//     let value_element = Element::Item(b"value1".to_vec());
//     value_element.insert(&mut inner_tree_merk, b"key1".to_vec());
//     let value_element = Element::Item(b"value2".to_vec());
//     value_element.insert(&mut inner_tree_merk, b"key2".to_vec());
//
//     let mut test_leaf_merk = TempMerk::new();
//     let inner_tree_root_element = Element::Tree(inner_tree_merk.root_hash());
//     inner_tree_root_element.insert(&mut test_leaf_merk,
// b"innertree".to_vec());
//
//     let another_test_leaf_merk = TempMerk::new();
//
//     let leaves = [
//         test_leaf_merk.root_hash(),
//         another_test_leaf_merk.root_hash(),
//     ];
//     let root_tree = MerkleTree::<Sha256>::from_leaves(&leaves);
//
//     // Generate grove db proof
//     let mut proof_query = Query::new();
//     proof_query.insert_key(b"key1".to_vec());
//     let proof = temp_db
//         .proof(&[TEST_LEAF, b"innertree"], proof_query)
//         .expect("Successful proof generation");
//
//     assert_eq!(proof.len(), 4);
//
//     let mut proof_query = Query::new();
//     proof_query.insert_key(b"key1".to_vec());
//     assert_eq!(proof[0], inner_tree_merk.prove(proof_query).unwrap());
//
//     let mut proof_query = Query::new();
//     proof_query.insert_key(b"innertree".to_vec());
//     assert_eq!(proof[1], test_leaf_merk.prove(proof_query).unwrap());
//
//     assert_eq!(proof[2], root_tree.proof(&vec![0]).to_bytes());
//
//     let root_leaf_keys: HashMap<Vec<u8>, usize> =
// bincode::deserialize(&proof[3][..]).unwrap();     assert_eq!(root_leaf_keys.
// len(), temp_db.root_leaf_keys.len());     for (key, index) in &root_leaf_keys
// {         assert_eq!(root_leaf_keys[key], temp_db.root_leaf_keys[key]);
//     }
// }

// #[test]
// fn test_successful_proof_verification() {
//     let mut temp_db = make_grovedb();
//     temp_db
//         .insert(&[TEST_LEAF], b"innertree".to_vec(), Element::empty_tree())
//         .expect("successful subtree insert");
//     temp_db
//         .insert(
//             &[TEST_LEAF, b"innertree"],
//             b"innertree1.1".to_vec(),
//             Element::empty_tree(),
//         )
//         .expect("successful subtree insert");
//
//     temp_db
//         .insert(&[TEST_LEAF], b"innertree2".to_vec(), Element::empty_tree())
//         .expect("successful subtree insert");
//
//     temp_db
//         .insert(
//             &[TEST_LEAF, b"innertree", b"innertree1.1"],
//             b"key1".to_vec(),
//             Element::Item(b"value1".to_vec()),
//         )
//         .expect("successful item insert");
//
//     temp_db
//         .insert(
//             &[TEST_LEAF, b"innertree2"],
//             b"key1".to_vec(),
//             Element::Item(b"value2".to_vec()),
//         )
//         .expect("successful item insert");
//
//     // dbg!(temp_db.root_tree.root().unwrap());
//
//     let mut proof_query = Query::new();
//     proof_query.insert_key(b"key1".to_vec());
//     let mut proof = temp_db
//         .proof(&[TEST_LEAF, b"innertree", b"innertree1.1"], proof_query)
//         .unwrap();
//
//     let (root_hash, result_map) =
//         GroveDb::execute_proof(&[TEST_LEAF, b"innertree", b"innertree1.1"],
// &mut proof).unwrap();
//
//     // Check that the root hash matches
//     assert_eq!(temp_db.root_tree.root().unwrap(), root_hash);
//
//     // Check that the result map is correct
//     let elem: Element =
// bincode::deserialize(result_map.get(b"key1").unwrap().unwrap()).unwrap();
//     assert_eq!(elem, Element::Item(b"value1".to_vec()));
// }
//
// #[test]
// #[should_panic]
// fn test_malicious_proof_verification() {
//     // Verification should detect when the proofs don't follow a valid path
//     // i.e. root - leaf (with each individual merk connected to their parent
// by     // their root hash) Grovedb enforces a valid path, so will manually
//     // construct a malicious proof
//
//     // 4 trees, merk_one, merk_two, merk_three, root
//     // root references m3, m3 references m1 (instead of m2), m2 references m1
//     // m3 breaks the chain and as such the proof should not be considered
// valid
//
//     let mut proofs: Vec<Vec<u8>> = Vec::new();
//
//     // Merk One
//     let mut merk_one = TempMerk::new();
//     let value_element = Element::Item(b"value1".to_vec());
//     value_element.insert(&mut merk_one, b"key1".to_vec());
//
//     let mut proof_query = Query::new();
//     proof_query.insert_key(b"key1".to_vec());
//     proofs.push(merk_one.prove(proof_query).unwrap());
//
//     // Merk Two
//     let mut merk_two = TempMerk::new();
//     let merk_two_element = Element::Tree(merk_one.root_hash());
//     merk_two_element.insert(&mut merk_two, b"innertree-2".to_vec());
//
//     let mut proof_query = Query::new();
//     proof_query.insert_key(b"innertree-2".to_vec());
//     proofs.push(merk_two.prove(proof_query).unwrap());
//
//     // Merk Three
//     let mut merk_three = TempMerk::new();
//     let merk_three_element = Element::Tree(merk_one.root_hash());
//     merk_three_element.insert(&mut merk_three, b"innertree".to_vec());
//
//     let mut proof_query = Query::new();
//     proof_query.insert_key(b"innertree".to_vec());
//     proofs.push(merk_three.prove(proof_query).unwrap());
//
//     let another_test_leaf_merk = TempMerk::new();
//
//     // Root Tree
//     let leaves = [merk_three.root_hash(),
// another_test_leaf_merk.root_hash()];     let root_tree =
// MerkleTree::<Sha256>::from_leaves(&leaves);     proofs.push(root_tree.proof(&
// vec![0]).to_bytes());
//
//     let (root_hash, result_map) =
//         GroveDb::execute_proof(&[TEST_LEAF, b"innertree", b"innertree-2"],
// &mut proofs).unwrap();
//
//     // Check that the root hash matches
//     assert_eq!(root_tree.root().unwrap(), root_hash);
//
//     let elem: Element =
// bincode::deserialize(result_map.get(b"key1").unwrap().unwrap()).unwrap();
//     assert_eq!(elem, Element::Item(b"value1".to_vec()));
// }

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
