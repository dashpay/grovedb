#[macro_use]
extern crate criterion;

use criterion::{BenchmarkId, Criterion};
use grovedb_merkle_mountain_range::{MMR, MMRStoreReadOps, MemStore, MmrNode};
use rand::{seq::SliceRandom, thread_rng};

/// Create an MmrNode leaf from an integer (for benchmarking).
fn leaf_from_u32(i: u32) -> MmrNode {
    MmrNode::leaf(i.to_le_bytes().to_vec())
}

fn prepare_mmr(count: u32) -> (u64, MemStore, Vec<u64>) {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let positions: Vec<u64> = (0u32..count)
        .map(|i| mmr.push(leaf_from_u32(i)).unwrap().expect("push"))
        .collect();
    let mmr_size = mmr.mmr_size();
    mmr.commit().unwrap().expect("write to store");
    (mmr_size, store, positions)
}

fn bench(c: &mut Criterion) {
    {
        let mut group = c.benchmark_group("MMR insertion");
        let inputs = [10_000, 100_000, 100_0000];
        for input in inputs.iter() {
            group.bench_with_input(BenchmarkId::new("times", input), &input, |b, &&size| {
                b.iter(|| prepare_mmr(size));
            });
        }
    }

    c.bench_function("MMR gen proof", |b| {
        let (mmr_size, store, positions) = prepare_mmr(100_0000);
        let mmr = MMR::new(mmr_size, &store);
        let mut rng = thread_rng();
        b.iter(|| {
            mmr.gen_proof(vec![*positions.choose(&mut rng).unwrap()])
                .unwrap()
        });
    });

    c.bench_function("MMR verify", |b| {
        let (mmr_size, store, positions) = prepare_mmr(100_0000);
        let mmr = MMR::new(mmr_size, &store);
        let mut rng = thread_rng();
        let root = mmr.get_root().unwrap().expect("get root");
        let proofs: Vec<_> = (0..10_000)
            .map(|_| {
                let pos = positions.choose(&mut rng).unwrap();
                let elem = (&store)
                    .element_at_position(*pos)
                    .unwrap()
                    .expect("read")
                    .expect("exists");
                let proof = mmr.gen_proof(vec![*pos]).unwrap().expect("gen proof");
                (pos, elem, proof)
            })
            .collect();
        b.iter(|| {
            let (pos, elem, proof) = proofs.choose(&mut rng).unwrap();
            proof
                .verify(root.clone(), vec![(**pos, elem.clone())])
                .expect("verify");
        });
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench
);
criterion_main!(benches);
