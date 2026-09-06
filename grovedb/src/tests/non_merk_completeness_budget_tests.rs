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

use std::collections::BTreeMap;

use bincode::config;
use grovedb_bulk_append_tree::{serialize_chunk_blob, BulkAppendTreeProof, DenseTreeProof};
use grovedb_merk::{
    proofs::{encoding::encode_into, Decoder, Node, Op},
    tree::hash::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_merkle_mountain_range::{leaf_index_to_mmr_size, MmrNode, MmrTreeProof};
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
