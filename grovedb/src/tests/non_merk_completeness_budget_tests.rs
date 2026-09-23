//! Regression tests for audit finding P05 (issue #856): the append-only
//! (non-Merk) lower-layer verifiers must not do work proportional to the
//! parent element's count before that count is bound to the trusted root.
//!
//! The parent `Element` (carrying `mmr_size` / `total_count`) is read out of
//! the proof itself. A forger can therefore declare an astronomically large
//! count, ship an internally consistent lower-layer proof for a single
//! position, and query a broad range with a tiny result limit. Before the
//! fix the verifier expanded every expected position into a `BTreeSet`
//! (bounded only by a 10 M cap) and then `Debug`-formatted every missing
//! position into the error string. Now the expected set is kept as
//! intervals, so the rejection costs O(proof bytes + query items) and the
//! diagnostic spells out at most a fixed number of positions.
//!
//! That fix left the BulkAppendTree / CommitmentTree extraction itself
//! unbounded (audit 2026-09-23): each chunk blob was decoded to whatever
//! entry count it declared, and a 9-byte blob can declare 2^20 entries. The
//! tests after `forged_non_canonical_mmr_size_rejected_before_position_arithmetic`
//! cover that follow-up.

use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

use bincode::config;
use grovedb_bulk_append_tree::{serialize_chunk_blob, BulkAppendTreeProof, DenseTreeProof};
use grovedb_merk::{
    proofs::{encoding::encode_into, Decoder, Node, Op},
    tree::hash::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_merkle_mountain_range::{
    leaf_index_to_mmr_size, leaf_index_to_pos, MmrNode, MmrTreeProof,
};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof, ProofBytes},
    query_result_type::PathKeyOptionalElementTrio,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, Error, GroveDb, PathQuery, Query, SizedQuery,
};

/// The verifier may spell out at most this many positions in a
/// completeness/soundness rejection, whatever the declared count.
const DIAGNOSTIC_BYTES_CEILING: usize = 512;

fn envelope_config() -> impl config::Config {
    config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>()
}

/// A path query at the root selecting `key` with a `RangeFull` subquery and
/// a tiny result limit — the "broad query, small limit" shape from the
/// finding.
fn range_full_under(key: &[u8], limit: u16) -> PathQuery {
    let mut query = Query::new();
    query.insert_key(key.to_vec());
    query.set_subquery(Query::new_range_full());
    PathQuery::new(Vec::new(), SizedQuery::new(query, Some(limit), None))
}

/// Build a synthetic MMR proof for leaf index 0 of an MMR with the given
/// size. Every other node is an arbitrary internal hash, so the proof is
/// internally consistent but commits to a root nobody holds. Generation is
/// lazy (O(log n) node reads), so a 2^33-leaf MMR is cheap to describe.
fn synthetic_mmr_proof(mmr_size: u64, leaf_value: Vec<u8>) -> MmrTreeProof {
    MmrTreeProof::generate(mmr_size, &[0], |pos| {
        Ok(Some(if pos == 0 {
            MmrNode::leaf(leaf_value.clone())
        } else {
            MmrNode::internal(*blake3::hash(&pos.to_be_bytes()).as_bytes())
        }))
    })
    .expect("synthetic MMR proof")
}

/// Take an honest V1 proof whose root layer proves `key` in the root Merk,
/// and rewrite that node to carry `forged_element` bound (via the V1
/// `combine_hash(value_hash, lower_hash)` chain) to `forged_lower_hash`,
/// with `forged_lower` as the lower layer. The result is internally
/// consistent up to the (unauthenticated) root hash it derives.
fn forge_root_layer_node(
    honest_proof: &[u8],
    key: &[u8],
    forged_element: &Element,
    forged_lower_hash: CryptoHash,
    forged_lower: ProofBytes,
    grove_version: &GroveVersion,
) -> Vec<u8> {
    let cfg = envelope_config();
    let decoded: GroveDBProof = bincode::decode_from_slice(honest_proof, cfg)
        .expect("decode honest envelope")
        .0;
    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = decoded else {
        panic!("expected a V1 envelope under the latest grove version");
    };
    let ProofBytes::Merk(root_bytes) = &root_layer.merk_proof else {
        panic!("root layer must be a Merk proof");
    };

    let forged_value = forged_element
        .serialize(grove_version)
        .expect("serialize forged element");
    let forged_value_hash = combine_hash(value_hash(&forged_value).value(), &forged_lower_hash)
        .value()
        .to_owned();

    let mut replaced = 0;
    let ops: Vec<Op> = Decoder::new(root_bytes)
        .map(|op| op.expect("decode honest op"))
        .map(|op| {
            let mut rewrite = |node: Node| match node {
                Node::KVValueHash(k, _, _) if k == key => {
                    replaced += 1;
                    Node::KVValueHash(k, forged_value.clone(), forged_value_hash)
                }
                Node::KVValueHashFeatureType(k, _, _, ft) if k == key => {
                    replaced += 1;
                    Node::KVValueHashFeatureType(k, forged_value.clone(), forged_value_hash, ft)
                }
                other => other,
            };
            match op {
                Op::Push(node) => Op::Push(rewrite(node)),
                Op::PushInverted(node) => Op::PushInverted(rewrite(node)),
                other => other,
            }
        })
        .collect();
    assert_eq!(
        replaced, 1,
        "honest proof must carry exactly one node for the key"
    );

    let mut forged_root_bytes = Vec::new();
    encode_into(ops.iter(), &mut forged_root_bytes);

    let mut lower_layers = BTreeMap::new();
    lower_layers.insert(
        key.to_vec(),
        LayerProof {
            merk_proof: forged_lower,
            lower_layers: BTreeMap::new(),
        },
    );
    let forged = GroveDBProof::V1(GroveDBProofV1 {
        root_layer: LayerProof {
            merk_proof: ProofBytes::Merk(forged_root_bytes),
            lower_layers,
        },
    });
    bincode::encode_to_vec(&forged, cfg).expect("encode forged envelope")
}

fn expect_bounded_rejection(
    result: Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error>,
    needle: &str,
) -> String {
    let message = match result {
        Err(Error::InvalidProof(_, message)) => message,
        Err(other) => panic!("expected InvalidProof, got {other:?}"),
        Ok((root, rows)) => panic!(
            "forged proof accepted: root {} with {} rows",
            hex::encode(root),
            rows.len()
        ),
    };
    assert!(
        message.contains(needle),
        "rejection must come from the completeness check; got: {message}"
    );
    assert!(
        message.len() < DIAGNOSTIC_BYTES_CEILING,
        "diagnostic must be bounded regardless of the declared count; got {} bytes: {message}",
        message.len()
    );
    message
}

#[test]
fn forged_mmr_count_rejected_in_bounded_work() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");
    db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![7], None, grove_version)
        .unwrap()
        .expect("append one leaf");

    let path_query = range_full_under(b"mmr", 2);
    let honest = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("honest proof");

    // Declare 2^33 leaves — far beyond the old 10 M enumeration cap —
    // while proving only leaf 0.
    let forged_leaf_count: u64 = 1 << 33;
    let forged_mmr_size = leaf_index_to_mmr_size(forged_leaf_count - 1);
    let forged_mmr = synthetic_mmr_proof(forged_mmr_size, vec![7]);
    let (forged_root, _) = forged_mmr
        .verify_and_get_root()
        .expect("synthetic MMR proof is internally consistent");

    let forged = forge_root_layer_node(
        &honest,
        b"mmr",
        &Element::new_mmr_tree(forged_mmr_size, None),
        forged_root,
        ProofBytes::MMR(forged_mmr.encode_to_vec().expect("encode MMR proof")),
        grove_version,
    );

    let started = std::time::Instant::now();
    let result = GroveDb::verify_query(&forged, &path_query, grove_version);
    let elapsed = started.elapsed();

    let message = expect_bounded_rejection(result, "MMR proof missing requested leaf indices");
    // Total is arithmetic (2^33 - 1 missing), examples are capped.
    assert!(
        message.contains(&format!(
            "{} positions, first 16: [1, 2, 3",
            forged_leaf_count - 1
        )),
        "unexpected diagnostic shape: {message}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "rejection took {elapsed:?}; verification must not scale with the declared count"
    );
}

#[test]
fn forged_bulk_append_count_rejected_in_bounded_work() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let height: u8 = 1;
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(height).expect("valid chunk power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");
    db.bulk_append(EMPTY_PATH, b"bulk", vec![9], None, grove_version)
        .unwrap()
        .expect("append one entry");

    let path_query = range_full_under(b"bulk", 2);
    let honest = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("honest proof");

    // 2^32 completed chunks of 2 entries each: total_count = 2^33 with an
    // empty buffer, so only the chunk MMR needs a (synthetic) proof.
    let chunk_item_count: u64 = 1 << height;
    let completed_chunks: u64 = 1 << 32;
    let forged_total_count = completed_chunks * chunk_item_count;
    let forged_chunk_mmr_size = leaf_index_to_mmr_size(completed_chunks - 1);
    let chunk_blob = serialize_chunk_blob(&[vec![9], vec![10]]).expect("chunk blob");
    let forged_bulk = BulkAppendTreeProof {
        chunk_proof: synthetic_mmr_proof(forged_chunk_mmr_size, chunk_blob),
        buffer_proof: DenseTreeProof {
            entries: Vec::new(),
            node_value_hashes: Vec::new(),
            node_hashes: Vec::new(),
        },
    };
    let (forged_state_root, _) = forged_bulk
        .verify_and_compute_root(height, forged_total_count)
        .expect("synthetic bulk proof is internally consistent");

    let forged = forge_root_layer_node(
        &honest,
        b"bulk",
        &Element::new_bulk_append_tree(forged_total_count, height, None),
        forged_state_root,
        ProofBytes::BulkAppendTree(forged_bulk.encode_to_vec().expect("encode bulk proof")),
        grove_version,
    );

    let started = std::time::Instant::now();
    let result = GroveDb::verify_query(&forged, &path_query, grove_version);
    let elapsed = started.elapsed();

    let message =
        expect_bounded_rejection(result, "BulkAppendTree proof missing requested positions");
    // Chunk 0 proves positions 0 and 1; everything from 2 on is missing.
    assert!(
        message.contains(&format!(
            "{} positions, first 16: [2, 3, 4",
            forged_total_count - 2
        )),
        "unexpected diagnostic shape: {message}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "rejection took {elapsed:?}; verification must not scale with the declared count"
    );
}

/// Honest proofs over every range shape still verify with the interval
/// representation, and a proof that carries an unrequested MMR leaf is
/// still rejected as unsound.
#[test]
fn interval_completeness_keeps_honest_and_unsound_behaviour() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");
    for i in 0..6u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Honest: two disjoint keys plus an overlapping range, ascending.
    let be = |v: u64| v.to_be_bytes().to_vec();
    let mut inner = Query::new();
    inner.insert_key(be(1));
    inner.insert_range(be(3)..be(5));
    inner.insert_key(be(4));
    let mut query = Query::new();
    query.insert_key(b"mmr".to_vec());
    query.set_subquery(inner);
    let path_query = PathQuery::new_unsized(Vec::new(), query);

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("honest proof");
    let (root, rows) =
        GroveDb::verify_query(&proof, &path_query, grove_version).expect("honest verifies");
    assert_eq!(root, db.root_hash(None, grove_version).unwrap().unwrap());
    let keys: Vec<u64> = rows
        .into_iter()
        .map(|(_, key, _)| u64::from_be_bytes(key.as_slice().try_into().unwrap()))
        .collect();
    assert_eq!(keys, vec![1, 3, 4]);

    // Unsound: a proof for leaves {1, 2, 3, 4} answering a query for {1, 3, 4}.
    let wider_query = {
        let mut inner = Query::new();
        inner.insert_range_inclusive(be(1)..=be(4));
        let mut query = Query::new();
        query.insert_key(b"mmr".to_vec());
        query.set_subquery(inner);
        PathQuery::new_unsized(Vec::new(), query)
    };
    let wider_proof = db
        .prove_query(&wider_query, None, grove_version)
        .unwrap()
        .expect("wider honest proof");
    match GroveDb::verify_query(&wider_proof, &path_query, grove_version) {
        Err(Error::InvalidProof(_, message)) => {
            assert!(
                message.contains("MMR proof contains unrequested leaf indices (1 positions: [2])"),
                "unexpected soundness diagnostic: {message}"
            );
        }
        other => panic!("expected soundness rejection, got {other:?}"),
    }
}

/// Issue #692: the forged parent element may also declare an `mmr_size`
/// that no MMR can have. `u64::MAX` maps to `2^63` leaves, so leaf index
/// `2^63 - 1` passed the range check and overflowed the position
/// arithmetic — a panic under overflow checks — before the element was
/// bound to the trusted root. Sizes 2, 5 and 6 round down to the peak set
/// of a smaller MMR. All must be refused as an invalid proof.
#[test]
fn forged_non_canonical_mmr_size_rejected_before_position_arithmetic() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");
    db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![7], None, grove_version)
        .unwrap()
        .expect("append one leaf");

    let path_query = range_full_under(b"mmr", 2);
    let honest = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("honest proof");

    for (forged_mmr_size, leaf_index) in [
        (u64::MAX, (1u64 << 63) - 1),
        (u64::MAX - 1, 0),
        (2, 0),
        (5, 0),
        (6, 0),
    ] {
        let forged_mmr = MmrTreeProof::new(forged_mmr_size, vec![(leaf_index, vec![7])], vec![]);
        let forged = forge_root_layer_node(
            &honest,
            b"mmr",
            &Element::new_mmr_tree(forged_mmr_size, None),
            [0u8; 32],
            ProofBytes::MMR(forged_mmr.encode_to_vec().expect("encode MMR proof")),
            grove_version,
        );

        let result = GroveDb::verify_query(&forged, &path_query, grove_version);
        expect_bounded_rejection(
            result,
            &format!("{forged_mmr_size} is not a valid MMR size"),
        );
    }
}

// ── Forged chunk blobs: extraction bounded before the root is bound ──────

/// A path query at the root selecting `key`, with an unlimited
/// `[0, window)` position range below it. Platform's shielded-note
/// verification uses this shape.
fn window_under(key: &[u8], window: u64) -> PathQuery {
    let mut inner = Query::new();
    inner.insert_range(0u64.to_be_bytes().to_vec()..window.to_be_bytes().to_vec());
    let mut query = Query::new();
    query.insert_key(key.to_vec());
    query.set_subquery(inner);
    PathQuery::new_unsized(Vec::new(), query)
}

/// A path query at the root selecting `key` with an unlimited `RangeFull`
/// subquery.
fn unlimited_range_full_under(key: &[u8]) -> PathQuery {
    let mut query = Query::new();
    query.insert_key(key.to_vec());
    query.set_subquery(Query::new_range_full());
    PathQuery::new_unsized(Vec::new(), query)
}

/// The fixed-format chunk blob of `count` zero-sized entries: 9 bytes
/// whatever the count.
fn zero_size_chunk_blob(count: usize) -> Vec<u8> {
    serialize_chunk_blob(&vec![Vec::new(); count]).expect("chunk blob")
}

/// A synthetic MMR proof of `leaf_count` leaves carrying `blobs` as leaves
/// `0..blobs.len()`. Every other node is an arbitrary internal hash, so the
/// proof is internally consistent but commits to a root nobody holds.
fn synthetic_mmr_proof_with_leaves(leaf_count: u64, blobs: &[Vec<u8>]) -> MmrTreeProof {
    let by_position: HashMap<u64, &Vec<u8>> = blobs
        .iter()
        .enumerate()
        .map(|(index, blob)| (leaf_index_to_pos(index as u64), blob))
        .collect();
    let indices: Vec<u64> = (0..blobs.len() as u64).collect();
    MmrTreeProof::generate(leaf_index_to_mmr_size(leaf_count - 1), &indices, |pos| {
        Ok(Some(match by_position.get(&pos) {
            Some(blob) => MmrNode::leaf((*blob).clone()),
            None => MmrNode::internal(*blake3::hash(&pos.to_be_bytes()).as_bytes()),
        }))
    })
    .expect("synthetic MMR proof")
}

/// A root-level BulkAppendTree at `key` with the given chunk power and
/// entries, and an honest proof for `path_query` over it.
fn honest_bulk_proof(
    key: &[u8],
    height: u8,
    entries: &[Vec<u8>],
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> (crate::tests::TempGroveDb, Vec<u8>) {
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        key,
        Element::empty_bulk_append_tree(height).expect("valid chunk power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");
    for entry in entries {
        db.bulk_append(EMPTY_PATH, key, entry.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }
    let proof = db
        .prove_query(path_query, None, grove_version)
        .unwrap()
        .expect("honest proof");
    (db, proof)
}

/// Forged `height = 1` chunk blobs, each 9 bytes and declaring 2^20
/// zero-sized entries, under a forged element whose value hash is
/// consistent with the forged lower root, queried over an unlimited
/// 8192-position window. The proof carries every chunk the claimed
/// `total_count` has, so completeness holds. Before the fix every blob's
/// positions overlapped the window: extraction decoded 64 × 2^20 entries and
/// `verify_query` returned thousands of duplicate rows under a root nobody
/// holds (about 520k with `total_count = 8192`). Only the caller's later
/// root comparison would have refused it. Now the first blob the window
/// touches is refused. BulkAppendTree and CommitmentTree share this lower
/// layer.
#[test]
fn forged_chunk_blob_counts_refused_before_expansion() {
    let grove_version = GroveVersion::latest();
    let height: u8 = 1;
    let window: u64 = 8192;
    let path_query = window_under(b"bulk", window);
    let (_db, honest) = honest_bulk_proof(b"bulk", height, &[vec![9]], &path_query, grove_version);

    // 64 completed chunks of 2, all carried by the proof.
    let forged_total_count = 128;
    let forged_bulk = BulkAppendTreeProof {
        chunk_proof: synthetic_mmr_proof_with_leaves(
            forged_total_count / 2,
            &vec![zero_size_chunk_blob(1 << 20); 64],
        ),
        buffer_proof: DenseTreeProof {
            entries: Vec::new(),
            node_value_hashes: Vec::new(),
            node_hashes: Vec::new(),
        },
    };
    let (forged_state_root, _) = forged_bulk
        .verify_and_compute_root(height, forged_total_count)
        .expect("synthetic bulk proof is internally consistent");
    let bulk_bytes = forged_bulk.encode_to_vec().expect("encode bulk proof");
    let sinsemilla_root = [3u8; 32];

    for (element, lower_hash, lower) in [
        (
            Element::new_bulk_append_tree(forged_total_count, height, None),
            forged_state_root,
            ProofBytes::BulkAppendTree(bulk_bytes.clone()),
        ),
        (
            Element::new_commitment_tree(forged_total_count, height, None),
            grovedb_commitment_tree::compute_commitment_tree_state_root(
                &sinsemilla_root,
                &forged_state_root,
            ),
            ProofBytes::CommitmentTree([sinsemilla_root.as_slice(), &bulk_bytes].concat()),
        ),
    ] {
        let forged =
            forge_root_layer_node(&honest, b"bulk", &element, lower_hash, lower, grove_version);
        assert!(
            forged.len() < 4096,
            "forged proof is {} bytes",
            forged.len()
        );

        let started = Instant::now();
        let result = GroveDb::verify_query(&forged, &path_query, grove_version);
        let elapsed = started.elapsed();

        expect_bounded_rejection(result, "holds 1048576 entries, expected exactly 2");
        assert!(
            elapsed < Duration::from_secs(5),
            "rejection took {elapsed:?}; work must not scale with declared blob counts"
        );
    }
}

/// The append-only lower layers bind their computed root to the parent
/// row's committed value hash before they extract any row, as the
/// standalone `BulkAppendTreeProof::verify_against_query` does. A lower
/// layer whose root the parent row does not commit to is refused by that
/// binding. Before, it was read for rows first and refused by the
/// completeness check.
#[test]
fn lower_layer_root_bound_before_rows_are_extracted() {
    let grove_version = GroveVersion::latest();

    let bulk_query = unlimited_range_full_under(b"bulk");
    let (_bulk_db, honest_bulk) =
        honest_bulk_proof(b"bulk", 1, &[vec![9]], &bulk_query, grove_version);
    // A buffer proof carrying only a node hash (the #691 shape) derives a
    // root but proves no entry.
    let unentried_bulk = BulkAppendTreeProof {
        chunk_proof: MmrTreeProof::new(0, Vec::new(), Vec::new()),
        buffer_proof: DenseTreeProof {
            entries: Vec::new(),
            node_value_hashes: Vec::new(),
            node_hashes: vec![(0, [7u8; 32])],
        },
    }
    .encode_to_vec()
    .expect("encode bulk proof");

    let mmr_query = unlimited_range_full_under(b"mmr");
    let mmr_db = make_empty_grovedb();
    mmr_db
        .insert(
            EMPTY_PATH,
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");
    for leaf in [vec![7], vec![8]] {
        mmr_db
            .mmr_tree_append(EMPTY_PATH, b"mmr", leaf, None, grove_version)
            .unwrap()
            .expect("append leaf");
    }
    let honest_mmr = mmr_db
        .prove_query(&mmr_query, None, grove_version)
        .unwrap()
        .expect("honest proof");
    // Leaf 0 of two, with an arbitrary sibling standing in for leaf 1.
    let one_of_two_leaves = MmrTreeProof::new(3, vec![(0, vec![7])], vec![[9u8; 32]])
        .encode_to_vec()
        .expect("encode MMR proof");

    let cases = [
        (
            &honest_bulk,
            b"bulk".as_slice(),
            &bulk_query,
            Element::new_bulk_append_tree(1, 1, None),
            ProofBytes::BulkAppendTree(unentried_bulk.clone()),
        ),
        (
            &honest_bulk,
            b"bulk".as_slice(),
            &bulk_query,
            Element::new_commitment_tree(1, 1, None),
            ProofBytes::CommitmentTree([[0u8; 32].as_slice(), &unentried_bulk].concat()),
        ),
        (
            &honest_mmr,
            b"mmr".as_slice(),
            &mmr_query,
            Element::new_mmr_tree(3, None),
            ProofBytes::MMR(one_of_two_leaves),
        ),
    ];
    for (honest, key, path_query, element, lower) in cases {
        // The parent row commits to a lower root of all zeros, which none
        // of these layers derives.
        let forged = forge_root_layer_node(honest, key, &element, [0u8; 32], lower, grove_version);
        match GroveDb::verify_query(&forged, path_query, grove_version) {
            Err(Error::InvalidProof(_, message)) => assert!(
                message.contains("V1 mismatch in lower layer hash"),
                "{element:?}: the binding must refuse before extraction; got: {message}"
            ),
            other => panic!("{element:?}: expected the binding refusal, got {other:?}"),
        }
    }
}

/// Forge a root-level `BulkAppendTree` at `key` of `total_count` entries at
/// `height`, whose chunk MMR carries `blobs` as its first leaves and whose
/// buffer is empty, on top of an honest proof for `path_query`.
fn forge_bulk_chunks(
    path_query: &PathQuery,
    height: u8,
    total_count: u64,
    blobs: &[Vec<u8>],
    grove_version: &GroveVersion,
) -> Vec<u8> {
    let (_db, honest) = honest_bulk_proof(b"bulk", 1, &[vec![9]], path_query, grove_version);
    let forged_bulk = BulkAppendTreeProof {
        chunk_proof: synthetic_mmr_proof_with_leaves(total_count >> height, blobs),
        buffer_proof: DenseTreeProof {
            entries: Vec::new(),
            node_value_hashes: Vec::new(),
            node_hashes: Vec::new(),
        },
    };
    let (forged_state_root, _) = forged_bulk
        .verify_and_compute_root(height, total_count)
        .expect("synthetic bulk proof is internally consistent");
    forge_root_layer_node(
        &honest,
        b"bulk",
        &Element::new_bulk_append_tree(total_count, height, None),
        forged_state_root,
        ProofBytes::BulkAppendTree(forged_bulk.encode_to_vec().expect("encode bulk proof")),
        grove_version,
    )
}

/// A forged proof that claims far more chunks than it carries is refused by
/// completeness before any chunk is decoded. Every carried blob is garbage,
/// so a verifier that decoded before checking completeness would fail on
/// the blob instead. Before, the verifier extracted `2^height` values per
/// carried chunk first: 64 honest-shaped zero-size chunks at `height = 16`
/// gave about 4M values from a 1 KB proof.
#[test]
fn incomplete_forged_chunks_refused_before_decoding() {
    let grove_version = GroveVersion::latest();
    let path_query = unlimited_range_full_under(b"bulk");
    let height: u8 = 16;
    let forged = forge_bulk_chunks(
        &path_query,
        height,
        (1u64 << 20) << height,
        &vec![vec![0xFF]; 64],
        grove_version,
    );

    let message = expect_bounded_rejection(
        GroveDb::verify_query(&forged, &path_query, grove_version),
        "BulkAppendTree proof missing requested positions",
    );
    assert!(
        message.contains(&format!("{} positions", ((1u64 << 20) - 64) << height)),
        "unexpected diagnostic: {message}"
    );
}

/// A broad query with a small limit decodes only the entries it reports.
/// The forged proof carries every chunk of the claimed count, so it is
/// complete, but only the chunk the limit is met in holds real entries; the
/// rest are garbage, and the query still verifies from either end. Before,
/// every chunk was decoded before the limit applied: `2^height` values per
/// carried chunk.
#[test]
fn limited_query_decodes_only_what_it_reports() {
    let grove_version = GroveVersion::latest();
    let height: u8 = 16;
    let chunk_item_count = 1u64 << height;
    let chunks = 64;
    let total_count = chunks as u64 * chunk_item_count;
    let honest_chunk = zero_size_chunk_blob(chunk_item_count as usize);

    for left_to_right in [true, false] {
        let mut blobs = vec![vec![0xFF]; chunks];
        let (real_chunk, expected) = if left_to_right {
            (0, vec![0, 1])
        } else {
            (chunks - 1, vec![total_count - 1, total_count - 2])
        };
        blobs[real_chunk] = honest_chunk.clone();
        let mut query = Query::new();
        query.insert_key(b"bulk".to_vec());
        query.set_subquery(Query {
            left_to_right,
            ..Query::new_range_full()
        });
        let path_query = PathQuery::new(Vec::new(), SizedQuery::new(query, Some(2), None));
        let forged = forge_bulk_chunks(&path_query, height, total_count, &blobs, grove_version);

        let (_forged_root, rows) = GroveDb::verify_query(&forged, &path_query, grove_version)
            .expect("complete forged proof; only the real chunk is decoded");
        let positions: Vec<u64> = rows
            .into_iter()
            .map(|(_, key, element)| {
                assert_eq!(element, Some(Element::new_item(Vec::new())));
                u64::from_be_bytes(key.as_slice().try_into().unwrap())
            })
            .collect();
        assert_eq!(positions, expected, "left_to_right = {left_to_right}");
    }
}

/// Honest proofs still verify through the interval-driven extraction:
/// disjoint keys and ranges across completed chunks and the buffer, both
/// directions, under a limit, over variable-format chunks with empty values
/// and over zero-entry-size fixed-format chunks.
#[test]
fn honest_bulk_queries_across_chunks_and_buffer() {
    let grove_version = GroveVersion::latest();
    let be = |position: u64| position.to_be_bytes().to_vec();
    // height = 2: chunks [0, 4) and [4, 8), buffer [8, 11). Every third
    // value is empty, so the chunks use the variable format.
    let mixed: Vec<Vec<u8>> = (0..11u8)
        .map(|i| if i % 3 == 0 { Vec::new() } else { vec![i] })
        .collect();
    let all_empty = vec![Vec::new(); 11];

    let disjoint = |left_to_right: bool| {
        let mut inner = Query::new_with_direction(left_to_right);
        inner.insert_key(be(1));
        inner.insert_range(be(2)..be(4));
        inner.insert_key(be(6));
        inner.insert_key(be(9));
        inner
    };
    let range_full_desc = Query {
        left_to_right: false,
        ..Query::new_range_full()
    };
    let under = |inner: Query, limit: Option<u16>| {
        let mut query = Query::new();
        query.insert_key(b"bulk".to_vec());
        query.set_subquery(inner);
        PathQuery::new(Vec::new(), SizedQuery::new(query, limit, None))
    };

    for entries in [&mixed, &all_empty] {
        for (path_query, expected) in [
            (under(disjoint(true), None), vec![1u64, 2, 3, 6, 9]),
            (under(disjoint(false), Some(2)), vec![9, 6]),
            (
                under(range_full_desc.clone(), Some(5)),
                vec![10, 9, 8, 7, 6],
            ),
        ] {
            let (db, proof) = honest_bulk_proof(b"bulk", 2, entries, &path_query, grove_version);
            let (root, rows) = GroveDb::verify_query(&proof, &path_query, grove_version)
                .expect("honest proof verifies");
            assert_eq!(root, db.root_hash(None, grove_version).unwrap().unwrap());
            let got: Vec<(u64, Vec<u8>)> = rows
                .into_iter()
                .map(|(_, key, element)| {
                    let position = u64::from_be_bytes(key.as_slice().try_into().unwrap());
                    match element {
                        Some(Element::Item(value, None)) => (position, value),
                        other => panic!("position {position}: expected an item, got {other:?}"),
                    }
                })
                .collect();
            let want: Vec<(u64, Vec<u8>)> = expected
                .iter()
                .map(|&position| (position, entries[position as usize].clone()))
                .collect();
            assert_eq!(got, want);
        }
    }
}
