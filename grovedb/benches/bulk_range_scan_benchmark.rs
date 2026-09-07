//! Benchmark for the BulkAppendTree paged-scan pattern.
//!
//! Clients of append-only stores walk "all entries since my cursor" in
//! pages. This benchmark measures that pattern end to end on a
//! BulkAppendTree populated with fixed-size entries:
//!
//! 1. **Paged read** (`bulk_get_range`): fetching one page of entries,
//!    chunk-aligned, at various page sizes.
//! 2. **Paged proof generation** (`prove_bulk_position_range`): proving one
//!    page.
//! 3. **Paged proof verification** (`verify_bulk_position_range_proof`):
//!    verifying one page.
//! 4. **Full scan**: walking the entire tree page by page with proofs, the
//!    shielded-pool-style sync flow.

#[cfg(feature = "minimal")]
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
#[cfg(feature = "minimal")]
use grovedb::{Element, GroveDb};
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use tempfile::TempDir;

/// Total number of entries appended to the tree.
#[cfg(feature = "minimal")]
const N_ENTRIES: u32 = 4096;

/// Chunk power of the tree: chunk size = 2^6 = 64 entries, so the tree holds
/// 64 completed chunks with the buffer empty.
#[cfg(feature = "minimal")]
const CHUNK_POWER: u8 = 6;

/// Size of each entry in bytes (a 32-byte commitment plus a small payload).
#[cfg(feature = "minimal")]
const ENTRY_SIZE: usize = 96;

#[cfg(feature = "minimal")]
fn setup_db() -> (TempDir, GroveDb) {
    let grove_version = GroveVersion::latest();
    let dir = TempDir::new().expect("cannot create temp dir");
    let db = GroveDb::open(dir.path()).expect("cannot open grovedb");

    db.insert(
        &[] as &[&[u8]],
        b"bulk",
        Element::empty_bulk_append_tree(CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    for i in 0..N_ENTRIES {
        let mut entry = vec![0u8; ENTRY_SIZE];
        entry[..4].copy_from_slice(&i.to_be_bytes());
        db.bulk_append(&[] as &[&[u8]], b"bulk", entry, None, grove_version)
            .unwrap()
            .expect("bulk append");
    }

    (dir, db)
}

/// Read one page of entries at various page sizes, starting mid-tree so the
/// page is not chunk-aligned.
#[cfg(feature = "minimal")]
pub fn paged_read(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = setup_db();
    let mut group = c.benchmark_group("bulk_range_paged_read");

    for &page_size in &[16u16, 256, 1024] {
        group.bench_function(BenchmarkId::from_parameter(page_size), |b| {
            b.iter(|| {
                let page = db
                    .bulk_get_range(
                        &[] as &[&[u8]],
                        b"bulk",
                        (N_ENTRIES / 3) as u64,
                        page_size,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("bulk get range");
                assert_eq!(page.entries.len(), page_size as usize);
            });
        });
    }
    group.finish();
}

/// Prove one page at various page sizes.
#[cfg(feature = "minimal")]
pub fn paged_proof_generation(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = setup_db();
    let mut group = c.benchmark_group("bulk_range_paged_prove");

    for &page_size in &[16u16, 256, 1024] {
        group.bench_function(BenchmarkId::from_parameter(page_size), |b| {
            b.iter(|| {
                let _proof = db
                    .prove_bulk_position_range(
                        vec![],
                        b"bulk",
                        (N_ENTRIES / 3) as u64,
                        page_size,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("prove bulk position range");
            });
        });
    }
    group.finish();
}

/// Verify one page proof at various page sizes.
#[cfg(feature = "minimal")]
pub fn paged_proof_verification(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = setup_db();
    let mut group = c.benchmark_group("bulk_range_paged_verify");

    for &page_size in &[16u16, 256, 1024] {
        let start = (N_ENTRIES / 3) as u64;
        let proof = db
            .prove_bulk_position_range(vec![], b"bulk", start, page_size, None, grove_version)
            .unwrap()
            .expect("prove bulk position range");

        group.bench_function(BenchmarkId::from_parameter(page_size), |b| {
            b.iter(|| {
                let (_root, page) = GroveDb::verify_bulk_position_range_proof(
                    &proof,
                    vec![],
                    b"bulk",
                    start,
                    page_size,
                    grove_version,
                )
                .expect("verify bulk position range proof");
                assert_eq!(page.entries.len(), page_size as usize);
            });
        });
    }
    group.finish();
}

/// Walk the entire tree with proved pages of 256 — the client sync flow.
#[cfg(feature = "minimal")]
pub fn full_paged_scan_with_proofs(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = setup_db();
    const PAGE: u16 = 256;

    c.bench_function("bulk_range_full_scan_with_proofs", |b| {
        b.iter(|| {
            let mut cursor = 0u64;
            let mut total = 0usize;
            loop {
                let proof = db
                    .prove_bulk_position_range(vec![], b"bulk", cursor, PAGE, None, grove_version)
                    .unwrap()
                    .expect("prove page");
                let (_root, page) = GroveDb::verify_bulk_position_range_proof(
                    &proof,
                    vec![],
                    b"bulk",
                    cursor,
                    PAGE,
                    grove_version,
                )
                .expect("verify page");
                if page.entries.is_empty() {
                    break;
                }
                cursor += page.entries.len() as u64;
                total += page.entries.len();
            }
            assert_eq!(total, N_ENTRIES as usize);
        });
    });
}

#[cfg(feature = "minimal")]
criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = paged_read,
        paged_proof_generation,
        paged_proof_verification,
        full_paged_scan_with_proofs
);
#[cfg(feature = "minimal")]
criterion_main!(benches);

#[cfg(not(feature = "minimal"))]
fn main() {}
