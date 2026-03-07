use std::fmt;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};

#[cfg(feature = "minimal")]
use crate::proofs::query::{Map, MapBuilder};
use crate::{
    error::Error,
    proofs::{hex_to_ascii, tree::execute, Decoder, Node, Query},
    tree::value_hash,
    CryptoHash as MerkHash, CryptoHash,
};

/// Verify proof against expected hash
#[cfg(feature = "minimal")]
#[deprecated]
#[allow(unused)]
pub fn verify(bytes: &[u8], expected_hash: MerkHash) -> CostResult<Map, Error> {
    let ops = Decoder::new(bytes);
    let mut map_builder = MapBuilder::new();

    execute(ops, true, |node| map_builder.insert(node)).flat_map_ok(|root| {
        root.hash().map(|hash| {
            if hash != expected_hash {
                Err(Error::InvalidProofError(format!(
                    "Proof did not match expected hash\n\tExpected: {:?}\n\tActual: {:?}",
                    expected_hash,
                    root.hash()
                )))
            } else {
                Ok(map_builder.build())
            }
        })
    })
}

/// Options controlling proof verification behavior.
#[derive(Copy, Clone, Debug)]
pub struct VerifyOptions {
    /// When set to true, this will give back absence proofs for any query items
    /// that are keys. This means QueryItem::Key(), and not the ranges.
    pub absence_proofs_for_non_existing_searched_keys: bool,
    /// When true, reject proofs that contain extra lower-layer data beyond
    /// what the query requires (e.g. proof covers subtrees A and B but query
    /// only asks for A). When false, extra data is tolerated (subset
    /// verification).
    pub verify_proof_succinctness: bool,
    /// Should return empty trees in the result?
    pub include_empty_trees_in_result: bool,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: true,
            verify_proof_succinctness: true,
            include_empty_trees_in_result: false,
        }
    }
}

/// Extension trait adding proof verification methods to `Query`.
///
/// These methods depend on merk-internal types (Node, Op, Decoder, etc.)
/// and therefore cannot live in the `grovedb-query` crate.
pub trait QueryProofVerify {
    /// Verifies the encoded proof with the given query, returning the root
    /// hash and verification result.
    fn execute_proof(
        &self,
        bytes: &[u8],
        limit: Option<u16>,
        left_to_right: bool,
    ) -> CostResult<(MerkHash, ProofVerificationResult), Error>;

    /// Verifies the encoded proof with the given query and expected hash.
    fn verify_proof(
        &self,
        bytes: &[u8],
        limit: Option<u16>,
        left_to_right: bool,
        expected_hash: MerkHash,
    ) -> CostResult<ProofVerificationResult, Error>;
}

impl QueryProofVerify for Query {
    /// Verifies the encoded proof with the given query
    ///
    /// Every key in `keys` is checked to either have a key/value pair in the
    /// proof, or to have its absence in the tree proven.
    ///
    /// Returns `Err` if the proof is invalid, or a list of proven values
    /// associated with `keys`. For example, if `keys` contains keys `A` and
    /// `B`, the returned list will contain 2 elements, the value of `A` and
    /// the value of `B`. Keys proven to be absent in the tree will have an
    /// entry of `None`, keys that have a proven value will have an entry of
    /// `Some(value)`.
    fn execute_proof(
        &self,
        bytes: &[u8],
        limit: Option<u16>,
        left_to_right: bool,
    ) -> CostResult<(MerkHash, ProofVerificationResult), Error> {
        #[cfg(feature = "proof_debug")]
        {
            println!(
                "executing proof with limit {:?} going {} using query {}",
                limit,
                if left_to_right {
                    "left to right"
                } else {
                    "right to left"
                },
                self
            );
        }
        let mut cost = OperationCost::default();

        let mut output = Vec::with_capacity(self.len());
        let mut last_push = None;
        let mut query = self.directional_iter(left_to_right).peekable();
        let mut in_range = false;
        let original_limit = limit;
        let mut current_limit = limit;

        let ops = Decoder::new(bytes);

        let root_wrapped = execute(ops, true, |node| {
            let mut execute_node = |key: &Vec<u8>,
                                    value: Option<&Vec<u8>>,
                                    value_hash: CryptoHash,
                                    value_hash_is_computed: bool,
                                    is_reference_result: bool|
             -> Result<_, Error> {
                while let Some(item) = query.peek() {
                    // get next item in query
                    let query_item = *item;
                    let (lower_bound, start_non_inclusive) = query_item.lower_bound();
                    let (upper_bound, end_inclusive) = query_item.upper_bound();

                    // terminate if we encounter a node before the current query item.
                    // this means a node less than the current query item for left to right.
                    // and a node greater than the current query item for right to left.
                    let terminate = if left_to_right {
                        // if the query item is lower unbounded, then a node cannot be less than it.
                        // checks that the lower bound of the query item not greater than the key
                        // if they are equal make sure the start is inclusive
                        !query_item.lower_unbounded()
                            && ((lower_bound.expect("confirmed not unbounded") > key.as_slice())
                                || (start_non_inclusive
                                    && lower_bound.expect("confirmed not unbounded")
                                        == key.as_slice()))
                    } else {
                        !query_item.upper_unbounded()
                            && ((upper_bound.expect("confirmed not unbounded") < key.as_slice())
                                || (!end_inclusive
                                    && upper_bound.expect("confirmed not unbounded")
                                        == key.as_slice()))
                    };
                    if terminate {
                        break;
                    }

                    if !in_range {
                        // this is the first data we have encountered for this query item
                        if left_to_right {
                            // ensure lower bound of query item is proven
                            match last_push {
                                // lower bound is proven - we have an exact match
                                // ignoring the case when the lower bound is unbounded
                                // as it's not possible the get an exact key match for
                                // an unbounded value
                                _ if Some(key.as_slice()) == query_item.lower_bound().0 => {}

                                // lower bound is proven - this is the leftmost node
                                // in the tree
                                None => {}

                                // lower bound is proven - the preceding tree node
                                // is lower than the bound
                                Some(Node::KV(..)) => {}
                                Some(Node::KVDigest(..)) => {}
                                Some(Node::KVDigestCount(..)) => {}
                                Some(Node::KVRefValueHash(..)) => {}
                                Some(Node::KVValueHash(..)) => {}
                                Some(Node::KVValueHashFeatureType(..)) => {}
                                Some(Node::KVRefValueHashCount(..)) => {}
                                Some(Node::KVCount(..)) => {}

                                // cannot verify lower bound - we have an abridged
                                // tree, so we cannot tell what the preceding key was
                                Some(_) => {
                                    return Err(Error::InvalidProofError(
                                        "Cannot verify lower bound of queried range".to_string(),
                                    ));
                                }
                            }
                        } else {
                            // ensure upper bound of query item is proven
                            match last_push {
                                // upper bound is proven - we have an exact match
                                // ignoring the case when the upper bound is unbounded
                                // as it's not possible the get an exact key match for
                                // an unbounded value
                                _ if Some(key.as_slice()) == query_item.upper_bound().0 => {}

                                // lower bound is proven - this is the rightmost node
                                // in the tree
                                None => {}

                                // upper bound is proven - the preceding tree node
                                // is greater than the bound
                                Some(Node::KV(..)) => {}
                                Some(Node::KVDigest(..)) => {}
                                Some(Node::KVDigestCount(..)) => {}
                                Some(Node::KVRefValueHash(..)) => {}
                                Some(Node::KVValueHash(..)) => {}
                                Some(Node::KVValueHashFeatureType(..)) => {}
                                Some(Node::KVRefValueHashCount(..)) => {}
                                Some(Node::KVCount(..)) => {}

                                // cannot verify upper bound - we have an abridged
                                // tree so we cannot tell what the previous key was
                                Some(_) => {
                                    return Err(Error::InvalidProofError(
                                        "Cannot verify upper bound of queried range".to_string(),
                                    ));
                                }
                            }
                        }
                    }

                    if left_to_right {
                        if query_item.upper_bound().0.is_some()
                            && Some(key.as_slice()) >= query_item.upper_bound().0
                        {
                            // at or past upper bound of range (or this was an exact
                            // match on a single-key queryitem), advance to next query
                            // item
                            query.next();
                            in_range = false;
                        } else {
                            // have not reached upper bound, we expect more values
                            // to be proven in the range (and all pushes should be
                            // unabridged until we reach end of range)
                            in_range = true;
                        }
                    } else if query_item.lower_bound().0.is_some()
                        && Some(key.as_slice()) <= query_item.lower_bound().0
                    {
                        // at or before lower bound of range (or this was an exact
                        // match on a single-key queryitem), advance to next query
                        // item
                        query.next();
                        in_range = false;
                    } else {
                        // have not reached lower bound, we expect more values
                        // to be proven in the range (and all pushes should be
                        // unabridged until we reach end of range)
                        in_range = true;
                    }

                    // this push matches the queried item
                    if query_item.contains(key) {
                        if let Some(val) = value {
                            if let Some(limit) = current_limit {
                                if limit == 0 {
                                    return Err(Error::InvalidProofError(format!(
                                        "Proof returns more data than limit {:?}",
                                        original_limit
                                    )));
                                } else {
                                    current_limit = Some(limit - 1);
                                    if current_limit == Some(0) {
                                        in_range = false;
                                    }
                                }
                            }
                            #[cfg(feature = "proof_debug")]
                            {
                                println!(
                                    "pushing {}",
                                    ProvedKeyOptionalValue {
                                        key: key.clone(),
                                        value: Some(val.clone()),
                                        proof: value_hash,
                                        value_hash_is_computed,
                                        is_reference_result,
                                    }
                                );
                            }
                            // add data to output
                            output.push(ProvedKeyOptionalValue {
                                key: key.clone(),
                                value: Some(val.clone()),
                                proof: value_hash,
                                value_hash_is_computed,
                                is_reference_result,
                            });

                            // continue to next push
                            break;
                        } else {
                            return Err(Error::InvalidProofError(
                                "Proof is missing data for query".to_string(),
                            ));
                        }
                    }
                    {}
                    // continue to next queried item
                }
                Ok(())
            };

            match node {
                Node::KV(key, value) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KV node");
                    }
                    // value_hash computed by verifier — tamper-proof
                    execute_node(key, Some(value), value_hash(value).unwrap(), true, false)?;
                }
                Node::KVValueHash(key, value, value_hash) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVValueHash node");
                    }
                    // value_hash provided by proof — NOT independently verified.
                    // Required for subtrees where value_hash = combine_hash(H(value), subtree_root).
                    // GroveDB layer verifies the combine_hash for subtrees.
                    execute_node(key, Some(value), *value_hash, false, false)?;
                }
                Node::KVDigest(key, value_hash) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVDigest node");
                    }
                    // No value returned — absence proof
                    execute_node(key, None, *value_hash, false, false)?;
                }
                Node::KVDigestCount(key, value_hash, _count) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVDigestCount node");
                    }
                    // No value returned — absence proof in ProvableCountTree
                    execute_node(key, None, *value_hash, false, false)?;
                }
                Node::KVRefValueHash(key, value, value_hash) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVRefValueHash node");
                    }
                    // Reference: value_hash is the reference node's hash, not the
                    // referenced value's hash. Secure via combine_hash in tree.rs.
                    execute_node(key, Some(value), *value_hash, false, true)?;
                }
                Node::KVCount(key, value, _count) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVCount node");
                    }
                    // value_hash computed by verifier — tamper-proof
                    execute_node(key, Some(value), value_hash(value).unwrap(), true, false)?;
                }
                Node::KVValueHashFeatureType(key, value, value_hash, _feature_type) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVValueHashFeatureType node");
                    }
                    // value_hash provided by proof — same as KVValueHash but in
                    // ProvableCountTree context.
                    execute_node(key, Some(value), *value_hash, false, false)?;
                }
                Node::KVRefValueHashCount(key, value, value_hash, _count) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVRefValueHashCount node");
                    }
                    // Reference in ProvableCountTree context.
                    execute_node(key, Some(value), *value_hash, false, true)?;
                }
                Node::Hash(_) | Node::KVHash(_) | Node::KVHashCount(..) => {
                    if in_range {
                        return Err(Error::InvalidProofError(format!(
                            "Proof is missing data for query range. Encountered unexpected node \
                             type: {}",
                            node
                        )));
                    }
                }
            }

            last_push = Some(node.clone());

            Ok(())
        });

        let root = cost_return_on_error!(&mut cost, root_wrapped);

        // we have remaining query items, check absence proof against right edge of
        // tree
        if query.peek().is_some() {
            if current_limit == Some(0) {
            } else {
                match last_push {
                    // last node in tree was less than queried item
                    Some(Node::KV(..)) => {}
                    Some(Node::KVDigest(..)) => {}
                    Some(Node::KVDigestCount(..)) => {}
                    Some(Node::KVRefValueHash(..)) => {}
                    Some(Node::KVValueHash(..)) => {}
                    Some(Node::KVCount(..)) => {}
                    Some(Node::KVValueHashFeatureType(..)) => {}
                    Some(Node::KVRefValueHashCount(..)) => {}

                    // proof contains abridged data so we cannot verify absence of
                    // remaining query items
                    _ => {
                        return Err(Error::InvalidProofError(
                            "Proof is missing data for query".to_string(),
                        ))
                        .wrap_with_cost(cost);
                    }
                }
            }
        }

        Ok((
            root.hash().unwrap_add_cost(&mut cost),
            ProofVerificationResult {
                result_set: output,
                limit: current_limit,
            },
        ))
        .wrap_with_cost(cost)
    }

    /// Verifies the encoded proof with the given query and expected hash
    fn verify_proof(
        &self,
        bytes: &[u8],
        limit: Option<u16>,
        left_to_right: bool,
        expected_hash: MerkHash,
    ) -> CostResult<ProofVerificationResult, Error> {
        self.execute_proof(bytes, limit, left_to_right)
            .map_ok(|(root_hash, verification_result)| {
                if root_hash == expected_hash {
                    Ok(verification_result)
                } else {
                    Err(Error::InvalidProofError(format!(
                        "Proof did not match expected hash\n\tExpected: \
                         {expected_hash:?}\n\tActual: {root_hash:?}"
                    )))
                }
            })
            .flatten()
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
/// Proved key-value
pub struct ProvedKeyOptionalValue {
    /// Key
    pub key: Vec<u8>,
    /// Value
    pub value: Option<Vec<u8>>,
    /// Proof
    pub proof: CryptoHash,
    /// Whether the value_hash was independently computed by the verifier from
    /// the value bytes (true for KV, KVCount nodes) or was provided by the
    /// proof and trusted (false for KVValueHash, KVRefValueHash, etc.).
    ///
    /// When `true`, the proof field is guaranteed to equal `value_hash(value)`.
    /// When `false`, the proof field may be a combined hash (e.g., for subtrees
    /// or references) and does NOT necessarily equal `value_hash(value)`.
    pub value_hash_is_computed: bool,
    /// Whether this result came from a reference node (KVRefValueHash or
    /// KVRefValueHashCount). For reference results, the value is the
    /// dereferenced target value, and the proof hash is the reference node's
    /// value_hash — NOT `value_hash(dereferenced_value)`. The security of the
    /// dereferenced value comes from the `combine_hash` in tree.rs.
    pub is_reference_result: bool,
}

impl From<ProvedKeyValue> for ProvedKeyOptionalValue {
    fn from(value: ProvedKeyValue) -> Self {
        let ProvedKeyValue {
            key,
            value,
            proof,
            value_hash_is_computed,
            is_reference_result,
        } = value;

        ProvedKeyOptionalValue {
            key,
            value: Some(value),
            proof,
            value_hash_is_computed,
            is_reference_result,
        }
    }
}

impl TryFrom<ProvedKeyOptionalValue> for ProvedKeyValue {
    type Error = Error;

    fn try_from(value: ProvedKeyOptionalValue) -> Result<Self, Self::Error> {
        let ProvedKeyOptionalValue {
            key,
            value,
            proof,
            value_hash_is_computed,
            is_reference_result,
        } = value;
        let value = value.ok_or(Error::InvalidProofError(format!(
            "expected {}",
            hex_to_ascii(&key)
        )))?;
        Ok(ProvedKeyValue {
            key,
            value,
            proof,
            value_hash_is_computed,
            is_reference_result,
        })
    }
}

impl fmt::Display for ProvedKeyOptionalValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key_string = if self.key.len() == 1 && self.key[0] < b"0"[0] {
            hex::encode(&self.key)
        } else {
            String::from_utf8(self.key.clone()).unwrap_or_else(|_| hex::encode(&self.key))
        };
        write!(
            f,
            "ProvedKeyOptionalValue {{ key: {}, value: {}, proof: {} }}",
            key_string,
            if let Some(value) = &self.value {
                hex::encode(value)
            } else {
                "None".to_string()
            },
            hex::encode(self.proof)
        )
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
/// Proved key-value
pub struct ProvedKeyValue {
    /// Key
    pub key: Vec<u8>,
    /// Value
    pub value: Vec<u8>,
    /// Proof
    pub proof: CryptoHash,
    /// Whether the value_hash was independently computed by the verifier.
    /// See [`ProvedKeyOptionalValue::value_hash_is_computed`] for details.
    pub value_hash_is_computed: bool,
    /// Whether this result came from a reference node.
    /// See [`ProvedKeyOptionalValue::is_reference_result`] for details.
    pub is_reference_result: bool,
}

impl fmt::Display for ProvedKeyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ProvedKeyValue {{ key: {}, value: {}, proof: {} }}",
            String::from_utf8(self.key.clone()).unwrap_or_else(|_| hex::encode(&self.key)),
            hex::encode(&self.value),
            hex::encode(self.proof)
        )
    }
}

#[derive(PartialEq, Eq, Debug)]
/// Proof verification result
pub struct ProofVerificationResult {
    /// Result set
    pub result_set: Vec<ProvedKeyOptionalValue>,
    /// Limit
    pub limit: Option<u16>,
}

impl fmt::Display for ProofVerificationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ProofVerificationResult {{")?;
        writeln!(f, "  result_set: [")?;
        for (index, proved_key_value) in self.result_set.iter().enumerate() {
            writeln!(f, "    {}: {},", index, proved_key_value)?;
        }
        writeln!(f, "  ],")?;
        writeln!(f, "  limit: {:?}", self.limit)?;
        write!(f, "}}")
    }
}
