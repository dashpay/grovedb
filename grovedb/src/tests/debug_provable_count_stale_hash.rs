use grovedb_merk::proofs::Query;
use grovedb_version::version::GroveVersion;

use crate::{tests::make_deep_tree, Element, GroveDb, PathQuery};

#[test]
fn debug_provable_count_stale_hash() {
    let grove_version = GroveVersion::latest();
    let db = make_deep_tree(grove_version);

    // Create an empty ProvableCountTree
    let element = Element::empty_provable_count_tree();
    db.insert(
        &[] as &[&[u8]],
        b"verified",
        element,
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree");

    // Insert items one by one and check the parent tree element after each
    for i in 1..=4 {
        let key = format!("key{}", i);
        let value = format!("value{}", i);

        println!("\n--- Before inserting {} ---", key);

        // Get the tree element before insert
        let tree_element_before = db
            .get(&[] as &[&[u8]], b"verified", None, grove_version)
            .unwrap()
            .expect("should get element");

        if let Element::ProvableCountTree(root_key, count, _) = &tree_element_before {
            println!(
                "Tree element: root_key={:?}, count={}",
                root_key.as_ref().map(|k| hex::encode(k)),
                count
            );
        }

        // Get root hash before
        let root_hash_before = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");
        println!("Root hash before: {}", hex::encode(&root_hash_before));

        // Insert the item
        db.insert(
            &[b"verified"],
            key.as_bytes(),
            Element::new_item(value.as_bytes().to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        println!("\n--- After inserting {} ---", key);

        // Get the tree element after insert
        let tree_element_after = db
            .get(&[] as &[&[u8]], b"verified", None, grove_version)
            .unwrap()
            .expect("should get element");

        if let Element::ProvableCountTree(root_key, count, _) = &tree_element_after {
            println!(
                "Tree element: root_key={:?}, count={}",
                root_key.as_ref().map(|k| hex::encode(k)),
                count
            );
        }

        // Get root hash after
        let root_hash_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");
        println!("Root hash after: {}", hex::encode(&root_hash_after));
    }

    // Now generate a proof and see what hash it contains
    println!("\n--- Generating proof ---");

    let path_query = PathQuery::new_unsized(vec![], Query::new_single_key(b"verified".to_vec()));

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

    // Decode the proof to see its structure
    use crate::operations::proof::GroveDBProof;
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let decoded_proof: GroveDBProof = bincode::decode_from_slice(&proof, config)
        .expect("should decode proof")
        .0;

    if let GroveDBProof::V0(proof_v0) = decoded_proof {
        // Try to decode the merk proof ops
        use grovedb_merk::proofs::Decoder;
        let ops = Decoder::new(&proof_v0.root_layer.merk_proof);
        for op in ops {
            if let Ok(op) = op {
                match &op {
                    grovedb_merk::proofs::Op::Push(node) => {
                        match node {
                            grovedb_merk::proofs::Node::KVValueHash(k, v, h) => {
                                if k == b"verified" {
                                    println!("\nFound 'verified' in proof:");
                                    println!("  Value: {}", hex::encode(v));
                                    println!("  Hash: {}", hex::encode(h));

                                    // Deserialize to see the element
                                    let elem = Element::deserialize(v, grove_version).unwrap();
                                    println!("  Element: {:?}", elem);
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
