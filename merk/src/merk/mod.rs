pub mod chunks;
pub(crate) mod defaults;
pub mod options;
pub mod restore;

use std::{
    cell::Cell,
    cmp::Ordering,
    collections::{BTreeSet, LinkedList},
    fmt,
    io::{Read, Write},
};

use anyhow::{anyhow, Error, Result};
use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    },
    CostContext, CostResult, CostsExt, OperationCost,
};
use ed::{Decode, Encode, Terminated};
use integer_encoding::{VarInt, VarIntReader, VarIntWriter};
use storage::{self, error::Error::CostError, Batch, RawIterator, StorageContext};

use crate::{
    merk::{
        defaults::{MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES, ROOT_KEY_KEY},
        options::MerkOptions,
    },
    proofs::{encode_into, query::QueryItem, Op as ProofOp, Query},
    tree::{
        kv::KV, AuxMerkBatch, Commit, CryptoHash, Fetch, Link, MerkBatch, Op, RefWalker, Tree,
        Walker, NULL_HASH,
    },
    MerkType::{BaseMerk, LayeredMerk, StandaloneMerk},
    TreeFeatureType,
};

type Proof = (LinkedList<ProofOp>, Option<u16>, Option<u16>);

pub struct ProofConstructionResult {
    pub proof: Vec<u8>,
    pub limit: Option<u16>,
    pub offset: Option<u16>,
}

impl ProofConstructionResult {
    pub fn new(proof: Vec<u8>, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            proof,
            limit,
            offset,
        }
    }
}

pub struct ProofWithoutEncodingResult {
    pub proof: LinkedList<ProofOp>,
    pub limit: Option<u16>,
    pub offset: Option<u16>,
}

impl ProofWithoutEncodingResult {
    pub fn new(proof: LinkedList<ProofOp>, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            proof,
            limit,
            offset,
        }
    }
}

/// A bool type
pub type IsSumTree = bool;

/// KVIterator allows you to lazily iterate over each kv pair of a subtree
pub struct KVIterator<'a, I: RawIterator> {
    raw_iter: I,
    _query: &'a Query,
    left_to_right: bool,
    query_iterator: Box<dyn Iterator<Item = &'a QueryItem> + 'a>,
    current_query_item: Option<&'a QueryItem>,
}

impl<'a, I: RawIterator> KVIterator<'a, I> {
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
    pub fn next(&mut self) -> CostContext<Option<(Vec<u8>, Vec<u8>)>> {
        let mut cost = OperationCost::default();

        if let Some(query_item) = self.current_query_item {
            let kv_pair = self.get_kv(query_item).unwrap_add_cost(&mut cost);

            if kv_pair.is_some() {
                kv_pair.wrap_with_cost(cost)
            } else {
                self.seek().unwrap_add_cost(&mut cost);
                self.next().add_cost(cost)
            }
        } else {
            None.wrap_with_cost(cost)
        }
    }
}

#[derive(PartialEq, Eq)]
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
    pub storage: S,
    pub merk_type: MerkType,
    pub is_sum_tree: bool,
}

impl<S> fmt::Debug for Merk<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Merk").finish()
    }
}

// key, maybe value, maybe child reference hooks, maybe key value storage costs
pub type UseTreeMutResult = CostContext<
    Result<
        Vec<(
            Vec<u8>,
            Option<(Vec<u8>, Option<u32>, Option<u32>)>,
            Option<KeyValueStorageCost>,
        )>,
    >,
>;

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
    <S as StorageContext<'db>>::Error: std::error::Error,
{
    pub fn open_empty(storage: S, merk_type: MerkType, is_sum_tree: bool) -> Self {
        Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type,
            is_sum_tree,
        }
    }

    pub fn open_standalone(storage: S, is_sum_tree: bool) -> CostContext<Result<Self>> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: StandaloneMerk,
            is_sum_tree,
        };

        merk.load_base_root().map_ok(|_| merk)
    }

    pub fn open_base(storage: S, is_sum_tree: bool) -> CostContext<Result<Self>> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: BaseMerk,
            is_sum_tree,
        };

        merk.load_base_root().map_ok(|_| merk)
    }

    pub fn open_layered_with_root_key(
        storage: S,
        root_key: Option<Vec<u8>>,
        is_sum_tree: bool,
    ) -> CostContext<Result<Self>> {
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
    pub fn clear(&mut self) -> CostContext<Result<()>> {
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
            self.storage.commit_batch(to_delete).map_err(|e| e.into())
        );
        self.tree.set(None);
        Ok(()).wrap_with_cost(cost)
    }

    /// Gets an auxiliary value.
    pub fn get_aux(&self, key: &[u8]) -> CostContext<Result<Option<Vec<u8>>>> {
        self.storage.get_aux(key).map_err(|e| e.into())
    }

    /// Returns if the value at the given key exists
    ///
    /// Note that this is essentially the same as a normal RocksDB `get`, so
    /// should be a fast operation and has almost no tree overhead.
    pub fn exists(&self, key: &[u8]) -> CostContext<Result<bool>> {
        self.has_node(key)
    }

    /// Gets a value for the given key. If the key is not found, `None` is
    /// returned.
    ///
    /// Note that this is essentially the same as a normal RocksDB `get`, so
    /// should be a fast operation and has almost no tree overhead.
    pub fn get(&self, key: &[u8]) -> CostContext<Result<Option<Vec<u8>>>> {
        self.get_node_fn(key, |node| {
            node.value_as_slice()
                .to_vec()
                .wrap_with_cost(Default::default())
        })
    }

    /// Returns the feature type for the node at the given key.
    pub fn get_feature_type(&self, key: &[u8]) -> CostContext<Result<Option<TreeFeatureType>>> {
        self.get_node_fn(key, |node| {
            node.feature_type().wrap_with_cost(Default::default())
        })
    }

    /// Gets a hash of a node by a given key, `None` is returned in case
    /// when node not found by the key.
    pub fn get_hash(&self, key: &[u8]) -> CostContext<Result<Option<CryptoHash>>> {
        self.get_node_fn(key, |node| node.hash())
    }

    /// Gets the value hash of a node by a given key, `None` is returned in case
    /// when node not found by the key.
    pub fn get_value_hash(&self, key: &[u8]) -> CostContext<Result<Option<CryptoHash>>> {
        self.get_node_fn(key, |node| {
            node.value_hash()
                .clone()
                .wrap_with_cost(OperationCost::default())
        })
    }

    /// Gets a hash of a node by a given key, `None` is returned in case
    /// when node not found by the key.
    pub fn get_kv_hash(&self, key: &[u8]) -> CostContext<Result<Option<CryptoHash>>> {
        self.get_node_fn(key, |node| {
            node.inner
                .kv
                .hash()
                .clone()
                .wrap_with_cost(OperationCost::default())
        })
    }

    /// Gets the value and value hash of a node by a given key, `None` is
    /// returned in case when node not found by the key.
    pub fn get_value_and_value_hash(
        &self,
        key: &[u8],
    ) -> CostContext<Result<Option<(Vec<u8>, CryptoHash)>>> {
        self.get_node_fn(key, |node| {
            (node.value_as_slice().to_vec(), node.value_hash().clone())
                .wrap_with_cost(OperationCost::default())
        })
    }

    /// See if a node's field exists
    fn has_node(&self, key: &[u8]) -> CostContext<Result<bool>> {
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
                        break Tree::get(&self.storage, key).map_ok(|x| x.is_some());
                    }
                    Some(child) => cursor = child, // traverse to child
                }
            }
        })
    }

    /// Generic way to get a node's field
    fn get_node_fn<T, F>(&self, key: &[u8], f: F) -> CostContext<Result<Option<T>>>
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
                        break Tree::get(&self.storage, key).flat_map_ok(|maybe_node| {
                            let mut cost = OperationCost::default();
                            Ok(maybe_node.map(|node| f(&node).unwrap_add_cost(&mut cost)))
                                .wrap_with_cost(cost)
                        });
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
    pub fn sum(&self) -> Option<i64> {
        self.use_tree(|tree| tree.map_or(None, |tree| tree.sum()))
    }

    /// Returns the root non-prefixed key of the tree. If the tree is empty,
    /// None.
    pub fn root_key(&self) -> Option<Vec<u8>> {
        self.use_tree(|tree| tree.map(|tree| tree.key().to_vec()))
    }

    /// Returns the root hash and non-prefixed key of the tree.
    pub fn root_hash_key_and_sum(&self) -> CostContext<(CryptoHash, Option<Vec<u8>>, Option<i64>)> {
        self.use_tree(|tree| {
            tree.map_or(
                (NULL_HASH, None, None).wrap_with_cost(Default::default()),
                |tree| {
                    tree.hash()
                        .map(|hash| (hash, Some(tree.key().to_vec()), tree.sum()))
                },
            )
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
    /// # let mut store = merk::test_utils::TempMerk::new();
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0], BasicMerk))], &[], None)
    ///         .unwrap().expect("");
    ///
    /// use merk::Op;
    /// use merk::TreeFeatureType::BasicMerk;
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
    ) -> CostContext<Result<()>>
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
    /// # let mut store = merk::test_utils::TempMerk::new();
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0], BasicMerk))], &[], None)
    ///         .unwrap().expect("");
    ///
    /// use merk::Op;
    /// use merk::TreeFeatureType::BasicMerk;
    ///
    /// let batch = &[
    ///     // puts value [4,5,6] to key[1,2,3]
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk)),
    ///     // deletes key [4,5,6]
    ///     (vec![4, 5, 6], Op::Delete),
    /// ];
    /// store.apply::<_, Vec<_>>(batch, &[], None).unwrap().expect("");
    /// ```
    pub fn apply_with_tree_costs<KB, KA>(
        &mut self,
        batch: &MerkBatch<KB>,
        aux: &AuxMerkBatch<KA>,
        options: Option<MerkOptions>,
        old_tree_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32>,
    ) -> CostContext<Result<()>>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
    {
        self.apply_with_costs_just_in_time_value_update(
            batch,
            aux,
            options,
            old_tree_cost,
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
    /// # let mut store = merk::test_utils::TempMerk::new();
    /// # store.apply_with_costs_just_in_time_value_update::<_, Vec<_>>(
    ///     &[(vec![4,5,6], Op::Put(vec![0], BasicMerk))],
    ///     &[],
    ///     None,
    ///     &|k, v| Ok(0),
    ///     &mut |s, v, o| Ok((false, None)),
    ///     &mut |s, k, v| Ok((NoStorageRemoval, NoStorageRemoval))
    /// ).unwrap().expect("");
    ///
    /// use costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
    /// use merk::Op;
    /// use merk::TreeFeatureType::BasicMerk;
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
        old_tree_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<(bool, Option<u32>)>,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        )
            -> Result<(StorageRemovedBytes, StorageRemovedBytes)>,
    ) -> CostContext<Result<()>>
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
                        return Err(anyhow!("Keys in batch must be sorted"))
                            .wrap_with_cost(Default::default())
                    }
                    Ordering::Equal => {
                        return Err(anyhow!("Keys in batch must be unique"))
                            .wrap_with_cost(Default::default())
                    }
                    _ => (),
                }
            }
            maybe_prev_key = Some(key);
        }

        unsafe {
            self.apply_unchecked(
                batch,
                aux,
                options,
                old_tree_cost,
                update_tree_value_based_on_costs,
                section_removal_bytes,
            )
        }
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
    /// # let mut store = merk::test_utils::TempMerk::new();
    /// # store.apply_with_costs_just_in_time_value_update::<_, Vec<_>>(
    ///     &[(vec![4,5,6], Op::Put(vec![0], BasicMerk))],
    ///     &[],
    ///     None,
    ///     &|k, v| Ok(0),
    ///     &mut |s, o, v| Ok((false, None)),
    ///     &mut |s, k, v| Ok((NoStorageRemoval, NoStorageRemoval))
    /// ).unwrap().expect("");
    ///
    /// use costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
    /// use merk::Op;
    /// use merk::TreeFeatureType::BasicMerk;
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
    pub unsafe fn apply_unchecked<KB, KA, C, U, R>(
        &mut self,
        batch: &MerkBatch<KB>,
        aux: &AuxMerkBatch<KA>,
        options: Option<MerkOptions>,
        old_tree_cost: &C,
        update_tree_value_based_on_costs: &mut U,
        section_removal_bytes: &mut R,
    ) -> CostContext<Result<()>>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
        C: Fn(&Vec<u8>, &Vec<u8>) -> Result<u32>,
        U: FnMut(&StorageCost, &Vec<u8>, &mut Vec<u8>) -> Result<(bool, Option<u32>)>,
        R: FnMut(&Vec<u8>, u32, u32) -> Result<(StorageRemovedBytes, StorageRemovedBytes)>,
    {
        let maybe_walker = self
            .tree
            .take()
            .take()
            .map(|tree| Walker::new(tree, self.source()));

        if maybe_walker.is_some() {
            // dbg!(&maybe_walker.as_ref().unwrap().tree());
        }

        Walker::apply_to(
            maybe_walker,
            batch,
            self.source(),
            old_tree_cost,
            section_removal_bytes,
        )
        .flat_map_ok(
            |(maybe_tree, new_keys, updated_keys, deleted_keys, updated_root_key_from)| {
                // we set the new root node of the merk tree
                self.tree.set(maybe_tree);
                // commit changes to db
                self.commit(
                    new_keys,
                    updated_keys,
                    deleted_keys,
                    updated_root_key_from,
                    aux,
                    options,
                    old_tree_cost,
                    update_tree_value_based_on_costs,
                    section_removal_bytes,
                )
            },
        )
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
    ) -> CostContext<Result<ProofConstructionResult>> {
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
    ) -> CostContext<Result<ProofWithoutEncodingResult>> {
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
    ) -> CostContext<Result<Proof>>
    where
        Q: Into<QueryItem>,
        I: IntoIterator<Item = Q>,
    {
        let query_vec: Vec<QueryItem> = query.into_iter().map(Into::into).collect();

        self.use_tree_mut(|maybe_tree| {
            maybe_tree
                .ok_or_else(|| anyhow!("Cannot create proof for empty tree"))
                .wrap_with_cost(Default::default())
                .flat_map_ok(|tree| {
                    let mut ref_walker = RefWalker::new(tree, self.source());
                    ref_walker.create_proof(query_vec.as_slice(), limit, offset, left_to_right)
                })
                .map_ok(|(proof, _, limit, offset, ..)| (proof, limit, offset))
        })
    }

    pub fn commit<K>(
        &mut self,
        new_keys: BTreeSet<Vec<u8>>,
        _updated_keys: BTreeSet<Vec<u8>>,
        deleted_keys: LinkedList<(Vec<u8>, Option<KeyValueStorageCost>)>,
        updated_root_key_from: Option<Vec<u8>>,
        aux: &AuxMerkBatch<K>,
        options: Option<MerkOptions>,
        old_tree_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<(bool, Option<u32>)>,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        )
            -> Result<(StorageRemovedBytes, StorageRemovedBytes)>,
    ) -> CostResult<(), Error>
    where
        K: AsRef<[u8]>,
    {
        // dbg!("committing");
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
                        old_tree_cost,
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
                    if updated_root_key_from.is_some() || new_keys.contains(tree_key) {
                        let costs = if self.merk_type == StandaloneMerk {
                            // if we are a standalone merk we want real costs
                            Some(KeyValueStorageCost::for_updated_root_cost(
                                updated_root_key_from.as_ref().map(|k| k.len() as u32),
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
                                .map_err(CostError)
                                .map_err(|e| e.into())
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
        for (key, maybe_cost) in deleted_keys {
            to_batch.push((key, None, maybe_cost));
        }
        to_batch.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, maybe_value, maybe_cost) in to_batch {
            if let Some((value, left_size, right_size)) = maybe_value {
                cost_return_on_error_no_add!(
                    &cost,
                    batch
                        .put(&key, &value, Some((left_size, right_size)), maybe_cost)
                        .map_err(|e| e.into())
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
                        .map_err(|e| e.into())
                ),
                Op::Delete => batch.delete_aux(key, storage_cost.clone()),
                _ => {
                    cost_return_on_error_no_add!(
                        &cost,
                        Err(anyhow!("only put and delete allowed for aux storage"))
                    );
                }
            };
        }

        // write to db
        self.storage
            .commit_batch(batch)
            .map_err(|e| e.into())
            .add_cost(cost)
    }

    pub fn walk<'s, T>(&'s self, f: impl FnOnce(Option<RefWalker<MerkSource<'s, S>>>) -> T) -> T {
        let mut tree = self.tree.take();
        let maybe_walker = tree
            .as_mut()
            .map(|tree| RefWalker::new(tree, self.source()));
        let res = f(maybe_walker);
        self.tree.set(tree);
        res
    }

    pub fn is_empty_tree(&self) -> CostContext<bool> {
        let mut iter = self.storage.raw_iter();
        iter.seek_to_first().flat_map(|_| iter.valid().map(|x| !x))
    }

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
        }
    }

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
    pub fn set_base_root_key(&mut self, key: Option<Vec<u8>>) -> CostContext<Result<(), Error>> {
        if let Some(key) = key {
            self.storage
                .put_root(ROOT_KEY_KEY, key.as_slice(), None)
                .map_err(|e| anyhow!(e)) // todo: maybe change None?
        } else {
            self.storage
                .delete_root(ROOT_KEY_KEY, None)
                .map_err(|e| anyhow!(e)) // todo: maybe change None?
        }
    }

    /// Loads the Merk from the base root key
    /// The base root key should only be used if the Merk tree is independent
    /// Meaning that it doesn't have a parent Merk
    pub(crate) fn load_base_root(&mut self) -> CostContext<Result<()>> {
        self.storage
            .get_root(ROOT_KEY_KEY)
            .map(|root_result| root_result.map_err(|e| anyhow!(e)))
            .flat_map_ok(|tree_root_key_opt| {
                // In case of successful seek for root key check if it exists
                if let Some(tree_root_key) = tree_root_key_opt {
                    // Trying to build a tree out of it, costs will be accumulated because
                    // `Tree::get` returns `CostContext` and this call happens inside `flat_map_ok`.
                    Tree::get(&self.storage, &tree_root_key).map_ok(|tree| {
                        tree.as_ref().map(|t| {
                            self.root_tree_key = Cell::new(Some(t.key().to_vec()));
                        });
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
    pub(crate) fn load_root(&mut self) -> CostContext<Result<()>> {
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
}

fn fetch_node<'db>(db: &impl StorageContext<'db>, key: &[u8]) -> Result<Option<Tree>> {
    let bytes = db.get(key).unwrap()?; // TODO: get_pinned ?
    if let Some(bytes) = bytes {
        Ok(Some(Tree::decode(key.to_vec(), &bytes)?))
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
}

impl<'s, S> Clone for MerkSource<'s, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
        }
    }
}

impl<'s, 'db, S> Fetch for MerkSource<'s, S>
where
    S: StorageContext<'db>,
{
    fn fetch(&self, link: &Link) -> CostContext<Result<Tree>> {
        Tree::get(self.storage, link.key())
            .map_ok(|x| x.ok_or_else(|| anyhow!("Key not found")))
            .flatten()
    }
}

struct MerkCommitter {
    /// The batch has a key, maybe a value, with the value bytes, maybe the left
    /// child size and maybe the right child size, then the
    /// key_value_storage_cost
    batch: Vec<(
        Vec<u8>,
        Option<(Vec<u8>, Option<u32>, Option<u32>)>,
        Option<KeyValueStorageCost>,
    )>,
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
        old_tree_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<(bool, Option<u32>)>,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        )
            -> Result<(StorageRemovedBytes, StorageRemovedBytes)>,
    ) -> Result<()> {
        let tree_size = tree.encoding_length();
        let (mut current_tree_plus_hook_size, mut storage_costs) =
            tree.kv_with_parent_hook_size_and_storage_cost(old_tree_cost)?;
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
                        tree.value_encoding_length_with_parent_to_child_reference() as u32;
                    if after_update_tree_plus_hook_size == current_tree_plus_hook_size {
                        break;
                    }
                    let new_size_and_storage_costs =
                        tree.kv_with_parent_hook_size_and_storage_cost(old_tree_cost)?;
                    current_tree_plus_hook_size = new_size_and_storage_costs.0;
                    storage_costs = new_size_and_storage_costs.1;
                }
                if i > MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES {
                    return Err(anyhow!("updated value based on costs too many times"));
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

        let mut buf = Vec::with_capacity(tree_size as usize);
        tree.encode_into(&mut buf);

        let left_child_ref_size = tree.child_ref_size(true);
        let right_child_ref_size = tree.child_ref_size(false);
        self.batch.push((
            tree.key().to_vec(),
            Some((buf, left_child_ref_size, right_child_ref_size)),
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
    use std::iter::empty;

    use costs::OperationCost;
    use storage::{
        rocksdb_storage::{PrefixedRocksDbStorageContext, RocksDbStorage},
        RawIterator, Storage, StorageContext,
    };
    use tempfile::TempDir;

    use super::{Merk, MerkSource, RefWalker};
    use crate::{test_utils::*, Op, TreeFeatureType, TreeFeatureType::BasicMerk};

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
        let test_prefix = [b"ayy"].into_iter().map(|x| x.as_slice());
        let mut merk = Merk::open_base(
            storage.get_storage_context(test_prefix.clone()).unwrap(),
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
        drop(merk);
        let merk = Merk::open_base(storage.get_storage_context(test_prefix).unwrap(), false)
            .unwrap()
            .unwrap();
        assert_eq!(merk.root_hash(), root_hash);
    }

    #[test]
    fn test_open_fee() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let test_prefix = [b"ayy"].into_iter().map(|x| x.as_slice());
        let merk_fee_context = Merk::open_base(
            storage.get_storage_context(test_prefix.clone()).unwrap(),
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

        drop(merk);

        let merk_fee_context =
            Merk::open_base(storage.get_storage_context(test_prefix).unwrap(), false);

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
    fn insert_rand() {
        let tree_size = 40;
        let batch_size = 4;
        let mut merk = TempMerk::new();

        for i in 0..(tree_size / batch_size) {
            println!("i:{}", i);
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
        let val = merk.get_aux(&[1, 2, 3]).unwrap().unwrap();
        assert_eq!(val, Some(vec![4, 5, 6]));
    }

    #[test]
    fn simulated_crash() {
        let mut merk = CrashMerk::open_base().expect("failed to open merk");

        merk.apply::<_, Vec<_>>(
            &[(vec![0], Op::Put(vec![1], BasicMerk))],
            &[(vec![2], Op::Put(vec![3], BasicMerk), None)],
            None,
        )
        .unwrap()
        .expect("apply failed");

        // make enough changes so that main column family gets auto-flushed
        for i in 0..250 {
            merk.apply::<_, Vec<_>>(&make_batch_seq(i * 2_000..(i + 1) * 2_000), &[], None)
                .unwrap()
                .expect("apply failed");
        }
        merk.crash();

        assert_eq!(merk.get_aux(&[2]).unwrap().unwrap(), Some(vec![3]));
    }

    #[test]
    fn get_not_found() {
        let mut merk = TempMerk::new();

        // no root
        assert!(merk.get(&[1, 2, 3]).unwrap().unwrap().is_none());

        // cached
        merk.apply::<_, Vec<_>>(&[(vec![5, 5, 5], Op::Put(vec![], BasicMerk))], &[], None)
            .unwrap()
            .unwrap();
        assert!(merk.get(&[1, 2, 3]).unwrap().unwrap().is_none());

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
        assert!(merk.get(&[3, 3, 3]).unwrap().unwrap().is_none());
    }

    #[test]
    fn reopen_check_root_hash() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
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
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
            .unwrap()
            .expect("cannot open merk");
        let batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
            .unwrap()
            .unwrap();
        drop(merk);

        // let merk =
        // Merk::open_base(storage.get_storage_context(empty()).unwrap())
        //     .unwrap()
        //     .expect("cannot open merk");
        // let m = merk.get(&9_u64.to_be_bytes());
        // let merk.get(&8_u64.to_be_bytes());
        // dbg!(m);
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
            let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
                .unwrap()
                .expect("cannot open merk");
            let batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
                .unwrap()
                .unwrap();
            let mut tree = merk.tree.take().unwrap();
            let walker = RefWalker::new(&mut tree, merk.source());

            let mut nodes = vec![];
            collect(walker, &mut nodes);
            nodes
        };

        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
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
            let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
                .unwrap()
                .expect("cannot open merk");
            let batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
                .unwrap()
                .unwrap();

            let mut nodes = vec![];
            collect(&mut merk.storage.raw_iter(), &mut nodes);
            nodes
        };
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
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
        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
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
            .get(b"10".as_slice())
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
            .get(b"10".as_slice())
            .unwrap()
            .expect("should get successfully");
        assert_eq!(result, Some(b"b".to_vec()));

        drop(merk);

        let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
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
            .get(b"10".as_slice())
            .unwrap()
            .expect("should get successfully");
        assert_eq!(result, Some(b"c".to_vec()));
    }
}
