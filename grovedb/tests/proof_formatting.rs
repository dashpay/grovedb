//! Regression coverage for formatting malformed inner Merk proofs.

#![cfg(any(feature = "minimal", feature = "verify"))]

use std::collections::BTreeMap;

use grovedb::operations::proof::{
    GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof, ProofBytes,
    ProveOptions,
};
use grovedb_merk::proofs::{Decoder, Node, Op};

// Check progress before invoking Display so reverting the decoder fix fails
// promptly instead of letting a regression test build an unbounded String.
fn assert_error_is_terminal(bytes: &[u8]) {
    let mut decoder = Decoder::new(bytes);
    for _ in 0..=bytes.len() {
        match decoder.next() {
            Some(Ok(_)) => {}
            Some(Err(_)) => {
                assert!(decoder.next().is_none());
                assert!(decoder.remaining_bytes() > 0);
                return;
            }
            None => panic!("expected malformed proof bytes"),
        }
    }
    panic!("decoder did not reach the malformed operation");
}

fn assert_malformed_display(proof: GroveDBProof) {
    // Exercise the outer-envelope decode followed by Display, as diagnostic
    // callers do. Formatting must not change the serialized proof.
    let config = bincode::config::standard().with_big_endian();
    let encoded = bincode::encode_to_vec(&proof, config).unwrap();
    let (decoded, consumed): (GroveDBProof, _) =
        bincode::decode_from_slice(&encoded, config).unwrap();
    assert_eq!(consumed, encoded.len());
    let display = decoded.to_string();
    assert_eq!(display.matches("Error decoding op:").count(), 1);
    assert!(display.contains("0: Parent"));
    assert!(display.contains("1: Error decoding op:"));
    assert!(!display.contains("2:"));
    assert!(display.len() < 1024, "malformed diagnostic must terminate");
    assert_eq!(bincode::encode_to_vec(&decoded, config).unwrap(), encoded);
}

#[test]
fn malformed_inner_proofs_display_once_in_all_merk_envelopes() {
    for invalid in [
        vec![0x10, 0xFF, 0x10], // Parent, unknown opcode, then valid Parent.
        vec![0x10, 0x01, 0x02], // Parent, then truncated Hash.
    ] {
        assert_error_is_terminal(&invalid);
        for nested in [false, true] {
            let mut root_layer = MerkOnlyLayerProof {
                merk_proof: invalid.clone(),
                lower_layers: BTreeMap::new(),
            };
            if nested {
                root_layer = MerkOnlyLayerProof {
                    merk_proof: vec![],
                    lower_layers: BTreeMap::from([(b"child".to_vec(), root_layer)]),
                };
            }
            assert_malformed_display(GroveDBProof::V0(GroveDBProofV0 {
                root_layer,
                prove_options: ProveOptions::default(),
            }));

            let mut indexed = vec![0; 32];
            indexed.extend_from_slice(&invalid);
            for merk_proof in [
                ProofBytes::Merk(invalid.clone()),
                ProofBytes::CountIndexedTree(indexed),
            ] {
                let mut root_layer = LayerProof {
                    merk_proof,
                    lower_layers: BTreeMap::new(),
                };
                if nested {
                    root_layer = LayerProof {
                        merk_proof: ProofBytes::Merk(vec![]),
                        lower_layers: BTreeMap::from([(b"child".to_vec(), root_layer)]),
                    };
                }
                assert_malformed_display(GroveDBProof::V1(GroveDBProofV1 { root_layer }));
            }
        }
    }
}

#[test]
fn valid_inner_proof_display_preserves_operations() {
    assert_eq!(ProofBytes::Merk(vec![]).to_string(), "Merk()");
    let ops = [
        Op::Push(Node::Hash([0xAB; 32])),
        Op::Parent,
        Op::ChildInverted,
    ];
    let mut encoded = vec![];
    grovedb_query::proofs::encode_into(ops.iter(), &mut encoded);
    assert_eq!(
        ProofBytes::Merk(encoded).to_string(),
        format!(
            "Merk(\n    0: Push(Hash(HASH[{}]))\n    1: Parent\n    2: ChildInverted)",
            "ab".repeat(32)
        )
    );
}

#[test]
fn invalid_element_display_still_returns_a_format_error() {
    use std::fmt::Write;

    let ops = [Op::Push(Node::KV(b"key".to_vec(), vec![0xFF]))];
    let mut encoded = vec![];
    grovedb_query::proofs::encode_into(ops.iter(), &mut encoded);
    assert!(write!(&mut String::new(), "{}", ProofBytes::Merk(encoded)).is_err());
}

#[cfg(feature = "minimal")]
#[test]
fn formatting_preserves_generated_proof_bytes_roots_and_results() {
    use grovedb::{Element, GroveDb, PathQuery, Query};
    use grovedb_version::version::{v2::GROVE_V2, GroveVersion};

    for grove_version in [&GROVE_V2, GroveVersion::latest()] {
        let directory = tempfile::tempdir().unwrap();
        let db = GroveDb::open(directory.path()).unwrap();
        db.insert(
            &[] as &[&[u8]],
            b"key",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        let mut query = Query::new();
        query.insert_key(b"key".to_vec());
        let query = PathQuery::new_unsized(vec![], query);
        let encoded = db
            .prove_query(&query, None, grove_version)
            .unwrap()
            .unwrap();
        let verified = GroveDb::verify_query_raw(&encoded, &query, grove_version).unwrap();
        assert_eq!(
            verified.0,
            db.root_hash(None, grove_version).unwrap().unwrap()
        );
        assert_eq!(verified.1.len(), 1);

        let config = bincode::config::standard().with_big_endian();
        let (proof, consumed): (GroveDBProof, _) =
            bincode::decode_from_slice(&encoded, config).unwrap();
        assert_eq!(consumed, encoded.len());
        let display = proof.to_string();
        assert!(display.contains("key"));
        assert!(display.contains("value"));
        assert!(!display.contains("Error"));
        let after_display = bincode::encode_to_vec(&proof, config).unwrap();
        assert_eq!(after_display, encoded);
        assert_eq!(
            GroveDb::verify_query_raw(&after_display, &query, grove_version).unwrap(),
            verified
        );
    }
}
