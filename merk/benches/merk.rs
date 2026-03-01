// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Merk benches

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use grovedb_costs::storage_cost::{removal::StorageRemovedBytes::BasicStorageRemoval, StorageCost};
use grovedb_merk::{
    proofs,
    test_utils::{make_batch_rand, make_batch_seq, make_del_batch_rand, TempMerk},
    tree::{kv::ValueDefinedCostType, MerkBatch},
    tree_type::TreeType,
    Merk, Restorer,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage};
use grovedb_version::version::GroveVersion;
use rand::prelude::*;

fn apply_batch_default<KB: AsRef<[u8]>>(
    merk: &mut TempMerk,
    batch: &MerkBatch<KB>,
    grove_version: &GroveVersion,
) {
    merk.apply_unchecked::<KB, Vec<u8>, _, _, _, _, _>(
        batch,
        &[],
        None,
        &|_k, _v| Ok(0),
        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
        &|_old_value, _new_value| Ok(None),
        &mut |_costs: &StorageCost, _old_value: &Vec<u8>, _new_value: &mut Vec<u8>| {
            Ok((false, None))
        },
        &mut |_key: &Vec<u8>, key_bytes_to_remove: u32, value_bytes_to_remove: u32| {
            Ok((
                BasicStorageRemoval(key_bytes_to_remove),
                BasicStorageRemoval(value_bytes_to_remove),
            ))
        },
        grove_version,
    )
    .unwrap()
    .expect("apply failed");
}

/// 1 million gets in 2k batches
pub fn get(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;
    let num_batches = initial_size / batch_size;

    let mut merk = TempMerk::new(grove_version);

    let mut batches = vec![];
    for i in 0..num_batches {
        let batch = make_batch_rand(batch_size, i);
        apply_batch_default(&mut merk, &batch, grove_version);
        batches.push(batch);
    }

    c.bench_function("get", |b| {
        let mut i = 0;

        b.iter(|| {
            let batch_index = (i % num_batches) as usize;
            let key_index = (i / num_batches) as usize;

            let key = &batches[batch_index][key_index].0;
            merk.get(
                key,
                true,
                None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
            .unwrap()
            .expect("get failed");

            i = (i + 1) % initial_size;
        })
    });
}

/// 1 million sequential inserts in 2k batches
pub fn insert_1m_2k_seq(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;
    let mut batches = Vec::with_capacity(n_batches);

    for i in 0..n_batches {
        let batch = make_batch_seq(((i * batch_size) as u64)..((i + 1) * batch_size) as u64);
        batches.push(batch);
    }

    c.bench_function("insert_1m_2k_seq", |b| {
        let mut merk = TempMerk::new(grove_version);
        let mut i = 0;

        b.iter_with_large_drop(|| {
            let batch = &batches[i % n_batches];
            apply_batch_default(&mut merk, batch, grove_version);
            i += 1;
        });
    });
}

/// 1 million random inserts in 2k batches
pub fn insert_1m_2k_rand(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;
    let mut batches = Vec::with_capacity(n_batches);

    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size as u64, i as u64);
        batches.push(batch);
    }

    c.bench_function("insert_1m_2k_rand", |b| {
        let mut merk = TempMerk::new(grove_version);
        let mut i = 0;

        b.iter_with_large_drop(|| {
            let batch = &batches[i % n_batches];
            apply_batch_default(&mut merk, batch, grove_version);
            i += 1;
        });
    });
}

/// 1 million sequential updates in 2k batches
pub fn update_1m_2k_seq(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;
    let mut batches = Vec::with_capacity(n_batches);

    let mut merk = TempMerk::new(grove_version);

    for i in 0..n_batches {
        let batch = make_batch_seq(((i * batch_size) as u64)..((i + 1) * batch_size) as u64);
        apply_batch_default(&mut merk, &batch, grove_version);

        batches.push(batch);
    }

    c.bench_function("update_1m_2k_seq", |b| {
        let mut i = 0;

        b.iter_with_large_drop(|| {
            let batch = &batches[i % n_batches];
            apply_batch_default(&mut merk, batch, grove_version);
            i += 1;
        });
    });
}

/// 1 million random updates in 2k batches
pub fn update_1m_2k_rand(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;
    let mut batches = Vec::with_capacity(n_batches);

    let mut merk = TempMerk::new(grove_version);

    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size as u64, i as u64);
        apply_batch_default(&mut merk, &batch, grove_version);

        batches.push(batch);
    }

    c.bench_function("update_1m_2k_rand", |b| {
        let mut i = 0;

        b.iter_with_large_drop(|| {
            let batch = &batches[i % n_batches];
            apply_batch_default(&mut merk, batch, grove_version);
            i += 1;
        });
    });
}

/// 1 million random deletes in 2k batches
pub fn delete_1m_2k_rand(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;
    let mut batches = Vec::with_capacity(n_batches);
    let mut delete_batches = Vec::with_capacity(n_batches);

    let mut merk = TempMerk::new(grove_version);

    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size as u64, i as u64);
        let delete_batch = make_del_batch_rand(batch_size as u64, i as u64);
        apply_batch_default(&mut merk, &batch, grove_version);

        batches.push(batch);
        delete_batches.push(delete_batch);
    }

    c.bench_function("delete_1m_2k_rand", |b| {
        let mut i = 0;

        let delete_batch = &delete_batches[i % n_batches];
        let insert_batch = &batches[i % n_batches];

        // Merk tree is kept with 1m elements before each bench iteration for more or
        // less same inputs.
        apply_batch_default(&mut merk, insert_batch, grove_version);

        b.iter_with_large_drop(|| {
            apply_batch_default(&mut merk, delete_batch, grove_version);
            i += 1;
        });
    });
}

/// 1 million random proofs in 2k batches
pub fn prove_1m_2k_rand(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;
    let mut batches = Vec::with_capacity(n_batches);
    let mut prove_keys_per_batch = Vec::with_capacity(n_batches);

    let mut merk = TempMerk::new(grove_version);

    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size as u64, i as u64);
        apply_batch_default(&mut merk, &batch, grove_version);
        let mut prove_keys = Vec::with_capacity(batch_size);
        for (key, _) in batch.iter() {
            prove_keys.push(proofs::query::query_item::QueryItem::Key(key.clone()));
        }
        prove_keys_per_batch.push(prove_keys);
        batches.push(batch);
    }

    c.bench_function("prove_1m_2k_rand", |b| {
        let mut i = 0;

        b.iter_with_large_drop(|| {
            let keys = prove_keys_per_batch[i % n_batches].clone();

            merk.prove_unchecked(keys, None, true, grove_version)
                .unwrap()
                .expect("prove failed");
            i += 1;
        });
    });
}

/// Build 1 million trunk chunks in 2k batches, random
pub fn build_trunk_chunk_1m_2k_rand(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;

    let mut merk = TempMerk::new(grove_version);

    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size as u64, i as u64);
        apply_batch_default(&mut merk, &batch, grove_version);
    }

    c.bench_function("build_trunk_chunk_1m_2k_rand", |b| {
        let mut bytes = Vec::new();

        b.iter(|| {
            bytes.clear();

            let (chunk, _) = merk
                .chunks()
                .unwrap()
                .chunk_with_index(1, grove_version)
                .unwrap();
            proofs::encode_into(chunk.iter(), &mut bytes);
        });
    });
}

/// Chunk producer random 1 million
pub fn chunkproducer_rand_1m_1_rand(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;

    let mut merk = TempMerk::new(grove_version);

    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size as u64, i as u64);
        apply_batch_default(&mut merk, &batch, grove_version);
    }

    let mut rng = rand::thread_rng();
    let chunk_count = merk.chunks().unwrap().len();

    c.bench_function("chunkproducer_rand_1m_1_rand", |b| {
        b.iter_with_large_drop(|| {
            let index = rng.gen_range(1..=chunk_count);
            let _chunk = merk
                .chunks()
                .unwrap()
                .chunk_with_index(index, grove_version)
                .unwrap();
        });
    });
}

/// Chunk iter 1 million
pub fn chunk_iter_1m_1(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let initial_size = 1_000_000;
    let batch_size = 2_000;

    let n_batches: usize = initial_size / batch_size;

    let mut merk = TempMerk::new(grove_version);

    for i in 0..n_batches {
        let batch = make_batch_rand(batch_size as u64, i as u64);
        apply_batch_default(&mut merk, &batch, grove_version);
    }

    let mut chunk_producer = merk.chunks().unwrap();

    c.bench_function("chunk_iter_1m_1", |b| {
        b.iter_with_large_drop(|| match chunk_producer.next(grove_version) {
            Some(chunk) => {
                let _ = chunk.unwrap();
            }
            None => {
                chunk_producer = merk.chunks().unwrap();
                let _ = chunk_producer.next(grove_version).unwrap().unwrap();
            }
        });
    });
}

/// Restore merk of size 500
pub fn restore_500_1(c: &mut Criterion) {
    let grove_version = GroveVersion::latest();
    let merk_size = 500;

    let mut merk = TempMerk::new(grove_version);

    let batch = make_batch_rand(merk_size as u64, 0_u64);
    apply_batch_default(&mut merk, &batch, grove_version);

    let root_hash = merk.root_hash().unwrap();

    c.bench_function("restore_500_1", |b| {
        b.iter_batched(
            TempStorage::new,
            |storage| {
                let tx = storage.start_transaction();
                let ctx = storage
                    .get_immediate_storage_context(SubtreePath::empty(), &tx)
                    .unwrap();
                let m = Merk::open_standalone(
                    ctx,
                    TreeType::NormalTree,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version,
                )
                .unwrap()
                .unwrap();
                let mut restorer = Restorer::new(m, root_hash, None);
                let mut chunk_producer = merk.chunks().unwrap();
                let mut chunk_id: Option<Vec<u8>> = Some(Vec::new());

                while let Some(current_id) = chunk_id.take() {
                    let (chunk, next_id) = chunk_producer
                        .chunk(current_id.as_slice(), grove_version)
                        .unwrap();
                    restorer
                        .process_chunk(current_id.as_slice(), chunk, grove_version)
                        .unwrap();
                    chunk_id = next_id;
                }
                storage.commit_transaction(tx).unwrap().unwrap();
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    benches,
    get,
    insert_1m_2k_seq,
    insert_1m_2k_rand,
    update_1m_2k_seq,
    update_1m_2k_rand,
    delete_1m_2k_rand,
    prove_1m_2k_rand,
    build_trunk_chunk_1m_2k_rand,
    chunkproducer_rand_1m_1_rand,
    chunk_iter_1m_1,
    restore_500_1,
);
criterion_main!(benches);
