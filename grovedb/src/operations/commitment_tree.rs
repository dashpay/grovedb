//! Commitment tree operations for GroveDB.
//!
//! Provides methods to interact with CommitmentTree subtrees, which combine a
//! GroveDB CountTree (for queryable items) with a lightweight Sinsemilla
//! frontier (for Orchard anchor computation).
//!
//! Items are stored as `cmx (32 bytes) || payload` with sequential u64 BE keys.
//! The Sinsemilla frontier is stored in aux storage (~1KB, O(1) append).
//!
//! Historical anchors for spend authorization are managed by Platform in a
//! separate provable tree — GroveDB only tracks the current frontier state.

use std::collections::HashMap;

use grovedb_commitment_tree::{Anchor, CommitmentFrontier};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    util::TxRef,
    Element, Error, GroveDb, Merk, Transaction, TransactionArg,
};

/// Key used to store the serialized commitment frontier data in aux storage.
pub(crate) const COMMITMENT_TREE_DATA_KEY: &[u8] = b"__ct_data__";

/// Load a `CommitmentFrontier` from aux storage, returning a new empty frontier
/// if no data exists.
fn load_frontier_from_aux<'db, C: StorageContext<'db>>(
    ctx: &C,
    cost: &mut OperationCost,
) -> Result<CommitmentFrontier, Error> {
    let aux_data = ctx.get_aux(COMMITMENT_TREE_DATA_KEY).unwrap_add_cost(cost);
    match aux_data {
        Ok(Some(bytes)) => CommitmentFrontier::deserialize(&bytes).map_err(|e| {
            Error::CorruptedData(format!("failed to deserialize commitment frontier: {}", e))
        }),
        Ok(None) => Ok(CommitmentFrontier::new()),
        Err(e) => Err(e.into()),
    }
}

impl GroveDb {
    /// Insert a note commitment into a CommitmentTree subtree.
    ///
    /// This is the primary write operation for CommitmentTree. It:
    /// 1. Appends the cmx to the Sinsemilla frontier (updating the anchor)
    /// 2. Inserts `cmx || payload` as an item in the underlying CountTree
    /// 3. Propagates changes through the GroveDB Merk hierarchy
    ///
    /// The `path` must point to the parent of the commitment tree key,
    /// and `key` must identify a CommitmentTree element.
    ///
    /// Returns `(sinsemilla_root, position)`: the new anchor hash and the
    /// 0-indexed position of the inserted note.
    pub fn commitment_tree_insert<'b, B, P>(
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
        if !element.is_commitment_tree() {
            return Err(Error::InvalidInput("element is not a commitment tree"))
                .wrap_with_cost(cost);
        }

        let existing_flags = match &element {
            Element::CommitmentTree(_, _, _, flags) => flags.clone(),
            _ => unreachable!(),
        };

        // 2. Build ct_path (the subtree path for the commitment tree itself)
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        // 3. Load frontier from aux, append cmx, get new root and position
        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path.clone(), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let mut frontier =
            cost_return_on_error_no_add!(cost, load_frontier_from_aux(&storage_ctx, &mut cost));

        let position = frontier.tree_size(); // next sequential position

        let new_sinsemilla_root = cost_return_on_error_no_add!(
            cost,
            frontier
                .append(cmx)
                .map_err(|e| Error::CommitmentTreeError(format!("append failed: {}", e)))
        );

        // 4. Save frontier back to aux
        let serialized = frontier.serialize();
        cost_return_on_error!(
            &mut cost,
            storage_ctx
                .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                .map_err(|e| e.into())
        );

        #[allow(clippy::drop_non_drop)]
        drop(storage_ctx);

        // 5. Create the item and insert into the subtree
        let item_key = position.to_be_bytes();
        let mut item_value = Vec::with_capacity(32 + payload.len());
        item_value.extend_from_slice(&cmx);
        item_value.extend_from_slice(&payload);
        let item_element = Element::new_item(item_value);

        let batch = StorageBatch::new();

        // Open the subtree Merk (the CommitmentTree's own Merk)
        let mut subtree_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                ct_path.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version,
            )
        );

        // Insert the item
        cost_return_on_error_into!(
            &mut cost,
            item_element.insert_if_not_exists(&mut subtree_merk, &item_key, None, grove_version,)
        );

        // Get the subtree's new root hash and aggregate data
        let (subtree_root_hash, subtree_root_key, subtree_aggregate_data) =
            cost_return_on_error_into!(&mut cost, subtree_merk.root_hash_key_and_aggregate_data());

        // Drop subtree Merk to release the storage context
        drop(subtree_merk);

        // 6. Open parent Merk and update the CommitmentTree element with all new fields
        //    at once (root_key, sinsemilla_root, count)
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
            subtree_root_key,
            new_sinsemilla_root,
            subtree_aggregate_data.as_count_u64(),
            existing_flags,
        );

        cost_return_on_error_into!(
            &mut cost,
            updated_element.insert_subtree(
                &mut parent_merk,
                key,
                subtree_root_hash,
                None,
                grove_version,
            )
        );

        // 7. Propagate changes from parent upward
        let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::new();
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

        // 8. Commit batch and transaction
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
        _grove_version: &GroveVersion,
    ) -> CostResult<Anchor, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        // Validate element type
        cost_return_on_error_no_add!(
            cost,
            self.validate_is_commitment_tree(
                path.clone(),
                key,
                transaction,
                _grove_version,
                &mut cost
            )
        );

        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let frontier =
            cost_return_on_error_no_add!(cost, load_frontier_from_aux(&storage_ctx, &mut cost));

        Ok(frontier.anchor()).wrap_with_cost(cost)
    }

    /// Build the subtree path for a commitment tree at path/key.
    fn build_ct_path<B: AsRef<[u8]>>(&self, path: &SubtreePath<B>, key: &[u8]) -> Vec<Vec<u8>> {
        let mut v = path.to_vec();
        v.push(key.to_vec());
        v
    }

    /// Verify that the element at `path/key` is a CommitmentTree.
    fn validate_is_commitment_tree<'b, B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
        cost: &mut OperationCost,
    ) -> Result<(), Error> {
        let element = self
            .get_raw_caching_optional(path, key, true, transaction, grove_version)
            .unwrap_add_cost(cost)?;
        if !element.is_commitment_tree() {
            return Err(Error::InvalidInput("element is not a commitment tree"));
        }
        Ok(())
    }

    /// Preprocess `CommitmentTreeInsert` ops in a batch.
    ///
    /// For each group of insert ops targeting the same (path, key):
    /// 1. Loads the Sinsemilla frontier from aux storage
    /// 2. Opens the subtree Merk and inserts all items
    /// 3. Saves the updated frontier to aux storage
    /// 4. Replaces the ops with a single `ReplaceTreeRootKey` carrying the new
    ///    sinsemilla_root
    ///
    /// The returned ops list contains no `CommitmentTreeInsert` variants.
    pub(crate) fn preprocess_commitment_tree_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
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

        /// Path + key pair identifying a commitment tree in a batch.
        type PathKey = (Vec<Vec<u8>>, Vec<u8>);

        // Group commitment tree insert ops by (path, key), preserving order.
        let mut ct_groups: HashMap<PathKey, Vec<([u8; 32], Vec<u8>)>> = HashMap::new();

        for op in ops.iter() {
            if let GroveOp::CommitmentTreeInsert { cmx, payload } = &op.op {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                ct_groups
                    .entry(path_key)
                    .or_default()
                    .push((*cmx, payload.clone()));
            }
        }

        // Create a batch for preprocessing Merk writes
        let preprocessing_batch = StorageBatch::new();

        // Process each group
        let mut replacements: HashMap<PathKey, QualifiedGroveDbOp> = HashMap::new();

        for (path_key, inserts) in ct_groups.iter() {
            let (path_vec, key_bytes) = path_key;

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
            if !element.is_commitment_tree() {
                return Err(Error::InvalidInput("element is not a commitment tree"))
                    .wrap_with_cost(cost);
            }

            // Build ct_path
            let mut ct_path_vec = path_vec.clone();
            ct_path_vec.push(key_bytes.clone());
            let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
            let ct_path = SubtreePath::from(ct_path_refs.as_slice());

            // Load frontier from aux
            let storage_ctx = self
                .db
                .get_immediate_storage_context(ct_path.clone(), transaction)
                .unwrap_add_cost(&mut cost);

            let mut frontier =
                cost_return_on_error_no_add!(cost, load_frontier_from_aux(&storage_ctx, &mut cost));

            // Open the subtree Merk for item inserts
            let mut subtree_merk = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    ct_path.clone(),
                    transaction,
                    Some(&preprocessing_batch),
                    grove_version,
                )
            );

            // Execute all inserts in order
            for (cmx, payload) in inserts {
                let position = frontier.tree_size();

                // Append to frontier
                cost_return_on_error_no_add!(
                    cost,
                    frontier
                        .append(*cmx)
                        .map_err(|e| Error::CommitmentTreeError(format!("append failed: {}", e)))
                );

                // Insert item into subtree Merk
                let item_key = position.to_be_bytes();
                let mut item_value = Vec::with_capacity(32 + payload.len());
                item_value.extend_from_slice(cmx);
                item_value.extend_from_slice(payload);
                let item_element = Element::new_item(item_value);

                cost_return_on_error_into!(
                    &mut cost,
                    item_element.insert_if_not_exists(
                        &mut subtree_merk,
                        &item_key,
                        None,
                        grove_version,
                    )
                );
            }

            // Save frontier back to aux
            let serialized = frontier.serialize();
            cost_return_on_error!(
                &mut cost,
                storage_ctx
                    .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                    .map_err(|e| e.into())
            );

            #[allow(clippy::drop_non_drop)]
            drop(storage_ctx);

            let new_sinsemilla_root = frontier.root_hash();

            // Get subtree root hash and aggregate data
            let (root_hash, root_key, aggregate_data) = cost_return_on_error_into!(
                &mut cost,
                subtree_merk.root_hash_key_and_aggregate_data()
            );
            drop(subtree_merk);

            // Create a ReplaceTreeRootKey with sinsemilla_root
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec.clone()),
                key: crate::batch::key_info::KeyInfo::KnownKey(key_bytes.clone()),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: root_hash,
                    root_key,
                    aggregate_data,
                    sinsemilla_root: Some(new_sinsemilla_root),
                },
            };
            replacements.insert(path_key.clone(), replacement);
        }

        // Commit the preprocessing batch (subtree Merk writes)
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(preprocessing_batch, Some(transaction))
                .map_err(Into::into)
        );

        // Build the new ops list: keep non-CT ops, replace first CT insert op
        // per group with ReplaceTreeRootKey, skip the rest
        let mut first_seen: HashMap<PathKey, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::CommitmentTreeInsert { .. }) {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                if !first_seen.contains_key(&path_key) {
                    first_seen.insert(path_key.clone(), true);
                    if let Some(replacement) = replacements.remove(&path_key) {
                        result.push(replacement);
                    }
                }
                // Skip subsequent CT ops for the same key
            } else {
                result.push(op);
            }
        }

        Ok(result).wrap_with_cost(cost)
    }
}
