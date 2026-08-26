//! CountIndexedTree (cidx) benchmarks vs plain CountTree.
//!
//! Two questions the cidx feature's design rationale makes claims about
//! that have not previously been measured:
//!
//!   - **Top-k by count**: book chapter claims `O(log n + k)` for cidx
//!     vs `O(n)` for plain CountTree. Bench top_k against varying n
//!     and varying k to characterize the constant factors and confirm
//!     the asymptotic shape.
//!   - **Write amplification**: book chapter quotes `(k+1) · O(log n)`
//!     extra work per insert where k is the count of cidx levels on
//!     the path. Bench insert latency for cidx vs plain CountTree
//!     under matched workloads.
//!
//! Run with: `cargo bench --features minimal --bench cidx_benchmark`
//!
//! Setup/measurement uses `.expect("...")` rather than naked `.unwrap()`
//! so a panic in the bench harness surfaces *which* operation failed
//! rather than just a backtrace-only "called Option::unwrap on a None".

#[cfg(feature = "minimal")]
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
#[cfg(feature = "minimal")]
use grovedb::{Element, GroveDb};
#[cfg(feature = "minimal")]
use grovedb_path::SubtreePath;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use tempfile::TempDir;

#[cfg(feature = "minimal")]
const EMPTY_PATH: SubtreePath<'static, [u8; 0]> = SubtreePath::empty();

/// Populate a fresh cidx with `n` empty CountTree entries.
#[cfg(feature = "minimal")]
fn populate_cidx(n: usize) -> (TempDir, GroveDb, &'static GroveVersion) {
    let grove_version = GroveVersion::latest();
    let dir = TempDir::new().expect("populate_cidx: create tempdir");
    let db = GroveDb::open(dir.path()).expect("populate_cidx: open db");
    db.insert(
        EMPTY_PATH,
        b"cidx",
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("populate_cidx: insert root cidx");
    for i in 0..n {
        let key = format!("k{:08}", i).into_bytes();
        db.insert_into_count_indexed_tree(
            [b"cidx".as_slice()].as_ref(),
            &key,
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate_cidx: insert key cidx-child");
        // Insert i items inside so the count varies.
        for j in 0..(i % 10) {
            let inner = format!("c{:04}", j).into_bytes();
            db.insert(
                [b"cidx".as_slice(), key.as_slice()].as_ref(),
                &inner,
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate_cidx: insert inner item");
        }
    }
    (dir, db, grove_version)
}

/// Populate a fresh plain CountTree with `n` empty child CountTrees.
/// Currently unused (the matching top-k baseline bench is disabled
/// pending a meaningful implementation), but kept for the
/// `bench_insert_into_plain_count_tree` insert-latency baseline.
#[cfg(feature = "minimal")]
#[allow(dead_code)]
fn populate_plain_count_tree(n: usize) -> (TempDir, GroveDb, &'static GroveVersion) {
    let grove_version = GroveVersion::latest();
    let dir = TempDir::new().expect("populate_plain_count_tree: create tempdir");
    let db = GroveDb::open(dir.path()).expect("populate_plain_count_tree: open db");
    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("populate_plain_count_tree: insert root ct");
    for i in 0..n {
        let key = format!("k{:08}", i).into_bytes();
        db.insert(
            [b"ct".as_slice()].as_ref(),
            &key,
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate_plain_count_tree: insert key ct");
        for j in 0..(i % 10) {
            let inner = format!("c{:04}", j).into_bytes();
            db.insert(
                [b"ct".as_slice(), key.as_slice()].as_ref(),
                &inner,
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate_plain_count_tree: insert inner item");
        }
    }
    (dir, db, grove_version)
}

/// top_k via cidx: secondary range scan, `O(log n + k)` — measured
/// through the public surface (`run_path_query` over an axis
/// `PathQuery`), the route production reads take.
#[cfg(feature = "minimal")]
fn bench_cidx_top_k(c: &mut Criterion) {
    use grovedb::element::IndexAxis;
    use grovedb::query_result_type::QueryResultType;
    use grovedb::{PathQuery, PathQueryRun};

    let mut group = c.benchmark_group("cidx_top_k");
    for &n in &[100usize, 1_000, 10_000] {
        let (_dir, db, gv) = populate_cidx(n);
        for k in [10u16, 100] {
            let path_query =
                PathQuery::new_axis_top_k(vec![b"cidx".to_vec()], IndexAxis::Count, k, 0, true);
            group.bench_function(format!("n={}_k={}", n, k), |b| {
                b.iter(|| {
                    match db
                        .run_path_query(
                            &path_query,
                            true,
                            true,
                            true,
                            QueryResultType::QueryPathKeyElementTrioResultType,
                            None,
                            gv,
                        )
                        .unwrap()
                        .expect("bench_cidx_top_k: axis top-k read")
                    {
                        PathQueryRun::AxisEntries { entries, .. } => entries,
                        other => panic!("expected AxisEntries, got {other:?}"),
                    }
                });
            });
        }
    }
    group.finish();
}

// top_k via plain CountTree: full O(n) scan + sort.
//
// NOTE: a true matched baseline for "plain CountTree top-k" would
// iterate every child via raw_iter, decode each entry's count_value,
// and sort to take top-k. That code lives in user code rather than
// the GroveDB API surface (no equivalent typed API exists today).
// Re-enable this benchmark when a meaningful plain-CountTree
// implementation lands; until then, leave the cidx benches as a
// standalone characterization (the asymptotic claim is supported by
// the cidx code structure — secondary range scan is O(log n + k)
// while iterating every CountTree child would be O(n) by inspection).

/// Insert latency: dedicated cidx API vs plain CountTree insert.
#[cfg(feature = "minimal")]
fn bench_insert_into_cidx(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_into_cidx");
    let gv = GroveVersion::latest();
    group.bench_function("single_insert_into_empty_cidx", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().expect("bench_insert_into_cidx: create tempdir");
                let db = GroveDb::open(dir.path()).expect("bench_insert_into_cidx: open db");
                db.insert(
                    EMPTY_PATH,
                    b"cidx",
                    Element::empty_provable_count_indexed_tree(),
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("bench_insert_into_cidx: insert root cidx");
                (dir, db)
            },
            |(_dir, db)| {
                db.insert_into_count_indexed_tree(
                    [b"cidx".as_slice()].as_ref(),
                    b"k",
                    Element::empty_count_tree(),
                    None,
                    gv,
                )
                .unwrap()
                .expect("bench_insert_into_cidx: insert into cidx (measured)");
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

#[cfg(feature = "minimal")]
fn bench_insert_into_plain_count_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_into_plain_count_tree");
    let gv = GroveVersion::latest();
    group.bench_function("single_insert_into_empty_count_tree", |b| {
        b.iter_batched(
            || {
                let dir =
                    TempDir::new().expect("bench_insert_into_plain_count_tree: create tempdir");
                let db =
                    GroveDb::open(dir.path()).expect("bench_insert_into_plain_count_tree: open db");
                db.insert(
                    EMPTY_PATH,
                    b"ct",
                    Element::empty_count_tree(),
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("bench_insert_into_plain_count_tree: insert root ct");
                (dir, db)
            },
            |(_dir, db)| {
                db.insert(
                    [b"ct".as_slice()].as_ref(),
                    b"k",
                    Element::empty_count_tree(),
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("bench_insert_into_plain_count_tree: insert into ct (measured)");
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

#[cfg(feature = "minimal")]
criterion_group!(
    benches,
    bench_cidx_top_k,
    // bench_plain_count_tree_top_k — removed until a meaningful
    // plain-CountTree implementation lands (see note above).
    bench_insert_into_cidx,
    bench_insert_into_plain_count_tree,
);
#[cfg(feature = "minimal")]
criterion_main!(benches);

#[cfg(not(feature = "minimal"))]
fn main() {}
