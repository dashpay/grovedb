mod operations;
mod subtree;
#[cfg(test)]
mod tests;

use std::{collections::HashMap, path::Path, rc::Rc};

pub use merk::proofs::{query::QueryItem, Query};
use merk::{self, Merk};
use rs_merkle::{algorithms::Sha256, Hasher, MerkleTree};
use serde::{Deserialize, Serialize};
use storage::{
    rocksdb_storage::{PrefixedRocksDbStorage, PrefixedRocksDbStorageError},
    Storage,
};
pub use subtree::Element;

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
    // Irrecoverable errors
    #[error("storage error: {0}")]
    StorageError(#[from] PrefixedRocksDbStorageError),
    #[error("data corruption error: {0}")]
    CorruptedData(String),
}

pub struct PathQuery<'a> {
    path: &'a [&'a [u8]],
    query: Query,
}

impl PathQuery<'_> {
    pub fn new<'a>(path: &'a [&'a [u8]], query: Query) -> PathQuery {
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
    root_leaf_keys: HashMap<Vec<u8>, usize>,
    subtrees: HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
    meta_storage: PrefixedRocksDbStorage,
    db: Rc<storage::rocksdb_storage::DB>,
}

impl GroveDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = Rc::new(
            storage::rocksdb_storage::DB::open_cf_descriptors(
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

        Ok(GroveDb {
            root_tree: Self::build_root_tree(&subtrees, &root_leaf_keys),
            db,
            subtrees,
            root_leaf_keys,
            meta_storage,
        })
    }

    pub fn get_root_hash(&self) -> Option<[u8; 32]> {
        self.root_tree.root()
    }

    pub fn checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<GroveDb, Error> {
        storage::rocksdb_storage::Checkpoint::new(&self.db)
            .and_then(|x| x.create_checkpoint(&path))
            .map_err(PrefixedRocksDbStorageError::RocksDbError)?;
        GroveDb::open(path)
    }

    fn store_subtrees_keys_data(&self) -> Result<(), Error> {
        let prefixes: Vec<Vec<u8>> = self.subtrees.keys().map(|x| x.clone()).collect();
        self.meta_storage.put_meta(
            SUBTREES_SERIALIZED_KEY,
            &bincode::serialize(&prefixes)
                .map_err(|_| Error::CorruptedData(String::from("unable to serialize prefixes")))?,
        )?;
        self.meta_storage.put_meta(
            ROOT_LEAFS_SERIALIZED_KEY,
            &bincode::serialize(&self.root_leaf_keys).map_err(|_| {
                Error::CorruptedData(String::from("unable to serialize root leafs"))
            })?,
        )?;
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
        let res = MerkleTree::<Sha256>::from_leaves(&leaf_hashes);
        res
    }

    pub fn elements_iterator(&self, path: &[&[u8]]) -> Result<subtree::ElementsIterator, Error> {
        let merk = self
            .subtrees
            .get(&Self::compress_subtree_key(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        Ok(Element::iterator(merk.raw_iter()))
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn propagate_changes(&mut self, path: &[&[u8]]) -> Result<(), Error> {
        let mut split_path = path.split_last();
        // Go up until only one element in path, which means a key of a root tree
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // Hit the root tree
                self.root_tree = Self::build_root_tree(&self.subtrees, &self.root_leaf_keys);
                break;
            } else {
                let compressed_path_upper_tree = Self::compress_subtree_key(path_slice, None);
                let compressed_path_subtree = Self::compress_subtree_key(path_slice, Some(key));
                let subtree = self
                    .subtrees
                    .get(&compressed_path_subtree)
                    .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                let element = Element::Tree(subtree.root_hash());
                let upper_tree = self
                    .subtrees
                    .get_mut(&compressed_path_upper_tree)
                    .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                element.insert(upper_tree, key.to_vec())?;
                split_path = path_slice.split_last();
            }
        }
        Ok(())
    }

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    fn compress_subtree_key(path: &[&[u8]], key: Option<&[u8]>) -> Vec<u8> {
        let segments_iter = path.into_iter().map(|x| *x).chain(key.into_iter());
        let mut segments_count = path.len();
        if key.is_some() {
            segments_count += 1;
        }
        let mut res = segments_iter.fold(Vec::<u8>::new(), |mut acc, p| {
            acc.extend(p.into_iter());
            acc
        });

        res.extend(segments_count.to_ne_bytes());
        path.into_iter()
            .map(|x| *x)
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
}
