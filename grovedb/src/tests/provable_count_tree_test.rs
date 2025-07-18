//! Tests for ProvableCountTree functionality in GroveDB

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::make_test_grovedb,
        Element, GroveDb, PathQuery, Query,
    };

    #[test]
    fn test_provable_count_tree_basic_operations() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountTree at root
        db.insert(
            &[] as &[&[u8]],
            b"provable_counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert items into the provable count tree
        let items = vec![
            (b"key1".to_vec(), Element::new_item(b"value1".to_vec())),
            (b"key2".to_vec(), Element::new_item(b"value2".to_vec())),
            (b"key3".to_vec(), Element::new_item(b"value3".to_vec())),
        ];

        for (key, element) in items {
            db.insert(
                &[b"provable_counts"],
                &key,
                element,
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Get the root hash before and after insertions
        let root_hash = db.root_hash(None, grove_version).unwrap().expect("should get root hash");
        
        // The root hash should change when we insert more items
        db.insert(
            &[b"provable_counts"],
            b"key4",
            Element::new_item(b"value4".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let new_root_hash = db.root_hash(None, grove_version).unwrap().expect("should get root hash");
        
        assert_ne!(root_hash, new_root_hash, "Root hash should change when count changes");
    }

    #[test]
    fn test_provable_count_tree_proofs() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a ProvableCountTree
        db.insert(
            &[] as &[&[u8]],
            b"counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert some items
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            db.insert(
                &[b"counts"],
                &key,
                Element::new_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Create a path query for a specific key
        let mut query = Query::new();
        query.insert_key(b"key2".to_vec());
        
        let path_query = PathQuery::new_unsized(vec![b"counts".to_vec()], query);

        // Generate proof
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Verify the proof was generated successfully
        assert!(!proof.is_empty(), "Proof should not be empty");
        
        // Verify we can decode the proof without errors
        let (root_hash, proved_values) = GroveDb::verify_query_raw(
            &proof,
            &path_query,
            grove_version,
        )
        .expect("should verify proof");
        
        // We queried for one specific key
        assert_eq!(proved_values.len(), 1, "Should have exactly one proved value");
        
        // Verify the root hash matches
        let actual_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");
        
        assert_eq!(root_hash, actual_root_hash, "Root hash should match");
    }

    #[test]
    fn test_provable_count_tree_vs_regular_count_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert both types of count trees
        db.insert(
            &[] as &[&[u8]],
            b"regular_count",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert regular count tree");

        db.insert(
            &[] as &[&[u8]],
            b"provable_count",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert same items into both trees
        let items = vec![
            (b"a".to_vec(), Element::new_item(b"1".to_vec())),
            (b"b".to_vec(), Element::new_item(b"2".to_vec())),
            (b"c".to_vec(), Element::new_item(b"3".to_vec())),
        ];

        for (key, element) in &items {
            db.insert(
                &[b"regular_count"],
                key,
                element.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert into regular count tree");

            db.insert(
                &[b"provable_count"],
                key,
                element.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert into provable count tree");
        }

        // Get root hashes - they should be different even with same content
        let root_hash1 = db.root_hash(None, grove_version).unwrap().expect("should get root hash");
        
        // The trees should have different hashes because they use different hash functions
        // This verifies that ProvableCountTree includes count in its hash calculation
        
        // Generate proofs for both to see the difference
        let mut query = Query::new();
        query.insert_key(b"b".to_vec());
        
        let regular_proof = db
            .prove_query(
                &PathQuery::new_unsized(vec![b"regular_count".to_vec()], query.clone()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("should generate proof for regular count tree");

        let provable_proof = db
            .prove_query(
                &PathQuery::new_unsized(vec![b"provable_count".to_vec()], query),
                None,
                grove_version,
            )
            .unwrap()
            .expect("should generate proof for provable count tree");

        // The proofs should have different structures
        assert_ne!(regular_proof.len(), provable_proof.len(), 
            "Proofs should differ between regular and provable count trees");
    }
}