#![feature(trivial_bounds)]
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
};

use merk::{self, rocksdb, Merk};
use rs_merkle::{algorithms::Sha256, MerkleTree};
use subtree::Element;
mod subtree;

const MAX_REFERENCE_HOPS: usize = 10;

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
    BincodeError(#[from] bincode::Error),
    #[error("cyclic reference path")]
    CyclicReference,
    #[error("reference hops limit exceeded")]
    ReferenceLimit,
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
        let db = Rc::new(rocksdb::DB::open_cf_descriptors(
            &Merk::default_db_opts(),
            path,
            merk::column_families(),
        )?);
        let mut subtrees = HashMap::new();
        // TODO: this will work only for a fresh RocksDB and it cannot restore any
        // other subtree than these constants at any level!
        for subtree_path in [
            COMMON_TREE_KEY,
            IDENTITIES_TREE_KEY,
            PUBLIC_KEYS_TO_IDENTITY_IDS_TREE_KEY,
            DATA_CONTRACTS_TREE_KEY,
        ] {
            let subtree_merk = Merk::open(db.clone(), subtree_path.to_vec())?;
            subtrees.insert(subtree_path.to_vec(), subtree_merk);
        }

        Ok(GroveDb {
            root_tree: Self::build_root_tree(&subtrees),
            db: db.clone(),
            subtrees,
        })
    }

    // TODO: evntually there should be no hardcoded root tree structure
    fn build_root_tree(subtrees: &HashMap<Vec<u8>, Merk>) -> MerkleTree<Sha256> {
        let mut leaf_hashes = Vec::new();
        for subtree_path in [
            COMMON_TREE_KEY,
            IDENTITIES_TREE_KEY,
            PUBLIC_KEYS_TO_IDENTITY_IDS_TREE_KEY,
            DATA_CONTRACTS_TREE_KEY,
        ] {
            let subtree_merk = subtrees
                .get(subtree_path)
                .expect("root tree structure is hardcoded");
            leaf_hashes.push(subtree_merk.root_hash());
        }
        MerkleTree::<Sha256>::from_leaves(&leaf_hashes)
    }

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
                let create_subtree_merk = || -> Result<(Vec<u8>, Merk), Error> {
                    let compressed_path_subtree = Self::compress_path(path, Some(&key));
                    Ok((
                        compressed_path_subtree.clone(),
                        Merk::open(self.db.clone(), compressed_path_subtree)?,
                    ))
                };
                if path.is_empty() {
                    // Add subtree to the root tree
                    let (compressed_path_subtree, subtree_merk) = create_subtree_merk()?;
                    self.subtrees.insert(compressed_path_subtree, subtree_merk);
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

    fn follow_reference<'a>(&self, mut path: Vec<Vec<u8>>) -> Result<subtree::Element, Error> {
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
                self.root_tree = Self::build_root_tree(&self.subtrees);
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

#[cfg(test)]
mod tests {
    use tempdir::TempDir;

    use super::*;

    #[test]
    fn test_init() {
        let tmp_dir = TempDir::new("db").unwrap();
        GroveDb::open(tmp_dir).expect("empty tree is ok");
    }

    #[test]
    fn test_insert_value_to_merk() {
        let tmp_dir = TempDir::new("db").unwrap();
        let mut db = GroveDb::open(tmp_dir).unwrap();
        let element = Element::Item(b"ayy".to_vec());
        db.insert(&[COMMON_TREE_KEY], b"key".to_vec(), element.clone())
            .expect("successful insert");
        assert_eq!(
            db.get(&[COMMON_TREE_KEY], b"key").expect("succesful get"),
            element
        );
    }

    #[test]
    fn test_insert_value_to_subtree() {
        let tmp_dir = TempDir::new("db").unwrap();
        let mut db = GroveDb::open(tmp_dir).unwrap();
        let element = Element::Item(b"ayy".to_vec());

        // Insert a subtree first
        db.insert(&[COMMON_TREE_KEY], b"key1".to_vec(), Element::empty_tree())
            .expect("successful subtree insert");
        // Insert an element into subtree
        db.insert(
            &[COMMON_TREE_KEY, b"key1"],
            b"key2".to_vec(),
            element.clone(),
        )
        .expect("successful value insert");
        assert_eq!(
            db.get(&[COMMON_TREE_KEY, b"key1"], b"key2")
                .expect("succesful get"),
            element
        );
    }

    #[test]
    fn test_changes_propagated() {
        let tmp_dir = TempDir::new("db").unwrap();
        let mut db = GroveDb::open(tmp_dir).unwrap();
        let old_hash = db.root_tree.root();
        let element = Element::Item(b"ayy".to_vec());

        // Insert some nested subtrees
        db.insert(&[COMMON_TREE_KEY], b"key1".to_vec(), Element::empty_tree())
            .expect("successful subtree 1 insert");
        db.insert(
            &[COMMON_TREE_KEY, b"key1"],
            b"key2".to_vec(),
            Element::empty_tree(),
        )
        .expect("successful subtree 2 insert");
        // Insert an element into subtree
        db.insert(
            &[COMMON_TREE_KEY, b"key1", b"key2"],
            b"key3".to_vec(),
            element.clone(),
        )
        .expect("successful value insert");
        assert_eq!(
            db.get(&[COMMON_TREE_KEY, b"key1", b"key2"], b"key3")
                .expect("succesful get"),
            element
        );
        assert_ne!(old_hash, db.root_tree.root());
    }

    #[test]
    fn test_follow_references() {
        let tmp_dir = TempDir::new("db").unwrap();
        let mut db = GroveDb::open(tmp_dir).unwrap();
        let element = Element::Item(b"ayy".to_vec());

        // Insert a reference
        db.insert(
            &[COMMON_TREE_KEY],
            b"reference_key".to_vec(),
            Element::Reference(vec![
                COMMON_TREE_KEY.to_vec(),
                b"key2".to_vec(),
                b"key3".to_vec(),
            ]),
        )
        .expect("successful reference insert");

        // Insert an item to refer to
        db.insert(&[COMMON_TREE_KEY], b"key2".to_vec(), Element::empty_tree())
            .expect("successful subtree 1 insert");
        db.insert(
            &[COMMON_TREE_KEY, b"key2"],
            b"key3".to_vec(),
            element.clone(),
        )
        .expect("successful value insert");
        assert_eq!(
            db.get(&[COMMON_TREE_KEY], b"reference_key")
                .expect("succesful get"),
            element
        );
    }

    #[test]
    fn test_cyclic_references() {
        let tmp_dir = TempDir::new("db").unwrap();
        let mut db = GroveDb::open(tmp_dir).unwrap();

        db.insert(
            &[COMMON_TREE_KEY],
            b"reference_key_1".to_vec(),
            Element::Reference(vec![COMMON_TREE_KEY.to_vec(), b"reference_key_2".to_vec()]),
        )
        .expect("successful reference 1 insert");

        db.insert(
            &[COMMON_TREE_KEY],
            b"reference_key_2".to_vec(),
            Element::Reference(vec![COMMON_TREE_KEY.to_vec(), b"reference_key_1".to_vec()]),
        )
        .expect("successful reference 2 insert");

        assert!(matches!(
            db.get(&[COMMON_TREE_KEY], b"reference_key_1").unwrap_err(),
            Error::CyclicReference
        ));
    }

    #[test]
    fn test_too_many_indirections() {
        let tmp_dir = TempDir::new("db").unwrap();
        let mut db = GroveDb::open(tmp_dir).unwrap();

        let keygen = |idx| format!("key{}", idx).bytes().collect::<Vec<u8>>();

        db.insert(
            &[COMMON_TREE_KEY],
            b"key0".to_vec(),
            Element::Item(b"oops".to_vec()),
        )
        .expect("successful item insert");

        for i in 1..=(MAX_REFERENCE_HOPS + 1) {
            db.insert(
                &[COMMON_TREE_KEY],
                keygen(i),
                Element::Reference(vec![COMMON_TREE_KEY.to_vec(), keygen(i - 1)]),
            )
            .expect("successful reference insert");
        }

        assert!(matches!(
            db.get(&[COMMON_TREE_KEY], &keygen(MAX_REFERENCE_HOPS + 1))
                .unwrap_err(),
            Error::ReferenceLimit
        ));
    }
}
