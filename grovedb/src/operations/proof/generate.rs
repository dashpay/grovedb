// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Generate proof operations

// TODO: entire file is due for a refactor, need some kind of path generator
//  that supports multiple implementations for verbose and non-verbose
// generation

use costs::cost_return_on_error_default;
#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use merk::{
    proofs::{encode_into, Node, Op},
    tree::value_hash,
    KVIterator, Merk, ProofWithoutEncodingResult,
};
#[cfg(feature = "full")]
use storage::{rocksdb_storage::PrefixedRocksDbStorageContext, StorageContext};

#[cfg(feature = "full")]
use crate::element::helpers::raw_decode;
#[cfg(feature = "full")]
use crate::{
    operations::proof::util::{
        reduce_limit_and_offset_by, write_to_vec, ProofTokenType, EMPTY_TREE_HASH,
    },
    reference_path::path_from_reference_path_type,
    Element, Error, GroveDb, PathQuery, Query,
};
use crate::{
    operations::proof::util::{write_slice_of_slice_to_slice, write_slice_to_vec},
    versioning::{add_version_to_bytes, PROOF_VERSION},
};

#[cfg(feature = "full")]
type LimitOffset = (Option<u16>, Option<u16>);

#[cfg(feature = "full")]
impl GroveDb {
    /// Prove query many
    pub fn prove_query_many(&self, query: Vec<&PathQuery>) -> CostResult<Vec<u8>, Error> {
        if query.len() > 1 {
            let query = cost_return_on_error_default!(PathQuery::merge(query));
            self.prove_query(&query)
        } else {
            self.prove_query(query[0])
        }
    }

    /// Prove verbose many
    pub fn prove_verbose_many(&self, query: Vec<&PathQuery>) -> CostResult<Vec<u8>, Error> {
        if query.len() > 1 {
            let query = cost_return_on_error_default!(PathQuery::merge(query));
            self.prove_verbose(&query)
        } else {
            self.prove_verbose(query[0])
        }
    }

    /// Generate a minimalistic proof for a given path query
    /// doesn't allow for subset verification
    pub fn prove_query(&self, query: &PathQuery) -> CostResult<Vec<u8>, Error> {
        self.prove_internal(query, false)
    }

    /// Generate a verbose proof for a given path query
    /// allows for subset verification
    pub fn prove_verbose(&self, query: &PathQuery) -> CostResult<Vec<u8>, Error> {
        self.prove_internal(query, true)
    }

    /// Generates a verbose or non verbose proof based on a bool
    fn prove_internal(&self, query: &PathQuery, is_verbose: bool) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost::default();

        let mut proof_result =
            cost_return_on_error_default!(add_version_to_bytes(vec![], PROOF_VERSION));

        let mut limit: Option<u16> = query.query.limit;
        let mut offset: Option<u16> = query.query.offset;

        // We prevent the generation of verbose proofs for path queries that have a
        // limit or offset.
        // This is because once they are set, the subset nature
        // of the path query now depends not only on the path query definition,
        // but also on the current state of the db.
        // Meaning a path query that could have been a valid subset could potentially
        // not be due to the current state.
        // Hence verification errors due to non subset issues will not be deterministic.
        if (limit.is_some() || offset.is_some()) && is_verbose {
            return Err(Error::InvalidInput(
                "cannot generate verbose proof for path-query with a limit or offset value",
            ))
            .wrap_with_cost(cost);
        }

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        let subtree_exists = self
            .check_subtree_exists_path_not_found(path_slices.clone(), None)
            .unwrap_add_cost(&mut cost);

        // if the subtree at the given path doesn't exists, prove that this path
        // doesn't point to a valid subtree
        match subtree_exists {
            Ok(_) => {
                // subtree exists
                // do nothing
            }
            Err(_) => {
                cost_return_on_error!(
                    &mut cost,
                    self.generate_and_store_absent_path_proof(
                        &path_slices,
                        &mut proof_result,
                        is_verbose
                    )
                );
                // return the absence proof no need to continue proof generation
                return Ok(proof_result).wrap_with_cost(cost);
            }
        }

        // if the subtree exists and the proof type is verbose we need to insert
        // the path information to the proof
        if is_verbose {
            cost_return_on_error!(
                &mut cost,
                Self::generate_and_store_path_proof(path_slices.clone(), &mut proof_result)
            );
        }

        cost_return_on_error!(
            &mut cost,
            self.prove_subqueries(
                &mut proof_result,
                path_slices.clone(),
                query,
                &mut limit,
                &mut offset,
                true,
                is_verbose
            )
        );
        cost_return_on_error!(
            &mut cost,
            self.prove_path(&mut proof_result, path_slices, is_verbose)
        );

        Ok(proof_result).wrap_with_cost(cost)
    }

    /// Perform a pre-order traversal of the tree based on the provided
    /// subqueries
    fn prove_subqueries(
        &self,
        proofs: &mut Vec<u8>,
        path: Vec<&[u8]>,
        query: &PathQuery,
        current_limit: &mut Option<u16>,
        current_offset: &mut Option<u16>,
        is_first_call: bool,
        is_verbose: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut to_add_to_result_set: u16 = 0;

        let subtree = cost_return_on_error!(&mut cost, self.open_subtree(path.iter().copied()));
        if subtree.root_hash().unwrap_add_cost(&mut cost) == EMPTY_TREE_HASH {
            cost_return_on_error_no_add!(
                &cost,
                write_to_vec(proofs, &[ProofTokenType::EmptyTree.into()])
            );
            return Ok(()).wrap_with_cost(cost);
        }

        let reached_limit = query.query.limit.is_some() && query.query.limit.unwrap() == 0;
        if reached_limit {
            if is_first_call {
                cost_return_on_error!(
                    &mut cost,
                    self.generate_and_store_merk_proof(
                        path.iter().copied(),
                        &subtree,
                        &query.query.query,
                        (*current_limit, *current_offset),
                        ProofTokenType::SizedMerk,
                        proofs,
                        is_verbose,
                        path.iter().last().unwrap_or(&(&[][..]))
                    )
                );
            }
            return Ok(()).wrap_with_cost(cost);
        }

        let mut is_leaf_tree = true;

        let mut kv_iterator = KVIterator::new(subtree.storage.raw_iter(), &query.query.query)
            .unwrap_add_cost(&mut cost);

        while let Some((key, value_bytes)) = kv_iterator.next_kv().unwrap_add_cost(&mut cost) {
            let mut encountered_absence = false;

            let element = cost_return_on_error_no_add!(&cost, raw_decode(&value_bytes));
            match element {
                Element::Tree(root_key, _) | Element::SumTree(root_key, ..) => {
                    let (mut subquery_path, subquery_value) =
                        Element::subquery_paths_and_value_for_sized_query(&query.query, &key);

                    if subquery_value.is_none() && subquery_path.is_none() {
                        // this element should be added to the result set
                        // hence we have to update the limit and offset value
                        reduce_limit_and_offset_by(current_limit, current_offset, 1);
                        continue;
                    }

                    if root_key.is_none() {
                        continue;
                    }

                    // if the element is a non empty tree then current tree is not a leaf tree
                    if is_leaf_tree {
                        is_leaf_tree = false;
                        cost_return_on_error!(
                            &mut cost,
                            self.generate_and_store_merk_proof(
                                path.iter().copied(),
                                &subtree,
                                &query.query.query,
                                (None, None),
                                ProofTokenType::Merk,
                                proofs,
                                is_verbose,
                                path.iter().last().unwrap_or(&Default::default())
                            )
                        );
                    }

                    let mut new_path = path.clone();
                    new_path.push(key.as_ref());

                    let mut query = subquery_value;

                    if query.is_some() {
                        if let Some(subquery_path) = &subquery_path {
                            for subkey in subquery_path.iter() {
                                let inner_subtree = cost_return_on_error!(
                                    &mut cost,
                                    self.open_subtree(new_path.iter().copied())
                                );

                                let mut key_as_query = Query::new();
                                key_as_query.insert_key(subkey.clone());

                                cost_return_on_error!(
                                    &mut cost,
                                    self.generate_and_store_merk_proof(
                                        new_path.iter().copied(),
                                        &inner_subtree,
                                        &key_as_query,
                                        (None, None),
                                        ProofTokenType::Merk,
                                        proofs,
                                        is_verbose,
                                        new_path.iter().last().unwrap_or(&Default::default())
                                    )
                                );

                                new_path.push(subkey);

                                if self
                                    .check_subtree_exists_path_not_found(new_path.clone(), None)
                                    .unwrap_add_cost(&mut cost)
                                    .is_err()
                                {
                                    encountered_absence = true;
                                    break;
                                }
                            }

                            if encountered_absence {
                                continue;
                            }
                        }
                    } else if let Some(subquery_path) = &mut subquery_path {
                        if subquery_path.is_empty() {
                            // nothing to do on this path, since subquery path is empty
                            // and there is no consecutive subquery value
                            continue;
                        }

                        let last_key = subquery_path.remove(subquery_path.len() - 1);

                        for subkey in subquery_path.iter() {
                            let inner_subtree = cost_return_on_error!(
                                &mut cost,
                                self.open_subtree(new_path.iter().copied())
                            );

                            let mut key_as_query = Query::new();
                            key_as_query.insert_key(subkey.clone());

                            cost_return_on_error!(
                                &mut cost,
                                self.generate_and_store_merk_proof(
                                    new_path.iter().copied(),
                                    &inner_subtree,
                                    &key_as_query,
                                    (None, None),
                                    ProofTokenType::Merk,
                                    proofs,
                                    is_verbose,
                                    new_path.iter().last().unwrap_or(&Default::default())
                                )
                            );

                            new_path.push(subkey);

                            // check if the new path points to a valid subtree
                            // if it does not, we should stop proof generation on this path
                            if self
                                .check_subtree_exists_path_not_found(new_path.clone(), None)
                                .unwrap_add_cost(&mut cost)
                                .is_err()
                            {
                                encountered_absence = true;
                                break;
                            }
                        }

                        if encountered_absence {
                            continue;
                        }

                        let mut key_as_query = Query::new();
                        key_as_query.insert_key(last_key);
                        query = Some(key_as_query);
                    } else {
                        return Err(Error::CorruptedCodeExecution("subquery_path must exist"))
                            .wrap_with_cost(cost);
                    }

                    let new_path_owned = new_path.iter().map(|a| a.to_vec()).collect();

                    let new_path_query = PathQuery::new_unsized(new_path_owned, query.unwrap());

                    if self
                        .check_subtree_exists_path_not_found(new_path.clone(), None)
                        .unwrap_add_cost(&mut cost)
                        .is_err()
                    {
                        continue;
                    }

                    cost_return_on_error!(
                        &mut cost,
                        self.prove_subqueries(
                            proofs,
                            new_path,
                            &new_path_query,
                            current_limit,
                            current_offset,
                            false,
                            is_verbose,
                        )
                    );

                    if *current_limit == Some(0) {
                        break;
                    }
                }
                _ => {
                    to_add_to_result_set += 1;
                }
            }
        }

        if is_leaf_tree {
            // if no useful subtree, then we care about the result set of this subtree.
            // apply the sized query
            let limit_offset = cost_return_on_error!(
                &mut cost,
                self.generate_and_store_merk_proof(
                    path.iter().copied(),
                    &subtree,
                    &query.query.query,
                    (*current_limit, *current_offset),
                    ProofTokenType::SizedMerk,
                    proofs,
                    is_verbose,
                    path.iter().last().unwrap_or(&Default::default())
                )
            );

            // update limit and offset values
            *current_limit = limit_offset.0;
            *current_offset = limit_offset.1;
        } else {
            reduce_limit_and_offset_by(current_limit, current_offset, to_add_to_result_set);
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Given a path, construct and append a set of proofs that shows there is
    /// a valid path from the root of the db to that point.
    fn prove_path(
        &self,
        proof_result: &mut Vec<u8>,
        path_slices: Vec<&[u8]>,
        is_verbose: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        // generate proof to show that the path leads up to the root
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            let subtree =
                cost_return_on_error!(&mut cost, self.open_subtree(path_slice.iter().copied()));
            let mut query = Query::new();
            query.insert_key(key.to_vec());

            cost_return_on_error!(
                &mut cost,
                self.generate_and_store_merk_proof(
                    path_slice.iter().copied(),
                    &subtree,
                    &query,
                    (None, None),
                    ProofTokenType::Merk,
                    proof_result,
                    is_verbose,
                    path_slice.iter().last().unwrap_or(&Default::default())
                )
            );
            split_path = path_slice.split_last();
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Generates query proof given a subtree and appends the result to a proof
    /// list
    fn generate_and_store_merk_proof<'a, 'p, S: 'a, P>(
        &self,
        path: P,
        subtree: &'a Merk<S>,
        query: &Query,
        limit_offset: LimitOffset,
        proof_token_type: ProofTokenType,
        proofs: &mut Vec<u8>,
        is_verbose: bool,
        key: &[u8],
    ) -> CostResult<(Option<u16>, Option<u16>), Error>
    where
        S: StorageContext<'a>,
        P: IntoIterator<Item = &'p [u8]> + Iterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        if proof_token_type != ProofTokenType::Merk && proof_token_type != ProofTokenType::SizedMerk
        {
            return Err(Error::InvalidInput(
                "expect proof type for merk proof generation to be sized or merk proof type",
            ))
            .wrap_with_cost(Default::default());
        }

        let mut cost = OperationCost::default();

        let mut proof_result = subtree
            .prove_without_encoding(query.clone(), limit_offset.0, limit_offset.1)
            .unwrap()
            .expect("should generate proof");

        cost_return_on_error!(&mut cost, self.post_process_proof(path, &mut proof_result));

        let mut proof_bytes = Vec::with_capacity(128);
        encode_into(proof_result.proof.iter(), &mut proof_bytes);

        cost_return_on_error_no_add!(&cost, write_to_vec(proofs, &[proof_token_type.into()]));

        // if is verbose, write the key
        if is_verbose {
            cost_return_on_error_no_add!(&cost, write_slice_to_vec(proofs, key));
        }

        // write the merk proof
        cost_return_on_error_no_add!(&cost, write_slice_to_vec(proofs, &proof_bytes));

        Ok((proof_result.limit, proof_result.offset)).wrap_with_cost(cost)
    }

    /// Serializes a path and add it to the proof vector
    pub fn generate_and_store_path_proof(
        path: Vec<&[u8]>,
        proofs: &mut Vec<u8>,
    ) -> CostResult<(), Error> {
        let cost = OperationCost::default();

        cost_return_on_error_no_add!(
            &cost,
            write_to_vec(proofs, &[ProofTokenType::PathInfo.into()])
        );

        cost_return_on_error_no_add!(&cost, write_slice_of_slice_to_slice(proofs, &path));

        Ok(()).wrap_with_cost(cost)
    }

    fn generate_and_store_absent_path_proof(
        &self,
        path_slices: &[&[u8]],
        proof_result: &mut Vec<u8>,
        is_verbose: bool,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        cost_return_on_error_no_add!(
            &cost,
            write_to_vec(proof_result, &[ProofTokenType::AbsentPath.into()])
        );
        let mut current_path: Vec<&[u8]> = vec![];

        let mut split_path = path_slices.split_first();
        while let Some((key, path_slice)) = split_path {
            let subtree = self
                .open_subtree(current_path.iter().copied())
                .unwrap_add_cost(&mut cost);

            if subtree.is_err() {
                break;
            }

            let has_item = Element::get(
                subtree.as_ref().expect("confirmed not error above"),
                key,
                true,
            )
            .unwrap_add_cost(&mut cost);

            let mut next_key_query = Query::new();
            next_key_query.insert_key(key.to_vec());
            cost_return_on_error!(
                &mut cost,
                self.generate_and_store_merk_proof(
                    current_path.iter().copied(),
                    &subtree.expect("confirmed not error above"),
                    &next_key_query,
                    (None, None),
                    ProofTokenType::Merk,
                    proof_result,
                    is_verbose,
                    current_path.iter().last().unwrap_or(&(&[][..]))
                )
            );

            current_path.push(key);

            if has_item.is_err() || path_slice.is_empty() {
                // reached last key
                break;
            }

            split_path = path_slice.split_first();
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Converts Items to Node::KV from Node::KVValueHash
    /// Converts References to Node::KVRefValueHash and sets the value to the
    /// referenced element
    fn post_process_proof<'p, P>(
        &self,
        path: P,
        proof_result: &mut ProofWithoutEncodingResult,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter().collect::<Vec<_>>();

        for op in proof_result.proof.iter_mut() {
            match op {
                Op::Push(node) | Op::PushInverted(node) => match node {
                    Node::KV(key, value) | Node::KVValueHash(key, value, ..) => {
                        let elem = Element::deserialize(value);
                        match elem {
                            Ok(Element::Reference(reference_path, ..)) => {
                                let current_path = path_iter.clone();
                                let absolute_path = cost_return_on_error!(
                                    &mut cost,
                                    path_from_reference_path_type(
                                        reference_path,
                                        current_path,
                                        Some(key.as_slice())
                                    )
                                    .wrap_with_cost(OperationCost::default())
                                );

                                let referenced_elem = cost_return_on_error!(
                                    &mut cost,
                                    self.follow_reference(absolute_path, true, None)
                                );

                                let serialized_referenced_elem = referenced_elem.serialize();
                                if serialized_referenced_elem.is_err() {
                                    return Err(Error::CorruptedData(String::from(
                                        "unable to serialize element",
                                    )))
                                    .wrap_with_cost(cost);
                                }

                                *node = Node::KVRefValueHash(
                                    key.to_owned(),
                                    serialized_referenced_elem.expect("confirmed ok above"),
                                    value_hash(value).unwrap_add_cost(&mut cost),
                                )
                            }
                            Ok(Element::Item(..)) => {
                                *node = Node::KV(key.to_owned(), value.to_owned())
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Opens merk at a given path without transaction
    fn open_subtree<'p, P>(&self, path: P) -> CostResult<Merk<PrefixedRocksDbStorageContext>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.open_non_transactional_merk_at_path(path)
    }
}

#[cfg(test)]
mod tests {
    use merk::{execute_proof, proofs::Query};

    use crate::{
        operations::proof::util::{ProofReader, ProofTokenType},
        tests::{make_deep_tree, TEST_LEAF},
        GroveDb,
    };

    #[test]
    fn test_path_info_encoding_and_decoding() {
        let path = vec![b"a".as_slice(), b"b".as_slice(), b"c".as_slice()];
        let mut proof_vector = vec![];
        GroveDb::generate_and_store_path_proof(path.clone(), &mut proof_vector)
            .unwrap()
            .unwrap();

        let mut proof_reader = ProofReader::new(proof_vector.as_slice());
        let decoded_path = proof_reader.read_path_info().unwrap();

        assert_eq!(path, decoded_path);
    }

    #[test]
    fn test_reading_of_verbose_proofs() {
        let db = make_deep_tree();

        let path = vec![TEST_LEAF, b"innertree"];
        let mut query = Query::new();
        query.insert_all();

        let merk = db
            .open_non_transactional_merk_at_path([TEST_LEAF, b"innertree"])
            .unwrap()
            .unwrap();
        let expected_root_hash = merk.root_hash().unwrap();

        let mut proof = vec![];
        db.generate_and_store_merk_proof(
            path.iter().copied(),
            &merk,
            &query,
            (None, None),
            ProofTokenType::Merk,
            &mut proof,
            true,
            b"innertree",
        )
        .unwrap()
        .unwrap();
        assert_ne!(proof.len(), 0);

        let mut proof_reader = ProofReader::new(&proof);
        let (proof_token_type, proof, key) = proof_reader.read_verbose_proof().unwrap();

        assert_eq!(proof_token_type, ProofTokenType::Merk);
        assert_eq!(key, Some(b"innertree".to_vec()));

        let (root_hash, result_set) = execute_proof(&proof, &query, None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(root_hash, expected_root_hash);
        assert_eq!(result_set.result_set.len(), 3);

        // what is the key is empty??
        let merk = db.open_non_transactional_merk_at_path([]).unwrap().unwrap();
        let expected_root_hash = merk.root_hash().unwrap();

        let path = vec![];
        let mut proof = vec![];
        db.generate_and_store_merk_proof(
            path.iter().copied(),
            &merk,
            &query,
            (None, None),
            ProofTokenType::Merk,
            &mut proof,
            true,
            path.iter().last().unwrap_or(&(&[][..])),
        )
        .unwrap()
        .unwrap();
        assert_ne!(proof.len(), 0);

        let mut proof_reader = ProofReader::new(&proof);
        let (proof_token_type, proof, key) = proof_reader.read_verbose_proof().unwrap();

        assert_eq!(proof_token_type, ProofTokenType::Merk);
        assert_eq!(key, Some(vec![]));

        let (root_hash, result_set) = execute_proof(&proof, &query, None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(root_hash, expected_root_hash);
        assert_eq!(result_set.result_set.len(), 3);
    }

    #[test]
    fn test_reading_verbose_proof_at_key() {
        // going to generate an array of multiple proofs with different keys
        let db = make_deep_tree();
        let mut proofs = vec![];

        let mut query = Query::new();
        query.insert_all();

        // insert all under inner tree
        let path = vec![TEST_LEAF, b"innertree"];

        let merk = db
            .open_non_transactional_merk_at_path(path.iter().copied())
            .unwrap()
            .unwrap();
        let inner_tree_root_hash = merk.root_hash().unwrap();
        db.generate_and_store_merk_proof(
            path.iter().copied(),
            &merk,
            &query,
            (None, None),
            ProofTokenType::Merk,
            &mut proofs,
            true,
            path.iter().last().unwrap_or(&(&[][..])),
        )
        .unwrap()
        .unwrap();

        // insert all under innertree4
        let path = vec![TEST_LEAF, b"innertree4"];
        let merk = db
            .open_non_transactional_merk_at_path(path.iter().copied())
            .unwrap()
            .unwrap();
        let inner_tree_4_root_hash = merk.root_hash().unwrap();
        db.generate_and_store_merk_proof(
            path.iter().copied(),
            &merk,
            &query,
            (None, None),
            ProofTokenType::Merk,
            &mut proofs,
            true,
            path.iter().last().unwrap_or(&(&[][..])),
        )
        .unwrap()
        .unwrap();

        // insert all for deeper_1
        let path: Vec<&[u8]> = vec![b"deep_leaf", b"deep_node_1", b"deeper_1"];
        let merk = db
            .open_non_transactional_merk_at_path(path.iter().copied())
            .unwrap()
            .unwrap();
        let deeper_1_root_hash = merk.root_hash().unwrap();
        db.generate_and_store_merk_proof(
            path.iter().copied(),
            &merk,
            &query,
            (None, None),
            ProofTokenType::Merk,
            &mut proofs,
            true,
            path.iter().last().unwrap_or(&(&[][..])),
        )
        .unwrap()
        .unwrap();

        // read the proof at innertree
        let contextual_proof = proofs.clone();
        let mut proof_reader = ProofReader::new(&contextual_proof);
        let (proof_token_type, proof) = proof_reader
            .read_verbose_proof_at_key(b"innertree")
            .unwrap();

        assert_eq!(proof_token_type, ProofTokenType::Merk);

        let (root_hash, result_set) = execute_proof(&proof, &query, None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(root_hash, inner_tree_root_hash);
        assert_eq!(result_set.result_set.len(), 3);

        // read the proof at innertree4
        let contextual_proof = proofs.clone();
        let mut proof_reader = ProofReader::new(&contextual_proof);
        let (proof_token_type, proof) = proof_reader
            .read_verbose_proof_at_key(b"innertree4")
            .unwrap();

        assert_eq!(proof_token_type, ProofTokenType::Merk);

        let (root_hash, result_set) = execute_proof(&proof, &query, None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(root_hash, inner_tree_4_root_hash);
        assert_eq!(result_set.result_set.len(), 2);

        // read the proof at deeper_1
        let contextual_proof = proofs.clone();
        let mut proof_reader = ProofReader::new(&contextual_proof);
        let (proof_token_type, proof) =
            proof_reader.read_verbose_proof_at_key(b"deeper_1").unwrap();

        assert_eq!(proof_token_type, ProofTokenType::Merk);

        let (root_hash, result_set) = execute_proof(&proof, &query, None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(root_hash, deeper_1_root_hash);
        assert_eq!(result_set.result_set.len(), 3);

        // read the proof at an invalid key
        let contextual_proof = proofs.clone();
        let mut proof_reader = ProofReader::new(&contextual_proof);
        let reading_result = proof_reader.read_verbose_proof_at_key(b"unknown_key");
        assert!(reading_result.is_err())
    }
}
