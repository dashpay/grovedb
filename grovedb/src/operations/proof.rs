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
        // A path query has a path and then a query
        // First we find the merk at the defined path
        // if there is no merk found at that path, then we return an error
        // if there is then we construct a proof on the merk with the query
        // then subsequently construct proofs for all parents up to the
        // root tree.
        // As we do this we aggregate the proofs in a reproducible structure

        // for encoding the proof, need to know when the length of a merk proof
        // so I can read that amount
        // Using type, length, value encoding
        // merk_path - 0x01
        // root-path - 0x02
        // meta-data - 0x10
        // TODO: Remove this assumptions (length are represented with a single byte)
        // TODO: Transition to variable length encoding
        // length is represented by a single byte specifying how long the proof is
        // value is the proof (just read the size specified by the length)
        // to do this I need to be able to write certain info into a slice
        // a generic function that takes a writer and a vector to be written??
        let mut proof_result: Vec<u8> = vec![];

        // 1. Get the merk at the path defined by the query
        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        // checks if the subtree exists
        self.check_subtree_exists_path_not_found(path_slices.clone(), None, None)?;

        // Leaf node, we care about the result set of this
        merk_optional_tx!(self.db, path_slices.clone(), None, subtree, {
            // let a = subtree.get(b"key1");
            // dbg!(a);
            // let b = subtree.get(b"key2");
            // dbg!(b);
            // panic!();
            // dbg!("MERK!!");
            // TODO: Not allowed to create proof for an empty tree (handle this)
            // dbg!(subtree.root_hash());
            let proof = subtree
                .prove(query.query.query, None, None)
                .expect("should generate proof");
            // TODO: Switch to variable length encoding
            debug_assert!(proof.len() < 256);
            write_to_vec(&mut proof_result, &vec![MERK_PROOF, proof.len() as u8]);
            write_to_vec(&mut proof_result, &proof);
        });

        // Generate proof up to root
        // Non leaf nodes, we don't care about the result set of this
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // dbg!("ROOT");
                // dbg!("gotten to root");
                // generate the root proof
                // rs-merkle stores the root keys as indexes
                // grovedb has a way to convert from readable names to those indexes
                // the goal here is to take the key value and convert it to the correct index
                // insert it into a vector, then use the vector to generate a root proof
                meta_storage_context_optional_tx!(self.db, None, meta_storage, {
                    // TODO: verify the correctness of this
                    // if we cannot get the root_left_keys then something is wrong should propagate
                    let root_leaf_keys = Self::get_root_leaf_keys_internal(&meta_storage)?;
                    let mut root_index: Vec<usize> = vec![];
                    match root_leaf_keys.get(&key.to_vec()) {
                        Some(index) => root_index.push(*index),
                        // technically, this should not be possible as the path should
                        // have caught this already
                        None => return Err(InvalidPath("invalid root key")),
                    }
                    let root_tree = self.get_root_tree(None).expect("should get root tree");
                    let root_proof = root_tree.proof(&root_index).to_bytes();
                    // dbg!(root_proof);

                    debug_assert!(root_proof.len() < 256);
                    write_to_vec(&mut proof_result, &vec![ROOT_PROOF, root_proof.len() as u8]);
                    write_to_vec(&mut proof_result, &root_proof);

                    // add the root proof to output vec
                    let mut root_index_bytes = root_index
                        .into_iter()
                        .map(|index| index as u8)
                        .collect::<Vec<u8>>();

                    // TODO: Save an extra byte?
                    // write_to_vec(&mut proof_result, &vec![0x10]);
                    write_to_vec(&mut proof_result, &root_index_bytes);
                })
            } else {
                // dbg!("MERK");
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
                    // dbg!(proof);
                });
            }
            split_path = path_slice.split_last();
        }

        Ok(proof_result)
        // dbg!(proof_result);
        //
        // Err(Error::InvalidQuery("invalid query"))
    }

    // Abstract the reading process
    // we really only care about the proof packets
    // we currently read merk proofs, root proofs and metadata
    // we verify that those proofs are the next in the proof object
    // how can we encapsulate this in a single function
    // read_proof, takes the type for validation

    fn read_proof(mut proof_data: &[u8], expected_data_type: u8) -> Result<Vec<u8>, Error> {
        let mut data_type = [0; 1];
        proof_data.read(&mut data_type);

        if data_type != [expected_data_type] {
           return Err(Error::InvalidProof("wrong data_type"));
        }

        let mut length = vec![0; 1];
        proof_data.read(&mut length);
        let mut proof = vec![0; length[0] as usize];
        proof_data.read(&mut proof);

        Ok(proof)
    }

    pub fn execute_proof(
        mut proof: &[u8],
        query: PathQuery,
    ) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        // Path is composed of keys, need to split last and verify that the root hash
        // of last merk is a value of parent merk at specified key

        // let result_set;
        // let mut last_root_hash;

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        // Sequence
        // Read type
        // if merk type, read proof length, then read proof
        // execute the proof, store the result set and the last hash
        // split the path, execute the next proof, verify that the
        // result set contains the root hash of the previous tree at that key
        let mut data_type = [0; 1];
        proof.read(&mut data_type);

        let mut length = vec![0; 1];
        proof.read(&mut length);
        let mut proof_data = vec![0; length[0] as usize];
        proof.read(&mut proof_data);
        // dbg!(&proof_data);

        let (mut last_root_hash, result_set) =
            merk::execute_proof(&proof_data, &query.query.query, None, None, true)
                .expect("should execute proof");
        // dbg!(last_root_hash);

        // Validate the path
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            // dbg!("in");
            if !path_slice.is_empty() {
                // more merk proofs
                // TODO: remove duplication
                let mut data_type = [0; 1];
                proof.read(&mut data_type);
                // dbg!(&data_type);

                if data_type != [MERK_PROOF] {
                    return Err(Error::InvalidProof("proof invalid: not merk proof"));
                }

                let mut length = vec![0; 1];
                proof.read(&mut length);
                let mut proof_data = vec![0; length[0] as usize];
                proof.read(&mut proof_data);

                let mut query = Query::new();
                query.insert_key(key.to_vec());

                let proof_result = merk::execute_proof(&proof_data, &query, None, None, true)
                    .expect("should execute proof");
                let result_set = proof_result.1.result_set;
                // dbg!(&result_set);

                // Take the first tuple of the result set
                // TODO: convert result_set to hash_map
                // make sure the key matches
                // convert the result to an element and make sure the
                // hash of the element is the same as the last root hash
                // set the current hash to the last root_hash
                if result_set[0].0 != key.to_vec() {
                    return Err(Error::InvalidProof("proof invalid: invalid parent"));
                }
                let elem = Element::deserialize(result_set[0].1.as_slice())?;
                let child_hash = match elem {
                    Element::Tree(hash) => Ok(hash),
                    _ => Err(Error::InvalidProof(
                        "intermidiate proofs should be for trees",
                    )),
                }?;
                // dbg!(&child_hash);

                if child_hash != last_root_hash {
                    return Err(Error::InvalidProof("Bad path"));
                }

                last_root_hash = proof_result.0;
            } else {
                break;
            }
            split_path = path_slice.split_last();
        }

        // match data_type {
        //     [0x01] => {
        //         dbg!("merk proof");
        //         let mut length = vec![0; 1];
        //         proof.read(&mut length);
        //         let mut proof_data = vec![0; length[0] as usize];
        //         proof.read(&mut proof_data);
        //         dbg!(proof_data);
        //     }
        //     _ => {
        //         dbg!("unknown");
        //     }
        // }

        // Verify the root proof
        // read the root proof data
        // read the meta data
        // read the root data
        let mut data_type = [0; 1];
        proof.read(&mut data_type);
        // dbg!(&data_type);

        if data_type != [ROOT_PROOF] {
            return Err(Error::InvalidProof("proof invalid: not root proof"));
        }

        let mut length = vec![0; 1];
        proof.read(&mut length);
        let mut root_proof = vec![0; length[0] as usize];
        proof.read(&mut root_proof);
        // dbg!(&root_proof);

        // dbg!(&proof);
        let mut root_meta_data = vec![];
        proof.read_to_end(&mut root_meta_data);
        let mut root_index_usize = root_meta_data
            .into_iter()
            .map(|index| index as usize)
            .collect::<Vec<usize>>();

        // Get the root hash after verifying the root proof
        let root_proof_terrible_name = match MerkleProof::<Sha256>::try_from(root_proof) {
            Ok(proof) => Ok(proof),
            Err(_) => Err(Error::InvalidProof("invalid proof element")),
        }?;

        let root_hash = match root_proof_terrible_name.root(&root_index_usize, &[last_root_hash], 2)
        {
            Ok(hash) => Ok(hash),
            Err(_) => Err(Error::InvalidProof("Invalid proof element")),
        }?;

        Ok((root_hash, result_set.result_set))
        // dgb!(root_hash);
    }
}
