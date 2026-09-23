//! Regression tests for the 2026-09-23 audit finding: forged chunk blobs in
//! a `BulkAppendTree` / `CommitmentTree` proof drove unbounded work in value
//! extraction before the proof was bound to any trusted root. This was an
//! incomplete fix of issue #856 (PR #872).
//!
//! The GroveDB lower-layer verifier takes `total_count` and `height` from
//! element bytes carried in the proof, calls `verify_and_compute_root`, which
//! computes a root without comparing it to anything, and then extracts
//! values. The caller authenticates the root only after verification
//! returns. The fixed chunk-blob format `[0x01][count u32][entry_size u32]`
//! with `entry_size = 0` is 9 bytes whatever `count` says. With a forged
//! `height = 1`, every blob's positions overlapped the query window. So a
//! proof of a few hundred bytes made the verifier allocate and iterate
//! 2^20 entries per blob.
//!
//! These tests use only the crate's public API. A malicious responder can
//! emit the same forged proof on the wire.

use std::time::{Duration, Instant};

use grovedb_bulk_append_tree::{
    deserialize_chunk_blob, deserialize_completed_chunk_blob, leaf_count_to_mmr_size,
    serialize_chunk_blob, BulkAppendError, BulkAppendTreeProof, DenseTreeProof,
};
use grovedb_merkle_mountain_range::MmrTreeProof;

/// `MAX_CHUNK_ENTRIES` in `chunk.rs`: the most the general decoder accepts.
const MAX_CHUNK_ENTRIES: u32 = 1 << 20;

/// The fixed-format chunk blob of `count` zero-sized entries: 9 bytes
/// whatever `count` is.
fn zero_size_blob(count: u32) -> Vec<u8> {
    serialize_chunk_blob(&vec![Vec::new(); count as usize]).expect("serialize")
}

fn empty_buffer_proof() -> DenseTreeProof {
    DenseTreeProof {
        entries: Vec::new(),
        node_value_hashes: Vec::new(),
        node_hashes: Vec::new(),
    }
}

/// A proof whose chunk MMR carries every one of `blobs` as a leaf, with no
/// buffer. Proving every leaf needs no sibling hashes, so the proof is
/// internally consistent. Its root matches no real tree.
fn forged_proof(blobs: Vec<Vec<u8>>) -> BulkAppendTreeProof {
    let leaf_count = blobs.len() as u64;
    let leaves = blobs.into_iter().enumerate().map(|(i, b)| (i as u64, b));
    BulkAppendTreeProof {
        chunk_proof: MmrTreeProof::new(
            leaf_count_to_mmr_size(leaf_count),
            leaves.collect(),
            vec![],
        ),
        buffer_proof: empty_buffer_proof(),
    }
}

/// A proof carrying the blob is refused as an invalid proof.
fn assert_count_refused(err: BulkAppendError, declared: u64, expected: u64) {
    let needle = format!("holds {declared} entries, expected exactly {expected}");
    assert!(
        matches!(&err, BulkAppendError::InvalidProof(m) if m.contains(&needle)),
        "expected the completed-chunk count refusal ({needle}), got {err:?}"
    );
}

/// T1: the 9-byte blob still decodes to 2^20 entries through the general
/// decoder, which reads trusted local storage. A completed chunk from a
/// proof goes through the exact-count decoder, which refuses it before
/// allocating.
#[test]
fn t1_nine_byte_blob_cannot_expand_past_its_chunk() {
    let blob = zero_size_blob(MAX_CHUNK_ENTRIES);
    assert_eq!(blob.len(), 9);
    assert_eq!(
        deserialize_chunk_blob(&blob)
            .expect("general decoder")
            .len(),
        MAX_CHUNK_ENTRIES as usize
    );

    let err = deserialize_completed_chunk_blob(&blob, 2).expect_err("height-1 chunks hold 2");
    assert!(
        matches!(&err, BulkAppendError::CorruptedData(m)
            if m.contains("holds 1048576 entries, expected exactly 2")),
        "{err:?}"
    );
}

/// T2: a 17-byte-payload forged proof passes `verify_and_compute_root`, by
/// design: it has no root to compare against. Extraction then refuses the
/// blob instead of expanding it.
#[test]
fn t2_single_forged_leaf_is_refused_at_extraction() {
    let height: u8 = 1;
    let total_count: u64 = 2; // one completed chunk, empty buffer
    let proof = forged_proof(vec![zero_size_blob(MAX_CHUNK_ENTRIES)]);

    let (_unbound_root, result) = proof
        .verify_and_compute_root(height, total_count)
        .expect("internally consistent; nothing to bind it to yet");

    let err = result
        .values_in_range(0, total_count)
        .expect_err("a height-1 chunk cannot hold 2^20 entries");
    assert_count_refused(err, MAX_CHUNK_ENTRIES as u64, 2);
}

/// T3: 64 forged leaves under a forged `height = 1`, queried over an
/// 8192-position window (the shape Platform's shielded-note verification
/// uses, with no limit). Before the fix every blob's 2^20 positions
/// overlapped the window, so extraction produced 64 × 8192 duplicate rows
/// after about 67M decode iterations. Now the first blob the window touches
/// is refused.
#[test]
fn t3_many_forged_leaves_are_refused_in_bounded_work() {
    let height: u8 = 1;
    let k: u64 = 64;
    let proof = forged_proof(vec![zero_size_blob(MAX_CHUNK_ENTRIES); k as usize]);
    let proof_bytes = proof.encode_to_vec().expect("encodes").len();
    assert!(proof_bytes < 1024, "forged proof is {proof_bytes} bytes");

    let (_unbound_root, result) = proof
        .verify_and_compute_root(height, k * 2)
        .expect("internally consistent");

    let started = Instant::now();
    let err = result
        .values_in_range(0, 8192)
        .expect_err("forged blobs must be refused");
    let elapsed = started.elapsed();
    assert_count_refused(err, MAX_CHUNK_ENTRIES as u64, 2);
    assert!(
        elapsed < Duration::from_secs(5),
        "refusal took {elapsed:?}; work must not scale with declared counts"
    );
}

/// The same refusal through `verify_against_query`, the standalone
/// verifier, which binds the root first. A consistent forged root reaches
/// extraction and is refused there too.
#[test]
fn forged_blob_refused_by_verify_against_query() {
    let height: u8 = 1;
    let proof = forged_proof(vec![zero_size_blob(8192)]);
    let (forged_root, _) = proof
        .verify_and_compute_root(height, 2)
        .expect("internally consistent");
    let err = proof
        .verify_range(&forged_root, height, 2, 0, 2)
        .expect_err("forged blob must be refused");
    assert_count_refused(err, 8192, 2);
}

/// Chunks of zero-sized entries are honest, since empty values are
/// appendable. They stay honest-sized: each chunk yields at most its own
/// `2^height` positions, and only the chunks the query touches are decoded.
/// Every untouched chunk here is garbage, so extraction succeeding shows
/// they were never decoded.
#[test]
fn honest_shaped_zero_size_chunks_are_bounded_by_the_query() {
    let height: u8 = 16;
    let chunk_item_count: u64 = 1 << height;
    let k: u64 = 256;
    let honest_blob = zero_size_blob(chunk_item_count as u32);
    assert_eq!(honest_blob.len(), 9);
    let blobs = (0..k)
        .map(|i| {
            if i == 3 || i == 4 {
                honest_blob.clone()
            } else {
                vec![0xFF]
            }
        })
        .collect();
    let proof = forged_proof(blobs);
    let (_root, result) = proof
        .verify_and_compute_root(height, k * chunk_item_count)
        .expect("internally consistent");

    // A window straddling chunks 3 and 4: two chunks decoded, 8192 rows.
    let start = 4 * chunk_item_count - 4096;
    let rows = result
        .values_in_range(start, start + 8192)
        .expect("only the two honest chunks are decoded");
    assert_eq!(rows.len(), 8192);
    assert!(
        rows.iter()
            .enumerate()
            .all(|(i, (pos, value))| *pos == start + i as u64 && value.is_empty()),
        "rows must be the window's positions, once each"
    );
}
