//! Producer/consumer coverage for shallow V1 proofs with many child layers.
#![cfg(any(feature = "minimal", feature = "verify"))]

use std::collections::BTreeMap;

use grovedb::operations::proof::{
    GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof, ProofBytes,
    ProveOptions, MAX_PROOF_DEPTH,
};

fn assert_all_decoders_round_trip(bytes: &[u8]) {
    assert_all_decoders_round_trip_with_limit::<{ 256 * 1024 * 1024 }>(bytes);
}

fn assert_all_decoders_round_trip_with_limit<const LIMIT: usize>(bytes: &[u8]) {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_limit::<LIMIT>();
    let decoded: [(GroveDBProof, usize); 4] = [
        bincode::decode_from_slice(bytes, config).expect("ordinary decode"),
        bincode::borrow_decode_from_slice(bytes, config).expect("borrowed decode"),
        bincode::decode_from_slice_untrusted(bytes, config).expect("untrusted decode"),
        bincode::borrow_decode_from_slice_untrusted(bytes, config)
            .expect("borrowed untrusted decode"),
    ];
    for (proof, consumed) in decoded {
        assert_eq!(consumed, bytes.len());
        assert_eq!(bincode::encode_to_vec(proof, config).unwrap(), bytes);
    }
}

fn empty_layer() -> LayerProof {
    LayerProof {
        merk_proof: ProofBytes::Merk(Vec::new()),
        lower_layers: BTreeMap::new(),
    }
}

fn encode_layer(layer: LayerProof) -> Vec<u8> {
    bincode::encode_to_vec(
        GroveDBProof::V1(GroveDBProofV1 { root_layer: layer }),
        bincode::config::standard().with_big_endian(),
    )
    .unwrap()
}

fn assert_decode_error_with_limit<const LIMIT: usize>(bytes: &[u8], message: &str) {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_limit::<LIMIT>();
    let results = [
        bincode::decode_from_slice::<GroveDBProof, _>(bytes, config),
        bincode::borrow_decode_from_slice::<GroveDBProof, _>(bytes, config),
        bincode::decode_from_slice_untrusted::<GroveDBProof, _>(bytes, config),
        bincode::borrow_decode_from_slice_untrusted::<GroveDBProof, _>(bytes, config),
    ];
    for result in results {
        let error = result.err().expect("decode must reject the proof");
        assert!(error.to_string().contains(message), "{error}");
    }
}

#[test]
fn v1_child_limit_is_independent_of_depth() {
    let mut root = empty_layer();
    for key in 0..u16::MAX {
        root.lower_layers
            .insert(key.to_be_bytes().to_vec(), empty_layer());
    }
    let bytes = encode_layer(root);
    assert_all_decoders_round_trip(&bytes);

    // Reject an excessive advertised count before trying to decode any child.
    let config = bincode::config::standard().with_big_endian();
    for count in [u16::MAX as u64 + 1, u64::MAX] {
        let mut bytes = vec![1, 0, 0]; // V1 envelope, Merk variant, empty bytes
        bytes.extend(bincode::encode_to_vec(count, config).unwrap());
        assert_decode_error_with_limit::<{ 256 * 1024 * 1024 }>(&bytes, "too many children");
    }
}

#[test]
fn v1_decode_budget_counts_retained_nodes_across_child_maps() {
    let mut small = empty_layer();
    small.lower_layers.insert(vec![0], empty_layer());
    assert_all_decoders_round_trip_with_limit::<4096>(&encode_layer(small));

    let mut root = empty_layer();
    for group in 0..8 {
        let mut child = empty_layer();
        for key in 0..8 {
            child.lower_layers.insert(vec![key], empty_layer());
        }
        root.lower_layers.insert(vec![group], child);
    }
    let bytes = encode_layer(root);
    assert!(bytes.len() < 4096);
    assert_all_decoders_round_trip(&bytes);
    assert_decode_error_with_limit::<4096>(&bytes, "LimitExceeded");
}

#[test]
fn v1_decode_budget_counts_duplicate_encoded_children() {
    let config = bincode::config::standard().with_big_endian();
    let mut bytes = vec![1, 0, 0, 128]; // V1, empty Merk proof, 128 children
    for _ in 0..128 {
        bytes.extend(bincode::encode_to_vec(vec![0u8], config).unwrap());
        bytes.extend(bincode::encode_to_vec(empty_layer(), config).unwrap());
    }
    assert!(bytes.len() < 4096);
    assert_decode_error_with_limit::<4096>(&bytes, "LimitExceeded");
}

#[test]
fn v0_keeps_its_historical_child_limit_and_bytes() {
    for width in [MAX_PROOF_DEPTH, MAX_PROOF_DEPTH + 1] {
        let root_layer = MerkOnlyLayerProof {
            merk_proof: Vec::new(),
            lower_layers: (0..width)
                .map(|key| {
                    (
                        (key as u16).to_be_bytes().to_vec(),
                        MerkOnlyLayerProof {
                            merk_proof: Vec::new(),
                            lower_layers: BTreeMap::new(),
                        },
                    )
                })
                .collect(),
        };
        let bytes = bincode::encode_to_vec(
            GroveDBProof::V0(GroveDBProofV0 {
                root_layer,
                prove_options: ProveOptions::default(),
            }),
            bincode::config::standard().with_big_endian(),
        )
        .unwrap();
        if width == MAX_PROOF_DEPTH {
            assert_all_decoders_round_trip(&bytes);
        } else {
            assert_decode_error_with_limit::<{ 256 * 1024 * 1024 }>(&bytes, "too many children");
        }
    }
}

#[test]
fn v1_depth_limit_is_unchanged_in_all_decode_modes() {
    for depth in [MAX_PROOF_DEPTH, MAX_PROOF_DEPTH + 1] {
        let mut root = empty_layer();
        for _ in 0..depth {
            root = LayerProof {
                merk_proof: ProofBytes::Merk(Vec::new()),
                lower_layers: BTreeMap::from([(vec![0], root)]),
            };
        }
        let bytes = encode_layer(root);
        if depth == MAX_PROOF_DEPTH {
            assert_all_decoders_round_trip(&bytes);
        } else {
            assert_decode_error_with_limit::<{ 256 * 1024 * 1024 }>(&bytes, "nesting depth");
        }
    }
}

#[cfg(feature = "minimal")]
mod generation {
    use grovedb::{
        operations::proof::MAX_PROOF_CHILDREN, Element, GroveDb, PathQuery, Query, SizedQuery,
    };
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use super::*;

    fn insert(db: &GroveDb, path: &[Vec<u8>], key: &[u8], element: Element) {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        db.insert(
            path.as_slice(),
            key,
            element,
            None,
            None,
            GroveVersion::latest(),
        )
        .unwrap()
        .unwrap();
    }

    fn check_composite_count_proof(compound: bool, count: u16) {
        let version = GroveVersion::latest();
        let directory = tempfile::TempDir::new().unwrap();
        let db = GroveDb::open(directory.path()).unwrap();
        let index_path = vec![
            vec![1],
            vec![42; 32],
            vec![1],
            b"widget".to_vec(),
            b"ownerId".to_vec(),
        ];
        for depth in 0..index_path.len() {
            insert(
                &db,
                &index_path[..depth],
                &index_path[depth],
                Element::empty_tree(),
            );
        }
        for owner in 0..count {
            let owner = owner.to_be_bytes().to_vec();
            if compound {
                insert(&db, &index_path, &owner, Element::empty_tree());
                let mut owner_path = index_path.clone();
                owner_path.push(owner);
                insert(&db, &owner_path, b"status", Element::empty_tree());
                owner_path.push(b"status".to_vec());
                insert(&db, &owner_path, b"published", Element::empty_count_tree());
                owner_path.push(b"published".to_vec());
                insert(&db, &owner_path, b"doc", Element::new_item(vec![1]));
            } else {
                insert(&db, &index_path, &owner, Element::empty_count_tree());
                let mut owner_path = index_path.clone();
                owner_path.push(owner);
                insert(&db, &owner_path, b"doc", Element::new_item(vec![1]));
            }
        }

        // Platform's composite count queries allow disjoint bindings of up to
        // 100 values on the same index. For [ownerId, status], an IN binding on
        // ownerId followed by status == published lowers to this subquery path.
        // An IN binding on the terminal property has no child-layer descent.
        let components: Vec<PathQuery> = (0..count)
            .collect::<Vec<_>>()
            .chunks(100)
            .map(|owners| {
                let mut query = Query::new();
                for owner in owners {
                    query.insert_key(owner.to_be_bytes().to_vec());
                }
                if compound {
                    query.set_subquery_path(vec![b"status".to_vec()]);
                    query.set_subquery(Query::new_single_key(b"published".to_vec()));
                }
                PathQuery::new(index_path.clone(), SizedQuery::new(query, None, None))
            })
            .collect();
        let merged = PathQuery::merge(components.iter().collect(), version).unwrap();
        let generated = db
            .prove_query_non_serialized(&merged, None, version)
            .unwrap()
            .unwrap();
        let GroveDBProof::V1(proof) = generated else {
            panic!("current generation must use V1");
        };
        let mut layer = &proof.root_layer;
        for key in &index_path {
            layer = layer.lower_layers.get(key).expect("index path layer");
        }
        assert_eq!(
            layer.lower_layers.len(),
            if compound { count as usize } else { 0 }
        );

        let bytes = db
            .prove_query_many(components.iter().collect(), None, version)
            .unwrap()
            .unwrap();
        assert_all_decoders_round_trip(&bytes);
        if compound && count as usize > MAX_PROOF_DEPTH {
            let error = GroveDb::verify_query(&bytes, &merged, &GROVE_V3)
                .expect_err("historical verification keeps its 128-child limit");
            assert!(error.to_string().contains("too many children"));
        }
        let (root, results) = GroveDb::verify_query(&bytes, &merged, version).unwrap();
        assert_eq!(root, db.root_hash(None, version).unwrap().unwrap());
        assert_eq!(results.len(), count as usize);
        for (_, _, result) in results {
            assert_eq!(result.unwrap().count_value_or_default(), 1);
        }
    }

    #[test]
    fn compound_count_proofs_cross_the_old_breadth_boundary() {
        for count in [128, 129, 200] {
            check_composite_count_proof(true, count);
        }
    }

    #[test]
    fn terminal_count_results_do_not_consume_child_layers() {
        check_composite_count_proof(false, 200);
    }

    /// One parent tree with `width` non-empty child subtrees, built in a
    /// single batch. Empty children are proven inline by their parent row
    /// and emit no child layer, so each child carries one item.
    fn wide_parent(db: &GroveDb, width: usize) -> Vec<Vec<u8>> {
        use grovedb::batch::QualifiedGroveDbOp;

        let parent = vec![b"wide".to_vec()];
        insert(db, &[], b"wide", Element::empty_tree());
        let ops = (0..width)
            .flat_map(|key| {
                let key = (key as u32).to_be_bytes().to_vec();
                let mut child = parent.clone();
                child.push(key.clone());
                [
                    QualifiedGroveDbOp::insert_or_replace_op(
                        parent.clone(),
                        key,
                        Element::empty_tree(),
                    ),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        child,
                        b"doc".to_vec(),
                        Element::new_item(vec![1]),
                    ),
                ]
            })
            .collect();
        db.apply_batch(ops, None, None, GroveVersion::latest())
            .unwrap()
            .unwrap();
        parent
    }

    #[test]
    fn generation_refuses_a_layer_wider_than_the_child_cap_under_v4_only() {
        let version = GroveVersion::latest();
        let directory = tempfile::TempDir::new().unwrap();
        let db = GroveDb::open(directory.path()).unwrap();
        let parent = wide_parent(&db, MAX_PROOF_CHILDREN + 1);
        let mut query = Query::new_range_full();
        query.set_subquery(Query::new_range_full());
        let query = PathQuery::new(parent, SizedQuery::new(query, None, None));

        // V4 refuses to emit a layer its own decoder would reject.
        let error = db
            .prove_query(&query, None, version)
            .unwrap()
            .expect_err("V4 generation must refuse an over-wide layer");
        assert!(error.to_string().contains("child layer limit"), "{error}");

        // GROVE_V3 keeps the shipped producer/consumer mismatch: the proof
        // is generated, and the V3 verifier rejects it at the historical cap.
        let bytes = db.prove_query(&query, None, &GROVE_V3).unwrap().unwrap();
        let error = GroveDb::verify_query(&bytes, &query, &GROVE_V3)
            .expect_err("the V3 decoder keeps its 128-child cap");
        assert!(error.to_string().contains("too many children"), "{error}");
    }
}
