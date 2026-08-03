//! `bind_terminal_non_merk_tree` — **v1** (`GROVE_V4`+).
//!
//! Derives the tree's own state root from storage and rewrites the proof node
//! to `Node::KVValueHashFeatureTypeWithChildHash` carrying it. The merk
//! verifier then checks `combine_hash(H(value), child_hash) == value_hash`,
//! which is exactly the composition `insert_subtree` commits for these types —
//! so forged element bytes no longer verify against a genuine root hash.
//!
//! This differs from [`super::v0`] (which leaves the node untouched) in that it
//! both reads storage and mutates the node. Those extra reads are why it cannot
//! apply to the released versions; see the module docs in
//! [`super`][`mod@super`].
//!
//! Proof serving is latency-sensitive, so the common path does no hashing at
//! all: the `value_hash` the node already carries is the one the parent
//! committed, and is reused as-is. Only a node shape that carries no
//! `value_hash` has to derive one. The correctness of the derived state root is
//! checked by a `debug_assert` rather than at runtime — it can fire only on a
//! prover bug or corrupted storage, and the derivation is pinned by tests.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    proofs::Node,
    tree::{combine_hash, value_hash, NULL_HASH},
    CryptoHash, TreeFeatureType,
};
use grovedb_storage::{Storage, StorageContext};

use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// `bind_terminal_non_merk_tree` v1 — see the module documentation.
    pub(crate) fn bind_terminal_non_merk_tree_v1(
        &self,
        node: &mut Node,
        element: &Element,
        parent_path: &[&[u8]],
        tx: &Transaction,
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

        // Reuse the value_hash the node already carries — it is the one the
        // parent committed, so there is nothing to recompute. Proof serving is
        // latency-sensitive and this runs per terminal non-Merk tree, so the
        // common path must not hash.
        let (vh, ft) = match &*node {
            Node::KVValueHashFeatureType(_, _, vh, ft) => (*vh, *ft),
            Node::KVValueHash(_, _, vh) => (*vh, TreeFeatureType::BasicMerkNode),
            // A node shape that carries no value_hash to reuse (`KV`,
            // `KVCount`, `KVSum`, `KVCountSum`). Only here do we have to
            // derive it. Trees are proved with a value_hash-bearing node in
            // practice, so this is the cold path.
            _ => {
                let element_vh = value_hash(&value).unwrap_add_cost(&mut cost);
                let derived = combine_hash(&element_vh, &child_hash).unwrap_add_cost(&mut cost);
                (derived, TreeFeatureType::BasicMerkNode)
            }
        };

        // Drift check: the derived state root must reproduce the committed
        // value_hash, or the node we are about to emit would be rejected by the
        // verifier. Debug-only and deliberately uncosted — it can fire only on
        // a prover bug or corrupted storage, never on attacker input, and the
        // arms of `non_merk_tree_child_hash` are pinned by tests across all
        // four types, empty and populated. Keeping it out of release spares the
        // hot path two hashes; keeping it uncosted keeps `OperationCost`
        // identical between debug and release builds.
        #[cfg(debug_assertions)]
        {
            let element_vh = value_hash(&value).unwrap();
            let recomputed = combine_hash(&element_vh, &child_hash).unwrap();
            debug_assert_eq!(
                recomputed,
                vh,
                "non-Merk tree at key {} has state root {} which does not reproduce the \
                 committed value hash {}",
                hex::encode(&key),
                hex::encode(child_hash),
                hex::encode(vh),
            );
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
            Element::PrivateDocumentStore(total_count, entry_size, chunk_power, _) => {
                // The state root binds the committed config even when the
                // store is empty, so the empty case is the precomputed
                // config-parametrized root rather than NULL_HASH.
                if *total_count == 0 {
                    return Ok(
                        grovedb_private_document_store::empty_private_document_store_state_root(
                            *entry_size,
                            *chunk_power,
                        ),
                    )
                    .wrap_with_cost(cost);
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(storage_path, None, tx)
                    .unwrap_add_cost(&mut cost);
                let store = cost_return_on_error_no_add!(
                    cost,
                    grovedb_private_document_store::PrivateDocumentStore::from_state(
                        *total_count,
                        *entry_size,
                        *chunk_power,
                        storage_ctx,
                    )
                    .map_err(|e| Error::CorruptedData(format!(
                        "failed to open PrivateDocumentStore: {}",
                        e
                    )))
                );
                let state_root = cost_return_on_error_no_add!(
                    cost,
                    store.compute_current_state_root().map_err(|e| {
                        Error::CorruptedData(format!(
                            "private document store state root failed: {}",
                            e
                        ))
                    })
                );
                Ok(state_root).wrap_with_cost(cost)
            }
            _ => Err(Error::CorruptedCodeExecution(
                "non_merk_tree_child_hash called on an element that is not a non-Merk tree",
            ))
            .wrap_with_cost(cost),
        }
    }
}
