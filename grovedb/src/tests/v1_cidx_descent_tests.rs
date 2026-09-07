//! Tampering tests for the V1 cidx-descent path in
//! `grovedb/src/operations/proof/verify.rs`.
//!
//! These tests build a real V1 proof against a PCIT subquery, decode
//! the proof, mutate its internals (wrong `ProofBytes` variant, too-short
//! `CountIndexedTree` bytes, tampered intermediate-layer prefix), then
//! re-encode and verify — expecting `InvalidProof` rejection.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::Query;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::{GroveDBProof, LayerProof, ProofBytes},
        query::{PathQuery, SizedQuery},
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element, Error, GroveDb, QueryItem, SubqueryBranch,
    };

    fn decode_proof(bytes: &[u8]) -> GroveDBProof {
        let config = bincode::config::standard().with_limit::<{ 64 * 1024 * 1024 }>();
        let (proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(bytes, config).expect("decode GroveDBProof");
        proof
    }

    fn build_cidx_grove(grove_version: &GroveVersion) -> crate::tests::TempGroveDb {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");
        db.insert(
            [b"parent".as_slice()].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cidx");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [b"parent".as_slice(), b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate cidx");
        }
        db
    }

    fn cidx_subquery_path_query() -> PathQuery {
        let mut inner = Query::new();
        inner.insert_all();
        PathQuery {
            path: vec![b"parent".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(b"cidx".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                    limit: None,
                },
                limit: None,
                offset: None,
            },
        }
    }

    fn encode_proof(p: &GroveDBProof) -> Vec<u8> {
        bincode::encode_to_vec(p, bincode::config::standard()).expect("encode")
    }

    fn mutate_v1<F: FnOnce(&mut LayerProof)>(proof: GroveDBProof, mutator: F) -> GroveDBProof {
        match proof {
            GroveDBProof::V1(mut v1) => {
                mutator(&mut v1.root_layer);
                GroveDBProof::V1(v1)
            }
            _ => panic!("expected V1 proof"),
        }
    }

    #[test]
    fn v1_cidx_lower_layer_must_use_count_indexed_tree_variant() {
        // Build a real V1 cidx proof, then swap the cidx lower layer's
        // ProofBytes from CountIndexedTree to Merk. The verifier must
        // reject with "V1 lower layer for CountIndexedTree element must
        // use ProofBytes::CountIndexedTree".
        let grove_version = GroveVersion::latest();
        let db = build_cidx_grove(grove_version);
        let pq = cidx_subquery_path_query();
        let proof_bytes = db
            .prove_query(&pq, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let proof = decode_proof(&proof_bytes);

        let tampered = mutate_v1(proof, |root| {
            // root_layer has lower_layers[b"parent"] which itself has
            // lower_layers[b"cidx"] holding the cidx ProofBytes.
            let parent = root
                .lower_layers
                .get_mut(b"parent".as_slice())
                .expect("parent lower layer");
            let cidx = parent
                .lower_layers
                .get_mut(b"cidx".as_slice())
                .expect("cidx lower layer");
            // Extract the inner bytes (excluding the 32-byte prefix) and
            // re-wrap as Merk to break the variant check.
            let bytes = match &cidx.merk_proof {
                ProofBytes::CountIndexedTree(b) if b.len() >= 32 => b[32..].to_vec(),
                _ => panic!("expected CountIndexedTree ProofBytes >= 32"),
            };
            cidx.merk_proof = ProofBytes::Merk(bytes);
        });
        let bytes = encode_proof(&tampered);
        let result = GroveDb::verify_query(&bytes, &pq, grove_version);
        assert!(
            matches!(result, Err(Error::InvalidProof(_, ref msg)) if msg.contains("CountIndexedTree")),
            "expected InvalidProof about CountIndexedTree variant, got {:?}",
            result
        );
    }

    #[test]
    fn v1_cidx_lower_layer_truncated_below_32_bytes_rejected() {
        // Swap CountIndexedTree bytes for a truncated version (<32B).
        // The verifier should reject with the short-prefix check.
        let grove_version = GroveVersion::latest();
        let db = build_cidx_grove(grove_version);
        let pq = cidx_subquery_path_query();
        let proof_bytes = db
            .prove_query(&pq, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let proof = decode_proof(&proof_bytes);

        let tampered = mutate_v1(proof, |root| {
            let parent = root.lower_layers.get_mut(b"parent".as_slice()).unwrap();
            let cidx = parent.lower_layers.get_mut(b"cidx".as_slice()).unwrap();
            // Truncate the CountIndexedTree bytes to under 32 to trigger
            // the "shorter than 32-byte secondary root attestation
            // prefix" rejection.
            let truncated = vec![0u8; 8];
            cidx.merk_proof = ProofBytes::CountIndexedTree(truncated);
        });
        let bytes = encode_proof(&tampered);
        let result = GroveDb::verify_query(&bytes, &pq, grove_version);
        assert!(
            matches!(result, Err(Error::InvalidProof(_, _))),
            "expected InvalidProof rejection, got {:?}",
            result
        );
    }

    #[test]
    fn v1_cidx_tampered_secondary_prefix_rejected() {
        // Flip the first byte of the 32-byte secondary attestation
        // prefix on the cidx lower layer. The combine_hash_three check
        // must fail.
        let grove_version = GroveVersion::latest();
        let db = build_cidx_grove(grove_version);
        let pq = cidx_subquery_path_query();
        let proof_bytes = db
            .prove_query(&pq, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let proof = decode_proof(&proof_bytes);

        let tampered = mutate_v1(proof, |root| {
            let parent = root.lower_layers.get_mut(b"parent".as_slice()).unwrap();
            let cidx = parent.lower_layers.get_mut(b"cidx".as_slice()).unwrap();
            if let ProofBytes::CountIndexedTree(bytes) = &mut cidx.merk_proof {
                bytes[0] ^= 0xFF;
            } else {
                panic!("expected CountIndexedTree");
            }
        });
        let bytes = encode_proof(&tampered);
        let result = GroveDb::verify_query(&bytes, &pq, grove_version);
        assert!(
            matches!(result, Err(Error::InvalidProof(_, _))),
            "expected InvalidProof for tampered prefix, got {:?}",
            result
        );
    }

    #[test]
    fn v1_proof_bytes_count_indexed_under_non_cidx_parent_rejected() {
        // Build a proof for a regular tree subquery, then swap the
        // lower-layer ProofBytes::Merk for ProofBytes::CountIndexedTree
        // under a non-cidx parent — the verifier must reject with
        // "ProofBytes::CountIndexedTree under a non-cidx ...".
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");
        db.insert(
            [b"parent".as_slice()].as_ref(),
            b"inner",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner");
        db.insert(
            [b"parent".as_slice(), b"inner"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate inner");

        let mut sub = Query::new();
        sub.insert_all();
        let pq = PathQuery {
            path: vec![b"parent".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(b"inner".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(sub.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                    limit: None,
                },
                limit: None,
                offset: None,
            },
        };

        let proof_bytes = db
            .prove_query(&pq, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let proof = decode_proof(&proof_bytes);

        let tampered = mutate_v1(proof, |root| {
            let parent = root.lower_layers.get_mut(b"parent".as_slice()).unwrap();
            let inner = parent.lower_layers.get_mut(b"inner".as_slice()).unwrap();
            // Take the existing Merk proof bytes and re-wrap as cidx
            // with a 32-byte prefix. The verifier sees that the parent
            // element is a regular Tree (not PCIT) and must reject.
            let original = match &inner.merk_proof {
                ProofBytes::Merk(b) => b.clone(),
                _ => panic!("expected Merk"),
            };
            let mut faked = vec![0u8; 32];
            faked.extend_from_slice(&original);
            inner.merk_proof = ProofBytes::CountIndexedTree(faked);
        });
        let bytes = encode_proof(&tampered);
        let result = GroveDb::verify_query(&bytes, &pq, grove_version);
        assert!(
            matches!(result, Err(Error::InvalidProof(_, ref msg)) if msg.contains("non-indexed")),
            "expected non-indexed parent rejection, got {:?}",
            result
        );
    }

    #[test]
    fn v1_proof_decode_canonical_works_for_v1() {
        // Sanity check: decode + re-encode round-trip preserves the
        // proof. This exercises the decode_grovedb_proof_canonical
        // path which the verifier relies on.
        let grove_version = GroveVersion::latest();
        let db = build_cidx_grove(grove_version);
        let pq = cidx_subquery_path_query();
        let bytes = db
            .prove_query(&pq, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let proof = decode_proof(&bytes);
        assert!(matches!(proof, GroveDBProof::V1(_)));
    }

    #[test]
    fn v1_cidx_proof_succinctness_check_succeeds_on_clean_proof() {
        // Make sure the verify_query path (with succinctness) works
        // end-to-end on a fresh cidx proof, locking the happy path.
        let grove_version = GroveVersion::latest();
        let db = build_cidx_grove(grove_version);
        let pq = cidx_subquery_path_query();
        let proof_bytes = db
            .prove_query(&pq, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let (root_hash, results) =
            GroveDb::verify_query(&proof_bytes, &pq, grove_version).expect("verify_query");
        assert_eq!(
            root_hash,
            db.root_hash(None, grove_version).unwrap().expect("root")
        );
        assert_eq!(results.len(), 3);
    }

    /// Removing the cidx lower layer entirely makes the verifier fall
    /// into the "no lower layer" path. This exercises a separate code
    /// path from the variant-rejection arms.
    #[test]
    fn v1_cidx_missing_lower_layer_rejected() {
        let grove_version = GroveVersion::latest();
        let db = build_cidx_grove(grove_version);
        let pq = cidx_subquery_path_query();
        let proof_bytes = db
            .prove_query(&pq, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let proof = decode_proof(&proof_bytes);
        let tampered = mutate_v1(proof, |root| {
            let parent = root.lower_layers.get_mut(b"parent".as_slice()).unwrap();
            parent.lower_layers.remove(b"cidx".as_slice());
        });
        let bytes = encode_proof(&tampered);
        let result = GroveDb::verify_query(&bytes, &pq, grove_version);
        assert!(
            result.is_err(),
            "expected error on missing lower layer; got {:?}",
            result
        );
    }
}
