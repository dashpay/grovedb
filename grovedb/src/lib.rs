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

//! GroveDB is a database that enables cryptographic proofs for complex queries.
//!
//! # Examples
//!
//! ## Open
//! Open an existing instance of GroveDB or create a new one at a given path.
//! ```
//! use grovedb::GroveDb;
//! use tempfile::TempDir;
//!
//! // Specify the path where you want to set up the GroveDB instance
//! let tmp_dir = TempDir::new().unwrap();
//! let path = tmp_dir.path();
//!
//! // Open a new GroveDB at the path
//! let db = GroveDb::open(&path).unwrap();
//! ```
//!
//! ## Basic Operations
//! Insert, Update, Delete and Prove elements.
//! ```
//! use grovedb::{Element, GroveDb};
//! use tempfile::TempDir;
//!
//! // Specify the path where you want to set up the GroveDB instance
//! let tmp_dir = TempDir::new().unwrap();
//! let path = tmp_dir.path();
//!
//! // Open a new GroveDB at the path
//! let db = GroveDb::open(&path).unwrap();
//!
//! let root_path: &[&[u8]] = &[];
//!
//! // Insert new tree to root
//! db.insert(root_path, b"tree1", Element::empty_tree(), None, None)
//!     .unwrap()
//!     .expect("successful tree insert");
//!
//! // Insert key-value 1 into tree1
//! // key - hello, value - world
//! db.insert(
//!     &[b"tree1"],
//!     b"hello",
//!     Element::new_item(b"world".to_vec()),
//!     None,
//!     None,
//! )
//! .unwrap()
//! .expect("successful key1 insert");
//!
//! // Insert key-value 2 into tree1
//! // key - grovedb, value = rocks
//! db.insert(
//!     &[b"tree1"],
//!     b"grovedb",
//!     Element::new_item(b"rocks".to_vec()),
//!     None,
//!     None,
//! )
//! .unwrap()
//! .expect("successful key2 insert");
//!
//! // Retrieve inserted elements
//! let elem = db
//!     .get(&[b"tree1"], b"hello", None)
//!     .unwrap()
//!     .expect("successful get");
//! assert_eq!(elem, Element::new_item(b"world".to_vec()));
//!
//! let elem = db
//!     .get(&[b"tree1"], b"grovedb", None)
//!     .unwrap()
//!     .expect("successful get");
//! assert_eq!(elem, Element::new_item(b"rocks".to_vec()));
//!
//! // Update inserted element
//! // for non-tree elements, insertion to an already existing key updates it
//! db.insert(
//!     &[b"tree1"],
//!     b"hello",
//!     Element::new_item(b"WORLD".to_vec()),
//!     None,
//!     None,
//! )
//! .unwrap()
//! .expect("successful update");
//!
//! // Retrieve updated element
//! let elem = db
//!     .get(&[b"tree1"], b"hello", None)
//!     .unwrap()
//!     .expect("successful get");
//! assert_eq!(elem, Element::new_item(b"WORLD".to_vec()));
//!
//! // Deletion
//! db.delete(&[b"tree1"], b"hello", None, None)
//!     .unwrap()
//!     .expect("successful delete");
//! let elem_result = db.get(&[b"tree1"], b"hello", None).unwrap();
//! assert_eq!(elem_result.is_err(), true);
//!
//! // State Root
//! // Get the GroveDB root hash
//! let root_hash = db.root_hash(None).unwrap().unwrap();
//! assert_eq!(
//!     hex::encode(root_hash),
//!     "3884be3d197ac49981e54b21ea423351fc4ccdb770aaf7cf40f5e65dc3e2e1aa"
//! );
//! ```
//!
//! For more documentation see our
//! [Architectural Decision Records](https://github.com/dashpay/grovedb/tree/master/adr) or
//! [Tutorial](https://www.grovedb.org/tutorials.html)

#[cfg(feature = "full")]
extern crate core;

#[cfg(feature = "full")]
pub mod batch;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod element;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod error;
#[cfg(feature = "estimated_costs")]
mod estimated_costs;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod operations;
#[cfg(any(feature = "full", feature = "verify"))]
mod query;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod query_result_type;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod reference_path;
#[cfg(all(test, feature = "full"))]
mod tests;
#[cfg(feature = "full")]
mod util;
mod versioning;
#[cfg(feature = "full")]
mod visualize;

#[cfg(feature = "full")]
use std::{collections::HashMap, option::Option::None, path::Path, fmt};
use std::collections::{BTreeMap, BTreeSet, LinkedList, VecDeque};
use std::marker::PhantomData;
use itertools::Chunk;

#[cfg(any(feature = "full", feature = "verify"))]
use element::helpers;
#[cfg(any(feature = "full", feature = "verify"))]
pub use element::Element;
#[cfg(feature = "full")]
pub use element::ElementFlags;
#[cfg(feature = "full")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;
#[cfg(feature = "estimated_costs")]
pub use grovedb_merk::estimated_costs::{
    average_case_costs::{
        EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes, EstimatedSumTrees,
    },
    worst_case_costs::WorstCaseLayerInformation,
};
#[cfg(any(feature = "full", feature = "verify"))]
pub use grovedb_merk::proofs::query::query_item::QueryItem;
#[cfg(any(feature = "full", feature = "verify"))]
pub use grovedb_merk::proofs::Query;
#[cfg(feature = "full")]
use grovedb_merk::tree::kv::ValueDefinedCostType;
#[cfg(feature = "full")]
use grovedb_merk::{
    self,
    tree::{combine_hash, value_hash},
    BatchEntry, CryptoHash, KVIterator, Merk,
};
use grovedb_merk::{ChunkProducer, Restorer, TreeFeatureType};
use grovedb_merk::Error::ChunkingError;
use grovedb_merk::proofs::{Node, Op};
use grovedb_merk::proofs::chunk::error::ChunkError;
use grovedb_merk::proofs::chunk::util::{generate_traversal_instruction_as_string, number_of_chunks};
use grovedb_merk::tree::kv_digest_to_kv_hash;
use grovedb_path::SubtreePath;
#[cfg(feature = "full")]
use grovedb_storage::rocksdb_storage::PrefixedRocksDbImmediateStorageContext;
#[cfg(feature = "full")]
use grovedb_storage::rocksdb_storage::RocksDbStorage;
#[cfg(feature = "full")]
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext},
    StorageBatch,
};
#[cfg(feature = "full")]
use grovedb_storage::{Storage, StorageContext};
#[cfg(feature = "full")]
use grovedb_visualize::DebugByteVectors;
#[cfg(any(feature = "full", feature = "verify"))]
pub use query::{PathQuery, SizedQuery};

#[cfg(any(feature = "full", feature = "verify"))]
pub use crate::error::Error;
#[cfg(feature = "full")]
use crate::helpers::raw_decode;
#[cfg(feature = "full")]
use crate::util::{root_merk_optional_tx, storage_context_optional_tx};
use crate::Error::MerkError;

#[cfg(feature = "full")]
type Hash = [u8; 32];

/// GroveDb
pub struct GroveDb {
    #[cfg(feature = "full")]
    db: RocksDbStorage,
}

pub struct s_db_snapshot {
    pub root_hash: CryptoHash,
    pub data: Vec<(String, Vec<Op>)>
}

impl s_db_snapshot {
    pub fn new() -> s_db_snapshot {
        s_db_snapshot {
            root_hash: CryptoHash::default(),
            data: Vec::new(),
        }
    }
}

impl fmt::Debug for s_db_snapshot {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "root_hash:{:?}\n", hex::encode(self.root_hash));
        for (global_chunk_id, _) in self.data.iter() {
            write!(f, " global_chunk_id:{:?}\n", global_chunk_id);
        }
        Ok(())
    }
}

pub struct s_db_snapshot_sorted {
    pub root_hash: CryptoHash,
    pub data: BTreeMap<String, Vec<(String, Vec<Op>)>>
}

impl s_db_snapshot_sorted {
    pub fn new() -> s_db_snapshot_sorted {
        s_db_snapshot_sorted {
            root_hash: CryptoHash::default(),
            data: BTreeMap::new(),
        }
    }
}

impl fmt::Debug for s_db_snapshot_sorted {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "root_hash:{:?}\n", hex::encode(self.root_hash));
        for (prefix, chunk_vec) in self.data.iter() {
            write!(f, " prefix:{:?}\n", prefix);
            for (chunk_id, _) in chunk_vec.iter() {
                write!(f, "     chunk_id:{:?}\n", chunk_id);
            }
        }
        Ok(())
    }
}

pub struct s_subtrees_metadata {
    pub data: BTreeMap<String, (Vec<Vec<u8>>, CryptoHash, CryptoHash)>
}

impl s_subtrees_metadata {
    pub fn new() -> s_subtrees_metadata {
        s_subtrees_metadata {
            data: BTreeMap::new(),
        }
    }
}

impl fmt::Debug for s_subtrees_metadata {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (prefix, metadata) in self.data.iter() {
            let metadata_path = &metadata.0;
            let metadata_path_str = s_util_path_to_string(&metadata_path);
            let metadata_hash_0 = &metadata.1;
            let metadata_hash_1 = &metadata.2;
            write!(f, " prefix:{:?} -> path:{:?} ({:?}:{:?})\n", prefix, metadata_path_str, hex::encode(metadata_hash_0), hex::encode(metadata_hash_1));
        }
        Ok(())
    }
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

    /// Uses raw iter to delete GroveDB key values pairs from rocksdb
    pub fn wipe(&self) -> Result<(), Error> {
        self.db.wipe()?;
        Ok(())
    }

    /// Opens the transactional Merk at the given path. Returns CostResult.
    fn open_transactional_merk_at_path<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        tx: &'db Transaction,
        batch: Option<&'db StorageBatch>,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();

        let storage = self
            .db
            .get_transactional_storage_context(path.clone(), batch, tx)
            .unwrap_add_cost(&mut cost);
        if let Some((parent_path, parent_key)) = path.derive_parent() {
            let parent_storage = self
                .db
                .get_transactional_storage_context(parent_path.clone(), batch, tx)
                .unwrap_add_cost(&mut cost);
            let element = cost_return_on_error!(
                &mut cost,
                Element::get_from_storage(&parent_storage, parent_key).map_err(|e| {
                    Error::InvalidParentLayerPath(format!(
                        "could not get key {} for parent {:?} of subtree: {}",
                        hex::encode(parent_key),
                        DebugByteVectors(parent_path.to_vec()),
                        e
                    ))
                })
            );
            let is_sum_tree = element.is_sum_tree();
            if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                Merk::open_layered_with_root_key(
                    storage,
                    root_key,
                    is_sum_tree,
                    Some(&Element::value_defined_cost_for_serialized_value),
                )
                .map_err(|_| {
                    Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                })
                .add_cost(cost)
            } else {
                Err(Error::CorruptedPath(
                    "cannot open a subtree as parent exists but is not a tree",
                ))
                .wrap_with_cost(cost)
            }
        } else {
            Merk::open_base(
                storage,
                false,
                Some(&Element::value_defined_cost_for_serialized_value),
            )
            .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned()))
            .add_cost(cost)
        }
    }

    /// Opens a Merk at given path for with direct write access. Intended for
    /// replication purposes.
    fn open_merk_for_replication<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        tx: &'db Transaction,
    ) -> Result<Merk<PrefixedRocksDbImmediateStorageContext<'db>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();

        let storage = self
            .db
            .get_immediate_storage_context(path.clone(), tx)
            .unwrap_add_cost(&mut cost);
        if let Some((parent_path, parent_key)) = path.derive_parent() {
            let parent_storage = self
                .db
                .get_immediate_storage_context(parent_path.clone(), tx)
                .unwrap_add_cost(&mut cost);
            let element = Element::get_from_storage(&parent_storage, parent_key)
                .map_err(|e| {
                    Error::InvalidParentLayerPath(format!(
                        "could not get key {} for parent {:?} of subtree: {}",
                        hex::encode(parent_key),
                        DebugByteVectors(parent_path.to_vec()),
                        e
                    ))
                })
                .unwrap()?;
            let is_sum_tree = element.is_sum_tree();
            if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                Merk::open_layered_with_root_key(
                    storage,
                    root_key,
                    is_sum_tree,
                    Some(&Element::value_defined_cost_for_serialized_value),
                )
                .map_err(|_| {
                    Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                })
                .unwrap()
            } else {
                Err(Error::CorruptedPath(
                    "cannot open a subtree as parent exists but is not a tree",
                ))
            }
        } else {
            Merk::open_base(
                storage,
                false,
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned()))
            .unwrap()
        }
    }

    /// Opens the non-transactional Merk at the given path. Returns CostResult.
    fn open_non_transactional_merk_at_path<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        batch: Option<&'db StorageBatch>,
    ) -> CostResult<Merk<PrefixedRocksDbStorageContext>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();

        let storage = self
            .db
            .get_storage_context(path.clone(), batch)
            .unwrap_add_cost(&mut cost);

        if let Some((parent_path, parent_key)) = path.derive_parent() {
            let parent_storage = self
                .db
                .get_storage_context(parent_path.clone(), batch)
                .unwrap_add_cost(&mut cost);
            let element = cost_return_on_error!(
                &mut cost,
                Element::get_from_storage(&parent_storage, parent_key).map_err(|e| {
                    Error::InvalidParentLayerPath(format!(
                        "could not get key {} for parent {:?} of subtree: {}",
                        hex::encode(parent_key),
                        DebugByteVectors(parent_path.to_vec()),
                        e
                    ))
                })
            );
            let is_sum_tree = element.is_sum_tree();
            if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                Merk::open_layered_with_root_key(
                    storage,
                    root_key,
                    is_sum_tree,
                    Some(&Element::value_defined_cost_for_serialized_value),
                )
                .map_err(|_| {
                    Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                })
                .add_cost(cost)
            } else {
                Err(Error::CorruptedPath(
                    "cannot open a subtree as parent exists but is not a tree",
                ))
                .wrap_with_cost(cost)
            }
        } else {
            Merk::open_base(
                storage,
                false,
                Some(&Element::value_defined_cost_for_serialized_value),
            )
            .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned()))
            .add_cost(cost)
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

        root_merk_optional_tx!(&mut cost, self.db, None, transaction, subtree, {
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

        root_merk_optional_tx!(&mut cost, self.db, None, transaction, subtree, {
            let root_hash = subtree.root_hash().unwrap_add_cost(&mut cost);
            Ok(root_hash).wrap_with_cost(cost)
        })
    }

    /// Method to propagate updated subtree key changes one level up inside a
    /// transaction
    fn propagate_changes_with_batch_transaction<'b, B: AsRef<[u8]>>(
        &self,
        storage_batch: &StorageBatch,
        mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>>,
        path: &SubtreePath<'b, B>,
        transaction: &Transaction,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut child_tree = cost_return_on_error_no_add!(
            &cost,
            merk_cache.remove(path).ok_or(Error::CorruptedCodeExecution(
                "Merk Cache should always contain the last path",
            ))
        );

        let mut current_path = path.clone();

        while let Some((parent_path, parent_key)) = current_path.derive_parent() {
            let mut parent_tree = cost_return_on_error!(
                &mut cost,
                self.open_batch_transactional_merk_at_path(
                    storage_batch,
                    parent_path.clone(),
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
                    parent_key,
                    root_key,
                    root_hash,
                    sum
                )
            );
            child_tree = parent_tree;
            current_path = parent_path;
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree key changes one level up inside a
    /// transaction
    fn propagate_changes_with_transaction<'b, B: AsRef<[u8]>>(
        &self,
        mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>>,
        path: SubtreePath<'b, B>,
        transaction: &Transaction,
        batch: &StorageBatch,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut child_tree = cost_return_on_error_no_add!(
            &cost,
            merk_cache
                .remove(&path)
                .ok_or(Error::CorruptedCodeExecution(
                    "Merk Cache should always contain the last path",
                ))
        );

        let mut current_path = path.clone();

        while let Some((parent_path, parent_key)) = current_path.derive_parent() {
            let mut parent_tree: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(parent_path.clone(), transaction, Some(batch))
            );
            let (root_hash, root_key, sum) = cost_return_on_error!(
                &mut cost,
                child_tree.root_hash_key_and_sum().map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(
                    &mut parent_tree,
                    parent_key,
                    root_key,
                    root_hash,
                    sum
                )
            );
            child_tree = parent_tree;
            current_path = parent_path;
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree key changes one level up
    fn propagate_changes_without_transaction<'b, B: AsRef<[u8]>>(
        &self,
        mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbStorageContext>>,
        path: SubtreePath<'b, B>,
        batch: &StorageBatch,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut child_tree = cost_return_on_error_no_add!(
            &cost,
            merk_cache
                .remove(&path)
                .ok_or(Error::CorruptedCodeExecution(
                    "Merk Cache should always contain the last path",
                ))
        );

        let mut current_path: SubtreePath<B> = path;

        while let Some((parent_path, parent_key)) = current_path.derive_parent() {
            let mut parent_tree: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
                &mut cost,
                self.open_non_transactional_merk_at_path(parent_path.clone(), Some(batch))
            );
            let (root_hash, root_key, sum) = cost_return_on_error!(
                &mut cost,
                child_tree.root_hash_key_and_sum().map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(
                    &mut parent_tree,
                    parent_key,
                    root_key,
                    root_hash,
                    sum
                )
            );
            child_tree = parent_tree;
            current_path = parent_path;
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Updates a tree item and preserves flags. Returns CostResult.
    pub(crate) fn update_tree_item_preserve_flag<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        parent_tree: &mut Merk<S>,
        key: K,
        maybe_root_key: Option<Vec<u8>>,
        root_tree_hash: Hash,
        sum: Option<i64>,
    ) -> CostResult<(), Error> {
        let key_ref = key.as_ref();

        Self::get_element_from_subtree(parent_tree, key_ref).flat_map_ok(|element| {
            if let Element::Tree(_, flag) = element {
                let tree = Element::new_tree_with_flags(maybe_root_key, flag);
                tree.insert_subtree(parent_tree, key_ref, root_tree_hash, None)
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
            .get(
                key.as_ref(),
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
            )
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
    /// # use grovedb_path::SubtreePath;
    /// #
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::option::Option::None;
    /// const TEST_LEAF: &[u8] = b"test_leaf";
    ///
    /// let tmp_dir = TempDir::new().unwrap();
    /// let mut db = GroveDb::open(tmp_dir.path())?;
    /// db.insert(
    ///     SubtreePath::empty(),
    ///     TEST_LEAF,
    ///     Element::empty_tree(),
    ///     None,
    ///     None,
    /// )
    /// .unwrap()?;
    ///
    /// let tx = db.start_transaction();
    ///
    /// let subtree_key = b"subtree_key";
    /// db.insert(
    ///     [TEST_LEAF].as_ref(),
    ///     subtree_key,
    ///     Element::empty_tree(),
    ///     None,
    ///     Some(&tx),
    /// )
    /// .unwrap()?;
    ///
    /// // This action exists only inside the transaction for now
    /// let result = db.get([TEST_LEAF].as_ref(), subtree_key, None).unwrap();
    /// assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
    ///
    /// // To access values inside the transaction, transaction needs to be passed to the `db::get`
    /// let result_with_transaction = db
    ///     .get([TEST_LEAF].as_ref(), subtree_key, Some(&tx))
    ///     .unwrap()?;
    /// assert_eq!(result_with_transaction, Element::empty_tree());
    ///
    /// // After transaction is committed, the value from it can be accessed normally.
    /// db.commit_transaction(tx);
    /// let result = db.get([TEST_LEAF].as_ref(), subtree_key, None).unwrap()?;
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
    pub fn visualize_verify_grovedb(
        &self,
    ) -> Result<HashMap<String, (String, String, String)>, Error> {
        Ok(self
            .verify_grovedb(None)?
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
            .collect())
    }

    /// Method to check that the value_hash of Element::Tree nodes are computed
    /// correctly.
    pub fn verify_grovedb(
        &self,
        transaction: TransactionArg,
    ) -> Result<HashMap<Vec<Vec<u8>>, (CryptoHash, CryptoHash, CryptoHash)>, Error> {
        if let Some(transaction) = transaction {
            let root_merk = self
                .open_transactional_merk_at_path(SubtreePath::empty(), transaction, None)
                .unwrap()?;
            self.verify_merk_and_submerks_in_transaction(
                root_merk,
                &SubtreePath::empty(),
                None,
                transaction,
            )
        } else {
            let root_merk = self
                .open_non_transactional_merk_at_path(SubtreePath::empty(), None)
                .unwrap()?;
            self.verify_merk_and_submerks(root_merk, &SubtreePath::empty(), None)
        }
    }

    /// Verifies that the root hash of the given merk and all submerks match
    /// those of the merk and submerks at the given path. Returns any issues.
    fn verify_merk_and_submerks<'db, B: AsRef<[u8]>, S: StorageContext<'db>>(
        &'db self,
        merk: Merk<S>,
        path: &SubtreePath<B>,
        batch: Option<&'db StorageBatch>,
    ) -> Result<HashMap<Vec<Vec<u8>>, (CryptoHash, CryptoHash, CryptoHash)>, Error> {
        let mut all_query = Query::new();
        all_query.insert_all();

        let _in_sum_tree = merk.is_sum_tree;
        let mut issues = HashMap::new();
        let mut element_iterator = KVIterator::new(merk.storage.raw_iter(), &all_query).unwrap();

        while let Some((key, element_value)) = element_iterator.next_kv().unwrap() {
            let element = raw_decode(&element_value)?;
            if element.is_tree() {
                let (kv_value, element_value_hash) = merk
                    .get_value_and_value_hash(
                        &key,
                        true,
                        None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                    )
                    .unwrap()
                    .map_err(MerkError)?
                    .ok_or(Error::CorruptedData(
                        "expected merk to contain value at key".to_string(),
                    ))?;
                let new_path = path.derive_owned_with_child(key);
                let new_path_ref = SubtreePath::from(&new_path);

                let inner_merk = self
                    .open_non_transactional_merk_at_path(new_path_ref.clone(), batch)
                    .unwrap()?;
                let root_hash = inner_merk.root_hash().unwrap();

                let actual_value_hash = value_hash(&kv_value).unwrap();
                let combined_value_hash = combine_hash(&actual_value_hash, &root_hash).unwrap();

                if combined_value_hash != element_value_hash {
                    issues.insert(
                        new_path.to_vec(),
                        (root_hash, combined_value_hash, element_value_hash),
                    );
                }
                issues.extend(self.verify_merk_and_submerks(inner_merk, &new_path_ref, batch)?);
            } else if element.is_item() {
                let (kv_value, element_value_hash) = merk
                    .get_value_and_value_hash(
                        &key,
                        true,
                        None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                    )
                    .unwrap()
                    .map_err(MerkError)?
                    .ok_or(Error::CorruptedData(
                        "expected merk to contain value at key".to_string(),
                    ))?;
                let actual_value_hash = value_hash(&kv_value).unwrap();
                if actual_value_hash != element_value_hash {
                    issues.insert(
                        path.derive_owned_with_child(key).to_vec(),
                        (actual_value_hash, element_value_hash, actual_value_hash),
                    );
                }
            }
        }
        Ok(issues)
    }

    fn verify_merk_and_submerks_in_transaction<'db, B: AsRef<[u8]>, S: StorageContext<'db>>(
        &'db self,
        merk: Merk<S>,
        path: &SubtreePath<B>,
        batch: Option<&'db StorageBatch>,
        transaction: &Transaction,
    ) -> Result<HashMap<Vec<Vec<u8>>, (CryptoHash, CryptoHash, CryptoHash)>, Error> {
        let mut all_query = Query::new();
        all_query.insert_all();

        let _in_sum_tree = merk.is_sum_tree;
        let mut issues = HashMap::new();
        let mut element_iterator = KVIterator::new(merk.storage.raw_iter(), &all_query).unwrap();

        while let Some((key, element_value)) = element_iterator.next_kv().unwrap() {
            let element = raw_decode(&element_value)?;
            if element.is_tree() {
                let (kv_value, element_value_hash) = merk
                    .get_value_and_value_hash(
                        &key,
                        true,
                        None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                    )
                    .unwrap()
                    .map_err(MerkError)?
                    .ok_or(Error::CorruptedData(
                        "expected merk to contain value at key".to_string(),
                    ))?;
                let new_path = path.derive_owned_with_child(key);
                let new_path_ref = SubtreePath::from(&new_path);

                let inner_merk = self
                    .open_transactional_merk_at_path(new_path_ref.clone(), transaction, batch)
                    .unwrap()?;
                let root_hash = inner_merk.root_hash().unwrap();

                let actual_value_hash = value_hash(&kv_value).unwrap();
                let combined_value_hash = combine_hash(&actual_value_hash, &root_hash).unwrap();

                if combined_value_hash != element_value_hash {
                    issues.insert(
                        new_path.to_vec(),
                        (root_hash, combined_value_hash, element_value_hash),
                    );
                }
                issues.extend(self.verify_merk_and_submerks_in_transaction(
                    inner_merk,
                    &new_path_ref,
                    batch,
                    transaction,
                )?);
            } else if element.is_item() {
                let (kv_value, element_value_hash) = merk
                    .get_value_and_value_hash(
                        &key,
                        true,
                        None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                    )
                    .unwrap()
                    .map_err(MerkError)?
                    .ok_or(Error::CorruptedData(
                        "expected merk to contain value at key".to_string(),
                    ))?;
                let actual_value_hash = value_hash(&kv_value).unwrap();
                if actual_value_hash != element_value_hash {
                    issues.insert(
                        path.derive_owned_with_child(key).to_vec(),
                        (actual_value_hash, element_value_hash, actual_value_hash),
                    );
                }
            }
        }
        Ok(issues)
    }

    pub fn s_create_db_snapshot(
        &self,
        list_only_chunk_ids: bool,
    ) -> Result<s_db_snapshot, Error> {
        let mut db_snapsot = s_db_snapshot::new();

        db_snapsot.root_hash = self.root_hash(None).unwrap().unwrap();

        let subtrees_root = self.find_subtrees(&SubtreePath::empty(), None).unwrap()?;
        for subtree in subtrees_root.into_iter() {
            let subtree_path: Vec<&[u8]> = subtree.iter().map(|vec| vec.as_slice()).collect();
            let path: &[&[u8]] = &subtree_path;

            let continue_storage_batch = StorageBatch::new();

            let prefix = RocksDbStorage::build_prefix(path.as_ref().into()).unwrap();
            let merk = self.open_batch_merk_at_path(&continue_storage_batch, path.into(), false).value.unwrap();

            if (merk.is_empty_tree().unwrap()) {
                continue;
            }

            let mut chunk_producer = ChunkProducer::new(&merk).unwrap();

            let mut chunk_id_opt = Some("".to_string());
            while let Some(chunk_id) = chunk_id_opt {
                let (chunk, next_chunk_id) = chunk_producer.chunk(chunk_id.as_str()).unwrap();

                let global_chunk_id = hex::encode(prefix) + &chunk_id;
                if (list_only_chunk_ids) {
                    db_snapsot.data.push((global_chunk_id, vec![]));
                }
                else {
                    db_snapsot.data.push((global_chunk_id, chunk));
                }

                chunk_id_opt = next_chunk_id;
            }
        }

        Ok(db_snapsot)
    }

    fn s_sort_db_snapshot(
        &self,
        snapshot: s_db_snapshot,
    ) -> Result<s_db_snapshot_sorted, Error> {
        let mut db_snapsot_sorted = s_db_snapshot_sorted::new();
        db_snapsot_sorted.root_hash = snapshot.root_hash;

        let CHUNK_PREFIX_LENGTH: usize = 64;

        for chunk_entry in snapshot.data {
            let global_chunk_id = chunk_entry.0;
            let chunk_data = chunk_entry.1;

            if (global_chunk_id.len() < CHUNK_PREFIX_LENGTH) {
                return Err(Error::CorruptedData(
                    "expected global chunk id of at least 64 length".to_string(),
                ));
            }

            let chunk_prefix = global_chunk_id.chars().take(CHUNK_PREFIX_LENGTH).collect::<String>();
            let chunk_id = global_chunk_id.chars().skip(CHUNK_PREFIX_LENGTH).collect::<String>();

            db_snapsot_sorted.data.entry(chunk_prefix).or_insert(Vec::new()).push((chunk_id, chunk_data.to_vec()));
        }

        for (_key, vec) in db_snapsot_sorted.data.iter_mut() {
            vec.sort_by(|a, b| a.0.len().cmp(&b.0.len()));
        }

        Ok(db_snapsot_sorted)
    }

    fn s_get_subtrees_metadata<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
    ) -> Result<s_subtrees_metadata, Error> {
        let mut subtrees_metadata = crate::s_subtrees_metadata::new();

        let subtrees_root = self.find_subtrees(&SubtreePath::empty(), None).unwrap().unwrap();
        for subtree in subtrees_root.into_iter() {
            let subtree_path: Vec<&[u8]> = subtree.iter().map(|vec| vec.as_slice()).collect();
            let path: &[&[u8]] = &subtree_path;
            let prefix = RocksDbStorage::build_prefix(path.as_ref().into()).unwrap();

            let current_path = SubtreePath::from(path);

            let parent_path_opt = current_path.derive_parent();
            if (parent_path_opt.is_some()) {
                let parent_path = parent_path_opt.unwrap().0;
                let continue_storage_batch = StorageBatch::new();
                let parent_merk = self.open_batch_merk_at_path(&continue_storage_batch, parent_path, false).value.unwrap();
                let parent_key = subtree.last().unwrap();
                let (elem_value, elem_value_hash) = parent_merk
                    .get_value_and_value_hash(
                        parent_key,
                        true,
                        None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                    )
                    .unwrap()
                    .expect("should get value hash")
                    .expect("value hash should be some");

                let actual_value_hash =  value_hash(&elem_value).unwrap();
                subtrees_metadata.data.insert(hex::encode(prefix), (current_path.to_vec(), actual_value_hash, elem_value_hash));
            }
            else {
                subtrees_metadata.data.insert(hex::encode(prefix), (current_path.to_vec(), CryptoHash::default(), CryptoHash::default()));
            }
        }
        Ok(subtrees_metadata)
    }

    pub fn s_fetch_chunk(
        &self,
        global_chunk_id: String
    ) -> Result<Vec<Op>, Error> {
        let CHUNK_PREFIX_LENGTH: usize = 64;
        if (global_chunk_id.len() < CHUNK_PREFIX_LENGTH) {
            return Err(Error::CorruptedData(
                "expected global chunk id of at least 64 length".to_string(),
            ));
        }

        let chunk_prefix = global_chunk_id.chars().take(CHUNK_PREFIX_LENGTH).collect::<String>();
        let chunk_id = global_chunk_id.chars().skip(CHUNK_PREFIX_LENGTH).collect::<String>();

        let subtrees_metadata = self.s_get_subtrees_metadata(&SubtreePath::empty()).unwrap();

        match subtrees_metadata.data.get(&chunk_prefix) {
            Some(path_data) => {
                let subtree = &path_data.0;
                let subtree_path: Vec<&[u8]> = subtree.iter().map(|vec| vec.as_slice()).collect();
                let path: &[&[u8]] = &subtree_path;

                let continue_storage_batch = StorageBatch::new();
                let merk = self.open_batch_merk_at_path(&continue_storage_batch, path.into(), false).value?;

                if (merk.is_empty_tree().unwrap()) {
                    return Err(Error::CorruptedData(
                        "Empty merk".to_string(),
                    ));
                }

                let mut chunk_producer = ChunkProducer::new(&merk).unwrap();
                let (chunk, _) = chunk_producer.chunk(chunk_id.as_str()).unwrap();
                Ok(chunk)
            },
            None => {
                return Err(Error::CorruptedData(
                    "Prefix not found".to_string(),
                ));
            }
        }
    }

    pub fn s_reconstruct_db(
        &self,
        snapshot: s_db_snapshot
    ) -> Result<(), Error> {
        let mut sorted_snapshot = self.s_sort_db_snapshot(snapshot)?;

        //Always start by empty prefix = root
        if let Some(chunk_vec) = sorted_snapshot.data.remove(&hex::encode(CryptoHash::default())) {
            let tx = self.start_transaction();
            let merk = self.open_merk_for_replication(SubtreePath::empty(), &tx).unwrap();
            let mut restorer = Restorer::new(merk, sorted_snapshot.root_hash, None);
            for (chunk_id, chunk) in chunk_vec {
                restorer.process_chunk(chunk_id, chunk).expect("should process chunk successfully");
            }
            restorer.finalize().expect("should finalize");
            self.commit_transaction(tx);
        } else {
            return Err(Error::CorruptedData(
                "No root prefix chunks found".to_string(),
            ));
        }

        let mut processed_prefixes :BTreeSet<String> = BTreeSet::new();
        processed_prefixes.insert(hex::encode(CryptoHash::default()));

        let mut queue_prefixes_to_be_processed : VecDeque<String> = VecDeque::new();

        let mut subtrees_metadata = self.s_get_subtrees_metadata(&SubtreePath::empty()).unwrap();
        for prefix in subtrees_metadata.data.keys() {
            if !processed_prefixes.contains(prefix) {
                //println!("prefix:{:?} pending for processing", prefix);
                queue_prefixes_to_be_processed.push_back(prefix.to_string());
            }
        }

        while (!queue_prefixes_to_be_processed.is_empty()) {
            while let Some(current_prefix) = queue_prefixes_to_be_processed.pop_front() {
                let prefix_metadata = &subtrees_metadata.data[&current_prefix];
                let s_path = &prefix_metadata.0;
                let s_actual_value_hash = &prefix_metadata.1;
                let s_elem_value_hash = &prefix_metadata.2;
                println!("    about to process{:?} with ({:?}:{:?})", s_util_path_to_string(&s_path), hex::encode(s_actual_value_hash), hex::encode(s_elem_value_hash));

                let subtree_path: Vec<&[u8]> = s_path.iter().map(|vec| vec.as_slice()).collect();
                let path: &[&[u8]] = &subtree_path;

                if let Some(chunk_vec) = sorted_snapshot.data.remove(&current_prefix) {
                    let tx = self.start_transaction();
                    if (chunk_vec.is_empty()) {println!("empty"); }
                    let merk = self.open_merk_for_replication(path.into(), &tx).unwrap();
                    let mut restorer = Restorer::new(merk, *s_elem_value_hash, Some(*s_actual_value_hash));
                    for (chunk_id, chunk) in chunk_vec {
                        restorer.process_chunk(chunk_id, chunk).expect("should process chunk successfully");
                    }
                    restorer.finalize().expect("should finalize");
                    self.commit_transaction(tx);
                } else {
                    println!("    skipping empty {:?}", s_util_path_to_string(&s_path));
                }

                processed_prefixes.insert(current_prefix);
            }

            subtrees_metadata = self.s_get_subtrees_metadata(&SubtreePath::empty()).unwrap();
            for prefix in subtrees_metadata.data.keys() {
                if !processed_prefixes.contains(prefix) {
                    queue_prefixes_to_be_processed.push_back(prefix.to_string());
                }
            }
        }

        if (sorted_snapshot.data.len() > 0) {
            return Err(Error::CorruptedData(
                "Remaining chunks not processed".to_string(),
            ));
        }

        let incorrect_hashes = self.verify_grovedb(None)?;
        if (incorrect_hashes.len() > 0) {
            return Err(Error::CorruptedData(
                "DB verification failed".to_string(),
            ));
        }

        Ok(())
    }
}

pub fn s_util_path_to_string(
    path: &Vec<Vec<u8>>,
) -> Vec<String> {
    let mut subtree_path_str: Vec<String> = vec![];
    for subtree in path.to_vec() {
        let string = std::str::from_utf8(&subtree).unwrap();
        subtree_path_str.push(string.parse().unwrap());
    }
    subtree_path_str
}
