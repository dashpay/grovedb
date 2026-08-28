//! State-sync support for the non-Merk append-only tree family
//! (`CommitmentTree`, `MmrTree`, `BulkAppendTree`,
//! `DenseAppendOnlyFixedSizeTree`, `PrivateDocumentStore`) — see
//! <https://github.com/dashpay/grovedb/issues/785> (and #783 / #784 for
//! `PrivateDocumentStore`).
//!
//! These tree types keep an always-empty Merk (`root_key = None`) and store
//! their payload as raw non-Element entries in the subtree's data namespace,
//! so the Merk chunk protocol cannot transfer them. Instead, the transfer is
//! **target-driven entry replay**:
//!
//! - The target already holds the subtree's element (entry counts and
//!   parameters) from the hash-verified parent Merk, so it encodes a
//!   [`NonMerkChunkId`] — `(start_position, state, param)` — into every local
//!   chunk id it requests.
//! - The source serves pages of **leaf entries only** (plus the serialized
//!   Sinsemilla frontier for commitment trees, which is an accumulator and
//!   cannot be recomputed from entries), read through the same public
//!   accessors normal reads use.
//! - The target replays each entry through the real append primitives
//!   (`BulkAppendTree::append`, `MMR::push`, `DenseFixedSizedMerkleTree::
//!   insert`), so every internal node, chunk blob, and cached hash on the
//!   target is **locally derived** from the wire entries.
//! - At subtree completion the target recomputes the type-specific state
//!   root from its own storage
//!   ([`GroveDb::compute_non_merk_state_root`]) and requires
//!   `combine_hash(value_hash(element_bytes), state_root)` to equal the
//!   element value hash bound into the (already restored, hash-verified)
//!   parent Merk. Any tampering with wire bytes — entries, frontier, counts
//!   — changes the recomputed root and rejects the subtree.
//!
//! # Resource bounds against Byzantine peers
//!
//! Both ends of the wire are untrusted:
//!
//! - The **source** validates every peer-controlled cursor field before it
//!   drives tree arithmetic: an MMR `state` must be a canonical MMR size
//!   (so `leaf_to_pos` is never fed an index it cannot represent), dense
//!   counts must fit `u16`, and bulk/dense parameters go through the trees'
//!   own `from_state` validation. Any mismatch produces a bounded read error,
//!   never a panic, and every page is bounded by [`MAX_PAGE_BYTES`] /
//!   [`MAX_PAGE_ENTRIES`].
//! - The **target** enforces the same page budget on receipt
//!   ([`decode_non_merk_page`]) *before* allocating or replaying anything,
//!   so a few megabytes of wire bytes can never turn into millions of hashes
//!   and transactional writes; the byzantine page is rejected outright
//!   instead of at the final root check. The transport's absolute message
//!   size cap still bounds the single-entry case (entries have no
//!   protocol-level maximum size).
//!
//! `PrivateDocumentStore` replays through [`PrivateDocumentStore::append`]
//! (issues #783 / #784), so the committed `entry_size` from the target's
//! hash-verified element is enforced on every replayed entry, and the
//! config-binding state root is recomputed and checked at finalize like
//! every other type in the family.

use grovedb_bulk_append_tree::{deserialize_chunk_blob, BulkAppendTree};
use grovedb_commitment_tree::{CommitmentFrontier, COMMITMENT_TREE_DATA_KEY};
use grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree;
use grovedb_merk::{
    tree::{combine_hash, hash::CryptoHash},
    tree_type::TreeType,
};
use grovedb_merkle_mountain_range::{
    leaf_to_pos, mmr_size_to_leaf_count, MMRStoreReadOps, MmrNode, MmrStore, MMR,
};
use grovedb_path::SubtreePath;
use grovedb_private_document_store::PrivateDocumentStore;
use grovedb_storage::{Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    replication::utils::{pack_nested_bytes, unpack_nested_bytes},
    Element, Error, GroveDb, Transaction,
};

/// Soft byte budget for a single page of entries. A page always carries at
/// least one entry even if that entry alone exceeds the budget.
pub(crate) const MAX_PAGE_BYTES: usize = 1 << 20; // 1 MiB

/// Hard cap on the number of entries in a single page, so pages of tiny
/// entries stay bounded in element count as well as bytes. Enforced by the
/// sender loops AND by [`decode_non_merk_page`] on receipt.
pub(crate) const MAX_PAGE_ENTRIES: usize = 8192;

/// Encoded length of a [`NonMerkChunkId`]: start (8) + state (8) + param (1).
const NON_MERK_CHUNK_ID_LEN: usize = 17;

/// Whether state sync transfers this (non-Merk) tree type by entry replay.
///
/// Covers every tree type for which
/// [`TreeType::uses_non_merk_data_storage`] is true. Kept as its own
/// predicate (rather than aliasing that one) so a future non-Merk type
/// without a replay arm fails closed here instead of being routed into
/// replay it does not support.
pub(crate) fn supports_entry_replay(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::CommitmentTree(_)
            | TreeType::MmrTree
            | TreeType::BulkAppendTree(_)
            | TreeType::DenseAppendOnlyFixedSizeTree(_)
            | TreeType::PrivateDocumentStore(_)
    )
}

/// Element-level twin of [`supports_entry_replay`].
pub(crate) fn element_supports_entry_replay(element: &Element) -> bool {
    matches!(
        element.underlying(),
        Element::CommitmentTree(..)
            | Element::MmrTree(..)
            | Element::BulkAppendTree(..)
            | Element::DenseAppendOnlyFixedSizeTree(..)
            | Element::PrivateDocumentStore(..)
    )
}

/// Validate that `mmr_size` is a canonical MMR size and return its leaf
/// count.
///
/// `mmr_size_to_leaf_count` silently maps a non-canonical size to the leaf
/// count of the last valid MMR below it, and for sizes near `u64::MAX` it
/// yields leaf counts whose positions `leaf_to_pos` cannot compute without
/// overflowing. Both the source (peer-controlled cursor) and the target
/// (element from the parent) go through this check so the arithmetic that
/// follows is always in range.
pub(crate) fn validate_mmr_size(mmr_size: u64) -> Result<u64, Error> {
    let leaf_count = mmr_size_to_leaf_count(mmr_size);
    // Canonical size for `leaf_count` leaves is `2 * leaf_count -
    // popcount(leaf_count)`; compute it with checked arithmetic so a
    // pathological size can never overflow here either.
    let canonical = leaf_count
        .checked_mul(2)
        .and_then(|twice| twice.checked_sub(u64::from(leaf_count.count_ones())));
    if canonical != Some(mmr_size) {
        return Err(Error::CorruptedData(format!(
            "{mmr_size} is not a valid MMR size"
        )));
    }
    Ok(leaf_count)
}

/// Local chunk id for a non-Merk subtree page request. The target — which
/// holds the hash-verified element — tells the source everything it needs to
/// serve the page, so the source never has to reconstruct tree geometry from
/// its raw namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NonMerkChunkId {
    /// First entry position (0-based) this page should start at.
    pub start: u64,
    /// Type-specific size state from the element: `total_count` for
    /// commitment/bulk trees and private document stores, `mmr_size` for
    /// MMR trees, entry `count` for dense trees.
    pub state: u64,
    /// Type-specific parameter from the element: `chunk_power` for
    /// commitment/bulk trees and private document stores, `height` for
    /// dense trees, 0 for MMR trees.
    pub param: u8,
}

impl NonMerkChunkId {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(NON_MERK_CHUNK_ID_LEN);
        out.extend_from_slice(&self.start.to_be_bytes());
        out.extend_from_slice(&self.state.to_be_bytes());
        out.push(self.param);
        out
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != NON_MERK_CHUNK_ID_LEN {
            return Err(Error::CorruptedData(format!(
                "non-merk chunk id must be {NON_MERK_CHUNK_ID_LEN} bytes, got {}",
                bytes.len()
            )));
        }
        let start = u64::from_be_bytes(bytes[0..8].try_into().expect("checked length"));
        let state = u64::from_be_bytes(bytes[8..16].try_into().expect("checked length"));
        let param = bytes[16];
        Ok(NonMerkChunkId {
            start,
            state,
            param,
        })
    }
}

/// Encode a page of entries.
///
/// Layout: `[more_flag: u8] ++ pack_nested_bytes([aux, entry_1, ...,
/// entry_n])`. `more_flag` is 1 when further pages follow, 0 on the final
/// page. `aux` carries type-specific side data — the serialized Sinsemilla
/// frontier on a commitment tree's first page — and is empty everywhere
/// else.
pub(crate) fn encode_non_merk_page(
    more: bool,
    aux: Vec<u8>,
    entries: Vec<Vec<u8>>,
) -> Result<Vec<u8>, Error> {
    let mut sections = Vec::with_capacity(entries.len() + 1);
    sections.push(aux);
    sections.extend(entries);
    let mut out = vec![u8::from(more)];
    out.extend(pack_nested_bytes(sections)?);
    Ok(out)
}

/// Decode a page of entries. Returns `(more, aux, entries)`.
///
/// Enforces the sender's page budget on receipt, before anything is
/// allocated per entry or replayed:
/// - at most [`MAX_PAGE_ENTRIES`] entries (the packed section count is
///   checked *before* `unpack_nested_bytes` materialises the sections), and
/// - the honest sender stops adding entries once the running byte total
///   reaches [`MAX_PAGE_BYTES`], so every entry but the last must fit under
///   that budget cumulatively. Only the final entry may overhang it, which
///   bounds a page to `MAX_PAGE_BYTES + one entry` — the transport's message
///   size cap bounds that last term.
pub(crate) fn decode_non_merk_page(data: &[u8]) -> Result<(bool, Vec<u8>, Vec<Vec<u8>>), Error> {
    let (&flag, packed) = data.split_first().ok_or_else(|| {
        Error::CorruptedData("non-merk page is empty (missing more-flag)".to_string())
    })?;
    let more = match flag {
        0 => false,
        1 => true,
        other => {
            return Err(Error::CorruptedData(format!(
                "non-merk page has invalid more-flag {other}"
            )));
        }
    };
    // Peek the section count (aux + entries) and apply the entry cap before
    // `unpack_nested_bytes` allocates one `Vec` header per declared section.
    let declared_sections = packed
        .get(0..4)
        .map(|b| u32::from_be_bytes(b.try_into().expect("4 bytes")) as usize)
        .ok_or_else(|| {
            Error::CorruptedData("non-merk page is missing its section count".to_string())
        })?;
    if declared_sections > MAX_PAGE_ENTRIES + 1 {
        return Err(Error::CorruptedData(format!(
            "non-merk page declares {} entries, more than the {MAX_PAGE_ENTRIES} per-page cap",
            declared_sections.saturating_sub(1)
        )));
    }
    let mut sections = unpack_nested_bytes(packed)?;
    if sections.is_empty() {
        return Err(Error::CorruptedData(
            "non-merk page is missing its aux section".to_string(),
        ));
    }
    let entries = sections.split_off(1);
    let aux = sections.pop().expect("checked non-empty");

    // Byte budget: everything before the last entry must have fit under the
    // sender's budget, otherwise the sender would have cut the page earlier.
    if let Some((_last, head)) = entries.split_last() {
        let head_bytes: usize = head.iter().map(Vec::len).sum();
        if head_bytes >= MAX_PAGE_BYTES {
            return Err(Error::CorruptedData(format!(
                "non-merk page carries {head_bytes} entry bytes before its final entry, \
                 exceeding the {MAX_PAGE_BYTES}-byte page budget"
            )));
        }
    }
    Ok((more, aux, entries))
}

// ── Source side ─────────────────────────────────────────────────────────

impl GroveDb {
    /// Serve one page of entries for a non-Merk append-only subtree,
    /// identified by its 32-byte prefix. `chunk_id_bytes` is the
    /// target-encoded [`NonMerkChunkId`] cursor.
    ///
    /// The `state`/`param` fields in the cursor come from the *target's*
    /// element. An honest pair always agrees with the source's own data; a
    /// mismatching cursor from a byzantine peer only produces read errors or
    /// short pages here (bounded by the page budget) — target-side
    /// verification is what protects the *syncing* node. Every
    /// peer-controlled field is validated before it drives tree arithmetic
    /// (see the module docs).
    pub(crate) fn fetch_non_merk_page(
        &self,
        chunk_prefix: crate::SubtreePrefix,
        tree_type: TreeType,
        chunk_id_bytes: &[u8],
        transaction: &Transaction,
    ) -> Result<Vec<u8>, Error> {
        let id = NonMerkChunkId::decode(chunk_id_bytes)?;

        match tree_type {
            // A private document store's payload IS a bulk append tree
            // (the wrapper only adds entry-size validation and the
            // config-binding state root, neither of which affects how
            // stored entries are read), so all three serve pages through
            // `BulkAppendTree::from_state`.
            TreeType::CommitmentTree(_)
            | TreeType::BulkAppendTree(_)
            | TreeType::PrivateDocumentStore(_) => {
                // For a commitment tree, the first page also carries the
                // serialized Sinsemilla frontier: it is an accumulator over
                // the whole append history and cannot be replayed from
                // entries without redoing every Sinsemilla hash.
                let aux = if matches!(tree_type, TreeType::CommitmentTree(_)) && id.start == 0 {
                    let ctx = self
                        .db
                        .get_transactional_storage_context_by_subtree_prefix(
                            chunk_prefix,
                            None,
                            transaction,
                        )
                        .unwrap();
                    ctx.get(COMMITMENT_TREE_DATA_KEY)
                        .unwrap()
                        .map_err(|e| {
                            Error::CorruptedData(format!(
                                "cannot read commitment tree frontier: {e}"
                            ))
                        })?
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                let ctx = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        chunk_prefix,
                        None,
                        transaction,
                    )
                    .unwrap();
                let tree = BulkAppendTree::from_state(id.state, id.param, ctx).map_err(|e| {
                    Error::CorruptedData(format!(
                        "cannot open bulk store of {} entries for page serving: {e}",
                        id.state
                    ))
                })?;

                let epoch_size = tree.epoch_size();
                let chunk_count = tree.chunk_count();
                let buffer_start = chunk_count * epoch_size;
                let total = id.state;

                let mut entries = Vec::new();
                let mut bytes = 0usize;
                let mut pos = id.start;
                // Cache the deserialized blob of the chunk currently being
                // walked so a page spanning a chunk deserializes it once.
                let mut cached_chunk: Option<(u64, Vec<Vec<u8>>)> = None;
                while pos < total && entries.len() < MAX_PAGE_ENTRIES && bytes < MAX_PAGE_BYTES {
                    let value = if pos >= buffer_start {
                        tree.get_buffer_value((pos - buffer_start) as u16)
                            .map_err(|e| {
                                Error::CorruptedData(format!("cannot read buffer entry {pos}: {e}"))
                            })?
                            .ok_or_else(|| {
                                Error::CorruptedData(format!("missing buffer entry {pos}"))
                            })?
                    } else {
                        let chunk_idx = pos / epoch_size;
                        if cached_chunk.as_ref().map(|(idx, _)| *idx) != Some(chunk_idx) {
                            let blob = tree
                                .get_chunk_value(chunk_idx)
                                .map_err(|e| {
                                    Error::CorruptedData(format!(
                                        "cannot read chunk blob {chunk_idx}: {e}"
                                    ))
                                })?
                                .ok_or_else(|| {
                                    Error::CorruptedData(format!("missing chunk blob {chunk_idx}"))
                                })?;
                            let blob_entries = deserialize_chunk_blob(&blob).map_err(|e| {
                                Error::CorruptedData(format!(
                                    "cannot deserialize chunk blob {chunk_idx}: {e}"
                                ))
                            })?;
                            cached_chunk = Some((chunk_idx, blob_entries));
                        }
                        let (_, blob_entries) = cached_chunk.as_ref().expect("just cached");
                        blob_entries
                            .get((pos % epoch_size) as usize)
                            .cloned()
                            .ok_or_else(|| {
                                Error::CorruptedData(format!(
                                    "chunk blob {chunk_idx} has no entry at position {pos}"
                                ))
                            })?
                    };
                    bytes += value.len();
                    entries.push(value);
                    pos += 1;
                }

                encode_non_merk_page(pos < total, aux, entries)
            }
            TreeType::MmrTree => {
                // `id.state` is peer-controlled: reject non-canonical sizes
                // before any leaf index is converted to a position.
                let leaf_count = validate_mmr_size(id.state)?;
                let ctx = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        chunk_prefix,
                        None,
                        transaction,
                    )
                    .unwrap();
                let store = MmrStore::new(&ctx);

                let store_ref: &MmrStore<_> = &store;

                let mut entries = Vec::new();
                let mut bytes = 0usize;
                let mut leaf = id.start;
                while leaf < leaf_count
                    && entries.len() < MAX_PAGE_ENTRIES
                    && bytes < MAX_PAGE_BYTES
                {
                    let node = store_ref
                        .element_at_position(leaf_to_pos(leaf))
                        .value
                        .map_err(|e| {
                            Error::CorruptedData(format!("cannot read MMR leaf {leaf}: {e}"))
                        })?
                        .ok_or_else(|| Error::CorruptedData(format!("missing MMR leaf {leaf}")))?;
                    let value = node.into_value().ok_or_else(|| {
                        Error::CorruptedData(format!("MMR leaf {leaf} carries no value"))
                    })?;
                    bytes += value.len();
                    entries.push(value);
                    leaf += 1;
                }

                encode_non_merk_page(leaf < leaf_count, Vec::new(), entries)
            }
            TreeType::DenseAppendOnlyFixedSizeTree(_) => {
                let count = u16::try_from(id.state).map_err(|_| {
                    Error::CorruptedData(format!(
                        "dense tree count {} exceeds u16 in page cursor",
                        id.state
                    ))
                })?;
                let ctx = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        chunk_prefix,
                        None,
                        transaction,
                    )
                    .unwrap();
                let tree =
                    DenseFixedSizedMerkleTree::from_state(id.param, count, ctx).map_err(|e| {
                        Error::CorruptedData(format!(
                            "cannot open dense tree of {count} entries for page serving: {e}"
                        ))
                    })?;

                let mut entries = Vec::new();
                let mut bytes = 0usize;
                let mut pos = id.start;
                while pos < count as u64
                    && entries.len() < MAX_PAGE_ENTRIES
                    && bytes < MAX_PAGE_BYTES
                {
                    let value = tree
                        .get(pos as u16)
                        .unwrap()
                        .map_err(|e| {
                            Error::CorruptedData(format!("cannot read dense entry {pos}: {e}"))
                        })?
                        .ok_or_else(|| {
                            Error::CorruptedData(format!("missing dense entry {pos}"))
                        })?;
                    bytes += value.len();
                    entries.push(value);
                    pos += 1;
                }

                encode_non_merk_page(pos < count as u64, Vec::new(), entries)
            }
            _ => Err(Error::InternalError(format!(
                "fetch_non_merk_page called for non append-only tree type {tree_type}"
            ))),
        }
    }
}

// ── Target side ─────────────────────────────────────────────────────────

/// Restorer for a non-Merk append-only subtree. Holds only plain data — the
/// per-page storage context is created (and dropped) inside each call, so
/// this type has no lifetime entanglement with the sync transaction.
#[derive(Debug)]
pub(crate) struct NonMerkRestorer {
    /// The subtree's element as declared by the (hash-verified) parent.
    element: Element,
    /// The element value hash bound into the parent leaf:
    /// `combine_hash(value_hash(element_bytes), state_root)`.
    expected_elem_value_hash: CryptoHash,
    /// `value_hash(element_bytes)` for the element as stored in the parent.
    actual_value_hash: CryptoHash,
    /// Total number of leaf entries the element declares.
    expected_entries: u64,
    /// `state` field for outgoing page cursors (see [`NonMerkChunkId`]).
    state_for_source: u64,
    /// `param` field for outgoing page cursors (see [`NonMerkChunkId`]).
    param: u8,
    /// Number of entries replayed so far.
    replayed: u64,
    /// Current MMR size of the partially replayed MMR (MmrTree only).
    mmr_size_so_far: u64,
    /// Whether the final page has been received.
    finished: bool,
}

impl NonMerkRestorer {
    pub(crate) fn new(
        element: Element,
        expected_elem_value_hash: CryptoHash,
        actual_value_hash: CryptoHash,
    ) -> Result<Self, Error> {
        let (expected_entries, state_for_source, param) = match element.underlying() {
            Element::CommitmentTree(total_count, chunk_power, _) => {
                (*total_count, *total_count, *chunk_power)
            }
            Element::BulkAppendTree(total_count, chunk_power, _) => {
                (*total_count, *total_count, *chunk_power)
            }
            Element::MmrTree(mmr_size, _) => (validate_mmr_size(*mmr_size)?, *mmr_size, 0),
            Element::DenseAppendOnlyFixedSizeTree(count, height, _) => {
                (*count as u64, *count as u64, *height)
            }
            Element::PrivateDocumentStore(total_count, _entry_size, chunk_power, _) => {
                (*total_count, *total_count, *chunk_power)
            }
            other => {
                return Err(Error::InternalError(format!(
                    "NonMerkRestorer::new called on a non append-only element: {}",
                    other.type_str()
                )));
            }
        };
        Ok(NonMerkRestorer {
            element,
            expected_elem_value_hash,
            actual_value_hash,
            expected_entries,
            state_for_source,
            param,
            replayed: 0,
            mmr_size_so_far: 0,
            finished: false,
        })
    }

    /// The local chunk id for the first page of this subtree.
    pub(crate) fn initial_chunk_id(&self) -> Vec<u8> {
        NonMerkChunkId {
            start: 0,
            state: self.state_for_source,
            param: self.param,
        }
        .encode()
    }

    /// Validate the aux section of a page and, for a populated commitment
    /// tree's first page, return the frontier bytes to persist.
    ///
    /// The frontier is authenticated at finalize time through the Sinsemilla
    /// root inside the state root, but that root only covers the *decoded*
    /// frontier: `CommitmentFrontier::deserialize` tolerates trailing bytes,
    /// and the target stores the wire bytes verbatim. Their length is what
    /// later frontier saves are billed against (`persisted_frontier_len`),
    /// so padded-but-decodable bytes would silently make the synced node's
    /// storage accounting diverge from the network's. Hence the frontier
    /// must round-trip byte-for-byte through the codec, and its declared
    /// tree size must already agree with the element (the same invariant
    /// `CommitmentTree::open` enforces, checked early here for a precise
    /// error).
    ///
    /// An empty commitment tree never has a stored frontier (the honest
    /// source sends an empty aux), and its state root is a constant that
    /// never reads the payload — so a planted frontier would pass
    /// verification yet be loaded by the target's next append. Reject it.
    fn validate_aux<'a>(&self, aux: &'a [u8]) -> Result<Option<&'a [u8]>, Error> {
        if !(self.element.is_commitment_tree() && self.replayed == 0) {
            if !aux.is_empty() {
                return Err(Error::CorruptedData(
                    "unexpected aux data in non-merk page".to_string(),
                ));
            }
            return Ok(None);
        }
        if self.expected_entries == 0 {
            if !aux.is_empty() {
                return Err(Error::CorruptedData(
                    "empty commitment tree page must not carry a frontier".to_string(),
                ));
            }
            return Ok(None);
        }
        if aux.is_empty() {
            return Err(Error::CorruptedData(
                "populated commitment tree page 0 is missing the frontier".to_string(),
            ));
        }
        let frontier = CommitmentFrontier::deserialize(aux).map_err(|e| {
            Error::CorruptedData(format!("commitment tree frontier is invalid: {e}"))
        })?;
        if frontier.serialize() != aux {
            return Err(Error::CorruptedData(
                "commitment tree frontier is not canonically encoded".to_string(),
            ));
        }
        if frontier.tree_size() != self.expected_entries {
            return Err(Error::CorruptedData(format!(
                "commitment tree frontier covers {} entries, element declares {}",
                frontier.tree_size(),
                self.expected_entries
            )));
        }
        Ok(Some(aux))
    }

    /// Apply one received page: replay its entries through the real append
    /// primitives and return the next page cursor(s), empty when the source
    /// declared this the final page.
    ///
    /// `grove_version` only selects how the replayed writes would be
    /// *billed* (costs are discarded during sync); the bytes written are
    /// identical under every version.
    pub(crate) fn apply_page(
        &mut self,
        db: &GroveDb,
        tx: &Transaction,
        path: &[Vec<u8>],
        chunk_id: &[u8],
        data: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let id = NonMerkChunkId::decode(chunk_id)?;
        if id.start != self.replayed || id.state != self.state_for_source || id.param != self.param
        {
            return Err(Error::InternalError(format!(
                "non-merk page cursor out of order: got start {}, expected {}",
                id.start, self.replayed
            )));
        }
        if self.finished {
            return Err(Error::InternalError(
                "non-merk page received after the final page".to_string(),
            ));
        }

        let (more, aux, entries) = decode_non_merk_page(data)?;

        if self.replayed + entries.len() as u64 > self.expected_entries {
            return Err(Error::CorruptedData(format!(
                "non-merk page overflows declared entry count: {} + {} > {}",
                self.replayed,
                entries.len(),
                self.expected_entries
            )));
        }
        if more && entries.is_empty() {
            return Err(Error::CorruptedData(
                "non-merk page declares more data but carries no entries".to_string(),
            ));
        }
        let frontier_to_store = self.validate_aux(&aux)?;

        let path_refs: Vec<&[u8]> = path.iter().map(|v| v.as_slice()).collect();
        let subtree_path: SubtreePath<&[u8]> = SubtreePath::from(path_refs.as_slice());

        if let Some(frontier) = frontier_to_store {
            let ctx = db
                .db
                .get_immediate_storage_context(subtree_path.clone(), tx)
                .unwrap();
            ctx.put(COMMITMENT_TREE_DATA_KEY, frontier, None, None)
                .unwrap()
                .map_err(|e| {
                    Error::CorruptedData(format!("cannot write commitment tree frontier: {e}"))
                })?;
        }

        match self.element.underlying() {
            Element::CommitmentTree(..) | Element::BulkAppendTree(..) => {
                let ctx = db
                    .db
                    .get_immediate_storage_context(subtree_path, tx)
                    .unwrap();
                let mut tree =
                    BulkAppendTree::from_state(self.replayed, self.param, ctx).map_err(|e| {
                        Error::CorruptedData(format!(
                            "cannot open partially replayed bulk store ({} entries): {e}",
                            self.replayed
                        ))
                    })?;
                for entry in &entries {
                    tree.append(entry, grove_version).map_err(|e| {
                        Error::CorruptedData(format!("cannot replay bulk entry: {e}"))
                    })?;
                }
                tree.commit_mmr(grove_version).map_err(|e| {
                    Error::CorruptedData(format!("cannot flush replayed chunk MMR: {e}"))
                })?;
            }
            Element::MmrTree(..) => {
                let ctx = db
                    .db
                    .get_immediate_storage_context(subtree_path, tx)
                    .unwrap();
                let store = MmrStore::new(&ctx);
                let mut mmr = MMR::new(self.mmr_size_so_far, &store);
                for entry in entries.iter().cloned() {
                    mmr.push(MmrNode::leaf(entry), grove_version)
                        .unwrap()
                        .map_err(|e| {
                            Error::CorruptedData(format!("cannot replay MMR leaf: {e}"))
                        })?;
                }
                mmr.commit()
                    .unwrap()
                    .map_err(|e| Error::CorruptedData(format!("cannot flush replayed MMR: {e}")))?;
                self.mmr_size_so_far = mmr.mmr_size;
            }
            Element::DenseAppendOnlyFixedSizeTree(..) => {
                let ctx = db
                    .db
                    .get_immediate_storage_context(subtree_path, tx)
                    .unwrap();
                let mut tree =
                    DenseFixedSizedMerkleTree::from_state(self.param, self.replayed as u16, ctx)
                        .map_err(|e| {
                            Error::CorruptedData(format!(
                                "cannot open partially replayed dense tree ({} entries): {e}",
                                self.replayed
                            ))
                        })?;
                for entry in &entries {
                    tree.insert(entry, grove_version).unwrap().map_err(|e| {
                        Error::CorruptedData(format!("cannot replay dense entry: {e}"))
                    })?;
                }
            }
            Element::PrivateDocumentStore(_, entry_size, ..) => {
                // Replay through the store wrapper (not the raw bulk tree)
                // so the committed entry_size from the target's
                // hash-verified element is enforced on every wire entry
                // before anything is written.
                let entry_size = *entry_size;
                let ctx = db
                    .db
                    .get_immediate_storage_context(subtree_path, tx)
                    .unwrap();
                let mut store =
                    PrivateDocumentStore::from_state(self.replayed, entry_size, self.param, ctx)
                        .unwrap()
                        .map_err(|e| {
                            Error::CorruptedData(format!(
                        "cannot open partially replayed private document store ({} entries): {e}",
                        self.replayed
                    ))
                        })?;
                store
                    .append_many(entries.iter().map(Vec::as_slice), grove_version)
                    .unwrap()
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "cannot replay private document store entries: {e}"
                        ))
                    })?;
                store.commit_mmr(grove_version).map_err(|e| {
                    Error::CorruptedData(format!("cannot flush replayed document store MMR: {e}"))
                })?;
            }
            _ => unreachable!("NonMerkRestorer::new only accepts append-only elements"),
        }

        self.replayed += entries.len() as u64;

        if more {
            Ok(vec![NonMerkChunkId {
                start: self.replayed,
                state: self.state_for_source,
                param: self.param,
            }
            .encode()])
        } else {
            self.finished = true;
            Ok(vec![])
        }
    }

    /// Verify the fully replayed subtree against the parent binding:
    /// `combine_hash(value_hash(element_bytes), recomputed_state_root)` must
    /// equal the element value hash the parent Merk committed to.
    pub(crate) fn finalize(
        &self,
        db: &GroveDb,
        tx: &Transaction,
        path: &[Vec<u8>],
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        if self.replayed != self.expected_entries {
            return Err(Error::CorruptedData(format!(
                "non-merk subtree replay incomplete: got {} entries, element declares {}",
                self.replayed, self.expected_entries
            )));
        }
        if let Element::MmrTree(mmr_size, _) = self.element.underlying()
            && self.mmr_size_so_far != *mmr_size
        {
            return Err(Error::CorruptedData(format!(
                "replayed MMR size {} does not match element mmr_size {}",
                self.mmr_size_so_far, mmr_size
            )));
        }

        let path_refs: Vec<&[u8]> = path.iter().map(|v| v.as_slice()).collect();
        let subtree_path: SubtreePath<&[u8]> = SubtreePath::from(path_refs.as_slice());

        let state_root =
            db.compute_non_merk_state_root(&self.element, subtree_path, tx, grove_version)?;
        let combined = combine_hash(&self.actual_value_hash, &state_root).unwrap();
        if combined != self.expected_elem_value_hash {
            return Err(Error::CorruptedData(format!(
                "non-merk subtree state root mismatch after replay: combined hash {} does not \
                 match the parent binding {}",
                hex::encode(combined),
                hex::encode(self.expected_elem_value_hash),
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_merk_chunk_id_roundtrip() {
        let id = NonMerkChunkId {
            start: 123456789,
            state: 987654321,
            param: 7,
        };
        let encoded = id.encode();
        assert_eq!(encoded.len(), NON_MERK_CHUNK_ID_LEN);
        assert_eq!(NonMerkChunkId::decode(&encoded).unwrap(), id);
    }

    #[test]
    fn non_merk_chunk_id_rejects_wrong_length() {
        assert!(NonMerkChunkId::decode(&[]).is_err());
        assert!(NonMerkChunkId::decode(&[0u8; 16]).is_err());
        assert!(NonMerkChunkId::decode(&[0u8; 18]).is_err());
    }

    #[test]
    fn non_merk_page_roundtrip() {
        let entries = vec![b"one".to_vec(), b"two".to_vec(), Vec::new()];
        let aux = b"frontier".to_vec();
        let encoded = encode_non_merk_page(true, aux.clone(), entries.clone()).unwrap();
        let (more, got_aux, got_entries) = decode_non_merk_page(&encoded).unwrap();
        assert!(more);
        assert_eq!(got_aux, aux);
        assert_eq!(got_entries, entries);

        let encoded = encode_non_merk_page(false, Vec::new(), Vec::new()).unwrap();
        let (more, got_aux, got_entries) = decode_non_merk_page(&encoded).unwrap();
        assert!(!more);
        assert!(got_aux.is_empty());
        assert!(got_entries.is_empty());
    }

    #[test]
    fn non_merk_page_rejects_malformed() {
        // Empty data: missing flag.
        assert!(decode_non_merk_page(&[]).is_err());
        // Invalid flag.
        assert!(decode_non_merk_page(&[2u8, 0, 0, 0, 0]).is_err());
        // Valid flag but no section count at all.
        assert!(decode_non_merk_page(&[0u8]).is_err());
        // Valid flag but no sections at all (count = 0): aux is mandatory.
        let no_sections = {
            let mut d = vec![0u8];
            d.extend(pack_nested_bytes(vec![]).unwrap());
            d
        };
        assert!(decode_non_merk_page(&no_sections).is_err());
    }

    /// A page with exactly `MAX_PAGE_ENTRIES` (zero-length) entries is the
    /// largest an honest sender produces and must decode; one more entry is
    /// rejected — from the declared section count alone, before any section
    /// is materialised — so a few megabytes of tiny entries can never turn
    /// into millions of replayed writes on the target.
    #[test]
    fn non_merk_page_enforces_entry_cap_on_receipt() {
        let at_cap =
            encode_non_merk_page(false, Vec::new(), vec![Vec::new(); MAX_PAGE_ENTRIES]).unwrap();
        let (_, _, entries) = decode_non_merk_page(&at_cap).unwrap();
        assert_eq!(entries.len(), MAX_PAGE_ENTRIES);

        let over_cap =
            encode_non_merk_page(false, Vec::new(), vec![Vec::new(); MAX_PAGE_ENTRIES + 1])
                .unwrap();
        let err = decode_non_merk_page(&over_cap).unwrap_err();
        assert!(format!("{err:?}").contains("per-page cap"), "got: {err:?}");

        // The cap is applied to the declared count even when the body is
        // truncated — the decoder must not trust the count and allocate.
        let mut lying = vec![0u8];
        lying.extend_from_slice(&(u32::MAX).to_be_bytes());
        lying.extend_from_slice(&[0u8; 64]);
        let err = decode_non_merk_page(&lying).unwrap_err();
        assert!(format!("{err:?}").contains("per-page cap"), "got: {err:?}");
    }

    /// The honest sender stops once the running byte total reaches
    /// `MAX_PAGE_BYTES`, so only the final entry may overhang the budget.
    #[test]
    fn non_merk_page_enforces_byte_budget_on_receipt() {
        // Largest honest shape: just under budget, then one big final entry.
        let honest = encode_non_merk_page(
            true,
            Vec::new(),
            vec![vec![1u8; MAX_PAGE_BYTES - 1], vec![2u8; 4096]],
        )
        .unwrap();
        assert!(decode_non_merk_page(&honest).is_ok());

        // A single entry far over budget is legal (the transport caps it).
        let single =
            encode_non_merk_page(false, Vec::new(), vec![vec![3u8; 3 * MAX_PAGE_BYTES]]).unwrap();
        assert!(decode_non_merk_page(&single).is_ok());

        // Budget already reached before the final entry: the sender would
        // have cut the page — reject.
        let over = encode_non_merk_page(
            true,
            Vec::new(),
            vec![vec![1u8; MAX_PAGE_BYTES], vec![2u8; 1]],
        )
        .unwrap();
        let err = decode_non_merk_page(&over).unwrap_err();
        assert!(format!("{err:?}").contains("page budget"), "got: {err:?}");
    }

    #[test]
    fn validate_mmr_size_accepts_canonical_and_rejects_others() {
        // Leaf counts and their canonical sizes 2n - popcount(n), including
        // the largest representable ones: 2^63 - 1 is one perfect tree of
        // 2^62 leaves, 2^63 is that tree plus a single leaf.
        for (size, leaves) in [
            (0u64, 0u64),
            (1, 1),
            (3, 2),
            (4, 3),
            (7, 4),
            (8, 5),
            (10, 6),
            (11, 7),
            (15, 8),
            ((1 << 63) - 1, 1 << 62),
            (1 << 63, (1 << 62) + 1),
        ] {
            assert_eq!(validate_mmr_size(size).unwrap(), leaves, "size {size}");
        }
        // Non-canonical sizes, including the ones whose naive leaf count
        // (2^63 for u64::MAX) would overflow `leaf_to_pos`.
        for size in [
            2u64,
            5,
            6,
            9,
            12,
            13,
            14,
            (1 << 63) + 1,
            u64::MAX - 1,
            u64::MAX,
        ] {
            let err = validate_mmr_size(size).unwrap_err();
            assert!(
                format!("{err:?}").contains("not a valid MMR size"),
                "size {size}: {err:?}"
            );
        }
    }

    #[test]
    fn entry_replay_predicate_covers_non_merk_family() {
        assert!(supports_entry_replay(TreeType::CommitmentTree(4)));
        assert!(supports_entry_replay(TreeType::MmrTree));
        assert!(supports_entry_replay(TreeType::BulkAppendTree(4)));
        assert!(supports_entry_replay(
            TreeType::DenseAppendOnlyFixedSizeTree(4)
        ));
        assert!(supports_entry_replay(TreeType::PrivateDocumentStore(4)));
        assert!(!supports_entry_replay(TreeType::NormalTree));
        assert!(!supports_entry_replay(TreeType::ProvableCountTree));

        assert!(element_supports_entry_replay(&Element::empty_mmr_tree()));
        assert!(element_supports_entry_replay(
            &Element::empty_private_document_store(16, 4).unwrap()
        ));
        assert!(!element_supports_entry_replay(&Element::empty_tree()));
    }
}
