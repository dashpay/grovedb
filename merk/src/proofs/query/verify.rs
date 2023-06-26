use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};

#[cfg(feature = "full")]
use crate::proofs::query::{Map, MapBuilder};
use crate::{
    error::Error,
    proofs::{tree::execute, Decoder, Node, Op, Query},
    tree::value_hash,
    CryptoHash as MerkHash, CryptoHash,
};

#[cfg(any(feature = "full", feature = "verify"))]
pub type ProofAbsenceLimitOffset = (LinkedList<Op>, (bool, bool), Option<u16>, Option<u16>);

#[cfg(feature = "full")]
/// Verify proof against expected hash
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

#[cfg(any(feature = "full", feature = "verify"))]
/// Verifies the encoded proof with the given query
///
/// Every key in `keys` is checked to either have a key/value pair in the proof,
/// or to have its absence in the tree proven.
///
/// Returns `Err` if the proof is invalid, or a list of proven values associated
/// with `keys`. For example, if `keys` contains keys `A` and `B`, the returned
/// list will contain 2 elements, the value of `A` and the value of `B`. Keys
/// proven to be absent in the tree will have an entry of `None`, keys that have
/// a proven value will have an entry of `Some(value)`.
pub fn execute_proof(
    bytes: &[u8],
    query: &Query,
    limit: Option<u16>,
    offset: Option<u16>,
    left_to_right: bool,
) -> CostResult<(MerkHash, ProofVerificationResult), Error> {
    let mut cost = OperationCost::default();

    let mut output = Vec::with_capacity(query.len());
    let mut last_push = None;
    let mut query = query.directional_iter(left_to_right).peekable();
    let mut in_range = false;
    let mut current_limit = limit;
    let mut current_offset = offset;

    let ops = Decoder::new(bytes);

    let root_wrapped = execute(ops, true, |node| {
        let mut execute_node = |key: &Vec<u8>,
                                value: Option<&Vec<u8>>,
                                value_hash: CryptoHash|
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
                                && lower_bound.expect("confirmed not unbounded") == key.as_slice()))
                } else {
                    !query_item.upper_unbounded()
                        && ((upper_bound.expect("confirmed not unbounded") < key.as_slice())
                            || (!end_inclusive
                                && upper_bound.expect("confirmed not unbounded") == key.as_slice()))
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
                            Some(Node::KVRefValueHash(..)) => {}
                            Some(Node::KVValueHash(..)) => {}

                            // cannot verify lower bound - we have an abridged
                            // tree so we cannot tell what the preceding key was
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
                            Some(Node::KVRefValueHash(..)) => {}
                            Some(Node::KVValueHash(..)) => {}

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
                    // if there are still offset slots, and node is of type kvdigest
                    // reduce the offset counter
                    // also, verify that a kv node was not pushed before offset is exhausted
                    if let Some(offset) = current_offset {
                        if offset > 0 && value.is_none() {
                            current_offset = Some(offset - 1);
                            break;
                        } else if offset > 0 && value.is_some() {
                            // inserting a kv node before exhausting offset
                            return Err(Error::InvalidProofError(
                                "Proof returns data before offset is exhausted".to_string(),
                            ));
                        }
                    }

                    // offset is equal to zero or none
                    if let Some(val) = value {
                        if let Some(limit) = current_limit {
                            if limit == 0 {
                                return Err(Error::InvalidProofError(
                                    "Proof returns more data than limit".to_string(),
                                ));
                            } else {
                                current_limit = Some(limit - 1);
                                if current_limit == Some(0) {
                                    in_range = false;
                                }
                            }
                        }
                        // add data to output
                        output.push(ProvedKeyValue {
                            key: key.clone(),
                            value: val.clone(),
                            proof: value_hash,
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

        if let Node::KV(key, value) = node {
            execute_node(key, Some(value), value_hash(value).unwrap())?;
        } else if let Node::KVValueHash(key, value, value_hash) = node {
            execute_node(key, Some(value), *value_hash)?;
        } else if let Node::KVDigest(key, value_hash) = node {
            execute_node(key, None, *value_hash)?;
        } else if let Node::KVRefValueHash(key, value, value_hash) = node {
            execute_node(key, Some(value), *value_hash)?;
        } else if in_range {
            // we encountered a queried range but the proof was abridged (saw a
            // non-KV push), we are missing some part of the range
            return Err(Error::InvalidProofError(
                "Proof is missing data for query for range".to_string(),
            ));
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
                Some(Node::KVRefValueHash(..)) => {}
                Some(Node::KVValueHash(..)) => {}

                // proof contains abridged data so we cannot verify absence of
                // remaining query items
                _ => {
                    return Err(Error::InvalidProofError(
                        "Proof is missing data for query".to_string(),
                    ))
                    .wrap_with_cost(cost)
                }
            }
        }
    }

    Ok((
        root.hash().unwrap_add_cost(&mut cost),
        ProofVerificationResult {
            result_set: output,
            limit: current_limit,
            offset: current_offset,
        },
    ))
    .wrap_with_cost(cost)
}

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(PartialEq, Eq, Debug)]
/// Proved key-value
pub struct ProvedKeyValue {
    /// Key
    pub key: Vec<u8>,
    /// Value
    pub value: Vec<u8>,
    /// Proof
    pub proof: CryptoHash,
}

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(PartialEq, Eq, Debug)]
/// Proof verification result
pub struct ProofVerificationResult {
    /// Result set
    pub result_set: Vec<ProvedKeyValue>,
    /// Limit
    pub limit: Option<u16>,
    /// Offset
    pub offset: Option<u16>,
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Verifies the encoded proof with the given query and expected hash
pub fn verify_query(
    bytes: &[u8],
    query: &Query,
    limit: Option<u16>,
    offset: Option<u16>,
    left_to_right: bool,
    expected_hash: MerkHash,
) -> CostResult<ProofVerificationResult, Error> {
    execute_proof(bytes, query, limit, offset, left_to_right)
        .map_ok(|(root_hash, verification_result)| {
            if root_hash == expected_hash {
                Ok(verification_result)
            } else {
                Err(Error::InvalidProofError(format!(
                    "Proof did not match expected hash\n\tExpected: {expected_hash:?}\n\tActual: \
                     {root_hash:?}"
                )))
            }
        })
        .flatten()
}
