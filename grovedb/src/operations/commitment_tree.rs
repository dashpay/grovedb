//! Commitment tree operations for GroveDB.
//!
//! Provides methods to interact with CommitmentTree subtrees using the
//! Sinsemilla Merkle tree from the `grovedb-commitment-tree` crate.
//! Commitment tree data is stored in the subtree's auxiliary (aux) storage.

use std::collections::HashMap;

use grovedb_commitment_tree::{
    merkle_hash_from_bytes, Anchor, CommitmentTree, KvShardStore, MemKvStore, MerkleHashOrchard,
    MerklePath, Position, Retention,
};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::PrefixedRocksDbTransactionContext;
use grovedb_storage::{Storage, StorageBatch, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    util::TxRef,
    AggregateData, Element, Error, GroveDb, Merk, Transaction, TransactionArg,
};

/// Key used to store the serialized commitment tree data in aux storage.
const COMMITMENT_TREE_DATA_KEY: &[u8] = b"__ct_data__";

/// Maximum number of historical checkpoints to retain in commitment trees.
const DEFAULT_MAX_CHECKPOINTS: usize = 100;

type CtShardStore = KvShardStore<MemKvStore, MerkleHashOrchard>;
type Ct = CommitmentTree<CtShardStore>;

/// Load a `MemKvStore` from aux storage, returning a default empty store if
/// no data exists.
fn load_mem_store_from_aux<'db, C: StorageContext<'db>>(
    ctx: &C,
    cost: &mut OperationCost,
) -> Result<MemKvStore, Error> {
    let aux_data = ctx
        .get_aux(COMMITMENT_TREE_DATA_KEY)
        .unwrap_add_cost(cost);
    match aux_data {
        Ok(Some(bytes)) => MemKvStore::deserialize(&bytes).map_err(|e| {
            Error::CorruptedData(format!("failed to deserialize commitment tree: {}", e))
        }),
        Ok(None) => Ok(MemKvStore::new()),
        Err(e) => Err(e.into()),
    }
}

/// Create a `CommitmentTree` from a `MemKvStore`.
fn tree_from_mem_store(mem_store: MemKvStore) -> Ct {
    let kv_store = KvShardStore::new(mem_store);
    CommitmentTree::new(kv_store, DEFAULT_MAX_CHECKPOINTS)
}

impl GroveDb {
    /// Append a raw 32-byte leaf hash to a CommitmentTree subtree.
    ///
    /// The `path` must point to the parent of the commitment tree key,
    /// and `key` must identify a CommitmentTree element.
    /// `checkpoint_id` is a monotonically increasing identifier for this append.
    ///
    /// Returns `(root_hash, position)`: the new Sinsemilla root hash and the
    /// position of the appended leaf in the commitment tree.
    pub fn commitment_tree_append<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        leaf: [u8; 32],
        checkpoint_id: u64,
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

        // Verify the element at path/key is a CommitmentTree
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path.clone(), key, true, transaction, grove_version)
        );
        if !element.is_commitment_tree() {
            return Err(Error::InvalidInput("element is not a commitment tree"))
                .wrap_with_cost(cost);
        }

        // Build the subtree path for the commitment tree itself
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        // Load commitment tree data from aux storage
        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let mem_store = cost_return_on_error_no_add!(
            cost,
            load_mem_store_from_aux(&storage_ctx, &mut cost)
        );

        let mut tree = tree_from_mem_store(mem_store);

        // Convert bytes to MerkleHashOrchard
        let leaf_hash = cost_return_on_error_no_add!(
            cost,
            merkle_hash_from_bytes(&leaf).ok_or(Error::InvalidInput(
                "invalid commitment leaf: not a valid Pallas field element"
            ))
        );

        // Append and checkpoint
        cost_return_on_error_no_add!(
            cost,
            tree.append_raw(leaf_hash, Retention::Marked)
                .map_err(|e| Error::CommitmentTreeError(format!("append failed: {}", e)))
        );

        cost_return_on_error_no_add!(
            cost,
            tree.checkpoint(checkpoint_id)
                .map_err(|e| Error::CommitmentTreeError(format!("checkpoint failed: {}", e)))
        );

        // Get the new root hash and position
        let root_hash = cost_return_on_error_no_add!(
            cost,
            tree.root_hash()
                .map_err(|e| Error::CommitmentTreeError(format!("root hash failed: {}", e)))
        );

        let position = cost_return_on_error_no_add!(
            cost,
            tree.max_leaf_position()
                .map_err(|e| Error::CommitmentTreeError(format!("position query failed: {}", e)))
        )
        .expect("tree cannot be empty after successful append");

        // Save commitment tree data back to aux storage
        let mem_store = tree.into_store().into_inner();
        let serialized = mem_store.serialize();

        cost_return_on_error!(
            &mut cost,
            storage_ctx
                .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                .map_err(|e| e.into())
        );

        // Drop storage context to release borrow on self.db before Merk operations
        drop(storage_ctx);

        // Propagate Sinsemilla root hash through the GroveDB Merk hierarchy
        let batch = StorageBatch::new();

        // Open the parent Merk at `path` (where the CommitmentTree element lives)
        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version,
            )
        );

        // Extract existing root_key to preserve it (Sinsemilla ops don't change Merk root key)
        let existing_root_key = match &element {
            Element::CommitmentTree(rk, _) => rk.clone(),
            _ => unreachable!(),
        };

        // Update the CommitmentTree element with the new Sinsemilla root hash
        cost_return_on_error!(
            &mut cost,
            Self::update_tree_item_preserve_flag(
                &mut parent_merk,
                key,
                existing_root_key,
                root_hash,
                AggregateData::NoAggregateData,
                grove_version,
            )
        );

        // Propagate the change up to GroveDB root
        let mut merk_cache: HashMap<
            SubtreePath<B>,
            Merk<PrefixedRocksDbTransactionContext>,
        > = HashMap::new();
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

        // Commit batch (Merk propagation changes) and transaction
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local()
            .map(|()| (root_hash, u64::from(position)))
            .wrap_with_cost(cost)
    }

    /// Get the current Sinsemilla root hash of a CommitmentTree subtree.
    ///
    /// The `path` must point to the parent of the commitment tree key,
    /// and `key` must identify a CommitmentTree element.
    pub fn commitment_tree_root_hash<'b, B, P>(
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

        // Build the subtree path for the commitment tree
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let tree = cost_return_on_error_no_add!(
            cost,
            self.load_commitment_tree_inner(ct_path, tx.as_ref(), &mut cost)
        );

        let root_hash = cost_return_on_error_no_add!(
            cost,
            tree.root_hash()
                .map_err(|e| Error::CommitmentTreeError(format!("root hash failed: {}", e)))
        );

        Ok(root_hash).wrap_with_cost(cost)
    }

    /// Generate a Sinsemilla Merkle inclusion proof (witness) for the leaf
    /// at the given position in the commitment tree.
    ///
    /// Returns `None` if no witness can be generated (e.g., position pruned).
    /// The returned vector contains the 32-byte sibling hashes along the path.
    pub fn commitment_tree_witness<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        position: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<[u8; 32]>>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        // Build the subtree path for the commitment tree
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let tree = cost_return_on_error_no_add!(
            cost,
            self.load_commitment_tree_inner(ct_path, tx.as_ref(), &mut cost)
        );

        let witness = cost_return_on_error_no_add!(
            cost,
            tree.witness(Position::from(position))
                .map_err(|e| Error::CommitmentTreeError(format!("witness failed: {}", e)))
        );

        // Convert witness to serializable form: Vec of 32-byte sibling hashes
        let result = witness.map(|path| {
            path.path_elems()
                .iter()
                .map(|h| h.to_bytes())
                .collect::<Vec<[u8; 32]>>()
        });

        Ok(result).wrap_with_cost(cost)
    }

    /// Get the position of the last appended leaf in a CommitmentTree.
    ///
    /// Returns `None` if the tree is empty. The position is a zero-based
    /// index corresponding to the order in which leaves were appended.
    /// Platform uses this to track note positions for later witness generation.
    pub fn commitment_tree_current_end_position<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<u64>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        // Build the subtree path for the commitment tree
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let tree = cost_return_on_error_no_add!(
            cost,
            self.load_commitment_tree_inner(ct_path, tx.as_ref(), &mut cost)
        );

        let position = cost_return_on_error_no_add!(
            cost,
            tree.max_leaf_position()
                .map_err(|e| Error::CommitmentTreeError(format!("position query failed: {}", e)))
        );

        Ok(position.map(|p| u64::from(p))).wrap_with_cost(cost)
    }

    /// Get the Orchard `Anchor` for a CommitmentTree subtree.
    ///
    /// Returns the anchor directly as an `orchard::tree::Anchor`, avoiding
    /// byte-array conversions at the caller level. This is what Platform feeds
    /// into `orchard::builder::Builder` for spend authorization.
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

        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let tree = cost_return_on_error_no_add!(
            cost,
            self.load_commitment_tree_inner(ct_path, tx.as_ref(), &mut cost)
        );

        let anchor = cost_return_on_error_no_add!(
            cost,
            tree.anchor()
                .map_err(|e| Error::CommitmentTreeError(format!("anchor failed: {}", e)))
        );

        Ok(anchor).wrap_with_cost(cost)
    }

    /// Generate an Orchard `MerklePath` for the leaf at the given position.
    ///
    /// Returns the Orchard-specific Merkle path directly, suitable for use
    /// in `orchard::builder::Builder::add_spend`. Returns `None` if no
    /// witness can be generated (e.g., the position has been pruned).
    pub fn commitment_tree_orchard_witness<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        position: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<MerklePath>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let tree = cost_return_on_error_no_add!(
            cost,
            self.load_commitment_tree_inner(ct_path, tx.as_ref(), &mut cost)
        );

        let witness = cost_return_on_error_no_add!(
            cost,
            tree.orchard_witness(Position::from(position))
                .map_err(|e| Error::CommitmentTreeError(format!("witness failed: {}", e)))
        );

        Ok(witness).wrap_with_cost(cost)
    }

    /// Prepare everything needed for a spend in one call: `(Anchor, MerklePath)`.
    ///
    /// Loads the commitment tree once and returns both the anchor and the
    /// Merkle path for the given position, avoiding a double load when
    /// Platform needs both for `Builder::add_spend`.
    ///
    /// Returns `None` if no witness can be generated for the position.
    pub fn commitment_tree_prepare_spend<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        position: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<(Anchor, MerklePath)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        let tree = cost_return_on_error_no_add!(
            cost,
            self.load_commitment_tree_inner(ct_path, tx.as_ref(), &mut cost)
        );

        let anchor = cost_return_on_error_no_add!(
            cost,
            tree.anchor()
                .map_err(|e| Error::CommitmentTreeError(format!("anchor failed: {}", e)))
        );

        let witness = cost_return_on_error_no_add!(
            cost,
            tree.orchard_witness(Position::from(position))
                .map_err(|e| Error::CommitmentTreeError(format!("witness failed: {}", e)))
        );

        Ok(witness.map(|path| (anchor, path))).wrap_with_cost(cost)
    }

    /// Build the subtree path for a commitment tree at path/key.
    fn build_ct_path<B: AsRef<[u8]>>(&self, path: &SubtreePath<B>, key: &[u8]) -> Vec<Vec<u8>> {
        let mut v = path.to_vec();
        v.push(key.to_vec());
        v
    }

    /// Load a commitment tree from aux storage at the given subtree path.
    fn load_commitment_tree_inner<B: AsRef<[u8]>>(
        &self,
        ct_path: SubtreePath<B>,
        transaction: &crate::Transaction,
        cost: &mut OperationCost,
    ) -> Result<Ct, Error> {
        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path, transaction)
            .unwrap_add_cost(cost);

        let mem_store = load_mem_store_from_aux(&storage_ctx, cost)?;
        Ok(tree_from_mem_store(mem_store))
    }

    /// Preprocess `CommitmentTreeAppend` ops in a batch.
    ///
    /// For each group of appends targeting the same (path, key):
    /// 1. Loads the Sinsemilla tree from aux storage
    /// 2. Appends all leaves in order
    /// 3. Saves back to aux storage
    /// 4. Replaces the ops with a single `ReplaceTreeRootKey`
    ///
    /// The returned ops list contains no `CommitmentTreeAppend` variants.
    pub(crate) fn preprocess_commitment_tree_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        // Quick check: if no CommitmentTreeAppend ops, return as-is
        let has_ct_ops = ops
            .iter()
            .any(|op| matches!(op.op, GroveOp::CommitmentTreeAppend { .. }));
        if !has_ct_ops {
            return Ok(ops).wrap_with_cost(cost);
        }

        // Group CommitmentTreeAppend ops by (path, key), preserving order.
        // Key: (path_bytes, key_bytes) -> Vec of (leaf, checkpoint_id) in order
        let mut ct_groups: HashMap<(Vec<Vec<u8>>, Vec<u8>), Vec<([u8; 32], u64)>> = HashMap::new();
        // Track which indices are CommitmentTreeAppend ops
        let mut ct_indices: HashMap<(Vec<Vec<u8>>, Vec<u8>), Vec<usize>> = HashMap::new();

        for (i, op) in ops.iter().enumerate() {
            if let GroveOp::CommitmentTreeAppend {
                leaf,
                checkpoint_id,
            } = &op.op
            {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                ct_groups
                    .entry(path_key.clone())
                    .or_default()
                    .push((*leaf, *checkpoint_id));
                ct_indices.entry(path_key).or_default().push(i);
            }
        }

        // Process each group: execute Sinsemilla operations and produce ReplaceTreeRootKey
        let mut replacements: HashMap<(Vec<Vec<u8>>, Vec<u8>), QualifiedGroveDbOp> = HashMap::new();

        for (path_key, leaves) in ct_groups.iter() {
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

            let existing_root_key = match &element {
                Element::CommitmentTree(rk, _) => rk.clone(),
                _ => unreachable!(),
            };

            // Build ct_path and load Sinsemilla tree from aux
            let mut ct_path_vec = path_vec.clone();
            ct_path_vec.push(key_bytes.clone());
            let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
            let ct_path = SubtreePath::from(ct_path_refs.as_slice());

            let storage_ctx = self
                .db
                .get_immediate_storage_context(ct_path, transaction)
                .unwrap_add_cost(&mut cost);

            let mem_store = cost_return_on_error_no_add!(
                cost,
                load_mem_store_from_aux(&storage_ctx, &mut cost)
            );

            let mut tree = tree_from_mem_store(mem_store);

            // Append all leaves in order
            for (leaf, checkpoint_id) in leaves {
                let leaf_hash = cost_return_on_error_no_add!(
                    cost,
                    merkle_hash_from_bytes(leaf).ok_or(Error::InvalidInput(
                        "invalid commitment leaf: not a valid Pallas field element"
                    ))
                );

                cost_return_on_error_no_add!(
                    cost,
                    tree.append_raw(leaf_hash, Retention::Marked)
                        .map_err(|e| Error::CommitmentTreeError(format!(
                            "append failed: {}",
                            e
                        )))
                );

                cost_return_on_error_no_add!(
                    cost,
                    tree.checkpoint(*checkpoint_id)
                        .map_err(|e| Error::CommitmentTreeError(format!(
                            "checkpoint failed: {}",
                            e
                        )))
                );
            }

            // Get final root hash and save
            let root_hash = cost_return_on_error_no_add!(
                cost,
                tree.root_hash()
                    .map_err(|e| Error::CommitmentTreeError(format!("root hash failed: {}", e)))
            );

            let mem_store = tree.into_store().into_inner();
            let serialized = mem_store.serialize();

            cost_return_on_error!(
                &mut cost,
                storage_ctx
                    .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                    .map_err(|e| e.into())
            );

            drop(storage_ctx);

            // Create replacement op
            let replacement = QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(path_vec.clone()),
                key: crate::batch::key_info::KeyInfo::KnownKey(key_bytes.clone()),
                op: GroveOp::ReplaceTreeRootKey {
                    hash: root_hash,
                    root_key: existing_root_key,
                    aggregate_data: AggregateData::NoAggregateData,
                },
            };
            replacements.insert(path_key.clone(), replacement);
        }

        // Build the new ops list: keep non-CT ops, replace first CT op per group
        // with ReplaceTreeRootKey, skip the rest
        let mut first_seen: HashMap<(Vec<Vec<u8>>, Vec<u8>), bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            if let GroveOp::CommitmentTreeAppend { .. } = &op.op {
                let path_key = (op.path.to_path(), op.key.get_key_clone());
                if !first_seen.contains_key(&path_key) {
                    first_seen.insert(path_key.clone(), true);
                    // Replace first occurrence with the ReplaceTreeRootKey
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
