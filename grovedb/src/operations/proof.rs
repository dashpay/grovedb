use std::{
    env::split_paths,
    io::{Read, Write},
};

use rs_merkle::{algorithms::Sha256, MerkleProof};
use storage::{RawIterator, StorageContext};

use crate::{
    util::{merk_optional_tx, meta_storage_context_optional_tx},
    Element, Error,
    Error::InvalidPath,
    GroveDb, PathQuery, Query,
};

const MERK_PROOF: u8 = 0x01;
const ROOT_PROOF: u8 = 0x02;

fn write_to_vec<W: Write>(dest: &mut W, value: &Vec<u8>) {
    dest.write_all(value);
}

impl GroveDb {
    pub fn prove(&self, query: PathQuery) -> Result<Vec<u8>, Error> {
        let mut proof_result: Vec<u8> = vec![];

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        self.check_subtree_exists_path_not_found(path_slices.clone(), None, None)?;

        merk_optional_tx!(self.db, path_slices.clone(), None, subtree, {
            // TODO: Not allowed to create proof for an empty tree (handle this)
            let proof = subtree
                .prove(query.query.query, None, None)
                .expect("should generate proof");

            // TODO: Switch to variable length encoding
            debug_assert!(proof.len() < 256);
            write_to_vec(&mut proof_result, &vec![MERK_PROOF, proof.len() as u8]);
            write_to_vec(&mut proof_result, &proof);
        });

        // generate proof up to root
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // generate root proof
                meta_storage_context_optional_tx!(self.db, None, meta_storage, {
                    let root_leaf_keys = Self::get_root_leaf_keys_internal(&meta_storage)?;
                    let mut root_index: Vec<usize> = vec![];
                    match root_leaf_keys.get(&key.to_vec()) {
                        Some(index) => root_index.push(*index),
                        None => return Err(InvalidPath("invalid root key")),
                    }
                    let root_tree = self.get_root_tree(None).expect("should get root tree");
                    let root_proof = root_tree.proof(&root_index).to_bytes();

                    debug_assert!(root_proof.len() < 256);
                    write_to_vec(&mut proof_result, &vec![ROOT_PROOF, root_proof.len() as u8]);
                    write_to_vec(&mut proof_result, &root_proof);

                    // add the index values required to prove the root
                    let mut root_index_bytes = root_index
                        .into_iter()
                        .map(|index| index as u8)
                        .collect::<Vec<u8>>();

                    write_to_vec(&mut proof_result, &root_index_bytes);
                })
            } else {
                let path_slices = path_slice.iter().map(|x| *x).collect::<Vec<_>>();

                merk_optional_tx!(self.db, path_slices, None, subtree, {
                    // TODO: Not allowed to create proof for an empty tree (handle this)
                    let mut query = Query::new();
                    query.insert_key(key.to_vec());

                    let proof = subtree
                        .prove(query, None, None)
                        .expect("should generate proof");

                    debug_assert!(proof.len() < 256);
                    write_to_vec(&mut proof_result, &vec![MERK_PROOF, proof.len() as u8]);
                    write_to_vec(&mut proof_result, &proof);
                });
            }
            split_path = path_slice.split_last();
        }

        Ok(proof_result)
    }

    pub fn execute_proof(
        mut proof: &[u8],
        query: PathQuery,
    ) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();
        let mut proof_reader = ProofReader::new(proof);

        let merk_proof = proof_reader.read_proof(MERK_PROOF)?;

        let (mut last_root_hash, result_set) =
            merk::execute_proof(&merk_proof, &query.query.query, None, None, true)
                .expect("should execute proof");

        // Validate the path
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if !path_slice.is_empty() {
                let merk_proof = proof_reader.read_proof(MERK_PROOF)?;

                let mut query = Query::new();
                query.insert_key(key.to_vec());

                let proof_result = merk::execute_proof(&merk_proof, &query, None, None, true)
                    .expect("should execute proof");
                let result_set = proof_result.1.result_set;

                if result_set[0].0 != key.to_vec() {
                    return Err(Error::InvalidProof("proof invalid: invalid parent"));
                }
                let elem = Element::deserialize(result_set[0].1.as_slice())?;
                let child_hash = match elem {
                    Element::Tree(hash) => Ok(hash),
                    _ => Err(Error::InvalidProof(
                        "intermediate proofs should be for trees",
                    )),
                }?;

                if child_hash != last_root_hash {
                    return Err(Error::InvalidProof("Bad path"));
                }

                last_root_hash = proof_result.0;
            } else {
                break;
            }
            split_path = path_slice.split_last();
        }

        let root_proof = proof_reader.read_proof(ROOT_PROOF)?;

        let root_meta_data = proof_reader.read_to_end();
        let mut root_index_usize = root_meta_data
            .into_iter()
            .map(|index| index as usize)
            .collect::<Vec<usize>>();

        let root_proof_terrible_name = match MerkleProof::<Sha256>::try_from(root_proof) {
            Ok(proof) => Ok(proof),
            Err(_) => Err(Error::InvalidProof("invalid proof element")),
        }?;

        // TODO: Don't hard code the leave count
        let root_hash = match root_proof_terrible_name.root(&root_index_usize, &[last_root_hash], 2)
        {
            Ok(hash) => Ok(hash),
            Err(_) => Err(Error::InvalidProof("Invalid proof element")),
        }?;

        Ok((root_hash, result_set.result_set))
    }
}

struct ProofReader<'a> {
    proof_data: &'a [u8],
}

impl<'a> ProofReader<'a> {
    fn new(proof_data: &'a [u8]) -> Self {
        Self { proof_data }
    }

    fn read_proof(&mut self, expected_data_type: u8) -> Result<Vec<u8>, Error> {
        let mut data_type = [0; 1];
        self.proof_data.read(&mut data_type);

        if data_type != [expected_data_type] {
            return Err(Error::InvalidProof("wrong data_type"));
        }

        let mut length = vec![0; 1];
        self.proof_data.read(&mut length);
        let mut proof = vec![0; length[0] as usize];
        self.proof_data.read(&mut proof);

        Ok(proof)
    }

    fn read_to_end(&mut self) -> Vec<u8> {
        let mut data = vec![];
        self.proof_data.read_to_end(&mut data);
        data
    }
}
