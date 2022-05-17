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
        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        if path_slices.len() < 1 {
            return Err(Error::InvalidPath("can't verify proof for empty path"));
        }

        let mut result_set: Vec<(Vec<u8>, Vec<u8>)> = vec![];
        let mut proof_reader = ProofReader::new(proof);

        let mut current_limit = query.query.limit;
        let mut current_offset = query.query.offset;

        let mut expected_root_hash = GroveDb::execute_subquery_proof(
            &mut proof_reader,
            &mut result_set,
            &mut current_limit,
            &mut current_offset,
            query.clone(),
        )?;

        // validate the path elements are connected
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if !path_slice.is_empty() {
                // for every subtree, there should be a corresponding proof for the parent
                // which should prove that this subtree is a child of the parent tree
                let parent_merk_proof =
                    proof_reader.read_proof_of_type(ProofType::MerkProof.into())?;

                let mut parent_query = Query::new();
                parent_query.insert_key(key.to_vec());

                let proof_result = execute_merk_proof(
                    &parent_merk_proof,
                    &parent_query,
                    None,
                    None,
                    query.query.query.left_to_right,
                )?;

                let result_set = proof_result.1.result_set;
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

                if child_hash != expected_root_hash {
                    return Err(Error::InvalidProof("Bad path"));
                }

                expected_root_hash = proof_result.0;
            } else {
                break;
            }
            split_path = path_slice.split_last();
        }

        // execute the root proof
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
            &[expected_root_hash],
            root_leaf_count[0] as usize,
        ) {
            Ok(hash) => Ok(hash),
            Err(_) => Err(Error::InvalidProof("Invalid proof element")),
        }?;

        Ok((root_hash, result_set))
    }

    fn execute_subquery_proof(
        proof_reader: &mut ProofReader,
        result_set: &mut Vec<(Vec<u8>, Vec<u8>)>,
        current_limit: &mut Option<u16>,
        current_offset: &mut Option<u16>,
        query: PathQuery,
    ) -> Result<[u8; 32], Error> {
        let root_hash: [u8; 32];
        let (proof_type, proof) = proof_reader.read_proof()?;
        match proof_type {
            ProofType::SizedMerkProof => {
                let verification_result = execute_merk_proof(
                    &proof,
                    &query.query.query,
                    *current_limit,
                    *current_offset,
                    query.query.query.left_to_right,
                )?;

                root_hash = verification_result.0;
                result_set.extend(verification_result.1.result_set);

                // update limit and offset
                *current_limit = verification_result.1.limit;
                *current_offset = verification_result.1.offset;
            }
            ProofType::MerkProof => {
                // for non leaf subtrees, we want to prove that all their keys
                // have an accompanying proof as long as the limit is non zero
                // and their child subtree is not empty
                let mut all_key_query = Query::new_with_direction(query.query.query.left_to_right);
                all_key_query.insert_all();

                let verification_result = execute_merk_proof(
                    &proof,
                    &all_key_query,
                    None,
                    None,
                    all_key_query.left_to_right,
                )?;

                root_hash = verification_result.0;

                for (key, value_bytes) in verification_result.1.result_set {
                    let child_element = Element::deserialize(value_bytes.as_slice())?;
                    match child_element {
                        Element::Tree(mut expected_root_hash) => {
                            if expected_root_hash == EMPTY_TREE_HASH {
                                // child node is empty, move on to next
                                continue;
                            }

                            if *current_limit == Some(0) {
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

                            if subquery_key.is_some() {
                                // prove that the subquery key was used, update the expected hash
                                // if the proof shows absence, path is no longer useful
                                // move on to next
                                let (proof_type, subkey_proof) = proof_reader.read_proof()?;
                                if proof_type != ProofType::MerkProof {
                                    return Err(Error::InvalidProof(
                                        "expected unsized merk proof for subquery key",
                                    ));
                                }

                                let mut key_as_query = Query::new();
                                key_as_query.insert_key(subquery_key.clone().unwrap());

                                let verification_result = execute_merk_proof(
                                    &subkey_proof,
                                    &key_as_query,
                                    None,
                                    None,
                                    key_as_query.left_to_right,
                                )?;

                                let subquery_key_result_set = verification_result.1.result_set;
                                if subquery_key_result_set.len() == 0 {
                                    // subquery key does not exist in the subtree
                                    // proceed to another subtree
                                    continue;
                                } else {
                                    let elem_value = &subquery_key_result_set[0].1;
                                    let subquery_key_element = Element::deserialize(elem_value)
                                        .map_err(|_| {
                                            Error::CorruptedData(
                                                "failed to deserialize element".to_string(),
                                            )
                                        })?;
                                    match subquery_key_element {
                                        Element::Tree(new_exptected_hash) => {
                                            expected_root_hash = new_exptected_hash;
                                        }
                                        _ => {
                                            // the means that the subquery key pointed to a non tree
                                            // element
                                            // what do you do in that case, say it points to an item
                                            // or reference
                                            // pointing to a non tree element means we cannot apply
                                            // TODO: Remove panic
                                            panic!("figure out what to do in this case");
                                        }
                                    }
                                }
                            }

                            let new_path_query;
                            if subquery_value.is_some() {
                                new_path_query =
                                    PathQuery::new_unsized(vec![], subquery_value.unwrap());
                            } else {
                                let mut key_as_query = Query::new();
                                key_as_query.insert_key(subquery_key.unwrap());
                                new_path_query = PathQuery::new_unsized(vec![], key_as_query);
                            }

                            let child_hash = GroveDb::execute_subquery_proof(
                                proof_reader,
                                result_set,
                                current_limit,
                                current_offset,
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
        Ok(root_hash)
    }
}

fn execute_merk_proof(
    proof: &Vec<u8>,
    query: &Query,
    limit: Option<u16>,
    offset: Option<u16>,
    left_to_right: bool,
) -> Result<(Hash, ProofVerificationResult), Error> {
    Ok(
        merk::execute_proof(proof, query, limit, offset, left_to_right).map_err(|e| {
            eprintln!("{}", e.to_string());
            Error::InvalidProof("invalid proof verification parameters")
        })?,
    )
}
