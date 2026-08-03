//! State-sync support for the non-Merk append-only tree family
//! (`CommitmentTree`, `MmrTree`, `BulkAppendTree`,
//! `DenseAppendOnlyFixedSizeTree`) — see
//! <https://github.com/dashpay/grovedb/issues/785>.
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

use grovedb_bulk_append_tree::{deserialize_chunk_blob, BulkAppendTree};
use grovedb_commitment_tree::COMMITMENT_TREE_DATA_KEY;
use grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree;
use grovedb_merk::{
    tree::{combine_hash, hash::CryptoHash},
    tree_type::TreeType,
};
use grovedb_merkle_mountain_range::{
    leaf_to_pos, mmr_size_to_leaf_count, MMRStoreReadOps, MmrNode, MmrStore, MMR,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageContext};

use crate::{
    replication::utils::{pack_nested_bytes, unpack_nested_bytes},
    Element, Error, GroveDb, Transaction,
};

/// Soft byte budget for a single page of entries. A page always carries at
/// least one entry even if that entry alone exceeds the budget.
const MAX_PAGE_BYTES: usize = 1 << 20; // 1 MiB

/// Hard cap on the number of entries in a single page, so pages of tiny
/// entries stay bounded in element count as well as bytes.
const MAX_PAGE_ENTRIES: usize = 8192;

/// Encoded length of a [`NonMerkChunkId`]: start (8) + state (8) + param (1).
const NON_MERK_CHUNK_ID_LEN: usize = 17;

/// Local chunk id for a non-Merk subtree page request. The target — which
/// holds the hash-verified element — tells the source everything it needs to
/// serve the page, so the source never has to reconstruct tree geometry from
/// its raw namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NonMerkChunkId {
    /// First entry position (0-based) this page should start at.
    pub start: u64,
    /// Type-specific size state from the element: `total_count` for
    /// commitment/bulk trees, `mmr_size` for MMR trees, entry `count` for
    /// dense trees.
    pub state: u64,
    /// Type-specific parameter from the element: `chunk_power` for
    /// commitment/bulk trees, `height` for dense trees, 0 for MMR trees.
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
    let mut sections = unpack_nested_bytes(packed)?;
    if sections.is_empty() {
        return Err(Error::CorruptedData(
            "non-merk page is missing its aux section".to_string(),
        ));
    }
    let entries = sections.split_off(1);
    let aux = sections.pop().expect("checked non-empty");
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
    /// verification is what protects the *syncing* node.
    pub(crate) fn fetch_non_merk_page(
        &self,
        chunk_prefix: crate::SubtreePrefix,
        tree_type: TreeType,
        chunk_id_bytes: &[u8],
        transaction: &Transaction,
    ) -> Result<Vec<u8>, Error> {
        let id = NonMerkChunkId::decode(chunk_id_bytes)?;

        match tree_type {
            TreeType::CommitmentTree(_) | TreeType::BulkAppendTree(_) => {
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
                let mmr_size = id.state;
                let leaf_count = mmr_size_to_leaf_count(mmr_size);
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
            Element::MmrTree(mmr_size, _) => (mmr_size_to_leaf_count(*mmr_size), *mmr_size, 0),
            Element::DenseAppendOnlyFixedSizeTree(count, height, _) => {
                (*count as u64, *count as u64, *height)
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

    /// Apply one received page: replay its entries through the real append
    /// primitives and return the next page cursor(s), empty when the source
    /// declared this the final page.
    pub(crate) fn apply_page(
        &mut self,
        db: &GroveDb,
        tx: &Transaction,
        path: &[Vec<u8>],
        chunk_id: &[u8],
        data: &[u8],
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

        let path_refs: Vec<&[u8]> = path.iter().map(|v| v.as_slice()).collect();
        let subtree_path: SubtreePath<&[u8]> = SubtreePath::from(path_refs.as_slice());

        // Aux section: only a commitment tree's first page may carry data —
        // the serialized frontier, copied verbatim (it is authenticated at
        // finalize time through the sinsemilla root inside the state root).
        if self.element.is_commitment_tree() && self.replayed == 0 {
            if !aux.is_empty() {
                let ctx = db
                    .db
                    .get_immediate_storage_context(subtree_path.clone(), tx)
                    .unwrap();
                ctx.put(COMMITMENT_TREE_DATA_KEY, &aux, None, None)
                    .unwrap()
                    .map_err(|e| {
                        Error::CorruptedData(format!("cannot write commitment tree frontier: {e}"))
                    })?;
            } else if self.expected_entries > 0 {
                return Err(Error::CorruptedData(
                    "populated commitment tree page 0 is missing the frontier".to_string(),
                ));
            }
        } else if !aux.is_empty() {
            return Err(Error::CorruptedData(
                "unexpected aux data in non-merk page".to_string(),
            ));
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
                    tree.append(entry).map_err(|e| {
                        Error::CorruptedData(format!("cannot replay bulk entry: {e}"))
                    })?;
                }
                tree.commit_mmr().map_err(|e| {
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
                    mmr.push(MmrNode::leaf(entry)).unwrap().map_err(|e| {
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
                    tree.insert(entry).unwrap().map_err(|e| {
                        Error::CorruptedData(format!("cannot replay dense entry: {e}"))
                    })?;
                }
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

        let state_root = db.compute_non_merk_state_root(&self.element, subtree_path, tx)?;
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
        // Valid flag but no sections at all (count = 0): aux is mandatory.
        let no_sections = {
            let mut d = vec![0u8];
            d.extend(pack_nested_bytes(vec![]).unwrap());
            d
        };
        assert!(decode_non_merk_page(&no_sections).is_err());
    }
}
