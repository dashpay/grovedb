//! Dense fixed-sized Merkle tree operations for GroveDB.
//!
//! Provides methods to interact with DenseAppendOnlyFixedSizeTree subtrees,
//! which store values in a complete binary tree of height h with 2^h - 1
//! positions, filled sequentially in level-order (BFS).
//!
//! Node values are stored in the data namespace keyed by position. The root
//! hash and count are tracked in the Element itself and propagated through
//! the GroveDB Merk hierarchy.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_dense_fixed_sized_merkle_tree::{position_key, DenseFixedSizedMerkleTree};
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

impl GroveDb {
    /// Insert a value into a DenseAppendOnlyFixedSizeTree subtree.
    ///
    /// Returns `(root_hash, position)`.
    pub fn dense_tree_insert<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        value: Vec<u8>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<([u8; 32], u16), Error>
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

        let (existing_count, height, existing_flags) = match &element {
            Element::DenseAppendOnlyFixedSizeTree(count, h, flags) => (*count, *h, flags.clone()),
            _ => {
                return Err(Error::InvalidInput("element is not a dense tree"))
                    .wrap_with_cost(cost);
            }
        };

        // 2. Build subtree path
        let subtree_path_vec = self.build_subtree_path_for_dense_tree(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        // 3. Open storage, create tree with embedded storage, insert
        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path.clone(), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let mut tree = cost_return_on_error_no_add!(
            cost,
            DenseFixedSizedMerkleTree::from_state(height, existing_count, storage_ctx)
                .map_err(|e| Error::CorruptedData(format!("dense tree state error: {}", e)))
        );

        let (new_root_hash, position) = cost_return_on_error!(
            &mut cost,
            tree.insert(&value)
                .map_err(|e| Error::CorruptedData(format!("dense tree insert failed: {}", e)))
        );

        let new_count = tree.count();

        // Drop tree (and its embedded storage context) before opening merk
        drop(tree);

        // 4. Update element and propagate
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

        let updated_element = Element::new_dense_tree(new_count, height, existing_flags);

        cost_return_on_error_into!(
            &mut cost,
            updated_element.insert_subtree(
                &mut parent_merk,
                key,
                new_root_hash,
                None,
                grove_version,
            )
        );

        // 5. Propagate changes
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

        // 6. Commit
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local()
            .map(|()| (new_root_hash, position))
            .wrap_with_cost(cost)
    }

    /// Get a value from a DenseAppendOnlyFixedSizeTree by position.
    pub fn dense_tree_get<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        position: u16,
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

        let count = match &element {
            Element::DenseAppendOnlyFixedSizeTree(count, ..) => *count,
            _ => {
                return Err(Error::InvalidInput("element is not a dense tree"))
                    .wrap_with_cost(cost);
            }
        };

        if position >= count {
            return Ok(None).wrap_with_cost(cost);
        }

        let subtree_path_vec = self.build_subtree_path_for_dense_tree(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        // Read directly from storage using position_key (no need to construct tree)
        let pos_key = position_key(position);
        let result = storage_ctx.get(pos_key).unwrap_add_cost(&mut cost);

        match result {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())).wrap_with_cost(cost),
            Ok(None) => Ok(None).wrap_with_cost(cost),
            Err(e) => Err(e.into()).wrap_with_cost(cost),
        }
    }

    /// Get the root hash of a DenseAppendOnlyFixedSizeTree.
    ///
    /// Computes the root hash from storage (same pattern as MmrTree).
    pub fn dense_tree_root_hash<'b, B, P>(
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

        let (count, height) = match &element {
            Element::DenseAppendOnlyFixedSizeTree(count, h, _) => (*count, *h),
            _ => {
                return Err(Error::InvalidInput("element is not a dense tree"))
                    .wrap_with_cost(cost);
            }
        };

        if count == 0 {
            return Ok([0u8; 32]).wrap_with_cost(cost);
        }

        let subtree_path_vec = self.build_subtree_path_for_dense_tree(&path, key);
        let subtree_path_refs: Vec<&[u8]> = subtree_path_vec.iter().map(|v| v.as_slice()).collect();
        let subtree_path = SubtreePath::from(subtree_path_refs.as_slice());

        let storage_ctx = self
            .db
            .get_immediate_storage_context(subtree_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let tree = cost_return_on_error_no_add!(
            cost,
            DenseFixedSizedMerkleTree::from_state(height, count, storage_ctx)
                .map_err(|e| Error::CorruptedData(format!("dense tree state error: {}", e)))
        );

        let root_hash = cost_return_on_error!(
            &mut cost,
            tree.root_hash()
                .map_err(|e| Error::CorruptedData(format!("dense tree root hash error: {}", e)))
        );

        Ok(root_hash).wrap_with_cost(cost)
    }

    /// Get the count of a DenseAppendOnlyFixedSizeTree.
    pub fn dense_tree_count<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<u16, Error>
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
            Element::DenseAppendOnlyFixedSizeTree(count, ..) => Ok(count).wrap_with_cost(cost),
            _ => Err(Error::InvalidInput("element is not a dense tree")).wrap_with_cost(cost),
        }
    }

    /// Build the subtree path for a dense tree at path/key.
    fn build_subtree_path_for_dense_tree<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        key: &[u8],
    ) -> Vec<Vec<u8>> {
        let mut v = path.to_vec();
        v.push(key.to_vec());
        v
    }

    /// Preprocess `DenseTreeInsert` ops in a batch.
    ///
    /// For each group of insert ops targeting the same (path, key):
    /// 1. Loads existing tree state from storage
    /// 2. Inserts all values sequentially
    /// 3. Replaces the ops with a single `ReplaceTreeRootKey` carrying the new
    ///    root_hash and count
    pub(crate) fn preprocess_dense_tree_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        let has_dense_ops = ops
            .iter()
            .any(|op| matches!(op.op, GroveOp::DenseTreeInsert { .. }));
        if !has_dense_ops {
            return Ok(ops).wrap_with_cost(cost);
        }

        type TreePath = Vec<Vec<u8>>;

        let mut groups: HashMap<TreePath, Vec<Vec<u8>>> = HashMap::new();

        for op in ops.iter() {
            if let GroveOp::DenseTreeInsert { value } = &op.op {
                let tree_path = op.path.to_path();
                groups.entry(tree_path).or_default().push(value.clone());
            }
        }

        let mut replacements: HashMap<TreePath, QualifiedGroveDbOp> = HashMap::new();

        for (tree_path, values) in groups.iter() {
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

            let (existing_count, height) = match &element {
                Element::DenseAppendOnlyFixedSizeTree(count, h, _) => (*count, *h),
                _ => {
                    return Err(Error::InvalidInput("element is not a dense tree"))
                        .wrap_with_cost(cost);
                }
            };

            // Build subtree path for storage
            let mut st_path_vec = path_vec.clone();
            st_path_vec.push(key_bytes.clone());
            let st_path_refs: Vec<&[u8]> = st_path_vec.iter().map(|v| v.as_slice()).collect();
            let st_path = SubtreePath::from(st_path_refs.as_slice());

            // Use immediate storage for read-after-write visibility
            let storage_ctx = self
                .db
                .get_immediate_storage_context(st_path.clone(), transaction)
                .unwrap_add_cost(&mut cost);

            let mut tree = cost_return_on_error_no_add!(
                cost,
                DenseFixedSizedMerkleTree::from_state(height, existing_count, storage_ctx)
                    .map_err(|e| Error::CorruptedData(format!("dense tree state error: {}", e)))
            );

            let mut new_root_hash = [0u8; 32];
            for value in values {
                let (hash, _pos) = cost_return_on_error!(
                    &mut cost,
                    tree.insert(value).map_err(|e| {
                        Error::CorruptedData(format!("dense tree insert failed: {}", e))
                    })
                );
                new_root_hash = hash;
            }

            let new_count = tree.count();

            // Drop tree (and its embedded storage context)
            drop(tree);

            // Key is restored for downstream (from_ops, execute_ops_on_path)
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec),
                key: Some(crate::batch::key_info::KeyInfo::KnownKey(key_bytes)),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: new_root_hash,
                    root_key: None,
                    aggregate_data: grovedb_merk::tree::AggregateData::NoAggregateData,
                    custom_root: None,
                    custom_count: Some(new_count as u64),
                    bulk_state: None,
                },
            };
            replacements.insert(tree_path.clone(), replacement);
        }

        // Build new ops list
        let mut first_seen: HashMap<TreePath, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if matches!(op.op, GroveOp::DenseTreeInsert { .. }) {
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
