//! Storage adapter bridging GroveDB's `StorageContext` to the composite
//! commitment tree.
//!
//! Provides [`CommitmentTree`], which owns both the in-memory
//! [`CommitmentFrontier`] and a [`BulkAppendTree`], combining the Sinsemilla
//! frontier (for anchor computation) with the two-level append-only store (for
//! `cmx||payload` persistence with epoch compaction) into a single struct.
//!
//! All mutating operations return [`CostResult`] to propagate storage costs.

use std::marker::PhantomData;

use grovedb_bulk_append_tree::BulkAppendTree;
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;
use orchard::{
    memo::{DashMemo, MemoSize},
    note::TransmittedNoteCiphertext,
    zcash_note_encryption::note_bytes::NoteBytes,
};

use crate::{compute_commitment_tree_state_root, CommitmentFrontier, CommitmentTreeError};

mod tests;

/// Key used to store the serialized commitment frontier in data storage.
pub const COMMITMENT_TREE_DATA_KEY: &[u8] = b"__ct_data__";

/// Result of appending to a [`CommitmentTree`].
#[derive(Debug, Clone)]
pub struct CommitmentAppendResult {
    /// The new Sinsemilla frontier root hash.
    pub sinsemilla_root: [u8; 32],
    /// The BulkAppendTree state root (`blake3(mmr_root || dense_tree_root)`).
    /// This flows as the Merk child hash via `insert_subtree`.
    pub bulk_state_root: [u8; 32],
    /// The 0-based global position of the appended value.
    pub global_position: u64,
    /// Number of blake3 hash calls performed during the bulk append.
    pub hash_count: u32,
    /// Whether compaction (epoch flush) occurred during this append.
    pub compacted: bool,
}

// ── Ciphertext serialization helpers ─────────────────────────────────────

/// Compute the expected ciphertext payload size (excluding the 32-byte cmx
/// prefix) for a given `MemoSize`.
///
/// Layout: `epk_bytes (32) || enc_ciphertext (variable) || out_ciphertext (80)`
///
/// For `DashMemo`: `32 + 104 + 80 = 216 bytes`.
pub fn ciphertext_payload_size<M: MemoSize>() -> usize {
    32 + std::mem::size_of::<M::NoteCiphertextBytes>() + 80
}

/// Serialize a [`TransmittedNoteCiphertext`] to bytes.
///
/// Output layout: `epk_bytes (32) || enc_ciphertext || out_ciphertext (80)`
pub fn serialize_ciphertext<M: MemoSize>(ct: &TransmittedNoteCiphertext<M>) -> Vec<u8> {
    let enc = ct.enc_ciphertext.as_ref();
    let mut buf = Vec::with_capacity(32 + enc.len() + 80);
    buf.extend_from_slice(&ct.epk_bytes);
    buf.extend_from_slice(enc);
    buf.extend_from_slice(&ct.out_ciphertext);
    buf
}

/// Deserialize a [`TransmittedNoteCiphertext`] from bytes.
///
/// Expected layout: `epk_bytes (32) || enc_ciphertext || out_ciphertext (80)`
pub fn deserialize_ciphertext<M: MemoSize>(data: &[u8]) -> Option<TransmittedNoteCiphertext<M>> {
    let enc_size = data.len().checked_sub(32 + 80)?;
    let epk_bytes: [u8; 32] = data[..32].try_into().ok()?;
    let enc_ciphertext =
        <M::NoteCiphertextBytes as NoteBytes>::from_slice(&data[32..32 + enc_size])?;
    let out_ciphertext: [u8; 80] = data[32 + enc_size..].try_into().ok()?;
    Some(TransmittedNoteCiphertext::from_parts(
        epk_bytes,
        enc_ciphertext,
        out_ciphertext,
    ))
}

/// Commitment tree combining in-memory frontier state with a
/// [`BulkAppendTree`].
///
/// Owns both the [`CommitmentFrontier`] (Sinsemilla anchor computation) and a
/// [`BulkAppendTree`] (efficient append-only storage with epoch compaction).
/// Storage is owned by the `BulkAppendTree` via its dense tree.
///
/// The type parameter `M` controls the memo size for note ciphertext
/// validation. It defaults to [`DashMemo`] so code that doesn't care about M
/// (like `verify_grovedb`, `commitment_tree_anchor`) works without specifying
/// it.
///
/// - [`open`](CommitmentTree::open) loads the frontier from storage (or starts
///   empty) and reconstructs the `BulkAppendTree` from persisted state
/// - [`append`](CommitmentTree::append) appends `cmx||ciphertext` to the bulk
///   tree and `cmx` to the frontier
/// - [`save`](CommitmentTree::save) persists the frontier back to storage
///
/// # Authentication model
///
/// The Sinsemilla root (from [`CommitmentFrontier`]) authenticates the **cmx
/// values** — it is a standard Orchard-compatible anchor. The **ciphertext
/// payload** is not independently authenticated by the Sinsemilla root;
/// instead, it is covered by the [`BulkAppendTree`]'s state root
/// (`blake3(mmr_root || dense_tree_root)`), which includes the full
/// `cmx||ciphertext` entries. Both roots flow up through GroveDB's Merk
/// hierarchy, providing authentication for the entire data set.
///
/// # Atomicity
///
/// [`append`](CommitmentTree::append) mutates both the BulkAppendTree (in
/// storage) and the frontier (in memory). The caller must call
/// [`save`](CommitmentTree::save) to persist the frontier. In a GroveDB
/// context, both writes happen within the same transaction, so atomicity is
/// guaranteed by the transaction boundary. If a crash occurs before the
/// transaction commits, both the BulkAppendTree and frontier changes are
/// rolled back.
pub struct CommitmentTree<S, M: MemoSize = DashMemo> {
    frontier: CommitmentFrontier,
    pub(crate) bulk_tree: BulkAppendTree<S>,
    _memo: PhantomData<M>,
}

impl<S, M: MemoSize> std::fmt::Debug for CommitmentTree<S, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitmentTree")
            .field("frontier", &self.frontier)
            .field("total_count", &self.bulk_tree.total_count)
            .field("memo_type", &std::any::type_name::<M>())
            .finish_non_exhaustive()
    }
}

impl<'db, S: StorageContext<'db>, M: MemoSize> CommitmentTree<S, M> {
    /// Create a new empty commitment tree.
    ///
    /// `chunk_power` is the log2 of the epoch size for the underlying
    /// `BulkAppendTree` (height parameter).
    pub fn new(chunk_power: u8, storage: S) -> Result<Self, CommitmentTreeError> {
        let bulk_tree = BulkAppendTree::new(chunk_power, storage)
            .map_err(|e| CommitmentTreeError::InvalidData(format!("bulk tree new: {}", e)))?;
        Ok(Self {
            frontier: CommitmentFrontier::new(),
            bulk_tree,
            _memo: PhantomData,
        })
    }

    /// Load a commitment tree from storage, or start with an empty frontier if
    /// no data exists yet.
    ///
    /// Reconstructs the `BulkAppendTree` from `total_count` and `chunk_power`,
    /// then reads the serialized `CommitmentFrontier` from storage.
    pub fn open(
        total_count: u64,
        chunk_power: u8,
        storage: S,
    ) -> CostResult<Self, CommitmentTreeError> {
        let mut cost = OperationCost::default();

        let bulk_tree = match BulkAppendTree::from_state(total_count, chunk_power, storage) {
            Ok(t) => t,
            Err(e) => {
                return Err(CommitmentTreeError::InvalidData(format!(
                    "bulk tree from_state: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };

        // Read frontier from the bulk tree's storage
        let data = bulk_tree
            .dense_tree
            .storage
            .get(COMMITMENT_TREE_DATA_KEY)
            .unwrap_add_cost(&mut cost);

        let frontier = match data {
            Ok(Some(bytes)) => match CommitmentFrontier::deserialize(&bytes) {
                Ok(f) => f,
                Err(e) => return Err(e).wrap_with_cost(cost),
            },
            Ok(None) => CommitmentFrontier::new(),
            Err(e) => {
                return Err(CommitmentTreeError::InvalidData(format!(
                    "storage error loading frontier: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };

        // Validate that the frontier and bulk tree agree on the number of
        // appended items. A mismatch indicates a partial commit or data
        // corruption.
        let frontier_size = frontier.tree_size();
        if frontier_size != total_count {
            return Err(CommitmentTreeError::InvalidData(format!(
                "frontier tree_size ({}) != bulk tree total_count ({})",
                frontier_size, total_count
            )))
            .wrap_with_cost(cost);
        }

        Ok(Self {
            frontier,
            bulk_tree,
            _memo: PhantomData,
        })
        .wrap_with_cost(cost)
    }

    /// Append a typed ciphertext and note commitment to the commitment tree.
    ///
    /// This is the primary typed API. It serializes the ciphertext internally
    /// and delegates to [`append_raw`](Self::append_raw).
    ///
    /// Call [`save`](Self::save) afterwards to persist the updated frontier.
    pub fn append(
        &mut self,
        cmx: [u8; 32],
        ciphertext: &TransmittedNoteCiphertext<M>,
    ) -> CostResult<CommitmentAppendResult, CommitmentTreeError> {
        let payload = serialize_ciphertext(ciphertext);
        self.append_raw(cmx, &payload)
    }

    /// Append a note commitment and raw payload bytes to the commitment tree.
    ///
    /// Validates that `payload.len() == ciphertext_payload_size::<M>()`.
    ///
    /// 1. Appends `cmx || payload` to the `BulkAppendTree` (data storage)
    /// 2. Appends `cmx` to the Sinsemilla frontier (in-memory)
    ///
    /// Call [`save`](Self::save) afterwards to persist the updated frontier.
    pub fn append_raw(
        &mut self,
        cmx: [u8; 32],
        payload: &[u8],
    ) -> CostResult<CommitmentAppendResult, CommitmentTreeError> {
        let mut cost = OperationCost::default();

        // Validate cmx is a valid Pallas field element before any mutation.
        // This prevents inconsistent state if BulkAppendTree is mutated but
        // the frontier rejects the cmx.
        if crate::commitment_frontier::merkle_hash_from_bytes(&cmx).is_none() {
            return Err(CommitmentTreeError::InvalidFieldElement).wrap_with_cost(cost);
        }

        // Validate payload size
        let expected = ciphertext_payload_size::<M>();
        if payload.len() != expected {
            return Err(CommitmentTreeError::InvalidPayloadSize {
                expected,
                actual: payload.len(),
            })
            .wrap_with_cost(cost);
        }

        // 1. Build cmx||payload and append to BulkAppendTree
        let mut item_value = Vec::with_capacity(32 + payload.len());
        item_value.extend_from_slice(&cmx);
        item_value.extend_from_slice(payload);

        let bulk_result = match self.bulk_tree.append(&item_value) {
            Ok(r) => r,
            Err(e) => {
                return Err(CommitmentTreeError::InvalidData(format!(
                    "bulk append: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        cost.hash_node_calls += bulk_result.hash_count;

        // 2. Append cmx to Sinsemilla frontier (tracks sinsemilla_hash_calls)
        let sinsemilla_root = match self.frontier.append(cmx) {
            grovedb_costs::CostContext {
                value: Ok(root),
                cost: frontier_cost,
            } => {
                cost += frontier_cost;
                root
            }
            grovedb_costs::CostContext {
                value: Err(e),
                cost: frontier_cost,
            } => {
                cost += frontier_cost;
                return Err(e).wrap_with_cost(cost);
            }
        };

        Ok(CommitmentAppendResult {
            sinsemilla_root,
            bulk_state_root: bulk_result.state_root,
            global_position: bulk_result.global_position,
            hash_count: bulk_result.hash_count,
            compacted: bulk_result.compacted,
        })
        .wrap_with_cost(cost)
    }

    /// Persist the current frontier state to storage.
    pub fn save(&self) -> CostResult<(), CommitmentTreeError> {
        let mut cost = OperationCost::default();
        let serialized = self.frontier.serialize();
        let result = self
            .bulk_tree
            .dense_tree
            .storage
            .put(COMMITMENT_TREE_DATA_KEY, &serialized, None, None)
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(()) => Ok(()).wrap_with_cost(cost),
            Err(e) => Err(CommitmentTreeError::InvalidData(format!(
                "storage error saving frontier: {}",
                e
            )))
            .wrap_with_cost(cost),
        }
    }

    // ── Frontier accessors ────────────────────────────────────────────

    /// Get the current Sinsemilla root hash as 32 bytes.
    pub fn root_hash(&self) -> [u8; 32] {
        self.frontier.root_hash()
    }

    /// Get the current root as an Orchard `Anchor`.
    pub fn anchor(&self) -> crate::Anchor {
        self.frontier.anchor()
    }

    /// Get the position of the most recently appended leaf, or `None` if empty.
    pub fn position(&self) -> Option<u64> {
        self.frontier.position()
    }

    /// Get the number of leaves that have been appended to the frontier.
    pub fn tree_size(&self) -> u64 {
        self.frontier.tree_size()
    }

    // ── BulkAppendTree delegates ──────────────────────────────────────

    /// Get the total count of items appended (from the BulkAppendTree).
    pub fn total_count(&self) -> u64 {
        self.bulk_tree.total_count
    }

    /// Compute the combined state root that binds the Sinsemilla anchor to the
    /// BulkAppendTree data root.
    ///
    /// Returns `blake3("ct_state" || sinsemilla_root || bulk_state_root)`.
    /// This is the value that flows as the Merk child hash, ensuring both the
    /// Orchard anchor and the bulk data are authenticated.
    pub fn compute_current_state_root(&self) -> Result<[u8; 32], CommitmentTreeError> {
        let bulk_root = self
            .bulk_tree
            .compute_current_state_root()
            .map_err(|e| CommitmentTreeError::InvalidData(format!("state root: {}", e)))?;
        let sinsemilla_root = self.frontier.root_hash();
        Ok(compute_commitment_tree_state_root(
            &sinsemilla_root,
            &bulk_root,
        ))
    }

    /// Get a single value from the dense tree buffer by buffer-local position.
    pub fn get_buffer_value(&self, position: u16) -> Result<Option<Vec<u8>>, CommitmentTreeError> {
        self.bulk_tree
            .get_buffer_value(position)
            .map_err(|e| CommitmentTreeError::InvalidData(format!("buffer value: {}", e)))
    }

    /// Get a single completed chunk's raw blob by chunk index.
    pub fn get_chunk_value(
        &self,
        chunk_index: u64,
    ) -> Result<Option<Vec<u8>>, CommitmentTreeError> {
        self.bulk_tree
            .get_chunk_value(chunk_index)
            .map_err(|e| CommitmentTreeError::InvalidData(format!("chunk value: {}", e)))
    }

    /// The number of entries per completed chunk (epoch).
    pub fn epoch_size(&self) -> u64 {
        self.bulk_tree.epoch_size()
    }

    /// Number of completed chunks in the MMR.
    pub fn chunk_count(&self) -> u64 {
        self.bulk_tree.chunk_count()
    }
}
