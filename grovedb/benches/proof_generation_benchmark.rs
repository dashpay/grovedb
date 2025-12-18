//! Proof-generation benchmark

#[cfg(feature = "minimal")]
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
#[cfg(feature = "minimal")]
use grovedb::{Element, GroveDb, TransactionArg};
#[cfg(feature = "minimal")]
use grovedb::{PathQuery, Query, SizedQuery};
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use rand::{rngs::StdRng, Rng, SeedableRng};
#[cfg(feature = "minimal")]
use std::ops::Range;
#[cfg(feature = "minimal")]
use tempfile::TempDir;

#[cfg(feature = "minimal")]
const N_KEYS: u32 = 10_000;

/// Single-item proof at various depths
#[cfg(feature = "minimal")]
pub fn single_item_proof(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_item_proof");

    for &depth in &[0_usize, 5, 10] {
        group.bench_function(BenchmarkId::from_parameter(format!("depth_{depth}")), |b| {
            b.iter_batched(
                || setup_db(depth), // fresh DB each sample
                |(db, path, key)| {
                    // Build PathQuery for the single key
                    let mut query = Query::new();
                    query.insert_key(key.clone());
                    let pq = PathQuery::new(
                        path,
                        grovedb::SizedQuery {
                            query,
                            limit: None,
                            offset: None,
                        },
                    );

                    let _proof = db.get_proved_path_query(
                        &pq,
                        /* prove_options */ None,
                        TransactionArg::default(),
                        &GroveVersion::default(),
                    );
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// “full-leaf” proof (range query over ≈10 k items)
#[cfg(feature = "minimal")]
pub fn full_leaf_proof(c: &mut Criterion) {
    // Build DB once (Criterion clones Arc internally; no mutation)
    let (db, path, _first_key) = setup_db(1); // depth = 1 keeps things tidy

    // Range query that returns every key in the subtree
    let mut q = Query::new();
    q.insert_range(Range {
        start: b"\x00".to_vec(),
        end: b"\xFF".to_vec(),
    });
    let pq = PathQuery::new(
        path,
        SizedQuery {
            query: q,
            limit: None,
            offset: None,
        },
    );

    c.bench_function("full_leaf_proof", |b| {
        b.iter(|| {
            let _proof = db.get_proved_path_query(
                &pq,
                None,
                TransactionArg::default(),
                &GroveVersion::default(),
            );
        });
    });
}

///  Helper to construct a GroveDB with <depth> nested empty subtrees and
///  populate the deepest leaf with N_KEYS items.
///  Returns (db, path_to_leaf, first_inserted_key)
#[cfg(feature = "minimal")]
fn setup_db(depth: usize) -> (GroveDb, Vec<Vec<u8>>, Vec<u8>) {
    let dir = TempDir::new().expect("tmpdir");
    let db = GroveDb::open(dir.path()).expect("open");
    let mut rng = StdRng::seed_from_u64(42);

    // Build nested path:  [], [k0], [k0,k1], …
    let mut path: Vec<Vec<u8>> = Vec::with_capacity(depth);
    for _ in 0..depth {
        let node: [u8; 32] = rng.random();
        db.insert(
            path.as_slice(),
            &node,
            Element::empty_tree(),
            None,
            None,
            &GroveVersion::default(),
        )
        .unwrap()
        .unwrap();
        path.push(node.to_vec());
    }

    // Fill leaf with data
    let mut first_key = None;
    for _ in 0..N_KEYS {
        let k: [u8; 32] = rng.random();
        if first_key.is_none() {
            first_key = Some(k.to_vec());
        }
        db.insert(
            path.as_slice(),
            &k,
            Element::new_item(k.to_vec()),
            None,
            None,
            &GroveVersion::default(),
        )
        .unwrap()
        .unwrap();
    }

    (db, path, first_key.unwrap())
}

#[cfg(feature = "minimal")]
criterion_group!(benches, single_item_proof, full_leaf_proof);
#[cfg(feature = "minimal")]
criterion_main!(benches);
