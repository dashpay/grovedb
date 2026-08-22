//! The dense fixed-sized Merkle tree: storage layout, the write-through
//! caches, and the hashing primitives shared by every root-maintenance
//! version. The version-dependent insert / root paths live in
//! [`root_maintenance`].

#[cfg(feature = "storage")]
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

/// Encode the key of the path record written by the insert at `pos`:
/// `b'h' || position (BE u16)`.
pub fn record_key(pos: u16) -> [u8; 3] {
    let [hi, lo] = pos.to_be_bytes();
    [HASH_RECORD_KEY_PREFIX, hi, lo]
}

/// Fixed part of a serialized [`PathRecord`]: generation (8) + present mask
/// (2) + value hash (32). The node-hash entries add 32 bytes per level of
/// the tree.
pub const PATH_RECORD_HEADER_LEN: usize = 8 + 2 + 32;

/// Serialized length of a [`PathRecord`] for a tree of `height`: fixed per
/// tree, whatever the depth of the inserting position, so every insert
/// writes the same number of bytes.
pub fn path_record_len(height: u8) -> usize {
    PATH_RECORD_HEADER_LEN + 32 * height as usize
}

/// The record root-maintenance version 1 writes for each insert — one per
/// inserted position, under that position's key, never rewritten by later
/// inserts (only filled in further by a catch-up, see below).
///
/// It carries the inserted position's own `value_hash` (`blake3(value)`) and
/// the `node_hash` of every position on its ancestor path — `entry[depth]`
/// for depths `0..=depth(position)` — as of this insert. Because positions
/// fill in BFS order, the record of the LAST insert into a subtree holds
/// that subtree's current hash (no later insert touched it), and every
/// position's own record holds its value hash for good; both are located
/// arithmetically from `count`, so no record is ever rewritten by normal
/// inserts.
///
/// `generation` tags the epoch the record belongs to. The bulk-append tree
/// reuses the same position keys every epoch (see
/// [`DenseFixedSizedMerkleTree::reset`]); a record left over from an earlier
/// epoch describes values that are no longer there, so a reader that finds
/// a record with a different generation treats it as absent rather than
/// trusting it. `present` says which entries hold a hash: every depth up to
/// the inserting position's for a record written by an insert; a subset for
/// a record synthesized by a catch-up (a buffer filled without records).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRecord {
    /// Epoch tag; see the type doc.
    pub generation: u64,
    /// Bit `d` set when `entries[d]` holds the node hash of the path position
    /// at depth `d`.
    pub present: u16,
    /// `blake3(value)` of the position this record belongs to.
    pub value_hash: [u8; 32],
    /// Node hashes by depth (index 0 = root); `height` entries, zero when not
    /// present.
    pub entries: Vec<[u8; 32]>,
}

impl PathRecord {
    /// An empty record for a tree of `height`: no entries present.
    pub fn new(generation: u64, value_hash: [u8; 32], height: u8) -> Self {
        Self {
            generation,
            present: 0,
            value_hash,
            entries: vec![[0u8; 32]; height as usize],
        }
    }

    /// The node hash recorded for depth `depth`, if present.
    pub fn entry(&self, depth: u8) -> Option<[u8; 32]> {
        ((self.present >> depth) & 1 == 1)
            .then(|| self.entries.get(depth as usize).copied())
            .flatten()
    }

    /// Record the node hash for depth `depth`.
    pub fn set_entry(&mut self, depth: u8, hash: [u8; 32]) {
        if let Some(slot) = self.entries.get_mut(depth as usize) {
            *slot = hash;
            self.present |= 1 << depth;
        }
    }

    /// Serialize as `generation (BE u64) || present (BE u16) || value_hash ||
    /// entries`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PATH_RECORD_HEADER_LEN + 32 * self.entries.len());
        out.extend_from_slice(&self.generation.to_be_bytes());
        out.extend_from_slice(&self.present.to_be_bytes());
        out.extend_from_slice(&self.value_hash);
        for e in &self.entries {
            out.extend_from_slice(e);
        }
        out
    }

    /// Parse a record for a tree of `height`; `None` if the bytes are not a
    /// record of that shape.
    pub fn from_bytes(bytes: &[u8], height: u8) -> Option<Self> {
        if bytes.len() != path_record_len(height) {
            return None;
        }
        let mut generation = [0u8; 8];
        generation.copy_from_slice(&bytes[..8]);
        let present = u16::from_be_bytes([bytes[8], bytes[9]]);
        let mut value_hash = [0u8; 32];
        value_hash.copy_from_slice(&bytes[10..42]);
        let entries = bytes[42..]
            .chunks_exact(32)
            .map(|c| {
                let mut h = [0u8; 32];
                h.copy_from_slice(c);
                h
            })
            .collect();
        Some(Self {
            generation: u64::from_be_bytes(generation),
            present,
            value_hash,
            entries,
        })
    }
}

/// Depth of a BFS position (root = 0).
pub fn depth_of(position: u16) -> u8 {
    (position as u32 + 1).ilog2() as u8
}

/// The last filled position inside the subtree rooted at `position`, given
/// `count` filled positions in BFS order — the insert whose path record holds
/// that subtree's current hash. `None` when `position >= count`.
pub fn last_filled_in_subtree(position: u16, count: u16, height: u8) -> Option<u16> {
    if position >= count {
        return None;
    }
    let depth = depth_of(position);
    // Descendants at distance k span `(position + 1) * 2^k - 1 ..=
    // (position + 2) * 2^k - 2`; take the deepest level that has any filled
    // position — its last filled one is the last insert into the subtree.
    let max_k = (height - 1).saturating_sub(depth);
    for k in (0..=max_k as u32).rev() {
        let lo = ((position as u64 + 1) << k) - 1;
        if lo < count as u64 {
            let hi = ((position as u64 + 2) << k) - 2;
            return Some(hi.min(count as u64 - 1) as u16);
        }
    }
    Some(position)
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
    /// The slot is a position of a transient buffer: the bytes it holds are
    /// churn, not the tree's long-term storage (the bulk-append tree rewrites
    /// every slot each epoch and the values live on in the chunk blob, whose
    /// bytes each append prepays). The write — and the path record written
    /// beside it — is reported as an in-place replacement of its own size,
    /// whether or not the key exists yet: `replaced_bytes` = the paid size,
    /// nothing added, no key charged, and nothing is read to size it.
    Churn,
}

/// A path record as this session knows it, together with whether its key
/// exists in committed storage — what a rewrite of it must be sized against
/// for an owner whose records are long-term storage.
#[cfg(feature = "storage")]
#[derive(Clone, Debug)]
pub(crate) struct CachedRecord {
    pub record: PathRecord,
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
    /// Write-through cache of path records touched in this session —
    /// written, or read from storage and found current — keyed by the
    /// inserting position. Absent means "not looked at in this session" (fall
    /// back to storage).
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

    /// The root derived from the stored VALUES alone — every filled position
    /// read back and hashed, never a hash record — under every grove version.
    ///
    /// This is the independent audit derivation: integrity walks
    /// (`verify_grovedb`, a state-sync restore's final binding check) must
    /// use it, because the record fast path of [`root_hash`](Self::root_hash)
    /// (root-maintenance version 1) returns what the records say and would
    /// not notice a payload value altered underneath them. `[0u8; 32]` for an
    /// empty tree.
    pub fn root_hash_from_values(&self) -> CostResult<[u8; 32], DenseMerkleError> {
        self.compute_root_hash()
    }

    /// The root the path records claim — what a root-maintenance-version-1
    /// root read returns: entry 0 of the last insert's record, if that record
    /// is current and holds it. `None` when the tree is empty or no such
    /// record exists (a buffer filled under version 0, an earlier epoch's
    /// leftover, or a catch-up-synthesized record without the root). Does
    /// not walk.
    ///
    /// Audits compare it with [`root_hash_from_values`](Self::root_hash_from_values):
    /// a difference means the records and the values disagree — a payload
    /// altered behind the records, or records written for other values.
    pub fn recorded_root(&self) -> CostResult<Option<[u8; 32]>, DenseMerkleError> {
        let mut cost = OperationCost::default();
        if self.count == 0 {
            return Ok(None).wrap_with_cost(cost);
        }
        let last = self.count - 1;
        if let Some(cached) = self.cached_record(last) {
            return Ok(cached.record.entry(0)).wrap_with_cost(cost);
        }
        match cost_return_on_error!(cost, self.read_record_from_storage(last)) {
            Some((Some(record), true)) => Ok(record.entry(0)).wrap_with_cost(cost),
            _ => Ok(None).wrap_with_cost(cost),
        }
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
            // Churn: the paid size replaced, nothing added, key not charged.
            SlotWriteAccounting::Churn => Some(KeyValueStorageCost::for_in_place_value_rewrite(
                value.len() as u32,
                value.len() as u32,
            )),
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

    // ── Path-record storage helpers (root-maintenance version 1) ──────

    /// The path record this session knows for the insert at `position`, if
    /// it has been written or read-and-found-current in this session.
    pub(crate) fn cached_record(&self, position: u16) -> Option<&CachedRecord> {
        self.record_cache.get(&position)
    }

    /// Drop every cached record (after a failed insert left the in-memory
    /// view in doubt; storage is re-read next time).
    pub(crate) fn clear_record_cache(&mut self) {
        self.record_cache.clear();
    }

    /// Read the path record of the insert at `position` from the underlying
    /// storage — committed state plus the surrounding transaction, never this
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
    ) -> CostResult<Option<(Option<PathRecord>, bool)>, DenseMerkleError> {
        let mut cost = OperationCost::default();
        let result = self
            .storage
            .get(record_key(position))
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(None) => Ok(None).wrap_with_cost(cost),
            Ok(Some(bytes)) => {
                let parsed = PathRecord::from_bytes(&bytes, self.height);
                let current = parsed
                    .as_ref()
                    .is_some_and(|r| r.generation == self.generation);
                Ok(Some((parsed, current))).wrap_with_cost(cost)
            }
            Err(e) => Err(DenseMerkleError::StoreError(format!(
                "path record get at pos {}: {}",
                position, e
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Resolve the current path record of the insert at `position` for this
    /// session: the cached one if present, otherwise the stored one, cached
    /// when current.
    ///
    /// Returns `(record_if_current, key_exists_in_committed_storage)`.
    ///
    /// Cache hits are charged like a storage read (one seek, the record
    /// bytes) so the same work costs the same in the session that wrote the
    /// record and in a later one. (Root-maintenance version 1 bills a fixed
    /// model per insert anyway; this keeps the crate-level figures honest.)
    pub(crate) fn resolve_record(
        &mut self,
        position: u16,
    ) -> CostResult<(Option<PathRecord>, bool), DenseMerkleError> {
        if let Some(cached) = self.cached_record(position) {
            return Ok((Some(cached.record.clone()), cached.committed)).wrap_with_cost(
                OperationCost {
                    seek_count: 1,
                    storage_loaded_bytes: path_record_len(self.height) as u64,
                    ..Default::default()
                },
            );
        }
        let mut cost = OperationCost::default();
        let stored = cost_return_on_error!(cost, self.read_record_from_storage(position));
        match stored {
            None => Ok((None, false)).wrap_with_cost(cost),
            Some((Some(record), true)) => {
                self.record_cache.insert(
                    position,
                    CachedRecord {
                        record: record.clone(),
                        committed: true,
                    },
                );
                Ok((Some(record), true)).wrap_with_cost(cost)
            }
            Some(_) => Ok((None, true)).wrap_with_cost(cost),
        }
    }

    /// Write the path record of the insert at `position` to storage and
    /// cache it.
    ///
    /// How the write is sized follows the owner's slot accounting: under
    /// [`SlotWriteAccounting::Churn`] it is an in-place replacement of its own
    /// (fixed) size; otherwise `committed` says whether the key already holds
    /// a value in committed storage (see [`CachedRecord::committed`]) — a
    /// rewrite is a replacement, a first write new storage.
    pub(crate) fn put_record(
        &mut self,
        position: u16,
        record: PathRecord,
        committed: bool,
        accounting: SlotWriteAccounting,
    ) -> CostResult<(), DenseMerkleError> {
        let mut cost = OperationCost::default();
        let bytes = record.to_bytes();
        let len = bytes.len() as u32;
        let cost_info = match accounting {
            SlotWriteAccounting::Churn => {
                Some(KeyValueStorageCost::for_in_place_value_rewrite(len, len))
            }
            SlotWriteAccounting::AsNew | SlotWriteAccounting::Overwrite { .. } => {
                committed.then(|| KeyValueStorageCost::for_in_place_value_rewrite(len, len))
            }
        };
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
                "path record put at pos {}: {}",
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
