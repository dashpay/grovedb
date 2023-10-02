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
pub mod restore;

use std::{
    cell::Cell,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, LinkedList},
    fmt,
};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_no_add,
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    },
    ChildrenSizesWithValue, CostContext, CostResult, CostsExt, FeatureSumLength, OperationCost,
};
use grovedb_storage::{self, Batch, RawIterator, StorageContext};

use crate::{
    error::Error,
    merk::{
        defaults::{MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES, ROOT_KEY_KEY},
        options::MerkOptions,
    },
    proofs::{
        chunk::{
            chunk::{LEFT, RIGHT},
            util::traversal_instruction_as_string,
        },
        encode_into,
        query::query_item::QueryItem,
        Op as ProofOp, Query,
    },
    tree::{
        kv::{ValueDefinedCostType, KV},
        AuxMerkBatch, Commit, CryptoHash, Fetch, Link, MerkBatch, Op, RefWalker, Tree, Walker,
        NULL_HASH,
    },
    verify_query,
    Error::{CostsError, EdError, StorageError},
    MerkType::{BaseMerk, LayeredMerk, StandaloneMerk},
    TreeFeatureType,
};

type Proof = (LinkedList<ProofOp>, Option<u16>, Option<u16>);

/// Proof construction result
pub struct ProofConstructionResult {
    /// Proof
    pub proof: Vec<u8>,
    /// Limit
    pub limit: Option<u16>,
    /// Offset
    pub offset: Option<u16>,
}

impl ProofConstructionResult {
    /// New ProofConstructionResult
    pub fn new(proof: Vec<u8>, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            proof,
            limit,
            offset,
        }
    }
}

/// Proof without encoding result
pub struct ProofWithoutEncodingResult {
    /// Proof
    pub proof: LinkedList<ProofOp>,
    /// Limit
    pub limit: Option<u16>,
    /// Offset
    pub offset: Option<u16>,
}

impl ProofWithoutEncodingResult {
    /// New ProofWithoutEncodingResult
    pub fn new(proof: LinkedList<ProofOp>, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            proof,
            limit,
            offset,
        }
    }
}

/// Key update types
pub struct KeyUpdates {
    pub new_keys: BTreeSet<Vec<u8>>,
    pub updated_keys: BTreeSet<Vec<u8>>,
    pub deleted_keys: LinkedList<(Vec<u8>, Option<KeyValueStorageCost>)>,
    pub updated_root_key_from: Option<Vec<u8>>,
}

impl KeyUpdates {
    /// New KeyUpdate
    pub fn new(
        new_keys: BTreeSet<Vec<u8>>,
        updated_keys: BTreeSet<Vec<u8>>,
        deleted_keys: LinkedList<(Vec<u8>, Option<KeyValueStorageCost>)>,
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
    Option<KeyValueStorageCost>,
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
    pub(crate) tree: Cell<Option<Tree>>,
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
        Option<KeyValueStorageCost>,
    )>,
    Error,
>;

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Open empty tree
    pub fn open_empty(storage: S, merk_type: MerkType, is_sum_tree: bool) -> Self {
        Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type,
            is_sum_tree,
        }
    }

    /// Open standalone tree
    pub fn open_standalone(storage: S, is_sum_tree: bool) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: StandaloneMerk,
            is_sum_tree,
        };

        merk.load_base_root().map_ok(|_| merk)
    }

    /// Open base tree
    pub fn open_base(storage: S, is_sum_tree: bool) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: BaseMerk,
            is_sum_tree,
        };

        merk.load_base_root().map_ok(|_| merk)
    }

    /// Open layered tree with root key
    pub fn open_layered_with_root_key(
        storage: S,
        root_key: Option<Vec<u8>>,
        is_sum_tree: bool,
    ) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(root_key),
            storage,
            merk_type: LayeredMerk,
            is_sum_tree,
        };

        merk.load_root().map_ok(|_| merk)
    }

    /// Deletes tree data
    pub fn clear(&mut self) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut iter = self.storage.raw_iter();
        iter.seek_to_first().unwrap_add_cost(&mut cost);

        let mut to_delete = self.storage.new_batch();
        while iter.valid().unwrap_add_cost(&mut cost) {
            if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                // todo: deal with cost reimbursement
                to_delete.delete(key, None);
            }
            iter.next().unwrap_add_cost(&mut cost);
        }
        cost_return_on_error!(
            &mut cost,
            self.storage.commit_batch(to_delete).map_err(StorageError)
        );
        self.tree.set(None);
        Ok(()).wrap_with_cost(cost)
    }

    /// Gets an auxiliary value.
    pub fn get_aux(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage.get_aux(key).map_err(StorageError)
    }

    /// Returns if the value at the given key exists
    ///
    /// Note that this is essentially the same as a normal RocksDB `get`, so
    /// should be a fast operation and has almost no tree overhead.
    pub fn exists(&self, key: &[u8]) -> CostResult<bool, Error> {
        self.has_node_direct(key)
    }

    /// Returns if the value at the given key exists
    ///
    /// Note that this is essentially the same as a normal RocksDB `get`, so
    /// should be a fast operation and has almost no tree overhead.
    /// Contrary to a simple exists, this traverses the tree and can be faster
    /// if the tree is cached, but slower if it is not
    pub fn exists_by_traversing_tree(&self, key: &[u8]) -> CostResult<bool, Error> {
        self.has_node(key)
    }

    /// Gets a value for the given key. If the key is not found, `None` is
    /// returned.
    ///
    /// Note that this is essentially the same as a normal RocksDB `get`, so
    /// should be a fast operation and has almost no tree overhead.
    pub fn get(&self, key: &[u8], allow_cache: bool) -> CostResult<Option<Vec<u8>>, Error> {
        if allow_cache {
            self.get_node_fn(key, |node| {
                node.value_as_slice()
                    .to_vec()
                    .wrap_with_cost(Default::default())
            })
        } else {
            self.get_node_direct_fn(key, |node| {
                node.value_as_slice()
                    .to_vec()
                    .wrap_with_cost(Default::default())
            })
        }
    }

    /// Returns the feature type for the node at the given key.
    pub fn get_feature_type(
        &self,
        key: &[u8],
        allow_cache: bool,
    ) -> CostResult<Option<TreeFeatureType>, Error> {
        if allow_cache {
            self.get_node_fn(key, |node| {
                node.feature_type().wrap_with_cost(Default::default())
            })
        } else {
            self.get_node_direct_fn(key, |node| {
                node.feature_type().wrap_with_cost(Default::default())
            })
        }
    }

    /// Gets a hash of a node by a given key, `None` is returned in case
    /// when node not found by the key.
    pub fn get_hash(&self, key: &[u8], allow_cache: bool) -> CostResult<Option<CryptoHash>, Error> {
        if allow_cache {
            self.get_node_fn(key, |node| node.hash())
        } else {
            self.get_node_direct_fn(key, |node| node.hash())
        }
    }

    /// Gets the value hash of a node by a given key, `None` is returned in case
    /// when node not found by the key.
    pub fn get_value_hash(
        &self,
        key: &[u8],
        allow_cache: bool,
    ) -> CostResult<Option<CryptoHash>, Error> {
        if allow_cache {
            self.get_node_fn(key, |node| {
                (*node.value_hash()).wrap_with_cost(OperationCost::default())
            })
        } else {
            self.get_node_direct_fn(key, |node| {
                (*node.value_hash()).wrap_with_cost(OperationCost::default())
            })
        }
    }

    /// Gets a hash of a node by a given key, `None` is returned in case
    /// when node not found by the key.
    pub fn get_kv_hash(
        &self,
        key: &[u8],
        allow_cache: bool,
    ) -> CostResult<Option<CryptoHash>, Error> {
        if allow_cache {
            self.get_node_fn(key, |node| {
                (*node.inner.kv.hash()).wrap_with_cost(OperationCost::default())
            })
        } else {
            self.get_node_direct_fn(key, |node| {
                (*node.inner.kv.hash()).wrap_with_cost(OperationCost::default())
            })
        }
    }

    /// Gets the value and value hash of a node by a given key, `None` is
    /// returned in case when node not found by the key.
    pub fn get_value_and_value_hash(
        &self,
        key: &[u8],
        allow_cache: bool,
    ) -> CostResult<Option<(Vec<u8>, CryptoHash)>, Error> {
        if allow_cache {
            self.get_node_fn(key, |node| {
                (node.value_as_slice().to_vec(), *node.value_hash())
                    .wrap_with_cost(OperationCost::default())
            })
        } else {
            self.get_node_direct_fn(key, |node| {
                (node.value_as_slice().to_vec(), *node.value_hash())
                    .wrap_with_cost(OperationCost::default())
            })
        }
    }

    /// See if a node's field exists
    fn has_node_direct(&self, key: &[u8]) -> CostResult<bool, Error> {
        Tree::get(&self.storage, key).map_ok(|x| x.is_some())
    }

    /// See if a node's field exists
    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error> {
        self.use_tree(move |maybe_tree| {
            let mut cursor = match maybe_tree {
                None => return Ok(false).wrap_with_cost(Default::default()), // empty tree
                Some(tree) => tree,
            };

            loop {
                if key == cursor.key() {
                    return Ok(true).wrap_with_cost(OperationCost::default());
                }

                let left = key < cursor.key();
                let link = match cursor.link(left) {
                    None => return Ok(false).wrap_with_cost(Default::default()), // not found
                    Some(link) => link,
                };

                let maybe_child = link.tree();
                match maybe_child {
                    None => {
                        // fetch from RocksDB
                        break self.has_node_direct(key);
                    }
                    Some(child) => cursor = child, // traverse to child
                }
            }
        })
    }

    /// Generic way to get a node's field
    fn get_node_direct_fn<T, F>(&self, key: &[u8], f: F) -> CostResult<Option<T>, Error>
    where
        F: FnOnce(&Tree) -> CostContext<T>,
    {
        Tree::get(&self.storage, key).flat_map_ok(|maybe_node| {
            let mut cost = OperationCost::default();
            Ok(maybe_node.map(|node| f(&node).unwrap_add_cost(&mut cost))).wrap_with_cost(cost)
        })
    }

    /// Generic way to get a node's field
    fn get_node_fn<T, F>(&self, key: &[u8], f: F) -> CostResult<Option<T>, Error>
    where
        F: FnOnce(&Tree) -> CostContext<T>,
    {
        self.use_tree(move |maybe_tree| {
            let mut cursor = match maybe_tree {
                None => return Ok(None).wrap_with_cost(Default::default()), // empty tree
                Some(tree) => tree,
            };

            loop {
                if key == cursor.key() {
                    return f(cursor).map(|x| Ok(Some(x)));
                }

                let left = key < cursor.key();
                let link = match cursor.link(left) {
                    None => return Ok(None).wrap_with_cost(Default::default()), // not found
                    Some(link) => link,
                };

                let maybe_child = link.tree();
                match maybe_child {
                    None => {
                        // fetch from RocksDB
                        break self.get_node_direct_fn(key, f);
                    }
                    Some(child) => cursor = child, // traverse to child
                }
            }
        })
    }

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

    /// Returns the height of the Merk tree
    pub fn height(&self) -> Option<u8> {
        self.use_tree(|tree| match tree {
            None => None,
            Some(tree) => Some(tree.height()),
        })
    }

    // TODO: remove this
    // /// Returns a clone of the Tree instance in Merk
    // pub fn get_root_tree(&self) -> Option<Tree> {
    //     self.use_tree(|tree| match tree {
    //         None => None,
    //         Some(tree) => Some(tree.clone()),
    //     })
    // }

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

    /// Applies a batch of operations (puts and deletes) to the tree.
    ///
    /// This will fail if the keys in `batch` are not sorted and unique. This
    /// check creates some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `apply_unchecked` for a small performance
    /// gain.
    ///
    /// # Example
    /// ```
    /// # let mut store = grovedb_merk::test_utils::TempMerk::new();
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0], BasicMerk))], &[], None)
    ///         .unwrap().expect("");
    ///
    /// use grovedb_merk::Op;
    /// use grovedb_merk::TreeFeatureType::BasicMerk;
    ///
    /// let batch = &[
    ///     // puts value [4,5,6] to key[1,2,3]
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk)),
    ///     // deletes key [4,5,6]
    ///     (vec![4, 5, 6], Op::Delete),
    /// ];
    /// store.apply::<_, Vec<_>>(batch, &[], None).unwrap().expect("");
    /// ```
    pub fn apply<KB, KA>(
        &mut self,
        batch: &MerkBatch<KB>,
        aux: &AuxMerkBatch<KA>,
        options: Option<MerkOptions>,
    ) -> CostResult<(), Error>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
    {
        let use_sum_nodes = self.is_sum_tree;
        self.apply_with_costs_just_in_time_value_update(
            batch,
            aux,
            options,
            &|key, value| {
                Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key.len() as u32,
                    value.len() as u32,
                    use_sum_nodes,
                ))
            },
            &mut |_costs, _old_value, _value| Ok((false, None)),
            &mut |_a, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
    }

    /// Applies a batch of operations (puts and deletes) to the tree.
    ///
    /// This will fail if the keys in `batch` are not sorted and unique. This
    /// check creates some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `apply_unchecked` for a small performance
    /// gain.
    ///
    /// # Example
    /// ```
    /// # let mut store = grovedb_merk::test_utils::TempMerk::new();
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0], BasicMerk))], &[], None)
    ///         .unwrap().expect("");
    ///
    /// use grovedb_merk::Op;
    /// use grovedb_merk::TreeFeatureType::BasicMerk;
    ///
    /// let batch = &[
    ///     // puts value [4,5,6] to key[1,2,3]
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk)),
    ///     // deletes key [4,5,6]
    ///     (vec![4, 5, 6], Op::Delete),
    /// ];
    /// store.apply::<_, Vec<_>>(batch, &[], None).unwrap().expect("");
    /// ```
    pub fn apply_with_specialized_costs<KB, KA>(
        &mut self,
        batch: &MerkBatch<KB>,
        aux: &AuxMerkBatch<KA>,
        options: Option<MerkOptions>,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
    ) -> CostResult<(), Error>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
    {
        self.apply_with_costs_just_in_time_value_update(
            batch,
            aux,
            options,
            old_specialized_cost,
            &mut |_costs, _old_value, _value| Ok((false, None)),
            &mut |_a, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
    }

    /// Applies a batch of operations (puts and deletes) to the tree with the
    /// ability to update values based on costs.
    ///
    /// This will fail if the keys in `batch` are not sorted and unique. This
    /// check creates some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `apply_unchecked` for a small performance
    /// gain.
    ///
    /// # Example
    /// ```
    /// # let mut store = grovedb_merk::test_utils::TempMerk::new();
    /// # store.apply_with_costs_just_in_time_value_update::<_, Vec<_>>(
    ///     &[(vec![4,5,6], Op::Put(vec![0], BasicMerk))],
    ///     &[],
    ///     None,
    ///     &|k, v| Ok(0),
    ///     &mut |s, v, o| Ok((false, None)),
    ///     &mut |s, k, v| Ok((NoStorageRemoval, NoStorageRemoval))
    /// ).unwrap().expect("");
    ///
    /// use grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
    /// use grovedb_merk::Op;
    /// use grovedb_merk::TreeFeatureType::BasicMerk;
    ///
    /// let batch = &[
    ///     // puts value [4,5,6] to key[1,2,3]
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk)),
    ///     // deletes key [4,5,6]
    ///     (vec![4, 5, 6], Op::Delete),
    /// ];
    ///
    /// store.apply_with_costs_just_in_time_value_update::<_, Vec<_>>(
    ///     batch,
    ///     &[],
    ///     None,
    ///     &|k, v| Ok(0),
    ///     &mut |s, v, o| Ok((false, None)),
    ///     &mut |s, k, v| Ok((NoStorageRemoval, NoStorageRemoval))
    /// ).unwrap().expect("");
    /// ```
    pub fn apply_with_costs_just_in_time_value_update<KB, KA>(
        &mut self,
        batch: &MerkBatch<KB>,
        aux: &AuxMerkBatch<KA>,
        options: Option<MerkOptions>,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<
            (bool, Option<ValueDefinedCostType>),
            Error,
        >,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> CostResult<(), Error>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
    {
        // ensure keys in batch are sorted and unique
        let mut maybe_prev_key: Option<&KB> = None;
        for (key, ..) in batch.iter() {
            if let Some(prev_key) = maybe_prev_key {
                match prev_key.as_ref().cmp(key.as_ref()) {
                    Ordering::Greater => {
                        return Err(Error::InvalidInputError("Keys in batch must be sorted"))
                            .wrap_with_cost(Default::default())
                    }
                    Ordering::Equal => {
                        return Err(Error::InvalidInputError("Keys in batch must be unique"))
                            .wrap_with_cost(Default::default())
                    }
                    _ => (),
                }
            }
            maybe_prev_key = Some(key);
        }

        self.apply_unchecked(
            batch,
            aux,
            options,
            old_specialized_cost,
            update_tree_value_based_on_costs,
            section_removal_bytes,
        )
    }

    /// Applies a batch of operations (puts and deletes) to the tree.
    ///
    /// # Safety
    /// This is unsafe because the keys in `batch` must be sorted and unique -
    /// if they are not, there will be undefined behavior. For a safe version of
    /// this method which checks to ensure the batch is sorted and unique, see
    /// `apply`.
    ///
    /// # Example
    /// ```
    /// # let mut store = grovedb_merk::test_utils::TempMerk::new();
    /// # store.apply_with_costs_just_in_time_value_update::<_, Vec<_>>(
    ///     &[(vec![4,5,6], Op::Put(vec![0], BasicMerk))],
    ///     &[],
    ///     None,
    ///     &|k, v| Ok(0),
    ///     &mut |s, o, v| Ok((false, None)),
    ///     &mut |s, k, v| Ok((NoStorageRemoval, NoStorageRemoval))
    /// ).unwrap().expect("");
    ///
    /// use grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
    /// use grovedb_merk::Op;
    /// use grovedb_merk::TreeFeatureType::BasicMerk;
    ///
    /// let batch = &[
    ///     // puts value [4,5,6] to key [1,2,3]
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk)),
    ///     // deletes key [4,5,6]
    ///     (vec![4, 5, 6], Op::Delete),
    /// ];
    ///     unsafe { store.apply_unchecked::<_, Vec<_>, _, _, _>(    /// /// ///
    ///     batch,
    ///     &[],
    ///     None,
    ///     &|k, v| Ok(0),
    ///     &mut |s, o, v| Ok((false, None)),
    ///     &mut |s, k, v| Ok((NoStorageRemoval, NoStorageRemoval))
    /// ).unwrap().expect("");
    /// }
    /// ```
    pub fn apply_unchecked<KB, KA, C, U, R>(
        &mut self,
        batch: &MerkBatch<KB>,
        aux: &AuxMerkBatch<KA>,
        options: Option<MerkOptions>,
        old_specialized_cost: &C,
        update_tree_value_based_on_costs: &mut U,
        section_removal_bytes: &mut R,
    ) -> CostResult<(), Error>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
        C: Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        U: FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<(bool, Option<ValueDefinedCostType>), Error>,
        R: FnMut(&Vec<u8>, u32, u32) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let maybe_walker = self
            .tree
            .take()
            .take()
            .map(|tree| Walker::new(tree, self.source()));

        Walker::apply_to(
            maybe_walker,
            batch,
            self.source(),
            old_specialized_cost,
            section_removal_bytes,
        )
        .flat_map_ok(|(maybe_tree, key_updates)| {
            // we set the new root node of the merk tree
            self.tree.set(maybe_tree);
            // commit changes to db
            self.commit(
                key_updates,
                aux,
                options,
                old_specialized_cost,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            )
        })
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in the
    /// query, if the key is found in the store then the value will be proven to
    /// be in the tree. For each key in the query that does not exist in the
    /// tree, its absence will be proven by including boundary keys.
    ///
    /// The proof returned is in an encoded format which can be verified with
    /// `merk::verify`.
    ///
    /// This will fail if the keys in `query` are not sorted and unique. This
    /// check adds some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `prove_unchecked` for a small performance
    /// gain.
    pub fn prove(
        &self,
        query: Query,
        limit: Option<u16>,
        offset: Option<u16>,
    ) -> CostResult<ProofConstructionResult, Error> {
        let left_to_right = query.left_to_right;
        self.prove_unchecked(query, limit, offset, left_to_right)
            .map_ok(|(proof, limit, offset)| {
                let mut bytes = Vec::with_capacity(128);
                encode_into(proof.iter(), &mut bytes);
                ProofConstructionResult::new(bytes, limit, offset)
            })
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in the
    /// query, if the key is found in the store then the value will be proven to
    /// be in the tree. For each key in the query that does not exist in the
    /// tree, its absence will be proven by including boundary keys.
    ///
    /// The proof returned is in an intermediate format to be later encoded
    ///
    /// This will fail if the keys in `query` are not sorted and unique. This
    /// check adds some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `prove_unchecked` for a small performance
    /// gain.
    pub fn prove_without_encoding(
        &self,
        query: Query,
        limit: Option<u16>,
        offset: Option<u16>,
    ) -> CostResult<ProofWithoutEncodingResult, Error> {
        let left_to_right = query.left_to_right;
        self.prove_unchecked(query, limit, offset, left_to_right)
            .map_ok(|(proof, limit, offset)| ProofWithoutEncodingResult::new(proof, limit, offset))
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in
    /// the query, if the key is found in the store then the value will be
    /// proven to be in the tree. For each key in the query that does not
    /// exist in the tree, its absence will be proven by including
    /// boundary keys.
    /// The proof returned is in an encoded format which can be verified with
    /// `merk::verify`.
    ///
    /// This is unsafe because the keys in `query` must be sorted and unique -
    /// if they are not, there will be undefined behavior. For a safe version
    /// of this method which checks to ensure the batch is sorted and
    /// unique, see `prove`.
    pub fn prove_unchecked<Q, I>(
        &self,
        query: I,
        limit: Option<u16>,
        offset: Option<u16>,
        left_to_right: bool,
    ) -> CostResult<Proof, Error>
    where
        Q: Into<QueryItem>,
        I: IntoIterator<Item = Q>,
    {
        let query_vec: Vec<QueryItem> = query.into_iter().map(Into::into).collect();

        self.use_tree_mut(|maybe_tree| {
            maybe_tree
                .ok_or(Error::CorruptedCodeExecution(
                    "Cannot create proof for empty tree",
                ))
                .wrap_with_cost(Default::default())
                .flat_map_ok(|tree| {
                    let mut ref_walker = RefWalker::new(tree, self.source());
                    ref_walker.create_proof(query_vec.as_slice(), limit, offset, left_to_right)
                })
                .map_ok(|(proof, _, limit, offset, ..)| (proof, limit, offset))
        })
    }

    /// Commit tree changes
    pub fn commit<K>(
        &mut self,
        key_updates: KeyUpdates,
        aux: &AuxMerkBatch<K>,
        options: Option<MerkOptions>,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<
            (bool, Option<ValueDefinedCostType>),
            Error,
        >,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
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
                    tree.commit(
                        &mut committer,
                        old_specialized_cost,
                        update_tree_value_based_on_costs,
                        section_removal_bytes
                    )
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
        for (key, maybe_sum_tree_cost, maybe_value, maybe_cost) in to_batch {
            if let Some((value, left_size, right_size)) = maybe_value {
                cost_return_on_error_no_add!(
                    &cost,
                    batch
                        .put(
                            &key,
                            &value,
                            Some((maybe_sum_tree_cost, left_size, right_size)),
                            maybe_cost
                        )
                        .map_err(CostsError)
                );
            } else {
                batch.delete(&key, maybe_cost);
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

    fn source(&self) -> MerkSource<S> {
        MerkSource {
            storage: &self.storage,
            is_sum_tree: self.is_sum_tree,
        }
    }

    /// Use tree
    pub(crate) fn use_tree<T>(&self, f: impl FnOnce(Option<&Tree>) -> T) -> T {
        let tree = self.tree.take();
        let res = f(tree.as_ref());
        self.tree.set(tree);
        res
    }

    fn use_tree_mut<T>(&self, mut f: impl FnMut(Option<&mut Tree>) -> T) -> T {
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
    pub(crate) fn load_base_root(&mut self) -> CostResult<(), Error> {
        self.storage
            .get_root(ROOT_KEY_KEY)
            .map(|root_result| root_result.map_err(Error::StorageError))
            .flat_map_ok(|tree_root_key_opt| {
                // In case of successful seek for root key check if it exists
                if let Some(tree_root_key) = tree_root_key_opt {
                    // Trying to build a tree out of it, costs will be accumulated because
                    // `Tree::get` returns `CostContext` and this call happens inside `flat_map_ok`.
                    Tree::get(&self.storage, tree_root_key).map_ok(|tree| {
                        if let Some(t) = tree.as_ref() {
                            self.root_tree_key = Cell::new(Some(t.key().to_vec()));
                        }
                        self.tree = Cell::new(tree);
                    })
                } else {
                    Ok(()).wrap_with_cost(Default::default())
                }
            })
    }

    /// Loads the Merk from it's parent root key
    /// The base root key should only be used if the Merk tree is independent
    /// Meaning that it doesn't have a parent Merk
    pub(crate) fn load_root(&mut self) -> CostResult<(), Error> {
        // In case of successful seek for root key check if it exists
        if let Some(tree_root_key) = self.root_tree_key.get_mut() {
            // Trying to build a tree out of it, costs will be accumulated because
            // `Tree::get` returns `CostContext` and this call happens inside `flat_map_ok`.
            Tree::get(&self.storage, tree_root_key).map_ok(|tree| {
                self.tree = Cell::new(tree);
            })
        } else {
            // The tree is empty
            Ok(()).wrap_with_cost(Default::default())
        }
    }

    /// Verifies the correctness of a merk tree
    /// hash values are computed correctly, heights are accurate and links
    /// consistent with backing store.
    // TODO: define the return types
    pub fn verify(&self) -> (BTreeMap<String, CryptoHash>, BTreeMap<String, Vec<u8>>) {
        let tree = self.tree.take();

        let mut bad_link_map: BTreeMap<String, CryptoHash> = BTreeMap::new();
        let mut parent_keys: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut root_traversal_instruction = vec![];

        // TODO: remove clone
        self.verify_tree(
            // TODO: handle unwrap
            &tree.clone().unwrap(),
            &mut root_traversal_instruction,
            &mut bad_link_map,
            &mut parent_keys,
        );
        self.tree.set(tree);

        return (bad_link_map, parent_keys);
    }

    fn verify_tree(
        &self,
        tree: &Tree,
        traversal_instruction: &mut Vec<bool>,
        bad_link_map: &mut BTreeMap<String, CryptoHash>,
        parent_keys: &mut BTreeMap<String, Vec<u8>>,
    ) {
        if let Some(link) = tree.link(LEFT) {
            traversal_instruction.push(LEFT);
            self.verify_link(
                link,
                tree.key(),
                traversal_instruction,
                bad_link_map,
                parent_keys,
            );
            traversal_instruction.pop();
        }

        if let Some(link) = tree.link(RIGHT) {
            traversal_instruction.push(RIGHT);
            self.verify_link(
                link,
                tree.key(),
                traversal_instruction,
                bad_link_map,
                parent_keys,
            );
            traversal_instruction.pop();
        }
    }

    fn verify_link(
        &self,
        link: &Link,
        parent_key: &[u8],
        traversal_instruction: &mut Vec<bool>,
        bad_link_map: &mut BTreeMap<String, CryptoHash>,
        parent_keys: &mut BTreeMap<String, Vec<u8>>,
    ) {
        let (hash, key, sum) = match link {
            Link::Reference { hash, key, sum, .. } => {
                (hash.to_owned(), key.to_owned(), sum.to_owned())
            }
            Link::Modified {
                tree,
                child_heights,
                ..
            } => (
                tree.hash().unwrap(),
                tree.key().to_vec(),
                tree.sum().unwrap(),
            ),
            Link::Loaded {
                hash,
                child_heights,
                sum,
                tree,
            } => (hash.to_owned(), tree.key().to_vec(), sum.to_owned()),
            _ => todo!(),
        };

        let instruction_id = traversal_instruction_as_string(&traversal_instruction);
        let node = Tree::get(&self.storage, key).unwrap();

        if node.is_err() {
            bad_link_map.insert(instruction_id.clone(), hash.clone());
            parent_keys.insert(instruction_id, parent_key.to_vec());
            return;
        }

        let node = node.unwrap();
        if node.is_none() {
            bad_link_map.insert(instruction_id.clone(), hash.clone());
            parent_keys.insert(instruction_id, parent_key.to_vec());
            return;
        }

        let node = node.unwrap();
        if &node.hash().unwrap() != &hash {
            bad_link_map.insert(instruction_id.clone(), hash.clone());
            parent_keys.insert(instruction_id, parent_key.to_vec());
            return;
        }

        if node.sum().unwrap() != sum {
            bad_link_map.insert(instruction_id.clone(), hash.clone());
            parent_keys.insert(instruction_id, parent_key.to_vec());
            return;
        }

        // TODO: check child heights
        // all checks passed, recurse
        self.verify_tree(&node, traversal_instruction, bad_link_map, parent_keys);
    }
}

fn fetch_node<'db>(db: &impl StorageContext<'db>, key: &[u8]) -> Result<Option<Tree>, Error> {
    let bytes = db.get(key).unwrap().map_err(StorageError)?; // TODO: get_pinned ?
    if let Some(bytes) = bytes {
        Ok(Some(Tree::decode(key.to_vec(), &bytes).map_err(EdError)?))
    } else {
        Ok(None)
    }
}

// impl Clone for Merk<S> {
//     fn clone(&self) -> Self {
//         let tree_clone = match self.tree.take() {
//             None => None,
//             Some(tree) => {
//                 let clone = tree.clone();
//                 self.tree.set(Some(tree));
//                 Some(clone)
//             }
//         };
//         Self {
//             tree: Cell::new(tree_clone),
//             storage_cost: self.storage_cost.clone(),
//         }
//     }
// }

// // TODO: get rid of Fetch/source and use GroveDB storage_cost abstraction

#[derive(Debug)]
pub struct MerkSource<'s, S> {
    storage: &'s S,
    is_sum_tree: bool,
}

impl<'s, S> Clone for MerkSource<'s, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
            is_sum_tree: self.is_sum_tree,
        }
    }
}

impl<'s, 'db, S> Fetch for MerkSource<'s, S>
where
    S: StorageContext<'db>,
{
    fn fetch(&self, link: &Link) -> CostResult<Tree, Error> {
        Tree::get(self.storage, link.key())
            .map_ok(|x| x.ok_or(Error::KeyNotFoundError("Key not found for fetch")))
            .flatten()
    }
}

struct MerkCommitter {
    /// The batch has a key, maybe a value, with the value bytes, maybe the left
    /// child size and maybe the right child size, then the
    /// key_value_storage_cost
    batch: Vec<BatchValue>,
    height: u8,
    levels: u8,
}

impl MerkCommitter {
    fn new(height: u8, levels: u8) -> Self {
        Self {
            batch: Vec::with_capacity(10000),
            height,
            levels,
        }
    }
}

impl Commit for MerkCommitter {
    fn write(
        &mut self,
        tree: &mut Tree,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<
            (bool, Option<ValueDefinedCostType>),
            Error,
        >,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> Result<(), Error> {
        let tree_size = tree.encoding_length();
        let (mut current_tree_plus_hook_size, mut storage_costs) =
            tree.kv_with_parent_hook_size_and_storage_cost(old_specialized_cost)?;
        let mut i = 0;

        if let Some(old_value) = tree.old_value.clone() {
            // At this point the tree value can be updated based on client requirements
            // For example to store the costs
            loop {
                let (flags_changed, value_defined_cost) = update_tree_value_based_on_costs(
                    &storage_costs.value_storage_cost,
                    &old_value,
                    tree.value_mut_ref(),
                )?;
                if !flags_changed {
                    break;
                } else {
                    tree.inner.kv.value_defined_cost = value_defined_cost;
                    let after_update_tree_plus_hook_size =
                        tree.value_encoding_length_with_parent_to_child_reference();
                    if after_update_tree_plus_hook_size == current_tree_plus_hook_size {
                        break;
                    }
                    let new_size_and_storage_costs =
                        tree.kv_with_parent_hook_size_and_storage_cost(old_specialized_cost)?;
                    current_tree_plus_hook_size = new_size_and_storage_costs.0;
                    storage_costs = new_size_and_storage_costs.1;
                }
                if i > MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES {
                    return Err(Error::CyclicError(
                        "updated value based on costs too many times",
                    ));
                }
                i += 1;
            }

            if let BasicStorageRemoval(removed_bytes) =
                storage_costs.value_storage_cost.removed_bytes
            {
                let (_, value_removed_bytes) = section_removal_bytes(&old_value, 0, removed_bytes)?;
                storage_costs.value_storage_cost.removed_bytes = value_removed_bytes;
            }
        }

        // Update old tree size after generating value storage_cost cost
        tree.old_size_with_parent_to_child_hook = current_tree_plus_hook_size;
        tree.old_value = Some(tree.value_ref().clone());

        let mut buf = Vec::with_capacity(tree_size);
        tree.encode_into(&mut buf);

        let left_child_sizes = tree.child_ref_and_sum_size(true);
        let right_child_sizes = tree.child_ref_and_sum_size(false);
        self.batch.push((
            tree.key().to_vec(),
            tree.feature_type().sum_length(),
            Some((buf, left_child_sizes, right_child_sizes)),
            Some(storage_costs),
        ));
        Ok(())
    }

    fn prune(&self, tree: &Tree) -> (bool, bool) {
        // keep N top levels of tree
        let prune = (self.height - tree.height()) >= self.levels;
        (prune, prune)
    }
}

#[cfg(test)]
mod test {
    use grovedb_costs::OperationCost;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{test_utils::TempStorage, PrefixedRocksDbStorageContext, RocksDbStorage},
        RawIterator, Storage, StorageBatch, StorageContext,
    };
    use tempfile::TempDir;

    use super::{Merk, MerkSource, RefWalker};
    use crate::{test_utils::*, Op, TreeFeatureType::BasicMerk};

    // TODO: Close and then reopen test

    fn assert_invariants(merk: &TempMerk) {
        merk.use_tree(|maybe_tree| {
            let tree = maybe_tree.expect("expected tree");
            assert_tree_invariants(tree);
        })
    }

    #[test]
    fn test_reopen_root_hash() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let test_prefix = [b"ayy"];

        let batch = StorageBatch::new();
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::from(test_prefix.as_ref()), Some(&batch))
                .unwrap(),
            false,
        )
        .unwrap()
        .unwrap();

        merk.apply::<_, Vec<_>>(
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("apply failed");

        let root_hash = merk.root_hash();

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::from(test_prefix.as_ref()), None)
                .unwrap(),
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(merk.root_hash(), root_hash);
    }

    #[test]
    fn test_open_fee() {
        let storage = TempStorage::new();
        let batch = StorageBatch::new();

        let merk_fee_context = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), Some(&batch))
                .unwrap(),
            false,
        );

        // Opening not existing merk should cost only root key seek (except context
        // creation)
        assert!(matches!(
            merk_fee_context.cost(),
            OperationCost { seek_count: 1, .. }
        ));

        let mut merk = merk_fee_context.unwrap().unwrap();
        merk.apply::<_, Vec<_>>(
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("apply failed");

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let merk_fee_context = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
        );

        // Opening existing merk should cost two seeks. (except context creation)
        assert!(matches!(
            merk_fee_context.cost(),
            OperationCost { seek_count: 2, .. }
        ));
        assert!(merk_fee_context.cost().storage_loaded_bytes > 0);
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
    fn tree_height() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..1);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(1));

        // height 2
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..2);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(2));

        // height 5
        // 2^5 - 1 = 31 (max number of elements in tree of height 5)
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..31);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(5));

        // should still be height 5 for 29 elements
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..29);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(5));
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
    fn test_has_node_with_empty_tree() {
        let mut merk = TempMerk::new();

        let key = b"something";

        let result = merk.has_node(key).unwrap().unwrap();

        assert!(!result);

        let batch_entry = (key, Op::Put(vec![123; 60], BasicMerk));

        let batch = vec![batch_entry];

        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("should ...");

        let result = merk.has_node(key).unwrap().unwrap();

        assert!(result);
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
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk), None)],
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
        assert!(merk.get(&[1, 2, 3], true).unwrap().unwrap().is_none());

        // cached
        merk.apply::<_, Vec<_>>(&[(vec![5, 5, 5], Op::Put(vec![], BasicMerk))], &[], None)
            .unwrap()
            .unwrap();
        assert!(merk.get(&[1, 2, 3], true).unwrap().unwrap().is_none());

        // uncached
        merk.apply::<_, Vec<_>>(
            &[
                (vec![0, 0, 0], Op::Put(vec![], BasicMerk)),
                (vec![1, 1, 1], Op::Put(vec![], BasicMerk)),
                (vec![2, 2, 2], Op::Put(vec![], BasicMerk)),
            ],
            &[],
            None,
        )
        .unwrap()
        .unwrap();
        assert!(merk.get(&[3, 3, 3], true).unwrap().unwrap().is_none());
    }

    // TODO: what this test should do?
    #[test]
    fn reopen_check_root_hash() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
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
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
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
            if let Some(c) = node.walk(true).unwrap().unwrap() {
                collect(c, nodes);
            }
            if let Some(c) = node.walk(false).unwrap().unwrap() {
                collect(c, nodes);
            }
        }

        let tmp_dir = TempDir::new().expect("cannot open tempdir");

        let original_nodes = {
            let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
                .expect("cannot open rocksdb storage");
            let batch = StorageBatch::new();
            let mut merk = Merk::open_base(
                storage
                    .get_storage_context(SubtreePath::empty(), Some(&batch))
                    .unwrap(),
                false,
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
            )
            .unwrap()
            .expect("cannot open merk");

            let mut tree = merk.tree.take().unwrap();
            let walker = RefWalker::new(&mut tree, merk.source());

            let mut nodes = vec![];
            collect(walker, &mut nodes);
            nodes
        };

        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
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
        fn collect<'db, 'ctx>(
            iter: PrefixedStorageIter<'db, 'ctx>,
            nodes: &mut Vec<(Vec<u8>, Vec<u8>)>,
        ) {
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
            let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
                .expect("cannot open rocksdb storage");
            let batch = StorageBatch::new();
            let mut merk = Merk::open_base(
                storage
                    .get_storage_context(SubtreePath::empty(), Some(&batch))
                    .unwrap(),
                false,
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
            )
            .unwrap()
            .expect("cannot open merk");
            collect(&mut merk.storage.raw_iter(), &mut nodes);
            nodes
        };
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
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
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let batch = StorageBatch::new();
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), Some(&batch))
                .unwrap(),
            false,
        )
        .unwrap()
        .expect("cannot open merk");

        merk.apply::<_, Vec<_>>(
            &[(b"9".to_vec(), Op::Put(b"a".to_vec(), BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");
        merk.apply::<_, Vec<_>>(
            &[(b"10".to_vec(), Op::Put(b"a".to_vec(), BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");

        let result = merk
            .get(b"10".as_slice(), true)
            .unwrap()
            .expect("should get successfully");
        assert_eq!(result, Some(b"a".to_vec()));

        // Update the node
        merk.apply::<_, Vec<_>>(
            &[(b"10".to_vec(), Op::Put(b"b".to_vec(), BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");
        let result = merk
            .get(b"10".as_slice(), true)
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
        )
        .unwrap()
        .expect("cannot open merk");

        // Update the node after dropping merk
        merk.apply::<_, Vec<_>>(
            &[(b"10".to_vec(), Op::Put(b"c".to_vec(), BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("should insert successfully");
        let result = merk
            .get(b"10".as_slice(), true)
            .unwrap()
            .expect("should get successfully");
        assert_eq!(result, Some(b"c".to_vec()));
    }
}
