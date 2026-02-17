//! Commitment tree operations for GroveDB.
//!
//! Provides methods to interact with CommitmentTree subtrees, which combine a
//! BulkAppendTree (for efficient append-only storage of cmx||encrypted_note
//! payloads with epoch compaction) and a lightweight Sinsemilla frontier (for
//! Orchard anchor computation).
//!
//! Items are stored as `cmx (32 bytes) || payload` in the BulkAppendTree data
//! namespace. The Sinsemilla frontier is stored in aux storage (~1KB, O(1)
//! append). Both share the same subtree path but use different storage
//! namespaces (data vs aux), so there is no key collision.
//!
//! Historical anchors for spend authorization are managed by Platform in a
//! separate provable tree — GroveDB only tracks the current frontier state.

use std::{cell::RefCell, collections::HashMap};

use grovedb_bulk_append_tree::{BulkAppendTree, BulkStore, CachedBulkStore};
use grovedb_commitment_tree::{Anchor, CommitmentFrontier};
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

// ── Constants ───────────────────────────────────────────────────────────

/// Key used to store the serialized commitment frontier data in aux storage.
pub(crate) const COMMITMENT_TREE_DATA_KEY: &[u8] = b"__ct_data__";

// ── Storage adapters ────────────────────────────────────────────────────

/// Adapter implementing `BulkStore` for a GroveDB `StorageContext`.
///
/// Uses the data namespace (`get`/`put`/`delete`), NOT aux. This keeps
/// BulkAppendTree data separate from the Sinsemilla frontier (which uses aux).
struct DataBulkStore<'a, C> {
    ctx: &'a C,
    cost: RefCell<OperationCost>,
}

impl<'a, C> DataBulkStore<'a, C> {
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

impl<'db, C: StorageContext<'db>> BulkStore for DataBulkStore<'_, C> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let result = self.ctx.get(key);
        let mut c = self.cost.borrow_mut();
        match result.unwrap_add_cost(&mut c) {
            Ok(v) => Ok(v),
            Err(e) => Err(format!("{}", e)),
        }
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String> {
        let result = self.ctx.put(key, value, None, None);
        let mut c = self.cost.borrow_mut();
        match result.unwrap_add_cost(&mut c) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("{}", e)),
        }
    }

    fn delete(&self, key: &[u8]) -> Result<(), String> {
        let result = self.ctx.delete(key, None);
        let mut c = self.cost.borrow_mut();
        match result.unwrap_add_cost(&mut c) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("{}", e)),
        }
    }
}

/// Write-through caching wrapper for `StorageContext` aux operations.
///
/// Caches `get_aux` results at the raw byte level. `put_aux` writes through
/// to the underlying context and updates the cache, ensuring read-after-write
/// visibility even when the context defers writes to a batch.
struct CachedAuxContext<'a, 'db, C: StorageContext<'db>> {
    ctx: &'a C,
    cache: RefCell<HashMap<Vec<u8>, Option<Vec<u8>>>>,
    cost: RefCell<OperationCost>,
    _marker: std::marker::PhantomData<&'db ()>,
}

impl<'a, 'db, C: StorageContext<'db>> CachedAuxContext<'a, 'db, C> {
    fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            cache: RefCell::new(HashMap::new()),
            cost: RefCell::new(OperationCost::default()),
            _marker: std::marker::PhantomData,
        }
    }

    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        if let Some(cached) = self.cache.borrow().get(key) {
            return Ok(cached.clone());
        }
        let result = self
            .ctx
            .get_aux(key)
            .unwrap_add_cost(&mut self.cost.borrow_mut());
        match result {
            Ok(data) => {
                self.cache.borrow_mut().insert(key.to_vec(), data.clone());
                Ok(data)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        let result = self
            .ctx
            .put_aux(key, value, None)
            .unwrap_add_cost(&mut self.cost.borrow_mut());
        match result {
            Ok(()) => {
                self.cache
                    .borrow_mut()
                    .insert(key.to_vec(), Some(value.to_vec()));
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn take_cost(&self) -> OperationCost {
        self.cost.take()
    }
}

// ── Frontier helpers ────────────────────────────────────────────────────

/// Load a `CommitmentFrontier` from aux storage, returning a new empty frontier
/// if no data exists.
pub(crate) fn load_frontier_from_aux<'db, C: StorageContext<'db>>(
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

/// Load a `CommitmentFrontier` from a `CachedAuxContext`, returning a new empty
/// frontier if no data exists.
fn load_frontier_from_cached_aux<'db, C: StorageContext<'db>>(
    ctx: &CachedAuxContext<'_, 'db, C>,
) -> Result<CommitmentFrontier, Error> {
    match ctx.get_aux(COMMITMENT_TREE_DATA_KEY)? {
        Some(bytes) => CommitmentFrontier::deserialize(&bytes).map_err(|e| {
            Error::CorruptedData(format!("failed to deserialize commitment frontier: {}", e))
        }),
        None => Ok(CommitmentFrontier::new()),
    }
}

/// Map a `BulkAppendError` to a GroveDB `Error`.
fn map_bulk_err(e: grovedb_bulk_append_tree::BulkAppendError) -> Error {
    Error::CorruptedData(format!("{}", e))
}

// ── Operations ──────────────────────────────────────────────────────────

impl GroveDb {
    /// Insert a note commitment into a CommitmentTree subtree.
    ///
    /// This is the primary write operation for CommitmentTree. It:
    /// 1. Appends `cmx || payload` to the BulkAppendTree (data namespace)
    /// 2. Appends the cmx to the Sinsemilla frontier (aux namespace)
    /// 3. Updates the CommitmentTree element with new sinsemilla_root +
    ///    total_count
    /// 4. Propagates changes through the GroveDB Merk hierarchy
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

        let (total_count, epoch_size, existing_flags) = match &element {
            Element::CommitmentTree(_, _, tc, es, flags) => (*tc, *es, flags.clone()),
            _ => {
                return Err(Error::InvalidInput("element is not a commitment tree"))
                    .wrap_with_cost(cost);
            }
        };

        // 2. Build subtree path (shared by BulkAppendTree data + Sinsemilla aux)
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        // 3. Open storage context (data + aux namespaces)
        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path.clone(), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        // 4. Append item to BulkAppendTree (data namespace)
        let store = CachedBulkStore::new(DataBulkStore::new(&storage_ctx));
        let mut tree = cost_return_on_error_no_add!(
            cost,
            BulkAppendTree::load_from_store(&store, total_count, epoch_size).map_err(map_bulk_err)
        );

        let mut item_value = Vec::with_capacity(32 + payload.len());
        item_value.extend_from_slice(&cmx);
        item_value.extend_from_slice(&payload);

        let result = cost_return_on_error_no_add!(
            cost,
            tree.append(&store, &item_value).map_err(map_bulk_err)
        );
        cost.hash_node_calls += result.hash_count;
        cost += store.into_inner().take_cost();

        let position = result.global_position;
        let new_total_count = tree.total_count();

        // 5. Load Sinsemilla frontier from aux, append cmx, save back
        let mut frontier =
            cost_return_on_error_no_add!(cost, load_frontier_from_aux(&storage_ctx, &mut cost));

        // Track Sinsemilla hash count
        let ommer_hashes = frontier.position().map(|p| p.trailing_ones()).unwrap_or(0);
        cost.sinsemilla_hash_calls += 32 + ommer_hashes;

        let new_sinsemilla_root = cost_return_on_error_no_add!(
            cost,
            frontier
                .append(cmx)
                .map_err(|e| Error::CommitmentTreeError(format!("append failed: {}", e)))
        );

        let serialized = frontier.serialize();
        cost_return_on_error!(
            &mut cost,
            storage_ctx
                .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                .map_err(|e| e.into())
        );

        #[allow(clippy::drop_non_drop)]
        drop(storage_ctx);

        // 6. Update element in parent Merk
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
            None,
            new_sinsemilla_root,
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

        // 7. Propagate changes from parent upward
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

        let (total_count, epoch_size) = match &element {
            Element::CommitmentTree(_, _, tc, es, _) => (*tc, *es),
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

        let store = CachedBulkStore::new(DataBulkStore::new(&storage_ctx));
        let tree = cost_return_on_error_no_add!(
            cost,
            BulkAppendTree::from_state(total_count, epoch_size, 0, [0u8; 32]).map_err(map_bulk_err)
        );
        let result = cost_return_on_error_no_add!(
            cost,
            tree.get_value(&store, global_position)
                .map_err(map_bulk_err)
        );
        cost += store.into_inner().take_cost();

        Ok(result).wrap_with_cost(cost)
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
            Element::CommitmentTree(_, _, total_count, ..) => Ok(total_count).wrap_with_cost(cost),
            _ => Err(Error::InvalidInput("element is not a commitment tree")).wrap_with_cost(cost),
        }
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
    /// 2. Appends all items to the BulkAppendTree (data namespace)
    /// 3. Saves the updated frontier to aux storage
    /// 4. Replaces the ops with a single `ReplaceTreeRootKey` carrying the new
    ///    sinsemilla_root and total_count
    ///
    /// The returned ops list contains no `CommitmentTreeInsert` variants.
    pub(crate) fn preprocess_commitment_tree_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        batch: &StorageBatch,
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

            let (total_count, epoch_size) = match &element {
                Element::CommitmentTree(_, _, tc, es, _) => (*tc, *es),
                _ => {
                    return Err(Error::InvalidInput("element is not a commitment tree"))
                        .wrap_with_cost(cost);
                }
            };

            // Build subtree path (shared by data + aux)
            let mut ct_path_vec = path_vec.clone();
            ct_path_vec.push(key_bytes.clone());
            let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
            let ct_path = SubtreePath::from(ct_path_refs.as_slice());

            // Open transactional storage context
            let storage_ctx = self
                .db
                .get_transactional_storage_context(ct_path.clone(), Some(batch), transaction)
                .unwrap_add_cost(&mut cost);

            // Load BulkAppendTree from data namespace
            let store = CachedBulkStore::new(DataBulkStore::new(&storage_ctx));
            let mut tree = cost_return_on_error_no_add!(
                cost,
                BulkAppendTree::load_from_store(&store, total_count, epoch_size)
                    .map_err(map_bulk_err)
            );

            // Load existing buffer entries for in-memory tracking
            let mut mem_buffer: Vec<Vec<u8>> =
                cost_return_on_error_no_add!(cost, tree.get_buffer(&store).map_err(map_bulk_err));

            // Load Sinsemilla frontier from aux namespace
            let cached_aux = CachedAuxContext::new(&storage_ctx);
            let mut frontier =
                cost_return_on_error_no_add!(cost, load_frontier_from_cached_aux(&cached_aux));

            // Execute all inserts in order
            for (cmx, payload) in inserts {
                // Append to BulkAppendTree
                let mut item_value = Vec::with_capacity(32 + payload.len());
                item_value.extend_from_slice(cmx);
                item_value.extend_from_slice(payload);

                let result = cost_return_on_error_no_add!(
                    cost,
                    tree.append_with_mem_buffer(&store, &item_value, &mut mem_buffer)
                        .map_err(map_bulk_err)
                );
                cost.hash_node_calls += result.hash_count;

                // Append to Sinsemilla frontier
                let ommer_hashes = frontier.position().map(|p| p.trailing_ones()).unwrap_or(0);
                cost.sinsemilla_hash_calls += 32 + ommer_hashes;

                cost_return_on_error_no_add!(
                    cost,
                    frontier
                        .append(*cmx)
                        .map_err(|e| Error::CommitmentTreeError(format!("append failed: {}", e)))
                );
            }

            // Save BulkAppendTree metadata
            cost_return_on_error_no_add!(cost, tree.save_meta(&store).map_err(map_bulk_err));
            cost += store.into_inner().take_cost();

            // Save Sinsemilla frontier back to aux
            let serialized = frontier.serialize();
            cost_return_on_error_no_add!(
                cost,
                cached_aux.put_aux(COMMITMENT_TREE_DATA_KEY, &serialized)
            );
            cost += cached_aux.take_cost();

            let new_sinsemilla_root = frontier.root_hash();
            let current_total_count = tree.total_count();

            #[allow(clippy::drop_non_drop)]
            drop(storage_ctx);

            // Create a ReplaceTreeRootKey with sinsemilla_root + bulk_state
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec.clone()),
                key: crate::batch::key_info::KeyInfo::KnownKey(key_bytes.clone()),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: grovedb_merk::tree::NULL_HASH,
                    root_key: None,
                    aggregate_data: grovedb_merk::tree::AggregateData::NoAggregateData,
                    sinsemilla_root: Some(new_sinsemilla_root),
                    mmr_size: Some(current_total_count),
                    bulk_state: Some((current_total_count, epoch_size)),
                },
            };
            replacements.insert(path_key.clone(), replacement);
        }

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
