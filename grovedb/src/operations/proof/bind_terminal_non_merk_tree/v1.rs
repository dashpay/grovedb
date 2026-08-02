//! `bind_terminal_non_merk_tree` — **v1** (`GROVE_V4`+).
//!
//! Derives the tree's own state root from storage and rewrites the proof node
//! to `Node::KVValueHashFeatureTypeWithChildHash` carrying it. The merk
//! verifier then checks `combine_hash(H(value), child_hash) == value_hash`,
//! which is exactly the composition `insert_subtree` commits for these types —
//! so forged element bytes no longer verify against a genuine root hash.
//!
//! This differs from [`super::v0`] (which leaves the node untouched) in that it
//! both reads storage and mutates the node. The extra reads and hash calls are
//! why it cannot apply to the released versions; see the module docs in
//! [`super`][`mod@super`].

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    proofs::Node,
    tree::{combine_hash, value_hash, NULL_HASH},
    CryptoHash, TreeFeatureType,
};
use grovedb_storage::{Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// `bind_terminal_non_merk_tree` v1 — see the module documentation.
    pub(crate) fn bind_terminal_non_merk_tree_v1(
        &self,
        node: &mut Node,
        element: &Element,
        parent_path: &[&[u8]],
        tx: &Transaction,
        _grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        // Read what we need out of the node before mutating it. The key also
        // names the child subtree holding this tree's data.
        let (key, value) = match &*node {
            Node::KV(key, value)
            | Node::KVValueHash(key, value, ..)
            | Node::KVValueHashFeatureType(key, value, ..)
            | Node::KVValueHashFeatureTypeWithChildHash(key, value, ..) => {
                (key.clone(), value.clone())
            }
            other => {
                return Err(Error::CorruptedData(format!(
                    "bind_terminal_non_merk_tree called on a non-value-bearing proof node: {}",
                    other
                )))
                .wrap_with_cost(cost);
            }
        };

        let mut child_path: Vec<&[u8]> = parent_path.to_vec();
        child_path.push(key.as_slice());

        let child_hash = cost_return_on_error!(
            &mut cost,
            self.non_merk_tree_child_hash(element, &child_path, tx)
        );

        let element_vh = value_hash(&value).unwrap_add_cost(&mut cost);
        let recomputed = combine_hash(&element_vh, &child_hash).unwrap_add_cost(&mut cost);

        let (vh, ft) = match &*node {
            Node::KVValueHashFeatureType(_, _, vh, ft) => (*vh, *ft),
            Node::KVValueHash(_, _, vh) => (*vh, TreeFeatureType::BasicMerkNode),
            _ => (recomputed, TreeFeatureType::BasicMerkNode),
        };

        // Self-check: if the recomputed state root does not reproduce the
        // committed value_hash, the node we are about to emit would be rejected
        // by the verifier. Fail here, where the cause is visible, rather than
        // shipping a proof that cannot verify.
        if recomputed != vh {
            return Err(Error::CorruptedData(format!(
                "non-Merk tree at key {} has state root {} which does not reproduce the \
                 committed value hash {}",
                hex::encode(&key),
                hex::encode(child_hash),
                hex::encode(vh),
            )))
            .wrap_with_cost(cost);
        }

        *node = Node::KVValueHashFeatureTypeWithChildHash(key, value, vh, ft, child_hash);

        Ok(()).wrap_with_cost(cost)
    }

    /// Compute the child hash that a non-Merk tree element's parent Merk
    /// commits to, i.e. the `child_hash` satisfying
    /// `combine_hash(H(value), child_hash) == value_hash`.
    ///
    /// `CommitmentTree`, `MmrTree`, `BulkAppendTree` and
    /// `DenseAppendOnlyFixedSizeTree` have no child Merk; their parent entry is
    /// written by `insert_subtree` with the tree's own state root as the
    /// supplied hash. This reproduces that hash.
    ///
    /// Each arm must mirror the corresponding write path exactly:
    /// - `MmrTree` / `DenseAppendOnlyFixedSizeTree` / `BulkAppendTree` are
    ///   inserted with `NULL_HASH` while still empty, and only start committing
    ///   a computed root once the first append lands. Note that an empty
    ///   `BulkAppendTree`'s `compute_current_state_root()` is *not* `NULL_HASH`,
    ///   so the zero-count case has to short-circuit.
    /// - `CommitmentTree` is inserted with `EMPTY_COMMITMENT_TREE_STATE_ROOT`,
    ///   which is exactly what the sinsemilla/bulk composition below yields at
    ///   count 0 — no special case needed.
    fn non_merk_tree_child_hash(
        &self,
        element: &Element,
        subtree_path: &[&[u8]],
        tx: &Transaction,
    ) -> CostResult<CryptoHash, Error> {
        let mut cost = OperationCost::default();

        let path_vec: Vec<Vec<u8>> = subtree_path.iter().map(|s| s.to_vec()).collect();
        let path_refs: Vec<&[u8]> = path_vec.iter().map(|v| v.as_slice()).collect();
        let storage_path = grovedb_path::SubtreePath::from(path_refs.as_slice());

        match element {
            Element::MmrTree(mmr_size, _) => {
                if *mmr_size == 0 {
                    return Ok(NULL_HASH).wrap_with_cost(cost);
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(storage_path, None, tx)
                    .unwrap_add_cost(&mut cost);
                let store = grovedb_merkle_mountain_range::MmrStore::new(&storage_ctx);
                let mmr = grovedb_merkle_mountain_range::MMR::new(*mmr_size, &store);
                let root = cost_return_on_error!(
                    &mut cost,
                    mmr.get_root()
                        .map_err(|e| Error::CorruptedData(format!("MMR get_root failed: {}", e)))
                );
                Ok(root.hash()).wrap_with_cost(cost)
            }
            Element::DenseAppendOnlyFixedSizeTree(count, height, _) => {
                if *count == 0 {
                    return Ok(NULL_HASH).wrap_with_cost(cost);
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(storage_path, None, tx)
                    .unwrap_add_cost(&mut cost);
                let tree = cost_return_on_error_no_add!(
                    cost,
                    grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree::from_state(
                        *height,
                        *count,
                        storage_ctx,
                    )
                    .map_err(|e| Error::CorruptedData(format!("dense tree state error: {}", e)))
                );
                let root_hash = cost_return_on_error!(
                    &mut cost,
                    tree.root_hash().map_err(|e| Error::CorruptedData(format!(
                        "dense tree root hash error: {}",
                        e
                    )))
                );
                Ok(root_hash).wrap_with_cost(cost)
            }
            Element::BulkAppendTree(total_count, chunk_power, _) => {
                if *total_count == 0 {
                    return Ok(NULL_HASH).wrap_with_cost(cost);
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(storage_path, None, tx)
                    .unwrap_add_cost(&mut cost);
                let tree = cost_return_on_error_no_add!(
                    cost,
                    grovedb_bulk_append_tree::BulkAppendTree::from_state(
                        *total_count,
                        *chunk_power,
                        storage_ctx,
                    )
                    .map_err(|e| Error::CorruptedData(format!(
                        "failed to create BulkAppendTree: {}",
                        e
                    )))
                );
                let state_root = cost_return_on_error_no_add!(
                    cost,
                    tree.compute_current_state_root().map_err(|e| {
                        Error::CorruptedData(format!("bulk append state root failed: {}", e))
                    })
                );
                Ok(state_root).wrap_with_cost(cost)
            }
            Element::CommitmentTree(total_count, chunk_power, _) => {
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(storage_path, None, tx)
                    .unwrap_add_cost(&mut cost);

                let sinsemilla_root = match storage_ctx
                    .get(grovedb_commitment_tree::COMMITMENT_TREE_DATA_KEY)
                    .value
                {
                    Ok(Some(frontier_bytes)) => {
                        match grovedb_commitment_tree::CommitmentFrontier::deserialize(
                            frontier_bytes.as_ref(),
                        ) {
                            Ok(frontier) => frontier.root_hash(),
                            Err(_) => grovedb_commitment_tree::EMPTY_SINSEMILLA_ROOT,
                        }
                    }
                    _ => grovedb_commitment_tree::EMPTY_SINSEMILLA_ROOT,
                };

                let tree = cost_return_on_error_no_add!(
                    cost,
                    grovedb_bulk_append_tree::BulkAppendTree::from_state(
                        *total_count,
                        *chunk_power,
                        storage_ctx,
                    )
                    .map_err(|e| Error::CorruptedData(format!(
                        "failed to create BulkAppendTree: {}",
                        e
                    )))
                );
                let bulk_state_root = cost_return_on_error_no_add!(
                    cost,
                    tree.compute_current_state_root().map_err(|e| {
                        Error::CorruptedData(format!("bulk append state root failed: {}", e))
                    })
                );

                Ok(grovedb_commitment_tree::compute_commitment_tree_state_root(
                    &sinsemilla_root,
                    &bulk_state_root,
                ))
                .wrap_with_cost(cost)
            }
            _ => Err(Error::CorruptedCodeExecution(
                "non_merk_tree_child_hash called on an element that is not a non-Merk tree",
            ))
            .wrap_with_cost(cost),
        }
    }
}
