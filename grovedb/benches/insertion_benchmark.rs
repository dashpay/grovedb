use criterion::{criterion_group, criterion_main, Criterion};
use grovedb::{Element, GroveDb};
use rand::Rng;
use tempfile::TempDir;

const N_ITEMS: usize = 10_000;

pub fn insertion_benchmark_without_transaction(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let test_leaf: &[u8] = b"leaf1";
    db.insert([], test_leaf, Element::empty_tree(), None)
        .unwrap();
    let keys = std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(N_ITEMS);

    c.bench_function("scalars insertion without transaction", |b| {
        b.iter(|| {
            for k in keys.clone() {
                db.insert([test_leaf], &k, Element::Item(k.to_vec()), None)
                    .unwrap();
            }
        })
    });
}

pub fn insertion_benchmark_with_transaction(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let test_leaf: &[u8] = b"leaf1";
    db.insert([], test_leaf, Element::empty_tree(), None)
        .unwrap();
    let keys = std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(N_ITEMS);

    c.bench_function("scalars insertion with transaction", |b| {
        b.iter(|| {
            let tx = db.start_transaction();
            for k in keys.clone() {
                db.insert([test_leaf], &k, Element::Item(k.to_vec()), Some(&tx))
                    .unwrap();
            }
            db.commit_transaction(tx).unwrap();
        })
    });
}

pub fn root_leaf_insertion_benchmark_without_transaction(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let keys = std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(10);

    c.bench_function("root leaves insertion without transaction", |b| {
        b.iter(|| {
            for k in keys.clone() {
                db.insert([], &k, Element::empty_tree(), None).unwrap();
            }
        })
    });
}

pub fn root_leaf_insertion_benchmark_with_transaction(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let keys = std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(10);

    c.bench_function("root leaves insertion with transaction", |b| {
        b.iter(|| {
            let tx = db.start_transaction();
            for k in keys.clone() {
                db.insert([], &k, Element::empty_tree(), Some(&tx)).unwrap();
            }
            db.commit_transaction(tx).unwrap();
        })
    });
}

pub fn deeply_nested_insertion_benchmark_without_transaction(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let mut nested_subtrees: Vec<[u8; 32]> = Vec::new();
    for s in std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(10) {
        db.insert(
            nested_subtrees.iter().map(|x| x.as_slice()),
            &s,
            Element::empty_tree(),
            None,
        )
        .unwrap();
        nested_subtrees.push(s);
    }

    let keys = std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(N_ITEMS);

    c.bench_function("deeply nested scalars insertion without transaction", |b| {
        b.iter(|| {
            for k in keys.clone() {
                db.insert(
                    nested_subtrees.iter().map(|x| x.as_slice()),
                    &k,
                    Element::Item(k.to_vec()),
                    None,
                )
                .unwrap();
            }
        })
    });
}

pub fn deeply_nested_insertion_benchmark_with_transaction(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let mut nested_subtrees: Vec<[u8; 32]> = Vec::new();
    for s in std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(10) {
        db.insert(
            nested_subtrees.iter().map(|x| x.as_slice()),
            &s,
            Element::empty_tree(),
            None,
        )
        .unwrap();
        nested_subtrees.push(s);
    }

    let keys = std::iter::repeat_with(|| rand::thread_rng().gen::<[u8; 32]>()).take(N_ITEMS);

    c.bench_function("deeply nested scalars insertion with transaction", |b| {
        b.iter(|| {
            let tx = db.start_transaction();
            for k in keys.clone() {
                db.insert(
                    nested_subtrees.iter().map(|x| x.as_slice()),
                    &k,
                    Element::Item(k.to_vec()),
                    Some(&tx),
                )
                .unwrap();
            }
            db.commit_transaction(tx).unwrap();
        })
    });
}

criterion_group!(
    benches,
    insertion_benchmark_without_transaction,
    insertion_benchmark_with_transaction,
    root_leaf_insertion_benchmark_without_transaction,
    root_leaf_insertion_benchmark_with_transaction,
    deeply_nested_insertion_benchmark_without_transaction,
    deeply_nested_insertion_benchmark_with_transaction,
);
criterion_main!(benches);
