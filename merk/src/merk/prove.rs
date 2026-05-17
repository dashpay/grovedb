use std::collections::LinkedList;

use grovedb_costs::{CostResult, CostsExt};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{
    proofs::{
        encode_into,
        query::{count_offset::ProverCountOffsetResult, QueryItem},
        Op as ProofOp, Query,
    },
    tree::RefWalker,
    Error, Merk,
};

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Creates a Merkle proof for the list of queried keys. For each key in the
    /// query, if the key is found in the store then the value will be proven to
    /// be in the tree. For each key in the query that does not exist in the
    /// tree, its absence will be proven by including boundary keys.
    ///
    /// The proof returned is in an encoded format which can be verified with
    /// `merk::verify`.
    ///
    /// This will fail if the keys in `query` are not sorted and unique. This
    /// check adds some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `prove_unchecked` for a small performance
    /// gain.
    pub fn prove(
        &self,
        query: Query,
        limit: Option<u16>,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofConstructionResult, Error> {
        let left_to_right = query.left_to_right;
        self.prove_unchecked(query, limit, left_to_right, grove_version)
            .map_ok(|(proof, limit)| {
                let mut bytes = Vec::with_capacity(128);
                encode_into(proof.iter(), &mut bytes);
                ProofConstructionResult::new(bytes, limit)
            })
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in the
    /// query, if the key is found in the store then the value will be proven to
    /// be in the tree. For each key in the query that does not exist in the
    /// tree, its absence will be proven by including boundary keys.
    ///
    /// The proof returned is in an intermediate format to be later encoded
    ///
    /// This will fail if the keys in `query` are not sorted and unique. This
    /// check adds some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `prove_unchecked` for a small performance
    /// gain.
    pub fn prove_without_encoding(
        &self,
        query: Query,
        limit: Option<u16>,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofWithoutEncodingResult, Error> {
        let left_to_right = query.left_to_right;
        self.prove_unchecked(query, limit, left_to_right, grove_version)
            .map_ok(|(proof, limit)| ProofWithoutEncodingResult::new(proof, limit))
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in
    /// the query, if the key is found in the store then the value will be
    /// proven to be in the tree. For each key in the query that does not
    /// exist in the tree, its absence will be proven by including
    /// boundary keys.
    /// The proof returned is in an encoded format which can be verified with
    /// `merk::verify`.
    ///
    /// This is unsafe because the keys in `query` must be sorted and unique -
    /// if they are not, there will be undefined behavior. For a safe version
    /// of this method which checks to ensure the batch is sorted and
    /// unique, see `prove`.
    pub fn prove_unchecked<Q, I>(
        &self,
        query: I,
        limit: Option<u16>,
        left_to_right: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<Proof, Error>
    where
        Q: Into<QueryItem>,
        I: IntoIterator<Item = Q>,
    {
        let query_vec: Vec<QueryItem> = query.into_iter().map(Into::into).collect();

        self.use_tree_mut(|maybe_tree| {
            maybe_tree
                .ok_or(Error::CorruptedCodeExecution(
                    "Cannot create proof for empty tree",
                ))
                .wrap_with_cost(Default::default())
                .flat_map_ok(|tree| {
                    let mut ref_walker = RefWalker::new(tree, self.source());
                    ref_walker.create_proof(
                        query_vec.as_slice(),
                        limit,
                        left_to_right,
                        grove_version,
                    )
                })
                .map_ok(|(proof, _, status, ..)| (proof, status.limit))
        })
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in
    /// the query, if the key is found in the store then the value will be
    /// proven to be in the tree. For each key in the query that does not
    /// exist in the tree, its absence will be proven by including
    /// boundary keys.
    /// The proof returned is in an encoded format which can be verified with
    /// `merk::verify`.
    ///
    /// This is unsafe because the keys in `query` must be sorted and unique -
    /// if they are not, there will be undefined behavior. For a safe version
    /// of this method which checks to ensure the batch is sorted and
    /// unique, see `prove`.
    pub fn prove_unchecked_query_items(
        &self,
        query_items: &[QueryItem],
        limit: Option<u16>,
        left_to_right: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<Proof, Error> {
        self.use_tree_mut(|maybe_tree| {
            maybe_tree
                .ok_or(Error::CorruptedCodeExecution(
                    "Cannot create proof for empty tree",
                ))
                .wrap_with_cost(Default::default())
                .flat_map_ok(|tree| {
                    let mut ref_walker = RefWalker::new(tree, self.source());
                    ref_walker.create_proof(query_items, limit, left_to_right, grove_version)
                })
                .map_ok(|(proof, _, status, ..)| (proof, status.limit))
        })
    }

    /// Generate a count-only proof for an `AggregateCountOnRange` query.
    ///
    /// `inner_range` is the `QueryItem` wrapped by `AggregateCountOnRange`
    /// (the caller is expected to have already validated and stripped the
    /// wrapper at the `Query` level via
    /// `Query::validate_aggregate_count_on_range`).
    ///
    /// The merk's `tree_type` must be one of `ProvableCountTree` or
    /// `ProvableCountSumTree` (regardless of whether the merk is empty).
    /// Any other tree type is rejected with `Error::InvalidProofError`
    /// before any walking happens.
    ///
    /// On a tree-type-valid but empty Merk this returns
    /// `(empty proof, count = 0)` — an empty subtree is a valid input for a
    /// count query and the answer is unambiguously zero.
    pub fn prove_aggregate_count_on_range(
        &self,
        inner_range: &QueryItem,
        grove_version: &GroveVersion,
    ) -> CostResult<(LinkedList<ProofOp>, u64), Error> {
        let tree_type = self.tree_type;
        if !matches!(
            tree_type,
            crate::TreeType::ProvableCountTree | crate::TreeType::ProvableCountSumTree
        ) {
            return Err(Error::InvalidProofError(format!(
                "AggregateCountOnRange is only valid against ProvableCountTree or \
                 ProvableCountSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(Default::default());
        }
        self.use_tree_mut(|maybe_tree| match maybe_tree {
            None => Ok((LinkedList::new(), 0u64)).wrap_with_cost(Default::default()),
            Some(tree) => {
                let mut ref_walker = RefWalker::new(tree, self.source());
                ref_walker.create_aggregate_count_on_range_proof(
                    inner_range,
                    tree_type,
                    grove_version,
                )
            }
        })
    }

    /// Generate an offset-paginated proof for a single-range query
    /// against a `ProvableCountTree` or `ProvableCountSumTree`.
    ///
    /// This is the count-tree analogue of the regular [`Self::prove`]
    /// path, with one key extension: a non-zero `offset` is honored.
    /// The proof commits the count of skipped items via the same
    /// `HashWithCount` infrastructure used by
    /// [`Self::prove_aggregate_count_on_range`], so the offset region
    /// pays O(log n) proof size per skipped subtree rather than
    /// O(skipped). Returned items inside the limit window emit as
    /// normal count-bearing value nodes, so the verifier-side result
    /// shape matches what a regular range query without offset would
    /// produce.
    ///
    /// `inner_range` is the single `QueryItem` to scan (already
    /// validated at the caller's `Query`/`PathQuery` level). `offset`
    /// is how many leading in-range items to skip (in directional
    /// order); `limit` is the maximum number of items to return after
    /// the offset (`None` means unlimited). `left_to_right` controls
    /// iteration direction.
    ///
    /// The merk's `tree_type` must be one of `ProvableCountTree` /
    /// `ProvableCountSumTree`. Any other tree type is rejected with
    /// `Error::InvalidProofError` before any walking happens — count
    /// commitments are only meaningful against trees that bind their
    /// count into the node hash. Empty merk: returns an empty
    /// `ProverCountOffsetResult` (no ops, 0 returned, full offset
    /// remaining).
    pub fn prove_count_offset_on_range(
        &self,
        inner_range: &QueryItem,
        offset: u64,
        limit: Option<u64>,
        left_to_right: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<ProverCountOffsetResult, Error> {
        let tree_type = self.tree_type;
        if !matches!(
            tree_type,
            crate::TreeType::ProvableCountTree | crate::TreeType::ProvableCountSumTree
        ) {
            return Err(Error::InvalidProofError(format!(
                "count-offset paginated proof is only valid against ProvableCountTree or \
                 ProvableCountSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(Default::default());
        }
        self.use_tree_mut(|maybe_tree| match maybe_tree {
            None => Ok(ProverCountOffsetResult {
                ops: LinkedList::new(),
                returned: 0,
                offset_remaining: offset,
            })
            .wrap_with_cost(Default::default()),
            Some(tree) => {
                let mut ref_walker = RefWalker::new(tree, self.source());
                ref_walker.create_count_offset_on_range_proof(
                    inner_range,
                    offset,
                    limit,
                    left_to_right,
                    tree_type,
                    grove_version,
                )
            }
        })
    }

    /// Generate a sum-only proof for an `AggregateSumOnRange` query.
    /// Mirror of [`Self::prove_aggregate_count_on_range`] for the
    /// `ProvableSumTree` flavor.
    ///
    /// The merk's `tree_type` must be `ProvableSumTree`; any other tree type
    /// is rejected with `Error::InvalidProofError` before any walking
    /// happens. Empty merk: returns `(empty proof, sum = 0)`.
    pub fn prove_aggregate_sum_on_range(
        &self,
        inner_range: &QueryItem,
        grove_version: &GroveVersion,
    ) -> CostResult<(LinkedList<ProofOp>, i64), Error> {
        let tree_type = self.tree_type;
        if !matches!(tree_type, crate::TreeType::ProvableSumTree) {
            return Err(Error::InvalidProofError(format!(
                "AggregateSumOnRange is only valid against ProvableSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(Default::default());
        }
        self.use_tree_mut(|maybe_tree| match maybe_tree {
            None => Ok((LinkedList::new(), 0i64)).wrap_with_cost(Default::default()),
            Some(tree) => {
                let mut ref_walker = RefWalker::new(tree, self.source());
                ref_walker.create_aggregate_sum_on_range_proof(
                    inner_range,
                    tree_type,
                    grove_version,
                )
            }
        })
    }
}

type Proof = (LinkedList<ProofOp>, Option<u16>);

/// Proof construction result
pub struct ProofConstructionResult {
    /// Proof
    pub proof: Vec<u8>,
    /// Limit
    pub limit: Option<u16>,
}

impl ProofConstructionResult {
    /// New ProofConstructionResult
    pub fn new(proof: Vec<u8>, limit: Option<u16>) -> Self {
        Self { proof, limit }
    }
}

/// Proof without encoding result
pub struct ProofWithoutEncodingResult {
    /// Proof
    pub proof: LinkedList<ProofOp>,
    /// Limit
    pub limit: Option<u16>,
}

impl ProofWithoutEncodingResult {
    /// New ProofWithoutEncodingResult
    pub fn new(proof: LinkedList<ProofOp>, limit: Option<u16>) -> Self {
        Self { proof, limit }
    }
}
