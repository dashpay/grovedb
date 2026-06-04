//! Caller-driven subtree-root replacement.
//!
//! Replaces a subtree leaf's child hash + element data inside its parent
//! Merk with caller-provided values, **without** re-reading the subtree to
//! compute the hash. This is the tail of every `*_insert` operation
//! (commitment_tree, mmr_tree, bulk_append_tree, dense_tree) factored out as
//! a single generic helper, intended for **snapshot-based bootstrap** only.
//!
//! # Safety contract
//!
//! The caller is responsible for ensuring `new_combined_root` actually
//! matches the subtree's underlying storage state. Intended pattern:
//!
//! 1. Ingest the subtree's raw storage out-of-band (typically via
//!    [`grovedb_storage::rocksdb_storage::RocksDbStorage::ingest_subtree_sst`]).
//! 2. Open a `StorageContext` at the subtree path and reconstruct the
//!    appropriate typed tree (CommitmentTree / Mmr / BulkAppend / Dense).
//! 3. Compute the post-ingest combined root via that tree's own root fn.
//! 4. Call [`GroveDb::replace_subtree_root`] with the verified hash and a
//!    correctly-constructed [`Element`] (e.g. `Element::new_commitment_tree`).
//!
//! Mismatches between `new_combined_root` and what re-reading the subtree
//! would compute produce an inconsistent Merk tree (the parent's recorded
//! child hash diverges from actual subtree state). This method does NOT
//! detect such mismatches at runtime; misuse is silent corruption.
//!
//! Gated behind the `unsafe-dump-load` Cargo feature.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::element::{
    insert::ElementInsertToStorageExtensions, tree_type::ElementTreeTypeExtensions,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageBatch};
use grovedb_version::version::GroveVersion;

use crate::{util::TxRef, Element, Error, GroveDb, TransactionArg};

impl GroveDb {
    /// Replace the child hash + element data of a subtree leaf in its parent
    /// Merk with caller-provided values. See the
    /// [module-level docs](self) for the safety contract.
    pub fn replace_subtree_root<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        new_element: Element,
        new_combined_root: [u8; 32],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();

        // Cheap sanity check: must be a tree-shaped Element (Item / Reference
        // have no child hash slot). Caller still owns hash-vs-state
        // correctness.
        if new_element.tree_type().is_none() {
            return Err(Error::InvalidInput(
                "replace_subtree_root: element is not a tree variant",
            ))
            .wrap_with_cost(cost);
        }

        let path: SubtreePath<B> = path.into();
        let tx = TxRef::new(&self.db, transaction);
        let batch = StorageBatch::new();

        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version,
            )
        );

        cost_return_on_error_into!(
            &mut cost,
            new_element.insert_subtree(
                &mut parent_merk,
                key,
                new_combined_root,
                None,
                grove_version,
            )
        );

        let mut merk_cache = HashMap::new();
        merk_cache.insert(path.clone(), parent_merk);

        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(
                merk_cache,
                path,
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }
}
