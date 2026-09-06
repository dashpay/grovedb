//! Runtime validation for issue #865: proving a BulkAppendTree /
//! CommitmentTree layer must charge the shared limit for the rows the
//! selection actually matches — the normalized cardinality of the
//! selected positions — never for the bounding span of the selection.
//!
//! A gap inside a disjoint selection, a selection entirely beyond the
//! stored positions, or a reversed range matches nothing, so it must
//! leave the budget untouched for a later sibling. The prover and the
//! verifier derive that charge independently; when they disagree the
//! verifier rejects an honest proof (the later sibling's layer is
//! missing or unexpected), so every case below is checked end to end:
//! prove, verify against the live root, and count the rows each path
//! received.

use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::ProveOptions,
    tests::{common::EMPTY_PATH, make_test_grovedb, TempGroveDb},
    Element, GroveDb, PathQuery, Query, SizedQuery,
};

/// Chunk size 4: ten entries span two completed chunks plus a two-entry
/// dense buffer, so a gap can cross both storage regions.
const CHUNK_POWER: u8 = 2;
const ENTRY_COUNT: u64 = 10;
/// The append layer sorts before the sibling so its charge is what the
/// sibling's remaining budget is derived from.
const LAYER_KEY: &[u8] = b"a_layer";
const SIBLING_KEY: &[u8] = b"z_tree";
const SIBLING_ROWS: u8 = 4;
/// Keys at the root: the append layer and its sibling.
const ROOT_KEYS: usize = 2;

#[derive(Clone, Copy)]
enum Layer {
    Bulk,
    Commitment,
}

impl Layer {
    fn name(self) -> &'static str {
        match self {
            Layer::Bulk => "bulk",
            Layer::Commitment => "commitment",
        }
    }
}

fn pos_key(position: u64) -> Vec<u8> {
    position.to_be_bytes().to_vec()
}

/// `root/a_layer` (the append layer, ten entries) beside
/// `root/z_tree/r0..r3` (a plain Merk sibling with four items).
fn make_db(layer: Layer, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    match layer {
        Layer::Bulk => {
            db.insert(
                EMPTY_PATH,
                LAYER_KEY,
                Element::empty_bulk_append_tree(CHUNK_POWER).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert bulk tree");
            for i in 0..ENTRY_COUNT {
                db.bulk_append(
                    EMPTY_PATH,
                    LAYER_KEY,
                    format!("bulk_{i}").into_bytes(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("bulk append");
            }
        }
        Layer::Commitment => {
            db.insert(
                EMPTY_PATH,
                LAYER_KEY,
                Element::empty_commitment_tree(CHUNK_POWER).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree");
            for i in 0..ENTRY_COUNT {
                let i = i as u8;
                let mut cmx = [0u8; 32];
                cmx[0] = i;
                let mut rho = [0u8; 32];
                rho[0] = i;
                rho[1] = 0xB0;
                let mut cv_net = [0u8; 32];
                cv_net[0] = i;
                cv_net[1] = 0xCC;
                let mut enc_data = [0u8; 104];
                enc_data[0] = i;
                let mut epk = [0u8; 32];
                epk[0] = i;
                let mut out_ct = [0u8; 80];
                out_ct[0] = i;
                let ciphertext = grovedb_commitment_tree::TransmittedNoteCiphertext::<
                    grovedb_commitment_tree::DashMemo,
                >::from_parts(
                    epk,
                    grovedb_commitment_tree::NoteBytesData(enc_data),
                    out_ct,
                );
                db.commitment_tree_insert(
                    EMPTY_PATH,
                    LAYER_KEY,
                    cmx,
                    rho,
                    cv_net,
                    ciphertext,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("commitment tree insert");
            }
        }
    }
    db.insert(
        EMPTY_PATH,
        SIBLING_KEY,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert sibling tree");
    for i in 0..SIBLING_ROWS {
        db.insert(
            [SIBLING_KEY].as_ref(),
            format!("r{i}").as_bytes(),
            Element::new_item(vec![i]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sibling row");
    }
    db
}

/// Routes `selection` into the append layer and a full scan into the
/// sibling, under a global limit.
fn budget_query(selection: Query, global_limit: u16) -> PathQuery {
    let mut root = Query::new();
    root.insert_key(LAYER_KEY.to_vec());
    root.insert_key(SIBLING_KEY.to_vec());
    root.add_conditional_subquery(
        grovedb_query::QueryItem::Key(LAYER_KEY.to_vec()),
        None,
        Some(selection),
    );
    root.add_conditional_subquery(
        grovedb_query::QueryItem::Key(SIBLING_KEY.to_vec()),
        None,
        Some(Query::new_range_full()),
    );
    PathQuery::new(vec![], SizedQuery::new(root, Some(global_limit), None))
}

/// Proves and verifies `path_query`, returning `(layer rows, sibling
/// rows)` and asserting the layer rows are exactly `expected_positions`
/// in ascending order.
fn prove_and_count(
    db: &TempGroveDb,
    path_query: &PathQuery,
    prove_options: Option<ProveOptions>,
    grove_version: &GroveVersion,
) -> (Vec<u64>, usize) {
    let proof = db
        .prove_query(path_query, prove_options, grove_version)
        .unwrap()
        .expect("prove");
    let (root_hash, result_set) = GroveDb::verify_query(&proof, path_query, grove_version)
        .expect("an honest proof must verify — prover and verifier budgets stay in sync");
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version).unwrap().unwrap(),
        "verified root must be the database root"
    );
    let mut layer_rows = Vec::new();
    let mut sibling_rows = 0usize;
    for (path, key, element) in result_set {
        assert!(element.is_some(), "every row carries its element");
        match path.as_slice() {
            [p] if p.as_slice() == LAYER_KEY => layer_rows.push(u64::from_be_bytes(
                key.as_slice().try_into().expect("8-byte position"),
            )),
            [p] if p.as_slice() == SIBLING_KEY => sibling_rows += 1,
            other => panic!("unexpected result path {other:?}"),
        }
    }
    (layer_rows, sibling_rows)
}

/// A selection shape, what it matches, and why.
struct Case {
    name: &'static str,
    selection: Query,
    matched: Vec<u64>,
}

fn cases() -> Vec<Case> {
    let mut disjoint = Query::new();
    disjoint.insert_key(pos_key(0));
    disjoint.insert_key(pos_key(9));

    let mut disjoint_ranges = Query::new();
    disjoint_ranges.insert_range(pos_key(1)..pos_key(3));
    disjoint_ranges.insert_range_inclusive(pos_key(8)..=pos_key(9));

    let mut beyond_count = Query::new();
    beyond_count.insert_range(pos_key(20)..pos_key(30));

    let mut absent_key = Query::new();
    absent_key.insert_key(pos_key(50));

    let mut reversed_range = Query::new();
    reversed_range.insert_range(pos_key(8)..pos_key(2));

    let mut reversed_inclusive = Query::new();
    reversed_inclusive.insert_range_inclusive(pos_key(8)..=pos_key(2));

    let mut mixed = Query::new();
    mixed.insert_key(pos_key(1));
    mixed.insert_range(pos_key(8)..pos_key(2));
    mixed.insert_range(pos_key(20)..pos_key(30));

    let mut overlapping = Query::new();
    overlapping.insert_range(pos_key(0)..pos_key(3));
    overlapping.insert_range_inclusive(pos_key(1)..=pos_key(2));
    overlapping.insert_key(pos_key(2));

    vec![
        Case {
            name: "disjoint keys 0 and 9 — the gap spans chunk 0, chunk 1 and the buffer",
            selection: disjoint,
            matched: vec![0, 9],
        },
        Case {
            name: "disjoint ranges [1,3) and [8,9] — the gap spans chunk 1",
            selection: disjoint_ranges,
            matched: vec![1, 2, 8, 9],
        },
        Case {
            name: "range [20,30) entirely beyond the stored positions",
            selection: beyond_count,
            matched: vec![],
        },
        Case {
            name: "absent key 50",
            selection: absent_key,
            matched: vec![],
        },
        Case {
            name: "reversed range [8,2)",
            selection: reversed_range,
            matched: vec![],
        },
        Case {
            name: "reversed inclusive range [8,2]",
            selection: reversed_inclusive,
            matched: vec![],
        },
        Case {
            name: "key 1 beside a reversed range and an out-of-range span",
            selection: mixed,
            matched: vec![1],
        },
        Case {
            name: "overlapping items [0,3), [1,2], key 2 — counted once each",
            selection: overlapping,
            matched: vec![0, 1, 2],
        },
    ]
}

fn check_layer(layer: Layer) {
    let grove_version = GroveVersion::latest();
    let db = make_db(layer, grove_version);
    let prove_option_variants: [Option<ProveOptions>; 3] = [
        None,
        Some(ProveOptions {
            decrease_limit_on_empty_sub_query_result: false,
        }),
        Some(ProveOptions {
            decrease_limit_on_empty_sub_query_result: true,
        }),
    ];

    for case in cases() {
        for global_limit in [1u16, 3, 6, 50] {
            for prove_options in &prove_option_variants {
                let context = format!(
                    "{} / {} / limit {global_limit} / {prove_options:?}",
                    layer.name(),
                    case.name
                );
                let path_query = budget_query(case.selection.clone(), global_limit);
                let (layer_rows, sibling_rows) =
                    prove_and_count(&db, &path_query, *prove_options, grove_version);

                // The layer yields exactly its matched positions, capped by
                // the global limit.
                let expected_layer: Vec<u64> = case
                    .matched
                    .iter()
                    .copied()
                    .take(global_limit as usize)
                    .collect();
                assert_eq!(
                    layer_rows, expected_layer,
                    "{context}: layer rows must be the normalized selection"
                );

                // The sibling receives whatever the matched rows — not the
                // span — left of the budget. It is reachable only when the
                // root Merk proof, which proves at most `limit` keys, has
                // room for both root keys (see
                // `parent_key_cap_not_the_layer_charge_starves_the_sibling_under_limit_one`).
                let expected_sibling = if (global_limit as usize) < ROOT_KEYS {
                    0
                } else {
                    (global_limit as usize)
                        .saturating_sub(expected_layer.len())
                        .min(SIBLING_ROWS as usize)
                };
                assert_eq!(
                    sibling_rows, expected_sibling,
                    "{context}: the sibling's remaining budget is limit minus matched rows"
                );
            }
        }
    }
}

#[test]
fn bulk_layer_charges_matched_rows_not_the_selection_span() {
    check_layer(Layer::Bulk);
}

#[test]
fn commitment_layer_charges_matched_rows_not_the_selection_span() {
    check_layer(Layer::Commitment);
}

#[test]
fn empty_selection_leaves_the_whole_budget_to_the_sibling() {
    // The sharpest form of the finding: a selection that matches nothing
    // must leave every slot to the sibling. Charging the span (or an
    // empty-layer unit) here would starve the sibling and desynchronize
    // prover and verifier. Limit 2 is the tightest budget under which the
    // sibling key is still in the root Merk proof at all.
    let grove_version = GroveVersion::latest();
    for layer in [Layer::Bulk, Layer::Commitment] {
        let db = make_db(layer, grove_version);
        for (name, selection) in [
            ("beyond count", {
                let mut q = Query::new();
                q.insert_range(pos_key(ENTRY_COUNT)..pos_key(ENTRY_COUNT + 5));
                q
            }),
            ("reversed", {
                let mut q = Query::new();
                q.insert_range_inclusive(pos_key(9)..=pos_key(0));
                q
            }),
            ("no items", Query::new()),
        ] {
            for decrease_on_empty in [false, true] {
                for global_limit in [2u16, 3] {
                    let path_query = budget_query(selection.clone(), global_limit);
                    let options = Some(ProveOptions {
                        decrease_limit_on_empty_sub_query_result: decrease_on_empty,
                    });
                    let (layer_rows, sibling_rows) =
                        prove_and_count(&db, &path_query, options, grove_version);
                    assert!(
                        layer_rows.is_empty(),
                        "{} / {name}: nothing is selected",
                        layer.name()
                    );
                    assert_eq!(
                        sibling_rows,
                        global_limit as usize,
                        "{} / {name} / decrease_on_empty={decrease_on_empty} / limit \
                         {global_limit}: the sibling keeps the whole budget",
                        layer.name()
                    );
                }
            }
        }
    }
}

#[test]
fn parent_key_cap_not_the_layer_charge_starves_the_sibling_under_limit_one() {
    // Under a global limit of 1 the sibling receives nothing even when
    // the append layer matches nothing. That is not a charge by the
    // layer: the root Merk proof proves at most `limit` keys, and the
    // sibling is the second root key. A plain empty Merk child in the
    // layer's place starves the sibling identically, and the moment the
    // limit admits both root keys the empty bulk selection hands the
    // sibling the whole budget.
    let grove_version = GroveVersion::latest();

    let mut empty_selection = Query::new();
    empty_selection.insert_range(pos_key(20)..pos_key(30));
    for layer in [Layer::Bulk, Layer::Commitment] {
        let db = make_db(layer, grove_version);
        let (layer_rows, sibling_rows) = prove_and_count(
            &db,
            &budget_query(empty_selection.clone(), 1),
            None,
            grove_version,
        );
        assert!(layer_rows.is_empty());
        assert_eq!(
            sibling_rows,
            0,
            "{}: only one root key fits the limit-1 root proof",
            layer.name()
        );
        let (_, sibling_rows) = prove_and_count(
            &db,
            &budget_query(empty_selection.clone(), 2),
            None,
            grove_version,
        );
        assert_eq!(
            sibling_rows,
            2,
            "{}: with both root keys admitted, the empty selection charges nothing",
            layer.name()
        );
    }

    // Same layout with a plain empty Merk subtree in the layer's place.
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        LAYER_KEY,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert empty tree");
    db.insert(
        EMPTY_PATH,
        SIBLING_KEY,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert sibling tree");
    for i in 0..SIBLING_ROWS {
        db.insert(
            [SIBLING_KEY].as_ref(),
            format!("r{i}").as_bytes(),
            Element::new_item(vec![i]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sibling row");
    }
    let (layer_rows, sibling_rows) = prove_and_count(
        &db,
        &budget_query(Query::new_range_full(), 1),
        None,
        grove_version,
    );
    assert!(layer_rows.is_empty());
    assert_eq!(
        sibling_rows, 0,
        "a plain empty Merk child is starved by the same root key cap"
    );
}
