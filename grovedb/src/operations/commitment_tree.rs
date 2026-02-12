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
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    util::TxRef,
    AggregateData, Element, Error, GroveDb, Merk, Transaction, TransactionArg,
};

/// Key used to store the serialized commitment tree data in aux storage.
pub(crate) const COMMITMENT_TREE_DATA_KEY: &[u8] = b"__ct_data__";

/// Maximum number of historical checkpoints to retain in commitment trees.
const DEFAULT_MAX_CHECKPOINTS: usize = 1000;

type CtShardStore = KvShardStore<MemKvStore, MerkleHashOrchard>;
type Ct = CommitmentTree<CtShardStore>;

/// Load a `MemKvStore` from aux storage, returning a default empty store if
/// no data exists.
fn load_mem_store_from_aux<'db, C: StorageContext<'db>>(
    ctx: &C,
    cost: &mut OperationCost,
) -> Result<MemKvStore, Error> {
    let aux_data = ctx.get_aux(COMMITMENT_TREE_DATA_KEY).unwrap_add_cost(cost);
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
    ///
    /// Returns `(root_hash, position)`: the new Sinsemilla root hash and the
    /// position of the appended leaf in the commitment tree.
    pub fn commitment_tree_append<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        leaf: [u8; 32],
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

        let mem_store =
            cost_return_on_error_no_add!(cost, load_mem_store_from_aux(&storage_ctx, &mut cost));

        let mut tree = tree_from_mem_store(mem_store);

        // Convert bytes to MerkleHashOrchard
        let leaf_hash = cost_return_on_error_no_add!(
            cost,
            merkle_hash_from_bytes(&leaf).ok_or(Error::InvalidInput(
                "invalid commitment leaf: not a valid Pallas field element"
            ))
        );

        // Append leaf (no checkpoint -- checkpointing is done separately per block)
        cost_return_on_error_no_add!(
            cost,
            tree.append_raw(leaf_hash, Retention::Marked)
                .map_err(|e| Error::CommitmentTreeError(format!("append failed: {}", e)))
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
        );
        let position = cost_return_on_error_no_add!(
            cost,
            position.ok_or(Error::CorruptedData(
                "commitment tree reports empty after successful append".to_string()
            ))
        );

        // Save commitment tree data back to aux storage
        let mem_store = tree.into_store().into_inner();
        let serialized = mem_store.serialize();

        cost_return_on_error!(
            &mut cost,
            storage_ctx
                .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                .map_err(|e| e.into())
        );

        // End borrow on self.db before Merk operations
        #[allow(clippy::drop_non_drop)]
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

        // Extract existing root_key to preserve it (Sinsemilla ops don't change Merk
        // root key)
        let existing_root_key = match &element {
            Element::CommitmentTree(rk, _) => rk.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "element changed type between check and use".to_string(),
                ))
                .wrap_with_cost(cost);
            }
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

    /// Create a checkpoint in a CommitmentTree subtree.
    ///
    /// Checkpoints record tree state boundaries (typically one per block) so
    /// that witness generation can produce inclusion proofs relative to a
    /// known root. Checkpointing does **not** change the Sinsemilla root
    /// hash, so no Merk propagation is performed.
    ///
    /// `checkpoint_id` must be monotonically increasing across calls for the
    /// same commitment tree.
    pub fn commitment_tree_checkpoint<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        checkpoint_id: u64,
        transaction: TransactionArg,
        _grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
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

        // Build the subtree path for the commitment tree
        let ct_path_vec = self.build_ct_path(&path, key);
        let ct_path_refs: Vec<&[u8]> = ct_path_vec.iter().map(|v| v.as_slice()).collect();
        let ct_path = SubtreePath::from(ct_path_refs.as_slice());

        // Load commitment tree data from aux storage
        let storage_ctx = self
            .db
            .get_immediate_storage_context(ct_path, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let mem_store =
            cost_return_on_error_no_add!(cost, load_mem_store_from_aux(&storage_ctx, &mut cost));

        let mut tree = tree_from_mem_store(mem_store);

        // Create checkpoint
        cost_return_on_error_no_add!(
            cost,
            tree.checkpoint(checkpoint_id)
                .map_err(|e| Error::CommitmentTreeError(format!("checkpoint failed: {}", e)))
        );

        // Save commitment tree data back to aux storage
        let mem_store = tree.into_store().into_inner();
        let serialized = mem_store.serialize();

        cost_return_on_error!(
            &mut cost,
            storage_ctx
                .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                .map_err(|e| e.into())
        );

        // End borrow on self.db before committing transaction
        #[allow(clippy::drop_non_drop)]
        drop(storage_ctx);

        tx.commit_local().wrap_with_cost(cost)
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
        _grove_version: &GroveVersion,
    ) -> CostResult<[u8; 32], Error>
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
        _grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<[u8; 32]>>, Error>
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
        _grove_version: &GroveVersion,
    ) -> CostResult<Option<u64>, Error>
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

        Ok(position.map(u64::from)).wrap_with_cost(cost)
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
        _grove_version: &GroveVersion,
    ) -> CostResult<Option<MerklePath>, Error>
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

    /// Prepare everything needed for a spend in one call: `(Anchor,
    /// MerklePath)`.
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
        _grove_version: &GroveVersion,
    ) -> CostResult<Option<(Anchor, MerklePath)>, Error>
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

        let tree = cost_return_on_error_no_add!(
            cost,
            self.load_commitment_tree_inner(ct_path, tx.as_ref(), &mut cost)
        );

        let witness = cost_return_on_error_no_add!(
            cost,
            tree.orchard_witness(Position::from(position))
                .map_err(|e| Error::CommitmentTreeError(format!("witness failed: {}", e)))
        );

        let spend_data = match witness {
            Some(path) => {
                // Witnesses are generated against checkpoint depth 0, so compute the
                // anchor from that same checkpointed state to keep the pair consistent.
                let checkpoint_root = cost_return_on_error_no_add!(
                    cost,
                    tree.root_at_checkpoint_depth(Some(0)).map_err(|e| {
                        Error::CommitmentTreeError(format!("checkpoint root query failed: {}", e))
                    })
                );
                Some((Anchor::from(checkpoint_root), path))
            }
            None => None,
        };

        Ok(spend_data).wrap_with_cost(cost)
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

    /// Preprocess `CommitmentTreeAppend` and `CommitmentTreeCheckpoint` ops in
    /// a batch.
    ///
    /// For each group of append/checkpoint ops targeting the same (path, key):
    /// 1. Loads the Sinsemilla tree from aux storage
    /// 2. Appends all leaves in order (no per-leaf checkpoint)
    /// 3. Applies any checkpoint ops in order
    /// 4. Saves back to aux storage
    /// 5. Replaces the append ops with a single `ReplaceTreeRootKey`
    ///
    /// The returned ops list contains no `CommitmentTreeAppend` or
    /// `CommitmentTreeCheckpoint` variants.
    pub(crate) fn preprocess_commitment_tree_ops(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
        let mut cost = OperationCost::default();

        // Quick check: if no commitment tree ops, return as-is
        let has_ct_ops = ops.iter().any(|op| {
            matches!(
                op.op,
                GroveOp::CommitmentTreeAppend { .. } | GroveOp::CommitmentTreeCheckpoint { .. }
            )
        });
        if !has_ct_ops {
            return Ok(ops).wrap_with_cost(cost);
        }

        /// Internal enum to preserve ordering of appends and checkpoints.
        enum CtAction {
            Append([u8; 32]),
            Checkpoint(u64),
        }

        /// Path + key pair identifying a commitment tree in a batch.
        type PathKey = (Vec<Vec<u8>>, Vec<u8>);

        // Group commitment tree ops by (path, key), preserving order.
        let mut ct_groups: HashMap<PathKey, Vec<CtAction>> = HashMap::new();

        for op in ops.iter() {
            match &op.op {
                GroveOp::CommitmentTreeAppend { leaf } => {
                    let path_key = (op.path.to_path(), op.key.get_key_clone());
                    ct_groups
                        .entry(path_key)
                        .or_default()
                        .push(CtAction::Append(*leaf));
                }
                GroveOp::CommitmentTreeCheckpoint { checkpoint_id } => {
                    let path_key = (op.path.to_path(), op.key.get_key_clone());
                    ct_groups
                        .entry(path_key)
                        .or_default()
                        .push(CtAction::Checkpoint(*checkpoint_id));
                }
                _ => {}
            }
        }

        // Process each group: execute Sinsemilla operations and produce
        // ReplaceTreeRootKey
        let mut replacements: HashMap<PathKey, QualifiedGroveDbOp> = HashMap::new();
        // Track groups that only had checkpoint ops (no appends) -- those don't need
        // a ReplaceTreeRootKey since the root hash doesn't change.
        let mut checkpoint_only_groups: HashMap<PathKey, bool> = HashMap::new();

        for (path_key, actions) in ct_groups.iter() {
            let (path_vec, key_bytes) = path_key;

            let has_appends = actions.iter().any(|a| matches!(a, CtAction::Append(_)));

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
                _ => {
                    return Err(Error::CorruptedData(
                        "element changed type between check and use".to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
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

            // Execute all actions in order
            for action in actions {
                match action {
                    CtAction::Append(leaf) => {
                        let leaf_hash = cost_return_on_error_no_add!(
                            cost,
                            merkle_hash_from_bytes(leaf).ok_or(Error::InvalidInput(
                                "invalid commitment leaf: not a valid Pallas field element"
                            ))
                        );

                        cost_return_on_error_no_add!(
                            cost,
                            tree.append_raw(leaf_hash, Retention::Marked).map_err(|e| {
                                Error::CommitmentTreeError(format!("append failed: {}", e))
                            })
                        );
                    }
                    CtAction::Checkpoint(checkpoint_id) => {
                        cost_return_on_error_no_add!(
                            cost,
                            tree.checkpoint(*checkpoint_id).map_err(
                                |e| Error::CommitmentTreeError(format!("checkpoint failed: {}", e))
                            )
                        );
                    }
                }
            }

            // Save the modified tree back to aux storage
            let root_hash = if has_appends {
                // Root hash changed -- we need a ReplaceTreeRootKey
                let rh = cost_return_on_error_no_add!(
                    cost,
                    tree.root_hash()
                        .map_err(|e| Error::CommitmentTreeError(format!(
                            "root hash failed: {}",
                            e
                        )))
                );
                Some(rh)
            } else {
                // Checkpoint-only: no root hash change
                checkpoint_only_groups.insert(path_key.clone(), true);
                None
            };

            let mem_store = tree.into_store().into_inner();
            let serialized = mem_store.serialize();

            cost_return_on_error!(
                &mut cost,
                storage_ctx
                    .put_aux(COMMITMENT_TREE_DATA_KEY, &serialized, None)
                    .map_err(|e| e.into())
            );

            #[allow(clippy::drop_non_drop)]
            drop(storage_ctx);

            // Create replacement op only if there were appends
            if let Some(root_hash) = root_hash {
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
        }

        // Build the new ops list: keep non-CT ops, replace first CT append op per
        // group with ReplaceTreeRootKey, skip the rest of CT ops
        let mut first_seen: HashMap<PathKey, bool> = HashMap::new();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops.into_iter() {
            match &op.op {
                GroveOp::CommitmentTreeAppend { .. } => {
                    let path_key = (op.path.to_path(), op.key.get_key_clone());
                    if !first_seen.contains_key(&path_key) {
                        first_seen.insert(path_key.clone(), true);
                        // Replace first occurrence with the ReplaceTreeRootKey
                        if let Some(replacement) = replacements.remove(&path_key) {
                            result.push(replacement);
                        }
                    }
                    // Skip subsequent CT ops for the same key
                }
                GroveOp::CommitmentTreeCheckpoint { .. } => {
                    // Checkpoint ops are fully handled in preprocessing; skip
                    // them. If there were no appends for
                    // this group, just skip (aux was already
                    // updated above).
                }
                _ => {
                    result.push(op);
                }
            }
        }

        Ok(result).wrap_with_cost(cost)
    }
}
