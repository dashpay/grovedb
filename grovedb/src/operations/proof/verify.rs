use merk::{
    proofs::{query::ProofVerificationResult, Query},
    Hash,
};
use rs_merkle::{algorithms::Sha256, MerkleProof};

use crate::{
    operations::proof::util::{ProofReader, ProofType, EMPTY_TREE_HASH},
    Element, Error, GroveDb, PathQuery,
};

impl GroveDb {
    pub fn execute_proof(
        proof: &[u8],
        query: PathQuery,
    ) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        let mut m = ProofVerifier::new(&query);
        m.execute_proof(proof, query)
    }
}

struct ProofVerifier {
    limit: Option<u16>,
    offset: Option<u16>,
}

impl ProofVerifier {
    pub fn new(query: &PathQuery) -> Self {
        ProofVerifier {
            limit: query.query.limit,
            offset: query.query.offset,
        }
    }

    pub fn execute_proof(
        &mut self,
        proof: &[u8],
        query: PathQuery,
    ) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        let mut result_set: Vec<(Vec<u8>, Vec<u8>)> = vec![];
        let mut proof_reader = ProofReader::new(proof);

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();
        if path_slices.len() < 1 {
            return Err(Error::InvalidPath("can't verify proof for empty path"));
        }

        let mut last_subtree_root_hash =
            self.execute_subquery_proof(&mut proof_reader, &mut result_set, query.clone())?;

        // validate the path elements are connected
        self.verify_path_to_root(
            &query,
            path_slices,
            &mut proof_reader,
            &mut last_subtree_root_hash,
        )?;

        // execute the root proof
        let root_hash = Self::execute_root_proof(&mut proof_reader, last_subtree_root_hash)?;

        Ok((root_hash, result_set))
    }

    fn execute_subquery_proof(
        &mut self,
        proof_reader: &mut ProofReader,
        result_set: &mut Vec<(Vec<u8>, Vec<u8>)>,
        query: PathQuery,
    ) -> Result<[u8; 32], Error> {
        let last_root_hash: [u8; 32];
        let (proof_type, proof) = proof_reader.read_proof()?;

        match proof_type {
            ProofType::SizedMerkProof => {
                // verify proof with limit and offset values
                let verification_result = self.execute_merk_proof(
                    ProofType::SizedMerkProof,
                    &proof,
                    &query.query.query,
                    query.query.query.left_to_right,
                )?;

                last_root_hash = verification_result.0;
                result_set.extend(verification_result.1);
            }
            ProofType::MerkProof => {
                // for non leaf subtrees, we want to prove that all the queried keys
                // have an accompanying proof as long as the limit is non zero
                // and their child subtree is not empty
                let verification_result = self.execute_merk_proof(
                    ProofType::MerkProof,
                    &proof,
                    &query.query.query,
                    query.query.query.left_to_right,
                )?;

                last_root_hash = verification_result.0;

                for (key, value_bytes) in verification_result.1 {
                    let child_element = Element::deserialize(value_bytes.as_slice())?;
                    match child_element {
                        Element::Tree(mut expected_root_hash) => {
                            if expected_root_hash == EMPTY_TREE_HASH {
                                // child node is empty, move on to next
                                continue;
                            }

                            if self.limit == Some(0) {
                                // we are done verifying the subqueries
                                break;
                            }

                            let (subquery_key, subquery_value) =
                                Element::subquery_paths_for_sized_query(
                                    &query.query,
                                    key.as_slice(),
                                );

                            if subquery_value.is_none() && subquery_key.is_none() {
                                continue;
                            }

                            // I need to reinsert the ability for subquery key to work independently
                            // if a subquery has only a key, that key should be used as the query
                            // hence subqueries can point to non tree type, and you don't need to
                            // update the root hash everytime. based on
                            // the query we can know the exact condition
                            // we are experiencing. and the perform the verification
                            // accordingly states:
                            // - subquery value and key in this case, verify the subquery key first,
                            //   update the hash and then move on to the next
                            // - subquery key only in this case, create a sized merk proof for that
                            //   subquery, don't
                            // - subquery value only in this case, no verification, just proceed.
                            //   (this is the default state);

                            if subquery_key.is_some() {
                                // prove that the subquery key was used and update the expected hash
                                // if the proof shows subquery key does not exist, path is no longer
                                // useful move on to next

                                // TODO
                                // depending on the type of query will need to update the limit and
                                // offset values would be nice if
                                // that update just happened automatically

                                let verification_result =
                                    self.verify_subquery_key(proof_reader, subquery_key)?;
                                let subquery_key_result_set = verification_result.1;
                                let subquery_key_not_in_tree = subquery_key_result_set.len() == 0;

                                if subquery_key_not_in_tree || subquery_value.is_none() {
                                    continue;
                                } else {
                                    Self::update_root_hash_from_subquery_key_element(
                                        &mut expected_root_hash,
                                        &subquery_key_result_set,
                                    )?;
                                }
                            }

                            // let has_subquery_key_and_value =
                            //     subquery_value.is_some() && subquery_key.is_some();
                            // if has_subquery_key_and_value {
                            //     // prove that the subquery key was used and update the expected
                            // hash     // if the proof shows subquery
                            // key does not exist, path is no longer
                            //     // useful move on to next
                            //     let verification_result =
                            //         Self::verify_subquery_key(proof_reader, subquery_key)?;
                            //     let subquery_key_result_set = verification_result.1.result_set;
                            //     let subquery_key_not_in_tree = subquery_key_result_set.len() ==
                            // 0;
                            //
                            //     if subquery_key_not_in_tree {
                            //         continue;
                            //     } else {
                            //         Self::update_root_hash_from_subquery_key_element(
                            //             &mut expected_root_hash,
                            //             &subquery_key_result_set,
                            //         )?;
                            //     }
                            // }

                            let new_path_query =
                                PathQuery::new_unsized(vec![], subquery_value.unwrap());
                            let child_hash = self.execute_subquery_proof(
                                proof_reader,
                                result_set,
                                new_path_query,
                            )?;

                            if child_hash != expected_root_hash {
                                return Err(Error::InvalidProof(
                                    "child hash doesn't match the expected hash",
                                ));
                            }
                        }
                        _ => {
                            // MerkProof type signifies there are more subtrees to explore
                            // reaching here under a merk proof means proof for required
                            // subtree(s) were not provided
                            return Err(Error::InvalidProof("Missing proof for subtree"));
                        }
                    }
                }
            }
            _ => {
                // execute_subquery_proof only expects proofs for merk trees
                // root proof is handled separately
                return Err(Error::InvalidProof("wrong proof type"));
            }
        }
        Ok(last_root_hash)
    }

    /// Deserialize subkey_element and update expected root hash
    fn update_root_hash_from_subquery_key_element(
        expected_root_hash: &mut [u8; 32],
        subquery_key_result_set: &Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<(), Error> {
        let elem_value = &subquery_key_result_set[0].1;
        let subquery_key_element = Element::deserialize(elem_value)
            .map_err(|_| Error::CorruptedData("failed to deserialize element".to_string()))?;
        match subquery_key_element {
            Element::Tree(new_exptected_hash) => {
                *expected_root_hash = new_exptected_hash;
            }
            _ => {
                // the means that the subquery key pointed to a non tree
                // element, this is not valid as you cannot apply the
                // the subquery value to non tree items
                return Err(Error::InvalidProof(
                    "subquery key cannot point to non tree element",
                ));
            }
        }
        Ok(())
    }

    /// Checks that a valid proof showing the existence or absence of the
    /// subquery key is present
    fn verify_subquery_key(
        &mut self,
        proof_reader: &mut ProofReader,
        subquery_key: Option<Vec<u8>>,
    ) -> Result<(Hash,  Vec<(Vec<u8>, Vec<u8>)>), Error> {
        let (proof_type, subkey_proof) = proof_reader.read_proof()?;
        if proof_type != ProofType::MerkProof {
            return Err(Error::InvalidProof(
                "expected unsized merk proof for subquery key",
            ));
        }

        let mut key_as_query = Query::new();
        key_as_query.insert_key(subquery_key.clone().unwrap());

        let verification_result = self.execute_merk_proof(
            ProofType::MerkProof,
            &subkey_proof,
            &key_as_query,
            key_as_query.left_to_right,
        )?;

        Ok(verification_result)
    }

    /// Verifies that the correct proof was provided to confirm the path in
    /// query
    fn verify_path_to_root(
        &mut self,
        query: &PathQuery,
        path_slices: Vec<&[u8]>,
        proof_reader: &mut ProofReader,
        expected_root_hash: &mut [u8; 32],
    ) -> Result<(), Error> {
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if !path_slice.is_empty() {
                // for every subtree, there should be a corresponding proof for the parent
                // which should prove that this subtree is a child of the parent tree
                let parent_merk_proof =
                    proof_reader.read_proof_of_type(ProofType::MerkProof.into())?;

                let mut parent_query = Query::new();
                parent_query.insert_key(key.to_vec());

                let proof_result = self.execute_merk_proof(
                    ProofType::MerkProof,
                    &parent_merk_proof,
                    &parent_query,
                    query.query.query.left_to_right,
                )?;

                let result_set = proof_result.1;
                if result_set.len() == 0 || &result_set[0].0 != key {
                    return Err(Error::InvalidProof("proof invalid: invalid parent"));
                }

                let elem = Element::deserialize(result_set[0].1.as_slice())?;
                let child_hash = match elem {
                    Element::Tree(hash) => Ok(hash),
                    _ => Err(Error::InvalidProof(
                        "intermediate proofs should be for trees",
                    )),
                }?;

                if child_hash != *expected_root_hash {
                    return Err(Error::InvalidProof("Bad path"));
                }

                *expected_root_hash = proof_result.0;
            } else {
                break;
            }
            split_path = path_slice.split_last();
        }

        Ok(())
    }

    /// Generate expected root hash based on root proof and leaf hashes
    fn execute_root_proof(
        proof_reader: &mut ProofReader,
        leaf_hash: [u8; 32],
    ) -> Result<[u8; 32], Error> {
        let root_proof_bytes = proof_reader.read_proof_of_type(ProofType::RootProof.into())?;

        // makes the assumption that 1 byte is enough to represent the root leaf count
        // hence max of 255 root leaf keys
        let root_leaf_count = proof_reader.read_byte()?;

        let index_to_prove_as_bytes = proof_reader.read_to_end()?;
        let index_to_prove_as_usize = index_to_prove_as_bytes
            .into_iter()
            .map(|index| index as usize)
            .collect::<Vec<usize>>();

        let root_proof = match MerkleProof::<Sha256>::try_from(root_proof_bytes) {
            Ok(proof) => Ok(proof),
            Err(_) => Err(Error::InvalidProof("invalid proof element")),
        }?;

        let root_hash = match root_proof.root(
            &index_to_prove_as_usize,
            &[leaf_hash],
            root_leaf_count[0] as usize,
        ) {
            Ok(hash) => Ok(hash),
            Err(_) => Err(Error::InvalidProof("Invalid proof element")),
        }?;

        Ok(root_hash)
    }

    fn execute_merk_proof(
        &mut self,
        proof_type: ProofType,
        proof: &Vec<u8>,
        query: &Query,
        left_to_right: bool,
    ) -> Result<(Hash, Vec<(Vec<u8>, Vec<u8>)>), Error> {
        let is_sized_proof = proof_type == ProofType::SizedMerkProof;
        let mut limit = None;
        let mut offset = None;

        if is_sized_proof {
            limit = self.limit;
            offset = self.offset;
        }

        let (hash, result) = merk::execute_proof(proof, query, limit, offset, left_to_right)
            .map_err(|e| {
                eprintln!("{}", e.to_string());
                Error::InvalidProof("invalid proof verification parameters")
            })?;

        if is_sized_proof {
            self.limit = result.limit;
            self.offset = result.offset;
        }

        Ok((hash, result.result_set))
    }
}
