use std::ops::{Deref, DerefMut};

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

    // First insertion
    let result = db.insert_if_not_exists(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree());
    match result {
        Ok(_) => (),
        Err(_e) => panic!("First insertion should be a success")
    }

    db.insert_if_not_exists(&[TEST_LEAF], b"key1".to_vec(), Element::empty_tree());
    match result {
        Ok(_) => panic!("Second insertion for same key should not work"),
        Err(_e) => (),
    }
}
