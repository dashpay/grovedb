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

//! Verify proof operations

use std::{borrow::Cow, collections::BTreeMap};

use grovedb_merk::proofs::query::PathKey;
#[cfg(any(feature = "full", feature = "verify"))]
pub use grovedb_merk::proofs::query::{Path, ProvedKeyValue};
#[cfg(any(feature = "full", feature = "verify"))]
use grovedb_merk::{
    proofs::Query,
    tree::{combine_hash, value_hash as value_hash_fn},
    CryptoHash,
};
use grovedb_storage::Storage;

use crate::{
    operations::proof::util::{
        reduce_limit_and_offset_by, ProvedPathKeyValue, ProvedPathKeyValues,
    },
    query_result_type::PathKeyOptionalElementTrio,
    versioning::read_and_consume_proof_version,
    SizedQuery,
};
#[cfg(any(feature = "full", feature = "verify"))]
use crate::{
    operations::proof::util::{
        ProofReader, ProofTokenType, ProofTokenType::AbsentPath, EMPTY_TREE_HASH,
    },
    Element, Error, GroveDb, PathQuery,
};

#[cfg(any(feature = "full", feature = "verify"))]
pub type ProvedKeyValues = Vec<ProvedKeyValue>;

#[cfg(any(feature = "full", feature = "verify"))]
type EncounteredAbsence = bool;

#[cfg(any(feature = "full", feature = "verify"))]
impl<S: Storage> GroveDb<S> {
    /// Verify proof given a path query
    /// Returns the root hash + deserialized elements
    pub fn verify_query(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        let (root_hash, proved_path_key_values) = Self::verify_query_raw(proof, query)?;
        let path_key_optional_elements = proved_path_key_values
            .into_iter()
            .map(|pkv| pkv.try_into())
            .collect::<Result<Vec<PathKeyOptionalElementTrio>, Error>>()?;
        Ok((root_hash, path_key_optional_elements))
    }

    /// Verify proof for a given path query returns serialized elements
    pub fn verify_query_raw(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], ProvedPathKeyValues), Error> {
        let mut verifier = ProofVerifier::new(query);
        let hash = verifier.execute_proof(proof, query, false)?;

        Ok((hash, verifier.result_set))
    }

    /// Verify proof given multiple path queries.
    /// If we have more than one path query we merge before performing
    /// verification.
    pub fn verify_query_many(
        proof: &[u8],
        query: Vec<&PathQuery>,
    ) -> Result<([u8; 32], ProvedPathKeyValues), Error> {
        if query.len() > 1 {
            let query = PathQuery::merge(query)?;
            GroveDb::<S>::verify_query_raw(proof, &query)
        } else {
            GroveDb::<S>::verify_query_raw(proof, query[0])
        }
    }

    /// Given a verbose proof, we can verify it with a subset path query.
    /// Returning the root hash and the deserialized result set.
    pub fn verify_subset_query(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        let (root_hash, proved_path_key_values) = Self::verify_subset_query_raw(proof, query)?;
        let path_key_optional_elements = proved_path_key_values
            .into_iter()
            .map(|pkv| pkv.try_into())
            .collect::<Result<Vec<PathKeyOptionalElementTrio>, Error>>()?;
        Ok((root_hash, path_key_optional_elements))
    }

    /// Given a verbose proof, we can verify it with a subset path query.
    /// Returning the root hash and the serialized result set.
    pub fn verify_subset_query_raw(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], ProvedPathKeyValues), Error> {
        let mut verifier = ProofVerifier::new(query);
        let hash = verifier.execute_proof(proof, query, true)?;
        Ok((hash, verifier.result_set))
    }

    /// Verify non subset query return the absence proof
    /// Returns all possible keys within the Path Query with an optional Element
    /// Value Element is set to None if absent
    pub fn verify_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        Self::verify_with_absence_proof(proof, query, Self::verify_query)
    }

    /// Verify subset query return the absence proof
    /// Returns all possible keys within the Path Query with an optional Element
    /// Value Element is set to None if absent
    pub fn verify_subset_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        Self::verify_with_absence_proof(proof, query, Self::verify_subset_query)
    }

    /// Verifies the proof and returns both elements in the result set and the
    /// elements in query but not in state.
    /// Note: This only works for certain path queries.
    // TODO: We should not care about terminal keys, as theoretically they can be
    //  infinite  we should perform the absence check solely on the proof and the
    //  given key, this is a temporary solution
    fn verify_with_absence_proof<T>(
        proof: &[u8],
        query: &PathQuery,
        verification_fn: T,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error>
    where
        T: Fn(&[u8], &PathQuery) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error>,
    {
        // must have a limit
        let max_results = query.query.limit.ok_or(Error::NotSupported(
            "limits must be set in verify_query_with_absence_proof",
        ))? as usize;

        // must have no offset
        if query.query.offset.is_some() {
            return Err(Error::NotSupported(
                "offsets are not supported for verify_query_with_absence_proof",
            ));
        }

        let terminal_keys = query.terminal_keys(max_results)?;

        // need to actually verify the query
        let (root_hash, result_set) = verification_fn(proof, query)?;

        // convert the result set to a btree map
        let mut result_set_as_map: BTreeMap<PathKey, Option<Element>> = result_set
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        let result_set_with_absence: Vec<PathKeyOptionalElementTrio> = terminal_keys
            .into_iter()
            .map(|terminal_key| {
                let element = result_set_as_map.remove(&terminal_key).flatten();
                (terminal_key.0, terminal_key.1, element)
            })
            .collect();

        Ok((root_hash, result_set_with_absence))
    }

    /// Verify subset proof with a chain of path query functions.
    /// After subset verification with the first path query, the result if
    /// passed to the next path query generation function which generates a
    /// new path query Apply the new path query, and pass the result to the
    /// next ... This is useful for verifying proofs with multiple path
    /// queries that depend on one another.
    pub fn verify_query_with_chained_path_queries<C>(
        proof: &[u8],
        first_query: &PathQuery,
        chained_path_queries: Vec<C>,
    ) -> Result<(CryptoHash, Vec<Vec<PathKeyOptionalElementTrio>>), Error>
    where
        C: Fn(Vec<PathKeyOptionalElementTrio>) -> Option<PathQuery>,
    {
        let mut results = vec![];

        let (last_root_hash, elements) = Self::verify_subset_query(proof, first_query)?;
        results.push(elements);

        // we should iterate over each chained path queries
        for path_query_generator in chained_path_queries {
            let new_path_query = path_query_generator(results[results.len() - 1].clone()).ok_or(
                Error::InvalidInput("one of the path query generators returns no path query"),
            )?;
            let (new_root_hash, new_elements) = Self::verify_subset_query(proof, &new_path_query)?;
            if new_root_hash != last_root_hash {
                return Err(Error::InvalidProof(
                    "root hash for different path queries do no match",
                ));
            }
            results.push(new_elements);
        }

        Ok((last_root_hash, results))
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Proof verifier
struct ProofVerifier {
    limit: Option<u16>,
    offset: Option<u16>,
    result_set: ProvedPathKeyValues,
}

#[cfg(any(feature = "full", feature = "verify"))]
impl ProofVerifier {
    /// New query
    pub fn new(query: &PathQuery) -> Self {
        ProofVerifier {
            limit: query.query.limit,
            offset: query.query.offset,
            result_set: vec![],
        }
    }

    /// Execute proof
    pub fn execute_proof(
        &mut self,
        proof: &[u8],
        query: &PathQuery,
        is_verbose: bool,
    ) -> Result<[u8; 32], Error> {
        let (_proof_version, proof) = read_and_consume_proof_version(proof)?;
        let mut proof_reader = ProofReader::new_with_verbose_status(proof, is_verbose);

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();
        let mut query = Cow::Borrowed(query);

        // TODO: refactor and add better comments
        // if verbose, the first thing we want to do is read the path info
        if is_verbose {
            let original_path = proof_reader.read_path_info()?;

            if original_path == path_slices {
                // do nothing
            } else if original_path.len() > path_slices.len() {
                // TODO: can we relax this constraint
                return Err(Error::InvalidProof(
                    "original path query path must not be greater than the subset path len",
                ));
            } else {
                let original_path_in_new_path = original_path
                    .iter()
                    .all(|key| path_slices.contains(&key.as_slice()));

                if !original_path_in_new_path {
                    return Err(Error::InvalidProof(
                        "the original path should be a subset of the subset path",
                    ));
                } else {
                    // We construct a new path query
                    let path_not_common = path_slices[original_path.len()..].to_vec();
                    let mut path_iter = path_not_common.iter();

                    let mut new_query = Query::new();
                    if path_iter.len() >= 1 {
                        new_query
                            .insert_key(path_iter.next().expect("confirmed has value").to_vec());
                    }

                    // need to add the first key to the query
                    new_query.set_subquery_path(path_iter.map(|a| a.to_vec()).collect());
                    new_query.set_subquery(query.query.query.clone());

                    query = Cow::Owned(PathQuery::new(
                        original_path,
                        SizedQuery::new(new_query, query.query.limit, query.query.offset),
                    ));
                }
            }
        }

        let (proof_token_type, proof, _) = proof_reader.read_proof()?;

        let root_hash = if proof_token_type == AbsentPath {
            self.verify_absent_path(&mut proof_reader, path_slices)?
        } else {
            let path_owned = query.path.iter().map(|a| a.to_vec()).collect();
            let mut last_subtree_root_hash = self.execute_subquery_proof(
                proof_token_type,
                proof,
                &mut proof_reader,
                query.as_ref(),
                path_owned,
            )?;

            // validate the path elements are connected
            self.verify_path_to_root(
                query.as_ref(),
                query.path.iter().map(|a| a.as_ref()).collect(),
                &mut proof_reader,
                &mut last_subtree_root_hash,
            )?
        };

        Ok(root_hash)
    }

    fn execute_subquery_proof(
        &mut self,
        proof_token_type: ProofTokenType,
        proof: Vec<u8>,
        proof_reader: &mut ProofReader,
        query: &PathQuery,
        path: Path,
    ) -> Result<[u8; 32], Error> {
        let last_root_hash: [u8; 32];

        match proof_token_type {
            ProofTokenType::SizedMerk => {
                // verify proof with limit and offset values
                let verification_result = self.execute_merk_proof(
                    ProofTokenType::SizedMerk,
                    &proof,
                    &query.query.query,
                    query.query.query.left_to_right,
                    path,
                )?;

                last_root_hash = verification_result.0;
            }
            ProofTokenType::Merk => {
                // for non leaf subtrees, we want to prove that all the queried keys
                // have an accompanying proof as long as the limit is non zero
                // and their child subtree is not empty
                let (proof_root_hash, children) = self.execute_merk_proof(
                    ProofTokenType::Merk,
                    &proof,
                    &query.query.query,
                    query.query.query.left_to_right,
                    path,
                )?;

                last_root_hash = proof_root_hash;
                let children = children.ok_or(Error::InvalidProof(
                    "MERK_PROOF always returns a result set",
                ))?;

                for proved_path_key_value in children {
                    let ProvedPathKeyValue {
                        path,
                        key,
                        value: value_bytes,
                        proof: value_hash,
                    } = proved_path_key_value;
                    let child_element = Element::deserialize(value_bytes.as_slice())?;
                    match child_element {
                        Element::Tree(expected_root_key, _)
                        | Element::SumTree(expected_root_key, ..) => {
                            let mut expected_combined_child_hash = value_hash;
                            let mut current_value_bytes = value_bytes;

                            if self.limit == Some(0) {
                                // we are done verifying the subqueries
                                break;
                            }

                            let (subquery_path, subquery_value) =
                                Element::subquery_paths_and_value_for_sized_query(
                                    &query.query,
                                    key.as_slice(),
                                );

                            if subquery_value.is_none() && subquery_path.is_none() {
                                // add this element to the result set
                                let skip_limit = reduce_limit_and_offset_by(
                                    &mut self.limit,
                                    &mut self.offset,
                                    1,
                                );

                                if !skip_limit {
                                    // only insert to the result set if the offset value is not
                                    // greater than 0
                                    self.result_set.push(
                                        ProvedPathKeyValue::from_proved_key_value(
                                            path,
                                            ProvedKeyValue {
                                                key,
                                                value: current_value_bytes,
                                                proof: value_hash,
                                            },
                                        ),
                                    );
                                }

                                continue;
                            }

                            // What is the equivalent for an empty tree
                            if expected_root_key.is_none() {
                                // child node is empty, move on to next
                                continue;
                            }

                            // update the path, we are about to perform a subquery call
                            let mut new_path = path.to_owned();
                            new_path.push(key);

                            if subquery_path.is_some()
                                && !subquery_path.as_ref().unwrap().is_empty()
                            {
                                if subquery_value.is_none() {
                                    self.verify_subquery_path(
                                        proof_reader,
                                        ProofTokenType::SizedMerk,
                                        &mut subquery_path.expect("confirmed it has a value above"),
                                        &mut expected_combined_child_hash,
                                        &mut current_value_bytes,
                                        &mut new_path,
                                    )?;
                                    continue;
                                } else {
                                    let (_, result_set_opt, encountered_absence) = self
                                        .verify_subquery_path(
                                            proof_reader,
                                            ProofTokenType::Merk,
                                            &mut subquery_path
                                                .expect("confirmed it has a value above"),
                                            &mut expected_combined_child_hash,
                                            &mut current_value_bytes,
                                            &mut new_path,
                                        )?;

                                    if encountered_absence {
                                        // we hit an absence proof while verifying the subquery path
                                        continue;
                                    }

                                    let subquery_path_result_set = result_set_opt;
                                    if subquery_path_result_set.is_none() {
                                        // this means a sized proof was generated for the subquery
                                        // key
                                        // which is invalid as there exists a subquery value
                                        return Err(Error::InvalidProof(
                                            "expected unsized proof for subquery path as subquery \
                                             value exists",
                                        ));
                                    }
                                    let subquery_path_result_set =
                                        subquery_path_result_set.expect("confirmed exists above");

                                    if subquery_path_result_set.is_empty() {
                                        // we have a valid proof that shows the absence of the
                                        // subquery path in the tree, hence the subquery value
                                        // cannot be applied, move on to the next.
                                        continue;
                                    }

                                    Self::update_root_key_from_subquery_path_element(
                                        &mut expected_combined_child_hash,
                                        &mut current_value_bytes,
                                        &subquery_path_result_set,
                                    )?;
                                }
                            }

                            let new_path_query =
                                PathQuery::new_unsized(vec![], subquery_value.unwrap());

                            let (child_proof_token_type, child_proof) = proof_reader
                                .read_next_proof(new_path.last().unwrap_or(&Default::default()))?;

                            let child_hash = self.execute_subquery_proof(
                                child_proof_token_type,
                                child_proof,
                                proof_reader,
                                &new_path_query,
                                new_path,
                            )?;

                            let combined_child_hash = combine_hash(
                                value_hash_fn(&current_value_bytes).value(),
                                &child_hash,
                            )
                            .value()
                            .to_owned();

                            if combined_child_hash != expected_combined_child_hash {
                                return Err(Error::InvalidProof(
                                    "child hash doesn't match the expected hash",
                                ));
                            }
                        }
                        _ => {
                            // encountered a non tree element, we can't apply a subquery to it
                            // add it to the result set.
                            if self.limit == Some(0) {
                                break;
                            }

                            let skip_limit =
                                reduce_limit_and_offset_by(&mut self.limit, &mut self.offset, 1);

                            if !skip_limit {
                                // only insert to the result set if the offset value is not greater
                                // than 0
                                self.result_set
                                    .push(ProvedPathKeyValue::from_proved_key_value(
                                        path,
                                        ProvedKeyValue {
                                            key,
                                            value: value_bytes,
                                            proof: value_hash,
                                        },
                                    ));
                            }
                        }
                    }
                }
            }
            ProofTokenType::EmptyTree => {
                last_root_hash = EMPTY_TREE_HASH;
            }
            _ => {
                // execute_subquery_proof only expects proofs for merk trees
                // root proof is handled separately
                return Err(Error::InvalidProof("wrong proof type"));
            }
        }
        Ok(last_root_hash)
    }

    /// Deserialize subkey_element and update expected root hash and element
    /// value
    fn update_root_key_from_subquery_path_element(
        expected_child_hash: &mut CryptoHash,
        current_value_bytes: &mut Vec<u8>,
        subquery_path_result_set: &[ProvedPathKeyValue],
    ) -> Result<(), Error> {
        let elem_value = &subquery_path_result_set[0].value;
        let subquery_path_element = Element::deserialize(elem_value)
            .map_err(|_| Error::CorruptedData("failed to deserialize element".to_string()))?;
        match subquery_path_element {
            Element::Tree(..) | Element::SumTree(..) => {
                *expected_child_hash = subquery_path_result_set[0].proof;
                *current_value_bytes = subquery_path_result_set[0].value.to_owned();
            }
            _ => {
                // the means that the subquery path pointed to a non tree
                // element, this is not valid as you cannot apply the
                // the subquery value to non tree items
                return Err(Error::InvalidProof(
                    "subquery path cannot point to non tree element",
                ));
            }
        }
        Ok(())
    }

    /// Checks that a valid proof showing the existence or absence of the
    /// subquery path is present
    fn verify_subquery_path(
        &mut self,
        proof_reader: &mut ProofReader,
        expected_proof_token_type: ProofTokenType,
        subquery_path: &mut Path,
        expected_root_hash: &mut CryptoHash,
        current_value_bytes: &mut Vec<u8>,
        current_path: &mut Path,
    ) -> Result<(CryptoHash, Option<ProvedPathKeyValues>, EncounteredAbsence), Error> {
        // the subquery path contains at least one item.
        let last_key = subquery_path.remove(subquery_path.len() - 1);

        for subquery_key in subquery_path.iter() {
            let (proof_token_type, subkey_proof) =
                proof_reader.read_next_proof(current_path.last().unwrap_or(&Default::default()))?;
            // intermediate proofs are all going to be unsized merk proofs
            if proof_token_type != ProofTokenType::Merk {
                return Err(Error::InvalidProof(
                    "expected MERK proof type for intermediate subquery path keys",
                ));
            }
            match proof_token_type {
                ProofTokenType::Merk => {
                    let mut key_as_query = Query::new();
                    key_as_query.insert_key(subquery_key.to_owned());
                    current_path.push(subquery_key.to_owned());

                    let (proof_root_hash, result_set) = self.execute_merk_proof(
                        proof_token_type,
                        &subkey_proof,
                        &key_as_query,
                        key_as_query.left_to_right,
                        current_path.to_owned(),
                    )?;

                    // should always be some as we force the proof type to be MERK
                    debug_assert!(result_set.is_some(), "{}", true);

                    // result_set being empty means we could not find the given key in the subtree
                    // which essentially means an absence proof
                    if result_set
                        .as_ref()
                        .expect("result set should always be some for merk proof type")
                        .is_empty()
                    {
                        return Ok((proof_root_hash, None, true));
                    }

                    // verify that the elements in the subquery path are linked by root hashes.
                    let combined_child_hash =
                        combine_hash(value_hash_fn(current_value_bytes).value(), &proof_root_hash)
                            .value()
                            .to_owned();

                    if combined_child_hash != *expected_root_hash {
                        return Err(Error::InvalidProof(
                            "child hash doesn't match the expected hash",
                        ));
                    }

                    // after confirming they are linked use the latest hash values for subsequent
                    // checks
                    Self::update_root_key_from_subquery_path_element(
                        expected_root_hash,
                        current_value_bytes,
                        &result_set.expect("confirmed is some"),
                    )?;
                }
                _ => {
                    return Err(Error::InvalidProof(
                        "expected merk of sized merk proof type for subquery path",
                    ));
                }
            }
        }

        let (proof_token_type, subkey_proof) =
            proof_reader.read_next_proof(current_path.last().unwrap_or(&Default::default()))?;
        if proof_token_type != expected_proof_token_type {
            return Err(Error::InvalidProof(
                "unexpected proof type for subquery path",
            ));
        }

        match proof_token_type {
            ProofTokenType::Merk | ProofTokenType::SizedMerk => {
                let mut key_as_query = Query::new();
                key_as_query.insert_key(last_key.to_owned());

                let verification_result = self.execute_merk_proof(
                    proof_token_type,
                    &subkey_proof,
                    &key_as_query,
                    key_as_query.left_to_right,
                    current_path.to_owned(),
                )?;

                current_path.push(last_key);

                Ok((verification_result.0, verification_result.1, false))
            }
            _ => Err(Error::InvalidProof(
                "expected merk or sized merk proof type for subquery path",
            )),
        }
    }

    fn verify_absent_path(
        &mut self,
        proof_reader: &mut ProofReader,
        path_slices: Vec<&[u8]>,
    ) -> Result<[u8; 32], Error> {
        let mut root_key_hash = None;
        let mut expected_child_hash = None;
        let mut last_result_set: ProvedPathKeyValues = vec![];

        for key in path_slices {
            let (proof_token_type, merk_proof, _) = proof_reader.read_proof()?;
            if proof_token_type == ProofTokenType::EmptyTree {
                // when we encounter the empty tree op, we need to ensure
                // that the expected tree hash is the combination of the
                // Element_value_hash and the empty root hash [0; 32]
                let combined_hash = combine_hash(
                    value_hash_fn(last_result_set[0].value.as_slice()).value(),
                    &[0; 32],
                )
                .unwrap();
                if Some(combined_hash) != expected_child_hash {
                    return Err(Error::InvalidProof(
                        "proof invalid: could not verify empty subtree while generating absent \
                         path proof",
                    ));
                } else {
                    last_result_set = vec![];
                    break;
                }
            } else if proof_token_type != ProofTokenType::Merk {
                return Err(Error::InvalidProof("expected a merk proof for absent path"));
            }

            let mut child_query = Query::new();
            child_query.insert_key(key.to_vec());

            // TODO: don't pass empty vec
            let proof_result = self.execute_merk_proof(
                ProofTokenType::Merk,
                &merk_proof,
                &child_query,
                true,
                // cannot return a result set
                Vec::new(),
            )?;

            if expected_child_hash.is_none() {
                root_key_hash = Some(proof_result.0);
            } else {
                let combined_hash = combine_hash(
                    value_hash_fn(last_result_set[0].value.as_slice()).value(),
                    &proof_result.0,
                )
                .value()
                .to_owned();
                if Some(combined_hash) != expected_child_hash {
                    return Err(Error::InvalidProof("proof invalid: invalid parent"));
                }
            }

            last_result_set = proof_result
                .1
                .expect("MERK_PROOF always returns a result set");
            if last_result_set.is_empty() {
                // if result set is empty then we have reached the absence point, break
                break;
            }

            let elem = Element::deserialize(last_result_set[0].value.as_slice())?;
            let child_hash = match elem {
                Element::Tree(..) | Element::SumTree(..) => Ok(Some(last_result_set[0].proof)),
                _ => Err(Error::InvalidProof(
                    "intermediate proofs should be for trees",
                )),
            }?;
            expected_child_hash = child_hash;
        }

        if last_result_set.is_empty() {
            if let Some(hash) = root_key_hash {
                Ok(hash)
            } else {
                Err(Error::InvalidProof("proof invalid: no non root tree found"))
            }
        } else {
            Err(Error::InvalidProof("proof invalid: path not absent"))
        }
    }

    /// Verifies that the correct proof was provided to confirm the path in
    /// query
    fn verify_path_to_root(
        &mut self,
        query: &PathQuery,
        path_slices: Vec<&[u8]>,
        proof_reader: &mut ProofReader,
        expected_root_hash: &mut [u8; 32],
    ) -> Result<[u8; 32], Error> {
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            // for every subtree, there should be a corresponding proof for the parent
            // which should prove that this subtree is a child of the parent tree
            let (proof_token_type, parent_merk_proof) =
                proof_reader.read_next_proof(path_slice.last().unwrap_or(&Default::default()))?;
            if proof_token_type != ProofTokenType::Merk {
                return Err(Error::InvalidProof("wrong data_type expected merk proof"));
            }

            let mut parent_query = Query::new();
            parent_query.insert_key(key.to_vec());

            let proof_result = self.execute_merk_proof(
                ProofTokenType::Merk,
                &parent_merk_proof,
                &parent_query,
                query.query.query.left_to_right,
                // TODO: don't pass empty vec
                Vec::new(),
            )?;

            let result_set = proof_result
                .1
                .expect("MERK_PROOF always returns a result set");
            if result_set.is_empty() || &result_set[0].key != key {
                return Err(Error::InvalidProof("proof invalid: invalid parent"));
            }

            let elem = Element::deserialize(result_set[0].value.as_slice())?;
            let child_hash = match elem {
                Element::Tree(..) | Element::SumTree(..) => Ok(result_set[0].proof),
                _ => Err(Error::InvalidProof(
                    "intermediate proofs should be for trees",
                )),
            }?;

            let combined_root_hash = combine_hash(
                value_hash_fn(&result_set[0].value).value(),
                expected_root_hash,
            )
            .value()
            .to_owned();
            if child_hash != combined_root_hash {
                return Err(Error::InvalidProof(
                    "Bad path: tree hash does not have expected hash",
                ));
            }

            *expected_root_hash = proof_result.0;

            split_path = path_slice.split_last();
        }

        Ok(*expected_root_hash)
    }

    /// Execute a merk proof, update the state when a sized proof is
    /// encountered i.e. update the limit, offset and result set values
    fn execute_merk_proof(
        &mut self,
        proof_token_type: ProofTokenType,
        proof: &[u8],
        query: &Query,
        left_to_right: bool,
        path: Path,
    ) -> Result<(CryptoHash, Option<ProvedPathKeyValues>), Error> {
        let is_sized_proof = proof_token_type == ProofTokenType::SizedMerk;
        let mut limit = None;
        let mut offset = None;

        if is_sized_proof {
            limit = self.limit;
            offset = self.offset;
        }

        let (hash, result) =
            grovedb_merk::execute_proof(proof, query, limit, offset, left_to_right)
                .unwrap()
                .map_err(|e| {
                    eprintln!("{e}");
                    Error::InvalidProof("invalid proof verification parameters")
                })?;

        // convert the result set to proved_path_key_values
        let proved_path_key_values =
            ProvedPathKeyValue::from_proved_key_values(path, result.result_set);

        if is_sized_proof {
            self.limit = result.limit;
            self.offset = result.offset;
            self.result_set.extend(proved_path_key_values);
            Ok((hash, None))
        } else {
            Ok((hash, Some(proved_path_key_values)))
        }
    }
}
