use std::fmt;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_element::ElementType;

#[cfg(feature = "minimal")]
use crate::proofs::query::{Map, MapBuilder};
use crate::{
    error::Error,
    proofs::{
        hex_to_ascii,
        tree::{execute, MAX_PROOF_OPS},
        Decoder, Node, Op, Query,
    },
    tree::{combine_hash, value_hash},
    CryptoHash as MerkHash, CryptoHash,
};

/// The latest proof version.
/// - V0 (0): lenient — permits item elements in KVValueHash nodes
///   (backwards compatibility with older proofs)
/// - V1 (1): strict — rejects item elements in KVValueHash /
///   KVValueHashFeatureType / KVValueHashFeatureTypeWithChildHash nodes
///   to prevent KV-to-KVValueHash proof forgery
pub const PROOF_VERSION_LATEST: u16 = 1;

/// Verify proof against expected hash
#[cfg(feature = "minimal")]
#[deprecated]
#[allow(unused)]
pub fn verify(bytes: &[u8], expected_hash: MerkHash) -> CostResult<Map, Error> {
    let mut decoder = Decoder::new(bytes);
    let mut map_builder = MapBuilder::new();

    execute(decoder.by_ref(), true, |node| map_builder.insert(node)).flat_map_ok(|root| {
        if decoder.remaining_bytes() > 0 {
            return Err(Error::InvalidProofError(format!(
                "Proof has {} unconsumed trailing bytes",
                decoder.remaining_bytes()
            )))
            .wrap_with_cost(Default::default());
        }

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

/// How a verifier interprets the `limit` it is given.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProofLimitMode {
    /// The prover used exactly this limit. The proof may hide the rest of
    /// the queried range only once `limit` results have been returned; a
    /// hidden node inside the range before that is an error.
    Exact,
    /// The prover used this limit or a smaller one, so the verifier does
    /// not need to know it. Once the proof has returned at least one result,
    /// the walk stops at the first point the proof leaves unrevealed: a
    /// hidden node inside a queried range, a boundary key reached across
    /// hidden nodes, or the end of the proof with query items unproven. No
    /// result may follow, so the results are a gap-free prefix of the query,
    /// and [`ProofVerificationResult::exhausted`] is false. A proof that
    /// hides the start of the query is still rejected, so a page is never
    /// empty unless nothing matches or `limit` is `Some(0)`.
    ///
    /// Requires proof version 1 or later.
    UpperBound,
}

/// Extension trait adding proof verification methods to `Query`.
///
/// These methods depend on merk-internal types (Node, Op, Decoder, etc.)
/// and therefore cannot live in the `grovedb-query` crate.
pub trait QueryProofVerify {
    /// Verifies the encoded proof with the given query, returning the root
    /// hash and verification result.
    ///
    /// `proof_version` controls which security checks are applied:
    /// - V0 (0): lenient — permits item elements in KVValueHash nodes
    ///   (backwards compatibility with older proofs)
    /// - V1+ (≥1): strict — rejects item elements in KVValueHash /
    ///   KVValueHashFeatureType / KVValueHashFeatureTypeWithChildHash nodes
    ///   to prevent KV-to-KVValueHash proof forgery
    fn execute_proof(
        &self,
        bytes: &[u8],
        limit: Option<u16>,
        left_to_right: bool,
        proof_version: u16,
    ) -> CostResult<(MerkHash, ProofVerificationResult), Error>;

    /// [`QueryProofVerify::execute_proof`] with an explicit
    /// [`ProofLimitMode`]. `execute_proof` is this with
    /// [`ProofLimitMode::Exact`].
    fn execute_proof_with_limit_mode(
        &self,
        bytes: &[u8],
        limit: Option<u16>,
        limit_mode: ProofLimitMode,
        left_to_right: bool,
        proof_version: u16,
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
        proof_version: u16,
    ) -> CostResult<(MerkHash, ProofVerificationResult), Error> {
        self.execute_proof_with_limit_mode(
            bytes,
            limit,
            ProofLimitMode::Exact,
            left_to_right,
            proof_version,
        )
    }

    fn execute_proof_with_limit_mode(
        &self,
        bytes: &[u8],
        limit: Option<u16>,
        limit_mode: ProofLimitMode,
        left_to_right: bool,
        proof_version: u16,
    ) -> CostResult<(MerkHash, ProofVerificationResult), Error> {
        if limit_mode == ProofLimitMode::UpperBound && proof_version < 1 {
            return Err(Error::InvalidProofError(
                "upper-bound limit verification requires proof version 1 or later".to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }
        #[cfg(feature = "proof_debug")]
        {
            println!(
                "executing proof with {:?} limit {:?} going {} using query {}",
                limit_mode,
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
        // Upper-bound mode only: set where the proof stops revealing the
        // query, which is where the prover stopped walking.
        let mut stopped_early = false;

        let mut decoder = Decoder::new(bytes);

        // Issue #863: the stream must be encoded in the op family of the
        // direction it is walked in, and in that family only.
        //
        // `execute` checks per op that upright pushes ascend and inverted
        // pushes descend, but it never ties the family to `left_to_right`
        // and it lets the two families mix. Everything below that turns a
        // visited node into a bound witness — "the previous push was
        // key-bearing, so nothing lies between it and this key", "this is
        // the first push, so it is the leftmost (rightmost) node", "the
        // limit is met, so the abridged tail is fine" — assumes the visit
        // order is the tree's in-order for an ascending walk and its exact
        // reverse for a descending one. That holds only for a homogeneous
        // stream in the walk's own family: an upright stream walked
        // descending reads the smallest revealed key as the rightmost
        // node, and a mixed stream can rebuild the honest tree while
        // visiting an abridged root *after* both of its children, so two
        // revealed leaves look adjacent. Either way an authentic root hash
        // carries a wrong absence or a page filled from the wrong end.
        //
        // Enforced for V1 proofs only: V0 is a locked wire format whose
        // verifier is not to change, and it is gated at `proof_version`
        // like the other V1-only strictness checks in this function.
        let oriented_ops = decoder.by_ref().map(|op_result| {
            let op = op_result?;
            if proof_version >= 1 && op_is_upright(&op) != left_to_right {
                return Err(Error::InvalidProofError(format!(
                    "Proof op family does not match the query direction: {} op in a {} walk; a \
                     layer proof is emitted entirely in the family of its own direction",
                    if op_is_upright(&op) {
                        "upright"
                    } else {
                        "inverted"
                    },
                    if left_to_right {
                        "left-to-right"
                    } else {
                        "right-to-left"
                    },
                )));
            }
            Ok(op)
        });

        let root_wrapped = execute(oriented_ops, true, |node| {
            let mut execute_node = |key: &Vec<u8>,
                                    value: Option<&Vec<u8>>,
                                    value_hash: CryptoHash,
                                    child_hash_verified: bool,
                                    plain_trusted_value: bool|
             -> Result<_, Error> {
                // Upper-bound mode: once the walk has stopped, the rest of the
                // proof is structure the prover still had to include (ancestors
                // and boundary keys). A node carrying a value there would be a
                // result after a gap the verifier cannot see.
                if stopped_early {
                    if value.is_some() {
                        return Err(Error::InvalidProofError(
                            "Proof returns data after the walk stopped".to_string(),
                        ));
                    }
                    return Ok(());
                }
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
                                Some(Node::KVDigestSum(..)) => {}
                                Some(Node::KVRefValueHash(..)) => {}
                                Some(Node::KVValueHash(..)) => {}
                                Some(Node::KVBackwardsReferencesValueHash(..)) => {}
                                Some(Node::KVValueHashFeatureType(..)) => {}
                                Some(Node::KVValueHashFeatureTypeWithChildHash(..)) => {}
                                Some(Node::KVRefValueHashCount(..)) => {}
                                Some(Node::KVRefValueHashSum(..)) => {}
                                Some(Node::KVCount(..)) => {}
                                Some(Node::KVSum(..)) => {}
                                // ProvableCountProvableSumTree (dual-axis)
                                // key-bearing nodes are also acceptable
                                // bound-proving boundaries.
                                Some(Node::KVCountSum(..)) => {}
                                Some(Node::KVDigestCountSum(..)) => {}
                                Some(Node::KVRefValueHashCountSum(..)) => {}

                                // cannot verify lower bound - we have an abridged
                                // tree, so we cannot tell what the preceding key was
                                Some(_) => {
                                    // Upper-bound mode: a boundary key after hidden
                                    // nodes, once the walk has results, is where the
                                    // prover stopped; end the walk there.
                                    if limit_mode == ProofLimitMode::UpperBound
                                        && value.is_none()
                                        && !output.is_empty()
                                    {
                                        stopped_early = true;
                                        return Ok(());
                                    }
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
                                Some(Node::KVDigestSum(..)) => {}
                                Some(Node::KVRefValueHash(..)) => {}
                                Some(Node::KVValueHash(..)) => {}
                                Some(Node::KVBackwardsReferencesValueHash(..)) => {}
                                Some(Node::KVValueHashFeatureType(..)) => {}
                                Some(Node::KVValueHashFeatureTypeWithChildHash(..)) => {}
                                Some(Node::KVRefValueHashCount(..)) => {}
                                Some(Node::KVRefValueHashSum(..)) => {}
                                Some(Node::KVCount(..)) => {}
                                Some(Node::KVSum(..)) => {}
                                // ProvableCountProvableSumTree (dual-axis)
                                // key-bearing nodes are also acceptable
                                // upper-bound-proving boundaries.
                                Some(Node::KVCountSum(..)) => {}
                                Some(Node::KVDigestCountSum(..)) => {}
                                Some(Node::KVRefValueHashCountSum(..)) => {}

                                // cannot verify upper bound - we have an abridged
                                // tree so we cannot tell what the previous key was
                                Some(_) => {
                                    // Upper-bound mode: as for the lower bound.
                                    if limit_mode == ProofLimitMode::UpperBound
                                        && value.is_none()
                                        && !output.is_empty()
                                    {
                                        stopped_early = true;
                                        return Ok(());
                                    }
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
                            // Terminal downgrade guard (V1 strict): the V4
                            // prover rewrites every bidirectional-reference
                            // node — result or filler — into a
                            // KVRefValueHash* node whose target bytes are
                            // bound by recomputation. One arriving as a plain
                            // trusted-value result is therefore a
                            // downgraded/forged node whose bytes ride unbound
                            // on the carried hash. (Plain references can
                            // legitimately appear raw in mixed-level V1
                            // proofs and keep their long-standing handling.)
                            if plain_trusted_value
                                && proof_version >= 1
                                && matches!(
                                    ElementType::from_serialized_value(val).map(|et| et.base()),
                                    Ok(ElementType::BidirectionalReference)
                                )
                            {
                                return Err(Error::InvalidProofError(
                                    "bidirectional-reference elements must be dereferenced \
                                     into KVRefValueHash-family nodes in proof results"
                                        .to_string(),
                                ));
                            }
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
                                        child_hash_verified,
                                    }
                                );
                            }
                            // add data to output
                            output.push(ProvedKeyOptionalValue {
                                key: key.clone(),
                                value: Some(val.clone()),
                                proof: value_hash,
                                child_hash_verified,
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
                    execute_node(key, Some(value), value_hash(value).unwrap(), false, false)?;
                }
                Node::KVValueHash(key, value, value_hash) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVValueHash node");
                    }
                    // KVValueHash exists for elements whose value_hash is a
                    // combine_hash (subtrees and references). Reject item
                    // elements to prevent KV→KVValueHash forgery where an
                    // attacker substitutes a KV node with KVValueHash to inject
                    // a fake value while keeping the original hash.
                    // Skipped for V0 backwards compatibility.
                    //
                    // Reference elements deliberately PASS here even though
                    // this node hashes only (key, value_hash) and so binds
                    // none of their bytes. A reference row that lies past
                    // the query limit legitimately stays a bare KVValueHash
                    // in released V1 proofs — the GroveDB post-pass only
                    // rewrites rows within the limit into KVRefValueHash* —
                    // so refusing references at this level would reject
                    // honest proofs. The binding contract for reference
                    // bytes is enforced by the only consumer of these rows:
                    // `verify_layer_proof_v1` rejects any raw reference row
                    // it consumes (issue #862).
                    if proof_version >= 1 {
                        let element_type =
                            ElementType::from_serialized_value(value).map_err(|e| {
                                Error::InvalidProofError(format!(
                                    "cannot determine element type in KVValueHash node: {e}"
                                ))
                            })?;
                        if element_type.has_simple_value_hash() {
                            return Err(Error::InvalidProofError(
                                "KVValueHash node must not contain an item element".to_string(),
                            ));
                        }
                        // Backward-references elements must come through
                        // KVBackwardsReferencesValueHash, whose combined
                        // hash is RECOMPUTED — as a KVValueHash the value
                        // bytes would ride unbound on the carried hash.
                        if matches!(
                            element_type.base(),
                            ElementType::ItemWithBackwardsReferences
                                | ElementType::SumItemWithBackwardsReferences
                                | ElementType::ItemWithSumItemWithBackwardsReferences
                        ) {
                            return Err(Error::InvalidProofError(
                                "KVValueHash node must not contain a backward-references \
                                 element; use KVBackwardsReferencesValueHash"
                                    .to_string(),
                            ));
                        }
                    }
                    execute_node(key, Some(value), *value_hash, false, true)?;
                }
                Node::KVDigest(key, value_hash) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVDigest node");
                    }
                    execute_node(key, None, *value_hash, false, false)?;
                }
                Node::KVDigestCount(key, value_hash, _count) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVDigestCount node");
                    }
                    execute_node(key, None, *value_hash, false, false)?;
                }
                Node::KVRefValueHash(key, value, value_hash) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVRefValueHash node");
                    }
                    execute_node(key, Some(value), *value_hash, false, false)?;
                }
                Node::KVBackwardsReferencesValueHash(key, value, backrefs_hash) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVBackwardsReferencesValueHash node");
                    }
                    // The node kind was introduced with GROVE_V4 / V1
                    // envelopes; a V0 proof carrying it would be accepted
                    // here but rejected by every released verifier.
                    if proof_version == 0 {
                        return Err(Error::InvalidProofError(
                            "KVBackwardsReferencesValueHash nodes are not allowed in V0 proofs"
                                .to_string(),
                        ));
                    }
                    // The node's combined hash is recomputed from the
                    // stripped payload bytes it carries, so the bytes are
                    // bound; the result set receives the stripped element.
                    // The row is reported as hash-bound (`combine_hash(H(value),
                    // backrefs_hash) == value_hash` was checked end to end),
                    // the same evidence a child-hash node yields — readers that
                    // classify rows from their bytes may trust these bytes.
                    let combined = value_hash(value)
                        .unwrap()
                        .wrap_with_cost(Default::default())
                        .flat_map(|inner| crate::tree::hash::combine_hash(&inner, backrefs_hash))
                        .unwrap();
                    execute_node(key, Some(value), combined, true, false)?;
                }
                Node::KVCount(key, value, _count) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVCount node");
                    }
                    execute_node(key, Some(value), value_hash(value).unwrap(), false, false)?;
                }
                Node::KVValueHashFeatureType(key, value, value_hash, _feature_type) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVValueHashFeatureType node");
                    }
                    // Same check as KVValueHash — reject item elements.
                    // References pass here for the same reason as on
                    // KVValueHash (beyond-limit reference rows stay bare in
                    // released proofs); `verify_layer_proof_v1` refuses any
                    // raw reference row it consumes.
                    // Skipped for V0 backwards compatibility.
                    if proof_version >= 1 {
                        let element_type =
                            ElementType::from_serialized_value(value).map_err(|e| {
                                Error::InvalidProofError(format!(
                                    "cannot determine element type in KVValueHashFeatureType \
                                     node: {e}"
                                ))
                            })?;
                        if element_type.has_simple_value_hash() {
                            return Err(Error::InvalidProofError(
                                "KVValueHashFeatureType node must not contain an item element"
                                    .to_string(),
                            ));
                        }
                        // Same rationale as the KVValueHash guard above.
                        if matches!(
                            element_type.base(),
                            ElementType::ItemWithBackwardsReferences
                                | ElementType::SumItemWithBackwardsReferences
                                | ElementType::ItemWithSumItemWithBackwardsReferences
                        ) {
                            return Err(Error::InvalidProofError(
                                "KVValueHashFeatureType node must not contain a \
                                 backward-references element"
                                    .to_string(),
                            ));
                        }
                    }
                    execute_node(key, Some(value), *value_hash, false, true)?;
                }
                Node::KVRefValueHashCount(key, value, value_hash, _count) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVRefValueHashCount node");
                    }
                    execute_node(key, Some(value), *value_hash, false, false)?;
                }
                Node::KVValueHashFeatureTypeWithChildHash(
                    key,
                    value,
                    node_value_hash,
                    _feature_type,
                    child_hash,
                ) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVValueHashFeatureTypeWithChildHash node");
                    }
                    // Same element-type check as KVValueHashFeatureType.
                    // Skipped for V0 backwards compatibility.
                    if proof_version >= 1 {
                        let element_type =
                            ElementType::from_serialized_value(value).map_err(|e| {
                                Error::InvalidProofError(format!(
                                    "cannot determine element type in \
                                     KVValueHashFeatureTypeWithChildHash node: {e}"
                                ))
                            })?;
                        if element_type.has_simple_value_hash() {
                            return Err(Error::InvalidProofError(
                                "KVValueHashFeatureTypeWithChildHash node must not contain \
                                 an item element"
                                    .to_string(),
                            ));
                        }
                    }
                    // Verify value integrity: combine_hash(H(value), child_hash) must
                    // equal the provided value_hash. This prevents an attacker from
                    // swapping the serialized element bytes (e.g. changing a CountTree's
                    // count) while reusing the original value_hash.
                    let element_value_hash = value_hash(value).unwrap();
                    let computed_value_hash =
                        combine_hash(&element_value_hash, child_hash).unwrap();
                    if computed_value_hash != *node_value_hash {
                        return Err(Error::InvalidProofError(format!(
                            "value/child hash mismatch: combine_hash(H(value), child_hash) \
                             = {} but value_hash = {}",
                            hex::encode(computed_value_hash),
                            hex::encode(node_value_hash)
                        )));
                    }
                    execute_node(key, Some(value), *node_value_hash, true, false)?;
                }
                Node::Hash(_)
                | Node::KVHash(_)
                | Node::KVHashCount(..)
                | Node::KVHashSum(..)
                | Node::KVHashCountSum(..) => {
                    if in_range {
                        // `in_range` is only set by a node the current item
                        // contains, which is a result, so `output` is never
                        // empty here. The check keeps the "no stop before
                        // the first result" rule explicit, as at the other
                        // two stopping points.
                        if limit_mode == ProofLimitMode::UpperBound && !output.is_empty() {
                            // The prover stopped here: it hides everything
                            // after its last result once its own limit runs
                            // out. Everything from this node on is
                            // unrevealed, so the walk ends with the results
                            // so far.
                            in_range = false;
                            stopped_early = true;
                        } else {
                            return Err(Error::InvalidProofError(format!(
                                "Proof is missing data for query range. Encountered unexpected \
                                 node type: {}",
                                node
                            )));
                        }
                    }
                }
                Node::HashWithCount(..) => {
                    // `HashWithCount` is only safe inside the dedicated
                    // aggregate-count verifier, which shape-checks each
                    // collapsed subtree against the queried range. The plain
                    // query verifier does no such shape check, and
                    // `Tree::hash()` for a `HashWithCount` recomputes its
                    // hash from the embedded `(kv_hash, l, r, count)` while
                    // *ignoring* any reconstructed children. A malicious
                    // prover could therefore hang fake KV pushes under a
                    // `HashWithCount`, satisfy `execute_node` from those
                    // pushes (so they appear as query results) while still
                    // preserving the parent's hash chain. Fail fast here so
                    // the regular query path can never accept one.
                    return Err(Error::InvalidProofError(
                        "HashWithCount node is only valid in aggregate-count proofs; \
                         encountered in regular query verification"
                            .to_string(),
                    ));
                }
                Node::HashWithSum(..) => {
                    // Same fail-fast rationale as `HashWithCount` above.
                    // `HashWithSum` is reserved for the dedicated
                    // aggregate-sum verifier; it must never reach the
                    // regular query verifier.
                    return Err(Error::InvalidProofError(
                        "HashWithSum node is only valid in aggregate-sum proofs; \
                         encountered in regular query verification"
                            .to_string(),
                    ));
                }
                Node::HashWithCountAndSum(..) => {
                    // Same fail-fast rationale as `HashWithCount` /
                    // `HashWithSum`. The combined variant is reserved for
                    // the dedicated aggregate-count and aggregate-sum
                    // verifiers against `ProvableCountProvableSumTree`;
                    // it must never reach the regular query verifier.
                    return Err(Error::InvalidProofError(
                        "HashWithCountAndSum node is only valid in aggregate-count / \
                         aggregate-sum proofs; encountered in regular query verification"
                            .to_string(),
                    ));
                }
                Node::KVSum(key, value, _sum) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVSum node");
                    }
                    execute_node(key, Some(value), value_hash(value).unwrap(), false, false)?;
                }
                Node::KVDigestSum(key, value_hash, _sum) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVDigestSum node");
                    }
                    execute_node(key, None, *value_hash, false, false)?;
                }
                Node::KVRefValueHashSum(key, value, value_hash, _sum) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVRefValueHashSum node");
                    }
                    execute_node(key, Some(value), *value_hash, false, false)?;
                }
                Node::KVCountSum(key, value, _count, _sum) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVCountSum node");
                    }
                    execute_node(key, Some(value), value_hash(value).unwrap(), false, false)?;
                }
                Node::KVDigestCountSum(key, value_hash, _count, _sum) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVDigestCountSum node");
                    }
                    execute_node(key, None, *value_hash, false, false)?;
                }
                Node::KVRefValueHashCountSum(key, value, value_hash, _count, _sum) => {
                    #[cfg(feature = "proof_debug")]
                    {
                        println!("Processing KVRefValueHashCountSum node");
                    }
                    execute_node(key, Some(value), *value_hash, false, false)?;
                }
            }

            last_push = Some(node.clone());

            Ok(())
        });

        let root = cost_return_on_error!(&mut cost, root_wrapped);

        if decoder.remaining_bytes() > 0 {
            return Err(Error::InvalidProofError(format!(
                "Proof has {} unconsumed trailing bytes",
                decoder.remaining_bytes()
            )))
            .wrap_with_cost(cost);
        }

        // we have remaining query items, check absence proof against right edge of
        // tree
        if query.peek().is_some() {
            if current_limit == Some(0) || stopped_early {
            } else {
                match last_push {
                    // last node in tree was less than queried item
                    Some(Node::KV(..)) => {}
                    Some(Node::KVDigest(..)) => {}
                    Some(Node::KVDigestCount(..)) => {}
                    Some(Node::KVRefValueHash(..)) => {}
                    Some(Node::KVValueHash(..)) => {}
                    Some(Node::KVBackwardsReferencesValueHash(..)) => {}
                    Some(Node::KVCount(..)) => {}
                    Some(Node::KVValueHashFeatureType(..)) => {}
                    Some(Node::KVValueHashFeatureTypeWithChildHash(..)) => {}
                    Some(Node::KVRefValueHashCount(..)) => {}
                    // ProvableSumTree key-bearing nodes are also acceptable
                    // absence-proof boundaries.
                    Some(Node::KVSum(..)) => {}
                    Some(Node::KVDigestSum(..)) => {}
                    Some(Node::KVRefValueHashSum(..)) => {}
                    // ProvableCountProvableSumTree (dual-axis) key-bearing
                    // nodes are also acceptable absence-proof boundaries.
                    Some(Node::KVCountSum(..)) => {}
                    Some(Node::KVDigestCountSum(..)) => {}
                    Some(Node::KVRefValueHashCountSum(..)) => {}

                    // Upper-bound mode: the prover stopped after the results so
                    // far and hid the rest; the remaining items are unproven.
                    _ if limit_mode == ProofLimitMode::UpperBound && !output.is_empty() => {
                        stopped_early = true;
                    }

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

        // Whether the proof shows nothing further matches: true unless the walk
        // stopped (at the limit, or early in upper-bound mode) with query items
        // left. When items were left and it is true, the absence check above
        // proved them.
        let exhausted = !stopped_early && (query.peek().is_none() || current_limit != Some(0));

        Ok((
            root.hash().unwrap_add_cost(&mut cost),
            ProofVerificationResult {
                result_set: output,
                limit: current_limit,
                exhausted,
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
        self.execute_proof(bytes, limit, left_to_right, PROOF_VERSION_LATEST)
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
    /// Whether the merk verifier confirmed `combine_hash(H(value), other)
    /// == value_hash` for this element, binding the presented value bytes
    /// through a recomputed combined hash. True for
    /// `KVValueHashFeatureTypeWithChildHash` nodes (`other` = the carried
    /// child hash) and for `KVBackwardsReferencesValueHash` nodes (`other`
    /// = the referrer-list hash, recomputed into the merk root itself).
    pub child_hash_verified: bool,
}

impl From<ProvedKeyValue> for ProvedKeyOptionalValue {
    fn from(value: ProvedKeyValue) -> Self {
        let ProvedKeyValue { key, value, proof } = value;

        ProvedKeyOptionalValue {
            key,
            value: Some(value),
            proof,
            child_hash_verified: false,
        }
    }
}

impl TryFrom<ProvedKeyOptionalValue> for ProvedKeyValue {
    type Error = Error;

    fn try_from(value: ProvedKeyOptionalValue) -> Result<Self, Self::Error> {
        let ProvedKeyOptionalValue {
            key, value, proof, ..
        } = value;
        let value = value.ok_or(Error::InvalidProofError(format!(
            "expected {}",
            hex_to_ascii(&key)
        )))?;
        Ok(ProvedKeyValue { key, value, proof })
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
            "ProvedKeyOptionalValue {{ key: {}, value: {}, proof: {}, child_hash_verified: {} }}",
            key_string,
            if let Some(value) = &self.value {
                hex::encode(value)
            } else {
                "None".to_string()
            },
            hex::encode(self.proof),
            self.child_hash_verified
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
    /// Whether the proof shows there is nothing more to return: every
    /// query item was walked to its end. False when the walk stopped with
    /// query items left, because the limit ran out or, in
    /// [`ProofLimitMode::UpperBound`] mode, because the proof stopped
    /// revealing the query; later results may then exist. This covers this
    /// merk layer's query only, not a whole GroveDB path query with
    /// subqueries.
    pub exhausted: bool,
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
        writeln!(f, "  exhausted: {}", self.exhausted)?;
        write!(f, "}}")
    }
}

/// Checks whether a key exists as a boundary element in the given merk proof
/// bytes. A boundary element is a `KVDigest`, `KVDigestCount`,
/// `KVDigestSum`, or `KVDigestCountSum` (dual-axis PCPS) node — it proves
/// the key exists in the tree without revealing the value. (Same
/// node-type coverage as [`boundaries_in_proof`]; the two helpers must
/// agree.)
///
/// This is useful for exclusive range queries (e.g. `RangeAfter(10)`) where
/// the boundary key (10) is included in the proof as a digest node to anchor
/// the range, but is not part of the result set.
pub fn key_exists_as_boundary_in_proof(proof_bytes: &[u8], key: &[u8]) -> Result<bool, Error> {
    let decoder = Decoder::new(proof_bytes);
    for op_result in decoder {
        let op = op_result?;
        match &op {
            Op::Push(Node::KVDigest(k, _))
            | Op::PushInverted(Node::KVDigest(k, _))
            | Op::Push(Node::KVDigestCount(k, _, _))
            | Op::PushInverted(Node::KVDigestCount(k, _, _))
            | Op::Push(Node::KVDigestSum(k, _, _))
            | Op::PushInverted(Node::KVDigestSum(k, _, _))
            | Op::Push(Node::KVDigestCountSum(k, _, _, _))
            | Op::PushInverted(Node::KVDigestCountSum(k, _, _, _))
                if k.as_slice() == key =>
            {
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}

#[cfg(test)]
mod provable_sum_tree_bound_regression_tests {
    //! Regression coverage for a Codex-flagged bug: the lower- and
    //! upper-bound `last_push` checks in `execute_proof` accepted
    //! `KVCount` / `KVDigestCount` / `KVRefValueHashCount` (the Count
    //! family) but omitted the parallel `KVSum` / `KVDigestSum` /
    //! `KVRefValueHashSum` variants, so a multi-item query like
    //! `Key(...)` + `Range(...)` against a `ProvableSumTree` would
    //! reject a perfectly valid proof with
    //! `Cannot verify lower bound of queried range` whenever the
    //! preceding boundary happened to be a `KVDigestSum` node.
    //!
    //! These tests build a populated `ProvableSumTree`, prove a
    //! `Key("aa") + Range("g".."j")` query in both directions, and
    //! verify the resulting proof. Without the fix in
    //! `merk/src/proofs/query/verify.rs::execute_proof` these
    //! verifications return `InvalidProofError`.

    use grovedb_version::version::GroveVersion;

    use crate::{
        proofs::{
            query::{
                verify::{QueryProofVerify, PROOF_VERSION_LATEST},
                QueryItem,
            },
            Query,
        },
        test_utils::TempMerk,
        tree::Op,
        TreeFeatureType::ProvableSummedMerkNode,
        TreeType,
    };

    /// Build a `ProvableSumTree` populated with single-byte keys
    /// "a", "b", ..., "o" (15 keys), each carrying sum `i+1`.
    fn make_15_key_provable_sum_tree(grove_version: &GroveVersion) -> TempMerk {
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableSumTree);
        let entries: Vec<(Vec<u8>, Op)> = (b'a'..=b'o')
            .enumerate()
            .map(|(i, c)| {
                let s = (i as i64) + 1;
                (vec![c], Op::Put(vec![i as u8], ProvableSummedMerkNode(s)))
            })
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
            .unwrap()
            .expect("apply should succeed");
        merk.commit(grove_version);
        merk
    }

    /// Helper: prove a `[Key("aa"), Range("g".."j")]` query in a given
    /// direction and verify the resulting proof. With the fix in place
    /// this must succeed. The query mixes an absence boundary
    /// (`Key("aa")` — between "a" and "b") with a range, which is the
    /// shape that surfaces the `KVDigestSum`-as-prior-boundary case.
    fn run_multi_item_query_verifies(left_to_right: bool, grove_version: &GroveVersion) {
        let merk = make_15_key_provable_sum_tree(grove_version);
        let mut query = Query::new();
        // Absent key — proves absence via a `KVDigest`-family boundary.
        query.insert_item(QueryItem::Key(b"aa".to_vec()));
        // Range that doesn't touch "aa". The verifier must accept the
        // sequence regardless of which boundary node preceded it.
        query.insert_item(QueryItem::Range(b"g".to_vec()..b"j".to_vec()));
        query.left_to_right = left_to_right;

        let proof = merk
            .prove(query.clone(), None, grove_version)
            .unwrap()
            .expect("prove should succeed");

        let (_root_hash, _result) = query
            .execute_proof(&proof.proof, None, left_to_right, PROOF_VERSION_LATEST)
            .unwrap()
            .expect(
                "Key+Range verify on ProvableSumTree must succeed; failure here means the \
                 KVDigestSum boundary still isn't accepted by the bound checks",
            );
    }

    #[test]
    fn key_plus_range_on_provable_sum_tree_left_to_right_verifies() {
        let v = GroveVersion::latest();
        run_multi_item_query_verifies(true, v);
    }

    #[test]
    fn key_plus_range_on_provable_sum_tree_right_to_left_verifies() {
        let v = GroveVersion::latest();
        run_multi_item_query_verifies(false, v);
    }

    /// Boundary-extraction parallel: `KVDigestSum` produced by a
    /// `ProvableSumTree` proof must surface in `boundaries_in_proof`
    /// just like its `KVDigest` / `KVDigestCount` siblings, AND
    /// `key_exists_as_boundary_in_proof` must agree (the two helpers
    /// are documented to behave identically).
    #[test]
    fn kv_digest_sum_appears_in_both_boundary_helpers() {
        use crate::proofs::query::verify::{boundaries_in_proof, key_exists_as_boundary_in_proof};

        let v = GroveVersion::latest();
        let merk = make_15_key_provable_sum_tree(v);
        // Querying an absent key emits a `KVDigestSum` boundary.
        let mut query = Query::new();
        query.insert_item(QueryItem::Key(b"aa".to_vec()));

        let proof = merk
            .prove(query, None, v)
            .unwrap()
            .expect("prove should succeed");

        let boundaries = boundaries_in_proof(&proof.proof).expect("boundaries");
        assert!(
            !boundaries.is_empty(),
            "boundaries_in_proof must report KVDigestSum nodes from ProvableSumTree proofs"
        );

        // Every boundary key surfaced by `boundaries_in_proof` must
        // round-trip through `key_exists_as_boundary_in_proof` as well —
        // the two helpers must agree on the same node-type coverage.
        for boundary in &boundaries {
            let found = key_exists_as_boundary_in_proof(&proof.proof, boundary)
                .expect("key_exists_as_boundary_in_proof");
            assert!(
                found,
                "key_exists_as_boundary_in_proof disagreed with boundaries_in_proof on {:?}",
                boundary
            );
        }
    }
}

#[cfg(test)]
mod provable_count_provable_sum_tree_bound_regression_tests {
    //! Dual-axis parallel of `provable_sum_tree_bound_regression_tests`.
    //!
    //! The `execute_proof` lower/upper-bound `last_push` matches, the
    //! absence-proof last-push match, and the `boundaries_in_proof` +
    //! `key_exists_as_boundary_in_proof` helpers all gained
    //! dual-axis Node variant arms (`KVCountSum`, `KVDigestCountSum`,
    //! `KVRefValueHashCountSum`). Without those arms a multi-item
    //! query like `Key(...)` + `Range(...)` against a
    //! `ProvableCountProvableSumTree` would reject a valid proof with
    //! "Cannot verify lower bound of queried range" whenever the
    //! preceding boundary happened to be a `KVDigestCountSum`. These
    //! tests exercise exactly that shape.
    //!
    //! Together with the parallel sum-only tests above, this pins the
    //! verifier's dual-axis coverage end-to-end (prove → verify
    //! round-trip on a PCPS merk).

    use grovedb_version::version::GroveVersion;

    use crate::{
        proofs::{
            query::{
                verify::{
                    boundaries_in_proof, key_exists_as_boundary_in_proof, QueryProofVerify,
                    PROOF_VERSION_LATEST,
                },
                QueryItem,
            },
            Query,
        },
        test_utils::TempMerk,
        tree::Op,
        TreeFeatureType::ProvableCountedAndProvableSummedMerkNode,
        TreeType,
    };

    /// Build a `ProvableCountProvableSumTree` populated with single-byte
    /// keys "a", "b", ..., "o" (15 keys), each carrying
    /// `(count=1, sum=i+1)`.
    fn make_15_key_pcps(grove_version: &GroveVersion) -> TempMerk {
        let mut merk =
            TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountProvableSumTree);
        let entries: Vec<(Vec<u8>, Op)> = (b'a'..=b'o')
            .enumerate()
            .map(|(i, c)| {
                let s = (i as i64) + 1;
                (
                    vec![c],
                    Op::Put(
                        vec![i as u8],
                        ProvableCountedAndProvableSummedMerkNode(1, s),
                    ),
                )
            })
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
            .unwrap()
            .expect("apply should succeed");
        merk.commit(grove_version);
        merk
    }

    fn run_pcps_multi_item_query_verifies(left_to_right: bool, grove_version: &GroveVersion) {
        let merk = make_15_key_pcps(grove_version);
        let mut query = Query::new();
        // Absent key between "a" and "b" — proves absence via a
        // `KVDigestCountSum` boundary.
        query.insert_item(QueryItem::Key(b"aa".to_vec()));
        // Range that doesn't touch "aa". The verifier must accept the
        // sequence regardless of which boundary node preceded it.
        query.insert_item(QueryItem::Range(b"g".to_vec()..b"j".to_vec()));
        query.left_to_right = left_to_right;

        let proof = merk
            .prove(query.clone(), None, grove_version)
            .unwrap()
            .expect("prove should succeed");

        let (_root_hash, _result) = query
            .execute_proof(&proof.proof, None, left_to_right, PROOF_VERSION_LATEST)
            .unwrap()
            .expect(
                "Key+Range verify on PCPS must succeed; failure here means the \
                 KVDigestCountSum boundary still isn't accepted by the bound checks",
            );
    }

    #[test]
    fn key_plus_range_on_pcps_left_to_right_verifies() {
        let v = GroveVersion::latest();
        run_pcps_multi_item_query_verifies(true, v);
    }

    #[test]
    fn key_plus_range_on_pcps_right_to_left_verifies() {
        let v = GroveVersion::latest();
        run_pcps_multi_item_query_verifies(false, v);
    }

    /// A regular range query against a PCPS that includes every key in
    /// the tree — exercises every dual-axis Node variant that the
    /// verifier's `execute_node` callback dispatches on (KVCountSum
    /// for queried Items, KVHashCountSum for path nodes,
    /// KVDigestCountSum for boundary nodes). Without the dual-axis
    /// arms in `execute_proof`'s match the proof would fail to verify.
    #[test]
    fn full_range_round_trips_through_dual_axis_verify_arms() {
        let v = GroveVersion::latest();
        let merk = make_15_key_pcps(v);
        let query =
            Query::new_single_query_item(QueryItem::RangeInclusive(b"a".to_vec()..=b"o".to_vec()));
        let proof = merk
            .prove(query.clone(), None, v)
            .unwrap()
            .expect("prove succeeds");

        let (root, result) = query
            .execute_proof(&proof.proof, None, true, PROOF_VERSION_LATEST)
            .unwrap()
            .expect("verify succeeds — dual-axis nodes must all be processed");

        // Sanity: root matches the merk's root, and we got all 15 keys.
        assert_eq!(root, merk.root_hash().unwrap());
        assert_eq!(result.result_set.len(), 15);
    }

    /// `KVDigestCountSum` produced by a PCPS proof must surface in
    /// `boundaries_in_proof` AND `key_exists_as_boundary_in_proof` — the
    /// two helpers are documented to agree on node-type coverage.
    #[test]
    fn kv_digest_count_sum_appears_in_both_boundary_helpers() {
        let v = GroveVersion::latest();
        let merk = make_15_key_pcps(v);
        let mut query = Query::new();
        query.insert_item(QueryItem::Key(b"aa".to_vec()));

        let proof = merk.prove(query, None, v).unwrap().expect("prove succeeds");

        let boundaries = boundaries_in_proof(&proof.proof).expect("boundaries");
        assert!(
            !boundaries.is_empty(),
            "boundaries_in_proof must report KVDigestCountSum nodes from PCPS proofs"
        );

        for boundary in &boundaries {
            let found = key_exists_as_boundary_in_proof(&proof.proof, boundary)
                .expect("key_exists_as_boundary_in_proof");
            assert!(
                found,
                "key_exists_as_boundary_in_proof disagreed with boundaries_in_proof on {:?}",
                boundary
            );
        }
    }
}

/// Whether `op` belongs to the upright (left-to-right) op family.
///
/// `Push` / `Parent` / `Child` are the family an ascending walk is
/// encoded in; `PushInverted` / `ParentInverted` / `ChildInverted` are
/// the descending family. A layer proof is emitted entirely in one
/// family, chosen by the generating query's direction.
pub fn op_is_upright(op: &Op) -> bool {
    match op {
        Op::Push(_) | Op::Parent | Op::Child => true,
        Op::PushInverted(_) | Op::ParentInverted | Op::ChildInverted => false,
    }
}

/// The orientation of a Merk layer proof's op stream, read off the op
/// families in the bytes themselves.
///
/// Every proof op comes in an upright / inverted pair — `Push` /
/// `PushInverted`, `Parent` / `ParentInverted`, `Child` /
/// `ChildInverted` — and `create_proof` picks the family once per layer
/// from a single `left_to_right`, so an honest layer proof is
/// homogeneous: all upright (nodes emitted in ascending key order) or
/// all inverted (descending).
///
/// This is deliberately **not** a trusted read of a proof-supplied
/// parameter. [`execute`] independently checks, for every op it
/// decodes, that an upright push's key is strictly greater than the
/// previous key-bearing node's and an inverted push's key strictly
/// less. A stream that claims one orientation while being ordered the
/// other way is therefore rejected by `execute` itself, before any
/// orientation-sensitive bound-witness check can be misapplied: the
/// orientation reported here is pinned to a structural property of the
/// same bytes, not chosen freely by whoever produced them.
///
/// Returns `Ok(Some(true))` for an all-upright stream, `Ok(Some(false))`
/// for an all-inverted one, `Ok(None)` when there is no
/// direction-bearing op at all (an empty stream, which `execute` then
/// rejects on its own), and `Err` for a stream that mixes the two: no
/// honest prover emits one, and a mixed stream has no single
/// orientation for the bound-witness checks to be correct against, so
/// it is refused rather than guessed at.
///
/// The scan enforces the same [`MAX_PROOF_OPS`] bound as [`execute`]:
/// this pass runs on untrusted bytes *before* the bounded execution
/// pass, so without its own cap an oversized stream would be fully
/// decoded — node allocations included — only to be rejected by
/// `execute` at op 50,001. Any stream over the cap fails verification
/// regardless, so rejecting it here changes no verdict, only how much
/// work the verifier spends reaching it.
pub fn proof_stream_direction(proof_bytes: &[u8]) -> Result<Option<bool>, Error> {
    let mut direction: Option<bool> = None;
    let mut op_count: usize = 0;
    for op_result in Decoder::new(proof_bytes) {
        op_count += 1;
        if op_count > MAX_PROOF_OPS {
            return Err(Error::InvalidProofError(format!(
                "Proof exceeds maximum operation count ({})",
                MAX_PROOF_OPS
            )));
        }
        let upright = op_is_upright(&op_result?);
        match direction {
            None => direction = Some(upright),
            Some(previous) if previous == upright => {}
            Some(_) => {
                return Err(Error::InvalidProofError(
                    "Proof mixes upright and inverted ops; a layer proof is emitted \
                     entirely in one direction"
                        .to_string(),
                ));
            }
        }
    }
    Ok(direction)
}

/// Returns all boundary keys found in the given merk proof bytes.
/// Boundary keys appear as `KVDigest`, `KVDigestCount`, `KVDigestSum`,
/// or `KVDigestCountSum` (dual-axis PCPS) nodes — they prove a key
/// exists in the tree without revealing the value. (The Sum/CountSum
/// variants are the `ProvableSumTree` / `ProvableCountProvableSumTree`
/// analogues of the Count variant; all behave identically for
/// boundary-detection purposes.)
pub fn boundaries_in_proof(proof_bytes: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let decoder = Decoder::new(proof_bytes);
    let mut keys = Vec::new();
    for op_result in decoder {
        let op = op_result?;
        match op {
            Op::Push(Node::KVDigest(k, _))
            | Op::PushInverted(Node::KVDigest(k, _))
            | Op::Push(Node::KVDigestCount(k, _, _))
            | Op::PushInverted(Node::KVDigestCount(k, _, _))
            | Op::Push(Node::KVDigestSum(k, _, _))
            | Op::PushInverted(Node::KVDigestSum(k, _, _))
            | Op::Push(Node::KVDigestCountSum(k, _, _, _))
            | Op::PushInverted(Node::KVDigestCountSum(k, _, _, _)) => {
                keys.push(k);
            }
            _ => {}
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod proof_stream_direction_tests {
    //! `proof_stream_direction` is what lets a verifier run the
    //! orientation-sensitive bound-witness checks over a layer whose
    //! generating direction it cannot know (a synthesized
    //! path-component level under `verify_subset_query`). It must
    //! report the op family faithfully and refuse to pick one when the
    //! stream does not have a single family.

    use grovedb_query::proofs::encode_into;

    use super::proof_stream_direction;
    use crate::proofs::{Node, Op};

    fn encoded(ops: &[Op]) -> Vec<u8> {
        let mut bytes = vec![];
        encode_into(ops.iter(), &mut bytes);
        bytes
    }

    #[test]
    fn upright_stream_reads_as_left_to_right() {
        let bytes = encoded(&[
            Op::Push(Node::KV(vec![1], vec![1])),
            Op::Push(Node::KV(vec![2], vec![2])),
            Op::Parent,
            Op::Push(Node::KV(vec![3], vec![3])),
            Op::Child,
        ]);
        assert_eq!(proof_stream_direction(&bytes).expect("reads"), Some(true));
    }

    #[test]
    fn inverted_stream_reads_as_right_to_left() {
        let bytes = encoded(&[
            Op::PushInverted(Node::KV(vec![3], vec![3])),
            Op::PushInverted(Node::KV(vec![2], vec![2])),
            Op::ParentInverted,
            Op::PushInverted(Node::KV(vec![1], vec![1])),
            Op::ChildInverted,
        ]);
        assert_eq!(proof_stream_direction(&bytes).expect("reads"), Some(false));
    }

    #[test]
    fn empty_stream_has_no_direction() {
        assert_eq!(proof_stream_direction(&[]).expect("reads"), None);
    }

    /// A mixed stream has no single orientation, so there is no correct
    /// mirror of the bound-witness check to run against it. Refuse it
    /// rather than pick from the first op — an all-but-the-first
    /// inverted stream is exactly how a forger would try to get the
    /// ascending checks applied to a descending stream.
    #[test]
    fn mixed_stream_is_refused() {
        let bytes = encoded(&[
            Op::Push(Node::KV(vec![9], vec![9])),
            Op::PushInverted(Node::KV(vec![8], vec![8])),
        ]);
        proof_stream_direction(&bytes).expect_err("mixed families must not resolve to a direction");

        // Mixed in the structural ops alone counts too.
        let bytes = encoded(&[
            Op::Push(Node::KV(vec![1], vec![1])),
            Op::Push(Node::KV(vec![2], vec![2])),
            Op::ParentInverted,
        ]);
        proof_stream_direction(&bytes).expect_err("mixed structural ops must not resolve");
    }

    /// The scan runs on untrusted bytes before the bounded execution
    /// pass, so it must enforce the same op-count cap as `execute` —
    /// otherwise an oversized homogeneous stream would be fully decoded
    /// here only to be rejected there. Exactly at the cap still reads
    /// (matching `execute`, which errors only when the count exceeds
    /// it); one past the cap is refused.
    #[test]
    fn oversized_stream_is_refused_at_the_execute_cap() {
        use crate::proofs::tree::MAX_PROOF_OPS;

        let at_cap: Vec<Op> = (0..MAX_PROOF_OPS)
            .map(|_| Op::Push(Node::Hash([0u8; 32])))
            .collect();
        assert_eq!(
            proof_stream_direction(&encoded(&at_cap)).expect("at-cap stream reads"),
            Some(true)
        );

        let over_cap: Vec<Op> = (0..=MAX_PROOF_OPS)
            .map(|_| Op::Push(Node::Hash([0u8; 32])))
            .collect();
        let err = proof_stream_direction(&encoded(&over_cap))
            .expect_err("over-cap stream must be refused before full decode");
        assert!(
            err.to_string().contains("maximum operation count"),
            "unexpected error: {err}"
        );
    }
}

#[cfg(test)]
mod limit_mode_tests {
    //! `ProofLimitMode::UpperBound` lets a verifier accept a proof the
    //! prover cut short at a limit no greater than the verifier's, without
    //! knowing the prover's limit. These pin that honest early stops
    //! verify, that the results stay a gap-free prefix, and that
    //! `exhausted` only claims completeness the proof shows.

    use grovedb_version::version::GroveVersion;

    use super::{ProofLimitMode, ProofVerificationResult, QueryProofVerify, PROOF_VERSION_LATEST};
    use crate::{
        proofs::{query::QueryItem, Query},
        test_utils::TempMerk,
        tree::Op,
        CryptoHash,
        TreeFeatureType::BasicMerkNode,
    };

    /// A plain merk holding the single-byte keys `0..20`.
    fn make_20_key_merk(grove_version: &GroveVersion) -> TempMerk {
        let mut merk = TempMerk::new(grove_version);
        let entries: Vec<(Vec<u8>, Op)> = (0u8..20)
            .map(|i| (vec![i], Op::Put(vec![i], BasicMerkNode)))
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
            .unwrap()
            .expect("apply should succeed");
        merk.commit(grove_version);
        merk
    }

    fn query_of(items: Vec<QueryItem>, left_to_right: bool) -> Query {
        let mut query = Query::new();
        for item in items {
            query.insert_item(item);
        }
        query.left_to_right = left_to_right;
        query
    }

    fn prove(merk: &TempMerk, query: &Query, limit: Option<u16>, v: &GroveVersion) -> Vec<u8> {
        merk.prove(query.clone(), limit, v)
            .unwrap()
            .expect("prove should succeed")
            .proof
    }

    fn verify(
        query: &Query,
        proof: &[u8],
        limit: Option<u16>,
        limit_mode: ProofLimitMode,
    ) -> Result<(CryptoHash, ProofVerificationResult), crate::Error> {
        query
            .execute_proof_with_limit_mode(
                proof,
                limit,
                limit_mode,
                query.left_to_right,
                PROOF_VERSION_LATEST,
            )
            .unwrap()
    }

    fn keys(result: &ProofVerificationResult) -> Vec<u8> {
        result.result_set.iter().map(|r| r.key[0]).collect()
    }

    /// A proof the prover cut at 5 results verifies under any upper bound
    /// of at least 5, including none, and is reported as not exhausted.
    /// Exact mode still needs the prover's limit exactly.
    fn check_honest_early_stop(left_to_right: bool, expected: Vec<u8>) {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let root = merk.root_hash().unwrap();
        let query = query_of(vec![QueryItem::RangeFrom(vec![2]..)], left_to_right);
        let proof = prove(&merk, &query, Some(5), v);

        for ceiling in [None, Some(5), Some(10), Some(u16::MAX)] {
            let (hash, result) = verify(&query, &proof, ceiling, ProofLimitMode::UpperBound)
                .unwrap_or_else(|e| panic!("upper bound {ceiling:?} must accept: {e}"));
            assert_eq!(hash, root);
            assert_eq!(keys(&result), expected, "upper bound {ceiling:?}");
            assert!(
                !result.exhausted,
                "upper bound {ceiling:?}: more keys exist"
            );
        }

        // More results than the ceiling is still refused.
        let err = verify(&query, &proof, Some(4), ProofLimitMode::UpperBound)
            .expect_err("5 results must not verify under an upper bound of 4");
        assert!(err.to_string().contains("more data than limit"), "{err}");

        // Exact mode is unchanged: the prover's limit verifies, no limit
        // fails at the hidden tail.
        let (_, result) =
            verify(&query, &proof, Some(5), ProofLimitMode::Exact).expect("exact limit verifies");
        assert_eq!(keys(&result), expected);
        let err = verify(&query, &proof, None, ProofLimitMode::Exact)
            .expect_err("exact mode without the prover's limit must fail");
        assert!(
            err.to_string().contains("missing data for query range"),
            "{err}"
        );
    }

    #[test]
    fn honest_early_stop_verifies_as_upper_bound_ascending() {
        check_honest_early_stop(true, vec![2, 3, 4, 5, 6]);
    }

    #[test]
    fn honest_early_stop_verifies_as_upper_bound_descending() {
        check_honest_early_stop(false, vec![19, 18, 17, 16, 15]);
    }

    /// A proof that walks the whole range is reported exhausted, in
    /// either mode.
    #[test]
    fn complete_proof_is_exhausted() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        for left_to_right in [true, false] {
            let query = query_of(vec![QueryItem::Range(vec![2]..vec![8])], left_to_right);
            let proof = prove(&merk, &query, None, v);
            for (limit, mode) in [
                (None, ProofLimitMode::UpperBound),
                (Some(100), ProofLimitMode::UpperBound),
                (None, ProofLimitMode::Exact),
            ] {
                let (_, result) = verify(&query, &proof, limit, mode).expect("full proof verifies");
                assert_eq!(result.result_set.len(), 6);
                assert!(result.exhausted, "{limit:?} {mode:?}");
            }
        }
    }

    /// `exhausted` is conservative: when the walk ends exactly at the limit
    /// with the range still open, the proof does not show what follows, so
    /// it is not claimed even though nothing does. `2..` holds exactly 18
    /// keys here.
    #[test]
    fn walk_ending_at_the_limit_is_not_claimed_exhausted() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let query = query_of(vec![QueryItem::RangeFrom(vec![2]..)], true);
        let proof = prove(&merk, &query, Some(18), v);
        let (_, result) = verify(&query, &proof, Some(18), ProofLimitMode::UpperBound)
            .expect("proof at its own limit verifies");
        assert_eq!(keys(&result), (2u8..20).collect::<Vec<_>>());
        assert!(!result.exhausted);
    }

    /// An early stop inside one query item leaves the later items
    /// unproven: they are not returned and the result is not exhausted.
    #[test]
    fn early_stop_leaves_later_query_items_unproven() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let query = query_of(
            vec![
                QueryItem::RangeInclusive(vec![2]..=vec![9]),
                QueryItem::Key(vec![15]),
            ],
            true,
        );
        let proof = prove(&merk, &query, Some(3), v);
        let (_, result) = verify(&query, &proof, None, ProofLimitMode::UpperBound)
            .expect("early stop in the first item verifies");
        assert_eq!(keys(&result), vec![2, 3, 4]);
        assert!(!result.exhausted);
    }

    /// A proof that hides keys inside the range and then reveals a later
    /// result is not a prefix. Here the proof was made for the keys
    /// `{1, 2, 7, 8}`, so `3..=6` are hidden; read as `1..5` plus `8` it
    /// would skip 3 and 4, and must be rejected.
    #[test]
    fn result_after_a_hidden_node_in_range_is_rejected() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        for left_to_right in [true, false] {
            let proved = query_of(
                [1u8, 2, 7, 8]
                    .into_iter()
                    .map(|k| QueryItem::Key(vec![k]))
                    .collect(),
                left_to_right,
            );
            let proof = prove(&merk, &proved, None, v);
            let read_as = query_of(
                vec![QueryItem::Range(vec![1]..vec![5]), QueryItem::Key(vec![8])],
                left_to_right,
            );
            verify(&read_as, &proof, None, ProofLimitMode::UpperBound)
                .expect_err("a gap inside the range must not verify as a prefix");
        }
    }

    /// A proof that hides the start of the range returns nothing, which
    /// would be an empty page that is not exhausted. Upper-bound mode still
    /// refuses it, as exact mode does.
    #[test]
    fn hidden_start_of_range_is_rejected() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let proved = query_of(vec![QueryItem::Key(vec![0]), QueryItem::Key(vec![9])], true);
        let proof = prove(&merk, &proved, None, v);
        let read_as = query_of(vec![QueryItem::Range(vec![2]..vec![6])], true);
        for limit in [None, Some(10)] {
            verify(&read_as, &proof, limit, ProofLimitMode::UpperBound)
                .expect_err("a proof hiding the whole range must not verify");
        }
    }

    /// Every shape a prover can cut short verifies at any ceiling at least
    /// the prover's limit: exclusive bounds in either direction, a cut
    /// landing on a query-item boundary, and `Key` items. Results are the
    /// honest prefix and never exhausted. The exclusive-bound cases reveal
    /// the bound key after the cut, which exact verification rejects even at
    /// the prover's own limit.
    #[test]
    fn honest_cut_of_every_shape_verifies_as_upper_bound() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let cases: Vec<(Vec<QueryItem>, bool, u16, Vec<u8>)> = vec![
            (
                vec![QueryItem::Range(vec![2]..vec![10])],
                true,
                3,
                vec![2, 3, 4],
            ),
            (vec![QueryItem::RangeTo(..vec![10])], true, 3, vec![0, 1, 2]),
            (
                vec![QueryItem::RangeAfterTo(vec![1]..vec![10])],
                true,
                3,
                vec![2, 3, 4],
            ),
            (
                vec![QueryItem::RangeAfter(vec![10]..)],
                false,
                3,
                vec![19, 18, 17],
            ),
            (
                vec![QueryItem::RangeAfterTo(vec![10]..vec![18])],
                false,
                3,
                vec![17, 16, 15],
            ),
            (
                vec![
                    QueryItem::RangeInclusive(vec![2]..=vec![4]),
                    QueryItem::RangeFrom(vec![10]..),
                ],
                true,
                3,
                vec![2, 3, 4],
            ),
            (
                [1u8, 3, 5, 7]
                    .into_iter()
                    .map(|k| QueryItem::Key(vec![k]))
                    .collect(),
                true,
                2,
                vec![1, 3],
            ),
        ];
        for (items, left_to_right, prover_limit, expected) in cases {
            let query = query_of(items, left_to_right);
            let proof = prove(&merk, &query, Some(prover_limit), v);
            for ceiling in [None, Some(prover_limit), Some(prover_limit + 3)] {
                let (_, result) = verify(&query, &proof, ceiling, ProofLimitMode::UpperBound)
                    .unwrap_or_else(|e| panic!("{query} cut at {prover_limit}, {ceiling:?}: {e}"));
                assert_eq!(keys(&result), expected, "{query} {ceiling:?}");
                assert!(!result.exhausted, "{query} {ceiling:?}");
            }
        }
    }

    /// A ceiling of 0 admits no results.
    #[test]
    fn upper_bound_of_zero_refuses_any_result() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let query = query_of(vec![QueryItem::RangeFrom(vec![2]..)], true);
        let proof = prove(&merk, &query, Some(5), v);
        let err = verify(&query, &proof, Some(0), ProofLimitMode::UpperBound)
            .expect_err("a ceiling of 0 must refuse results");
        assert!(err.to_string().contains("more data than limit"), "{err}");
    }

    /// Hand-built streams for what may follow a stop. In key order: results
    /// 2 and 3, a hidden node, then a node for key 9 (outside `2..=5`). A
    /// boundary key there ends the walk; a key with a value is data after the
    /// stop and is rejected, even though 9 does not match the query.
    #[test]
    fn after_a_stop_only_value_free_nodes_are_accepted() {
        use grovedb_query::proofs::encode_into;

        use crate::proofs::{Node, Op as ProofOp};

        let stream = |after: Node| {
            let ops = [
                ProofOp::Push(Node::KV(vec![2], vec![2])),
                ProofOp::Push(Node::KV(vec![3], vec![3])),
                ProofOp::Parent,
                ProofOp::Push(Node::Hash([7u8; 32])),
                ProofOp::Child,
                ProofOp::Push(after),
                ProofOp::Parent,
            ];
            let mut bytes = vec![];
            encode_into(ops.iter(), &mut bytes);
            bytes
        };
        let query = query_of(vec![QueryItem::RangeInclusive(vec![2]..=vec![5])], true);

        let (_, result) = verify(
            &query,
            &stream(Node::KVDigest(vec![9], [9u8; 32])),
            None,
            ProofLimitMode::UpperBound,
        )
        .expect("a boundary key after the stop is accepted");
        assert_eq!(keys(&result), vec![2, 3]);
        assert!(!result.exhausted);

        let err = verify(
            &query,
            &stream(Node::KV(vec![9], vec![9])),
            None,
            ProofLimitMode::UpperBound,
        )
        .expect_err("a value after the stop must be rejected");
        assert!(err.to_string().contains("after the walk stopped"), "{err}");

        // Exact mode still refuses the hidden node inside the range.
        verify(
            &query,
            &stream(Node::KVDigest(vec![9], [9u8; 32])),
            None,
            ProofLimitMode::Exact,
        )
        .expect_err("exact mode must not accept a hidden node in range");
    }

    /// An empty result is exhausted when the proof shows nothing matches,
    /// in both modes and directions.
    #[test]
    fn verified_empty_range_is_exhausted() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let root = merk.root_hash().unwrap();
        for left_to_right in [true, false] {
            let query = query_of(vec![QueryItem::Range(vec![50]..vec![60])], left_to_right);
            let proof = prove(&merk, &query, None, v);
            for mode in [ProofLimitMode::Exact, ProofLimitMode::UpperBound] {
                for limit in [None, Some(5)] {
                    let (hash, result) = verify(&query, &proof, limit, mode)
                        .unwrap_or_else(|e| panic!("{mode:?} {limit:?}: {e}"));
                    assert_eq!(hash, root);
                    assert!(result.result_set.is_empty());
                    assert!(result.exhausted, "{left_to_right} {mode:?} {limit:?}");
                }
            }
        }
    }

    /// With a limit of 0 nothing is walked, so the result is empty and not
    /// exhausted even though keys match, in both modes and directions.
    #[test]
    fn zero_limit_empty_result_is_not_exhausted() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let root = merk.root_hash().unwrap();
        for left_to_right in [true, false] {
            let query = query_of(vec![QueryItem::RangeFrom(vec![2]..)], left_to_right);
            let proof = prove(&merk, &query, Some(0), v);
            for mode in [ProofLimitMode::Exact, ProofLimitMode::UpperBound] {
                let (hash, result) = verify(&query, &proof, Some(0), mode)
                    .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
                assert_eq!(hash, root);
                assert!(result.result_set.is_empty());
                assert!(!result.exhausted, "{left_to_right} {mode:?}");
            }
        }
    }

    /// Upper-bound mode is V1-only; V0 verification is frozen.
    #[test]
    fn upper_bound_mode_refuses_proof_version_0() {
        let v = GroveVersion::latest();
        let merk = make_20_key_merk(v);
        let query = query_of(vec![QueryItem::RangeFrom(vec![2]..)], true);
        let proof = prove(&merk, &query, Some(5), v);
        let err = query
            .execute_proof_with_limit_mode(&proof, None, ProofLimitMode::UpperBound, true, 0)
            .unwrap()
            .expect_err("proof version 0 must be refused");
        assert!(err.to_string().contains("proof version 1"), "{err}");
    }
}
