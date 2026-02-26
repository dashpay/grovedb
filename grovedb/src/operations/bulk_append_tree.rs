//! BulkAppendTree operations for GroveDB.
//!
//! Thin bridge between GroveDB's storage/transaction/batch infrastructure
//! and the `grovedb-bulk-append-tree` crate which owns all pure data-structure
//! logic (buffer management, chunk compaction, MMR orchestration, hashing).

use std::collections::HashMap;

use grovedb_bulk_append_tree::{deserialize_chunk_blob, BulkAppendTree};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageBatch};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    util::TxRef,
    Element, Error, GroveDb, Transaction, TransactionArg,
};

/// Map a `BulkAppendError` to a GroveDB `Error`.
fn map_bulk_err(e: grovedb_bulk_append_tree::BulkAppendError) -> Error {
    Error::CorruptedData(format!("{}", e))
}

impl GroveDb {
    /// Append a value to a BulkAppendTree subtree.
    ///
    /// Auto-compacts when the buffer fills: serializes entries into a chunk
    /// blob, computes dense Merkle root, appends to chunk MMR, clears buffer.
    ///
    /// Returns `(state_root, global_position)` where global_position is the
    /// 0-based index of the appended value across all chunks and buffer.
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

        let (total_count, chunk_power, existing_flags) = match &element {
            Element::BulkAppendTree(tc, cp, flags) => (*tc, *cp, flags.clone()),
            _ => {
                return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                    .wrap_with_cost(cost);
            }
        };

        // 2. Open storage
        let subtree_path_vec = self.build_subtree_path_for_bulk(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        // 3. Load tree, append
        let mut tree = cost_return_on_error_no_add!(
            cost,
            BulkAppendTree::from_state(total_count, chunk_power, storage_ctx).map_err(map_bulk_err)
        );

        let result = cost_return_on_error_no_add!(cost, tree.append(&value).map_err(map_bulk_err));

        cost.hash_node_calls += result.hash_count;

        let new_state_root = result.state_root;
        let new_total_count = tree.total_count;

        // Drop tree (and its embedded storage context) before opening merk
        drop(tree);

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

        let updated_element =
            Element::new_bulk_append_tree(new_total_count, chunk_power, existing_flags);

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

        let (total_count, chunk_power) = match &element {
            Element::BulkAppendTree(tc, cp, _) => (*tc, *cp),
            _ => {
                return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                    .wrap_with_cost(cost);
            }
        };

        if global_position >= total_count {
            return Ok(None).wrap_with_cost(cost);
        }

        let subtree_path_vec = self.build_subtree_path_for_bulk(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let tree = cost_return_on_error_no_add!(
            cost,
            BulkAppendTree::from_state(total_count, chunk_power, storage_ctx).map_err(map_bulk_err)
        );

        let epoch_size = tree.epoch_size();
        let chunk_count = tree.chunk_count();
        let buffer_start = chunk_count * epoch_size;

        if global_position >= buffer_start {
            // Value is in the current buffer
            let buffer_pos = (global_position - buffer_start) as u16;
            let result = cost_return_on_error_no_add!(
                cost,
                tree.get_buffer_value(buffer_pos).map_err(map_bulk_err)
            );
            Ok(result).wrap_with_cost(cost)
        } else {
            // Value is in a completed chunk
            let chunk_idx = global_position / epoch_size;
            let pos_in_chunk = (global_position % epoch_size) as usize;
            let blob = cost_return_on_error_no_add!(
                cost,
                tree.get_chunk_value(chunk_idx)
                    .map_err(map_bulk_err)
                    .and_then(|opt| opt.ok_or_else(|| Error::CorruptedData(format!(
                        "missing chunk blob for index {}",
                        chunk_idx
                    ))))
            );
            let entries = cost_return_on_error_no_add!(
                cost,
                deserialize_chunk_blob(&blob).map_err(map_bulk_err)
            );
            Ok(entries.get(pos_in_chunk).cloned()).wrap_with_cost(cost)
        }
    }

    /// Get a completed chunk blob from a BulkAppendTree.
    ///
    /// Returns the raw serialized blob (length-prefixed entries) for the given
    /// chunk index, or None if the chunk hasn't been completed yet.
    pub fn bulk_get_chunk<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        chunk_index: u64,
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

        let (total_count, chunk_power) = match &element {
            Element::BulkAppendTree(tc, cp, _) => (*tc, *cp),
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

        let tree = cost_return_on_error_no_add!(
            cost,
            BulkAppendTree::from_state(total_count, chunk_power, storage_ctx).map_err(map_bulk_err)
        );

        let result = cost_return_on_error_no_add!(
            cost,
            tree.get_chunk_value(chunk_index).map_err(map_bulk_err)
        );

        Ok(result).wrap_with_cost(cost)
    }

    /// Get all current buffer entries from a BulkAppendTree.
    ///
    /// Returns entries that haven't been compacted into a chunk yet.
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

        let (total_count, chunk_power) = match &element {
            Element::BulkAppendTree(tc, cp, _) => (*tc, *cp),
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

        let tree = cost_return_on_error_no_add!(
            cost,
            BulkAppendTree::from_state(total_count, chunk_power, storage_ctx).map_err(map_bulk_err)
        );

        let buffer_count = tree.buffer_count();
        let mut entries = Vec::with_capacity(buffer_count as usize);
        for i in 0..buffer_count {
            let value = cost_return_on_error_no_add!(
                cost,
                tree.get_buffer_value(i)
                    .map_err(map_bulk_err)
                    .and_then(|opt| opt.ok_or_else(|| Error::CorruptedData(format!(
                        "missing buffer value at position {}",
                        i
                    ))))
            );
            entries.push(value);
        }

        Ok(entries).wrap_with_cost(cost)
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
            Element::BulkAppendTree(total_count, ..) => Ok(total_count).wrap_with_cost(cost),
            _ => Err(Error::InvalidInput("element is not a BulkAppendTree")).wrap_with_cost(cost),
        }
    }

    /// Get the number of completed chunks in a BulkAppendTree.
    pub fn bulk_chunk_count<'b, B, P>(
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
            Element::BulkAppendTree(total_count, chunk_power, _) => {
                Ok(total_count / (1u32 << chunk_power) as u64).wrap_with_cost(cost)
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

        type TreePath = Vec<Vec<u8>>;

        // Group by path (which includes tree key)
        let mut bulk_groups: HashMap<TreePath, Vec<Vec<u8>>> = HashMap::new();
        for op in ops.iter() {
            if let GroveOp::BulkAppend { value } = &op.op {
                let tree_path = op.path.to_path();
                bulk_groups
                    .entry(tree_path)
                    .or_default()
                    .push(value.clone());
            }
        }

        let mut replacements: HashMap<TreePath, QualifiedGroveDbOp> = HashMap::new();

        for (tree_path, values) in bulk_groups.iter() {
            // Extract parent path and tree key from the full path
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

            let (total_count, chunk_power, _flags) = match &element {
                Element::BulkAppendTree(tc, cp, flags) => (*tc, *cp, flags.clone()),
                _ => {
                    return Err(Error::InvalidInput("element is not a BulkAppendTree"))
                        .wrap_with_cost(cost);
                }
            };

            // Open immediate storage (for read-after-write visibility)
            let mut st_path_vec = path_vec.clone();
            st_path_vec.push(key_bytes.clone());
            let st_path_refs: Vec<&[u8]> = st_path_vec.iter().map(|v| v.as_slice()).collect();
            let st_path = SubtreePath::from(st_path_refs.as_slice());

            let storage_ctx = self
                .db
                .get_immediate_storage_context(st_path, transaction)
                .unwrap_add_cost(&mut cost);

            // Load tree with embedded storage
            let mut tree = cost_return_on_error_no_add!(
                cost,
                BulkAppendTree::from_state(total_count, chunk_power, storage_ctx)
                    .map_err(map_bulk_err)
            );

            // Process each value
            for value in values {
                let result =
                    cost_return_on_error_no_add!(cost, tree.append(value).map_err(map_bulk_err));
                cost.hash_node_calls += result.hash_count;
            }

            // Compute final state root
            let new_state_root = cost_return_on_error_no_add!(
                cost,
                tree.compute_current_state_root().map_err(map_bulk_err)
            );
            cost.hash_node_calls += 1;

            let current_total_count = tree.total_count;

            // Drop tree (and its embedded storage context)
            drop(tree);

            // Create replacement op
            // Key is restored for downstream (from_ops, execute_ops_on_path)
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec),
                key: Some(crate::batch::key_info::KeyInfo::KnownKey(key_bytes)),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: new_state_root,
                    root_key: None,
                    aggregate_data: grovedb_merk::tree::AggregateData::NoAggregateData,
                    custom_root: None,
                    custom_count: Some(current_total_count),
                    bulk_state: Some((current_total_count, chunk_power)),
                },
            };
            replacements.insert(tree_path.clone(), replacement);
        }

        // Build new ops list
        let mut first_seen: HashMap<TreePath, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::BulkAppend { .. }) {
                let tree_path = op.path.to_path();
                if !first_seen.contains_key(&tree_path) {
                    first_seen.insert(tree_path.clone(), true);
                    if let Some(replacement) = replacements.remove(&tree_path) {
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
