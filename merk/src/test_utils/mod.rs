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

//! Test utils

mod crash_merk;

mod temp_merk;

use std::{convert::TryInto, ops::Range};

use costs::storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval;
pub use crash_merk::CrashMerk;
use rand::prelude::*;
pub use temp_merk::TempMerk;

use crate::{
    tree::{kv::KV, BatchEntry, MerkBatch, NoopCommit, Op, PanicSource, Tree, Walker},
    TreeFeatureType::{BasicMerk, SummedMerk},
};

/// Assert tree invariants
pub fn assert_tree_invariants(tree: &Tree) {
    assert!(tree.balance_factor().abs() < 2);

    let maybe_left = tree.link(true);
    if let Some(left) = maybe_left {
        assert!(left.key() < tree.key());
        assert!(!left.is_modified());
    }

    let maybe_right = tree.link(false);
    if let Some(right) = maybe_right {
        assert!(right.key() > tree.key());
        assert!(!right.is_modified());
    }

    if let Some(left) = tree.child(true) {
        assert_tree_invariants(left);
    }
    if let Some(right) = tree.child(false) {
        assert_tree_invariants(right);
    }
}

/// Apply given batch to given tree and commit using memory only.
/// Used by `apply_memonly` which also performs checks using
/// `assert_tree_invariants`. Return Tree.
pub fn apply_memonly_unchecked(tree: Tree, batch: &MerkBatch<Vec<u8>>) -> Tree {
    let is_sum_node = tree.is_sum_node();
    let walker = Walker::<PanicSource>::new(tree, PanicSource {});
    let mut tree = Walker::<PanicSource>::apply_to(
        Some(walker),
        batch,
        PanicSource {},
        &|key, value| {
            Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                key.len() as u32,
                value.len() as u32,
                is_sum_node,
            ))
        },
        &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
            Ok((
                BasicStorageRemoval(key_bytes_to_remove),
                BasicStorageRemoval(value_bytes_to_remove),
            ))
        },
    )
    .unwrap()
    .expect("apply failed")
    .0
    .expect("expected tree");
    let is_sum_node = tree.is_sum_node();
    tree.commit(
        &mut NoopCommit {},
        &|key, value| {
            Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                key.len() as u32,
                value.len() as u32,
                is_sum_node,
            ))
        },
        &mut |_, _, _| Ok((false, None)),
        &mut |_, key_bytes_to_remove, value_bytes_to_remove| {
            Ok((
                BasicStorageRemoval(key_bytes_to_remove),
                BasicStorageRemoval(value_bytes_to_remove),
            ))
        },
    )
    .unwrap()
    .expect("commit failed");
    tree
}

/// Apply given batch to given tree and commit using memory only.
/// Perform checks using `assert_tree_invariants`. Return Tree.
pub fn apply_memonly(tree: Tree, batch: &MerkBatch<Vec<u8>>) -> Tree {
    let tree = apply_memonly_unchecked(tree, batch);
    assert_tree_invariants(&tree);
    tree
}

/// Applies given batch to given tree or creates a new tree to apply to and
/// commits to memory only.
pub fn apply_to_memonly(
    maybe_tree: Option<Tree>,
    batch: &MerkBatch<Vec<u8>>,
    is_sum_tree: bool,
) -> Option<Tree> {
    let maybe_walker = maybe_tree.map(|tree| Walker::<PanicSource>::new(tree, PanicSource {}));
    Walker::<PanicSource>::apply_to(
        maybe_walker,
        batch,
        PanicSource {},
        &|key, value| {
            Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                key.len() as u32,
                value.len() as u32,
                is_sum_tree,
            ))
        },
        &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
            Ok((
                BasicStorageRemoval(key_bytes_to_remove),
                BasicStorageRemoval(value_bytes_to_remove),
            ))
        },
    )
    .unwrap()
    .expect("apply failed")
    .0
    .map(|mut tree| {
        let is_sum_node = tree.is_sum_node();
        tree.commit(
            &mut NoopCommit {},
            &|key, value| {
                Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key.len() as u32,
                    value.len() as u32,
                    is_sum_node,
                ))
            },
            &mut |_, _, _| Ok((false, None)),
            &mut |_, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
        .unwrap()
        .expect("commit failed");
        println!("{:?}", &tree);
        assert_tree_invariants(&tree);
        tree
    })
}

/// Format key to bytes
pub const fn seq_key(n: u64) -> [u8; 8] {
    n.to_be_bytes()
}

/// Create batch entry with Put op using key n and a fixed value
pub fn put_entry(n: u64) -> BatchEntry<Vec<u8>> {
    (seq_key(n).to_vec(), Op::Put(vec![123; 60], BasicMerk))
}

/// Create batch entry with Delete op using key n
pub fn del_entry(n: u64) -> BatchEntry<Vec<u8>> {
    (seq_key(n).to_vec(), Op::Delete)
}

/// Create a batch of Put ops using given sequential range as keys and fixed
/// values
pub fn make_batch_seq(range: Range<u64>) -> Vec<BatchEntry<Vec<u8>>> {
    let mut batch = Vec::with_capacity((range.end - range.start).try_into().unwrap());
    for n in range {
        batch.push(put_entry(n));
    }
    batch
}

/// Create a batch of Delete ops using given sequential range as keys
pub fn make_del_batch_seq(range: Range<u64>) -> Vec<BatchEntry<Vec<u8>>> {
    let mut batch = Vec::with_capacity((range.end - range.start).try_into().unwrap());
    for n in range {
        batch.push(del_entry(n));
    }
    batch
}

/// Create a batch of Put ops using fixed values and random numbers as keys
pub fn make_batch_rand(size: u64, seed: u64) -> Vec<BatchEntry<Vec<u8>>> {
    let mut rng: SmallRng = SeedableRng::seed_from_u64(seed);
    let mut batch = Vec::with_capacity(size.try_into().unwrap());
    for _ in 0..size {
        let n = rng.gen::<u64>();
        batch.push(put_entry(n));
    }
    batch.sort_by(|a, b| a.0.cmp(&b.0));
    batch
}

/// Create a batch of Delete ops using random numbers as keys
pub fn make_del_batch_rand(size: u64, seed: u64) -> Vec<BatchEntry<Vec<u8>>> {
    let mut rng: SmallRng = SeedableRng::seed_from_u64(seed);
    let mut batch = Vec::with_capacity(size.try_into().unwrap());
    for _ in 0..size {
        let n = rng.gen::<u64>();
        batch.push(del_entry(n));
    }
    batch.sort_by(|a, b| a.0.cmp(&b.0));
    batch
}

/// Create tree with initial fixed values and apply `node count` Put ops with
/// random keys using memory only
pub fn make_tree_rand(
    node_count: u64,
    batch_size: u64,
    initial_seed: u64,
    is_sum_tree: bool,
) -> Tree {
    assert!(node_count >= batch_size);
    assert_eq!((node_count % batch_size), 0);

    let value = vec![123; 60];
    let feature_type = if is_sum_tree {
        SummedMerk(0)
    } else {
        BasicMerk
    };
    let mut tree = Tree::new(vec![0; 20], value, None, feature_type).unwrap();

    let mut seed = initial_seed;

    let batch_count = node_count / batch_size;
    for _ in 0..batch_count {
        let batch = make_batch_rand(batch_size, seed);
        tree = apply_memonly(tree, &batch);
        seed += 1;
    }

    tree
}

/// Create tree with initial fixed values and apply `node count` Put ops using
/// sequential keys using memory only
pub fn make_tree_seq(node_count: u64) -> Tree {
    let batch_size = if node_count >= 10_000 {
        assert_eq!(node_count % 10_000, 0);
        10_000
    } else {
        node_count
    };

    let value = vec![123; 60];
    let mut tree = Tree::new(vec![0; 20], value, None, BasicMerk).unwrap();

    let batch_count = node_count / batch_size;
    for i in 0..batch_count {
        let batch = make_batch_seq((i * batch_size)..((i + 1) * batch_size));
        tree = apply_memonly(tree, &batch);
    }

    tree
}
