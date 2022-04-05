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

        prove_subqueries(
            &self.db,
            &mut proof_result,
            path_slices.clone(),
            query.clone(),
        );

        fn prove_subqueries(
            db: &RocksDbStorage,
            proofs: &mut Vec<u8>,
            path: Vec<&[u8]>,
            query: PathQuery,
        ) -> Result<(), Error> {
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

                // This is used to determine if we should create a proof with
                // the limit and offset values. If true then yes, false then no
                let mut has_subtree = false;

                // before getting the elements of the subtree, we should get the
                // subquery key and value
                // we have a query, that is inserted in a sized query for the path query
                // we only care about the query (not so simple)
                // need to understand conditional_subqueries and default_subqueries
                let (subquery_key, subquery_value) =
                    Element::default_subquery_paths_for_sized_query(&query.query);
                // if there is a subquery and subquery key then combine key to path and use
                // other as query if there is just a subquery key then convert
                // subquery key to query

                // if there is no subquery or subquery key then don't iterate
                // if there is either one then iterate
                // TODO: Convert to or ||
                if subquery_key.is_some() || subquery_value.is_some() {
                    dbg!("start");
                    let subtree_key_values = subtree.get_kv_pairs();
                    // TODO: make use of the direction
                    for (key, value_bytes) in subtree_key_values.iter() {
                        // TODO: Figure out what to do if decoding fails
                        let element = raw_decode(value_bytes).unwrap();
                        // check if the element is of type tree
                        // if is it a tree, set has_subtree
                        match element {
                            Element::Tree(_) => {
                                // following a greedy approach, one we encounter a
                                // subtree we exhaust it before moving on to the
                                // next subtree
                                // has_subtree, was to make sure we don't make use
                                // of the result set (do we still need this?)
                                has_subtree = true;
                                // recurse on this subtree, by creating a new
                                // path_slice
                                // with the new key
                                // function should return the resulting limits and
                                // offset should add to a global
                                // proof set (most likely a closure);
                                // TODO: cleanup
                                let mut new_path = path.clone();
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

                                let new_path_owned = new_path.iter().map(|x| x.to_vec()).collect();
                                let new_path_query =
                                    PathQuery::new_unsized(new_path_owned, query.unwrap());

                                prove_subqueries(db, proofs, new_path, new_path_query);
                            }
                            _ => {
                                // if no subtree then we care about the result set
                                dbg!("not tree");
                            }
                        }
                    }
                    // dbg!(m);
                    dbg!("end");
                }

                let limit = if !has_subtree {
                    query.query.limit
                } else {
                    None
                };
                let offset = if !has_subtree {
                    query.query.offset
                } else {
                    None
                };

                let ProofConstructionResult { proof, .. } = subtree
                    .prove(query.query.query, limit, offset)
                    .expect("should generate proof");

                // only adding to the proof result set, after you have added that of
                // your child nodes
                // TODO: Switch to variable length encoding
                debug_assert!(proof.len() < 256);
                write_to_vec(proofs, &vec![MERK_PROOF, proof.len() as u8]);
                write_to_vec(proofs, &proof);
            });

            Ok(())
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
                    write_to_vec(&mut proof_result, &vec![ROOT_PROOF, root_proof.len() as u8]);
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
                    write_to_vec(&mut proof_result, &vec![MERK_PROOF, proof.len() as u8]);
                    write_to_vec(&mut proof_result, &proof);
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
        let mut proof_reader = ProofReader::new(proof);

        let merk_proof = proof_reader.read_proof(MERK_PROOF)?;

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
                let merk_proof = proof_reader.read_proof(MERK_PROOF)?;

                let mut parent_query = Query::new();
                parent_query.insert_key(key.to_vec());

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

        let root_proof = proof_reader.read_proof(ROOT_PROOF)?;

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
