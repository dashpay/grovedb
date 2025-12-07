use grovedb_merk::tree::value_hash;
use grovedb_version::version::GroveVersion;

use crate::{tests::make_deep_tree, Element};

#[test]
fn debug_provable_count_tree_hash() {
    let grove_version = GroveVersion::latest();
    let db = make_deep_tree(grove_version);

    // Create a ProvableCountTree
    let element = Element::new_provable_count_tree(Some(b"bob".to_vec()));
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

    // Insert some items
    db.insert(
        &[b"verified"],
        b"alice",
        Element::new_item(b"data1".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    db.insert(
        &[b"verified"],
        b"bob",
        Element::new_item(b"data2".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    db.insert(
        &[b"verified"],
        b"carol",
        Element::new_item(b"data3".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    db.insert(
        &[b"verified"],
        b"dave",
        Element::new_item(b"data4".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    // Get the tree element as stored
    let tree_element = db
        .get(&[] as &[&[u8]], b"verified", None, grove_version)
        .unwrap()
        .expect("should get element");

    println!("Tree element: {:?}", tree_element);

    // Get the serialized value
    let serialized = tree_element.serialize(grove_version).unwrap();
    println!("Serialized tree element: {}", hex::encode(&serialized));

    // Calculate value hash
    let val_hash = value_hash(&serialized).unwrap();
    println!("Value hash: {}", hex::encode(&val_hash));

    // Get the count from the element
    if let Element::ProvableCountTree(root_key, count, _) = &tree_element {
        println!("\nTree root key: {:?}", root_key.as_ref().map(hex::encode));
        println!("Tree count: {}", count);
    }

    // Now let's generate a proof and see what hash it contains
    use grovedb_merk::proofs::Query;

    use crate::{query::SizedQuery, PathQuery};

    let path_query = PathQuery::new(
        vec![],
        SizedQuery::new(Query::new_single_key(b"verified".to_vec()), None, None),
    );

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

    println!("\nProof length: {} bytes", proof.len());

    // Decode the proof to see its structure
    use crate::operations::proof::GroveDBProof;
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let decoded_proof: GroveDBProof = bincode::decode_from_slice(&proof, config)
        .expect("should decode proof")
        .0;

    if let GroveDBProof::V0(proof_v0) = decoded_proof {
        println!("\nDecoded proof structure:");
        println!(
            "Root layer merk proof length: {}",
            proof_v0.root_layer.merk_proof.len()
        );

        // Try to decode the merk proof ops
        use grovedb_merk::proofs::Decoder;
        let ops = Decoder::new(&proof_v0.root_layer.merk_proof);
        println!("\nRoot layer proof ops:");
        for (i, op) in ops.enumerate() {
            if let Ok(op) = op {
                match &op {
                    grovedb_merk::proofs::Op::Push(node) => {
                        match node {
                            grovedb_merk::proofs::Node::KVValueHash(k, v, h) => {
                                println!(
                                    "  Op {}: Push(KVValueHash(key={}, value={}, hash={}))",
                                    i,
                                    hex::encode(k),
                                    hex::encode(v),
                                    hex::encode(h)
                                );

                                // If this is the verified key, check the value
                                if k == b"verified" {
                                    println!("    Found 'verified' key in proof!");
                                    println!("    Value in proof: {}", hex::encode(v));
                                    println!("    Hash in proof: {}", hex::encode(h));

                                    // Compare with our calculated value hash
                                    let proof_val_hash = value_hash(v).unwrap();
                                    println!(
                                        "    Calculated value hash from proof value: {}",
                                        hex::encode(&proof_val_hash)
                                    );

                                    // Deserialize the value to see what element it is
                                    let proof_element =
                                        Element::deserialize(v, grove_version).unwrap();
                                    println!("    Element from proof: {:?}", proof_element);
                                }
                            }
                            _ => {
                                println!("  Op {}: {:?}", i, op);
                            }
                        }
                    }
                    _ => {
                        println!("  Op {}: {:?}", i, op);
                    }
                }
            }
        }
    }
}
