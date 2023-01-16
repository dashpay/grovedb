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

//! A hierarchical "grove" of trees with proofs and secondary indexes.

#![deny(missing_docs)]

#[cfg(feature = "full")]
extern crate core;

#[cfg(feature = "full")]
pub mod batch;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod element;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod error;
#[cfg(feature = "full")]
mod estimated_costs;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod operations;
#[cfg(any(feature = "full", feature = "verify"))]
mod query;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod query_result_type;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod reference_path;
#[cfg(feature = "full")]
mod replication;
#[cfg(feature = "full")]
#[cfg(test)]
mod tests;
#[cfg(feature = "full")]
mod util;
#[cfg(feature = "full")]
mod visualize;

#[cfg(feature = "full")]
use std::{collections::HashMap, option::Option::None, path::Path};

#[cfg(feature = "full")]
use ::visualize::DebugByteVectors;
#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use element::helpers::raw_decode;
#[cfg(any(feature = "full", feature = "verify"))]
pub use element::Element;
#[cfg(feature = "full")]
pub use element::ElementFlags;
#[cfg(feature = "full")]
use merk::{
    self,
    tree::{combine_hash, value_hash},
    BatchEntry, CryptoHash, KVIterator, Merk,
};
#[cfg(feature = "full")]
pub use merk::{
    estimated_costs::{
        average_case_costs::{
            EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes, EstimatedSumTrees,
        },
        worst_case_costs::WorstCaseLayerInformation,
    },
    proofs::{query::QueryItem, Query},
};
#[cfg(any(feature = "full", feature = "verify"))]
pub use query::{PathQuery, SizedQuery};
#[cfg(feature = "full")]
pub use replication::{BufferedRestorer, Restorer, SiblingsChunkProducer, SubtreeChunkProducer};
#[cfg(any(feature = "full", feature = "verify"))]
pub use storage::rocksdb_storage::RocksDbStorage;
#[cfg(feature = "full")]
pub use storage::{
    rocksdb_storage::{self},
    Storage, StorageContext,
};
#[cfg(feature = "full")]
use storage::{
    rocksdb_storage::{
        PrefixedRocksDbBatchTransactionContext, PrefixedRocksDbStorageContext,
        PrefixedRocksDbTransactionContext,
    },
    StorageBatch,
};

#[cfg(any(feature = "full", feature = "verify"))]
pub use crate::error::Error;
#[cfg(feature = "full")]
use crate::util::{root_merk_optional_tx, storage_context_optional_tx};

#[cfg(feature = "full")]
type Hash = [u8; 32];

/// GroveDb
#[cfg(any(feature = "full", feature = "verify"))]
pub struct GroveDb {
    db: RocksDbStorage,
}

/// Transaction
#[cfg(feature = "full")]
pub type Transaction<'db> = <RocksDbStorage as Storage<'db>>::Transaction;
/// TransactionArg
#[cfg(feature = "full")]
pub type TransactionArg<'db, 'a> = Option<&'a Transaction<'db>>;

#[cfg(feature = "full")]
impl GroveDb {
    /// Opens a given path
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = RocksDbStorage::default_rocksdb_with_path(path)?;
        Ok(GroveDb { db })
    }

    /// Opens the transactional Merk at the given path. Returns CostResult.
    pub fn open_transactional_merk_at_path<'db, 'p, P>(
        &'db self,
        path: P,
        tx: &'db Transaction,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + Clone,
    {
        let mut path_iter = path.into_iter();
        let mut cost = OperationCost::default();
        let storage = self
            .db
            .get_transactional_storage_context(path_iter.clone(), tx)
            .unwrap_add_cost(&mut cost);
        match path_iter.next_back() {
            Some(key) => {
                let parent_storage = self
                    .db
                    .get_transactional_storage_context(path_iter.clone(), tx)
                    .unwrap_add_cost(&mut cost);
                let element = cost_return_on_error!(
                    &mut cost,
                    Element::get_from_storage(&parent_storage, key).map_err(|e| {
                        Error::InvalidParentLayerPath(format!(
                            "could not get key {} for parent {:?} of subtree: {}",
                            hex::encode(key),
                            DebugByteVectors(path_iter.clone().map(|x| x.to_vec()).collect()),
                            e
                        ))
                    })
                );
                let is_sum_tree = element.is_sum_tree();
                if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                    Merk::open_layered_with_root_key(storage, root_key, is_sum_tree)
                        .map_err(|_| {
                            Error::CorruptedData(
                                "cannot open a subtree with given root key".to_owned(),
                            )
                        })
                        .add_cost(cost)
                } else {
                    Err(Error::CorruptedPath(
                        "cannot open a subtree as parent exists but is not a tree",
                    ))
                    .wrap_with_cost(cost)
                }
            }
            None => Merk::open_base(storage, false)
                .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned()))
                .add_cost(cost),
        }
    }

    /// Opens the non-transactional Merk at the given path. Returns CostResult.
    pub fn open_non_transactional_merk_at_path<'p, P>(
        &self,
        path: P,
    ) -> CostResult<Merk<PrefixedRocksDbStorageContext>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + Clone,
    {
        let mut path_iter = path.into_iter();
        let mut cost = OperationCost::default();
        let storage = self
            .db
            .get_storage_context(path_iter.clone())
            .unwrap_add_cost(&mut cost);
        match path_iter.next_back() {
            Some(key) => {
                let parent_storage = self
                    .db
                    .get_storage_context(path_iter.clone())
                    .unwrap_add_cost(&mut cost);
                let element = cost_return_on_error!(
                    &mut cost,
                    Element::get_from_storage(&parent_storage, key).map_err(|e| {
                        Error::InvalidParentLayerPath(format!(
                            "could not get key {} for parent {:?} of subtree: {}",
                            hex::encode(key),
                            DebugByteVectors(path_iter.clone().map(|x| x.to_vec()).collect()),
                            e
                        ))
                    })
                );
                let is_sum_tree = element.is_sum_tree();
                if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                    Merk::open_layered_with_root_key(storage, root_key, is_sum_tree)
                        .map_err(|_| {
                            Error::CorruptedData(
                                "cannot open a subtree with given root key".to_owned(),
                            )
                        })
                        .add_cost(cost)
                } else {
                    Err(Error::CorruptedPath(
                        "cannot open a subtree as parent exists but is not a tree",
                    ))
                    .wrap_with_cost(cost)
                }
            }
            None => Merk::open_base(storage, false)
                .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned()))
                .add_cost(cost),
        }
    }

    /// Creates a checkpoint
    pub fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        self.db.create_checkpoint(path).map_err(|e| e.into())
    }

    /// Returns root key of GroveDb.
    /// Will be `None` if GroveDb is empty.
    pub fn root_key(&self, transaction: TransactionArg) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost {
            ..Default::default()
        };

        root_merk_optional_tx!(&mut cost, self.db, transaction, subtree, {
            let root_key = subtree.root_key().unwrap();
            Ok(root_key).wrap_with_cost(cost)
        })
    }

    /// Returns root hash of GroveDb.
    /// Will be `None` if GroveDb is empty.
    pub fn root_hash(&self, transaction: TransactionArg) -> CostResult<Hash, Error> {
        let mut cost = OperationCost {
            ..Default::default()
        };

        root_merk_optional_tx!(&mut cost, self.db, transaction, subtree, {
            let root_hash = subtree.root_hash().unwrap_add_cost(&mut cost);
            Ok(root_hash).wrap_with_cost(cost)
        })
    }

    /// Method to propagate updated subtree key changes one level up inside a
    /// transaction
    fn propagate_changes_with_batch_transaction<'p, P>(
        &self,
        storage_batch: &StorageBatch,
        mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbBatchTransactionContext>>,
        path: P,
        transaction: &Transaction,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();

        let mut child_tree = cost_return_on_error_no_add!(
            &cost,
            merk_cache
                .remove(
                    path_iter
                        .clone()
                        .map(|k| k.to_vec())
                        .collect::<Vec<Vec<u8>>>()
                        .as_slice()
                )
                .ok_or(Error::CorruptedCodeExecution(
                    "Merk Cache should always contain the last path",
                ))
        );

        while path_iter.len() > 0 {
            let key = path_iter.next_back().expect("next element is `Some`");
            let mut parent_tree = cost_return_on_error!(
                &mut cost,
                self.open_batch_transactional_merk_at_path(
                    storage_batch,
                    path_iter.clone(),
                    transaction,
                    false
                )
            );
            let (root_hash, root_key, sum) = cost_return_on_error!(
                &mut cost,
                child_tree.root_hash_key_and_sum().map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(
                    &mut parent_tree,
                    key,
                    root_key,
                    root_hash,
                    sum
                )
            );
            child_tree = parent_tree;
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree key changes one level up inside a
    /// transaction
    fn propagate_changes_with_transaction<'p, P>(
        &self,
        mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbTransactionContext>>,
        path: P,
        transaction: &Transaction,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();

        let mut child_tree = cost_return_on_error_no_add!(
            &cost,
            merk_cache
                .remove(
                    path_iter
                        .clone()
                        .map(|k| k.to_vec())
                        .collect::<Vec<Vec<u8>>>()
                        .as_slice()
                )
                .ok_or(Error::CorruptedCodeExecution(
                    "Merk Cache should always contain the last path",
                ))
        );

        while path_iter.len() > 0 {
            let key = path_iter.next_back().expect("next element is `Some`");
            let mut parent_tree: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(path_iter.clone(), transaction)
            );
            let (root_hash, root_key, sum) = cost_return_on_error!(
                &mut cost,
                child_tree.root_hash_key_and_sum().map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(
                    &mut parent_tree,
                    key,
                    root_key,
                    root_hash,
                    sum
                )
            );
            child_tree = parent_tree;
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree key changes one level up
    fn propagate_changes_without_transaction<'p, P>(
        &self,
        mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbStorageContext>>,
        path: P,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();

        let mut child_tree = cost_return_on_error_no_add!(
            &cost,
            merk_cache
                .remove(
                    path_iter
                        .clone()
                        .map(|k| k.to_vec())
                        .collect::<Vec<Vec<u8>>>()
                        .as_slice()
                )
                .ok_or(Error::CorruptedCodeExecution(
                    "Merk Cache should always contain the last path",
                ))
        );

        while path_iter.len() > 0 {
            let key = path_iter.next_back().expect("next element is `Some`");
            let mut parent_tree: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
                &mut cost,
                self.open_non_transactional_merk_at_path(path_iter.clone())
            );
            let (root_hash, root_key, sum) = cost_return_on_error!(
                &mut cost,
                child_tree.root_hash_key_and_sum().map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(
                    &mut parent_tree,
                    key,
                    root_key,
                    root_hash,
                    sum
                )
            );
            child_tree = parent_tree;
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Updates a tree item and preserves flags. Returns CostResult.
    pub(crate) fn update_tree_item_preserve_flag<
        'db,
        K: AsRef<[u8]> + Copy,
        S: StorageContext<'db>,
    >(
        parent_tree: &mut Merk<S>,
        key: K,
        maybe_root_key: Option<Vec<u8>>,
        root_tree_hash: Hash,
        sum: Option<i64>,
    ) -> CostResult<(), Error> {
        Self::get_element_from_subtree(parent_tree, key).flat_map_ok(|element| {
            if let Element::Tree(_, flag) = element {
                let tree = Element::new_tree_with_flags(maybe_root_key, flag);
                tree.insert_subtree(parent_tree, key.as_ref(), root_tree_hash, None)
            } else if let Element::SumTree(.., flag) = element {
                let tree = Element::new_sum_tree_with_flags_and_sum_value(
                    maybe_root_key,
                    sum.unwrap_or_default(),
                    flag,
                );
                tree.insert_subtree(parent_tree, key.as_ref(), root_tree_hash, None)
            } else {
                Err(Error::InvalidPath(
                    "can only propagate on tree items".to_owned(),
                ))
                .wrap_with_cost(Default::default())
            }
        })
    }

    /// Pushes to batch an operation which updates a tree item and preserves
    /// flags. Returns CostResult.
    pub(crate) fn update_tree_item_preserve_flag_into_batch_operations<
        'db,
        K: AsRef<[u8]>,
        S: StorageContext<'db>,
    >(
        parent_tree: &Merk<S>,
        key: K,
        maybe_root_key: Option<Vec<u8>>,
        root_tree_hash: Hash,
        sum: Option<i64>,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        Self::get_element_from_subtree(parent_tree, key.as_ref()).flat_map_ok(|element| {
            if let Element::Tree(_, flag) = element {
                let tree = Element::new_tree_with_flags(maybe_root_key, flag);
                let merk_feature_type = cost_return_on_error!(
                    &mut cost,
                    tree.get_feature_type(parent_tree.is_sum_tree)
                        .wrap_with_cost(OperationCost::default())
                );
                tree.insert_subtree_into_batch_operations(
                    key,
                    root_tree_hash,
                    true,
                    batch_operations,
                    merk_feature_type,
                )
            } else if let Element::SumTree(.., flag) = element {
                let tree = Element::new_sum_tree_with_flags_and_sum_value(
                    maybe_root_key,
                    sum.unwrap_or_default(),
                    flag,
                );
                let merk_feature_type = cost_return_on_error!(
                    &mut cost,
                    tree.get_feature_type(parent_tree.is_sum_tree)
                        .wrap_with_cost(OperationCost::default())
                );
                tree.insert_subtree_into_batch_operations(
                    key,
                    root_tree_hash,
                    true,
                    batch_operations,
                    merk_feature_type,
                )
            } else {
                Err(Error::InvalidPath(
                    "can only propagate on tree items".to_owned(),
                ))
                .wrap_with_cost(Default::default())
            }
        })
    }

    /// Get element from subtree. Return CostResult.
    fn get_element_from_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        subtree: &Merk<S>,
        key: K,
    ) -> CostResult<Element, Error> {
        subtree
            .get(key.as_ref(), true)
            .map_err(|_| {
                Error::InvalidPath("can't find subtree in parent during propagation".to_owned())
            })
            .map_ok(|subtree_opt| {
                subtree_opt.ok_or_else(|| {
                    let key = hex::encode(key.as_ref());
                    Error::PathKeyNotFound(format!(
                        "can't find subtree with key {} in parent during propagation (subtree is \
                         {})",
                        key,
                        if subtree.root_key().is_some() {
                            "not empty"
                        } else {
                            "empty"
                        }
                    ))
                })
            })
            .flatten()
            .map_ok(|element_bytes| {
                Element::deserialize(&element_bytes).map_err(|_| {
                    Error::CorruptedData(
                        "failed to deserialized parent during propagation".to_owned(),
                    )
                })
            })
            .flatten()
    }

    /// Flush memory table to disk.
    pub fn flush(&self) -> Result<(), Error> {
        Ok(self.db.flush()?)
    }

    /// Starts database transaction. Please note that you have to start
    /// underlying storage transaction manually.
    ///
    /// ## Examples:
    /// ```
    /// # use grovedb::{Element, Error, GroveDb};
    /// # use std::convert::TryFrom;
    /// # use tempfile::TempDir;
    /// #
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::option::Option::None;
    /// const TEST_LEAF: &[u8] = b"test_leaf";
    ///
    /// let tmp_dir = TempDir::new().unwrap();
    /// let mut db = GroveDb::open(tmp_dir.path())?;
    /// db.insert([], TEST_LEAF, Element::empty_tree(), None, None)
    ///     .unwrap()?;
    ///
    /// let tx = db.start_transaction();
    ///
    /// let subtree_key = b"subtree_key";
    /// db.insert(
    ///     [TEST_LEAF],
    ///     subtree_key,
    ///     Element::empty_tree(),
    ///     None,
    ///     Some(&tx),
    /// )
    /// .unwrap()?;
    ///
    /// // This action exists only inside the transaction for now
    /// let result = db.get([TEST_LEAF], subtree_key, None).unwrap();
    /// assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
    ///
    /// // To access values inside the transaction, transaction needs to be passed to the `db::get`
    /// let result_with_transaction = db.get([TEST_LEAF], subtree_key, Some(&tx)).unwrap()?;
    /// assert_eq!(result_with_transaction, Element::empty_tree());
    ///
    /// // After transaction is committed, the value from it can be accessed normally.
    /// db.commit_transaction(tx);
    /// let result = db.get([TEST_LEAF], subtree_key, None).unwrap()?;
    /// assert_eq!(result, Element::empty_tree());
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn start_transaction(&self) -> Transaction {
        self.db.start_transaction()
    }

    /// Commits previously started db transaction. For more details on the
    /// transaction usage, please check [`GroveDb::start_transaction`]
    pub fn commit_transaction(&self, transaction: Transaction) -> CostResult<(), Error> {
        self.db.commit_transaction(transaction).map_err(Into::into)
    }

    /// Rollbacks previously started db transaction to initial state.
    /// For more details on the transaction usage, please check
    /// [`GroveDb::start_transaction`]
    pub fn rollback_transaction(&self, transaction: &Transaction) -> Result<(), Error> {
        Ok(self.db.rollback_transaction(transaction)?)
    }

    /// Method to visualize hash mismatch after verification
    pub fn visualize_verify_grovedb(&self) -> HashMap<String, (String, String, String)> {
        self.verify_grovedb()
            .iter()
            .map(|(path, (root_hash, expected, actual))| {
                (
                    path.iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                        .join("/"),
                    (
                        hex::encode(root_hash),
                        hex::encode(expected),
                        hex::encode(actual),
                    ),
                )
            })
            .collect()
    }

    /// Method to check that the value_hash of Element::Tree nodes are computed
    /// correctly.
    pub fn verify_grovedb(&self) -> HashMap<Vec<Vec<u8>>, (CryptoHash, CryptoHash, CryptoHash)> {
        let root_merk = self
            .open_non_transactional_merk_at_path([])
            .unwrap()
            .expect("should exist");
        self.verify_merk_and_submerks(root_merk, vec![])
    }

    /// Verifies that the root hash of the given merk and all submerks match
    /// those of the merk and submerks at the given path. Returns any issues.
    fn verify_merk_and_submerks(
        &self,
        merk: Merk<PrefixedRocksDbStorageContext>,
        path: Vec<Vec<u8>>,
    ) -> HashMap<Vec<Vec<u8>>, (CryptoHash, CryptoHash, CryptoHash)> {
        let mut all_query = Query::new();
        all_query.insert_all();

        let _in_sum_tree = merk.is_sum_tree;
        let mut issues = HashMap::new();
        let mut element_iterator = KVIterator::new(merk.storage.raw_iter(), &all_query).unwrap();
        while let Some((key, element_value)) = element_iterator.next_kv().unwrap() {
            let element = raw_decode(&element_value).unwrap();
            if element.is_tree() {
                let (kv_value, element_value_hash) = merk
                    .get_value_and_value_hash(&key, true)
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let mut new_path = path.clone();
                new_path.push(key.to_vec());

                let inner_merk = self
                    .open_non_transactional_merk_at_path(new_path.iter().map(|x| x.as_slice()))
                    .unwrap()
                    .expect("should exist");
                let root_hash = inner_merk.root_hash().unwrap();

                let actual_value_hash = value_hash(&kv_value).unwrap();
                let combined_value_hash = combine_hash(&actual_value_hash, &root_hash).unwrap();

                if combined_value_hash != element_value_hash {
                    issues.insert(
                        new_path.clone(),
                        (root_hash, combined_value_hash, element_value_hash),
                    );
                }
                issues.extend(self.verify_merk_and_submerks(inner_merk, new_path));
            }
        }
        issues
    }
}
