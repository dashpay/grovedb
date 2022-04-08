use std::io::{Read, Write};

use rs_merkle::{algorithms::Sha256, MerkleProof};
use storage::rocksdb_storage::RocksDbStorage;

use crate::{
    merk::ProofConstructionResult,
    subtree::raw_decode,
    util::{merk_optional_tx, meta_storage_context_optional_tx},
    Element, Error,
    Error::InvalidPath,
    GroveDb, PathQuery, Query, SizedQuery,
};

#[derive(Debug)]
enum ProofType {
    MERK_PROOF,
    SIZED_MERK_PROOF,
    ROOT_PROOF,
    INVALID_TYPE,
}

impl From<ProofType> for u8 {
    fn from(proof_type: ProofType) -> Self {
        match proof_type {
            ProofType::MERK_PROOF => 0x01,
            ProofType::SIZED_MERK_PROOF => 0x02,
            ProofType::ROOT_PROOF => 0x03,
            ProofType::INVALID_TYPE => 0x10
        }
    }
}

impl From<u8> for ProofType {
    fn from(val: u8) -> Self {
        match val {
            0x01 => ProofType::MERK_PROOF,
            0x02 => ProofType::SIZED_MERK_PROOF,
            0x03 => ProofType::ROOT_PROOF,
            _ => ProofType::INVALID_TYPE,
        }
    }
}

fn write_to_vec<W: Write>(dest: &mut W, value: &Vec<u8>) {
    dbg!(&value);
    dest.write_all(value);
}

impl GroveDb {
    pub fn prove(&self, query: PathQuery) -> Result<Vec<u8>, Error> {
        // TODO: Should people be allowed to get proofs for tree items?? defaulting to
        // yes
        let mut proof_result: Vec<u8> = vec![];

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        self.check_subtree_exists_path_not_found(path_slices.clone(), None, None)?;

        // Next up is to take into account subqueries, starting with just one subquery
        // deep might need two additional markers, to signify children and
        // parents limit and offset relationship would also change, we only want
        // to apply the limit and offset parameters to the leaf nodes i.e. nodes
        // that after query application they have no children that are of the
        // tree element type or there is no new subquery essentially, this means
        // that we need to get the result of applying a query to an subtree
        // first, before constructing the proof (not sure I like this)
        // If the result set has elements that are tree items then we construct a new
        // path, pass the new path query ....

        // Need to take into account, limit, offset, path_query_exists, has subtree
        // Once you enter a parent, you have to prove all the children?? (that won't be
        // very efficient).
        // Say a parent has 10 subtrees, and there is a limit of 100, if the first two
        // subtrees exhaust this limit, then we have no reason to generate
        // proofs for 8 other subtrees and we close with an early parent step,
        // also no need to go down further on any subsequent parent
        // (only recurse and prove if the limit is non zero)

        // There is nothing stopping a tree from having a combination of different
        // element types TODO: Figure out how to properly deal with references
        // There is a possibility that the result set might span multiple trees if the
        // subtrees hold different element types (brings too much complexity,
        // will ignore for now) TODO: What to do if a subtree returns a
        // combination of item, reference and tree elements TODO: How would they
        // be added to the result set, limit, offset e.t.c.

        // Latest assumption: Trees only contain one type of Element
        // TODO: Add debug assert to enforce this constraint
        // Either the subquery can be applied to all elements or none

        // How do we deal with limits and offset in such a system.
        // we generally only want to apply the limit and offset values to subtree trees
        // that don't have subtrees (we also want to propagate the resulting
        // values up for further subtrees) since we create proofs for all child
        // nodes before parents, we know if any of the child nodes was a subtree
        // (set a flag then generate this merk's proof without limit and offset).
        // we keep a global track of the limit and offset??

        // Rough algo
        // Get the elements of the subtree
        // For all values that are trees, we do the same thing
        // (set a flag, add the child start marker to the proof)
        // after checking all the child values, check the flag if leaf node then
        // generate this proof with limit and offset, if not just generate a
        // plain proof.

        // How would verification work, we need to create a parent child context
        // everything we see the child marker, we create a new scope that keeps track
        // of the child's path key and the last_root_hash of that child
        // ISSUE: We don't know the child's key damn!!!!!!
        // we need the child's key to verify that it is indeed a child of the parent
        // that's coming the parent knows it's children (and what we really care
        // about is that all the parent's children were proved)
        // hence we only care about the root hashes of the child merks and their
        // relative order. No problem then, we create a new context when we see
        // the child marker verify each child element (with the appropriate
        // query, limit and offset) and store their root hash when the parent
        // marker is seen, pull the parent, verify the merk proof

        // Ideally, you prove every child in the parent merk, if you do not then you
        // need to show that the limit is 0 (as justification for truncating the
        // child proofs)

        // TODO: Reorganize thoughts
        // if allowed to prove tree items, then only prove with limit and offset it you
        // cannot go further down, else prove without them
        // Factors that determine if you can go further down are:
        // - are there any more subqueries
        // - does this element have any subtree
        // - is the limit non zero

        // For verification, the structure of the query should inform you about the
        // structure of the proof, so based on the query, you know what proof item
        // to expect, if you get something different then the proof was not constructed
        // correctly.
        // if you get the right thing, then perform additional constraint checks.

        prove_subqueries(
            &self.db,
            &mut proof_result,
            path_slices.clone(),
            query.clone(),
        );

        // TODO: return the propagated limit and offset values after running this
        fn prove_subqueries(
            db: &RocksDbStorage,
            proofs: &mut Vec<u8>,
            path: Vec<&[u8]>,
            query: PathQuery,
        ) -> Result<(Option<u16>, Option<u16>), Error> {
            // Track final limit and offset values for correct propagation
            let mut current_limit: Option<u16> = query.query.limit;
            let mut current_offset: Option<u16> = query.query.offset;

            // get subtree at given path
            // if there is no subquery
            // prove the current tree
            // if there is a subquery then
            // get all elements of the subtree
            // for each element in the subtree
            // if limit is zero break
            // else continue
            // if the element is a tree, then recurse
            // if it had subtrees, then generate proof without limit and offset
            // else use the limit and offset
            merk_optional_tx!(db, path.clone(), None, subtree, {
                // TODO: Not allowed to create proof for an empty tree (handle this)

                // Track if we can apply more subqueries to result set of the current merk
                // Factors that determine if you can go further down are:
                // - are there any more subqueries
                // - does this element have any subtree
                // - is the limit non zero
                let mut has_useful_subtree = false;

                // before getting the elements of the subtree, we should get the
                // subquery key and value
                // we have a query, that is inserted in a sized query for the path query
                // we only care about the query (not so simple)

                let (subquery_key, subquery_value) =
                    Element::default_subquery_paths_for_sized_query(&query.query);

                let has_subquery = subquery_key.is_some() || subquery_value.is_some();
                let exhausted_limit =
                    query.query.limit.is_some() && query.query.limit.unwrap() == 0;

                if has_subquery && !exhausted_limit {
                    dbg!("start");
                    let subtree_key_values = subtree.get_kv_pairs();
                    // TODO: make use of the direction
                    for (key, value_bytes) in subtree_key_values.iter() {
                        // TODO: Figure out what to do if decoding fails
                        let element = raw_decode(value_bytes).unwrap();
                        // dbg!(&element);

                        match element {
                            Element::Tree(_) => {
                                // we should add the proof of the current element
                                // before hitting the children
                                // since we know it has a useful subtree, then we
                                // know this is not a leaf node as such we can prove
                                // it without limit and offset
                                if !has_useful_subtree {
                                    // add the current elements merk proof
                                    has_useful_subtree = true;

                                    // generate unsized merk proof for current element
                                    // TODO: Remove duplication
                                    // TODO: How do you handle mixed tree types?
                                    // TODO: Get rid of query clone
                                    let ProofConstructionResult { proof, .. } = subtree
                                        .prove(query.query.query.clone(), None, None)
                                        .expect("should generate proof");

                                    // TODO: Switch to variable length encoding
                                    debug_assert!(proof.len() < 256);
                                    write_to_vec(
                                        proofs,
                                        &vec![ProofType::MERK_PROOF.into(), proof.len() as u8],
                                    );
                                    write_to_vec(proofs, &proof);
                                }

                                // recurse on this subtree, by creating a new
                                // path_slice
                                // with the new key
                                // function should return the resulting limits and
                                // offset should add to a global
                                // proof set (most likely a closure);
                                // TODO: cleanup
                                let mut new_path = path.clone();
                                new_path.push(key.as_ref());

                                let mut query = subquery_value.clone();
                                let sub_key = subquery_key.clone();

                                if query.is_some() {
                                    if sub_key.is_some() {
                                        new_path.push(sub_key.as_ref().unwrap());
                                    }
                                } else {
                                    // only subquery key must exist, convert to query
                                    // TODO: add direction
                                    let mut key_as_query = Query::new();
                                    key_as_query.insert_key(sub_key.unwrap());
                                    query = Some(key_as_query);
                                }

                                // dbg!(&new_path);
                                // dbg!(&query);

                                let new_path_owned = new_path.iter().map(|x| x.to_vec()).collect();
                                // TODO: Propagate the limit and offset values by creating a sized
                                // query
                                let new_path_query =
                                    PathQuery::new_unsized(new_path_owned, query.unwrap());

                                // signify you are about to add child proofs
                                // TODO: Taking this out for now as instruction might be in query
                                // itself write_to_vec(proofs,
                                // &vec![CHILD]);

                                // add proofs for child nodes
                                // TODO: Handle error properly, what could cause an error?
                                let limit_offset_result =
                                    prove_subqueries(db, proofs, new_path, new_path_query).unwrap();

                                // signify that you are done with child proofs
                                // TODO: Taking this out for now as instruction might be in query
                                // itself write_to_vec(proofs,
                                // &vec![PARENT]);

                                current_limit = limit_offset_result.0;
                                current_offset = limit_offset_result.1;

                                // if we hit the limit, we should kill the loop
                                if current_limit.is_some() && current_limit.unwrap() == 0 {
                                    dbg!("killing because we hit the limit");
                                    break;
                                }
                            }
                            _ => {
                                // Current implementation makes the assumption that all elements of
                                // a result set are of the same type i.e either all trees, all items
                                // e.t.c and not mixed types.
                                // This catches when that invariant is not preserved.
                                debug_assert!(has_useful_subtree == false);
                            }
                        }
                    }
                    dbg!("end");
                }

                // if the current element has a useful subtree then we already added the proof
                // for this element (skip proof addition).
                if !has_useful_subtree {
                    let proof_result = subtree
                        .prove(query.query.query, current_limit, current_offset)
                        .expect("should generate proof");

                    // update limit and offset values
                    current_limit = proof_result.limit;
                    current_offset = proof_result.offset;

                    // only adding to the proof result set, after you have added that of
                    // your child nodes
                    // TODO: Switch to variable length encoding
                    debug_assert!(proof_result.proof.len() < 256);
                    write_to_vec(
                        proofs,
                        &vec![
                            ProofType::SIZED_MERK_PROOF.into(),
                            proof_result.proof.len() as u8,
                        ],
                    );
                    write_to_vec(proofs, &proof_result.proof);
                }
            });

            Ok((current_limit, current_offset))
        }

        // generate proof up to root
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                // generate root proof
                // TODO: Encode the leave count
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
                    write_to_vec(
                        &mut proof_result,
                        &vec![ProofType::ROOT_PROOF.into(), root_proof.len() as u8],
                    );
                    write_to_vec(&mut proof_result, &root_proof);

                    // add the index values required to prove the root
                    let root_index_bytes = root_index
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

                    let ProofConstructionResult { proof, .. } = subtree
                        .prove(query, None, None)
                        .expect("should generate proof");

                    debug_assert!(proof.len() < 256);
                    write_to_vec(
                        &mut proof_result,
                        &vec![ProofType::MERK_PROOF.into(), proof.len() as u8],
                    );
                    write_to_vec(&mut proof_result, &proof);
                });
            }
            split_path = path_slice.split_last();
        }

        Ok(proof_result)
    }

    // Proof is divided into 2 main parts.
    // - subquery proofs and path proof
    // subquery proofs have the parent elements first then their children
    // there are two types of merk proofs now (sized and unsized), we care about
    // the result set of sized (sized just means this is a proof for a leaf node)
    // have to keep track of the result set (the same way we keep track of the proof
    // result for construction) the result set would be seen in their ordered
    // form Need a subroutine to verify the subqueries proof
    // How do we know when the subqery proof is done??
    // We know it starts the proof, we can either read a sized or unsized merk proof
    // TODO: Define changes to the proof reader
    // verify_subquery routine should be recursive also I believe
    // to verify you need to pass the query, limit and offset
    // we are doing parent first so that we know what query the child nodes would
    // need Hence we have to verify the first proof we read (ah the parents
    // would tell us what to expect) TODO: Might be possible to remove the child
    // and parent markers?? Read the parent, verify the proof with the current
    // query (get the result set) That tells you the keys you require proofs for
    // (if it was an unsized query - signifying there is more) TODO: Note that
    // this makes the same assumption that all elements of the result set are of the
    // same type really, all we care about it the actual type of the proof we
    // read unsized - verify and expect proof for the result set (unless limit
    // has been hit) sized - verify, update limit and offset + add result to
    // global result set subroutine should return the root_hash + updated limit
    // and offset

    pub fn execute_proof(
        proof: &[u8],
        query: PathQuery,
    ) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();
        let mut proof_reader = ProofReader::new(proof);

        let merk_proof = proof_reader.read_proof(ProofType::SIZED_MERK_PROOF.into())?;

        let (mut last_root_hash, result_set) = merk::execute_proof(
            &merk_proof,
            &query.query.query,
            query.query.limit,
            query.query.offset,
            query.query.query.left_to_right,
        )
        .expect("should execute proof");

        // Validate the path
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if !path_slice.is_empty() {
                let merk_proof = proof_reader.read_proof(ProofType::MERK_PROOF.into())?;

                let mut parent_query = Query::new();
                parent_query.insert_key(key.to_vec());

                // TODO: Handle this better, should not be using expect
                let proof_result = merk::execute_proof(
                    &merk_proof,
                    &parent_query,
                    None,
                    None,
                    query.query.query.left_to_right,
                )
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

        let root_proof = proof_reader.read_proof(ProofType::ROOT_PROOF.into())?;

        let root_meta_data = proof_reader.read_to_end();
        let root_index_usize = root_meta_data
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

// I need this to be able to read data and tell me what type of data it has read
// maybe just read proof without
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
