mod subtree;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
};

use merk::{self, Merk};
use rs_merkle::{algorithms::Sha256, MerkleTree};
use storage::{
    rocksdb_storage::{PrefixedRocksDbStorage, PrefixedRocksDbStorageError},
    Storage,
};
pub use subtree::Element;

/// Limit of possible indirections
const MAX_REFERENCE_HOPS: usize = 10;
/// A key to store serialized data about subtree prefixes to restore HADS
/// structure
const SUBTRESS_SERIALIZED_KEY: &[u8] = b"subtreesSerialized";
/// A key to store serialized data about root tree leafs keys and order
const ROOT_LEAFS_SERIALIZED_KEY: &[u8] = b"rootLeafsSerialized";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("cyclic reference path")]
    CyclicReference,
    #[error("reference hops limit exceeded")]
    ReferenceLimit,
    #[error("invalid path: {0}")]
    InvalidPath(&'static str),
    // Irrecoverable errors
    #[error("storage error: {0}")]
    StorageError(#[from] PrefixedRocksDbStorageError),
    #[error("data corruption error: {0}")]
    CorruptedData(String),
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
        if let Some(prefixes_serialized) = meta_storage.get_meta(SUBTRESS_SERIALIZED_KEY)? {
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

    pub fn checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<GroveDb, Error> {
        storage::rocksdb_storage::Checkpoint::new(&self.db)
            .and_then(|x| x.create_checkpoint(&path))
            .map_err(PrefixedRocksDbStorageError::RocksDbError)?;
        GroveDb::open(path)
    }

    fn store_subtrees_keys_data(&self) -> Result<(), Error> {
        let prefixes: Vec<Vec<u8>> = self.subtrees.keys().map(|x| x.clone()).collect();
        self.meta_storage.put_meta(
            SUBTRESS_SERIALIZED_KEY,
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

    // TODO: split the function into smaller ones
    pub fn insert(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        mut element: subtree::Element,
    ) -> Result<(), Error> {
        let compressed_path = Self::compress_path(path, None);
        match &mut element {
            Element::Tree(subtree_root_hash) => {
                // Helper closure to create a new subtree under path + key
                let create_subtree_merk =
                    || -> Result<(Vec<u8>, Merk<PrefixedRocksDbStorage>), Error> {
                        let compressed_path_subtree = Self::compress_path(path, Some(&key));
                        Ok((
                            compressed_path_subtree.clone(),
                            Merk::open(PrefixedRocksDbStorage::new(
                                self.db.clone(),
                                compressed_path_subtree,
                            )?)
                            .map_err(|e| Error::CorruptedData(e.to_string()))?,
                        ))
                    };
                if path.is_empty() {
                    // Add subtree to the root tree

                    // Open Merk and put handle into `subtrees` dictionary accessible by its
                    // compressed path
                    let (compressed_path_subtree, subtree_merk) = create_subtree_merk()?;
                    self.subtrees
                        .insert(compressed_path_subtree.clone(), subtree_merk);

                    // Update root leafs index to persist rs-merkle structure later
                    if self.root_leaf_keys.get(&compressed_path_subtree).is_none() {
                        self.root_leaf_keys
                            .insert(compressed_path_subtree, self.root_tree.leaves_len());
                    }
                    self.propagate_changes(&[&key])?;
                } else {
                    // Add subtree to another subtree.
                    // First, check if a subtree exists to create a new subtree under it
                    self.subtrees
                        .get(&compressed_path)
                        .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                    let (compressed_path_subtree, subtree_merk) = create_subtree_merk()?;
                    // Set tree value as a a subtree root hash
                    *subtree_root_hash = subtree_merk.root_hash();
                    self.subtrees.insert(compressed_path_subtree, subtree_merk);
                    // Had to take merk from `subtrees` once again to solve multiple &mut s
                    let mut merk = self
                        .subtrees
                        .get_mut(&compressed_path)
                        .expect("merk object must exist in `subtrees`");
                    // need to mark key as taken in the upper tree
                    element.insert(&mut merk, key)?;
                    self.propagate_changes(path)?;
                }
                self.store_subtrees_keys_data()?;
            }
            _ => {
                // If path is empty that means there is an attempt to insert something into a
                // root tree and this branch is for anything but trees
                if path.is_empty() {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ));
                }
                // Get a Merk by a path
                let mut merk = self
                    .subtrees
                    .get_mut(&compressed_path)
                    .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                element.insert(&mut merk, key)?;
                self.propagate_changes(path)?;
            }
        }
        Ok(())
    }

    pub fn insert_if_not_exists(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        mut element: subtree::Element,
    ) -> Result<(), Error> {
        Ok(())
    }

    pub fn get(&self, path: &[&[u8]], key: &[u8]) -> Result<subtree::Element, Error> {
        match self.get_raw(path, key)? {
            Element::Reference(reference_path) => self.follow_reference(reference_path),
            other => Ok(other),
        }
    }

    /// Get tree item without following references
    fn get_raw(&self, path: &[&[u8]], key: &[u8]) -> Result<subtree::Element, Error> {
        let merk = self
            .subtrees
            .get(&Self::compress_path(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        Element::get(&merk, key)
    }

    fn follow_reference(&self, mut path: Vec<Vec<u8>>) -> Result<subtree::Element, Error> {
        let mut hops_left = MAX_REFERENCE_HOPS;
        let mut current_element;
        let mut visited = HashSet::new();

        while hops_left > 0 {
            if visited.contains(&path) {
                return Err(Error::CyclicReference);
            }
            if let Some((key, path_slice)) = path.split_last() {
                current_element = self.get_raw(
                    path_slice
                        .iter()
                        .map(|x| x.as_slice())
                        .collect::<Vec<_>>()
                        .as_slice(),
                    key,
                )?;
            } else {
                return Err(Error::InvalidPath("empty path"));
            }
            visited.insert(path);
            match current_element {
                Element::Reference(reference_path) => path = reference_path,
                other => return Ok(other),
            }
            hops_left -= 1;
        }
        Err(Error::ReferenceLimit)
    }

    pub fn proof(&self) -> ! {
        todo!()
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
                let compressed_path_upper_tree = Self::compress_path(path_slice, None);
                let compressed_path_subtree = Self::compress_path(path_slice, Some(key));
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
    fn compress_path(path: &[&[u8]], key: Option<&[u8]>) -> Vec<u8> {
        let mut res = path.iter().fold(Vec::<u8>::new(), |mut acc, p| {
            acc.extend(p.into_iter());
            acc
        });
        if let Some(k) = key {
            res.extend_from_slice(k);
        }
        res
    }
}
