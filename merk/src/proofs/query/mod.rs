// Re-export grovedb-query types so downstream can use
// grovedb_merk::proofs::query::*
pub use grovedb_query::*;

#[cfg(test)]
mod merk_integration_tests;

#[cfg(any(feature = "minimal", feature = "verify"))]
mod aggregate_common;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod aggregate_count;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod aggregate_sum;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod map;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod verify;

#[cfg(any(feature = "minimal", feature = "verify"))]
pub use aggregate_count::verify_aggregate_count_on_range_proof;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use aggregate_sum::verify_aggregate_sum_on_range_proof;

#[cfg(feature = "minimal")]
use grovedb_costs::{cost_return_on_error, CostContext, CostResult, CostsExt, OperationCost};
#[cfg(feature = "minimal")]
use grovedb_element::{ElementType, ProofNodeType};
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use map::{Map, MapBuilder};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use verify::{
    boundaries_in_proof, key_exists_as_boundary_in_proof, ProofVerificationResult,
    ProvedKeyOptionalValue, ProvedKeyValue, QueryProofVerify, VerifyOptions, PROOF_VERSION_LATEST,
};
#[cfg(feature = "minimal")]
use {super::Op, std::collections::LinkedList};

#[cfg(feature = "minimal")]
use super::Node;
#[cfg(feature = "minimal")]
use crate::error::Error;
#[cfg(feature = "minimal")]
use crate::tree::kv::ValueDefinedCostType;
#[cfg(feature = "minimal")]
use crate::tree::AggregateData;
#[cfg(feature = "minimal")]
use crate::tree::{Fetch, Link, RefWalker};
#[cfg(feature = "minimal")]
use crate::TreeFeatureType;
#[cfg(feature = "minimal")]
use crate::TreeType;

/// Result type for proof generation: contains the proof ops, a tuple indicating
/// left/right absence, and the current proof status (remaining limit).
#[cfg(feature = "minimal")]
pub type ProofAbsenceLimit = (LinkedList<Op>, (bool, bool), ProofStatus);

#[cfg(feature = "minimal")]
impl<S> RefWalker<'_, S>
where
    S: Fetch + Sized + Clone,
{
    #[allow(dead_code)]
    /// Creates a `Node::KV` from the key/value pair of the root node.
    pub(crate) fn to_kv_node(&self) -> Node {
        Node::KV(
            self.tree().key().to_vec(),
            self.tree().value_as_slice().to_vec(),
        )
    }

    /// Creates a `Node::KVValueHash` from the key/value pair of the root node.
    pub(crate) fn to_kv_value_hash_node(&self) -> Node {
        Node::KVValueHash(
            self.tree().key().to_vec(),
            self.tree().value_ref().to_vec(),
            *self.tree().value_hash(),
        )
    }

    /// Creates a `Node::KVValueHashFeatureType` from the key/value pair of the
    /// root node
    /// Note: For ProvableCountTree, ProvableCountSumTree, and ProvableSumTree,
    /// uses aggregate value to match hash calculation
    pub(crate) fn to_kv_value_hash_feature_type_node(&self) -> Node {
        // For ProvableCountTree, ProvableCountSumTree, and ProvableSumTree
        // we need to use the aggregate value (sum of self + children) because
        // the hash calculation uses aggregate_data(), not feature_type()
        let feature_type = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableCount(count)) => {
                TreeFeatureType::ProvableCountedMerkNode(count)
            }
            Ok(AggregateData::ProvableCountAndSum(count, sum)) => {
                TreeFeatureType::ProvableCountedSummedMerkNode(count, sum)
            }
            Ok(AggregateData::ProvableSum(sum)) => TreeFeatureType::ProvableSummedMerkNode(sum),
            _ => self.tree().feature_type(),
        };
        Node::KVValueHashFeatureType(
            self.tree().key().to_vec(),
            self.tree().value_ref().to_vec(),
            *self.tree().value_hash(),
            feature_type,
        )
    }

    /// Creates a `Node::KVHash` from the hash of the key/value pair of the root
    /// node.
    pub(crate) fn to_kvhash_node(&self) -> Node {
        Node::KVHash(*self.tree().kv_hash())
    }

    /// Creates a `Node::KVDigest` from the key/value_hash pair of the root
    /// node.
    pub(crate) fn to_kvdigest_node(&self) -> Node {
        Node::KVDigest(self.tree().key().to_vec(), *self.tree().value_hash())
    }

    /// Creates a `Node::KVDigestCount` from the key/value_hash pair and count
    /// of the root node. Used for boundary nodes (proving absence) in
    /// ProvableCountTree and ProvableCountSumTree.
    /// Note: Uses aggregate count (sum of self + children) to match hash
    /// calculation
    pub(crate) fn to_kvdigest_count_node(&self) -> Node {
        let count = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableCount(count)) => count,
            Ok(AggregateData::ProvableCountAndSum(count, _)) => count,
            _ => 0, // Fallback, should not happen for ProvableCount trees
        };
        Node::KVDigestCount(self.tree().key().to_vec(), *self.tree().value_hash(), count)
    }

    /// Creates a `Node::Hash` from the hash of the node.
    #[allow(dead_code)]
    pub(crate) fn to_hash_node(&self) -> CostContext<Node> {
        self.tree().hash().map(Node::Hash)
    }

    /// Creates a `Node::Hash` from the hash of the node, using tree type
    /// aware hashing. For ProvableCountTree and ProvableCountSumTree, this
    /// uses `hash_for_link` which includes the count in the hash computation.
    pub(crate) fn to_hash_node_for_tree_type(&self, tree_type: TreeType) -> CostContext<Node> {
        self.tree().hash_for_link(tree_type).map(Node::Hash)
    }

    /// Creates a `Node::KVHashCount` from the kv hash and count of the root
    /// node Used for ProvableCountTree and ProvableCountSumTree
    /// Note: Uses aggregate count (sum of self + children) to match hash
    /// calculation
    pub(crate) fn to_kvhash_count_node(&self) -> Node {
        let count = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableCount(count)) => count,
            Ok(AggregateData::ProvableCountAndSum(count, _)) => count,
            _ => 0, // Fallback, should not happen for ProvableCount trees
        };
        Node::KVHashCount(*self.tree().kv_hash(), count)
    }

    /// Creates a `Node::KVCount` from the key/value pair and count of the root
    /// node. Used for Items in ProvableCountTree or ProvableCountSumTree -
    /// tamper-resistant (verifier computes hash from value) while including the
    /// count. Note: Uses aggregate count (sum of self + children) to match hash
    /// calculation
    pub(crate) fn to_kv_count_node(&self) -> Node {
        let count = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableCount(count)) => count,
            Ok(AggregateData::ProvableCountAndSum(count, _)) => count,
            Ok(AggregateData::ProvableCountAndProvableSum(count, _)) => count,
            _ => 0, // Fallback, should not happen for ProvableCount trees
        };
        Node::KVCount(
            self.tree().key().to_vec(),
            self.tree().value_as_slice().to_vec(),
            count,
        )
    }

    /// Creates a `Node::KVCountSum` from the key/value pair and (count, sum)
    /// of the root node. Used for Items in `ProvableCountProvableSumTree` —
    /// tamper-resistant (verifier computes hash from value) while including
    /// both the count and the sum so the verifier can recompute
    /// `node_hash_with_count_and_sum`.
    pub(crate) fn to_kv_count_sum_node(&self) -> Node {
        let (count, sum) = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableCountAndProvableSum(c, s)) => (c, s),
            _ => (0, 0),
        };
        Node::KVCountSum(
            self.tree().key().to_vec(),
            self.tree().value_as_slice().to_vec(),
            count,
            sum,
        )
    }

    /// Boundary (absence-proof) analogue of `to_kv_count_sum_node`.
    pub(crate) fn to_kvdigest_count_sum_node(&self) -> Node {
        let (count, sum) = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableCountAndProvableSum(c, s)) => (c, s),
            _ => (0, 0),
        };
        Node::KVDigestCountSum(
            self.tree().key().to_vec(),
            *self.tree().value_hash(),
            count,
            sum,
        )
    }

    /// Non-queried-path analogue of `to_kv_count_sum_node` — emits a
    /// `Node::KVHashCountSum` carrying the per-node kv hash and both
    /// aggregates so the verifier can recompute the hash for nodes that
    /// don't contribute their value to the proof.
    pub(crate) fn to_kvhash_count_sum_node(&self) -> Node {
        let (count, sum) = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableCountAndProvableSum(c, s)) => (c, s),
            _ => (0, 0),
        };
        Node::KVHashCountSum(*self.tree().kv_hash(), count, sum)
    }

    /// Creates a `Node::KVDigestSum` from the key/value_hash pair and sum
    /// of the root node. Parallel to `to_kvdigest_count_node` for
    /// ProvableSumTree boundary nodes (proving absence). Uses aggregate sum
    /// (self + children) to match the `node_hash_with_sum` calculation.
    pub(crate) fn to_kvdigest_sum_node(&self) -> Node {
        let sum = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableSum(sum)) => sum,
            _ => 0,
        };
        Node::KVDigestSum(self.tree().key().to_vec(), *self.tree().value_hash(), sum)
    }

    /// Creates a `Node::KVHashSum` from the kv hash and sum of the root
    /// node. Parallel to `to_kvhash_count_node` for ProvableSumTree
    /// non-queried nodes on the path.
    pub(crate) fn to_kvhash_sum_node(&self) -> Node {
        let sum = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableSum(sum)) => sum,
            _ => 0,
        };
        Node::KVHashSum(*self.tree().kv_hash(), sum)
    }

    /// Creates a `Node::KVSum` from the key/value pair and sum of the root
    /// node. Parallel to `to_kv_count_node` for queried Items in a
    /// ProvableSumTree. Tamper-resistant (verifier computes hash from value)
    /// while including the sum in the node hash.
    pub(crate) fn to_kv_sum_node(&self) -> Node {
        let sum = match self.tree().aggregate_data() {
            Ok(AggregateData::ProvableSum(sum)) => sum,
            _ => 0,
        };
        Node::KVSum(
            self.tree().key().to_vec(),
            self.tree().value_as_slice().to_vec(),
            sum,
        )
    }

    #[cfg(feature = "minimal")]
    pub(crate) fn create_proof(
        &mut self,
        query: &[QueryItem],
        limit: Option<u16>,
        left_to_right: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofAbsenceLimit, Error> {
        let (proof_query_items, proof_params) =
            ProofItems::new_with_query_items(query, left_to_right);
        let proof_status = ProofStatus::new_with_limit(limit);
        self.create_proof_internal(
            &proof_query_items,
            &proof_params,
            proof_status,
            grove_version,
        )
    }

    /// Generates a proof for the list of queried items. Returns a tuple
    /// containing the generated proof operators, and a tuple representing if
    /// any keys were queried were less than the left edge or greater than the
    /// right edge, respectively.
    #[cfg(feature = "minimal")]
    pub(crate) fn create_proof_internal(
        &mut self,
        proof_query_items: &ProofItems,
        proof_params: &ProofParams,
        proof_status: ProofStatus,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofAbsenceLimit, Error> {
        let mut cost = OperationCost::default();

        // We get the key from the current node we are at
        let key = self.tree().key().to_vec(); // there is no escaping this clone

        // We check to see if that key matches our current proof items
        // We also split our proof items for query items that would be active on the
        // left of our node and other query items that would be active on the
        // right of our node. For example if we are looking for keys 3, 5, 8 and
        // 9, and we are at key 6, we split the keys we are searching for, as 3
        // and 5 won't be on the right of 6 and 8 and 9 won't be on the left of
        // 6. The same logic applies to range queries. If we are searching for
        // items 1 to 4 it would not make sense to push this to the right of 6.

        let (mut found_item, on_boundary_not_found, mut left_proof_items, mut right_proof_items) =
            proof_query_items.process_key(&key);

        if let Some(current_limit) = proof_status.limit
            && current_limit == 0
        {
            left_proof_items = ProofItems::default();
            found_item = false;
            right_proof_items = ProofItems::default();
        }

        let proof_direction = proof_params.left_to_right; // search the opposite path on second pass
        let (mut proof, left_absence, proof_status) = if proof_params.left_to_right {
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    &left_proof_items,
                    proof_params,
                    proof_status,
                    grove_version
                )
            )
        } else {
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    &right_proof_items,
                    proof_params,
                    proof_status,
                    grove_version
                )
            )
        };

        let mut new_limit = None;

        if let Some(current_limit) = proof_status.limit {
            // if after generating proof for the left subtree, the limit becomes 0
            // clear the current node and clear the right batch
            if current_limit == 0 {
                if proof_params.left_to_right {
                    right_proof_items = ProofItems::default();
                } else {
                    left_proof_items = ProofItems::default();
                }
                found_item = false;
            } else if found_item && !on_boundary_not_found {
                // if limit is not zero, reserve a limit slot for the current node
                // before generating proof for the right subtree
                new_limit = Some(current_limit - 1);
                // if after limit slot reservation, limit becomes 0, right query
                // should be cleared
                if current_limit - 1 == 0 {
                    if proof_params.left_to_right {
                        right_proof_items = ProofItems::default();
                    } else {
                        left_proof_items = ProofItems::default();
                    }
                }
            }
        }

        let proof_direction = !proof_direction; // search the opposite path on second pass
        let (mut right_proof, right_absence, new_limit) = if proof_params.left_to_right {
            let new_proof_status = proof_status.update_limit(new_limit);
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    &right_proof_items,
                    proof_params,
                    new_proof_status,
                    grove_version
                )
            )
        } else {
            let new_proof_status = proof_status.update_limit(new_limit);
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    &left_proof_items,
                    proof_params,
                    new_proof_status,
                    grove_version
                )
            )
        };

        let (has_left, has_right) = (!proof.is_empty(), !right_proof.is_empty());

        let is_provable_count_only_tree = matches!(
            self.tree().feature_type(),
            TreeFeatureType::ProvableCountedMerkNode(_)
                | TreeFeatureType::ProvableCountedSummedMerkNode(..)
        );
        // Sibling family for ProvableSumTree, whose nodes carry the i64 sum
        // in their feature_type.
        let is_provable_sum_only_tree = matches!(
            self.tree().feature_type(),
            TreeFeatureType::ProvableSummedMerkNode(_)
        );
        // ProvableCountProvableSumTree carries BOTH a count and a sum in
        // its feature_type; both axes are baked into the node hash.
        let is_provable_count_and_provable_sum_tree = matches!(
            self.tree().feature_type(),
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(..)
        );
        // Combined predicate used by the boundary / non-queried-path
        // selectors below to gate the aggregate emission.
        let is_provable_count_tree =
            is_provable_count_only_tree || is_provable_count_and_provable_sum_tree;
        let is_provable_sum_tree =
            is_provable_sum_only_tree || is_provable_count_and_provable_sum_tree;
        let is_provable_aggregate_tree = is_provable_count_tree || is_provable_sum_tree;

        // Convert the tree kind to an `ElementType` so `proof_node_type()`
        // can dispatch — three mutually-exclusive families:
        //   - count-only      → `ProvableCountTree`
        //   - sum-only        → `ProvableSumTree`
        //   - count-and-sum   → `ProvableCountProvableSumTree`
        let parent_tree_type = if is_provable_count_and_provable_sum_tree {
            Some(ElementType::ProvableCountProvableSumTree)
        } else if is_provable_count_only_tree {
            Some(ElementType::ProvableCountTree)
        } else if is_provable_sum_only_tree {
            Some(ElementType::ProvableSumTree)
        } else {
            None // Regular tree or unknown - treated the same
        };

        let proof_op = if found_item {
            // For query proofs, we need to include the actual key/value data.
            // The node type depends on the element type stored in the value:
            // - Items (simple hash): use KV or KVCount (verifier computes hash -
            //   tamper-proof)
            // - Trees/References (combined hash): use KVValueHash or KVValueHashFeatureType
            //
            // Determine proof node type from element type (first byte of value)
            // - For valid Element types: use the element's proof_node_type()
            //   - Items in regular trees -> Kv (tamper-resistant)
            //   - Items in ProvableCountTree -> KvCount (tamper-resistant + count)
            //   - Trees/References -> KvValueHash (required for combined hashes)
            // - For invalid/unknown types (raw Merk usage): default to Kv
            //   - Raw Merk values should be tamper-resistant by default
            //   - Only GroveDB subtrees need KvValueHash for combined hash verification
            let proof_node_type = ElementType::from_serialized_value(self.tree().value_as_slice())
                .map(|et| et.proof_node_type(parent_tree_type))
                .unwrap_or(ProofNodeType::Kv); // Default to tamper-resistant for raw Merk

            // Convert ProofNodeType to actual Node
            // Note: References use KvRefValueHash or KvRefValueHashCount, but at the merk
            // level these generate KVValueHash or KVValueHashFeatureType nodes.
            // GroveDB post-processes these to KVRefValueHash or KVRefValueHashCount
            // with dereferenced values.
            let node = match proof_node_type {
                ProofNodeType::Kv => self.to_kv_node(),
                ProofNodeType::KvCount => self.to_kv_count_node(),
                ProofNodeType::KvSum => self.to_kv_sum_node(),
                ProofNodeType::KvCountSum => self.to_kv_count_sum_node(),
                ProofNodeType::KvValueHash => self.to_kv_value_hash_node(),
                ProofNodeType::KvValueHashFeatureType => self.to_kv_value_hash_feature_type_node(),
                // References: at merk level, generate same node type as non-ref counterpart
                // GroveDB will post-process to KVRefValueHash with dereferenced value
                ProofNodeType::KvRefValueHash => self.to_kv_value_hash_node(),
                // ProvableCountTree references: generate KVValueHashFeatureType
                // GroveDB will post-process to KVRefValueHashCount with dereferenced value
                ProofNodeType::KvRefValueHashCount => self.to_kv_value_hash_feature_type_node(),
                // ProvableSumTree references: same merk-level shape as
                // KvRefValueHashCount — emit KVValueHashFeatureType so the
                // feature_type carries the sum, then GroveDB post-processes
                // to KVRefValueHashSum with the dereferenced value.
                ProofNodeType::KvRefValueHashSum => self.to_kv_value_hash_feature_type_node(),
                // ProvableCountProvableSumTree references: emit
                // KVValueHashFeatureType carrying the dual feature_type,
                // then GroveDB post-processes to KVRefValueHashCountSum
                // with the dereferenced value.
                ProofNodeType::KvRefValueHashCountSum => self.to_kv_value_hash_feature_type_node(),
            };

            if proof_params.left_to_right {
                Op::Push(node)
            } else {
                Op::PushInverted(node)
            }
        } else if on_boundary_not_found || left_absence.1 || right_absence.0 {
            // On boundary (proving absence): use KVDigest / KVDigestCount /
            // KVDigestSum / KVDigestCountSum depending on the parent's
            // aggregate kind. The combined ProvableCountProvableSumTree
            // family is checked first because it is a member of BOTH the
            // count and sum families.
            let node = if is_provable_count_and_provable_sum_tree {
                self.to_kvdigest_count_sum_node()
            } else if is_provable_count_only_tree {
                self.to_kvdigest_count_node()
            } else if is_provable_sum_only_tree {
                self.to_kvdigest_sum_node()
            } else {
                self.to_kvdigest_node()
            };
            if proof_params.left_to_right {
                Op::Push(node)
            } else {
                Op::PushInverted(node)
            }
        } else if is_provable_aggregate_tree {
            // Non-queried path nodes carry the aggregate(s) so the
            // verifier can recompute the node hash. Check the combined
            // family first for the same reason as the boundary path
            // above.
            let node = if is_provable_count_and_provable_sum_tree {
                self.to_kvhash_count_sum_node()
            } else if is_provable_count_only_tree {
                self.to_kvhash_count_node()
            } else {
                // is_provable_sum_only_tree
                self.to_kvhash_sum_node()
            };
            if proof_params.left_to_right {
                Op::Push(node)
            } else {
                Op::PushInverted(node)
            }
        } else if proof_params.left_to_right {
            Op::Push(self.to_kvhash_node())
        } else {
            Op::PushInverted(self.to_kvhash_node())
        };

        proof.push_back(proof_op);

        if has_left {
            if proof_params.left_to_right {
                proof.push_back(Op::Parent);
            } else {
                proof.push_back(Op::ParentInverted);
            }
        }

        if has_right {
            proof.append(&mut right_proof);
            if proof_params.left_to_right {
                proof.push_back(Op::Child);
            } else {
                proof.push_back(Op::ChildInverted);
            }
        }

        Ok((proof, (left_absence.0, right_absence.1), new_limit)).wrap_with_cost(cost)
    }

    /// Similar to `create_proof`. Recurses into the child on the given side and
    /// generates a proof for the queried keys.
    #[cfg(feature = "minimal")]
    fn create_child_proof(
        &mut self,
        left: bool,
        query_items: &ProofItems,
        params: &ProofParams,
        proof_status: ProofStatus,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofAbsenceLimit, Error> {
        if !query_items.has_no_query_items() {
            self.walk(
                left,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
            .flat_map_ok(|child_opt| {
                if let Some(mut child) = child_opt {
                    child.create_proof_internal(query_items, params, proof_status, grove_version)
                } else {
                    Ok((LinkedList::new(), (true, true), proof_status))
                        .wrap_with_cost(Default::default())
                }
            })
        } else if let Some(link) = self.tree().link(left) {
            let mut proof = LinkedList::new();
            proof.push_back(if params.left_to_right {
                Op::Push(link.to_hash_node())
            } else {
                Op::PushInverted(link.to_hash_node())
            });
            Ok((proof, (false, false), proof_status)).wrap_with_cost(Default::default())
        } else {
            Ok((LinkedList::new(), (false, false), proof_status)).wrap_with_cost(Default::default())
        }
    }
}

#[cfg(feature = "minimal")]
impl Link {
    /// Creates a `Node::Hash` from this link. Panics if the link is of variant
    /// `Link::Modified` since its hash has not yet been computed.
    #[cfg(feature = "minimal")]
    const fn to_hash_node(&self) -> Node {
        let hash = match self {
            Link::Reference { hash, .. } => hash,
            Link::Modified { .. } => {
                panic!("Cannot convert Link::Modified to proof hash node");
            }
            Link::Uncommitted { hash, .. } => hash,
            Link::Loaded { hash, .. } => hash,
        };
        Node::Hash(*hash)
    }
}
