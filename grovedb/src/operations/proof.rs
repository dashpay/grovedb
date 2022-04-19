use std::{
    io::{Read, Write},
    ptr::write,
};

use merk::{
    proofs::{
        encode_into,
        query::{ProofVerificationResult, QueryItem},
        Node, Op,
    },
    Hash, Merk,
};
use rs_merkle::{algorithms::Sha256, MerkleProof};
use storage::{rocksdb_storage::RocksDbStorage, StorageContext};

use crate::{
    merk::ProofConstructionResult,
    subtree::raw_decode,
    util::{merk_optional_tx, meta_storage_context_optional_tx},
    Element, Error,
    Error::{InvalidPath, InvalidProof},
    GroveDb, PathQuery, Query, SizedQuery,
};

const EMPTY_TREE_HASH: [u8; 32] = [0; 32];

#[derive(Debug, PartialEq)]
enum ProofType {
    MerkProof,
    SizedMerkProof,
    RootProof,
    InvalidProof,
}

impl From<ProofType> for u8 {
    fn from(proof_type: ProofType) -> Self {
        match proof_type {
            ProofType::MerkProof => 0x01,
            ProofType::SizedMerkProof => 0x02,
            ProofType::RootProof => 0x03,
            ProofType::InvalidProof => 0x10,
        }
    }
}

impl From<u8> for ProofType {
    fn from(val: u8) -> Self {
        match val {
            0x01 => ProofType::MerkProof,
            0x02 => ProofType::SizedMerkProof,
            0x03 => ProofType::RootProof,
            _ => ProofType::InvalidProof,
        }
    }
}

impl GroveDb {
    pub fn prove(&self, query: PathQuery) -> Result<Vec<u8>, Error> {
        // TODO: should it be possible to generate proofs for tree items (currently yes)
        let mut proof_result: Vec<u8> = vec![];

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        if path_slices.len() < 1 {
            return Err(Error::InvalidPath("can't generate proof for empty path"));
        }

        self.check_subtree_exists_path_not_found(path_slices.clone(), None, None)?;

        let mut current_limit: Option<u16> = query.query.limit;
        let mut current_offset: Option<u16> = query.query.offset;

        GroveDb::prove_subqueries(
            &self,
            &mut proof_result,
            path_slices.clone(),
            query.clone(),
            &mut current_limit,
            &mut current_offset,
        )?;

        // generate a proof to show that the path leads up to the root
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // generate root proof
                meta_storage_context_optional_tx!(self.db, None, meta_storage, {
                    let root_leaf_keys = Self::get_root_leaf_keys_internal(&meta_storage)?;
                    let mut index_to_prove: Vec<usize> = vec![];
                    match root_leaf_keys.get(&key.to_vec()) {
                        Some(index) => index_to_prove.push(*index),
                        None => return Err(InvalidPath("invalid root key")),
                    }
                    let root_tree = self.get_root_tree(None).expect("should get root tree");
                    let root_proof = root_tree.proof(&index_to_prove).to_bytes();

                    debug_assert!(root_proof.len() < 256);
                    write_to_vec(
                        &mut proof_result,
                        &vec![ProofType::RootProof.into(), root_proof.len() as u8],
                    );
                    write_to_vec(&mut proof_result, &root_proof);

                    // write the number of root leafs
                    // this makes the assumption that 1 byte is enough to represent the number of
                    // root leafs i.e max of 255 root leaf keys
                    // TODO: How do we enforce this? does it make sense to make this variable
                    // length?
                    write_to_vec(&mut proof_result, &vec![root_leaf_keys.len() as u8]);

                    // add the index values required to prove the root
                    let index_to_prove_as_bytes = index_to_prove
                        .into_iter()
                        .map(|index| index as u8)
                        .collect::<Vec<u8>>();

                    write_to_vec(&mut proof_result, &index_to_prove_as_bytes);
                })
            } else {
                // generate proofs for the intermediate paths
                let path_slices = path_slice.iter().map(|x| *x).collect::<Vec<_>>();

                merk_optional_tx!(self.db, path_slices, None, subtree, {
                    let mut query = Query::new();
                    query.insert_key(key.to_vec());

                    generate_and_store_merk_proof(
                        &self,
                        &subtree,
                        query,
                        None,
                        None,
                        ProofType::MerkProof,
                        &mut proof_result,
                    );
                });
            }
            split_path = path_slice.split_last();
        }

        Ok(proof_result)
    }

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
                // for every subtree, we should have a corresponding proof for the parent
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
                if result_set.len() == 0 || result_set[0].0 != key.to_vec() {
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

        let index_to_prove_as_bytes = proof_reader.read_to_end();
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

    fn prove_subqueries(
        db: &GroveDb,
        proofs: &mut Vec<u8>,
        path: Vec<&[u8]>,
        query: PathQuery,
        current_limit: &mut Option<u16>,
        current_offset: &mut Option<u16>,
    ) -> Result<(), Error> {
        // there is a chance that the subquery key would lead to something that is not a
        // tree same thing for the subquery itself
        merk_optional_tx!(db.db, path.clone(), None, subtree, {
            let mut has_useful_subtree = false;
            let exhausted_limit = query.query.limit.is_some() && query.query.limit.unwrap() == 0;

            if !exhausted_limit {
                let subtree_key_values = subtree.get_kv_pairs(query.query.query.left_to_right);
                for (key, value_bytes) in subtree_key_values.iter() {
                    let (subquery_key, subquery_value) =
                        Element::subquery_paths_for_sized_query(&query.query, key);

                    if subquery_key.is_none() && subquery_value.is_none() {
                        continue;
                    }

                    let element = raw_decode(value_bytes)?;

                    match element {
                        // TODO: Look here when dealing with references
                        Element::Tree(tree_hash) => {
                            if tree_hash == EMPTY_TREE_HASH {
                                continue;
                            }

                            if !has_useful_subtree {
                                has_useful_subtree = true;

                                let mut all_key_query =
                                    Query::new_with_direction(query.query.query.left_to_right);
                                all_key_query.insert_all();

                                generate_and_store_merk_proof(
                                    db,
                                    &subtree,
                                    all_key_query,
                                    None,
                                    None,
                                    ProofType::MerkProof,
                                    proofs,
                                );
                            }

                            let mut new_path = path.clone();
                            new_path.push(key.as_ref());

                            let mut query = subquery_value.clone();
                            let sub_key = subquery_key.clone();

                            if query.is_some() {
                                if sub_key.is_some() {
                                    // intermediate step here, generate a proof that show
                                    // the existence or absence of the subquery key
                                    merk_optional_tx!(
                                        db.db,
                                        new_path.clone(),
                                        None,
                                        inner_subtree,
                                        {
                                            let mut key_as_query = Query::new();
                                            key_as_query.insert_key(sub_key.clone().unwrap());

                                            generate_and_store_merk_proof(
                                                db,
                                                &inner_subtree,
                                                key_as_query,
                                                None,
                                                None,
                                                ProofType::MerkProof,
                                                proofs,
                                            );
                                        }
                                    );

                                    new_path.push(sub_key.as_ref().unwrap());

                                    let subquery_key_path_exists = db
                                        .check_subtree_exists_path_not_found(
                                            new_path.clone(),
                                            None,
                                            None,
                                        );

                                    if subquery_key_path_exists.is_err() {
                                        dbg!("leaving");
                                        continue;
                                    }
                                }
                            } else {
                                let mut key_as_query = Query::new();
                                key_as_query.insert_key(sub_key.unwrap());
                                query = Some(key_as_query);
                            }

                            let new_path_owned = new_path.iter().map(|x| x.to_vec()).collect();
                            let new_path_query =
                                PathQuery::new_unsized(new_path_owned, query.unwrap());

                            GroveDb::prove_subqueries(
                                db,
                                proofs,
                                new_path,
                                new_path_query,
                                current_limit,
                                current_offset,
                            )?;

                            // if we hit the limit, we should kill the loop
                            if current_limit.is_some() && current_limit.unwrap() == 0 {
                                break;
                            }
                        }
                        _ => {
                            // Current implementation makes the assumption that all elements of
                            // a result set are of the same type i.e either all trees, all items
                            // e.t.c and not mixed types.
                            // This ensures that invariant is preserved
                            debug_assert!(has_useful_subtree == false);
                        }
                    }
                }
            }

            // TODO: Explore the chance that a subquerykey might lead to non tree element
            if !has_useful_subtree {
                // if no useful subtree, then we care about the result set of this subtree.
                // apply the sized query
                let limit_offset = generate_and_store_merk_proof(
                    db,
                    &subtree,
                    query.query.query,
                    *current_limit,
                    *current_offset,
                    ProofType::SizedMerkProof,
                    proofs,
                );

                // update limit and offset values
                *current_limit = limit_offset.0;
                *current_offset = limit_offset.1;
            }
        });

        Ok(())
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

                            if current_limit.is_some() && current_limit.unwrap() == 0 {
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
                                    let subquery_key_element =
                                        Element::deserialize(elem_value).unwrap();
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
                            // TODO: why this error??
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

// Helpers
// TODO: Extract into seperate files
#[derive(Debug)]
struct ProofReader<'a> {
    proof_data: &'a [u8],
}

impl<'a> ProofReader<'a> {
    fn new(proof_data: &'a [u8]) -> Self {
        Self { proof_data }
    }

    // TODO: handle error (e.g. not enough bytes to read)
    fn read_byte(&mut self) -> Result<[u8; 1], Error> {
        let mut data = [0; 1];
        self.proof_data.read(&mut data);
        Ok(data)
    }

    fn read_proof(&mut self) -> Result<(ProofType, Vec<u8>), Error> {
        self.read_proof_with_optional_type(None)
    }

    fn read_proof_of_type(&mut self, expected_data_type: u8) -> Result<Vec<u8>, Error> {
        match self.read_proof_with_optional_type(Some(expected_data_type)) {
            Ok((_, proof)) => Ok(proof),
            Err(e) => Err(e),
        }
    }

    // TODO: handle error (e.g. not enough bytes to read)
    fn read_proof_with_optional_type(
        &mut self,
        expected_data_type_option: Option<u8>,
    ) -> Result<(ProofType, Vec<u8>), Error> {
        let mut data_type = [0; 1];
        self.proof_data.read(&mut data_type);

        if let Some(expected_data_type) = expected_data_type_option {
            if data_type != [expected_data_type] {
                return Err(Error::InvalidProof("wrong data_type"));
            }
        }

        // TODO: This should not be the invalid proof type
        let proof_type: ProofType = data_type[0].into();

        let mut length = vec![0; 1];
        self.proof_data.read(&mut length);
        let mut proof = vec![0; length[0] as usize];
        self.proof_data.read(&mut proof);

        Ok((proof_type, proof))
    }

    fn read_to_end(&mut self) -> Vec<u8> {
        let mut data = vec![];
        self.proof_data.read_to_end(&mut data);
        data
    }
}

// TODO: Isn't it possible for this to return some kind of error?
fn generate_and_store_merk_proof<'a, S: 'a>(
    db: &GroveDb,
    subtree: &'a Merk<S>,
    query: Query,
    limit: Option<u16>,
    offset: Option<u16>,
    proof_type: ProofType,
    proofs: &mut Vec<u8>,
) -> (Option<u16>, Option<u16>)
where
    S: StorageContext<'a, 'a>,
{
    // TODO: How do you handle mixed tree types?
    let mut proof_result = subtree
        .prove_without_encoding(query, limit, offset)
        .expect("should generate proof");

    for a in proof_result.proof.iter_mut() {
        match a {
            Op::Push(node) | Op::PushInverted(node) => {
                match node {
                    Node::KV(key, value) => {
                        let elem = Element::deserialize(value);
                        // only care about the reference type
                        if let Ok(Element::Reference(reference_path)) = elem {
                            // TODO: handle error better
                            let referenced_elem =
                                db.follow_reference(reference_path, None).unwrap();
                            dbg!(&referenced_elem);
                            // update the current elem
                            // Handle error
                            *value = referenced_elem.serialize().unwrap();
                            // *a = Node::KV(key.clone(),
                            // referenced_elem.serialize()?);
                        }
                    }
                    _ => continue,
                }
            }
            _ => continue,
        }
    }

    let mut proof_bytes = Vec::with_capacity(128);
    encode_into(proof_result.proof.iter(), &mut proof_bytes);

    // TODO: Switch to variable length encoding
    debug_assert!(proof_bytes.len() < 256);
    dbg!(proof_bytes.len());
    write_to_vec(proofs, &vec![proof_type.into(), proof_bytes.len() as u8]);
    write_to_vec(proofs, &proof_bytes);

    (proof_result.limit, proof_result.offset)
}

fn write_to_vec<W: Write>(dest: &mut W, value: &Vec<u8>) {
    dest.write_all(value);
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
