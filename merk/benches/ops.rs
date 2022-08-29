use criterion::{criterion_group, criterion_main, Criterion};
use merk::{owner::Owner, test_utils::*};

fn insert_1m_10k_seq_memonly(c: &mut Criterion) {
    let initial_size = 1_000_000;
    let batch_size = 10_000;
    let n_batches = initial_size / batch_size;

    let mut tree = Owner::new(make_tree_seq(initial_size));

    let mut batches = Vec::new();
    for i in 0..n_batches {
        batches.push(make_batch_seq((i * batch_size)..((i + 1) * batch_size)));
    }

    c.bench_function("insert_1m_10k_seq_memonly", |b| {
        let mut i = 0;

        b.iter(|| {
            let batch = &batches[i % n_batches as usize];
            tree.own(|tree| apply_memonly_unchecked(tree, batch));
            i += 1;
        });
    });
}

fn insert_1m_10k_rand_memonly(c: &mut Criterion) {
    let initial_size = 1_000_000;
    let batch_size = 10_000;
    let n_batches = initial_size / batch_size;

    let mut tree = Owner::new(make_tree_rand(initial_size, batch_size, 0));

    let mut batches = Vec::new();
    for i in 0..n_batches {
        batches.push(make_batch_rand(batch_size, i));
    }

    c.bench_function("insert_1m_10k_rand_memonly", |b| {
        let mut i = 0;

        b.iter(|| {
            let batch = &batches[i % n_batches as usize];
            tree.own(|tree| apply_memonly_unchecked(tree, batch));
            i += 1;
        });
    });
}

fn update_1m_10k_seq_memonly(c: &mut Criterion) {
    let initial_size = 1_000_000;
    let batch_size = 10_000;
    let n_batches = initial_size / batch_size;

    let mut tree = Owner::new(make_tree_seq(initial_size));

    let mut batches = Vec::new();
    for i in 0..n_batches {
        let batch = make_batch_seq((i * batch_size)..((i + 1) * batch_size));
        tree.own(|tree| apply_memonly_unchecked(tree, &batch));
        batches.push(batch);
    }

    c.bench_function("update_1m_10k_seq_memonly", |b| {
        let mut i = 0;

        b.iter(|| {
            let batch = &batches[i % n_batches as usize];
            tree.own(|tree| apply_memonly_unchecked(tree, batch));
            i += 1;
        });
    });
}

fn update_1m_10k_rand_memonly(c: &mut Criterion) {
    let initial_size = 1_000_000;
    let batch_size = 10_000;
    let n_batches = initial_size / batch_size;

    let mut tree = Owner::new(make_tree_rand(initial_size, batch_size, 0));

    let mut batches = Vec::new();
    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size, i);
        tree.own(|tree| apply_memonly_unchecked(tree, &batch));
        batches.push(batch);
    }

    c.bench_function("update_1m_10k_rand_memonly", |b| {
        let mut i = 0;

        b.iter(|| {
            let batch = &batches[i % n_batches as usize];
            tree.own(|tree| apply_memonly_unchecked(tree, batch));
            i += 1;
        });
    });
}

criterion_group!(
    benches,
    insert_1m_10k_seq_memonly,
    insert_1m_10k_rand_memonly,
    update_1m_10k_seq_memonly,
    update_1m_10k_rand_memonly
);
criterion_main!(benches);
