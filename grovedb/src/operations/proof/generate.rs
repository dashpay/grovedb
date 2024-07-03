//! Generate proof operations

use std::collections::BTreeMap;

use bincode::{Decode, Encode};
use derive_more::From;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::{
    proofs::{
        encode_into,
        query::{Key, QueryItem},
        Node, Op,
    },
    tree::value_hash,
    Merk, ProofWithoutEncodingResult,
};
use grovedb_path::SubtreePath;
use grovedb_storage::StorageContext;

use crate::{
    query_result_type::{BTreeMapLevelResult, BTreeMapLevelResultOrItem, QueryResultType},
    reference_path::path_from_reference_path_type,
    Element, Error, GroveDb, PathQuery,
};

#[derive(Debug, Clone, Copy)]
pub struct ProveOptions {
    pub is_verbose: bool,
    pub multilevel_results: bool,
}

impl Default for ProveOptions {
    fn default() -> Self {
        ProveOptions {
            is_verbose: false,
            multilevel_results: false,
        }
    }
}

#[derive(Encode, Decode)]
pub struct LayerProof {
    pub merk_proof: Vec<u8>,
    pub lower_layers: BTreeMap<Key, LayerProof>,
}

#[derive(Encode, Decode, From)]
pub enum GroveDBProof {
    V0(GroveDBProofV0),
}

#[derive(Encode, Decode)]
pub struct GroveDBProofV0 {
    pub root_layer: LayerProof,
}

impl GroveDb {
    /// Prove one or more path queries.
    /// If we have more than one path query, we merge into a single path query
    /// before proving.
    pub fn prove_query_many(
        &self,
        query: Vec<&PathQuery>,
        prove_options: Option<ProveOptions>,
    ) -> CostResult<Vec<u8>, Error> {
        if query.len() > 1 {
            let query = cost_return_on_error_default!(PathQuery::merge(query));
            self.prove_query(&query, prove_options)
        } else {
            self.prove_query(query[0], prove_options)
        }
    }

    /// Generate a minimalistic proof for a given path query
    /// doesn't allow for subset verification
    /// Proofs generated with this can only be verified by the path query used
    /// to generate them.
    pub fn prove_query(
        &self,
        query: &PathQuery,
        prove_options: Option<ProveOptions>,
    ) -> CostResult<Vec<u8>, Error> {
        self.prove_internal_serialized(query, prove_options)
    }

    /// Generates a proof and serializes it
    fn prove_internal_serialized(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
    ) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost::default();
        let proof =
            cost_return_on_error!(&mut cost, self.prove_internal(path_query, prove_options));
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let encoded_proof = cost_return_on_error_no_add!(
            &cost,
            bincode::encode_to_vec(proof, config)
                .map_err(|e| Error::CorruptedData(format!("unable to encode proof {}", e)))
        );
        Ok(encoded_proof).wrap_with_cost(cost)
    }

    /// Generates a proof
    fn prove_internal(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
    ) -> CostResult<GroveDBProof, Error> {
        let mut cost = OperationCost::default();

        if path_query.query.offset.is_some() && path_query.query.offset != Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not have offsets",
            ))
            .wrap_with_cost(cost);
        }

        // we want to query raw because we want the references to not be resolved at
        // this point

        let precomputed_result_map = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                false,
                true,
                false,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None
            )
        )
        .0
        .to_btree_map_level_results();

        println!("precomputed results are {:?}", precomputed_result_map);

        let root_layer = cost_return_on_error!(
            &mut cost,
            self.prove_subqueries(vec![], path_query, precomputed_result_map,)
        );

        Ok(GroveDBProofV0 { root_layer }.into()).wrap_with_cost(cost)
    }

    /// Perform a pre-order traversal of the tree based on the provided
    /// subqueries
    fn prove_subqueries(
        &self,
        path: Vec<&[u8]>,
        path_query: &PathQuery,
        layer_precomputed_results: BTreeMapLevelResult,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        let (query_at_path, left_to_right) = cost_return_on_error_no_add!(
            &cost,
            path_query
                .query_items_at_path(path.as_slice())
                .ok_or(Error::CorruptedPath("path should be part of path_query"))
        );

        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path.as_slice().into(), None)
        );

        let limit = layer_precomputed_results.key_values.len();

        let merk_proof = cost_return_on_error!(
            &mut cost,
            self.generate_merk_proof(
                &path.as_slice().into(),
                &subtree,
                &query_at_path,
                left_to_right,
                Some(limit as u16),
            )
        );

        let lower_layers = cost_return_on_error_no_add!(
            &cost,
            layer_precomputed_results
                .key_values
                .into_iter()
                .filter_map(|(key, value)| {
                    match value {
                        BTreeMapLevelResultOrItem::BTreeMapLevelResult(layer) => {
                            let mut lower_path = path.clone();
                            lower_path.push(key.as_slice());
                            match self
                                .prove_subqueries(lower_path, path_query, layer)
                                .unwrap_add_cost(&mut cost)
                            {
                                Ok(layer_proof) => Some(Ok((key, layer_proof))),
                                Err(e) => Some(Err(e)),
                            }
                        }
                        BTreeMapLevelResultOrItem::ResultItem(_) => None,
                    }
                })
                .collect::<Result<BTreeMap<Key, LayerProof>, Error>>()
        );

        Ok(LayerProof {
            merk_proof,
            lower_layers,
        })
        .wrap_with_cost(cost)
    }

    /// Generates query proof given a subtree and appends the result to a proof
    /// list
    fn generate_merk_proof<'a, S, B>(
        &self,
        path: &SubtreePath<B>,
        subtree: &'a Merk<S>,
        query_items: &Vec<QueryItem>,
        left_to_right: bool,
        limit: Option<u16>,
    ) -> CostResult<Vec<u8>, Error>
    where
        S: StorageContext<'a> + 'a,
        B: AsRef<[u8]>,
    {
        let mut cost = OperationCost::default();

        let mut proof_result = cost_return_on_error_no_add!(
            &cost,
            subtree
                .prove_unchecked_query_items(query_items, limit, left_to_right)
                .map_ok(|(proof, limit)| ProofWithoutEncodingResult::new(proof, limit))
                .unwrap()
                .map_err(|_e| Error::InternalError("failed to generate proof"))
        );

        cost_return_on_error!(
            &mut cost,
            self.post_process_merk_proof(path, &mut proof_result)
        );

        let mut proof_bytes = Vec::with_capacity(128);
        encode_into(proof_result.proof.iter(), &mut proof_bytes);

        Ok(proof_bytes).wrap_with_cost(cost)
    }

    /// Converts Items to Node::KV from Node::KVValueHash
    /// Converts References to Node::KVRefValueHash and sets the value to the
    /// referenced element
    fn post_process_merk_proof<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        proof_result: &mut ProofWithoutEncodingResult,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        for op in proof_result.proof.iter_mut() {
            match op {
                Op::Push(node) | Op::PushInverted(node) => match node {
                    Node::KV(key, value) | Node::KVValueHash(key, value, ..) => {
                        let elem = Element::deserialize(value);
                        match elem {
                            Ok(Element::Reference(reference_path, ..)) => {
                                let absolute_path = cost_return_on_error!(
                                    &mut cost,
                                    path_from_reference_path_type(
                                        reference_path,
                                        &path.to_vec(),
                                        Some(key.as_slice())
                                    )
                                    .wrap_with_cost(OperationCost::default())
                                );

                                let referenced_elem = cost_return_on_error!(
                                    &mut cost,
                                    self.follow_reference(
                                        absolute_path.as_slice().into(),
                                        true,
                                        None
                                    )
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
}
// #[cfg(test)]
// mod tests {
//     use grovedb_merk::{execute_proof, proofs::Query};
//     use grovedb_storage::StorageBatch;
//
//     use crate::{
//         operations::proof::util::{ProofReader, ProofTokenType},
//         tests::{common::EMPTY_PATH, make_deep_tree, TEST_LEAF},
//         GroveDb,
//     };
//
//     #[test]
//     fn test_path_info_encoding_and_decoding() {
//         let path = vec![b"a".as_slice(), b"b".as_slice(), b"c".as_slice()];
//         let mut proof_vector = vec![];
//         GroveDb::generate_and_store_path_proof(path.clone(), &mut
// proof_vector)             .unwrap()
//             .unwrap();
//
//         let mut proof_reader = ProofReader::new(proof_vector.as_slice());
//         let decoded_path = proof_reader.read_path_info().unwrap();
//
//         assert_eq!(path, decoded_path);
//     }
//
//     #[test]
//     fn test_reading_of_verbose_proofs() {
//         let db = make_deep_tree();
//
//         let path = vec![TEST_LEAF, b"innertree"];
//         let mut query = Query::new();
//         query.insert_all();
//
//         let batch = StorageBatch::new();
//
//         let merk = db
//             .open_non_transactional_merk_at_path(
//                 [TEST_LEAF, b"innertree"].as_ref().into(),
//                 Some(&batch),
//             )
//             .unwrap()
//             .unwrap();
//         let expected_root_hash = merk.root_hash().unwrap();
//
//         let mut proof = vec![];
//         db.generate_and_store_merk_proof(
//             &path.as_slice().into(),
//             &merk,
//             &query,
//             None,
//             ProofTokenType::Merk,
//             &mut proof,
//             true,
//             b"innertree",
//         )
//         .unwrap()
//         .unwrap();
//         assert_ne!(proof.len(), 0);
//
//         let mut proof_reader = ProofReader::new(&proof);
//         let (proof_token_type, proof, key) =
// proof_reader.read_verbose_proof().unwrap();
//
//         assert_eq!(proof_token_type, ProofTokenType::Merk);
//         assert_eq!(key, Some(b"innertree".to_vec()));
//
//         let (root_hash, result_set) = execute_proof(&proof, &query, None,
// true)             .unwrap()
//             .unwrap();
//         assert_eq!(root_hash, expected_root_hash);
//         assert_eq!(result_set.result_set.len(), 3);
//
//         // what is the key is empty??
//         let merk = db
//             .open_non_transactional_merk_at_path(EMPTY_PATH, Some(&batch))
//             .unwrap()
//             .unwrap();
//         let expected_root_hash = merk.root_hash().unwrap();
//
//         let mut proof = vec![];
//         db.generate_and_store_merk_proof(
//             &EMPTY_PATH,
//             &merk,
//             &query,
//             None,
//             ProofTokenType::Merk,
//             &mut proof,
//             true,
//             &[],
//         )
//         .unwrap()
//         .unwrap();
//         assert_ne!(proof.len(), 0);
//
//         let mut proof_reader = ProofReader::new(&proof);
//         let (proof_token_type, proof, key) =
// proof_reader.read_verbose_proof().unwrap();
//
//         assert_eq!(proof_token_type, ProofTokenType::Merk);
//         assert_eq!(key, Some(vec![]));
//
//         let (root_hash, result_set) = execute_proof(&proof, &query, None,
// true)             .unwrap()
//             .unwrap();
//         assert_eq!(root_hash, expected_root_hash);
//         assert_eq!(result_set.result_set.len(), 3);
//     }
//
//     #[test]
//     fn test_reading_verbose_proof_at_key() {
//         // going to generate an array of multiple proofs with different keys
//         let db = make_deep_tree();
//         let mut proofs = vec![];
//
//         let mut query = Query::new();
//         query.insert_all();
//
//         // insert all under inner tree
//         let path = vec![TEST_LEAF, b"innertree"];
//
//         let batch = StorageBatch::new();
//
//         let merk = db
//             .open_non_transactional_merk_at_path(path.as_slice().into(),
// Some(&batch))             .unwrap()
//             .unwrap();
//         let inner_tree_root_hash = merk.root_hash().unwrap();
//         db.generate_and_store_merk_proof(
//             &path.as_slice().into(),
//             &merk,
//             &query,
//             None,
//             ProofTokenType::Merk,
//             &mut proofs,
//             true,
//             path.iter().last().unwrap_or(&(&[][..])),
//         )
//         .unwrap()
//         .unwrap();
//
//         // insert all under innertree4
//         let path = vec![TEST_LEAF, b"innertree4"];
//         let merk = db
//             .open_non_transactional_merk_at_path(path.as_slice().into(),
// Some(&batch))             .unwrap()
//             .unwrap();
//         let inner_tree_4_root_hash = merk.root_hash().unwrap();
//         db.generate_and_store_merk_proof(
//             &path.as_slice().into(),
//             &merk,
//             &query,
//             None,
//             ProofTokenType::Merk,
//             &mut proofs,
//             true,
//             path.iter().last().unwrap_or(&(&[][..])),
//         )
//         .unwrap()
//         .unwrap();
//
//         // insert all for deeper_1
//         let path: Vec<&[u8]> = vec![b"deep_leaf", b"deep_node_1",
// b"deeper_1"];         let merk = db
//             .open_non_transactional_merk_at_path(path.as_slice().into(),
// Some(&batch))             .unwrap()
//             .unwrap();
//         let deeper_1_root_hash = merk.root_hash().unwrap();
//         db.generate_and_store_merk_proof(
//             &path.as_slice().into(),
//             &merk,
//             &query,
//             None,
//             ProofTokenType::Merk,
//             &mut proofs,
//             true,
//             path.iter().last().unwrap_or(&(&[][..])),
//         )
//         .unwrap()
//         .unwrap();
//
//         // read the proof at innertree
//         let contextual_proof = proofs.clone();
//         let mut proof_reader = ProofReader::new(&contextual_proof);
//         let (proof_token_type, proof) = proof_reader
//             .read_verbose_proof_at_key(b"innertree")
//             .unwrap();
//
//         assert_eq!(proof_token_type, ProofTokenType::Merk);
//
//         let (root_hash, result_set) = execute_proof(&proof, &query, None,
// true)             .unwrap()
//             .unwrap();
//         assert_eq!(root_hash, inner_tree_root_hash);
//         assert_eq!(result_set.result_set.len(), 3);
//
//         // read the proof at innertree4
//         let contextual_proof = proofs.clone();
//         let mut proof_reader = ProofReader::new(&contextual_proof);
//         let (proof_token_type, proof) = proof_reader
//             .read_verbose_proof_at_key(b"innertree4")
//             .unwrap();
//
//         assert_eq!(proof_token_type, ProofTokenType::Merk);
//
//         let (root_hash, result_set) = execute_proof(&proof, &query, None,
// true)             .unwrap()
//             .unwrap();
//         assert_eq!(root_hash, inner_tree_4_root_hash);
//         assert_eq!(result_set.result_set.len(), 2);
//
//         // read the proof at deeper_1
//         let contextual_proof = proofs.clone();
//         let mut proof_reader = ProofReader::new(&contextual_proof);
//         let (proof_token_type, proof) =
//             proof_reader.read_verbose_proof_at_key(b"deeper_1").unwrap();
//
//         assert_eq!(proof_token_type, ProofTokenType::Merk);
//
//         let (root_hash, result_set) = execute_proof(&proof, &query, None,
// true)             .unwrap()
//             .unwrap();
//         assert_eq!(root_hash, deeper_1_root_hash);
//         assert_eq!(result_set.result_set.len(), 3);
//
//         // read the proof at an invalid key
//         let contextual_proof = proofs.clone();
//         let mut proof_reader = ProofReader::new(&contextual_proof);
//         let reading_result =
// proof_reader.read_verbose_proof_at_key(b"unknown_key");         assert!
// (reading_result.is_err())     }
// }
