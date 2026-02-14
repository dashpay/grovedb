//! BulkAppendTree operations for GroveDB.
//!
//! Thin bridge between GroveDB's storage/transaction/batch infrastructure
//! and the `grovedb-bulk-append-tree` crate which owns all pure data-structure
//! logic (buffer management, epoch compaction, MMR orchestration, hashing).

use std::{cell::RefCell, collections::HashMap};

use grovedb_bulk_append_tree::{BulkAppendTree, BulkStore};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageBatch, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    util::TxRef,
    Element, Error, GroveDb, Transaction, TransactionArg,
};

// ── Storage adapter ─────────────────────────────────────────────────────

/// Adapter implementing `BulkStore` for a GroveDB `StorageContext`.
///
/// Wraps `get_aux`/`put_aux`/`delete_aux` calls and accumulates their
/// `OperationCost` in a `RefCell` for later retrieval.
struct AuxBulkStore<'a, C> {
    ctx: &'a C,
    cost: RefCell<OperationCost>,
}

impl<'a, C> AuxBulkStore<'a, C> {
    fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            cost: RefCell::new(OperationCost::default()),
        }
    }

    fn take_cost(&self) -> OperationCost {
        self.cost.take()
    }
}

impl<'db, C: StorageContext<'db>> BulkStore for AuxBulkStore<'_, C> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let result = self.ctx.get_aux(key);
        let mut c = self.cost.borrow_mut();
        match result.unwrap_add_cost(&mut c) {
            Ok(v) => Ok(v),
            Err(e) => Err(format!("{}", e)),
        }
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String> {
        let result = self.ctx.put_aux(key, value, None);
        let mut c = self.cost.borrow_mut();
        match result.unwrap_add_cost(&mut c) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("{}", e)),
        }
    }

    fn delete(&self, key: &[u8]) -> Result<(), String> {
        let result = self.ctx.delete_aux(key, None);
        let mut c = self.cost.borrow_mut();
        match result.unwrap_add_cost(&mut c) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("{}", e)),
        }
    }
}

/// Map a `BulkAppendError` to a GroveDB `Error`.
fn map_bulk_err(e: grovedb_bulk_append_tree::BulkAppendError) -> Error {
    Error::CorruptedData(format!("{}", e))
}

impl GroveDb {
    /// Append a value to a BulkAppendTree subtree.
    ///
    /// Auto-compacts when the buffer fills: serializes entries into an epoch
    /// blob, computes dense Merkle root, appends to epoch MMR, clears buffer.
    ///
    /// Returns `(state_root, global_position)` where global_position is the
    /// 0-based index of the appended value across all epochs and buffer.
    pub fn bulk_append<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        value: Vec<u8>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<([u8; 32], u64), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        // 1. Validate element
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let (total_count, epoch_size, existing_flags) = match &element {
            Element::BulkAppendTree(_, _, tc, es, flags) => (*tc, *es, flags.clone()),
            _ => {
                return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                    .wrap_with_cost(cost);
            }
        };

        // 2. Open aux storage
        let subtree_path_vec = self.build_subtree_path_for_bulk(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        // 3. Load tree, append, persist
        let store = AuxBulkStore::new(&storage_ctx);
        let mut tree = cost_return_on_error_no_add!(
            cost,
            BulkAppendTree::load_from_store(&store, total_count, epoch_size).map_err(map_bulk_err)
        );

        let result =
            cost_return_on_error_no_add!(cost, tree.append(&store, &value).map_err(map_bulk_err));

        cost.hash_node_calls += result.hash_count;
        cost += store.take_cost();

        let new_state_root = result.state_root;
        let new_total_count = tree.total_count();

        #[allow(clippy::drop_non_drop)]
        drop(storage_ctx);

        // 4. Update element in parent Merk
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

        let updated_element = Element::new_bulk_append_tree(
            new_state_root,
            new_total_count,
            epoch_size,
            existing_flags,
        );

        cost_return_on_error_into!(
            &mut cost,
            updated_element.insert_subtree(
                &mut parent_merk,
                key,
                grovedb_merk::tree::NULL_HASH,
                None,
                grove_version,
            )
        );

        // 5. Propagate changes
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

        // 6. Commit
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local()
            .map(|()| (new_state_root, total_count))
            .wrap_with_cost(cost)
    }

    /// Get a value from a BulkAppendTree by its global 0-based position.
    ///
    /// Transparently reads from either a completed epoch blob or the current
    /// buffer depending on the position.
    pub fn bulk_get_value<'b, B, P>(
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
        let tx = TxRef::new(&self.db, transaction);

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let (total_count, epoch_size) = match &element {
            Element::BulkAppendTree(_, _, tc, es, _) => (*tc, *es),
            _ => {
                return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                    .wrap_with_cost(cost);
            }
        };

        let subtree_path_vec = self.build_subtree_path_for_bulk(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let store = AuxBulkStore::new(&storage_ctx);
        let tree = BulkAppendTree::from_state(total_count, epoch_size, 0, [0u8; 32]);
        let result = cost_return_on_error_no_add!(
            cost,
            tree.get_value(&store, global_position)
                .map_err(map_bulk_err)
        );
        cost += store.take_cost();

        Ok(result).wrap_with_cost(cost)
    }

    /// Get a completed epoch blob from a BulkAppendTree.
    ///
    /// Returns the raw serialized blob (length-prefixed entries) for the given
    /// epoch index, or None if the epoch hasn't been completed yet.
    pub fn bulk_get_epoch<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        epoch_index: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<u8>>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let (total_count, epoch_size) = match &element {
            Element::BulkAppendTree(_, _, tc, es, _) => (*tc, *es),
            _ => {
                return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                    .wrap_with_cost(cost);
            }
        };

        let subtree_path_vec = self.build_subtree_path_for_bulk(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let store = AuxBulkStore::new(&storage_ctx);
        let tree = BulkAppendTree::from_state(total_count, epoch_size, 0, [0u8; 32]);
        let result = cost_return_on_error_no_add!(
            cost,
            tree.get_epoch(&store, epoch_index).map_err(map_bulk_err)
        );
        cost += store.take_cost();

        Ok(result).wrap_with_cost(cost)
    }

    /// Get all current buffer entries from a BulkAppendTree.
    ///
    /// Returns entries that haven't been compacted into an epoch yet.
    pub fn bulk_get_buffer<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<Vec<u8>>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let (total_count, epoch_size) = match &element {
            Element::BulkAppendTree(_, _, tc, es, _) => (*tc, *es),
            _ => {
                return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                    .wrap_with_cost(cost);
            }
        };

        let subtree_path_vec = self.build_subtree_path_for_bulk(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let store = AuxBulkStore::new(&storage_ctx);
        let tree = BulkAppendTree::from_state(total_count, epoch_size, 0, [0u8; 32]);
        let result =
            cost_return_on_error_no_add!(cost, tree.get_buffer(&store).map_err(map_bulk_err));
        cost += store.take_cost();

        Ok(result).wrap_with_cost(cost)
    }

    /// Get the total count of values in a BulkAppendTree.
    pub fn bulk_count<'b, B, P>(
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

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path, key, true, transaction, grove_version)
        );

        match element {
            Element::BulkAppendTree(_, _, total_count, ..) => Ok(total_count).wrap_with_cost(cost),
            _ => Err(Error::InvalidInput("element is not a BulkAppendTree")).wrap_with_cost(cost),
        }
    }

    /// Get the number of completed epochs in a BulkAppendTree.
    pub fn bulk_epoch_count<'b, B, P>(
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

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path, key, true, transaction, grove_version)
        );

        match element {
            Element::BulkAppendTree(_, _, total_count, epoch_size, _) => {
                Ok(total_count / epoch_size as u64).wrap_with_cost(cost)
            }
            _ => Err(Error::InvalidInput("element is not a BulkAppendTree")).wrap_with_cost(cost),
        }
    }

    /// Build subtree path for a BulkAppendTree at path/key.
    fn build_subtree_path_for_bulk<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        key: &[u8],
    ) -> Vec<Vec<u8>> {
        let mut v = path.to_vec();
        v.push(key.to_vec());
        v
    }

    /// Preprocess `BulkAppend` ops in a batch.
    ///
    /// Groups ops by (path, key), executes all appends (including compactions)
    /// via the `grovedb-bulk-append-tree` crate, then replaces them with
    /// `ReplaceTreeRootKey` ops carrying the final state_root and total_count.
    pub(crate) fn preprocess_bulk_append_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        let has_bulk_ops = ops
            .iter()
            .any(|op| matches!(op.op, GroveOp::BulkAppend { .. }));
        if !has_bulk_ops {
            return Ok(ops).wrap_with_cost(cost);
        }

        type PathKey = (Vec<Vec<u8>>, Vec<u8>);

        // Group by (path, key)
        let mut bulk_groups: HashMap<PathKey, Vec<Vec<u8>>> = HashMap::new();
        for op in ops.iter() {
            if let GroveOp::BulkAppend { value } = &op.op {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                bulk_groups.entry(path_key).or_default().push(value.clone());
            }
        }

        let mut replacements: HashMap<PathKey, QualifiedGroveDbOp> = HashMap::new();

        for (path_key, values) in bulk_groups.iter() {
            let (path_vec, key_bytes) = path_key;

            // Read existing element
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

            let (total_count, epoch_size, _flags) = match &element {
                Element::BulkAppendTree(_, _, tc, es, flags) => (*tc, *es, flags.clone()),
                _ => {
                    return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                        .wrap_with_cost(cost);
                }
            };

            // Open transactional storage
            let mut st_path_vec = path_vec.clone();
            st_path_vec.push(key_bytes.clone());
            let st_path_refs: Vec<&[u8]> = st_path_vec.iter().map(|v| v.as_slice()).collect();
            let st_path = SubtreePath::from(st_path_refs.as_slice());

            let storage_ctx = self
                .db
                .get_transactional_storage_context(st_path, Some(batch), transaction)
                .unwrap_add_cost(&mut cost);

            let store = AuxBulkStore::new(&storage_ctx);

            // Load tree from store
            let mut tree = cost_return_on_error_no_add!(
                cost,
                BulkAppendTree::load_from_store(&store, total_count, epoch_size)
                    .map_err(map_bulk_err)
            );

            // Load existing buffer entries for in-memory tracking
            let mut mem_buffer: Vec<Vec<u8>> =
                cost_return_on_error_no_add!(cost, tree.get_buffer(&store).map_err(map_bulk_err));

            // Process each value
            for value in values {
                let result = cost_return_on_error_no_add!(
                    cost,
                    tree.append_with_mem_buffer(&store, value, &mut mem_buffer)
                        .map_err(map_bulk_err)
                );
                cost.hash_node_calls += result.hash_count;
            }

            // Save final metadata
            cost_return_on_error_no_add!(cost, tree.save_meta(&store).map_err(map_bulk_err));

            // Compute final state root
            let new_state_root = cost_return_on_error_no_add!(
                cost,
                tree.compute_current_state_root(&store)
                    .map_err(map_bulk_err)
            );
            cost.hash_node_calls += 1;

            // Accumulate storage costs
            cost += store.take_cost();

            let current_total_count = tree.total_count();

            #[allow(clippy::drop_non_drop)]
            drop(storage_ctx);

            // Create replacement op
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec.clone()),
                key: crate::batch::key_info::KeyInfo::KnownKey(key_bytes.clone()),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: grovedb_merk::tree::NULL_HASH,
                    root_key: None,
                    aggregate_data: grovedb_merk::tree::AggregateData::NoAggregateData,
                    sinsemilla_root: Some(new_state_root),
                    mmr_size: Some(current_total_count),
                    bulk_state: Some((current_total_count, epoch_size)),
                },
            };
            replacements.insert(path_key.clone(), replacement);
        }

        // Build new ops list
        let mut first_seen: HashMap<PathKey, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::BulkAppend { .. }) {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                if !first_seen.contains_key(&path_key) {
                    first_seen.insert(path_key.clone(), true);
                    if let Some(replacement) = replacements.remove(&path_key) {
                        result.push(replacement);
                    }
                }
            } else {
                result.push(op);
            }
        }

        Ok(result).wrap_with_cost(cost)
    }
}
