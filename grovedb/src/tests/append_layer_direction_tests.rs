//! Runtime validation for issue #855: a descending, limited read of a
//! BulkAppendTree / CommitmentTree layer must hand back the SAME window
//! from the direct read, the generated proof, and the verified result.
//!
//! The append-only layers store rows by position, and their proof
//! extraction sorts positions ascending. The verifier orders the rows by
//! the query's trusted direction BEFORE consuming the shared limit, so a
//! descending cap keeps the highest positions (the last appended rows)
//! rather than truncating the ascending order to its lowest ones. These
//! tests pin that across chunk blobs and the dense buffer, for both
//! `SizedQuery::limit` (global) and `Query::limit` (per-instance), and
//! compare every window against the canonical direct range read.

use grovedb_version::version::GroveVersion;

use crate::{
    query_result_type::QueryResultType,
    tests::{common::EMPTY_PATH, make_test_grovedb, TempGroveDb},
    Element, GroveDb, PathQuery, Query, SizedQuery,
};

/// Chunk size 4: ten entries span two completed chunks plus a two-entry
/// dense buffer, so a window can straddle both storage regions.
const CHUNK_POWER: u8 = 2;
const ENTRY_COUNT: u64 = 10;
const BULK_KEY: &[u8] = b"bulk";
const CT_KEY: &[u8] = b"ct";

#[derive(Clone, Copy)]
enum Layer {
    Bulk,
    Commitment,
}

impl Layer {
    fn key(self) -> &'static [u8] {
        match self {
            Layer::Bulk => BULK_KEY,
            Layer::Commitment => CT_KEY,
        }
    }

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

fn make_db(layer: Layer, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    match layer {
        Layer::Bulk => {
            db.insert(
                EMPTY_PATH,
                BULK_KEY,
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
                    BULK_KEY,
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
                CT_KEY,
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
                    CT_KEY,
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
    db
}

/// The canonical direct read: every `(position, value)` of the layer,
/// ascending, straight from storage.
fn direct_all(db: &TempGroveDb, layer: Layer, grove_version: &GroveVersion) -> Vec<(u64, Vec<u8>)> {
    let page = match layer {
        Layer::Bulk => db
            .bulk_get_range(EMPTY_PATH, BULK_KEY, 0, u16::MAX, None, grove_version)
            .unwrap()
            .expect("bulk range read"),
        Layer::Commitment => db
            .commitment_tree_get_range(EMPTY_PATH, CT_KEY, 0, u16::MAX, None, grove_version)
            .unwrap()
            .expect("commitment tree range read"),
    };
    assert_eq!(page.total_count, ENTRY_COUNT);
    assert_eq!(page.entries.len() as u64, ENTRY_COUNT);
    page.entries
}

/// Which side of a query carries the cap.
#[derive(Clone, Copy, Debug)]
enum Cap {
    /// `SizedQuery::limit` on the whole path query.
    Global(u16),
    /// `Query::limit` on the layer's own subquery (per-instance).
    Instance(u16),
}

/// Builds `root[key] -> inner` where `inner` selects `[start, end)` of
/// the layer in `left_to_right` order under `cap`.
fn window_query(layer: Layer, start: u64, end: u64, left_to_right: bool, cap: Cap) -> PathQuery {
    let mut inner = Query::new_with_direction(left_to_right);
    inner.insert_range(pos_key(start)..pos_key(end));
    let mut global = None;
    match cap {
        Cap::Global(limit) => global = Some(limit),
        Cap::Instance(limit) => inner.limit = Some(limit),
    }
    let mut root = Query::new_single_key(layer.key().to_vec());
    root.set_subquery(inner);
    PathQuery::new(vec![], SizedQuery::new(root, global, None))
}

/// What a correct read of `[start, end)` in `left_to_right` order capped
/// at `limit` rows must return, derived from the direct read alone.
fn expected_window(
    all: &[(u64, Vec<u8>)],
    start: u64,
    end: u64,
    left_to_right: bool,
    limit: u16,
) -> Vec<(u64, Vec<u8>)> {
    let mut rows: Vec<(u64, Vec<u8>)> = all
        .iter()
        .filter(|(p, _)| *p >= start && *p < end)
        .cloned()
        .collect();
    if !left_to_right {
        rows.reverse();
    }
    rows.truncate(limit as usize);
    rows
}

/// Proves `path_query`, verifies it against the live root, and returns
/// the verified rows as `(position, value)` in result order.
fn proved_window(
    db: &TempGroveDb,
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> Vec<(u64, Vec<u8>)> {
    let proof = db
        .prove_query(path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let (root_hash, result_set) =
        GroveDb::verify_query(&proof, path_query, grove_version).expect("verify");
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version).unwrap().unwrap(),
        "verified root must be the database root"
    );
    result_set
        .into_iter()
        .map(|(path, key, element)| {
            assert_eq!(path.len(), 1, "rows live one level below the root");
            let position = u64::from_be_bytes(key.as_slice().try_into().expect("8-byte position"));
            let value = match element.expect("every row carries its element") {
                Element::Item(bytes, _) => bytes,
                other => panic!("append-layer rows are items, got {other:?}"),
            };
            (position, value)
        })
        .collect()
}

/// The trusted (non-proof) read of the same path query.
fn trusted_window(
    db: &TempGroveDb,
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> Result<Vec<(u64, Vec<u8>)>, crate::Error> {
    let (elements, _) = db
        .query_raw(
            path_query,
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            grove_version,
        )
        .unwrap()?;
    Ok(elements
        .to_path_key_elements()
        .into_iter()
        .map(|(_, key, element)| {
            let position = u64::from_be_bytes(key.as_slice().try_into().expect("8-byte position"));
            let value = match element {
                Element::Item(bytes, _) => bytes,
                other => panic!("append-layer rows are items, got {other:?}"),
            };
            (position, value)
        })
        .collect())
}

/// Windows that straddle the storage regions: the full tree, a span
/// crossing both chunk boundaries, the second chunk into the buffer, and
/// a chunk-interior span.
const WINDOWS: [(u64, u64); 4] = [(0, ENTRY_COUNT), (2, 9), (5, ENTRY_COUNT), (1, 3)];

fn check_layer(layer: Layer) {
    let grove_version = GroveVersion::latest();
    let db = make_db(layer, grove_version);
    let all = direct_all(&db, layer, grove_version);
    for (position, value) in &all {
        assert!(
            !value.is_empty(),
            "{}: position {position} has a value",
            layer.name()
        );
    }

    for (start, end) in WINDOWS {
        for left_to_right in [true, false] {
            for limit in [1u16, 3, 200] {
                for cap in [Cap::Global(limit), Cap::Instance(limit)] {
                    let path_query = window_query(layer, start, end, left_to_right, cap);
                    let expected = expected_window(&all, start, end, left_to_right, limit);
                    let proved = proved_window(&db, &path_query, grove_version);
                    let context = format!(
                        "{} [{start}, {end}) left_to_right={left_to_right} {cap:?}",
                        layer.name()
                    );
                    assert_eq!(
                        proved, expected,
                        "{context}: verified proof rows must be the direct read's window"
                    );

                    // The proof must carry the whole selected span, not
                    // only the yielded window: verifying the SAME proof
                    // against the uncapped query in either direction
                    // still succeeds and returns every selected row.
                    let proof = db
                        .prove_query(&path_query, None, grove_version)
                        .unwrap()
                        .expect("prove");
                    for uncapped_dir in [true, false] {
                        let uncapped =
                            window_query(layer, start, end, uncapped_dir, Cap::Global(u16::MAX));
                        let (_, rows) = GroveDb::verify_query(&proof, &uncapped, grove_version)
                            .unwrap_or_else(|e| panic!("{context}: uncapped verify failed: {e}"));
                        assert_eq!(
                            rows.len() as u64,
                            end - start,
                            "{context}: the proof carries the whole selected span"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn bulk_descending_limited_proofs_return_the_direct_reads_window() {
    check_layer(Layer::Bulk);
}

#[test]
fn commitment_descending_limited_proofs_return_the_direct_reads_window() {
    check_layer(Layer::Commitment);
}

#[test]
fn descending_window_is_the_reverse_of_its_ascending_twin() {
    // A descending capped read is the LAST rows of the ascending
    // uncapped read, reversed — never a prefix of it.
    let grove_version = GroveVersion::latest();
    for layer in [Layer::Bulk, Layer::Commitment] {
        let db = make_db(layer, grove_version);
        let asc = proved_window(
            &db,
            &window_query(layer, 0, ENTRY_COUNT, true, Cap::Global(u16::MAX)),
            grove_version,
        );
        assert_eq!(asc.len() as u64, ENTRY_COUNT);
        for limit in [1u16, 4, 7] {
            for cap in [Cap::Global(limit), Cap::Instance(limit)] {
                let desc = proved_window(
                    &db,
                    &window_query(layer, 0, ENTRY_COUNT, false, cap),
                    grove_version,
                );
                let tail: Vec<(u64, Vec<u8>)> =
                    asc.iter().rev().take(limit as usize).cloned().collect();
                assert_eq!(
                    desc,
                    tail,
                    "{} {cap:?}: descending cap keeps the highest positions",
                    layer.name()
                );
                assert_ne!(
                    desc.first().map(|(p, _)| *p),
                    Some(0),
                    "{} {cap:?}: a descending cap must not start at position 0",
                    layer.name()
                );
            }
        }
    }
}

#[test]
fn trusted_reads_of_append_layers_never_disagree_with_verified_proofs() {
    // The trusted read engine (`query_raw`) does not descend into the
    // append-only layers: their position rows are served by the
    // dedicated range reads (`bulk_get_range` /
    // `commitment_tree_get_range`, compared above) and by proofs. What
    // must hold is that the trusted engine never hands back a
    // DIFFERENT window from the verified proof — it may refuse the
    // shape or yield nothing, but it must not serve the opposite end
    // of the range in either direction under either cap kind.
    let grove_version = GroveVersion::latest();
    for layer in [Layer::Bulk, Layer::Commitment] {
        let db = make_db(layer, grove_version);
        for left_to_right in [true, false] {
            for cap in [Cap::Global(3), Cap::Instance(3)] {
                let path_query = window_query(layer, 0, ENTRY_COUNT, left_to_right, cap);
                let proved = proved_window(&db, &path_query, grove_version);
                assert_eq!(proved.len(), 3);
                if let Ok(trusted) = trusted_window(&db, &path_query, grove_version) {
                    assert!(
                        trusted.is_empty() || trusted == proved,
                        "{} left_to_right={left_to_right} {cap:?}: trusted read served a \
                         different window ({trusted:?}) from the verified proof ({proved:?})",
                        layer.name()
                    );
                }
            }
        }
    }
}
