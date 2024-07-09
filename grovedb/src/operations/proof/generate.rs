//! Generate proof operations

use std::{collections::BTreeMap, fmt};

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
        Decoder, Node, Op,
    },
    tree::value_hash,
    Merk, ProofWithoutEncodingResult,
};
use grovedb_storage::StorageContext;

#[cfg(feature = "proof_debug")]
use crate::query_result_type::QueryResultType;
use crate::{
    operations::proof::util::{element_hex_to_ascii, hex_to_ascii},
    reference_path::path_from_reference_path_type,
    Element, Error, GroveDb, PathQuery,
};

#[derive(Debug, Clone, Copy, Encode, Decode)]
pub struct ProveOptions {
    pub decrease_limit_on_empty_sub_query_result: bool,
}

impl fmt::Display for ProveOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ProveOptions {{ decrease_limit_on_empty_sub_query_result: {} }}",
            self.decrease_limit_on_empty_sub_query_result
        )
    }
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
    pub prove_options: ProveOptions,
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
        #[cfg(feature = "proof_debug")]
        {
            println!("constructed proof is {}", proof);
        }
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

        if path_query.query.limit == Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not be for limit 0",
            ))
            .wrap_with_cost(cost);
        }

        #[cfg(feature = "proof_debug")]
        {
            // we want to query raw because we want the references to not be resolved at
            // this point

            let values = cost_return_on_error!(
                &mut cost,
                self.query_raw(
                    path_query,
                    false,
                    prove_options.decrease_limit_on_empty_sub_query_result,
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
                    prove_options.decrease_limit_on_empty_sub_query_result,
                    false,
                    QueryResultType::QueryPathKeyElementTrioResultType,
                    None
                )
            )
            .0
            .to_btree_map_level_results();

            println!("precomputed results are {}", precomputed_result_map);
        }

        let mut limit = path_query.query.limit;

        let root_layer = cost_return_on_error!(
            &mut cost,
            self.prove_subqueries(vec![], path_query, &mut limit, &prove_options)
        );

        Ok(GroveDBProofV0 {
            root_layer,
            prove_options,
        }
        .into())
        .wrap_with_cost(cost)
    }

    /// Perform a pre-order traversal of the tree based on the provided
    /// subqueries
    fn prove_subqueries(
        &self,
        path: Vec<&[u8]>,
        path_query: &PathQuery,
        overall_limit: &mut Option<u16>,
        prove_options: &ProveOptions,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        let query = cost_return_on_error_no_add!(
            &cost,
            path_query
                .query_items_at_path(path.as_slice())
                .ok_or(Error::CorruptedPath(format!(
                    "prove subqueries: path {} should be part of path_query {}",
                    path.iter()
                        .map(|a| hex_to_ascii(*a))
                        .collect::<Vec<_>>()
                        .join("/"),
                    path_query
                )))
        );

        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path.as_slice().into(), None)
        );

        let limit = if path.len() < path_query.path.len() {
            // There is no need for a limit because we are only asking for a single item
            None
        } else {
            *overall_limit
        };

        let mut merk_proof = cost_return_on_error!(
            &mut cost,
            self.generate_merk_proof(&subtree, &query.items, query.left_to_right, limit)
        );

        #[cfg(feature = "proof_debug")]
        {
            println!(
                "generated merk proof at level path level [{}], limit is {:?}, {}",
                path.iter()
                    .map(|a| hex_to_ascii(*a))
                    .collect::<Vec<_>>()
                    .join("/"),
                overall_limit,
                if query.left_to_right {
                    "left to right"
                } else {
                    "right to left"
                }
            );
        }

        let mut lower_layers = BTreeMap::new();

        let mut has_a_result_at_level = false;
        let mut done_with_results = false;

        for op in merk_proof.proof.iter_mut() {
            done_with_results |= overall_limit == &Some(0);
            match op {
                Op::Push(node) | Op::PushInverted(node) => match node {
                    Node::KV(key, value) | Node::KVValueHash(key, value, ..)
                        if !done_with_results =>
                    {
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
                                overall_limit.as_mut().map(|limit| *limit -= 1);
                                has_a_result_at_level |= true;
                            }
                            Ok(Element::Item(..)) if !done_with_results => {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!("found {}", hex_to_ascii(key));
                                }
                                *node = Node::KV(key.to_owned(), value.to_owned());
                                overall_limit.as_mut().map(|limit| *limit -= 1);
                                has_a_result_at_level |= true;
                            }
                            Ok(Element::Tree(Some(_), _)) | Ok(Element::SumTree(Some(_), ..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!(
                                        "found tree {}, query is {}",
                                        hex_to_ascii(key),
                                        query
                                    );
                                }
                                // We only want to check in sub nodes for the proof if the tree has
                                // elements
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries(
                                        lower_path,
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                    )
                                );

                                if previous_limit != *overall_limit {
                                    // a lower layer updated the limit, don't subtract 1 at this
                                    // level
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            Ok(Element::Tree(..)) | Ok(Element::SumTree(..))
                                if !done_with_results =>
                            {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!(
                                        "found tree {}, no subquery query is {:?}",
                                        hex_to_ascii(key),
                                        query
                                    );
                                }
                                overall_limit.as_mut().map(|limit| *limit -= 1);
                                has_a_result_at_level |= true;
                            }
                            // todo: transform the unused trees into a Hash or KVHash to make proof
                            // smaller Ok(Element::Tree(..)) if
                            // done_with_results => {     *node =
                            // Node::Hash()     // we are done with the
                            // results, we can modify the proof to alter
                            // }
                            _ => continue,
                        }
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }

        if !has_a_result_at_level
            && !done_with_results
            && prove_options.decrease_limit_on_empty_sub_query_result
        {
            #[cfg(feature = "proof_debug")]
            {
                println!(
                    "no results at level {}",
                    path.iter()
                        .map(|a| hex_to_ascii(*a))
                        .collect::<Vec<_>>()
                        .join("/")
                );
            }
            overall_limit.as_mut().map(|limit| *limit -= 1);
        }

        let mut serialized_merk_proof = Vec::with_capacity(1024);
        encode_into(merk_proof.proof.iter(), &mut serialized_merk_proof);

        Ok(LayerProof {
            merk_proof: serialized_merk_proof,
            lower_layers,
        })
        .wrap_with_cost(cost)
    }

    /// Generates query proof given a subtree and appends the result to a proof
    /// list
    fn generate_merk_proof<'a, S>(
        &self,
        subtree: &'a Merk<S>,
        query_items: &Vec<QueryItem>,
        left_to_right: bool,
        limit: Option<u16>,
    ) -> CostResult<ProofWithoutEncodingResult, Error>
    where
        S: StorageContext<'a> + 'a,
    {
        subtree
            .prove_unchecked_query_items(query_items, limit, left_to_right)
            .map_ok(|(proof, limit)| ProofWithoutEncodingResult::new(proof, limit))
            .map_err(|e| {
                Error::InternalError(format!(
                    "failed to generate proof for query_items [{}] error is : {}",
                    query_items
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    e
                ))
            })
    }
}
