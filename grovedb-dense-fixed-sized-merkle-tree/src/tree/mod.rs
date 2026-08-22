//! The dense fixed-sized Merkle tree: storage layout, the write-through
//! caches, and the hashing primitives shared by every root-maintenance
//! version. The version-dependent insert / root paths live in
//! [`root_maintenance`].

pub(crate) mod root_maintenance;

#[cfg(feature = "storage")]
use std::collections::HashMap;

#[cfg(feature = "storage")]
use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "storage")]
use grovedb_storage::StorageContext;

#[cfg(feature = "storage")]
use crate::{
    hash::{node_hash, validate_height},
    DenseMerkleError,
};

/// Unwrap a `CostResult`, accumulate its cost into `$cost`, and return early
/// (with accumulated cost) on error.
#[cfg(feature = "storage")]
macro_rules! cost_return_on_error {
    ($cost:ident, $expr:expr) => {
        match $expr.unwrap_add_cost(&mut $cost) {
            Ok(x) => x,
            Err(e) => return Err(e).wrap_with_cost($cost),
        }
    };
}
#[cfg(feature = "storage")]
pub(crate) use cost_return_on_error;

/// Encode a position as a big-endian 2-byte key for storage.
pub fn position_key(pos: u16) -> [u8; 2] {
    pos.to_be_bytes()
}

/// Leading byte of a hash-record key. Values live under 2-byte keys (the
/// bare position); records live under 3-byte keys, so the two namespaces
/// cannot collide, nor can either collide with the 4- and 8-byte MMR keys or
/// the named keys the owning trees put beside them.
pub const HASH_RECORD_KEY_PREFIX: u8 = b'h';

/// Encode the key of the hash record for `pos`: `b'h' || position (BE u16)`.
pub fn record_key(pos: u16) -> [u8; 3] {
    let [hi, lo] = pos.to_be_bytes();
    [HASH_RECORD_KEY_PREFIX, hi, lo]
}

/// Serialized length of a [`HashRecord`]: generation (8) + value hash (32) +
/// node hash (32).
pub const HASH_RECORD_LEN: usize = 8 + 32 + 32;

/// The per-position hash record version 1 of `root_maintenance` keeps beside
/// each value.
///
/// `generation` tags the epoch the record belongs to. The bulk-append tree
/// reuses the same position keys every epoch (see
/// [`DenseFixedSizedMerkleTree::reset`]); a record left over from an earlier
/// epoch describes a value that is no longer there, so a reader that finds a
/// record with a different generation treats it as absent rather than
/// trusting it. `value_hash` is `blake3(value)` — kept so that re-hashing an
/// ancestor does not need its (possibly large) value read back —
/// and `node_hash` is `blake3(value_hash || H(left) || H(right))` over the
/// subtree as of the last insert into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashRecord {
    /// Epoch tag; see the type doc.
    pub generation: u64,
    /// `blake3(value)` of the position's own value.
    pub value_hash: [u8; 32],
    /// The position's subtree hash as of the last insert into it.
    pub node_hash: [u8; 32],
}

impl HashRecord {
    /// Serialize as `generation (BE u64) || value_hash || node_hash`.
    pub fn to_bytes(&self) -> [u8; HASH_RECORD_LEN] {
        let mut out = [0u8; HASH_RECORD_LEN];
        out[..8].copy_from_slice(&self.generation.to_be_bytes());
        out[8..40].copy_from_slice(&self.value_hash);
        out[40..].copy_from_slice(&self.node_hash);
        out
    }

    /// Parse a record; `None` if the bytes are not a record (wrong length).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != HASH_RECORD_LEN {
            return None;
        }
        let mut generation = [0u8; 8];
        generation.copy_from_slice(&bytes[..8]);
        let mut value_hash = [0u8; 32];
        value_hash.copy_from_slice(&bytes[8..40]);
        let mut node_hash = [0u8; 32];
        node_hash.copy_from_slice(&bytes[40..]);
        Some(Self {
            generation: u64::from_be_bytes(generation),
            value_hash,
            node_hash,
        })
    }
}

/// How a slot write is reported to the storage cost layer.
///
/// The tree itself never overwrites a slot: positions fill sequentially and
/// only [`reset`](DenseFixedSizedMerkleTree::reset) — used by the bulk-append
/// tree to start a new epoch over the same position keys — makes a later
/// insert land on a key that already holds a committed value. The owner,
/// which knows whether that is the case and what the committed value is,
/// chooses the mode; the tree attaches the matching cost information.
#[cfg(feature = "storage")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotWriteAccounting {
    /// Issue the put with no cost information: the commit path charges the
    /// key and the value as new storage. Right for a slot that has never
    /// been written (and what every shipped version reports for all slots).
    AsNew,
    /// The slot holds a committed value of `previous_value_len` bytes: report
    /// the write as replacing it — `replaced_bytes` is the smaller of the
    /// previous and the new paid size, `added_bytes` is growth only, and
    /// shrink is not credited (no refund semantics for a rolling buffer).
    /// The key, which already exists, is not charged.
    ///
    /// The owner supplies the committed size (the bulk-append tree reads
    /// the slot from storage — committed state plus the surrounding
    /// transaction, never this session's write-through cache — and bills
    /// that read). That is deliberate: a `StorageBatch` keeps one put per
    /// key, so when a session writes the same slot twice (an epoch boundary
    /// inside one batch) only the last put is charged, and it must describe
    /// the transition from the committed value, not from the intermediate
    /// one.
    Overwrite {
        /// Length of the value the slot holds in committed storage.
        previous_value_len: u32,
    },
}

/// A hash record as this session knows it, together with whether its key
/// exists in committed storage — what a rewrite of it must be sized against.
#[cfg(feature = "storage")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CachedRecord {
    pub record: HashRecord,
    /// Whether the record's key held a value in committed storage (the
    /// underlying context, outside this session's batch) when this session
    /// first looked. A `StorageBatch` keeps one put per key, so the put that
    /// is eventually charged must describe the transition from committed
    /// state: an existing key is a replacement, an absent one new storage.
    pub committed: bool,
}

/// A dense fixed-sized Merkle tree with embedded storage.
///
/// Positions are indexed level-order (BFS): root=0, left child=2i+1, right
/// child=2i+2. The tree has height `h` (max 16) and capacity `2^h - 1`.
///
/// Storage is embedded directly on the struct (like Merk).
///
/// A write-through cache (`cache`) holds values written during this session.
/// Reads check the cache first, falling back to storage. This enables use
/// with transactional storage contexts where writes are deferred to a batch
/// and not yet visible through reads. Hash records (root-maintenance version
/// 1) have the same arrangement in `record_cache`.
///
/// How the root is derived — recomputed from every filled position, or
/// maintained incrementally through hash records — is selected per grove
/// version; see [`root_maintenance`].
pub struct DenseFixedSizedMerkleTree<S> {
    height: u8,
    count: u16,
    /// Epoch tag written into every hash record and required of every record
    /// read back. The owner sets it (the bulk-append tree uses its chunk
    /// count); [`reset`](Self::reset) advances it. `0` for a tree that is
    /// never reset.
    generation: u64,
    /// The underlying storage context.
    pub storage: S,
    /// Write-through cache: holds values written in this session.
    /// Indexed by position. `None` means the value has not been written
    /// in this session (fall back to storage).
    /// Only compiled when storage-dependent operations are available.
    #[cfg(feature = "storage")]
    cache: Vec<Option<Vec<u8>>>,
    /// Write-through cache of hash records touched in this session — written,
    /// or read from storage and found current. Absent means "not looked at in
    /// this session" (fall back to storage).
    #[cfg(feature = "storage")]
    record_cache: HashMap<u16, CachedRecord>,
}

// ── Pure accessors (no storage bounds needed) ─────────────────────────

impl<S> DenseFixedSizedMerkleTree<S> {
    /// Maximum number of values this tree can hold.
    pub fn capacity(&self) -> u16 {
        Self::capacity_for_height(self.height)
    }

    /// Compute capacity from height. Height must be 1..=16.
    /// Uses u32 internally to avoid overflow since 1u16 << 16 would overflow.
    fn capacity_for_height(height: u8) -> u16 {
        ((1u32 << height) - 1) as u16
    }

    /// Current number of values stored.
    pub fn count(&self) -> u16 {
        self.count
    }

    /// Height of the tree.
    pub fn height(&self) -> u8 {
        self.height
    }

    /// The epoch tag hash records are written with and checked against. See
    /// [`HashRecord::generation`].
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Set the epoch tag. The owner calls this right after constructing the
    /// tree over storage that has been through earlier epochs (the
    /// bulk-append tree passes its chunk count), so records left by those
    /// epochs are recognised as stale. Changing it mid-session is not
    /// meaningful.
    pub fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }
}

// ── Storage-dependent operations ──────────────────────────────────────

#[cfg(feature = "storage")]
impl<'db, S: StorageContext<'db>> DenseFixedSizedMerkleTree<S> {
    /// Create a new empty tree with the given height and storage.
    ///
    /// Height must be between 1 and 16 inclusive.
    pub fn new(height: u8, storage: S) -> Result<Self, DenseMerkleError> {
        validate_height(height)?;
        let capacity = Self::capacity_for_height(height);
        Ok(Self {
            height,
            count: 0,
            generation: 0,
            storage,
            cache: vec![None; capacity as usize],
            record_cache: HashMap::new(),
        })
    }

    /// Reconstitute a tree from stored state.
    ///
    /// The cache starts empty — pre-existing values are loaded from storage
    /// on demand. Only values written via [`insert`] or [`try_insert`] in
    /// this session are cached.
    ///
    /// [`insert`]: Self::insert
    /// [`try_insert`]: Self::try_insert
    pub fn from_state(height: u8, count: u16, storage: S) -> Result<Self, DenseMerkleError> {
        validate_height(height)?;
        let capacity = Self::capacity_for_height(height);
        if count > capacity {
            return Err(DenseMerkleError::InvalidData(format!(
                "count {} exceeds capacity {} for height {}",
                count, capacity, height
            )));
        }
        Ok(Self {
            height,
            count,
            generation: 0,
            storage,
            cache: vec![None; capacity as usize],
            record_cache: HashMap::new(),
        })
    }

    /// Get a value by position.
    ///
    /// Returns `None` if position >= count. Returns an error if position <
    /// count but the store has no value (store inconsistency).
    pub fn get(&self, position: u16) -> CostResult<Option<Vec<u8>>, DenseMerkleError> {
        let mut cost = OperationCost::default();

        if position >= self.count {
            return Ok(None).wrap_with_cost(cost);
        }

        let opt = cost_return_on_error!(cost, self.get_value(position));
        match opt {
            Some(v) => Ok(Some(v)).wrap_with_cost(cost),
            None => Err(DenseMerkleError::StoreError(format!(
                "expected value at position {} but found none (count={})",
                position, self.count
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Compute the hash of a specific position in the tree by walking its
    /// subtree's values — the version-0 derivation, used by proof generation
    /// for sibling subtree hashes under every version (it never consults hash
    /// records, so it is correct for any buffer).
    ///
    /// Returns `[0u8; 32]` for positions beyond count or capacity.
    pub(crate) fn hash_position(&self, position: u16) -> CostResult<[u8; 32], DenseMerkleError> {
        self.hash_node(position)
    }

    /// Reset the tree to empty state.
    ///
    /// Sets count to 0, clears the write-through caches and advances the
    /// generation. Old values and hash records remain in the underlying
    /// storage; positions are overwritten on the next cycle, and records
    /// from this cycle are recognised as stale by their generation.
    pub fn reset(&mut self) {
        self.count = 0;
        self.generation = self.generation.wrapping_add(1);
        self.cache.fill(None);
        self.record_cache.clear();
    }

    // ── Internal storage helpers ──────────────────────────────────────

    /// Read a value by position, checking the write-through cache first.
    ///
    /// Cache hits return deterministic costs (seek_count=1,
    /// storage_loaded_bytes=len) matching the MMRBatch pattern, so fee
    /// estimates are consistent regardless of cache state.
    pub(crate) fn get_value(&self, position: u16) -> CostResult<Option<Vec<u8>>, DenseMerkleError> {
        // Check write-through cache first
        if let Some(Some(cached)) = self.cache.get(position as usize) {
            return Ok(Some(cached.clone())).wrap_with_cost(OperationCost {
                seek_count: 1,
                storage_loaded_bytes: cached.len() as u64,
                ..Default::default()
            });
        }
        // Fall back to storage
        let mut cost = OperationCost::default();
        let key = position_key(position);
        let result = self.storage.get(key).unwrap_add_cost(&mut cost);
        match result {
            Ok(opt) => Ok(opt).wrap_with_cost(cost),
            Err(e) => Err(DenseMerkleError::StoreError(format!(
                "get at pos {}: {}",
                position, e
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Write a value by position to storage and cache.
    ///
    /// On success, the value is stored in the write-through cache so that
    /// subsequent reads (e.g., during root hash computation) can be served
    /// from memory even when the storage context defers writes.
    ///
    /// `accounting` selects the cost information attached to the put — see
    /// [`SlotWriteAccounting`].
    pub(crate) fn put_value(
        &mut self,
        position: u16,
        value: &[u8],
        accounting: SlotWriteAccounting,
    ) -> CostResult<(), DenseMerkleError> {
        debug_assert!(
            (position as usize) < self.cache.len(),
            "put_value called with position {} >= cache capacity {}",
            position,
            self.cache.len()
        );
        let mut cost = OperationCost::default();
        let key = position_key(position);

        let cost_info = match accounting {
            SlotWriteAccounting::AsNew => None,
            SlotWriteAccounting::Overwrite { previous_value_len } => {
                Some(KeyValueStorageCost::for_in_place_value_rewrite(
                    previous_value_len,
                    value.len() as u32,
                ))
            }
        };

        let result = self
            .storage
            .put(key, value, None, cost_info)
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(()) => {
                // Cache on successful write
                if let Some(slot) = self.cache.get_mut(position as usize) {
                    *slot = Some(value.to_vec());
                }
                Ok(()).wrap_with_cost(cost)
            }
            Err(e) => Err(DenseMerkleError::StoreError(format!(
                "put at pos {}: {}",
                position, e
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Forget the value cached for `position` (after a failed insert, so the
    /// in-memory state matches the count that was rolled back).
    pub(crate) fn uncache_value(&mut self, position: u16) {
        if let Some(slot) = self.cache.get_mut(position as usize) {
            *slot = None;
        }
    }

    /// Count bookkeeping for the root-maintenance paths.
    pub(crate) fn set_count(&mut self, count: u16) {
        self.count = count;
    }

    // ── Hash-record storage helpers (root-maintenance version 1) ──────

    /// The hash record this session knows for `position`, if it has been
    /// written or read-and-found-current in this session.
    pub(crate) fn cached_record(&self, position: u16) -> Option<CachedRecord> {
        self.record_cache.get(&position).copied()
    }

    /// Drop every cached record (after a failed insert left the in-memory
    /// view of the ancestor path in doubt; storage is re-read next time).
    pub(crate) fn clear_record_cache(&mut self) {
        self.record_cache.clear();
    }

    /// Read the hash record for `position` from the underlying storage —
    /// committed state plus the surrounding transaction, never this
    /// session's batch.
    ///
    /// Returns what the key holds: `None` when absent, otherwise the record
    /// and whether it is current (its generation matches the tree's). A
    /// record from another generation is reported so the caller knows the
    /// key exists (a rewrite, for cost sizing) but must not use its hashes.
    /// Bytes that do not parse as a record are treated as an absent record
    /// for hashing and an existing key for sizing, and never trusted.
    ///
    /// Cost: the storage read (one seek, the record bytes).
    pub(crate) fn read_record_from_storage(
        &self,
        position: u16,
    ) -> CostResult<Option<(Option<HashRecord>, bool)>, DenseMerkleError> {
        let mut cost = OperationCost::default();
        let result = self
            .storage
            .get(record_key(position))
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(None) => Ok(None).wrap_with_cost(cost),
            Ok(Some(bytes)) => {
                let parsed = HashRecord::from_bytes(&bytes);
                let current = parsed.is_some_and(|r| r.generation == self.generation);
                Ok(Some((parsed, current))).wrap_with_cost(cost)
            }
            Err(e) => Err(DenseMerkleError::StoreError(format!(
                "hash record get at pos {}: {}",
                position, e
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Resolve the hash record for `position` for this session: the cached
    /// one if present, otherwise the stored one, cached when current.
    ///
    /// Returns `(record_if_current, key_exists_in_committed_storage)`.
    ///
    /// Cache hits are charged like a storage read (one seek, the record
    /// bytes) so that the same append costs the same whether it runs in the
    /// session that wrote the record or a later one.
    pub(crate) fn resolve_record(
        &mut self,
        position: u16,
    ) -> CostResult<(Option<HashRecord>, bool), DenseMerkleError> {
        if let Some(cached) = self.cached_record(position) {
            return Ok((Some(cached.record), cached.committed)).wrap_with_cost(OperationCost {
                seek_count: 1,
                storage_loaded_bytes: HASH_RECORD_LEN as u64,
                ..Default::default()
            });
        }
        let mut cost = OperationCost::default();
        let stored = cost_return_on_error!(cost, self.read_record_from_storage(position));
        match stored {
            None => Ok((None, false)).wrap_with_cost(cost),
            Some((parsed, true)) => {
                // `current` implies `parsed` is `Some`.
                if let Some(record) = parsed {
                    self.record_cache.insert(
                        position,
                        CachedRecord {
                            record,
                            committed: true,
                        },
                    );
                }
                Ok((parsed, true)).wrap_with_cost(cost)
            }
            Some((_, false)) => Ok((None, true)).wrap_with_cost(cost),
        }
    }

    /// Write the hash record for `position` to storage and cache it.
    ///
    /// `committed` says whether the record's key already holds a value in
    /// committed storage (see [`CachedRecord::committed`]): a rewrite is
    /// reported as an in-place replacement of the same-size record, a first
    /// write as new storage.
    pub(crate) fn put_record(
        &mut self,
        position: u16,
        record: HashRecord,
        committed: bool,
    ) -> CostResult<(), DenseMerkleError> {
        let mut cost = OperationCost::default();
        let bytes = record.to_bytes();
        let cost_info = committed.then(|| {
            KeyValueStorageCost::for_in_place_value_rewrite(
                HASH_RECORD_LEN as u32,
                HASH_RECORD_LEN as u32,
            )
        });
        let result = self
            .storage
            .put(record_key(position), &bytes, None, cost_info)
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(()) => {
                self.record_cache
                    .insert(position, CachedRecord { record, committed });
                Ok(()).wrap_with_cost(cost)
            }
            Err(e) => Err(DenseMerkleError::StoreError(format!(
                "hash record put at pos {}: {}",
                position, e
            )))
            .wrap_with_cost(cost),
        }
    }

    // ── Internal hash computation (the value walk) ────────────────────

    /// Root hash derived by walking every filled position's value: the
    /// version-0 derivation. `[0u8; 32]` for an empty tree.
    pub(crate) fn compute_root_hash(&self) -> CostResult<[u8; 32], DenseMerkleError> {
        if self.count == 0 {
            return Ok([0u8; 32]).wrap_with_cost(OperationCost::default());
        }
        self.hash_node(0)
    }

    /// Recursively compute the hash of a node from the stored values.
    ///
    /// All nodes use the same scheme: `blake3(H(value) || H(left) ||
    /// H(right))`. Leaf nodes simply have `[0; 32]` for both child hashes.
    pub(crate) fn hash_node(&self, position: u16) -> CostResult<[u8; 32], DenseMerkleError> {
        let mut cost = OperationCost::default();
        let (_, hash) = cost_return_on_error!(cost, self.hash_node_with_value_hash(position));
        Ok(hash).wrap_with_cost(cost)
    }

    /// [`hash_node`](Self::hash_node) that also returns the position's own
    /// value hash (`blake3(value)`), which a hash record needs. Both are
    /// `[0; 32]` for an unfilled position.
    pub(crate) fn hash_node_with_value_hash(
        &self,
        position: u16,
    ) -> CostResult<([u8; 32], [u8; 32]), DenseMerkleError> {
        let mut cost = OperationCost::default();
        let capacity = self.capacity();

        // Position beyond capacity or unfilled -> zero hash
        if position >= capacity || position >= self.count {
            return Ok(([0u8; 32], [0u8; 32])).wrap_with_cost(cost);
        }

        let opt = cost_return_on_error!(cost, self.get_value(position));
        let value = match opt {
            Some(v) => v,
            None => {
                return Err(DenseMerkleError::StoreError(format!(
                    "expected value at position {} but found none",
                    position
                )))
                .wrap_with_cost(cost);
            }
        };

        let value_hash = *blake3::hash(&value).as_bytes();
        cost.hash_node_calls += 1; // value hash

        // Use u32 to avoid overflow for leaf positions near capacity.
        let left_child_u32 = 2 * position as u32 + 1;
        let right_child_u32 = 2 * position as u32 + 2;

        let left_hash = if left_child_u32 < capacity as u32 {
            cost_return_on_error!(cost, self.hash_node(left_child_u32 as u16))
        } else {
            [0u8; 32]
        };
        let right_hash = if right_child_u32 < capacity as u32 {
            cost_return_on_error!(cost, self.hash_node(right_child_u32 as u16))
        } else {
            [0u8; 32]
        };

        let hash = node_hash(&value_hash, &left_hash, &right_hash);
        cost.hash_node_calls += 1; // node_hash

        Ok((value_hash, hash)).wrap_with_cost(cost)
    }
}
