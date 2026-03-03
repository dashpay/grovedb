//! MMR tree operations for GroveDB.
//!
//! Provides methods to interact with MmrTree subtrees, which store append-only
//! authenticated data using a Merkle Mountain Range (MMR) backed by Blake3.
//!
//! MMR nodes are stored in data storage keyed by position. The MMR size is
//! tracked in the Element, and the MMR root hash is propagated as the Merk
//! child hash through the GroveDB hierarchy.

use std::collections::HashMap;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
use grovedb_merkle_mountain_range::{
    hash_count_for_push, mmr_size_to_leaf_count, MmrNode, MmrStore, MMR,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    util::TxRef,
    Element, Error, GroveDb, Merk, Transaction, TransactionArg,
};

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
            Element::MmrTree(size, flags) => (*size, flags.clone()),
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
        let mut mmr = MMR::new(mmr_size, &store);
        cost_return_on_error!(
            &mut cost,
            mmr.push(leaf)
                .map_err(|e| Error::CorruptedData(format!("MMR push failed: {}", e)))
        );

        // Get root BEFORE commit — data is still in the MMRBatch overlay
        let new_root = cost_return_on_error!(
            &mut cost,
            mmr.get_root()
                .map_err(|e| Error::CorruptedData(format!("MMR get_root failed: {}", e)))
        );
        let new_mmr_root = new_root.hash();
        let new_mmr_size = mmr.mmr_size;

        cost_return_on_error!(
            &mut cost,
            mmr.commit()
                .map_err(|e| Error::CorruptedData(format!("MMR commit failed: {}", e)))
        );

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

        let updated_element = Element::new_mmr_tree(new_mmr_size, existing_flags);

        // MMR root hash flows as the Merk child hash
        cost_return_on_error!(
            &mut cost,
            updated_element
                .insert_subtree(&mut parent_merk, key, new_mmr_root, None, grove_version,)
                .map_err(|e| e.into())
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
    /// Computes the root from the MMR data in storage. For an empty MMR,
    /// returns `[0u8; 32]`.
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
        let tx = TxRef::new(&self.db, transaction);

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );

        let mmr_size = match &element {
            Element::MmrTree(size, _) => *size,
            _ => {
                return Err(Error::InvalidInput("element is not an MMR tree")).wrap_with_cost(cost);
            }
        };

        if mmr_size == 0 {
            return Ok([0u8; 32]).wrap_with_cost(cost);
        }

        // Open data storage at subtree path and compute root from MMR
        let subtree_path_vec = self.build_subtree_path(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let store = MmrStore::new(&storage_ctx);
        let mmr = MMR::new(mmr_size, &store);

        let root = cost_return_on_error!(
            &mut cost,
            mmr.get_root()
                .map_err(|e| Error::CorruptedData(format!("MMR get_root failed: {}", e)))
        );

        Ok(root.hash()).wrap_with_cost(cost)
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
            Element::MmrTree(size, _) => *size,
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

        let store = MmrStore::new(&storage_ctx);
        let pos = grovedb_merkle_mountain_range::leaf_to_pos(leaf_index);

        use grovedb_merkle_mountain_range::MMRStoreReadOps;
        let store_ref: &MmrStore<_> = &store;
        let read_result = store_ref.element_at_position(pos);
        cost += read_result.cost;

        match read_result.value {
            Ok(Some(node)) => Ok(node.into_value()).wrap_with_cost(cost),
            Ok(None) => Ok(None).wrap_with_cost(cost),
            Err(e) => Err(Error::CorruptedData(format!(
                "failed to read MMR node: {}",
                e
            )))
            .wrap_with_cost(cost),
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
            Element::MmrTree(mmr_size, _) => {
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

        type TreePath = Vec<Vec<u8>>;

        // Group MMR tree append ops by path (which includes tree key).
        let mut mmr_groups: HashMap<TreePath, Vec<Vec<u8>>> = HashMap::new();

        for op in ops.iter() {
            if let GroveOp::MmrTreeAppend { value } = &op.op {
                let tree_path = op.path.to_path();
                mmr_groups.entry(tree_path).or_default().push(value.clone());
            }
        }

        // Process each group
        let mut replacements: HashMap<TreePath, QualifiedGroveDbOp> = HashMap::new();

        for (tree_path, values) in mmr_groups.iter() {
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
                Element::MmrTree(size, _) => *size,
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

            // Open immediate storage context (consistent with other preprocessors)
            let storage_ctx = self
                .db
                .get_immediate_storage_context(st_path, transaction)
                .unwrap_add_cost(&mut cost);

            let store = MmrStore::new(&storage_ctx);

            // Push all values into a single MMR instance
            let mut mmr = MMR::new(mmr_size, &store);
            for value in values {
                let leaf_count = mmr_size_to_leaf_count(mmr.mmr_size);
                cost.hash_node_calls += hash_count_for_push(leaf_count);

                let leaf = MmrNode::leaf(value.clone());
                cost_return_on_error!(
                    &mut cost,
                    mmr.push(leaf)
                        .map_err(|e| Error::CorruptedData(format!("MMR push failed: {}", e)))
                );
            }

            // Get root BEFORE commit — data is still in the MMRBatch overlay
            let new_root = cost_return_on_error!(
                &mut cost,
                mmr.get_root()
                    .map_err(|e| Error::CorruptedData(format!("MMR get_root failed: {}", e)))
            );
            let new_mmr_root = new_root.hash();
            let new_mmr_size = mmr.mmr_size;

            cost_return_on_error!(
                &mut cost,
                mmr.commit()
                    .map_err(|e| Error::CorruptedData(format!("MMR commit failed: {}", e)))
            );

            #[allow(clippy::drop_non_drop)]
            drop(storage_ctx);

            // Create a ReplaceNonMerkTreeRoot — MMR root flows as child hash
            // Key is restored for downstream (from_ops, execute_ops_on_path)
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec),
                key: Some(crate::batch::key_info::KeyInfo::KnownKey(key_bytes)),
                op: GroveOp::ReplaceNonMerkTreeRoot {
                    hash: new_mmr_root,
                    meta: crate::batch::NonMerkTreeMeta::MmrTree {
                        mmr_size: new_mmr_size,
                    },
                },
            };
            replacements.insert(tree_path.clone(), replacement);
        }

        // Build the new ops list: keep non-MMR ops, replace first MMR append op
        // per group with ReplaceTreeRootKey, skip the rest
        let mut first_seen: HashMap<TreePath, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::MmrTreeAppend { .. }) {
                let tree_path = op.path.to_path();
                if !first_seen.contains_key(&tree_path) {
                    first_seen.insert(tree_path.clone(), true);
                    if let Some(replacement) = replacements.remove(&tree_path) {
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
