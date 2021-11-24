use tempdir::TempDir;

use super::*;

#[test]
fn test_init() {
    let tmp_dir = TempDir::new("db").unwrap();
    GroveDb::open(tmp_dir).expect("empty tree is ok");
}

#[test]
fn test_insert_value_to_merk() {
    let tmp_dir = TempDir::new("db").unwrap();
    let mut db = GroveDb::open(tmp_dir).unwrap();
    let element = Element::Item(b"ayy".to_vec());
    db.insert(&[COMMON_TREE_KEY], b"key".to_vec(), element.clone())
        .expect("successful insert");
    assert_eq!(
        db.get(&[COMMON_TREE_KEY], b"key").expect("succesful get"),
        element
    );
}

#[test]
fn test_insert_value_to_subtree() {
    let tmp_dir = TempDir::new("db").unwrap();
    let mut db = GroveDb::open(tmp_dir).unwrap();
    let element = Element::Item(b"ayy".to_vec());

    // Insert a subtree first
    db.insert(&[COMMON_TREE_KEY], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree insert");
    // Insert an element into subtree
    db.insert(
        &[COMMON_TREE_KEY, b"key1"],
        b"key2".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    assert_eq!(
        db.get(&[COMMON_TREE_KEY, b"key1"], b"key2")
            .expect("succesful get"),
        element
    );
}

#[test]
fn test_changes_propagated() {
    let tmp_dir = TempDir::new("db").unwrap();
    let mut db = GroveDb::open(tmp_dir).unwrap();
    let old_hash = db.root_tree.root();
    let element = Element::Item(b"ayy".to_vec());

    // Insert some nested subtrees
    db.insert(&[COMMON_TREE_KEY], b"key1".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(
        &[COMMON_TREE_KEY, b"key1"],
        b"key2".to_vec(),
        Element::empty_tree(),
    )
    .expect("successful subtree 2 insert");
    // Insert an element into subtree
    db.insert(
        &[COMMON_TREE_KEY, b"key1", b"key2"],
        b"key3".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    assert_eq!(
        db.get(&[COMMON_TREE_KEY, b"key1", b"key2"], b"key3")
            .expect("succesful get"),
        element
    );
    assert_ne!(old_hash, db.root_tree.root());
}

#[test]
fn test_follow_references() {
    let tmp_dir = TempDir::new("db").unwrap();
    let mut db = GroveDb::open(tmp_dir).unwrap();
    let element = Element::Item(b"ayy".to_vec());

    // Insert a reference
    db.insert(
        &[COMMON_TREE_KEY],
        b"reference_key".to_vec(),
        Element::Reference(vec![
            COMMON_TREE_KEY.to_vec(),
            b"key2".to_vec(),
            b"key3".to_vec(),
        ]),
    )
    .expect("successful reference insert");

    // Insert an item to refer to
    db.insert(&[COMMON_TREE_KEY], b"key2".to_vec(), Element::empty_tree())
        .expect("successful subtree 1 insert");
    db.insert(
        &[COMMON_TREE_KEY, b"key2"],
        b"key3".to_vec(),
        element.clone(),
    )
    .expect("successful value insert");
    assert_eq!(
        db.get(&[COMMON_TREE_KEY], b"reference_key")
            .expect("succesful get"),
        element
    );
}

#[test]
fn test_cyclic_references() {
    let tmp_dir = TempDir::new("db").unwrap();
    let mut db = GroveDb::open(tmp_dir).unwrap();

    db.insert(
        &[COMMON_TREE_KEY],
        b"reference_key_1".to_vec(),
        Element::Reference(vec![COMMON_TREE_KEY.to_vec(), b"reference_key_2".to_vec()]),
    )
    .expect("successful reference 1 insert");

    db.insert(
        &[COMMON_TREE_KEY],
        b"reference_key_2".to_vec(),
        Element::Reference(vec![COMMON_TREE_KEY.to_vec(), b"reference_key_1".to_vec()]),
    )
    .expect("successful reference 2 insert");

    assert!(matches!(
        db.get(&[COMMON_TREE_KEY], b"reference_key_1").unwrap_err(),
        Error::CyclicReference
    ));
}

#[test]
fn test_too_many_indirections() {
    let tmp_dir = TempDir::new("db").unwrap();
    let mut db = GroveDb::open(tmp_dir).unwrap();

    let keygen = |idx| format!("key{}", idx).bytes().collect::<Vec<u8>>();

    db.insert(
        &[COMMON_TREE_KEY],
        b"key0".to_vec(),
        Element::Item(b"oops".to_vec()),
    )
    .expect("successful item insert");

    for i in 1..=(MAX_REFERENCE_HOPS + 1) {
        db.insert(
            &[COMMON_TREE_KEY],
            keygen(i),
            Element::Reference(vec![COMMON_TREE_KEY.to_vec(), keygen(i - 1)]),
        )
        .expect("successful reference insert");
    }

    assert!(matches!(
        db.get(&[COMMON_TREE_KEY], &keygen(MAX_REFERENCE_HOPS + 1))
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

        // Insert some nested subtrees
        db.insert(&[COMMON_TREE_KEY], b"key1".to_vec(), Element::empty_tree())
            .expect("successful subtree 1 insert");
        db.insert(
            &[COMMON_TREE_KEY, b"key1"],
            b"key2".to_vec(),
            Element::empty_tree(),
        )
        .expect("successful subtree 2 insert");
        // Insert an element into subtree
        db.insert(
            &[COMMON_TREE_KEY, b"key1", b"key2"],
            b"key3".to_vec(),
            element.clone(),
        )
        .expect("successful value insert");
        assert_eq!(
            db.get(&[COMMON_TREE_KEY, b"key1", b"key2"], b"key3")
                .expect("succesful get 1"),
            element
        );
    }
    // Open a persisted GroveDB
    let db = GroveDb::open(tmp_dir).unwrap();
    assert_eq!(
        db.get(&[COMMON_TREE_KEY, b"key1", b"key2"], b"key3")
            .expect("succesful get 2"),
        element
    );
    assert!(db
        .get(&[COMMON_TREE_KEY, b"key1", b"key2"], b"key4")
        .is_err());
}
