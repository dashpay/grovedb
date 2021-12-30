mod subtree;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
};

pub use merk::proofs::{query::QueryItem, Query};
use merk::{self, proofs::query::Map, Merk};
use rs_merkle::{algorithms::Sha256, Hasher, MerkleProof, MerkleTree};
use serde::{Deserialize, Serialize};
use storage::{
    rocksdb_storage::{self, PrefixedRocksDbStorage, PrefixedRocksDbStorageError},
    Storage,
};
pub use subtree::Element;

/// Limit of possible indirections
const MAX_REFERENCE_HOPS: usize = 10;
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

    pub fn delete(&mut self, path: &[&[u8]], key: Vec<u8>) -> Result<(), Error> {
        let element = self.get_raw(path, &key)?;
        if path.is_empty() {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leafs currently cannot be deleted",
            ))
        } else {
            let mut merk = self
                .subtrees
                .get_mut(&Self::compress_subtree_key(path, None))
                .ok_or(Error::InvalidPath("no subtree found under that path"))?;
            Element::delete(&mut merk, key.clone())?;
            if let Element::Tree(_) = element {
                // TODO: dumb traversal should not be tolerated
                let mut concat_path: Vec<Vec<u8>> = path.iter().map(|x| x.to_vec()).collect();
                concat_path.push(key);
                let subtrees_paths = self.find_subtrees(concat_path)?;
                for subtree_path in subtrees_paths {
                    // TODO: eventually we need to do something about this nested slices
                    let subtree_path_ref: Vec<&[u8]> =
                        subtree_path.iter().map(|x| x.as_slice()).collect();
                    let prefix = Self::compress_subtree_key(&subtree_path_ref, None);
                    if let Some(subtree) = self.subtrees.remove(&prefix) {
                        subtree.clear().map_err(|e| {
                            Error::CorruptedData(format!(
                                "unable to cleanup tree from storage: {}",
                                e
                            ))
                        })?;
                    }
                }
            }
            self.propagate_changes(path)?;
            Ok(())
        }
    }

    // TODO: dumb traversal should not be tolerated
    /// Finds keys which are trees for a given subtree recursively.
    /// One element means a key of a `merk`, n > 1 elements mean relative path
    /// for a deeply nested subtree.
    fn find_subtrees(&self, path: Vec<Vec<u8>>) -> Result<Vec<Vec<Vec<u8>>>, Error> {
        let mut queue: Vec<Vec<Vec<u8>>> = vec![path.clone()];
        let mut result: Vec<Vec<Vec<u8>>> = vec![path.clone()];

        while let Some(q) = queue.pop() {
            // TODO: eventually we need to do something about this nested slices
            let q_ref: Vec<&[u8]> = q.iter().map(|x| x.as_slice()).collect();
            let mut iter = self.elements_iterator(&q_ref)?;
            while let Some((key, value)) = iter.next()? {
                match value {
                    Element::Tree(_) => {
                        let mut sub_path = q.clone();
                        sub_path.push(key);
                        queue.push(sub_path.clone());
                        result.push(sub_path);
                    }
                    _ => {}
                }
            }
        }
        Ok(result)
    }

    // TODO: split the function into smaller ones
    pub fn insert(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        mut element: subtree::Element,
    ) -> Result<(), Error> {
        match &mut element {
            Element::Tree(subtree_root_hash) => {
                // Helper closure to create a new subtree under path + key
                let create_subtree_merk =
                    || -> Result<(Vec<u8>, Merk<PrefixedRocksDbStorage>), Error> {
                        let compressed_path_subtree = Self::compress_subtree_key(&path, Some(&key));
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
                    let compressed_path = Self::compress_subtree_key(path, None);
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
                    .get_mut(&Self::compress_subtree_key(path, None))
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
        element: subtree::Element,
    ) -> Result<bool, Error> {
        if self.get(path, &key).is_ok() {
            return Ok(false);
        }
        match self.insert(path, key, element) {
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    }

    pub fn get(&self, path: &[&[u8]], key: &[u8]) -> Result<subtree::Element, Error> {
        match self.get_raw(path, key)? {
            Element::Reference(reference_path) => self.follow_reference(reference_path),
            other => Ok(other),
        }
    }

    pub fn elements_iterator(&self, path: &[&[u8]]) -> Result<subtree::ElementsIterator, Error> {
        let merk = self
            .subtrees
            .get(&Self::compress_subtree_key(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        Ok(Element::iterator(merk.raw_iter()))
    }

    /// Get tree item without following references
    fn get_raw(&self, path: &[&[u8]], key: &[u8]) -> Result<subtree::Element, Error> {
        let merk = self
            .subtrees
            .get(&Self::compress_subtree_key(path, None))
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

    pub fn proof(&mut self, proof_queries: Vec<PathQuery>) -> Result<Vec<u8>, Error> {
        // To prove a path we need to return a proof for each node on the path including
        // the root. With multiple paths, nodes can overlap i.e two or more paths can
        // share the same nodes. We should only have one proof for each node,
        // if a node forks into multiple relevant paths then we should create a
        // combined proof for that node with all the relevant keys
        let mut query_paths = Vec::new();
        let mut proof_spec: HashMap<Vec<u8>, Query> = HashMap::new();
        let mut root_keys: Vec<Vec<u8>> = Vec::new();
        let mut proofs: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();

        // For each unique node including the root
        // determine what keys would need to be included in the proof
        for proof_query in proof_queries {
            query_paths.push(
                proof_query
                    .path
                    .iter()
                    .map(|x| x.to_vec())
                    .collect::<Vec<_>>(),
            );

            let compressed_path = GroveDb::compress_subtree_key(proof_query.path, None);
            proof_spec.insert(compressed_path, proof_query.query);

            let mut split_path = proof_query.path.split_last();
            while let Some((key, path_slice)) = split_path {
                if path_slice.is_empty() {
                    // We have gotten to the root node
                    let compressed_path = GroveDb::compress_subtree_key(&[], Some(key));
                    root_keys.push(compressed_path);
                } else {
                    let compressed_path = GroveDb::compress_subtree_key(path_slice, None);
                    if let Some(path_query) = proof_spec.get_mut(&compressed_path) {
                        path_query.insert_key(key.to_vec());
                    } else {
                        let mut path_query = Query::new();
                        path_query.insert_key(key.to_vec());
                        proof_spec.insert(compressed_path, path_query);
                    }
                }
                split_path = path_slice.split_last();
            }
        }

        // Construct the sub proofs
        for (path, query) in proof_spec {
            let proof = self.prove_item(&path, query)?;
            proofs.insert(path, proof);
        }

        // Construct the root proof
        let mut root_index: Vec<usize> = Vec::new();
        for key in root_keys {
            let index = self
                .root_leaf_keys
                .get(&key)
                .ok_or(Error::InvalidPath("root key not found"))?;
            root_index.push(*index);
        }
        let root_proof = self.root_tree.proof(&root_index).to_bytes();

        let proof = Proof {
            query_paths,
            proofs,
            root_proof,
            root_leaf_keys: self.root_leaf_keys.clone(),
        };

        let seralized_proof = bincode::serialize(&proof)
            .map_err(|_| Error::CorruptedData(String::from("unable to serialize proof")))?;

        Ok(seralized_proof)
    }

    fn prove_item(&self, path: &Vec<u8>, proof_query: Query) -> Result<Vec<u8>, Error> {
        let merk = self
            .subtrees
            .get(path)
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;

        let proof_result = merk
            .prove(proof_query)
            .expect("should prove both inclusion and absence");

        Ok(proof_result)
    }

    pub fn execute_proof(proof: Vec<u8>) -> Result<([u8; 32], HashMap<Vec<u8>, Map>), Error> {
        // Deserialize the proof
        let proof: Proof = bincode::deserialize(&proof)
            .map_err(|_| Error::CorruptedData(String::from("unable to deserialize proof")))?;

        // Required to execute the root proof
        let mut root_keys_index: Vec<usize> = Vec::new();
        let mut root_hashes: Vec<[u8; 32]> = Vec::new();

        // Collects the result map for each query
        let mut result_map: HashMap<Vec<u8>, Map> = HashMap::new();

        for path in proof.query_paths {
            let path = path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();
            // For each query path, get the result map after execution
            // and store hash + index for later root proof execution
            let root_key = &path[0];
            let (hash, proof_result_map) = GroveDb::execute_path(&path, &proof.proofs)?;
            let compressed_root_key_path = GroveDb::compress_subtree_key(&[], Some(&root_key));
            let compressed_query_path = GroveDb::compress_subtree_key(&path, None);

            let index = proof
                .root_leaf_keys
                .get(&compressed_root_key_path)
                .ok_or(Error::InvalidPath("Bad path"))?;
            if !root_keys_index.contains(&index) {
                root_keys_index.push(*index);
                root_hashes.push(hash);
            }

            result_map.insert(compressed_query_path, proof_result_map);
        }

        let root_proof = match MerkleProof::<Sha256>::try_from(proof.root_proof) {
            Ok(proof) => Ok(proof),
            Err(_) => Err(Error::InvalidProof("Invalid proof element")),
        }?;

        let root_hash =
            match root_proof.root(&root_keys_index, &root_hashes, proof.root_leaf_keys.len()) {
                Ok(hash) => Ok(hash),
                Err(_) => Err(Error::InvalidProof("Invalid proof element")),
            }?;

        Ok((root_hash, result_map))
    }

    // Given a query path and a set of proofs
    // execute_path validates that the nodes represented by the paths
    // are connected to one another i.e root hash of child node is in parent node
    // at the correct key.
    // If path is valid, it returns the root hash of topmost merk and result map of
    // leaf merk.
    fn execute_path(
        path: &[&[u8]],
        proofs: &HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<([u8; 32], Map), Error> {
        let compressed_path = GroveDb::compress_subtree_key(path, None);
        let proof = proofs
            .get(&compressed_path)
            .ok_or(Error::InvalidPath("Bad path"))?;

        // Execute the leaf merk proof
        let (mut last_root_hash, result_map) = match merk::execute_proof(&proof[..]) {
            Ok(result) => Ok(result),
            Err(_) => Err(Error::InvalidPath("Invalid proof element")),
        }?;

        // Validate the path
        let mut split_path = path.split_last();
        while let Some((key, path_slice)) = split_path {
            if !path_slice.is_empty() {
                let compressed_path = GroveDb::compress_subtree_key(path_slice, None);
                let proof = proofs
                    .get(&compressed_path)
                    .ok_or(Error::InvalidPath("Bad path"))?;

                let proof_result = match merk::execute_proof(&proof[..]) {
                    Ok(result) => Ok(result),
                    Err(_) => Err(Error::InvalidPath("Invalid proof element")),
                }?;

                let result_map = proof_result.1;
                // TODO: Handle the error better here
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
                break;
            }

            split_path = path_slice.split_last();
        }

        Ok((last_root_hash, result_map))
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
}
