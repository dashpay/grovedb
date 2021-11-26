#![feature(trivial_bounds)]
mod subtree;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
};

use merk::{self, proofs::Query, rocksdb, Merk};
use rs_merkle::{algorithms::Sha256, MerkleProof, MerkleTree};
use subtree::Element;

/// Limit of possible indirections
const MAX_REFERENCE_HOPS: usize = 10;
/// A key to store serialized data about subtree prefixes to restore HADS
/// structure
const SUBTRESS_SERIALIZED_KEY: &[u8] = b"subtreesSerialized";
/// A key to store serialized data about root tree leafs keys and order
const ROOT_LEAFS_SERIALIZED_KEY: &[u8] = b"rootLeafsSerialized";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rocksdb error")]
    RocksDBError(#[from] merk::rocksdb::Error),
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
    root_leaf_keys: Vec<Vec<u8>>,
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
        // TODO: owned `get` is not required for deserialization
        if let Some(prefixes_serialized) = db.get(SUBTRESS_SERIALIZED_KEY)? {
            let subtrees_prefixes: Vec<Vec<u8>> = bincode::deserialize(&prefixes_serialized)?;
            for prefix in subtrees_prefixes {
                let subtree_merk = Merk::open(db.clone(), prefix.to_vec())?;
                subtrees.insert(prefix.to_vec(), subtree_merk);
            }
        }

        // TODO: owned `get` is not required for deserialization
        let root_leaf_keys: Vec<Vec<u8>> =
            if let Some(root_leaf_keys_serialized) = db.get(ROOT_LEAFS_SERIALIZED_KEY)? {
                bincode::deserialize(&root_leaf_keys_serialized)?
            } else {
                Vec::new()
            };

        Ok(GroveDb {
            root_tree: Self::build_root_tree(&subtrees, &root_leaf_keys),
            db: db.clone(),
            subtrees,
            root_leaf_keys,
        })
    }

    fn store_subtrees_prefixes(
        subtrees: &HashMap<Vec<u8>, Merk>,
        db: &rocksdb::DB,
    ) -> Result<(), Error> {
        let prefixes: Vec<Vec<u8>> = subtrees.keys().map(|x| x.clone()).collect();
        Ok(db.put(SUBTRESS_SERIALIZED_KEY, bincode::serialize(&prefixes)?)?)
    }

    fn build_root_tree(
        subtrees: &HashMap<Vec<u8>, Merk>,
        root_leaf_keys: &Vec<Vec<u8>>,
    ) -> MerkleTree<Sha256> {
        let mut leaf_hashes = Vec::new();
        for subtree_path in root_leaf_keys {
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
                    self.subtrees
                        .insert(compressed_path_subtree.clone(), subtree_merk);
                    // TODO: fine for now, not fine after
                    if !self.root_leaf_keys.contains(&compressed_path_subtree) {
                        self.root_leaf_keys.push(compressed_path_subtree);
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
                Self::store_subtrees_prefixes(&self.subtrees, &self.db)?;
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

    pub fn proof(
        &self,
        path: &[&[u8]],
        key: &[u8],
    ) -> Result<(Option<MerkleProof<Sha256>>, Vec<Vec<u8>>), Error> {
        // Grab the merk at a given path, create proof on merk for that key
        // Continuously split path and generate proof
        // if path is empty, then generate proof for root with given key
        let mut split_path = path.split_last();
        let mut proofs: Vec<Vec<u8>> = Vec::new();
        let mut root_proof: Option<MerkleProof<Sha256>> = None;

        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // We have hit the root key
                // Need to generate proof for this based on the index
                // TODO: Use the correct index for the path name

                // let root_key_index = self
                //     .root_leaf_keys
                //     .iter()
                //     .position(|&leaf| leaf.as_slice() == *key)
                //     .expect("Root key should exist");
                root_proof = Some(self.root_tree.proof(&vec![0]));
            }
            let merk = self
                .subtrees
                .get(&Self::compress_path(path, None))
                .ok_or(Error::InvalidPath("no subtree found under that path"))?;

            // Generate a proof of this merk with the given key
            let mut proof_query = Query::new();
            proof_query.insert_key(key.to_vec());

            let proof_result = merk
                .prove(proof_query)
                .expect("should prove both inclusion and absence");

            proofs.push(proof_result);
            split_path = path_slice.split_last();
        }

        Ok((root_proof, proofs))
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
