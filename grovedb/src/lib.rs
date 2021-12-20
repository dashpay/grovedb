#![feature(trivial_bounds)]
mod subtree;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
};

pub use merk::proofs::{query::QueryItem, Query};
use merk::{self, execute_proof, proofs::query::Map, rocksdb, Merk};
use rs_merkle::{algorithms::Sha256, MerkleProof, MerkleTree};
use subtree::Element;

use crate::Error::InvalidProof;

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
    #[error("invalid proof: {0}")]
    InvalidProof(&'static str),
}

impl From<merk::Error> for Error {
    fn from(e: merk::Error) -> Self {
        Error::MerkError(e)
    }
}

pub struct GroveDb {
    root_tree: MerkleTree<Sha256>,
    root_leaf_keys: HashMap<Vec<u8>, usize>,
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
        let root_leaf_keys: HashMap<Vec<u8>, usize> =
            if let Some(root_leaf_keys_serialized) = db.get(ROOT_LEAFS_SERIALIZED_KEY)? {
                bincode::deserialize(&root_leaf_keys_serialized)?
            } else {
                HashMap::new()
            };

        Ok(GroveDb {
            root_tree: Self::build_root_tree(&subtrees, &root_leaf_keys),
            db: db.clone(),
            subtrees,
            root_leaf_keys,
        })
    }

    fn store_subtrees_keys_data(&self) -> Result<(), Error> {
        let prefixes: Vec<Vec<u8>> = self.subtrees.keys().map(|x| x.clone()).collect();
        self.db
            .put(SUBTRESS_SERIALIZED_KEY, bincode::serialize(&prefixes)?)?;
        self.db.put(
            ROOT_LEAFS_SERIALIZED_KEY,
            bincode::serialize(&self.root_leaf_keys)?,
        )?;
        Ok(())
    }

    fn build_root_tree(
        subtrees: &HashMap<Vec<u8>, Merk>,
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
                let create_subtree_merk = || -> Result<(Vec<u8>, Merk), Error> {
                    let compressed_path_subtree = Self::compress_path(path, Some(&key));
                    Ok((
                        compressed_path_subtree.clone(),
                        Merk::open(self.db.clone(), compressed_path_subtree)?,
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

    pub fn proof(&self, path: &[&[u8]], proof_query: Query) -> Result<Vec<Vec<u8>>, Error> {
        let mut proofs: Vec<Vec<u8>> = Vec::new();

        // First prove the query
        proofs.push(self.prove_item(path, proof_query)?);

        // Next prove the query path
        let mut split_path = path.split_last();
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // Get proof for root tree at current key
                let root_key_index = self
                    .root_leaf_keys
                    .get(*key)
                    .ok_or(Error::InvalidPath("root key not found"))?;
                proofs.push(self.root_tree.proof(&[*root_key_index]).to_bytes());
            } else {
                let mut path_query = Query::new();
                path_query.insert_item(QueryItem::Key(key.to_vec()));
                proofs.push(self.prove_item(path_slice, path_query)?);
            }
            split_path = path_slice.split_last();
        }

        // Append the root leaf keys hash map to proof to provide context when verifying
        // proof
        let aux_data = bincode::serialize(&self.root_leaf_keys)?;
        proofs.push(aux_data);

        Ok(proofs)
    }

    fn prove_item(&self, path: &[&[u8]], proof_query: Query) -> Result<Vec<u8>, Error> {
        let merk = self
            .subtrees
            .get(&Self::compress_path(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;

        // Generate a proof for this merk at the given key
        // let mut proof_query = Query::new();
        // proof_query.insert_item(item);

        let proof_result = merk
            .prove(proof_query)
            .expect("should prove both inclusion and absence");

        Ok(proof_result)
    }

    pub fn verify_proof(
        path: &[&[u8]],
        proofs: &mut Vec<Vec<u8>>, // Generic into_iterator (trait) u8
        expected_root_hash: [u8; 32],
    ) -> Result<Map, Error> {
        if proofs.len() < 2 {
            return Err(Error::InvalidProof("Proof length should be 2 or more"));
        }

        if proofs.len() - 2 != path.len() {
            return Err(Error::InvalidProof(
                "Proof length should be two greater than path",
            ));
        }

        let root_leaf_keys: HashMap<Vec<u8>, usize> =
            bincode::deserialize(&proofs.pop().unwrap()[..])?;

        let mut proof_iterator = proofs.iter();
        let reverse_path_iterator = path.iter().rev();

        let leaf_proof = proof_iterator
            .next()
            .expect("Constraint checks above enforces leaf proof must exist");

        let (mut last_root_hash, leaf_result_map) = match execute_proof(&leaf_proof[..]) {
            Ok(result) => Ok(result),
            Err(e) => Err(Error::InvalidProof("Invalid proof element")),
        }?;

        let mut proof_path_zip = proof_iterator.zip(reverse_path_iterator).peekable();

        while let Some((proof, key)) = proof_path_zip.next() {
            if proof_path_zip.peek().is_some() {
                // Non root proof, validate that the proof is valid and
                // the result map contains the last subtree root hash i.e the previous
                // subtree is a child of this tree
                let proof_result = match execute_proof(&proof[..]) {
                    Ok(result) => Ok(result),
                    Err(e) => Err(Error::InvalidProof("Invalid proof element")),
                }?;
                let result_map = proof_result.1;

                let elem: Element =
                    bincode::deserialize(result_map.get(key).unwrap().unwrap()).unwrap();
                let merk_root_hash = match elem {
                    Element::Tree(hash) => Ok(hash),
                    _ => Err(Error::InvalidProof(
                        "Intermediate proofs should be for trees",
                    )),
                }?;

                if merk_root_hash != last_root_hash {
                    return Err(Error::InvalidProof("Bad path"));
                }

                last_root_hash = proof_result.0;
            } else {
                // Last proof (root proof)
                let root_proof = match MerkleProof::<Sha256>::try_from(&proof[..]) {
                    Ok(root_proof) => Ok(root_proof),
                    Err(e) => Err(Error::InvalidProof("Invalid proof element")),
                }?;
                let a: [u8; 32] = last_root_hash;
                if root_proof.verify(
                    expected_root_hash,
                    &[root_leaf_keys[*key]],
                    &[a],
                    root_leaf_keys.len(),
                ) {
                    break;
                } else {
                    return Err(Error::InvalidProof("Root hashes didn't match"));
                }
            }
        }

        Ok(leaf_result_map)
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
