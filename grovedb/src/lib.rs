mod operations;
mod subtree;
#[cfg(test)]
mod tests;
mod transaction;
mod subtrees;

use std::{collections::HashMap, path::Path, rc::Rc};

pub use merk::proofs::{query::QueryItem, Query};
use merk::{self, Merk};
use rs_merkle::{algorithms::Sha256, Hasher, MerkleTree};
use serde::{Deserialize, Serialize};
pub use storage::{rocksdb_storage::PrefixedRocksDbStorage, Storage};
use storage::{
    rocksdb_storage::{OptimisticTransactionDBTransaction, PrefixedRocksDbStorageError},
    Transaction,
};
pub use subtree::Element;
use subtrees::Subtrees;

// use crate::transaction::GroveDbTransaction;
// pub use transaction::GroveDbTransaction;

/// A key to store serialized data about subtree prefixes to restore HADS
/// structure
const SUBTREES_SERIALIZED_KEY: &[u8] = b"subtreesSerialized";
/// A key to store serialized data about root tree leafs keys and order
const ROOT_LEAFS_SERIALIZED_KEY: &[u8] = b"rootLeafsSerialized";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("cyclic reference path")]
    CyclicReference,
    #[error("reference hops limit exceeded")]
    ReferenceLimit,
    #[error("invalid proof: {0}")]
    InvalidProof(&'static str),
    #[error("invalid path: {0}")]
    InvalidPath(&'static str),
    #[error("invalid query: {0}")]
    InvalidQuery(&'static str),
    #[error("missing parameter: {0}")]
    MissingParameter(&'static str),
    // Irrecoverable errors
    #[error("storage error: {0}")]
    StorageError(#[from] PrefixedRocksDbStorageError),
    #[error("data corruption error: {0}")]
    CorruptedData(String),
    #[error(
        "db is in readonly mode due to the active transaction. Please provide transaction or \
         commit it"
    )]
    DbIsInReadonlyMode,
}

pub struct PathQuery<'a> {
    path: &'a [&'a [u8]],
    query: SizedQuery,
    subquery_key: Option<Vec<u8>>,
    subquery: Option<Query>,
}

// If a subquery exists :
// limit should be applied to the elements returned by the subquery
// offset should be applied to the first item that will subqueried (first in the
// case of a range)
pub struct SizedQuery {
    query: Query,
    limit: Option<u16>,
    offset: Option<u16>,
    left_to_right: bool,
}

impl SizedQuery {
    pub fn new(
        query: Query,
        limit: Option<u16>,
        offset: Option<u16>,
        left_to_right: bool,
    ) -> SizedQuery {
        SizedQuery {
            query,
            limit,
            offset,
            left_to_right,
        }
    }
}

impl PathQuery<'_> {
    pub fn new<'a>(
        path: &'a [&'a [u8]],
        query: SizedQuery,
        subquery_key: Option<Vec<u8>>,
        subquery: Option<Query>,
    ) -> PathQuery<'a> {
        PathQuery {
            path,
            query,
            subquery_key,
            subquery,
        }
    }

    pub fn new_unsized<'a>(
        path: &'a [&'a [u8]],
        query: Query,
        subquery_key: Option<Vec<u8>>,
        subquery: Option<Query>,
    ) -> PathQuery<'a> {
        let query = SizedQuery::new(query, None, None, true);
        PathQuery {
            path,
            query,
            subquery_key,
            subquery,
        }
    }

    pub fn new_unsized_basic<'a>(path: &'a [&'a [u8]], query: Query) -> PathQuery<'a> {
        let query = SizedQuery::new(query, None, None, true);
        PathQuery {
            path,
            query,
            subquery_key: None,
            subquery: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Proof {
    query_paths: Vec<Vec<Vec<u8>>>,
    proofs: HashMap<Vec<u8>, Vec<u8>>,
    root_proof: Vec<u8>,
    root_leaf_keys: HashMap<Vec<u8>, usize>,
}

pub struct GroveDb {
    root_tree: MerkleTree<Sha256>,
    root_leaf_keys: HashMap<Vec<u8>, usize>,
    subtrees: HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
    meta_storage: PrefixedRocksDbStorage,
    db: Rc<storage::rocksdb_storage::OptimisticTransactionDB>,
    // Locks the database for writes during the transaction
    is_readonly: bool,
    // Temp trees used for writes during transaction
    temp_root_tree: MerkleTree<Sha256>,
    temp_root_leaf_keys: HashMap<Vec<u8>, usize>,
    temp_subtrees: HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
}

impl GroveDb {
    pub fn new(
        root_tree: MerkleTree<Sha256>,
        root_leaf_keys: HashMap<Vec<u8>, usize>,
        subtrees: HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
        meta_storage: PrefixedRocksDbStorage,
        db: Rc<storage::rocksdb_storage::OptimisticTransactionDB>,
    ) -> Self {
        Self {
            root_tree,
            root_leaf_keys,
            subtrees,
            meta_storage,
            db,
            temp_root_tree: MerkleTree::new(),
            temp_root_leaf_keys: HashMap::new(),
            temp_subtrees: HashMap::new(),
            is_readonly: false,
        }
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = Rc::new(
            storage::rocksdb_storage::OptimisticTransactionDB::open_cf_descriptors(
                &storage::rocksdb_storage::default_db_opts(),
                path,
                storage::rocksdb_storage::column_families(),
            )
            .map_err(Into::<PrefixedRocksDbStorageError>::into)?,
        );
        let meta_storage = PrefixedRocksDbStorage::new(db.clone(), Vec::new())?;

        let mut subtrees = HashMap::new();
        // TODO: owned `get` is not required for deserialization
        if let Some(prefixes_serialized) = meta_storage.get_meta(SUBTREES_SERIALIZED_KEY)? {
            let subtrees_prefixes: Vec<Vec<u8>> = bincode::deserialize(&prefixes_serialized)
                .map_err(|_| {
                    Error::CorruptedData(String::from("unable to deserialize prefixes"))
                })?;
            for prefix in subtrees_prefixes {
                let subtree_merk =
                    Merk::open(PrefixedRocksDbStorage::new(db.clone(), prefix.to_vec())?)
                        .map_err(|e| Error::CorruptedData(e.to_string()))?;
                subtrees.insert(prefix.to_vec(), subtree_merk);
            }
        }

        // TODO: owned `get` is not required for deserialization
        let root_leaf_keys: HashMap<Vec<u8>, usize> = if let Some(root_leaf_keys_serialized) =
            meta_storage.get_meta(ROOT_LEAFS_SERIALIZED_KEY)?
        {
            bincode::deserialize(&root_leaf_keys_serialized).map_err(|_| {
                Error::CorruptedData(String::from("unable to deserialize root leafs"))
            })?
        } else {
            HashMap::new()
        };

        Ok(GroveDb::new(
            Self::build_root_tree(&subtrees, &root_leaf_keys),
            root_leaf_keys,
            subtrees,
            meta_storage,
            db,
        ))
    }

    // TODO: Checkpoints are currently not implemented for the transactional DB
    // pub fn checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<GroveDb, Error> {
    //     // let snapshot = self.db.transaction().snapshot();
    //
    //     storage::rocksdb_storage::Checkpoint::new(&self.db)
    //         .and_then(|x| x.create_checkpoint(&path))
    //         .map_err(PrefixedRocksDbStorageError::RocksDbError)?;
    //     GroveDb::open(path)
    // }

    /// Returns root hash of GroveDb.
    /// Will be `None` if GroveDb is empty.
    pub fn root_hash(
        &self,
        db_transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Option<[u8; 32]> {
        if db_transaction.is_some() {
            self.temp_root_tree.root()
        } else {
            self.root_tree.root()
        }
    }

    fn store_subtrees_keys_data(
        &self,
        db_transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(), Error> {
        let subtrees = match db_transaction {
            None => &self.subtrees,
            Some(_) => &self.temp_subtrees,
        };

        let prefixes: Vec<Vec<u8>> = subtrees.keys().cloned().collect();

        // TODO: make StorageOrTransaction which will has the access to either storage
        // or transaction
        match db_transaction {
            None => {
                self.meta_storage.put_meta(
                    SUBTREES_SERIALIZED_KEY,
                    &bincode::serialize(&prefixes).map_err(|_| {
                        Error::CorruptedData(String::from("unable to serialize prefixes"))
                    })?,
                )?;
                self.meta_storage.put_meta(
                    ROOT_LEAFS_SERIALIZED_KEY,
                    &bincode::serialize(&self.temp_root_leaf_keys).map_err(|_| {
                        Error::CorruptedData(String::from("unable to serialize root leafs"))
                    })?,
                )?;
            }
            Some(tx) => {
                let transaction = self.meta_storage.transaction(tx);
                transaction.put_meta(
                    SUBTREES_SERIALIZED_KEY,
                    &bincode::serialize(&prefixes).map_err(|_| {
                        Error::CorruptedData(String::from("unable to serialize prefixes"))
                    })?,
                )?;
                transaction.put_meta(
                    ROOT_LEAFS_SERIALIZED_KEY,
                    &bincode::serialize(&self.root_leaf_keys).map_err(|_| {
                        Error::CorruptedData(String::from("unable to serialize root leafs"))
                    })?,
                )?;
            }
        }

        Ok(())
    }

    fn build_root_tree(
        subtrees: &HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
        root_leaf_keys: &HashMap<Vec<u8>, usize>,
    ) -> MerkleTree<Sha256> {
        let mut leaf_hashes: Vec<[u8; 32]> = vec![[0; 32]; root_leaf_keys.len()];
        for (subtree_path, root_leaf_idx) in root_leaf_keys {
            let subtree_merk = subtrees
                .get(subtree_path)
                .expect("`root_leaf_keys` must be in sync with `subtrees`");
            leaf_hashes[*root_leaf_idx] = subtree_merk.root_hash();
        }
        MerkleTree::<Sha256>::from_leaves(&leaf_hashes)
    }

    pub fn elements_iterator(
        &self,
        path: &[&[u8]],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<subtree::ElementsIterator, Error> {
        let subtrees = match transaction {
            None => &self.subtrees,
            Some(_) => &self.temp_subtrees,
        };

        let merk = subtrees
            .get(&Self::compress_subtree_key(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        Ok(Element::iterator(merk.raw_iter()))
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn propagate_changes<'a: 'b, 'b>(
        &'a mut self,
        path: &[&[u8]],
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error> {
        let subtrees = match transaction {
            None => &mut self.subtrees,
            Some(_) => &mut self.temp_subtrees,
        };

        let root_leaf_keys = match transaction {
            None => &mut self.root_leaf_keys,
            Some(_) => &mut self.temp_root_leaf_keys,
        };

        let mut split_path = path.split_last();
        // Go up until only one element in path, which means a key of a root tree
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // Hit the root tree
                match transaction {
                    None => self.root_tree = Self::build_root_tree(subtrees, root_leaf_keys),
                    Some(_) => {
                        self.temp_root_tree = Self::build_root_tree(subtrees, root_leaf_keys)
                    }
                };
                break;
            } else {
                let compressed_path_upper_tree = Self::compress_subtree_key(path_slice, None);
                let compressed_path_subtree = Self::compress_subtree_key(path_slice, Some(key));
                let subtree = subtrees
                    .get(&compressed_path_subtree)
                    .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                let element = Element::Tree(subtree.root_hash());
                let upper_tree = subtrees
                    .get_mut(&compressed_path_upper_tree)
                    .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                element.insert(upper_tree, key.to_vec(), transaction)?;
                split_path = path_slice.split_last();
            }
        }
        Ok(())
    }

    fn get_subtrees(&self) -> Subtrees {
       Subtrees{
           root_leaf_keys: &self.root_leaf_keys,
           temp_subtrees: &self.temp_subtrees,
           storage: self.storage(),
       }
    }

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    fn compress_subtree_key(path: &[&[u8]], key: Option<&[u8]>) -> Vec<u8> {
        let segments_iter = path.iter().copied().chain(key.into_iter());
        let mut segments_count = path.len();
        if key.is_some() {
            segments_count += 1;
        }
        let mut res = segments_iter.fold(Vec::<u8>::new(), |mut acc, p| {
            acc.extend(p.iter());
            acc
        });

        res.extend(segments_count.to_ne_bytes());
        path.iter()
            .copied()
            .chain(key.into_iter())
            .fold(&mut res, |acc, p| {
                acc.extend(p.len().to_ne_bytes());
                acc
            });
        res = Sha256::hash(&res).to_vec();
        res
    }

    pub fn flush(&self) -> Result<(), Error> {
        Ok(self.meta_storage.flush()?)
    }

    /// Returns a clone of reference counter to the underlying db storage.
    /// Useful when working with transactions. For more details, please
    /// refer to the [`GroveDb::start_transaction`] examples section.
    pub fn storage(&self) -> Rc<storage::rocksdb_storage::OptimisticTransactionDB> {
        self.db.clone()
    }

    /// Starts database transaction. Please note that you have to start
    /// underlying storage transaction manually.
    ///
    /// ## Examples:
    /// ```
    /// # use grovedb::{Element, Error, GroveDb};
    /// # use rs_merkle::{MerkleTree, MerkleProof, algorithms::Sha256, Hasher, utils};
    /// # use std::convert::TryFrom;
    /// # use tempdir::TempDir;
    /// #
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// const TEST_LEAF: &[u8] = b"test_leaf";
    ///
    /// let tmp_dir = TempDir::new("db").unwrap();
    /// let mut db = GroveDb::open(tmp_dir.path())?;
    /// db.insert(&[], TEST_LEAF.to_vec(), Element::empty_tree(), None)?;
    ///
    /// let storage = db.storage();
    /// let db_transaction = storage.transaction();
    /// db.start_transaction();
    ///
    /// let subtree_key = b"subtree_key".to_vec();
    /// db.insert(
    ///     &[TEST_LEAF],
    ///     subtree_key.clone(),
    ///     Element::empty_tree(),
    ///     Some(&db_transaction),
    /// )?;
    ///
    /// // This action exists only inside the transaction for now
    /// let result = db.get(&[TEST_LEAF], &subtree_key, None);
    /// assert!(matches!(result, Err(Error::InvalidPath(_))));
    ///
    /// // To access values inside the transaction, transaction needs to be passed to the `db::get`
    /// let result_with_transaction = db.get(&[TEST_LEAF], &subtree_key, Some(&db_transaction))?;
    /// assert_eq!(result_with_transaction, Element::empty_tree());
    ///
    /// // After transaction is committed, the value from it can be accessed normally.
    /// db.commit_transaction(db_transaction);
    /// let result = db.get(&[TEST_LEAF], &subtree_key, None)?;
    /// assert_eq!(result, Element::empty_tree());
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn start_transaction(&mut self) -> Result<(), Error> {
        if self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }
        // Locking all writes outside of the transaction
        self.is_readonly = true;

        // Cloning all the trees to maintain original state before the transaction
        self.temp_root_tree = self.root_tree.clone();
        self.temp_root_leaf_keys = self.root_leaf_keys.clone();
        self.temp_subtrees = self.subtrees.clone();

        Ok(())
    }

    /// Returns true if transaction is started. For more details on the
    /// transaction usage, please check [`GroveDb::start_transaction`]
    pub fn is_transaction_started(&self) -> bool {
        self.is_readonly
    }

    /// Commits previously started db transaction. For more details on the
    /// transaction usage, please check [`GroveDb::start_transaction`]
    pub fn commit_transaction(
        &mut self,
        db_transaction: OptimisticTransactionDBTransaction,
    ) -> Result<(), Error> {
        // Copying all changes that were made during the transaction into the db

        // TODO: root tree actually does support transactions, so this
        //  code can be reworked to account for that
        self.root_tree = self.temp_root_tree.clone();
        self.root_leaf_keys = self.temp_root_leaf_keys.drain().collect();
        self.subtrees = self.temp_subtrees.drain().collect();

        self.cleanup_transactional_data();

        Ok(db_transaction
            .commit()
            .map_err(PrefixedRocksDbStorageError::RocksDbError)?)
    }

    /// Rollbacks previously started db transaction to initial state.
    /// For more details on the transaction usage, please check
    /// [`GroveDb::start_transaction`]
    pub fn rollback_transaction(
        &mut self,
        db_transaction: &OptimisticTransactionDBTransaction,
    ) -> Result<(), Error> {
        // Cloning all the trees to maintain to rollback transactional changes
        self.temp_root_tree = self.root_tree.clone();
        self.temp_root_leaf_keys = self.root_leaf_keys.clone();
        self.temp_subtrees = self.subtrees.clone();

        Ok(db_transaction
            .rollback()
            .map_err(PrefixedRocksDbStorageError::RocksDbError)?)
    }

    /// Rollbacks previously started db transaction to initial state.
    /// For more details on the transaction usage, please check
    /// [`GroveDb::start_transaction`]
    pub fn abort_transaction(
        &mut self,
        db_transaction: OptimisticTransactionDBTransaction,
    ) -> Result<(), Error> {
        // Cloning all the trees to maintain to rollback transactional changes
        self.cleanup_transactional_data();

        Ok(())
    }

    /// Cleanup transactional data after commit or abort
    fn cleanup_transactional_data(&mut self) {
        // Enabling writes again
        self.is_readonly = false;

        // Free transactional data
        self.temp_root_tree = MerkleTree::new();
        self.temp_root_leaf_keys = HashMap::new();
        self.temp_subtrees = HashMap::new();
    }
}
