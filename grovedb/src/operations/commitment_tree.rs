//! Commitment tree operations for GroveDB.
//!
//! Provides methods to interact with CommitmentTree subtrees, which combine a
//! BulkAppendTree (for efficient append-only storage of cmx||encrypted_note
//! payloads with chunk compaction) and a lightweight Sinsemilla frontier (for
//! Orchard anchor computation) in a single composite type.
//!
//! Items are stored as `cmx (32 bytes) || payload` in the BulkAppendTree data
//! namespace. The Sinsemilla frontier is also stored in data storage (~1KB,
//! O(1) append) under a reserved key (`COMMITMENT_TREE_DATA_KEY`).
//!
//! Historical anchors for spend authorization are managed by Platform in a
//! separate provable tree — GroveDB only tracks the current frontier state.

use std::collections::HashMap;

use grovedb_commitment_tree::{
    deserialize_chunk_blob, serialize_ciphertext, Anchor, CommitmentTree, DashMemo, MemoSize,
    TransmittedNoteCiphertext,
};
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

// ── Helpers ──────────────────────────────────────────────────────────────

/// Map a `CommitmentTreeError` to a GroveDB `Error`.
fn map_ct_err(e: grovedb_commitment_tree::CommitmentTreeError) -> Error {
    Error::CommitmentTreeError(format!("{}", e))
}

// ── Operations ──────────────────────────────────────────────────────────

impl GroveDb {
    /// Insert a note commitment into a CommitmentTree subtree.
    ///
    /// This is the primary typed write operation for CommitmentTree. It:
    /// 1. Opens the composite CommitmentTree (BulkAppendTree + frontier)
    /// 2. Serializes the ciphertext and appends `cmx || ciphertext` to the bulk
    ///    tree and `cmx` to the frontier
    /// 3. Saves the updated frontier to storage
    /// 4. Updates the CommitmentTree element with new sinsemilla_root +
    ///    total_count
    /// 5. Propagates changes through the GroveDB Merk hierarchy
    ///
    /// The `path` must point to the parent of the commitment tree key,
    /// and `key` must identify a CommitmentTree element.
    ///
    /// Returns `(sinsemilla_root, position)`: the new anchor hash and the
    /// 0-indexed position of the inserted note.
    pub fn commitment_tree_insert<'b, B, P, M: MemoSize>(
        &self,
        path: P,
        key: &[u8],
        cmx: [u8; 32],
        ciphertext: TransmittedNoteCiphertext<M>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<([u8; 32], u64), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let payload = serialize_ciphertext(&ciphertext);
        self.commitment_tree_insert_raw(path, key, cmx, payload, transaction, grove_version)
    }

    /// Insert a note commitment into a CommitmentTree subtree using raw payload
    /// bytes.
    ///
    /// This is the raw write operation used by batch preprocessing. The payload
    /// is validated against `DashMemo`'s expected size by `append_raw`.
    pub(crate) fn commitment_tree_insert_raw<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        cmx: [u8; 32],
        payload: Vec<u8>,
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

        // 1. Validate the element at path/key is a CommitmentTree
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let (total_count, chunk_power, existing_flags) = match &element {
            Element::CommitmentTree(_, total_count, chunk_power, flags) => {
                (*total_count, *chunk_power, flags.clone())
            }
            _ => {
                return Err(Error::InvalidInput("element is not a commitment tree"))
                    .wrap_with_cost(cost);
            }
        };

        // 2. Build subtree path and open storage context
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        // 3. Open composite CommitmentTree and append (uses default DashMemo for
        //    payload validation in append_raw)
        let mut ct = cost_return_on_error!(
            &mut cost,
            CommitmentTree::<_, DashMemo>::open(total_count, chunk_power, storage_ctx)
                .map(|r| r.map_err(map_ct_err))
        );

        let append_result = cost_return_on_error!(
            &mut cost,
            ct.append_raw(cmx, &payload).map(|r| r.map_err(map_ct_err))
        );

        // 4. Save frontier to storage
        cost_return_on_error!(&mut cost, ct.save().map(|r| r.map_err(map_ct_err)));

        let new_sinsemilla_root = append_result.sinsemilla_root;
        let bulk_state_root = append_result.bulk_state_root;
        let position = append_result.global_position;
        let new_total_count = ct.total_count();

        // Drop ct (and its storage context) before opening merk
        drop(ct);

        // 5. Update element in parent Merk
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

        let updated_element = Element::new_commitment_tree_with_all(
            new_sinsemilla_root,
            new_total_count,
            chunk_power,
            existing_flags,
        );

        cost_return_on_error_into!(
            &mut cost,
            updated_element.insert_subtree(
                &mut parent_merk,
                key,
                bulk_state_root,
                None,
                grove_version,
            )
        );

        // 6. Propagate changes from parent upward
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

        // 7. Commit batch and transaction
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local()
            .map(|()| (new_sinsemilla_root, position))
            .wrap_with_cost(cost)
    }

    /// Get the Orchard `Anchor` for a CommitmentTree subtree.
    ///
    /// Returns the anchor directly as an `orchard::tree::Anchor`, suitable for
    /// use in `orchard::builder::Builder` for spend authorization.
    pub fn commitment_tree_anchor<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Anchor, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        // Read element to get total_count and chunk_power
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let (total_count, chunk_power) = match &element {
            Element::CommitmentTree(_, tc, cp, _) => (*tc, *cp),
            _ => {
                return Err(Error::InvalidInput("element is not a commitment tree"))
                    .wrap_with_cost(cost);
            }
        };

        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let ct = cost_return_on_error!(
            &mut cost,
            CommitmentTree::<_, DashMemo>::open(total_count, chunk_power, storage_ctx)
                .map(|r| r.map_err(map_ct_err))
        );

        Ok(ct.anchor()).wrap_with_cost(cost)
    }

    /// Get a value from a CommitmentTree by its global 0-based position.
    ///
    /// Returns the raw `cmx || payload` bytes, or None if position is out of
    /// range.
    pub fn commitment_tree_get_value<'b, B, P>(
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
            Element::CommitmentTree(_, tc, cp, _) => (*tc, *cp),
            _ => {
                return Err(Error::InvalidInput("element is not a commitment tree"))
                    .wrap_with_cost(cost);
            }
        };

        if global_position >= total_count {
            return Ok(None).wrap_with_cost(cost);
        }

        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let ct = cost_return_on_error!(
            &mut cost,
            CommitmentTree::<_, DashMemo>::open(total_count, chunk_power, storage_ctx)
                .map(|r| r.map_err(map_ct_err))
        );

        let epoch_size = ct.epoch_size();
        let chunk_count = ct.chunk_count();
        let buffer_start = chunk_count * epoch_size;

        if global_position >= buffer_start {
            // Value is in the current buffer
            let buffer_pos = (global_position - buffer_start) as u16;
            let result = cost_return_on_error_no_add!(
                cost,
                ct.get_buffer_value(buffer_pos).map_err(map_ct_err)
            );
            Ok(result).wrap_with_cost(cost)
        } else {
            // Value is in a completed chunk
            let chunk_idx = global_position / epoch_size;
            let pos_in_chunk = (global_position % epoch_size) as usize;
            let blob = cost_return_on_error_no_add!(
                cost,
                ct.get_chunk_value(chunk_idx)
                    .map_err(map_ct_err)
                    .and_then(|opt| opt.ok_or_else(|| Error::CorruptedData(format!(
                        "missing chunk blob for index {}",
                        chunk_idx
                    ))))
            );
            let entries = cost_return_on_error_no_add!(
                cost,
                deserialize_chunk_blob(&blob).map_err(|e| Error::CorruptedData(format!("{}", e)))
            );
            Ok(entries.get(pos_in_chunk).cloned()).wrap_with_cost(cost)
        }
    }

    /// Get the total count of items in a CommitmentTree.
    pub fn commitment_tree_count<'b, B, P>(
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
            Element::CommitmentTree(_, total_count, ..) => Ok(total_count).wrap_with_cost(cost),
            _ => Err(Error::InvalidInput("element is not a commitment tree")).wrap_with_cost(cost),
        }
    }

    /// Build the subtree path for a commitment tree at path/key.
    fn build_ct_path<B: AsRef<[u8]>>(&self, path: &SubtreePath<B>, key: &[u8]) -> Vec<Vec<u8>> {
        let mut v = path.to_vec();
        v.push(key.to_vec());
        v
    }

    /// Preprocess `CommitmentTreeInsert` ops in a batch.
    ///
    /// For each group of insert ops targeting the same path:
    /// 1. Opens the composite CommitmentTree (BulkAppendTree + frontier)
    /// 2. Appends all items
    /// 3. Saves the updated frontier
    /// 4. Replaces the ops with a single `ReplaceTreeRootKey` carrying the new
    ///    sinsemilla_root and total_count
    ///
    /// The returned ops list contains no `CommitmentTreeInsert` variants.
    pub(crate) fn preprocess_commitment_tree_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        _batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        // Quick check: if no commitment tree ops, return as-is
        let has_ct_ops = ops
            .iter()
            .any(|op| matches!(op.op, GroveOp::CommitmentTreeInsert { .. }));
        if !has_ct_ops {
            return Ok(ops).wrap_with_cost(cost);
        }

        /// Tree path identifying a commitment tree in a batch (includes tree
        /// key as last segment).
        type TreePath = Vec<Vec<u8>>;

        // Group commitment tree insert ops by path (which includes tree key).
        let mut ct_groups: HashMap<TreePath, Vec<([u8; 32], Vec<u8>)>> = HashMap::new();

        for op in ops.iter() {
            if let GroveOp::CommitmentTreeInsert { cmx, payload } = &op.op {
                let tree_path = op.path.to_path();
                ct_groups
                    .entry(tree_path)
                    .or_default()
                    .push((*cmx, payload.clone()));
            }
        }

        // Process each group
        let mut replacements: HashMap<TreePath, QualifiedGroveDbOp> = HashMap::new();

        for (tree_path, inserts) in ct_groups.iter() {
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

            // Read existing element to verify it's a CommitmentTree
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

            let (total_count, chunk_power) = match &element {
                Element::CommitmentTree(_, tc, cp, _) => (*tc, *cp),
                _ => {
                    return Err(Error::InvalidInput("element is not a commitment tree"))
                        .wrap_with_cost(cost);
                }
            };

            // Build subtree path and open single storage context
            let mut ct_path_vec = path_vec.clone();
            ct_path_vec.push(key_bytes.clone());
            let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
            let ct_path = SubtreePath::from(ct_path_refs.as_slice());

            let storage_ctx = self
                .db
                .get_immediate_storage_context(ct_path, transaction)
                .unwrap_add_cost(&mut cost);

            // Open composite CommitmentTree
            let mut ct = cost_return_on_error!(
                &mut cost,
                CommitmentTree::<_, DashMemo>::open(total_count, chunk_power, storage_ctx)
                    .map(|r| r.map_err(map_ct_err))
            );

            // Execute all inserts in order
            for (cmx, payload) in inserts {
                cost_return_on_error!(
                    &mut cost,
                    ct.append_raw(*cmx, payload).map(|r| r.map_err(map_ct_err))
                );
            }

            // Save frontier to storage
            cost_return_on_error!(&mut cost, ct.save().map(|r| r.map_err(map_ct_err)));

            // Read state for the replacement op
            let bulk_state_root = cost_return_on_error_no_add!(
                cost,
                ct.compute_current_state_root().map_err(map_ct_err)
            );
            let new_sinsemilla_root = ct.root_hash();
            let current_total_count = ct.total_count();

            // Drop ct (and its storage context)
            drop(ct);

            // Create a ReplaceTreeRootKey with sinsemilla_root + bulk_state
            // Key is restored for downstream (from_ops, execute_ops_on_path)
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec),
                key: Some(crate::batch::key_info::KeyInfo::KnownKey(key_bytes)),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: bulk_state_root,
                    root_key: None,
                    aggregate_data: grovedb_merk::tree::AggregateData::NoAggregateData,
                    custom_root: Some(new_sinsemilla_root),
                    custom_count: Some(current_total_count),
                    bulk_state: Some((current_total_count, chunk_power)),
                },
            };
            replacements.insert(tree_path.clone(), replacement);
        }

        // Build the new ops list: keep non-CT ops, replace first CT insert op
        // per group with ReplaceTreeRootKey, skip the rest
        let mut first_seen: HashMap<TreePath, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::CommitmentTreeInsert { .. }) {
                let tree_path = op.path.to_path();
                if !first_seen.contains_key(&tree_path) {
                    first_seen.insert(tree_path.clone(), true);
                    if let Some(replacement) = replacements.remove(&tree_path) {
                        result.push(replacement);
                    }
                }
                // Skip subsequent CT ops for the same tree
            } else {
                result.push(op);
            }
        }

        Ok(result).wrap_with_cost(cost)
    }
}
