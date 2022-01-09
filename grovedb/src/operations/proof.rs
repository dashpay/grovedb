use std::collections::HashMap;

use merk::proofs::query::{Map, QueryItem};
use rs_merkle::{algorithms::Sha256, MerkleProof};

use crate::{Element, Error, GroveDb, PathQuery, Proof, Query, SizedQuery};

impl GroveDb {
    pub fn proof(&mut self, proof_queries: Vec<PathQuery>) -> Result<Vec<u8>, Error> {
        // To prove a path we need to return a proof for each node on the path including
        // the root. With multiple paths, nodes can overlap i.e two or more paths can
        // share the same nodes. We should only have one proof for each node,
        // if a node forks into multiple relevant paths then we should create a
        // combined proof for that node with all the relevant keys
        let mut query_paths = Vec::new();
        let mut intermediate_proof_spec: HashMap<Vec<u8>, Query> = HashMap::new();
        let mut root_keys: Vec<Vec<u8>> = Vec::new();
        let mut proofs: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();

        // For each unique node including the root
        // determine what keys would need to be included in the proof
        for proof_query in proof_queries.iter() {
            query_paths.push(
                proof_query
                    .path
                    .iter()
                    .map(|x| x.to_vec())
                    .collect::<Vec<_>>(),
            );

            let mut split_path = proof_query.path.split_last();
            while let Some((key, path_slice)) = split_path {
                if path_slice.is_empty() {
                    // We have gotten to the root node
                    let compressed_path = GroveDb::compress_subtree_key(&[], Some(key));
                    root_keys.push(compressed_path);
                } else {
                    let compressed_path = GroveDb::compress_subtree_key(path_slice, None);
                    if let Some(path_query) = intermediate_proof_spec.get_mut(&compressed_path) {
                        path_query.insert_key(key.to_vec());
                    } else {
                        let mut path_query = Query::new();
                        path_query.insert_key(key.to_vec());
                        intermediate_proof_spec.insert(compressed_path, path_query);
                    }
                }
                split_path = path_slice.split_last();
            }
        }

        // Construct the path proofs
        for (path, query) in intermediate_proof_spec {
            let proof = self.prove_item(&path, query)?;
            proofs.insert(path, proof);
        }

        // Construct the leaf proofs
        for proof_query in proof_queries {
            let mut path = proof_query.path;

            // If there is a subquery with a limit it's possible that we only need a reduced proof
            // for this leaf.
            let mut reduced_proof_query = proof_query;

            // First we must get elements

            if reduced_proof_query.subquery_key.is_some() {
                self.get_path_queries(&[&reduced_proof_query]);


                let mut path_vec = path.to_vec();
                path_vec.push(reduced_proof_query.subquery_key.unwrap());
                let compressed_path = GroveDb::compress_subtree_key(path_vec.as_slice(), None);

            }

            // Now we must insert the final proof for the sub leaves
            let compressed_path = GroveDb::compress_subtree_key(path, None);
            let proof = self.prove_path_item(&compressed_path, reduced_proof_query)?;
            proofs.insert(compressed_path, proof);
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

    fn prove_path_item(&self, compressed_path: &Vec<u8>, path_query: PathQuery) -> Result<Vec<u8>, Error> {
        let merk = self
            .subtrees
            .get(compressed_path)
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;

        let sized_query = path_query.query;

        if path_query.subquery.is_none() {
            //then limit should be applied directly to the proof here
            let proof_result = merk
                .prove(sized_query.query, sized_query.limit, sized_query.offset, sized_query.left_to_right)
                .expect("should prove both inclusion and absence");
            Ok(proof_result)
        } else {
            let proof_result = merk
                .prove(sized_query.query, None, None, sized_query.left_to_right)
                .expect("should prove both inclusion and absence");
            Ok(proof_result)
        }
    }

    fn prove_item(&self, path: &Vec<u8>, query: Query) -> Result<Vec<u8>, Error> {
        let merk = self
            .subtrees
            .get(path)
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;

            //then limit should be applied directly to the proof here
            let proof_result = merk
                .prove(query, None, None, true)
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
}
