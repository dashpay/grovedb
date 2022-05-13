use criterion::{criterion_group, criterion_main, Criterion};
use grovedb::{Element, GroveDb, GroveDbOp};
use rand::Rng;
use tempfile::TempDir;

const N_ITEMS: usize = 10_000;

pub fn shallow_insertion(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let test_leaf: &[u8] = b"leaf1";
    let item = Element::Item(b"value".to_vec());
    db.insert([], test_leaf, Element::empty_tree(), None)
        .unwrap();
    c.bench_function("shallow insertion without batch", |b| {
        b.iter(|| {
            db.insert([test_leaf], b"key", item.clone(), None).unwrap();
        })
    });
}

pub fn shallow_insertion_batch(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let test_leaf: &[u8] = b"leaf1";
    let item = Element::Item(b"value".to_vec());
    db.insert([], test_leaf, Element::empty_tree(), None)
        .unwrap();
    c.bench_function("shallow insertion with batch", |b| {
        b.iter(|| {
            let batch = vec![GroveDbOp::insert(
                vec![test_leaf.to_vec()],
                b"key".to_vec(),
                item.clone(),
            )];
            db.apply_batch(batch, None).unwrap()
        })
    });
}

pub fn deep_insertion(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let full_path = vec![
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
    ];

    let mut acc_path: Vec<Vec<u8>> = vec![];
    for p in full_path.into_iter() {
        db.insert(
            acc_path.iter().map(|x| x.as_slice()),
            &p,
            Element::empty_tree(),
            None,
        )
        .unwrap();
        acc_path.push(p);
    }

    let item = Element::Item(b"value".to_vec());

    c.bench_function("deep insertion without batch", |b| {
        b.iter(|| {
            db.insert(
                acc_path.iter().map(|x| x.as_slice()),
                b"key",
                item.clone(),
                None,
            )
            .unwrap();
        })
    });
}

pub fn deep_insertion_full_path(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let full_path = vec![
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
    ];

    c.bench_function("deep insertion full path without batch", |b| {
        b.iter(|| {
            let mut acc_path: Vec<Vec<u8>> = vec![];
            for p in full_path.clone().into_iter() {
                db.insert(
                    acc_path.iter().map(|x| x.as_slice()),
                    &p,
                    Element::empty_tree(),
                    None,
                )
                .unwrap();
                acc_path.push(p);
            }

            let item = Element::Item(b"value".to_vec());

            db.insert(
                acc_path.iter().map(|x| x.as_slice()),
                b"key",
                item.clone(),
                None,
            )
            .unwrap();
        })
    });
}

pub fn deep_insertion_batch(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let full_path = vec![
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
    ];

    let mut acc_path: Vec<Vec<u8>> = vec![];
    for p in full_path.into_iter() {
        db.insert(
            acc_path.iter().map(|x| x.as_slice()),
            &p,
            Element::empty_tree(),
            None,
        )
        .unwrap();
        acc_path.push(p);
    }

    let item = Element::Item(b"value".to_vec());

    c.bench_function("deep insertion with batch", |b| {
        b.iter(|| {
            let batch = vec![GroveDbOp::insert(
                acc_path.clone(),
                b"key".to_vec(),
                item.clone(),
            )];
            db.apply_batch(batch, None).unwrap()
        })
    });
}

pub fn deep_insertion_batch_full_path(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    let full_path = vec![
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
    ];

    let item = Element::Item(b"value".to_vec());

    c.bench_function("deep insertion full path with batch", |b| {
        b.iter(|| {
            let mut batch_arg = vec![];
            let mut acc_path: Vec<Vec<u8>> = vec![];
            for p in full_path.clone().into_iter() {
                batch_arg.push((acc_path.clone(), p.clone()));
                acc_path.push(p);
            }
            let mut batch: Vec<_> = batch_arg
                .into_iter()
                .map(|(path, key)| GroveDbOp::insert(path, key, Element::empty_tree()))
                .collect();
            batch.push(GroveDbOp::insert(acc_path, b"key".to_vec(), item.clone()));
            db.apply_batch(batch, None).unwrap()
        })
    });
}

criterion_group!(
    benches,
    // shallow_insertion,
    // shallow_insertion_batch,
    // deep_insertion,
    // deep_insertion_batch,
    deep_insertion_full_path,
    deep_insertion_batch_full_path,
);
criterion_main!(benches);
