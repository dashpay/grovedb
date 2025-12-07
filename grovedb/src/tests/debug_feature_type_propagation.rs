//! Debug test for feature type propagation in ProvableCountTree

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{Decoder, Op};
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element, PathQuery, Query};

    #[test]
    fn debug_feature_type_propagation() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountTree
        db.insert(
            &[] as &[&[u8]],
            b"provable_tree",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert an item to ensure tree isn't empty
        db.insert(
            &[b"provable_tree"],
            b"test_key",
            Element::new_item(b"test_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Open the merk directly to check tree type
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(
                vec![b"provable_tree".as_slice()].as_slice().into(),
                &tx,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should open merk");

        eprintln!("Merk tree type: {:?}", merk.tree_type);

        // We can't directly access the root tree feature type from here,
        // but we'll see it in the proof nodes

        // Generate a proof and check what nodes are generated
        let mut query = Query::new();
        query.insert_key(b"test_key".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"provable_tree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Decode and analyze the proof
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let grovedb_proof: crate::operations::proof::GroveDBProof =
            bincode::decode_from_slice(&proof, config)
                .expect("should deserialize proof")
                .0;

        if let crate::operations::proof::GroveDBProof::V0(proof_v0) = &grovedb_proof {
            eprintln!("Root layer proof nodes:");
            let decoder = Decoder::new(proof_v0.root_layer.merk_proof.as_slice());
            for op in decoder {
                if let Ok(op) = op {
                    match op {
                        Op::Push(node) | Op::PushInverted(node) => {
                            eprintln!("  Root layer node: {:?}", node);
                        }
                        _ => {}
                    }
                }
            }

            if let Some(lower_layer) = proof_v0
                .root_layer
                .lower_layers
                .get(b"provable_tree".as_slice())
            {
                eprintln!("\nProvable tree layer proof nodes:");
                let decoder = Decoder::new(lower_layer.merk_proof.as_slice());
                for op in decoder {
                    if let Ok(op) = op {
                        match op {
                            Op::Push(node) | Op::PushInverted(node) => {
                                eprintln!("  Provable tree layer node: {:?}", node);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}
