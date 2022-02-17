mod operations;
mod subtree;
mod subtrees;
#[cfg(test)]
mod tests;
#[cfg(feature = "visualize")]
mod visualize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    rc::Rc,
};

pub use merk::proofs::{query::QueryItem, Query};
use merk::{self, Merk};
use rs_merkle::{algorithms::Sha256, MerkleTree};
use serde::{Deserialize, Serialize};
use storage::rocksdb_storage::{OptimisticTransactionDBTransaction, PrefixedRocksDbStorageError};
pub use storage::{rocksdb_storage::PrefixedRocksDbStorage, Storage, Transaction};
pub use subtree::Element;
use subtrees::Subtrees;
#[cfg(feature = "visualize")]
pub use visualize::{visualize_stderr, visualize_stdout, Drawer, Visualize};

/// A key to store serialized data about subtree prefixes to restore HADS
/// structure
/// A key to store serialized data about root tree leafs keys and order
const ROOT_LEAFS_SERIALIZED_KEY: &[u8] = b"rootLeafsSerialized";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("cyclic reference path")]
    CyclicReference,
    #[error("reference hops limit exceeded")]
    ReferenceLimit,
    #[error("internal error: {0}")]
    InternalError(&'static str),
    #[error("invalid proof: {0}")]
    InvalidProof(&'static str),

    // Path errors

    // The path key not found could represent a valid query, just where the path key isn't there
    #[error("path key not found: {0}")]
    PathKeyNotFound(String),
    // The path not found could represent a valid query, just where the path isn't there
    #[error("path not found: {0}")]
    PathNotFound(&'static str),
    // The invalid path represents a logical error from the client library
    #[error("invalid path: {0}")]
    InvalidPath(&'static str),
    // The corrupted path represents a consistency error in internal groveDB logic
    #[error("corrupted path: {0}")]
    CorruptedPath(&'static str),

    // Query errors
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

#[derive(Debug)]
pub struct PathQuery {
    // TODO: Make generic over path type
    path: Vec<Vec<u8>>,
    query: SizedQuery,
}

// If a subquery exists :
// limit should be applied to the elements returned by the subquery
// offset should be applied to the first item that will subqueried (first in the
// case of a range)
#[derive(Debug)]
pub struct SizedQuery {
    query: Query,
    limit: Option<u16>,
    offset: Option<u16>,
}

impl SizedQuery {
    pub const fn new(query: Query, limit: Option<u16>, offset: Option<u16>) -> SizedQuery {
        SizedQuery {
            query,
            limit,
            offset,
        }
    }
}

impl PathQuery {
    pub const fn new(path: Vec<Vec<u8>>, query: SizedQuery) -> PathQuery {
        PathQuery { path, query }
    }

    pub const fn new_unsized(path: Vec<Vec<u8>>, query: Query) -> PathQuery {
        let query = SizedQuery::new(query, None, None);
        PathQuery { path, query }
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
    root_leaf_keys: BTreeMap<Vec<u8>, usize>,
    meta_storage: PrefixedRocksDbStorage,
    db: Rc<storage::rocksdb_storage::OptimisticTransactionDB>,
    // Locks the database for writes during the transaction
    is_readonly: bool,
    // Temp trees used for writes during transaction
    temp_root_tree: MerkleTree<Sha256>,
    temp_root_leaf_keys: BTreeMap<Vec<u8>, usize>,
    temp_subtrees: RefCell<HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>>,
    temp_deleted_subtrees: RefCell<HashSet<Vec<u8>>>,
}

impl GroveDb {
    pub fn new(
        root_tree: MerkleTree<Sha256>,
        root_leaf_keys: BTreeMap<Vec<u8>, usize>,
        meta_storage: PrefixedRocksDbStorage,
        db: Rc<storage::rocksdb_storage::OptimisticTransactionDB>,
    ) -> Self {
        Self {
            root_tree,
            root_leaf_keys,
            meta_storage,
            db,
            temp_root_tree: MerkleTree::new(),
            temp_root_leaf_keys: BTreeMap::new(),
            temp_subtrees: RefCell::new(HashMap::new()),
            temp_deleted_subtrees: RefCell::new(HashSet::new()),
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

        // TODO: owned `get` is not required for deserialization
        let root_leaf_keys: BTreeMap<Vec<u8>, usize> = if let Some(root_leaf_keys_serialized) =
            meta_storage.get_meta(ROOT_LEAFS_SERIALIZED_KEY)?
        {
            bincode::deserialize(&root_leaf_keys_serialized).map_err(|_| {
                Error::CorruptedData(String::from("unable to deserialize root leafs"))
            })?
        } else {
            BTreeMap::new()
        };

        let temp_subtrees: RefCell<HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>> =
            RefCell::new(HashMap::new());
        let subtrees_view = Subtrees {
            root_leaf_keys: &root_leaf_keys,
            temp_subtrees: &temp_subtrees,
            deleted_subtrees: &RefCell::new(HashSet::new()),
            storage: db.clone(),
        };

        Ok(GroveDb::new(
            Self::build_root_tree(&subtrees_view, &root_leaf_keys, None),
            root_leaf_keys,
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

    fn build_root_tree(
        subtrees: &Subtrees,
        root_leaf_keys: &BTreeMap<Vec<u8>, usize>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> MerkleTree<Sha256> {
        let mut leaf_hashes: Vec<[u8; 32]> = vec![[0; 32]; root_leaf_keys.len()];
        for (subtree_path, root_leaf_idx) in root_leaf_keys {
            leaf_hashes[*root_leaf_idx] = subtrees
                .borrow_mut([subtree_path.as_slice()], transaction)
                .expect("`root_leaf_keys` must be in sync with `subtrees`")
                .apply(|s| s.root_hash());
        }
        MerkleTree::<Sha256>::from_leaves(&leaf_hashes)
    }

    fn store_root_leafs_keys_data(
        &self,
        db_transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(), Error> {
        match db_transaction {
            None => {
                self.meta_storage.put_meta(
                    ROOT_LEAFS_SERIALIZED_KEY,
                    &bincode::serialize(&self.root_leaf_keys).map_err(|_| {
                        Error::CorruptedData(String::from("unable to serialize root leaves data"))
                    })?,
                )?;
            }
            Some(tx) => {
                let transaction = self.meta_storage.transaction(tx);
                transaction.put_meta(
                    ROOT_LEAFS_SERIALIZED_KEY,
                    &bincode::serialize(&self.temp_root_leaf_keys).map_err(|_| {
                        Error::CorruptedData(String::from("unable to serialize root leaves data"))
                    })?,
                )?;
            }
        }

        Ok(())
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn propagate_changes<'a: 'b, 'b, 'c, P>(
        &'a mut self,
        path: P,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'c [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let subtrees = self.get_subtrees();

        // Go up until only one element in path, which means a key of a root tree
        let mut path_iter = path.into_iter();

        while path_iter.len() > 1 {
            // non root leaf node
            let element = subtrees
                .borrow_mut(path_iter.clone(), transaction)?
                .apply(|s| Element::Tree(s.root_hash()));

            let key = path_iter.next_back().expect("next element is `Some`");

            subtrees
                .borrow_mut(path_iter.clone(), transaction)?
                .apply(|s| element.insert(s, key.as_ref(), transaction))?;
        }

        let root_leaf_keys = match transaction {
            None => &self.root_leaf_keys,
            Some(_) => &self.temp_root_leaf_keys,
        };
        let root_tree = GroveDb::build_root_tree(&subtrees, root_leaf_keys, transaction);
        match transaction {
            None => self.root_tree = root_tree,
            Some(_) => self.temp_root_tree = root_tree,
        }
        self.store_root_leafs_keys_data(transaction)?;
        Ok(())
    }

    fn get_subtrees(&self) -> Subtrees {
        Subtrees {
            root_leaf_keys: &self.root_leaf_keys,
            temp_subtrees: &self.temp_subtrees,
            deleted_subtrees: &self.temp_deleted_subtrees,
            storage: self.storage(),
        }
    }

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    fn compress_subtree_key<'a, P>(path: P, key: Option<&'a [u8]>) -> Vec<u8>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let segments_iter = path.into_iter().chain(key.into_iter());
        let mut segments_count: usize = 0;
        let mut res = Vec::new();
        let mut lengthes = Vec::new();

        for s in segments_iter {
            segments_count += 1;
            res.extend_from_slice(s);
            lengthes.extend(s.len().to_ne_bytes());
        }

        res.extend(segments_count.to_ne_bytes());
        res.extend(lengthes);
        res = blake3::hash(&res).as_bytes().to_vec();
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
    /// db.insert([], TEST_LEAF, Element::empty_tree(), None)?;
    ///
    /// let storage = db.storage();
    /// let db_transaction = storage.transaction();
    /// db.start_transaction();
    ///
    /// let subtree_key = b"subtree_key";
    /// db.insert(
    ///     [TEST_LEAF],
    ///     subtree_key,
    ///     Element::empty_tree(),
    ///     Some(&db_transaction),
    /// )?;
    ///
    /// // This action exists only inside the transaction for now
    /// let result = db.get([TEST_LEAF], subtree_key, None);
    /// assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
    ///
    /// // To access values inside the transaction, transaction needs to be passed to the `db::get`
    /// let result_with_transaction = db.get([TEST_LEAF], subtree_key, Some(&db_transaction))?;
    /// assert_eq!(result_with_transaction, Element::empty_tree());
    ///
    /// // After transaction is committed, the value from it can be accessed normally.
    /// db.commit_transaction(db_transaction);
    /// let result = db.get([TEST_LEAF], subtree_key, None)?;
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

        Ok(())
    }

    /// Returns true if transaction is started. For more details on the
    /// transaction usage, please check [`GroveDb::start_transaction`]
    pub const fn is_transaction_started(&self) -> bool {
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

        self.root_leaf_keys = self.temp_root_leaf_keys.clone();

        self.is_readonly = false;

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
        self.cleanup_transactional_data();

        Ok(db_transaction
            .rollback()
            .map_err(PrefixedRocksDbStorageError::RocksDbError)?)
    }

    /// Rollbacks previously started db transaction to initial state.
    /// For more details on the transaction usage, please check
    /// [`GroveDb::start_transaction`]
    pub fn abort_transaction(
        &mut self,
        _db_transaction: OptimisticTransactionDBTransaction,
    ) -> Result<(), Error> {

        // Enabling writes again
        self.is_readonly = false;
        // Cloning all the trees to maintain to rollback transactional changes
        self.cleanup_transactional_data();

        Ok(())
    }

    /// Cleanup transactional data after commit or abort
    fn cleanup_transactional_data(&mut self) {
        // Free transactional data
        self.temp_root_tree = MerkleTree::new();
        self.temp_root_leaf_keys = BTreeMap::new();
        self.temp_subtrees = RefCell::new(HashMap::new());
        self.temp_deleted_subtrees = RefCell::new(HashSet::new());
    }
}
