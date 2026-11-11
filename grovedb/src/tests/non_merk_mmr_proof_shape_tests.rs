//! An MMR proof verifies only in the shape the prover emits, so the rows of
//! one mountain cannot be presented at the positions of another.
//!
//! Three MMR leaves are two mountains (leaves 0 and 1 under one peak, leaf 2
//! alone) and the root bags them with the same hash that merges two
//! siblings. A verifier that walked only as many peaks as the proof had
//! items for rebuilt that root from leaf 2's value at position 0. The MMR
//! verifier now accounts for every peak; these tests hold the three
//! adapters that sit on an MMR to it through `GroveDb::verify_query`: an
//! `MmrTree` (leaves are the rows) and the chunk MMR of a `BulkAppendTree`
//! and of a `CommitmentTree` (leaves are whole chunks of rows).

use grovedb_bulk_append_tree::BulkAppendTreeProof;
use grovedb_merkle_mountain_range::{blake3_merge, leaf_hash, MmrTreeProof};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes},
    tests::{common::EMPTY_PATH, make_test_grovedb, TempGroveDb},
    Element, GroveDb, PathQuery, Query, SizedQuery,
};

/// Chunk power 1: two rows per chunk, so six rows are three completed
/// chunks and an empty buffer.
const CHUNK_POWER: u8 = 1;
const ROWS_PER_CHUNK: u64 = 2;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Mmr,
    Bulk,
    CommitmentTree,
}

impl Kind {
    fn key(self) -> &'static [u8] {
        match self {
            Kind::Mmr => b"mmr",
            Kind::Bulk => b"bulk",
            Kind::CommitmentTree => b"ct",
        }
    }

    /// Rows behind one MMR leaf.
    fn rows_per_leaf(self) -> u64 {
        match self {
            Kind::Mmr => 1,
            Kind::Bulk | Kind::CommitmentTree => ROWS_PER_CHUNK,
        }
    }
}

/// A tree whose MMR has exactly three leaves.
fn populate(kind: Kind, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    let key = kind.key();
    let element = match kind {
        Kind::Mmr => Element::empty_mmr_tree(),
        Kind::Bulk => Element::empty_bulk_append_tree(CHUNK_POWER).expect("valid chunk power"),
        Kind::CommitmentTree => {
            Element::empty_commitment_tree(CHUNK_POWER).expect("valid chunk power")
        }
    };
    db.insert(EMPTY_PATH, key, element, None, None, grove_version)
        .unwrap()
        .expect("insert tree element");
    for i in 0..(3 * kind.rows_per_leaf()) as u8 {
        match kind {
            Kind::Mmr => {
                db.mmr_tree_append(EMPTY_PATH, key, vec![i, 0xA0], None, grove_version)
                    .unwrap()
                    .expect("mmr append");
            }
            Kind::Bulk => {
                db.bulk_append(EMPTY_PATH, key, vec![i, 0xB0], None, grove_version)
                    .unwrap()
                    .expect("bulk append");
            }
            Kind::CommitmentTree => {
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

/// The rows behind MMR leaves `first_leaf..=last_leaf`.
fn leaves_query(kind: Kind, first_leaf: u64, last_leaf: u64) -> PathQuery {
    let first_row = first_leaf * kind.rows_per_leaf();
    let last_row = (last_leaf + 1) * kind.rows_per_leaf() - 1;
    let mut inner = Query::new();
    inner
        .insert_range_inclusive(first_row.to_be_bytes().to_vec()..=last_row.to_be_bytes().to_vec());
    let mut root = Query::new_single_key(kind.key().to_vec());
    root.set_subquery(inner);
    PathQuery::new(vec![], SizedQuery::new(root, None, None))
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

/// Apply `change` to the MMR proof inside the lower layer under the tree key
/// and return the re-encoded GroveDB proof. Everything around the MMR proof
/// is kept: the Merk layers, a bulk tree's buffer proof and a commitment
/// tree's Sinsemilla root.
fn with_mmr_proof(
    proof: &[u8],
    kind: Kind,
    change: &dyn Fn(&MmrTreeProof) -> MmrTreeProof,
) -> Vec<u8> {
    let change_bulk = |bytes: &[u8]| {
        let mut bulk = BulkAppendTreeProof::decode_from_slice(bytes).expect("decode bulk proof");
        bulk.chunk_proof = change(&bulk.chunk_proof);
        bulk.encode_to_vec().expect("encode bulk proof")
    };

    let mut decoded = decode_envelope(proof);
    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
        panic!("expected a V1 envelope");
    };
    let layer = root_layer
        .lower_layers
        .get_mut(kind.key())
        .unwrap_or_else(|| panic!("{kind:?}: lower layer under the tree key"));
    layer.merk_proof = match (&layer.merk_proof, kind) {
        (ProofBytes::MMR(bytes), Kind::Mmr) => {
            let mmr = MmrTreeProof::decode_from_slice(bytes).expect("decode mmr proof");
            ProofBytes::MMR(change(&mmr).encode_to_vec().expect("encode mmr proof"))
        }
        (ProofBytes::BulkAppendTree(bytes), Kind::Bulk) => {
            ProofBytes::BulkAppendTree(change_bulk(bytes))
        }
        (ProofBytes::CommitmentTree(bytes), Kind::CommitmentTree) => {
            let (sinsemilla_root, bulk) = bytes.split_at(32);
            let mut rebuilt = sinsemilla_root.to_vec();
            rebuilt.extend(change_bulk(bulk));
            ProofBytes::CommitmentTree(rebuilt)
        }
        _ => panic!("{kind:?}: lower-layer proof bytes are not the adapter's variant"),
    };
    reencode_envelope(decoded)
}

/// The MMR proof inside the lower layer under the tree key.
fn mmr_proof_of(proof: &[u8], kind: Kind) -> MmrTreeProof {
    let found = std::cell::RefCell::new(None);
    with_mmr_proof(proof, kind, &|mmr| {
        *found.borrow_mut() = Some(mmr.clone());
        mmr.clone()
    });
    found.into_inner().expect("the proof carries an MMR proof")
}

/// Verified rows as `(position, element)`.
fn verified_rows(
    proof: &[u8],
    query: &PathQuery,
    grove_version: &GroveVersion,
) -> Result<([u8; 32], Vec<(u64, Element)>), crate::Error> {
    let (root_hash, rows) = GroveDb::verify_query(proof, query, grove_version)?;
    let rows = rows
        .into_iter()
        .map(|(_path, key, element)| {
            let position = u64::from_be_bytes(key.try_into().expect("position key is u64"));
            (position, element.expect("a queried position has a row"))
        })
        .collect();
    Ok((root_hash, rows))
}

#[test]
fn rows_of_the_last_mountain_do_not_verify_at_the_first_positions() {
    let grove_version = GroveVersion::latest();
    let mut accepted = Vec::new();
    for kind in [Kind::Mmr, Kind::Bulk, Kind::CommitmentTree] {
        let db = populate(kind, grove_version);

        // Every leaf, to read what a prover holds: the three MMR leaves.
        let all_query = leaves_query(kind, 0, 2);
        let all_proof = db
            .prove_query(&all_query, None, grove_version)
            .unwrap()
            .expect("prove every leaf");
        let (honest_root, all_rows) =
            verified_rows(&all_proof, &all_query, grove_version).expect("verify every leaf");
        let all_mmr = mmr_proof_of(&all_proof, kind);
        assert_eq!(all_mmr.mmr_size(), 4, "{kind:?}: three leaves");
        let leaves = all_mmr.leaves().to_vec();
        assert_eq!(leaves.len(), 3, "{kind:?}: three leaves");

        // The honest answer for leaf 0.
        let first_query = leaves_query(kind, 0, 0);
        let first_proof = db
            .prove_query(&first_query, None, grove_version)
            .unwrap()
            .expect("prove leaf 0");
        let (first_root, first_rows) =
            verified_rows(&first_proof, &first_query, grove_version).expect("verify leaf 0");
        assert_eq!(first_root, honest_root);
        let rows_per_leaf = kind.rows_per_leaf() as usize;
        assert_eq!(first_rows, all_rows[..rows_per_leaf], "{kind:?}");

        // Leaf 2 presented as leaf 0, with the peak over leaves 0 and 1 as
        // the only proof item: `merge(leaf 2, peak)` is the root.
        let first_peak = blake3_merge(&leaf_hash(&leaves[0].1), &leaf_hash(&leaves[1].1));
        let relocated_proof = with_mmr_proof(&first_proof, kind, &|honest| {
            MmrTreeProof::new(
                honest.mmr_size(),
                vec![(0, leaves[2].1.clone())],
                vec![first_peak],
            )
        });
        if let Ok((root, rows)) = verified_rows(&relocated_proof, &first_query, grove_version) {
            accepted.push(format!(
                "{kind:?}: a proof carrying leaf 2 as leaf 0 verified (honest root: {}), with \
                 positions {:?} holding the rows of positions {:?}",
                root == honest_root,
                rows.iter()
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>(),
                all_rows
                    .iter()
                    .filter(|(_, element)| rows.iter().any(|(_, row)| row == element))
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>(),
            ));
        }
    }
    assert!(accepted.is_empty(), "{accepted:#?}");
}
