//! PrivateDocumentStore operations for GroveDB.
//!
//! Thin bridge between GroveDB's storage/transaction/batch infrastructure and
//! the `grovedb-private-document-store` crate, which wraps a `BulkAppendTree`
//! with a committed `{entry_size, chunk_power}` configuration. Appends are
//! validated against the committed entry size and the state root binds the
//! config (`blake3("pds_state" || config_hash || bulk_state_root)`), so a
//! proof can never be reinterpreted under a different configuration.
//!
//! There is no per-entry delete or update — immutability is enforced by the
//! type. Every entry point here fails closed on protocol versions that
//! predate the element type (all slots are 0 before `GROVE_V4`).

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
use grovedb_path::SubtreePath;
use grovedb_private_document_store::PrivateDocumentStore;
use grovedb_storage::{Storage, StorageBatch};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    util::TxRef,
    Element, Error, GroveDb, Transaction, TransactionArg,
};

/// Map a `PrivateDocumentStoreError` to a GroveDB `Error`.
fn map_pds_err(e: grovedb_private_document_store::PrivateDocumentStoreError) -> Error {
    Error::PrivateDocumentStoreError(format!("{}", e))
}

/// Fail-closed capability gate for the PrivateDocumentStore family.
///
/// Slot `0` (every version before `GROVE_V4`) means the operation is
/// unavailable and returns a version-mismatch error; slot `1` is the active
/// v1 implementation. Unlike the `check_grovedb_v0!` family this rejects
/// *older* versions rather than newer ones — the element type must not be
/// creatable or operable under released protocol versions.
pub(crate) fn check_pds_enabled(
    method: &str,
    slot: grovedb_version::version::FeatureVersion,
) -> Result<(), Error> {
    if slot < 1 {
        return Err(GroveVersionError::UnknownVersionMismatch {
            method: method.to_string(),
            known_versions: vec![1],
            received: slot,
        }
        .into());
    }
    Ok(())
}

impl GroveDb {
    /// Append an entry to a PrivateDocumentStore subtree.
    ///
    /// The entry's byte length must equal the store's committed `entry_size`;
    /// any other length is rejected before mutation. The append:
    /// 1. Opens the store (a `BulkAppendTree` reconstructed from the
    ///    element's `total_count` / `chunk_power`)
    /// 2. Appends the entry (auto-compacting a full buffer into a chunk)
    /// 3. Updates the `PrivateDocumentStore` element with the new
    ///    `total_count` and re-binds the composite state root as the Merk
    ///    child hash
    /// 4. Propagates changes through the GroveDB Merk hierarchy
    ///
    /// `path` must point to the parent of the store's key, and `key` must
    /// identify a `PrivateDocumentStore` element.
    ///
    /// Returns `(state_root, position)`: the new composite state root and the
    /// 0-based global position of the appended entry.
    pub fn private_document_store_insert<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        entry: Vec<u8>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<([u8; 32], u64), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();

        cost_return_on_error_no_add!(
            cost,
            check_pds_enabled(
                "private_document_store_insert",
                grove_version
                    .grovedb_versions
                    .operations
                    .private_document_store
                    .insert,
            )
        );

        let tx = TxRef::new(&self.db, transaction);

        // 1. Validate the element at path/key is a PrivateDocumentStore.
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        // Look through NonCounted: a wrapped PrivateDocumentStore is still one.
        let (total_count, entry_size, chunk_power, existing_flags) = match element.underlying() {
            Element::PrivateDocumentStore(tc, es, cp, flags) => (*tc, *es, *cp, flags.clone()),
            _ => {
                return Err(Error::InvalidInput(
                    "element is not a private document store",
                ))
                .wrap_with_cost(cost);
            }
        };

        // 2. Open transactional storage (write-through cache + MMR overlay
        //    provide read-after-write visibility).
        let store_path_vec = self.build_pds_path(&path, key);
        let store_path_refs: Vec<&[u8]> = store_path_vec.iter().map(|v| v.as_slice()).collect();
        let store_path = SubtreePath::from(store_path_refs.as_slice());

        let data_batch = StorageBatch::new();
        let storage_ctx = self
            .db
            .get_transactional_storage_context(store_path, Some(&data_batch), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        // 3. Open the store and append (validates the entry size).
        let mut store = cost_return_on_error_no_add!(
            cost,
            PrivateDocumentStore::from_state(total_count, entry_size, chunk_power, storage_ctx)
                .map_err(map_pds_err)
        );

        let append_result = cost_return_on_error!(
            &mut cost,
            store.append(&entry).map(|r| r.map_err(map_pds_err))
        );

        let new_state_root = append_result.state_root;
        let position = append_result.global_position;
        let new_total_count = store.total_count();

        // Flush MMR overlay to storage (through the batch).
        cost_return_on_error_no_add!(cost, store.commit_mmr().map_err(map_pds_err));

        // Drop the store (and its storage context) before opening merk.
        drop(store);

        // Commit data batch to make writes visible in the transaction.
        // Note: this commits subtree data before the parent element update
        // below. If the parent Merk update fails, the subtree data is orphaned
        // in the transaction. This is the same pattern as other direct GroveDB
        // operations — the caller is expected to rollback the tx on error.
        // The batch path (preprocess_private_document_store_ops) avoids this
        // by using a shared StorageBatch that commits atomically with all
        // other ops.
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(data_batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        // 4. Update element in parent Merk.
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

        let updated_element = Element::new_private_document_store(
            new_total_count,
            entry_size,
            chunk_power,
            existing_flags,
        );

        cost_return_on_error_into!(
            &mut cost,
            updated_element.insert_subtree(
                &mut parent_merk,
                key,
                new_state_root,
                None,
                grove_version,
            )
        );

        // 5. Propagate changes from parent upward.
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

        // 6. Commit batch and transaction.
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local()
            .map(|()| (new_state_root, position))
            .wrap_with_cost(cost)
    }

    /// Get an entry from a PrivateDocumentStore by its global 0-based
    /// position.
    ///
    /// Returns the raw fixed-size entry bytes, or `None` if the position is
    /// out of range (`position >= total_count`).
    pub fn private_document_store_get_value<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        global_position: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<u8>>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();

        cost_return_on_error_no_add!(
            cost,
            check_pds_enabled(
                "private_document_store_get_value",
                grove_version
                    .grovedb_versions
                    .operations
                    .private_document_store
                    .get_value,
            )
        );

        let tx = TxRef::new(&self.db, transaction);

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        // Look through NonCounted: a wrapped PrivateDocumentStore is still one.
        let (total_count, entry_size, chunk_power) = match element.underlying() {
            Element::PrivateDocumentStore(tc, es, cp, _) => (*tc, *es, *cp),
            _ => {
                return Err(Error::InvalidInput(
                    "element is not a private document store",
                ))
                .wrap_with_cost(cost);
            }
        };

        if global_position >= total_count {
            return Ok(None).wrap_with_cost(cost);
        }

        let store_path_vec = self.build_pds_path(&path, key);
        let store_path_refs: Vec<&[u8]> = store_path_vec.iter().map(|v| v.as_slice()).collect();
        let store_path = SubtreePath::from(store_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_transactional_storage_context(store_path, None, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let store = cost_return_on_error_no_add!(
            cost,
            PrivateDocumentStore::from_state(total_count, entry_size, chunk_power, storage_ctx)
                .map_err(map_pds_err)
        );

        let value = cost_return_on_error_no_add!(
            cost,
            store.get_value(global_position).map_err(map_pds_err)
        );

        Ok(value).wrap_with_cost(cost)
    }

    /// Get the total count of entries in a PrivateDocumentStore.
    pub fn private_document_store_count<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<u64, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();

        cost_return_on_error_no_add!(
            cost,
            check_pds_enabled(
                "private_document_store_count",
                grove_version
                    .grovedb_versions
                    .operations
                    .private_document_store
                    .count,
            )
        );

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path, key, true, transaction, grove_version)
        );

        // Look through NonCounted: a wrapped PrivateDocumentStore is still one.
        match element.into_underlying() {
            Element::PrivateDocumentStore(total_count, ..) => Ok(total_count).wrap_with_cost(cost),
            _ => Err(Error::InvalidInput(
                "element is not a private document store",
            ))
            .wrap_with_cost(cost),
        }
    }

    /// Build the subtree path for a private document store at path/key.
    fn build_pds_path<B: AsRef<[u8]>>(&self, path: &SubtreePath<B>, key: &[u8]) -> Vec<Vec<u8>> {
        let mut v = path.to_vec();
        v.push(key.to_vec());
        v
    }

    /// Preprocess `PrivateDocumentStoreInsert` ops in a batch.
    ///
    /// For each group of insert ops targeting the same store:
    /// 1. Opens the store (BulkAppendTree + committed config)
    /// 2. Appends all entries in order (each validated against `entry_size`)
    /// 3. Replaces the ops with a single `ReplaceNonMerkTreeRoot` carrying
    ///    the new composite state root and updated element metadata
    ///
    /// The returned ops list contains no `PrivateDocumentStoreInsert`
    /// variants.
    pub(crate) fn preprocess_private_document_store_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        storage_batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        let has_pds_ops = ops
            .iter()
            .any(|op| matches!(op.op, GroveOp::PrivateDocumentStoreInsert { .. }));
        if !has_pds_ops {
            return Ok(ops).wrap_with_cost(cost);
        }

        cost_return_on_error_no_add!(
            cost,
            check_pds_enabled(
                "batch GroveOp::PrivateDocumentStoreInsert",
                grove_version
                    .grovedb_versions
                    .operations
                    .private_document_store
                    .insert,
            )
        );

        /// Tree path identifying a store in a batch (includes tree key as
        /// last segment).
        type TreePath = Vec<Vec<u8>>;

        // Group insert ops by path (which includes tree key).
        let mut pds_groups: HashMap<TreePath, Vec<Vec<u8>>> = HashMap::new();
        for op in ops.iter() {
            if let GroveOp::PrivateDocumentStoreInsert { entry } = &op.op {
                let tree_path = op.path.to_path();
                pds_groups.entry(tree_path).or_default().push(entry.clone());
            }
        }

        let mut replacements: HashMap<TreePath, QualifiedGroveDbOp> = HashMap::new();

        for (tree_path, entries) in pds_groups.iter() {
            // Extract parent path and tree key from the full path.
            let (path_vec, key_bytes) = {
                let mut p = tree_path.clone();
                let k = match p.pop() {
                    Some(k) => k,
                    None => {
                        return Err(Error::InvalidBatchOperation(
                            "append op path must have at least one segment",
                        ))
                        .wrap_with_cost(cost);
                    }
                };
                (p, k)
            };

            // Read the existing element to verify it's a PrivateDocumentStore.
            let path_slices: Vec<&[u8]> = path_vec.iter().map(|v| v.as_slice()).collect();
            let subtree_path = SubtreePath::from(path_slices.as_slice());

            let element = cost_return_on_error!(
                &mut cost,
                self.get_raw_caching_optional(
                    subtree_path.clone(),
                    key_bytes.as_slice(),
                    true,
                    Some(transaction),
                    grove_version
                )
            );

            // Look through NonCounted: a wrapped PrivateDocumentStore is
            // still one.
            let (total_count, entry_size, chunk_power) = match element.underlying() {
                Element::PrivateDocumentStore(tc, es, cp, _) => (*tc, *es, *cp),
                _ => {
                    return Err(Error::InvalidInput(
                        "element is not a private document store",
                    ))
                    .wrap_with_cost(cost);
                }
            };

            // Open transactional storage (write-through cache + MMR overlay
            // provide read-after-write visibility).
            let mut st_path_vec = path_vec.clone();
            st_path_vec.push(key_bytes.clone());
            let st_path_refs: Vec<&[u8]> = st_path_vec.iter().map(|v| v.as_slice()).collect();
            let st_path = SubtreePath::from(st_path_refs.as_slice());

            let storage_ctx = self
                .db
                .get_transactional_storage_context(st_path, Some(storage_batch), transaction)
                .unwrap_add_cost(&mut cost);

            let mut store = cost_return_on_error_no_add!(
                cost,
                PrivateDocumentStore::from_state(total_count, entry_size, chunk_power, storage_ctx)
                    .map_err(map_pds_err)
            );

            // Execute all inserts in order; each validates the entry size.
            let mut last_state_root = None;
            for entry in entries {
                let r = cost_return_on_error!(
                    &mut cost,
                    store.append(entry).map(|r| r.map_err(map_pds_err))
                );
                last_state_root = Some(r.state_root);
            }

            let new_state_root = match last_state_root {
                Some(root) => root,
                // Unreachable: groups only exist for at least one op.
                None => cost_return_on_error_no_add!(
                    cost,
                    store.compute_current_state_root().map_err(map_pds_err)
                ),
            };
            let current_total_count = store.total_count();

            // Flush MMR overlay to storage (through the batch).
            cost_return_on_error_no_add!(cost, store.commit_mmr().map_err(map_pds_err));

            // Drop the store (and its storage context).
            drop(store);

            // Create a ReplaceNonMerkTreeRoot carrying the new state root and
            // element metadata. Key is restored for downstream (from_ops,
            // execute_ops_on_path).
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec),
                key: Some(crate::batch::key_info::KeyInfo::KnownKey(key_bytes)),
                op: GroveOp::ReplaceNonMerkTreeRoot {
                    hash: new_state_root,
                    meta: crate::batch::NonMerkTreeMeta::PrivateDocumentStore {
                        total_count: current_total_count,
                        entry_size,
                        chunk_power,
                    },
                },
            };
            replacements.insert(tree_path.clone(), replacement);
        }

        // Build the new ops list: keep non-PDS ops, replace the first PDS
        // insert op per group with the replacement, skip the rest.
        let mut first_seen: HashMap<TreePath, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::PrivateDocumentStoreInsert { .. }) {
                let tree_path = op.path.to_path();
                if !first_seen.contains_key(&tree_path) {
                    first_seen.insert(tree_path.clone(), true);
                    if let Some(replacement) = replacements.remove(&tree_path) {
                        result.push(replacement);
                    }
                }
                // Skip subsequent PDS ops for the same store.
            } else {
                result.push(op);
            }
        }

        Ok(result).wrap_with_cost(cost)
    }
}
