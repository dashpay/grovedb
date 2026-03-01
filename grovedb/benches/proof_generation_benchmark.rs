//! Proof generation and verification benchmarks

#[cfg(feature = "minimal")]
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
#[cfg(feature = "minimal")]
use grovedb::{Element, GroveDb, PathQuery, Query, SizedQuery};
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use rand::{rngs::StdRng, Rng, SeedableRng};
#[cfg(feature = "minimal")]
use tempfile::TempDir;

#[cfg(feature = "minimal")]
const N_KEYS: u32 = 10_000;

/// Single-item proof generation at various depths
#[cfg(feature = "minimal")]
pub fn single_item_proof(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let mut group = c.benchmark_group("single_item_proof");

    for &depth in &[0_usize, 5, 10] {
        group.bench_function(BenchmarkId::from_parameter(format!("depth_{depth}")), |b| {
            b.iter_batched(
                || setup_db(depth),
                |(_dir, db, path, key)| {
                    let mut query = Query::new();
                    query.insert_key(key);
                    let pq = PathQuery::new(
                        path,
                        SizedQuery::new(query, None, None),
                    );

                    let _proof = db
                        .get_proved_path_query(&pq, None, None, grove_version)
                        .unwrap()
                        .expect("proof generation should succeed");
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// Full-leaf proof: range query over all items in a subtree
#[cfg(feature = "minimal")]
pub fn full_leaf_proof(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let (_dir, db, path, _first_key) = setup_db(1);

    let mut q = Query::new();
    q.insert_range(b"\x00".to_vec()..b"\xFF".to_vec());
    let pq = PathQuery::new(path, SizedQuery::new(q, None, None));

    c.bench_function("full_leaf_proof", |b| {
        b.iter(|| {
            let _proof = db
                .get_proved_path_query(&pq, None, None, grove_version)
                .unwrap()
                .expect("proof generation should succeed");
        });
    });
}

/// Proof verification benchmark
#[cfg(feature = "minimal")]
pub fn proof_verification(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let (_dir, db, path, key) = setup_db(3);

    // Generate a single-item proof once
    let mut query = Query::new();
    query.insert_key(key);
    let pq = PathQuery::new(path, SizedQuery::new(query, None, None));
    let proof = db
        .get_proved_path_query(&pq, None, None, grove_version)
        .unwrap()
        .expect("proof generation should succeed");

    c.bench_function("proof_verification", |b| {
        b.iter(|| {
            let _result = GroveDb::verify_query(&proof, &pq, grove_version)
                .expect("proof verification should succeed");
        });
    });
}

/// Construct a GroveDB with `depth` nested subtrees, populate the deepest
/// with `N_KEYS` items. Returns (TempDir, GroveDb, path, first_key).
///
/// TempDir must be kept alive for the GroveDb to remain valid.
#[cfg(feature = "minimal")]
fn setup_db(depth: usize) -> (TempDir, GroveDb, Vec<Vec<u8>>, Vec<u8>) {
    let grove_version = GroveVersion::latest();
    let dir = TempDir::new().expect("should create tmpdir");
    let db = GroveDb::open(dir.path()).expect("should open db");
    let mut rng = StdRng::seed_from_u64(42);

    let mut path: Vec<Vec<u8>> = Vec::with_capacity(depth);
    for _ in 0..depth {
        let node: [u8; 32] = rng.random();
        db.insert(
            path.as_slice(),
            &node,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");
        path.push(node.to_vec());
    }

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
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
    }

    (dir, db, path, first_key.expect("should have at least one key"))
}

#[cfg(feature = "minimal")]
criterion_group!(benches, single_item_proof, full_leaf_proof, proof_verification);
#[cfg(feature = "minimal")]
criterion_main!(benches);
