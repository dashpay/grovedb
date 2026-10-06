//! Aggregate-on-range proofs whose path runs through an EMPTY aggregate tree
//! above the aggregate terminal.
//!
//! An aggregate-on-range query descends into an empty `ProvableCountTree`,
//! `ProvableCountSumTree`, `ProvableSumTree` or `ProvableCountProvableSumTree`
//! it routes through, so that an empty terminal answers with the
//! authenticated zero of its aggregate short-circuit. Above the terminal that
//! descent reaches an ordinary layer over an empty Merk, which no Merk proof
//! can be generated for, and generation failed with "Cannot create proof for
//! empty tree". Such a tree is proved in its parent layer with no lower layer
//! instead, exactly like an empty tree of any other type there, and the
//! single-key query over the same path verifies the proof, at every grove
//! version that proves aggregate queries (`GROVE_V3` and later).
//!
//! The shape is a document index nobody has written to yet:
//! `widget / brand / <value> / color / <value>`, with `brand` an empty
//! aggregate tree, queried for the colors after "blue" of brand "acme" (leaf
//! shape) or of brands "acme" and "zeta" (carrier shape).

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{encoding::encode_into, query::QueryItem, Decoder, Node, Op};
    use grovedb_query::Query;
    use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, GroveVersion};

    use crate::{
        operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof, ProofBytes},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, SizedQuery,
    };

    const WIDGET: &[u8] = b"widget";
    const BRAND: &[u8] = b"brand";
    const ACME: &[u8] = b"acme";
    const ZETA: &[u8] = b"zeta";
    const COLOR: &[u8] = b"color";

    type CryptoHash = [u8; 32];

    #[derive(Clone, Copy, Debug)]
    enum Aggregate {
        Count,
        Sum,
        CountAndSum,
    }

    /// Every empty tree type an aggregate of each kind descends into, paired
    /// with that kind.
    const EMPTY_AGGREGATE_TREES: [(Aggregate, fn() -> Element); 6] = [
        (Aggregate::Count, Element::empty_provable_count_tree),
        (Aggregate::Count, Element::empty_provable_count_sum_tree),
        (
            Aggregate::Count,
            Element::empty_provable_count_provable_sum_tree,
        ),
        (Aggregate::Sum, Element::empty_provable_sum_tree),
        (
            Aggregate::Sum,
            Element::empty_provable_count_provable_sum_tree,
        ),
        (
            Aggregate::CountAndSum,
            Element::empty_provable_count_provable_sum_tree,
        ),
    ];

    impl Aggregate {
        fn inner_range() -> QueryItem {
            QueryItem::RangeAfter(b"blue".to_vec()..)
        }

        fn subquery(self) -> Query {
            match self {
                Aggregate::Count => Query::new_aggregate_count_on_range(Self::inner_range()),
                Aggregate::Sum => Query::new_aggregate_sum_on_range(Self::inner_range()),
                Aggregate::CountAndSum => {
                    Query::new_aggregate_count_and_sum_on_range(Self::inner_range())
                }
            }
        }

        /// The terminal the aggregate is answered against, populated with
        /// "blue", "green" and "red".
        fn terminal(self) -> Element {
            match self {
                Aggregate::Count => Element::empty_provable_count_tree(),
                Aggregate::Sum => Element::empty_provable_sum_tree(),
                Aggregate::CountAndSum => Element::empty_provable_count_provable_sum_tree(),
            }
        }

        fn terminal_row(self, value: i64) -> Element {
            match self {
                Aggregate::Count => Element::new_item(vec![value as u8]),
                Aggregate::Sum | Aggregate::CountAndSum => Element::new_sum_item(value),
            }
        }

        /// `(count, sum)` after "blue" over a terminal holding blue = 1,
        /// green = 2, red = 3; the axis the aggregate does not read is 0.
        fn populated_answer(self) -> (u64, i64) {
            match self {
                Aggregate::Count => (2, 0),
                Aggregate::Sum => (0, 5),
                Aggregate::CountAndSum => (2, 5),
            }
        }

        /// The leaf-shape verifier for this kind, as `(root, count, sum)`.
        fn verify_leaf(
            self,
            proof: &[u8],
            path_query: &PathQuery,
            grove_version: &GroveVersion,
        ) -> Result<(CryptoHash, u64, i64), Error> {
            match self {
                Aggregate::Count => {
                    GroveDb::verify_aggregate_count_query(proof, path_query, grove_version)
                        .map(|(root, count)| (root, count, 0))
                }
                Aggregate::Sum => {
                    GroveDb::verify_aggregate_sum_query(proof, path_query, grove_version)
                        .map(|(root, sum)| (root, 0, sum))
                }
                Aggregate::CountAndSum => {
                    GroveDb::verify_aggregate_count_and_sum_query(proof, path_query, grove_version)
                }
            }
        }

        /// The per-key verifier for this kind, as `(root, [(key, count, sum)])`.
        #[allow(clippy::type_complexity)]
        fn verify_per_key(
            self,
            proof: &[u8],
            path_query: &PathQuery,
            grove_version: &GroveVersion,
        ) -> Result<(CryptoHash, Vec<(Vec<u8>, u64, i64)>), Error> {
            match self {
                Aggregate::Count => {
                    GroveDb::verify_aggregate_count_query_per_key(proof, path_query, grove_version)
                        .map(|(root, rows)| {
                            (root, rows.into_iter().map(|(k, c)| (k, c, 0)).collect())
                        })
                }
                Aggregate::Sum => {
                    GroveDb::verify_aggregate_sum_query_per_key(proof, path_query, grove_version)
                        .map(|(root, rows)| {
                            (root, rows.into_iter().map(|(k, s)| (k, 0, s)).collect())
                        })
                }
                Aggregate::CountAndSum => GroveDb::verify_aggregate_count_and_sum_query_per_key(
                    proof,
                    path_query,
                    grove_version,
                ),
            }
        }
    }

    fn insert(db: &TempGroveDb, path: &[&[u8]], key: &[u8], element: Element, v: &GroveVersion) {
        db.insert(path, key, element, None, None, v)
            .unwrap()
            .expect("insert");
    }

    fn root_hash(db: &TempGroveDb, v: &GroveVersion) -> CryptoHash {
        db.root_hash(None, v).unwrap().expect("root hash")
    }

    /// `TEST_LEAF / widget / brand`, with `brand` the given (empty) tree.
    fn with_empty_brand(brand: Element, v: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], WIDGET, Element::empty_tree(), v);
        insert(&db, &[TEST_LEAF, WIDGET], BRAND, brand, v);
        db
    }

    /// Insert `<parent> / color` as the populated terminal of `aggregate`.
    fn insert_populated_color(
        db: &TempGroveDb,
        parent: &[&[u8]],
        aggregate: Aggregate,
        v: &GroveVersion,
    ) {
        insert(db, parent, COLOR, aggregate.terminal(), v);
        let mut color_path = parent.to_vec();
        color_path.push(COLOR);
        for (key, value) in [(&b"blue"[..], 1), (&b"green"[..], 2), (&b"red"[..], 3)] {
            insert(db, &color_path, key, aggregate.terminal_row(value), v);
        }
    }

    /// The leaf shape: the colors after "blue" of brand "acme".
    fn leaf_path_query(aggregate: Aggregate) -> PathQuery {
        PathQuery::new_unsized(
            vec![
                TEST_LEAF.to_vec(),
                WIDGET.to_vec(),
                BRAND.to_vec(),
                ACME.to_vec(),
                COLOR.to_vec(),
            ],
            aggregate.subquery(),
        )
    }

    /// The carrier shape: the colors after "blue" of brands "acme" and
    /// "zeta", through the `color` subquery path.
    fn carrier_path_query(aggregate: Aggregate) -> PathQuery {
        let mut carrier = Query::new();
        carrier.insert_key(ACME.to_vec());
        carrier.insert_key(ZETA.to_vec());
        carrier.set_subquery_path(vec![COLOR.to_vec()]);
        carrier.set_subquery(aggregate.subquery());
        PathQuery::new(
            vec![TEST_LEAF.to_vec(), WIDGET.to_vec(), BRAND.to_vec()],
            SizedQuery::new(carrier, None, None),
        )
    }

    /// The plain single-key query over the same path, which a caller can
    /// verify such a proof with when an aggregate verifier refuses it.
    fn single_key_path_query() -> PathQuery {
        PathQuery::new_single_key(
            vec![
                TEST_LEAF.to_vec(),
                WIDGET.to_vec(),
                BRAND.to_vec(),
                ACME.to_vec(),
            ],
            COLOR.to_vec(),
        )
    }

    fn assert_refused_for_a_missing_lower_layer(error: &Error, key: &[u8], context: &str) {
        assert!(
            matches!(error, Error::InvalidProof(..)),
            "{context}: expected InvalidProof, got: {error:?}"
        );
        let message = error.to_string();
        assert!(
            (message.contains("lower layer") || message.contains("lower-layer"))
                && message.contains(&hex::encode(key)),
            "{context}: expected a missing lower-layer refusal at key {}, got: {message}",
            hex::encode(key)
        );
    }

    fn decode_envelope(proof: &[u8]) -> GroveDBProof {
        bincode::decode_from_slice(
            proof,
            bincode::config::standard()
                .with_big_endian()
                .with_limit::<{ 256 * 1024 * 1024 }>(),
        )
        .expect("decode envelope")
        .0
    }

    fn reencode_envelope(decoded: GroveDBProof) -> Vec<u8> {
        bincode::encode_to_vec(
            decoded,
            bincode::config::standard()
                .with_big_endian()
                .with_no_limit(),
        )
        .expect("re-encode envelope")
    }

    /// The `TEST_LEAF / widget` layer of a V1 proof: the layer that proves
    /// `brand`.
    fn widget_layer(proof: &mut GroveDBProof) -> &mut LayerProof {
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = proof else {
            panic!("aggregate proofs use the V1 envelope");
        };
        root_layer
            .lower_layers
            .get_mut(TEST_LEAF)
            .expect("TEST_LEAF layer")
            .lower_layers
            .get_mut(WIDGET)
            .expect("widget layer")
    }

    #[test]
    fn proves_a_leaf_aggregate_through_an_empty_aggregate_tree() {
        for v in [&GROVE_V3, &GROVE_V4] {
            proves_a_leaf_aggregate_through_an_empty_aggregate_tree_at(v);
        }
    }

    fn proves_a_leaf_aggregate_through_an_empty_aggregate_tree_at(v: &GroveVersion) {
        for (aggregate, empty_tree) in EMPTY_AGGREGATE_TREES {
            let brand = empty_tree();
            let context = format!("{aggregate:?} through an empty {}", brand.type_str());
            let db = with_empty_brand(brand, v);
            let expected_root = root_hash(&db, v);
            let path_query = leaf_path_query(aggregate);

            let proof = db
                .prove_query(&path_query, None, v)
                .unwrap()
                .unwrap_or_else(|e| {
                    panic!(
                        "{context}: proves at grove version {}: {e}",
                        v.protocol_version
                    )
                });

            // `brand` is proved in the widget layer with nothing below it:
            // the very proof of the plain single-key query over the path.
            let mut decoded = decode_envelope(&proof);
            assert!(
                widget_layer(&mut decoded).lower_layers.is_empty(),
                "{context}: no lower layer under the empty brand"
            );
            let single_key_proof = db
                .prove_query(&single_key_path_query(), None, v)
                .unwrap()
                .expect("the plain single-key query proves");
            assert_eq!(
                proof, single_key_proof,
                "{context}: the aggregate proof is the single-key proof"
            );

            // The single-key query verifies it to the real root, with no row.
            let (root, rows) = GroveDb::verify_query(&proof, &single_key_path_query(), v)
                .unwrap_or_else(|e| panic!("{context}: single-key verification: {e}"));
            assert_eq!(root, expected_root, "{context}");
            assert!(rows.is_empty(), "{context}: nothing exists under brand");

            // The aggregate verifiers still demand a descent along the whole
            // path, so they refuse a path that ends in an empty tree.
            let error = aggregate
                .verify_leaf(&proof, &path_query, v)
                .expect_err("the leaf aggregate verifier refuses the missing descent");
            assert_refused_for_a_missing_lower_layer(&error, BRAND, &format!("{context} (leaf)"));
            let error = aggregate
                .verify_per_key(&proof, &path_query, v)
                .expect_err("the per-key aggregate verifier refuses the missing descent");
            assert_refused_for_a_missing_lower_layer(
                &error,
                BRAND,
                &format!("{context} (per key)"),
            );
        }
    }

    #[test]
    fn proves_a_carrier_aggregate_whose_path_ends_in_an_empty_aggregate_tree() {
        for v in [&GROVE_V3, &GROVE_V4] {
            proves_a_carrier_aggregate_whose_path_ends_in_an_empty_aggregate_tree_at(v);
        }
    }

    fn proves_a_carrier_aggregate_whose_path_ends_in_an_empty_aggregate_tree_at(v: &GroveVersion) {
        for (aggregate, empty_tree) in EMPTY_AGGREGATE_TREES {
            let brand = empty_tree();
            let context = format!("{aggregate:?} carrier under an empty {}", brand.type_str());
            let db = with_empty_brand(brand, v);
            let expected_root = root_hash(&db, v);
            let path_query = carrier_path_query(aggregate);

            let proof = db
                .prove_query(&path_query, None, v)
                .unwrap()
                .unwrap_or_else(|e| {
                    panic!(
                        "{context}: proves at grove version {}: {e}",
                        v.protocol_version
                    )
                });
            let mut decoded = decode_envelope(&proof);
            assert!(
                widget_layer(&mut decoded).lower_layers.is_empty(),
                "{context}: no lower layer under the empty brand"
            );

            let (root, rows) = GroveDb::verify_query(&proof, &single_key_path_query(), v)
                .unwrap_or_else(|e| panic!("{context}: single-key verification: {e}"));
            assert_eq!(root, expected_root, "{context}");
            assert!(rows.is_empty(), "{context}");

            let error = aggregate
                .verify_per_key(&proof, &path_query, v)
                .expect_err("the per-key aggregate verifier refuses the missing descent");
            assert_refused_for_a_missing_lower_layer(&error, BRAND, &context);
        }
    }

    #[test]
    fn proves_a_carrier_key_that_is_an_empty_aggregate_tree_above_the_terminal() {
        // `brand` is populated: "acme" leads to a populated `color`
        // terminal, "zeta" is an empty aggregate tree with the `color`
        // subquery path still below it.
        for (aggregate, empty_tree) in EMPTY_AGGREGATE_TREES {
            let zeta = empty_tree();
            let context = format!(
                "{aggregate:?} carrier key zeta, an empty {}",
                zeta.type_str()
            );
            let build = |v: &GroveVersion| {
                let db = make_test_grovedb(v);
                insert(&db, &[TEST_LEAF], WIDGET, Element::empty_tree(), v);
                insert(&db, &[TEST_LEAF, WIDGET], BRAND, Element::empty_tree(), v);
                insert(
                    &db,
                    &[TEST_LEAF, WIDGET, BRAND],
                    ACME,
                    Element::empty_tree(),
                    v,
                );
                insert_populated_color(&db, &[TEST_LEAF, WIDGET, BRAND, ACME], aggregate, v);
                insert(&db, &[TEST_LEAF, WIDGET, BRAND], ZETA, empty_tree(), v);
                db
            };
            let path_query = carrier_path_query(aggregate);

            for v in [&GROVE_V3, &GROVE_V4] {
                let db = build(v);
                let proof = db
                    .prove_query(&path_query, None, v)
                    .unwrap()
                    .unwrap_or_else(|e| {
                        panic!(
                            "{context}: proves at grove version {}: {e}",
                            v.protocol_version
                        )
                    });
                let mut decoded = decode_envelope(&proof);
                let brand_layer = widget_layer(&mut decoded)
                    .lower_layers
                    .get(BRAND)
                    .expect("brand layer");
                assert!(
                    brand_layer.lower_layers.contains_key(ACME),
                    "{context}: acme is descended into"
                );
                assert!(
                    !brand_layer.lower_layers.contains_key(ZETA),
                    "{context}: no lower layer under the empty zeta"
                );

                // The per-key verifier expects a descent for every matched key.
                let error = aggregate
                    .verify_per_key(&proof, &path_query, v)
                    .expect_err("the per-key aggregate verifier refuses the missing descent");
                assert_refused_for_a_missing_lower_layer(&error, ZETA, &context);
            }
        }
    }

    #[test]
    fn an_empty_aggregate_terminal_is_proved_identically_under_grove_v3_and_v4() {
        // Unchanged: an empty tree that IS the terminal is descended into at
        // every version and answers zero.
        for (aggregate, empty_tree) in EMPTY_AGGREGATE_TREES {
            let color = empty_tree();
            let context = format!("{aggregate:?} over an empty {} terminal", color.type_str());
            let proofs: Vec<Vec<u8>> = [&GROVE_V3, &GROVE_V4]
                .into_iter()
                .map(|v| {
                    let db = make_test_grovedb(v);
                    insert(&db, &[TEST_LEAF], WIDGET, Element::empty_tree(), v);
                    insert(&db, &[TEST_LEAF, WIDGET], BRAND, Element::empty_tree(), v);
                    insert(
                        &db,
                        &[TEST_LEAF, WIDGET, BRAND],
                        ACME,
                        Element::empty_tree(),
                        v,
                    );
                    insert(
                        &db,
                        &[TEST_LEAF, WIDGET, BRAND, ACME],
                        COLOR,
                        empty_tree(),
                        v,
                    );
                    let path_query = leaf_path_query(aggregate);
                    let proof = db
                        .prove_query(&path_query, None, v)
                        .unwrap()
                        .unwrap_or_else(|e| panic!("{context}: proves: {e}"));
                    let (root, count, sum) = aggregate
                        .verify_leaf(&proof, &path_query, v)
                        .unwrap_or_else(|e| panic!("{context}: verifies: {e}"));
                    assert_eq!(root, root_hash(&db, v), "{context}");
                    assert_eq!((count, sum), (0, 0), "{context}");
                    proof
                })
                .collect();
            assert_eq!(proofs[0], proofs[1], "{context}: identical proof bytes");
        }
    }

    #[test]
    fn a_populated_tree_cannot_be_passed_off_as_the_empty_tree_on_the_path() {
        // The proof shape for an empty tree on the path is a bare node
        // with no lower layer. Forge it from an honest proof over a POPULATED
        // `brand`: give its node the empty tree's element bytes (keeping the
        // committed value hash, so the Merk root is unchanged) and drop its
        // lower layer. The empty claim is bound to the value hash, so the
        // forgery is refused.
        let v = &GROVE_V4;
        for (aggregate, empty_tree) in EMPTY_AGGREGATE_TREES {
            let context = format!("{aggregate:?}, forged empty {}", empty_tree().type_str());
            let db = make_test_grovedb(v);
            insert(&db, &[TEST_LEAF], WIDGET, Element::empty_tree(), v);
            insert(&db, &[TEST_LEAF, WIDGET], BRAND, empty_tree(), v);
            insert(
                &db,
                &[TEST_LEAF, WIDGET, BRAND],
                ACME,
                Element::empty_tree(),
                v,
            );
            insert_populated_color(&db, &[TEST_LEAF, WIDGET, BRAND, ACME], aggregate, v);
            let path_query = leaf_path_query(aggregate);

            let proof = db
                .prove_query(&path_query, None, v)
                .unwrap()
                .expect("the honest proof");
            let (root, count, sum) = aggregate
                .verify_leaf(&proof, &path_query, v)
                .unwrap_or_else(|e| panic!("{context}: the honest proof verifies: {e}"));
            assert_eq!(root, root_hash(&db, v), "{context}");
            assert_eq!((count, sum), aggregate.populated_answer(), "{context}");

            let fake_bytes = empty_tree().serialize(v).expect("serialize the empty tree");
            let mut decoded = decode_envelope(&proof);
            let layer = widget_layer(&mut decoded);
            assert!(
                layer.lower_layers.remove(BRAND).is_some(),
                "{context}: the honest proof descends into brand"
            );
            let ProofBytes::Merk(bytes) = &mut layer.merk_proof else {
                panic!("the widget layer is a Merk proof");
            };
            let mut ops: Vec<Op> = Decoder::new(bytes).map(|op| op.expect("op")).collect();
            let mut forged = false;
            for op in &mut ops {
                let (Op::Push(node) | Op::PushInverted(node)) = op else {
                    continue;
                };
                let replacement = match node {
                    Node::KVValueHash(key, _, value_hash) if key.as_slice() == BRAND => {
                        Node::KVValueHash(key.clone(), fake_bytes.clone(), *value_hash)
                    }
                    Node::KVValueHashFeatureType(key, _, value_hash, feature_type)
                    | Node::KVValueHashFeatureTypeWithChildHash(
                        key,
                        _,
                        value_hash,
                        feature_type,
                        _,
                    ) if key.as_slice() == BRAND => Node::KVValueHashFeatureType(
                        key.clone(),
                        fake_bytes.clone(),
                        *value_hash,
                        *feature_type,
                    ),
                    _ => continue,
                };
                *node = replacement;
                forged = true;
            }
            assert!(forged, "{context}: the widget layer carries the brand node");
            let mut forged_bytes = Vec::new();
            encode_into(ops.iter(), &mut forged_bytes);
            *bytes = forged_bytes;
            let forged_proof = reencode_envelope(decoded);

            let error = GroveDb::verify_query(&forged_proof, &single_key_path_query(), v)
                .expect_err("the single-key verifier binds the empty claim");
            assert!(
                error.to_string().contains("empty tree value hash mismatch"),
                "{context}: refused by the empty-tree binding, got: {error}"
            );
            assert!(
                aggregate
                    .verify_leaf(&forged_proof, &path_query, v)
                    .is_err(),
                "{context}: the aggregate verifier refuses the forgery"
            );
        }
    }
}
