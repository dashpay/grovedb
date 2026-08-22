//! Storage adapter bridging GroveDB's `StorageContext` to the composite
//! commitment tree.
//!
//! Provides [`CommitmentTree`], which owns both the in-memory
//! [`CommitmentFrontier`] and a [`BulkAppendTree`], combining the Sinsemilla
//! frontier (for anchor computation) with the two-level append-only store (for
//! `cmx||rho||cv_net||payload` persistence with epoch compaction) into a single
//! struct.
//!
//! All mutating operations return [`CostResult`] to propagate storage costs.

use std::marker::PhantomData;

use grovedb_bulk_append_tree::BulkAppendTree;
use grovedb_costs::{
    storage_cost::{key_value_cost::KeyValueStorageCost, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_storage::StorageContext;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};
use integer_encoding::VarInt;
use orchard::{
    memo::{DashMemo, MemoSize},
    note::TransmittedNoteCiphertext,
    zcash_note_encryption::note_bytes::NoteBytes,
};

use crate::{compute_commitment_tree_state_root, CommitmentFrontier, CommitmentTreeError};

mod tests;

/// Key used to store the serialized commitment frontier in data storage.
pub const COMMITMENT_TREE_DATA_KEY: &[u8] = b"__ct_data__";

/// Cost info for persisting the frontier under storage accounting v1: the
/// previous serialization's bytes are replaced, only growth is added; the
/// first save of a tree that never stored a frontier is a new key.
fn frontier_save_cost_info(new_len: u32, previous_len: Option<u32>) -> KeyValueStorageCost {
    let total = new_len.saturating_add(new_len.required_space() as u32);
    let previous_total = previous_len
        .map(|l| l.saturating_add(l.required_space() as u32))
        .unwrap_or(0);
    let replaced = total.min(previous_total);
    KeyValueStorageCost {
        key_storage_cost: StorageCost::default(),
        value_storage_cost: StorageCost {
            added_bytes: total - replaced,
            replaced_bytes: replaced,
            removed_bytes: Default::default(),
        },
        new_node: previous_len.is_none(),
        needs_value_verification: false,
    }
}

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

/// A single entry for [`CommitmentTree::append_many_raw`].
///
/// Carries the same data as a per-leaf [`CommitmentTree::append_raw`] call.
/// Named fields (instead of a bare `([u8; 32], [u8; 32], [u8; 32], Vec<u8>)`
/// tuple) keep the three adjacent, type-identical 32-byte protocol fields from
/// being silently transposed: `cmx`, `rho`, and `cv_net` are interchangeable to
/// the type system but carry distinct meaning, and a swap would build an
/// internally consistent tree while corrupting nullifier association and
/// outgoing-note (OVK) recovery.
#[derive(Debug, Clone)]
pub struct CommitmentEntry {
    /// Note commitment (must be a valid Pallas field element).
    pub cmx: [u8; 32],
    /// Nullifier (`rho`) of the spent note.
    pub rho: [u8; 32],
    /// Value commitment (`cv_net`) of the note — required for OVK recovery.
    pub cv_net: [u8; 32],
    /// Serialized ciphertext payload (`ciphertext_payload_size::<M>()` bytes).
    pub payload: Vec<u8>,
}

// ── Ciphertext serialization helpers ─────────────────────────────────────

/// Compute the expected ciphertext payload size (excluding the unencrypted
/// protocol-level prefix `cmx (32) || rho (32) || cv_net (32)`) for a given
/// `MemoSize`.
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
/// - [`append`](CommitmentTree::append) appends `cmx||rho||cv_net||ciphertext`
///   to the bulk tree and `cmx` to the frontier
/// - [`save`](CommitmentTree::save) persists the frontier back to storage
///
/// # Authentication model
///
/// The Sinsemilla root (from [`CommitmentFrontier`]) authenticates the **cmx
/// values** — it is a standard Orchard-compatible anchor. The **ciphertext
/// payload** is not independently authenticated by the Sinsemilla root;
/// instead, it is covered by the [`BulkAppendTree`]'s state root
/// (`blake3(mmr_root || dense_tree_root)`), which includes the full
/// `cmx||rho||cv_net||ciphertext` entries. Both roots flow up through GroveDB's
/// Merk hierarchy, providing authentication for the entire data set.
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
    /// Serialized length of the frontier as last persisted (loaded at open,
    /// refreshed by `save`), or `None` when no frontier has been stored
    /// yet. Lets `save` report the rewrite as replacement of the previous
    /// serialization under storage accounting v1.
    stored_frontier_len: Option<u32>,
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
            stored_frontier_len: None,
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

        let (frontier, stored_frontier_len) = match data {
            Ok(Some(bytes)) => match CommitmentFrontier::deserialize(&bytes) {
                Ok(f) => (f, Some(bytes.len() as u32)),
                Err(e) => return Err(e).wrap_with_cost(cost),
            },
            Ok(None) => (CommitmentFrontier::new(), None),
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
        // corruption; both [`append_raw`] and [`append_many_raw`] keep the two
        // in sync.
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
            stored_frontier_len,
            _memo: PhantomData,
        })
        .wrap_with_cost(cost)
    }

    /// Append a typed ciphertext and note commitment to the commitment tree.
    ///
    /// This is the primary typed API. It serializes the ciphertext internally
    /// and delegates to [`append_raw`](Self::append_raw).
    ///
    /// `cv_net` is the note's value commitment, stored as an unencrypted
    /// protocol-level field. It is required for outgoing-note (OVK) recovery —
    /// it is an input to Orchard's `derive_ock(ovk, cv, cmx, epk)` key that
    /// decrypts `out_ciphertext`, and it cannot be recomputed from the note.
    ///
    /// Call [`save`](Self::save) afterwards to persist the updated frontier.
    /// The note, chunk and roots are identical under every grove version;
    /// what the version selects is the hash count a compacting append
    /// reports, which this method bills.
    pub fn append(
        &mut self,
        cmx: [u8; 32],
        rho: [u8; 32],
        cv_net: [u8; 32],
        ciphertext: &TransmittedNoteCiphertext<M>,
        grove_version: &GroveVersion,
    ) -> CostResult<CommitmentAppendResult, CommitmentTreeError> {
        let payload = serialize_ciphertext(ciphertext);
        self.append_raw(cmx, rho, cv_net, &payload, grove_version)
    }

    /// Append a note commitment and raw payload bytes to the commitment tree.
    ///
    /// Validates that `payload.len() == ciphertext_payload_size::<M>()`.
    /// `cv_net` is a separate fixed 32-byte field, inherently validated by its
    /// `[u8; 32]` type.
    ///
    /// 1. Appends `cmx || rho || cv_net || payload` to the `BulkAppendTree`
    ///    (data storage)
    /// 2. Appends `cmx` to the Sinsemilla frontier (in-memory)
    ///
    /// The `rho` (nullifier) and `cv_net` (value commitment) are stored as
    /// unencrypted protocol-level fields between `cmx` and the ciphertext
    /// payload. `rho` lets light clients recover the nullifier association
    /// without additional lookups; `cv_net` is required for outgoing-note (OVK)
    /// recovery — it is an input to Orchard's `derive_ock(ovk, cv, cmx, epk)`
    /// key that decrypts `out_ciphertext`, and it cannot be recomputed from the
    /// note. Neither field enters the Sinsemilla frontier, so the Orchard anchor
    /// is unaffected.
    ///
    /// Call [`save`](Self::save) afterwards to persist the updated frontier.
    pub fn append_raw(
        &mut self,
        cmx: [u8; 32],
        rho: [u8; 32],
        cv_net: [u8; 32],
        payload: &[u8],
        grove_version: &GroveVersion,
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

        // 1. Build cmx||rho||cv_net||payload and append to BulkAppendTree
        let mut item_value = Vec::with_capacity(96 + payload.len());
        item_value.extend_from_slice(&cmx);
        item_value.extend_from_slice(&rho);
        item_value.extend_from_slice(&cv_net);
        item_value.extend_from_slice(payload);

        let bulk_result = match self.bulk_tree.append(&item_value, grove_version) {
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

    /// Batch-append a sequence of [`CommitmentEntry`] values.
    ///
    /// Byte-for-byte equivalent to calling [`append_raw`](Self::append_raw)
    /// once per entry — same dense-buffer state, same chunk MMR, same
    /// `CommitmentFrontier` serialization, same final `bulk_state_root` and
    /// `sinsemilla_root` — but computes the Sinsemilla anchor and the
    /// BulkAppendTree state root **exactly once at the end** instead of once
    /// per leaf.
    ///
    /// # Why batch
    ///
    /// Per-leaf [`append_raw`](Self::append_raw) walks the full depth-32
    /// Sinsemilla path to derive a fresh anchor on every call — ~32
    /// Sinsemilla hashes per leaf, ~33× the actual carry-chain work the
    /// upstream `Frontier::append` performs internally. For 1M leaves that
    /// dominates everything. Batching defers the depth walk to one final
    /// `root_hash` call.
    ///
    /// # Returns
    ///
    /// A [`CommitmentAppendResult`] shaped like what the **final** per-leaf
    /// [`append_raw`](Self::append_raw) would have returned: `sinsemilla_root`
    /// and `bulk_state_root` are the post-batch values; `global_position` is
    /// the position of the last appended entry; `hash_count` is the sum across
    /// the batch (including the one final state-root blake3); `compacted` is
    /// `true` if any compaction occurred during the batch.
    ///
    /// # Atomicity
    ///
    /// On error (e.g. invalid cmx or payload size in the middle of the input)
    /// any entries already processed remain in the tree — discard the
    /// surrounding transaction if you need all-or-nothing semantics. This
    /// matches the per-leaf behavior of calling [`append_raw`](Self::append_raw)
    /// in a loop.
    ///
    /// # Persistence — caller responsibilities
    ///
    /// Like [`append_raw`](Self::append_raw), this method does **not** flush
    /// state to disk on its own. After your final batch — i.e. just before
    /// committing the surrounding `StorageBatch` / transaction — the caller
    /// **must** call:
    ///
    /// 1. [`commit_mmr`](Self::commit_mmr) to write MMR nodes staged in the
    ///    overlay during compactions, and
    /// 2. [`save`](Self::save) to persist the Sinsemilla frontier.
    ///
    /// **Do not call [`commit_mmr`](Self::commit_mmr) between chained
    /// `append_many_raw` calls.** A GroveDB `StorageContext::get` does not see
    /// writes that are sitting in its `StorageBatch` (reads go straight to the
    /// underlying transaction). Flushing the overlay mid-session would put the
    /// MMR peaks into the batch, where the *next* batch's compactions can't
    /// read them, and the second batch would then fail with `InconsistentStore`.
    /// Keeping the overlay alive across chained calls is what lets later
    /// compactions resolve their sibling reads in memory.
    pub fn append_many_raw<I>(
        &mut self,
        entries: I,
        grove_version: &GroveVersion,
    ) -> CostResult<CommitmentAppendResult, CommitmentTreeError>
    where
        I: IntoIterator<Item = CommitmentEntry>,
    {
        let mut cost = OperationCost::default();
        let expected_payload = ciphertext_payload_size::<M>();

        let mut appended: u64 = 0;
        let mut hash_count: u32 = 0;
        let mut any_compacted = false;
        // Track the last appended position. If the input is empty we fall back
        // to the tree's current top of range (or 0 if empty) — the byte-for-byte
        // contract only governs frontier+bulk state, not this field for N=0.
        let starting_total = self.bulk_tree.total_count;
        let mut last_global_position: u64 = starting_total.saturating_sub(1);

        for CommitmentEntry {
            cmx,
            rho,
            cv_net,
            payload,
        } in entries
        {
            // Pre-validate cmx (Pallas field element) and payload size *before*
            // any mutation for this entry — mirrors append_raw's ordering so a
            // bad entry doesn't leave a half-written row behind.
            if crate::commitment_frontier::merkle_hash_from_bytes(&cmx).is_none() {
                return Err(CommitmentTreeError::InvalidFieldElement).wrap_with_cost(cost);
            }
            if payload.len() != expected_payload {
                return Err(CommitmentTreeError::InvalidPayloadSize {
                    expected: expected_payload,
                    actual: payload.len(),
                })
                .wrap_with_cost(cost);
            }

            // 1. Build cmx||rho||cv_net||payload and append to BulkAppendTree,
            //    deferring the per-leaf state_root blake3.
            let mut item_value = Vec::with_capacity(96 + payload.len());
            item_value.extend_from_slice(&cmx);
            item_value.extend_from_slice(&rho);
            item_value.extend_from_slice(&cv_net);
            item_value.extend_from_slice(&payload);

            let r = match self
                .bulk_tree
                .append_no_state_root(&item_value, grove_version)
            {
                Ok(r) => r,
                // codecov:ignore — only reachable on a storage fault during the
                // dense-tree insert or MMR compaction (see `append_raw`'s
                // sibling branch for the full rationale).
                Err(e) => {
                    return Err(CommitmentTreeError::InvalidData(format!(
                        "bulk append: {}",
                        e
                    )))
                    .wrap_with_cost(cost);
                }
            };
            hash_count = hash_count.saturating_add(r.hash_count);
            if r.compacted {
                any_compacted = true;
            }
            last_global_position = r.global_position;

            // 2. Append cmx to the Sinsemilla frontier, deferring the depth-32
            //    root walk. Validation here is now redundant with the pre-check
            //    above but is cheap and keeps the cost accounting correct.
            if let Err(e) = self.frontier.append_no_root(cmx).unwrap_add_cost(&mut cost) {
                // codecov:ignore — `append_no_root` can only error on
                // `InvalidFieldElement` (already filtered by the pre-validation
                // above) or `TreeFull` (2^32 leaves, unreachable).
                return Err(e).wrap_with_cost(cost);
            }

            appended += 1;
        }

        // End-of-batch: pay the deferred costs **once**.
        // * `compute_current_state_root` runs one blake3 (matching the +1 each
        //   per-leaf `append` would have added).
        // * `root_hash_with_cost` runs the depth-32 Sinsemilla walk and
        //   attributes its sinsemilla_hash_calls to `cost`.
        let bulk_state_root = match self.bulk_tree.compute_current_state_root() {
            Ok(r) => r,
            // codecov:ignore — reachable only when an upstream storage read
            // fails for the dense-tree root or the cached MMR root. The
            // dense-tree write-through cache and the in-session MMR cache
            // mean a fault-injecting mock can't hit this from inside
            // `append_many_raw` without a separately-opened (cache-cold) tree
            // — out of scope for this unit-test fixture.
            Err(e) => {
                return Err(CommitmentTreeError::InvalidData(format!(
                    "state root: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        if appended > 0 {
            hash_count = hash_count.saturating_add(1);
        }

        let root_ctx = self.frontier.root_hash_with_cost();
        cost += root_ctx.cost;
        let sinsemilla_root = root_ctx.value;

        // Intentionally do NOT call `commit_mmr` here. Any nodes staged in the
        // MMR overlay during compactions must stay in memory until the caller
        // finishes the whole session — see the "Persistence" section in the
        // method docs. Flushing here would put the peaks into the surrounding
        // `StorageBatch`, where the next chained `append_many_raw` would be
        // unable to read them (GroveDB `get` does not see batched writes), and
        // the next compaction would fail with `InconsistentStore`.

        Ok(CommitmentAppendResult {
            sinsemilla_root,
            bulk_state_root,
            global_position: last_global_position,
            hash_count,
            compacted: any_compacted,
        })
        .wrap_with_cost(cost)
    }

    /// Persist the current frontier state to storage.
    ///
    /// The frontier is one value rewritten in place on every append. Under
    /// storage accounting v1 (`bulk_append_tree_versions.cost
    /// .storage_accounting`) the write is reported as replacement of its
    /// previous serialization, with only growth as added bytes; v0 lets the
    /// storage layer report the whole value as added every time.
    pub fn save(&mut self, grove_version: &GroveVersion) -> CostResult<(), CommitmentTreeError> {
        let mut cost = OperationCost::default();
        let serialized = self.frontier.serialize();
        let cost_info = match grove_version
            .bulk_append_tree_versions
            .cost
            .storage_accounting
        {
            0 => None,
            1 => Some(frontier_save_cost_info(
                serialized.len() as u32,
                self.stored_frontier_len,
            )),
            version => {
                return Err(CommitmentTreeError::VersionError(
                    GroveVersionError::UnknownVersionMismatch {
                        method: "CommitmentTree frontier storage accounting".to_string(),
                        known_versions: vec![0, 1],
                        received: version,
                    }
                    .to_string(),
                ))
                .wrap_with_cost(cost)
            }
        };
        let result = self
            .bulk_tree
            .dense_tree
            .storage
            .put(COMMITMENT_TREE_DATA_KEY, &serialized, None, cost_info)
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(()) => {
                self.stored_frontier_len = Some(serialized.len() as u32);
                Ok(()).wrap_with_cost(cost)
            }
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
