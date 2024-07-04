//! Generate proof operations

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

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
        Decoder, Node, Op, Tree,
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
    pub decrease_limit_on_empty_sub_query_result: bool,
}

impl Default for ProveOptions {
    fn default() -> Self {
        ProveOptions {
            decrease_limit_on_empty_sub_query_result: true,
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

impl fmt::Display for LayerProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "LayerProof {{")?;
        writeln!(f, "  merk_proof: {}", decode_merk_proof(&self.merk_proof))?;
        if !self.lower_layers.is_empty() {
            writeln!(f, "  lower_layers: {{")?;
            for (key, layer_proof) in &self.lower_layers {
                writeln!(f, "    {} => {{", hex_to_ascii(key))?;
                for line in format!("{}", layer_proof).lines() {
                    writeln!(f, "      {}", line)?;
                }
                writeln!(f, "    }}")?;
            }
            writeln!(f, "  }}")?;
        }
        write!(f, "}}")
    }
}

impl fmt::Display for GroveDBProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GroveDBProof::V0(proof) => write!(f, "{}", proof),
        }
    }
}

impl fmt::Display for GroveDBProofV0 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "GroveDBProofV0 {{")?;
        for line in format!("{}", self.root_layer).lines() {
            writeln!(f, "  {}", line)?;
        }
        write!(f, "}}")
    }
}

fn decode_merk_proof(proof: &[u8]) -> String {
    let mut result = String::new();
    let ops = Decoder::new(proof);

    for (i, op) in ops.enumerate() {
        match op {
            Ok(op) => {
                result.push_str(&format!("\n    {}: {}", i, op_to_string(&op)));
            }
            Err(e) => {
                result.push_str(&format!("\n    {}: Error decoding op: {}", i, e));
            }
        }
    }

    result
}

fn op_to_string(op: &Op) -> String {
    match op {
        Op::Push(node) => format!("Push({})", node_to_string(node)),
        Op::PushInverted(node) => format!("PushInverted({})", node_to_string(node)),
        Op::Parent => "Parent".to_string(),
        Op::Child => "Child".to_string(),
        Op::ParentInverted => "ParentInverted".to_string(),
        Op::ChildInverted => "ChildInverted".to_string(),
    }
}

fn node_to_string(node: &Node) -> String {
    match node {
        Node::Hash(hash) => format!("Hash(HASH[{}])", hex::encode(hash)),
        Node::KVHash(kv_hash) => format!("KVHash(HASH[{}])", hex::encode(kv_hash)),
        Node::KV(key, value) => {
            format!("KV({}, {})", hex_to_ascii(key), element_hex_to_ascii(value))
        }
        Node::KVValueHash(key, value, value_hash) => format!(
            "KVValueHash({}, {}, HASH[{}])",
            hex_to_ascii(key),
            element_hex_to_ascii(value),
            hex::encode(value_hash)
        ),
        Node::KVDigest(key, value_hash) => format!(
            "KVDigest({}, HASH[{}])",
            hex_to_ascii(key),
            hex::encode(value_hash)
        ),
        Node::KVRefValueHash(key, value, value_hash) => format!(
            "KVRefValueHash({}, {}, HASH[{}])",
            hex_to_ascii(key),
            element_hex_to_ascii(value),
            hex::encode(value_hash)
        ),
        Node::KVValueHashFeatureType(key, value, value_hash, feature_type) => format!(
            "KVValueHashFeatureType({}, {}, HASH[{}], {:?})",
            hex_to_ascii(key),
            element_hex_to_ascii(value),
            hex::encode(value_hash),
            feature_type
        ),
    }
}

fn element_hex_to_ascii(hex_value: &[u8]) -> String {
    Element::deserialize(hex_value)
        .map(|e| e.to_string())
        .unwrap_or_else(|_| hex::encode(hex_value))
}

fn hex_to_ascii(hex_value: &[u8]) -> String {
    String::from_utf8(hex_value.to_vec()).unwrap_or_else(|_| hex::encode(hex_value))
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
        println!("constructed proof is {}", proof);
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

        let prove_options = prove_options.unwrap_or_default();

        if path_query.query.offset.is_some() && path_query.query.offset != Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not have offsets",
            ))
            .wrap_with_cost(cost);
        }

        // we want to query raw because we want the references to not be resolved at
        // this point

        let values = cost_return_on_error!(
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
        .0;

        println!("values are {}", values);

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

        println!("precomputed results are {}", precomputed_result_map);

        let mut limit = path_query.query.limit;

        let root_layer = cost_return_on_error!(
            &mut cost,
            self.prove_subqueries(
                vec![],
                path_query,
                &mut limit,
                Some(precomputed_result_map),
                &prove_options
            )
        );

        Ok(GroveDBProofV0 { root_layer }.into()).wrap_with_cost(cost)
    }

    /// Perform a pre-order traversal of the tree based on the provided
    /// subqueries
    fn prove_subqueries(
        &self,
        path: Vec<&[u8]>,
        path_query: &PathQuery,
        overall_limit: &mut Option<u16>,
        mut layer_precomputed_results: Option<BTreeMapLevelResult>,
        prove_options: &ProveOptions,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        let (query_at_path, left_to_right, has_subqueries) = cost_return_on_error_no_add!(
            &cost,
            path_query
                .query_items_at_path(path.as_slice())
                .ok_or(Error::CorruptedPath(format!(
                    "prove subqueries: path {} should be part of path_query {}",
                    path.iter().map(hex::encode).collect::<Vec<_>>().join("/"),
                    path_query
                )))
        );

        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path.as_slice().into(), None)
        );

        // let mut items_to_prove: BTreeSet<Vec<u8>> = layer_precomputed_results
        //     .as_ref()
        //     .map_or(BTreeSet::new(), |map| {
        //         map.key_values.keys().cloned().collect()
        //     });
        //
        // for query_item in query_at_path.as_slice() {
        //     match query_item {
        //         QueryItem::Key(key) => {
        //             items_to_prove.insert(key.clone());
        //         }
        //         _ => {}
        //     }
        // }

        let limit = if path.len() < path_query.path.len() {
            // There is no need for a limit because we are only asking for a single item
            None
        } else {
            *overall_limit
        };

        let (merk_proof, sub_level_keys, results_found) = cost_return_on_error!(
            &mut cost,
            self.generate_merk_proof(
                &path.as_slice().into(),
                &subtree,
                &query_at_path,
                left_to_right,
                has_subqueries,
                limit,
            )
        );

        if prove_options.decrease_limit_on_empty_sub_query_result
            && sub_level_keys.is_empty()
            && results_found == 0
        {
            // In this case we should reduce by 1 to prevent attacks on proofs
            overall_limit.as_mut().map(|limit| *limit -= 1);
        } else if results_found > 0 {
            overall_limit.as_mut().map(|limit| *limit -= results_found);
        };

        println!(
            "generated merk proof at level path level [{}] sub level keys found are [{}], limit \
             is {:?}, results found were {}, has subqueries {}, {}",
            path.iter()
                .map(|a| hex_to_ascii(*a))
                .collect::<Vec<_>>()
                .join("/"),
            sub_level_keys
                .iter()
                .map(|a| hex_to_ascii(a))
                .collect::<Vec<_>>()
                .join(", "),
            overall_limit,
            results_found,
            has_subqueries,
            if left_to_right {
                "left to right"
            } else {
                "right to left"
            }
        );

        let lower_layers = cost_return_on_error_no_add!(
            &cost,
            sub_level_keys
                .into_iter()
                .map_while(|(key)| {
                    // Check if we should stop after processing this key
                    if *overall_limit == Some(0) {
                        return None;
                    }
                    let mut lower_path = path.clone();
                    lower_path.push(key.as_slice());
                    let mut early_exit = false;
                    let lower_known_layer: Option<BTreeMapLevelResult> =
                        match layer_precomputed_results
                            .as_mut()
                            .and_then(|mut layer_precomputed_results| {
                                layer_precomputed_results.key_values.remove(&key).and_then(
                                    |result_or_item| match result_or_item {
                                        BTreeMapLevelResultOrItem::BTreeMapLevelResult(value) => {
                                            Some(Ok(value))
                                        }
                                        _ => {
                                            early_exit = true;
                                            None
                                        }
                                    },
                                )
                            })
                            .transpose()
                        {
                            Ok(lower_known_layer) => lower_known_layer,
                            Err(e) => return Some(Some(Err(e))),
                        };
                    if early_exit {
                        return Some(None);
                    }
                    let result = self
                        .prove_subqueries(
                            lower_path,
                            path_query,
                            overall_limit,
                            lower_known_layer,
                            prove_options,
                        )
                        .unwrap_add_cost(&mut cost)
                        .map(|layer_proof| (key, layer_proof));

                    Some(Some(result))
                })
                .filter_map(|a| a)
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
        has_any_subquery: bool,
        limit: Option<u16>,
    ) -> CostResult<(Vec<u8>, Vec<Vec<u8>>, u16), Error>
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
                .map_err(|e| Error::InternalError(format!(
                    "failed to generate proof for query_items [{}] error is : {}",
                    query_items
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    e
                )))
        );

        let (tree_keys, results_found) = cost_return_on_error!(
            &mut cost,
            self.post_process_merk_proof(path, has_any_subquery, &mut proof_result)
        );

        let mut proof_bytes = Vec::with_capacity(128);
        encode_into(proof_result.proof.iter(), &mut proof_bytes);

        Ok((proof_bytes, tree_keys, results_found)).wrap_with_cost(cost)
    }

    /// Converts Items to Node::KV from Node::KVValueHash
    /// Converts References to Node::KVRefValueHash and sets the value to the
    /// referenced element
    fn post_process_merk_proof<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        has_any_subquery: bool,
        proof_result: &mut ProofWithoutEncodingResult,
    ) -> CostResult<(Vec<Key>, u16), Error> {
        let mut cost = OperationCost::default();
        let mut results_found = 0;

        let mut sub_level_keys = vec![];

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
                                );
                                results_found += 1;
                            }
                            Ok(Element::Item(..)) => {
                                println!("found {}", hex_to_ascii(key));
                                *node = Node::KV(key.to_owned(), value.to_owned());
                                results_found += 1;
                            }
                            Ok(Element::Tree(Some(_), _)) => {
                                println!("found tree {}", hex_to_ascii(key));
                                // We only want to check in sub nodes for the proof if the tree has
                                // elements
                                sub_level_keys.push(key.clone());
                            }
                            Ok(Element::SumTree(Some(_), ..)) => {
                                // We only want to check in sub nodes for the proof if the tree has
                                // elements
                                sub_level_keys.push(key.clone());
                                if !has_any_subquery {
                                    results_found += 1; // if there is no
                                                        // subquery we return
                                                        // Empty trees
                                }
                            }
                            Ok(Element::Tree(None, _)) | Ok(Element::SumTree(None, ..)) => {
                                if !has_any_subquery {
                                    results_found += 1; // if there is no
                                                        // subquery we return
                                                        // Empty trees
                                }
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }

        Ok((sub_level_keys, results_found)).wrap_with_cost(cost)
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
