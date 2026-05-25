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
/// and 32-byte rho prefix) for a given `MemoSize`.
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
/// - [`append`](CommitmentTree::append) appends `cmx||rho||ciphertext` to the
///   bulk tree and `cmx` to the frontier
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
        //
        // Skipped entirely under `test-seeding-ct`: the frontier-less seeding
        // methods (`append_*_without_frontier`) deliberately leave the frontier
        // out of sync with the bulk tree, and a seeded tree may then have notes
        // added on top, so any `(frontier_size, total_count)` pair must reopen.
        #[cfg(not(feature = "test-seeding-ct"))]
        {
            let frontier_size = frontier.tree_size();
            if frontier_size != total_count {
                return Err(CommitmentTreeError::InvalidData(format!(
                    "frontier tree_size ({}) != bulk tree total_count ({})",
                    frontier_size, total_count
                )))
                .wrap_with_cost(cost);
            }
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
        rho: [u8; 32],
        ciphertext: &TransmittedNoteCiphertext<M>,
    ) -> CostResult<CommitmentAppendResult, CommitmentTreeError> {
        let payload = serialize_ciphertext(ciphertext);
        self.append_raw(cmx, rho, &payload)
    }

    /// Append a note commitment and raw payload bytes to the commitment tree.
    ///
    /// Validates that `payload.len() == ciphertext_payload_size::<M>()`.
    ///
    /// 1. Appends `cmx || rho || payload` to the `BulkAppendTree` (data
    ///    storage)
    /// 2. Appends `cmx` to the Sinsemilla frontier (in-memory)
    ///
    /// The `rho` (nullifier) is stored as an unencrypted protocol-level field
    /// between `cmx` and the ciphertext payload. This allows light clients to
    /// recover the nullifier association without additional lookups.
    ///
    /// Call [`save`](Self::save) afterwards to persist the updated frontier.
    pub fn append_raw(
        &mut self,
        cmx: [u8; 32],
        rho: [u8; 32],
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

        // 1. Build cmx||rho||payload and append to BulkAppendTree
        let mut item_value = Vec::with_capacity(64 + payload.len());
        item_value.extend_from_slice(&cmx);
        item_value.extend_from_slice(&rho);
        item_value.extend_from_slice(payload);

        let bulk_result = match self.bulk_tree.append(&item_value) {
            Ok(r) => r,
            // codecov:ignore — requires BulkAppendTree::append to fail, which only happens on
            // storage faults (put/get errors) during dense tree insert or MMR compaction;
            // MockDataStorageContext always succeeds and FailingDataStorageContext prevents
            // construction, so this path cannot be reached without a fault-injecting mock
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
            // codecov:ignore — CommitmentFrontier::append can only fail with InvalidFieldElement
            // (already checked at line 258) or TreeFull (requires 2^32 Sinsemilla appends);
            // neither case is reachable in practice
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

    /// Flush the MMR overlay to storage.
    ///
    /// Delegates to [`BulkAppendTree::commit_mmr`]. Call this at the end of a
    /// session to persist MMR nodes buffered during compaction cycles.
    pub fn commit_mmr(&mut self) -> Result<(), CommitmentTreeError> {
        self.bulk_tree
            .commit_mmr()
            .map_err(|e| CommitmentTreeError::InvalidData(format!("MMR commit: {}", e)))
    }

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

/// Result of a single frontier-less append via
/// [`CommitmentTree::append_raw_without_frontier`].
///
/// Mirrors [`CommitmentAppendResult`] but omits `sinsemilla_root`, because the
/// Sinsemilla frontier is intentionally left untouched. Only available with the
/// `test-seeding-ct` feature.
#[cfg(feature = "test-seeding-ct")]
#[derive(Debug, Clone)]
pub struct FrontierLessAppendResult {
    /// The BulkAppendTree state root after the append (the Merk child hash).
    pub bulk_state_root: [u8; 32],
    /// The 0-based global position of the appended value.
    pub global_position: u64,
    /// Number of blake3 hash calls performed during the bulk append.
    pub hash_count: u32,
    /// Whether compaction (epoch flush) occurred during this append.
    pub compacted: bool,
}

/// Summary of a frontier-less bulk seed via
/// [`CommitmentTree::append_many_without_frontier`].
///
/// Only available with the `test-seeding-ct` feature.
#[cfg(feature = "test-seeding-ct")]
#[derive(Debug, Clone)]
pub struct BulkSeedSummary {
    /// Number of notes appended during this call.
    pub appended: u64,
    /// The tree's total item count after the call.
    pub total_count: u64,
    /// The final BulkAppendTree state root (the Merk child hash). This binds
    /// only the bulk data; a frontier-less seeded tree has no valid Orchard
    /// anchor.
    pub bulk_state_root: [u8; 32],
    /// Total blake3 hash calls performed across all appends and compactions.
    pub hash_count: u64,
    /// Number of epoch compactions (chunk finalizations) triggered.
    pub compactions: u64,
}

/// Frontier-less seeding API — **test / devnet only**.
///
/// These methods append note entries to the underlying [`BulkAppendTree`]
/// WITHOUT updating the Sinsemilla [`CommitmentFrontier`]. They exist to
/// pre-populate the shielded pool with a large number of filler notes for
/// sync/scale benchmarking without paying the per-note Pallas/Sinsemilla
/// hashing cost (or the full Drive insert path).
///
/// # Consequences
///
/// - The tree has **no valid Orchard anchor** afterwards: [`anchor`] and
///   [`root_hash`] reflect an empty frontier, so notes seeded this way are
///   **not spendable**. The BulkAppendTree chunk proofs — authenticated by the
///   blake3 `bulk_state_root` rather than the frontier — are unaffected and
///   still verify, which is exactly what client sync exercises.
/// - `cmx` values are **not** validated as Pallas field elements (unlike
///   [`append`] and [`append_raw`]), so arbitrary 32-byte filler is accepted.
/// - A seeded tree can only be re-[`open`]ed by a build that also enables
///   `test-seeding-ct`, which drops the frontier/bulk consistency check in
///   [`open`] entirely. After seeding you may keep adding filler this way, or
///   switch to the regular [`append`] / [`append_raw`] to add real,
///   frontier-tracked notes on top (those build the frontier from where it is,
///   i.e. over the post-seed notes only).
///
/// [`anchor`]: CommitmentTree::anchor
/// [`root_hash`]: CommitmentTree::root_hash
/// [`append`]: CommitmentTree::append
/// [`append_raw`]: CommitmentTree::append_raw
/// [`open`]: CommitmentTree::open
#[cfg(feature = "test-seeding-ct")]
impl<'db, S: StorageContext<'db>, M: MemoSize> CommitmentTree<S, M> {
    /// Append a single note (`cmx || rho || payload`) to the BulkAppendTree
    /// **without** updating the Sinsemilla frontier.
    ///
    /// The payload length must equal [`ciphertext_payload_size::<M>()`]. The
    /// `cmx` is stored verbatim and is *not* validated as a Pallas field
    /// element. See the impl-level docs for the consequences.
    pub fn append_raw_without_frontier(
        &mut self,
        cmx: [u8; 32],
        rho: [u8; 32],
        payload: &[u8],
    ) -> CostResult<FrontierLessAppendResult, CommitmentTreeError> {
        let mut cost = OperationCost::default();

        // Frontier-less seeding is meant for a fresh tree whose frontier hasn't
        // been built yet — the seeded filler is never reflected in the frontier.
        // Reject seeding once the frontier has leaves so filler can only sit at
        // the bottom of the tree, never interleaved under real,
        // frontier-tracked notes added afterwards.
        if self.frontier.tree_size() != 0 {
            return Err(CommitmentTreeError::InvalidData(
                "frontier-less seeding requires an empty frontier".to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // Validate payload size — kept because it keeps stored entries
        // well-formed for chunk proofs and client deserialization. (cmx is
        // intentionally not validated as a Pallas field element here.)
        let expected = ciphertext_payload_size::<M>();
        if payload.len() != expected {
            return Err(CommitmentTreeError::InvalidPayloadSize {
                expected,
                actual: payload.len(),
            })
            .wrap_with_cost(cost);
        }

        let mut item_value = Vec::with_capacity(64 + payload.len());
        item_value.extend_from_slice(&cmx);
        item_value.extend_from_slice(&rho);
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

        Ok(FrontierLessAppendResult {
            bulk_state_root: bulk_result.state_root,
            global_position: bulk_result.global_position,
            hash_count: bulk_result.hash_count,
            compacted: bulk_result.compacted,
        })
        .wrap_with_cost(cost)
    }

    /// Bulk-append many notes to the BulkAppendTree **without** updating the
    /// Sinsemilla frontier.
    ///
    /// Each item is `(cmx, rho, payload)`. The iterator is consumed lazily, so
    /// a generator can stream a large `N` without materializing it. MMR nodes
    /// buffered during compaction are flushed via [`commit_mmr`] before
    /// returning, so the caller does not need a separate `commit_mmr()` call.
    /// The frontier is left untouched; callers typically set the parent
    /// `Element::CommitmentTree(total_count, ..)` from the returned summary.
    ///
    /// [`commit_mmr`]: CommitmentTree::commit_mmr
    pub fn append_many_without_frontier<I>(
        &mut self,
        notes: I,
    ) -> CostResult<BulkSeedSummary, CommitmentTreeError>
    where
        I: IntoIterator<Item = ([u8; 32], [u8; 32], Vec<u8>)>,
    {
        let mut cost = OperationCost::default();

        // Fail fast before consuming the iterator: frontier-less seeding is only
        // valid on an empty frontier (see `append_raw_without_frontier`).
        if self.frontier.tree_size() != 0 {
            return Err(CommitmentTreeError::InvalidData(
                "frontier-less seeding requires an empty frontier".to_string(),
            ))
            .wrap_with_cost(cost);
        }

        let mut appended: u64 = 0;
        let mut hash_count: u64 = 0;
        let mut compactions: u64 = 0;
        let mut bulk_state_root = [0u8; 32];

        for (cmx, rho, payload) in notes {
            let r = match self
                .append_raw_without_frontier(cmx, rho, &payload)
                .unwrap_add_cost(&mut cost)
            {
                Ok(r) => r,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };
            appended += 1;
            hash_count += u64::from(r.hash_count);
            if r.compacted {
                compactions += 1;
            }
            bulk_state_root = r.bulk_state_root;
        }

        // Flush MMR nodes staged during compaction so the seeded state is
        // fully persisted; callers don't need a separate commit_mmr().
        if let Err(e) = self.commit_mmr() {
            // codecov:ignore — commit_mmr only fails on a storage fault during
            // the final MMR flush; this method always flushes within the same
            // call after appending, so there's no way to leave a pending overlay
            // and then fail only the flush via the test mocks.
            return Err(e).wrap_with_cost(cost);
        }

        if appended == 0 {
            // Nothing appended — report the current (unchanged) state root.
            bulk_state_root = match self.bulk_tree.compute_current_state_root() {
                Ok(r) => r,
                Err(e) => {
                    return Err(CommitmentTreeError::InvalidData(format!(
                        "state root: {}",
                        e
                    )))
                    .wrap_with_cost(cost);
                }
            };
        }

        Ok(BulkSeedSummary {
            appended,
            total_count: self.bulk_tree.total_count,
            bulk_state_root,
            hash_count,
            compactions,
        })
        .wrap_with_cost(cost)
    }
}
