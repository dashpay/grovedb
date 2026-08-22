//! Root-maintenance version 0: no intermediate hashes.
//!
//! Every insert writes its slot and then re-derives the root by walking
//! every filled position out of storage — one read and two blake3 calls per
//! position — and a root read performs the same walk. Nothing but the values
//! is stored.
//!
//! Locked: GROVE_V1..V3 are released, the shielded pool has been charged this
//! walk on mainnet since activation, and a replayed block must be charged
//! what it was admitted under. Behaviour and cost here must not change.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;

use crate::{
    tree::{cost_return_on_error, DenseFixedSizedMerkleTree, SlotWriteAccounting},
    DenseMerkleError,
};

/// Write `value` at the next position and re-derive the root from every
/// filled position. The caller has checked the tree is not full.
pub(super) fn insert_next<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    value: &[u8],
    accounting: SlotWriteAccounting,
) -> CostResult<([u8; 32], u16), DenseMerkleError> {
    let mut cost = OperationCost::default();
    let position = tree.count();
    cost_return_on_error!(cost, tree.put_value(position, value, accounting));
    tree.set_count(position + 1);

    match tree.compute_root_hash().unwrap_add_cost(&mut cost) {
        Ok(root_hash) => Ok((root_hash, position)).wrap_with_cost(cost),
        // codecov:ignore — requires compute_root_hash to fail after put_value
        // succeeds, which needs a storage fault (get fails on a key that was
        // just written). Not reachable with any StorageContext implementation.
        Err(e) => {
            // Roll back count and cache so the tree state remains
            // consistent. The value remains in the store; the caller
            // is responsible for store-level cleanup if needed.
            tree.set_count(position);
            tree.uncache_value(position);
            Err(e).wrap_with_cost(cost)
        }
    }
}

/// Write `value` at the next position without deriving the root. The caller
/// has checked the tree is not full.
pub(super) fn insert_next_no_root<'db, S: StorageContext<'db>>(
    tree: &mut DenseFixedSizedMerkleTree<S>,
    value: &[u8],
    accounting: SlotWriteAccounting,
) -> CostResult<u16, DenseMerkleError> {
    let mut cost = OperationCost::default();
    let position = tree.count();
    cost_return_on_error!(cost, tree.put_value(position, value, accounting));
    tree.set_count(position + 1);
    Ok(position).wrap_with_cost(cost)
}

/// The root, walked from every filled position.
pub(super) fn root_hash<'db, S: StorageContext<'db>>(
    tree: &DenseFixedSizedMerkleTree<S>,
) -> CostResult<[u8; 32], DenseMerkleError> {
    tree.compute_root_hash()
}
