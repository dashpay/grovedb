mod operations;
mod subtree;
mod util;
// mod subtrees;
#[cfg(test)]
mod tests;
#[cfg(feature = "visualize")]
mod visualize;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

pub use merk::proofs::{query::QueryItem, Query};
use merk::{self, Merk};
use rs_merkle::{algorithms::Sha256, MerkleTree};
use serde::{Deserialize, Serialize};
use storage::{
    rocksdb_storage::{self, RocksDbStorage},
    Storage, StorageContext,
};
pub use subtree::Element;
// use subtrees::Subtrees;
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
    StorageError(#[from] rocksdb_storage::Error),
    #[error("data corruption error: {0}")]
    CorruptedData(String),
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
    // root_tree: MerkleTree<Sha256>,
    // root_leaf_keys: BTreeMap<Vec<u8>, usize>,
    db: RocksDbStorage,
}

type Transaction<'db> = <RocksDbStorage as Storage<'db>>::Transaction;
type TransactionArg<'db, 'a> = Option<&'a Transaction<'db>>;

impl GroveDb {
    // pub fn new(
    //     // root_tree: MerkleTree<Sha256>,
    //     // root_leaf_keys: BTreeMap<Vec<u8>, usize>,
    //     db: RocksDbStorage,
    // ) -> Self {
    //     Self {
    //         // root_tree,
    //         // root_leaf_keys,
    //         db,
    //     }
    // }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = RocksDbStorage::default_rocksdb_with_path(path)?;
        // TODO: owned `get` is not required for deserialization
        // let meta_storage = db.get_prefixed_context(Vec::new());
        // let root_leaf_keys: BTreeMap<Vec<u8>, usize> = if let
        // Some(root_leaf_keys_serialized) =     meta_storage.
        // get_meta(ROOT_LEAFS_SERIALIZED_KEY)? {
        //     bincode::deserialize(&root_leaf_keys_serialized).map_err(|_| {
        //         Error::CorruptedData(String::from("unable to deserialize root
        // leafs"))     })?
        // } else {
        //     BTreeMap::new()
        // };

        // Ok(GroveDb::new(
        //     Self::get_root_tree(&db, None)?,
        //     root_leaf_keys,
        //     db,
        // ))
        Ok(GroveDb { db })
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
    pub fn root_hash(&self, transaction: TransactionArg) -> Result<Option<[u8; 32]>, Error> {
        Ok(Self::get_root_tree(&self.db, transaction)?.root())
    }

    fn get_root_leaf_keys<'db, 'ctx, S>(meta_storage: &S) -> Result<BTreeMap<Vec<u8>, usize>, Error>
    where
        S: StorageContext<'db, 'ctx>,
        Error: From<<S as StorageContext<'db, 'ctx>>::Error>,
    {
        let root_leaf_keys: BTreeMap<Vec<u8>, usize> = if let Some(root_leaf_keys_serialized) =
            meta_storage.get_meta(ROOT_LEAFS_SERIALIZED_KEY)?
        {
            bincode::deserialize(&root_leaf_keys_serialized).map_err(|_| {
                Error::CorruptedData(String::from("unable to deserialize root leafs"))
            })?
        } else {
            BTreeMap::new()
        };
        Ok(root_leaf_keys)
    }

    fn get_root_tree(
        db: &RocksDbStorage,
        transaction: TransactionArg,
    ) -> Result<MerkleTree<Sha256>, Error> {
        let root_leaf_keys = if let Some(tx) = transaction {
            let meta_storage = db.get_prefixed_transactional_context(Vec::new(), tx);
            Self::get_root_leaf_keys(&meta_storage)?
        } else {
            let meta_storage = db.get_prefixed_context(Vec::new());
            Self::get_root_leaf_keys(&meta_storage)?
        };

        let mut leaf_hashes: Vec<[u8; 32]> = vec![[0; 32]; root_leaf_keys.len()];
        for (subtree_path, root_leaf_idx) in root_leaf_keys {
            let subtree_storage = db.get_prefixed_context_from_path([subtree_path.as_slice()]);
            let subtree = Merk::open(subtree_storage)
                .map_err(|_| Error::CorruptedData("cannot open root leaf".to_owned()))?;
            leaf_hashes[root_leaf_idx] = subtree.root_hash();
        }
        Ok(MerkleTree::<Sha256>::from_leaves(&leaf_hashes))
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn propagate_changes<'p, P>(&self, path: P, transaction: TransactionArg) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        // Go up until only one element in path, which means a key of a root tree
        let mut path_iter = path.into_iter();

        while path_iter.len() > 1 {
            if let Some(tx) = transaction {
                let subtree_storage = self
                    .db
                    .get_prefixed_transactional_context_from_path(path_iter.clone(), tx);
                let subtree = Merk::open(subtree_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                let element = Element::Tree(subtree.root_hash());
                let key = path_iter.next_back().expect("next element is `Some`");
                let parent_storage = self
                    .db
                    .get_prefixed_transactional_context_from_path(path_iter.clone(), tx);
                let mut parent_tree = Merk::open(parent_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                element.insert(&mut parent_tree, key.as_ref())?;
            } else {
                let subtree_storage = self.db.get_prefixed_context_from_path(path_iter.clone());
                let subtree = Merk::open(subtree_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                let element = Element::Tree(subtree.root_hash());
                let key = path_iter.next_back().expect("next element is `Some`");
                let parent_storage = self.db.get_prefixed_context_from_path(path_iter.clone());
                let mut parent_tree = Merk::open(parent_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                element.insert(&mut parent_tree, key.as_ref())?;
            }
        }

        Ok(())
    }

    fn get_storage(&self) -> &RocksDbStorage {
        &self.db
    }

    // fn get_subtrees(&self) -> Subtrees {
    //     Subtrees {
    //         root_leaf_keys: &self.root_leaf_keys,
    //         temp_subtrees: &self.temp_subtrees,
    //         deleted_subtrees: &self.temp_deleted_subtrees,
    //         storage: self.storage(),
    //     }
    // }

    pub fn flush(&self) -> Result<(), Error> {
        Ok(self.db.flush()?)
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
    pub fn start_transaction(&self) -> Transaction {
        self.db.start_transaction()
    }

    // /// Returns true if transaction is started. For more details on the
    // /// transaction usage, please check [`GroveDb::start_transaction`]
    // pub const fn is_transaction_started(&self) -> bool {
    //     self.is_readonly
    // }

    /// Commits previously started db transaction. For more details on the
    /// transaction usage, please check [`GroveDb::start_transaction`]
    pub fn commit_transaction(&self, transaction: Transaction) -> Result<(), Error> {
        Ok(self.db.commit_transaction(transaction)?)
    }

    /// Rollbacks previously started db transaction to initial state.
    /// For more details on the transaction usage, please check
    /// [`GroveDb::start_transaction`]
    pub fn rollback_transaction(&mut self, transaction: &Transaction) -> Result<(), Error> {
        Ok(self.db.rollback_transaction(transaction)?)
    }
}
