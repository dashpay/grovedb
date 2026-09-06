//! Issue #869 — the V1 verifier must not act on a tree's *claimed*
//! emptiness before binding it.
//!
//! A merk `KVValueHash*` node hashes the key and the `value_hash`, never
//! the element bytes, so a prover can rewrite a non-empty tree's bytes
//! into the empty form of the same type without disturbing the root.
//! The verifier authenticates the bytes of trees it descends into
//! (through the lower layer's root) and of empty trees it *reports*
//! (`combine_hash(H(value), NULL_HASH)`), but two paths used to act on
//! the unbound classification alone: an "empty" tree under a subquery
//! was charged as an empty child and skipped, and an "empty" plain tree
//! at a terminal position was dropped when empty trees are excluded
//! from results. Either turns an omitted descent into a verified
//! absence against a valid root.
//!
//! These tests forge exactly that and require rejection, and check that
//! genuinely empty trees keep verifying and agree with trusted reads.

use grovedb_merk::proofs::{encoding::encode_into, query::VerifyOptions, Decoder, Node, Op};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes},
    query_result_type::QueryResultType,
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, GroveDb, PathQuery, Query, SizedQuery,
};

const DOCS: &[u8] = b"docs";
const EMPTY: &[u8] = b"empty";
const ROWS: u8 = 3;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Tree,
    SumTree,
    ProvableCountTree,
    /// Two child Merks: the empty form commits
    /// `combine_hash_three(H(value), NULL_HASH, NULL_HASH)`.
    ProvableSumIndexedTree,
    ProvableCountIndexedTree,
    /// Axes list: the empty form commits
    /// `combine_hash_three(H(value), NULL_HASH, axes_digest(zero_axes))`.
    ProvableCountProvableSumIndexedTree,
}

const KINDS: [Kind; 6] = [
    Kind::Tree,
    Kind::SumTree,
    Kind::ProvableCountTree,
    Kind::ProvableSumIndexedTree,
    Kind::ProvableCountIndexedTree,
    Kind::ProvableCountProvableSumIndexedTree,
];

/// The PCPSIT fixture indexes all three axes.
fn pcpsit_axes() -> Vec<(u8, Option<Vec<u8>>)> {
    vec![(0, None), (1, None), (2, None)]
}

impl Kind {
    fn empty(self) -> Element {
        match self {
            Kind::Tree => Element::empty_tree(),
            Kind::SumTree => Element::empty_sum_tree(),
            Kind::ProvableCountTree => Element::empty_provable_count_tree(),
            Kind::ProvableSumIndexedTree => Element::empty_provable_sum_indexed_tree(),
            Kind::ProvableCountIndexedTree => Element::empty_provable_count_indexed_tree(),
            Kind::ProvableCountProvableSumIndexedTree => {
                Element::empty_provable_count_provable_sum_indexed_tree(pcpsit_axes())
                    .expect("canonical axes")
            }
        }
    }

    /// Insert row `i` under `TEST_LEAF/docs` through the tree's own
    /// insert path.
    fn insert_row(self, db: &TempGroveDb, i: u8, grove_version: &GroveVersion) {
        let path = [TEST_LEAF, DOCS];
        let key = [b'k', i];
        match self {
            Kind::Tree | Kind::ProvableCountTree => {
                db.insert(
                    path.as_ref(),
                    &key,
                    Element::new_item(vec![i]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert row");
            }
            Kind::SumTree => {
                db.insert(
                    path.as_ref(),
                    &key,
                    Element::new_sum_item(i as i64 + 1),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert row");
            }
            Kind::ProvableSumIndexedTree => {
                db.insert_into_provable_sum_indexed_tree(
                    path.as_ref(),
                    &key,
                    Element::new_sum_item(i as i64 + 1),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert PSIT row");
            }
            Kind::ProvableCountIndexedTree => {
                // PCIT rows are counted subtrees; give each one an item
                // so the row is a non-empty tree.
                db.insert_into_count_indexed_tree(
                    path.as_ref(),
                    &key,
                    Element::empty_provable_count_tree(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert PCIT row");
                db.insert(
                    [TEST_LEAF, DOCS, key.as_slice()].as_ref(),
                    b"v",
                    Element::new_item(vec![i]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert PCIT row item");
            }
            Kind::ProvableCountProvableSumIndexedTree => {
                db.insert_into_provable_count_provable_sum_indexed_tree(
                    path.as_ref(),
                    &key,
                    Element::new_item_with_sum_item(vec![i], i as i64 + 1),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert PCPSIT row");
            }
        }
    }
}

/// `TEST_LEAF/docs` — a populated tree of `kind` — and `TEST_LEAF/empty`,
/// a genuinely empty tree of the same kind.
fn populate(kind: Kind, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    for key in [DOCS, EMPTY] {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            kind.empty(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");
    }
    for i in 0..ROWS {
        kind.insert_row(&db, i, grove_version);
    }
    db
}

/// The rejection must come from binding the claimed emptiness — the
/// two-input check for plain trees, the three-input one for indexed
/// trees — not from some unrelated structural complaint.
fn assert_rejected_by_emptiness_binding(kind: Kind, include_empty: bool, error: &crate::Error) {
    let message = format!("{error}");
    assert!(
        message.contains("empty tree value hash mismatch")
            || message.contains("empty indexed-tree value hash mismatch"),
        "{kind:?} include_empty={include_empty}: rejection must come from binding the \
         claimed emptiness, got: {message}"
    );
}

/// `TEST_LEAF/<key>` with a `RangeFull` subquery — a required descent.
fn descent_query(key: &[u8]) -> PathQuery {
    let mut root = Query::new_single_key(key.to_vec());
    root.set_subquery(Query::new_range_full());
    PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], root)
}

/// `TEST_LEAF/<key>` with no subquery — the tree element itself is the
/// terminal row.
fn terminal_query(key: &[u8], limit: Option<u16>) -> PathQuery {
    PathQuery::new(
        vec![TEST_LEAF.to_vec()],
        SizedQuery::new(Query::new_single_key(key.to_vec()), limit, None),
    )
}

fn trusted_row_count(db: &TempGroveDb, query: &PathQuery, grove_version: &GroveVersion) -> usize {
    db.query_raw(
        query,
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        grove_version,
    )
    .unwrap()
    .expect("trusted read")
    .0
    .len()
}

fn options(include_empty_trees_in_result: bool) -> VerifyOptions {
    VerifyOptions {
        absence_proofs_for_non_existing_searched_keys: false,
        verify_proof_succinctness: true,
        include_empty_trees_in_result,
    }
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

/// Rewrite the merk node carrying `target_key` in the `TEST_LEAF` layer
/// so its element bytes decode as `fake`, keeping the node's
/// authenticated `value_hash` (and feature type) untouched, and drop any
/// lower layer under that key. The merk root is unchanged: nothing the
/// node hashes was modified.
fn forge_element_bytes(
    proof: &[u8],
    target_key: &[u8],
    fake: &Element,
    grove_version: &GroveVersion,
) -> Vec<u8> {
    let mut decoded = decode_envelope(proof);
    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
        panic!("expected a V1 envelope");
    };
    let layer = root_layer
        .lower_layers
        .get_mut(TEST_LEAF)
        .expect("TEST_LEAF lower layer");
    layer.lower_layers.remove(target_key);
    let ProofBytes::Merk(bytes) = &mut layer.merk_proof else {
        panic!("TEST_LEAF layer is a merk proof");
    };
    let fake_bytes = fake.serialize(grove_version).expect("serialize fake");

    let mut ops: Vec<Op> = Decoder::new(bytes).map(|r| r.expect("decode op")).collect();
    let mut forged = false;
    for op in &mut ops {
        let node = match op {
            Op::Push(node) | Op::PushInverted(node) => node,
            _ => continue,
        };
        let replacement = match node {
            Node::KVValueHash(key, _, value_hash) if key.as_slice() == target_key => {
                Node::KVValueHash(key.clone(), fake_bytes.clone(), *value_hash)
            }
            Node::KVValueHashFeatureType(key, _, value_hash, feature_type)
                if key.as_slice() == target_key =>
            {
                Node::KVValueHashFeatureType(
                    key.clone(),
                    fake_bytes.clone(),
                    *value_hash,
                    feature_type.clone(),
                )
            }
            // Drop the child hash: the merk verifier would otherwise
            // bind the bytes through it. The V1 verifier only demands
            // a child hash for trees it believes are NON-empty.
            Node::KVValueHashFeatureTypeWithChildHash(key, _, value_hash, feature_type, _)
                if key.as_slice() == target_key =>
            {
                Node::KVValueHashFeatureType(
                    key.clone(),
                    fake_bytes.clone(),
                    *value_hash,
                    feature_type.clone(),
                )
            }
            _ => continue,
        };
        *node = replacement;
        forged = true;
    }
    assert!(forged, "the TEST_LEAF layer must carry the target node");
    let mut new_bytes = Vec::new();
    encode_into(ops.iter(), &mut new_bytes);
    *bytes = new_bytes;
    reencode_envelope(decoded)
}

#[test]
fn forged_empty_tree_under_a_subquery_is_rejected() {
    // A required descent whose lower layer was omitted and whose
    // element bytes now claim "empty": the verifier must not settle for
    // the claim, it must bind it — and the claim is false.
    let grove_version = GroveVersion::latest();
    for kind in KINDS {
        let db = populate(kind, grove_version);
        let query = descent_query(DOCS);
        let trusted = trusted_row_count(&db, &query, grove_version);
        assert_eq!(
            trusted, ROWS as usize,
            "{kind:?}: trusted read sees the rows"
        );

        let honest = db
            .prove_query(&query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (honest_root, rows) =
            GroveDb::verify_query_with_options(&honest, &query, options(false), grove_version)
                .expect("honest proof verifies");
        assert_eq!(
            rows.len(),
            trusted,
            "{kind:?}: honest proof matches trusted read"
        );

        let forged = forge_element_bytes(&honest, DOCS, &kind.empty(), grove_version);
        for include_empty in [false, true] {
            let outcome = GroveDb::verify_query_with_options(
                &forged,
                &query,
                options(include_empty),
                grove_version,
            );
            match outcome {
                Err(e) => assert_rejected_by_emptiness_binding(kind, include_empty, &e),
                Ok((root, rows)) => panic!(
                    "{kind:?} include_empty={include_empty}: forged-empty tree under a \
                     subquery verified as {} rows against root {} (honest root {})",
                    rows.len(),
                    hex::encode(root),
                    hex::encode(honest_root)
                ),
            }
        }
    }
}

#[test]
fn forged_empty_tree_at_a_terminal_position_is_rejected() {
    // The tree itself is the queried row. With empty trees excluded from
    // results the verifier used to drop a plain `Tree(None)` without
    // binding it, so a forged-empty non-empty tree became a verified
    // absence of its key.
    let grove_version = GroveVersion::latest();
    for kind in KINDS {
        let db = populate(kind, grove_version);
        let query = terminal_query(DOCS, None);
        assert_eq!(trusted_row_count(&db, &query, grove_version), 1);

        let honest = db
            .prove_query(&query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (_, rows) =
            GroveDb::verify_query_with_options(&honest, &query, options(false), grove_version)
                .expect("honest proof verifies");
        assert_eq!(rows.len(), 1, "{kind:?}: a non-empty tree is a result row");

        let forged = forge_element_bytes(&honest, DOCS, &kind.empty(), grove_version);
        for include_empty in [false, true] {
            let outcome = GroveDb::verify_query_with_options(
                &forged,
                &query,
                options(include_empty),
                grove_version,
            );
            match outcome {
                Err(e) => assert_rejected_by_emptiness_binding(kind, include_empty, &e),
                Ok((_, rows)) => panic!(
                    "{kind:?} include_empty={include_empty}: forged-empty terminal tree \
                     verified as {} rows",
                    rows.len()
                ),
            }
        }
    }

    // The absence-proof projection is the sharpest form of the outcome:
    // the searched key would come back as a proven `None`.
    let db = populate(Kind::Tree, grove_version);
    let query = terminal_query(DOCS, Some(1));
    let honest = db
        .prove_query(&query, None, grove_version)
        .unwrap()
        .expect("prove");
    let forged = forge_element_bytes(&honest, DOCS, &Element::empty_tree(), grove_version);
    let outcome = GroveDb::verify_query_with_options(
        &forged,
        &query,
        VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: true,
            verify_proof_succinctness: true,
            include_empty_trees_in_result: false,
        },
        grove_version,
    );
    assert!(
        outcome.is_err(),
        "a forged-empty tree must not project to a proven absence, got {:?}",
        outcome.map(|(_, rows)| rows)
    );
}

#[test]
fn genuinely_empty_trees_keep_verifying_and_match_trusted_reads() {
    let grove_version = GroveVersion::latest();
    for kind in KINDS {
        let db = populate(kind, grove_version);

        // Required descent into an empty tree: an empty child, no rows.
        let query = descent_query(EMPTY);
        let trusted = trusted_row_count(&db, &query, grove_version);
        assert_eq!(trusted, 0, "{kind:?}: nothing under an empty tree");
        let proof = db
            .prove_query(&query, None, grove_version)
            .unwrap()
            .expect("prove");
        for include_empty in [false, true] {
            let (_, rows) = GroveDb::verify_query_with_options(
                &proof,
                &query,
                options(include_empty),
                grove_version,
            )
            .unwrap_or_else(|e| panic!("{kind:?} include_empty={include_empty}: {e}"));
            assert_eq!(
                rows.len(),
                trusted,
                "{kind:?}: empty descent yields no rows"
            );
        }

        // The empty tree as the terminal row: reported only when the
        // caller asks for empty trees (plain `Tree`), or always for the
        // typed empties, exactly as before.
        let query = terminal_query(EMPTY, None);
        let proof = db
            .prove_query(&query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (_, excluded) =
            GroveDb::verify_query_with_options(&proof, &query, options(false), grove_version)
                .unwrap_or_else(|e| panic!("{kind:?} exclude-empty: {e}"));
        let (_, included) =
            GroveDb::verify_query_with_options(&proof, &query, options(true), grove_version)
                .unwrap_or_else(|e| panic!("{kind:?} include-empty: {e}"));
        assert_eq!(
            included.len(),
            1,
            "{kind:?}: included empty tree is one row"
        );
        let expected_excluded = match kind {
            Kind::Tree => 0,
            _ => 1,
        };
        assert_eq!(
            excluded.len(),
            expected_excluded,
            "{kind:?}: exclusion applies to plain empty trees only"
        );
    }
}

#[test]
fn unbound_non_tree_classification_under_a_subquery_is_rejected() {
    // The arm chain's last resort: an element that is neither an item,
    // a descended tree, a reported row, nor a bound empty tree. Honest
    // V1 proofs dereference references into their target's bytes, so
    // raw reference bytes under a subquery can only come from a prover
    // rewriting the (unhashed) element bytes. They used to be skipped
    // silently — another route to a verified absence.
    let grove_version = GroveVersion::latest();
    let db = populate(Kind::Tree, grove_version);
    let query = descent_query(DOCS);
    let honest = db
        .prove_query(&query, None, grove_version)
        .unwrap()
        .expect("prove");
    let fake = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
        TEST_LEAF.to_vec(),
        EMPTY.to_vec(),
    ]));
    let forged = forge_element_bytes(&honest, DOCS, &fake, grove_version);
    for include_empty in [false, true] {
        let outcome = GroveDb::verify_query_with_options(
            &forged,
            &query,
            options(include_empty),
            grove_version,
        );
        match outcome {
            Err(e) => {
                let message = format!("{e}");
                assert!(
                    message.contains("neither descended into nor bound"),
                    "include_empty={include_empty}: got: {message}"
                );
            }
            Ok((_, rows)) => panic!(
                "include_empty={include_empty}: unbound reference under a subquery verified \
                 as {} rows",
                rows.len()
            ),
        }
    }
}
