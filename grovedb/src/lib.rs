#![feature(trivial_bounds)]
use std::{collections::HashMap, path::Path, rc::Rc};

use ed::Encode;
use merk::{self, column_families, rocksdb, Merk};
use rs_merkle::{algorithms::Sha256, Hasher, MerkleTree};
use subtree::Element;
mod subtree;

// Root tree has hardcoded leafs; each of them is `pub` to be easily used in
// `path` arg
pub const COMMON_TREE_KEY: &[u8] = b"common";
pub const IDENTITIES_TREE_KEY: &[u8] = b"identities";
pub const PUBLIC_KEYS_TO_IDENTITY_IDS_TREE_KEY: &[u8] = b"publicKeysToIdentityIDs";
pub const DATA_CONTRACTS_TREE_KEY: &[u8] = b"dataContracts";

// pub const SPENT_ASSET_LOCK_TRANSACTIONS_TREE_KEY: &[u8] =
// b"spentAssetLockTransactions";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rocksdb error")]
    RocksDBError(#[from] rocksdb::Error),
    #[error("unable to open Merk db")]
    MerkError(merk::Error),
    #[error("invalid path")]
    InvalidPath(&'static str),
    #[error("unable to decode")]
    EdError(#[from] ed::Error),
    #[error("cyclic reference path")]
    CyclicReferencePath,
}

impl From<merk::Error> for Error {
    fn from(e: merk::Error) -> Self {
        Error::MerkError(e)
    }
}

pub struct GroveDb {
    root_tree: MerkleTree<Sha256>,
    subtrees: HashMap<Vec<u8>, Merk>,
    db: Rc<rocksdb::DB>,
}

impl GroveDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        // 1. We should open a rocksdb connection
        // 2. from rocksdb by a special key we need to take array of path to every
        // possible merk 3. create for each path a new Merk (with prefix =
        // concat path) insance using rocksdb from 1. 4. put into `subtrees`
        // hashmap path -> Merk object 5. get(path, key) :
        // grovedb.subtrees[concat_path] (Merk) m, m.get(key) 6. insert(value,
        // path, key) : grovedb.subtrees[concat_path] (create if no entry) (Merk).apply

        let db = Rc::new(rocksdb::DB::open_cf_descriptors(
            &Merk::default_db_opts(),
            path,
            merk::column_families(),
        )?);
        let mut subtrees = HashMap::new();
        let mut leaf_hashes = Vec::new();
        for subtree_path in [
            COMMON_TREE_KEY,
            IDENTITIES_TREE_KEY,
            PUBLIC_KEYS_TO_IDENTITY_IDS_TREE_KEY,
            DATA_CONTRACTS_TREE_KEY,
        ] {
            let subtree_merk = Merk::open(db.clone(), subtree_path)?;
            leaf_hashes.push(subtree_merk.root_hash());
            subtrees.insert(subtree_path.to_vec(), subtree_merk);
        }

        Ok(GroveDb {
            root_tree: MerkleTree::<Sha256>::from_leaves(&leaf_hashes),
            db: db.clone(),
            subtrees,
        })
    }

    pub fn insert(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        element: subtree::Element,
    ) -> Result<(), Error> {
        let compressed_path = Self::compress_path(path, None);
        match element {
            Element::Tree => {
                // Helper closure to create a new subtree under path + key
                let create_subtree_merk = || -> Result<(Vec<u8>, Merk), Error> {
                    let compressed_path_subtree = Self::compress_path(path, Some(&key));
                    Ok((
                        compressed_path_subtree.clone(),
                        Merk::open(self.db.clone(), &compressed_path_subtree)?,
                    ))
                };
                if path.is_empty() {
                    // Add subtree to the root tree
                    let (compressed_path_subtree, subtree_merk) = create_subtree_merk()?;
                    self.subtrees.insert(compressed_path_subtree, subtree_merk);
                    Ok(())
                    // TODO: update root tree hashes
                } else {
                    // Add subtree to another subtree.
                    // First, check if a subtree exists to create a new subtree under it
                    self.subtrees
                        .get(&compressed_path)
                        .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                    let (compressed_path_subtree, subtree_merk) = create_subtree_merk()?;
                    self.subtrees.insert(compressed_path_subtree, subtree_merk);
                    // Had to take merk from `subtrees` once again to solve multiple &mut s
                    let mut merk = self
                        .subtrees
                        .get_mut(&compressed_path)
                        .expect("merk object must exist in `subtrees`");
                    // need to mark key as taken in the upper tree
                    element.insert(&mut merk, key)
                    // TODO: propagate updated hashses to upper trees
                }
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
                element.insert(&mut merk, key)
                // TODO: propagate updated hashes to upper trees
            }
        }
    }

    pub fn get(&self, path: &[&[u8]], key: &[u8]) -> Result<subtree::Element, Error> {
        let merk = self
            .subtrees
            .get(&Self::compress_path(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        Element::get(&merk, key)
    }

    pub fn proof(&self) -> ! {
        todo!()
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

#[cfg(test)]
mod tests {
    use tempdir::TempDir;

    use super::*;

    #[test]
    fn test_init() {
        let tmp_dir = TempDir::new("db").unwrap();
        GroveDb::open(tmp_dir).expect("empty tree is ok");
    }
}
