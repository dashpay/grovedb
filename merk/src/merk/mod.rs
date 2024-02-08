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

//! Merk

pub mod chunks;

pub(crate) mod defaults;

pub mod options;

pub mod apply;
pub mod clear;
pub mod committer;
pub mod get;
pub mod open;
pub mod prove;
pub mod restore;
pub mod source;

use std::{
    cell::Cell,
    collections::{BTreeSet, LinkedList},
    fmt,
};

use committer::MerkCommitter;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_no_add,
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithValue, CostContext,
    CostResult, CostsExt, FeatureSumLength, OperationCost,
};
use grovedb_storage::{self, Batch, RawIterator, StorageContext};
use source::MerkSource;

use crate::{
    error::Error,
    merk::{defaults::ROOT_KEY_KEY, options::MerkOptions},
    proofs::{query::query_item::QueryItem, Query},
    tree::{
        kv::ValueDefinedCostType, AuxMerkBatch, CryptoHash, Op, RefWalker, TreeNode, NULL_HASH,
    },
    Error::{CostsError, EdError, StorageError},
    MerkType::{BaseMerk, LayeredMerk, StandaloneMerk},
};

/// Key update types
pub struct KeyUpdates {
    pub new_keys: BTreeSet<Vec<u8>>,
    pub updated_keys: BTreeSet<Vec<u8>>,
    pub deleted_keys: LinkedList<(Vec<u8>, KeyValueStorageCost)>,
    pub updated_root_key_from: Option<Vec<u8>>,
}

impl KeyUpdates {
    /// New KeyUpdate
    pub fn new(
        new_keys: BTreeSet<Vec<u8>>,
        updated_keys: BTreeSet<Vec<u8>>,
        deleted_keys: LinkedList<(Vec<u8>, KeyValueStorageCost)>,
        updated_root_key_from: Option<Vec<u8>>,
    ) -> Self {
        Self {
            new_keys,
            updated_keys,
            deleted_keys,
            updated_root_key_from,
        }
    }
}

/// Type alias for simple function signature
pub type BatchValue = (
    Vec<u8>,
    Option<FeatureSumLength>,
    ChildrenSizesWithValue,
    KeyValueStorageCost,
);

/// A bool type
pub type IsSumTree = bool;

/// Root hash key and sum
pub type RootHashKeyAndSum = (CryptoHash, Option<Vec<u8>>, Option<i64>);

/// KVIterator allows you to lazily iterate over each kv pair of a subtree
pub struct KVIterator<'a, I: RawIterator> {
    raw_iter: I,
    _query: &'a Query,
    left_to_right: bool,
    query_iterator: Box<dyn Iterator<Item = &'a QueryItem> + 'a>,
    current_query_item: Option<&'a QueryItem>,
}

impl<'a, I: RawIterator> KVIterator<'a, I> {
    /// New iterator
    pub fn new(raw_iter: I, query: &'a Query) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        let mut iterator = KVIterator {
            raw_iter,
            _query: query,
            left_to_right: query.left_to_right,
            current_query_item: None,
            query_iterator: query.directional_iter(query.left_to_right),
        };
        iterator.seek().unwrap_add_cost(&mut cost);
        iterator.wrap_with_cost(cost)
    }

    /// Returns the current node the iter points to if it's valid for the given
    /// query item returns None otherwise
    fn get_kv(&mut self, query_item: &QueryItem) -> CostContext<Option<(Vec<u8>, Vec<u8>)>> {
        let mut cost = OperationCost::default();

        if query_item
            .iter_is_valid_for_type(&self.raw_iter, None, self.left_to_right)
            .unwrap_add_cost(&mut cost)
        {
            let kv = (
                self.raw_iter
                    .key()
                    .unwrap_add_cost(&mut cost)
                    .expect("key must exist as iter is valid")
                    .to_vec(),
                self.raw_iter
                    .value()
                    .unwrap_add_cost(&mut cost)
                    .expect("value must exists as iter is valid")
                    .to_vec(),
            );
            if self.left_to_right {
                self.raw_iter.next().unwrap_add_cost(&mut cost)
            } else {
                self.raw_iter.prev().unwrap_add_cost(&mut cost)
            }
            Some(kv).wrap_with_cost(cost)
        } else {
            None.wrap_with_cost(cost)
        }
    }

    /// Moves the iter to the start of the next query item
    fn seek(&mut self) -> CostContext<()> {
        let mut cost = OperationCost::default();

        self.current_query_item = self.query_iterator.next();
        if let Some(query_item) = self.current_query_item {
            query_item
                .seek_for_iter(&mut self.raw_iter, self.left_to_right)
                .unwrap_add_cost(&mut cost);
        }

        ().wrap_with_cost(cost)
    }
}

// Cannot be an Iterator as it should return cost
impl<'a, I: RawIterator> KVIterator<'a, I> {
    /// Next key-value
    pub fn next_kv(&mut self) -> CostContext<Option<(Vec<u8>, Vec<u8>)>> {
        let mut cost = OperationCost::default();

        if let Some(query_item) = self.current_query_item {
            let kv_pair = self.get_kv(query_item).unwrap_add_cost(&mut cost);

            if kv_pair.is_some() {
                kv_pair.wrap_with_cost(cost)
            } else {
                self.seek().unwrap_add_cost(&mut cost);
                self.next_kv().add_cost(cost)
            }
        } else {
            None.wrap_with_cost(cost)
        }
    }
}

#[derive(PartialEq, Eq)]
/// Merk types
pub enum MerkType {
    /// A StandaloneMerk has it's root key storage on a field and pays for root
    /// key updates
    StandaloneMerk,
    /// A BaseMerk has it's root key storage on a field but does not pay for
    /// when these keys change
    BaseMerk,
    /// A LayeredMerk has it's root key storage inside a parent merk
    LayeredMerk,
}

impl MerkType {
    /// Returns bool
    pub(crate) fn requires_root_storage_update(&self) -> bool {
        match self {
            StandaloneMerk => true,
            BaseMerk => true,
            LayeredMerk => false,
        }
    }
}

/// A handle to a Merkle key/value store backed by RocksDB.
pub struct Merk<S> {
    pub(crate) tree: Cell<Option<TreeNode>>,
    pub(crate) root_tree_key: Cell<Option<Vec<u8>>>,
    /// Storage
    pub storage: S,
    /// Merk type
    pub merk_type: MerkType,
    /// Is sum tree?
    pub is_sum_tree: bool,
}

impl<S> fmt::Debug for Merk<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Merk").finish()
    }
}

// key, maybe value, maybe child reference hooks, maybe key value storage costs
pub type UseTreeMutResult = CostResult<
    Vec<(
        Vec<u8>,
        Option<FeatureSumLength>,
        ChildrenSizesWithValue,
        KeyValueStorageCost,
    )>,
    Error,
>;

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Returns the root hash of the tree (a digest for the entire store which
    /// proofs can be checked against). If the tree is empty, returns the null
    /// hash (zero-filled).
    pub fn root_hash(&self) -> CostContext<CryptoHash> {
        self.use_tree(|tree| {
            tree.map_or(NULL_HASH.wrap_with_cost(Default::default()), |tree| {
                tree.hash()
            })
        })
    }

    /// Returns the total sum value in the Merk tree
    pub fn sum(&self) -> Result<Option<i64>, Error> {
        self.use_tree(|tree| match tree {
            None => Ok(None),
            Some(tree) => tree.sum(),
        })
    }

    /// Returns the root non-prefixed key of the tree. If the tree is empty,
    /// None.
    pub fn root_key(&self) -> Option<Vec<u8>> {
        self.use_tree(|tree| tree.map(|tree| tree.key().to_vec()))
    }

    /// Returns the root hash and non-prefixed key of the tree.
    pub fn root_hash_key_and_sum(&self) -> CostResult<RootHashKeyAndSum, Error> {
        self.use_tree(|tree| match tree {
            None => Ok((NULL_HASH, None, None)).wrap_with_cost(Default::default()),
            Some(tree) => {
                let sum = cost_return_on_error_default!(tree.sum());
                tree.hash()
                    .map(|hash| Ok((hash, Some(tree.key().to_vec()), sum)))
            }
        })
    }

    /// Commit tree changes
    pub fn commit<K>(
        &mut self,
        key_updates: KeyUpdates,
        aux: &AuxMerkBatch<K>,
        options: Option<MerkOptions>,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
    ) -> CostResult<(), Error>
    where
        K: AsRef<[u8]>,
    {
        let mut cost = OperationCost::default();
        let options = options.unwrap_or_default();
        let mut batch = self.storage.new_batch();
        let to_batch_wrapped = self.use_tree_mut(|maybe_tree| -> UseTreeMutResult {
            // TODO: concurrent commit
            let mut inner_cost = OperationCost::default();

            if let Some(tree) = maybe_tree {
                // TODO: configurable committer
                let mut committer = MerkCommitter::new(tree.height(), 100);
                cost_return_on_error!(
                    &mut inner_cost,
                    tree.commit(&mut committer, old_specialized_cost)
                );

                let tree_key = tree.key();
                // if they are a base merk we should update the root key
                if self.merk_type.requires_root_storage_update() {
                    // there are two situation where we want to put the root key
                    // it was updated from something else
                    // or it is part of new keys
                    if key_updates.updated_root_key_from.is_some()
                        || key_updates.new_keys.contains(tree_key)
                    {
                        let costs = if self.merk_type == StandaloneMerk {
                            // if we are a standalone merk we want real costs
                            Some(KeyValueStorageCost::for_updated_root_cost(
                                key_updates
                                    .updated_root_key_from
                                    .as_ref()
                                    .map(|k| k.len() as u32),
                                tree_key.len() as u32,
                            ))
                        } else {
                            // if we are a base merk we estimate these costs are free
                            // This None does not guarantee they are free though
                            None
                        };

                        // update pointer to root node
                        cost_return_on_error_no_add!(
                            &inner_cost,
                            batch
                                .put_root(ROOT_KEY_KEY, tree_key, costs)
                                .map_err(CostsError)
                        );
                    }
                }

                Ok(committer.batch)
            } else {
                if self.merk_type.requires_root_storage_update() {
                    // empty tree, delete pointer to root
                    let cost = if options.base_root_storage_is_free {
                        Some(KeyValueStorageCost::default()) // don't pay for
                                                             // root costs
                    } else {
                        None // means it will be calculated
                    };
                    batch.delete_root(ROOT_KEY_KEY, cost);
                }

                Ok(vec![])
            }
            .wrap_with_cost(inner_cost)
        });

        let mut to_batch = cost_return_on_error!(&mut cost, to_batch_wrapped);

        // TODO: move this to MerkCommitter impl?
        for (key, maybe_cost) in key_updates.deleted_keys {
            to_batch.push((key, None, None, maybe_cost));
        }
        to_batch.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, maybe_sum_tree_cost, maybe_value, storage_cost) in to_batch {
            if let Some((value, left_size, right_size)) = maybe_value {
                cost_return_on_error_no_add!(
                    &cost,
                    batch
                        .put(
                            &key,
                            &value,
                            Some((maybe_sum_tree_cost, left_size, right_size)),
                            Some(storage_cost)
                        )
                        .map_err(CostsError)
                );
            } else {
                batch.delete(&key, Some(storage_cost));
            }
        }

        for (key, value, storage_cost) in aux {
            match value {
                Op::Put(value, ..) => cost_return_on_error_no_add!(
                    &cost,
                    batch
                        .put_aux(key, value, storage_cost.clone())
                        .map_err(CostsError)
                ),
                Op::Delete => batch.delete_aux(key, storage_cost.clone()),
                _ => {
                    cost_return_on_error_no_add!(
                        &cost,
                        Err(Error::InvalidOperation(
                            "only put and delete allowed for aux storage"
                        ))
                    );
                }
            };
        }

        // write to db
        self.storage
            .commit_batch(batch)
            .map_err(StorageError)
            .add_cost(cost)
    }

    /// Walk
    pub fn walk<'s, T>(&'s self, f: impl FnOnce(Option<RefWalker<MerkSource<'s, S>>>) -> T) -> T {
        let mut tree = self.tree.take();
        let maybe_walker = tree
            .as_mut()
            .map(|tree| RefWalker::new(tree, self.source()));
        let res = f(maybe_walker);
        self.tree.set(tree);
        res
    }

    /// Checks if it's an empty tree
    pub fn is_empty_tree(&self) -> CostContext<bool> {
        let mut iter = self.storage.raw_iter();
        iter.seek_to_first().flat_map(|_| iter.valid().map(|x| !x))
    }

    /// Checks if it's an empty tree excluding exceptions
    pub fn is_empty_tree_except(&self, mut except_keys: BTreeSet<&[u8]>) -> CostContext<bool> {
        let mut cost = OperationCost::default();

        let mut iter = self.storage.raw_iter();
        iter.seek_to_first().unwrap_add_cost(&mut cost);
        while let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
            if except_keys.take(key).is_none() {
                return false.wrap_with_cost(cost);
            }
            iter.next().unwrap_add_cost(&mut cost)
        }
        true.wrap_with_cost(cost)
    }

    /// Use tree
    pub(crate) fn use_tree<T>(&self, f: impl FnOnce(Option<&TreeNode>) -> T) -> T {
        let tree = self.tree.take();
        let res = f(tree.as_ref());
        self.tree.set(tree);
        res
    }

    fn use_tree_mut<T>(&self, mut f: impl FnMut(Option<&mut TreeNode>) -> T) -> T {
        let mut tree = self.tree.take();
        let res = f(tree.as_mut());
        self.tree.set(tree);
        res
    }

    /// Sets the tree's top node (base) key
    /// The base root key should only be used if the Merk tree is independent
    /// Meaning that it doesn't have a parent Merk
    pub fn set_base_root_key(&mut self, key: Option<Vec<u8>>) -> CostResult<(), Error> {
        if let Some(key) = key {
            self.storage
                .put_root(ROOT_KEY_KEY, key.as_slice(), None)
                .map_err(Error::StorageError) // todo: maybe
                                              // change None?
        } else {
            self.storage
                .delete_root(ROOT_KEY_KEY, None)
                .map_err(Error::StorageError) // todo: maybe
                                              // change None?
        }
    }

    /// Loads the Merk from the base root key
    /// The base root key should only be used if the Merk tree is independent
    /// Meaning that it doesn't have a parent Merk
    pub(crate) fn load_base_root(
        &mut self,
        value_defined_cost_fn: Option<impl Fn(&[u8]) -> Option<ValueDefinedCostType>>,
    ) -> CostResult<(), Error> {
        self.storage
            .get_root(ROOT_KEY_KEY)
            .map(|root_result| root_result.map_err(Error::StorageError))
            .flat_map_ok(|tree_root_key_opt| {
                // In case of successful seek for root key check if it exists
                if let Some(tree_root_key) = tree_root_key_opt {
                    // Trying to build a tree out of it, costs will be accumulated because
                    // `Tree::get` returns `CostContext` and this call happens inside `flat_map_ok`.
                    TreeNode::get(&self.storage, tree_root_key, value_defined_cost_fn).map_ok(
                        |tree| {
                            if let Some(t) = tree.as_ref() {
                                self.root_tree_key = Cell::new(Some(t.key().to_vec()));
                            }
                            self.tree = Cell::new(tree);
                        },
                    )
                } else {
                    Ok(()).wrap_with_cost(Default::default())
                }
            })
    }

    /// Loads the Merk from it's parent root key
    /// The base root key should only be used if the Merk tree is independent
    /// Meaning that it doesn't have a parent Merk
    pub(crate) fn load_root(
        &mut self,
        value_defined_cost_fn: Option<impl Fn(&[u8]) -> Option<ValueDefinedCostType>>,
    ) -> CostResult<(), Error> {
        // In case of successful seek for root key check if it exists
        if let Some(tree_root_key) = self.root_tree_key.get_mut() {
            // Trying to build a tree out of it, costs will be accumulated because
            // `Tree::get` returns `CostContext` and this call happens inside `flat_map_ok`.
            TreeNode::get(&self.storage, tree_root_key, value_defined_cost_fn).map_ok(|tree| {
                self.tree = Cell::new(tree);
            })
        } else {
            // The tree is empty
            Ok(()).wrap_with_cost(Default::default())
        }
    }
}

fn fetch_node<'db>(
    db: &impl StorageContext<'db>,
    key: &[u8],
    value_defined_cost_fn: Option<impl Fn(&[u8]) -> Option<ValueDefinedCostType>>,
) -> Result<Option<TreeNode>, Error> {
    let bytes = db.get(key).unwrap().map_err(StorageError)?; // TODO: get_pinned ?
    if let Some(bytes) = bytes {
        Ok(Some(
            TreeNode::decode(key.to_vec(), &bytes, value_defined_cost_fn).map_err(EdError)?,
        ))
    } else {
        Ok(None)
    }
}

// // TODO: get rid of Fetch/source and use GroveDB storage_cost abstraction

#[cfg(test)]
mod test {
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{PrefixedRocksDbStorageContext, RocksDbStorage},
        RawIterator, Storage, StorageBatch, StorageContext,
    };
    use tempfile::TempDir;

    use super::{Merk, RefWalker};
    use crate::{
        merk::source::MerkSource, test_utils::*, tree::kv::ValueDefinedCostType, Op,
        TreeFeatureType::BasicMerkNode,
    };

    // TODO: Close and then reopen test

    fn assert_invariants(merk: &TempMerk) {
        merk.use_tree(|maybe_tree| {
            let tree = maybe_tree.expect("expected tree");
            assert_tree_invariants(tree);
        })
    }

    #[test]
    fn simple_insert_apply() {
        let batch_size = 20;
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..batch_size);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");

        assert_invariants(&merk);
        assert_eq!(
            merk.root_hash().unwrap(),
            [
                126, 168, 96, 201, 59, 225, 123, 33, 206, 154, 87, 23, 139, 143, 136, 52, 103, 9,
                218, 90, 71, 153, 240, 47, 227, 168, 1, 104, 239, 237, 140, 147
            ]
        );
    }

    #[test]
    fn insert_uncached() {
        let batch_size = 20;
        let mut merk = TempMerk::new();

        let batch = make_batch_seq(0..batch_size);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_invariants(&merk);

        let batch = make_batch_seq(batch_size..(batch_size * 2));
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_invariants(&merk);
    }

    #[test]
    fn insert_two() {
        let tree_size = 2;
        let batch_size = 1;
        let mut merk = TempMerk::new();

        for i in 0..(tree_size / batch_size) {
            let batch = make_batch_rand(batch_size, i);
            merk.apply::<_, Vec<_>>(&batch, &[], None)
                .unwrap()
                .expect("apply failed");
        }
    }

    #[test]
    fn insert_rand() {
        let tree_size = 40;
        let batch_size = 4;
        let mut merk = TempMerk::new();

        for i in 0..(tree_size / batch_size) {
            println!("i:{i}");
            let batch = make_batch_rand(batch_size, i);
            merk.apply::<_, Vec<_>>(&batch, &[], None)
                .unwrap()
                .expect("apply failed");
        }
    }

    #[test]
    fn actual_deletes() {
        let mut merk = TempMerk::new();

        let batch = make_batch_rand(10, 1);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");

        let key = batch.first().unwrap().0.clone();
        merk.apply::<_, Vec<_>>(&[(key.clone(), Op::Delete)], &[], None)
            .unwrap()
            .unwrap();

        let value = merk.storage.get(key.as_slice()).unwrap().unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn aux_data() {
        let mut merk = TempMerk::new();
        merk.apply::<Vec<_>, _>(
            &[],
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerkNode), None)],
            None,
        )
        .unwrap()
        .expect("apply failed");
        merk.commit();

        let val = merk.get_aux(&[1, 2, 3]).unwrap().unwrap();
        assert_eq!(val, Some(vec![4, 5, 6]));
    }

    #[test]
    fn get_not_found() {
        let mut merk = TempMerk::new();

        // no root
        assert!(merk
            .get(
                &[1, 2, 3],
                true,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>
            )
            .unwrap()
            .unwrap()
            .is_none());

        // cached
        merk.apply::<_, Vec<_>>(
            &[(vec![5, 5, 5], Op::Put(vec![], BasicMerkNode))],
            &[],
            None,
        )
        .unwrap()
        .unwrap();
        assert!(merk
            .get(
                &[1, 2, 3],
                true,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>
            )
            .unwrap()
            .unwrap()
            .is_none());

        // uncached
        merk.apply::<_, Vec<_>>(
            &[
                (vec![0, 0, 0], Op::Put(vec![], BasicMerkNode)),
                (vec![1, 1, 1], Op::Put(vec![], BasicMerkNode)),
                (vec![2, 2, 2], Op::Put(vec![], BasicMerkNode)),
            ],
            &[],
            None,
        )
        .unwrap()
        .unwrap();
        assert!(merk
            .get(
                &[3, 3, 3],
                true,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>
            )
            .unwrap()
            .unwrap()
            .is_none());
    }

    // TODO: what this test should do?
    #[test]
    fn reopen_check_root_hash() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_primary_rocksdb(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");
        let batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
            .unwrap()
            .unwrap();
        let batch = make_batch_seq(11..12);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
            .unwrap()
            .unwrap();
    }

    #[test]
    fn test_get_node_cost() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_primary_rocksdb(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");
        let batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
            .unwrap()
            .unwrap();
        drop(merk);
    }

    #[test]
    fn reopen() {
        fn collect(
            mut node: RefWalker<MerkSource<PrefixedRocksDbStorageContext>>,
            nodes: &mut Vec<Vec<u8>>,
        ) {
            nodes.push(node.tree().encode());
            if let Some(c) = node
                .walk(true, None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>)
                .unwrap()
                .unwrap()
            {
                collect(c, nodes);
            }
            if let Some(c) = node
                .walk(false, None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>)
                .unwrap()
                .unwrap()
            {
                collect(c, nodes);
            }
        }

        let tmp_dir = TempDir::new().expect("cannot open tempdir");

        let original_nodes = {
            let storage = RocksDbStorage::default_primary_rocksdb(tmp_dir.path())
                .expect("cannot open rocksdb storage");
            let batch = StorageBatch::new();
            let mut merk = Merk::open_base(
                storage
                    .get_storage_context(SubtreePath::empty(), Some(&batch))
                    .unwrap(),
                false,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap()
            .expect("cannot open merk");
            let merk_batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(merk_batch.as_slice(), &[], None)
                .unwrap()
                .unwrap();

            storage
                .commit_multi_context_batch(batch, None)
                .unwrap()
                .expect("cannot commit batch");
            let merk = Merk::open_base(
                storage
                    .get_storage_context(SubtreePath::empty(), None)
                    .unwrap(),
                false,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap()
            .expect("cannot open merk");

            let mut tree = merk.tree.take().unwrap();
            let walker = RefWalker::new(&mut tree, merk.source());

            let mut nodes = vec![];
            collect(walker, &mut nodes);
            nodes
        };

        let storage = RocksDbStorage::default_primary_rocksdb(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");
        let mut tree = merk.tree.take().unwrap();
        let walker = RefWalker::new(&mut tree, merk.source());

        let mut reopen_nodes = vec![];
        collect(walker, &mut reopen_nodes);

        assert_eq!(reopen_nodes, original_nodes);
    }

    type PrefixedStorageIter<'db, 'ctx> =
        &'ctx mut <PrefixedRocksDbStorageContext<'db> as StorageContext<'db>>::RawIterator;

    #[test]
    fn reopen_iter() {
        fn collect(iter: PrefixedStorageIter<'_, '_>, nodes: &mut Vec<(Vec<u8>, Vec<u8>)>) {
            while iter.valid().unwrap() {
                nodes.push((
                    iter.key().unwrap().unwrap().to_vec(),
                    iter.value().unwrap().unwrap().to_vec(),
                ));
                iter.next().unwrap();
            }
        }
        let tmp_dir = TempDir::new().expect("cannot open tempdir");

        let original_nodes = {
            let storage = RocksDbStorage::default_primary_rocksdb(tmp_dir.path())
                .expect("cannot open rocksdb storage");
            let batch = StorageBatch::new();
            let mut merk = Merk::open_base(
                storage
                    .get_storage_context(SubtreePath::empty(), Some(&batch))
                    .unwrap(),
                false,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap()
            .expect("cannot open merk");
            let merk_batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(merk_batch.as_slice(), &[], None)
                .unwrap()
                .unwrap();

            storage
                .commit_multi_context_batch(batch, None)
                .unwrap()
                .expect("cannot commit batch");

            let mut nodes = vec![];
            let merk = Merk::open_base(
                storage
                    .get_storage_context(SubtreePath::empty(), None)
                    .unwrap(),
                false,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap()
            .expect("cannot open merk");
            collect(&mut merk.storage.raw_iter(), &mut nodes);
            nodes
        };
        let storage = RocksDbStorage::default_primary_rocksdb(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");

        let mut reopen_nodes = vec![];
        collect(&mut merk.storage.raw_iter(), &mut reopen_nodes);

        assert_eq!(reopen_nodes, original_nodes);
    }

    #[test]
    fn update_node() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_primary_rocksdb(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let batch = StorageBatch::new();
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), Some(&batch))
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");

        merk.apply::<_, Vec<_>>(
            &[(b"9".to_vec(), Op::Put(b"a".to_vec(), BasicMerkNode))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");
        merk.apply::<_, Vec<_>>(
            &[(b"10".to_vec(), Op::Put(b"a".to_vec(), BasicMerkNode))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");

        let result = merk
            .get(
                b"10".as_slice(),
                true,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap()
            .expect("should get successfully");
        assert_eq!(result, Some(b"a".to_vec()));

        // Update the node
        merk.apply::<_, Vec<_>>(
            &[(b"10".to_vec(), Op::Put(b"b".to_vec(), BasicMerkNode))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");
        let result = merk
            .get(
                b"10".as_slice(),
                true,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap()
            .expect("should get successfully");
        assert_eq!(result, Some(b"b".to_vec()));

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .expect("cannot open merk");

        // Update the node after dropping merk
        merk.apply::<_, Vec<_>>(
            &[(b"10".to_vec(), Op::Put(b"c".to_vec(), BasicMerkNode))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");
        let result = merk
            .get(
                b"10".as_slice(),
                true,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap()
            .expect("should get successfully");
        assert_eq!(result, Some(b"c".to_vec()));
    }
}
