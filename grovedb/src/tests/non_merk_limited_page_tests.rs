//! Issue #854 — limited proofs over the non-Merk adapters (MMR, dense,
//! bulk-append, commitment tree) must select the page the *trusted*
//! read would: the verifier authenticates the complete position set
//! and the value bound to each position, but the sequence a proof
//! carries them in is NOT authenticated. Truncating that sequence to
//! the limit would let a prover choose which authenticated rows
//! survive. The verifier therefore orders rows by authenticated
//! position and the query direction before applying any limit, and
//! the standalone crate verifiers surface the canonical ascending
//! order regardless of the order they were handed.
//!
//! These tests compare limited and unlimited results against
//! per-position trusted reads in both directions, then re-run the
//! same queries on proofs whose lower layer was permuted and require
//! the identical page.

use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes},
    tests::{common::EMPTY_PATH, make_test_grovedb, TempGroveDb},
    Element, GroveDb, PathQuery, Query, SizedQuery,
};

/// Rows stored per fixture. Chosen so the bulk-append fixture (chunk
/// power 2 → 4 rows per chunk) spans two completed chunks plus a
/// partially filled buffer, exercising both halves of its proof.
const ROWS: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Mmr,
    Dense,
    Bulk,
    CommitmentTree,
}

const ALL_KINDS: [Kind; 4] = [Kind::Mmr, Kind::Dense, Kind::Bulk, Kind::CommitmentTree];

impl Kind {
    fn key(self) -> &'static [u8] {
        match self {
            Kind::Mmr => b"mmr",
            Kind::Dense => b"dense",
            Kind::Bulk => b"bulk",
            Kind::CommitmentTree => b"ct",
        }
    }

    /// Position keys: BE u64 for every adapter except the dense tree,
    /// whose positions are BE u16.
    fn position_key(self, position: u64) -> Vec<u8> {
        match self {
            Kind::Dense => (position as u16).to_be_bytes().to_vec(),
            _ => position.to_be_bytes().to_vec(),
        }
    }

    fn position_from_key(self, key: &[u8]) -> u64 {
        match self {
            Kind::Dense => u16::from_be_bytes(key.try_into().expect("dense key is u16")) as u64,
            _ => u64::from_be_bytes(key.try_into().expect("position key is u64")),
        }
    }
}

fn populate(kind: Kind, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    let key = kind.key();
    let element = match kind {
        Kind::Mmr => Element::empty_mmr_tree(),
        Kind::Dense => Element::empty_dense_tree(4),
        Kind::Bulk => Element::empty_bulk_append_tree(2).expect("valid chunk power"),
        Kind::CommitmentTree => Element::empty_commitment_tree(11).expect("valid chunk power"),
    };
    db.insert(EMPTY_PATH, key, element, None, None, grove_version)
        .unwrap()
        .expect("insert tree element");
    for i in 0..ROWS {
        match kind {
            Kind::Mmr => {
                db.mmr_tree_append(EMPTY_PATH, key, vec![i as u8, 0xA0], None, grove_version)
                    .unwrap()
                    .expect("mmr append");
            }
            Kind::Dense => {
                db.dense_tree_insert(
                    EMPTY_PATH,
                    key,
                    format!("dense_{i}").into_bytes(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("dense insert");
            }
            Kind::Bulk => {
                db.bulk_append(EMPTY_PATH, key, vec![i as u8, 0xB0], None, grove_version)
                    .unwrap()
                    .expect("bulk append");
            }
            Kind::CommitmentTree => {
                let i = i as u8;
                let mut cmx = [0u8; 32];
                cmx[0] = i;
                cmx[31] &= 0x7f;
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
                    key,
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

/// The trusted, non-proof read of one position.
fn trusted_value(
    db: &TempGroveDb,
    kind: Kind,
    position: u64,
    grove_version: &GroveVersion,
) -> Vec<u8> {
    let key = kind.key();
    let read = match kind {
        Kind::Mmr => db.mmr_tree_get_value(EMPTY_PATH, key, position, None, grove_version),
        Kind::Dense => db.dense_tree_get(EMPTY_PATH, key, position as u16, None, grove_version),
        Kind::Bulk => db.bulk_get_value(EMPTY_PATH, key, position, None, grove_version),
        Kind::CommitmentTree => {
            db.commitment_tree_get_value(EMPTY_PATH, key, position, None, grove_version)
        }
    };
    read.unwrap()
        .expect("trusted read")
        .unwrap_or_else(|| panic!("{kind:?}: position {position} must exist"))
}

/// `[0, ROWS)` at the tree's path, walked in `left_to_right` order,
/// capped by the GLOBAL `SizedQuery::limit`.
fn page_query(kind: Kind, left_to_right: bool, limit: Option<u16>) -> PathQuery {
    let mut inner = Query::new_with_direction(left_to_right);
    inner.insert_range_inclusive(kind.position_key(0)..=kind.position_key(ROWS - 1));
    let mut root = Query::new_single_key(kind.key().to_vec());
    root.set_subquery(inner);
    PathQuery::new(vec![], SizedQuery::new(root, limit, None))
}

/// What the trusted read semantics select: positions in query
/// direction, first `limit` of them.
fn expected_positions(left_to_right: bool, limit: Option<u16>) -> Vec<u64> {
    let mut positions: Vec<u64> = (0..ROWS).collect();
    if !left_to_right {
        positions.reverse();
    }
    if let Some(limit) = limit {
        positions.truncate(limit as usize);
    }
    positions
}

/// Verify a proof and return `(root_hash, [(position, value)])`.
fn verified_page(
    proof: &[u8],
    query: &PathQuery,
    kind: Kind,
    grove_version: &GroveVersion,
) -> Result<([u8; 32], Vec<(u64, Vec<u8>)>), crate::Error> {
    let (root_hash, rows) = GroveDb::verify_query(proof, query, grove_version)?;
    let page = rows
        .into_iter()
        .map(|(path, key, element)| {
            assert_eq!(path, vec![kind.key().to_vec()], "row path is the tree path");
            let value = match element {
                Some(Element::Item(value, _)) => value,
                other => panic!("{kind:?}: expected an Item row, got {other:?}"),
            };
            (kind.position_from_key(&key), value)
        })
        .collect();
    Ok((root_hash, page))
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

fn permuted_mmr_bytes(bytes: &[u8]) -> Vec<u8> {
    use grovedb_merkle_mountain_range::MmrTreeProof;
    let honest = MmrTreeProof::decode_from_slice(bytes).expect("decode mmr proof");
    let mut leaves = honest.leaves().to_vec();
    leaves.reverse();
    MmrTreeProof::new(honest.mmr_size(), leaves, honest.proof_items().to_vec())
        .encode_to_vec()
        .expect("encode permuted mmr proof")
}

fn permuted_dense_bytes(bytes: &[u8]) -> Vec<u8> {
    use grovedb_dense_fixed_sized_merkle_tree::DenseTreeProof;
    let mut proof = DenseTreeProof::decode_from_slice(bytes).expect("decode dense proof");
    proof.entries.reverse();
    proof.encode_to_vec().expect("encode permuted dense proof")
}

fn permuted_bulk_bytes(bytes: &[u8]) -> Vec<u8> {
    use grovedb_bulk_append_tree::BulkAppendTreeProof;
    use grovedb_merkle_mountain_range::MmrTreeProof;
    let mut proof = BulkAppendTreeProof::decode_from_slice(bytes).expect("decode bulk proof");
    let mut chunk_leaves = proof.chunk_proof.leaves().to_vec();
    chunk_leaves.reverse();
    proof.chunk_proof = MmrTreeProof::new(
        proof.chunk_proof.mmr_size(),
        chunk_leaves,
        proof.chunk_proof.proof_items().to_vec(),
    );
    proof.buffer_proof.entries.reverse();
    proof.encode_to_vec().expect("encode permuted bulk proof")
}

/// Rebuild `proof` with the lower layer under the tree key carrying its
/// rows in reverse order. Nothing hashed changes: the position→value
/// bindings are intact, only the (unauthenticated) sequence moved.
fn permute_lower_layer(proof: &[u8], kind: Kind) -> Vec<u8> {
    let mut decoded = decode_envelope(proof);
    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
        panic!("expected a V1 envelope");
    };
    let layer = root_layer
        .lower_layers
        .get_mut(kind.key())
        .unwrap_or_else(|| panic!("{kind:?}: lower layer under the tree key"));
    layer.merk_proof = match (&layer.merk_proof, kind) {
        (ProofBytes::MMR(bytes), Kind::Mmr) => ProofBytes::MMR(permuted_mmr_bytes(bytes)),
        (ProofBytes::DenseTree(bytes), Kind::Dense) => {
            ProofBytes::DenseTree(permuted_dense_bytes(bytes))
        }
        (ProofBytes::BulkAppendTree(bytes), Kind::Bulk) => {
            ProofBytes::BulkAppendTree(permuted_bulk_bytes(bytes))
        }
        (ProofBytes::CommitmentTree(bytes), Kind::CommitmentTree) => {
            let (sinsemilla_root, bulk) = bytes.split_at(32);
            let mut rebuilt = sinsemilla_root.to_vec();
            rebuilt.extend(permuted_bulk_bytes(bulk));
            ProofBytes::CommitmentTree(rebuilt)
        }
        _ => panic!("{kind:?}: lower-layer proof bytes are not the adapter's variant"),
    };
    reencode_envelope(decoded)
}

const LIMITS: [Option<u16>; 4] = [None, Some(1), Some(3), Some(7)];

#[test]
fn limited_pages_match_trusted_reads_in_both_directions() {
    let grove_version = GroveVersion::latest();
    for kind in ALL_KINDS {
        let db = populate(kind, grove_version);
        for left_to_right in [true, false] {
            let unlimited_query = page_query(kind, left_to_right, None);
            let unlimited_proof = db
                .prove_query(&unlimited_query, None, grove_version)
                .unwrap()
                .expect("prove unlimited");
            let (_, unlimited_page) =
                verified_page(&unlimited_proof, &unlimited_query, kind, grove_version)
                    .expect("verify unlimited");
            assert_eq!(
                unlimited_page.len(),
                ROWS as usize,
                "{kind:?} ltr={left_to_right}: unlimited page is the whole tree"
            );

            for limit in LIMITS {
                let query = page_query(kind, left_to_right, limit);
                let proof = db
                    .prove_query(&query, None, grove_version)
                    .unwrap()
                    .expect("prove");
                let (_, page) = verified_page(&proof, &query, kind, grove_version).expect("verify");

                let positions: Vec<u64> = page.iter().map(|(p, _)| *p).collect();
                assert_eq!(
                    positions,
                    expected_positions(left_to_right, limit),
                    "{kind:?} ltr={left_to_right} limit={limit:?}: page positions"
                );
                // The limited page is a prefix of the unlimited page …
                assert_eq!(
                    page.as_slice(),
                    &unlimited_page[..page.len()],
                    "{kind:?} ltr={left_to_right} limit={limit:?}: limited is a prefix of unlimited"
                );
                // … and every row carries the trusted value for its position.
                for (position, value) in &page {
                    assert_eq!(
                        value,
                        &trusted_value(&db, kind, *position, grove_version),
                        "{kind:?} ltr={left_to_right} limit={limit:?}: value at {position}"
                    );
                }
            }
        }
    }
}

#[test]
fn permuted_lower_layers_cannot_move_the_page() {
    // The proof carries the COMPLETE requested set (the limit is applied
    // by the verifier, not by shrinking the proof), so a prover that
    // reorders the rows keeps every position/value binding and the
    // root. The verifier must still hand back the honest page.
    let grove_version = GroveVersion::latest();
    for kind in ALL_KINDS {
        let db = populate(kind, grove_version);
        for left_to_right in [true, false] {
            for limit in LIMITS {
                let query = page_query(kind, left_to_right, limit);
                let honest_proof = db
                    .prove_query(&query, None, grove_version)
                    .unwrap()
                    .expect("prove");
                let (honest_root, honest_page) =
                    verified_page(&honest_proof, &query, kind, grove_version)
                        .expect("verify honest");
                assert_eq!(
                    honest_page.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
                    expected_positions(left_to_right, limit),
                );

                let permuted_proof = permute_lower_layer(&honest_proof, kind);
                assert_ne!(
                    permuted_proof, honest_proof,
                    "{kind:?}: the permutation must change the encoded proof"
                );
                let (permuted_root, permuted_page) =
                    verified_page(&permuted_proof, &query, kind, grove_version).unwrap_or_else(
                        |e| panic!("{kind:?} ltr={left_to_right} limit={limit:?}: {e}"),
                    );
                assert_eq!(
                    permuted_root, honest_root,
                    "{kind:?}: permuting unauthenticated order keeps the root"
                );
                assert_eq!(
                    permuted_page, honest_page,
                    "{kind:?} ltr={left_to_right} limit={limit:?}: a permuted proof must \
                     select the same page as the honest one"
                );
            }
        }
    }
}
