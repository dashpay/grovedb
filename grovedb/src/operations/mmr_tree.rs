//! MMR tree operations for GroveDB.
//!
//! Provides methods to interact with MmrTree subtrees, which store append-only
//! authenticated data using a Merkle Mountain Range (MMR) backed by Blake3.
//!
//! MMR nodes are stored in data storage keyed by position. The MMR root hash
//! and size are tracked in the Element itself and propagated through the
//! GroveDB Merk hierarchy.

use std::{cell::RefCell, collections::HashMap};

use ckb_merkle_mountain_range::{MMRStoreReadOps, MMRStoreWriteOps, MMR};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
use grovedb_mmr::{
    hash_count_for_push, mmr_node_key, mmr_size_to_leaf_count, MergeBlake3, MmrNode,
};
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

/// Storage adapter wrapping a GroveDB `StorageContext` for ckb MMR operations.
///
/// Reads and writes MMR nodes to data storage keyed by position.
/// Uses `RefCell` for cost accumulation and a write-through cache since ckb
/// traits take `&self`. The cache ensures nodes written by `append` are
/// immediately readable by `get_elem`, which is necessary when the underlying
/// storage context defers writes to a batch.
pub(crate) struct MmrStore<'a, C> {
    ctx: &'a C,
    cost: RefCell<OperationCost>,
    cache: RefCell<HashMap<u64, MmrNode>>,
}

impl<'a, C> MmrStore<'a, C> {
    pub fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            cost: RefCell::new(OperationCost::default()),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Take accumulated costs out of this store.
    pub fn take_cost(&self) -> OperationCost {
        self.cost.take()
    }
}

impl<'db, C: StorageContext<'db>> MMRStoreReadOps<MmrNode> for &MmrStore<'_, C> {
    fn get_elem(&self, pos: u64) -> ckb_merkle_mountain_range::Result<Option<MmrNode>> {
        // Check the write-through cache first
        if let Some(node) = self.cache.borrow().get(&pos) {
            return Ok(Some(node.clone()));
        }

        let key = mmr_node_key(pos);
        let result = self.ctx.get(&key);
        let mut cost = self.cost.borrow_mut();
        *cost += result.cost;
        match result.value {
            Ok(Some(bytes)) => {
                let node = MmrNode::deserialize(&bytes).map_err(|e| {
                    ckb_merkle_mountain_range::Error::StoreError(format!(
                        "deserialize node at pos {}: {}",
                        pos, e
                    ))
                })?;
                Ok(Some(node))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(ckb_merkle_mountain_range::Error::StoreError(format!(
                "get at pos {}: {}",
                pos, e
            ))),
        }
    }
}

impl<'db, C: StorageContext<'db>> MMRStoreWriteOps<MmrNode> for &MmrStore<'_, C> {
    fn append(&mut self, pos: u64, elems: Vec<MmrNode>) -> ckb_merkle_mountain_range::Result<()> {
        for (i, elem) in elems.into_iter().enumerate() {
            let node_pos = pos + i as u64;
            let key = mmr_node_key(node_pos);
            let serialized = elem.serialize().map_err(|e| {
                ckb_merkle_mountain_range::Error::StoreError(format!(
                    "serialize at pos {}: {}",
                    node_pos, e
                ))
            })?;
            let result = self.ctx.put(&key, &serialized, None, None);
            let mut cost = self.cost.borrow_mut();
            *cost += result.cost;
            result.value.map_err(|e| {
                ckb_merkle_mountain_range::Error::StoreError(format!(
                    "put at pos {}: {}",
                    node_pos, e
                ))
            })?;
            // Cache the original node directly for subsequent reads
            drop(cost);
            self.cache.borrow_mut().insert(node_pos, elem);
        }
        Ok(())
    }
}

impl GroveDb {
    /// Append a value to an MmrTree subtree.
    ///
    /// This is the primary write operation for MmrTree. It:
    /// 1. Loads existing MMR nodes from data storage
    /// 2. Pushes the new value (hashed with Blake3)
    /// 3. Commits new/modified nodes back to data storage
    /// 4. Updates the MmrTree element with the new root hash and size
    /// 5. Propagates changes through the GroveDB Merk hierarchy
    ///
    /// Returns `(mmr_root_hash, leaf_index)`.
    pub fn mmr_tree_append<'b, B, P>(
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

        // 1. Validate the element at path/key is an MmrTree
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let (mmr_size, existing_flags) = match &element {
            Element::MmrTree(_, _, size, flags) => (*size, flags.clone()),
            _ => {
                return Err(Error::InvalidInput("element is not an MMR tree")).wrap_with_cost(cost);
            }
        };

        // 2. Build subtree path (path + key)
        let subtree_path_vec = self.build_subtree_path(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        // 3. Open storage, create store adapter, push value
        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path.clone(), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let store = MmrStore::new(&storage_ctx);
        let leaf_count = mmr_size_to_leaf_count(mmr_size);

        // Track Blake3 hash cost for this push
        cost.hash_node_calls += hash_count_for_push(leaf_count);

        let leaf = MmrNode::leaf(value);
        let new_mmr_size;
        {
            let mut mmr = MMR::<MmrNode, MergeBlake3, _>::new(mmr_size, &store);
            cost_return_on_error_no_add!(
                cost,
                mmr.push(leaf)
                    .map_err(|e| { Error::CorruptedData(format!("MMR push failed: {}", e)) })
            );
            cost_return_on_error_no_add!(
                cost,
                mmr.commit()
                    .map_err(|e| { Error::CorruptedData(format!("MMR commit failed: {}", e)) })
            );
            new_mmr_size = mmr.mmr_size();
        }

        // Get new root hash
        let new_mmr = MMR::<MmrNode, MergeBlake3, _>::new(new_mmr_size, &store);
        let new_root = cost_return_on_error_no_add!(
            cost,
            new_mmr
                .get_root()
                .map_err(|e| { Error::CorruptedData(format!("MMR get_root failed: {}", e)) })
        );
        let new_mmr_root = new_root.hash;

        // Accumulate storage costs from the store
        cost += store.take_cost();

        #[allow(clippy::drop_non_drop)]
        drop(storage_ctx);

        // 4. Open parent Merk and update the MmrTree element
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

        let updated_element = Element::new_mmr_tree(new_mmr_root, new_mmr_size, existing_flags);

        // MmrTree has no child Merk, so use NULL_HASH as subtree root
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

        // 5. Propagate changes from parent upward
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

        // 6. Commit batch and transaction
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local()
            .map(|()| (new_mmr_root, leaf_count))
            .wrap_with_cost(cost)
    }

    /// Get the root hash of an MmrTree subtree.
    ///
    /// Reads the root hash directly from the Element (no storage access
    /// needed).
    pub fn mmr_tree_root_hash<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<[u8; 32], Error>
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
            Element::MmrTree(_, mmr_root, ..) => Ok(mmr_root).wrap_with_cost(cost),
            _ => Err(Error::InvalidInput("element is not an MMR tree")).wrap_with_cost(cost),
        }
    }

    /// Get a leaf value from an MmrTree by its 0-based leaf index.
    ///
    /// Reads the individual node from data storage at the leaf's MMR position.
    pub fn mmr_tree_get_value<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        leaf_index: u64,
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

        // Validate element type and check bounds
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let mmr_size = match &element {
            Element::MmrTree(_, _, size, _) => *size,
            _ => {
                return Err(Error::InvalidInput("element is not an MMR tree")).wrap_with_cost(cost);
            }
        };

        let leaf_count = mmr_size_to_leaf_count(mmr_size);
        if leaf_index >= leaf_count {
            return Ok(None).wrap_with_cost(cost);
        }

        // Build subtree path and read the node
        let subtree_path_vec = self.build_subtree_path(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let pos = grovedb_mmr::leaf_to_pos(leaf_index);
        let node_key = mmr_node_key(pos);
        let result = storage_ctx.get(&node_key).unwrap_add_cost(&mut cost);

        match result {
            Ok(Some(bytes)) => {
                let node = cost_return_on_error_no_add!(
                    cost,
                    MmrNode::deserialize(&bytes).map_err(|e| {
                        Error::CorruptedData(format!("failed to deserialize MMR node: {}", e))
                    })
                );
                Ok(node.value).wrap_with_cost(cost)
            }
            Ok(None) => Ok(None).wrap_with_cost(cost),
            Err(e) => Err(e.into()).wrap_with_cost(cost),
        }
    }

    /// Get the leaf count of an MmrTree subtree.
    ///
    /// Derives the count from the Element's mmr_size field.
    pub fn mmr_tree_leaf_count<'b, B, P>(
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
            Element::MmrTree(_, _, mmr_size, _) => {
                Ok(mmr_size_to_leaf_count(mmr_size)).wrap_with_cost(cost)
            }
            _ => Err(Error::InvalidInput("element is not an MMR tree")).wrap_with_cost(cost),
        }
    }

    /// Build the subtree path for a tree at path/key.
    fn build_subtree_path<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        key: &[u8],
    ) -> Vec<Vec<u8>> {
        let mut v = path.to_vec();
        v.push(key.to_vec());
        v
    }

    /// Preprocess `MmrTreeAppend` ops in a batch.
    ///
    /// For each group of append ops targeting the same (path, key):
    /// 1. Loads existing MMR from data storage
    /// 2. Pushes all values
    /// 3. Saves updated nodes to data storage
    /// 4. Replaces the ops with a single `ReplaceTreeRootKey` carrying the new
    ///    mmr_root and mmr_size
    ///
    /// The returned ops list contains no `MmrTreeAppend` variants.
    pub(crate) fn preprocess_mmr_tree_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        // Quick check: if no MMR tree ops, return as-is
        let has_mmr_ops = ops
            .iter()
            .any(|op| matches!(op.op, GroveOp::MmrTreeAppend { .. }));
        if !has_mmr_ops {
            return Ok(ops).wrap_with_cost(cost);
        }

        type PathKey = (Vec<Vec<u8>>, Vec<u8>);

        // Group MMR tree append ops by (path, key), preserving order.
        let mut mmr_groups: HashMap<PathKey, Vec<Vec<u8>>> = HashMap::new();

        for op in ops.iter() {
            if let GroveOp::MmrTreeAppend { value } = &op.op {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                mmr_groups.entry(path_key).or_default().push(value.clone());
            }
        }

        // Process each group
        let mut replacements: HashMap<PathKey, QualifiedGroveDbOp> = HashMap::new();

        for (path_key, values) in mmr_groups.iter() {
            let (path_vec, key_bytes) = path_key;

            // Read existing element to get mmr_size
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

            let mmr_size = match &element {
                Element::MmrTree(_, _, size, _) => *size,
                _ => {
                    return Err(Error::InvalidInput("element is not an MMR tree"))
                        .wrap_with_cost(cost);
                }
            };

            // Build subtree path for storage
            let mut st_path_vec = path_vec.clone();
            st_path_vec.push(key_bytes.clone());
            let st_path_refs: Vec<&[u8]> = st_path_vec.iter().map(|v| v.as_slice()).collect();
            let st_path = SubtreePath::from(st_path_refs.as_slice());

            // Open transactional storage context (reads from transaction)
            let storage_ctx = self
                .db
                .get_transactional_storage_context(st_path.clone(), Some(batch), transaction)
                .unwrap_add_cost(&mut cost);

            let store = MmrStore::new(&storage_ctx);

            // Push all values
            let mut current_mmr_size = mmr_size;
            for value in values {
                let leaf_count = mmr_size_to_leaf_count(current_mmr_size);
                cost.hash_node_calls += hash_count_for_push(leaf_count);

                let leaf = MmrNode::leaf(value.clone());
                let mut mmr = MMR::<MmrNode, MergeBlake3, _>::new(current_mmr_size, &store);
                cost_return_on_error_no_add!(
                    cost,
                    mmr.push(leaf)
                        .map_err(|e| { Error::CorruptedData(format!("MMR push failed: {}", e)) })
                );
                cost_return_on_error_no_add!(
                    cost,
                    mmr.commit()
                        .map_err(|e| { Error::CorruptedData(format!("MMR commit failed: {}", e)) })
                );
                current_mmr_size = mmr.mmr_size();
            }

            // Get new root hash
            let final_mmr = MMR::<MmrNode, MergeBlake3, _>::new(current_mmr_size, &store);
            let new_root = cost_return_on_error_no_add!(
                cost,
                final_mmr
                    .get_root()
                    .map_err(|e| { Error::CorruptedData(format!("MMR get_root failed: {}", e)) })
            );
            let new_mmr_root = new_root.hash;
            let new_mmr_size = current_mmr_size;

            // Accumulate storage costs
            cost += store.take_cost();

            #[allow(clippy::drop_non_drop)]
            drop(storage_ctx);

            // Create a ReplaceTreeRootKey with mmr_root and mmr_size
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec.clone()),
                key: crate::batch::key_info::KeyInfo::KnownKey(key_bytes.clone()),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: grovedb_merk::tree::NULL_HASH,
                    root_key: None,
                    aggregate_data: grovedb_merk::tree::AggregateData::NoAggregateData,
                    sinsemilla_root: Some(new_mmr_root),
                    mmr_size: Some(new_mmr_size),
                    bulk_state: None,
                },
            };
            replacements.insert(path_key.clone(), replacement);
        }

        // Build the new ops list: keep non-MMR ops, replace first MMR append op
        // per group with ReplaceTreeRootKey, skip the rest
        let mut first_seen: HashMap<PathKey, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::MmrTreeAppend { .. }) {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                if !first_seen.contains_key(&path_key) {
                    first_seen.insert(path_key.clone(), true);
                    if let Some(replacement) = replacements.remove(&path_key) {
                        result.push(replacement);
                    }
                }
                // Skip subsequent MMR ops for the same key
            } else {
                result.push(op);
            }
        }

        Ok(result).wrap_with_cost(cost)
    }
}
