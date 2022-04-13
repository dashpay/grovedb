use std::io::{Read, Write};

use merk::proofs::query::QueryItem;
use rs_merkle::{algorithms::Sha256, MerkleProof};
use storage::rocksdb_storage::RocksDbStorage;

use crate::{
    merk::ProofConstructionResult,
    subtree::raw_decode,
    util::{merk_optional_tx, meta_storage_context_optional_tx},
    Element, Error,
    Error::{InvalidPath, InvalidProof},
    GroveDb, PathQuery, Query, SizedQuery,
};

const EMPTY_TREE_HASH: [u8; 32] = [0; 32];

#[derive(Debug)]
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

fn write_to_vec<W: Write>(dest: &mut W, value: &Vec<u8>) {
    // dbg!(&value);
    dest.write_all(value);
}

// TODO: Delete this function
fn print_path(path: Vec<&[u8]>) {
    let mut result = String::from("");
    for p in path {
        let m = std::str::from_utf8(p).unwrap();
        result.push_str(" -> ");
        result.push_str(m);
    }
    dbg!(result);
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

        prove_subqueries(&self, &mut proof_result, path_slices.clone(), query.clone());

        // TODO: return the propagated limit and offset values after running this
        fn prove_subqueries(
            db: &GroveDb,
            proofs: &mut Vec<u8>,
            path: Vec<&[u8]>,
            query: PathQuery,
            // TODO: describe subquery only, maybe a bool?
            // ignore_subquery_key: bool,
        ) -> Result<(Option<u16>, Option<u16>), Error> {
            // TODO: Not sure this is supposed to be inside
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
            // print_path(path.clone());
            merk_optional_tx!(db.db, path.clone(), None, subtree, {
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

                // Dealing with subquery key and value
                // a subquery key is essentially a key query item that you want to apply
                // first before applying the actual subquery
                // hence adding a subquery key and subquery is essentially the same
                // as adding two subqueries, one a key followed by the other query


                // how do I get this key??

                // let has_subquery = subquery_key.is_some() || subquery_value.is_some();
                let exhausted_limit =
                    query.query.limit.is_some() && query.query.limit.unwrap() == 0;
                // dbg!(&has_subquery);
                // dbg!(exhausted_limit);

                if !exhausted_limit {
                    // dbg!("start");
                    let subtree_key_values = subtree.get_kv_pairs();
                    // TODO: make use of the direction
                    for (key, value_bytes) in subtree_key_values.iter() {
                        let (subquery_key, subquery_value) =
                            Element::subquery_paths_for_sized_query(&query.query, key);
                        // TODO: Figure out what to do if decoding fails
                        // dbg!(&key);
                        // dbg!(&value_bytes);
                        let element = raw_decode(value_bytes).unwrap();
                        // dbg!(&element);

                        match element {
                            Element::Tree(tree_hash) => {
                                if tree_hash == EMPTY_TREE_HASH {
                                    // skip proof generation for empty trees
                                    continue;
                                }

                                // we should add the proof of the current element
                                // before hitting the children
                                // since we know it has a useful subtree, then we
                                // know this is not a leaf node as such we can prove
                                // it without limit and offset
                                if !has_useful_subtree {
                                    // add the current elements merk proof
                                    has_useful_subtree = true;

                                    // prove all the keys
                                    // TODO: Add direction
                                    let mut all_key_query = Query::new();
                                    all_key_query.insert_all();

                                    // generate unsized merk proof for current element
                                    // TODO: Remove duplication
                                    // TODO: How do you handle mixed tree types?
                                    let ProofConstructionResult { proof, .. } = subtree
                                        .prove(all_key_query, None, None)
                                        .expect("should generate proof");
                                    // dbg!("Writing", &proof);

                                    // TODO: Switch to variable length encoding
                                    debug_assert!(proof.len() < 256);
                                    write_to_vec(
                                        proofs,
                                        &vec![ProofType::MerkProof.into(), proof.len() as u8],
                                    );
                                    write_to_vec(proofs, &proof);
                                }

                                // This section is to prove the subtrees
                                // some queries might come with both a subquery key and a query
                                // they both need proofs on different paths (which requires
                                // different subtrees) constraint:
                                // you cannot modify the subquery keys
                                // normally, the way you get subtrees is by recursing on
                                // prove_subquery with the path
                                // why doesn't this work for us??
                                // should figure out what should be proved first

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
                                        // intermediate step here, generate a proof that the
                                        // subquery key
                                        // exists or doesn't exist in this subtree
                                        merk_optional_tx!(
                                            db.db,
                                            new_path.clone(),
                                            None,
                                            inner_subtree,
                                            {
                                                // generate a proof for the subquery key
                                                dbg!(std::str::from_utf8(
                                                    sub_key.clone().unwrap().as_slice()
                                                ));
                                                let mut key_as_query = Query::new();
                                                key_as_query.insert_key(sub_key.clone().unwrap());
                                                // query = Some(key_as_query);

                                                let ProofConstructionResult { proof, .. } =
                                                    inner_subtree
                                                        .prove(key_as_query.clone(), None, None)
                                                        .expect("should generate proof");
                                                dbg!(&proof);

                                                debug_assert!(proof.len() < 256);
                                                write_to_vec(
                                                    proofs,
                                                    &vec![
                                                        ProofType::MerkProof.into(),
                                                        proof.len() as u8,
                                                    ],
                                                );
                                                write_to_vec(proofs, &proof);
                                            }
                                        );

                                        new_path.push(sub_key.as_ref().unwrap());
                                        // verify that the new path exists
                                        let subquery_key_path_exists = db
                                            .check_subtree_exists_path_not_found(
                                                new_path.clone(),
                                                None,
                                                None,
                                            );
                                        if subquery_key_path_exists.is_err() {
                                            // dbg!("does not exist");
                                            continue;
                                        }
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

                                // add proofs for child nodes
                                // TODO: Handle error properly, what could cause an error?
                                let limit_offset_result =
                                    prove_subqueries(db, proofs, new_path, new_path_query).unwrap();

                                current_limit = limit_offset_result.0;
                                current_offset = limit_offset_result.1;

                                // if we hit the limit, we should kill the loop
                                if current_limit.is_some() && current_limit.unwrap() == 0 {
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
                    // dbg!("end");
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
                            ProofType::SizedMerkProof.into(),
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
                        &vec![ProofType::RootProof.into(), root_proof.len() as u8],
                    );
                    write_to_vec(&mut proof_result, &root_proof);

                    // write the number of root leafs
                    // makes the assumption that 1 byte is enough to represent the root leaf count
                    // size
                    write_to_vec(&mut proof_result, &vec![root_leaf_keys.len() as u8]);

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
                        &vec![ProofType::MerkProof.into(), proof.len() as u8],
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

    // TODO: Audit and make clearer logic of figuring out what to verify
    // TODO: There might be cases where we randomly stop proof generations because a
    // certain key TODO: does not exists, should also take that into account
    pub fn execute_proof(
        proof: &[u8],
        query: PathQuery,
    ) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        // dbg!("");
        // dbg!("Starting verification");
        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        // global result set
        let mut result_set: Vec<(Vec<u8>, Vec<u8>)> = vec![];
        // initialize proof reader
        let mut proof_reader = ProofReader::new(proof);
        let mut current_limit = query.query.limit;
        let mut current_offset = query.query.offset;

        // not sure the type of the initial merk proof (might be sized or unsized, but
        // should be merk proof right??) TODO: Is there a possibility that there
        // might only be a root proof (handle accordingly) maybe use the length
        // of the path to determine this?? assuming it starts with a merk proof
        // TODO: Remove
        // let merk_proof =
        // proof_reader.read_proof_of_type(ProofType::SIZED_MERK_PROOF.into())?;

        // TODO: optionally run this
        // TODO: Get rid of clone
        let mut last_root_hash = execute_subquery_proof(
            &mut proof_reader,
            &mut result_set,
            &mut current_limit,
            &mut current_offset,
            query.clone(),
        )?;

        // TODO: Should proof verification take into account the path??
        // TODO: Might need to prove subquery keys all the time
        // TODO: Should fail if we have subquery and subquery key
        // what should this take as argument?
        // needs the proof reader_for sure to read the merk proof
        // needs the query object also
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
                    // dbg!("got to sized proof");
                    // verify the proof with current offset and limit parameters
                    // TODO: remove expect clause + clone
                    let verification_result = merk::execute_proof(
                        &proof,
                        &query.query.query,
                        current_limit.clone(),
                        current_offset.clone(),
                        query.query.query.left_to_right,
                    )
                    .expect("should execute proof");

                    root_hash = verification_result.0;
                    result_set.extend(verification_result.1.result_set);

                    // update limit and offset
                    *current_limit = verification_result.1.limit;
                    *current_offset = verification_result.1.offset;
                }
                ProofType::MerkProof => {
                    // dbg!("got unsized prooooooof");
                    // dbg!("proving", &proof);
                    // verify with no limit and offset
                    // recurse on children (from result set)
                    // verify that their proof is equal to the given hash
                    // return the hash and updated limits and offset
                    // TODO: remove expect clause

                    // for non leaf subtrees, we want to prove that all their keys
                    // have an accompanying proof as long as the limit is non zero
                    // and their child subtree is not empty
                    let mut all_key_query = Query::new();
                    all_key_query.insert_all();

                    let verification_result = merk::execute_proof(
                        &proof,
                        &all_key_query,
                        None,
                        None,
                        query.query.query.left_to_right,
                    )
                    .expect("should execute proof");

                    root_hash = verification_result.0;
                    // dbg!(&verification_result.1.result_set);

                    // iterate over the children
                    // TODO: remove clone
                    for (key, value_bytes) in verification_result.1.result_set.clone() {
                        // we use the key to get the exact subquery
                        // TODO: Handle limits
                        // recurse with the new subquery
                        // verify that the root hash is what you expect
                        // value must represent a tree (error if it does not)
                        // maybe err first
                        // TODO: Remove duplication
                        // dbg!("decoding child");
                        let child_element = Element::deserialize(value_bytes.as_slice())?;
                        // dbg!(&child_element);
                        match child_element {
                            Element::Tree(mut expected_root_hash) => {
                                // construct the subquery
                                // TODO: Is it possible to prove that the subquery key was applied??
                                // TODO: Do I need to prove the path of the subqueries??

                                if expected_root_hash == EMPTY_TREE_HASH {
                                    // child node is empty, move on to next
                                    continue;
                                }

                                // TODO: add direction
                                // don't recurse if the limit is zero
                                if current_limit.is_some() && current_limit.unwrap() == 0 {
                                    // we are done verifying the subqueries
                                    break;
                                }

                                // TODO: Make use of the subquery_key
                                let (subquery_key, subquery_value) =
                                    Element::subquery_paths_for_sized_query(&query.query, key.as_slice());
                                // dbg!(&subquery_key);
                                // dbg!(&subquery_value);

                                // what do you do if there exists a subquery key
                                // if there is a subquery key then there would be a corresponding
                                // proof to prove it's existence.
                                // if it does not exist in the result set then stop
                                // if it does exists in the result set, update the expected root
                                // hash and continue
                                if subquery_key.is_some() {
                                    let (proof_type, subkey_proof) = proof_reader.read_proof()?;
                                    // TODO: verify it's a merk proof
                                    let mut key_as_query = Query::new();
                                    key_as_query.insert_key(subquery_key.clone().unwrap());
                                    // TODO: add direction
                                    let verification_result = merk::execute_proof(
                                        &subkey_proof,
                                        &key_as_query,
                                        None,
                                        None,
                                        true,
                                    );
                                    let rset = verification_result.unwrap().1.result_set;
                                    if rset.len() == 0 {
                                        // subquery key does not exist in the subtree
                                        // proceed to another subtree
                                        continue;
                                    } else {
                                        // if it does exist then update the expected root hash
                                        // dbg!(rset);
                                        let elem_value = &rset[0].1;
                                        let elem = Element::deserialize(elem_value).unwrap();
                                        match elem {
                                            Element::Tree(new_exptected_hash) => {
                                                expected_root_hash = new_exptected_hash;
                                            }
                                            _ => {
                                                dbg!("shouting");
                                            }
                                        }
                                        // expected_root_hash =
                                    }
                                }

                                // TODO: Write a test whose subqueries are more than the depth of
                                // the tree
                                let new_path_query;
                                if subquery_value.is_some() {
                                    new_path_query =
                                        PathQuery::new_unsized(vec![], subquery_value.unwrap());
                                } else {
                                    let mut key_as_query = Query::new();
                                    key_as_query.insert_key(subquery_key.unwrap());
                                    new_path_query = PathQuery::new_unsized(vec![], key_as_query);
                                }

                                let child_hash = execute_subquery_proof(
                                    proof_reader,
                                    result_set,
                                    current_limit,
                                    current_offset,
                                    new_path_query,
                                )?;

                                // child hash should be the same as expected hash
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
                    // TODO: Update here when you fix possibility of only root
                    return Err(Error::InvalidProof("wrong proof type"));
                }
            }
            Ok(root_hash)
        }

        // Validate the path
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if !path_slice.is_empty() {
                let merk_proof = proof_reader.read_proof_of_type(ProofType::MerkProof.into())?;

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

        let root_proof = proof_reader.read_proof_of_type(ProofType::RootProof.into())?;

        // makes the assumption that 1 byte is enough to represent the root leaf count
        // size
        let root_leaf_size = proof_reader.read_byte()?;

        let root_meta_data = proof_reader.read_to_end();
        let root_index_usize = root_meta_data
            .into_iter()
            .map(|index| index as usize)
            .collect::<Vec<usize>>();

        // TODO: Rename
        let root_proof_terrible_name = match MerkleProof::<Sha256>::try_from(root_proof) {
            Ok(proof) => Ok(proof),
            Err(_) => Err(Error::InvalidProof("invalid proof element")),
        }?;

        // getting rid of the leaf count:
        // for our purposes, root leafs are not expected to be very many
        // could theoretically be represented with just one byte
        // but nothing about the system prevents more than one byte of leaf keys
        let root_hash = match root_proof_terrible_name.root(
            &root_index_usize,
            &[last_root_hash],
            root_leaf_size[0] as usize,
        ) {
            Ok(hash) => Ok(hash),
            Err(_) => Err(Error::InvalidProof("Invalid proof element")),
        }?;

        Ok((root_hash, result_set))
    }
}

// I need this to be able to read data and tell me what type of data it has read
// maybe just read proof without an expected type
#[derive(Debug)]
struct ProofReader<'a> {
    proof_data: &'a [u8],
}

impl<'a> ProofReader<'a> {
    fn new(proof_data: &'a [u8]) -> Self {
        Self { proof_data }
    }

    fn read_byte(&mut self) -> Result<[u8; 1], Error> {
        let mut data = [0; 1];
        self.proof_data.read(&mut data);
        Ok(data)
    }

    // TODO: Handle duplication
    // TODO: handle error (e.g. not enough bytes to read)
    fn read_proof(&mut self) -> Result<(ProofType, Vec<u8>), Error> {
        let mut data_type = [0; 1];
        self.proof_data.read(&mut data_type);

        let proof_type: ProofType = data_type[0].into();

        let mut length = vec![0; 1];
        self.proof_data.read(&mut length);
        let mut proof = vec![0; length[0] as usize];
        self.proof_data.read(&mut proof);

        Ok((proof_type, proof))
    }

    fn read_proof_of_type(&mut self, expected_data_type: u8) -> Result<Vec<u8>, Error> {
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
