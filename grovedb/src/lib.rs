extern crate core;

pub mod batch;
pub mod error;
mod operations;
mod query;
pub mod query_result_type;
pub mod reference_path;
mod replication;
mod subtree;
#[cfg(test)]
mod tests;
mod util;
mod visualize;
mod worst_case_costs;

use std::path::Path;

use costs::{cost_return_on_error, CostContext, CostResult, CostsExt, OperationCost};
pub use merk::proofs::{query::QueryItem, Query};
use merk::{self, BatchEntry, Merk};
pub use query::{PathQuery, SizedQuery};
pub use replication::{BufferedRestorer, Restorer, SiblingsChunkProducer, SubtreeChunkProducer};
use storage::rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext};
pub use storage::{
    rocksdb_storage::{self, RocksDbStorage},
    Storage, StorageContext,
};
pub use subtree::{Element, ElementFlags};

pub use crate::error::Error;
use crate::util::root_merk_optional_tx;

// todo: remove this
const MAX_ELEMENTS_NUMBER: u32 = 42069;
type Hash = [u8; 32];

pub struct GroveDb {
    db: RocksDbStorage,
}

pub type Transaction<'db> = <RocksDbStorage as Storage<'db>>::Transaction;
pub type TransactionArg<'db, 'a> = Option<&'a Transaction<'db>>;

impl GroveDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = RocksDbStorage::default_rocksdb_with_path(path)?;
        Ok(GroveDb { db })
    }

    pub fn open_transactional_merk_at_path<'db, 'p, P>(
        &'db self,
        path: P,
        tx: &'db Transaction,
    ) -> CostContext<Result<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>>
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
                    Element::get_from_storage(&parent_storage, key).map_err(|_| {
                        Error::CorruptedData("could not get key for parent of subtree".to_owned())
                    })
                );
                if let Element::Tree(root_key, _) = element {
                    Merk::open_with_root_key(storage, root_key).map_err(|_| {
                        Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                    })
                } else {
                    Err(Error::CorruptedData(
                        "cannot open a subtree as parent exists but is not a tree".to_owned(),
                    ))
                    .wrap_with_cost(OperationCost::default())
                }
            }
            None => Merk::open_base(storage)
                .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned())),
        }
    }

    pub fn open_non_transactional_merk_at_path<'p, P>(
        &self,
        path: P,
    ) -> CostContext<Result<Merk<PrefixedRocksDbStorageContext>, Error>>
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
                    Element::get_from_storage(&parent_storage, key).map_err(|_| {
                        Error::CorruptedData("could not get key for parent of subtree".to_owned())
                    })
                );
                if let Element::Tree(root_key, _) = element {
                    Merk::open_with_root_key(storage, root_key).map_err(|_| {
                        Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                    })
                } else {
                    Err(Error::CorruptedData(
                        "cannot open a subtree as parent exists but is not a tree".to_owned(),
                    ))
                    .wrap_with_cost(OperationCost::default())
                }
            }
            None => Merk::open_base(storage)
                .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned())),
        }
    }

    // pub fn open_merk_with_parent_at_path<'p, 'db, P, S>(
    //     &self,
    //     path: P,
    //     tx: TransactionArg,
    // ) -> CostContext<Result<(Merk<S>, Merk<S>), Error>>
    // where
    //     P: IntoIterator<Item = &'p [u8]>,
    //     <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator +
    // Clone,     S: StorageContext<'db>,
    //     <S as StorageContext<'db>>::Error: std::error::Error,
    // {
    //     let mut path_iter = path.into_iter();
    //     if let Some(tx) = tx {
    //         let mut cost = OperationCost::default();
    //         let storage = self
    //             .db
    //             .get_transactional_storage_context(path_iter.clone(), tx)
    //             .unwrap_add_cost(&mut cost);
    //         let key = path_iter.next_back().expect("next element is `Some`");
    //         let parent_storage = self
    //             .db
    //             .get_transactional_storage_context(path_iter.clone(), tx)
    //             .unwrap_add_cost(&mut cost);
    //         let parent_key = path_iter.next_back().expect("next element is
    // `Some`");         let grandparent_storage = self
    //             .db
    //             .get_transactional_storage_context(path_iter.clone(), tx)
    //             .unwrap_add_cost(&mut cost);
    //         let element = cost_return_on_error!(
    //             &mut cost,
    //             Element::get_from_storage(&parent_storage, key).map_err(|_| {
    //                 Error::CorruptedData("could not get key for parent of
    // subtree".to_owned())             })
    //         );
    //         let parent_element = cost_return_on_error!(
    //             &mut cost,
    //             Element::get_from_storage(&grandparent_storage,
    // parent_key).map_err(|_| {                 Error::CorruptedData("could not
    // get key for parent of subtree".to_owned())             })
    //         );
    //         let merk = cost_return_on_error!(
    //             &mut cost,
    //             if let Element::Tree(root_key, _) = element {
    //                 Merk::open_with_root_key(storage, root_key).map_err(|_| {
    //                     Error::CorruptedData("cannot open a subtree with given
    // root key".to_owned())                 })
    //             } else {
    //                 Err(Error::CorruptedData(
    //                     "cannot open a subtree as parent exists but is not a
    // tree".to_owned(),                 ))
    //                 .wrap_with_cost(OperationCost::default())
    //             }
    //         );
    //         let parent_merk = cost_return_on_error!(
    //             &mut cost,
    //             if let Element::Tree(root_key, _) = parent_element {
    //                 Merk::open_with_root_key(parent_storage,
    // root_key).map_err(|_| {                     Error::CorruptedData("cannot
    // open a subtree with given root key".to_owned())                 })
    //             } else {
    //                 Err(Error::CorruptedData(
    //                     "cannot open a subtree as parent exists but is not a
    // tree".to_owned(),                 ))
    //                 .wrap_with_cost(OperationCost::default())
    //             }
    //         );
    //         Ok((merk, parent_merk)).wrap_with_cost(cost)
    //     } else {
    //         let mut cost = OperationCost::default();
    //         let storage = self
    //             .db
    //             .get_storage_context(path_iter.clone())
    //             .unwrap_add_cost(&mut cost);
    //         let key = path_iter.next_back().expect("next element is `Some`");
    //         let parent_storage = self
    //             .db
    //             .get_storage_context(path_iter.clone())
    //             .unwrap_add_cost(&mut cost);
    //         let parent_key = path_iter.next_back().expect("next element is
    // `Some`");         let grandparent_storage = self
    //             .db
    //             .get_storage_context(path_iter.clone())
    //             .unwrap_add_cost(&mut cost);
    //         let element = cost_return_on_error!(
    //             &mut cost,
    //             Element::get_from_storage(&parent_storage, key).map_err(|_| {
    //                 Error::CorruptedData("could not get key for parent of
    // subtree".to_owned())             })
    //         );
    //         let parent_element = cost_return_on_error!(
    //             &mut cost,
    //             Element::get_from_storage(&grandparent_storage,
    // parent_key).map_err(|_| {                 Error::CorruptedData("could not
    // get key for parent of subtree".to_owned())             })
    //         );
    //         let merk = cost_return_on_error!(
    //             &mut cost,
    //             if let Element::Tree(root_key, _) = element {
    //                 Merk::open_with_root_key(storage, root_key).map_err(|_| {
    //                     Error::CorruptedData("cannot open a subtree with given
    // root key".to_owned())                 })
    //             } else {
    //                 Err(Error::CorruptedData(
    //                     "cannot open a subtree as parent exists but is not a
    // tree".to_owned(),                 ))
    //                 .wrap_with_cost(OperationCost::default())
    //             }
    //         );
    //         let parent_merk = cost_return_on_error!(
    //             &mut cost,
    //             if let Element::Tree(root_key, _) = parent_element {
    //                 Merk::open_with_root_key(parent_storage,
    // root_key).map_err(|_| {                     Error::CorruptedData("cannot
    // open a subtree with given root key".to_owned())                 })
    //             } else {
    //                 Err(Error::CorruptedData(
    //                     "cannot open a subtree as parent exists but is not a
    // tree".to_owned(),                 ))
    //                 .wrap_with_cost(OperationCost::default())
    //             }
    //         );
    //         Ok((merk, parent_merk)).wrap_with_cost(cost)
    //     }
    // }

    pub fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        self.db.create_checkpoint(path).map_err(|e| e.into())
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
    fn propagate_changes_with_transaction<'p, P>(
        &self,
        child_tree: Merk<PrefixedRocksDbTransactionContext>,
        path: P,
        transaction: &Transaction,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();

        while path_iter.len() > 0 {
            let key = path_iter.next_back().expect("next element is `Some`");
            let mut parent_tree: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(path_iter.clone(), transaction)
            );
            let (root_hash, root_key) = child_tree.root_hash_and_key().unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(&mut parent_tree, key, root_key, root_hash)
            );
            child_tree = parent_tree;
        }
	Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree key changes one level up
    fn propagate_changes_without_transaction<'p, P>(
        &self,
        child_tree: Merk<PrefixedRocksDbStorageContext>,
        path: P,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();

        while path_iter.len() > 0 {
            let key = path_iter.next_back().expect("next element is `Some`");
            let mut parent_tree: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
                &mut cost,
                self.open_non_transactional_merk_at_path(path_iter.clone())
            );
            let (root_hash, root_key) = child_tree.root_hash_and_key().unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(&mut parent_tree, key, root_key, root_hash)
            );
            child_tree = parent_tree;
        }
	Ok(()).wrap_with_cost(cost)
    }

    pub(crate) fn update_tree_item_preserve_flag<
        'db,
        K: AsRef<[u8]> + Copy,
        S: StorageContext<'db>,
    >(
        parent_tree: &mut Merk<S>,
        key: K,
        maybe_root_key: Option<Vec<u8>>,
        root_tree_hash: Hash,
    ) -> CostResult<(), Error> {
        Self::get_element_from_subtree(parent_tree, key).flat_map_ok(|element| {
            if let Element::Tree(_, flag) = element {
                let tree = Element::new_tree_with_flags(maybe_root_key, flag);
                tree.insert_subtree(parent_tree, key.as_ref(), root_tree_hash)
            } else {
                Err(Error::InvalidPath("can only propagate on tree items"))
                    .wrap_with_cost(Default::default())
            }
        })
    }

    pub(crate) fn update_tree_item_preserve_flag_into_batch_operations<
        'db,
        K: AsRef<[u8]>,
        S: StorageContext<'db>,
    >(
        parent_tree: &Merk<S>,
        key: K,
        maybe_root_key: Option<Vec<u8>>,
        root_tree_hash: Hash,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        Self::get_element_from_subtree(parent_tree, key.as_ref()).flat_map_ok(|element| {
            if let Element::Tree(_, flag) = element {
                let tree = Element::new_tree_with_flags(maybe_root_key, flag);
                tree.insert_subtree_into_batch_operations(key, root_tree_hash, batch_operations)
            } else {
                Err(Error::InvalidPath("can only propagate on tree items"))
                    .wrap_with_cost(Default::default())
            }
        })
    }

    fn get_element_from_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        subtree: &Merk<S>,
        key: K,
    ) -> CostResult<Element, Error> {
        subtree
            .get(key.as_ref())
            .map_err(|_| Error::InvalidPath("can't find subtree in parent during propagation"))
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

    pub fn flush(&self) -> Result<(), Error> {
        Ok(self.db.flush()?)
    }

    /// Starts database transaction. Please note that you have to start
    /// underlying storage_cost transaction manually.
    ///
    /// ## Examples:
    /// ```
    /// # use grovedb::{Element, Error, GroveDb};
    /// # use rs_merkle::{MerkleTree, MerkleProof, algorithms::Sha256, Hasher, utils};
    /// # use std::convert::TryFrom;
    /// # use tempfile::TempDir;
    /// #
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// const TEST_LEAF: &[u8] = b"test_leaf";
    ///
    /// let tmp_dir = TempDir::new().unwrap();
    /// let mut db = GroveDb::open(tmp_dir.path())?;
    /// db.insert([], TEST_LEAF, Element::empty_tree(), None)
    ///     .unwrap()?;
    ///
    /// let tx = db.start_transaction();
    ///
    /// let subtree_key = b"subtree_key";
    /// db.insert([TEST_LEAF], subtree_key, Element::empty_tree(), Some(&tx))
    ///     .unwrap()?;
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
}
